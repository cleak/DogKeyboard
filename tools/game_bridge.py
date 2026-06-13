#!/usr/bin/env python3
"""Game readiness bridge for the DOGKBD multiplexer.

Polls the tea-leaves Godot game via its own file-based DevTools protocol
(`tools/devtools.py run-method ... --method debug_snapshot`) and POSTs the
result to the router's `/game-status` endpoint. The router uses `ready` to
gate the GAME route and re-emits the state as telemetry for the Pi display.

"Ready for input" here means: the game process answered and is not mid-
countdown. Tune `is_ready()` for the exhibit as needed.

Usage:
    python tools/game_bridge.py --router http://localhost:8080 \
        --game-dir "C:/Projects/Godot/tea-leaves-wip" --interval 0.5
"""
import argparse
import json
import subprocess
import sys
import time
import urllib.request

# Godot Phase enum (game.gd): COUNTDOWN, PLAYING, INTERMISSION, ENDED
PHASE_NAMES = {0: "COUNTDOWN", 1: "PLAYING", 2: "INTERMISSION", 3: "ENDED"}


def snapshot(game_dir: str) -> dict | None:
    """Run the game's DevTools debug_snapshot and return the parsed dict."""
    try:
        proc = subprocess.run(
            [sys.executable, "tools/devtools.py", "run-method",
             "--node", "/root/Game", "--method", "debug_snapshot"],
            cwd=game_dir, capture_output=True, text=True, timeout=8,
        )
    except (subprocess.TimeoutExpired, FileNotFoundError) as e:
        print(f"[game_bridge] devtools call failed: {e}", file=sys.stderr)
        return None
    if proc.returncode != 0:
        return None
    # devtools.py prints a JSON result; find the JSON object in stdout.
    out = proc.stdout.strip()
    start = out.find("{")
    if start < 0:
        return None
    try:
        return json.loads(out[start:])
    except json.JSONDecodeError:
        return None


def is_ready(snap: dict) -> bool:
    """The game is ready for fresh input when it is alive and not counting down."""
    phase = snap.get("phase")
    return phase is not None and phase != 0


def post(router: str, payload: dict) -> None:
    data = json.dumps(payload).encode()
    req = urllib.request.Request(
        f"{router}/game-status", data=data,
        headers={"Content-Type": "application/json"}, method="POST",
    )
    try:
        urllib.request.urlopen(req, timeout=2).read()
    except Exception as e:  # noqa: BLE001 - best-effort telemetry
        print(f"[game_bridge] post failed: {e}", file=sys.stderr)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--router", default="http://localhost:8080")
    ap.add_argument("--game-dir", default="C:/Projects/Godot/tea-leaves-wip")
    ap.add_argument("--interval", type=float, default=0.5)
    args = ap.parse_args()

    print(f"[game_bridge] polling {args.game_dir} -> {args.router}/game-status")
    last_ready = None
    while True:
        snap = snapshot(args.game_dir)
        if snap is None:
            # Game not reachable -> not ready.
            if last_ready is not False:
                post(args.router, {"ready": False, "state": "OFFLINE", "phase": "?"})
                last_ready = False
        else:
            ready = is_ready(snap)
            phase = snap.get("phase")
            payload = {
                "ready": ready,
                "state": PHASE_NAMES.get(phase, str(phase)),
                "phase": PHASE_NAMES.get(phase, str(phase)),
                "wave": int(snap.get("wave", 0)),
                "integrity": float(snap.get("integrity", 0.0)),
                "score": int(snap.get("score", 0)),
                # last_input / last_input_decoded are filled by the router on dispatch;
                # left blank here so we don't clobber them.
                "last_input": "",
                "last_input_decoded": "",
            }
            post(args.router, payload)
            last_ready = ready
        time.sleep(args.interval)


if __name__ == "__main__":
    raise SystemExit(main())
