# First-run owner walkthrough

`walkthrough.py` runs the single-owner first run against a real server binary,
the real `motion tui` in a pseudo-terminal, and headless Chromium
(`browser.cjs`, Playwright with axe-core). Every scenario uses a fresh
temporary data directory on a loopback port and that server's own operator
token file.

```sh
cargo build --bin playscale
(cd apps/terminal && go build -o /tmp/motion ./cmd/motion)
NODE_PATH=/path/with/playwright-and-axe python3 qualification/owner-setup/walkthrough.py \
  target/debug/playscale /tmp/motion /tmp/owner-walk
```

Scenarios and checks:

1. **TUI first run.** Loopback requests without the token are refused; the
   browser shows a pairing code; the TUI offers first run, creates the Owner
   (`s`, review, `y`), preselects it on `a`, shows the compare code and states
   that administration is granted after it is toggled deliberately; the
   browser signs in with the Owner's administrator grant. Repeated, 8-way
   concurrent and post-restart setup return the same Owner; the browser
   session survives a restart.
2. **Admin change from a paired-only browser.** The Owner browser creates a
   user; lacking recent sign-in it requests approval, the TUI approves its
   code under Action approvals, and the browser's change is applied. With two
   users the CLI refuses an approval without `--user` and the TUI shows the
   chooser.
3. **First run from an administrator browser** (stand-in for the desktop app's
   unassigned, never-fresh `desktop-local` session: an unassigned all-permission
   device). The browser creates the Owner and approves a second device for the
   preselected Owner with deliberate administration (refused until the
   confirmation is checked); each step is approved in the TUI. The second
   device then holds the Owner's administrator grant.
4. **Crash consistency.** 24 rounds: SIGKILL the server at a random point
   (0–30 ms) after a setup request, restart, and require either no user with
   setup required or exactly one complete Owner; a retry converges on one Owner.
5. **Accessibility.** axe (WCAG 2.1 AA tags) on the sign-in page, the first-run
   Users panel and the preselected pairing approval card, plus a horizontal
   overflow check, at 1280 px.

`receipt.json` is the receipt recorded for this change: one macOS run,
Chromium only. It does not drive the Electron desktop shell itself (its local
server listens on an undisclosed ephemeral port), other browsers, touch or
screen-reader hardware.
