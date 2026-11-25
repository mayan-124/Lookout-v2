//! Screenshot file saving, path resolution, and collision-free numbered filenames.

use std::fs;
use std::path::{Path, PathBuf};

use windows::Win32::System::SystemInformation::GetLocalTime;

use crate::image::ImageBuffer;

/// Resolves the destination folder for screenshots.
/// If `configured` is empty, defaults to `%USERPROFILE%\Pictures\Lookout Screenshots`.
pub fn resolve_save_folder(configured: &str) -> PathBuf {
    if !configured.trim().is_empty() {
        return PathBuf::from(configured.trim());
    }

    if let Ok(profile) = std::env::var("USERPROFILE") {
        PathBuf::from(profile)
            .join("Pictures")
            .join("Lookout Screenshots")
    } else {
        PathBuf::from("Lookout Screenshots")
    }
}

/// Generates a timestamp string for screenshot filenames: "YYYY-MM-DD HHMMSS".
pub fn current_timestamp_string() -> String {
    unsafe {
        let st = GetLocalTime();
        format!(
            "{:04}-{:02}-{:02} {:02}{:02}{:02}",
            st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
        )
    }
}

/// Generates a timestamp string for screen recordings: "YYYY-MM-DD HH-MM-SS".
pub fn current_timestamp_recording_string() -> String {
    unsafe {
        let st = GetLocalTime();
        format!(
            "{:04}-{:02}-{:02} {:02}-{:02}-{:02}",
            st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
        )
    }
}

/// Computes a unique, collision-free file path for a new screenshot or recording.
/// Never overwrites an existing file: appends numbered suffix " (1)", " (2)", etc.
pub fn next_available_named_path(dir: &Path, prefix: &str, timestamp: &str, ext: &str) -> PathBuf {
    let base_name = format!("{prefix} {timestamp}");
    let clean_ext = ext.trim_start_matches('.');
    let initial = dir.join(format!("{base_name}.{clean_ext}"));
    if !initial.exists() {
        return initial;
    }

    let mut counter = 1u32;
    loop {
        let candidate = dir.join(format!("{base_name} ({counter}).{clean_ext}"));
        if !candidate.exists() {
            return candidate;
        }
        counter += 1;
    }
}

/// Computes a unique, collision-free file path for a new screenshot.
/// Never overwrites an existing file: appends numbered suffix " (1)", " (2)", etc.
pub fn next_available_path(dir: &Path, timestamp: &str) -> PathBuf {
    next_available_named_path(dir, "Screenshot", timestamp, "png")
}

/// Saves the given image buffer to the configured save folder without overwriting existing files.
/// Returns the path to the saved PNG file.
pub fn save_screenshot(image: &ImageBuffer, folder: &Path) -> Result<PathBuf, String> {
    if let Err(e) = fs::create_dir_all(folder) {
        return Err(format!(
            "Could not create screenshots directory '{}': {e}",
            folder.display()
        ));
    }

    let ts = current_timestamp_string();
    // The name is claimed atomically, so two captures in the same second (a
    // double hotkey press) never overwrite each other.
    for _ in 0..1000 {
        let target_path = next_available_path(folder, &ts);
        match image.save_png_new(&target_path) {
            Ok(Ok(())) => return Ok(target_path),
            Ok(Err(e)) => {
                return Err(format!(
                    "Failed to save screenshot to '{}': {e}",
                    target_path.display()
                ))
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(format!(
                    "Failed to save screenshot to '{}': {e}",
                    target_path.display()
                ))
            }
        }
    }
    Err("Could not find a free file name for the screenshot".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unique_filename_generation() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path();
        let ts = "2026-11-21 160000";

        // First screenshot: should be Screenshot 2026-11-21 160000.png
        let p1 = next_available_path(path, ts);
        assert_eq!(p1.file_name().unwrap(), "Screenshot 2026-11-21 160000.png");
        fs::write(&p1, b"test1").unwrap();

        // Second screenshot with same timestamp: should be Screenshot 2026-11-21 160000 (1).png
        let p2 = next_available_path(path, ts);
        assert_eq!(
            p2.file_name().unwrap(),
            "Screenshot 2026-11-21 160000 (1).png"
        );
        fs::write(&p2, b"test2").unwrap();

        // Third screenshot: should be Screenshot 2026-11-21 160000 (2).png
        let p3 = next_available_path(path, ts);
        assert_eq!(
            p3.file_name().unwrap(),
            "Screenshot 2026-11-21 160000 (2).png"
        );
    }
}
