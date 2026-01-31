//! DOGKBD Sender
//!
//! Reads keystrokes from a USB keyboard via evdev, filters through allowlist,
//! and broadcasts KeyTap packets over UDP.
//!
//! This crate only works on Linux (requires evdev).

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("dogkbd-sender only works on Linux (requires evdev)");
    std::process::exit(1);
}

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    linux_main::run()
}

#[cfg(target_os = "linux")]
mod linux_main {
    use clap::Parser;
    use dogkbd_proto::{hid_allowed, KeyTap, MOD_SHIFT};
    use evdev::{Device, InputEventKind, Key};
    use std::net::UdpSocket;
    use std::path::PathBuf;

    /// DOGKBD sender daemon for Raspberry Pi
    #[derive(Parser, Debug)]
    #[command(name = "dogkbd-sender")]
    #[command(about = "Captures keystrokes and broadcasts them over UDP")]
    struct Args {
        /// Path to the evdev device (e.g., /dev/input/dogkbd)
        #[arg(short, long, default_value = "/dev/input/dogkbd")]
        device: PathBuf,

        /// UDP port to broadcast on
        #[arg(short, long, default_value_t = 44555)]
        port: u16,

        /// Broadcast address
        #[arg(short, long, default_value = "255.255.255.255")]
        broadcast: String,

        /// Number of times to send each packet (for UDP reliability)
        #[arg(long, default_value_t = 2)]
        duplicate: u8,
    }

    /// Convert evdev Key to HID usage code
    fn evdev_to_hid(key: Key) -> Option<u8> {
        let code = match key {
            // Letters A-Z
            Key::KEY_A => 0x04,
            Key::KEY_B => 0x05,
            Key::KEY_C => 0x06,
            Key::KEY_D => 0x07,
            Key::KEY_E => 0x08,
            Key::KEY_F => 0x09,
            Key::KEY_G => 0x0a,
            Key::KEY_H => 0x0b,
            Key::KEY_I => 0x0c,
            Key::KEY_J => 0x0d,
            Key::KEY_K => 0x0e,
            Key::KEY_L => 0x0f,
            Key::KEY_M => 0x10,
            Key::KEY_N => 0x11,
            Key::KEY_O => 0x12,
            Key::KEY_P => 0x13,
            Key::KEY_Q => 0x14,
            Key::KEY_R => 0x15,
            Key::KEY_S => 0x16,
            Key::KEY_T => 0x17,
            Key::KEY_U => 0x18,
            Key::KEY_V => 0x19,
            Key::KEY_W => 0x1a,
            Key::KEY_X => 0x1b,
            Key::KEY_Y => 0x1c,
            Key::KEY_Z => 0x1d,
            // Digits 1-0
            Key::KEY_1 => 0x1e,
            Key::KEY_2 => 0x1f,
            Key::KEY_3 => 0x20,
            Key::KEY_4 => 0x21,
            Key::KEY_5 => 0x22,
            Key::KEY_6 => 0x23,
            Key::KEY_7 => 0x24,
            Key::KEY_8 => 0x25,
            Key::KEY_9 => 0x26,
            Key::KEY_0 => 0x27,
            // Special keys
            Key::KEY_ENTER => 0x28,
            Key::KEY_BACKSPACE => 0x2a,
            Key::KEY_SPACE => 0x2c,
            // Punctuation
            Key::KEY_MINUS => 0x2d,
            Key::KEY_EQUAL => 0x2e,
            Key::KEY_LEFTBRACE => 0x2f,
            Key::KEY_RIGHTBRACE => 0x30,
            Key::KEY_BACKSLASH => 0x31,
            Key::KEY_SEMICOLON => 0x33,
            Key::KEY_APOSTROPHE => 0x34,
            Key::KEY_GRAVE => 0x35,
            Key::KEY_COMMA => 0x36,
            Key::KEY_DOT => 0x37,
            Key::KEY_SLASH => 0x38,
            _ => return None,
        };
        Some(code)
    }

    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        let args = Args::parse();

        // Open the evdev device
        let mut device = Device::open(&args.device)?;
        println!(
            "Opened device: {} ({})",
            device.name().unwrap_or("Unknown"),
            args.device.display()
        );

        // Grab the device to prevent local echo
        device.grab()?;
        println!("Grabbed device (local echo disabled)");

        // Set up UDP socket for broadcast
        let socket = UdpSocket::bind("0.0.0.0:0")?;
        socket.set_broadcast(true)?;
        let dest = format!("{}:{}", args.broadcast, args.port);
        println!("Broadcasting to {}", dest);

        // Generate random device ID for this session
        let device_id: u32 = rand_device_id();
        println!("Device ID: {:#x}", device_id);

        let mut seq: u32 = 0;
        let mut shift_held = false;

        println!("Listening for keystrokes...");

        loop {
            for event in device.fetch_events()? {
                if let InputEventKind::Key(key) = event.kind() {
                    let value = event.value();

                    // Track shift state
                    if matches!(key, Key::KEY_LEFTSHIFT | Key::KEY_RIGHTSHIFT) {
                        shift_held = value != 0;
                        continue;
                    }

                    // Only process key press (value=1), ignore release (0) and repeat (2)
                    if value != 1 {
                        continue;
                    }

                    // Convert to HID code
                    let Some(hid_code) = evdev_to_hid(key) else {
                        eprintln!("Unmapped key: {:?}", key);
                        continue;
                    };

                    // Check allowlist (safety belt)
                    if !hid_allowed(hid_code) {
                        eprintln!("Blocked key: {:?} (HID {:#x})", key, hid_code);
                        continue;
                    }

                    // Build and send packet
                    let mods = if shift_held { MOD_SHIFT } else { 0 };
                    let tap = KeyTap::new(device_id, seq, mods, hid_code);
                    let packet = tap.encode();

                    // Send duplicate times for UDP reliability
                    for _ in 0..args.duplicate {
                        socket.send_to(&packet, &dest)?;
                    }

                    seq = seq.wrapping_add(1);
                    println!(
                        "Sent: {:?} (HID {:#x}, shift={}, seq={})",
                        key, hid_code, shift_held, seq
                    );
                }
            }
        }
    }

    /// Generate a pseudo-random device ID using system time
    fn rand_device_id() -> u32 {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        let pid = std::process::id();
        nanos ^ pid
    }
}
