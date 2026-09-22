//! rivet command-line interface.
//!
//! Arguments, root discovery, scan/refresh orchestration, output, exit codes.
//! The command implementations live in the `rivet_cli` library target so
//! integration tests can drive the shared pipelines directly.

use clap::Parser;
use clap::error::ErrorKind;
use serde_json::Map;

use rivet_cli::transport::{CliError, emit_error, emit_success};
use rivet_cli::{context_cmd, index, init, refs, symbol};

/// Agent-native codebase CLI: structural code navigation and token-budgeted context for coding agents.
#[derive(Debug, Parser)]
#[command(name = "rivet", version)]
struct Cli {
    /// Emit exactly one machine-readable JSON object.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

/// The six MVP commands.
#[derive(Debug, clap::Subcommand)]
enum Command {
    /// Set up rivet configuration and instructions in a repository.
    Init {
        /// Install the managed instruction block (T33b; not implemented yet).
        #[arg(long = "write-snippet")]
        write_snippet: bool,
        /// Snippet destination, AGENTS.md or CLAUDE.md (T33b; requires `--write-snippet`).
        #[arg(long = "snippet-file", value_name = "AGENTS.md|CLAUDE.md")]
        snippet_file: Option<String>,
    },
    /// Build or refresh the local index.
    Index {
        /// Rebuild all facts even when content is unchanged.
        #[arg(long)]
        force: bool,
        /// Include `elapsed_ms` in JSON output.
        #[arg(long)]
        timing: bool,
        /// Comma-separated languages to index, overriding config.
        #[arg(long, value_name = "a,b")]
        languages: Option<String>,
        /// Freshness mode, overriding config.
        #[arg(long, value_name = "content|metadata")]
        freshness: Option<String>,
        /// Answer from the committed snapshot. Rejected for `index`.
        #[arg(long = "no-refresh")]
        no_refresh: bool,
    },
    /// Locate a symbol declaration.
    Symbol {
        /// Symbol name, qualified name, or file:line to look up.
        query: String,
        /// Maximum candidates to return.
        #[arg(long, value_name = "N")]
        limit: Option<u64>,
        /// Candidates to skip before the page.
        #[arg(long, value_name = "N")]
        offset: Option<u64>,
        /// Omit the call/caller lists.
        #[arg(long = "signature-only")]
        signature_only: bool,
        /// Include the symbol's source slice from stored bytes.
        #[arg(long)]
        source: bool,
        /// Minimum resolution tier to include in both call lists.
        #[arg(long = "min-resolution", value_name = "exact|scoped|name_match")]
        min_resolution: Option<String>,
        /// Freshness mode, overriding config.
        #[arg(long, value_name = "content|metadata")]
        freshness: Option<String>,
        /// Answer from the committed snapshot without refreshing.
        #[arg(long = "no-refresh")]
        no_refresh: bool,
    },
    /// Find references to a symbol.
    Refs {
        /// Symbol name, qualified name, or file:line to look up.
        query: String,
        /// Reference mode.
        #[arg(long, value_name = "references|candidates")]
        mode: Option<String>,
        /// Comma-separated `ref_kind` values to include.
        #[arg(long, value_name = "a,b")]
        kind: Option<String>,
        /// Minimum resolution tier to include.
        #[arg(long = "min-resolution", value_name = "exact|scoped|name_match")]
        min_resolution: Option<String>,
        /// Maximum references to return.
        #[arg(long, value_name = "N")]
        limit: Option<u64>,
        /// References to skip before the page.
        #[arg(long, value_name = "N")]
        offset: Option<u64>,
        /// Freshness mode, overriding config.
        #[arg(long, value_name = "content|metadata")]
        freshness: Option<String>,
        /// Answer from the committed snapshot without refreshing.
        #[arg(long = "no-refresh")]
        no_refresh: bool,
    },
    /// Build token-budgeted source context around a symbol.
    Context {
        /// Symbol name, qualified name, or file:line to look up.
        query: String,
        /// Source-token budget (1-1000000), overriding config.
        #[arg(long, value_name = "N")]
        tokens: Option<u64>,
        /// Relationship traversal depth, 1 or 2, overriding config.
        #[arg(long, value_name = "1|2")]
        depth: Option<u64>,
        /// Collapse mode, overriding config.
        #[arg(long, value_name = "auto|always|never")]
        collapse: Option<String>,
        /// Include callers from test files.
        #[arg(long = "include-tests")]
        include_tests: bool,
        /// Exclude callers from test files.
        #[arg(long = "exclude-tests")]
        exclude_tests: bool,
        /// Include callers of the target.
        #[arg(long = "include-callers")]
        include_callers: bool,
        /// Exclude callers of the target.
        #[arg(long = "exclude-callers")]
        exclude_callers: bool,
        /// Include callees of the target.
        #[arg(long = "include-callees")]
        include_callees: bool,
        /// Exclude callees of the target.
        #[arg(long = "exclude-callees")]
        exclude_callees: bool,
        /// Maximum segments, the target included.
        #[arg(long, value_name = "N")]
        limit: Option<u64>,
        /// Rejected for `context`, which does not paginate.
        #[arg(long, value_name = "N")]
        offset: Option<u64>,
        /// Freshness mode, overriding config.
        #[arg(long, value_name = "content|metadata")]
        freshness: Option<String>,
        /// Answer from the committed snapshot without refreshing.
        #[arg(long = "no-refresh")]
        no_refresh: bool,
    },
    /// Emit a short agent usage snippet.
    Snippet,
}

fn main() {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => handle_parse_error(error),
    };
    let json = cli.json;

    match cli.command {
        Command::Index {
            force,
            timing,
            languages,
            freshness,
            no_refresh,
        } => {
            let options = index::Options {
                force,
                timing,
                languages,
                freshness,
                no_refresh,
            };
            match index::run(options) {
                Ok(report) => {
                    if json {
                        emit_success(index::success_json(&report));
                    } else {
                        print!("{}", index::human(&report));
                    }
                }
                Err(error) => fail(json, &error, "index"),
            }
        }
        Command::Symbol {
            query,
            limit,
            offset,
            signature_only,
            source,
            min_resolution,
            freshness,
            no_refresh,
        } => {
            let options = symbol::Options {
                limit,
                offset,
                signature_only,
                source,
                min_resolution,
                freshness,
                no_refresh,
            };
            match symbol::run(&query, options) {
                Ok(value) => {
                    if json {
                        emit_success(value);
                    } else {
                        print!("{}", symbol::human(&value));
                    }
                }
                Err(error) => fail(json, &error, "symbol"),
            }
        }
        Command::Refs {
            query,
            mode,
            kind,
            min_resolution,
            limit,
            offset,
            freshness,
            no_refresh,
        } => {
            let options = refs::Options {
                mode,
                kind,
                min_resolution,
                limit,
                offset,
                freshness,
                no_refresh,
            };
            match refs::run(&query, options) {
                Ok(value) => {
                    if json {
                        emit_success(value);
                    } else {
                        print!("{}", refs::human(&value));
                    }
                }
                Err(error) => fail(json, &error, "refs"),
            }
        }
        Command::Context {
            query,
            tokens,
            depth,
            collapse,
            include_tests,
            exclude_tests,
            include_callers,
            exclude_callers,
            include_callees,
            exclude_callees,
            limit,
            offset,
            freshness,
            no_refresh,
        } => {
            let options = context_cmd::Options {
                tokens,
                depth,
                collapse,
                include_tests,
                exclude_tests,
                include_callers,
                exclude_callers,
                include_callees,
                exclude_callees,
                limit,
                offset,
                freshness,
                no_refresh,
            };
            match context_cmd::run(&query, options) {
                Ok(value) => {
                    if json {
                        emit_success(value);
                    } else {
                        print!("{}", context_cmd::human(&value));
                    }
                }
                Err(error) => fail(json, &error, "context"),
            }
        }
        Command::Init {
            write_snippet,
            snippet_file,
        } => {
            let options = init::Options {
                write_snippet,
                snippet_file,
            };
            match init::run(options) {
                Ok(report) => {
                    if json {
                        emit_success(init::success_json(&report));
                    } else {
                        print!("{}", init::human(&report));
                    }
                }
                Err(error) => fail(json, &error, "init"),
            }
        }
        Command::Snippet => not_implemented(json, "snippet", "T33b"),
    }
}

/// Reports a failed command: a JSON error object in `--json` mode, human
/// text otherwise, always with the documented exit code.
fn fail(json: bool, error: &CliError, command: &str) -> ! {
    if json {
        emit_error(
            error.code,
            error.exit,
            &error.message,
            &error.hint,
            (*error.extra).clone(),
        );
    }
    eprintln!("rivet {command}: {}", error.message);
    std::process::exit(error.exit);
}

/// Reports an unimplemented command without ever faking success.
fn not_implemented(json: bool, name: &str, task: &str) -> ! {
    let message =
        format!("rivet {name} is not implemented yet (expected in {task}; see docs/TASKS.md)");
    let error = CliError::general(
        message,
        "Only `rivet init`, `rivet index`, `rivet symbol`, `rivet refs`, and `rivet context` are implemented in this build.",
    );
    if json {
        emit_error(
            error.code,
            error.exit,
            &error.message,
            &error.hint,
            *error.extra,
        );
    }
    eprintln!("{}", error.message);
    std::process::exit(error.exit);
}

/// Renders clap parse failures, using the JSON error envelope when `--json` was
/// present. `--help`/`--version` stay text-only.
fn handle_parse_error(error: clap::Error) -> ! {
    if !matches!(
        error.kind(),
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
    ) && json_requested()
    {
        let message = error.to_string();
        let message = message.trim();
        emit_error(
            "invalid_arguments",
            2,
            message,
            "Run `rivet index --help` for usage.",
            Map::new(),
        );
    }
    // Text mode, or the text-only help/version output.
    error.exit();
}

/// Reports whether `--json` appeared anywhere on the command line.
fn json_requested() -> bool {
    std::env::args_os().any(|argument| argument == "--json")
}
