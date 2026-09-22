//! Nearest-root discovery (spec §25).
//!
//! Walk upward from the start directory and stop at the first ancestor that
//! contains a `.rivet/` directory or a `.git` entry that is a directory or a
//! regular file (Git worktrees store `.git` as a file). A symlinked `.rivet`
//! is refused rather than followed (spec §27).

use std::fmt;
use std::path::{Path, PathBuf};

/// The nearest ancestor that establishes a repository root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootInfo {
    /// Canonicalized directory that contains the boundary.
    pub root: PathBuf,
    /// Whether the boundary is a `.rivet/` directory.
    pub has_rivet_dir: bool,
    /// Whether the boundary is a `.git` directory or file (worktree).
    pub has_git: bool,
}

/// Why root discovery could not produce a root.
#[derive(Debug)]
pub enum RootError {
    /// No `.rivet/` or `.git` boundary exists at or above the start path.
    NoBoundary {
        /// The canonicalized start path that was searched upward from.
        start: PathBuf,
    },
    /// A `.rivet` entry is a symlink; following it is refused (spec §27).
    SymlinkedRivetDir {
        /// The offending symlink path.
        path: PathBuf,
    },
    /// The start path or a boundary entry could not be inspected.
    Io {
        /// The path that could not be accessed.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },
}

impl fmt::Display for RootError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RootError::NoBoundary { start } => write!(
                f,
                "no .rivet/ or .git boundary found at or above {}; run `rivet init` to establish a root",
                start.display()
            ),
            RootError::SymlinkedRivetDir { path } => write!(
                f,
                "refusing to follow symlinked .rivet directory at {}",
                path.display()
            ),
            RootError::Io { path, source } => {
                write!(f, "cannot inspect {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for RootError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RootError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Finds the nearest root boundary at or above `start`.
///
/// `start` is canonicalized first, so the returned `root` and all comparisons
/// use canonical paths. The first ancestor (including `start` itself) with a
/// `.rivet/` directory or a `.git` directory/regular file wins; an outer
/// boundary is never selected once an inner one is found. A `.rivet` symlink is
/// an error naming the link rather than a boundary.
pub fn discover_root(start: &Path) -> Result<RootInfo, RootError> {
    let start = start.canonicalize().map_err(|source| RootError::Io {
        path: start.to_path_buf(),
        source,
    })?;

    for dir in start.ancestors() {
        if let Some(info) = check_rivet_dir(dir)? {
            return Ok(info);
        }
        if let Some(info) = check_git_entry(dir)? {
            return Ok(info);
        }
    }

    Err(RootError::NoBoundary { start })
}

/// Returns a root if `dir/.rivet` is a real directory, or an error if it is a
/// symlink. A missing or non-directory `.rivet` is not a boundary.
fn check_rivet_dir(dir: &Path) -> Result<Option<RootInfo>, RootError> {
    let path = dir.join(".rivet");
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(RootError::SymlinkedRivetDir { path })
        }
        Ok(metadata) if metadata.is_dir() => Ok(Some(RootInfo {
            root: dir.to_path_buf(),
            has_rivet_dir: true,
            has_git: false,
        })),
        Ok(_) => Ok(None),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(RootError::Io { path, source }),
    }
}

/// Returns a root if `dir/.git` is a directory or a regular file (worktree).
/// Symlinked or otherwise special `.git` entries are not boundaries.
fn check_git_entry(dir: &Path) -> Result<Option<RootInfo>, RootError> {
    let path = dir.join(".git");
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_dir() || metadata.file_type().is_file() => Ok(Some(RootInfo {
            root: dir.to_path_buf(),
            has_rivet_dir: false,
            has_git: true,
        })),
        Ok(_) => Ok(None),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(RootError::Io { path, source }),
    }
}

#[cfg(test)]
mod tests {
    use super::{RootError, discover_root};
    use crate::test_support::TempDir;
    use std::fs;

    #[test]
    fn start_directory_itself_is_a_boundary() {
        let temp = TempDir::new("root-self");
        fs::create_dir_all(temp.path().join(".git")).unwrap();

        let info = discover_root(temp.path()).unwrap();
        assert_eq!(info.root, temp.path().canonicalize().unwrap());
        assert!(info.has_git);
        assert!(!info.has_rivet_dir);
    }

    #[test]
    fn inner_rivet_dir_beats_outer_git_dir() {
        let temp = TempDir::new("root-inner-rivet");
        fs::create_dir_all(temp.path().join(".git")).unwrap();
        let inner = temp.path().join("packages/app");
        fs::create_dir_all(inner.join(".rivet")).unwrap();

        let info = discover_root(&inner).unwrap();
        assert_eq!(info.root, inner.canonicalize().unwrap());
        assert!(info.has_rivet_dir);
        assert!(!info.has_git);
    }

    #[test]
    fn inner_git_file_beats_outer_git_dir() {
        let temp = TempDir::new("root-worktree");
        fs::create_dir_all(temp.path().join(".git")).unwrap();
        let inner = temp.path().join("worktrees/feature");
        fs::create_dir_all(&inner).unwrap();
        fs::write(inner.join(".git"), "gitdir: ../../.git/worktrees/feature\n").unwrap();

        let info = discover_root(&inner).unwrap();
        assert_eq!(info.root, inner.canonicalize().unwrap());
        assert!(info.has_git);
        assert!(!info.has_rivet_dir);
    }

    #[test]
    fn start_inside_subdirectory_resolves_to_boundary_above() {
        let temp = TempDir::new("root-subdir");
        fs::create_dir_all(temp.path().join(".rivet")).unwrap();
        let nested = temp.path().join("src/deep/nested");
        fs::create_dir_all(&nested).unwrap();

        let info = discover_root(&nested).unwrap();
        assert_eq!(info.root, temp.path().canonicalize().unwrap());
        assert!(info.has_rivet_dir);
        assert!(!info.has_git);
    }

    #[test]
    fn no_boundary_reports_init_hint() {
        let temp = TempDir::new("root-none");

        let error = discover_root(temp.path()).unwrap_err();
        assert!(matches!(error, RootError::NoBoundary { .. }));
        let message = error.to_string();
        assert!(message.contains("rivet init"), "message was {message:?}");
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_rivet_dir_is_rejected() {
        let temp = TempDir::new("root-symlink");
        let real = temp.path().join("real-rivet");
        fs::create_dir_all(&real).unwrap();
        std::os::unix::fs::symlink(&real, temp.path().join(".rivet")).unwrap();

        let error = discover_root(temp.path()).unwrap_err();
        assert!(matches!(error, RootError::SymlinkedRivetDir { .. }));
        let message = error.to_string();
        assert!(message.contains(".rivet"), "message was {message:?}");
    }

    #[test]
    fn a_rivet_regular_file_is_not_a_boundary() {
        let temp = TempDir::new("root-rivet-file");
        fs::write(temp.path().join(".rivet"), "not a directory").unwrap();

        let error = discover_root(temp.path()).unwrap_err();
        assert!(matches!(error, RootError::NoBoundary { .. }));
    }
}
