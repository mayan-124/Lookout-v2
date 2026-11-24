//! Lookout Plugin Protocol Handler (JSON-RPC 2.0).

use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use lookout_core::plugins::sdk::Handler;
use serde_json::{json, Value};
use windows::core::{w, PCWSTR};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOW;

use crate::editor::show_editor;
use crate::hotkey::{parse_hotkey, HotkeyManager};
use crate::overlay::{
    select_capture_session, select_recording_area, select_scrolling_area, Destination,
    OverlayAction,
};
use crate::recording::{execute_screen_recording, execute_screen_recording_gif};
use crate::save::resolve_save_folder;
use crate::scrolling::execute_scrolling_capture;

pub struct ScreenshotPlugin {
    save_folder: Arc<Mutex<PathBuf>>,
    hotkey: Arc<Mutex<String>>,
    hotkey_scrolling: Arc<Mutex<String>>,
    hotkey_recording: Arc<Mutex<String>>,
    hotkey_text: Arc<Mutex<String>>,
    data_dir: Arc<Mutex<PathBuf>>,
    paused: Arc<AtomicBool>,
    hotkey_mgr: Option<HotkeyManager>,
    enable_hotkey: bool,
}

impl ScreenshotPlugin {
    pub fn new() -> Self {
        Self {
            save_folder: Arc::new(Mutex::new(resolve_save_folder(""))),
            hotkey: Arc::new(Mutex::new("Ctrl+Alt+S".to_string())),
            hotkey_scrolling: Arc::new(Mutex::new(String::new())),
            hotkey_recording: Arc::new(Mutex::new(String::new())),
            hotkey_text: Arc::new(Mutex::new(String::new())),
            data_dir: Arc::new(Mutex::new(PathBuf::from("."))),
            paused: Arc::new(AtomicBool::new(false)),
            hotkey_mgr: None,
            enable_hotkey: true,
        }
    }

    /// For unit tests: disables hotkey listener thread
    pub fn without_hotkey() -> Self {
        let mut s = Self::new();
        s.enable_hotkey = false;
        s
    }

    pub fn trigger_capture(&self) {
        if self.paused.load(Ordering::Acquire) {
            eprintln!("Lookout is paused; skipping capture.");
            return;
        }

        let folder = {
            let guard = self.save_folder.lock().unwrap_or_else(|e| e.into_inner());
            guard.clone()
        };

        // Run unified capture overlay and handle chosen action on a dedicated thread to never block RPC
        thread::Builder::new()
            .name("lookout-capture-flow".to_string())
            .spawn(move || {
                if let Some(outcome) = select_capture_session() {
                    match outcome.action {
                        OverlayAction::Copy => {
                            if matches!(
                                outcome.destination,
                                Destination::Clipboard | Destination::Both
                            ) {
                                let _ = crate::clipboard::copy_to_clipboard(&outcome.cropped_image);
                            }
                            if matches!(outcome.destination, Destination::File | Destination::Both)
                            {
                                if let Err(e) =
                                    crate::save::save_screenshot(&outcome.cropped_image, &folder)
                                {
                                    eprintln!("Failed to save screenshot: {e}");
                                }
                            }
                        }
                        OverlayAction::Edit => {
                            show_editor(outcome.cropped_image, outcome.is_drm_black, folder);
                        }
                        OverlayAction::Record => {
                            if let Err(e) = execute_screen_recording(
                                outcome.screen_rect,
                                30,
                                outcome.include_cursor,
                                &folder,
                            ) {
                                eprintln!("Screen recording failed: {e}");
                            }
                        }
                        OverlayAction::Scroll => {
                            match execute_scrolling_capture(outcome.screen_rect) {
                                Ok(img) => {
                                    show_editor(img, false, folder);
                                }
                                Err(e) => {
                                    eprintln!("Scrolling capture failed: {e}");
                                }
                            }
                        }
                        OverlayAction::Text => {
                            match crate::ocr::execute_ocr(&outcome.cropped_image) {
                                Ok(res) => {
                                    let _ = crate::clipboard::copy_text_to_clipboard(
                                        &res.formatted_text,
                                    );
                                    crate::ocr::show_ocr_preview_card(res, 1.0);
                                }
                                Err(e) => {
                                    eprintln!("OCR failed: {e}");
                                }
                            }
                        }
                    }
                }
            })
            .ok();
    }

    pub fn trigger_scrolling_capture(&self) {
        if self.paused.load(Ordering::Acquire) {
            eprintln!("Lookout is paused; skipping scrolling capture.");
            return;
        }

        let folder = {
            let guard = self.save_folder.lock().unwrap_or_else(|e| e.into_inner());
            guard.clone()
        };

        thread::Builder::new()
            .name("lookout-scrolling-capture-flow".to_string())
            .spawn(move || {
                if let Some(rect) = select_scrolling_area() {
                    match execute_scrolling_capture(rect) {
                        Ok(img) => {
                            show_editor(img, false, folder);
                        }
                        Err(e) => {
                            eprintln!("Scrolling capture failed: {e}");
                        }
                    }
                }
            })
            .ok();
    }

    pub fn trigger_recording(&self, export_gif: bool) {
        if self.paused.load(Ordering::Acquire) {
            eprintln!("Lookout is paused; skipping recording.");
            return;
        }

        let folder = {
            let guard = self.save_folder.lock().unwrap_or_else(|e| e.into_inner());
            guard.clone()
        };

        thread::Builder::new()
            .name("lookout-recording-flow".to_string())
            .spawn(move || {
                if let Some(rect) = select_recording_area() {
                    if export_gif {
                        if let Err(e) = execute_screen_recording_gif(rect, 15, 800, true, &folder) {
                            eprintln!("Screen recording to GIF failed: {e}");
                        }
                    } else {
                        if let Err(e) = execute_screen_recording(rect, 30, true, &folder) {
                            eprintln!("Screen recording failed: {e}");
                        }
                    }
                }
            })
            .ok();
    }

    pub fn trigger_ocr(&self) {
        if self.paused.load(Ordering::Acquire) {
            eprintln!("Lookout is paused; skipping text extraction.");
            return;
        }

        thread::Builder::new()
            .name("lookout-ocr-flow".to_string())
            .spawn(move || {
                if let Some(outcome) = select_capture_session() {
                    match crate::ocr::execute_ocr(&outcome.cropped_image) {
                        Ok(res) => {
                            let _ = crate::clipboard::copy_text_to_clipboard(&res.formatted_text);
                            crate::ocr::show_ocr_preview_card(res, 1.0);
                        }
                        Err(e) => {
                            eprintln!("OCR failed: {e}");
                        }
                    }
                }
            })
            .ok();
    }
}

impl Default for ScreenshotPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Handler for ScreenshotPlugin {
    fn notify(&mut self, method: &str, params: Value) {
        if method == "pause_changed" {
            let is_paused = params["paused"].as_bool().unwrap_or(false);
            self.paused.store(is_paused, Ordering::Release);
        }
    }

    fn handle(&mut self, method: &str, params: Value) -> Result<Value, String> {
        match method {
            "initialize" => {
                let is_paused = params["paused"].as_bool().unwrap_or(false);
                self.paused.store(is_paused, Ordering::Release);

                if let Some(dir) = params["data_dir"].as_str() {
                    let mut d = self.data_dir.lock().unwrap_or_else(|e| e.into_inner());
                    *d = PathBuf::from(dir);
                }

                let config = &params["config"];
                let hk_raw = config["hotkey"]
                    .as_str()
                    .unwrap_or("Ctrl+Alt+S")
                    .to_string();
                let hk_scroll_raw = config["hotkey_scrolling"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                let hk_rec_raw = config["hotkey_recording"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                let hk_text_raw = config["hotkey_text"].as_str().unwrap_or("").to_string();
                let sf_str = config["save_folder"].as_str().unwrap_or("");

                // Validate main hotkey string; fall back to default if invalid
                let hk_str = match parse_hotkey(&hk_raw) {
                    Ok(_) => hk_raw,
                    Err(e) => {
                        eprintln!("Warning: config hotkey '{hk_raw}' is invalid: {e}. Defaulting to Ctrl+Alt+S.");
                        "Ctrl+Alt+S".to_string()
                    }
                };

                let hk_scroll_opt = if !hk_scroll_raw.trim().is_empty() {
                    match parse_hotkey(&hk_scroll_raw) {
                        Ok(_) => Some(hk_scroll_raw.trim().to_string()),
                        Err(e) => {
                            eprintln!("Warning: config hotkey_scrolling '{hk_scroll_raw}' is invalid: {e}.");
                            None
                        }
                    }
                } else {
                    None
                };

                let hk_rec_opt = if !hk_rec_raw.trim().is_empty() {
                    match parse_hotkey(&hk_rec_raw) {
                        Ok(_) => Some(hk_rec_raw.trim().to_string()),
                        Err(e) => {
                            eprintln!(
                                "Warning: config hotkey_recording '{hk_rec_raw}' is invalid: {e}."
                            );
                            None
                        }
                    }
                } else {
                    None
                };

                let hk_text_opt = if !hk_text_raw.trim().is_empty() {
                    match parse_hotkey(&hk_text_raw) {
                        Ok(_) => Some(hk_text_raw.trim().to_string()),
                        Err(e) => {
                            eprintln!(
                                "Warning: config hotkey_text '{hk_text_raw}' is invalid: {e}."
                            );
                            None
                        }
                    }
                } else {
                    None
                };

                {
                    let mut hk = self.hotkey.lock().unwrap_or_else(|e| e.into_inner());
                    *hk = hk_str.clone();
                }
                {
                    let mut hks = self
                        .hotkey_scrolling
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    *hks = hk_scroll_opt.clone().unwrap_or_default();
                }
                {
                    let mut hkr = self
                        .hotkey_recording
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    *hkr = hk_rec_opt.clone().unwrap_or_default();
                }
                {
                    let mut hkt = self.hotkey_text.lock().unwrap_or_else(|e| e.into_inner());
                    *hkt = hk_text_opt.clone().unwrap_or_default();
                }
                {
                    let mut sf = self.save_folder.lock().unwrap_or_else(|e| e.into_inner());
                    *sf = resolve_save_folder(sf_str);
                }

                // Start global hotkey listener thread if enabled
                if self.enable_hotkey && self.hotkey_mgr.is_none() {
                    let paused_clone = Arc::clone(&self.paused);
                    let folder_clone = Arc::clone(&self.save_folder);
                    let paused_clone2 = Arc::clone(&self.paused);
                    let folder_clone2 = Arc::clone(&self.save_folder);
                    let paused_clone3 = Arc::clone(&self.paused);
                    let folder_clone3 = Arc::clone(&self.save_folder);
                    let paused_clone4 = Arc::clone(&self.paused);
                    let data_dir_opt = {
                        let d = self.data_dir.lock().unwrap_or_else(|e| e.into_inner());
                        if d.as_os_str().is_empty() {
                            None
                        } else {
                            Some(d.clone())
                        }
                    };

                    self.hotkey_mgr = Some(HotkeyManager::start(
                        hk_str,
                        hk_scroll_opt,
                        hk_rec_opt,
                        hk_text_opt,
                        data_dir_opt,
                        move || {
                            if paused_clone.load(Ordering::Acquire) {
                                return;
                            }
                            let folder = {
                                let g = folder_clone.lock().unwrap_or_else(|e| e.into_inner());
                                g.clone()
                            };
                            thread::Builder::new()
                                .name("lookout-hotkey-capture".to_string())
                                .spawn(move || {
                                    if let Some(outcome) = select_capture_session() {
                                        match outcome.action {
                                            OverlayAction::Copy => {
                                                if matches!(
                                                    outcome.destination,
                                                    Destination::Clipboard | Destination::Both
                                                ) {
                                                    let _ = crate::clipboard::copy_to_clipboard(
                                                        &outcome.cropped_image,
                                                    );
                                                }
                                                if matches!(
                                                    outcome.destination,
                                                    Destination::File | Destination::Both
                                                ) {
                                                    let _ = crate::save::save_screenshot(
                                                        &outcome.cropped_image,
                                                        &folder,
                                                    );
                                                }
                                            }
                                            OverlayAction::Edit => {
                                                show_editor(
                                                    outcome.cropped_image,
                                                    outcome.is_drm_black,
                                                    folder,
                                                );
                                            }
                                            OverlayAction::Record => {
                                                let _ = execute_screen_recording(
                                                    outcome.screen_rect,
                                                    30,
                                                    outcome.include_cursor,
                                                    &folder,
                                                );
                                            }
                                            OverlayAction::Scroll => {
                                                if let Ok(img) =
                                                    execute_scrolling_capture(outcome.screen_rect)
                                                {
                                                    show_editor(img, false, folder);
                                                }
                                            }
                                            OverlayAction::Text => {
                                                if let Ok(res) =
                                                    crate::ocr::execute_ocr(&outcome.cropped_image)
                                                {
                                                    let _ =
                                                        crate::clipboard::copy_text_to_clipboard(
                                                            &res.formatted_text,
                                                        );
                                                    crate::ocr::show_ocr_preview_card(res, 1.0);
                                                }
                                            }
                                        }
                                    }
                                })
                                .ok();
                        },
                        move || {
                            if paused_clone2.load(Ordering::Acquire) {
                                return;
                            }
                            let folder = {
                                let g = folder_clone2.lock().unwrap_or_else(|e| e.into_inner());
                                g.clone()
                            };
                            thread::Builder::new()
                                .name("lookout-hotkey-scrolling".to_string())
                                .spawn(move || {
                                    if let Some(rect) = select_scrolling_area() {
                                        if let Ok(img) = execute_scrolling_capture(rect) {
                                            show_editor(img, false, folder);
                                        }
                                    }
                                })
                                .ok();
                        },
                        move || {
                            if paused_clone3.load(Ordering::Acquire) {
                                return;
                            }
                            let folder = {
                                let g = folder_clone3.lock().unwrap_or_else(|e| e.into_inner());
                                g.clone()
                            };
                            thread::Builder::new()
                                .name("lookout-hotkey-recording".to_string())
                                .spawn(move || {
                                    if let Some(rect) = select_recording_area() {
                                        if let Err(e) =
                                            execute_screen_recording(rect, 30, true, &folder)
                                        {
                                            eprintln!("Screen recording failed: {e}");
                                        }
                                    }
                                })
                                .ok();
                        },
                        move || {
                            if paused_clone4.load(Ordering::Acquire) {
                                return;
                            }
                            thread::Builder::new()
                                .name("lookout-hotkey-text".to_string())
                                .spawn(move || {
                                    if let Some(outcome) = select_capture_session() {
                                        if let Ok(res) =
                                            crate::ocr::execute_ocr(&outcome.cropped_image)
                                        {
                                            let _ = crate::clipboard::copy_text_to_clipboard(
                                                &res.formatted_text,
                                            );
                                            crate::ocr::show_ocr_preview_card(res, 1.0);
                                        }
                                    }
                                })
                                .ok();
                        },
                    ));
                }

                Ok(json!({"protocol": 1}))
            }

            "query" => {
                let folder_display = {
                    let g = self.save_folder.lock().unwrap_or_else(|e| e.into_inner());
                    g.display().to_string()
                };
                let hk_display = {
                    let g = self.hotkey.lock().unwrap_or_else(|e| e.into_inner());
                    g.clone()
                };
                let hk_scroll_display = {
                    let g = self
                        .hotkey_scrolling
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    g.clone()
                };
                let hk_rec_display = {
                    let g = self
                        .hotkey_recording
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    g.clone()
                };
                let hk_text_display = {
                    let g = self.hotkey_text.lock().unwrap_or_else(|e| e.into_inner());
                    g.clone()
                };

                let scroll_sub = if hk_scroll_display.is_empty() {
                    "Capture a scrolling area or long page".to_string()
                } else {
                    format!("Capture a scrolling area or long page ({hk_scroll_display})")
                };
                let rec_sub = if hk_rec_display.is_empty() {
                    "Record screen area or window to MP4".to_string()
                } else {
                    format!("Record screen area or window to MP4 ({hk_rec_display})")
                };
                let text_sub = if hk_text_display.is_empty() {
                    "Select area to extract text and copy to clipboard".to_string()
                } else {
                    format!("Select area to extract text and copy to clipboard ({hk_text_display})")
                };

                let items = vec![
                    json!({
                        "id": "take_screenshot",
                        "title": "Take a screenshot",
                        "subtitle": format!("Select a region on screen ({hk_display})"),
                        "kind": "screenshot",
                        "action": "take_screenshot",
                        "score": 10.0
                    }),
                    json!({
                        "id": "take_scrolling_screenshot",
                        "title": "Scrolling capture",
                        "subtitle": scroll_sub,
                        "kind": "screenshot",
                        "action": "take_scrolling_screenshot",
                        "score": 9.5
                    }),
                    json!({
                        "id": "record_screen",
                        "title": "Record screen (MP4)",
                        "subtitle": rec_sub,
                        "kind": "recording",
                        "action": "record_screen",
                        "score": 9.2
                    }),
                    json!({
                        "id": "record_screen_gif",
                        "title": "Record screen (GIF)",
                        "subtitle": "Record screen area to animated GIF",
                        "kind": "recording",
                        "action": "record_screen_gif",
                        "score": 8.8
                    }),
                    json!({
                        "id": "extract_text",
                        "title": "Extract text (OCR)",
                        "subtitle": text_sub,
                        "kind": "text",
                        "action": "extract_text",
                        "score": 8.7
                    }),
                    json!({
                        "id": "open_folder",
                        "title": "Open screenshots folder",
                        "subtitle": folder_display,
                        "kind": "folder",
                        "action": "open_folder",
                        "score": 8.5
                    }),
                ];

                Ok(json!({ "items": items }))
            }

            "get_actions" => {
                let items = params["items"]
                    .as_array()
                    .ok_or_else(|| "items array is missing".to_string())?;
                let first = items
                    .first()
                    .ok_or_else(|| "items array is empty".to_string())?;
                let id = first["id"].as_str().unwrap_or("");

                if id == "open_folder" || id.ends_with(":open_folder") {
                    Ok(json!({
                        "actions": [
                            {"id": "open_folder", "label": "Open screenshots folder", "risk": "safe"}
                        ]
                    }))
                } else if id == "take_screenshot" || id.ends_with(":take_screenshot") {
                    Ok(json!({
                        "actions": [
                            {"id": "take_screenshot", "label": "Take a screenshot", "risk": "safe"}
                        ]
                    }))
                } else if id == "take_scrolling_screenshot"
                    || id.ends_with(":take_scrolling_screenshot")
                {
                    Ok(json!({
                        "actions": [
                            {"id": "take_scrolling_screenshot", "label": "Scrolling capture", "risk": "safe"}
                        ]
                    }))
                } else if id == "record_screen" || id.ends_with(":record_screen") {
                    Ok(json!({
                        "actions": [
                            {"id": "record_screen", "label": "Record screen (MP4)", "risk": "safe"}
                        ]
                    }))
                } else if id == "record_screen_gif" || id.ends_with(":record_screen_gif") {
                    Ok(json!({
                        "actions": [
                            {"id": "record_screen_gif", "label": "Record screen (GIF)", "risk": "safe"}
                        ]
                    }))
                } else if id == "extract_text" || id.ends_with(":extract_text") {
                    Ok(json!({
                        "actions": [
                            {"id": "extract_text", "label": "Extract text (OCR)", "risk": "safe"}
                        ]
                    }))
                } else {
                    Err(format!("unknown item id '{id}'"))
                }
            }

            "preview" => {
                let action_id = params["action_id"].as_str().unwrap_or("");
                match action_id {
                    "open_folder" => {
                        let path = {
                            let g = self.save_folder.lock().unwrap_or_else(|e| e.into_inner());
                            g.display().to_string()
                        };
                        Ok(json!({
                            "title": "Open screenshots folder",
                            "lines": [path],
                            "warnings": [],
                            "undoable": false,
                            "risk": "safe"
                        }))
                    }
                    "take_screenshot" => Ok(json!({
                        "title": "Take a screenshot",
                        "lines": ["Opens the screen capture overlay to select a region."],
                        "warnings": [],
                        "undoable": false,
                        "risk": "safe"
                    })),
                    "take_scrolling_screenshot" => Ok(json!({
                        "title": "Scrolling capture",
                        "lines": ["Selects an area and scrolls automatically to capture a full page."],
                        "warnings": [],
                        "undoable": false,
                        "risk": "safe"
                    })),
                    "record_screen" => Ok(json!({
                        "title": "Record screen (MP4)",
                        "lines": ["Selects an area and records screen video to an H.264 MP4 file."],
                        "warnings": [],
                        "undoable": false,
                        "risk": "safe"
                    })),
                    "record_screen_gif" => Ok(json!({
                        "title": "Record screen (GIF)",
                        "lines": ["Selects an area and records screen video directly to an animated GIF."],
                        "warnings": [],
                        "undoable": false,
                        "risk": "safe"
                    })),
                    "extract_text" => Ok(json!({
                        "title": "Extract text (OCR)",
                        "lines": ["Selects an area, extracts text using local Windows.Media.Ocr, and copies to clipboard."],
                        "warnings": [],
                        "undoable": false,
                        "risk": "safe"
                    })),
                    other => Err(format!("unknown action id '{other}'")),
                }
            }

            "execute" => {
                let action_id = params["action_id"].as_str().unwrap_or("");
                match action_id {
                    "open_folder" => {
                        let folder = {
                            let g = self.save_folder.lock().unwrap_or_else(|e| e.into_inner());
                            g.clone()
                        };
                        if let Err(e) = std::fs::create_dir_all(&folder) {
                            return Err(format!("Could not create screenshots folder: {e}"));
                        }

                        let mut wide_path: Vec<u16> = folder.as_os_str().encode_wide().collect();
                        wide_path.push(0);

                        let res = unsafe {
                            ShellExecuteW(
                                None,
                                w!("open"),
                                PCWSTR(wide_path.as_ptr()),
                                None,
                                None,
                                SW_SHOW,
                            )
                        };

                        // ShellExecute returns HINSTANCE. If value <= 32, it indicates failure.
                        if (res.0 as usize) <= 32 {
                            return Err(format!(
                                "Failed to open screenshots folder (ShellExecute error code {})",
                                res.0 as usize
                            ));
                        }

                        Ok(json!({"message": "Opened screenshots folder"}))
                    }
                    "take_screenshot" => {
                        self.trigger_capture();
                        Ok(json!({"message": "Screenshot capture started"}))
                    }
                    "take_scrolling_screenshot" => {
                        self.trigger_scrolling_capture();
                        Ok(json!({"message": "Scrolling capture started"}))
                    }
                    "record_screen" => {
                        self.trigger_recording(false);
                        Ok(json!({"message": "Screen recording started"}))
                    }
                    "record_screen_gif" => {
                        self.trigger_recording(true);
                        Ok(json!({"message": "GIF screen recording started"}))
                    }
                    "extract_text" => {
                        self.trigger_ocr();
                        Ok(json!({"message": "Text extraction started"}))
                    }
                    other => Err(format!("unknown action id '{other}'")),
                }
            }

            "undo" => Ok(json!({"message": "Nothing to undo"})),

            "shutdown" => {
                if let Some(mut mgr) = self.hotkey_mgr.take() {
                    mgr.stop();
                }
                Ok(Value::Null)
            }

            other => Err(format!("unknown method '{other}'")),
        }
    }
}
