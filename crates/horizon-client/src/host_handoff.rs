//! The host launch context (`host_env`) plus what only the game side does with it: the
//! session file's one-time read, the host's Vulkan driver loaded before the first
//! instance, the grab state mirrored into winit's host input, and the driver state the
//! host queries.
#![cfg_attr(not(target_os = "android"), allow(dead_code))]

pub use crate::host_env::*;

#[cfg(target_os = "android")]
use std::path::PathBuf;

/// 0 the system Vulkan driver, 1 the chosen driver, 2 the chosen driver failed to load.
static VULKAN_DRIVER_STATE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub fn vulkan_driver_state() -> i32 {
    VULKAN_DRIVER_STATE.load(std::sync::atomic::Ordering::Acquire) as i32
}

/// The host's session file, if any. Consumed once: the engine reads and deletes it, and
/// the variable is unset so a later launch cannot reuse a stale path.
#[cfg(target_os = "android")]
pub fn take_session() -> Option<PathBuf> {
    let path = std::env::var_os("FCL_HORIZON_SESSION")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute());
    if path.is_some() {
        // Safety: env mutation before/around process handoff races nothing else here; the
        // game thread is the only reader of this variable.
        unsafe { std::env::remove_var("FCL_HORIZON_SESSION") };
    }
    path
}

/// The game grabbed or released the pointer for gameplay. Mirrored into winit's host
/// input so relative motion is routed as camera, not menu cursor, while grabbed.
pub fn note_capture(capture: bool) {
    crate::host_env::note_capture(capture);
    #[cfg(target_os = "android")]
    if get().host_controls {
        winit::platform::android::host_input::set_pointer_grabbed(capture);
    }
}

/// Prepares a host-chosen Vulkan driver before the first Vulkan instance exists. Without
/// a linker-namespace loader, loading a driver outside the APK can fail on Android; the
/// host then shows the failure and the engine stays on the system driver.
#[cfg(target_os = "android")]
pub fn prepare_vulkan_driver() {
    let library = std::env::var_os("FCL_HORIZON_VULKAN_LIBRARY")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute());
    let Some(library) = library else {
        return;
    };
    let loaded = load_vulkan_library(&library);
    if let Some(handle) = loaded {
        // wgpu-hal's android_host_loader takes the dlopen handle as hex and resolves
        // vkGetInstanceProcAddr from it; the handle stays valid in this process.
        let handle = format!("{:#x}", handle as usize);
        // Safety: single-threaded launch setup, before any Vulkan instance exists.
        unsafe { std::env::set_var("WGPU_VULKAN_LOADER_HANDLE", handle) };
        VULKAN_DRIVER_STATE.store(1, std::sync::atomic::Ordering::Release);
        log::info!("Host Vulkan driver loaded: {}", library.display());
    } else {
        VULKAN_DRIVER_STATE.store(2, std::sync::atomic::Ordering::Release);
        log::warn!("Host Vulkan driver failed to load: {}", library.display());
    }
}

/// dlopens the driver and confirms it exports `vkGetInstanceProcAddr`.
#[cfg(target_os = "android")]
fn load_vulkan_library(path: &std::path::Path) -> Option<*mut core::ffi::c_void> {
    use std::ffi::{CStr, CString};
    let name = CString::new(path.to_string_lossy().as_bytes()).ok()?;
    unsafe extern "C" {
        fn dlopen(filename: *const core::ffi::c_char, flag: i32) -> *mut core::ffi::c_void;
        fn dlsym(
            handle: *mut core::ffi::c_void,
            symbol: *const core::ffi::c_char,
        ) -> *mut core::ffi::c_void;
        fn dlclose(handle: *mut core::ffi::c_void) -> i32;
        fn dlerror() -> *const core::ffi::c_char;
    }
    const RTLD_NOW: i32 = 2;
    const RTLD_LOCAL: i32 = 0;
    unsafe {
        // RTLD_LOCAL keeps the driver's symbols out of the global namespace.
        let handle = dlopen(name.as_ptr(), RTLD_NOW | RTLD_LOCAL);
        if handle.is_null() {
            let error = dlerror();
            let message = if error.is_null() {
                String::new()
            } else {
                CStr::from_ptr(error).to_string_lossy().into_owned()
            };
            log::warn!("dlopen {}: {message}", path.display());
            return None;
        }
        let symbol = CStr::from_bytes_with_nul(b"vkGetInstanceProcAddr\0")
            .unwrap_unchecked()
            .as_ptr();
        let entry = dlsym(handle, symbol);
        if entry.is_null() {
            log::warn!("{} has no vkGetInstanceProcAddr", path.display());
            dlclose(handle);
            return None;
        }
        Some(handle)
    }
}
