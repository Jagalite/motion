#!/usr/bin/env python3
"""First-run owner walkthrough against a real server, TUI and browser.

Starts an isolated server (fresh data directory, loopback), then:
  1. checks that loopback requests without the operator token get nothing;
  2. pairs headless Chromium from /signin and reads its code;
  3. types into the real `motion tui` (operator token) in a pseudo-terminal:
     s → review → y creates the Owner; a preselects it; the administration
     permission is toggled deliberately; the review shows the code and
     "Administration: GRANTED"; y approves;
  4. verifies the browser signs in with the Owner's administrator access;
  5. retries setup (sequential and concurrent) and restarts the server,
     checking the same owner and the browser session survive;
  6. adds a second user and checks that approval then requires an explicit
     choice in both the CLI and the TUI.

Usage: walkthrough.py SERVER_BINARY MOTION_BINARY OUT_DIR
Needs NODE_PATH with playwright for browser.cjs. Prints a JSON receipt.
"""
import concurrent.futures
import fcntl
import json
import os
import pty
import re
import select
import socket
import struct
import subprocess
import sys
import tempfile
import termios
import time
import urllib.error
import urllib.request

ANSI = re.compile(r"\x1b\[[0-9;?<>=]*[ -/]*[@-~]|\x1b\][^\x07]*\x07|\x1b.")
HERE = os.path.dirname(os.path.abspath(__file__))


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class Server:
    def __init__(self, binary, data, port):
        self.binary, self.data, self.port = binary, data, port
        self.proc = None

    def start(self):
        self.proc = subprocess.Popen([self.binary, "--listen", f"127.0.0.1:{self.port}", "--data-dir", self.data,
                                      "--access-mode", "restricted"], stdout=subprocess.PIPE, text=True)
        for line in self.proc.stdout:
            if line.startswith("Motion is running at "):
                return line.split("at ", 1)[1].strip()
        raise RuntimeError("server did not start")

    def stop(self):
        self.proc.terminate()
        self.proc.wait(timeout=20)


def request(url, method="GET", body=None, token=None):
    headers = {"Content-Type": "application/json", "Origin": url.split("/api/")[0]}
    if token:
        headers["Authorization"] = "Bearer " + token
    data = None if body is None else json.dumps(body).encode()
    try:
        with urllib.request.urlopen(urllib.request.Request(url, data=data, method=method, headers=headers)) as r:
            return r.status, json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"null")


class TUI:
    def __init__(self, motion, url, conf, token):
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            os.environ.update(TERM="xterm-256color", NO_COLOR="1")
            os.execv(motion, [motion, "--server", url, "--config-dir", conf, "--operator-token-file", token, "tui"])
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", 45, 150, 0, 0))
        self.buf = b""
        self.all = b""
        self.read(2.5)

    def read(self, seconds):
        end = time.time() + seconds
        while time.time() < end:
            ready, _, _ = select.select([self.fd], [], [], 0.05)
            if ready:
                try:
                    chunk = os.read(self.fd, 65536)
                    self.buf += chunk
                    self.all += chunk
                except OSError:
                    return

    def seen(self, marker):
        """Whether marker was ever drawn; the renderer redraws only changes."""
        return marker in re.sub(r"\s+", " ", ANSI.sub("", self.all.decode("utf-8", "replace")))

    def press(self, keys, seconds=0.7):
        self.buf = b""
        os.write(self.fd, keys)
        self.read(seconds)
        return ANSI.sub("", self.buf.decode("utf-8", "replace"))

    def page(self, marker):
        for key, steps in ((b"\x1b[A", 30), (b"\x1b[B", 60)):
            for _ in range(steps):
                if marker in self.press(key, 0.4):
                    return True
        return False

    def close(self):
        try:
            os.kill(self.pid, 9)
        except ProcessLookupError:
            pass


def wait_file(path, seconds=120):
    for _ in range(int(seconds * 4)):
        if os.path.exists(path):
            return True
        time.sleep(0.25)
    return False


def tui_approve(tui, out, n):
    """Approve the browser's n-th action request on Action approvals."""
    if not wait_file(os.path.join(out, f"approval-{n}.txt")):
        return False
    code = open(os.path.join(out, f"approval-{n}.txt")).read().strip()
    if not tui.page("Requested by"):
        return False
    tui.press(b"\t", 0.8)
    tui.press(b"r", 1.5)
    tui.all = b""
    tui.press(b"a", 1.2)
    if not tui.seen("Code shown on the requesting device"):
        return False
    tui.press(code.encode(), 0.6)
    tui.press(b"\x13", 0.8)
    ok = tui.seen("it must match the requesting device")
    tui.all = b""
    tui.press(b"y", 2.0)
    ok = ok and tui.seen("Request approved")
    open(os.path.join(out, f"approved-{n}"), "w").close()
    tui.press(b"\t", 0.6)  # back to the sidebar for the next navigation
    return ok


def main(server_bin, motion, out):
    os.makedirs(out, exist_ok=True)
    data, conf = tempfile.mkdtemp(prefix="motion-owner-data-"), tempfile.mkdtemp(prefix="motion-owner-conf-")
    server = Server(server_bin, data, free_port())
    url = server.start()
    token_path = os.path.join(data, "admin-token")
    token = open(token_path).read().strip()
    base = [motion, "--server", url, "--config-dir", conf, "--operator-token-file", token_path]
    checks, notes = {}, {}

    def cli(*args, json_out=True):
        p = subprocess.run(base + (["--json"] if json_out else []) + list(args), capture_output=True, text=True)
        if json_out and p.returncode == 0:
            return p.returncode, json.loads(p.stdout)["data"], p.stderr
        return p.returncode, p.stdout, p.stderr

    browser = None
    tui = None
    try:
        # 1. Loopback without the operator token is not authority.
        status, _ = request(url + "/api/v2/access/setup", "POST", {})
        checks["loopback_without_token_refused"] = status == 401
        status, body = request(url + "/api/v2/access/setup", token=token)
        checks["fresh_server_requires_setup"] = status == 200 and body["owner_setup"] == "required"

        # 2. The browser starts pairing and shows its code.
        browser = subprocess.Popen(["node", os.path.join(HERE, "browser.cjs"), "pair", url, out])
        code_file = os.path.join(out, "code.txt")
        for _ in range(120):
            if os.path.exists(code_file):
                break
            time.sleep(0.25)
        code = open(code_file).read().strip()
        notes["browser_code"] = code

        # 3. The operator's TUI creates the Owner and approves deliberately.
        tui = TUI(motion, url, conf, token_path)
        checks["tui_pairings_page"] = tui.page("Compare code")
        tui.press(b"\t", 1.0)
        checks["tui_first_run_hint"] = tui.seen("First run")
        checks["tui_setup_dialog"] = "Set up Motion's owner" in tui.press(b"s", 1.0)
        review = tui.press(b"\x13", 1.0)
        checks["tui_setup_review"] = "Devices: none approved or assigned" in review
        done = tui.press(b"y", 2.0)
        checks["tui_owner_ready"] = "is ready" in done
        _, setup, _ = cli("access", "setup")
        owner = setup["owner"]
        checks["single_owner_created"] = owner["name"] == "Owner" and "system:admin" in owner["permissions"]
        tui.press(b"r", 1.5)
        approve = tui.press(b"a", 1.2)
        checks["tui_owner_preselected"] = "Approve access for" in approve and "Preselected: the only enabled user" in approve
        tui.press(b"\x1b[A", 0.6)  # wraps to the last field: Permission: system:admin
        toggled = tui.press(b" ", 0.6)
        notes["admin_toggle_screen_has_check"] = "[✓] Permission: system:admin" in toggled
        review = tui.press(b"\x13", 1.0)
        checks["tui_review_compare_code"] = ("Compare code: " + code) in review
        checks["tui_review_states_admin"] = "Administration: GRANTED" in review
        checks["tui_approved"] = "Access updated." in tui.press(b"y", 2.5)

        # 4. The browser signs in with the Owner's administrator access.
        browser.wait(timeout=120)
        result = json.load(open(os.path.join(out, "browser-pair.json")))
        me = result.get("me") or {}
        notes["browser_pair"] = {k: result.get(k) for k in ("before", "settings_access", "owner_setup_offered", "signin_first_run_hint", "errors", "failure")}
        checks["signin_first_run_hint"] = bool(result.get("signin_first_run_hint"))
        checks["browser_was_unauthenticated"] = result.get("before") == 401
        checks["browser_signed_in_as_owner_admin"] = "system:admin" in json.dumps(me) and owner["id"] in json.dumps(me)
        checks["browser_admin_page"] = bool(result.get("settings_access")) and result.get("owner_setup_offered") == 0
        notes["me"] = me

        # 5. Retries, concurrency and restart keep the same owner.
        rc, again, _ = cli("access", "setup", "--name", "Someone else")
        checks["retry_returns_same_owner"] = rc == 0 and again["owner"]["id"] == owner["id"] and again["owner"]["name"] == "Owner"
        with concurrent.futures.ThreadPoolExecutor(8) as pool:
            replies = list(pool.map(lambda _: request(url + "/api/v2/access/setup", "POST", {}, token), range(8)))
        checks["concurrent_retries_same_owner"] = all(s == 200 and b["owner"]["id"] == owner["id"] for s, b in replies)
        rc, users, _ = cli("access", "users", "list")
        checks["exactly_one_user"] = rc == 0 and len(users["items"]) == 1
        tui.close()
        tui = None
        server.stop()
        url2 = server.start()
        checks["restart_same_origin"] = url2 == url
        rc, after, _ = cli("access", "setup")
        checks["restart_same_owner"] = rc == 0 and after["owner"]["id"] == owner["id"]
        rc = subprocess.run(["node", os.path.join(HERE, "browser.cjs"), "check", url, out]).returncode
        check = json.load(open(os.path.join(out, "browser-check.json")))
        checks["browser_session_survives_restart"] = rc == 0 and check.get("status") == 200 and check.get("settings_access")
        notes["browser_check"] = {k: check.get(k) for k in ("status", "settings_access", "errors", "failure")}

        # 6. The paired-only Owner browser makes an admin change (creates Sam);
        #    it lacks recent sign-in, so the operator approves it in the TUI.
        tui = TUI(motion, url, conf, token_path)
        change = subprocess.Popen(["node", os.path.join(HERE, "browser.cjs"), "change", url, out])
        checks["tui_approves_browser_change"] = tui_approve(tui, out, 1)
        change.wait(timeout=120)
        changed = json.load(open(os.path.join(out, "browser-change.json")))
        notes["browser_change"] = {k: changed.get(k) for k in ("created", "errors", "failure")}
        checks["browser_admin_change_applied"] = bool(changed.get("created"))
        tui.close()
        tui = None
        rc, users, _ = cli("access", "users", "list")
        sam = next((u for u in users["items"] if u["name"] == "Sam"), {"id": None})

        # 7. Several users: approval needs an explicit choice.
        status, setup = request(url + "/api/v2/access/setup", token=token)
        checks["two_users_no_suggestion"] = setup["pairing_user"] is None and setup["eligible_users"] == 2 and setup["owner"]["id"] == owner["id"]
        status, pairing = request(url + "/api/v2/auth/pairings", "POST", {"device_name": "Den TV", "client_name": "walkthrough"})
        rc, stdout, stderr = cli("auth", "approve", pairing["id"], "--code", pairing["user_code"], json_out=False)
        checks["cli_requires_user_with_two"] = rc != 0 and "access user" in stderr
        tui = TUI(motion, url, conf, token_path)
        tui.page("Compare code")
        tui.press(b"\t", 1.0)
        # Rows are ordered by ID; the claimed browser request may come first.
        for _ in range(3):
            tui.all = b""
            tui.press(b"a", 1.5)
            if not tui.seen("Select a pending request"):
                break
            tui.press(b"\x1b[B", 0.6)
        checks["tui_chooser_with_two"] = all(tui.seen(m) for m in ("Choose the intended user", "Owner ·", "Sam ·"))
        tui.press(b"\x1b", 0.5)
        rc, device, _ = cli("auth", "approve", pairing["id"], "--code", pairing["user_code"], "--user", sam["id"])
        checks["explicit_user_approval"] = rc == 0 and device["user_id"] == sam["id"] and "system:admin" not in device["permissions"]
    finally:
        if tui:
            tui.close()
        if browser and browser.poll() is None:
            browser.kill()
        try:
            server.stop()
        except Exception:
            pass
    desktop_checks, desktop_notes = desktop(server_bin, motion, os.path.join(out, "desktop"))
    checks.update(desktop_checks)
    notes["desktop"] = desktop_notes
    crash_checks, crash_notes = crash(server_bin)
    checks.update(crash_checks)
    notes["crash"] = crash_notes
    audits = []
    for name in ("browser-pair.json", "desktop/browser-desktop.json"):
        try:
            audits += json.load(open(os.path.join(out, name))).get("audits", [])
        except FileNotFoundError:
            pass
    notes["audits"] = audits
    checks["axe_wcag21aa_no_violations"] = len(audits) >= 3 and all(not a["violations"] and not a["overflow"] for a in audits)
    receipt = {"checks": checks, "notes": notes, "passed": all(checks.values())}
    with open(os.path.join(out, "receipt.json"), "w") as f:
        json.dump(receipt, f, indent=2)
    print(json.dumps(receipt, indent=2))
    return 0 if receipt["passed"] else 1



def desktop(server_bin, motion, out):
    """First run from an administrator browser that is not freshly signed in.

    The desktop app's session is the unassigned, all-permission desktop-local
    device without a sign-in family. An unassigned administrator device
    approved without a user (as older clients could) has the same authority,
    so it stands in for it here; the desktop shell itself is not driven.
    """
    os.makedirs(out, exist_ok=True)
    checks, notes = {}, {}
    data, conf = tempfile.mkdtemp(prefix="motion-owner-desk-"), tempfile.mkdtemp(prefix="motion-owner-desk-conf-")
    server = Server(server_bin, data, free_port())
    url = server.start()
    token_path = os.path.join(data, "admin-token")
    token = open(token_path).read().strip()
    tui = browser = None
    try:
        _, pairing = request(url + "/api/v2/auth/pairings", "POST", {"device_name": "Desktop stand-in", "client_name": "walkthrough"})
        everything = ["catalog:read", "catalog:write", "sources:manage", "profiles:manage", "viewing:write", "playback:request",
                      "processing:request", "downloads:manage", "collections:write", "events:read", "system:admin"]
        req = urllib.request.Request(url + f"/api/v2/auth/pairings/{pairing['id']}/approve", method="POST",
                                     data=json.dumps({"user_code": pairing["user_code"], "profile_ids": [], "permissions": everything}).encode(),
                                     headers={"Content-Type": "application/json", "Authorization": "Bearer " + token, "Idempotency-Key": "desktop-standin-approval"})
        urllib.request.urlopen(req).read()
        _, claimed = request(url + f"/api/v2/auth/pairings/{pairing['id']}/claim", "POST", {"device_code": pairing["device_code"]})
        browser = subprocess.Popen(["node", os.path.join(HERE, "browser.cjs"), "desktop", url, out, claimed["access_token"]])
        tui = TUI(motion, url, conf, token_path)
        checks["desktop_tui_approves_owner_setup"] = tui_approve(tui, out, 1)
        for _ in range(80):
            _, setup = request(url + "/api/v2/access/setup", token=token)
            if setup["owner_setup"] == "complete":
                break
            time.sleep(0.25)
        checks["desktop_owner_created_from_browser"] = setup["owner_setup"] == "complete" and setup["owner"]["name"] == "Owner"
        _, second = request(url + "/api/v2/auth/pairings", "POST", {"device_name": "Living room browser", "client_name": "walkthrough"})
        with open(os.path.join(out, "second-pairing.json"), "w") as f:
            json.dump({"id": second["id"], "user_code": second["user_code"]}, f)
        checks["desktop_tui_approves_pairing_approval"] = tui_approve(tui, out, 2)
        browser.wait(timeout=180)
        result = json.load(open(os.path.join(out, "browser-desktop.json")))
        notes["browser"] = {k: result.get(k) for k in ("session", "owner_setup_offered", "preselected", "admin_refused_without_confirmation", "second_status", "errors", "failure")}
        checks["desktop_setup_offered"] = result.get("owner_setup_offered") == 1
        checks["desktop_owner_preselected"] = result.get("preselected") == setup["owner"]["id"]
        checks["desktop_admin_needs_confirmation"] = bool(result.get("admin_refused_without_confirmation"))
        checks["desktop_pairing_approved"] = result.get("second_status") == "approved"
        _, device = request(url + f"/api/v2/auth/pairings/{second['id']}/claim", "POST", {"device_code": second["device_code"]})
        _, me = request(url + "/api/v2/me", token=device.get("access_token"))
        checks["desktop_second_device_owner_admin"] = me.get("user_id") == setup["owner"]["id"] and "system:admin" in me.get("permissions", [])
    finally:
        if tui:
            tui.close()
        if browser and browser.poll() is None:
            browser.kill()
        server.stop()
    return checks, notes


def crash(server_bin, rounds=24):
    """SIGKILL the server at random points around setup, then restart.

    Each round must end in exactly one of two durable states: no user and
    setup required, or one user recorded as the complete Owner. A retry then
    converges on exactly one Owner.
    """
    import random
    import signal
    import threading
    rng = random.Random(1015)
    outcomes = {"before_commit": 0, "after_commit": 0}
    consistent = converges = True
    for i in range(rounds):
        data = tempfile.mkdtemp(prefix="motion-owner-crash-")
        server = Server(server_bin, data, free_port())
        url = server.start()
        token = open(os.path.join(data, "admin-token")).read().strip()
        def attempt():
            try:
                request(url + "/api/v2/access/setup", "POST", {}, token)
            except OSError:
                pass  # the server was killed mid-request

        threading.Thread(target=attempt, daemon=True).start()
        time.sleep(rng.uniform(0, 0.03))
        server.proc.send_signal(signal.SIGKILL)
        server.proc.wait()
        server.start()
        _, state = request(url + "/api/v2/access/setup", token=token)
        users = state["eligible_users"]
        if state["owner_setup"] == "required" and users == 0 and state["owner"] is None:
            outcomes["before_commit"] += 1
        elif state["owner_setup"] == "complete" and users == 1 and state["owner"] is not None:
            outcomes["after_commit"] += 1
        else:
            consistent = False
            outcomes.setdefault("inconsistent", []).append(state)
        status, after = request(url + "/api/v2/access/setup", "POST", {}, token)
        converges &= status in (200, 201) and after["eligible_users"] == 1 and after["owner"] is not None
        server.stop()
    return {"crash_leaves_no_partial_owner": consistent, "crash_retry_converges": converges}, outcomes


if __name__ == "__main__":
    if len(sys.argv) != 4:
        sys.exit(__doc__)
    sys.exit(main(*sys.argv[1:]))
