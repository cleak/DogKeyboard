# DOGKBD

A network keyboard proxy that lets a dog type on a rubber USB keyboard connected to a Raspberry Pi, with keystrokes routed over UDP to a Windows/Linux receiver for injection into any application. Built-in safety filtering, treat dispensing, and an OBS streaming overlay round out the system.

This is one piece of a larger project to get my dog to vibe code video games. The game made with it: **[Quasar Saz](https://github.com/cleak/quasar-saz)**

## How It Works

```
┌─────────────────┐        UDP broadcast        ┌─────────────────────────────┐
│  Raspberry Pi   │  ─────────────────────────>  │  Windows / Linux Receiver   │
│                 │       port 44555             │                             │
│  USB keyboard   │       16-byte packets        │  Keystroke injection        │
│  evdev → filter │       (2x for reliability)   │  OBS overlay (WebSocket)    │
│  → encode       │                              │  Treat dispenser (SSH)      │
└─────────────────┘                              │  Claude Code integration    │
                                                 └─────────────────────────────┘
```

1. **Sender** (Pi) reads the keyboard via evdev, filters through a safety allowlist, and broadcasts UDP packets
2. **Receiver** (Windows/Linux) deduplicates packets, re-checks the allowlist, and injects keystrokes into the selected target window via `SendInput` (Windows) or `uinput` (Linux)
3. An **OBS overlay** served on port 8080 shows typed keys in real time with pop-in animations
4. A **treat dispenser** rewards the dog via SSH command when typing thresholds are met

## Safety

The system is designed around the principle that a dog should never be able to close a window, switch tabs, or trigger any dangerous key combination.

**Double allowlist** — keys are filtered at both sender and receiver. Only these HID codes pass:

| Keys | HID Range |
|------|-----------|
| A–Z | `0x04–0x1d` |
| 0–9 | `0x1e–0x27` |
| Enter | `0x28` |
| Space | `0x2c` |
| Punctuation (`-=[]\;'`,.`) | `0x2d–0x37` |

Everything else is blocked: Escape, Tab, Ctrl, Alt, Meta, F-keys, and all other system keys. Remote Enter keys from the keyboard are also filtered out at the receiver — only auto-generated Enters are injected, with configurable timing.

Additional safety measures:
- **Stateless tap events** — press+release pairs only, no stuck keys possible
- **Foreground-only injection** — keystrokes only go to the selected target window when it has focus
- **Packet deduplication** — sequence number tracking prevents double injection from UDP retransmits
- **Randomized device ID** per sender session to handle restarts cleanly

**Network trust warning:** There is no encryption or authentication. The sender broadcasts keystrokes to the entire local network, and the receiver accepts any packet with valid `DOGK` framing. This is convenient for setup but means anyone on the same network can sniff keystrokes or inject fake ones (limited to the allowlisted keys above). Run on a trusted or isolated network.

## Features

### Receiver GUI

The receiver provides an egui desktop app with:

- **Arm/disarm toggle** — master switch for all keystroke injection
- **Target window selector** — pick which window receives keystrokes
- **Key preview** — scrollable display of the last 50 keys typed
- **Input delay** — configurable 0–5000ms buffer before injection

### Automation

- **Auto-enter on idle** — automatically submits after 5s of no input (requires 16+ total chars and 4+ chars while Claude is idle)
- **Periodic auto-enter** — sends Enter every 15s while Claude Code is busy, nudging continued typing
- **Busy-state cleanup** — automatically backspaces buffered text if the dog types while Claude is processing

### Claude Code Integration

The receiver exposes `POST /claude-status` on its web server port. A hook script sends `{"status":"busy"}` or `{"status":"idle"}` to coordinate automation features. On busy-to-idle transitions, the receiver plays a chime, focuses the target window, and resets treat tracking.

### Treat Dispensing

When typing thresholds are met (16 total characters + 4 while Claude is idle), the receiver SSHs to a remote machine to trigger a treat dispenser — positive reinforcement for productive typing.

### OBS Overlay

An HTML overlay served at `http://localhost:8080` connects via WebSocket and displays each keystroke with a pop-in animation that fades after 6 seconds. Transparent background for easy OBS scene composition.

## Project Structure

```
dogkbd/
  proto/      Shared protocol: packet encoding/decoding, HID allowlist (zero dependencies)
  sender/     Pi daemon: evdev keyboard reading, filtering, UDP broadcast
  receiver/   Windows/Linux GUI: injection, OBS overlay, treat dispenser, automation
```

## Building

Requires Rust (edition 2024).

```bash
# Receiver (on Windows/Linux)
cargo build -p dogkbd-receiver --release

# Sender (on Raspberry Pi)
cargo build -p dogkbd-sender --release
```

## Running

```bash
# Sender (requires sudo for evdev grab)
sudo ./target/release/dogkbd-sender --device /dev/input/dogkbd

# Receiver
./target/release/dogkbd-receiver --port 44555 --web-port 8080
```

### Sender Options

| Flag | Default | Description |
|------|---------|-------------|
| `-d, --device` | `/dev/input/dogkbd` | evdev input device path |
| `-p, --port` | `44555` | UDP broadcast port |
| `-b, --broadcast` | `255.255.255.255` | Broadcast address |
| `--duplicate` | `2` | Packets sent per keystroke |

### Receiver Options

| Flag | Default | Description |
|------|---------|-------------|
| `--port` | `44555` | UDP listen port |
| `--web-port` | `8080` | HTTP/WebSocket server port |

## Protocol

16-byte little-endian UDP packets:

| Offset | Size | Field |
|--------|------|-------|
| 0 | 4 | `DOGK` magic |
| 4 | 1 | Version (1) |
| 5 | 1 | Message type (1 = KeyTap) |
| 6 | 4 | Device ID |
| 10 | 4 | Sequence number |
| 14 | 1 | Modifiers (bit 0 = shift) |
| 15 | 1 | HID usage code |

## Related Projects

- **[Quasar Saz](https://github.com/cleak/quasar-saz)** - The finished game made with this system, designed by Momo and developed by Claude Code ([watch the video](https://youtu.be/8BbPlPou3Bg))
- **[tea-leaves](https://github.com/cleak/tea-leaves)** - The base Godot project with all the tools and guardrails for making a game with Claude Code

## License

MIT
