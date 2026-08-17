//! USB HID usage ⇄ macOS virtual keycode.
//!
//! macOS virtual keycodes are positional (they date from the ADB keyboard) and
//! map cleanly onto HID usages, but the numbering is scrambled — `kVK_ANSI_A`
//! is 0, `kVK_ANSI_S` is 1. There is no arithmetic relation to exploit, so the
//! table is written out both ways.

use tether_proto::KeyCode;

/// HID usage → macOS virtual keycode. `None` for keys macOS has no code for.
pub fn hid_to_vk(key: KeyCode) -> Option<u16> {
    Some(match key {
        KeyCode::A => 0x00,
        KeyCode::S => 0x01,
        KeyCode::D => 0x02,
        KeyCode::F => 0x03,
        KeyCode::H => 0x04,
        KeyCode::G => 0x05,
        KeyCode::Z => 0x06,
        KeyCode::X => 0x07,
        KeyCode::C => 0x08,
        KeyCode::V => 0x09,
        KeyCode::B => 0x0B,
        KeyCode::Q => 0x0C,
        KeyCode::W => 0x0D,
        KeyCode::E => 0x0E,
        KeyCode::R => 0x0F,
        KeyCode::Y => 0x10,
        KeyCode::T => 0x11,
        KeyCode::NUM1 => 0x12,
        KeyCode::NUM2 => 0x13,
        KeyCode::NUM3 => 0x14,
        KeyCode::NUM4 => 0x15,
        KeyCode::NUM6 => 0x16,
        KeyCode::NUM5 => 0x17,
        KeyCode::EQUAL => 0x18,
        KeyCode::NUM9 => 0x19,
        KeyCode::NUM7 => 0x1A,
        KeyCode::MINUS => 0x1B,
        KeyCode::NUM8 => 0x1C,
        KeyCode::NUM0 => 0x1D,
        KeyCode::RIGHT_BRACKET => 0x1E,
        KeyCode::O => 0x1F,
        KeyCode::U => 0x20,
        KeyCode::LEFT_BRACKET => 0x21,
        KeyCode::I => 0x22,
        KeyCode::P => 0x23,
        KeyCode::ENTER => 0x24,
        KeyCode::L => 0x25,
        KeyCode::J => 0x26,
        KeyCode::QUOTE => 0x27,
        KeyCode::K => 0x28,
        KeyCode::SEMICOLON => 0x29,
        KeyCode::BACKSLASH => 0x2A,
        KeyCode::COMMA => 0x2B,
        KeyCode::SLASH => 0x2C,
        KeyCode::N => 0x2D,
        KeyCode::M => 0x2E,
        KeyCode::PERIOD => 0x2F,
        KeyCode::TAB => 0x30,
        KeyCode::SPACE => 0x31,
        KeyCode::GRAVE => 0x32,
        KeyCode::BACKSPACE => 0x33,
        KeyCode::ESCAPE => 0x35,
        KeyCode::RIGHT_META => 0x36,
        KeyCode::LEFT_META => 0x37,
        KeyCode::LEFT_SHIFT => 0x38,
        KeyCode::CAPS_LOCK => 0x39,
        KeyCode::LEFT_ALT => 0x3A,
        KeyCode::LEFT_CONTROL => 0x3B,
        KeyCode::RIGHT_SHIFT => 0x3C,
        KeyCode::RIGHT_ALT => 0x3D,
        KeyCode::RIGHT_CONTROL => 0x3E,
        KeyCode::F5 => 0x60,
        KeyCode::F6 => 0x61,
        KeyCode::F7 => 0x62,
        KeyCode::F3 => 0x63,
        KeyCode::F8 => 0x64,
        KeyCode::F9 => 0x65,
        KeyCode::F11 => 0x67,
        KeyCode::F10 => 0x6D,
        KeyCode::F12 => 0x6F,
        KeyCode::INSERT => 0x72,
        KeyCode::HOME => 0x73,
        KeyCode::PAGE_UP => 0x74,
        KeyCode::DELETE => 0x75,
        KeyCode::F4 => 0x76,
        KeyCode::END => 0x77,
        KeyCode::F2 => 0x78,
        KeyCode::PAGE_DOWN => 0x79,
        KeyCode::F1 => 0x7A,
        KeyCode::LEFT => 0x7B,
        KeyCode::RIGHT => 0x7C,
        KeyCode::DOWN => 0x7D,
        KeyCode::UP => 0x7E,
        _ => return None,
    })
}

/// macOS virtual keycode → HID usage.
pub fn vk_to_hid(vk: u16) -> Option<KeyCode> {
    Some(match vk {
        0x00 => KeyCode::A,
        0x01 => KeyCode::S,
        0x02 => KeyCode::D,
        0x03 => KeyCode::F,
        0x04 => KeyCode::H,
        0x05 => KeyCode::G,
        0x06 => KeyCode::Z,
        0x07 => KeyCode::X,
        0x08 => KeyCode::C,
        0x09 => KeyCode::V,
        0x0B => KeyCode::B,
        0x0C => KeyCode::Q,
        0x0D => KeyCode::W,
        0x0E => KeyCode::E,
        0x0F => KeyCode::R,
        0x10 => KeyCode::Y,
        0x11 => KeyCode::T,
        0x12 => KeyCode::NUM1,
        0x13 => KeyCode::NUM2,
        0x14 => KeyCode::NUM3,
        0x15 => KeyCode::NUM4,
        0x16 => KeyCode::NUM6,
        0x17 => KeyCode::NUM5,
        0x18 => KeyCode::EQUAL,
        0x19 => KeyCode::NUM9,
        0x1A => KeyCode::NUM7,
        0x1B => KeyCode::MINUS,
        0x1C => KeyCode::NUM8,
        0x1D => KeyCode::NUM0,
        0x1E => KeyCode::RIGHT_BRACKET,
        0x1F => KeyCode::O,
        0x20 => KeyCode::U,
        0x21 => KeyCode::LEFT_BRACKET,
        0x22 => KeyCode::I,
        0x23 => KeyCode::P,
        0x24 => KeyCode::ENTER,
        0x25 => KeyCode::L,
        0x26 => KeyCode::J,
        0x27 => KeyCode::QUOTE,
        0x28 => KeyCode::K,
        0x29 => KeyCode::SEMICOLON,
        0x2A => KeyCode::BACKSLASH,
        0x2B => KeyCode::COMMA,
        0x2C => KeyCode::SLASH,
        0x2D => KeyCode::N,
        0x2E => KeyCode::M,
        0x2F => KeyCode::PERIOD,
        0x30 => KeyCode::TAB,
        0x31 => KeyCode::SPACE,
        0x32 => KeyCode::GRAVE,
        0x33 => KeyCode::BACKSPACE,
        0x35 => KeyCode::ESCAPE,
        0x36 => KeyCode::RIGHT_META,
        0x37 => KeyCode::LEFT_META,
        0x38 => KeyCode::LEFT_SHIFT,
        0x39 => KeyCode::CAPS_LOCK,
        0x3A => KeyCode::LEFT_ALT,
        0x3B => KeyCode::LEFT_CONTROL,
        0x3C => KeyCode::RIGHT_SHIFT,
        0x3D => KeyCode::RIGHT_ALT,
        0x3E => KeyCode::RIGHT_CONTROL,
        0x60 => KeyCode::F5,
        0x61 => KeyCode::F6,
        0x62 => KeyCode::F7,
        0x63 => KeyCode::F3,
        0x64 => KeyCode::F8,
        0x65 => KeyCode::F9,
        0x67 => KeyCode::F11,
        0x6D => KeyCode::F10,
        0x6F => KeyCode::F12,
        0x72 => KeyCode::INSERT,
        0x73 => KeyCode::HOME,
        0x74 => KeyCode::PAGE_UP,
        0x75 => KeyCode::DELETE,
        0x76 => KeyCode::F4,
        0x77 => KeyCode::END,
        0x78 => KeyCode::F2,
        0x79 => KeyCode::PAGE_DOWN,
        0x7A => KeyCode::F1,
        0x7B => KeyCode::LEFT,
        0x7C => KeyCode::RIGHT,
        0x7D => KeyCode::DOWN,
        0x7E => KeyCode::UP,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_tables_agree() {
        // Every HID→VK entry must come back as the same HID usage. A one-sided
        // edit here shows up as a key that types the wrong character on one
        // machine only, which is a horrible bug to chase.
        for raw in 0u16..=0xE7 {
            let key = KeyCode(raw);
            if let Some(vk) = hid_to_vk(key) {
                assert_eq!(vk_to_hid(vk), Some(key), "mismatch for HID {raw:#04x}");
            }
        }
    }

    #[test]
    fn number_row_transposition_is_preserved() {
        // macOS really does put 6 before 5. Guard the easy typo.
        assert_eq!(hid_to_vk(KeyCode::NUM5), Some(0x17));
        assert_eq!(hid_to_vk(KeyCode::NUM6), Some(0x16));
    }

    #[test]
    fn modifiers_map_to_their_sided_codes() {
        assert_eq!(hid_to_vk(KeyCode::LEFT_META), Some(0x37));
        assert_eq!(hid_to_vk(KeyCode::RIGHT_META), Some(0x36));
        assert_eq!(vk_to_hid(0x3E), Some(KeyCode::RIGHT_CONTROL));
    }
}
