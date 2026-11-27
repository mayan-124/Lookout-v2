//! Letting something leave Lookout: dragging a result into another app, and
//! "Copy file". Files go through [`Rules::path`] both ways, so they allow the
//! same things; a clip is dragged as text, unless secrets in it were masked.
//! Only COPY and LINK are ever offered; nothing here can move a file.

use std::path::{Path, PathBuf};

use crate::content::extract::is_sensitive;
use crate::exclude::Excludes;
use crate::item::Kind;
use crate::preview::is_online_only;

pub const ONLINE_ONLY: &str = "Online only: download it before dragging or copying.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    #[error("Only files and folders can be dragged or copied.")]
    NotAFile,
    #[error("That file or folder no longer exists.")]
    Missing,
    #[error("Files that commonly hold secrets are not dragged or copied.")]
    Sensitive,
    #[error("That is in a place Lookout skips.")]
    Excluded,
    #[error("{ONLINE_ONLY}")]
    OnlineOnly,
    #[error("This clip has masked secrets, so it cannot be dragged.")]
    MaskedClip,
    #[error("That clip is no longer in the history.")]
    ClipGone,
}

/// What a drag carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Payload {
    File(PathBuf),
    /// A stored clip, links included.
    Text(String),
}

/// `CF_UNICODETEXT`: UTF-16 with its NUL.
pub fn unicode_text(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// What the indexer skips, and the folders it was given.
#[derive(Debug, Clone, Default)]
pub struct Rules {
    pub excludes: Excludes,
    pub roots: Vec<PathBuf>,
}

impl Rules {
    /// The file or folder a result stands for, if it may be dragged or copied.
    /// `from_plugin` results are commands about a file, not the file.
    pub fn path(
        &self,
        kind: &Kind,
        uri: Option<&str>,
        from_plugin: bool,
    ) -> Result<PathBuf, Refusal> {
        if from_plugin || !matches!(kind, Kind::File | Kind::Folder) {
            return Err(Refusal::NotAFile);
        }
        let text = uri.ok_or(Refusal::NotAFile)?.replace('/', "\\");
        if !is_plain_path(&text) {
            return Err(Refusal::NotAFile);
        }
        let path = PathBuf::from(text);
        // By name first: nothing on disk is looked at for these.
        if is_sensitive(&path) {
            return Err(Refusal::Sensitive);
        }
        if self.hides(&path) {
            return Err(Refusal::Excluded);
        }
        let meta = std::fs::symlink_metadata(&path).map_err(|_| Refusal::Missing)?;
        if is_online_only(attributes(&meta)) {
            return Err(Refusal::OnlineOnly);
        }
        Ok(path)
    }

    /// The indexer's skip rules. Below an indexed folder they apply from that
    /// folder down; anywhere else, from the drive.
    pub(crate) fn hides(&self, path: &Path) -> bool {
        if self.excludes.is_excluded_folder(path) {
            return true;
        }
        if self.roots.iter().any(|r| path.starts_with(r)) {
            return self.excludes.excluded_under(path, &self.roots);
        }
        let drive: PathBuf = path.components().take(2).collect();
        self.excludes.excludes_path(path, &drive)
    }
}

/// `C:\...` or `\\server\share\...`; no device paths, no `..`, no data streams.
fn is_plain_path(p: &str) -> bool {
    let b = p.as_bytes();
    let rest = if b.len() > 3 && b[0].is_ascii_alphabetic() && &b[1..3] == b":\\" {
        &p[3..]
    } else if let Some(unc) = p.strip_prefix("\\\\") {
        if unc.starts_with(['?', '.', '\\']) {
            return false;
        }
        unc
    } else {
        return false;
    };
    !rest.contains([':', '\0']) && !rest.split('\\').any(|c| c == "..")
}

#[cfg(windows)]
fn attributes(meta: &std::fs::Metadata) -> u32 {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes()
}

#[cfg(not(windows))]
fn attributes(_: &std::fs::Metadata) -> u32 {
    0
}

/// The clipboard's file list (`CF_HDROP`): a `DROPFILES` header, then each path
/// in UTF-16 with its NUL, then one more NUL.
pub fn drop_files(paths: &[PathBuf]) -> Vec<u8> {
    const HEADER: u32 = 20;
    // pFiles, pt.x, pt.y, fNC, fWide
    let mut out: Vec<u8> = [HEADER, 0, 0, 0, 1]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    for p in paths {
        for unit in p.to_string_lossy().encode_utf16().chain([0]) {
            out.extend_from_slice(&unit.to_le_bytes());
        }
    }
    out.extend_from_slice(&[0, 0]);
    out
}

#[cfg(windows)]
pub use native::{copy_files, drag};

#[cfg(windows)]
mod native {
    use std::mem::ManuallyDrop;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use windows::core::{implement, Interface, Ref, BOOL, HRESULT, PCWSTR};
    use windows::Win32::Foundation::{
        GlobalFree, COLORREF, DATA_S_SAMEFORMATETC, DRAGDROP_S_CANCEL, DRAGDROP_S_DROP,
        DRAGDROP_S_USEDEFAULTCURSORS, DV_E_DVASPECT, DV_E_FORMATETC, DV_E_TYMED, E_NOTIMPL,
        E_OUTOFMEMORY, E_POINTER, HWND, OLE_E_ADVISENOTSUPPORTED, POINT, SIZE, S_OK,
    };
    use windows::Win32::Graphics::Gdi::DeleteObject;
    use windows::Win32::System::Com::{
        CoCreateInstance, IAdviseSink, IDataObject, IDataObject_Impl, IEnumFORMATETC,
        IEnumSTATDATA, CLSCTX_INPROC_SERVER, DATADIR_GET, DVASPECT_CONTENT, FORMATETC, STGMEDIUM,
        STGMEDIUM_0, TYMED_HGLOBAL,
    };
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    use windows::Win32::System::Ole::{
        IDropSource, IDropSource_Impl, OleInitialize, OleUninitialize, DROPEFFECT, DROPEFFECT_COPY,
        DROPEFFECT_LINK, DROPEFFECT_NONE,
    };
    use windows::Win32::System::SystemServices::{MK_LBUTTON, MK_RBUTTON, MODIFIERKEYS_FLAGS};
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VK_ESCAPE, VK_LBUTTON, VK_RBUTTON,
    };
    use windows::Win32::UI::Shell::{
        BHID_DataObject, CLSID_DragDropHelper, IDragSourceHelper, IShellItem,
        IShellItemImageFactory, SHCreateItemFromParsingName, SHCreateStdEnumFmtEtc, SHDoDragDrop,
        SHDRAGIMAGE, SIIGBF_ICONONLY,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_SWAPBUTTON};

    use super::Payload;

    const CF_UNICODETEXT: u16 = 13;

    /// Side of the icon that follows the pointer, in pixels.
    const ICON: i32 = 48;

    fn key_down(key: u16) -> bool {
        // SAFETY: reads the key state only.
        unsafe { GetAsyncKeyState(i32::from(key)) < 0 }
    }

    /// The primary mouse button, also when the user swapped the buttons.
    fn button_down() -> bool {
        // SAFETY: reads a system setting only.
        let swapped = unsafe { GetSystemMetrics(SM_SWAPBUTTON) } != 0;
        key_down(if swapped { VK_RBUTTON.0 } else { VK_LBUTTON.0 })
    }

    /// Ends the drag: release drops, Esc or the other button cancels. Esc is
    /// read from the keyboard because the web page, not this thread, has focus.
    #[implement(IDropSource)]
    struct Source;

    impl IDropSource_Impl for Source_Impl {
        fn QueryContinueDrag(&self, escape: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
            if escape.as_bool() || key_down(VK_ESCAPE.0) || keys.0 & MK_RBUTTON.0 != 0 {
                DRAGDROP_S_CANCEL
            } else if keys.0 & MK_LBUTTON.0 == 0 {
                DRAGDROP_S_DROP
            } else {
                S_OK
            }
        }

        fn GiveFeedback(&self, _effect: DROPEFFECT) -> HRESULT {
            DRAGDROP_S_USEDEFAULTCURSORS
        }
    }

    fn text_format() -> FORMATETC {
        FORMATETC {
            cfFormat: CF_UNICODETEXT,
            ptd: std::ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0,
            lindex: -1,
            tymed: TYMED_HGLOBAL.0 as u32,
        }
    }

    /// Whether `format` asks for the one thing a text drag carries.
    fn offers_text(format: *const FORMATETC) -> HRESULT {
        // SAFETY: the caller of a COM method passes a valid pointer or null.
        let Some(f) = (unsafe { format.as_ref() }) else {
            return E_POINTER;
        };
        if f.cfFormat != CF_UNICODETEXT {
            DV_E_FORMATETC
        } else if f.dwAspect != DVASPECT_CONTENT.0 {
            DV_E_DVASPECT
        } else if f.tymed & TYMED_HGLOBAL.0 as u32 == 0 {
            DV_E_TYMED
        } else {
            S_OK
        }
    }

    /// The data object of a text drag: `CF_UNICODETEXT` and nothing else.
    #[implement(IDataObject)]
    struct TextData(Vec<u8>);

    impl IDataObject_Impl for TextData_Impl {
        fn GetData(&self, format: *const FORMATETC) -> windows::core::Result<STGMEDIUM> {
            offers_text(format).ok()?;
            // SAFETY: the block is as large as the bytes and filled while
            // locked; the receiver frees it (`ReleaseStgMedium`).
            unsafe {
                let block = GlobalAlloc(GMEM_MOVEABLE, self.0.len())?;
                let to = GlobalLock(block).cast::<u8>();
                if to.is_null() {
                    let _ = GlobalFree(Some(block));
                    return Err(E_OUTOFMEMORY.into());
                }
                std::ptr::copy_nonoverlapping(self.0.as_ptr(), to, self.0.len());
                // Reports an "error" when the lock count reaches zero.
                let _ = GlobalUnlock(block);
                Ok(STGMEDIUM {
                    tymed: TYMED_HGLOBAL.0 as u32,
                    u: STGMEDIUM_0 { hGlobal: block },
                    pUnkForRelease: ManuallyDrop::new(None),
                })
            }
        }

        fn GetDataHere(&self, _: *const FORMATETC, _: *mut STGMEDIUM) -> windows::core::Result<()> {
            Err(E_NOTIMPL.into())
        }

        fn QueryGetData(&self, format: *const FORMATETC) -> HRESULT {
            offers_text(format)
        }

        fn GetCanonicalFormatEtc(&self, _: *const FORMATETC, out: *mut FORMATETC) -> HRESULT {
            // SAFETY: the caller passes a writable structure or null.
            let Some(out) = (unsafe { out.as_mut() }) else {
                return E_POINTER;
            };
            out.ptd = std::ptr::null_mut();
            DATA_S_SAMEFORMATETC
        }

        fn SetData(
            &self,
            _: *const FORMATETC,
            _: *const STGMEDIUM,
            _: BOOL,
        ) -> windows::core::Result<()> {
            Err(E_NOTIMPL.into())
        }

        fn EnumFormatEtc(&self, direction: u32) -> windows::core::Result<IEnumFORMATETC> {
            if direction != DATADIR_GET.0 as u32 {
                return Err(E_NOTIMPL.into());
            }
            // SAFETY: the shell copies the list.
            unsafe { SHCreateStdEnumFmtEtc(&[text_format()]) }
        }

        fn DAdvise(
            &self,
            _: *const FORMATETC,
            _: u32,
            _: Ref<IAdviseSink>,
        ) -> windows::core::Result<u32> {
            Err(OLE_E_ADVISENOTSUPPORTED.into())
        }

        fn DUnadvise(&self, _: u32) -> windows::core::Result<()> {
            Err(OLE_E_ADVISENOTSUPPORTED.into())
        }

        fn EnumDAdvise(&self) -> windows::core::Result<IEnumSTATDATA> {
            Err(OLE_E_ADVISENOTSUPPORTED.into())
        }
    }

    fn wide(path: &Path) -> Vec<u16> {
        path.to_string_lossy().encode_utf16().chain([0]).collect()
    }

    /// The shell's own data object for the path (`CF_HDROP` and the shell formats).
    fn shell_data(path: &Path) -> windows::core::Result<(IShellItem, IDataObject)> {
        let text = wide(path);
        // SAFETY: `text` is NUL-terminated and outlives the call; COM is
        // initialised by the caller.
        unsafe {
            let item: IShellItem = SHCreateItemFromParsingName(PCWSTR(text.as_ptr()), None)?;
            let data: IDataObject = item.BindToHandler(None, &BHID_DataObject)?;
            Ok((item, data))
        }
    }

    /// Puts the item's icon under the pointer. A drag without one still works.
    fn set_image(item: &IShellItem, data: &IDataObject) -> windows::core::Result<()> {
        // SAFETY: plain COM calls; the helper owns the bitmap once it accepts
        // it, otherwise it is deleted here.
        unsafe {
            let factory: IShellItemImageFactory = item.cast()?;
            let size = SIZE { cx: ICON, cy: ICON };
            let bitmap = factory.GetImage(size, SIIGBF_ICONONLY)?;
            let image = SHDRAGIMAGE {
                sizeDragImage: size,
                ptOffset: POINT {
                    x: ICON / 2,
                    y: ICON / 2,
                },
                hbmpDragImage: bitmap,
                crColorKey: COLORREF(u32::MAX),
            };
            let taken = CoCreateInstance(&CLSID_DragDropHelper, None, CLSCTX_INPROC_SERVER)
                .and_then(|helper: IDragSourceHelper| helper.InitializeFromBitmap(&image, data));
            if taken.is_err() {
                let _ = DeleteObject(bitmap.into());
            }
            taken
        }
    }

    /// Drags `payload` until the button is released. Must run on the thread
    /// that owns `window`. True if another program took the drop.
    pub fn drag(payload: &Payload, window: isize) -> Result<bool, String> {
        // Released before we got here: starting now would drop it at once.
        if !button_down() {
            return Ok(false);
        }
        // SAFETY: paired with OleUninitialize below, on the same thread.
        unsafe { OleInitialize(None) }.map_err(|e| e.message())?;
        let result = (|| {
            let (data, effects) = match payload {
                Payload::File(path) => {
                    let (item, data) = shell_data(path)?;
                    let _ = set_image(&item, &data);
                    (data, DROPEFFECT_COPY | DROPEFFECT_LINK)
                }
                Payload::Text(text) => (
                    IDataObject::from(TextData(super::unicode_text(text))),
                    DROPEFFECT_COPY,
                ),
            };
            let source: IDropSource = Source.into();
            // SAFETY: both objects live across the call; never MOVE.
            let effect = unsafe {
                SHDoDragDrop(
                    Some(HWND(window as *mut core::ffi::c_void)),
                    &data,
                    &source,
                    effects,
                )
            }?;
            Ok::<_, windows::core::Error>(effect != DROPEFFECT_NONE)
        })();
        // SAFETY: matches the successful OleInitialize above.
        unsafe { OleUninitialize() };
        result.map_err(|e| format!("Could not drag that: {}", e.message()))
    }

    /// Puts the files themselves on the clipboard, to be pasted as copies.
    pub fn copy_files(paths: &[PathBuf]) -> Result<(), String> {
        use windows_sys::Win32::Foundation::GlobalFree;
        use windows_sys::Win32::System::DataExchange::{
            CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW,
            SetClipboardData,
        };
        use windows_sys::Win32::System::Memory::{
            GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
        };
        const CF_HDROP: u32 = 15;

        /// SAFETY: the clipboard must be open.
        unsafe fn put(format: u32, bytes: &[u8]) -> Result<(), String> {
            let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len());
            if h.is_null() {
                return Err("out of memory".to_owned());
            }
            let p = GlobalLock(h).cast::<u8>();
            if p.is_null() {
                GlobalFree(h);
                return Err("could not lock memory".to_owned());
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
            GlobalUnlock(h);
            // The clipboard owns the block unless it refuses it.
            if SetClipboardData(format, h).is_null() {
                GlobalFree(h);
                return Err("the clipboard refused the file".to_owned());
            }
            Ok(())
        }

        let list = super::drop_files(paths);
        let name: Vec<u16> = "Preferred DropEffect\0".encode_utf16().collect();
        // SAFETY: `name` is NUL-terminated; the clipboard is opened and closed here.
        unsafe {
            let effect = RegisterClipboardFormatW(name.as_ptr());
            let opened = (0..10).any(|_| {
                let ok = OpenClipboard(std::ptr::null_mut()) != 0;
                if !ok {
                    std::thread::sleep(Duration::from_millis(30));
                }
                ok
            });
            if !opened {
                return Err("the clipboard is in use by another program".to_owned());
            }
            EmptyClipboard();
            let result = put(CF_HDROP, &list);
            if result.is_ok() && effect != 0 {
                // Paste copies; without this some programs choose for themselves.
                let _ = put(effect, &DROPEFFECT_COPY.0.to_le_bytes());
            }
            CloseClipboard();
            result
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use windows::Win32::System::Ole::ReleaseStgMedium;
        use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};

        /// What a program receiving the list reads from it.
        fn read(list: HDROP) -> Vec<String> {
            // SAFETY: `list` is a valid file list for the whole call.
            unsafe {
                (0..DragQueryFileW(list, u32::MAX, None))
                    .map(|i| {
                        let mut buf = [0u16; 600];
                        let n = DragQueryFileW(list, i, Some(&mut buf)) as usize;
                        String::from_utf16_lossy(&buf[..n])
                    })
                    .collect()
            }
        }

        #[test]
        fn windows_reads_the_clipboard_file_list() {
            let paths = [
                PathBuf::from(r"C:\Docs\résumé 1.pdf"),
                PathBuf::from(r"D:\Work"),
            ];
            let bytes = super::super::drop_files(&paths);
            // SAFETY: the block is filled while locked and freed here; the
            // clipboard is not touched.
            unsafe {
                let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len()).unwrap();
                let p = GlobalLock(h).cast::<u8>();
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
                let _ = GlobalUnlock(h);
                assert_eq!(read(HDROP(h.0)), [r"C:\Docs\résumé 1.pdf", r"D:\Work"]);
                let _ = GlobalFree(Some(h));
            }
        }

        #[test]
        fn a_text_drag_offers_unicode_text_only() {
            let text = "https://example.com/é 🙂\r\nline two";
            let data = IDataObject::from(TextData(super::super::unicode_text(text)));
            // SAFETY: plain calls on the object; the medium is read while
            // locked and released.
            unsafe {
                let want = text_format();
                assert_eq!(data.QueryGetData(&want), S_OK);
                let mut other = want;
                other.cfFormat = 15;
                assert_ne!(data.QueryGetData(&other), S_OK);
                assert!(data.GetData(&other).is_err());

                let list = data.EnumFormatEtc(DATADIR_GET.0 as u32).unwrap();
                let mut got = [FORMATETC::default(); 2];
                let mut n = 0;
                let _ = list.Next(&mut got, Some(&mut n));
                assert_eq!((n, got[0].cfFormat), (1, CF_UNICODETEXT));

                let mut medium = data.GetData(&want).unwrap();
                let p = GlobalLock(medium.u.hGlobal).cast::<u16>();
                let len = (0..).take_while(|&i| *p.add(i) != 0).count();
                let read = String::from_utf16(std::slice::from_raw_parts(p, len)).unwrap();
                let _ = GlobalUnlock(medium.u.hGlobal);
                ReleaseStgMedium(&mut medium);
                assert_eq!(read, text);
            }
        }

        #[test]
        fn the_drag_data_offers_the_real_file() {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("drag me.txt");
            std::fs::write(&file, "x").unwrap();
            // SAFETY: COM is initialised for this test thread; the medium is released.
            unsafe {
                OleInitialize(None).unwrap();
                {
                    let (item, data) = shell_data(&file).unwrap();
                    let _ = set_image(&item, &data);
                    let want = FORMATETC {
                        cfFormat: 15,
                        ptd: std::ptr::null_mut(),
                        dwAspect: DVASPECT_CONTENT.0,
                        lindex: -1,
                        tymed: TYMED_HGLOBAL.0 as u32,
                    };
                    let mut medium = data.GetData(&want).unwrap();
                    let got = read(HDROP(medium.u.hGlobal.0));
                    ReleaseStgMedium(&mut medium);
                    assert_eq!(got.len(), 1);
                    // The shell may spell a temp folder's short name in full.
                    assert!(got[0].ends_with(r"\drag me.txt"), "{got:?}");
                }
                OleUninitialize();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make(dir: &Path, rel: &str) -> String {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "x").unwrap();
        p.to_string_lossy().into_owned()
    }

    fn rules(root: &Path) -> Rules {
        Rules {
            excludes: Excludes::default().with_folders([root.join("private")]),
            roots: vec![root.to_path_buf()],
        }
    }

    #[test]
    fn only_real_files_and_folders_may_leave() {
        let dir = tempfile::tempdir().unwrap();
        let r = rules(dir.path());
        let file = make(dir.path(), "docs/a.txt");
        let folder = dir.path().join("docs").to_string_lossy().into_owned();
        assert_eq!(
            r.path(&Kind::File, Some(&file), false),
            Ok(PathBuf::from(&file))
        );
        assert!(r.path(&Kind::Folder, Some(&folder), false).is_ok());
        assert!(r
            .path(&Kind::File, Some(&file.replace('\\', "/")), false)
            .is_ok());
        let no = Err(Refusal::NotAFile);
        for kind in [
            Kind::App,
            Kind::Clipboard,
            Kind::Snippet,
            Kind::Port,
            Kind::Process,
            Kind::Custom("url".into()),
        ] {
            assert_eq!(r.path(&kind, Some(&file), false), no, "{kind:?}");
        }
        // A plugin's row is a command about the file.
        assert_eq!(r.path(&Kind::File, Some(&file), true), no);
        for uri in [
            "https://example.com/a.txt",
            "a.txt",
            r"\\?\C:\a.txt",
            r"\\.\pipe\x",
            r"C:\a.txt:stream",
            r"C:\a\..\b.txt",
        ] {
            assert_eq!(r.path(&Kind::File, Some(uri), false), no, "{uri}");
        }
        assert_eq!(r.path(&Kind::File, None, false), no);
        assert_eq!(
            r.path(&Kind::File, Some(&format!("{file}.gone")), false),
            Err(Refusal::Missing)
        );
    }

    #[test]
    fn sensitive_and_skipped_files_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let r = rules(dir.path());
        for rel in [".env", ".env.local", "keys/id_rsa", "cert.pem"] {
            let p = make(dir.path(), rel);
            assert_eq!(
                r.path(&Kind::File, Some(&p), false),
                Err(Refusal::Sensitive),
                "{rel}"
            );
        }
        for rel in [
            "private/a.txt",
            "node_modules/x.js",
            "AppData/Local/a.txt",
            ".git/config",
        ] {
            let p = make(dir.path(), rel);
            assert_eq!(
                r.path(&Kind::File, Some(&p), false),
                Err(Refusal::Excluded),
                "{rel}"
            );
        }
    }

    #[test]
    fn outside_the_indexed_folders_the_same_skip_list_applies() {
        let r = Rules::default();
        let mut skipped = vec![
            r"C:\Users\u\AppData\Local\x.txt".to_owned(),
            r"C:\Windows\win.ini".to_owned(),
        ];
        for own in crate::exclude::own_data_folders() {
            skipped.push(own.join("settings.json").to_string_lossy().into_owned());
        }
        for p in &skipped {
            assert_eq!(
                r.path(&Kind::File, Some(p), false),
                Err(Refusal::Excluded),
                "{p}"
            );
        }
        // Lookout's own data stays refused even as an indexed folder.
        let own = crate::exclude::own_data_folders().remove(0);
        let r = Rules {
            roots: vec![own.clone()],
            ..Rules::default()
        };
        let p = own.join("a.txt").to_string_lossy().into_owned();
        assert_eq!(r.path(&Kind::File, Some(&p), false), Err(Refusal::Excluded));
    }

    #[cfg(windows)]
    #[test]
    fn online_only_placeholders_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let r = rules(dir.path());
        let p = make(dir.path(), "cloud.docx");
        // The offline attribute, as a sync program sets it; no download involved.
        let ok = std::process::Command::new("attrib")
            .args(["+o", &p])
            .status()
            .unwrap()
            .success();
        assert!(ok);
        assert_eq!(
            r.path(&Kind::File, Some(&p), false),
            Err(Refusal::OnlineOnly)
        );
    }

    #[test]
    fn the_file_list_has_the_clipboard_layout() {
        let bytes = drop_files(&[PathBuf::from(r"C:\a")]);
        assert_eq!(&bytes[..4], &20u32.to_le_bytes(), "paths start at 20");
        assert_eq!(&bytes[4..16], &[0; 12]);
        assert_eq!(&bytes[16..20], &1u32.to_le_bytes(), "wide characters");
        let units: Vec<u16> = bytes[20..]
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(units, "C:\\a\0\0".encode_utf16().collect::<Vec<_>>());
    }
}
