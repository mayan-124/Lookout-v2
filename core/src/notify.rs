//! Desktop notifications. [`Notifier`] is what timers need; [`WindowsToast`]
//! shows a Windows toast (with the default notification sound) through the
//! WinRT API that is already linked for OCR, so it needs no new dependency.
//! It makes no network connection.

use std::sync::{Arc, Mutex, PoisonError};

pub trait Notifier: Send + Sync {
    /// Shows a notification. Failures are returned so they can be logged; the
    /// caller never retries.
    fn notify(&self, title: &str, body: &str) -> Result<(), String>;
}

/// Records notifications instead of showing them (tests).
#[derive(Default)]
pub struct RecordingNotifier {
    pub shown: Mutex<Vec<(String, String)>>,
}

impl RecordingNotifier {
    pub fn all(&self) -> Vec<(String, String)> {
        self.shown
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Notifier for RecordingNotifier {
    fn notify(&self, title: &str, body: &str) -> Result<(), String> {
        self.shown
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((title.to_owned(), body.to_owned()));
        Ok(())
    }
}

/// The notifier the app uses.
pub fn default_notifier() -> Arc<dyn Notifier> {
    #[cfg(windows)]
    {
        Arc::new(WindowsToast)
    }
    #[cfg(not(windows))]
    {
        Arc::new(RecordingNotifier::default())
    }
}

/// Escapes text for an XML element.
pub fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// The toast XML. No `<audio>` element, so Windows plays its default
/// notification sound.
pub fn toast_xml(title: &str, body: &str) -> String {
    format!(
        "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>",
        xml_escape(title),
        xml_escape(body)
    )
}

#[cfg(windows)]
pub use real::WindowsToast;

#[cfg(windows)]
mod real {
    use windows::core::HSTRING;
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
    use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE,
        REG_OPTION_NON_VOLATILE, REG_SZ,
    };

    use super::{toast_xml, Notifier};

    /// Same as the bundle identifier, so an installed Lookout and this agree.
    const AUMID: &str = "dev.lookout.app";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Gives the app a name in the notification area. Without this entry an
    /// unpackaged program's toasts are silently dropped. Written under the
    /// current user only.
    fn register_name() {
        let path = wide(&format!(r"Software\Classes\AppUserModelId\{AUMID}"));
        let mut key: HKEY = std::ptr::null_mut();
        // SAFETY: `key` is written by the call; the path is NUL-terminated.
        let rc = unsafe {
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
        };
        if rc != 0 {
            return;
        }
        let (name, value) = (wide("DisplayName"), wide("Lookout"));
        // SAFETY: both buffers are NUL-terminated and `len` is their byte size.
        unsafe {
            RegSetValueExW(
                key,
                name.as_ptr(),
                0,
                REG_SZ,
                value.as_ptr().cast::<u8>(),
                (value.len() * 2) as u32,
            );
            RegCloseKey(key);
        }
    }

    pub struct WindowsToast;

    impl Notifier for WindowsToast {
        fn notify(&self, title: &str, body: &str) -> Result<(), String> {
            let show = || -> windows::core::Result<()> {
                // SAFETY: plain COM initialisation for this thread; repeating it is harmless.
                unsafe {
                    let _ = RoInitialize(RO_INIT_MULTITHREADED);
                }
                register_name();
                let doc = XmlDocument::new()?;
                doc.LoadXml(&HSTRING::from(toast_xml(title, body)))?;
                let toast = ToastNotification::CreateToastNotification(&doc)?;
                ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(AUMID))?
                    .Show(&toast)
            };
            show().map_err(|e| format!("could not show the notification: {e}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toast_text_is_escaped_and_has_no_audio_override() {
        let x = toast_xml("Tea <b>", "a & b \"q\"\n");
        assert!(x.contains("Tea &lt;b&gt;"));
        assert!(x.contains("a &amp; b &quot;q&quot;"));
        assert!(!x.contains("<audio"));
        assert!(!x.contains('\n'));
    }
}
