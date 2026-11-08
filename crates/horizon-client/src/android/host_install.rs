//! Resource installation driven by the host launcher's own progress UI
//! (`com.mio.horizon.HorizonInstaller`). The host service runs in the engine's own
//! `:horizon` process but outside `android_main`, so this path initializes the small
//! pieces of engine state the installer needs (logging, download cache).
//!
//! Only call from the host's game process; the engine library never loads into the
//! launcher's own process.

use std::path::PathBuf;
use std::sync::OnceLock;

use jni::objects::{JClass, JObject, JString, JValue};
use jni::sys::{jboolean, jint, jlong, jstring};
use jni::JNIEnv;

use horizon_client::official_install::{self, InstallStage};

fn init_logging() {
    static LOGGER: OnceLock<android_logger::AndroidLogger> = OnceLock::new();
    let logger = LOGGER.get_or_init(|| {
        android_logger::AndroidLogger::new(
            android_logger::Config::default()
                .with_max_level(log::LevelFilter::Info)
                .with_tag("Horizon"),
        )
    });
    let _ = log::set_logger(logger);
    log::set_max_level(log::LevelFilter::Info);
}

fn stage_index(stage: InstallStage) -> jint {
    InstallStage::ALL
        .iter()
        .position(|candidate| *candidate == stage)
        .unwrap_or(0) as jint
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
    official_install::ready(&home) as jboolean
}

/// Downloads and prepares Minecraft's resources for the engine. Blocks the calling
/// thread; progress goes to `listener.onProgress(stageIndex, label, done, total)` on that
/// same thread. Returns `null` on success, otherwise a message for the player.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_mio_horizon_HorizonInstaller_install<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    home: JString<'local>,
    cache: JString<'local>,
    listener: JObject<'local>,
) -> jstring {
    init_logging();
    let (Some(home), Some(cache)) = (jni_path(&mut env, &home), jni_path(&mut env, &cache)) else {
        return error_string(&env, "The host passed no installation directory");
    };
    official_install::configure_download_cache_dir(cache);
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
    let result = official_install::install_with_progress(&home, |update| {
        let Ok(mut env) = vm.attach_current_thread() else {
            return;
        };
        let Ok(label) = env.new_string(update.stage.label()) else {
            return;
        };
        let label = JObject::from(label);
        let _ = env.call_method(
            listener.as_obj(),
            "onProgress",
            "(ILjava/lang/String;JJ)V",
            &[
                JValue::from(stage_index(update.stage)),
                JValue::Object(&label),
                JValue::from(update.done as jlong),
                JValue::from(update.total as jlong),
            ],
        );
    });
    match result {
        Ok(()) => std::ptr::null_mut(),
        Err(error) => error_string(&env, &error.to_string()),
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
