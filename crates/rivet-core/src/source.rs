//! Bounded regular-file reads, content hashing, and skip classification
//! (spec §27).
//!
//! [`read_source`] reads one eligible [`FileEntry`] without following symlinks
//! and without ever opening a FIFO, device, socket, or directory. Reads are
//! bounded by a caller-supplied byte limit, so an oversized file is classified
//! from a small prefix instead of being read in full. Content is stored exactly
//! as read: no CRLF normalization and no BOM stripping. This module touches no
//! database and parses no source.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use crate::walk::FileEntry;

/// Number of leading bytes inspected for a NUL byte when classifying binary
/// content (8 KiB).
const BINARY_SNIFF_BYTES: usize = 8 * 1024;

/// Why a source file was skipped instead of being returned as content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// The file is larger than the caller's byte limit.
    Size,
    /// A NUL byte appeared within the first 8 KiB.
    Binary,
    /// The bytes are not valid UTF-8.
    Encoding,
    /// The path is a symbolic link.
    Symlink,
    /// The path is not a regular file (FIFO, socket, device, or directory).
    NotRegular,
}

/// The outcome of reading one source file.
#[derive(Debug)]
pub enum SourceRead {
    /// The file was read within bounds and is valid UTF-8 source.
    Ok {
        /// The exact bytes read, unmodified.
        bytes: Vec<u8>,
        /// The canonical `blake3:` content hash of `bytes`.
        hash: String,
    },
    /// The file was deliberately not read as source.
    Skipped {
        /// Why the file was skipped.
        reason: SkipReason,
    },
    /// A filesystem operation failed.
    Failed(io::Error),
}

/// Reads `entry` under `root` as bounded, regular-file UTF-8 source.
///
/// `entry.rel_path` is joined to `root`. The path is inspected with
/// `symlink_metadata` before opening: symlinks and non-regular files are
/// rejected without opening them, so a FIFO can never block the caller. The
/// opened handle is `fstat`ed and, on Unix, its `(dev, ino)` identity is
/// compared against the pre-open metadata to reject a swap between the two
/// steps. At most `max_bytes + 1` bytes are read; a file with more than
/// `max_bytes` available is reported as [`SkipReason::Size`] without reading
/// the remainder.
///
/// Classification order after a successful bounded read is size, then binary
/// (a NUL byte in the first 8 KiB), then encoding (`std::str::from_utf8`
/// failure). A UTF-8 BOM is valid UTF-8 and is preserved.
pub fn read_source(root: &Path, entry: &FileEntry, max_bytes: u64) -> SourceRead {
    let path = root.join(&entry.rel_path);

    // Pre-open check: never open symlinks, FIFOs, devices, directories, or
    // sockets. Checking before opening also avoids blocking on a FIFO open.
    let pre = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) => return SourceRead::Failed(error),
    };
    let pre_type = pre.file_type();
    if pre_type.is_symlink() {
        return SourceRead::Skipped {
            reason: SkipReason::Symlink,
        };
    }
    if !pre_type.is_file() {
        return SourceRead::Skipped {
            reason: SkipReason::NotRegular,
        };
    }
    #[cfg(unix)]
    let pre_id = {
        use std::os::unix::fs::MetadataExt;
        (pre.dev(), pre.ino())
    };

    let file = match File::open(&path) {
        Ok(file) => file,
        Err(error) => return SourceRead::Failed(error),
    };

    // fstat the open handle: the path may have changed since the pre-open
    // check, and the handle is what we actually read from.
    let opened = match file.metadata() {
        Ok(metadata) => metadata,
        Err(error) => return SourceRead::Failed(error),
    };
    if !opened.file_type().is_file() {
        return SourceRead::Skipped {
            reason: SkipReason::NotRegular,
        };
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if (opened.dev(), opened.ino()) != pre_id {
            return SourceRead::Skipped {
                reason: SkipReason::NotRegular,
            };
        }
    }

    // Bounded read: at most `max_bytes + 1` bytes, so a one-byte overshoot is
    // distinguishable from an exact fit without reading the whole file.
    let limit = max_bytes.saturating_add(1);
    let mut bytes = Vec::new();
    let mut reader = file.take(limit);
    if let Err(error) = reader.read_to_end(&mut bytes) {
        return SourceRead::Failed(error);
    }
    if bytes.len() as u64 > max_bytes {
        return SourceRead::Skipped {
            reason: SkipReason::Size,
        };
    }

    // Classification order: Size (above), Binary, then Encoding. A NUL byte is
    // valid UTF-8, so the binary check must precede the encoding check.
    let sniff_len = bytes.len().min(BINARY_SNIFF_BYTES);
    if bytes[..sniff_len].contains(&0) {
        return SourceRead::Skipped {
            reason: SkipReason::Binary,
        };
    }
    if std::str::from_utf8(&bytes).is_err() {
        return SourceRead::Skipped {
            reason: SkipReason::Encoding,
        };
    }

    let hash = content_hash(&bytes);
    SourceRead::Ok { bytes, hash }
}

/// Returns the canonical content hash of `bytes`: the literal prefix `blake3:`
/// followed by 64 lowercase hexadecimal digits.
pub fn content_hash(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

#[cfg(test)]
mod tests {
    use super::{SkipReason, SourceRead, content_hash, read_source};
    use crate::test_support::TempDir;
    use crate::walk::FileEntry;
    use std::fs;
    use std::path::Path;

    /// A minimal entry; only `rel_path` is consulted by [`read_source`].
    fn entry(rel_path: &str) -> FileEntry {
        FileEntry {
            rel_path: rel_path.to_string(),
            size: 0,
            mtime_ns: 0,
        }
    }

    fn read(root: &Path, rel_path: &str, max_bytes: u64) -> SourceRead {
        read_source(root, &entry(rel_path), max_bytes)
    }

    #[test]
    fn content_hash_has_documented_shape() {
        let hash = content_hash(b"");
        assert_eq!(
            hash,
            "blake3:af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
        let hex = hash.strip_prefix("blake3:").expect("blake3 prefix");
        assert_eq!(hex.len(), 64);
        assert!(
            hex.chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
            "expected 64 lowercase hex digits, got {hex}"
        );
    }

    #[test]
    fn reads_regular_utf8_with_matching_hash() {
        let temp = TempDir::new("source-ok");
        let root = temp.path();
        let contents = b"<?php echo 1;\n";
        fs::write(root.join("a.php"), contents).unwrap();

        match read(root, "a.php", 1024) {
            SourceRead::Ok { bytes, hash } => {
                assert_eq!(bytes, contents);
                assert_eq!(hash, content_hash(contents));
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn size_limit_boundary() {
        let temp = TempDir::new("source-size");
        let root = temp.path();
        fs::write(root.join("exact.txt"), b"abcd").unwrap();
        fs::write(root.join("over.txt"), b"abcde").unwrap();

        assert!(
            matches!(read(root, "exact.txt", 4), SourceRead::Ok { .. }),
            "a file exactly at max_bytes must be read"
        );
        assert!(
            matches!(
                read(root, "over.txt", 4),
                SourceRead::Skipped {
                    reason: SkipReason::Size
                }
            ),
            "a file one byte over max_bytes must be Size"
        );
    }

    #[test]
    fn nul_byte_in_prefix_is_binary() {
        let temp = TempDir::new("source-binary");
        let root = temp.path();
        fs::write(root.join("bin.txt"), b"ab\0cd").unwrap();

        assert!(matches!(
            read(root, "bin.txt", 1024),
            SourceRead::Skipped {
                reason: SkipReason::Binary
            }
        ));
    }

    #[test]
    fn invalid_utf8_is_encoding() {
        let temp = TempDir::new("source-encoding");
        let root = temp.path();
        fs::write(root.join("bad.txt"), [0xff, 0xfe]).unwrap();

        assert!(matches!(
            read(root, "bad.txt", 1024),
            SourceRead::Skipped {
                reason: SkipReason::Encoding
            }
        ));
    }

    #[test]
    fn crlf_and_bom_are_preserved_byte_for_byte() {
        let temp = TempDir::new("source-bytes");
        let root = temp.path();
        let contents = b"\xef\xbb\xbf<?php\r\n$x = 1;\r\n";
        fs::write(root.join("bom.php"), contents).unwrap();

        match read(root, "bom.php", 1024) {
            SourceRead::Ok { bytes, .. } => assert_eq!(bytes, contents),
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_valid_file_is_skipped() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new("source-symlink");
        let root = temp.path();
        fs::write(root.join("real.txt"), "real").unwrap();
        symlink(root.join("real.txt"), root.join("link.txt")).unwrap();

        assert!(matches!(
            read(root, "link.txt", 1024),
            SourceRead::Skipped {
                reason: SkipReason::Symlink
            }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn fifo_is_not_regular_without_blocking() {
        let temp = TempDir::new("source-fifo");
        let root = temp.path();
        let fifo = root.join("pipe");
        let status = std::process::Command::new("mkfifo").arg(&fifo).status();
        match status {
            Ok(status) if status.success() => {}
            // `mkfifo` is unavailable in this environment; skip the check.
            _ => return,
        }

        // The pre-open check must reject the FIFO without ever opening it, so
        // this call returns rather than blocking.
        assert!(matches!(
            read(root, "pipe", 1024),
            SourceRead::Skipped {
                reason: SkipReason::NotRegular
            }
        ));
    }
}
