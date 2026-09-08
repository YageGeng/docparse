//! C ABI shims for the deliberately single-threaded PDFium browser runtime.

/// Supplies a process identity for PDFium's single-instance browser libc.
#[allow(unsafe_code)] // Required C ABI exports; no Rust memory is dereferenced here.
#[unsafe(no_mangle)]
pub extern "C" fn getpid() -> i32 {
    1
}
/// Initializes a mutex in the deliberately single-threaded PDFium instance.
#[allow(unsafe_code)] // Required C ABI exports; no Rust memory is dereferenced here.
#[unsafe(no_mangle)]
pub extern "C" fn pthread_mutex_init(
    _mutex: *mut u8,
    _attributes: *const u8,
) -> i32 {
    0
}
/// Serial actor execution provides mutual exclusion for this browser instance.
#[allow(unsafe_code)] // Required C ABI exports; no Rust memory is dereferenced here.
#[unsafe(no_mangle)]
pub extern "C" fn pthread_mutex_lock(_mutex: *mut u8) -> i32 {
    0
}
/// Ends the actor-local critical section without emulating native threads.
#[allow(unsafe_code)] // Required C ABI exports; no Rust memory is dereferenced here.
#[unsafe(no_mangle)]
pub extern "C" fn pthread_mutex_unlock(_mutex: *mut u8) -> i32 {
    0
}
/// Releases the single-threaded mutex placeholder without owning external resources.
#[allow(unsafe_code)] // Required C ABI exports; no Rust memory is dereferenced here.
#[unsafe(no_mangle)]
pub extern "C" fn pthread_mutex_destroy(_mutex: *mut u8) -> i32 {
    0
}
