# SQLx SQLite alignment for Catabolic

Upstream: SQLx SQLite 0.8.6, MIT OR Apache-2.0.
Archive: https://static.crates.io/crates/sqlx-sqlite/sqlx-sqlite-0.8.6.crate
Archive SHA-256: c2d12fe70b2c1b4401038055f90f151b78208de1f9f89a7dbfd41587a10c3eea

Only upstream change: Cargo.toml [dependencies.libsqlite3-sys] requirement
0.30.1 -> =0.38.2. Cargo.toml.orig records the unchanged upstream manifest.
All upstream Rust sources and license notices are preserved.

Motion owns this compatibility patch to share Catabolic's bundled SQLite.
It is not a SQLx upstream-supported version combination. Requalify migrations,
transactions, WAL, backup/restore, query limits and linked SQLite identity when
updating either owner. Preserve Catabolic's fancy-regex deadline patch and SQLite
build flags in the consuming workspace; dependency-local Cargo patches/config
are not inherited.
