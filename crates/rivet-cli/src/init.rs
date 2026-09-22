//! `rivet init` (spec §22, §25, §27; OUTPUT-CONTRACT "Administrative
//! commands").
//!
//! T33a implements the core of `init`: select the root, create `.rivet/` and
//! `.rivet/config.toml`, and append `.rivet/` to the root `.gitignore` when that
//! file does not already ignore it. Snippet installation (`--write-snippet`,
//! `--snippet-file`) is T33b and fails clearly until then.
//!
//! Every destination is validated before anything is written, so a rejected
//! run leaves the tree exactly as it was:
//!
//! - A symlinked `.rivet`, `.rivet/config.toml`, or `.gitignore` is refused
//!   rather than written through (spec §27), as is any of them that exists but
//!   is not the expected kind of entry (a `.rivet` regular file, a
//!   `config.toml` directory, a `.gitignore` FIFO).
//! - New files are created with `create_new`, which never follows a symlink
//!   and never replaces an entry that appeared after validation.
//! - An existing `.gitignore` is opened for appending during validation and
//!   its open handle's identity is compared with the validated entry, so a
//!   swap between the check and the open is caught before the first write.
//!   Appending through that handle preserves its permissions and every
//!   existing byte.
//!
//! The written paths are fixed names directly under the canonical root or the
//! validated real `.rivet/` directory, so they always stay within the root.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use ignore::gitignore::GitignoreBuilder;
use serde_json::{Value, json};

use rivet_core::{Config, RootError, RootInfo, discover_root};

use crate::transport::CliError;

/// The `.gitignore` entry `init` appends.
const GITIGNORE_ENTRY: &str = ".rivet/";

/// Options accepted by `rivet init`.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// `--write-snippet`: install the managed instruction block (T33b).
    pub write_snippet: bool,
    /// `--snippet-file AGENTS.md|CLAUDE.md`: explicit snippet destination
    /// (T33b). Requires `--write-snippet`.
    pub snippet_file: Option<String>,
}

/// What one `init` run did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// The selected root (canonical).
    pub root: PathBuf,
    /// Root-relative paths created, sorted by UTF-8 bytes. Directories end
    /// with `/`.
    pub created: Vec<String>,
    /// Root-relative paths modified, sorted by UTF-8 bytes.
    pub modified: Vec<String>,
}

/// The planned action on the root `.gitignore`.
enum GitignorePlan {
    /// It already ignores `.rivet/`; leave it untouched.
    AlreadyIgnored,
    /// It does not exist; create it containing only the entry.
    Create,
    /// It exists and needs the entry; append `bytes` through the validated
    /// open handle.
    Append { file: File, bytes: Vec<u8> },
}

/// Runs one `rivet init`.
///
/// Argument validation happens before any filesystem work; every destination
/// is validated before the first write.
pub fn run(options: Options) -> Result<Report, CliError> {
    validate_options(&options)?;

    let root = select_root()?;
    let rivet_dir = root.join(".rivet");
    let config_path = rivet_dir.join("config.toml");
    let gitignore_path = root.join(".gitignore");

    // Validation: nothing below writes until every destination is checked.
    let create_rivet_dir = match inspect(&rivet_dir)? {
        Entry::Missing => true,
        Entry::Directory => false,
        Entry::Symlink => return Err(symlinked(&rivet_dir)),
        Entry::File | Entry::Other => {
            return Err(unexpected_kind(&rivet_dir, "a directory"));
        }
    };
    let create_config = if create_rivet_dir {
        true
    } else {
        ensure_within(&root, &rivet_dir)?;
        match inspect(&config_path)? {
            Entry::Missing => true,
            // Existing configuration is preserved byte for byte, even when a
            // user edited it (spec §25).
            Entry::File => false,
            Entry::Symlink => return Err(symlinked(&config_path)),
            Entry::Directory | Entry::Other => {
                return Err(unexpected_kind(&config_path, "a regular file"));
            }
        }
    };
    let gitignore = plan_gitignore(&gitignore_path)?;

    // Writes, in dependency order.
    let mut created = Vec::new();
    let mut modified = Vec::new();
    if create_rivet_dir {
        std::fs::create_dir(&rivet_dir).map_err(|error| write_failed(&rivet_dir, &error))?;
        created.push(".rivet/".to_string());
    }
    if create_config {
        create_file(&config_path, Config::default_toml().as_bytes())?;
        created.push(".rivet/config.toml".to_string());
    }
    match gitignore {
        GitignorePlan::AlreadyIgnored => {}
        GitignorePlan::Create => {
            create_file(&gitignore_path, format!("{GITIGNORE_ENTRY}\n").as_bytes())?;
            created.push(".gitignore".to_string());
        }
        GitignorePlan::Append { mut file, bytes } => {
            file.write_all(&bytes)
                .and_then(|()| file.flush())
                .map_err(|error| write_failed(&gitignore_path, &error))?;
            modified.push(".gitignore".to_string());
        }
    }

    created.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    modified.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    Ok(Report {
        root,
        created,
        modified,
    })
}

/// The `init --json` success object, in contract key order.
pub fn success_json(report: &Report) -> Value {
    json!({
        "schema_version": 1,
        "created": report.created,
        "modified": report.modified,
        "snippet_file": Value::Null,
    })
}

/// A short plain summary; full human formatting is T34.
pub fn human(report: &Report) -> String {
    let mut out = format!("rivet root: {}\n", report.root.display());
    if report.created.is_empty() && report.modified.is_empty() {
        out.push_str("Already initialized; nothing changed.\n");
        return out;
    }
    for path in &report.created {
        out.push_str(&format!("created {path}\n"));
    }
    for path in &report.modified {
        out.push_str(&format!("modified {path}\n"));
    }
    out
}

/// Rejects snippet flags: `--snippet-file` requires `--write-snippet`
/// (OUTPUT-CONTRACT "Flag applicability"), and snippet installation itself is
/// T33b. Neither ever reaches the filesystem.
fn validate_options(options: &Options) -> Result<(), CliError> {
    if options.snippet_file.is_some() && !options.write_snippet {
        return Err(CliError::invalid_arguments(
            "`--snippet-file` requires `--write-snippet`",
            "Re-run as `rivet init --write-snippet --snippet-file AGENTS.md|CLAUDE.md`.",
        ));
    }
    if options.write_snippet {
        return Err(CliError::general(
            "rivet init --write-snippet is not implemented yet (expected in T33b; see docs/TASKS.md)",
            "Run `rivet init` without `--write-snippet`; snippet installation arrives in T33b.",
        ));
    }
    Ok(())
}

/// Selects the root per spec §25: the nearest ancestor with `.rivet/` or a
/// `.git` directory/file; outside any boundary, the current directory.
fn select_root() -> Result<PathBuf, CliError> {
    let cwd = std::env::current_dir().map_err(|error| {
        CliError::repository_unavailable(
            format!("cannot determine the working directory: {error}"),
            "Run rivet init from the directory to use as the repository root.",
        )
    })?;
    match discover_root(&cwd) {
        Ok(RootInfo { root, .. }) => Ok(root),
        // Outside Git, `init` establishes the current directory; `start` is
        // already canonical.
        Err(RootError::NoBoundary { start }) => Ok(start),
        Err(error @ RootError::SymlinkedRivetDir { .. }) => Err(CliError::repository_unavailable(
            error.to_string(),
            "Replace the symlinked .rivet with a real directory, or remove it and re-run `rivet init`.",
        )),
        Err(error) => Err(CliError::repository_unavailable(
            error.to_string(),
            "Check the directory permissions and retry.",
        )),
    }
}

/// The kind of entry at a destination, without following symlinks.
enum Entry {
    Missing,
    Directory,
    File,
    Symlink,
    Other,
}

/// Inspects `path` with `symlink_metadata`.
fn inspect(path: &Path) -> Result<Entry, CliError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            let file_type = metadata.file_type();
            Ok(if file_type.is_symlink() {
                Entry::Symlink
            } else if file_type.is_dir() {
                Entry::Directory
            } else if file_type.is_file() {
                Entry::File
            } else {
                Entry::Other
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Entry::Missing),
        Err(error) => Err(CliError::repository_unavailable(
            format!("cannot inspect {}: {error}", path.display()),
            "Check the repository permissions and retry.",
        )),
    }
}

/// Plans the `.gitignore` update, reading an existing file through a
/// validated handle that is kept open for the append.
fn plan_gitignore(path: &Path) -> Result<GitignorePlan, CliError> {
    match inspect(path)? {
        Entry::Missing => return Ok(GitignorePlan::Create),
        Entry::File => {}
        Entry::Symlink => return Err(symlinked(path)),
        Entry::Directory | Entry::Other => {
            return Err(unexpected_kind(path, "a regular file"));
        }
    }

    let pre = std::fs::symlink_metadata(path).map_err(|error| inspect_failed(path, &error))?;
    let mut file = OpenOptions::new()
        .read(true)
        .append(true)
        .open(path)
        .map_err(|error| inspect_failed(path, &error))?;
    let opened = file
        .metadata()
        .map_err(|error| inspect_failed(path, &error))?;
    if !opened.file_type().is_file() || !same_file(&pre, &opened) {
        return Err(CliError::repository_unavailable(
            format!(
                "{} changed while rivet init was validating it",
                path.display()
            ),
            "Retry `rivet init` once nothing else is replacing .gitignore.",
        ));
    }

    let mut contents = Vec::new();
    file.read_to_end(&mut contents)
        .map_err(|error| inspect_failed(path, &error))?;
    if ignores_rivet_dir(path.parent().unwrap_or(path), &contents) {
        return Ok(GitignorePlan::AlreadyIgnored);
    }
    let bytes = append_bytes(&contents);
    Ok(GitignorePlan::Append { file, bytes })
}

/// Whether two metadata snapshots describe the same filesystem object.
#[cfg(unix)]
fn same_file(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    (a.dev(), a.ino()) == (b.dev(), b.ino())
}

/// Whether two metadata snapshots describe the same filesystem object.
#[cfg(not(unix))]
fn same_file(_a: &std::fs::Metadata, _b: &std::fs::Metadata) -> bool {
    true
}

/// Whether the root `.gitignore` bytes already ignore the `.rivet/` directory.
///
/// The file's rules are evaluated with the same gitignore matcher the walker
/// uses, as Git would for the root directory: a later negation re-includes,
/// comments and blank lines are ignored, trailing unescaped spaces and a
/// trailing CR are ignored. So `.rivet`, `.rivet/`, `/.rivet`, `/.rivet/`,
/// and broader rules such as `.*` all count, while `# .rivet/`,
/// `.rivet/index.db`, `sub/.rivet/`, or `.rivet/` followed by `!.rivet/` do
/// not. Only the root `.gitignore` is consulted, never `.git/info/exclude` or
/// global excludes, because those are not shared with other clones. A line the
/// matcher cannot parse is skipped, which can only cause an append, never a
/// missed one.
fn ignores_rivet_dir(root: &Path, contents: &[u8]) -> bool {
    let text = String::from_utf8_lossy(contents);
    let mut builder = GitignoreBuilder::new(root);
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let _ = builder.add_line(None, line);
    }
    match builder.build() {
        Ok(gitignore) => gitignore.matched(Path::new(".rivet"), true).is_ignore(),
        Err(_) => false,
    }
}

/// The bytes to append to an existing `.gitignore` that lacks the entry.
///
/// A file that does not end with a line ending first gets one, so the entry
/// never joins the last existing line. The line ending matches the file's last
/// one (CRLF or LF); a file with none uses LF.
fn append_bytes(contents: &[u8]) -> Vec<u8> {
    let newline: &[u8] = match contents.iter().rposition(|byte| *byte == b'\n') {
        Some(index) if index > 0 && contents[index - 1] == b'\r' => b"\r\n",
        _ => b"\n",
    };
    let mut bytes = Vec::new();
    if !contents.is_empty() && !contents.ends_with(b"\n") {
        bytes.extend_from_slice(newline);
    }
    bytes.extend_from_slice(GITIGNORE_ENTRY.as_bytes());
    bytes.extend_from_slice(newline);
    bytes
}

/// Creates a new regular file with `contents`, refusing any existing entry,
/// symlinks included (`O_CREAT | O_EXCL` never follows a symlink).
fn create_file(path: &Path, contents: &[u8]) -> Result<(), CliError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| write_failed(path, &error))?;
    file.write_all(contents)
        .and_then(|()| file.flush())
        .map_err(|error| write_failed(path, &error))
}

/// Confirms that a validated directory resolves inside the root.
fn ensure_within(root: &Path, dir: &Path) -> Result<(), CliError> {
    let resolved = dir
        .canonicalize()
        .map_err(|error| inspect_failed(dir, &error))?;
    if resolved.parent() == Some(root) {
        Ok(())
    } else {
        Err(CliError::repository_unavailable(
            format!(
                "{} resolves outside the repository root {}",
                dir.display(),
                root.display()
            ),
            "Replace it with a real directory inside the repository root.",
        ))
    }
}

/// A symlinked destination (spec §27).
fn symlinked(path: &Path) -> CliError {
    CliError::repository_unavailable(
        format!("refusing to write through symlinked {}", path.display()),
        "Replace the symlink with a real file or directory, or remove it, and re-run `rivet init`.",
    )
}

/// A destination that exists as the wrong kind of entry.
fn unexpected_kind(path: &Path, expected: &str) -> CliError {
    CliError::repository_unavailable(
        format!("{} exists but is not {expected}", path.display()),
        "Move the conflicting entry aside and re-run `rivet init`.",
    )
}

fn inspect_failed(path: &Path, error: &std::io::Error) -> CliError {
    CliError::repository_unavailable(
        format!("cannot inspect {}: {error}", path.display()),
        "Check the repository permissions and retry.",
    )
}

fn write_failed(path: &Path, error: &std::io::Error) -> CliError {
    CliError::repository_unavailable(
        format!("cannot write {}: {error}", path.display()),
        "Check that the repository root is writable and retry.",
    )
}

#[cfg(test)]
mod tests {
    use super::{append_bytes, ignores_rivet_dir};
    use std::path::Path;

    fn ignored(text: &str) -> bool {
        ignores_rivet_dir(Path::new("/repo"), text.as_bytes())
    }

    #[test]
    fn equivalent_spellings_ignore_the_directory() {
        for text in [
            ".rivet\n",
            ".rivet/\n",
            "/.rivet\n",
            "/.rivet/\n",
            ".rivet/   \n",
            "/.rivet/\r\n",
            "node_modules/\n.rivet",
            ".*\n",
        ] {
            assert!(ignored(text), "{text:?} should ignore .rivet/");
        }
    }

    #[test]
    fn non_equivalent_rules_do_not_count() {
        for text in [
            "",
            "# .rivet/\n",
            ".rivet/index.db\n",
            "sub/.rivet/\n",
            ".rivet/\n!.rivet/\n",
            ".rivet.bak\n",
            "rivet/\n",
        ] {
            assert!(
                !ignored(text),
                "{text:?} should not count as ignoring .rivet/"
            );
        }
    }

    #[test]
    fn append_matches_the_existing_line_ending() {
        assert_eq!(append_bytes(b""), b".rivet/\n");
        assert_eq!(append_bytes(b"a\n"), b".rivet/\n");
        assert_eq!(append_bytes(b"a"), b"\n.rivet/\n");
        assert_eq!(append_bytes(b"a\r\n"), b".rivet/\r\n");
        assert_eq!(append_bytes(b"a\r\nb"), b"\r\n.rivet/\r\n");
    }
}
