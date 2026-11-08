//! Launch handoff from a host launcher (Fold Craft Launcher) that embeds the engine in
//! its own `:horizon` process. The host passes the launch context through environment
//! variables set before the engine library loads; the standalone app leaves them unset
//! and every path below falls back to Horizon's own behaviour.
//!
//! Env contract (all optional):
//! - `FCL_HORIZON_HOME` — absolute engine home (resources, engine settings). Present
//!   means embedded: the host owns the process/task lifecycle. `game.rs` (exit path) and
//!   `save.rs` (world locks) read this name directly as well.
//! - `FCL_HORIZON_GAME_DIR` — the instance's game folder (saves, options.txt).
//! - `FCL_HORIZON_SESSION` — absolute path of a 0600 session JSON written moments before
//!   launch; the engine reads and deletes it (`auth::AuthSession::load`).
//! - `FCL_HORIZON_OFFLINE_NAME` — player name for offline launches without a session.
//! - `FCL_HORIZON_QUICK_PLAY_WORLD` / `_SERVER` — quick play target.
//! - `FCL_HORIZON_AUTOPLAY=1` — skip the launcher UI, go straight into the game.
//! - `FCL_HORIZON_HOST_CONTROLS=1` — the host's controls drive the game; the engine's
//!   touch UI, keypad and system keyboard stay out of the way.
//! - `FCL_HORIZON_RENDERER` — `vulkan`, `vulkan-batching` or `opengl` for this launch,
//!   overriding the engine's saved rendering mode without saving it.
//! - `FCL_HORIZON_VULKAN_LIBRARY` — absolute path of a custom Vulkan driver library the
//!   host picked; loaded via dlopen before the first Vulkan instance and handed to
//!   wgpu-hal through `WGPU_VULKAN_LOADER_HANDLE`.
//! - `FCL_HORIZON_LOCK_DIR` — private directory for world locks on shared storage;
//!   `save.rs` reads this name directly.

use std::path::PathBuf;
use std::sync::OnceLock;

/// The rendering mode a host asked for this launch. The engine maps these onto its own
/// `RendererMode`: `Vulkan` here is GPU batching, plain compatibility Vulkan is the default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostRenderer {
    Vulkan,
    VulkanBatching,
    OpenGl,
}

/// The launch context, read once from the environment.
pub struct HostHandoff {
    pub renderer: Option<HostRenderer>,
    pub autoplay: bool,
    pub host_controls: bool,
    engine_home: Option<PathBuf>,
    game_home: Option<PathBuf>,
    offline_name: Option<String>,
}

impl HostHandoff {
    fn from_env() -> Self {
        let var = |key: &str| std::env::var_os(key).map(|value| value.into_string().ok());
        let renderer = var("FCL_HORIZON_RENDERER").flatten().and_then(|value| {
            match value.trim().to_ascii_lowercase().as_str() {
                "vulkan" => Some(HostRenderer::Vulkan),
                "vulkan-batching" => Some(HostRenderer::VulkanBatching),
                "opengl" => Some(HostRenderer::OpenGl),
                _ => {
                    log::warn!("Host renderer choice unknown: {value}");
                    None
                }
            }
        });
        let text = |key: &str| -> Option<String> {
            var(key).flatten().map(|value| value.trim().to_owned()).filter(|value| !value.is_empty())
        };
        let path = |key: &str| -> Option<PathBuf> {
            var(key).flatten().map(PathBuf::from).filter(|path| path.is_absolute())
        };
        Self {
            renderer,
            autoplay: var("FCL_HORIZON_AUTOPLAY")
                .flatten()
                .is_some_and(|value| value.trim() == "1"),
            host_controls: var("FCL_HORIZON_HOST_CONTROLS")
                .flatten()
                .is_some_and(|value| value.trim() == "1"),
            engine_home: path("FCL_HORIZON_HOME"),
            game_home: path("FCL_HORIZON_GAME_DIR"),
            offline_name: text("FCL_HORIZON_OFFLINE_NAME"),
        }
    }

    /// The engine home (resources, engine settings, diagnostics). `default` is Android's
    /// own data directory for standalone runs; `None` here means no directory at all.
    pub fn engine_home(&self, default: Option<PathBuf>) -> Option<PathBuf> {
        self.engine_home.clone().or(default)
    }

    /// True when a host launcher embeds the engine in its own process/task.
    pub fn embedded(&self) -> bool {
        self.engine_home.is_some()
    }

    /// The game folder of the next launch: the host's instance, or the launcher's own.
    pub fn game_home(&self, fallback: impl FnOnce() -> PathBuf) -> PathBuf {
        self.game_home.clone().unwrap_or_else(fallback)
    }

    /// The player name for an offline launch, or `None` to keep the launcher's own name.
    pub fn offline_name(&self) -> Option<String> {
        self.offline_name.clone()
    }

    /// Quick play arguments for this launch, in the engine's own launch-argument format.
    pub fn quick_play_arguments(&self) -> Vec<String> {
        let world = std::env::var_os("FCL_HORIZON_QUICK_PLAY_WORLD")
            .map(|value| value.into_string().ok())
            .flatten()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        let server = std::env::var_os("FCL_HORIZON_QUICK_PLAY_SERVER")
            .map(|value| value.into_string().ok())
            .flatten()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        if let Some(world) = world {
            vec!["--quickPlaySingleplayer".into(), world]
        } else if let Some(server) = server {
            vec!["--server".into(), server]
        } else {
            Vec::new()
        }
    }

    /// The documents provider authority the engine may open from its own process.
    pub fn documents_authority(&self, package: &str) -> String {
        if self.embedded() {
            format!("{package}.horizon.documents")
        } else {
            format!("{package}.documents")
        }
    }
}

static HANDOFF: OnceLock<HostHandoff> = OnceLock::new();

/// The launch context, read from the environment on first use.
pub fn get() -> &'static HostHandoff {
    HANDOFF.get_or_init(HostHandoff::from_env)
}

/// The host's session file, if any. Consumed once: the engine reads and deletes it, and
/// the variable is unset so a later launch cannot reuse a stale path.
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

/// True when the game just returned to a host launcher: the event loop ends so the world
/// saves and `android_main` finishes the host's activity and process.
pub fn return_to_host() -> bool {
    get().embedded()
}

// --- Reporting back to the host (queried over JNI by its controls overlay) ---

static RENDERER_NOTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static RENDERER_GLES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static RENDERER_BATCHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static RENDERER_RECOVERED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static CURSOR_SHAPE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static FPS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static CAPTURE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// 0 the system Vulkan driver, 1 the chosen driver, 2 the chosen driver failed to load.
static VULKAN_DRIVER_STATE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// The rendering path chosen for this launch (configuration, before the first frame).
pub fn note_renderer(opengl: bool, batching: bool, recovered: bool) {
    RENDERER_NOTED.store(true, std::sync::atomic::Ordering::Release);
    RENDERER_GLES.store(opengl, std::sync::atomic::Ordering::Release);
    RENDERER_BATCHING.store(batching, std::sync::atomic::Ordering::Release);
    RENDERER_RECOVERED.store(recovered, std::sync::atomic::Ordering::Release);
}

/// The menu cursor the game's widgets want, as SDL system cursor ids.
pub fn note_cursor(shape: i32) {
    CURSOR_SHAPE.store(shape.max(0) as u32, std::sync::atomic::Ordering::Release);
}

/// The game's frames per second over the last second.
pub fn note_fps(fps: f32) {
    FPS.store(fps.max(0.0) as u32, std::sync::atomic::Ordering::Release);
}

/// The game grabbed or released the pointer for gameplay. Mirrored into winit's host
/// input so relative motion is routed as camera, not menu cursor, while grabbed.
pub fn note_capture(capture: bool) {
    CAPTURE.store(capture, std::sync::atomic::Ordering::Release);
    if get().host_controls {
        winit::platform::android::host_input::set_pointer_grabbed(capture);
    }
}

/// 0 not started, 1 Vulkan, 2 Vulkan with GPU batching, 3 OpenGL ES, 4 OpenGL ES recovery.
pub fn renderer_state() -> i32 {
    if !RENDERER_NOTED.load(std::sync::atomic::Ordering::Acquire) {
        return 0;
    }
    if RENDERER_RECOVERED.load(std::sync::atomic::Ordering::Acquire) {
        4
    } else if RENDERER_GLES.load(std::sync::atomic::Ordering::Acquire) {
        3
    } else if RENDERER_BATCHING.load(std::sync::atomic::Ordering::Acquire) {
        2
    } else {
        1
    }
}

pub fn cursor_shape() -> i32 {
    CURSOR_SHAPE.load(std::sync::atomic::Ordering::Acquire) as i32
}

pub fn fps() -> i32 {
    FPS.load(std::sync::atomic::Ordering::Acquire) as i32
}

pub fn grabbed() -> bool {
    CAPTURE.load(std::sync::atomic::Ordering::Acquire)
}

pub fn vulkan_driver_state() -> i32 {
    VULKAN_DRIVER_STATE.load(std::sync::atomic::Ordering::Acquire) as i32
}

/// Prepares a host-chosen Vulkan driver before the first Vulkan instance exists. Without
/// a linker-namespace loader, loading a driver outside the APK can fail on Android; the
/// host then shows the failure and the engine stays on the system driver.
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
fn load_vulkan_library(path: &std::path::Path) -> Option<*mut core::ffi::c_void> {
    use std::ffi::{CStr, CString};
    let name =
        CString::new(path.to_string_lossy().as_bytes()).ok()?;
    unsafe extern "C" {
        fn dlopen(filename: *const core::ffi::c_char, flag: i32) -> *mut core::ffi::c_void;
        fn dlsym(handle: *mut core::ffi::c_void, symbol: *const core::ffi::c_char)
            -> *mut core::ffi::c_void;
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
