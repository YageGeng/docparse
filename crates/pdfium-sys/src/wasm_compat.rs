//! Target and OS decisions for the generated PDFium bindings.
#[cfg(all(target_arch = "wasm32", not(feature = "wasm")))]
compile_error!("docparse PDFium Web builds require the wasm feature");
#[cfg(all(target_arch = "wasm32", not(target_os = "unknown")))]
compile_error!("docparse supports wasm32-unknown-unknown browser builds only");

#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
#[path = "dynamic.rs"]
pub mod dynamic;

#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
mod native {
    use std::path::PathBuf;
    const PDFIUM_LIB_DIR: &str = env!("PDFIUM_LIB_DIR");
    /// Shared library file name for the current platform.
    pub(crate) fn dylib_name() -> &'static str {
        #[cfg(target_os = "macos")]
        {
            "libpdfium.dylib"
        }
        #[cfg(target_os = "windows")]
        {
            "pdfium.dll"
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            "libpdfium.so"
        }
    }

    /// Get the directory containing the current shared library (the .pyd/.so/.node/.dll
    /// that this code is compiled into). This lets us find sibling files like pdfium.dll
    /// that are bundled next to the native extension in Python wheels, Node packages, etc.
    fn self_dir() -> Option<PathBuf> {
        // Use a static function in this module as the probe address.
        let addr = self_dir as *const ();

        #[cfg(target_os = "windows")]
        {
            use std::ffi::OsString;
            use std::os::windows::ffi::OsStringExt;

            #[link(name = "kernel32")]
            unsafe extern "system" {
                fn GetModuleHandleExW(
                    dwFlags: u32,
                    lpModuleName: *const u8,
                    phModule: *mut *mut std::ffi::c_void,
                ) -> i32;
                fn GetModuleFileNameW(
                    hModule: *mut std::ffi::c_void,
                    lpFilename: *mut u16,
                    nSize: u32,
                ) -> u32;
            }

            const FROM_ADDRESS: u32 = 0x00000004;
            const UNCHANGED_REFCOUNT: u32 = 0x00000002;

            unsafe {
                let mut module = std::ptr::null_mut();
                if GetModuleHandleExW(
                    FROM_ADDRESS | UNCHANGED_REFCOUNT,
                    addr as *const u8,
                    &mut module,
                ) == 0
                {
                    return None;
                }
                let mut buf = vec![0u16; 1024];
                let len = GetModuleFileNameW(
                    module,
                    buf.as_mut_ptr(),
                    buf.len() as u32,
                );
                if len == 0 || len >= buf.len() as u32 {
                    return None;
                }
                let path =
                    PathBuf::from(OsString::from_wide(&buf[..len as usize]));
                path.parent().map(|p| p.to_path_buf())
            }
        }

        #[cfg(not(target_os = "windows"))]
        {
            #[repr(C)]
            #[derive(typed_builder::TypedBuilder)]
            struct DlInfo {
                dli_fname: *const std::os::raw::c_char,
                dli_fbase: *mut std::ffi::c_void,
                dli_sname: *const std::os::raw::c_char,
                dli_saddr: *mut std::ffi::c_void,
            }

            unsafe extern "C" {
                fn dladdr(
                    addr: *const std::ffi::c_void,
                    info: *mut DlInfo,
                ) -> i32;
            }

            unsafe {
                let mut info = DlInfo::builder()
                    .dli_fname(std::ptr::null())
                    .dli_fbase(std::ptr::null_mut())
                    .dli_sname(std::ptr::null())
                    .dli_saddr(std::ptr::null_mut())
                    .build();
                if dladdr(addr as *const std::ffi::c_void, &mut info) != 0
                    && !info.dli_fname.is_null()
                {
                    let cstr = std::ffi::CStr::from_ptr(info.dli_fname);
                    let path = PathBuf::from(cstr.to_string_lossy().as_ref());
                    return path.parent().map(|p| p.to_path_buf());
                }
                None
            }
        }
    }

    /// Search paths for the pdfium shared library, in priority order.
    pub(crate) fn search_paths() -> Vec<PathBuf> {
        let mut paths = Vec::new();
        let name = dylib_name();

        // 1. Runtime env var override (directory containing the shared library)
        if let Ok(dir) = std::env::var("PDFIUM_LIB_PATH") {
            paths.push(PathBuf::from(&dir).join(name));
        }

        // 2. Compile-time baked path from build.rs
        if !PDFIUM_LIB_DIR.is_empty() {
            let lib_dir = PathBuf::from(PDFIUM_LIB_DIR);
            paths.push(lib_dir.join(name));

            // On Windows, pdfium-binaries puts the DLL in bin/, not lib/
            #[cfg(target_os = "windows")]
            if let Some(parent) = lib_dir.parent() {
                paths.push(parent.join("bin").join(name));
            }
        }

        // 3. Next to the native extension (Python .pyd/.so, Node .node, etc.)
        //    Uses dladdr (Unix) / GetModuleHandleExW (Windows) to find our own module path.
        if let Some(dir) = self_dir() {
            paths.push(dir.join(name));
        }

        // 4. Next to the current executable
        if let Ok(exe) = std::env::current_exe()
            && let Some(exe_dir) = exe.parent()
        {
            paths.push(exe_dir.join(name));
        }

        // 5. Bare library name (system search paths / LD_LIBRARY_PATH / DYLD_LIBRARY_PATH / PATH)
        paths.push(PathBuf::from(name));

        paths
    }
}
#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
pub(crate) use native::{dylib_name, search_paths};
