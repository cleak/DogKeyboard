//! Key preview types and conversion

use dogkbd_proto::{hid_to_us_ansi_char, KeyTap};

/// Preview representation of a key event for display
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyPreview {
    /// A printable character
    Char(char),
    /// Enter key
    Enter,
    /// Backspace key
    Backspace,
    /// Space key (separate for visibility in preview)
    Space,
}

impl KeyPreview {
    /// Convert a KeyTap to a KeyPreview
    pub fn from_tap(tap: &KeyTap) -> Option<Self> {
        match tap.hid_code {
            0x28 => Some(KeyPreview::Enter),
            0x2a => Some(KeyPreview::Backspace),
            0x2c => Some(KeyPreview::Space),
            _ => hid_to_us_ansi_char(tap.hid_code, tap.shift()).map(KeyPreview::Char),
        }
    }

    /// Get display string for the key
    pub fn display(&self) -> String {
        match self {
            KeyPreview::Char(c) => c.to_string(),
            KeyPreview::Enter => "[Enter]".to_string(),
            KeyPreview::Backspace => "[Backspace]".to_string(),
            KeyPreview::Space => " ".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_preview_char() {
        let tap = KeyTap::new(1, 1, 0, 0x04); // 'a'
        assert_eq!(KeyPreview::from_tap(&tap), Some(KeyPreview::Char('a')));

        let tap_shift = KeyTap::new(1, 1, 0x01, 0x04); // 'A'
        assert_eq!(KeyPreview::from_tap(&tap_shift), Some(KeyPreview::Char('A')));
    }

    #[test]
    fn test_key_preview_enter() {
        let tap = KeyTap::new(1, 1, 0, 0x28);
        assert_eq!(KeyPreview::from_tap(&tap), Some(KeyPreview::Enter));
    }

    #[test]
    fn test_key_preview_backspace() {
        let tap = KeyTap::new(1, 1, 0, 0x2a);
        assert_eq!(KeyPreview::from_tap(&tap), Some(KeyPreview::Backspace));
    }

    #[test]
    fn test_key_preview_space() {
        let tap = KeyTap::new(1, 1, 0, 0x2c);
        assert_eq!(KeyPreview::from_tap(&tap), Some(KeyPreview::Space));
    }
}
