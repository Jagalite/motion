# Combined playback/viewing acceptance

Continuation of PR #16, stacked on combined PR #18. This receipt supersedes
input-branch qualification for the combined code. A pending/cancelled build is
not a pass, and route inventory does not establish product qualification.

## Composition and supported policy

The continuation starts at `a518832`, preserving the scoped, transactional
`organization::facts_in` query and both catalog/playback watched regressions.
It incorporates catalog `ee842e4` (public preparations, schedules, backup durability, schema-41
upgrade and fixtures) and clients `f883f19` (v2 candidate callers, provider ownership
and bounded public live-position seeking). Generation-ticket/subtitle implementation is
`c72f5e5`; conservative subtitle/bitrate policy is `a672d6c`; affirmative HDR
capability policy is `1928c2c` with boundary coverage `0621762`. Admission now
establishes subtitle-cache ownership in `72d5243`; the real byte-route extraction
test exposed the missing directory, and extraction cannot recreate it after cleanup.

- File-revision and delivery-generation playback tickets use current credential,
  library/profile grants, revision and ownership observations. Full token hashing
  binds the generation scope. HLS child links preserve the ticket. Commands,
  lease renewal, another generation and the file route are outside its authority.
- Embedded components resolve only on the exact selected version/file/revision.
  Plain-text embedded subtitles on original delivery are extracted as WebVTT;
  admission, removal, re-addition and activation preserve the generation's pin.
  The shared player attaches them after its public generation open/seek step.
- Declared unsupported: automatic forced/foreign-audio selection when embedded
  captions exist; styled/bitmap/sidecar captions; caption delivery with conversion;
  explicit prepared-rendition component/track pins; multipart and interval routes;
  HDR conversion/tone mapping; bitrate caps; adaptive bitrate; cast receiver grants.
  `require` without a pin selects the first embedded subtitle and refuses it if
  unsupported, even if a later supported track exists. It never substitutes.
- Explicit height bounds need known facts. Average bitrate does not establish a
  peak bound, so no byte or CRF conversion route promises a bitrate cap. HDR bytes
  require affirmative client support; unknown/unsupported and `require_sdr` refuse
  them. An allowed attempt is not a display-fidelity qualification.

Core owns selection/refusal, component identity, quality admission, authority,
ordering, leases and watched aggregation. Adapters observe SQLite grants and
catalog facts, preserve transaction boundaries, extract subtitle bytes and stop
workers. The server still reports the broad playback feature as unqualified;
these bounded results do not qualify every platform or installed product.

## Independent review

GPT-6.1 Sol reviewed the combined merges and repairs. Findings addressed:
subtitle pins dropped by initial/replanned byte delivery; prepared pins silently
substituted; unknown budget facts accepted; average bitrate mistaken for a cap;
automatic subtitle preferences silently dropped; unknown HDR capability accepted.
No further findings remained after those repairs or the later subtitle-cache,
restricted-pairing startup and Go cancellation repairs. Review is separate from testing.

## Model evidence

Explicit Rust 1.98.0 via `RUSTC`, Kyoto build
`586a93e7c4604aa3a57075aaf0eec447`; executable hashes and source identity are in
`combined-receipts.json` when final acceptance is recorded.

- Core unit tests: 98 passed.
- Ordered heartbeat observations: 313 states / 2,867 transitions, graph exhausted.
  Four events, two generations with activation, three sequences and positions,
  lease expiry, close/restart; observation/order/lease and rejection effects checked.
  Watched aggregation and revoked/missing/narrowed-owner policies also pass.
- Playback admission: 253,696 states / 3,890,752 transitions, graph exhausted.
  Two principals and timelines, revocation, profile/library removal, file move,
  source changes and expiry.
- Timeline viewing: 1,890,681 states / 52,877,296 transitions, graph exhausted,
  zero skipped checks. Two sessions over two deliveries, known/unknown duration,
  two events each, one manual override and rebind, exact retries, reused identities,
  repeated/skipped sequences and boundary positions. Runtime: 404.94 seconds.

These models execute production core decisions. They do not prove SQL, filesystem,
FFmpeg, browser, native, or crash-acknowledgement conformance by themselves.

## Adapter and caller evidence

Explicit Rust 1.98.0 Kyoto build `cf95e4c904f943609687067004ff2ad5` on
`72d5243`: playback 11/11, scoped organization/viewing 8/8, access 32/32 and
contract 4/4 passed. These include real extracted WebVTT, generation ticket
GET/HEAD and HLS child scope, prepared-track refusal, unknown HDR/bitrate refusal,
restart, ordered history, revoked authority and playback-only pacing. Live worker
and output release after revocation took 1.883 seconds in the focused test.
Catalog metadata/processing passes 17/17 after the semantic-refusal fixture
correction `b65524c`, built through Kyoto `629ec17517d04506bfca791fc9e8a550`.
These five router targets total 72 passing tests.

The same server passed 11 public-TCP playback/preparation checks, viewing smoke,
restore (old observed epoch rejected without mutation; fresh identity accepted),
Go root integration/capability negotiation, and schema-41 candidate backup/restore
plus baseline rollback. The baseline identity and resulting schema/epoch checks
are retained in the upgrade receipt. Go API tests separately cover cancellation
winning over a completed error response without retrying or losing a successful
acknowledgment.

JavaScript controller/bridge/host/setup tests pass 79/79; contract structure and
JSON parity pass (99 paths, 150 operations, 148 schemas, 2,625 references).
The browser rerun uses the rebuilt embedded live-window intersection fix from
client commit `38db68c` (composed as `9264fec`) and the library-admission setup
repair `45d818b`. Kyoto `69674c954bee4338bf07d1a20386c714` rebuilt the embedded
assets; the server binary is SHA-256
`c438491e1ff31942190aefd9afacba5fc906e20f0b744c7c4ab9f00e20b31e0d`.
The harness records its own source separately from that binary. Later harness
repairs bind restart resume to the admission request and actual playhead, using
history observations before navigation and after readiness instead of treating
a later read as the render-time snapshot.

**C07 is not qualified.** Repeated Chromium/WebKit runs on the same deployment
intermittently reject conversion-only startup or audio switching because the
requested position is outside the public live window. Successful reruns do not
resolve that flakiness. A refused switch retains the original player; neither
server policy nor the one-second position bound was weakened. The exact engine
results and provider identity are in `combined-receipts.json`. The final run
records Chromium 30 passing / 1 failing product checks and WebKit 29 passing /
2 failing checks: live audio switching in both, plus conversion-only startup
in WebKit. Both corrected restart-position checks pass. Electron,
installed-package playback, physical A/V, HDR fidelity and positive Next remain
unqualified here. The clients lane received the concrete failure and receipts. A focused
[public-API diagnostic](live-window-diagnostic.json) captured an open resolving
with empty seek ranges and a seek to 0.346 seconds, inside both reported ranges,
resolving while the playhead remained at 4.002 seconds. It still had not reached
the target after a two-second settling observation. Four of six settling trials
passed and two failed. These diagnostics do not qualify playback or justify
widening the position bound; a simple settling wait was not a sufficient repair.

Fast CI at `b65524c` is green: [run 38113622580](https://github.com/Jagalite/motion/actions/runs/38113622580)
(fmt, clippy, fast Rust tests, contract, JavaScript and Go). Later changes are
browser evidence corrections and this handoff. Full macOS CI is a separate gate;
queued or cancelled runs are not passing evidence.

[Caller coordination and retirement gate](CALLERS.md) remains explicit. Current
legacy writer mounts cannot be removed while the legacy product entry is active.
Clients/integration owns default-entry cutover, installed product matrix, positive
public release-order provisioning and absence/no-mutation checks for retired
routes. Historical baseline writers and upgrade readers remain supported fixtures.
