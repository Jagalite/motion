#!/usr/bin/env python3
"""Drive the real `motion tui` in a pseudo-terminal against a real server.

Model tests do not stand in for keyboard and terminal behavior. This check
starts an isolated server, seeds a library and a finished scan job through
the CLI, then types into the TUI: it creates a profile with the profile form,
inspects the job, reviews and confirms a retry, and checks the outcomes
through the CLI (the same shared operations the TUI used).

Usage: tui_pty_check.py SERVER_BINARY MOTION_BINARY
Prints one JSON receipt and exits non-zero if any check fails.
"""
import fcntl
import json
import os
import pty
import re
import select
import struct
import subprocess
import sys
import tempfile
import termios
import time
import urllib.request

ANSI = re.compile(r"\x1b\[[0-9;?<>=]*[ -/]*[@-~]|\x1b\][^\x07]*\x07|\x1b.")


def main(server_bin, motion):
    data, conf, media = (tempfile.mkdtemp() for _ in range(3))
    server = subprocess.Popen([server_bin, "--listen", "127.0.0.1:0", "--data-dir", data, "--access-mode", "restricted"],
                              stdout=subprocess.PIPE, text=True)
    try:
        url = None
        for line in server.stdout:
            if line.startswith("Motion is running at "):
                url = line.split("at ", 1)[1].strip()
                break
        token = os.path.join(data, "admin-token")
        base = [motion, "--server", url, "--config-dir", conf, "--operator-token-file", token, "--json"]

        def cli(*args):
            out = subprocess.run(base + list(args), capture_output=True, text=True)
            return json.loads(out.stdout)["data"] if out.returncode == 0 else {"error": out.stderr}

        source = cli("sources", "add", "PTY source", "--root", media)
        library = cli("libraries", "add", "PTY library", "--source", source["id"])
        request = urllib.request.Request(url + "/api/v2/libraries/" + library["id"] + "/scans", method="POST",
                                         data=b'{"mode":"incremental","require_complete":false}',
                                         headers={"Authorization": "Bearer " + open(token).read().strip(),
                                                  "Content-Type": "application/json", "Idempotency-Key": "pty-check-scan-000001"})
        job = json.load(urllib.request.urlopen(request))["job_ids"][0]
        for _ in range(150):
            if cli("jobs", "show", job).get("phase") in ("completed", "failed", "cancelled"):
                break
            time.sleep(0.2)
        phase = cli("jobs", "show", job).get("phase")

        pid, fd = pty.fork()
        if pid == 0:
            os.environ.update(TERM="xterm-256color", NO_COLOR="1")
            os.execv(motion, [motion, "--server", url, "--config-dir", conf, "--operator-token-file", token, "tui"])
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 140, 0, 0))
        screen = {"bytes": b""}

        def read(seconds):
            end = time.time() + seconds
            while time.time() < end:
                ready, _, _ = select.select([fd], [], [], 0.05)
                if ready:
                    try:
                        screen["bytes"] += os.read(fd, 65536)
                    except OSError:
                        return

        def text():
            return ANSI.sub("", screen["bytes"].decode("utf-8", "replace"))

        def press(keys, seconds=0.6):
            screen["bytes"] = b""
            os.write(fd, keys)
            read(seconds)
            return text()

        def page(marker):
            # The sidebar order is the interface's; search both directions.
            for key, steps in ((b"\x1b[A", 30), (b"\x1b[B", 60)):
                for _ in range(steps):
                    if marker in press(key, 0.4):
                        return True
            return False

        checks = {}
        try:
            read(2.0)
            checks["profiles_page"] = page("Viewing profiles")
            press(b"\t", 0.4)
            checks["profile_form_opens"] = "Create viewing profile" in press(b"n", 0.8)
            press(b"PTY profile", 0.4)
            checks["profile_review"] = "Confirm?" in press(b"\x13", 0.8)
            press(b"y", 1.5)
            profiles = cli("profiles", "list")
            checks["profile_created"] = any(p["name"] == "PTY profile" for p in profiles.get("items", []))
            press(b"\t", 0.4)
            checks["processing_page"] = page(job[:8])
            press(b"\t", 0.4)
            checks["inspect"] = "Job details refreshed." in press(b"\r", 1.5)
            checks["retry_reviewed"] = ("Retry this %s job?" % phase) in press(b"R", 1.5)
            outcome = press(b"y", 1.5)
            if phase == "completed":
                checks["retry_refused_by_server"] = "Couldn't retry the job" in outcome
            else:
                checks["retry_requested"] = "Retry requested." in outcome
            press(b"q", 0.5)
        finally:
            try:
                os.kill(pid, 9)
            except ProcessLookupError:
                pass
        receipt = {"job_phase": phase, "checks": checks, "passed": all(checks.values())}
        print(json.dumps(receipt, indent=2))
        return 0 if receipt["passed"] else 1
    finally:
        server.terminate()
        server.wait(timeout=15)


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    sys.exit(main(sys.argv[1], sys.argv[2]))
