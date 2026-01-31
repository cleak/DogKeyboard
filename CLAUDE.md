# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

DOGKBD is a network keyboard proxy that captures keystrokes from a USB keyboard connected to a Raspberry Pi 5 and broadcasts them over UDP to a Windows/Linux receiver for injection. The primary use case is allowing a dog to "type" on a rubber keyboard while filtering dangerous keys.

## Architecture

**Cargo Workspace Structure:**
```
dogkbd/
  proto/      - Shared protocol: packet encoding/decoding, HMAC auth, HID allowlist
  sender/     - Pi daemon: reads evdev, filters keys, broadcasts UDP packets
  receiver/   - Windows/Linux GUI: receives packets, targets windows, injects keystrokes
```

**Data Flow:**
1. Sender reads `/dev/input/dogkbd` (evdev), tracks shift state, filters via allowlist
2. Broadcasts 32-byte HMAC-authenticated KeyTap packets over UDP (port 44555)
3. Receiver verifies HMAC, deduplicates by sequence number, checks allowlist
4. If armed and target window is foreground, injects keystroke via `SendInput` (Windows) or `uinput` (Linux)

**Key Design Decisions:**
- Stateless "tap" events (press+release) — no stuck keys possible
- Sender-side AND receiver-side allowlist for safety belt
- Duplicate-send (2x) for UDP reliability; receiver deduplicates
- `device_id` randomized per sender run to handle restarts cleanly

## Build Commands

```bash
# Build sender (on Raspberry Pi)
cargo build -p dogkbd-sender --release

# Build receiver (on Windows/Linux)
cargo build -p dogkbd-receiver --release

# Run sender (requires sudo for evdev grab)
sudo ./target/release/dogkbd-sender --device /dev/input/dogkbd --secret-file dogkbd.secret

# Run receiver
./target/release/dogkbd-receiver --port 44555 --secret-file dogkbd.secret
```

## Protocol Spec (32-byte packet, little-endian)

| Offset | Size | Field |
|--------|------|-------|
| 0 | 4 | `DOGK` magic |
| 4 | 1 | version (1) |
| 5 | 1 | msg_type (1=KeyTap) |
| 6 | 4 | device_id |
| 10 | 4 | seq |
| 14 | 1 | mods (bit0=shift) |
| 15 | 1 | HID usage code |
| 16 | 16 | HMAC-SHA256 truncated |

## Key Allowlist (Safety Critical)

Only these HID codes pass the allowlist:
- `0x04-0x1d`: A-Z
- `0x1e-0x27`: 1-0
- `0x28`: Enter
- `0x2a`: Backspace
- `0x2c`: Space
- `0x2d-0x38`: Punctuation (- = [ ] \ ; ' ` , . /)

Everything else (Esc, Tab, Ctrl, Alt, Meta, F-keys) is blocked at both sender and receiver.

## Platform-Specific Notes

**Sender (Linux/Pi):**
- Requires `evdev` crate access to `/dev/input/*`
- Uses `grab()` to prevent local echo
- Create udev symlink `/dev/input/dogkbd` for stable device path

**Receiver (Windows):**
- Uses `SendInput` with scan codes — injects to foreground window only
- Window targeting requires selected window to be foreground (strict mode)
- May need to run as admin if target app runs elevated

**Receiver (Linux):**
- Uses `/dev/uinput` — requires permissions (add user to `input` group)
- No window targeting; injects to focused window

## Secret File

Generate with: `openssl rand -hex 32 > dogkbd.secret`

Copy to both Pi and receiver machine. Anyone with this file can inject keystrokes.
