//! USB HID usage ⇄ Linux input event code.
//!
//! Linux codes are positional, like the HID usages they carry, but numbered
//! from a different starting point and in typewriter row order rather than
//! alphabetically — `KEY_Q` is 16 because it is the first key of the top
//! letter row. There is no arithmetic relation to exploit, so the table is
//! written out both ways, the same as the other two backends.
//!
//! The keypad is absent from both sides: the protocol has no keypad usages, so
//! there is nothing to map them to. A keypad press is dropped rather than
//! guessed at, because guessing turns the numeric 4 into a left-arrow.

use tether_proto::KeyCode;

/// HID usage → Linux event code. `None` for keys Linux has no code for.
pub fn hid_to_key(key: KeyCode) -> Option<u16> {
    Some(match key {
        KeyCode::ESCAPE => 1,
        KeyCode::NUM1 => 2,
        KeyCode::NUM2 => 3,
        KeyCode::NUM3 => 4,
        KeyCode::NUM4 => 5,
        KeyCode::NUM5 => 6,
        KeyCode::NUM6 => 7,
        KeyCode::NUM7 => 8,
        KeyCode::NUM8 => 9,
        KeyCode::NUM9 => 10,
        KeyCode::NUM0 => 11,
        KeyCode::MINUS => 12,
        KeyCode::EQUAL => 13,
        KeyCode::BACKSPACE => 14,
        KeyCode::TAB => 15,
        KeyCode::Q => 16,
        KeyCode::W => 17,
        KeyCode::E => 18,
        KeyCode::R => 19,
        KeyCode::T => 20,
        KeyCode::Y => 21,
        KeyCode::U => 22,
        KeyCode::I => 23,
        KeyCode::O => 24,
        KeyCode::P => 25,
        KeyCode::LEFT_BRACKET => 26,
        KeyCode::RIGHT_BRACKET => 27,
        KeyCode::ENTER => 28,
        KeyCode::LEFT_CONTROL => 29,
        KeyCode::A => 30,
        KeyCode::S => 31,
        KeyCode::D => 32,
        KeyCode::F => 33,
        KeyCode::G => 34,
        KeyCode::H => 35,
        KeyCode::J => 36,
        KeyCode::K => 37,
        KeyCode::L => 38,
        KeyCode::SEMICOLON => 39,
        KeyCode::QUOTE => 40,
        KeyCode::GRAVE => 41,
        KeyCode::LEFT_SHIFT => 42,
        KeyCode::BACKSLASH => 43,
        KeyCode::Z => 44,
        KeyCode::X => 45,
        KeyCode::C => 46,
        KeyCode::V => 47,
        KeyCode::B => 48,
        KeyCode::N => 49,
        KeyCode::M => 50,
        KeyCode::COMMA => 51,
        KeyCode::PERIOD => 52,
        KeyCode::SLASH => 53,
        KeyCode::RIGHT_SHIFT => 54,
        KeyCode::LEFT_ALT => 56,
        KeyCode::SPACE => 57,
        KeyCode::CAPS_LOCK => 58,
        KeyCode::F1 => 59,
        KeyCode::F2 => 60,
        KeyCode::F3 => 61,
        KeyCode::F4 => 62,
        KeyCode::F5 => 63,
        KeyCode::F6 => 64,
        KeyCode::F7 => 65,
        KeyCode::F8 => 66,
        KeyCode::F9 => 67,
        KeyCode::F10 => 68,
        KeyCode::SCROLL_LOCK => 70,
        KeyCode::F11 => 87,
        KeyCode::F12 => 88,
        KeyCode::RIGHT_CONTROL => 97,
        KeyCode::PRINT_SCREEN => 99,
        KeyCode::RIGHT_ALT => 100,
        KeyCode::HOME => 102,
        KeyCode::UP => 103,
        KeyCode::PAGE_UP => 104,
        KeyCode::LEFT => 105,
        KeyCode::RIGHT => 106,
        KeyCode::END => 107,
        KeyCode::DOWN => 108,
        KeyCode::PAGE_DOWN => 109,
        KeyCode::INSERT => 110,
        KeyCode::DELETE => 111,
        KeyCode::PAUSE => 119,
        KeyCode::LEFT_META => 125,
        KeyCode::RIGHT_META => 126,
        _ => return None,
    })
}

/// Linux event code → HID usage. `None` for anything the protocol has no
/// usage for, which is most of the extended set — media keys, the keypad, and
/// the vendor buttons laptops put above the function row.
pub fn key_to_hid(code: u16) -> Option<KeyCode> {
    Some(match code {
        1 => KeyCode::ESCAPE,
        2 => KeyCode::NUM1,
        3 => KeyCode::NUM2,
        4 => KeyCode::NUM3,
        5 => KeyCode::NUM4,
        6 => KeyCode::NUM5,
        7 => KeyCode::NUM6,
        8 => KeyCode::NUM7,
        9 => KeyCode::NUM8,
        10 => KeyCode::NUM9,
        11 => KeyCode::NUM0,
        12 => KeyCode::MINUS,
        13 => KeyCode::EQUAL,
        14 => KeyCode::BACKSPACE,
        15 => KeyCode::TAB,
        16 => KeyCode::Q,
        17 => KeyCode::W,
        18 => KeyCode::E,
        19 => KeyCode::R,
        20 => KeyCode::T,
        21 => KeyCode::Y,
        22 => KeyCode::U,
        23 => KeyCode::I,
        24 => KeyCode::O,
        25 => KeyCode::P,
        26 => KeyCode::LEFT_BRACKET,
        27 => KeyCode::RIGHT_BRACKET,
        28 => KeyCode::ENTER,
        29 => KeyCode::LEFT_CONTROL,
        30 => KeyCode::A,
        31 => KeyCode::S,
        32 => KeyCode::D,
        33 => KeyCode::F,
        34 => KeyCode::G,
        35 => KeyCode::H,
        36 => KeyCode::J,
        37 => KeyCode::K,
        38 => KeyCode::L,
        39 => KeyCode::SEMICOLON,
        40 => KeyCode::QUOTE,
        41 => KeyCode::GRAVE,
        42 => KeyCode::LEFT_SHIFT,
        43 => KeyCode::BACKSLASH,
        44 => KeyCode::Z,
        45 => KeyCode::X,
        46 => KeyCode::C,
        47 => KeyCode::V,
        48 => KeyCode::B,
        49 => KeyCode::N,
        50 => KeyCode::M,
        51 => KeyCode::COMMA,
        52 => KeyCode::PERIOD,
        53 => KeyCode::SLASH,
        54 => KeyCode::RIGHT_SHIFT,
        56 => KeyCode::LEFT_ALT,
        57 => KeyCode::SPACE,
        58 => KeyCode::CAPS_LOCK,
        59 => KeyCode::F1,
        60 => KeyCode::F2,
        61 => KeyCode::F3,
        62 => KeyCode::F4,
        63 => KeyCode::F5,
        64 => KeyCode::F6,
        65 => KeyCode::F7,
        66 => KeyCode::F8,
        67 => KeyCode::F9,
        68 => KeyCode::F10,
        70 => KeyCode::SCROLL_LOCK,
        87 => KeyCode::F11,
        88 => KeyCode::F12,
        97 => KeyCode::RIGHT_CONTROL,
        99 => KeyCode::PRINT_SCREEN,
        100 => KeyCode::RIGHT_ALT,
        102 => KeyCode::HOME,
        103 => KeyCode::UP,
        104 => KeyCode::PAGE_UP,
        105 => KeyCode::LEFT,
        106 => KeyCode::RIGHT,
        107 => KeyCode::END,
        108 => KeyCode::DOWN,
        109 => KeyCode::PAGE_DOWN,
        110 => KeyCode::INSERT,
        111 => KeyCode::DELETE,
        119 => KeyCode::PAUSE,
        125 => KeyCode::LEFT_META,
        126 => KeyCode::RIGHT_META,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_round_trips() {
        // Every usage the protocol defines that Linux knows about must come
        // back as itself. A one-sided edit here is how a keyboard ends up
        // typing the wrong letter on one machine only.
        for usage in 0u16..=0xE7 {
            let key = KeyCode(usage);
            if let Some(code) = hid_to_key(key) {
                assert_eq!(
                    key_to_hid(code),
                    Some(key),
                    "usage {usage:#04x} maps to Linux code {code} which maps back elsewhere"
                );
            }
        }
    }

    #[test]
    fn the_reverse_table_round_trips() {
        for code in 0u16..=255 {
            if let Some(key) = key_to_hid(code) {
                assert_eq!(
                    hid_to_key(key),
                    Some(code),
                    "Linux code {code} maps to a usage that maps back elsewhere"
                );
            }
        }
    }

    #[test]
    fn the_letter_rows_are_where_linux_puts_them() {
        // Spot-check the two starting points that make this table look wrong
        // at a glance: Q begins the top letter row, A the home row.
        assert_eq!(hid_to_key(KeyCode::Q), Some(16));
        assert_eq!(hid_to_key(KeyCode::A), Some(30));
        assert_eq!(hid_to_key(KeyCode::Z), Some(44));
    }

    #[test]
    fn modifiers_survive_the_trip() {
        for key in [
            KeyCode::LEFT_CONTROL,
            KeyCode::RIGHT_CONTROL,
            KeyCode::LEFT_SHIFT,
            KeyCode::RIGHT_SHIFT,
            KeyCode::LEFT_ALT,
            KeyCode::RIGHT_ALT,
            KeyCode::LEFT_META,
            KeyCode::RIGHT_META,
        ] {
            let code = hid_to_key(key).expect("modifier has no Linux code");
            assert_eq!(key_to_hid(code), Some(key));
        }
    }
}
