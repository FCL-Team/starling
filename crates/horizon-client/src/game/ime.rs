//! Input method (IME) text: committed composition goes to the focused text
//! field through the same sinks the keyboard and `horizon-ctl type` use.
//! The window only asks the OS for an input method while such a field has
//! focus, so movement keys keep arriving as plain key events.
use super::{Game, Screen, books::BookScreen};
use horizon_client::{
    hud::HudBatch,
    preedit::{self, PreeditEvent},
};
#[cfg(target_os = "android")]
use winit::keyboard::KeyCode;

impl Game<'_> {
    /// Whether a text box has focus right now (the fields `ime_commit` can fill).
    pub(super) fn text_field_focused(&mut self) -> bool {
        if self.dialog_open() {
            return self.dialog_has_text_field();
        }
        if let Some(title) = self.title.as_mut() {
            // A caret field, an append-only one (`focused_field`), or a game
            // rule's box (`has_caret_field` asks the rules screen itself, and
            // `title_edit` types into it).
            return title.has_caret_field() || title.focused_field().is_some();
        }
        if self.sign_edit.is_some() || matches!(self.book, Some(BookScreen::Edit { .. })) {
            return true;
        }
        let focused = match self.screen {
            Screen::Chat | Screen::CommandBlock | Screen::Social => true,
            Screen::DebugOptions => self
                .debug_options
                .as_ref()
                .is_some_and(|s| s.search_focused()),
            // The language list's search, the settings search and the pack
            // screen's search (OptionsUi::typing).
            Screen::Options => self.ui.options.typing(),
            Screen::WorldOptions => self.world_options_typing(),
            Screen::Creative => self.creative_screen.tab().search,
            Screen::BlockEditor => self.block_editor.as_ref().is_some_and(|s| s.text_focused()),
            Screen::Inventory => {
                self.mod_browser_typing() || self.recipe_search_focused() || self.station_typing()
            }
            _ => false,
        };
        focused || cfg!(target_os = "android") && self.widget_text_focused()
    }

    /// The focused text field's current value to pre-fill the host's keyboard with, so
    /// editing an existing value sends correct diffs.
    #[cfg(target_os = "android")]
    pub(super) fn host_keyboard_initial(&mut self) -> String {
        if let Some(title) = self.title.as_mut() {
            title.field_key(KeyCode::End, false, false);
            if let Some((_, value, _)) = title.focused_field() {
                return value.clone();
            }
        }
        String::new()
    }

    /// Allow the input method only while a text box has focus.
    pub(super) fn sync_ime(&mut self) {
        // Host-controls launches: the launcher owns the IME and the engine never has its own
        // editor. Focus alone never opens the keyboard there: a click inside the field does
        // (`host_text_field`), so a screen that focuses a box by itself stays quiet.
        #[cfg(target_os = "android")]
        if crate::host_handoff::get().host_controls {
            if self.host_keyboard && !self.text_field_focused() {
                self.host_keyboard = false;
                log::info!("Host keyboard: hide (the field lost the focus)");
                crate::android::hide_host_keyboard();
            }
            return;
        }
        // On Android asking for the input method shows the keyboard: there a tap on the field
        // decides (touch.rs), not its focus.
        if cfg!(target_os = "android") {
            return;
        }
        let want = self.text_field_focused();
        if want != self.ime_allowed
            && let Some(window) = &self.window
        {
            window.set_ime_allowed(want);
            self.ime_allowed = want;
            // The field lost the focus (or a new one took it): the composition
            // and the candidate window's place belong to the old one.
            self.ime_area = None;
            if !want {
                preedit::set_preedit(None);
            }
        }
    }

    /// `WindowEvent::Ime(Preedit)`: `KeyboardHandler.textEditing`. The text
    /// still being composed is shown under the focused field's caret.
    pub(super) fn ime_preedit(&mut self, text: &str, cursor: Option<(usize, usize)>) {
        preedit::set_preedit(
            self.text_field_focused()
                .then(|| PreeditEvent::from_winit(text, cursor))
                .flatten(),
        );
    }

    /// With the frame's GUI built: draw the composition over it and move the
    /// input method's candidate window to the caret
    /// (`TextInputManager.setTextInputArea`, in window pixels).
    pub(super) fn flush_ime(&mut self, hud: &mut HudBatch) {
        let field = preedit::take_focused_field();
        #[cfg(target_os = "android")]
        if crate::host_handoff::get().host_controls {
            self.host_text_field(hud, field);
        }
        self.touch_text_field(field);
        preedit::draw_overlay(hud, &self.gui.atlas);
        let Some([x0, y0, x1, y1]) = preedit::take_text_input_area() else {
            return;
        };
        let scale = hud.scale;
        let x = (x0 * scale).round() as i32;
        let y = (y0 * scale).round() as i32;
        let width = (((x1 - x0) * scale).round() as i32).max(1);
        let height = (((y1 - y0) * scale).round() as i32).max(1);
        let area = [x, y, width, height];
        if self.ime_area == Some(area) {
            return;
        }
        if let Some(window) = &self.window
            && self.ime_allowed
        {
            window.set_ime_cursor_area(
                winit::dpi::PhysicalPosition::new(x, y),
                winit::dpi::PhysicalSize::new(width as u32, height as u32),
            );
            self.ime_area = Some(area);
        }
    }

    /// Host-controls launches: publishes the focused field's bounds for the presentation
    /// pass, which moves the frame up just far enough to keep the field above the
    /// launcher's keyboard, and opens that keyboard only when the click that focused the
    /// field landed inside it (a screen focusing a box by itself stays quiet).
    #[cfg(target_os = "android")]
    fn host_text_field(&mut self, hud: &HudBatch, field: Option<[f32; 4]>) {
        let scale = hud.scale;
        crate::gfx::set_android_text_field(
            field.map(|[x, y, w, h]| [x * scale, y * scale, (x + w) * scale, (y + h) * scale]),
        );
        let Some((cx, cy)) = self.pending_host_click.take() else {
            return;
        };
        let Some([x, y, w, h]) = field else {
            return;
        };
        // 输入框边框外的少量容差,方便点边缘时也能弹出。
        let pad = 8.0;
        if cx < x - pad || cx > x + w + pad || cy < y - pad || cy > y + h + pad {
            return;
        }
        let initial = self.host_keyboard_initial();
        log::info!(
            "Host keyboard: show on click ({} chars)",
            initial.chars().count()
        );
        crate::android::request_host_keyboard(&initial);
        self.host_keyboard = true;
    }

    /// Text the input method committed (`WindowEvent::Ime(Commit)`).
    pub(super) fn ime_commit(&mut self, text: &str) {
        preedit::set_preedit(None);
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        // ScreenEvent.CharacterTyped.Pre: the input method's committed text
        // reaches a text box by characters, as a key's own text does, so the
        // open screen hears of each of them first and a cancelled one never
        // reaches the box.
        let Some(text) = self.screen_text_typed(&text) else {
            return;
        };
        if text.is_empty() || !self.text_field_focused() {
            return;
        }
        if self.dialog_type(&text) {
            return;
        }
        if self.title.is_some() {
            self.title_edit(Some(&text), false);
        } else if self.book.is_some() {
            self.book_input(None, Some(&text));
        } else if self.sign_edit.is_some() {
            self.sign_input(None, Some(&text));
        } else {
            match self.screen {
                Screen::Chat => self.chat_type(&text),
                Screen::CommandBlock => self.command_block_type(&text),
                Screen::Social => self.social_type(&text),
                Screen::DebugOptions => self.debug_options_type(&text),
                Screen::Options => self.ui.options.type_text(&self.gui.atlas, &text),
                Screen::WorldOptions => self.world_options_text(Some(&text)),
                Screen::Creative => self.creative_search_edit(Some(&text)),
                Screen::BlockEditor => self.block_editor_type(&text),
                Screen::Inventory if self.mod_browser_typing() => {
                    self.mod_browser_text(Some(&text), false)
                }
                Screen::Inventory if self.recipe_search_focused() => {
                    self.recipe_text(Some(&text), false)
                }
                Screen::Inventory => self.station_text(Some(&text), false),
                _ => {}
            }
        }
    }
}
