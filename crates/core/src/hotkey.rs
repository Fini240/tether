//! Hotkey bindings and their textual form.
//!
//! Hotkeys are matched on the host, against the physical keys it captures,
//! before routing. A hotkey is consumed — it does not also reach whichever
//! machine currently owns the cursor.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use tether_proto::{KeyCode, Modifiers};

use crate::layout::MachineId;

/// What a hotkey does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    /// Warp the cursor to a machine's primary display.
    SwitchTo { machine: MachineId },
    /// Warp back to the host.
    RecallCursor,
    /// Toggle the cursor lock on the machine that currently owns the cursor.
    ToggleLock,
    /// Lock the screen on every connected machine.
    LockAllScreens,
    /// Re-broadcast the host's clipboard, for when a stubborn app missed it.
    PushClipboard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hotkey {
    pub key: KeyCode,
    pub modifiers: Modifiers,
}

impl Hotkey {
    pub fn new(key: KeyCode, modifiers: Modifiers) -> Self {
        Self { key, modifiers }
    }

    /// Matches only on key-down, and only on an exact modifier match — a
    /// binding for Ctrl+Alt+1 must not fire on Ctrl+Alt+Shift+1, which the user
    /// may well have bound to something else.
    pub fn matches(&self, key: KeyCode, modifiers: Modifiers, pressed: bool) -> bool {
        pressed && self.key == key && self.chord_modifiers(modifiers) == self.modifiers
    }

    /// Caps Lock is a lock state, not part of a chord; ignore it when matching.
    fn chord_modifiers(&self, mut modifiers: Modifiers) -> Modifiers {
        modifiers.remove(Modifiers::CAPS_LOCK);
        modifiers
    }
}

/// The bindings table. Order matters only in that the first match wins.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HotkeyTable {
    pub bindings: Vec<Binding>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Binding {
    pub hotkey: Hotkey,
    #[serde(flatten)]
    pub action: Action,
}

impl HotkeyTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn bind(&mut self, hotkey: Hotkey, action: Action) {
        // Rebinding an existing chord replaces it rather than shadowing it,
        // so a config edited by hand cannot end up with a dead entry.
        self.bindings.retain(|b| b.hotkey != hotkey);
        self.bindings.push(Binding { hotkey, action });
    }

    pub fn unbind(&mut self, hotkey: Hotkey) {
        self.bindings.retain(|b| b.hotkey != hotkey);
    }

    pub fn lookup(&self, key: KeyCode, modifiers: Modifiers, pressed: bool) -> Option<&Action> {
        self.bindings
            .iter()
            .find(|b| b.hotkey.matches(key, modifiers, pressed))
            .map(|b| &b.action)
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ParseHotkeyError {
    #[error("empty hotkey")]
    Empty,
    #[error("unknown key or modifier: {0:?}")]
    Unknown(String),
    #[error("hotkey has no non-modifier key")]
    NoKey,
}

impl FromStr for Hotkey {
    type Err = ParseHotkeyError;

    /// Parses forms like `ctrl+alt+1`, `cmd+shift+l`, `super+f12`.
    fn from_str(s: &str) -> Result<Self, ParseHotkeyError> {
        if s.trim().is_empty() {
            return Err(ParseHotkeyError::Empty);
        }
        let mut modifiers = Modifiers::NONE;
        let mut key = None;

        for part in s.split('+') {
            let part = part.trim().to_ascii_lowercase();
            match part.as_str() {
                "shift" => modifiers.insert(Modifiers::SHIFT),
                "ctrl" | "control" => modifiers.insert(Modifiers::CONTROL),
                "alt" | "option" | "opt" => modifiers.insert(Modifiers::ALT),
                "cmd" | "command" | "super" | "win" | "meta" => modifiers.insert(Modifiers::META),
                other => {
                    let parsed = key_from_name(other)
                        .ok_or_else(|| ParseHotkeyError::Unknown(other.to_string()))?;
                    key = Some(parsed);
                }
            }
        }

        Ok(Hotkey {
            key: key.ok_or(ParseHotkeyError::NoKey)?,
            modifiers,
        })
    }
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (bit, name) in [
            (Modifiers::CONTROL, "ctrl"),
            (Modifiers::ALT, "alt"),
            (Modifiers::SHIFT, "shift"),
            (Modifiers::META, "cmd"),
        ] {
            if self.modifiers.contains(bit) {
                write!(f, "{name}+")?;
            }
        }
        f.write_str(key_name(self.key).unwrap_or("?"))
    }
}

/// Name → keycode for the subset a user would plausibly bind. Deliberately not
/// exhaustive over HID: the arrangement UI offers a key capture field, and this
/// parser exists for hand-edited config files.
fn key_from_name(name: &str) -> Option<KeyCode> {
    Some(match name {
        "a" => KeyCode::A,
        "b" => KeyCode::B,
        "c" => KeyCode::C,
        "d" => KeyCode::D,
        "e" => KeyCode::E,
        "f" => KeyCode::F,
        "g" => KeyCode::G,
        "h" => KeyCode::H,
        "i" => KeyCode::I,
        "j" => KeyCode::J,
        "k" => KeyCode::K,
        "l" => KeyCode::L,
        "m" => KeyCode::M,
        "n" => KeyCode::N,
        "o" => KeyCode::O,
        "p" => KeyCode::P,
        "q" => KeyCode::Q,
        "r" => KeyCode::R,
        "s" => KeyCode::S,
        "t" => KeyCode::T,
        "u" => KeyCode::U,
        "v" => KeyCode::V,
        "w" => KeyCode::W,
        "x" => KeyCode::X,
        "y" => KeyCode::Y,
        "z" => KeyCode::Z,
        "1" => KeyCode::NUM1,
        "2" => KeyCode::NUM2,
        "3" => KeyCode::NUM3,
        "4" => KeyCode::NUM4,
        "5" => KeyCode::NUM5,
        "6" => KeyCode::NUM6,
        "7" => KeyCode::NUM7,
        "8" => KeyCode::NUM8,
        "9" => KeyCode::NUM9,
        "0" => KeyCode::NUM0,
        "f1" => KeyCode::F1,
        "f2" => KeyCode::F2,
        "f3" => KeyCode::F3,
        "f4" => KeyCode::F4,
        "f5" => KeyCode::F5,
        "f6" => KeyCode::F6,
        "f7" => KeyCode::F7,
        "f8" => KeyCode::F8,
        "f9" => KeyCode::F9,
        "f10" => KeyCode::F10,
        "f11" => KeyCode::F11,
        "f12" => KeyCode::F12,
        "space" => KeyCode::SPACE,
        "tab" => KeyCode::TAB,
        "enter" | "return" => KeyCode::ENTER,
        "esc" | "escape" => KeyCode::ESCAPE,
        "left" => KeyCode::LEFT,
        "right" => KeyCode::RIGHT,
        "up" => KeyCode::UP,
        "down" => KeyCode::DOWN,
        "home" => KeyCode::HOME,
        "end" => KeyCode::END,
        "pageup" => KeyCode::PAGE_UP,
        "pagedown" => KeyCode::PAGE_DOWN,
        "insert" => KeyCode::INSERT,
        "delete" | "del" => KeyCode::DELETE,
        "grave" | "`" => KeyCode::GRAVE,
        _ => return None,
    })
}

fn key_name(key: KeyCode) -> Option<&'static str> {
    // Small reverse lookup over the same table; linear over ~70 entries is
    // irrelevant next to the cost of rendering a settings pane.
    const NAMES: &[&str] = &[
        "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r",
        "s", "t", "u", "v", "w", "x", "y", "z", "1", "2", "3", "4", "5", "6", "7", "8", "9", "0",
        "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10", "f11", "f12", "space", "tab",
        "enter", "escape", "left", "right", "up", "down", "home", "end", "pageup", "pagedown",
        "insert", "delete", "grave",
    ];
    NAMES
        .iter()
        .find(|n| key_from_name(n) == Some(key))
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_modifier_chord() {
        let hk: Hotkey = "ctrl+alt+1".parse().unwrap();
        assert_eq!(hk.key, KeyCode::NUM1);
        assert!(hk.modifiers.contains(Modifiers::CONTROL));
        assert!(hk.modifiers.contains(Modifiers::ALT));
        assert!(!hk.modifiers.contains(Modifiers::SHIFT));
    }

    #[test]
    fn accepts_platform_aliases_for_meta() {
        for s in ["cmd+l", "super+l", "win+l", "meta+l"] {
            assert_eq!(s.parse::<Hotkey>().unwrap().modifiers, Modifiers::META);
        }
    }

    #[test]
    fn rejects_a_modifier_only_chord() {
        assert_eq!("ctrl+alt".parse::<Hotkey>(), Err(ParseHotkeyError::NoKey));
    }

    #[test]
    fn rejects_an_unknown_key() {
        assert!(matches!(
            "ctrl+nope".parse::<Hotkey>(),
            Err(ParseHotkeyError::Unknown(_))
        ));
    }

    #[test]
    fn display_round_trips_through_the_parser() {
        let hk: Hotkey = "ctrl+alt+shift+f5".parse().unwrap();
        assert_eq!(hk.to_string().parse::<Hotkey>().unwrap(), hk);
    }

    #[test]
    fn an_extra_modifier_does_not_match() {
        let hk: Hotkey = "ctrl+alt+1".parse().unwrap();
        assert!(hk.matches(KeyCode::NUM1, Modifiers::CONTROL | Modifiers::ALT, true));
        assert!(!hk.matches(
            KeyCode::NUM1,
            Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT,
            true
        ));
    }

    #[test]
    fn caps_lock_does_not_break_a_match() {
        let hk: Hotkey = "ctrl+1".parse().unwrap();
        assert!(hk.matches(
            KeyCode::NUM1,
            Modifiers::CONTROL | Modifiers::CAPS_LOCK,
            true
        ));
    }

    #[test]
    fn key_up_never_matches() {
        let hk: Hotkey = "ctrl+1".parse().unwrap();
        assert!(!hk.matches(KeyCode::NUM1, Modifiers::CONTROL, false));
    }

    #[test]
    fn rebinding_a_chord_replaces_it() {
        let mut table = HotkeyTable::new();
        let hk: Hotkey = "ctrl+alt+l".parse().unwrap();
        table.bind(hk, Action::ToggleLock);
        table.bind(hk, Action::RecallCursor);
        assert_eq!(table.bindings.len(), 1);
        assert_eq!(
            table.lookup(hk.key, hk.modifiers, true),
            Some(&Action::RecallCursor)
        );
    }
}
