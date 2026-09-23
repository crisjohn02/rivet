//! Eligible-file traversal (spec §26).
//!
//! [`walk_eligible`] enumerates the regular files under a repository root
//! without reading file contents, hashing, or touching a database. It applies
//! the mandatory exclusions, the configured `index.exclude` globs, optional
//! repository-local Git ignore rules, and nested-repository boundaries. The
//! returned inventory is deterministic (byte-sorted) and separates non-UTF-8
//! paths into a diagnostic list rather than lossy-converting them. A skipped
//! path is identified by its repository-relative form with the invalid bytes
//! escaped ([`escaped_relative_path`]).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use ignore::WalkBuilder;
use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::Config;

/// Names always excluded, as gitignore-style patterns.
///
/// The first two are mandatory per spec §26; the rest are the built-in
/// dependency/build defaults, which `respect_gitignore = false` does not
/// disable. `.git` has no trailing slash so it matches a `.git` *file* too: in
/// a Git worktree or submodule checkout the root's `.git` is a regular file
/// pointing at the real Git directory. It is Git metadata, not source, and
/// admitting it made a worktree report one more unsupported file (and a
/// different snapshot) than a plain clone of the same tree (T36).
const MANDATORY_EXCLUDES: [&str; 8] = [
    ".git",
    ".rivet/",
    "node_modules/",
    "vendor/",
    "dist/",
    "build/",
    "target/",
    "coverage/",
];

/// One eligible regular file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    /// Repo-relative path using `/` separators, with filesystem spelling
    /// preserved (no case folding or Unicode normalization).
    pub rel_path: String,
    /// File size in bytes.
    pub size: u64,
    /// Modification time as nanoseconds since the Unix epoch; negative for
    /// times before the epoch.
    pub mtime_ns: i128,
}

/// A path that could not be represented in the eligible inventory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedPath {
    /// The repository-relative, `/`-separated path with every byte that is not
    /// part of valid UTF-8 written as `\xNN` and a literal backslash as `\\`
    /// (see [`escaped_relative_path`]). For diagnostics only; never used as an
    /// identity.
    pub rel_escaped: String,
    /// A stable reason code describing why the path was skipped.
    pub reason: String,
}

/// The outcome of an eligible-file walk.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WalkResult {
    /// Eligible regular files, sorted by `rel_path` bytes.
    pub files: Vec<FileEntry>,
    /// Non-UTF-8 paths that were skipped, sorted by escaped bytes.
    pub skipped: Vec<SkippedPath>,
}

/// Why an eligible-file walk failed.
#[derive(Debug)]
pub enum WalkError {
    /// A filesystem operation on an eligible file failed.
    Io {
        /// The path involved.
        path: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },
    /// The walker or one of its ignore matchers failed.
    Ignore {
        /// The path involved, when known.
        path: PathBuf,
        /// The underlying error.
        source: ignore::Error,
    },
}

impl std::fmt::Display for WalkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WalkError::Io { path, source } => {
                write!(f, "cannot inspect {}: {source}", path.display())
            }
            WalkError::Ignore { path, source } => {
                write!(f, "cannot walk {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for WalkError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            WalkError::Io { source, .. } => Some(source),
            WalkError::Ignore { source, .. } => Some(source),
        }
    }
}

/// Walks `root` and returns its eligible regular files.
///
/// Eligibility follows spec §26: mandatory and configured exclusions always
/// apply; repository-local `.gitignore` files and `.git/info/exclude` apply
/// only when `config.index.respect_gitignore` is true. Symlinks, special
/// files, and nested repositories are never returned or descended into.
pub fn walk_eligible(root: &Path, config: &Config) -> Result<WalkResult, WalkError> {
    let respect_gitignore = config.index.respect_gitignore;
    // The `ignore` crate opens each descended directory's Git ignore files for
    // reading, and opening a FIFO blocks until a writer appears (spec §27:
    // never open FIFOs/devices). The root's files are read when the walk
    // starts, so they are checked here; every other directory is checked in
    // the entry filter below, which the crate runs before it descends.
    if respect_gitignore {
        check_ignore_files(root)?;
    }
    let unsafe_ignore_file: Arc<Mutex<Option<WalkError>>> = Arc::new(Mutex::new(None));
    let filter_unsafe = Arc::clone(&unsafe_ignore_file);
    let mandatory = build_gitignore(root, &MANDATORY_EXCLUDES)?;
    let configured_lines: Vec<&str> = config.index.exclude.iter().map(String::as_str).collect();
    let configured = build_gitignore(root, &configured_lines)?;

    let mut builder = WalkBuilder::new(root);
    builder
        // Hidden files are eligible; only explicit rules exclude them.
        .hidden(false)
        // No `.ignore` files and no ignore files from parent directories.
        .ignore(false)
        .parents(false)
        // Reproducibility: never consult global Git ignore configuration.
        .git_global(false)
        // Git rules are repository-local only, and only when requested.
        .git_ignore(config.index.respect_gitignore)
        .git_exclude(config.index.respect_gitignore)
        // Honor `.gitignore` even when a root lacks a detectable Git marker.
        .require_git(false)
        // Never follow symlinks; symlink entries are dropped below.
        .follow_links(false);

    builder.filter_entry(move |entry| {
        if entry.depth() == 0 {
            return true;
        }
        let Some(file_type) = entry.file_type() else {
            return false;
        };
        if file_type.is_symlink() {
            return false;
        }
        if file_type.is_dir() {
            // A nested Git repository or submodule is outside the scan domain.
            if is_nested_repository(entry.path()) {
                return false;
            }
        } else if !file_type.is_file() {
            // FIFOs, sockets, and device nodes are never eligible.
            return false;
        }
        let is_dir = file_type.is_dir();
        let eligible = !mandatory.matched(entry.path(), is_dir).is_ignore()
            && !configured.matched(entry.path(), is_dir).is_ignore();
        if eligible
            && is_dir
            && respect_gitignore
            && let Err(error) = check_ignore_files(entry.path())
        {
            // Record the first failure and stop descending here; the walk
            // then fails as a whole rather than silently dropping files.
            let mut slot = filter_unsafe
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            slot.get_or_insert(error);
            return false;
        }
        eligible
    });

    let mut files = Vec::new();
    let mut skipped = Vec::new();
    for entry in builder.build() {
        let entry = entry.map_err(|source| WalkError::Ignore {
            path: root.to_path_buf(),
            source,
        })?;
        if !entry
            .file_type()
            .is_some_and(|file_type| file_type.is_file())
        {
            continue;
        }
        let path = entry.path();
        let Some(rel_path) = relative_utf8(root, path) else {
            skipped.push(SkippedPath {
                rel_escaped: escaped_relative_path(root, path),
                reason: "non-utf8-path".to_string(),
            });
            continue;
        };
        let metadata = entry.metadata().map_err(|source| WalkError::Ignore {
            path: path.to_path_buf(),
            source,
        })?;
        let mtime_ns = mtime_ns(&metadata, path)?;
        files.push(FileEntry {
            rel_path,
            size: metadata.len(),
            mtime_ns,
        });
    }

    let unsafe_ignore_file = unsafe_ignore_file
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .take();
    if let Some(error) = unsafe_ignore_file {
        return Err(error);
    }

    files.sort_by(|a, b| a.rel_path.as_bytes().cmp(b.rel_path.as_bytes()));
    skipped.sort_by(|a, b| a.rel_escaped.as_bytes().cmp(b.rel_escaped.as_bytes()));
    Ok(WalkResult { files, skipped })
}

/// Fails when a Git ignore file the walker would read in `dir` exists but is
/// not a regular file (after following symlinks, as the reader would).
///
/// A missing file is fine. A FIFO, socket, device, or directory is refused
/// instead of opened, so a FIFO can never block the walk.
fn check_ignore_files(dir: &Path) -> Result<(), WalkError> {
    for path in [dir.join(".gitignore"), dir.join(".git/info/exclude")] {
        match std::fs::metadata(&path) {
            Ok(metadata) if !metadata.is_file() => {
                return Err(WalkError::Io {
                    path,
                    source: io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "Git ignore file is not a regular file; refusing to open it",
                    ),
                });
            }
            _ => {}
        }
    }
    Ok(())
}

/// Builds a gitignore-style matcher from `lines` rooted at `root`.
fn build_gitignore(root: &Path, lines: &[&str]) -> Result<Gitignore, WalkError> {
    let mut builder = GitignoreBuilder::new(root);
    for line in lines {
        builder
            .add_line(None, line)
            .map_err(|source| WalkError::Ignore {
                path: root.to_path_buf(),
                source,
            })?;
    }
    builder.build().map_err(|source| WalkError::Ignore {
        path: root.to_path_buf(),
        source,
    })
}

/// Returns true when `dir` itself is a Git repository boundary (it contains a
/// `.git` directory or a `.git` file, as in a worktree or submodule).
fn is_nested_repository(dir: &Path) -> bool {
    match std::fs::symlink_metadata(dir.join(".git")) {
        Ok(metadata) => {
            let file_type = metadata.file_type();
            file_type.is_dir() || file_type.is_file()
        }
        Err(_) => false,
    }
}

/// Converts `path` to a repo-relative `/`-separated string, or `None` when any
/// component is not valid UTF-8.
fn relative_utf8(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let mut out = String::new();
    for component in rel.components() {
        if !out.is_empty() {
            out.push('/');
        }
        out.push_str(component.as_os_str().to_str()?);
    }
    Some(out)
}

/// Renders `path` relative to `root` for a diagnostic, escaping invalid bytes.
///
/// OUTPUT-CONTRACT "Common index metadata": "Non-UTF-8 paths are excluded with
/// a diagnostic using escaped bytes". Like every other diagnostic `file`, the
/// result is repository-relative with `/` separators, never the absolute
/// path (AF5, audit finding 17). Valid UTF-8 runs are kept as they are; each
/// byte outside one is written as `\xNN` in lowercase hex, and a literal `\` is
/// doubled, so an escape cannot be confused with a name that spells one.
///
/// `path` always lies under `root` for walker output; if it does not, the
/// whole path is escaped rather than silently dropped.
pub fn escaped_relative_path(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    let mut out = String::new();
    for component in rel.components() {
        if !out.is_empty() {
            out.push('/');
        }
        push_escaped(&mut out, component.as_os_str());
    }
    out
}

/// Appends one path component to `out`, escaping as [`escaped_relative_path`].
fn push_escaped(out: &mut String, component: &std::ffi::OsStr) {
    #[cfg(unix)]
    let bytes = {
        use std::os::unix::ffi::OsStrExt;
        component.as_bytes().to_vec()
    };
    // Non-Unix paths have no raw byte view; their lossy form is the best
    // available rendering.
    #[cfg(not(unix))]
    let bytes = component.to_string_lossy().into_owned().into_bytes();
    for chunk in bytes.utf8_chunks() {
        for ch in chunk.valid().chars() {
            if ch == '\\' {
                out.push_str("\\\\");
            } else {
                out.push(ch);
            }
        }
        for byte in chunk.invalid() {
            out.push_str(&format!("\\x{byte:02x}"));
        }
    }
}

/// Returns the modification time of `metadata` as nanoseconds since the epoch.
fn mtime_ns(metadata: &std::fs::Metadata, path: &Path) -> Result<i128, WalkError> {
    let modified = metadata.modified().map_err(|source| WalkError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(match modified.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_nanos() as i128,
        Err(error) => -(error.duration().as_nanos() as i128),
    })
}

#[cfg(test)]
mod tests {
    use super::{WalkResult, escaped_relative_path, walk_eligible};
    use crate::Config;
    use crate::test_support::TempDir;
    use std::fs;
    use std::path::Path;

    /// The `rel_path` values of a result, in returned order.
    fn paths(result: &WalkResult) -> Vec<&str> {
        result
            .files
            .iter()
            .map(|file| file.rel_path.as_str())
            .collect()
    }

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn honors_nested_gitignore() {
        let temp = TempDir::new("walk-nested-gitignore");
        let root = temp.path();
        fs::create_dir_all(root.join(".git")).unwrap();
        write(&root.join(".gitignore"), "ignored.txt\n");
        write(&root.join("sub/.gitignore"), "nested.txt\n");
        write(&root.join("keep.txt"), "keep");
        write(&root.join("ignored.txt"), "no");
        write(&root.join("sub/keep2.txt"), "keep2");
        write(&root.join("sub/nested.txt"), "no");

        let result = walk_eligible(root, &Config::default()).unwrap();
        assert_eq!(
            paths(&result),
            vec![".gitignore", "keep.txt", "sub/.gitignore", "sub/keep2.txt"]
        );
    }

    #[test]
    fn git_info_exclude_works() {
        let temp = TempDir::new("walk-git-info-exclude");
        let root = temp.path();
        write(&root.join(".git/info/exclude"), "excluded.txt\n");
        write(&root.join("kept.txt"), "keep");
        write(&root.join("excluded.txt"), "no");

        let result = walk_eligible(root, &Config::default()).unwrap();
        assert_eq!(paths(&result), vec!["kept.txt"]);
    }

    #[test]
    fn configured_exclude_glob_works() {
        let temp = TempDir::new("walk-configured-exclude");
        let root = temp.path();
        write(&root.join("a.tmp"), "no");
        write(&root.join("a.txt"), "keep");

        let config = Config::from_toml("[index]\nexclude = [\"*.tmp\"]\n").unwrap();
        let result = walk_eligible(root, &config).unwrap();
        assert_eq!(paths(&result), vec!["a.txt"]);
    }

    #[test]
    fn configured_exclude_negation_is_order_sensitive() {
        let temp = TempDir::new("walk-configured-negation");
        let root = temp.path();
        write(&root.join("keep.log"), "keep");
        write(&root.join("drop.log"), "drop");

        // A later negation re-includes `keep.log`.
        let reinclude =
            Config::from_toml("[index]\nexclude = [\"*.log\", \"!keep.log\"]\n").unwrap();
        let result = walk_eligible(root, &reinclude).unwrap();
        assert_eq!(paths(&result), vec!["keep.log"]);

        // Reversing the list changes the meaning: nothing is re-included.
        let excluded =
            Config::from_toml("[index]\nexclude = [\"!keep.log\", \"*.log\"]\n").unwrap();
        let result = walk_eligible(root, &excluded).unwrap();
        assert!(paths(&result).is_empty(), "{:?}", paths(&result));
    }

    #[test]
    fn respect_gitignore_false_still_excludes_defaults() {
        let temp = TempDir::new("walk-no-gitignore");
        let root = temp.path();
        fs::create_dir_all(root.join(".git")).unwrap();
        write(&root.join(".gitignore"), "gitignored.txt\n");
        write(&root.join("gitignored.txt"), "now eligible");
        write(&root.join("node_modules/pkg.js"), "no");
        write(&root.join(".rivet/cache"), "no");
        write(&root.join("plain.txt"), "keep");

        let config = Config::from_toml("[index]\nrespect_gitignore = false\n").unwrap();
        let result = walk_eligible(root, &config).unwrap();
        assert_eq!(
            paths(&result),
            vec![".gitignore", "gitignored.txt", "plain.txt"]
        );
    }

    #[test]
    fn hidden_file_is_included() {
        let temp = TempDir::new("walk-hidden");
        let root = temp.path();
        write(&root.join(".hidden"), "hidden");
        write(&root.join("visible.txt"), "visible");

        let result = walk_eligible(root, &Config::default()).unwrap();
        assert_eq!(paths(&result), vec![".hidden", "visible.txt"]);
    }

    #[test]
    fn default_excludes_apply_at_any_depth() {
        let temp = TempDir::new("walk-defaults");
        let root = temp.path();
        write(&root.join("node_modules/pkg.js"), "no");
        write(&root.join("vendor/v.php"), "no");
        write(&root.join("dist/d.js"), "no");
        write(&root.join("build/b.rs"), "no");
        write(&root.join("target/t"), "no");
        write(&root.join("coverage/c.txt"), "no");
        write(&root.join("sub/target/x"), "no");
        write(&root.join("plain.txt"), "keep");

        let result = walk_eligible(root, &Config::default()).unwrap();
        assert_eq!(paths(&result), vec!["plain.txt"]);
    }

    #[test]
    fn output_order_is_byte_sorted() {
        let temp = TempDir::new("walk-order");
        let root = temp.path();
        write(&root.join("z.txt"), "z");
        write(&root.join("a.txt"), "a");
        write(&root.join("m.txt"), "m");
        write(&root.join("B.txt"), "B");

        let result = walk_eligible(root, &Config::default()).unwrap();
        assert_eq!(paths(&result), vec!["B.txt", "a.txt", "m.txt", "z.txt"]);
    }

    #[test]
    fn nested_repository_with_git_file_is_skipped() {
        let temp = TempDir::new("walk-nested-file");
        let root = temp.path();
        write(&root.join("root.txt"), "keep");
        write(&root.join("nested/.git"), "gitdir: ../elsewhere\n");
        write(&root.join("nested/file.txt"), "no");

        let result = walk_eligible(root, &Config::default()).unwrap();
        assert_eq!(paths(&result), vec!["root.txt"]);
    }

    #[test]
    fn worktree_git_file_at_the_root_is_not_eligible() {
        let temp = TempDir::new("walk-worktree-file");
        let root = temp.path();
        write(&root.join(".git"), "gitdir: /elsewhere/.git/worktrees/wt\n");
        write(&root.join("root.txt"), "keep");
        write(&root.join("sub/.gitkeep"), "keep");

        let result = walk_eligible(root, &Config::default()).unwrap();
        assert_eq!(paths(&result), vec!["root.txt", "sub/.gitkeep"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_git_ignore_file_is_refused_without_being_opened() {
        // Opening any of these FIFOs for reading would block forever with no
        // writer, so returning at all proves none was opened.
        for rel in [".gitignore", "sub/.gitignore", ".git/info/exclude"] {
            let temp = TempDir::new("walk-fifo-ignore");
            let root = temp.path();
            fs::create_dir_all(root.join(".git/info")).unwrap();
            write(&root.join("keep.txt"), "keep");
            write(&root.join("sub/keep.txt"), "keep");
            let fifo = root.join(rel);
            let status = std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .expect("run mkfifo");
            assert!(status.success(), "mkfifo {rel}");

            // Guard against a regression that opens the FIFO: the walk runs on
            // a thread, and if it has not returned in time a writer is opened,
            // which releases a blocked reader, and the test fails.
            let (sender, receiver) = std::sync::mpsc::channel();
            let walk_root = root.to_path_buf();
            let worker = std::thread::spawn(move || {
                let _ = sender.send(walk_eligible(&walk_root, &Config::default()));
            });
            let outcome = match receiver.recv_timeout(std::time::Duration::from_secs(20)) {
                Ok(outcome) => outcome,
                Err(_) => {
                    let _ = fs::OpenOptions::new().write(true).open(&fifo);
                    let _ = worker.join();
                    panic!("{rel}: the walk blocked, so it opened the FIFO");
                }
            };
            worker.join().expect("walk thread");
            let error = outcome.expect_err(rel);
            assert!(
                error.to_string().contains("not a regular file"),
                "{rel}: {error}"
            );

            // With Git ignore rules disabled the files are never read, and the
            // FIFO itself is not an eligible file.
            let mut config = Config::default();
            config.index.respect_gitignore = false;
            let result = walk_eligible(root, &config).expect(rel);
            assert_eq!(paths(&result), vec!["keep.txt", "sub/keep.txt"], "{rel}");
        }
    }

    #[test]
    fn nested_repository_with_git_dir_is_skipped() {
        let temp = TempDir::new("walk-nested-dir");
        let root = temp.path();
        write(&root.join("root.txt"), "keep");
        fs::create_dir_all(root.join("nested/.git")).unwrap();
        write(&root.join("nested/file.txt"), "no");

        let result = walk_eligible(root, &Config::default()).unwrap();
        assert_eq!(paths(&result), vec!["root.txt"]);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_file_and_dir_are_skipped() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new("walk-symlinks");
        let root = temp.path();
        write(&root.join("real.txt"), "real");
        write(&root.join("realdir/inner.txt"), "inner");
        symlink(root.join("real.txt"), root.join("link.txt")).unwrap();
        symlink(root.join("realdir"), root.join("linkdir")).unwrap();

        let result = walk_eligible(root, &Config::default()).unwrap();
        assert_eq!(paths(&result), vec!["real.txt", "realdir/inner.txt"]);
    }

    #[cfg(unix)]
    #[test]
    fn fifo_is_skipped() {
        let temp = TempDir::new("walk-fifo");
        let root = temp.path();
        write(&root.join("regular.txt"), "regular");
        let fifo = root.join("pipe");
        let status = std::process::Command::new("mkfifo").arg(&fifo).status();
        match status {
            Ok(status) if status.success() => {}
            // `mkfifo` is unavailable in this environment; skip the check.
            _ => return,
        }

        let result = walk_eligible(root, &Config::default()).unwrap();
        assert_eq!(paths(&result), vec!["regular.txt"]);
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_path_is_reported_as_skipped() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let temp = TempDir::new("walk-non-utf8");
        let root = temp.path();
        write(&root.join("normal.txt"), "normal");
        let bad = root.join(OsStr::from_bytes(b"bad-\xff.txt"));
        if fs::write(&bad, "bad").is_err() {
            // Some host filesystems (notably APFS on macOS) reject a filename
            // containing a byte that is not valid UTF-8, so the path cannot be
            // created here. The walker's non-UTF-8 skip path stays exercised on
            // Linux filesystems instead of weakening the assertions below.
            return;
        }

        let result = walk_eligible(root, &Config::default()).unwrap();
        assert_eq!(paths(&result), vec!["normal.txt"]);
        assert_eq!(result.skipped.len(), 1, "{:?}", result.skipped);
        assert_eq!(result.skipped[0].reason, "non-utf8-path");
        // Repository-relative with the invalid byte escaped, never absolute.
        assert_eq!(result.skipped[0].rel_escaped, "bad-\\xff.txt");
    }

    /// The diagnostic form is relative to the root, escapes each invalid byte
    /// as `\xNN`, keeps valid UTF-8 (including non-ASCII) as it is, and doubles
    /// a literal backslash. This runs everywhere because it builds the path in
    /// memory; the walk above needs a filesystem that accepts the name.
    #[cfg(unix)]
    #[test]
    fn escaped_relative_path_is_relative_and_escaped() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        use std::path::PathBuf;

        let root = Path::new("/repo/root");
        let path = root
            .join("src")
            .join(OsStr::from_bytes(b"caf\xc3\xa9-\xff\xfe.php"));
        assert_eq!(
            escaped_relative_path(root, &path),
            "src/café-\\xff\\xfe.php"
        );

        let slash = root.join(OsStr::from_bytes(b"a\\b-\x80"));
        assert_eq!(escaped_relative_path(root, &slash), "a\\\\b-\\x80");

        // A truncated multi-byte sequence escapes only its bytes.
        let truncated = root.join("d").join(OsStr::from_bytes(b"x\xe2\x82.ts"));
        assert_eq!(escaped_relative_path(root, &truncated), "d/x\\xe2\\x82.ts");

        // A plain UTF-8 path is unchanged apart from being relative.
        let plain: PathBuf = root.join("a/b.php");
        assert_eq!(escaped_relative_path(root, &plain), "a/b.php");
    }
}
