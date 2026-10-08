# Framework probes

Run on 2026-10-06 with Rust 1.95.0 on macOS. These are isolated research programs, not the Playscale implementation. See [the review](../../FRAMEWORK_REVIEW.md).

From this directory:

```sh
cargo +1.95.0 build --locked --bins
python3 probe.py
./target/debug/schemas schemas
```

Requires Python 3 and the Rust 1.95.0 toolchain. Use Cargo's default local target directory; `probe.py` expects `target/debug/playscale-framework-probe`.

`Cargo.lock` fixes the dependency graph. `versions.json` records selected crates.io release metadata. `results.json` contains 90 observations: 15 requests against each of three adapters, for two fixtures. They are observations, not 90 passing conformance assertions. Fixture files and servers are temporary; the script terminates its own servers. ETags and Last-Modified values can differ between runs.

The Axum adapter uses Tower HTTP ServeFile, Poem uses StaticFileEndpoint, and Salvo uses NamedFile with explicit GET/HEAD dispatch. Helpers receive request headers unchanged, so the probe exposes their defaults rather than an application range policy.

`schemas/*.json` records generated OpenAPI documents. `schemas/payloads.json` compares Serde and Poem Object serialization of the example object. The smoke program checks that the documented GET operation and its 200 response exist. It does not validate entire specifications or generate client SDKs.

Scope: loopback HTTP/1.1 with ten-byte and empty fixtures, development build. No throughput, memory, disconnect, slow-client, HTTP/2, TLS, Tailscale, browser/Demuxe, NAS, FFmpeg, or GPU qualification. Server startup and teardown in this harness do not test graceful shutdown.
