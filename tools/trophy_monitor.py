#!/usr/bin/env python3
"""Trophy progress monitor for the DOGKBD multiplexer.

Watches the trophy_factory `runs/` directory for the newest run, reads its
`manifest.json`, and POSTs the trophy's details to the router's
`/trophy-status` endpoint whenever the current trophy changes. The router
re-emits it as telemetry for the Pi display (panel 3).

Usage:
    python tools/trophy_monitor.py --router http://localhost:8080 \
        --runs-dir "C:/Projects/Momo/trophy_factory/runs" --interval 1.0
"""
import argparse
import json
import os
import sys
import time
import urllib.request


def newest_run(runs_dir: str) -> str | None:
    try:
        entries = [
            os.path.join(runs_dir, d)
            for d in os.listdir(runs_dir)
            if os.path.isdir(os.path.join(runs_dir, d))
        ]
    except FileNotFoundError:
        return None
    if not entries:
        return None
    return max(entries, key=os.path.getmtime)


def read_manifest(run_dir: str) -> dict | None:
    path = os.path.join(run_dir, "manifest.json")
    try:
        with open(path, "r", encoding="utf-8") as f:
            return json.load(f)
    except (FileNotFoundError, json.JSONDecodeError):
        return None


def to_payload(run_dir: str, manifest: dict) -> dict:
    spec = manifest.get("spec", {}) or {}
    artifacts = manifest.get("artifacts", []) or []
    # An STL among the artifacts means geometry compiled successfully.
    complete = any(str(a).lower().endswith(".stl") for a in _flatten(artifacts))
    return {
        "run_id": os.path.basename(run_dir),
        "award_title": spec.get("award_title", ""),
        "family": spec.get("trophy_family", ""),
        "topper": spec.get("topper", ""),
        "source_burst": spec.get("source_burst") or spec.get("seed", ""),
        "status": "complete" if complete else "compiling",
        "created_at": manifest.get("created_at", ""),
    }


def _flatten(obj):
    """Yield leaf strings from a list/dict of artifact paths."""
    if isinstance(obj, str):
        yield obj
    elif isinstance(obj, dict):
        for v in obj.values():
            yield from _flatten(v)
    elif isinstance(obj, (list, tuple)):
        for v in obj:
            yield from _flatten(v)


def post(router: str, payload: dict) -> None:
    data = json.dumps(payload).encode()
    req = urllib.request.Request(
        f"{router}/trophy-status", data=data,
        headers={"Content-Type": "application/json"}, method="POST",
    )
    try:
        urllib.request.urlopen(req, timeout=2).read()
    except Exception as e:  # noqa: BLE001 - best-effort telemetry
        print(f"[trophy_monitor] post failed: {e}", file=sys.stderr)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--router", default="http://localhost:8080")
    ap.add_argument("--runs-dir", default="C:/Projects/Momo/trophy_factory/runs")
    ap.add_argument("--interval", type=float, default=1.0)
    args = ap.parse_args()

    print(f"[trophy_monitor] watching {args.runs_dir} -> {args.router}/trophy-status")
    last_key = None
    while True:
        run_dir = newest_run(args.runs_dir)
        if run_dir:
            manifest = read_manifest(run_dir)
            if manifest is not None:
                payload = to_payload(run_dir, manifest)
                # Re-post when the run or its completion status changes.
                key = (payload["run_id"], payload["status"])
                if key != last_key:
                    post(args.router, payload)
                    print(f"[trophy_monitor] {payload['status']}: "
                          f"{payload['run_id']} — {payload['award_title']!r}")
                    last_key = key
        time.sleep(args.interval)


if __name__ == "__main__":
    raise SystemExit(main())
