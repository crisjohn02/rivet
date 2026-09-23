//! rivet command-line interface.
//!
//! Arguments, root discovery, scan/refresh orchestration, output, exit codes.
//! The command implementations live in the `rivet_cli` library target so
//! integration tests can drive the shared pipelines directly.

use clap::error::ErrorKind;
use clap::{ColorChoice, Parser};
use serde_json::Map;

use rivet_cli::transport::{CliError, emit_error, emit_success};
use rivet_cli::{context_cmd, human, index, init, refs, snippet, symbol};

/// The help layout for every command (spec §30.1): examples first, then what
/// the command does and when to prefer it over `rg`, then clap's generated
/// usage and flag list, so the flags shown are always the flags parsed.
const HELP_TEMPLATE: &str =
    "{before-help}{about}\n\n{usage-heading} {usage}\n\n{all-args}{after-help}";

const TOP_EXAMPLES: &str = "\
Examples:
  rivet context SurveyService.launch --tokens 3000
  rivet symbol SurveyService.launch
  rivet refs SurveyService.launch --json
  rivet symbol app/Services/SurveyService.php:20
  rivet init --write-snippet";

const TOP_AFTER: &str = "\
When to use which:
  symbol   use instead of `rg` to find where a name is declared, with signature and callers
  refs     use instead of `rg` to list uses of one declaration, not every same-name string
  context  use instead of `rg` plus reading files: target and related source in one budget
  index    never instead of `rg`: optional warm-up and coverage report
  init     never instead of `rg`: one-time setup of .rivet/config.toml and instructions
  snippet  never instead of `rg`: prints the instruction block for AGENTS.md or CLAUDE.md

Names: short (launch), dotted (SurveyService.launch), qualified, canonical ID, or file:line.
Queries refresh the index automatically. `?` marks name-only (name_match) evidence.
Run `rivet help <command>` for its examples and flags.";

/// Structural code navigation and token-budgeted source retrieval for coding agents.
#[derive(Debug, Parser)]
#[command(
    name = "rivet",
    version,
    color = ColorChoice::Never,
    help_template = HELP_TEMPLATE,
    before_help = TOP_EXAMPLES,
    after_help = TOP_AFTER
)]
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
    /// Set up .rivet/config.toml and, with --write-snippet, the agent instruction block.
    #[command(
        help_template = HELP_TEMPLATE,
        before_help = "\
Examples:
  rivet init
  rivet init --write-snippet
  rivet init --write-snippet --snippet-file CLAUDE.md --json",
        after_help = "\
Use this instead of `rg` when: never; it is setup, not search. Run it once per repository.
Safe to re-run: existing files are left alone and the managed block is updated in place.
After setup, queries refresh the index automatically; no `rivet index` step is needed."
    )]
    Init {
        /// Install or update the managed instruction block.
        #[arg(long = "write-snippet")]
        write_snippet: bool,
        /// Block destination; requires `--write-snippet`.
        #[arg(long = "snippet-file", value_name = "AGENTS.md|CLAUDE.md")]
        snippet_file: Option<String>,
    },
    /// Build or refresh the local index and report coverage (optional; queries refresh).
    #[command(
        help_template = HELP_TEMPLATE,
        before_help = "\
Examples:
  rivet index
  rivet index --json
  rivet index --force --timing --json
  rivet index --languages php",
        after_help = "\
Use this instead of `rg` when: never; it builds the index that symbol/refs/context read.
No routine call is needed: every query refreshes first. Use it to warm the cache,
to see coverage (skipped files, diagnostics), or with --force after a suspect cache."
    )]
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
    /// Locate a declaration: kind, location, signature, and its calls and callers.
    #[command(
        help_template = HELP_TEMPLATE,
        before_help = "\
Examples:
  rivet symbol SurveyService.launch
  rivet symbol 'App\\Services\\SurveyService::launch' --source
  rivet symbol app/Services/SurveyService.php:20 --signature-only
  rivet symbol SurveyService.launch --min-resolution name_match --limit 20 --json",
        after_help = "\
Use this instead of `rg` when you need where a name is declared, not every line mentioning it.
Queries refresh automatically. `?` marks name-only (name_match) evidence; verify it.
Call lists default to --min-resolution scoped (exact and scoped rows) and count the
name-only rows they leave out; --min-resolution name_match lists those rows too.
Several matches exit 5 with candidate IDs: rerun with a quoted canonical ID.
Both call lists page with --limit/--offset; a truncated list prints its next --offset."
    )]
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
        /// Include the declaration's stored source text.
        #[arg(long)]
        source: bool,
        /// Minimum tier kept in call lists (default: scoped).
        #[arg(long = "min-resolution", value_name = "exact|scoped|name_match")]
        min_resolution: Option<String>,
        /// Freshness mode, overriding config.
        #[arg(long, value_name = "content|metadata")]
        freshness: Option<String>,
        /// Use the committed snapshot; skip the refresh.
        #[arg(long = "no-refresh")]
        no_refresh: bool,
    },
    /// Find references to one declaration, with containing symbol and resolution tier.
    #[command(
        help_template = HELP_TEMPLATE,
        before_help = "\
Examples:
  rivet refs SurveyService.launch
  rivet refs SurveyService.launch --min-resolution scoped
  rivet refs SurveyService.launch --mode candidates --kind call
  rivet refs SurveyService.launch --limit 50 --offset 50 --json",
        after_help = "\
Use this instead of `rg` when you want uses of one declaration, not every same-name string.
Queries refresh automatically. Tiers: exact, scoped, name_match (`?`, name-only; verify).
--mode candidates adds same-name uses bound elsewhere, for auditing. An empty result
never proves there are no runtime references; check the coverage line."
    )]
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
        /// Use the committed snapshot; skip the refresh.
        #[arg(long = "no-refresh")]
        no_refresh: bool,
    },
    /// Target source plus related callers, callees, and types within a token budget.
    #[command(
        help_template = HELP_TEMPLATE,
        before_help = "\
Examples:
  rivet context SurveyService.launch --tokens 3000
  rivet context SurveyService.launch --depth 2 --exclude-tests
  rivet context 'App\\Services\\SurveyService::launch' --collapse never --json",
        after_help = "\
Use this instead of `rg` when you would otherwise open several files to understand one symbol.
Queries refresh automatically. Each segment is `[reason, form]`; `signature` is a summary,
not the full body. The budget counts source only (utf8-bytes-v1), not metadata."
    )]
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
        /// Use the committed snapshot; skip the refresh.
        #[arg(long = "no-refresh")]
        no_refresh: bool,
    },
    /// Print the managed agent instruction block for AGENTS.md or CLAUDE.md.
    #[command(
        help_template = HELP_TEMPLATE,
        before_help = "\
Examples:
  rivet snippet
  rivet snippet --json",
        after_help = "\
Use this instead of `rg` when: never; it prints instructions, it does not search.
`rivet init --write-snippet` installs the same block. Needs no repository."
    )]
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
        Command::Snippet => {
            if json {
                emit_success(snippet::success_json());
            } else {
                print!("{}", snippet::SNIPPET);
            }
        }
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
    eprint!("{}", human::error_text(command, error));
    std::process::exit(error.exit);
}

/// Renders clap parse failures, using the JSON error envelope when `--json` was
/// present.
///
/// `--help`/`--version` are text-only and cannot be combined with `--json`
/// (OUTPUT-CONTRACT "Transport and common rules"), so the combination is an
/// argument error like any other when `--json` is present. Without `--json`
/// they print their text as usual.
fn handle_parse_error(error: clap::Error) -> ! {
    if json_requested() {
        let hint = usage_hint();
        let message = match error.kind() {
            ErrorKind::DisplayHelp => {
                "`--help` is text-only and cannot be combined with `--json`".to_string()
            }
            ErrorKind::DisplayVersion => {
                "`--version` is text-only and cannot be combined with `--json`".to_string()
            }
            _ => error.to_string().trim().to_string(),
        };
        emit_error("invalid_arguments", 2, &message, &hint, Map::new());
    }
    // Text mode, or the text-only help/version output.
    error.exit();
}

/// Reports whether `--json` appeared anywhere on the command line.
fn json_requested() -> bool {
    std::env::args_os().any(|argument| argument == "--json")
}

/// The usage hint for a parse failure, naming the subcommand that was given.
///
/// The first argument naming an MVP command selects it; without one the hint
/// points at the top-level help.
fn usage_hint() -> String {
    const COMMANDS: [&str; 6] = ["init", "index", "symbol", "refs", "context", "snippet"];
    let command = std::env::args_os().skip(1).find_map(|argument| {
        COMMANDS
            .iter()
            .find(|command| argument == **command)
            .copied()
    });
    match command {
        Some(command) => format!("Run `rivet {command} --help` for usage."),
        None => "Run `rivet --help` for usage.".to_string(),
    }
}
