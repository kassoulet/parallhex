//! Key bindings, as a pure function.
//!
//! Mapping keys to actions separately from applying them is what lets every
//! binding be tested without a terminal — the same discipline that keeps the
//! geometry testable without a window.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::core::color::Colormap;
use crate::core::geom::Nav;
use crate::tui::app::{Action, Focus};

/// Translate a key press into an action, or `None` when it is unbound.
///
/// `jumping` is passed rather than the focus because the jump prompt is modal:
/// while it is open, printable keys are text, not commands. `focus` is passed
/// because `=`/`+`/`-` are contextual: on the zoom column they change its pixel
/// size, anywhere else the entropy window.
pub(crate) fn key_to_action(key: KeyEvent, jumping: bool, focus: Focus) -> Option<Action> {
    // Ctrl+C quits from any state, including mid-prompt: a terminal user expects
    // it to work when nothing else does.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Some(Action::Quit);
    }
    if jumping {
        return match key.code {
            KeyCode::Enter => Some(Action::JumpSubmit),
            KeyCode::Esc => Some(Action::JumpCancel),
            KeyCode::Backspace => Some(Action::JumpBackspace),
            KeyCode::Char(c) => Some(Action::JumpChar(c)),
            _ => None,
        };
    }
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let nav = |n: Nav| {
        Some(if shift {
            Action::Extend(n)
        } else {
            Action::Move(n)
        })
    };
    match key.code {
        KeyCode::Tab => Some(Action::FocusNext),
        // Crossterm reports Shift+Tab as BackTab, and whether it also sets the
        // SHIFT modifier varies by terminal, so match the code alone.
        KeyCode::BackTab => Some(Action::FocusPrev),
        KeyCode::Left => nav(Nav::Left),
        KeyCode::Right => nav(Nav::Right),
        KeyCode::Up => nav(Nav::Up),
        KeyCode::Down => nav(Nav::Down),
        KeyCode::PageUp => nav(Nav::PageUp),
        KeyCode::PageDown => nav(Nav::PageDown),
        KeyCode::Home => nav(Nav::Home),
        KeyCode::End => nav(Nav::End),
        KeyCode::Char('1') => Some(Action::SetColormap(Colormap::None)),
        KeyCode::Char('2') => Some(Action::SetColormap(Colormap::Value)),
        KeyCode::Char('3') => Some(Action::SetColormap(Colormap::Class)),
        KeyCode::Char('4') => Some(Action::SetColormap(Colormap::Entropy)),
        KeyCode::Char('y') => Some(Action::CopyHex),
        KeyCode::Char('Y') => Some(Action::CopyAscii),
        KeyCode::Char('g') => Some(Action::OpenJump),
        // `+` needs Shift on most layouts, so accept `=` as well — the same
        // reason the gpui frontend binds `=` for zoom-in. On the zoom column
        // these keys change its pixel size (the TUI's equivalent of the gpui
        // frontend zooming the column under the pointer); anywhere else they
        // keep halving/doubling the entropy window.
        KeyCode::Char('+' | '=') if focus == Focus::Zoom => Some(Action::ZoomIn),
        KeyCode::Char('+' | '=') => Some(Action::EntropyDouble),
        KeyCode::Char('-') if focus == Focus::Zoom => Some(Action::ZoomOut),
        KeyCode::Char('-') => Some(Action::EntropyHalve),
        KeyCode::Char('q') | KeyCode::Esc => Some(Action::Quit),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn shift(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::SHIFT)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    /// The focus most bindings ignore; the zoom keys are the exception and get
    /// their own test.
    const HEX: Focus = Focus::Hex;

    #[test]
    fn every_documented_binding_maps() {
        assert_eq!(
            key_to_action(key(KeyCode::Tab), false, HEX),
            Some(Action::FocusNext)
        );
        assert_eq!(
            key_to_action(key(KeyCode::BackTab), false, HEX),
            Some(Action::FocusPrev)
        );
        assert_eq!(
            key_to_action(key(KeyCode::Left), false, HEX),
            Some(Action::Move(Nav::Left))
        );
        assert_eq!(
            key_to_action(key(KeyCode::PageUp), false, HEX),
            Some(Action::Move(Nav::PageUp))
        );
        assert_eq!(
            key_to_action(key(KeyCode::Home), false, HEX),
            Some(Action::Move(Nav::Home))
        );
        assert_eq!(
            key_to_action(key(KeyCode::End), false, HEX),
            Some(Action::Move(Nav::End))
        );
        assert_eq!(
            key_to_action(key(KeyCode::Char('1')), false, HEX),
            Some(Action::SetColormap(Colormap::None))
        );
        assert_eq!(
            key_to_action(key(KeyCode::Char('4')), false, HEX),
            Some(Action::SetColormap(Colormap::Entropy))
        );
        assert_eq!(
            key_to_action(key(KeyCode::Char('y')), false, HEX),
            Some(Action::CopyHex)
        );
        assert_eq!(
            key_to_action(key(KeyCode::Char('Y')), false, HEX),
            Some(Action::CopyAscii)
        );
        assert_eq!(
            key_to_action(key(KeyCode::Char('g')), false, HEX),
            Some(Action::OpenJump)
        );
        assert_eq!(
            key_to_action(key(KeyCode::Char('q')), false, HEX),
            Some(Action::Quit)
        );
        assert_eq!(key_to_action(ctrl('c'), false, HEX), Some(Action::Quit));
    }

    #[test]
    fn shift_arrows_extend_rather_than_move() {
        assert_eq!(
            key_to_action(shift(KeyCode::Right), false, HEX),
            Some(Action::Extend(Nav::Right))
        );
        assert_eq!(
            key_to_action(shift(KeyCode::Down), false, HEX),
            Some(Action::Extend(Nav::Down))
        );
        assert_eq!(
            key_to_action(shift(KeyCode::PageDown), false, HEX),
            Some(Action::Extend(Nav::PageDown))
        );
    }

    #[test]
    fn plus_is_reachable_without_shift() {
        assert_eq!(
            key_to_action(key(KeyCode::Char('+')), false, HEX),
            Some(Action::EntropyDouble)
        );
        assert_eq!(
            key_to_action(key(KeyCode::Char('=')), false, HEX),
            Some(Action::EntropyDouble)
        );
        assert_eq!(
            key_to_action(key(KeyCode::Char('-')), false, HEX),
            Some(Action::EntropyHalve)
        );
    }

    #[test]
    fn zoom_keys_follow_the_focused_panel() {
        // Only the zoom column's keys change its pixel size; on every other
        // panel the same keys keep adjusting the entropy window. Hex is
        // checked too, so no future panel can silently fall through to the
        // wrong binding.
        for focus in [Focus::Overview, Focus::Zoom, Focus::Hex] {
            assert_eq!(
                key_to_action(key(KeyCode::Char('=')), false, focus),
                Some(if focus == Focus::Zoom {
                    Action::ZoomIn
                } else {
                    Action::EntropyDouble
                }),
                "= with {focus:?} focused"
            );
            assert_eq!(
                key_to_action(key(KeyCode::Char('+')), false, focus),
                Some(if focus == Focus::Zoom {
                    Action::ZoomIn
                } else {
                    Action::EntropyDouble
                }),
                "+ with {focus:?} focused"
            );
            assert_eq!(
                key_to_action(key(KeyCode::Char('-')), false, focus),
                Some(if focus == Focus::Zoom {
                    Action::ZoomOut
                } else {
                    Action::EntropyHalve
                }),
                "- with {focus:?} focused"
            );
        }
    }

    #[test]
    fn the_jump_prompt_is_modal() {
        // While jumping, printable keys are text and must not fire commands.
        assert_eq!(
            key_to_action(key(KeyCode::Char('q')), true, HEX),
            Some(Action::JumpChar('q'))
        );
        assert_eq!(
            key_to_action(key(KeyCode::Char('1')), true, HEX),
            Some(Action::JumpChar('1'))
        );
        assert_eq!(
            key_to_action(key(KeyCode::Enter), true, HEX),
            Some(Action::JumpSubmit)
        );
        assert_eq!(
            key_to_action(key(KeyCode::Esc), true, HEX),
            Some(Action::JumpCancel)
        );
        assert_eq!(
            key_to_action(key(KeyCode::Backspace), true, HEX),
            Some(Action::JumpBackspace)
        );
        // Ctrl+C still quits mid-prompt.
        assert_eq!(key_to_action(ctrl('c'), true, HEX), Some(Action::Quit));
        // Arrows are not text, and are not commands here either.
        assert_eq!(key_to_action(key(KeyCode::Left), true, HEX), None);
    }

    #[test]
    fn unbound_keys_are_ignored() {
        assert_eq!(key_to_action(key(KeyCode::Char('z')), false, HEX), None);
        assert_eq!(key_to_action(key(KeyCode::F(5)), false, HEX), None);
        assert_eq!(key_to_action(key(KeyCode::Insert), false, HEX), None);
    }
}
