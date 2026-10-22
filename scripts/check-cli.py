#!/usr/bin/env python3
"""Exercise a running app's CLI against an empty, explicitly isolated library."""
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import time

if len(sys.argv) not in (2, 3) or (len(sys.argv) == 3 and sys.argv[2] != "--keychain"):
    raise SystemExit("Usage: SSH_M8_DATA_DIR=/isolated/path python3 scripts/check-cli.py BINARY [--keychain]")
binary = str(Path(sys.argv[1]).resolve())
directory = Path(os.environ.get("SSH_M8_DATA_DIR", "")).resolve()
if not os.environ.get("SSH_M8_DATA_DIR") or "Application Support" in directory.parts:
    raise SystemExit("Use an explicit isolated SSH_M8_DATA_DIR, outside Application Support.")


def run(*args, expected=0, password=None):
    result = subprocess.run([binary, *map(str, args)], input=password, text=True, capture_output=True, timeout=25)
    assert result.returncode == expected, (args, result.returncode, result.stdout, result.stderr)
    response = json.loads(result.stdout)
    assert response["ok"] == (expected == 0), response
    if password:
        assert password.strip() not in result.stdout + result.stderr
    return response.get("data")


def wait_status(state):
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        tunnels = run("tunnels", "list")
        if all(t["status"] == state for t in tunnels):
            return
        time.sleep(0.1)
    raise AssertionError("Tunnels did not reach " + state)


assert run("hosts", "list") == [], "Test library must start empty"
assert run("tunnels", "list") == [], "Test library must start empty"
assert run("status")["total"] == 0
host_id = None
tunnel_ids = []
try:
    # Hold an unlistening loopback port so connection attempts fail locally.
    with socket.socket() as unavailable:
        unavailable.bind(("127.0.0.1", 0))
        host = run("hosts", "add", "--name", "CLI fixture", "--address", "127.0.0.1",
                   "--user", "developer", "--port", unavailable.getsockname()[1])
        host_id = host["id"]
        assert host["saved_password"] is False
        run("hosts", "edit", host_id, "--address", "bad;hostname", expected=1)
        run("hosts", "edit", host_id, "--port", "0", expected=2)
        run("hosts", "edit", host_id, "--name", "Renamed fixture")
        for kind in ("local", "remote", "dynamic"):
            tunnel = run("tunnels", "add", "--name", kind, "--host", host_id, "--kind", kind,
                         "--listen", "18080", "--destination-port", "8080", "--keep-retrying", "true")
            tunnel_ids.append(tunnel["id"])
        run("hosts", "remove", host_id, expected=1)
        run("tunnels", "connect", "all")
        wait_status("retrying")
        run("tunnels", "edit", tunnel_ids[0], "--name", "Blocked", expected=1)
        run("tunnels", "remove", tunnel_ids[0], expected=1)
        run("hosts", "edit", host_id, "--name", "Blocked", expected=1)
        changed = run("tunnels", "edit", tunnel_ids[0], "--keep-retrying", "false", "--auto-connect", "true")
        assert not changed["keep_retrying"] and changed["auto_connect"]
        deadline = time.monotonic() + 5
        while True:
            tunnel = next(t for t in run("tunnels", "list") if t["id"] == tunnel_ids[0])
            if tunnel["status"] == "disconnected":
                break
            assert time.monotonic() < deadline, "Disabling reconnect did not cancel the pending retry"
            time.sleep(0.1)
        run("tunnels", "disconnect", "all")
        wait_status("disconnected")
        changed = run("tunnels", "edit", tunnel_ids[0], "--auto-connect", "true", "--keep-retrying", "false")
        assert changed["auto_connect"] and not changed["keep_retrying"]
        assert changed["host_id"] == host_id
        if len(sys.argv) == 3:
            password = "disposable-cli-fixture-value\n"
            assert run("hosts", "set-password", host_id, "--stdin", password=password)["saved_password"]
            assert run("hosts", "list")[0]["saved_password"]
            assert password.strip() not in (directory / "library.json").read_text()
            assert password.strip() not in json.dumps(run("logs", "--limit", "300"))
            assert not run("hosts", "clear-password", host_id)["saved_password"]
        assert 0 < len(run("logs", "--limit", "3")) <= 3
        assert run("status")["disconnected"] == 3
finally:
    for tunnel_id in tunnel_ids:
        run("tunnels", "disconnect", tunnel_id)
    wait_status("disconnected")
    for tunnel_id in tunnel_ids:
        run("tunnels", "remove", tunnel_id)
    if host_id is not None:
        run("hosts", "remove", host_id)
assert run("status")["total"] == 0
assert run("hosts", "list") == []
print("CLI checks passed: CRUD, validation, connection control, retry cancellation, logs, and cleanup.")
