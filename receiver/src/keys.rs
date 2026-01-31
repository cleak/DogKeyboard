//! Key preview types and conversion

use dogkbd_proto::{hid_to_us_ansi_char, KeyTap};

/// Preview representation of a key event for display
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyPreview {
    /// A printable character
    Char(char),
    /// Enter key (from keyboard)
    Enter,
    /// Auto-injected Enter key (from idle timeout or periodic timer)
    AutoEnter,
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
            KeyPreview::AutoEnter => "[Auto-Enter]".to_string(),
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
    fn test_key_preview_digits() {
        // Digits are HID 0x1e-0x27 (1-9, 0)
        let tap_1 = KeyTap::new(1, 1, 0, 0x1e); // '1'
        assert_eq!(KeyPreview::from_tap(&tap_1), Some(KeyPreview::Char('1')));

        let tap_0 = KeyTap::new(1, 1, 0, 0x27); // '0'
        assert_eq!(KeyPreview::from_tap(&tap_0), Some(KeyPreview::Char('0')));

        // Shift+1 = '!'
        let tap_exclaim = KeyTap::new(1, 1, 0x01, 0x1e);
        assert_eq!(KeyPreview::from_tap(&tap_exclaim), Some(KeyPreview::Char('!')));
    }

    #[test]
    fn test_key_preview_punctuation() {
        // Test various punctuation keys
        let tap_dash = KeyTap::new(1, 1, 0, 0x2d); // '-'
        assert_eq!(KeyPreview::from_tap(&tap_dash), Some(KeyPreview::Char('-')));

        let tap_period = KeyTap::new(1, 1, 0, 0x37); // '.'
        assert_eq!(KeyPreview::from_tap(&tap_period), Some(KeyPreview::Char('.')));

        let tap_comma = KeyTap::new(1, 1, 0, 0x36); // ','
        assert_eq!(KeyPreview::from_tap(&tap_comma), Some(KeyPreview::Char(',')));
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

    #[test]
    fn test_key_preview_display() {
        assert_eq!(KeyPreview::Char('a').display(), "a");
        assert_eq!(KeyPreview::Char('!').display(), "!");
        assert_eq!(KeyPreview::Enter.display(), "[Enter]");
        assert_eq!(KeyPreview::AutoEnter.display(), "[Auto-Enter]");
        assert_eq!(KeyPreview::Backspace.display(), "[Backspace]");
        assert_eq!(KeyPreview::Space.display(), " ");
    }

    #[test]
    fn test_auto_enter_distinct_from_enter() {
        // AutoEnter should be visually distinct from Enter
        assert_ne!(KeyPreview::Enter.display(), KeyPreview::AutoEnter.display());
        assert!(KeyPreview::AutoEnter.display().contains("Auto"));
    }
}
