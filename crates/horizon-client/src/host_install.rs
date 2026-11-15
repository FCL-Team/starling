//! The host launcher's own progress UI (`com.mio.horizon.HorizonInstaller`): checks the
//! engine's prepared game files and prepares them from the account session the host
//! passed, in the engine's own `:horizon` process but outside `android_main`. This path
//! initializes the small pieces of engine state the installer needs (logging).
//!
//! Only call from the host's game process; the engine library never loads into the
//! launcher's own process.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use jni::objects::{JClass, JObject, JString, JValue};
use jni::sys::{jboolean, jint, jlong, jstring};
use jni::JNIEnv;

use crate::preparer::{self, Stage};

/// The tag of the engine's console lines in logcat (`android::logcat`).
struct LogcatLogger;

impl log::Log for LogcatLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record) {
        crate::android::logcat(record.level(), &format!("{}", record.args()));
    }

    fn flush(&self) {}
}

fn init_logging() {
    static LOGGER: OnceLock<LogcatLogger> = OnceLock::new();
    let logger = LOGGER.get_or_init(|| LogcatLogger);
    let _ = log::set_logger(logger);
    log::set_max_level(log::LevelFilter::Info);
}

/// Where the game reads its prepared files from: `<home>/resources`.
fn resources_dir(home: &Path) -> PathBuf {
    home.join("resources")
}

/// True when the engine home already holds a complete installation.
fn ready(home: &Path) -> bool {
    let resources = resources_dir(home);
    let manifest = resources.join("horizon-resources.json");
    let size = std::fs::metadata(&manifest).map(|data| data.len()).unwrap_or(0);
    size > 0 && resources.join("assets").is_dir()
}

/// The progress stage of a preparation, in the order the host's UI shows them.
fn stage_index(stage: &Stage) -> jint {
    match stage {
        Stage::Idle | Stage::Java(..) => 0,
        Stage::Unpacking => 1,
        Stage::Preparing(_) => 2,
        Stage::Done | Stage::Failed(_) => 3,
    }
}

/// True when the engine home already holds a complete installation.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_mio_horizon_HorizonInstaller_isReady<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    home: JString<'local>,
) -> jboolean {
    init_logging();
    let Some(home) = jni_path(&mut env, &home) else {
        return false as jboolean;
    };
    ready(&home) as jboolean
}

/// Downloads and prepares Minecraft's resources for the engine. Blocks the calling
/// thread; progress goes to `listener.onProgress(stageIndex, label, done, total)` on that
/// same thread. Returns `null` on success, otherwise a message for the player.
///
/// `cache` is accepted for the host's contract; Starling keeps the download in its own
/// private folder, so the value is not used.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_mio_horizon_HorizonInstaller_install<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    home: JString<'local>,
    cache: JString<'local>,
    listener: JObject<'local>,
) -> jstring {
    init_logging();
    let (Some(home), Some(_cache)) = (jni_path(&mut env, &home), jni_path(&mut env, &cache)) else {
        return error_string(&env, "The host passed no installation directory");
    };
    // The engine's own folders (logs, the preparer's work files): a host installer runs
    // before the game's start-up sets these, and nothing else of ours runs yet.
    if std::env::var_os("HORIZON_PRIVATE").is_none() {
        // SAFETY: no other thread of ours runs yet.
        unsafe { std::env::set_var("HORIZON_PRIVATE", &home) };
    }
    if std::env::var_os("HORIZON_HOME").is_none() {
        // SAFETY: as above.
        unsafe { std::env::set_var("HORIZON_HOME", &home) };
    }
    // The progress closure must be `Sync`, so it carries the VM and a global reference
    // rather than the borrowed JNIEnv, and attaches on each callback.
    let vm = match env.get_java_vm() {
        Ok(vm) => vm,
        Err(_) => return error_string(&env, "Cannot reach the host JVM"),
    };
    let listener = match env.new_global_ref(&listener) {
        Ok(reference) => reference,
        Err(_) => return error_string(&env, "Cannot attach the progress listener"),
    };
    let report = |stage: &Stage| {
        let Ok(mut env) = vm.attach_current_thread() else {
            return;
        };
        let label = match stage {
            Stage::Java(..) => "Java runtime".to_owned(),
            Stage::Unpacking => "Unpacking".to_owned(),
            Stage::Preparing(line) => line.clone(),
            Stage::Done => "Done".to_owned(),
            Stage::Failed(error) => error.clone(),
            Stage::Idle => "Starting".to_owned(),
        };
        let Ok(label) = env.new_string(label) else {
            return;
        };
        let label = JObject::from(label);
        let (done, total) = match stage {
            Stage::Java(done, total) => (*done as jlong, *total as jlong),
            _ => (0, 0),
        };
        let _ = env.call_method(
            listener.as_obj(),
            "onProgress",
            "(ILjava/lang/String;JJ)V",
            &[
                JValue::from(stage_index(stage)),
                JValue::Object(&label),
                JValue::from(done),
                JValue::from(total),
            ],
        );
    };
    let resources = resources_dir(&home);
    preparer::start(resources);
    let mut last = Stage::Idle;
    loop {
        let stage = preparer::stage();
        if stage != last {
            report(&stage);
            match &stage {
                Stage::Done => return std::ptr::null_mut(),
                Stage::Failed(error) => return error_string(&env, error),
                _ => {}
            }
            last = stage;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn jni_path<'local>(env: &mut JNIEnv<'local>, value: &JString<'local>) -> Option<PathBuf> {
    let string = env.get_string(value).ok()?;
    let trimmed = string.to_string_lossy().trim().to_owned();
    (!trimmed.is_empty()).then_some(PathBuf::from(trimmed))
}

fn error_string<'local>(env: &JNIEnv<'local>, message: &str) -> jstring {
    env.new_string(message)
        .map(|string| string.into_raw())
        .unwrap_or(std::ptr::null_mut())
}
