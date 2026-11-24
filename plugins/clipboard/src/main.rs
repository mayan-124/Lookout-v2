#[cfg(windows)]
fn main() {
    use std::sync::Arc;

    let mut plugin = lookout_clipboard::handler::Clipboard::new(Arc::new(
        lookout_clipboard::win::WindowsClipboard::new(),
    ));
    lookout_core::plugins::sdk::serve(&mut plugin);
}

#[cfg(not(windows))]
fn main() {
    eprintln!("the clipboard plugin needs Windows");
}
