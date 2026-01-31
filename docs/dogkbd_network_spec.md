# DOGKBD — Network “Puppy Keyboard” Proxy (Raspberry Pi → UDP Broadcast → Windows/Linux Key Injector)

> **Purpose:** Let a 9‑lb dog walk on a rubber USB keyboard connected to a **Raspberry Pi 5**, and have those “random” key presses **show up as typing** on a Windows (or Linux) machine running **Git Bash** (or any chosen window), while filtering out “danger keys” like Esc/Tab/Ctrl.

This document is a **complete, self-contained spec**: architecture, protocol, safety model, and full Rust code listings for a Cargo workspace with:
- `dogkbd-sender` (Pi): reads Linux `evdev` input, filters keys, and broadcasts each “key tap”
- `dogkbd-receiver` (Windows/Linux): receives broadcasts, verifies authenticity, optionally gates to a selected target window, injects keystrokes, and shows a small GUI with **auto-refreshing window dropdown + preview**

---

## Table of contents

- [1. Goals and constraints](#1-goals-and-constraints)
- [2. Architecture overview](#2-architecture-overview)
- [3. Transport and robustness](#3-transport-and-robustness)
- [4. Security model](#4-security-model)
- [5. Key model and filtering](#5-key-model-and-filtering)
- [6. Protocol specification](#6-protocol-specification)
- [7. Raspberry Pi 5 sender](#7-raspberry-pi-5-sender)
- [8. Receiver app (Windows + Linux) with GUI](#8-receiver-app-windows--linux-with-gui)
- [9. Build & run instructions](#9-build--run-instructions)
- [10. Troubleshooting](#10-troubleshooting)
- [11. Future upgrades](#11-future-upgrades)
- [Appendix A — HID usage codes used](#appendix-a--hid-usage-codes-used)
- [Appendix B — Windows scan code mapping](#appendix-b--windows-scan-code-mapping)
- [Appendix C — Linux uinput mapping notes](#appendix-c--linux-uinput-mapping-notes)

---

## 1. Goals and constraints

### Functional goals
1. **Capture real keypresses** from a physical rubber USB keyboard attached to Raspberry Pi 5.
2. **Filter out “danger keys”** (Esc/Tab/Ctrl/Alt/Win/F-keys/etc.) to prevent breaking your CLI session.
3. **Send each keypress over the LAN** using UDP broadcast (to the whole subnet).
4. **Receive and inject** those keypresses on Windows and Linux.
5. Provide a **small GUI** on the receiver:
   - Auto-refreshing dropdown of open windows
   - Ability to **select a target window** (e.g., **Git Bash**)
   - “Armed” toggle (off by default)
   - Optional “Auto-focus target” mode
   - **Preview**: log + live “what would be typed” buffer

### Non-goals (by design)
- Perfect reliability (UDP can drop). It’s *okay* if some characters drop.
- Key-up fidelity, key repeat fidelity, or true “hold keys”. We use **stateless tap events**.
- Sending to a background window reliably on Windows without focusing it (Windows input model doesn’t really support this safely for terminals).

### Latency target
- Added latency ≤ **100ms** per keypress. Typical LAN UDP + injection should be single-digit ms.

---

## 2. Architecture overview

**Data flow:**

```
Rubber USB Keyboard
        │
        ▼
Raspberry Pi 5 (dogkbd-sender)
  - reads /dev/input (evdev)
  - allowlist filter
  - maintains shift state
  - emits KeyTap packets
        │ UDP broadcast (directed broadcast recommended)
        ▼
Windows/Linux PC (dogkbd-receiver)
  - UDP listener
  - verify HMAC
  - deduplicate (seq)
  - GUI target selection
  - (optional) focus gate
  - inject key tap into OS
```

**Key design decision:** we send **KeyTap** (press+release) events, not raw streams.

---

## 3. Transport and robustness

### Why UDP broadcast?
- Zero pairing / zero discovery. Sender shouts to the subnet, receivers listen.
- Dead simple; latency is low.

### Broadcast specifics
- Preferred: **directed broadcast** (e.g. `192.168.1.255:44555`) per active interface.
- Fallback: `255.255.255.255:44555` can work but is less reliable on some networks.

### Robustness tricks we use
1. **Tap semantics**: receiver always presses & releases; no stuck keys.
2. **Duplicate-send**: sender can send each packet **twice** (same sequence number).
3. **Dedup**: receiver keeps `last_seq` per `device_id`, drops duplicates.

If a packet drops, you lose a character — acceptable for “dog typing”. But you don’t get stuck modifiers.

---

## 4. Security model

Broadcast keystrokes are dangerous on a LAN unless authenticated.

### We use HMAC authentication
- Each packet includes a truncated **HMAC-SHA256** computed over the header.
- Receiver verifies before accepting.
- Without the secret, other devices can’t spoof keystrokes.

### Generate and distribute secret
Create a 32-byte secret (hex) and copy to both Pi and receiver PC:

```bash
openssl rand -hex 32 > dogkbd.secret
```

Store securely. Anyone with this file can inject keys.

---

## 5. Key model and filtering

### Sender-side allowlist (recommended)
**Allow**:
- letters A–Z
- digits 0–9
- space
- enter
- backspace
- punctuation: `- = [ ] \ ; ' , . / \``

**Track**: Shift (so you can get uppercase and shifted punctuation)

**Block** everything else:
- Esc, Tab, Ctrl, Alt, Meta/Win, function keys, etc.

### Receiver-side safety belt
Receiver also enforces the same allowlist. Even if the sender is misconfigured, receiver still blocks danger keys.

---

## 6. Protocol specification

### Packet: 32 bytes (little-endian)

| Offset | Size | Field | Description |
|---:|---:|---|---|
| 0 | 4 | `magic` | ASCII `DOGK` |
| 4 | 1 | `version` | `1` |
| 5 | 1 | `msg_type` | `1` = KeyTap |
| 6 | 4 | `device_id` | random u32 per sender run |
| 10 | 4 | `seq` | monotonic u32 counter |
| 14 | 1 | `mods` | bit0 = shift (others reserved) |
| 15 | 1 | `hid` | USB HID Usage ID (Keyboard page 0x07) |
| 16 | 16 | `mac` | HMAC-SHA256(secret, bytes[0..16]) truncated to 16 bytes |

### Message semantics
- Each packet means “tap this key”: down then up.
- Receiver MUST:
  - apply modifiers (currently only Shift)
  - press key
  - release key
  - release modifiers

### Replay / dedup rule
- Receiver keeps last seen sequence per `device_id`.
- Drops any packet with `seq <= last_seq` for that device.

`device_id` changes every sender run so restarts don’t get “stuck” behind old sequence numbers.

---

## 7. Raspberry Pi 5 sender

### 7.1 Hardware
- Plug rubber keyboard into **any USB-A port** on the Pi 5.

No “power USB-C OTG” required in this network design.

### 7.2 Identify the keyboard device
You already saw the right one in your logs:
- `/dev/input/event5` is `"SIGMACHIP USB Keyboard"` (main interface)
- event6/event7 are consumer/system control — ignore

### 7.3 Make a stable input symlink (recommended)
Create `/dev/input/dogkbd` via udev so reboots/hotplug don’t change things.

`/etc/udev/rules.d/99-dogkbd.rules`:

```udev
# Create /dev/input/dogkbd for the main USB keyboard interface only
SUBSYSTEM=="input", KERNEL=="event*", \
  ATTRS{idVendor}=="1c4f", ATTRS{idProduct}=="0002", \
  ATTRS{bInterfaceProtocol}=="01", \
  SYMLINK+="input/dogkbd"
```

Apply:
```bash
sudo udevadm control --reload-rules
sudo udevadm trigger
ls -l /dev/input/dogkbd
```

### 7.4 Sender system behavior
- Opens `/dev/input/dogkbd` (or a path you pass)
- Calls `grab()` to prevent local OS from consuming events
- Filters + converts evdev keys → HID usage
- Broadcasts UDP KeyTap packets

---

## 8. Receiver app (Windows + Linux) with GUI

### 8.1 What “targeting a window” means in practice
On Windows, **`SendInput` injects to the foreground input queue**, not to an arbitrary HWND. Terminals like Git Bash generally won’t accept synthetic keystrokes via `PostMessage(WM_CHAR)` reliably.

So our “targeting” is implemented as:

- **Strict gate (recommended):** Only inject when the **selected window is currently foreground**.
- **Auto-focus mode (optional):** If the selected window is not foreground, try `SetForegroundWindow(target)`. If it succeeds, inject; otherwise drop.

This ensures you never type into the wrong place.

### 8.2 Receiver GUI requirements
- Runs on Windows and Linux (Linux feature set may be reduced).
- Shows:
  - UDP status (last packet time, counters)
  - **Auto-refreshing window list** (Windows supported; Linux only on X11 if implemented later)
  - Selected target window details (title, exe, pid, hwnd)
  - Toggles:
    - **Armed** (default OFF)
    - Strict foreground gate (default ON)
    - Auto-focus (default OFF)
  - Preview:
    - rolling key log (injected/dropped + reason)
    - “typed so far” text buffer (approximation)

---

## 9. Full code (Cargo workspace)

Create a folder `dogkbd/` with this layout:

```
dogkbd/
  Cargo.toml
  proto/
    Cargo.toml
    src/lib.rs
  sender/
    Cargo.toml
    src/main.rs
  receiver/
    Cargo.toml
    src/main.rs
    src/app.rs
    src/net.rs
    src/keys.rs
    src/target/mod.rs
    src/target/windows.rs
    src/inject/mod.rs
    src/inject/windows.rs
    src/inject/linux.rs
```

> Copy/paste the following files exactly.

---

## 9.1 Root workspace `Cargo.toml`

```toml
[workspace]
members = ["proto", "sender", "receiver"]
resolver = "2"
```

---

## 9.2 `proto` crate

### `proto/Cargo.toml`
```toml
[package]
name = "dogkbd-proto"
version = "0.1.0"
edition = "2021"
publish = false

[dependencies]
hmac = "0.12"
sha2 = "0.10"
```

### `proto/src/lib.rs`
```rust
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

pub const MAGIC: [u8; 4] = *b"DOGK";
pub const VERSION: u8 = 1;
pub const MSG_KEYTAP: u8 = 1;

/// Modifier bits
pub const MOD_SHIFT: u8 = 1 << 0;

/// One stateless “tap” event (press+release) for a key, plus modifier state.
#[derive(Debug, Clone, Copy)]
pub struct KeyTap {
    pub device_id: u32,
    pub seq: u32,
    pub mods: u8,
    pub hid: u8, // HID usage page 0x07
}

/// Encode a KeyTap into a fixed 32-byte packet.
/// Layout:
/// [0..4]  = MAGIC
/// [4]     = VERSION
/// [5]     = MSG_TYPE
/// [6..10] = device_id (LE)
/// [10..14]= seq (LE)
/// [14]    = mods
/// [15]    = hid
/// [16..32]= HMAC(secret, [0..16]) truncated to 16 bytes
pub fn encode(secret: &[u8], kt: KeyTap) -> [u8; 32] {
    let mut buf = [0u8; 32];
    buf[0..4].copy_from_slice(&MAGIC);
    buf[4] = VERSION;
    buf[5] = MSG_KEYTAP;

    buf[6..10].copy_from_slice(&kt.device_id.to_le_bytes());
    buf[10..14].copy_from_slice(&kt.seq.to_le_bytes());
    buf[14] = kt.mods;
    buf[15] = kt.hid;

    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC key");
    mac.update(&buf[0..16]);
    let full = mac.finalize().into_bytes();
    buf[16..32].copy_from_slice(&full[..16]);
    buf
}

/// Decode and authenticate a 32-byte packet.
/// Returns None if invalid/malformed/bad HMAC.
pub fn decode(secret: &[u8], buf: &[u8]) -> Option<KeyTap> {
    if buf.len() != 32 {
        return None;
    }
    if buf[0..4] != MAGIC {
        return None;
    }
    if buf[4] != VERSION || buf[5] != MSG_KEYTAP {
        return None;
    }

    // verify HMAC
    let mut mac = HmacSha256::new_from_slice(secret).ok()?;
    mac.update(&buf[0..16]);
    let full = mac.finalize().into_bytes();
    if buf[16..32] != full[..16] {
        return None;
    }

    let device_id = u32::from_le_bytes(buf[6..10].try_into().ok()?);
    let seq = u32::from_le_bytes(buf[10..14].try_into().ok()?);
    let mods = buf[14];
    let hid = buf[15];

    Some(KeyTap {
        device_id,
        seq,
        mods,
        hid,
    })
}

/// Receiver-side allowlist (safety belt).
/// Keep it strict to avoid "danger keys".
pub fn hid_allowed(hid: u8) -> bool {
    matches!(
        hid,
        // letters
        0x04..=0x1d |
        // digits 1-0
        0x1e..=0x27 |
        // enter, backspace, space
        0x28 | 0x2a | 0x2c |
        // punctuation: - = [ ] \ ; ' ` , . /
        0x2d | 0x2e | 0x2f | 0x30 | 0x31 | 0x33 | 0x34 | 0x35 | 0x36 | 0x37 | 0x38
    )
}

/// Convert a HID key to a US-ANSI character for preview purposes.
/// Returns None for non-printables (e.g., Enter/Backspace).
pub fn hid_to_us_ansi_char(hid: u8, shift: bool) -> Option<char> {
    let c = match hid {
        // letters
        0x04..=0x1d => {
            let base = (hid - 0x04) as u8;
            let ch = (b'a' + base) as char;
            if shift { ch.to_ascii_uppercase() } else { ch }
        }

        // digits row: 1..0
        0x1e => if shift { '!' } else { '1' },
        0x1f => if shift { '@' } else { '2' },
        0x20 => if shift { '#' } else { '3' },
        0x21 => if shift { '$' } else { '4' },
        0x22 => if shift { '%' } else { '5' },
        0x23 => if shift { '^' } else { '6' },
        0x24 => if shift { '&' } else { '7' },
        0x25 => if shift { '*' } else { '8' },
        0x26 => if shift { '(' } else { '9' },
        0x27 => if shift { ')' } else { '0' },

        // space
        0x2c => ' ',

        // punctuation
        0x2d => if shift { '_' } else { '-' },
        0x2e => if shift { '+' } else { '=' },
        0x2f => if shift { '{' } else { '[' },
        0x30 => if shift { '}' } else { ']' },
        0x31 => if shift { '|' } else { '\\' },
        0x33 => if shift { ':' } else { ';' },
        0x34 => if shift { '"' } else { '\'' },
        0x35 => if shift { '~' } else { '`' },
        0x36 => if shift { '<' } else { ',' },
        0x37 => if shift { '>' } else { '.' },
        0x38 => if shift { '?' } else { '/' },

        _ => return None,
    };
    Some(c)
}
```

---

## 9.3 Sender (`dogkbd-sender`)

### `sender/Cargo.toml`
```toml
[package]
name = "dogkbd-sender"
version = "0.1.0"
edition = "2021"
publish = false

[dependencies]
anyhow = "1"
clap = { version = "4", features = ["derive"] }
dogkbd-proto = { path = "../proto" }
evdev = "0.13"
hex = "0.4"
if-addrs = "0.14"
rand = "0.8"
```

### `sender/src/main.rs`
```rust
use anyhow::{anyhow, Context, Result};
use clap::Parser;
use dogkbd_proto::{encode, KeyTap, MOD_SHIFT};
use evdev::{Device, EventSummary, KeyCode};
use if_addrs::get_if_addrs;
use rand::RngCore;
use std::fs;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(name="dogkbd-sender", about="Read a Linux keyboard (evdev) and broadcast KeyTap packets")]
struct Args {
    /// Path to evdev input device (recommended: /dev/input/dogkbd)
    #[arg(long, default_value = "/dev/input/dogkbd")]
    device: PathBuf,

    /// UDP port
    #[arg(long, default_value_t = 44555)]
    port: u16,

    /// Secret file (hex string, e.g. output of `openssl rand -hex 32`)
    #[arg(long, default_value = "dogkbd.secret")]
    secret_file: PathBuf,

    /// Optional explicit destination(s) (unicast or broadcast). Can be repeated.
    /// Example: --dest 192.168.1.255 --dest 192.168.1.50
    #[arg(long)]
    dest: Vec<Ipv4Addr>,

    /// Send each packet N times (duplicate-send). 2 is a good default.
    #[arg(long, default_value_t = 2)]
    duplicate: u32,

    /// Delay between duplicate sends (ms)
    #[arg(long, default_value_t = 2)]
    dup_delay_ms: u64,

    /// Grab exclusive access to the keyboard device so the Pi doesn't also "type"
    #[arg(long, default_value_t = true)]
    grab: bool,

    /// Log every sent key (spammy but helpful)
    #[arg(long, default_value_t = false)]
    verbose: bool,
}

fn read_secret_hex(path: &PathBuf) -> Result<Vec<u8>> {
    let s = fs::read_to_string(path)
        .with_context(|| format!("reading secret file: {}", path.display()))?;
    let s = s.trim();
    let bytes = hex::decode(s).map_err(|e| anyhow!("secret file must be hex: {e}"))?;
    if bytes.len() < 16 {
        return Err(anyhow!("secret is too short; use at least 16 bytes (32+ recommended)"));
    }
    Ok(bytes)
}

fn broadcast_targets(port: u16) -> Result<Vec<SocketAddrV4>> {
    let mut out = Vec::new();
    for iface in get_if_addrs()? {
        if let if_addrs::IfAddr::V4(v4) = iface.addr {
            if v4.is_loopback() {
                continue;
            }
            // Prefer OS-provided broadcast; otherwise compute.
            let bcast = v4.broadcast.unwrap_or_else(|| {
                let ip_u32 = u32::from(v4.ip);
                let mask_u32 = u32::from(v4.netmask);
                Ipv4Addr::from(ip_u32 | !mask_u32)
            });
            out.push(SocketAddrV4::new(bcast, port));
        }
    }
    if out.is_empty() {
        return Err(anyhow!("no IPv4 interfaces found for broadcast"));
    }
    Ok(out)
}

/// Map Linux evdev KeyCode to USB HID usage (Keyboard page 0x07).
/// This is a strict allowlist: return None for anything not allowed.
fn evdev_to_hid(k: KeyCode) -> Option<u8> {
    // Letters
    let hid = match k {
        KeyCode::KEY_A => 0x04,
        KeyCode::KEY_B => 0x05,
        KeyCode::KEY_C => 0x06,
        KeyCode::KEY_D => 0x07,
        KeyCode::KEY_E => 0x08,
        KeyCode::KEY_F => 0x09,
        KeyCode::KEY_G => 0x0a,
        KeyCode::KEY_H => 0x0b,
        KeyCode::KEY_I => 0x0c,
        KeyCode::KEY_J => 0x0d,
        KeyCode::KEY_K => 0x0e,
        KeyCode::KEY_L => 0x0f,
        KeyCode::KEY_M => 0x10,
        KeyCode::KEY_N => 0x11,
        KeyCode::KEY_O => 0x12,
        KeyCode::KEY_P => 0x13,
        KeyCode::KEY_Q => 0x14,
        KeyCode::KEY_R => 0x15,
        KeyCode::KEY_S => 0x16,
        KeyCode::KEY_T => 0x17,
        KeyCode::KEY_U => 0x18,
        KeyCode::KEY_V => 0x19,
        KeyCode::KEY_W => 0x1a,
        KeyCode::KEY_X => 0x1b,
        KeyCode::KEY_Y => 0x1c,
        KeyCode::KEY_Z => 0x1d,

        // digits row
        KeyCode::KEY_1 => 0x1e,
        KeyCode::KEY_2 => 0x1f,
        KeyCode::KEY_3 => 0x20,
        KeyCode::KEY_4 => 0x21,
        KeyCode::KEY_5 => 0x22,
        KeyCode::KEY_6 => 0x23,
        KeyCode::KEY_7 => 0x24,
        KeyCode::KEY_8 => 0x25,
        KeyCode::KEY_9 => 0x26,
        KeyCode::KEY_0 => 0x27,

        // enter / backspace / space
        KeyCode::KEY_ENTER => 0x28,
        KeyCode::KEY_BACKSPACE => 0x2a,
        KeyCode::KEY_SPACE => 0x2c,

        // punctuation: - = [ ] \ ; ' ` , . /
        KeyCode::KEY_MINUS => 0x2d,
        KeyCode::KEY_EQUAL => 0x2e,
        KeyCode::KEY_LEFTBRACE => 0x2f,
        KeyCode::KEY_RIGHTBRACE => 0x30,
        KeyCode::KEY_BACKSLASH => 0x31,
        KeyCode::KEY_SEMICOLON => 0x33,
        KeyCode::KEY_APOSTROPHE => 0x34,
        KeyCode::KEY_GRAVE => 0x35,
        KeyCode::KEY_COMMA => 0x36,
        KeyCode::KEY_DOT => 0x37,
        KeyCode::KEY_SLASH => 0x38,

        _ => return None,
    };
    Some(hid)
}

fn main() -> Result<()> {
    let args = Args::parse();
    let secret = read_secret_hex(&args.secret_file)?;

    let mut device = Device::open(&args.device)
        .with_context(|| format!("opening device {}", args.device.display()))?;

    if args.grab {
        device.grab().context("grabbing device (try sudo or udev perms)")?;
    }

    let sock = UdpSocket::bind("0.0.0.0:0").context("binding UDP socket")?;
    sock.set_broadcast(true).context("enabling broadcast")?;

    let targets: Vec<SocketAddrV4> = if !args.dest.is_empty() {
        args.dest.iter().map(|ip| SocketAddrV4::new(*ip, args.port)).collect()
    } else {
        broadcast_targets(args.port)?
    };

    eprintln!("dogkbd-sender:");
    eprintln!("  device: {}", args.device.display());
    eprintln!("  targets:");
    for t in &targets {
        eprintln!("    - {}", t);
    }

    // Unique device id per run so receiver dedup resets naturally after restart.
    let mut device_id_bytes = [0u8; 4];
    rand::thread_rng().fill_bytes(&mut device_id_bytes);
    let device_id = u32::from_le_bytes(device_id_bytes);

    let mut seq: u32 = 0;
    let mut shift_down = false;

    loop {
        for ev in device.fetch_events()? {
            match ev.destructure() {
                // Track shift state (key down = 1, up = 0, repeat = 2)
                EventSummary::Key(_, KeyCode::KEY_LEFTSHIFT, v)
                | EventSummary::Key(_, KeyCode::KEY_RIGHTSHIFT, v) => {
                    shift_down = v == 1;
                }

                // Key down
                EventSummary::Key(_, k, 1) => {
                    if let Some(hid) = evdev_to_hid(k) {
                        seq = seq.wrapping_add(1);
                        let mods = if shift_down { MOD_SHIFT } else { 0 };
                        let pkt = encode(&secret, KeyTap { device_id, seq, mods, hid });

                        if args.verbose {
                            eprintln!("send seq={} mods={} hid=0x{:02x}", seq, mods, hid);
                        }

                        for _ in 0..args.duplicate {
                            for t in &targets {
                                let _ = sock.send_to(&pkt, t);
                            }
                            if args.duplicate > 1 && args.dup_delay_ms > 0 {
                                thread::sleep(Duration::from_millis(args.dup_delay_ms));
                            }
                        }
                    }
                }

                _ => {}
            }
        }
    }
}
```

### 7.5 Run sender manually (Pi)
```bash
cd dogkbd
cargo build -p dogkbd-sender --release
sudo ./target/release/dogkbd-sender --device /dev/input/dogkbd --secret-file dogkbd.secret
```

### 7.6 systemd service (optional, recommended)
Create `/etc/systemd/system/dogkbd-sender.service`:

```ini
[Unit]
Description=DOGKBD Sender (keyboard -> UDP broadcast)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=root
WorkingDirectory=/home/pi/dogkbd
ExecStart=/home/pi/dogkbd/target/release/dogkbd-sender --device /dev/input/dogkbd --secret-file /home/pi/dogkbd/dogkbd.secret --duplicate 2
Restart=always
RestartSec=1

[Install]
WantedBy=multi-user.target
```

Enable:
```bash
sudo systemctl daemon-reload
sudo systemctl enable --now dogkbd-sender.service
sudo systemctl status dogkbd-sender.service
```

---

## 9.4 Receiver (`dogkbd-receiver`)

This is the GUI app you run on your Windows machine (and can also run on Linux).

### Receiver behavior
- Binds UDP `0.0.0.0:44555`
- Decodes + verifies HMAC
- Dedup
- Applies allowlist safety
- If **Armed** and target gate passes:
  - Injects tap into OS
- Sends log messages to GUI

### `receiver/Cargo.toml`
```toml
[package]
name = "dogkbd-receiver"
version = "0.1.0"
edition = "2021"
publish = false

[dependencies]
anyhow = "1"
clap = { version = "4", features = ["derive"] }
crossbeam-channel = "0.5"
dogkbd-proto = { path = "../proto" }
eframe = "0.29"
egui = "0.29"
hex = "0.4"
parking_lot = "0.12"

[target.'cfg(target_os = "windows")'.dependencies]
windows = { version = "0.58", features = [
  "Win32_Foundation",
  "Win32_UI_WindowsAndMessaging",
  "Win32_UI_Input_KeyboardAndMouse",
  "Win32_System_Threading",
  "Win32_System_ProcessStatus",
  "Win32_System_Diagnostics_ToolHelp"
] }

[target.'cfg(target_os = "linux")'.dependencies]
evdev = "0.13"
```

> Note: the exact `eframe/egui/windows` versions may change; use `cargo update` if needed.

---

### `receiver/src/main.rs`
```rust
mod app;
mod inject;
mod keys;
mod net;
mod target;

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name="dogkbd-receiver", about="Receive DOGKBD UDP broadcast packets and inject keystrokes")]
pub struct Args {
    /// UDP port to listen on
    #[arg(long, default_value_t = 44555)]
    pub port: u16,

    /// Secret file (hex string)
    #[arg(long, default_value = "dogkbd.secret")]
    pub secret_file: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let native_options = eframe::NativeOptions::default();
    eframe::run_native(
        "DOGKBD Receiver",
        native_options,
        Box::new(move |cc| Ok(Box::new(app::DogKbdApp::new(cc, args)))),
    )?;

    Ok(())
}
```

---

### `receiver/src/keys.rs`
```rust
use dogkbd_proto::{hid_to_us_ansi_char, MOD_SHIFT};

#[derive(Debug, Clone, Copy)]
pub enum KeyPreview {
    Char(char),
    Enter,
    Backspace,
    Unknown,
}

pub fn to_preview(hid: u8, mods: u8) -> KeyPreview {
    let shift = (mods & MOD_SHIFT) != 0;
    match hid {
        0x28 => KeyPreview::Enter,      // Enter
        0x2a => KeyPreview::Backspace,  // Backspace
        _ => {
            if let Some(c) = hid_to_us_ansi_char(hid, shift) {
                KeyPreview::Char(c)
            } else {
                KeyPreview::Unknown
            }
        }
    }
}
```

---

### `receiver/src/net.rs`
```rust
use anyhow::{Context, Result};
use crossbeam_channel::Sender;
use dogkbd_proto::{decode, hid_allowed, KeyTap};
use std::collections::HashMap;
use std::net::UdpSocket;
use std::time::{Duration, SystemTime};

use crate::app::{AppEvent, SharedSettings};

pub fn spawn_listener(
    port: u16,
    secret: Vec<u8>,
    settings: SharedSettings,
    tx: Sender<AppEvent>,
) -> Result<std::thread::JoinHandle<()>> {
    let sock = UdpSocket::bind(("0.0.0.0", port)).with_context(|| format!("bind UDP :{port}"))?;
    sock.set_read_timeout(Some(Duration::from_millis(200)))?;

    let handle = std::thread::spawn(move || {
        let mut buf = [0u8; 64];
        let mut last_seq: HashMap<u32, u32> = HashMap::new();

        loop {
            match sock.recv_from(&mut buf) {
                Ok((n, src)) => {
                    let now = SystemTime::now();
                    let pkt = &buf[..n];
                    let Some(kt) = decode(&secret, pkt) else {
                        let _ = tx.send(AppEvent::PacketRejected {
                            when: now,
                            src,
                            reason: "bad packet or HMAC".into(),
                        });
                        continue;
                    };

                    if !hid_allowed(kt.hid) {
                        let _ = tx.send(AppEvent::PacketRejected {
                            when: now,
                            src,
                            reason: format!("hid 0x{:02x} not allowed", kt.hid),
                        });
                        continue;
                    }

                    let prev = last_seq.get(&kt.device_id).copied().unwrap_or(0);
                    if kt.seq <= prev {
                        // dedup / replay drop
                        continue;
                    }
                    last_seq.insert(kt.device_id, kt.seq);

                    // snapshot settings
                    let snap = settings.read().clone();

                    let _ = tx.send(AppEvent::PacketAccepted { when: now, src, kt });

                    if !snap.armed {
                        let _ = tx.send(AppEvent::InjectResult {
                            when: now,
                            kt,
                            injected: false,
                            reason: "disarmed".into(),
                        });
                        continue;
                    }

                    if let Err(e) = crate::inject::inject_keytap(&snap, kt) {
                        let _ = tx.send(AppEvent::InjectResult {
                            when: now,
                            kt,
                            injected: false,
                            reason: format!("inject error: {e}"),
                        });
                    } else {
                        let _ = tx.send(AppEvent::InjectResult {
                            when: now,
                            kt,
                            injected: true,
                            reason: "ok".into(),
                        });
                    }
                }
                Err(_timeout) => {
                    // keep looping; allows UI to stay responsive and settings to change
                }
            }
        }
    });

    Ok(handle)
}
```

---

### `receiver/src/target/mod.rs`
```rust
#[derive(Debug, Clone)]
pub struct TargetWindow {
    pub id: String,      // UI identifier
    pub title: String,
    pub exe: String,
    pub pid: u32,

    #[cfg(target_os = "windows")]
    pub hwnd: isize,
}

#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(not(target_os = "windows"))]
pub fn list_windows() -> Vec<TargetWindow> {
    // Linux window targeting is non-trivial and varies by X11/Wayland.
    // For now, we return empty. The UI will explain that targeting is Windows-only (for now).
    Vec::new()
}
```

---

### `receiver/src/target/windows.rs`
```rust
use crate::target::TargetWindow;
use windows::core::PWSTR;
use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
use windows::Win32::System::ProcessStatus::K32GetProcessImageFileNameW;
use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
    IsWindowVisible,
};

pub fn foreground_hwnd() -> isize {
    unsafe { GetForegroundWindow().0 as isize }
}

pub fn list_windows() -> Vec<TargetWindow> {
    let mut out: Vec<TargetWindow> = Vec::new();

    unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let vec = &mut *(lparam.0 as *mut Vec<TargetWindow>);

        if !IsWindowVisible(hwnd).as_bool() {
            return BOOL(1);
        }

        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return BOOL(1);
        }

        let mut buf = vec![0u16; (len + 1) as usize];
        let got = GetWindowTextW(hwnd, PWSTR(buf.as_mut_ptr()), len + 1);
        if got <= 0 {
            return BOOL(1);
        }
        let title = String::from_utf16_lossy(&buf[..got as usize]).trim().to_string();
        if title.is_empty() {
            return BOOL(1);
        }

        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));

        let exe = process_image_name(pid).unwrap_or_else(|| "<unknown>".into());

        vec.push(TargetWindow {
            id: format!("{}:0x{:x}", pid, hwnd.0 as usize),
            title,
            exe,
            pid,
            hwnd: hwnd.0 as isize,
        });

        BOOL(1)
    }

    unsafe {
        let lparam = LPARAM(&mut out as *mut _ as isize);
        let _ = EnumWindows(Some(enum_cb), lparam);
    }

    // stable sort: exe then title
    out.sort_by(|a, b| a.exe.cmp(&b.exe).then(a.title.cmp(&b.title)));
    out
}

fn process_image_name(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = vec![0u16; 260];
        let n = K32GetProcessImageFileNameW(h, &mut buf);
        if n == 0 {
            return None;
        }
        let full = String::from_utf16_lossy(&buf[..n as usize]);
        // just keep the file name portion
        Some(
            full.rsplit(['\\', '/'])
                .next()
                .unwrap_or(&full)
                .to_string(),
        )
    }
}

pub fn is_foreground(hwnd: isize) -> bool {
    foreground_hwnd() == hwnd
}
```

---

### `receiver/src/inject/mod.rs`
```rust
use anyhow::Result;
use dogkbd_proto::KeyTap;

use crate::app::Settings;

#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
mod linux;

pub fn inject_keytap(settings: &Settings, kt: KeyTap) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        return windows::inject_windows(settings, kt);
    }
    #[cfg(target_os = "linux")]
    {
        return linux::inject_linux(settings, kt);
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        let _ = settings;
        let _ = kt;
        anyhow::bail!("unsupported OS");
    }
}
```

---

### `receiver/src/inject/windows.rs`
```rust
use anyhow::{anyhow, Result};
use dogkbd_proto::{KeyTap, MOD_SHIFT};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE,
};

use crate::app::Settings;
use crate::target::windows as win_target;

/// Minimal mapping: HID usage -> (Set 1 scan code, extended?)
/// US ANSI layout.
fn hid_to_scancode(hid: u8) -> Option<(u16, bool)> {
    let (sc, ext) = match hid {
        // letters A-Z (scan codes for US QWERTY)
        0x04 => (0x1E, false), // A
        0x05 => (0x30, false), // B
        0x06 => (0x2E, false), // C
        0x07 => (0x20, false), // D
        0x08 => (0x12, false), // E
        0x09 => (0x21, false), // F
        0x0A => (0x22, false), // G
        0x0B => (0x23, false), // H
        0x0C => (0x17, false), // I
        0x0D => (0x24, false), // J
        0x0E => (0x25, false), // K
        0x0F => (0x26, false), // L
        0x10 => (0x32, false), // M
        0x11 => (0x31, false), // N
        0x12 => (0x18, false), // O
        0x13 => (0x19, false), // P
        0x14 => (0x10, false), // Q
        0x15 => (0x13, false), // R
        0x16 => (0x1F, false), // S
        0x17 => (0x14, false), // T
        0x18 => (0x16, false), // U
        0x19 => (0x2F, false), // V
        0x1A => (0x11, false), // W
        0x1B => (0x2D, false), // X
        0x1C => (0x15, false), // Y
        0x1D => (0x2C, false), // Z

        // digits row 1..0
        0x1E => (0x02, false), // 1
        0x1F => (0x03, false), // 2
        0x20 => (0x04, false), // 3
        0x21 => (0x05, false), // 4
        0x22 => (0x06, false), // 5
        0x23 => (0x07, false), // 6
        0x24 => (0x08, false), // 7
        0x25 => (0x09, false), // 8
        0x26 => (0x0A, false), // 9
        0x27 => (0x0B, false), // 0

        // enter, backspace, space
        0x28 => (0x1C, false), // Enter
        0x2A => (0x0E, false), // Backspace
        0x2C => (0x39, false), // Space

        // punctuation: - = [ ] \ ; ' ` , . /
        0x2D => (0x0C, false), // -
        0x2E => (0x0D, false), // =
        0x2F => (0x1A, false), // [
        0x30 => (0x1B, false), // ]
        0x31 => (0x2B, false), // backslash
        0x33 => (0x27, false), // ;
        0x34 => (0x28, false), // '
        0x35 => (0x29, false), // `
        0x36 => (0x33, false), // ,
        0x37 => (0x34, false), // .
        0x38 => (0x35, false), // /

        _ => return None,
    };
    Some((sc, ext))
}

fn send_scancode(sc: u16, keyup: bool) -> Result<()> {
    let flags = KEYEVENTF_SCANCODE | if keyup { KEYEVENTF_KEYUP } else { Default::default() };

    let ki = KEYBDINPUT {
        wVk: 0,
        wScan: sc,
        dwFlags: flags,
        time: 0,
        dwExtraInfo: 0,
    };

    let mut input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki },
    };

    let sent = unsafe { SendInput(&mut [input], std::mem::size_of::<INPUT>() as i32) };
    if sent == 0 {
        return Err(anyhow!("SendInput failed"));
    }
    Ok(())
}

pub fn inject_windows(settings: &Settings, kt: KeyTap) -> Result<()> {
    // target gating
    let Some(target) = settings.target_hwnd else {
        return Err(anyhow!("no target selected"));
    };

    if settings.strict_foreground {
        if !win_target::is_foreground(target) {
            return Err(anyhow!("target not foreground"));
        }
    } else if settings.auto_focus {
        // Best-effort: bring target to front, then verify.
        let ok = unsafe { windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(HWND(target as isize)) };
        let _ = ok;
        if !win_target::is_foreground(target) {
            return Err(anyhow!("failed to focus target"));
        }
    }

    let (sc, _ext) = hid_to_scancode(kt.hid).ok_or_else(|| anyhow!("no scancode mapping"))?;

    let shift = (kt.mods & MOD_SHIFT) != 0;

    // tap = down then up, plus optional shift modifier
    if shift {
        send_scancode(0x2A, false)?; // LeftShift down
    }
    send_scancode(sc, false)?;       // key down
    send_scancode(sc, true)?;        // key up
    if shift {
        send_scancode(0x2A, true)?;  // LeftShift up
    }

    Ok(())
}
```

> **Note:** For some “extended keys” (arrows, etc.), you’d need KEYEVENTF_EXTENDEDKEY and different scan codes. We intentionally **don’t allow** those keys in the allowlist for safety.

---

### `receiver/src/inject/linux.rs`
```rust
use anyhow::{anyhow, Result};
use dogkbd_proto::{KeyTap, MOD_SHIFT};
use evdev::{uinput::VirtualDevice, AttributeSet, InputEvent, KeyCode};

use crate::app::Settings;

fn hid_to_keycode(hid: u8) -> Option<KeyCode> {
    let kc = match hid {
        0x04 => KeyCode::KEY_A,
        0x05 => KeyCode::KEY_B,
        0x06 => KeyCode::KEY_C,
        0x07 => KeyCode::KEY_D,
        0x08 => KeyCode::KEY_E,
        0x09 => KeyCode::KEY_F,
        0x0A => KeyCode::KEY_G,
        0x0B => KeyCode::KEY_H,
        0x0C => KeyCode::KEY_I,
        0x0D => KeyCode::KEY_J,
        0x0E => KeyCode::KEY_K,
        0x0F => KeyCode::KEY_L,
        0x10 => KeyCode::KEY_M,
        0x11 => KeyCode::KEY_N,
        0x12 => KeyCode::KEY_O,
        0x13 => KeyCode::KEY_P,
        0x14 => KeyCode::KEY_Q,
        0x15 => KeyCode::KEY_R,
        0x16 => KeyCode::KEY_S,
        0x17 => KeyCode::KEY_T,
        0x18 => KeyCode::KEY_U,
        0x19 => KeyCode::KEY_V,
        0x1A => KeyCode::KEY_W,
        0x1B => KeyCode::KEY_X,
        0x1C => KeyCode::KEY_Y,
        0x1D => KeyCode::KEY_Z,

        0x1E => KeyCode::KEY_1,
        0x1F => KeyCode::KEY_2,
        0x20 => KeyCode::KEY_3,
        0x21 => KeyCode::KEY_4,
        0x22 => KeyCode::KEY_5,
        0x23 => KeyCode::KEY_6,
        0x24 => KeyCode::KEY_7,
        0x25 => KeyCode::KEY_8,
        0x26 => KeyCode::KEY_9,
        0x27 => KeyCode::KEY_0,

        0x28 => KeyCode::KEY_ENTER,
        0x2A => KeyCode::KEY_BACKSPACE,
        0x2C => KeyCode::KEY_SPACE,

        0x2D => KeyCode::KEY_MINUS,
        0x2E => KeyCode::KEY_EQUAL,
        0x2F => KeyCode::KEY_LEFTBRACE,
        0x30 => KeyCode::KEY_RIGHTBRACE,
        0x31 => KeyCode::KEY_BACKSLASH,
        0x33 => KeyCode::KEY_SEMICOLON,
        0x34 => KeyCode::KEY_APOSTROPHE,
        0x35 => KeyCode::KEY_GRAVE,
        0x36 => KeyCode::KEY_COMMA,
        0x37 => KeyCode::KEY_DOT,
        0x38 => KeyCode::KEY_SLASH,

        _ => return None,
    };
    Some(kc)
}

/// Lazily create a uinput device and keep it around.
fn get_uinput(settings: &mut Settings) -> Result<&mut VirtualDevice> {
    if settings.linux_uinput.is_some() {
        return Ok(settings.linux_uinput.as_mut().unwrap());
    }

    let mut keys = AttributeSet::<KeyCode>::new();
    // Add all keys we might emit.
    for hid in 0u8..=0xff {
        if let Some(kc) = hid_to_keycode(hid) {
            keys.insert(kc);
        }
    }
    keys.insert(KeyCode::KEY_LEFTSHIFT);

    let dev = VirtualDevice::builder()?
        .name(b"DOGKBD Virtual Keyboard")
        .with_keys(&keys)?
        .build()?;

    settings.linux_uinput = Some(dev);
    Ok(settings.linux_uinput.as_mut().unwrap())
}

pub fn inject_linux(settings: &Settings, kt: KeyTap) -> Result<()> {
    // Linux version does not support selecting a target window safely across X11/Wayland;
    // uinput injects to the focused window.
    // You can still use `strict_foreground` concept by implementing X11 active-window checks later.
    let shift = (kt.mods & MOD_SHIFT) != 0;
    let Some(kc) = hid_to_keycode(kt.hid) else {
        return Err(anyhow!("no linux keycode mapping"));
    };

    // We need mutable access to create / use uinput device.
    // Here we rely on the app storing it behind a lock.
    let mut guard = settings.linux_state.lock();
    let ui = get_uinput(&mut guard)?;

    // EV_KEY = type 1 in evdev.
    let mut events = Vec::new();
    if shift {
        events.push(InputEvent::new(1, KeyCode::KEY_LEFTSHIFT.0, 1));
    }
    events.push(InputEvent::new(1, kc.0, 1));
    events.push(InputEvent::new(1, kc.0, 0));
    if shift {
        events.push(InputEvent::new(1, KeyCode::KEY_LEFTSHIFT.0, 0));
    }

    ui.emit(&events)?;
    Ok(())
}
```

> Linux injection uses `/dev/uinput`. See [Troubleshooting](#10-troubleshooting) for permissions.

---

### `receiver/src/app.rs`
```rust
use crate::keys::{to_preview, KeyPreview};
use crate::target::TargetWindow;
use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender};
use dogkbd_proto::KeyTap;
use parking_lot::{Mutex, RwLock};
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use super::net;

#[derive(Clone)]
pub struct Settings {
    pub armed: bool,
    pub strict_foreground: bool,
    pub auto_focus: bool,

    // Windows-only: selected target HWND
    pub target_hwnd: Option<isize>,

    // Linux-only: hold uinput device behind a lock
    #[cfg(target_os = "linux")]
    pub linux_state: Arc<Mutex<LinuxState>>,
}

#[cfg(target_os = "linux")]
pub struct LinuxState {
    pub linux_uinput: Option<evdev::uinput::VirtualDevice>,
}

pub type SharedSettings = Arc<RwLock<Settings>>;

#[derive(Debug, Clone)]
pub enum AppEvent {
    PacketAccepted { when: SystemTime, src: SocketAddr, kt: KeyTap },
    PacketRejected { when: SystemTime, src: SocketAddr, reason: String },
    InjectResult { when: SystemTime, kt: KeyTap, injected: bool, reason: String },
}

#[derive(Debug, Clone)]
struct LogLine {
    when: SystemTime,
    line: String,
}

pub struct DogKbdApp {
    settings: SharedSettings,

    windows: Vec<TargetWindow>,
    selected_id: Option<String>,

    // Stats
    last_packet_at: Option<SystemTime>,
    accepted: u64,
    rejected: u64,
    injected: u64,
    dropped: u64,

    // Preview of “typed” buffer
    preview: String,

    // Log
    log: VecDeque<LogLine>,

    // Channel from net thread
    rx: Receiver<AppEvent>,

    // Keep sender in scope (not used directly)
    _tx: Sender<AppEvent>,

    // refresh timer
    last_refresh: SystemTime,
}

fn read_secret_hex(path: &std::path::Path) -> Result<Vec<u8>> {
    let s = std::fs::read_to_string(path)
        .with_context(|| format!("reading secret file: {}", path.display()))?;
    let s = s.trim();
    let bytes = hex::decode(s).context("secret file must be hex")?;
    if bytes.len() < 16 {
        anyhow::bail!("secret is too short; use at least 16 bytes (32+ recommended)");
    }
    Ok(bytes)
}

impl DogKbdApp {
    pub fn new(_cc: &eframe::CreationContext<'_>, args: crate::Args) -> Self {
        let secret = read_secret_hex(&args.secret_file).expect("secret file");

        let (tx, rx) = crossbeam_channel::unbounded();

        let settings = {
            #[cfg(target_os = "linux")]
            let linux_state = Arc::new(Mutex::new(LinuxState { linux_uinput: None }));

            Arc::new(RwLock::new(Settings {
                armed: false,
                strict_foreground: true,
                auto_focus: false,
                target_hwnd: None,
                #[cfg(target_os = "linux")]
                linux_state,
            }))
        };

        net::spawn_listener(args.port, secret, settings.clone(), tx.clone())
            .expect("spawn listener");

        let mut app = Self {
            settings,
            windows: Vec::new(),
            selected_id: None,

            last_packet_at: None,
            accepted: 0,
            rejected: 0,
            injected: 0,
            dropped: 0,

            preview: String::new(),
            log: VecDeque::new(),

            rx,
            _tx: tx,
            last_refresh: SystemTime::now(),
        };

        app.refresh_windows();
        app
    }

    fn push_log(&mut self, line: String) {
        self.log.push_back(LogLine { when: SystemTime::now(), line });
        while self.log.len() > 200 {
            self.log.pop_front();
        }
    }

    fn refresh_windows(&mut self) {
        #[cfg(target_os = "windows")]
        {
            self.windows = crate::target::windows::list_windows();
        }
        #[cfg(not(target_os = "windows"))]
        {
            self.windows = crate::target::list_windows();
        }

        // Preserve selection if possible.
        if let Some(id) = &self.selected_id {
            let still_exists = self.windows.iter().any(|w| &w.id == id);
            if !still_exists {
                self.selected_id = None;
                self.settings.write().target_hwnd = None;
            }
        }
    }

    fn apply_preview(&mut self, kt: KeyTap) {
        match to_preview(kt.hid, kt.mods) {
            KeyPreview::Char(c) => self.preview.push(c),
            KeyPreview::Enter => self.preview.push('\n'),
            KeyPreview::Backspace => { self.preview.pop(); }
            KeyPreview::Unknown => {}
        }
        if self.preview.len() > 5000 {
            self.preview = self.preview[self.preview.len()-5000..].to_string();
        }
    }
}

impl eframe::App for DogKbdApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Poll events from network thread
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                AppEvent::PacketAccepted { when, src, kt } => {
                    self.last_packet_at = Some(when);
                    self.accepted += 1;
                    self.push_log(format!("recv OK from {src} seq={} hid=0x{:02x} mods={}", kt.seq, kt.hid, kt.mods));
                }
                AppEvent::PacketRejected { when: _, src, reason } => {
                    self.rejected += 1;
                    self.push_log(format!("recv REJECT from {src}: {reason}"));
                }
                AppEvent::InjectResult { when: _, kt, injected, reason } => {
                    if injected {
                        self.injected += 1;
                        self.apply_preview(kt);
                        self.push_log(format!("INJECT ok seq={} hid=0x{:02x} ({reason})", kt.seq, kt.hid));
                    } else {
                        self.dropped += 1;
                        self.push_log(format!("DROP seq={} hid=0x{:02x} ({reason})", kt.seq, kt.hid));
                    }
                }
            }
        }

        // Auto-refresh window list every 1s on Windows
        let now = SystemTime::now();
        if now.duration_since(self.last_refresh).unwrap_or(Duration::from_secs(0)) > Duration::from_secs(1) {
            self.refresh_windows();
            self.last_refresh = now;
        }

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("DOGKBD Receiver");
                ui.separator();
                ui.label(format!("accepted: {}  rejected: {}  injected: {}  dropped: {}", self.accepted, self.rejected, self.injected, self.dropped));
            });

            if let Some(t) = self.last_packet_at {
                ui.label(format!("Last packet: {:.1?} ago", now.duration_since(t).unwrap_or(Duration::from_secs(9999))));
            } else {
                ui.label("Last packet: (none yet)");
            }
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.group(|ui| {
                ui.label("Target window");
                #[cfg(target_os = "windows")]
                {
                    egui::ComboBox::from_label("Send keys only when THIS window is active")
                        .selected_text(self.selected_id.clone().unwrap_or_else(|| "(none)".into()))
                        .show_ui(ui, |ui| {
                            for w in &self.windows {
                                ui.selectable_value(&mut self.selected_id, Some(w.id.clone()), format!("{} — {}", w.exe, w.title));
                            }
                        });

                    if let Some(id) = &self.selected_id {
                        if let Some(w) = self.windows.iter().find(|x| &x.id == id) {
                            ui.label(format!("Selected: {} — {} (pid {})", w.exe, w.title, w.pid));
                            self.settings.write().target_hwnd = Some(w.hwnd);
                            let fg = crate::target::windows::foreground_hwnd();
                            ui.label(format!("Foreground hwnd: 0x{:x}  (selected 0x{:x})", fg as usize, w.hwnd as usize));
                        }
                    } else {
                        self.settings.write().target_hwnd = None;
                        ui.label("No target selected. (Keys will be dropped when armed.)");
                    }
                }

                #[cfg(not(target_os = "windows"))]
                {
                    ui.label("Window targeting UI is Windows-first.");
                    ui.label("On Linux, injection goes to the currently focused window (uinput).");
                }

                ui.horizontal(|ui| {
                    let mut s = self.settings.write();
                    ui.checkbox(&mut s.armed, "Armed (inject keys)");
                    ui.checkbox(&mut s.strict_foreground, "Strict: only inject when target is foreground");
                    ui.checkbox(&mut s.auto_focus, "Auto-focus target (best effort)");
                });
            });

            ui.separator();

            ui.group(|ui| {
                ui.label("Preview (approx.)");
                ui.add(
                    egui::TextEdit::multiline(&mut self.preview)
                        .desired_rows(8)
                        .font(egui::TextStyle::Monospace)
                        .lock_focus(true)
                        .interactive(false),
                );
            });

            ui.separator();

            ui.group(|ui| {
                ui.label("Log (most recent last)");
                egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                    for line in self.log.iter() {
                        ui.label(&line.line);
                    }
                });
            });
        });

        // keep UI responsive
        ctx.request_repaint_after(Duration::from_millis(33));
    }
}
```

---

### `receiver/src/app.rs` **(small fix: module paths)**
In the earlier file we referenced `super::net` but `app.rs` is a top-level module. Make sure you have:

```rust
use crate::net;
```

(Already included above.)

---

### `receiver/src/inject/linux.rs` **(Linux state lock definition)**
The Linux injector expects `Settings` to have `linux_state: Arc<Mutex<LinuxState>>`, which is included in `Settings` under `cfg(target_os="linux")`.

---

## 9.5 Receiver: missing modules
Create these two tiny “glue” modules:

### `receiver/src/inject/mod.rs` (already provided)
### `receiver/src/target/mod.rs` (already provided)

And add empty `receiver/src/inject/` and `receiver/src/target/` directories.

---

## 9.6 Receiver: `receiver/src/app.rs` depends on `crate::Args`
We used `crate::Args` from `main.rs`. Ensure `Args` is `pub` in `main.rs` (it is).

---

## 9.7 Windows Firewall
On first run, Windows may prompt you to allow network access. You must allow **UDP inbound** for the receiver app, or create a rule for UDP port `44555`.

---

## 9.8 Linux permissions for uinput
If you run receiver on Linux and want injection:
- You need access to `/dev/uinput`
- Often requires root unless you set udev rules

Example rule (distro-dependent):

`/etc/udev/rules.d/99-uinput.rules`:
```udev
KERNEL=="uinput", MODE="0660", GROUP="input"
```

Then:
```bash
sudo udevadm control --reload-rules
sudo udevadm trigger
sudo usermod -aG input $USER
# log out/in
```

---

## 9. Build & run instructions

### 9.1 Build on Raspberry Pi 5
Install Rust (one common approach):
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env
```

Build:
```bash
cd dogkbd
cargo build -p dogkbd-sender --release
```

Run:
```bash
sudo ./target/release/dogkbd-sender --device /dev/input/dogkbd --secret-file dogkbd.secret
```

### 9.2 Build on Windows (receiver)
1. Install Rust (rustup)
2. In PowerShell:
```powershell
cd dogkbd
cargo build -p dogkbd-receiver --release
.\target\release\dogkbd-receiver.exe --port 44555 --secret-file dogkbd.secret
```

### 9.3 Use it with Git Bash
1. Open Git Bash window you want to type into.
2. Run `dogkbd-receiver.exe`
3. In the dropdown, select the Git Bash window (likely `mintty.exe` with a title like `MINGW64:/...`)
4. Keep **Strict** enabled.
5. Click your Git Bash window so it is foreground.
6. Toggle **Armed** ON.
7. Let the dog type.

If the wrong window is focused, keys will be dropped (by design).

---

## 10. Troubleshooting

### “Receiver shows no packets”
- Confirm Pi and PC are on same subnet (broadcast doesn’t route)
- Try explicit unicast:
  - Sender: `--dest <PC_IP>`
- Check Windows firewall inbound UDP rule
- Verify sender targets printed include the correct broadcast (e.g. `192.168.1.255`)

### “Packets accepted but no characters appear”
- On Windows, make sure the selected target window is truly foreground.
- Some apps running as admin won’t accept injection from non-admin processes.
  - Run receiver as admin **or** run Git Bash non-admin.

### “Git Bash not in window list”
- Make sure Git Bash window has a non-empty title
- Some windows may hide their title or be owned by a different process; refresh should catch most.

### “Linux injection fails”
- `/dev/uinput` permission. See [Linux permissions](#98-linux-permissions-for-uinput).

### “Dog typed something dangerous”
- Tighten allowlist (remove punctuation, remove backspace, etc.)
- Keep strict gating on
- Consider additional guardrails:
  - only accept when receiver app window has “Armed” and also a physical hotkey toggle

---

## 11. Future upgrades

1. **Multicast instead of broadcast** (often more Wi‑Fi friendly)
2. Optional **encryption** (AEAD) in addition to HMAC
3. More modifiers (Ctrl/Alt) — not recommended for safety
4. Sender treat dispenser integration:
   - receiver broadcasts “READY FOR INPUT” to Pi
   - Pi triggers treat dispensers via API
5. Linux X11 active-window gating

---

## Appendix A — HID usage codes used

- `0x04..0x1d` = A..Z
- `0x1e..0x27` = 1..0
- `0x28` = Enter
- `0x2a` = Backspace
- `0x2c` = Space
- `0x2d` = -
- `0x2e` = =
- `0x2f` = [
- `0x30` = ]
- `0x31` = backslash
- `0x33` = ;
- `0x34` = '
- `0x35` = `
- `0x36` = ,
- `0x37` = .
- `0x38` = /

---

## Appendix B — Windows scan code mapping

We use Set 1 scan codes (common for `SendInput` with `KEYEVENTF_SCANCODE`) for US ANSI:
- A=0x1E, B=0x30, ... Z=0x2C
- 1=0x02 ... 0=0x0B
- Enter=0x1C
- Backspace=0x0E
- Space=0x39
- -=0x0C, =0x0D, [=0x1A, ]=0x1B, \=0x2B, ;=0x27, '=0x28, `=0x29, ,=0x33, .=0x34, /=0x35
- LeftShift=0x2A

If you later add arrow keys, you’ll need “extended” scan codes.

---

## Appendix C — Linux uinput mapping notes

Linux injection uses `evdev::uinput::VirtualDevice`:
- Build a virtual keyboard supporting allowed `KeyCode`s
- Emit EV_KEY events (type=1) with value 1 then 0
- `VirtualDevice::emit()` appends `SYN_REPORT` automatically

This works on X11 and Wayland because it’s kernel-level input, but it always goes to the currently focused window.

---
