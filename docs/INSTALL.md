# Installing rivet

> **Status:** pre-implementation. No releases exist yet. This document describes the intended installation paths so they can be reviewed before the first release.

Package registry ownership and the GitHub organization are not yet verified. Commands below are templates, not runnable installation instructions; do not assume a registry package called `rivet` is this project.

## Planned platforms

| Target | Release artifact | Current evidence |
|---|---|---|
| Linux x86_64 | `rivet-linux-x86_64.tar.gz` | not built or tested yet |
| Linux ARM64 | `rivet-linux-aarch64.tar.gz` | not built or tested yet |
| macOS ARM64 | `rivet-darwin-aarch64.tar.gz` | not built or tested yet |
| macOS x86_64 | `rivet-darwin-x86_64.tar.gz` | not built or tested yet |
| Windows x86_64 | `rivet-windows-x86_64.zip` | planned CI build; not built or tested yet |

## Option 1: prebuilt binary

```bash
VERSION=0.1.0
TARGET=linux-x86_64          # or linux-aarch64, darwin-aarch64, darwin-x86_64

curl -fsSLO "https://github.com/<org>/rivet/releases/download/v${VERSION}/rivet-${TARGET}.tar.gz"
curl -fsSLO "https://github.com/<org>/rivet/releases/download/v${VERSION}/SHA256SUMS"
sha256sum --check --ignore-missing SHA256SUMS

tar -xzf "rivet-${TARGET}.tar.gz"
mkdir -p ~/.local/bin
install -m 0755 rivet ~/.local/bin/rivet   # or /usr/local/bin
```

Make sure the install directory is on `PATH`, then:

```bash
rivet --version
```

## Option 2: from crates.io

Requires a Rust toolchain and a C compiler (Tree-sitter grammars are C).

```text
cargo install <verified-package-name> --bin rivet
```

Language grammars are Cargo features. The default feature set includes the MVP languages. To install with a specific set:

```text
cargo install <verified-package-name> --bin rivet --no-default-features --features lang-php,lang-typescript
```

## Option 3: from source

See [BUILDING.md](BUILDING.md).

## Setting up a repository

Run once per repository:

```bash
cd your-repo
rivet init
```

This creates:

```text
.rivet/
├── config.toml      default configuration, edit freely
└── index.db         SQLite index and metadata, created on first query or `rivet index`
```

and appends `.rivet/` to `.gitignore` if it is not already ignored.

The first query on a large repository builds the index and may take a few seconds. Subsequent queries hash eligible source content and reparse only changed files. Returned source and positions refer to the same stored snapshot. `--freshness metadata` trades verification for speed; `--no-refresh` explicitly returns cached data. You can also build it explicitly:

```bash
rivet index
```

`rivet init` is optional in a Git repository. Any query run from inside one will locate the Git root and create `.rivet/` there. Auto-created caches do not modify `.gitignore` or instruction files; run `init` for that setup. Outside Git, `rivet init` is required so `rivet` knows where the repository root is. The nearest `.rivet/` or `.git` file/directory wins, including worktree roots.

## Making your agent aware of rivet

For complete Codex/Claude Code walkthroughs and the optional skill plan, see [INTEGRATION](INTEGRATION.md). Pick the destination explicitly for predictable setup: `rivet init --write-snippet --snippet-file AGENTS.md` for Codex, or `--snippet-file CLAUDE.md` for Claude Code. An explicit destination is created if missing. Install the binary in the same environment as the agent's shell.

The harness has to know the tool exists and when to prefer it. Append the standard snippet to whichever agent instruction file your project uses:

```bash
rivet snippet >> AGENTS.md      # Codex and others
rivet snippet >> CLAUDE.md      # Claude Code
```

Shell appending can duplicate the block. Prefer the idempotent managed-block installer:

```bash
rivet init --write-snippet
```

If both instruction files exist, select `--snippet-file AGENTS.md` or `--snippet-file CLAUDE.md`. If neither exists, init creates `AGENTS.md`. Text outside the managed block is preserved.

The snippet text is in [AGENT-SNIPPET.md](AGENT-SNIPPET.md).

## Configuration

`.rivet/config.toml` controls ignores, enabled languages, default context budget, and output limits. Command-line flags override it. See §22 of the specification for the full file.

## Upgrading

Prebuilt: repeat the download steps with the new version. crates.io: `cargo install <verified-package-name> --bin rivet` again. A supported old index format is rebuilt automatically, preserving configuration. Unknown newer formats or corruption return a clear error with a rebuild hint. JSON mode emits no progress prose on stderr.

## Uninstalling

Remove the binary and, per repository, delete `.rivet/`. If used, remove the managed instruction block and the `.gitignore` entry added by `init`; uninstall does not automatically edit those files.

## Verifying determinism

Two identical navigation queries against unchanged source/configuration with the same tool versions must produce byte-identical output (administrative work counters are excluded):

```bash
rivet refs Foo --json > a.json
rivet refs Foo --json > b.json
cmp a.json b.json && echo identical
```

If they differ, file a bug. Determinism is a contract, not a goal.
