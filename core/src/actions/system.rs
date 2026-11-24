//! What the actions need from the operating system, behind a trait so that
//! tests can use a fake for the dangerous parts. [`WindowsSystem`] is the
//! real implementation.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum SystemError {
    #[error("could not read the list of listening ports (Windows error {0})")]
    PortTable(u32),
    #[error("{0}")]
    Process(String),
}

/// A process as it was when it was looked at. `created` (the creation time
/// in 100 ns ticks) together with `pid` identifies the process even if Windows
/// later reuses the pid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcInfo {
    pub pid: u32,
    pub name: String,
    pub exe: Option<String>,
    /// Full argument list as the process was started, if it could be read.
    pub cmd: Option<Vec<String>>,
    pub cwd: Option<String>,
    pub created: u64,
    pub parent: Option<u32>,
}

/// What to start to bring a stopped process back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnSpec {
    pub exe: String,
    pub args: Vec<String>,
    pub cwd: String,
}

pub trait ProcessBackend: Send + Sync {
    /// Pids with a TCP listener on `port` (IPv4 and IPv6), sorted, unique.
    fn listeners(&self, port: u16) -> Result<Vec<u32>, SystemError>;
    /// Full details of a running process; `None` if it is gone.
    fn inspect(&self, pid: u32) -> Option<ProcInfo>;
    /// Cheap lookup of the program file name, for result rows.
    fn image_name(&self, pid: u32) -> Option<String>;
    /// Ends exactly the process described, refusing if the pid now belongs
    /// to a different process.
    fn terminate(&self, process: &ProcInfo) -> Result<(), SystemError>;
    fn spawn(&self, spec: &SpawnSpec) -> Result<u32, SystemError>;
    fn own_pid(&self) -> u32;
    /// The Windows folder, for flagging system programs.
    fn system_root(&self) -> PathBuf;
}

#[cfg(windows)]
pub use windows_impl::WindowsSystem;

#[cfg(windows)]
mod windows_impl {
    use std::ffi::OsString;
    use std::mem::{offset_of, size_of, zeroed};
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, UpdateKind};
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, NO_ERROR};
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCP6TABLE_OWNER_PID, MIB_TCPROW_OWNER_PID,
        MIB_TCPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
    };
    use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, TerminateProcess,
        WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        PROCESS_TERMINATE,
    };

    use super::{ProcInfo, ProcessBackend, SpawnSpec, SystemError};

    const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const WAIT_OBJECT_0: u32 = 0;

    pub struct WindowsSystem;

    /// Owns a process handle and closes it.
    struct Handle(HANDLE);

    impl Handle {
        fn open(pid: u32, access: u32) -> Option<Handle> {
            // SAFETY: OpenProcess has no memory-safety preconditions; a null
            // result means failure and is checked.
            let h = unsafe { OpenProcess(access, 0, pid) };
            (!h.is_null()).then_some(Handle(h))
        }

        fn creation_time(&self) -> Option<u64> {
            // SAFETY: the four FILETIMEs are written by the call and live for it.
            unsafe {
                let (mut c, mut e, mut k, mut u): (FILETIME, FILETIME, FILETIME, FILETIME) =
                    (zeroed(), zeroed(), zeroed(), zeroed());
                if GetProcessTimes(self.0, &mut c, &mut e, &mut k, &mut u) == 0 {
                    return None;
                }
                Some((u64::from(c.dwHighDateTime) << 32) | u64::from(c.dwLowDateTime))
            }
        }

        fn image_path(&self) -> Option<PathBuf> {
            let mut buf = vec![0u16; 1024];
            let mut len = buf.len() as u32;
            // SAFETY: `buf` has `len` writable elements; the call updates `len`
            // to the number of characters written.
            let ok = unsafe { QueryFullProcessImageNameW(self.0, 0, buf.as_mut_ptr(), &mut len) };
            if ok == 0 {
                return None;
            }
            buf.truncate(len as usize);
            Some(PathBuf::from(OsString::from_wide(&buf)))
        }
    }

    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: the handle was returned by OpenProcess and is closed once.
            unsafe { CloseHandle(self.0) };
        }
    }

    /// Calls `GetExtendedTcpTable` until the buffer is big enough.
    fn tcp_table(family: u32) -> Result<Vec<u64>, SystemError> {
        let mut size: u32 = 0;
        let mut buf: Vec<u64> = Vec::new();
        for _ in 0..6 {
            let ptr = if buf.is_empty() {
                std::ptr::null_mut()
            } else {
                buf.as_mut_ptr().cast()
            };
            // SAFETY: `ptr` is null (size query) or points to `size` writable
            // bytes: the Vec<u64> holds at least `size` bytes.
            let rc = unsafe {
                GetExtendedTcpTable(ptr, &mut size, 0, family, TCP_TABLE_OWNER_PID_LISTENER, 0)
            };
            if rc == NO_ERROR {
                return Ok(buf);
            }
            if rc != ERROR_INSUFFICIENT_BUFFER {
                return Err(SystemError::PortTable(rc));
            }
            // Room for a few more rows that may appear between the calls.
            buf = vec![0u64; (size as usize + 512).div_ceil(8)];
            size = (buf.len() * 8) as u32;
        }
        Err(SystemError::PortTable(ERROR_INSUFFICIENT_BUFFER))
    }

    fn rows<T: Copy>(buf: &[u64], header: usize) -> Vec<T> {
        let bytes = buf.len() * 8;
        if bytes < header + 4 {
            return Vec::new();
        }
        let base = buf.as_ptr().cast::<u8>();
        // SAFETY: the table starts with a u32 row count, and `bytes` bounds every read below.
        let count = unsafe { base.cast::<u32>().read_unaligned() } as usize;
        let mut out = Vec::new();
        for i in 0..count {
            let at = header + i * size_of::<T>();
            if at + size_of::<T>() > bytes {
                break;
            }
            // SAFETY: `at..at + size_of::<T>()` is inside the buffer (checked above)
            // and T is a plain-old-data row type.
            out.push(unsafe { base.add(at).cast::<T>().read_unaligned() });
        }
        out
    }

    /// Network byte order port stored in the low 16 bits of a DWORD.
    fn port_of(raw: u32) -> u16 {
        u16::from_be((raw & 0xFFFF) as u16)
    }

    impl ProcessBackend for WindowsSystem {
        fn listeners(&self, port: u16) -> Result<Vec<u32>, SystemError> {
            let mut pids = Vec::new();
            let v4 = tcp_table(u32::from(AF_INET))?;
            let header = offset_of!(MIB_TCPTABLE_OWNER_PID, table);
            for row in rows::<MIB_TCPROW_OWNER_PID>(&v4, header) {
                if port_of(row.dwLocalPort) == port {
                    pids.push(row.dwOwningPid);
                }
            }
            let v6 = tcp_table(u32::from(AF_INET6))?;
            let header = offset_of!(MIB_TCP6TABLE_OWNER_PID, table);
            for row in rows::<MIB_TCP6ROW_OWNER_PID>(&v6, header) {
                if port_of(row.dwLocalPort) == port {
                    pids.push(row.dwOwningPid);
                }
            }
            pids.sort_unstable();
            pids.dedup();
            Ok(pids)
        }

        fn inspect(&self, pid: u32) -> Option<ProcInfo> {
            let id = Pid::from_u32(pid);
            let mut sys = sysinfo::System::new();
            sys.refresh_processes_specifics(
                ProcessesToUpdate::Some(&[id]),
                true,
                ProcessRefreshKind::nothing()
                    .with_exe(UpdateKind::Always)
                    .with_cmd(UpdateKind::Always)
                    .with_cwd(UpdateKind::Always),
            );
            let p = sys.process(id)?;
            let created = Handle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION)
                .and_then(|h| h.creation_time())
                .unwrap_or(0);
            let cmd: Vec<String> = p
                .cmd()
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            let non_empty =
                |p: &Path| (!p.as_os_str().is_empty()).then(|| p.to_string_lossy().into_owned());
            Some(ProcInfo {
                pid,
                name: p.name().to_string_lossy().into_owned(),
                exe: p.exe().and_then(non_empty),
                cmd: (!cmd.is_empty()).then_some(cmd),
                cwd: p.cwd().and_then(non_empty),
                created,
                parent: p.parent().map(|p| p.as_u32()),
            })
        }

        fn image_name(&self, pid: u32) -> Option<String> {
            let path = Handle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?.image_path()?;
            Some(path.file_name()?.to_string_lossy().into_owned())
        }

        fn terminate(&self, process: &ProcInfo) -> Result<(), SystemError> {
            if process.created == 0 {
                return Err(SystemError::Process(format!(
                    "cannot tell that PID {} is still the same process, so it was not stopped",
                    process.pid
                )));
            }
            let access =
                PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE;
            let handle = Handle::open(process.pid, access).ok_or_else(|| {
                SystemError::Process(format!(
                    "Windows would not let Lookout stop {} (PID {}); it may need administrator rights",
                    process.name, process.pid
                ))
            })?;
            // The identity check and the kill use the same handle, so the pid
            // cannot be reassigned between them.
            if handle.creation_time() != Some(process.created) {
                return Err(SystemError::Process(format!(
                    "PID {} is not {} any more; nothing was stopped",
                    process.pid, process.name
                )));
            }
            // SAFETY: the handle is valid and has PROCESS_TERMINATE access.
            if unsafe { TerminateProcess(handle.0, 1) } == 0 {
                return Err(SystemError::Process(format!(
                    "could not stop {} (PID {}): {}",
                    process.name,
                    process.pid,
                    std::io::Error::last_os_error()
                )));
            }
            // SAFETY: the handle is valid and has SYNCHRONIZE access.
            let waited = unsafe { WaitForSingleObject(handle.0, 5000) };
            if waited != WAIT_OBJECT_0 {
                return Err(SystemError::Process(format!(
                    "asked {} (PID {}) to stop but it is still running",
                    process.name, process.pid
                )));
            }
            Ok(())
        }

        fn spawn(&self, spec: &SpawnSpec) -> Result<u32, SystemError> {
            let child = Command::new(&spec.exe)
                .args(&spec.args)
                .current_dir(&spec.cwd)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW)
                .spawn()
                .map_err(|e| SystemError::Process(format!("could not start {}: {e}", spec.exe)))?;
            Ok(child.id())
        }

        fn own_pid(&self) -> u32 {
            std::process::id()
        }

        fn system_root(&self) -> PathBuf {
            std::env::var_os("SystemRoot")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        }
    }
}
