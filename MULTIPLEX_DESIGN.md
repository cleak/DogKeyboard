# DOGKBD Multiplexer — Design

> Turns DOGKBD from a single-target keyboard proxy into a **router** that sends Momo's
> keystrokes to the right Claude Code instance, and streams rich telemetry back to a
> Raspberry-Pi-hosted display. Built for the *activitiesforhumans.com* exhibit.

## 1. The exhibit, end to end

```
            ┌─────────────┐   USB    ┌────────────────────┐
   Momo ───▶│ rubber kbd  │─────────▶│  Raspberry Pi 5    │
            └─────────────┘          │  dogkbd-sender     │  reads evdev, filters,
                                     │                    │  broadcasts KeyTap (UDP 44555)
                                     │  dogkbd-display    │◀─┐ hosts the monitor web app
                                     └────────────────────┘  │ (HTTP+WS, owns rendering)
                                              │              │
                              KeyTap UDP 44555│              │ Telemetry UDP 44556
                                              ▼              │ (newline-JSON, broadcast)
                                     ┌────────────────────┐  │
                                     │  Windows PC        │  │
                                     │  dogkbd-receiver   │──┘ emits telemetry for EVERY
                                     │  = THE ROUTER      │    event (key, buffer, drop,
                                     │                    │    route, dispatch, status…)
                                     │  ┌──────────────┐  │
                            inject ──┼─▶│ Route: TROPHY │  │  Claude Code terminal in
                            (SendInput)│ └──────────────┘  │  C:\Projects\Momo\trophy_factory
                                     │  ┌──────────────┐  │
                                     │  │ Route: GAME   │  │  Claude Code terminal in
                                     │  └──────────────┘  │  C:\Projects\Godot\tea-leaves-wip
                                     └────────────────────┘
                                              ▲  ▲
                  POST /claude-status?instance=…  │  POST /game-status (readiness)
                  (each project's .claude hook)   │  (game_bridge.py polls Godot devtools)
                                                  │
                  GET trophy_factory /runs  ──────┘  (trophy_monitor.py → POST /trophy-status)
```

Three processes, two of them on the Pi:

| Process | Host | Role |
|---|---|---|
| `dogkbd-sender` | Pi | unchanged — reads keyboard, broadcasts `KeyTap` packets |
| `dogkbd-receiver` | Windows | **the router** — receives keys, decides a destination, injects into the chosen Claude Code window, and emits telemetry |
| `dogkbd-display` | Pi | **new** — listens for telemetry, serves the exhibit monitor web app, fans events out to the browser over WebSocket |

## 2. Why this shape

- **Both targets are already Claude Code instances that fire `busy`/`idle` hooks at
  `http://<router>:8080/claude-status`.** tea-leaves ships this hook today; trophy_factory
  gets the same hook (with an `instance` tag). So "is this destination ready?" is already a
  solved signal — we just need to track it *per instance* instead of as one global flag.
- **The receiver already targets a window and injects keystrokes via `SendInput`**, with
  auto-enter-on-idle and busy-aware backoff. Routing to "a Claude Code instance" is literally
  "pick which window to foreground and inject into." Multiplexing = N target windows + a policy.
- **The Pi should decide what to display.** So the router does *not* format anything for the
  screen. It emits a firehose of structured events; the Pi keeps the authoritative display
  state and renders it. We can redesign the monitor entirely without touching the router.

## 3. Transport to the Pi (telemetry channel)

A second UDP stream, mirroring the proven forward-channel design:

- **Port 44556**, broadcast, **newline-delimited JSON**, one `TelemetryEvent` per datagram.
- **Duplicate-send** (default 2×) like `KeyTap`, with a **monotonic `seq`** per router run and a
  `run_id` (random per process start). The display de-dupes on `(run_id, seq)` and can *detect
  drops* by gaps in `seq` — which is itself surfaced on screen ("N events dropped").
- Fire-and-forget, connectionless, restart-resilient on either end — critical for an unattended
  exhibit. Latest-wins; no head-of-line blocking; no reconnect dance.
- Events are small (< 1 KB). Larger free-text (a trophy's interpretation) is truncated to a
  documented cap so a single event always fits one datagram.

Rationale over TCP/WebSocket-from-router: matches the existing stateless/broadcast ethos, and an
exhibit benefits more from "any box can reboot and rejoin silently" than from guaranteed delivery
of a live display feed.

### Envelope

```jsonc
{ "v": 1, "run_id": 2748392011, "seq": 412, "ts_ms": 1749700000123, "kind": "keystroke", ... }
```

### Event catalog (`kind`)

| kind | when | key fields |
|---|---|---|
| `hello` | router start / heartbeat (1 Hz) | `routes:[{id,label,busy,ready}]`, `active` |
| `keystroke` | every key Momo presses | `disposition` (`accepted`\|`blocked`\|`dup`), `hid`, `shift`, `decoded` (char/`SPACE`/`ENTER`/`BACKSPACE`), `device_id` |
| `buffer` | input buffer changed | `route`, `text` (current decoded buffer), `len` |
| `drop` | a key/packet was discarded | `reason` (`disarmed`\|`no_route`\|`blocked`\|`dedup`\|`target_not_foreground`), `hid?` |
| `route_decision` | a burst is about to be sent | `route`, `reason`, `candidates:[{id,busy,ready}]` |
| `dispatch` | input actually injected to a target | `route`, `text`, `chars`, `auto_enter` |
| `route_status` | a route's busy/idle/ready flips | `route`, `busy`, `ready`, `source` |
| `trophy` | trophy progress (from monitor) | `run_id`, `award_title`, `family`, `topper`, `source_burst`, `status`, `created_at` |
| `game` | game state (from bridge) | `state`, `phase`, `wave`, `integrity`, `score`, `ready`, `last_input`, `last_input_decoded` |

This is intentionally a superset of what the monitor shows today, so "the Pi decides" stays true.

## 4. Routing policy (the multiplexer)

Live keystrokes **always** stream to the Pi the instant they arrive (panel 1), regardless of
routing. Routing governs only where *bursts* get injected.

1. **Accumulate.** Each accepted key appends to the active input buffer and emits `keystroke` +
   `buffer`. Backspace pops; Space/letters/punct append.
2. **Burst boundary.** Finalize the buffer when Momo idles for `burst_idle` (default 2.5 s) or the
   buffer reaches `burst_max` chars.
3. **Choose a route** at boundary time:
   - Eligible = routes whose Claude is **idle**. The **GAME** route is additionally gated on
     `game_ready` (Godot reports an input-accepting state).
   - Among eligible routes, pick by configured **priority** (default: GAME > TROPHY when the game
     is ready, else TROPHY).
   - If **no route is eligible**, the buffer is **held** (not dropped) and we emit
     `route_decision{reason:"waiting"}`; it dispatches as soon as a route frees up.
4. **Dispatch.** Foreground the route's window, inject the buffered text char-by-char (existing
   `SendInput` path), then auto-enter. Emit `route_decision` + `dispatch`. Clear buffer.
5. **Safety unchanged.** Sender-side and receiver-side allowlists both still apply; nothing
   outside the allowlist can ever be injected or even buffered.

Manual override for the exhibit operator: `POST /route {"active":"trophy"|"game"|"auto"}` pins or
releases the active route; surfaced as a `route_status` event.

## 5. How each destination is driven & monitored

### TROPHY — `C:\Projects\Momo\trophy_factory` (Claude Code terminal)
- **Drive:** inject the burst into the trophy_factory Claude Code terminal window (route = window
  match on title/process). Momo's text becomes the "keyboard burst" Claude compiles into a trophy.
- **Ready signal:** the project's `.claude` `busy`/`idle` hook → `POST /claude-status?instance=trophy`.
  (Hook + script added by this change; mirrors tea-leaves.)
- **Progress for display:** `tools/trophy_monitor.py` polls `trophy_factory`'s `runs/` (or its
  `/runs` HTTP endpoint), reads the newest `manifest.json`, and `POST`s a `trophy` telemetry event
  to the router: `award_title`, `trophy_family`, `topper`, `source_burst`, artifact-complete status.

### GAME — `C:\Projects\Godot\tea-leaves-wip` (Claude Code terminal)
- **Drive:** inject the burst into the tea-leaves Claude Code terminal window. Momo's text becomes
  the cryptic design prompt Claude iterates the game from.
- **Busy/idle:** existing `.claude/hooks/claude-status.ps1` → `POST /claude-status?instance=game`.
- **Ready signal:** `tools/game_bridge.py` uses the game's own file-based **DevTools** protocol
  (`devtools.py run-method --method debug_snapshot`) to read `phase`/`state`, and `POST`s
  `/game-status {"ready":true,"state":"PLAYING","wave":3,...}`. The router stores it; it also
  re-emits it as a `game` telemetry event for the display.

Both destinations share one mechanism (window inject) and one readiness convention (HTTP status
posts), which is what makes the router small and the policy declarative.

## 6. Receiver (router) internals

New/changed modules in `receiver/`:

- `telemetry.rs` — owns the telemetry UDP socket, `run_id`, `seq`; `emit(TelemetryEvent)` from any
  thread (cheap channel → background sender that duplicate-sends).
- `router.rs` — `RouteTable` (per-route `busy`/`ready`/`window_match`/`priority`), the burst buffer,
  the policy in §4, and dispatch (foreground + inject + auto-enter).
- `overlay.rs` — extend HTTP API: `/claude-status?instance=…`, new `/game-status`,
  `/trophy-status`, `/route`. Keep `/` + `/ws` for the existing OBS overlay (still works).
- `app.rs` — the egui control panel gains per-route status, the active route, manual override, and a
  live telemetry tail for the operator.

Config: `routes.toml` (window match strings, priority, idle/burst timings) so the exhibit can be
retuned without recompiling. Sensible built-in defaults if absent.

## 7. Shared protocol (`proto/`)

`proto` gains `serde` and a `telem` module with `TelemetryEvent` + envelope, shared by the router
(emit) and the display (parse). The 16-byte `KeyTap` wire format is **unchanged**.

## 8. Pi display (`dogkbd-display/`)

- Rust `axum` server: UDP listener on 44556 → keeps in-memory display state → broadcasts each event
  to browser WebSocket clients (and serves a state snapshot on connect so a refreshed kiosk catches
  up instantly).
- `display.html` kiosk app, three panels, all rendering logic client-side ("the Pi decides"):
  1. **Live keys** — Momo's keystrokes one-by-one as she types (driven by `keystroke`).
  2. **Current game** — game state + her last input for the game + its decoded text (`game`).
  3. **Current trophy** — trophy being made + her input + the decoded/awarded result (`trophy`).
- Connection + drop indicators driven by `hello` heartbeats and `seq` gap detection.

## 9. Build / run

```bash
# Pi
cargo build -p dogkbd-sender  --release
cargo build -p dogkbd-display --release
sudo ./target/release/dogkbd-sender  --device /dev/input/dogkbd
./target/release/dogkbd-display --telemetry-port 44556 --web-port 8090

# Windows (router)
cargo build -p dogkbd-receiver --release
./target/release/dogkbd-receiver --port 44555 --web-port 8080 \
    --telemetry-addr 255.255.255.255 --telemetry-port 44556

# Glue (run alongside the router, on Windows)
python tools/game_bridge.py     --router http://localhost:8080
python tools/trophy_monitor.py  --router http://localhost:8080
```

## 10. Open items / tuning knobs
- Burst boundary timing (`burst_idle`, `burst_max`) tuned live during exhibit rehearsal.
- Window-match strings for each Claude terminal depend on how the operator launches them
  (Windows Terminal tab title, etc.); configurable in `routes.toml`.
- "Game ready" definition (which Godot `state`/`phase` counts as ready) lives in `game_bridge.py`.
</content>
</invoke>
