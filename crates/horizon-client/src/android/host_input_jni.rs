//! The host launcher's input, arriving as ordinary JNI calls from its controls overlay
//! (`com.mio.horizon.HorizonInput`). Events go straight into winit's host input queue;
//! queries read the engine state the host reports in its overlay.
//!
//! Call only from the host's game process, after the engine library has loaded.

use jni::objects::{JClass, JString};
use jni::sys::{jboolean, jfloat, jint, jstring};
use jni::JNIEnv;
use std::sync::Mutex;

use winit::keyboard::KeyCode;
use winit::platform::android::host_input::{self, HostInput};

/// The last pointer position the host reported, the baseline of each relative step.
static HOST_CURSOR: Mutex<(f64, f64)> = Mutex::new((0.0, 0.0));

/// Records the new baseline and returns the previous one.
fn baseline(position: &(f64, f64)) -> (f64, f64) {
    let mut guard = HOST_CURSOR.lock().unwrap_or_else(|error| error.into_inner());
    let last = *guard;
    *guard = *position;
    last
}

/// A GLFW key code (glfw3.h), pressed or released. Text is delivered separately as
/// committed text, so the key carries no character.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_mio_horizon_HorizonInput_key<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    glfw_key: jint,
    pressed: jboolean,
) {
    let Some(code) = glfw_to_winit(glfw_key) else {
        return;
    };
    host_input::push(HostInput::Key {
        code,
        pressed: pressed != 0,
        text: None,
    });
}

/// Committed text for chat and other text fields.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_mio_horizon_HorizonInput_text<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    text: JString<'local>,
) {
    let Ok(text) = env.get_string(&text) else {
        return;
    };
    let text = text.to_string_lossy().into_owned();
    if !text.is_empty() {
        host_input::push(HostInput::Text(text));
    }
}

/// The pointer at an absolute position, in Android window pixels. While the game holds
/// the pointer for gameplay, the position stream turns into relative motion: the engine's
/// captured camera consumes `DeviceEvent::MouseMotion` and ignores absolute cursor moves.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_mio_horizon_HorizonInput_cursor<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    x: jfloat,
    y: jfloat,
) {
    let position = (f64::from(x), f64::from(y));
    let (last_x, last_y) = baseline(&position);
    if crate::host_handoff::grabbed() {
        host_input::push(HostInput::MouseMotion {
            dx: position.0 - last_x,
            dy: position.1 - last_y,
        });
    } else {
        // The presentation lifts the frame while the keyboard is open; the
        // pointer rides the same shift to land on what the player sees.
        let y = position.1 + f64::from(horizon_client::gfx::android_ime_lift());
        host_input::push(HostInput::CursorMoved {
            x: position.0,
            y,
        });
    }
}

/// Moves the pointer baseline without generating motion.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_mio_horizon_HorizonInput_cursorSilently<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    x: jfloat,
    y: jfloat,
) {
    baseline(&(f64::from(x), f64::from(y)));
    host_input::set_cursor(f64::from(x), f64::from(y));
}

/// A GLFW mouse button (0 left, 1 right, 2 middle), pressed or released.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_mio_horizon_HorizonInput_mouseButton<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    button: jint,
    pressed: jboolean,
) {
    if (0..=u8::MAX as jint).contains(&button) {
        host_input::push(HostInput::MouseButton {
            button: button as u8,
            pressed: pressed != 0,
        });
    }
}

/// Wheel lines; positive y scrolls up.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_mio_horizon_HorizonInput_scroll<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    x: jfloat,
    y: jfloat,
) {
    host_input::push(HostInput::Wheel { x, y });
}

/// True while the game holds the pointer for gameplay.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_mio_horizon_HorizonInput_isGrabbed<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> jboolean {
    crate::host_handoff::grabbed() as jboolean
}

/// Lifts the finished frame by the IME height in the presentation pass, keeping the
/// render size and GUI scale untouched.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_mio_horizon_HorizonInput_setImeViewportBottomInset<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    inset: jint,
) {
    horizon_client::gfx::set_android_ime_viewport_bottom_inset(inset.max(0) as u32);
}

/// The game's frames per second over the last second; 0 when not known yet.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_mio_horizon_HorizonInput_fps<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> jint {
    crate::host_handoff::fps()
}

/// The cursor the game wants, as an SDL system cursor id (0 arrow, 10 not allowed, 11 hand).
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_mio_horizon_HorizonInput_cursorShape<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> jint {
    crate::host_handoff::cursor_shape()
}

/// GPU, backend and driver information reported by wgpu.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_mio_horizon_HorizonInput_graphicsInfo<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> jstring {
    let info = horizon_client::gfx::android_graphics_info();
    env.new_string(&info)
        .map(|string| string.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

/// 0 not started, 1 Vulkan, 2 Vulkan with GPU batching, 3 OpenGL ES, 4 OpenGL ES recovery.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_mio_horizon_HorizonInput_rendererState<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> jint {
    crate::host_handoff::renderer_state()
}

/// 0 the system Vulkan driver, 1 the chosen driver, 2 the chosen driver failed to load.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_mio_horizon_HorizonInput_vulkanDriverState<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> jint {
    crate::host_handoff::vulkan_driver_state()
}

/// glfw3.h key codes onto winit's physical key codes.
fn glfw_to_winit(code: i32) -> Option<KeyCode> {
    let key = match code {
        32 => KeyCode::Space,
        39 => KeyCode::Quote,
        44 => KeyCode::Comma,
        45 => KeyCode::Minus,
        46 => KeyCode::Period,
        47 => KeyCode::Slash,
        48 => KeyCode::Digit0,
        49 => KeyCode::Digit1,
        50 => KeyCode::Digit2,
        51 => KeyCode::Digit3,
        52 => KeyCode::Digit4,
        53 => KeyCode::Digit5,
        54 => KeyCode::Digit6,
        55 => KeyCode::Digit7,
        56 => KeyCode::Digit8,
        57 => KeyCode::Digit9,
        59 => KeyCode::Semicolon,
        61 => KeyCode::Equal,
        65 => KeyCode::KeyA,
        66 => KeyCode::KeyB,
        67 => KeyCode::KeyC,
        68 => KeyCode::KeyD,
        69 => KeyCode::KeyE,
        70 => KeyCode::KeyF,
        71 => KeyCode::KeyG,
        72 => KeyCode::KeyH,
        73 => KeyCode::KeyI,
        74 => KeyCode::KeyJ,
        75 => KeyCode::KeyK,
        76 => KeyCode::KeyL,
        77 => KeyCode::KeyM,
        78 => KeyCode::KeyN,
        79 => KeyCode::KeyO,
        80 => KeyCode::KeyP,
        81 => KeyCode::KeyQ,
        82 => KeyCode::KeyR,
        83 => KeyCode::KeyS,
        84 => KeyCode::KeyT,
        85 => KeyCode::KeyU,
        86 => KeyCode::KeyV,
        87 => KeyCode::KeyW,
        88 => KeyCode::KeyX,
        89 => KeyCode::KeyY,
        90 => KeyCode::KeyZ,
        91 => KeyCode::BracketLeft,
        92 => KeyCode::Backslash,
        93 => KeyCode::BracketRight,
        96 => KeyCode::Backquote,
        161 => return None, // GLFW World1（ISO 键盘专有键）不在 winit 键码表内
        162 => return None,
        256 => KeyCode::Escape,
        257 => KeyCode::Enter,
        258 => KeyCode::Tab,
        259 => KeyCode::Backspace,
        260 => KeyCode::Insert,
        261 => KeyCode::Delete,
        262 => KeyCode::ArrowRight,
        263 => KeyCode::ArrowLeft,
        264 => KeyCode::ArrowDown,
        265 => KeyCode::ArrowUp,
        266 => KeyCode::PageUp,
        267 => KeyCode::PageDown,
        268 => KeyCode::Home,
        269 => KeyCode::End,
        280 => KeyCode::CapsLock,
        281 => KeyCode::ScrollLock,
        282 => KeyCode::NumLock,
        283 => KeyCode::PrintScreen,
        284 => KeyCode::Pause,
        290..=313 => {
            // F1..F24 in GLFW order.
            let index = code - 290;
            let key = match index {
                0 => KeyCode::F1,
                1 => KeyCode::F2,
                2 => KeyCode::F3,
                3 => KeyCode::F4,
                4 => KeyCode::F5,
                5 => KeyCode::F6,
                6 => KeyCode::F7,
                7 => KeyCode::F8,
                8 => KeyCode::F9,
                9 => KeyCode::F10,
                10 => KeyCode::F11,
                11 => KeyCode::F12,
                12 => KeyCode::F13,
                13 => KeyCode::F14,
                14 => KeyCode::F15,
                15 => KeyCode::F16,
                16 => KeyCode::F17,
                17 => KeyCode::F18,
                18 => KeyCode::F19,
                19 => KeyCode::F20,
                20 => KeyCode::F21,
                21 => KeyCode::F22,
                22 => KeyCode::F23,
                _ => KeyCode::F24,
            };
            key
        }
        320 => KeyCode::Numpad0,
        321 => KeyCode::Numpad1,
        322 => KeyCode::Numpad2,
        323 => KeyCode::Numpad3,
        324 => KeyCode::Numpad4,
        325 => KeyCode::Numpad5,
        326 => KeyCode::Numpad6,
        327 => KeyCode::Numpad7,
        328 => KeyCode::Numpad8,
        329 => KeyCode::Numpad9,
        330 => KeyCode::NumpadDecimal,
        331 => KeyCode::NumpadDivide,
        332 => KeyCode::NumpadMultiply,
        333 => KeyCode::NumpadSubtract,
        334 => KeyCode::NumpadAdd,
        335 => KeyCode::NumpadEnter,
        336 => KeyCode::NumpadEqual,
        340 => KeyCode::ShiftLeft,
        341 => KeyCode::ControlLeft,
        342 => KeyCode::AltLeft,
        343 => KeyCode::SuperLeft,
        344 => KeyCode::ShiftRight,
        345 => KeyCode::ControlRight,
        346 => KeyCode::AltRight,
        347 => KeyCode::SuperRight,
        348 => KeyCode::ContextMenu,
        _ => return None,
    };
    Some(key)
}
