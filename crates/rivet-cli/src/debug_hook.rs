//! Debug-only test hooks that make concurrency tests deterministic (T31).
//!
//! Integration tests drive real `rivet` processes and need to stop one at a
//! named point, act (edit a file, start or kill another process), and then let
//! it continue. Sleeping and hoping is not deterministic, so the binary itself
//! signals and waits through files:
//!
//! - `RIVET_DEBUG_PAUSE_DIR=<dir>` enables the hooks. At every named point the
//!   process appends the point name and a newline to `<dir>/trace`, so a test
//!   can assert the exact sequence of points (for example that a race caused
//!   exactly one retry). When `<dir>/<point>.pause` exists, the process then
//!   creates `<dir>/<point>.reached` and blocks until `<dir>/<point>.go`
//!   exists. The wait gives up after [`MAX_PAUSE`] so an abandoned process
//!   cannot hang forever.
//! - `RIVET_DEBUG_BUSY_TIMEOUT_MS=<ms>` replaces the 5 second writer-lock busy
//!   timeout, so a lock-timeout test does not have to wait five seconds.
//! - `RIVET_DEBUG_MAX_NODES=<n>` and `RIVET_DEBUG_MAX_USES=<n>` lower the spec
//!   §27 parser resource bounds (T32), so a resource-limit test does not need a
//!   million-node file. They apply only to files this refresh actually parses:
//!   a normal refresh reuses an unchanged `ok` file without reparsing, so a test
//!   lowers them on a fresh repository or under `index --force`, which reparses
//!   every eligible file (AF6). A failed file is reparsed on every refresh.
//!
//! Call sites use [`debug_point!`], which expands to nothing in a release
//! build, and [`busy_timeout`] and [`resource_limits`] have
//! `cfg(not(debug_assertions))` twins that read no environment variable, so a
//! release binary contains neither the variable names, the point names, nor any
//! wait, and always uses the documented bounds.
//!
//! Points used by the refresh and query paths, where `N` is the refresh
//! attempt (1, or 2 after a detected race):
//!
//! | Point | Where |
//! |---|---|
//! | `refresh-before-lock-N` | just before `BEGIN IMMEDIATE` |
//! | `refresh-locked-N` | writer lock held, nothing loaded or walked yet |
//! | `refresh-staged-N` | facts and bindings written in the transaction, before the recheck walk |
//! | `refresh-raced-N` | recheck found a change; the attempt was rolled back (trace only) |
//! | `refresh-committed-N` | the snapshot committed |
//! | `query-snapshot-moved-N` | a query's read transaction saw another snapshot than the one just committed (trace only) |
//! | `query-snapshot-pinned` | a query's read transaction has read the snapshot digest and nothing else yet |

use std::time::Duration;

/// The longest a paused process waits for its `.go` file.
#[cfg(debug_assertions)]
const MAX_PAUSE: Duration = Duration::from_secs(120);

/// Records `point` and, when the test asked for it, pauses there.
#[cfg(debug_assertions)]
pub(crate) fn point(point: &str) {
    use std::io::Write as _;
    use std::path::PathBuf;

    let Some(dir) = std::env::var_os("RIVET_DEBUG_PAUSE_DIR").filter(|dir| !dir.is_empty()) else {
        return;
    };
    let dir = PathBuf::from(dir);
    if let Ok(mut trace) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("trace"))
    {
        let _ = writeln!(trace, "{point}");
    }
    if !dir.join(format!("{point}.pause")).exists() {
        return;
    }
    let _ = std::fs::write(dir.join(format!("{point}.reached")), b"");
    let go = dir.join(format!("{point}.go"));
    let started = std::time::Instant::now();
    while !go.exists() && started.elapsed() < MAX_PAUSE {
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Passes the named point `format!(...)` in a debug build; expands to nothing
/// in a release build, so not even the point name is formatted or stored.
macro_rules! debug_point {
    ($($name:tt)*) => {
        #[cfg(debug_assertions)]
        $crate::debug_hook::point(&format!($($name)*));
    };
}

/// The writer-lock busy timeout: [`rivet_store::WRITER_BUSY_TIMEOUT`], or the
/// test override.
#[cfg(debug_assertions)]
pub(crate) fn busy_timeout() -> Duration {
    std::env::var("RIVET_DEBUG_BUSY_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(rivet_store::WRITER_BUSY_TIMEOUT)
}

/// Release builds always use the documented 5 second timeout.
#[cfg(not(debug_assertions))]
pub(crate) fn busy_timeout() -> Duration {
    rivet_store::WRITER_BUSY_TIMEOUT
}

/// The parser resource bounds: [`rivet_parser::ResourceLimits::DEFAULT`], with
/// either count lowered (or raised) by its test override.
#[cfg(debug_assertions)]
pub(crate) fn resource_limits() -> rivet_parser::ResourceLimits {
    let read = |key: &str| {
        std::env::var(key)
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
    };
    let defaults = rivet_parser::ResourceLimits::DEFAULT;
    rivet_parser::ResourceLimits {
        max_visited_nodes: read("RIVET_DEBUG_MAX_NODES").unwrap_or(defaults.max_visited_nodes),
        max_extracted_uses: read("RIVET_DEBUG_MAX_USES").unwrap_or(defaults.max_extracted_uses),
    }
}

/// Release builds always use the spec §27 bounds.
#[cfg(not(debug_assertions))]
pub(crate) fn resource_limits() -> rivet_parser::ResourceLimits {
    rivet_parser::ResourceLimits::DEFAULT
}
