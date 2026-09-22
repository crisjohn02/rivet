//! rivet command-line interface.
//!
//! Arguments, root discovery, scan/refresh orchestration, output, exit codes.

use clap::{Parser, Subcommand};

/// Agent-native codebase CLI: structural code navigation and token-budgeted context for coding agents.
#[derive(Debug, Parser)]
#[command(name = "rivet", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// The six MVP commands.
#[derive(Debug, Subcommand)]
enum Command {
    /// Set up rivet configuration and instructions in a repository.
    Init,
    /// Build or refresh the local index.
    Index,
    /// Locate a symbol declaration.
    Symbol {
        /// Symbol name, qualified name, or file:line to look up.
        query: String,
    },
    /// Find references to a symbol.
    Refs {
        /// Symbol name, qualified name, or file:line to look up.
        query: String,
    },
    /// Build context around a symbol.
    Context {
        /// Symbol name, qualified name, or file:line to look up.
        query: String,
    },
    /// Emit a short agent usage snippet.
    Snippet,
}

impl Command {
    /// The literal command name as typed on the command line.
    fn name(&self) -> &'static str {
        match self {
            Command::Init => "init",
            Command::Index => "index",
            Command::Symbol { .. } => "symbol",
            Command::Refs { .. } => "refs",
            Command::Context { .. } => "context",
            Command::Snippet => "snippet",
        }
    }
}

fn main() {
    let cli = Cli::parse();
    let name = cli.command.name();
    eprintln!("rivet {name}: not implemented yet (see docs/TASKS.md)");
    std::process::exit(1);
}
