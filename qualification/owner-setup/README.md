# First-run owner walkthrough

`walkthrough.py` runs the single-owner first run against a real server binary,
the real `motion tui` in a pseudo-terminal, and headless Chromium
(`browser.cjs`, Playwright). It uses a fresh temporary data directory on a
loopback port and the server's own operator token file.

```sh
cargo build --bin playscale
(cd apps/terminal && go build -o /tmp/motion ./cmd/motion)
NODE_PATH=/path/with/playwright python3 qualification/owner-setup/walkthrough.py \
  target/debug/playscale /tmp/motion /tmp/owner-walk
```

It checks, in order: loopback requests without the token are refused; the
browser shows a pairing code; the TUI offers first run, creates the Owner
(`s`, review, `y`), preselects it on `a`, shows the compare code and states
whether administration is granted after it is toggled deliberately; the
browser then signs in with the Owner's administrator grant and opens
Authentication & Access; repeated, concurrent and post-restart setup return
the same Owner; the browser session survives a server restart; with a second
user the CLI refuses an approval without `--user` and the TUI shows the user
chooser.

`receipt.json` is the receipt of the run recorded for this change. It is
evidence for one macOS run with Chromium only. It does not cover interrupted
setup in a real process (the Rust test `owner_setup_is_atomic_under_concurrency_interruption_and_restart`
covers rollback with an injected failure), the desktop app, or other browsers.
