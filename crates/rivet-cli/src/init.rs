//! `rivet init` (spec §22, §25, §27; OUTPUT-CONTRACT "Administrative
//! commands").
//!
//! `init` selects the root, creates `.rivet/` and `.rivet/config.toml`, and
//! appends `.rivet/` to the root `.gitignore` when that file does not already
//! ignore it (T33a). With `--write-snippet` it also installs or updates the
//! managed instruction block in `AGENTS.md` or `CLAUDE.md` (T33b).
//!
//! Snippet destination selection (spec §25, AGENT-SNIPPET "Installation
//! behavior"): an explicit `--snippet-file AGENTS.md|CLAUDE.md` wins and is
//! created if missing; otherwise the sole existing one of the two is used,
//! `AGENTS.md` is created when neither exists, and both existing is
//! `invalid_arguments` (exit 2). Existence is decided by exact, byte-equal
//! directory entry names, so the result is the same on case-sensitive and
//! case-insensitive filesystems. When the chosen name has no exact entry but a
//! differently cased one exists (`agents.md`), the run is refused rather than
//! writing through, or next to, a file under another name.
//!
//! The managed block (see [`install_block`]) is appended when the file has no
//! marker, replaced marker to marker when it has exactly one well-formed
//! block, and left byte-identical when already current. Any other marker
//! arrangement is refused as `repository_unavailable` (exit 3) without writing.
//! The block's own bytes are always the exact shipped LF bytes, even in a CRLF
//! file, so the installed text keeps the recorded snippet hash; only the line
//! endings `init` adds outside the block follow the file's style.
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
//! - An existing instruction file is read through a handle whose identity is
//!   compared with the validated entry, and updated by writing a new file next
//!   to it with the original permissions, then renaming it over the original
//!   after re-checking that the original is still the validated file. A crash
//!   therefore leaves either the old or the new file, never a torn one.
//!
//! The written paths are fixed names directly under the canonical root or the
//! validated real `.rivet/` directory, so they always stay within the root.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use ignore::gitignore::GitignoreBuilder;
use serde_json::{Value, json};

use rivet_core::{Config, RootError, RootInfo, discover_root};

use crate::snippet::{END_MARKER, SNIPPET, START_MARKER};
use crate::transport::CliError;

/// The `.gitignore` entry `init` appends.
const GITIGNORE_ENTRY: &str = ".rivet/";

/// The instruction files `--write-snippet` may manage, root-relative (spec
/// §25). Auto-selection prefers `AGENTS.md` only when neither exists.
const INSTRUCTION_FILES: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];

/// Options accepted by `rivet init`.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// `--write-snippet`: install or update the managed instruction block.
    pub write_snippet: bool,
    /// `--snippet-file AGENTS.md|CLAUDE.md`: explicit snippet destination.
    /// Requires `--write-snippet`.
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
    /// The root-relative snippet destination with `--write-snippet`, whether
    /// or not its bytes changed; `None` otherwise.
    pub snippet_file: Option<String>,
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

/// The planned action on the selected instruction file.
enum SnippetPlan {
    /// It already holds the current block; change no byte.
    Unchanged,
    /// It does not exist; create it containing only the block.
    Create,
    /// It exists and needs `bytes` as its new contents. `validated` is the
    /// metadata of the file whose contents were read.
    Rewrite {
        validated: std::fs::Metadata,
        bytes: Vec<u8>,
    },
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
    let snippet = if options.write_snippet {
        let name = select_snippet_file(&root, options.snippet_file.as_deref())?;
        let path = root.join(name);
        let plan = plan_snippet(name, &path)?;
        Some((name, path, plan))
    } else {
        None
    };
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
    if let Some((name, path, plan)) = &snippet {
        match plan {
            SnippetPlan::Unchanged => {}
            SnippetPlan::Create => {
                create_file(path, SNIPPET.as_bytes())?;
                created.push((*name).to_string());
            }
            SnippetPlan::Rewrite { validated, bytes } => {
                rewrite_file(&root, name, path, validated, bytes)?;
                modified.push((*name).to_string());
            }
        }
    }

    created.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    modified.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    Ok(Report {
        root,
        created,
        modified,
        snippet_file: snippet.map(|(name, _, _)| name.to_string()),
    })
}

/// The `init --json` success object, in contract key order.
pub fn success_json(report: &Report) -> Value {
    json!({
        "schema_version": 1,
        "created": report.created,
        "modified": report.modified,
        "snippet_file": report.snippet_file,
    })
}

/// A short plain summary; full human formatting is T34.
pub fn human(report: &Report) -> String {
    let mut out = format!("rivet root: {}\n", report.root.display());
    if report.created.is_empty() && report.modified.is_empty() {
        out.push_str("Already initialized; nothing changed.\n");
    }
    for path in &report.created {
        out.push_str(&format!("created {path}\n"));
    }
    for path in &report.modified {
        out.push_str(&format!("modified {path}\n"));
    }
    if let Some(name) = &report.snippet_file {
        out.push_str(&format!("snippet file: {name}\n"));
    }
    out
}

/// Validates the snippet flags before any filesystem work:
/// `--snippet-file` requires `--write-snippet` (OUTPUT-CONTRACT "Flag
/// applicability"), and only the exact root-relative names `AGENTS.md` and
/// `CLAUDE.md` are accepted in v0.1 (spec §25).
fn validate_options(options: &Options) -> Result<(), CliError> {
    if options.snippet_file.is_some() && !options.write_snippet {
        return Err(CliError::invalid_arguments(
            "`--snippet-file` requires `--write-snippet`",
            "Re-run as `rivet init --write-snippet --snippet-file AGENTS.md|CLAUDE.md`.",
        ));
    }
    if let Some(name) = &options.snippet_file
        && !INSTRUCTION_FILES.contains(&name.as_str())
    {
        return Err(CliError::invalid_arguments(
            format!("unsupported --snippet-file {name:?}; expected AGENTS.md or CLAUDE.md"),
            "Pass `--snippet-file AGENTS.md` or `--snippet-file CLAUDE.md`; destinations are root-relative and limited to these two files.",
        ));
    }
    Ok(())
}

/// Chooses the snippet destination (spec §25). `explicit` has already been
/// validated to be one of [`INSTRUCTION_FILES`].
///
/// Existence is decided from the root's directory entries by exact name, not
/// by `stat`, which on a case-insensitive filesystem would report
/// `agents.md` as `AGENTS.md`. A chosen name whose only match is a
/// differently cased entry is refused: writing would either go through a file
/// reported under the wrong name or, on a case-sensitive filesystem, create a
/// near-duplicate next to it.
fn select_snippet_file(root: &Path, explicit: Option<&str>) -> Result<&'static str, CliError> {
    let entries: Vec<std::ffi::OsString> = std::fs::read_dir(root)
        .and_then(|dir| {
            dir.map(|entry| entry.map(|entry| entry.file_name()))
                .collect()
        })
        .map_err(|error| inspect_failed(root, &error))?;
    let exists = |name: &str| entries.iter().any(|entry| entry == name);

    let chosen = match explicit {
        Some(name) => INSTRUCTION_FILES
            .into_iter()
            .find(|candidate| *candidate == name)
            .unwrap_or(INSTRUCTION_FILES[0]),
        None => match (exists("AGENTS.md"), exists("CLAUDE.md")) {
            (true, true) => {
                return Err(CliError::invalid_arguments(
                    "both AGENTS.md and CLAUDE.md exist; rivet will not guess which to update",
                    "Re-run with `--snippet-file AGENTS.md` or `--snippet-file CLAUDE.md`.",
                ));
            }
            (false, true) => "CLAUDE.md",
            (true, false) | (false, false) => "AGENTS.md",
        },
    };

    if !exists(chosen) {
        let mut variants: Vec<String> = entries
            .iter()
            .filter_map(|entry| entry.to_str())
            .filter(|entry| entry.eq_ignore_ascii_case(chosen))
            .map(str::to_string)
            .collect();
        variants.sort();
        if let Some(variant) = variants.first() {
            return Err(CliError::repository_unavailable(
                format!(
                    "{} exists but is not named exactly {chosen}",
                    root.join(variant).display()
                ),
                format!(
                    "Rename {variant} to {chosen} (or move it aside) and re-run `rivet init --write-snippet`."
                ),
            ));
        }
    }
    Ok(chosen)
}

/// Plans the instruction-file update, reading an existing file through a
/// handle whose identity matches the validated entry.
fn plan_snippet(name: &str, path: &Path) -> Result<SnippetPlan, CliError> {
    match inspect(path)? {
        Entry::Missing => return Ok(SnippetPlan::Create),
        Entry::File => {}
        Entry::Symlink => return Err(symlinked(path)),
        Entry::Directory | Entry::Other => {
            return Err(unexpected_kind(path, "a regular file"));
        }
    }

    let pre = std::fs::symlink_metadata(path).map_err(|error| inspect_failed(path, &error))?;
    let mut file = File::open(path).map_err(|error| inspect_failed(path, &error))?;
    let opened = file
        .metadata()
        .map_err(|error| inspect_failed(path, &error))?;
    if !opened.file_type().is_file() || !same_file(&pre, &opened) {
        return Err(changed_during_init(path));
    }
    let mut contents = Vec::new();
    file.read_to_end(&mut contents)
        .map_err(|error| inspect_failed(path, &error))?;

    match install_block(&contents) {
        Ok(None) => Ok(SnippetPlan::Unchanged),
        Ok(Some(bytes)) => Ok(SnippetPlan::Rewrite {
            validated: opened,
            bytes,
        }),
        Err(problem) => Err(CliError::repository_unavailable(
            format!(
                "{} has a malformed rivet managed block: {problem}",
                path.display()
            ),
            format!(
                "Edit {name} so it contains at most one `{START_MARKER}` ... `{END_MARKER}` block, in that order, then re-run `rivet init --write-snippet`. rivet never appends a second block."
            ),
        )),
    }
}

/// Why existing markers cannot be updated safely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockProblem {
    StartWithoutEnd,
    EndWithoutStart,
    EndBeforeStart,
    RepeatedMarkers,
}

impl std::fmt::Display for BlockProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            BlockProblem::StartWithoutEnd => "a start marker has no end marker",
            BlockProblem::EndWithoutStart => "an end marker has no start marker",
            BlockProblem::EndBeforeStart => "the end marker comes before the start marker",
            BlockProblem::RepeatedMarkers => {
                "markers are repeated or nested (more than one start or end marker)"
            }
        })
    }
}

/// The new contents of an instruction file holding `contents`, or `None` when
/// the current block is already there byte for byte.
///
/// A marker is any occurrence of the exact marker text, wherever it appears,
/// so a mention inside prose or a code span also counts; refusing is safer
/// than guessing which occurrence is the block.
///
/// - No marker: the block is appended. An empty file becomes exactly the
///   block. Otherwise an unterminated last line is terminated, and one blank
///   line is added unless the last line is already blank (empty or only
///   spaces/tabs); existing trailing blank lines are kept, never removed. The
///   line endings added here follow the file's last line ending (CRLF or LF;
///   LF when it has none), as T33a does for `.gitignore`.
/// - Exactly one start marker followed by exactly one end marker: the bytes
///   from the first byte of the start marker through the last byte of the end
///   marker are replaced with the block without its final LF, so every byte
///   before and after is preserved. When the end marker is the last thing in
///   the file, the block's final LF is added after it.
/// - Anything else is a [`BlockProblem`].
///
/// The block itself is always the exact shipped LF bytes, even in a CRLF
/// file, so a repeat run is a no-op and the installed text keeps the recorded
/// hash.
fn install_block(contents: &[u8]) -> Result<Option<Vec<u8>>, BlockProblem> {
    let starts = find_all(contents, START_MARKER.as_bytes());
    let ends = find_all(contents, END_MARKER.as_bytes());
    match (starts.as_slice(), ends.as_slice()) {
        ([], []) => Ok(Some(append_block(contents))),
        ([start], [end]) if start < end => {
            let block = SNIPPET.strip_suffix('\n').unwrap_or(SNIPPET).as_bytes();
            let after = &contents[end + END_MARKER.len()..];
            let mut bytes = Vec::with_capacity(contents.len() + SNIPPET.len());
            bytes.extend_from_slice(&contents[..*start]);
            bytes.extend_from_slice(block);
            if after.is_empty() {
                bytes.push(b'\n');
            } else {
                bytes.extend_from_slice(after);
            }
            Ok((bytes != contents).then_some(bytes))
        }
        ([_], [_]) => Err(BlockProblem::EndBeforeStart),
        ([_], []) => Err(BlockProblem::StartWithoutEnd),
        ([], [_]) => Err(BlockProblem::EndWithoutStart),
        _ => Err(BlockProblem::RepeatedMarkers),
    }
}

/// `contents` with the block appended (see [`install_block`]).
fn append_block(contents: &[u8]) -> Vec<u8> {
    if contents.is_empty() {
        return SNIPPET.as_bytes().to_vec();
    }
    let newline: &[u8] = match contents.iter().rposition(|byte| *byte == b'\n') {
        Some(index) if index > 0 && contents[index - 1] == b'\r' => b"\r\n",
        _ => b"\n",
    };
    let mut bytes = contents.to_vec();
    if !bytes.ends_with(b"\n") {
        bytes.extend_from_slice(newline);
    }
    let body = &bytes[..bytes.len() - 1];
    let body = body.strip_suffix(b"\r").unwrap_or(body);
    let last_line = match body.iter().rposition(|byte| *byte == b'\n') {
        Some(index) => &body[index + 1..],
        None => body,
    };
    if !last_line.iter().all(|byte| *byte == b' ' || *byte == b'\t') {
        bytes.extend_from_slice(newline);
    }
    bytes.extend_from_slice(SNIPPET.as_bytes());
    bytes
}

/// Every offset at which `needle` occurs in `haystack`.
fn find_all(haystack: &[u8], needle: &[u8]) -> Vec<usize> {
    haystack
        .windows(needle.len())
        .enumerate()
        .filter(|(_, window)| *window == needle)
        .map(|(index, _)| index)
        .collect()
}

/// Replaces the validated file at `path` with `bytes` atomically, keeping its
/// permissions.
///
/// The new contents go to a fresh sibling created with `create_new`, get the
/// original permissions, and are synced; the original is then re-checked
/// (same file, same length and modification time as when it was read, still
/// not a symlink) and the sibling is renamed over it. Renaming replaces the
/// directory entry itself, so it can never write through a symlink swapped in
/// meanwhile. Ownership and hard links of the original are not carried over.
fn rewrite_file(
    root: &Path,
    name: &str,
    path: &Path,
    validated: &std::fs::Metadata,
    bytes: &[u8],
) -> Result<(), CliError> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let temp = root.join(format!(".{name}.rivet-{}-{nanos}.tmp", std::process::id()));

    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|error| write_failed(&temp, &error))?;
        file.set_permissions(validated.permissions())
            .and_then(|()| file.write_all(bytes))
            .and_then(|()| file.sync_all())
            .map_err(|error| write_failed(&temp, &error))?;
        drop(file);

        let current =
            std::fs::symlink_metadata(path).map_err(|error| inspect_failed(path, &error))?;
        if !current.file_type().is_file()
            || !same_file(validated, &current)
            || current.len() != validated.len()
            || current.modified().ok() != validated.modified().ok()
        {
            return Err(changed_during_init(path));
        }
        std::fs::rename(&temp, path).map_err(|error| write_failed(path, &error))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// A destination replaced between validation and use.
fn changed_during_init(path: &Path) -> CliError {
    CliError::repository_unavailable(
        format!(
            "{} changed while rivet init was validating it",
            path.display()
        ),
        "Retry `rivet init` once nothing else is modifying the file.",
    )
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
