//! Cross-platform modifier remapping.
//!
//! The problem: the user presses ⌘C on a Mac and expects a copy on the Windows
//! box the cursor is currently sitting on — where copy is Ctrl+C. Sending the
//! physical key faithfully would produce Win+C, which opens Copilot.
//!
//! The fix is a role swap. Modifiers are carried as abstract roles (Shift /
//! Control / Alt / Meta), and when a keystroke crosses between a macOS machine
//! and a non-macOS one, Control and Meta trade places. ⌘C becomes Ctrl+C going
//! one way; Ctrl+C becomes ⌘C coming back. Alt/Option and Shift never move.
//!
//! The swap is applied by the *receiver*, which knows both its own platform and
//! (from the `Welcome`/`Hello` frame) the sender's. Doing it on the host would
//! mean maintaining a different translation per connected client.

use serde::{Deserialize, Serialize};
use tether_proto::{InputEvent, KeyCode, Modifiers, Platform};

/// The abstract job a modifier does, independent of what the keycap says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ModRole {
    Shift,
    Control,
    Alt,
    Meta,
}

impl ModRole {
    const ALL: [ModRole; 4] = [ModRole::Shift, ModRole::Control, ModRole::Alt, ModRole::Meta];

    fn index(self) -> usize {
        match self {
            ModRole::Shift => 0,
            ModRole::Control => 1,
            ModRole::Alt => 2,
            ModRole::Meta => 3,
        }
    }

    fn bit(self) -> Modifiers {
        match self {
            ModRole::Shift => Modifiers::SHIFT,
            ModRole::Control => Modifiers::CONTROL,
            ModRole::Alt => Modifiers::ALT,
            ModRole::Meta => Modifiers::META,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Left,
    Right,
}

/// Split a modifier keycode into the job it does and which side it is on.
fn decompose(key: KeyCode) -> Option<(ModRole, Side)> {
    Some(match key {
        KeyCode::LEFT_CONTROL => (ModRole::Control, Side::Left),
        KeyCode::LEFT_SHIFT => (ModRole::Shift, Side::Left),
        KeyCode::LEFT_ALT => (ModRole::Alt, Side::Left),
        KeyCode::LEFT_META => (ModRole::Meta, Side::Left),
        KeyCode::RIGHT_CONTROL => (ModRole::Control, Side::Right),
        KeyCode::RIGHT_SHIFT => (ModRole::Shift, Side::Right),
        KeyCode::RIGHT_ALT => (ModRole::Alt, Side::Right),
        KeyCode::RIGHT_META => (ModRole::Meta, Side::Right),
        _ => return None,
    })
}

fn compose(role: ModRole, side: Side) -> KeyCode {
    match (role, side) {
        (ModRole::Control, Side::Left) => KeyCode::LEFT_CONTROL,
        (ModRole::Shift, Side::Left) => KeyCode::LEFT_SHIFT,
        (ModRole::Alt, Side::Left) => KeyCode::LEFT_ALT,
        (ModRole::Meta, Side::Left) => KeyCode::LEFT_META,
        (ModRole::Control, Side::Right) => KeyCode::RIGHT_CONTROL,
        (ModRole::Shift, Side::Right) => KeyCode::RIGHT_SHIFT,
        (ModRole::Alt, Side::Right) => KeyCode::RIGHT_ALT,
        (ModRole::Meta, Side::Right) => KeyCode::RIGHT_META,
    }
}

/// A permutation of the four modifier roles. Indexed by source role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModifierMap {
    to: [ModRole; 4],
}

impl Default for ModifierMap {
    fn default() -> Self {
        Self::identity()
    }
}

impl ModifierMap {
    pub const fn identity() -> Self {
        Self {
            to: [ModRole::Shift, ModRole::Control, ModRole::Alt, ModRole::Meta],
        }
    }

    /// Control ⇄ Meta. The mapping that makes ⌘C work as Ctrl+C.
    pub const fn swap_control_meta() -> Self {
        Self {
            to: [ModRole::Shift, ModRole::Meta, ModRole::Alt, ModRole::Control],
        }
    }

    /// The default mapping for a keystroke travelling `from` → `to`.
    ///
    /// Only the macOS boundary matters. Windows and Linux agree on Ctrl for
    /// shortcuts and reserve Super for the window manager, so nothing is
    /// swapped between them — remapping Win+D there would break far more than
    /// it fixed.
    pub fn between(from: Platform, to: Platform) -> Self {
        let crosses_mac = (from == Platform::MacOS) != (to == Platform::MacOS);
        if crosses_mac {
            Self::swap_control_meta()
        } else {
            Self::identity()
        }
    }

    /// Override a single role's destination, for user customisation.
    pub fn set(&mut self, from: ModRole, to: ModRole) {
        self.to[from.index()] = to;
    }

    pub fn get(&self, from: ModRole) -> ModRole {
        self.to[from.index()]
    }

    pub fn is_identity(&self) -> bool {
        *self == Self::identity()
    }

    /// Remap a modifier keycode, preserving which side of the keyboard it is
    /// on. Non-modifier keys pass through untouched: only the modifiers differ
    /// between platforms, letters and digits are the same physical positions
    /// everywhere.
    pub fn remap_key(&self, key: KeyCode) -> KeyCode {
        match decompose(key) {
            Some((role, side)) => compose(self.get(role), side),
            None => key,
        }
    }

    /// Remap a modifier bitmask. Caps Lock rides along unchanged — it is a
    /// lock state, not a chord modifier.
    pub fn remap_modifiers(&self, mods: Modifiers) -> Modifiers {
        let mut out = Modifiers::NONE;
        for role in ModRole::ALL {
            if mods.contains(role.bit()) {
                out.insert(self.get(role).bit());
            }
        }
        if mods.contains(Modifiers::CAPS_LOCK) {
            out.insert(Modifiers::CAPS_LOCK);
        }
        out
    }

    /// Remap a whole event. Mouse events are returned unchanged.
    pub fn remap_event(&self, event: InputEvent) -> InputEvent {
        match event {
            InputEvent::Key {
                key,
                pressed,
                modifiers,
                repeat,
            } => InputEvent::Key {
                key: self.remap_key(key),
                pressed,
                modifiers: self.remap_modifiers(modifiers),
                repeat,
            },
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_to_windows_swaps_command_into_control() {
        let map = ModifierMap::between(Platform::MacOS, Platform::Windows);
        assert_eq!(map.remap_key(KeyCode::LEFT_META), KeyCode::LEFT_CONTROL);
        assert_eq!(map.remap_modifiers(Modifiers::META), Modifiers::CONTROL);
    }

    #[test]
    fn windows_to_mac_swaps_control_into_command() {
        let map = ModifierMap::between(Platform::Windows, Platform::MacOS);
        assert_eq!(map.remap_key(KeyCode::LEFT_CONTROL), KeyCode::LEFT_META);
        assert_eq!(map.remap_modifiers(Modifiers::CONTROL), Modifiers::META);
    }

    #[test]
    fn the_swap_is_its_own_inverse() {
        let there = ModifierMap::between(Platform::MacOS, Platform::Linux);
        let back = ModifierMap::between(Platform::Linux, Platform::MacOS);
        for key in [KeyCode::LEFT_META, KeyCode::RIGHT_CONTROL, KeyCode::LEFT_ALT] {
            assert_eq!(back.remap_key(there.remap_key(key)), key);
        }
    }

    #[test]
    fn windows_to_linux_changes_nothing() {
        // Win+D must stay Super+D, not become Ctrl+D.
        let map = ModifierMap::between(Platform::Windows, Platform::Linux);
        assert!(map.is_identity());
        assert_eq!(map.remap_key(KeyCode::LEFT_META), KeyCode::LEFT_META);
    }

    #[test]
    fn same_platform_changes_nothing() {
        assert!(ModifierMap::between(Platform::MacOS, Platform::MacOS).is_identity());
    }

    #[test]
    fn the_side_of_the_keyboard_survives_the_swap() {
        let map = ModifierMap::swap_control_meta();
        assert_eq!(map.remap_key(KeyCode::RIGHT_META), KeyCode::RIGHT_CONTROL);
        assert_eq!(map.remap_key(KeyCode::RIGHT_CONTROL), KeyCode::RIGHT_META);
    }

    #[test]
    fn option_and_shift_never_move() {
        let map = ModifierMap::swap_control_meta();
        assert_eq!(map.remap_key(KeyCode::LEFT_ALT), KeyCode::LEFT_ALT);
        assert_eq!(map.remap_key(KeyCode::LEFT_SHIFT), KeyCode::LEFT_SHIFT);
    }

    #[test]
    fn ordinary_keys_pass_straight_through() {
        let map = ModifierMap::swap_control_meta();
        assert_eq!(map.remap_key(KeyCode::C), KeyCode::C);
        assert_eq!(map.remap_key(KeyCode::F5), KeyCode::F5);
    }

    #[test]
    fn caps_lock_rides_along_unchanged() {
        let map = ModifierMap::swap_control_meta();
        let mods = Modifiers::META | Modifiers::CAPS_LOCK;
        let out = map.remap_modifiers(mods);
        assert!(out.contains(Modifiers::CONTROL));
        assert!(out.contains(Modifiers::CAPS_LOCK));
        assert!(!out.contains(Modifiers::META));
    }

    #[test]
    fn cmd_c_becomes_ctrl_c_end_to_end() {
        let map = ModifierMap::between(Platform::MacOS, Platform::Windows);
        let event = InputEvent::Key {
            key: KeyCode::C,
            pressed: true,
            modifiers: Modifiers::META,
            repeat: false,
        };
        assert_eq!(
            map.remap_event(event),
            InputEvent::Key {
                key: KeyCode::C,
                pressed: true,
                modifiers: Modifiers::CONTROL,
                repeat: false,
            }
        );
    }

    #[test]
    fn mouse_events_are_untouched() {
        let map = ModifierMap::swap_control_meta();
        let ev = InputEvent::MouseMove { x: 3, y: 4 };
        assert_eq!(map.remap_event(ev.clone()), ev);
    }
}
