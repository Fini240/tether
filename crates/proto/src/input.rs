//! Platform-neutral input events.
//!
//! Keys travel as **USB HID usage IDs** (usage page 0x07), not as the host OS's
//! native scancodes and not as characters. That choice matters:
//!
//! * A HID usage identifies a *physical key position*, so a Dvorak host driving
//!   a QWERTY client produces what the client's own layout says it should —
//!   which is what users expect, and what character-based protocols get wrong.
//! * It is the one encoding every platform can map to and from losslessly.
//!
//! The per-OS translation tables live in `tether-platform`.

use serde::{Deserialize, Serialize};

/// A USB HID usage ID from usage page 0x07 (Keyboard/Keypad).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct KeyCode(pub u16);

impl KeyCode {
    pub const A: KeyCode = KeyCode(0x04);
    pub const B: KeyCode = KeyCode(0x05);
    pub const C: KeyCode = KeyCode(0x06);
    pub const D: KeyCode = KeyCode(0x07);
    pub const E: KeyCode = KeyCode(0x08);
    pub const F: KeyCode = KeyCode(0x09);
    pub const G: KeyCode = KeyCode(0x0A);
    pub const H: KeyCode = KeyCode(0x0B);
    pub const I: KeyCode = KeyCode(0x0C);
    pub const J: KeyCode = KeyCode(0x0D);
    pub const K: KeyCode = KeyCode(0x0E);
    pub const L: KeyCode = KeyCode(0x0F);
    pub const M: KeyCode = KeyCode(0x10);
    pub const N: KeyCode = KeyCode(0x11);
    pub const O: KeyCode = KeyCode(0x12);
    pub const P: KeyCode = KeyCode(0x13);
    pub const Q: KeyCode = KeyCode(0x14);
    pub const R: KeyCode = KeyCode(0x15);
    pub const S: KeyCode = KeyCode(0x16);
    pub const T: KeyCode = KeyCode(0x17);
    pub const U: KeyCode = KeyCode(0x18);
    pub const V: KeyCode = KeyCode(0x19);
    pub const W: KeyCode = KeyCode(0x1A);
    pub const X: KeyCode = KeyCode(0x1B);
    pub const Y: KeyCode = KeyCode(0x1C);
    pub const Z: KeyCode = KeyCode(0x1D);

    pub const NUM1: KeyCode = KeyCode(0x1E);
    pub const NUM2: KeyCode = KeyCode(0x1F);
    pub const NUM3: KeyCode = KeyCode(0x20);
    pub const NUM4: KeyCode = KeyCode(0x21);
    pub const NUM5: KeyCode = KeyCode(0x22);
    pub const NUM6: KeyCode = KeyCode(0x23);
    pub const NUM7: KeyCode = KeyCode(0x24);
    pub const NUM8: KeyCode = KeyCode(0x25);
    pub const NUM9: KeyCode = KeyCode(0x26);
    pub const NUM0: KeyCode = KeyCode(0x27);

    pub const ENTER: KeyCode = KeyCode(0x28);
    pub const ESCAPE: KeyCode = KeyCode(0x29);
    pub const BACKSPACE: KeyCode = KeyCode(0x2A);
    pub const TAB: KeyCode = KeyCode(0x2B);
    pub const SPACE: KeyCode = KeyCode(0x2C);
    pub const MINUS: KeyCode = KeyCode(0x2D);
    pub const EQUAL: KeyCode = KeyCode(0x2E);
    pub const LEFT_BRACKET: KeyCode = KeyCode(0x2F);
    pub const RIGHT_BRACKET: KeyCode = KeyCode(0x30);
    pub const BACKSLASH: KeyCode = KeyCode(0x31);
    pub const SEMICOLON: KeyCode = KeyCode(0x33);
    pub const QUOTE: KeyCode = KeyCode(0x34);
    pub const GRAVE: KeyCode = KeyCode(0x35);
    pub const COMMA: KeyCode = KeyCode(0x36);
    pub const PERIOD: KeyCode = KeyCode(0x37);
    pub const SLASH: KeyCode = KeyCode(0x38);
    pub const CAPS_LOCK: KeyCode = KeyCode(0x39);

    pub const F1: KeyCode = KeyCode(0x3A);
    pub const F2: KeyCode = KeyCode(0x3B);
    pub const F3: KeyCode = KeyCode(0x3C);
    pub const F4: KeyCode = KeyCode(0x3D);
    pub const F5: KeyCode = KeyCode(0x3E);
    pub const F6: KeyCode = KeyCode(0x3F);
    pub const F7: KeyCode = KeyCode(0x40);
    pub const F8: KeyCode = KeyCode(0x41);
    pub const F9: KeyCode = KeyCode(0x42);
    pub const F10: KeyCode = KeyCode(0x43);
    pub const F11: KeyCode = KeyCode(0x44);
    pub const F12: KeyCode = KeyCode(0x45);

    pub const PRINT_SCREEN: KeyCode = KeyCode(0x46);
    pub const SCROLL_LOCK: KeyCode = KeyCode(0x47);
    pub const PAUSE: KeyCode = KeyCode(0x48);
    pub const INSERT: KeyCode = KeyCode(0x49);
    pub const HOME: KeyCode = KeyCode(0x4A);
    pub const PAGE_UP: KeyCode = KeyCode(0x4B);
    pub const DELETE: KeyCode = KeyCode(0x4C);
    pub const END: KeyCode = KeyCode(0x4D);
    pub const PAGE_DOWN: KeyCode = KeyCode(0x4E);
    pub const RIGHT: KeyCode = KeyCode(0x4F);
    pub const LEFT: KeyCode = KeyCode(0x50);
    pub const DOWN: KeyCode = KeyCode(0x51);
    pub const UP: KeyCode = KeyCode(0x52);

    pub const LEFT_CONTROL: KeyCode = KeyCode(0xE0);
    pub const LEFT_SHIFT: KeyCode = KeyCode(0xE1);
    pub const LEFT_ALT: KeyCode = KeyCode(0xE2);
    pub const LEFT_META: KeyCode = KeyCode(0xE3);
    pub const RIGHT_CONTROL: KeyCode = KeyCode(0xE4);
    pub const RIGHT_SHIFT: KeyCode = KeyCode(0xE5);
    pub const RIGHT_ALT: KeyCode = KeyCode(0xE6);
    pub const RIGHT_META: KeyCode = KeyCode(0xE7);

    /// True for the eight modifier usages in the 0xE0..=0xE7 block.
    pub fn is_modifier(self) -> bool {
        (0xE0..=0xE7).contains(&self.0)
    }

    /// The modifier bit this key contributes, if it is a modifier.
    pub fn modifier_bit(self) -> Option<Modifiers> {
        Some(match self {
            Self::LEFT_CONTROL | Self::RIGHT_CONTROL => Modifiers::CONTROL,
            Self::LEFT_SHIFT | Self::RIGHT_SHIFT => Modifiers::SHIFT,
            Self::LEFT_ALT | Self::RIGHT_ALT => Modifiers::ALT,
            Self::LEFT_META | Self::RIGHT_META => Modifiers::META,
            _ => return None,
        })
    }
}

/// Modifier state as a bitmask.
///
/// `META` is Command on macOS and the Windows/Super key elsewhere; `ALT` is
/// Option on macOS. Keeping them as abstract roles rather than OS-specific
/// names is what makes the remap in `tether-core::keymap` expressible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Modifiers(pub u8);

impl Modifiers {
    pub const NONE: Modifiers = Modifiers(0);
    pub const SHIFT: Modifiers = Modifiers(1 << 0);
    pub const CONTROL: Modifiers = Modifiers(1 << 1);
    pub const ALT: Modifiers = Modifiers(1 << 2);
    pub const META: Modifiers = Modifiers(1 << 3);
    pub const CAPS_LOCK: Modifiers = Modifiers(1 << 4);

    pub const fn contains(self, other: Modifiers) -> bool {
        self.0 & other.0 == other.0
    }
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
    pub fn insert(&mut self, other: Modifiers) {
        self.0 |= other.0;
    }
    pub fn remove(&mut self, other: Modifiers) {
        self.0 &= !other.0;
    }
    pub fn set(&mut self, other: Modifiers, on: bool) {
        if on {
            self.insert(other)
        } else {
            self.remove(other)
        }
    }
}

impl std::ops::BitOr for Modifiers {
    type Output = Modifiers;
    fn bitor(self, rhs: Modifiers) -> Modifiers {
        Modifiers(self.0 | rhs.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
    Other(u8),
}

/// Input as *captured*, before any routing — the form a machine reports when
/// it is the one being physically touched.
///
/// Distinct from [`InputEvent`], which is input as *delivered*: motion here is
/// a delta from a physical device, whereas a delivered `MouseMove` is an
/// absolute position the router has already decided on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SourceEvent {
    MouseDelta {
        dx: i32,
        dy: i32,
    },
    Button {
        button: MouseButton,
        pressed: bool,
    },
    Wheel {
        dx: f32,
        dy: f32,
    },
    Key {
        key: KeyCode,
        pressed: bool,
        modifiers: Modifiers,
        repeat: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum InputEvent {
    /// Absolute position in the *receiving* machine's local coordinate space.
    /// The host translates before sending; clients never do layout maths.
    MouseMove {
        x: i32,
        y: i32,
    },
    /// Relative motion, used for pointer-locked contexts (games, 3D viewports)
    /// where absolute warping fights the application.
    MouseMoveRelative {
        dx: i32,
        dy: i32,
    },
    MouseButton {
        button: MouseButton,
        pressed: bool,
    },
    /// Scroll deltas in lines. Fractional to carry high-resolution trackpads.
    MouseWheel {
        dx: f32,
        dy: f32,
    },
    Key {
        key: KeyCode,
        pressed: bool,
        /// Modifier state *as the host saw it*, before any cross-platform
        /// remap. The client remaps using its own platform + the host's.
        modifiers: Modifiers,
        /// Set when the OS is auto-repeating a held key.
        repeat: bool,
    },
}
