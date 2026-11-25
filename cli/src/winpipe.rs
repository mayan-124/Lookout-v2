//! The local named pipe, Windows only. No TCP, no network port.
//!
//! Server: one thread that owns one pipe instance and serves one request at a
//! time. The pipe exists only while a [`Listener`] is alive, is created with an
//! explicit access list that names only the current user, and refuses remote
//! clients. Client: finds the pipe, checks that the process behind it runs as
//! the same user, sends one line and reads one line.

#![allow(unsafe_code)]

use std::ffi::c_void;
use std::io::{self, BufRead, BufReader, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::ptr::{null, null_mut};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NO_DATA,
    ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, TokenUser, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY,
    TOKEN_USER,
};
use windows_sys::Win32::Storage::FileSystem::{
    ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX,
    SECURITY_IDENTIFICATION,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeServerProcessId,
    PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateMutexW, GetCurrentProcess, OpenMutexW, OpenProcess, OpenProcessToken,
    ResetEvent, SetEvent, WaitForMultipleObjects, INFINITE,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

use crate::cmd::{ConnectError, Transport};
use crate::proto::{Backend, Service, MAX_REQUEST_BYTES};

const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
const SYNCHRONIZE: u32 = 0x0010_0000;
/// How long a client may take to send its request line.
const READ_TIMEOUT_MS: u32 = 2000;
/// How long the app waits for the client to take the answer.
const WRITE_TIMEOUT_MS: u32 = 5000;
const LINGER_MS: u32 = 3000;

struct Handle(HANDLE);

// SAFETY: a kernel handle may be used and closed from any thread.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            // SAFETY: the handle is owned by this value and closed once.
            unsafe { CloseHandle(self.0) };
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn last_error() -> io::Error {
    io::Error::last_os_error()
}

// ------------------------------------------------------------------ identity

fn sid_of_token(token: &Handle) -> Option<String> {
    // SAFETY: standard two-call pattern; buffers are sized by the first call and
    // 8-byte aligned for TOKEN_USER.
    unsafe {
        let mut len = 0u32;
        GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut len);
        if len == 0 {
            return None;
        }
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        if GetTokenInformation(token.0, TokenUser, buf.as_mut_ptr().cast(), len, &mut len) == 0 {
            return None;
        }
        let user = &*(buf.as_ptr().cast::<TOKEN_USER>());
        let mut text: *mut u16 = null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut text) == 0 || text.is_null() {
            return None;
        }
        let mut n = 0;
        while *text.add(n) != 0 {
            n += 1;
        }
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(text, n));
        LocalFree(text.cast());
        Some(s)
    }
}

/// The SID string of the user this process runs as, e.g. `S-1-5-21-...`.
pub fn current_user_sid() -> Option<String> {
    let mut token: HANDLE = null_mut();
    // SAFETY: valid out pointer; the pseudo handle needs no closing.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return None;
    }
    sid_of_token(&Handle(token))
}

fn process_user_sid(pid: u32) -> Option<String> {
    // SAFETY: plain handle calls; every handle is owned by a `Handle`.
    unsafe {
        let process = Handle(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid));
        if process.0.is_null() {
            return None;
        }
        let mut token: HANDLE = null_mut();
        if OpenProcessToken(process.0, TOKEN_QUERY, &mut token) == 0 {
            return None;
        }
        sid_of_token(&Handle(token))
    }
}

/// The pipe's name: includes the user's SID, so two signed-in users never collide.
pub fn pipe_name() -> Option<String> {
    current_user_sid().map(|sid| format!(r"\\.\pipe\lookout-cli-v1-{sid}"))
}

/// The access list of the pipe: nobody but this user. Protected, so it inherits nothing.
pub fn owner_only_sddl(sid: &str) -> String {
    format!("D:P(A;;GA;;;{sid})")
}

struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
            unsafe { LocalFree(self.0) };
        }
    }
}

fn owner_only_descriptor(sid: &str) -> io::Result<SecurityDescriptor> {
    let sddl = wide(&owner_only_sddl(sid));
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: valid NUL-terminated string and out pointer. Revision 1 is SDDL_REVISION_1.
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), 1, &mut sd, null_mut())
    };
    if ok == 0 {
        return Err(last_error());
    }
    Ok(SecurityDescriptor(sd))
}

// ------------------------------------------------------------ "app is running"

/// Held by the app for its whole life, so the command can tell "Lookout is
/// running but command-line access is off" from "Lookout is not running".
pub struct RunningMarker(#[allow(dead_code)] Handle);

pub const RUNNING_MARKER_NAME: &str = r"Local\lookout-app-running-v1";

pub fn mark_running(name: &str) -> Option<RunningMarker> {
    let n = wide(name);
    // SAFETY: valid NUL-terminated name.
    let h = unsafe { CreateMutexW(null(), 0, n.as_ptr()) };
    (!h.is_null()).then(|| RunningMarker(Handle(h)))
}

pub fn app_running(name: &str) -> bool {
    let n = wide(name);
    // SAFETY: valid NUL-terminated name; the handle is closed by `Handle`.
    let h = Handle(unsafe { OpenMutexW(SYNCHRONIZE, 0, n.as_ptr()) });
    !h.0.is_null()
}

// -------------------------------------------------------------------- server

#[derive(Debug, PartialEq, Eq)]
enum IoEnd {
    Closed,
    Stopped,
    Timeout,
    Failed,
}

/// Runs one overlapped call to the end, or cancels it on timeout or stop.
fn run_io(
    pipe: HANDLE,
    event: HANDLE,
    stop: HANDLE,
    timeout_ms: u32,
    start: impl FnOnce(*mut OVERLAPPED, *mut u32) -> i32,
) -> Result<u32, IoEnd> {
    // SAFETY: `ov` and `n` outlive the call; a pending operation is always
    // cancelled and waited for before they go out of scope.
    unsafe {
        let mut ov: OVERLAPPED = std::mem::zeroed();
        ov.hEvent = event;
        ResetEvent(event);
        let mut n = 0u32;
        if start(&mut ov, &mut n) != 0 {
            return Ok(n);
        }
        match GetLastError() {
            ERROR_IO_PENDING => {}
            ERROR_PIPE_CONNECTED => return Ok(0),
            ERROR_BROKEN_PIPE | ERROR_NO_DATA => return Err(IoEnd::Closed),
            _ => return Err(IoEnd::Failed),
        }
        let handles = [event, stop];
        let w = WaitForMultipleObjects(2, handles.as_ptr(), 0, timeout_ms);
        if w == WAIT_OBJECT_0 {
            return if GetOverlappedResult(pipe, &ov, &mut n, 0) != 0 {
                Ok(n)
            } else if matches!(GetLastError(), ERROR_BROKEN_PIPE | ERROR_NO_DATA) {
                Err(IoEnd::Closed)
            } else {
                Err(IoEnd::Failed)
            };
        }
        CancelIoEx(pipe, &ov);
        GetOverlappedResult(pipe, &ov, &mut n, 1);
        Err(if w == WAIT_OBJECT_0 + 1 {
            IoEnd::Stopped
        } else {
            IoEnd::Timeout
        })
    }
}

/// Reads up to the first newline. A request that grows past the size limit
/// comes back as `MAX_REQUEST_BYTES + 1` bytes, which [`Service`] refuses.
fn read_request(pipe: HANDLE, event: HANDLE, stop: HANDLE) -> Option<Vec<u8>> {
    let deadline = Instant::now() + Duration::from_millis(u64::from(READ_TIMEOUT_MS));
    let mut data: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let ms = u32::try_from(left.as_millis()).unwrap_or(READ_TIMEOUT_MS);
        let got = run_io(pipe, event, stop, ms, |ov, n| {
            // SAFETY: `chunk` is valid for 1024 bytes for the duration of the call.
            unsafe { ReadFile(pipe, chunk.as_mut_ptr(), 1024, n, ov) }
        })
        .ok()?;
        if got == 0 {
            return None;
        }
        data.extend_from_slice(&chunk[..got as usize]);
        if let Some(at) = data.iter().position(|b| *b == b'\n') {
            data.truncate(at);
            if data.last() == Some(&b'\r') {
                data.pop();
            }
            return Some(data);
        }
        if data.len() > MAX_REQUEST_BYTES {
            data.truncate(MAX_REQUEST_BYTES + 1);
            return Some(data);
        }
    }
}

fn write_all(pipe: HANDLE, event: HANDLE, stop: HANDLE, bytes: &[u8]) -> bool {
    let mut sent = 0usize;
    while sent < bytes.len() {
        let rest = &bytes[sent..];
        let n = run_io(pipe, event, stop, WRITE_TIMEOUT_MS, |ov, n| {
            // SAFETY: `rest` outlives the call; the length fits u32 (responses are far below 4 GB).
            unsafe {
                WriteFile(
                    pipe,
                    rest.as_ptr(),
                    u32::try_from(rest.len()).unwrap_or(u32::MAX),
                    n,
                    ov,
                )
            }
        });
        match n {
            Ok(0) | Err(_) => return false,
            Ok(n) => sent += n as usize,
        }
    }
    true
}

type Handler = Arc<dyn Fn(&[u8]) -> String + Send + Sync>;

fn serve(pipe: Handle, stop: Arc<Handle>, handler: Handler) {
    // SAFETY: plain event creation; closed by `Handle`.
    let event = Handle(unsafe { CreateEventW(null(), 1, 0, null()) });
    if event.0.is_null() {
        return;
    }
    loop {
        let connected = run_io(pipe.0, event.0, stop.0, INFINITE, |ov, _| {
            // SAFETY: `ov` is valid for the call.
            unsafe { ConnectNamedPipe(pipe.0, ov) }
        });
        if connected.is_err() {
            break;
        }
        if let Some(request) = read_request(pipe.0, event.0, stop.0) {
            let response = handler(&request);
            if write_all(pipe.0, event.0, stop.0, response.as_bytes()) {
                // Let the client take the answer: it closes its end when done.
                let mut b = [0u8; 1];
                let _ = run_io(pipe.0, event.0, stop.0, LINGER_MS, |ov, n| {
                    // SAFETY: `b` is valid for one byte for the duration of the call.
                    unsafe { ReadFile(pipe.0, b.as_mut_ptr(), 1, n, ov) }
                });
            }
        }
        // SAFETY: a live pipe handle.
        unsafe { DisconnectNamedPipe(pipe.0) };
        // SAFETY: a live event handle.
        if unsafe { windows_sys::Win32::System::Threading::WaitForSingleObject(stop.0, 0) }
            == WAIT_OBJECT_0
        {
            break;
        }
    }
}

/// The listener: one thread, one pipe instance, idle until a client connects.
/// Dropping it closes the pipe.
pub struct Listener {
    stop: Arc<Handle>,
    thread: Option<JoinHandle<()>>,
}

impl Listener {
    /// Creates the pipe (failing if one of this name already exists) and starts serving.
    pub fn start(name: &str, handler: Handler) -> io::Result<Self> {
        let sid = current_user_sid().ok_or_else(|| io::Error::other("cannot read the user id"))?;
        let sd = owner_only_descriptor(&sid)?;
        let attrs = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.0.cast::<c_void>(),
            bInheritHandle: 0,
        };
        let n = wide(name);
        // SAFETY: valid name and security attributes for the duration of the call.
        let raw = unsafe {
            CreateNamedPipeW(
                n.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                64 * 1024,
                MAX_REQUEST_BYTES as u32 * 2,
                0,
                &attrs,
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(last_error());
        }
        let pipe = Handle(raw);
        // SAFETY: plain manual-reset event.
        let stop = Arc::new(Handle(unsafe { CreateEventW(null(), 1, 0, null()) }));
        if stop.0.is_null() {
            return Err(last_error());
        }
        let thread_stop = stop.clone();
        let thread = std::thread::Builder::new()
            .name("lookout-cli-pipe".into())
            .spawn(move || serve(pipe, thread_stop, handler))?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }

    /// Closes the pipe and waits for the thread (an answer in flight finishes first).
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        // SAFETY: a live event handle.
        unsafe { SetEvent(self.stop.0) };
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Starts the listener for the app: requests go through [`Service`] (size and
/// rate limits) to `backend`.
pub fn listen<B: Backend + 'static>(name: &str, backend: B) -> io::Result<Listener> {
    let service = Service::new(backend);
    Listener::start(
        name,
        Arc::new(move |raw: &[u8]| service.handle(raw, Instant::now()).to_line()),
    )
}

// -------------------------------------------------------------------- client

/// Talks to the app over the pipe; tells "off" from "not running" with the marker.
pub struct PipeTransport {
    pub pipe: String,
    pub marker: String,
}

impl PipeTransport {
    pub fn default_names() -> Option<Self> {
        Some(Self {
            pipe: pipe_name()?,
            marker: RUNNING_MARKER_NAME.to_owned(),
        })
    }

    fn absent(&self) -> ConnectError {
        if app_running(&self.marker) {
            ConnectError::Off
        } else {
            ConnectError::NotRunning
        }
    }
}

impl Transport for PipeTransport {
    fn call(&self, request_line: &str) -> Result<String, ConnectError> {
        let started = Instant::now();
        let file = loop {
            let opened = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .security_qos_flags(SECURITY_IDENTIFICATION)
                .open(&self.pipe);
            match opened {
                Ok(f) => break f,
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                    if started.elapsed() > Duration::from_secs(3) {
                        return Err(ConnectError::Io("Lookout is busy; try again".into()));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(self.absent()),
                Err(e) => return Err(ConnectError::Io(e.to_string())),
            }
        };
        // Only talk to a pipe served by a process of the same Windows user.
        let mut pid = 0u32;
        // SAFETY: a live pipe handle and a valid out pointer.
        let got = unsafe { GetNamedPipeServerProcessId(file.as_raw_handle().cast(), &mut pid) };
        let mine = current_user_sid();
        if got == 0 || mine.is_none() || process_user_sid(pid) != mine {
            return Err(ConnectError::Io(
                "the pipe does not belong to your Windows account; not using it".into(),
            ));
        }
        let mut writer = &file;
        writer
            .write_all(request_line.as_bytes())
            .map_err(|e| ConnectError::Io(e.to_string()))?;
        let mut line = String::new();
        BufReader::new(&file)
            .read_line(&mut line)
            .map_err(|e| ConnectError::Io(e.to_string()))?;
        if line.is_empty() {
            return Err(ConnectError::Io("Lookout closed the connection".into()));
        }
        Ok(line)
    }
}

/// For tests: the discretionary access list of an open pipe, as SDDL text.
pub fn dacl_sddl_of(file: &std::fs::File) -> Option<String> {
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo, SE_KERNEL_OBJECT,
    };
    use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: a live handle and valid out pointers; the descriptor is freed below.
    unsafe {
        let rc = GetSecurityInfo(
            file.as_raw_handle().cast(),
            SE_KERNEL_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            null_mut(),
            null_mut(),
            &mut sd,
        );
        if rc != 0 || sd.is_null() {
            return None;
        }
        let mut text: *mut u16 = null_mut();
        let ok = ConvertSecurityDescriptorToStringSecurityDescriptorW(
            sd,
            1,
            DACL_SECURITY_INFORMATION,
            &mut text,
            null_mut(),
        );
        let out = if ok != 0 && !text.is_null() {
            let mut n = 0;
            while *text.add(n) != 0 {
                n += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(text, n));
            LocalFree(text.cast());
            Some(s)
        } else {
            None
        };
        LocalFree(sd);
        out
    }
}
