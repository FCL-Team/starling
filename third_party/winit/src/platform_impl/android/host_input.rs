//! Keyboard and mouse input pushed by a host app that embeds the engine in its own
//! Android views (touch controls, virtual mouse, controller mapper). Events are queued
//! here and drained by the Android event loop as ordinary window events; the standalone
//! app never enables this path.
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};

use android_activity::AndroidAppWaker;
use crate::keyboard::{Key, KeyCode, NamedKey, SmolStr};

/// Reserved device id for host-pushed input; Android device ids are never negative.
pub const DEVICE_ID: i32 = -2;

/// One host input event, expressed in window pixels and winit key codes.
#[derive(Debug, Clone)]
pub enum HostInput {
    /// Relative motion in pixels (camera look, or a captured menu cursor).
    MouseMotion { dx: f64, dy: f64 },
    /// Absolute pointer position in window pixels.
    CursorMoved { x: f64, y: f64 },
    /// GLFW button order: 0 left, 1 right, 2 middle, 3 back, 4 forward.
    MouseButton { button: u8, pressed: bool },
    /// Scroll lines.
    Wheel { x: f32, y: f32 },
    /// Committed text (chat and other text fields).
    Text(String),
    /// A physical key press or release, with the character it produces on press.
    Key {
        code: KeyCode,
        pressed: bool,
        text: Option<String>,
    },
}

static ENABLED: AtomicBool = AtomicBool::new(false);
static GAMEPADS_TO_HOST: AtomicBool = AtomicBool::new(false);
static POINTER_GRABBED: AtomicBool = AtomicBool::new(false);
static MENU_CAPTURE: AtomicBool = AtomicBool::new(false);
static CURSOR: Mutex<(f64, f64)> = Mutex::new((0.0, 0.0));
static MENU_CURSOR: Mutex<(f64, f64)> = Mutex::new((0.0, 0.0));
static QUEUE: Mutex<VecDeque<HostInput>> = Mutex::new(VecDeque::new());
static WAKER: Mutex<Option<AndroidAppWaker>> = Mutex::new(None);

fn lock<T>(guard: &Mutex<T>) -> MutexGuard<'_, T> {
    // A poisoned lock only means a push panicked mid-event; the queue contents stay valid.
    guard.lock().unwrap_or_else(|error| error.into_inner())
}

fn wake() {
    if let Some(waker) = lock(&WAKER).as_ref() {
        waker.wake();
    }
}

/// Enables host input. The host owns controller translation as well: gamepad and pure
/// DPAD events are left unhandled so Android delivers them to the host activity's views.
pub fn enable(enabled: bool) {
    ENABLED.store(enabled, Ordering::Release);
    GAMEPADS_TO_HOST.store(enabled, Ordering::Release);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Acquire)
}

pub fn gamepads_to_host() -> bool {
    GAMEPADS_TO_HOST.load(Ordering::Acquire)
}

pub fn set_waker(waker: AndroidAppWaker) {
    *lock(&WAKER) = Some(waker);
}

/// The engine grabbed (gameplay camera) or released (menus) the pointer. A grab cancels
/// menu capture, whose relative motion must never fight the camera.
pub fn set_pointer_grabbed(grabbed: bool) {
    POINTER_GRABBED.store(grabbed, Ordering::Release);
    if grabbed {
        MENU_CAPTURE.store(false, Ordering::Release);
    }
}

/// The host's own mouse capture while a menu or text field is open: relative motion
/// moves a virtual menu cursor instead of reaching the game as camera input.
pub fn set_menu_capture(captured: bool) {
    MENU_CAPTURE.store(captured, Ordering::Release);
}

/// Moves the virtual menu cursor baseline without emitting an event.
pub fn menu_cursor(x: f64, y: f64) {
    *lock(&MENU_CURSOR) = (x, y);
}

/// Moves the pointer baseline without emitting an event (the host recentered its own
/// cursor; the next absolute position must not be read as motion).
pub fn set_cursor(x: f64, y: f64) {
    *lock(&CURSOR) = (x, y);
}

/// Real-device relative motion while a menu is captured: move the virtual menu cursor,
/// clamped to the window, and report its new position. `None` keeps the standalone
/// behaviour where relative motion reaches the game as camera input.
pub fn menu_cursor_motion(
    dx: f64,
    dy: f64,
    size: (f64, f64),
) -> Option<(f64, f64)> {
    if !MENU_CAPTURE.load(Ordering::Acquire) || POINTER_GRABBED.load(Ordering::Acquire) {
        return None;
    }
    let mut cursor = lock(&MENU_CURSOR);
    cursor.0 = (cursor.0 + dx).clamp(0.0, (size.0 - 1.0).max(0.0));
    cursor.1 = (cursor.1 + dy).clamp(0.0, (size.1 - 1.0).max(0.0));
    Some(*cursor)
}

/// Queues one host event for the event loop and wakes it.
pub fn push(input: HostInput) {
    if !enabled() {
        return;
    }
    if let HostInput::CursorMoved { x, y } = input {
        *lock(&CURSOR) = (x, y);
    }
    lock(&QUEUE).push_back(input);
    wake();
}

/// The queued events, in push order.
pub fn drain() -> VecDeque<HostInput> {
    std::mem::take(&mut lock(&QUEUE))
}

/// The logical key for a host-pushed physical key: named keys keep their names, printable
/// keys derive from the character the host reported, matching winit's Android mapping.
pub fn logical_key(code: KeyCode, text: Option<&str>) -> Key {
    let named = match code {
        KeyCode::Escape => Some(NamedKey::Escape),
        KeyCode::Enter | KeyCode::NumpadEnter => Some(NamedKey::Enter),
        KeyCode::Tab => Some(NamedKey::Tab),
        KeyCode::Backspace => Some(NamedKey::Backspace),
        KeyCode::Delete => Some(NamedKey::Delete),
        KeyCode::Insert => Some(NamedKey::Insert),
        KeyCode::Home => Some(NamedKey::Home),
        KeyCode::End => Some(NamedKey::End),
        KeyCode::PageUp => Some(NamedKey::PageUp),
        KeyCode::PageDown => Some(NamedKey::PageDown),
        KeyCode::ArrowUp => Some(NamedKey::ArrowUp),
        KeyCode::ArrowDown => Some(NamedKey::ArrowDown),
        KeyCode::ArrowLeft => Some(NamedKey::ArrowLeft),
        KeyCode::ArrowRight => Some(NamedKey::ArrowRight),
        KeyCode::CapsLock => Some(NamedKey::CapsLock),
        KeyCode::ScrollLock => Some(NamedKey::ScrollLock),
        KeyCode::NumLock => Some(NamedKey::NumLock),
        KeyCode::PrintScreen => Some(NamedKey::PrintScreen),
        KeyCode::Pause => Some(NamedKey::Pause),
        KeyCode::ContextMenu => Some(NamedKey::ContextMenu),
        KeyCode::Space => Some(NamedKey::Space),
        KeyCode::F1 => Some(NamedKey::F1),
        KeyCode::F2 => Some(NamedKey::F2),
        KeyCode::F3 => Some(NamedKey::F3),
        KeyCode::F4 => Some(NamedKey::F4),
        KeyCode::F5 => Some(NamedKey::F5),
        KeyCode::F6 => Some(NamedKey::F6),
        KeyCode::F7 => Some(NamedKey::F7),
        KeyCode::F8 => Some(NamedKey::F8),
        KeyCode::F9 => Some(NamedKey::F9),
        KeyCode::F10 => Some(NamedKey::F10),
        KeyCode::F11 => Some(NamedKey::F11),
        KeyCode::F12 => Some(NamedKey::F12),
        _ => None,
    };
    if let Some(key) = named {
        return Key::Named(key);
    }
    if let Some(text) = text {
        if !text.is_empty() && !text.chars().any(char::is_control) {
            return Key::Character(SmolStr::new(text));
        }
    }
    // Printable single-character codes without host text: letters, digits, punctuation.
    let derived = match code {
        KeyCode::KeyA => Some('a'),
        KeyCode::KeyB => Some('b'),
        KeyCode::KeyC => Some('c'),
        KeyCode::KeyD => Some('d'),
        KeyCode::KeyE => Some('e'),
        KeyCode::KeyF => Some('f'),
        KeyCode::KeyG => Some('g'),
        KeyCode::KeyH => Some('h'),
        KeyCode::KeyI => Some('i'),
        KeyCode::KeyJ => Some('j'),
        KeyCode::KeyK => Some('k'),
        KeyCode::KeyL => Some('l'),
        KeyCode::KeyM => Some('m'),
        KeyCode::KeyN => Some('n'),
        KeyCode::KeyO => Some('o'),
        KeyCode::KeyP => Some('p'),
        KeyCode::KeyQ => Some('q'),
        KeyCode::KeyR => Some('r'),
        KeyCode::KeyS => Some('s'),
        KeyCode::KeyT => Some('t'),
        KeyCode::KeyU => Some('u'),
        KeyCode::KeyV => Some('v'),
        KeyCode::KeyW => Some('w'),
        KeyCode::KeyX => Some('x'),
        KeyCode::KeyY => Some('y'),
        KeyCode::KeyZ => Some('z'),
        KeyCode::Digit0 => Some('0'),
        KeyCode::Digit1 => Some('1'),
        KeyCode::Digit2 => Some('2'),
        KeyCode::Digit3 => Some('3'),
        KeyCode::Digit4 => Some('4'),
        KeyCode::Digit5 => Some('5'),
        KeyCode::Digit6 => Some('6'),
        KeyCode::Digit7 => Some('7'),
        KeyCode::Digit8 => Some('8'),
        KeyCode::Digit9 => Some('9'),
        _ => None,
    };
    if let Some(character) = derived {
        return Key::Character(SmolStr::from_iter([character]));
    }
    Key::Unidentified(crate::keyboard::NativeKey::Unidentified)
}
