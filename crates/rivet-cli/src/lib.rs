//! rivet CLI library: command implementations, shared pipelines, and output
//! transport.
//!
//! The `rivet` binary in `main.rs` is a thin clap wrapper over these modules.
//! Keeping them in a library target lets integration tests open a committed
//! [`rivet_store::Store`] directly and exercise a shared pipeline against real
//! indexed rows without a second implementation for tests. In particular, T26's
//! context candidate collection lives in [`context`] and reuses the `refs`/
//! `symbol` reference pipeline in [`references`], which is only reachable from
//! this crate.

#[macro_use]
mod debug_hook;

pub mod budget;
pub mod context;
pub mod context_cmd;
pub mod human;
pub mod index;
pub mod init;
pub mod references;
pub mod refresh;
pub mod refs;
pub mod snippet;
pub mod symbol;
pub mod transport;
