# Releasing

> **Status:** process draft. Applies from the first tagged release.

## Versioning

`rivet` follows semantic versioning for the binary and a separate integer for the JSON schema.

| What | Where | Rule |
|---|---|---|
| Binary version | `Cargo.toml`, `rivet --version` | semver. Pre-1.0, a minor bump may break the CLI surface; the JSON schema is governed separately. |
| Schema version | `schema_version` in every JSON response | increments on a breaking JSON change (see OUTPUT-CONTRACT.md). |
| Index format version | `meta.index_format_version` in `index.db` | increments when the SQLite schema changes; supported older formats rebuild; unknown newer formats return an explicit error. |

## Checklist

0. **Readiness.** Confirm benchmark gates, actual platform test evidence, Rust MSRV, grammar licenses/attributions, registry package ownership, and GitHub owner. Replace every installation placeholder. No release exists yet.
1. **Changelog.** Move `Unreleased` entries in `CHANGELOG.md` under the new version with today's date. Classify snapshot changes as additive/breaking schema changes or behavior corrections.
2. **Schema.** If any entry is breaking, bump `schema_version` and update `docs/OUTPUT-CONTRACT.md`.
3. **Version.** Bump `version` in the workspace `Cargo.toml`. All crates share one version.
4. **Verify.**
   ```bash
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   cargo bench -- freshness_     # confirm no regression against the recorded baseline
   ```
5. **Tag.** `git tag -a v0.1.0 -m "rivet 0.1.0"` and push the tag. CI builds release targets and runs tests on configured native/emulated targets. Record build-only targets separately; draft the GitHub release with actual evidence.
6. **Artifacts.** CI attaches:
   ```text
   rivet-linux-x86_64.tar.gz
   rivet-linux-aarch64.tar.gz
   rivet-darwin-aarch64.tar.gz
   rivet-darwin-x86_64.tar.gz
   rivet-windows-x86_64.zip
   SHA256SUMS
   ```
   Each archive contains the binary, `LICENSE-MIT`, `LICENSE-APACHE`, and `README.md`.
7. **Smoke test.** Download one artifact, verify the checksum, run `rivet --version` and `rivet symbol` against a fixture. Do this by hand; CI already did it by machine.
8. **Publish.** Publish only after package names and ownership are verified. If internal crates remain separate registry dependencies, publish them in dependency order (core, languages, parser, store, index, CLI), verifying packaged path dependencies have matching registry versions. Otherwise consolidate packaging before advertising Cargo installation. Run `cargo package` and install the packaged CLI in a clean environment before publication.
9. **Release notes.** Paste the changelog section into the drafted release and publish it.
10. **Benchmark.** If the release changes anything in `refs`, `context`, resolution, or the snippet, rerun the benchmark and link the results from the release notes.

## Branching

`main` is always releasable. Work happens on short-lived branches. Release tags point at `main`.

## Hotfixes

Branch from the tag, fix, bump the patch version, tag, and cherry-pick to `main`.
