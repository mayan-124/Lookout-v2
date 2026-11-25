//! "Start Lookout when I sign in to Windows": one value under the current
//! user's Run key. No admin rights, no service, no scheduled task.

use std::io;
use std::path::Path;

use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteKeyValueW, RegGetValueW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
};

/// Added to the command Windows runs at sign-in: that start shows no window.
pub const AUTOSTART_FLAG: &str = "--autostart";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// The uninstaller (`ui/src-tauri/installer-hooks.nsh`) removes this name.
const VALUE: &str = "Lookout";
const NOT_FOUND: u32 = 2;

/// The command for `exe`: the full path in quotes, so spaces are safe.
pub fn command_for(exe: &Path) -> String {
    format!("\"{}\" {AUTOSTART_FLAG}", exe.display())
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn check(rc: u32) -> io::Result<()> {
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(rc as i32))
    }
}

/// Lookout's value in a Run key under `HKEY_CURRENT_USER`.
pub struct RunEntry {
    key: String,
}

impl Default for RunEntry {
    /// The key Windows reads at sign-in.
    fn default() -> Self {
        Self::at(RUN_KEY)
    }
}

impl RunEntry {
    /// Another subkey, so tests never touch the real one.
    pub fn at(key: &str) -> Self {
        Self {
            key: key.to_owned(),
        }
    }

    /// The stored command; `None` if there is no entry.
    pub fn read(&self) -> io::Result<Option<String>> {
        let (key, name) = (wide(&self.key), wide(VALUE));
        // Longer than any path Windows would run from this key.
        let mut data = vec![0u16; 33_000];
        let mut bytes = (data.len() * 2) as u32;
        // SAFETY: both names are NUL-terminated and `bytes` is the size of `data`.
        let rc = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                data.as_mut_ptr().cast(),
                &mut bytes,
            )
        };
        if rc == NOT_FOUND {
            return Ok(None);
        }
        check(rc)?;
        data.truncate(bytes as usize / 2);
        while data.last() == Some(&0) {
            data.pop();
        }
        Ok(Some(String::from_utf16_lossy(&data)))
    }

    fn write(&self, command: &str) -> io::Result<()> {
        let (path, name, value) = (wide(&self.key), wide(VALUE), wide(command));
        let mut key: HKEY = std::ptr::null_mut();
        // SAFETY: `key` is written by the call; the path is NUL-terminated.
        check(unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                path.as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                std::ptr::null(),
                &mut key,
                std::ptr::null_mut(),
            )
        })?;
        // SAFETY: both buffers are NUL-terminated and the length is `value` in bytes.
        let rc = unsafe {
            let rc = RegSetValueExW(
                key,
                name.as_ptr(),
                0,
                REG_SZ,
                value.as_ptr().cast::<u8>(),
                (value.len() * 2) as u32,
            );
            RegCloseKey(key);
            rc
        };
        check(rc)
    }

    fn remove(&self) -> io::Result<()> {
        let (key, name) = (wide(&self.key), wide(VALUE));
        // SAFETY: both names are NUL-terminated.
        let rc = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr()) };
        if rc == NOT_FOUND {
            return Ok(());
        }
        check(rc)
    }

    /// Makes the entry match the switch: on writes the command for `exe` (also
    /// repairing an old path), off removes it.
    pub fn set(&self, on: bool, exe: &Path) -> io::Result<()> {
        if !on {
            return self.remove();
        }
        let wanted = command_for(exe);
        // An unreadable value is simply written again.
        if self.read().ok().flatten().as_deref() == Some(wanted.as_str()) {
            return Ok(());
        }
        self.write(&wanted)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use windows_sys::Win32::System::Registry::RegDeleteTreeW;

    use super::*;
    use crate::settings::Settings;

    /// A throwaway subkey, deleted again when the test ends.
    struct TestKey(String);

    impl TestKey {
        fn new(name: &str) -> Self {
            Self(format!(
                r"Software\LookoutTest-{}-{name}",
                std::process::id()
            ))
        }
        fn entry(&self) -> RunEntry {
            RunEntry::at(&self.0)
        }
    }

    impl Drop for TestKey {
        fn drop(&mut self) {
            let key = wide(&self.0);
            // SAFETY: the name is NUL-terminated; only the test's own key is removed.
            unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, key.as_ptr()) };
        }
    }

    fn exe() -> PathBuf {
        PathBuf::from(r"C:\Users\Some One\AppData\Local\Lookout\lookout.exe")
    }

    #[test]
    fn the_command_quotes_a_path_with_spaces() {
        assert_eq!(
            command_for(&exe()),
            r#""C:\Users\Some One\AppData\Local\Lookout\lookout.exe" --autostart"#
        );
    }

    #[test]
    fn the_switch_writes_and_removes_the_value_and_is_saved() {
        let key = TestKey::new("switch");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let mut s = Settings::default();
        // Removing what was never there is fine.
        key.entry().set(false, &exe()).unwrap();
        s.set_start_with_windows(true, &path, &key.entry(), &exe())
            .unwrap();
        assert_eq!(key.entry().read().unwrap(), Some(command_for(&exe())));
        assert!(Settings::load(&path).unwrap().starts_with_windows());
        s.set_start_with_windows(false, &path, &key.entry(), &exe())
            .unwrap();
        assert_eq!(key.entry().read().unwrap(), None);
        assert!(!Settings::load(&path).unwrap().starts_with_windows());
    }

    #[test]
    fn a_failed_save_leaves_switch_and_entry_as_they_were() {
        let key = TestKey::new("nosave");
        let dir = tempfile::tempdir().unwrap();
        // The settings path is a folder: saving over it fails.
        let mut s = Settings::default();
        assert!(s
            .set_start_with_windows(true, dir.path(), &key.entry(), &exe())
            .is_err());
        assert!(!s.starts_with_windows());
        assert_eq!(key.entry().read().unwrap(), None);
    }

    #[test]
    fn a_start_repairs_the_path_only_while_the_switch_is_on() {
        let key = TestKey::new("repair");
        let old = PathBuf::from(r"D:\Old Place\lookout.exe");
        key.entry().set(true, &old).unwrap();
        let mut s = Settings::default();
        s.verify_start_with_windows(&key.entry(), &exe()).unwrap();
        assert_eq!(key.entry().read().unwrap(), Some(command_for(&old)));
        s.start_with_windows = Some(true);
        s.verify_start_with_windows(&key.entry(), &exe()).unwrap();
        assert_eq!(key.entry().read().unwrap(), Some(command_for(&exe())));
        // A missing entry comes back too.
        key.entry().set(false, &exe()).unwrap();
        s.verify_start_with_windows(&key.entry(), &exe()).unwrap();
        assert_eq!(key.entry().read().unwrap(), Some(command_for(&exe())));
    }
}
