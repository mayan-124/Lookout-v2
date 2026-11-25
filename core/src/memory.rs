//! Returning freed memory to Windows.

/// Asks every heap of this process to give back pages it holds but does not use.
#[cfg(windows)]
pub fn release_free_heap() {
    use windows_sys::Win32::System::Memory::{HeapOptimizeResources, HeapSetInformation};
    use windows_sys::Win32::System::SystemServices::{
        HEAP_OPTIMIZE_RESOURCES_CURRENT_VERSION, HEAP_OPTIMIZE_RESOURCES_INFORMATION,
    };
    let info = HEAP_OPTIMIZE_RESOURCES_INFORMATION {
        Version: HEAP_OPTIMIZE_RESOURCES_CURRENT_VERSION,
        Flags: 0,
    };
    // SAFETY: a null heap handle means every heap; `info` is a correctly sized
    // structure for this information class. Failure only means nothing is freed.
    unsafe {
        HeapSetInformation(
            std::ptr::null_mut(),
            HeapOptimizeResources,
            std::ptr::from_ref(&info).cast(),
            std::mem::size_of_val(&info),
        );
    }
}

#[cfg(not(windows))]
pub fn release_free_heap() {}
