//! USB HID usage ⇄ PS/2 set-1 scan code.
//!
//! Scan codes, not virtual keys. A virtual key has already been through the
//! active keyboard layout, so sending `VK_Y` from a QWERTY machine to a QWERTZ
//! one types the wrong letter. A scan code is the physical key position, which
//! is exactly what a HID usage is — the receiving machine's own layout then
//! decides what character that position produces, which is what a user expects.
//!
//! Codes at or above `0xE000` are "extended": the low byte goes in `wScan` and
//! `KEYEVENTF_EXTENDEDKEY` must be set alongside it.

use tether_proto::KeyCode;

/// HID usage → scan code. `None` for keys Windows has no set-1 code for.
pub fn hid_to_scancode(key: KeyCode) -> Option<u16> {
    Some(match key {
        KeyCode::A => 0x1E,
        KeyCode::B => 0x30,
        KeyCode::C => 0x2E,
        KeyCode::D => 0x20,
        KeyCode::E => 0x12,
        KeyCode::F => 0x21,
        KeyCode::G => 0x22,
        KeyCode::H => 0x23,
        KeyCode::I => 0x17,
        KeyCode::J => 0x24,
        KeyCode::K => 0x25,
        KeyCode::L => 0x26,
        KeyCode::M => 0x32,
        KeyCode::N => 0x31,
        KeyCode::O => 0x18,
        KeyCode::P => 0x19,
        KeyCode::Q => 0x10,
        KeyCode::R => 0x13,
        KeyCode::S => 0x1F,
        KeyCode::T => 0x14,
        KeyCode::U => 0x16,
        KeyCode::V => 0x2F,
        KeyCode::W => 0x11,
        KeyCode::X => 0x2D,
        KeyCode::Y => 0x15,
        KeyCode::Z => 0x2C,

        KeyCode::NUM1 => 0x02,
        KeyCode::NUM2 => 0x03,
        KeyCode::NUM3 => 0x04,
        KeyCode::NUM4 => 0x05,
        KeyCode::NUM5 => 0x06,
        KeyCode::NUM6 => 0x07,
        KeyCode::NUM7 => 0x08,
        KeyCode::NUM8 => 0x09,
        KeyCode::NUM9 => 0x0A,
        KeyCode::NUM0 => 0x0B,

        KeyCode::ENTER => 0x1C,
        KeyCode::ESCAPE => 0x01,
        KeyCode::BACKSPACE => 0x0E,
        KeyCode::TAB => 0x0F,
        KeyCode::SPACE => 0x39,
        KeyCode::MINUS => 0x0C,
        KeyCode::EQUAL => 0x0D,
        KeyCode::LEFT_BRACKET => 0x1A,
        KeyCode::RIGHT_BRACKET => 0x1B,
        KeyCode::BACKSLASH => 0x2B,
        KeyCode::SEMICOLON => 0x27,
        KeyCode::QUOTE => 0x28,
        KeyCode::GRAVE => 0x29,
        KeyCode::COMMA => 0x33,
        KeyCode::PERIOD => 0x34,
        KeyCode::SLASH => 0x35,
        KeyCode::CAPS_LOCK => 0x3A,

        KeyCode::F1 => 0x3B,
        KeyCode::F2 => 0x3C,
        KeyCode::F3 => 0x3D,
        KeyCode::F4 => 0x3E,
        KeyCode::F5 => 0x3F,
        KeyCode::F6 => 0x40,
        KeyCode::F7 => 0x41,
        KeyCode::F8 => 0x42,
        KeyCode::F9 => 0x43,
        KeyCode::F10 => 0x44,
        // F11 and F12 are not contiguous with F1..F10; they were added later.
        KeyCode::F11 => 0x57,
        KeyCode::F12 => 0x58,

        KeyCode::SCROLL_LOCK => 0x46,
        KeyCode::PRINT_SCREEN => 0xE037,
        KeyCode::INSERT => 0xE052,
        KeyCode::HOME => 0xE047,
        KeyCode::PAGE_UP => 0xE049,
        KeyCode::DELETE => 0xE053,
        KeyCode::END => 0xE04F,
        KeyCode::PAGE_DOWN => 0xE051,
        KeyCode::RIGHT => 0xE04D,
        KeyCode::LEFT => 0xE04B,
        KeyCode::DOWN => 0xE050,
        KeyCode::UP => 0xE048,

        KeyCode::LEFT_CONTROL => 0x1D,
        KeyCode::LEFT_SHIFT => 0x2A,
        KeyCode::LEFT_ALT => 0x38,
        KeyCode::LEFT_META => 0xE05B,
        KeyCode::RIGHT_CONTROL => 0xE01D,
        KeyCode::RIGHT_SHIFT => 0x36,
        KeyCode::RIGHT_ALT => 0xE038,
        KeyCode::RIGHT_META => 0xE05C,

        // Pause is 0xE1 0x1D 0x45 — a three-byte sequence with no single-code
        // form, so it cannot be expressed here. Dropped rather than mapped to
        // something that would send the wrong key.
        _ => return None,
    })
}

/// Scan code → HID usage. `extended` is `LLKHF_EXTENDED` from the hook.
pub fn scancode_to_hid(scancode: u16, extended: bool) -> Option<KeyCode> {
    let code = if extended {
        0xE000 | (scancode & 0xFF)
    } else {
        scancode & 0xFF
    };

    Some(match code {
        0x1E => KeyCode::A,
        0x30 => KeyCode::B,
        0x2E => KeyCode::C,
        0x20 => KeyCode::D,
        0x12 => KeyCode::E,
        0x21 => KeyCode::F,
        0x22 => KeyCode::G,
        0x23 => KeyCode::H,
        0x17 => KeyCode::I,
        0x24 => KeyCode::J,
        0x25 => KeyCode::K,
        0x26 => KeyCode::L,
        0x32 => KeyCode::M,
        0x31 => KeyCode::N,
        0x18 => KeyCode::O,
        0x19 => KeyCode::P,
        0x10 => KeyCode::Q,
        0x13 => KeyCode::R,
        0x1F => KeyCode::S,
        0x14 => KeyCode::T,
        0x16 => KeyCode::U,
        0x2F => KeyCode::V,
        0x11 => KeyCode::W,
        0x2D => KeyCode::X,
        0x15 => KeyCode::Y,
        0x2C => KeyCode::Z,

        0x02 => KeyCode::NUM1,
        0x03 => KeyCode::NUM2,
        0x04 => KeyCode::NUM3,
        0x05 => KeyCode::NUM4,
        0x06 => KeyCode::NUM5,
        0x07 => KeyCode::NUM6,
        0x08 => KeyCode::NUM7,
        0x09 => KeyCode::NUM8,
        0x0A => KeyCode::NUM9,
        0x0B => KeyCode::NUM0,

        0x1C => KeyCode::ENTER,
        0x01 => KeyCode::ESCAPE,
        0x0E => KeyCode::BACKSPACE,
        0x0F => KeyCode::TAB,
        0x39 => KeyCode::SPACE,
        0x0C => KeyCode::MINUS,
        0x0D => KeyCode::EQUAL,
        0x1A => KeyCode::LEFT_BRACKET,
        0x1B => KeyCode::RIGHT_BRACKET,
        0x2B => KeyCode::BACKSLASH,
        0x27 => KeyCode::SEMICOLON,
        0x28 => KeyCode::QUOTE,
        0x29 => KeyCode::GRAVE,
        0x33 => KeyCode::COMMA,
        0x34 => KeyCode::PERIOD,
        0x35 => KeyCode::SLASH,
        0x3A => KeyCode::CAPS_LOCK,

        0x3B => KeyCode::F1,
        0x3C => KeyCode::F2,
        0x3D => KeyCode::F3,
        0x3E => KeyCode::F4,
        0x3F => KeyCode::F5,
        0x40 => KeyCode::F6,
        0x41 => KeyCode::F7,
        0x42 => KeyCode::F8,
        0x43 => KeyCode::F9,
        0x44 => KeyCode::F10,
        0x57 => KeyCode::F11,
        0x58 => KeyCode::F12,

        0x46 => KeyCode::SCROLL_LOCK,
        0xE037 => KeyCode::PRINT_SCREEN,
        0xE052 => KeyCode::INSERT,
        0xE047 => KeyCode::HOME,
        0xE049 => KeyCode::PAGE_UP,
        0xE053 => KeyCode::DELETE,
        0xE04F => KeyCode::END,
        0xE051 => KeyCode::PAGE_DOWN,
        0xE04D => KeyCode::RIGHT,
        0xE04B => KeyCode::LEFT,
        0xE050 => KeyCode::DOWN,
        0xE048 => KeyCode::UP,

        0x1D => KeyCode::LEFT_CONTROL,
        0x2A => KeyCode::LEFT_SHIFT,
        0x38 => KeyCode::LEFT_ALT,
        0xE05B => KeyCode::LEFT_META,
        0xE01D => KeyCode::RIGHT_CONTROL,
        0x36 => KeyCode::RIGHT_SHIFT,
        0xE038 => KeyCode::RIGHT_ALT,
        0xE05C => KeyCode::RIGHT_META,

        _ => return None,
    })
}

/// Does this scan code need `KEYEVENTF_EXTENDEDKEY`?
pub fn is_extended(scancode: u16) -> bool {
    scancode & 0xFF00 == 0xE000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_tables_agree() {
        for raw in 0u16..=0xE7 {
            let key = KeyCode(raw);
            if let Some(code) = hid_to_scancode(key) {
                let back = scancode_to_hid(code & 0xFF, is_extended(code));
                assert_eq!(back, Some(key), "mismatch for HID {raw:#04x}");
            }
        }
    }

    #[test]
    fn the_arrow_keys_are_extended() {
        // The non-extended forms of these codes are the numpad keys. Getting
        // this wrong sends numbers instead of cursor movement.
        for key in [KeyCode::LEFT, KeyCode::RIGHT, KeyCode::UP, KeyCode::DOWN] {
            assert!(is_extended(hid_to_scancode(key).unwrap()), "{key:?}");
        }
    }

    #[test]
    fn the_sides_of_a_modifier_pair_differ() {
        assert_ne!(
            hid_to_scancode(KeyCode::LEFT_CONTROL),
            hid_to_scancode(KeyCode::RIGHT_CONTROL)
        );
        assert!(!is_extended(
            hid_to_scancode(KeyCode::LEFT_CONTROL).unwrap()
        ));
        assert!(is_extended(
            hid_to_scancode(KeyCode::RIGHT_CONTROL).unwrap()
        ));
    }

    #[test]
    fn the_function_row_is_not_contiguous() {
        // F11/F12 sit apart from F1..F10; assuming otherwise is an easy bug.
        assert_eq!(hid_to_scancode(KeyCode::F10), Some(0x44));
        assert_eq!(hid_to_scancode(KeyCode::F11), Some(0x57));
    }
}
