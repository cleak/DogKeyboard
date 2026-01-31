//! Windows keystroke injection using SendInput

use dogkbd_proto::KeyTap;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE,
};

/// Convert HID usage code to Windows scan code
fn hid_to_scancode(hid: u8) -> Option<u16> {
    let sc = match hid {
        // Letters A-Z (HID 0x04-0x1d -> scan codes vary)
        0x04 => 0x1e, // A
        0x05 => 0x30, // B
        0x06 => 0x2e, // C
        0x07 => 0x20, // D
        0x08 => 0x12, // E
        0x09 => 0x21, // F
        0x0a => 0x22, // G
        0x0b => 0x23, // H
        0x0c => 0x17, // I
        0x0d => 0x24, // J
        0x0e => 0x25, // K
        0x0f => 0x26, // L
        0x10 => 0x32, // M
        0x11 => 0x31, // N
        0x12 => 0x18, // O
        0x13 => 0x19, // P
        0x14 => 0x10, // Q
        0x15 => 0x13, // R
        0x16 => 0x1f, // S
        0x17 => 0x14, // T
        0x18 => 0x16, // U
        0x19 => 0x2f, // V
        0x1a => 0x11, // W
        0x1b => 0x2d, // X
        0x1c => 0x15, // Y
        0x1d => 0x2c, // Z
        // Digits 1-0
        0x1e => 0x02, // 1
        0x1f => 0x03, // 2
        0x20 => 0x04, // 3
        0x21 => 0x05, // 4
        0x22 => 0x06, // 5
        0x23 => 0x07, // 6
        0x24 => 0x08, // 7
        0x25 => 0x09, // 8
        0x26 => 0x0a, // 9
        0x27 => 0x0b, // 0
        // Special
        0x28 => 0x1c, // Enter
        0x2a => 0x0e, // Backspace
        0x2c => 0x39, // Space
        // Punctuation
        0x2d => 0x0c, // -
        0x2e => 0x0d, // =
        0x2f => 0x1a, // [
        0x30 => 0x1b, // ]
        0x31 => 0x2b, // \
        0x33 => 0x27, // ;
        0x34 => 0x28, // '
        0x35 => 0x29, // `
        0x36 => 0x33, // ,
        0x37 => 0x34, // .
        0x38 => 0x35, // /
        _ => return None,
    };
    Some(sc)
}

/// Inject a key tap using SendInput with scan codes
pub fn inject(tap: &KeyTap) -> Result<(), String> {
    let scancode = hid_to_scancode(tap.hid_code)
        .ok_or_else(|| format!("Unknown HID code: {:#x}", tap.hid_code))?;

    let shift = tap.shift();

    // Build input sequence
    let mut inputs: Vec<INPUT> = Vec::with_capacity(4);

    // Press shift if needed
    if shift {
        inputs.push(make_key_input(0x2a, false)); // Left shift scan code
    }

    // Press key
    inputs.push(make_key_input(scancode, false));

    // Release key
    inputs.push(make_key_input(scancode, true));

    // Release shift if needed
    if shift {
        inputs.push(make_key_input(0x2a, true));
    }

    // Send all inputs
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };

    if sent as usize != inputs.len() {
        return Err(format!(
            "SendInput failed: sent {} of {} inputs",
            sent,
            inputs.len()
        ));
    }

    Ok(())
}

fn make_key_input(scancode: u16, keyup: bool) -> INPUT {
    let mut flags = KEYEVENTF_SCANCODE;
    if keyup {
        flags |= KEYEVENTF_KEYUP;
    }

    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: Default::default(),
                wScan: scancode,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}
