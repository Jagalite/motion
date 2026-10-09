# Topcoat research and Motion adoption decision

**Research date:** October 9, 2026  
**Motion design:** 1.1.0  
**Scope:** Documentation and selected source inspection; no framework build, deployed security test or playback benchmark.  
**Topcoat source:** `tokio-rs/topcoat` at `341f3ff2fe16a73af5469685cf597693af5acb25`, retrieved as the repository head; commit dated October 8, 2026. [T0]

## Decision

Adopt **Topcoat for Rust-authored web and desktop-facing screens**. Keep Motion's Rust/Axum application server and public API, the Motion-owned Catabolic-inspired catalog and SQLite schema, Go terminal clients, the Demuxe browser player, and the previously selected Electron native host. Replace the React/Vite application prescription, not all browser JavaScript or the desktop execution environment.

The first implementation mode is **Topcoat SSR with a small prebuilt external JavaScript/TypeScript bridge**. The inspected optional reactive runtime has a material CSP compatibility constraint; it is not automatically approved with the frontend choice. This decision preserves the requested framework while preventing an unreviewed security regression.

The updated [main plan](../Motion_Final_Architecture_and_Implementation_Plan.md), [presentation contract](../contracts/TOPCOAT_PRESENTATION_CONTRACT.md) and [acceptance matrix](../qualification/TOPCOAT_ACCEPTANCE.md) turn the findings below into owned implementation work.

## 1. What Topcoat is—and what it does not replace

Topcoat renders HTML from asynchronous Rust views/components. Its optional runtime represents a supported subset of Rust expressions in JavaScript for immediate client updates; shards request new HTML from the server. This is not a full Rust application compiled to browser Wasm. [T1] [T5]

The inspected project does not provide Motion's native window, tray, installer, signing, OS credential store or update product. A browser or embedded browser still displays the HTML. **Topcoat and Electron occupy different layers.** Retaining Electron is continuity with the current plan, not a new requirement imposed by Topcoat. Another host would require its own platform/media qualification. [T1] [E1]

Topcoat's published examples allow components to query a database directly, but that is an application convention, not a requirement. For Motion, direct SQL in views would violate the chosen ownership model. Use the existing authorized application read services. A Rust UI framework does not justify migrating to Toasty, changing SQLite ownership, or creating a second authentication store. [T1] [T4]

## 2. Findings and their exact design consequences

| Finding from inspected source | Implication for Motion | Classification |
|---|---|---|
| Workspace declares `0.10.0`, edition 2024 and `rust-version = "1.98"`. [T2] | Pin a compatible compiler and matching Topcoat crate family/CLI/bundle; review the earlier Rust 1.95 packaging assumptions. | Source fact; Motion build untested. |
| README and runtime guide call the project/runtime experimental. [T1] [T5] | Contain framework types in the presentation adapter; pin upgrades and rerun affected tests. | Source fact; maintenance choice. |
| Tower integration supports a Topcoat router inside Axum via `TowerService`. [T3] | Preserve Axum API/media/auth routing and one listener. No reverse proxy or second application service is required. | Documented integration, not a compiled Motion proof. |
| Adapter documentation requires full-path forwarding and explicit transport peer metadata. [T3] | Do not strip the mount path or invent a trusted-proxy hop. Test generated links, route misses and origin handling. | Documented constraint. |
| SSR and client expressions are different layers. [T1] [T5] | Use Rust for screens; keep browser media and immediate controls in small JS/TS modules. | Documented model; Motion allocation. |
| Reactive expression compiler invokes `new Function`. [T7] | Unmodified runtime cannot satisfy the selected no-JavaScript-eval CSP. Ship SSR/external modules initially; gate optional runtime separately. | Direct source finding plus CSP inference. |
| Shard endpoints are callable without page/layout guards; restored signals are client input. [T5] [T6] | Authorize and validate each endpoint, including optional generated transports. No secrets in captured records. | Explicit upstream warning. |
| Navigation can prefetch on intent and execute a destination render before a visit. [T9] | Render functions must have no scan, delivery, job or viewing side effects; disable prefetch on sensitive/expensive pages. | Documented behavior. |
| Shard morphing matches elements by position/tag or ID; navigation preserves shared signals. [T6] [T9] | Neither is proof of retained Demuxe object/worker/audio identity. Keep a manually owned player subtree and test disposal. | Documented behavior; inference/qualification requirement. |
| Asset bundling is coupled to the built application and its asset references. [T1] [T10] | Deploy matching binary/UI bundle; keep Demuxe's internally consistent directory tree separate. | Documented asset behavior; Motion integration requirement. |
| Static export, islands, image optimization and OpenAPI endpoints appear in the README roadmap. [T1] | Do not rely on them for offline playback, DOM ownership, photo parity or public SDK generation. | Roadmap status at inspected commit. |
| Session/cookie support is documented despite broader auth work appearing on the roadmap. [T1] [T4] | Do not claim the framework has no session support. Reuse Motion's auth authority; helpers are not a finished account/policy system. | Capability distinction. |

Do not extrapolate platform support, memory use, rendering speed or audiovisual compatibility from these observations. No comparative React/Electron/Topcoat benchmark was performed.

## 3. Axum and API composition

The preferred composition is the existing Axum listener with reserved public API/media/SSE paths and a Topcoat `TowerService` for presentation. Upstream documents precisely this direction of embedding, as well as the reverse direction through `TowerRoute`. Motion does not need to rewrite its handlers into Topcoat procedures. [T3]

Implement the outer dispatch carefully. `/api/v2/unknown` should remain a JSON Problem Details response, not invoke a UI fallback. Binary/range/HLS/SSE responses must keep their headers, streaming bodies, cancellation and long-lived behavior; ordinary render timeouts/compression/buffering should not be applied globally. Keep authentic connection metadata and existing host/origin rules. The actual Axum, Tower, Topcoat and request-body types must compile together under the locked dependencies; source-level support is not a substitute for that check.

Topcoat SSR can avoid self-HTTP by calling `UiQueryFacade`, which delegates to the same authorized read use cases as the API. Browser mutations and Demuxe coordination continue to call public operations. This distinguishes **complete public application access** from an unrealistic requirement that a server-side HTML render must make network requests to itself.

The public API v2.0.0 retains 97 paths, 146 operations and 148 schemas. Only its non-wire design annotation changes. No Topcoat-generated procedure path becomes a stable API for Go, automation or future TV clients.

## 4. CSP finding and selected rollout

The inspected compiler constructs executable JavaScript from a source string using `new Function`. That is a direct code observation, not an inference from the framework's marketing description. [T7]

Under a CSP that restricts script execution without `unsafe-eval`, the Function constructor is blocked. Allowing WebAssembly compilation with `wasm-unsafe-eval` does not enable JavaScript string compilation, and an inline-script nonce does not change that distinction. Therefore a nonce-only fix is insufficient. This is a compatibility conclusion, not a claim that arbitrary attacker code is automatically exploitable in Topcoat. [CSP]

**Selected baseline:** server-rendered Topcoat HTML, external prebuilt event/command modules, and Demuxe qualified under the actual policy. Do not load the reactive runtime script by default. Review the facade's default features explicitly rather than assuming `topcoat` defaults satisfy this policy. [T4]

**Enhancement track:** enable reactive expressions/shards/navigation only after either a pinned no-eval implementation is integrated and qualified, or a separate maintainer-approved security ADR explicitly accepts a scoped exception. This revision does not approve that exception or assert a no-eval upstream alternative already exists. Never disable Electron sandboxing or `webSecurity` to make an example run.

This is a controlled subset of Topcoat, not a recommendation to abandon it. HTML templates, async components, routing, assets and shared Rust view models remain useful without the experimental runtime.

## 5. Demuxe, updates and player continuity

A Topcoat view emits the player host; the existing-style TypeScript coordinator owns Demuxe, source opening, time mapping, tracks, candidates, authority and teardown. HTML replacements cannot safely be presumed to preserve an externally managed video/canvas/WebAssembly/AudioWorklet graph. [T6] [T9]

Start with a playback page whose player subtree is outside mutable fragments. Before a deliberate leave, persist or queue the final event, close the delivery and await bounded destruction. For an in-place source replacement use the existing generation protocol. Store renderer/player/server/profile epochs and ignore late completions. Test a search/list refresh while playback is active, a real page leave, back/forward, logout, server switch and repeated open/close.

A persistent music mini-player requires a later verified persistent boundary or separate media document. Do not use a shared signal as proof of continuous sound. No server render is triggered per video frame or media clock update.

Topcoat's UI bundler and Demuxe's provider deployment remain separate inventories in one package. UI CSS and bridge entries may use content-hashed URLs; Demuxe's internal relative imports/worker paths and verification manifests must remain intact. Merely adding every Wasm file to `asset!` can change URLs without updating those relationships. The exact adapter, archive, asset tree, CSP and browser must be qualified together; this research did not do so.

## 6. Desktop, remote origins and offline operation

For a local server, Electron displays Topcoat HTML from the verified same loopback origin as API/media. The existing private bootstrap and cookie/CSRF exchange remain necessary. For a remote server, render that server's HTTPS UI in an isolated, Node-disabled content view with no privileged preload; keep native connection settings and credentials in trusted local chrome. Remote HTML is still remote code even when generated by Rust. [E1]

A failed remote render must not silently create a local Motion library. It should show a local connection error and offer explicit offline/download mode. The native credential facility may establish a scoped browser session through the reviewed auth exchange, but cannot put reusable administrator secrets in URLs, HTML or captured state.

Topcoat's ordinary SSR requires a rendering host; static export is not established in the inspected project. To retain the existing offline requirement, the plan adds an M5 **presentation-only local Rust host** that shares Topcoat download/player views and reads verified device-cache manifests. It has no access to Motion's authoritative database or source roots, no provider/scanner/encoder, and no public-API impersonation. Cold-start it with remote networking disabled. Browser offline is separately gated. [T1]

## 7. Benefits, costs and remaining uncertainty

The expected benefit is less full-SPA application code and shared Rust view/query types alongside the existing Rust services. API, native-host, media and persistent-state ownership remain clear. A full Catabolic rewrite or Python runtime is not reintroduced.

The costs are server-rendered navigation dependency, an experimental framework/toolchain upgrade, UI/API authorization consistency, a known reactive-runtime CSP constraint, and explicit offline presentation work. Browser JavaScript and desktop packaging do not disappear. SSR can add server work; neither lower total resource consumption nor better playback follows from the language choice. Those are measurement questions.

This revision is therefore an implementable direction with explicit qualification gates—not a claim that Topcoat is a drop-in native desktop framework, that all its advertised features work in Motion, or that the new app has been built.

## 8. Research scope and sources

The source inspection included README, workspace/facade manifests, framework/runtime guides, router/Tower adapters, shard/link semantics, and browser expression compilation. It is not a full security audit or exhaustive code review. No Rust compiler or GUI media environment was available for an implementation test in this task. Source downloads/builds are not represented as successful; reproducible static package validation is provided separately.

All repository references below are fixed to the inspected commit. Official browser/desktop documentation was consulted October 9, 2026. The design remains valid as a baseline only with deliberately pinned and retested dependencies.

[T0]: https://github.com/tokio-rs/topcoat/commit/341f3ff2fe16a73af5469685cf597693af5acb25
[T1]: https://github.com/tokio-rs/topcoat/blob/341f3ff2fe16a73af5469685cf597693af5acb25/README.md
[T2]: https://github.com/tokio-rs/topcoat/blob/341f3ff2fe16a73af5469685cf597693af5acb25/Cargo.toml
[T3]: https://github.com/tokio-rs/topcoat/blob/341f3ff2fe16a73af5469685cf597693af5acb25/crates/topcoat-router/docs/tower.md
[T4]: https://github.com/tokio-rs/topcoat/blob/341f3ff2fe16a73af5469685cf597693af5acb25/crates/topcoat/Cargo.toml
[T5]: https://github.com/tokio-rs/topcoat/blob/341f3ff2fe16a73af5469685cf597693af5acb25/docs/runtime.md
[T6]: https://github.com/tokio-rs/topcoat/blob/341f3ff2fe16a73af5469685cf597693af5acb25/docs/runtime/shard.md
[T7]: https://github.com/tokio-rs/topcoat/blob/341f3ff2fe16a73af5469685cf597693af5acb25/crates/topcoat-runtime/browser/src/expression/compile.ts
[T9]: https://github.com/tokio-rs/topcoat/blob/341f3ff2fe16a73af5469685cf597693af5acb25/docs/runtime/link.md
[T10]: https://github.com/tokio-rs/topcoat/blob/341f3ff2fe16a73af5469685cf597693af5acb25/llms.txt
[CSP]: https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/Content-Security-Policy/script-src
[E1]: https://www.electronjs.org/docs/latest/tutorial/security
