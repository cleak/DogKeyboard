//! Linux keystroke injection using uinput

use dogkbd_proto::KeyTap;

/// Convert HID usage code to Linux input event code
fn hid_to_linux_key(hid: u8) -> Option<i32> {
    // Linux KEY_* codes from linux/input-event-codes.h
    let key = match hid {
        // Letters A-Z
        0x04 => 30,  // KEY_A
        0x05 => 48,  // KEY_B
        0x06 => 46,  // KEY_C
        0x07 => 32,  // KEY_D
        0x08 => 18,  // KEY_E
        0x09 => 33,  // KEY_F
        0x0a => 34,  // KEY_G
        0x0b => 35,  // KEY_H
        0x0c => 23,  // KEY_I
        0x0d => 36,  // KEY_J
        0x0e => 37,  // KEY_K
        0x0f => 38,  // KEY_L
        0x10 => 50,  // KEY_M
        0x11 => 49,  // KEY_N
        0x12 => 24,  // KEY_O
        0x13 => 25,  // KEY_P
        0x14 => 16,  // KEY_Q
        0x15 => 19,  // KEY_R
        0x16 => 31,  // KEY_S
        0x17 => 20,  // KEY_T
        0x18 => 22,  // KEY_U
        0x19 => 47,  // KEY_V
        0x1a => 17,  // KEY_W
        0x1b => 45,  // KEY_X
        0x1c => 21,  // KEY_Y
        0x1d => 44,  // KEY_Z
        // Digits 1-0
        0x1e => 2,   // KEY_1
        0x1f => 3,   // KEY_2
        0x20 => 4,   // KEY_3
        0x21 => 5,   // KEY_4
        0x22 => 6,   // KEY_5
        0x23 => 7,   // KEY_6
        0x24 => 8,   // KEY_7
        0x25 => 9,   // KEY_8
        0x26 => 10,  // KEY_9
        0x27 => 11,  // KEY_0
        // Special
        0x28 => 28,  // KEY_ENTER
        0x2a => 14,  // KEY_BACKSPACE
        0x2c => 57,  // KEY_SPACE
        // Punctuation
        0x2d => 12,  // KEY_MINUS
        0x2e => 13,  // KEY_EQUAL
        0x2f => 26,  // KEY_LEFTBRACE
        0x30 => 27,  // KEY_RIGHTBRACE
        0x31 => 43,  // KEY_BACKSLASH
        0x33 => 39,  // KEY_SEMICOLON
        0x34 => 40,  // KEY_APOSTROPHE
        0x35 => 41,  // KEY_GRAVE
        0x36 => 51,  // KEY_COMMA
        0x37 => 52,  // KEY_DOT
        0x38 => 53,  // KEY_SLASH
        _ => return None,
    };
    Some(key)
}

/// Inject a key tap using uinput
///
/// Note: This is a stub implementation. Full implementation requires
/// creating and managing a uinput device.
pub fn inject(tap: &KeyTap) -> Result<(), String> {
    let _key_code = hid_to_linux_key(tap.hid_code)
        .ok_or_else(|| format!("Unknown HID code: {:#x}", tap.hid_code))?;

    // TODO: Implement uinput injection
    // This would require:
    // 1. Creating a uinput device at startup
    // 2. Writing EV_KEY events for press/release
    // 3. Writing EV_SYN to synchronize

    Err("Linux uinput injection not yet implemented".to_string())
}
