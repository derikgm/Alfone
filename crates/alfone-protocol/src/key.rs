//! Canonical key identifiers.
//!
//! Alfone uses **X11 keysyms** as the platform-neutral key identity, the same
//! idea Input Leap uses. A keysym describes the logical key (`a`, `Return`,
//! `Left`) independent of the physical keyboard. Physical identity travels
//! separately as a USB HID usage code so that a key release still matches its
//! press even if the layout changed.

/// X11 keysym.
pub type Keysym = u32;

pub const BACKSPACE: Keysym = 0xff08;
pub const TAB: Keysym = 0xff09;
pub const LINEFEED: Keysym = 0xff0a;
pub const CLEAR: Keysym = 0xff0b;
pub const RETURN: Keysym = 0xff0d;
pub const PAUSE: Keysym = 0xff13;
pub const SCROLL_LOCK: Keysym = 0xff14;
pub const SYS_REQ: Keysym = 0xff15;
pub const ESCAPE: Keysym = 0xff1b;
pub const DELETE: Keysym = 0xffff;

pub const HOME: Keysym = 0xff50;
pub const LEFT: Keysym = 0xff51;
pub const UP: Keysym = 0xff52;
pub const RIGHT: Keysym = 0xff53;
pub const DOWN: Keysym = 0xff54;
pub const PAGE_UP: Keysym = 0xff55;
pub const PAGE_DOWN: Keysym = 0xff56;
pub const END: Keysym = 0xff57;
pub const BEGIN: Keysym = 0xff58;
pub const INSERT: Keysym = 0xff63;
pub const MENU: Keysym = 0xff67;
pub const NUM_LOCK: Keysym = 0xff7f;

pub const KP_ENTER: Keysym = 0xff8d;
pub const KP_HOME: Keysym = 0xff95;
pub const KP_LEFT: Keysym = 0xff96;
pub const KP_UP: Keysym = 0xff97;
pub const KP_RIGHT: Keysym = 0xff98;
pub const KP_DOWN: Keysym = 0xff99;
pub const KP_PAGE_UP: Keysym = 0xff9a;
pub const KP_PAGE_DOWN: Keysym = 0xff9b;
pub const KP_END: Keysym = 0xff9c;
pub const KP_BEGIN: Keysym = 0xff9d;
pub const KP_INSERT: Keysym = 0xff9e;
pub const KP_DELETE: Keysym = 0xff9f;

pub const F1: Keysym = 0xffbe;
pub const F2: Keysym = 0xffbf;
pub const F3: Keysym = 0xffc0;
pub const F4: Keysym = 0xffc1;
pub const F5: Keysym = 0xffc2;
pub const F6: Keysym = 0xffc3;
pub const F7: Keysym = 0xffc4;
pub const F8: Keysym = 0xffc5;
pub const F9: Keysym = 0xffc6;
pub const F10: Keysym = 0xffc7;
pub const F11: Keysym = 0xffc8;
pub const F12: Keysym = 0xffc9;

/// `Print` (PrtSc) keysym.
pub const PRINT: Keysym = 0xff61;

/// Keypad digits and arithmetic operators (`0xffb0`–`0xffb9`).
pub const KP_0: Keysym = 0xffb0;
pub const KP_1: Keysym = 0xffb1;
pub const KP_2: Keysym = 0xffb2;
pub const KP_3: Keysym = 0xffb3;
pub const KP_4: Keysym = 0xffb4;
pub const KP_5: Keysym = 0xffb5;
pub const KP_6: Keysym = 0xffb6;
pub const KP_7: Keysym = 0xffb7;
pub const KP_8: Keysym = 0xffb8;
pub const KP_9: Keysym = 0xffb9;
pub const KP_MULTIPLY: Keysym = 0xffaa;
pub const KP_ADD: Keysym = 0xffab;
pub const KP_SUBTRACT: Keysym = 0xffad;
pub const KP_DECIMAL: Keysym = 0xffae;
pub const KP_DIVIDE: Keysym = 0xffaf;

pub const SHIFT_L: Keysym = 0xffe1;
pub const SHIFT_R: Keysym = 0xffe2;
pub const CONTROL_L: Keysym = 0xffe3;
pub const CONTROL_R: Keysym = 0xffe4;
pub const CAPS_LOCK: Keysym = 0xffe5;
pub const SHIFT_LOCK: Keysym = 0xffe6;
pub const META_L: Keysym = 0xffe7;
pub const META_R: Keysym = 0xffe8;
pub const ALT_L: Keysym = 0xffe9;
pub const ALT_R: Keysym = 0xffea;
pub const SUPER_L: Keysym = 0xffeb;
pub const SUPER_R: Keysym = 0xffec;
pub const HYPER_L: Keysym = 0xffed;
pub const HYPER_R: Keysym = 0xffee;

pub const SPACE: Keysym = 0x20;
pub const APOSTROPHE: Keysym = 0x27;
pub const COMMA: Keysym = 0x2c;
pub const MINUS: Keysym = 0x2d;
pub const PERIOD: Keysym = 0x2e;
pub const SLASH: Keysym = 0x2f;
pub const SEMICOLON: Keysym = 0x3b;
pub const EQUAL: Keysym = 0x3d;
pub const BRACKET_LEFT: Keysym = 0x5b;
pub const BACKSLASH: Keysym = 0x5c;
pub const BRACKET_RIGHT: Keysym = 0x5d;
pub const GRAVE: Keysym = 0x60;

/// X11 encodes non-Latin-1 characters as `0x01000000 | codepoint`.
pub fn from_char(c: char) -> Keysym {
    let cp = c as u32;
    if cp <= 0xff {
        cp
    } else {
        0x0100_0000 | cp
    }
}

/// Inverse of [`from_char`]. Returns `None` for non-printable keysyms.
pub fn to_char(keysym: Keysym) -> Option<char> {
    let cp = if keysym & 0xff00_0000 == 0x0100_0000 {
        keysym & 0x00ff_ffff
    } else if (0x20..=0x7e).contains(&keysym) || (0xa0..=0xff).contains(&keysym) {
        keysym
    } else {
        return None;
    };
    char::from_u32(cp)
}

/// True for keysyms that represent a modifier rather than a character.
pub fn is_modifier(keysym: Keysym) -> bool {
    matches!(
        keysym,
        SHIFT_L
            | SHIFT_R
            | CONTROL_L
            | CONTROL_R
            | ALT_L
            | ALT_R
            | META_L
            | META_R
            | SUPER_L
            | SUPER_R
            | HYPER_L
            | HYPER_R
            | CAPS_LOCK
            | SHIFT_LOCK
            | NUM_LOCK
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_and_unicode_roundtrip() {
        for c in ['a', 'Z', '0', ' ', 'ñ', '€', '😀'] {
            assert_eq!(to_char(from_char(c)), Some(c));
        }
    }

    #[test]
    fn control_keys_are_not_text() {
        assert_eq!(to_char(RETURN), None);
        assert_eq!(to_char(F5), None);
        assert!(is_modifier(CONTROL_L));
        assert!(!is_modifier(SPACE));
    }
}
