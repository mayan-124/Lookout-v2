//! A thumbnail helper that misbehaves on request, for `tests/thumbnail_helper`.
//! The file's name says how: `crash…`, `hang…` and `silent…` (the first two
//! after sending the icon), `real…` for the Windows shell; anything else sends
//! an icon and a thumbnail.

use std::time::Duration;

use lookout_core::thumbnail;

fn main() {
    thumbnail::serve(|path, side, icon_only, send| {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name.starts_with("real") {
            return thumbnail::shell(path, side, icon_only, send);
        }
        if name.starts_with("silent") {
            return;
        }
        send(true, b"icon".to_vec());
        if icon_only {
            return;
        }
        if name.starts_with("crash") {
            std::process::abort();
        }
        if name.starts_with("hang") {
            std::thread::sleep(Duration::from_secs(60));
        }
        send(false, b"thumb".to_vec());
    });
}
