# Building rivet

> **Status:** pre-implementation. The workspace described here does not exist yet. This document fixes the intended build workflow so the first commits follow it.

## Prerequisites

| Requirement | Version | Why |
|---|---|---|
| Rust toolchain | pin after the grammar/dependency spike; MSRV not yet established | CI must verify the selected toolchain and declared MSRV |
| C compiler | any recent `cc`, `clang`, or MSVC | Tree-sitter grammars are C and compile at build time |
| Git | any | fixture repositories are Git submodules |

SQLite is bundled via the `rusqlite` crate's `bundled` feature. No system SQLite is required.

## Build

```bash
git clone https://github.com/<org>/rivet
cd rivet
git submodule update --init         # test fixture repositories
cargo build -p rivet-cli --release
./target/release/rivet --version
```

A debug build is fine for development, but startup latency targets in the spec (§24) are measured on release builds only.

## Workspace layout

```text
rivet/
├── Cargo.toml                 workspace root; license = "MIT OR Apache-2.0"
├── LICENSE-MIT
├── LICENSE-APACHE
├── crates/
│   ├── rivet-cli/             clap definitions, output formatting, exit codes
│   ├── rivet-core/            data model: Symbol, Reference, Edge, Resolution, ids
│   ├── rivet-parser/          Tree-sitter driver, per-file parse and extraction
│   ├── rivet-index/           relationship graph, resolution, ranking, context builder
│   ├── rivet-store/           SQLite schema, atomic snapshots, persistence
│   └── rivet-languages/       one module per language implementing `Language`
│       └── src/
│           ├── php/
│           └── typescript/
├── tests/
│   ├── fixtures/              authored fixtures and real repositories as submodules
│   └── gold/                  gold annotations outside submodule directories
└── docs/
```

A virtual workspace does not discover root-level integration tests automatically. Put executable CLI tests in `crates/rivet-cli/tests/` and benchmarks in the owning package's `benches/`; keep shared data at the root. Commit `Cargo.lock` for reproducible binary builds. Package names here are local design names, not verified registry names.

## Language features

Each grammar is a Cargo feature so binaries can be trimmed:

```bash
cargo build -p rivet-cli --release --no-default-features --features lang-php
cargo build -p rivet-cli --release --features lang-php,lang-typescript
```

`default = ["lang-php", "lang-typescript"]` for the MVP. Adding a language is described in [ADDING-A-LANGUAGE.md](ADDING-A-LANGUAGE.md).

## Tests

```bash
cargo test --workspace
```

Test categories:

| Category | Location | What it checks |
|---|---|---|
| Unit | each crate | extraction, id construction, sort order, budget fitting |
| Snapshot | `crates/rivet-cli/tests/snapshots/` | JSON output for fixture queries is byte-stable; uses `insta` |
| Coverage/bindings | `crates/rivet-cli/tests/coverage.rs` | Candidate use spans and reference bindings match hand-annotated gold, including aliases/shadowing/top-level uses |
| Determinism | `crates/rivet-cli/tests/determinism.rs` | navigation bytes agree across repeated queries and full/incremental rebuilds; administrative counters are excluded |
| Freshness | `crates/rivet-cli/tests/freshness.rs` | content edits, preserved metadata, config/ignore invalidation, and concurrent refresh behavior match the acceptance matrix |

Update snapshots deliberately:

```bash
cargo insta review
```

Classify snapshot changes as schema or behavior changes in the PR (see [OUTPUT-CONTRACT.md](OUTPUT-CONTRACT.md)).

## Lint and format

CI fails on either:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

## Performance checks

Criterion benches live in each owning package's `benches/` and target the numbers in spec §24:

```bash
cargo bench
```

Separate `freshness_content_noop` and `freshness_metadata_noop` on a pinned 5,000-file corpus. Record bytes, p50/p95, hardware and cache state. Include end-to-end latency, not just time after refresh. Measure full re-resolution and stored-source index size too.

## Cross-compiling release binaries

Use native Linux/macOS runners where practical and a native Windows runner for MSVC. Build both planned architectures; execute tests natively or through an explicitly configured emulator. A successful cross-build does not count as a runtime test.

`cargo-zigbuild` is an optional Linux cross-build tool, not a replacement for all platform SDKs. It requires a separately installed compatible Zig; macOS cross-builds require an SDK. Follow its [official setup instructions](https://github.com/rust-cross/cargo-zigbuild) and pin tool versions in CI. Example after setup:

```bash
rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
cargo zigbuild -p rivet-cli --release --target x86_64-unknown-linux-musl
cargo zigbuild -p rivet-cli --release --target aarch64-unknown-linux-musl
```

Choose and record minimum OS versions only after artifact smoke tests, including bundled SQLite and grammar compilation. No target has been built or tested yet.

## Troubleshooting

**Grammar fails to compile.** Tree-sitter grammar crates need a working C compiler. On Debian/Ubuntu: `apt install build-essential`. On macOS: `xcode-select --install`.

**`rusqlite` link errors.** Ensure the `bundled` feature is enabled in `rivet-store/Cargo.toml`; do not depend on a system SQLite.

**Fixture tests fail with missing files.** Run `git submodule update --init`.

**Snapshot tests fail after an intentional change.** Run `cargo insta review`, accept, and classify the schema or behavior change in the PR and CHANGELOG.
