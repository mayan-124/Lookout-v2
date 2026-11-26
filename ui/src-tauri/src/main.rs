#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // The same program, started to make one thumbnail: no window, no app.
    let first = std::env::args().nth(1);
    if first.as_deref() == Some(lookout_core::thumbnail::HELPER_ARG) {
        lookout_core::thumbnail::helper_main();
        return;
    }
    lookout_lib::run();
}
