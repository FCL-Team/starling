//! Launch handoff from a host launcher (Fold Craft Launcher) that embeds the engine in
//! its own `:horizon` process. The host passes the launch context through environment
//! variables set before the engine library loads; the standalone app leaves them unset
//! and every path below falls back to Horizon's own behaviour.
//!
//! Env contract (all optional):
//! - `FCL_HORIZON_HOME` — absolute engine home (resources, engine settings). Present
//!   means embedded: the host owns the process/task lifecycle.
//! - `FCL_HORIZON_GAME_DIR` — the instance's game folder (saves, options.txt).
//! - `FCL_HORIZON_FILES_DIR` — the host's own Minecraft folder (the official launcher's
//!   layout: `versions/`, `libraries/`, `assets/`). The one-time preparation reads the
//!   official files there, so it needs no sign-in and no download.
//! - `FCL_HORIZON_JAVA_HOME` — a Java 25 runtime the host already installed (its `javac`
//!   included), for that preparation instead of fetching one.
//! - `FCL_HORIZON_SESSION` — absolute path of a 0600 session JSON written moments before
//!   launch; the engine reads and deletes it.
//! - `FCL_HORIZON_OFFLINE_NAME` — player name for offline launches without a session.
//! - `FCL_HORIZON_QUICK_PLAY_WORLD` / `_SERVER` — quick play target.
//! - `FCL_HORIZON_AUTOPLAY=1` — start the game without the host's own UI in between.
//! - `FCL_HORIZON_HOST_CONTROLS=1` — the host's controls drive the game; the engine's
//!   touch UI, keypad and system keyboard stay out of the way.
//! - `FCL_HORIZON_RENDERER` — `vulkan`, `vulkan-batching` or `opengl` for this launch,
//!   overriding the engine's saved rendering mode without saving it.
//! - `FCL_HORIZON_VULKAN_LIBRARY` — absolute path of a custom Vulkan driver library the
//!   host picked; `host_handoff` loads it before the first Vulkan instance and hands it to
//!   wgpu-hal through `WGPU_VULKAN_LOADER_HANDLE`.
//! - `FCL_HORIZON_LOCK_DIR` — private directory for world locks on shared storage;
//!   `save.rs` reads this name directly.
#![cfg_attr(not(target_os = "android"), allow(dead_code))]

use std::path::PathBuf;
use std::sync::OnceLock;

/// The rendering mode a host asked for this launch. The engine maps these onto its own
/// renderer choice: `Vulkan` here is the plain compatibility Vulkan, `VulkanBatching` the
/// engine's own default.
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
    /// The host's own Minecraft folder (the official launcher's layout), used in place for the
    /// official game files the one-time preparation reads.
    files_home: Option<PathBuf>,
    /// A Java runtime the host already installed (OpenJDK 25, `javac` included), instead of
    /// downloading one for the preparation.
    java_home: Option<PathBuf>,
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
            var(key)
                .flatten()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        };
        let path = |key: &str| -> Option<PathBuf> {
            var(key)
                .flatten()
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
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
            files_home: path("FCL_HORIZON_FILES_DIR"),
            java_home: path("FCL_HORIZON_JAVA_HOME"),
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
    pub fn game_home(&self) -> Option<&PathBuf> {
        self.game_home.as_ref()
    }

    /// The player name for an offline launch, or `None` to keep the launcher's own name.
    pub fn offline_name(&self) -> Option<String> {
        self.offline_name.clone()
    }

    /// The host's own Minecraft folder (the official launcher's layout: `versions/`,
    /// `libraries/`, `assets/`), when it launched from one of its game folders: the preparation
    /// reads the official files there instead of downloading them, and needs no sign-in.
    pub fn files_home(&self) -> Option<&PathBuf> {
        self.files_home.as_ref()
    }

    /// A Java runtime the host already installed for the one-time preparation.
    pub fn java_home(&self) -> Option<&PathBuf> {
        self.java_home.as_ref()
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

// --- Reporting back to the host (queried over JNI by its controls overlay) ---

static RENDERER_NOTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static RENDERER_GLES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static RENDERER_BATCHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static RENDERER_RECOVERED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static CURSOR_SHAPE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static FPS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static CAPTURE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

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

/// The game grabbed or released the pointer for gameplay.
pub fn note_capture(capture: bool) {
    CAPTURE.store(capture, std::sync::atomic::Ordering::Release);
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
