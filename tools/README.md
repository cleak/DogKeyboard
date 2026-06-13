# DOGKBD multiplexer — glue & exhibit setup

These helpers connect the two Claude Code destinations to the router
(`dogkbd-receiver`) and feed the Pi display (`dogkbd-display`). See
[`../MULTIPLEX_DESIGN.md`](../MULTIPLEX_DESIGN.md) for the full architecture.

## Pieces

| File | Runs on | Purpose |
|---|---|---|
| `game_bridge.py` | Windows | polls the tea-leaves Godot DevTools and POSTs `/game-status` (readiness + state) |
| `trophy_monitor.py` | Windows | watches `trophy_factory/runs/` and POSTs `/trophy-status` (current trophy) |
| `claude-status.ps1` | per project | a Claude Code hook that POSTs `/claude-status?instance=…` (busy/idle) |

## Bring-up order

1. **Pi** — keyboard sender + display:
   ```bash
   sudo ./target/release/dogkbd-sender  --device /dev/input/dogkbd
   ./target/release/dogkbd-display --telemetry-port 44556 --web-port 8090
   # open http://<pi>:8090/ fullscreen on the exhibit monitor
   ```
2. **Windows** — the router:
   ```bash
   ./target/release/dogkbd-receiver --port 44555 --web-port 8080 \
       --telemetry-addr 255.255.255.255 --telemetry-port 44556
   ```
   Use `--telemetry-addr <pi-ip>` instead of broadcast if your LAN drops
   broadcast UDP.
3. **Windows** — the two Claude Code terminals, one per project, each in its own
   window so the router can foreground + inject into it:
   ```bash
   cd C:/Projects/Momo/trophy_factory      && claude
   cd C:/Projects/Godot/tea-leaves-wip     && claude
   ```
   Window-match strings live in `receiver/src/router.rs`
   (`RouteTable::default_exhibit`): `trophy_factory` and `tea-leaves`. Adjust if
   your terminal titles differ.
4. **Windows** — the glue:
   ```bash
   python tools/game_bridge.py    --router http://localhost:8080
   python tools/trophy_monitor.py --router http://localhost:8080
   ```

## Claude status hooks

- **tea-leaves** already ships `.claude/hooks/claude-status.ps1` posting to
  `:8080/claude-status` with no instance — the router treats that as the `game`
  route. No change needed.
- **trophy_factory** — add this to its `.claude/settings.json` so its
  interactive Claude instance reports as the `trophy` route (see the header of
  `claude-status.ps1` for the exact JSON):
  ```
  -File C:/Projects/DogKeyboard/tools/claude-status.ps1 -Status busy  -Instance trophy   # UserPromptSubmit
  -File C:/Projects/DogKeyboard/tools/claude-status.ps1 -Status idle  -Instance trophy   # Stop
  ```

## Manual operator override

Pin or release the active route at runtime:
```bash
curl -X POST http://localhost:8080/route -H 'Content-Type: application/json' -d '{"active":"trophy"}'
curl -X POST http://localhost:8080/route -H 'Content-Type: application/json' -d '{"active":"auto"}'
curl -X POST http://localhost:8080/route -H 'Content-Type: application/json' -d '{"enabled":false}'
```
