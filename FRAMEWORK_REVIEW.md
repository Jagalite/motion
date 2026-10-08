# Axum, Poem, and Salvo for Playscale

Reviewed 2026-10-06. **Axum was selected following this review.** The recommended supporting stack is Tokio + Utoipa, with a dedicated media-delivery module. Salvo and Poem remain documented alternatives; the comparison below preserves the rationale and qualification limits.

Playscale needs a public API, a bundled Demuxe webpage, original-file streaming, background media processing, and private access through Tailscale. All three frameworks can support that architecture. The decision concerns composition, API contracts, operational behavior, and maintenance—not access to GPU transcoding.

## Evidence and version boundary

I inspected published crate source, current official documentation and repository activity, compiled equivalent route/OpenAPI examples, and made 90 loopback HTTP observations against file-serving helpers. The [probe sources, lockfile, schemas, and results](research/framework-review/README.md) are preserved. This is a focused architecture review, not a full security audit or performance benchmark.

| Component | Reviewed release | Release date | Declared minimum Rust |
| --- | --- | --- | --- |
| Axum | 0.8.9 | 2026-04-14 | 1.80 |
| Tower HTTP | 0.7.1 | 2026-08-31 | 1.65 |
| Utoipa-Axum | 0.3.0 | 2026-09-22 | 1.88 |
| Poem | 3.1.12 | 2025-07-28 | 1.85 |
| Poem OpenAPI | 5.1.16 | 2025-07-28 | 1.85 |
| Salvo / Salvo OAPI | 1.0.1 | 2026-10-04 | 1.94 |

These are individual package declarations, not promises about the minimum compiler for an entire resolved dependency graph. The probes built with installed Rust 1.95.0, without changing the default toolchain. Release metadata is preserved in [versions.json](research/framework-review/versions.json), obtained from the [crates.io API](https://crates.io/data-access).

## Architecture and ergonomics

| Concern | Axum | Poem | Salvo |
| --- | --- | --- | --- |
| Main abstraction | Handler/extractor plus Tower Service | Endpoint plus Middleware | Handler plus middleware “hoops” and FlowCtrl |
| Ordinary handler style | Typed arguments, returned response | Typed arguments, returned response | Mutable request/response/depot; typed endpoint macros also available |
| Application state | Typed State and FromRef | Data extraction or fields on API/endpoint structs | Depot extraction or fields on handler structs |
| OpenAPI route integration | Utoipa OpenApiRouter | OpenApiService and API impl blocks | Endpoint macro and merge_router |
| Middleware interoperability | Native Tower composition | Tower compatibility adapters | Tower compatibility adapters |
| Best fit here | Modular server with explicit policies | API-centric development | Integrated framework with broader built-ins |

### Axum

Axum's strongest architectural property is that routing and middleware fit directly into Tower's service model. Playscale can use typed handlers, explicit application state, and reusable middleware while keeping catalog, jobs, and media delivery in ordinary Rust modules. The framework does not have to become the application's domain model. [Axum documentation](https://docs.rs/axum/0.8.9/axum/)

Prefer `State<AppState>` and `FromRef` for required dependencies. This exposes missing state wiring through types; using request extensions everywhere gives up some of that benefit. The tradeoff is learning generic service/layer bounds and extractor ordering. Complex middleware errors can be harder to understand than a simple application handler.

Axum is less integrated: choose API generation, validation, TLS termination, and common errors explicitly. That costs initial design work but fits a server whose HTTP JSON operations and long-lived media responses need different policies.

A specific caveat is router readiness: Axum's routing does not automatically provide the backpressure semantics one might assume from a Tower stack. Backpressure-sensitive services need the documented wrapping/load-shedding arrangement. A generic request concurrency limit also must not be assumed to count a video stream until its body finishes. [Axum middleware and backpressure](https://docs.rs/axum/0.8.9/axum/middleware/index.html)

### Poem

Poem's main attraction is how naturally `poem-openapi` organizes a public API: methods live in an annotated API implementation, with typed payloads, parameters, response enums, and validators. This is appealing when every product function must be available to external clients. [Poem OpenAPI documentation](https://docs.rs/poem-openapi/5.1.16/poem_openapi/)

Poem uses its own Endpoint/Middleware interfaces but offers Tower compatibility; it is not isolated from that ecosystem. `Data<&T>` depends on correctly installed runtime data, while storing services directly on an API struct gives typed construction. Its abstraction is coherent, but moving away later means rewriting Poem-specific API objects, extraction, and validation. [Poem middleware](https://docs.rs/poem/3.1.12/poem/middleware/index.html)

There is a media-specific concern in the reviewed source: `StaticFileRequest::create_response` performs synchronous existence/type checks, file opening, metadata access, and seeking on the path called by the async endpoint. Body delivery then uses async I/O. Slow NAS metadata could therefore occupy an executor thread; this is a source-derived risk, not a measured NAS result. A custom media handler can avoid relying on that helper. [Published Poem file-serving source](https://docs.rs/crate/poem/3.1.12/source/src/web/static_file.rs)

### Salvo

Salvo combines routing, file responses, OpenAPI, testing utilities, server controls, and optional transport features in one framework family. The core style exposes mutable Request, Response, Depot, and FlowCtrl. It makes short-circuiting and response mutation direct, but ordering and mutation become important to review as middleware grows. Typed endpoint macros offer a higher-level API style. [Salvo core interfaces](https://docs.rs/salvo_core/1.0.1/salvo_core/)

Depot is a runtime container, so required dependencies retrieved from it can be absent at runtime. That does not force an untyped application: handler structs can own typed services. Keep the domain independent of Depot either way.

Salvo is the most interesting alternative if we want fewer separately chosen components. Its optional Quinn HTTP/3, TLS, and ACME integrations are real capabilities, but do not establish better playback over our proposed Tailscale Serve deployment. That requires end-to-end transport testing. Select only the needed Cargo features. [Salvo feature flags](https://docs.rs/salvo/1.0.1/salvo/)

## Public API and generated clients

All three compiled successfully with a documented `GET /api/v1/items` and the same example object. The generated documents and serializer output are preserved in [schemas](research/framework-review/schemas).

| Tested configuration | Generated OpenAPI | Optional number in this example |
| --- | --- | --- |
| Axum + Utoipa 6 + Utoipa-Axum 0.3 | 3.1.0 | Number or null, not required |
| Poem + Poem OpenAPI 5.1.16 | 3.0.0 | Number, not required, no nullable flag |
| Salvo OAPI 1.0.1 | 3.1.0 | Number or null, not required |

The Poem example serialized `duration_seconds: null`, including through Poem's own Object serializer, while its generated property did not advertise nullability. Optional presence and nullable values are different contract properties. Treat this as a concrete compatibility issue to resolve with explicit model configuration or a fix before relying on a generated client—not as proof that every Poem API has broken schemas. No client generator or complete OpenAPI validator was run.

Utoipa-Axum narrows Poem's apparent advantage: `OpenApiRouter` registers handlers and collects their OpenAPI definitions together. Axum does not require maintaining a separate route list and standalone spec by hand. Response annotations can still diverge from runtime behavior, so generated documentation is not contract enforcement. [Utoipa-Axum integration](https://docs.rs/utoipa-axum/0.3.0/utoipa_axum/)

For every candidate, explicitly define stable operation IDs and schema names, pagination, structured errors, validation failures, unknown-field behavior, and null-versus-absent semantics. Our Salvo example used a module-qualified component name; choose stable names so a Rust module refactor does not unnecessarily alter generated SDK types. Document media headers and binary responses separately from JSON resources.

Poem's integrated validators are useful. Utoipa schema constraints alone do not imply runtime validation; Axum needs deliberate validation in extractors/services. Salvo likewise needs an explicit policy for semantic validation. Database existence, allowed job transitions, and media availability belong in application services regardless of framework.

## Media delivery: observed behavior

Tests used Axum with Tower HTTP ServeFile, Poem StaticFileEndpoint, and Salvo NamedFile. These are helper/adaptor results, not universal framework limitations. Fixtures were `0123456789` and an empty file, served over loopback HTTP/1.1. Full results include response headers and body hashes.

| Request against ten-byte file | Axum / Tower HTTP | Poem | Salvo |
| --- | --- | --- | --- |
| GET, no Range | 200, ten bytes | Same | Same |
| HEAD, no Range | 200, no body | Same | Same |
| `bytes=0-3`, `bytes=-3`, `bytes=4-` | Correct partial bytes | Same | Same |
| `bytes=4-99` | 206, bytes 4–9 | 416 | 206, bytes 4–9 |
| `bytes=99-100` | 416 | 416 | 416 |
| Stale If-Range with `bytes=0-3` | 206, four bytes | Same | Same |
| HEAD with `bytes=0-3` | 206, no body | Same | Same |
| Matching If-None-Match | 304 | Same | Same |
| Matching If-None-Match plus unsatisfiable Range | 304 | Same | Same |
| Multiple ranges | 416 | 206, first range only | 200, complete representation |

For the empty file, Tower HTTP returned 206 and `Content-Range: bytes 0-0/0` for `bytes=0-3`; that describes a nonexistent byte. Poem and Salvo returned 416 for that request.

The key protocol implications are specific: a mismatching If-Range should cause Range to be ignored; a valid start with an overshooting end should be clamped; Range is defined for GET and should be ignored for HEAD. Ignoring Range and returning a complete 200 response can be legitimate, so different whole-file/multiple-range results are not automatically defects. [RFC 9110 range semantics](https://www.rfc-editor.org/rfc/rfc9110.html#name-range), [If-Range](https://www.rfc-editor.org/rfc/rfc9110.html#name-if-range)

Salvo's helper receives headers rather than a request method; our adapter explicitly selected send_head for HEAD. The application must still impose method-specific range policy. The same principle applies to wrapping the other helpers.

These findings make a dedicated Playscale media module necessary with any candidate. Resolve opaque IDs to approved files; evaluate preconditions against a stable source revision; normalize GET/HEAD behavior; enforce a deliberate single/multiple-range policy; and stream with bounded memory. Use separate policies for frontend assets, artwork, original media, and generated segments. Do not blanket-compress already compressed media or apply short JSON-request timeouts to hours-long responses.

## Runtime, cancellation, and background work

All three fit Tokio. None provides hardware transcoding by virtue of the web framework. Supervise FFmpeg subprocesses in an application job service, with explicit capability probing, concurrency limits, progress, persistence, cancellation, and cleanup.

Axum has graceful server shutdown; Poem exposes graceful shutdown with an optional timeout; Salvo provides graceful/forced server controls through its server-handle feature. Those are server lifecycle mechanisms, not a persistent-job or playback-session lifecycle. [Axum serve](https://docs.rs/axum/0.8.9/axum/serve/), [Poem Server](https://docs.rs/poem/3.1.12/poem/struct.Server.html), [Salvo ServerHandle](https://docs.rs/salvo_core/1.0.1/salvo_core/struct.ServerHandle.html)

Define separate limits for API work, open media bodies, and FFmpeg jobs. Hold media permits for the response-body lifetime. A dropped handler or disconnected socket must not be assumed to stop a spawned job. Shared cached transcodes might intentionally outlive one viewer; ephemeral processing might need prompt termination. Shutdown should stop admission, drain short work, bound long-stream draining, and reconcile unfinished jobs on restart.

SSE and WebSocket support exist across the candidates. Neither defines replay, ordering, reconnect cursors, dropped-event handling, or slow-consumer policy for us. Those need an application event contract and bounded queues.

## Tailscale and deployment

In the proposed Serve deployment, each candidate can listen on loopback and serve the webpage, public API, assets, and media from the same origin. No candidate wins authentication automatically: bind correctly, trust proxy information only through the intended path, and define browser-origin protections for mutations. Keep viewing profiles and administrative authorization explicit.

Embedded Tailscale is a separate adapter decision. Axum exposes a listener abstraction, and the other frameworks expose listener/acceptor abstractions, but this review did not compile an embedded Tailscale integration. Do not infer compatibility from “async Rust” alone. Native HTTP/3 or certificate automation does not automatically survive or improve a terminating proxy deployment.

## Maintenance and performance assessment

All three repositories showed recent activity in the review: Axum and Poem on October 5, Salvo on October 4. Poem's older stable release date is not evidence of abandonment; its main-branch documentation references upcoming version 4, which should not be confused with the tested version 3 API. Salvo reached 1.0 recently; that number alone does not establish operational maturity. [Axum repository](https://github.com/tokio-rs/axum), [Poem repository](https://github.com/poem-web/poem), [Salvo changelog](https://github.com/salvo-rs/salvo/blob/main/CHANGELOG.md)

Axum's Tower integration gives it an architectural maintenance advantage for this design: fewer custom adaptation boundaries around common service middleware. This is a judgment about our intended composition, not a measured support guarantee or quantified ecosystem ranking. Poem and Salvo remain credible maintained choices.

No throughput, memory, binary-size, or compile-time ranking is justified by these probes. A useful benchmark must combine slow media consumers with catalog API traffic, exercise local disk and NAS, and record p95/p99 API latency, CPU, memory, open descriptors, time to first byte, and cancellation latency. Repeat over direct and relayed Tailscale connections. A tiny JSON benchmark would not answer the important playback questions.

## Recommendation and acceptance gates

Choose **Tokio + Axum + Utoipa** unless we develop a strong preference for Salvo's integrated handler model or Poem's API objects. Axum offers the best fit for explicit, separate API/media/job policies and native Tower composition; Utoipa supplies the public API workflow without requiring Poem's framework coupling.

Choose **Salvo** if an integrated framework and its routing/middleware style materially simplify our implementation. Accept the newer compiler baseline and validate the recent 1.x release in our workload. Choose **Poem** if the API implementation style is the deciding productivity benefit, after resolving nullable contracts and replacing/wrapping the reviewed file helper.

Before declaring any stack production-ready, require:

1. A documented catalog/playback/job slice used by a generated independent client, with schema validation and stable errors.
2. Media conformance covering validators, empty/replaced files, GET/HEAD, suffix/overshooting/unsatisfiable ranges, and deliberate multiple-range handling.
3. Slow-client and disconnect tests proving bounded buffering and release of permits/file handles; separate FFmpeg cancellation tests.
4. Graceful shutdown with active playback and jobs, including restart reconciliation.
5. Demuxe playback and seeking through HTTPS/Tailscale on launch browsers, over direct and relayed connections.

The current probes establish compiled integration and selected HTTP behavior only. They do not establish these broader acceptance gates.
