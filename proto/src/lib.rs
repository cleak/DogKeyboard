//! DOGKBD Protocol
//!
//! 16-byte packet format (little-endian):
//! | Offset | Size | Field           |
//! |--------|------|-----------------|
//! | 0      | 4    | `DOGK` magic    |
//! | 4      | 1    | version (1)     |
//! | 5      | 1    | msg_type (1=KeyTap) |
//! | 6      | 4    | device_id       |
//! | 10     | 4    | seq             |
//! | 14     | 1    | mods (bit0=shift) |
//! | 15     | 1    | HID usage code  |

pub const MAGIC: [u8; 4] = *b"DOGK";
pub const VERSION: u8 = 1;
pub const MSG_TYPE_KEYTAP: u8 = 1;
pub const PACKET_SIZE: usize = 16;

pub const MOD_SHIFT: u8 = 0x01;

/// A key tap event (press + release)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyTap {
    pub device_id: u32,
    pub seq: u32,
    pub mods: u8,
    pub hid_code: u8,
}

impl KeyTap {
    /// Create a new KeyTap event
    pub fn new(device_id: u32, seq: u32, mods: u8, hid_code: u8) -> Self {
        Self {
            device_id,
            seq,
            mods,
            hid_code,
        }
    }

    /// Check if shift modifier is active
    pub fn shift(&self) -> bool {
        self.mods & MOD_SHIFT != 0
    }

    /// Encode to 16-byte packet
    pub fn encode(&self) -> [u8; PACKET_SIZE] {
        let mut buf = [0u8; PACKET_SIZE];
        buf[0..4].copy_from_slice(&MAGIC);
        buf[4] = VERSION;
        buf[5] = MSG_TYPE_KEYTAP;
        buf[6..10].copy_from_slice(&self.device_id.to_le_bytes());
        buf[10..14].copy_from_slice(&self.seq.to_le_bytes());
        buf[14] = self.mods;
        buf[15] = self.hid_code;
        buf
    }

    /// Decode from bytes
    pub fn decode(data: &[u8]) -> Result<Self, DecodeError> {
        if data.len() != PACKET_SIZE {
            return Err(DecodeError::WrongSize(data.len()));
        }
        if data[0..4] != MAGIC {
            return Err(DecodeError::InvalidMagic);
        }
        if data[4] != VERSION {
            return Err(DecodeError::InvalidVersion(data[4]));
        }
        if data[5] != MSG_TYPE_KEYTAP {
            return Err(DecodeError::InvalidMsgType(data[5]));
        }

        Ok(Self {
            device_id: u32::from_le_bytes([data[6], data[7], data[8], data[9]]),
            seq: u32::from_le_bytes([data[10], data[11], data[12], data[13]]),
            mods: data[14],
            hid_code: data[15],
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    WrongSize(usize),
    InvalidMagic,
    InvalidVersion(u8),
    InvalidMsgType(u8),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::WrongSize(size) => {
                write!(f, "wrong packet size: expected {PACKET_SIZE}, got {size}")
            }
            DecodeError::InvalidMagic => write!(f, "invalid magic bytes"),
            DecodeError::InvalidVersion(v) => write!(f, "invalid version: {v}"),
            DecodeError::InvalidMsgType(t) => write!(f, "invalid message type: {t}"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Check if an HID usage code is in the allowlist.
///
/// Allowed codes:
/// - 0x04-0x1d: A-Z
/// - 0x1e-0x27: 1-0
/// - 0x28: Enter
/// - 0x2c: Space
/// - 0x2d-0x37: Punctuation (- = [ ] \ ; ' ` , .)
pub fn hid_allowed(code: u8) -> bool {
    matches!(
        code,
        0x04..=0x1d  // A-Z
        | 0x1e..=0x27  // 1-0
        | 0x28  // Enter
        | 0x2c  // Space
        | 0x2d..=0x37  // Punctuation (no slash)
    )
}

/// Convert HID usage code to US ANSI character.
/// Returns None for non-printable keys (Enter, Backspace, etc.)
pub fn hid_to_us_ansi_char(code: u8, shift: bool) -> Option<char> {
    // Letters A-Z (0x04-0x1d)
    if (0x04..=0x1d).contains(&code) {
        let base = (code - 0x04) + b'a';
        return Some(if shift {
            (base - 32) as char // uppercase
        } else {
            base as char
        });
    }

    // Digits 1-9, 0 (0x1e-0x27)
    if (0x1e..=0x27).contains(&code) {
        if shift {
            // Shifted digits: !@#$%^&*()
            let shifted = match code {
                0x1e => '!', // 1
                0x1f => '@', // 2
                0x20 => '#', // 3
                0x21 => '$', // 4
                0x22 => '%', // 5
                0x23 => '^', // 6
                0x24 => '&', // 7
                0x25 => '*', // 8
                0x26 => '(', // 9
                0x27 => ')', // 0
                _ => unreachable!(),
            };
            return Some(shifted);
        } else {
            // Unshifted: digits 1-9, 0
            let digit = if code == 0x27 {
                b'0'
            } else {
                (code - 0x1e) + b'1'
            };
            return Some(digit as char);
        }
    }

    // Space
    if code == 0x2c {
        return Some(' ');
    }

    // Punctuation (0x2d-0x38)
    let (unshifted, shifted) = match code {
        0x2d => ('-', '_'),
        0x2e => ('=', '+'),
        0x2f => ('[', '{'),
        0x30 => (']', '}'),
        0x31 => ('\\', '|'),
        0x32 => ('#', '~'), // Non-US # and ~
        0x33 => (';', ':'),
        0x34 => ('\'', '"'),
        0x35 => ('`', '~'),
        0x36 => (',', '<'),
        0x37 => ('.', '>'),
        0x38 => ('/', '?'),
        // Enter (0x28), Backspace (0x2a) - non-printable
        _ => return None,
    };

    Some(if shift { shifted } else { unshifted })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_decode_roundtrip() {
        let original = KeyTap::new(0x12345678, 42, MOD_SHIFT, 0x04);
        let encoded = original.encode();
        let decoded = KeyTap::decode(&encoded).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_decode_invalid_magic() {
        let mut packet = KeyTap::new(1, 1, 0, 0x04).encode();
        packet[0] = b'X'; // corrupt magic
        assert_eq!(KeyTap::decode(&packet), Err(DecodeError::InvalidMagic));
    }

    #[test]
    fn test_decode_invalid_version() {
        let mut packet = KeyTap::new(1, 1, 0, 0x04).encode();
        packet[4] = 99; // wrong version
        assert_eq!(KeyTap::decode(&packet), Err(DecodeError::InvalidVersion(99)));
    }

    #[test]
    fn test_decode_wrong_size() {
        let short = [0u8; 10];
        assert_eq!(KeyTap::decode(&short), Err(DecodeError::WrongSize(10)));

        let long = [0u8; 20];
        assert_eq!(KeyTap::decode(&long), Err(DecodeError::WrongSize(20)));
    }

    #[test]
    fn test_hid_allowed_letters() {
        // A-Z (0x04-0x1d)
        for code in 0x04..=0x1d {
            assert!(hid_allowed(code), "HID code {:#x} should be allowed", code);
        }
    }

    #[test]
    fn test_hid_allowed_digits() {
        // 1-0 (0x1e-0x27)
        for code in 0x1e..=0x27 {
            assert!(hid_allowed(code), "HID code {:#x} should be allowed", code);
        }
    }

    #[test]
    fn test_hid_allowed_special() {
        assert!(hid_allowed(0x28), "Enter should be allowed");
        assert!(!hid_allowed(0x2a), "Backspace should be blocked");
        assert!(hid_allowed(0x2c), "Space should be allowed");
    }

    #[test]
    fn test_hid_blocked_danger_keys() {
        assert!(!hid_allowed(0x29), "Escape should be blocked");
        assert!(!hid_allowed(0x2b), "Tab should be blocked");
        assert!(!hid_allowed(0x38), "Slash should be blocked");
        assert!(!hid_allowed(0x39), "CapsLock should be blocked");
        assert!(!hid_allowed(0xe0), "Left Ctrl should be blocked");
        assert!(!hid_allowed(0xe1), "Left Shift should be blocked");
        assert!(!hid_allowed(0xe2), "Left Alt should be blocked");
        assert!(!hid_allowed(0xe3), "Left Meta should be blocked");
        // F-keys (0x3a-0x45)
        for code in 0x3a..=0x45 {
            assert!(!hid_allowed(code), "F-key {:#x} should be blocked", code);
        }
    }

    #[test]
    fn test_hid_to_char_letters() {
        // Lowercase
        assert_eq!(hid_to_us_ansi_char(0x04, false), Some('a'));
        assert_eq!(hid_to_us_ansi_char(0x1d, false), Some('z'));

        // Uppercase
        assert_eq!(hid_to_us_ansi_char(0x04, true), Some('A'));
        assert_eq!(hid_to_us_ansi_char(0x1d, true), Some('Z'));
    }

    #[test]
    fn test_hid_to_char_shifted_digits() {
        assert_eq!(hid_to_us_ansi_char(0x1e, true), Some('!')); // 1 -> !
        assert_eq!(hid_to_us_ansi_char(0x1f, true), Some('@')); // 2 -> @
        assert_eq!(hid_to_us_ansi_char(0x20, true), Some('#')); // 3 -> #
        assert_eq!(hid_to_us_ansi_char(0x21, true), Some('$')); // 4 -> $
        assert_eq!(hid_to_us_ansi_char(0x22, true), Some('%')); // 5 -> %
        assert_eq!(hid_to_us_ansi_char(0x23, true), Some('^')); // 6 -> ^
        assert_eq!(hid_to_us_ansi_char(0x24, true), Some('&')); // 7 -> &
        assert_eq!(hid_to_us_ansi_char(0x25, true), Some('*')); // 8 -> *
        assert_eq!(hid_to_us_ansi_char(0x26, true), Some('(')); // 9 -> (
        assert_eq!(hid_to_us_ansi_char(0x27, true), Some(')')); // 0 -> )
    }

    #[test]
    fn test_hid_to_char_punctuation() {
        // Unshifted
        assert_eq!(hid_to_us_ansi_char(0x2d, false), Some('-'));
        assert_eq!(hid_to_us_ansi_char(0x2e, false), Some('='));
        assert_eq!(hid_to_us_ansi_char(0x2f, false), Some('['));
        assert_eq!(hid_to_us_ansi_char(0x30, false), Some(']'));
        assert_eq!(hid_to_us_ansi_char(0x31, false), Some('\\'));
        assert_eq!(hid_to_us_ansi_char(0x33, false), Some(';'));
        assert_eq!(hid_to_us_ansi_char(0x34, false), Some('\''));
        assert_eq!(hid_to_us_ansi_char(0x35, false), Some('`'));
        assert_eq!(hid_to_us_ansi_char(0x36, false), Some(','));
        assert_eq!(hid_to_us_ansi_char(0x37, false), Some('.'));

        // Shifted
        assert_eq!(hid_to_us_ansi_char(0x2d, true), Some('_'));
        assert_eq!(hid_to_us_ansi_char(0x2e, true), Some('+'));
        assert_eq!(hid_to_us_ansi_char(0x2f, true), Some('{'));
        assert_eq!(hid_to_us_ansi_char(0x30, true), Some('}'));
        assert_eq!(hid_to_us_ansi_char(0x31, true), Some('|'));
        assert_eq!(hid_to_us_ansi_char(0x33, true), Some(':'));
        assert_eq!(hid_to_us_ansi_char(0x34, true), Some('"'));
        assert_eq!(hid_to_us_ansi_char(0x35, true), Some('~'));
        assert_eq!(hid_to_us_ansi_char(0x36, true), Some('<'));
        assert_eq!(hid_to_us_ansi_char(0x37, true), Some('>'));
    }

    #[test]
    fn test_hid_to_char_non_printable() {
        assert_eq!(hid_to_us_ansi_char(0x28, false), None); // Enter
        assert_eq!(hid_to_us_ansi_char(0x2a, false), None); // Backspace
    }

    #[test]
    fn test_shift_modifier() {
        let tap = KeyTap::new(1, 1, MOD_SHIFT, 0x04);
        assert!(tap.shift());

        let tap_no_shift = KeyTap::new(1, 1, 0, 0x04);
        assert!(!tap_no_shift.shift());
    }
}
