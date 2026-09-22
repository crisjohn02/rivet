//! SQLite schema and minimal store operations (spec §27; ARCHITECTURE
//! "Minimal logical schema" and "Concurrency and source consistency").
//!
//! This crate owns the disposable index cache. [`Store::open`] validates the
//! `.rivet/` destination before touching the database, opens `index.db` with
//! foreign keys enabled, and either creates the version-1 schema or refuses a
//! database whose `meta.index_format_version` differs. Minimal row operations
//! run inside explicit transactions. [`Store::publish_inventory`] atomically
//! replaces the complete file inventory and writes the configuration
//! fingerprints plus the deterministic snapshot digest. Walking, parsing, and
//! resolution belong to later tasks.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use rivet_core::ParseStatus;
use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

/// The database file name inside `.rivet/`.
const INDEX_DB_FILE: &str = "index.db";

/// The only supported value of `meta.index_format_version`.
pub const INDEX_FORMAT_VERSION: &str = "1";

/// The complete version-1 schema from ARCHITECTURE "Minimal logical schema".
///
/// `PRAGMA foreign_keys = ON` is applied per connection in
/// [`configure_connection`] rather than here.
const SCHEMA_SQL: &str = "
CREATE TABLE meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
); -- index_format_version, extractor_fingerprint, resolver_fingerprint,
   -- effective_config_fingerprint, snapshot_digest

CREATE TABLE files (
  path TEXT PRIMARY KEY,
  language TEXT,
  mtime_ns INTEGER NOT NULL,
  size INTEGER NOT NULL,
  content_hash TEXT,
  source BLOB,
  parse_status TEXT NOT NULL
); -- ok | parse_error | resource_limit | binary | size | encoding | unsupported
   -- source holds exactly the bytes parsed for ok files; NULL for skipped files

CREATE TABLE symbols (
  id TEXT PRIMARY KEY,
  file TEXT NOT NULL REFERENCES files(path) ON DELETE CASCADE,
  name TEXT NOT NULL,
  lookup_name TEXT NOT NULL,
  qualified_name TEXT NOT NULL,
  kind TEXT NOT NULL,
  parent_id TEXT REFERENCES symbols(id) ON DELETE SET NULL
    DEFERRABLE INITIALLY DEFERRED,
  start_byte INTEGER NOT NULL,
  end_byte INTEGER NOT NULL,
  start_line INTEGER NOT NULL,
  end_line INTEGER NOT NULL,
  signature TEXT,
  doc_comment TEXT,
  CHECK (0 <= start_byte AND start_byte < end_byte)
);
CREATE INDEX symbols_lookup ON symbols(lookup_name);
CREATE INDEX symbols_qname ON symbols(qualified_name);
CREATE INDEX symbols_file ON symbols(file, start_byte);

CREATE TABLE uses (
  use_id INTEGER PRIMARY KEY,
  file TEXT NOT NULL REFERENCES files(path) ON DELETE CASCADE,
  containing_symbol TEXT REFERENCES symbols(id) ON DELETE SET NULL,
  scope_key TEXT NOT NULL,
  spelling TEXT NOT NULL,
  lookup_name TEXT NOT NULL,
  ref_kind TEXT NOT NULL,
  start_byte INTEGER NOT NULL,
  end_byte INTEGER NOT NULL,
  line INTEGER NOT NULL,
  col INTEGER NOT NULL,
  receiver TEXT,
  hint_json TEXT NOT NULL,
  UNIQUE(file, start_byte, end_byte, ref_kind),
  CHECK (0 <= start_byte AND start_byte < end_byte)
);
CREATE INDEX uses_name ON uses(lookup_name);
CREATE INDEX uses_container ON uses(containing_symbol);

CREATE TABLE bindings (
  use_id INTEGER PRIMARY KEY REFERENCES uses(use_id) ON DELETE CASCADE,
  target_id TEXT NOT NULL REFERENCES symbols(id) ON DELETE CASCADE,
  resolution TEXT NOT NULL CHECK(resolution IN ('exact', 'scoped'))
);
CREATE INDEX bindings_target ON bindings(target_id);

CREATE TABLE scopes (
  file TEXT NOT NULL REFERENCES files(path) ON DELETE CASCADE,
  scope_key TEXT NOT NULL,
  parent_scope_key TEXT,
  facts_json TEXT NOT NULL,
  PRIMARY KEY(file, scope_key)
); -- language-neutral owned lexical bindings, import aliases/module specifiers,
   -- declarations, type hints, assignment spans and shadowing facts

CREATE TABLE diagnostics (
  file TEXT NOT NULL,
  code TEXT NOT NULL,
  detail TEXT NOT NULL,
  start_byte INTEGER
);
";

/// Converts a [`rivet_core::walk::FileEntry`] nanosecond timestamp to the
/// signed 64-bit integer SQLite stores.
///
/// `FileEntry::mtime_ns` is `i128` so pre-epoch and far-future times are
/// representable in memory, but SQLite `INTEGER` is signed 64-bit. Values
/// outside `i64::MIN..=i64::MAX` are clamped to the nearest bound. Nanoseconds
/// since the Unix epoch fit in `i64` until the year 2262, well past any real
/// file timestamp, so the clamp only affects synthetic values.
pub fn clamp_mtime_ns(mtime_ns: i128) -> i64 {
    mtime_ns.clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

/// One row of the `files` table.
///
/// `source` holds the exact bytes parsed for an `Ok` file and is `None` for a
/// skipped file. `size` is the on-disk byte length; source reads are bounded by
/// the caller, so it always fits the `i64` SQLite stores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRow {
    /// Repo-relative `/`-separated path (the `files` primary key).
    pub path: String,
    /// Detected language, or `None` when unknown.
    pub language: Option<String>,
    /// Modification time as nanoseconds since the Unix epoch.
    pub mtime_ns: i64,
    /// File size in bytes.
    pub size: u64,
    /// Canonical `blake3:` content hash, or `None` when content was not read.
    pub content_hash: Option<String>,
    /// Exact parsed bytes for `Ok` files; `None` for skipped files.
    pub source: Option<Vec<u8>>,
    /// Parse/skip classification persisted in `files.parse_status`.
    pub parse_status: ParseStatus,
}

/// The four fingerprint inputs that identify an indexed snapshot's
/// configuration (ARCHITECTURE "Refresh and invalidation").
///
/// They are stored in `meta` and fed to [`snapshot_digest`]. None of them
/// includes wall time or transaction counters, so re-indexing identical
/// content/configuration yields the same digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    /// Schema/index format version, stored as `meta.index_format_version`.
    pub index_format_version: String,
    /// Effective configuration fingerprint, stored as
    /// `meta.effective_config_fingerprint`.
    pub effective_config: String,
    /// Extractor/grammar fingerprint, stored as
    /// `meta.extractor_fingerprint`.
    pub extractor: String,
    /// Resolver fingerprint, stored as `meta.resolver_fingerprint`.
    pub resolver: String,
}

/// The complete inventory to publish atomically.
pub struct InventoryInput {
    /// Fingerprints written to `meta` alongside the inventory.
    pub fingerprint: Fingerprint,
    /// Every currently eligible file. Rows not listed here are deleted.
    pub files: Vec<FileRow>,
}

/// Counts and digest describing one [`Store::publish_inventory`] call.
///
/// `updated + unchanged == files.len()` of the published input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishReport {
    /// New rows, or rows whose content hash, parse status, or language changed.
    pub updated: u64,
    /// Rows present before and after with none of those three values changed.
    /// Metadata-only (mtime/size) changes still count as unchanged.
    pub unchanged: u64,
    /// Previously present rows removed by this publication.
    pub deleted: u64,
    /// The canonical `blake3:` snapshot digest (see [`snapshot_digest`]).
    pub digest: String,
}

/// Computes the deterministic snapshot digest of a fingerprint and inventory.
///
/// The digest is BLAKE3 over a canonical, length-prefixed byte sequence
/// (ARCHITECTURE "Refresh and invalidation"): the index-format version,
/// effective-config, extractor, and resolver fingerprints, then each file in
/// path-byte order with its path, language or empty, content hash or empty,
/// and parse/skip status. Every field is prefixed with its byte length as an
/// 8-byte little-endian integer so distinct field boundaries cannot collide.
/// mtime, size, absolute root, row IDs, and wall time are excluded, so an
/// mtime-only edit leaves the digest unchanged. `sorted_files` is sorted here
/// as well, so the result does not depend on caller input order either.
pub fn snapshot_digest(fingerprint: &Fingerprint, sorted_files: &[FileRow]) -> String {
    let mut files: Vec<&FileRow> = sorted_files.iter().collect();
    files.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));

    let mut hasher = blake3::Hasher::new();
    push_field(&mut hasher, fingerprint.index_format_version.as_bytes());
    push_field(&mut hasher, fingerprint.effective_config.as_bytes());
    push_field(&mut hasher, fingerprint.extractor.as_bytes());
    push_field(&mut hasher, fingerprint.resolver.as_bytes());
    for file in files {
        push_field(&mut hasher, file.path.as_bytes());
        push_field(
            &mut hasher,
            file.language.as_deref().unwrap_or("").as_bytes(),
        );
        push_field(
            &mut hasher,
            file.content_hash.as_deref().unwrap_or("").as_bytes(),
        );
        push_field(&mut hasher, file.parse_status.as_str().as_bytes());
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

/// Feeds one length-prefixed field into `hasher`.
fn push_field(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value);
}

/// Why a store operation failed.
#[derive(Debug)]
pub enum Error {
    /// The `.rivet/` destination or `index.db` failed pre-write validation
    /// (spec §27).
    InvalidDestination {
        /// The rejected path.
        path: PathBuf,
        /// A human-readable reason naming the problem.
        reason: String,
    },
    /// An existing database uses a different `meta.index_format_version`.
    IncompatibleIndexFormat {
        /// The version string found in the database.
        found: String,
    },
    /// A required connection setting could not be established.
    Configuration {
        /// What failed.
        detail: String,
    },
    /// An underlying SQLite error.
    Sqlite(rusqlite::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidDestination { path, reason } => {
                write!(f, "invalid cache destination {}: {reason}", path.display())
            }
            Error::IncompatibleIndexFormat { found } => write!(
                f,
                "incompatible index format version {found:?}; expected {INDEX_FORMAT_VERSION:?}"
            ),
            Error::Configuration { detail } => write!(f, "cannot configure store: {detail}"),
            Error::Sqlite(source) => write!(f, "sqlite error: {source}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Sqlite(source) => Some(source),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(source: rusqlite::Error) -> Error {
        Error::Sqlite(source)
    }
}

/// An open index cache.
#[derive(Debug)]
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens the index cache in `rivet_dir`.
    ///
    /// The destination is validated before any write: `rivet_dir` must exist
    /// and be a real directory, and `index.db` (if present) must be a regular
    /// file, never a symlink. The connection enables foreign keys and a five
    /// second busy timeout. An empty database receives the version-1 schema; an
    /// existing database with a different `meta.index_format_version` is
    /// refused without modifying it. WAL journaling is enabled only after the
    /// format is known to be compatible.
    pub fn open(rivet_dir: &Path) -> Result<Store, Error> {
        validate_destination(rivet_dir)?;
        let path = rivet_dir.join(INDEX_DB_FILE);
        let conn = Connection::open(&path)?;
        configure_connection(&conn)?;
        initialize(&conn)?;
        // Set WAL only after the format is accepted so a refused database is
        // left byte-for-byte unchanged (spec §27).
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Ok(Store { conn })
    }

    /// Opens a fresh in-memory index cache with the version-1 schema.
    ///
    /// Intended for tests: no filesystem destination is touched and WAL is not
    /// applicable.
    pub fn open_in_memory() -> Result<Store, Error> {
        let conn = Connection::open_in_memory()?;
        configure_connection(&conn)?;
        initialize(&conn)?;
        Ok(Store { conn })
    }

    /// Inserts or replaces one `files` row in a transaction.
    pub fn upsert_file(&self, file: &FileRow) -> Result<(), Error> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO files
                 (path, language, mtime_ns, size, content_hash, source, parse_status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(path) DO UPDATE SET
                 language = excluded.language,
                 mtime_ns = excluded.mtime_ns,
                 size = excluded.size,
                 content_hash = excluded.content_hash,
                 source = excluded.source,
                 parse_status = excluded.parse_status",
            params![
                file.path,
                file.language,
                file.mtime_ns,
                file.size as i64,
                file.content_hash,
                file.source,
                file.parse_status.as_str(),
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Atomically replaces the complete `files` inventory and writes the four
    /// fingerprint values plus the snapshot digest into `meta`.
    ///
    /// The whole publication runs in one `BEGIN IMMEDIATE` transaction: rows
    /// absent from `input.files` are deleted, present rows are inserted or
    /// updated, and the `meta` values are written before commit. Any failure
    /// rolls the transaction back so the previous complete inventory and meta
    /// stay untouched (spec §12.3; ARCHITECTURE "Refresh and invalidation" and
    /// "Concurrency and source consistency"). A duplicate new path aborts the
    /// transaction rather than collapsing two logically distinct inputs.
    pub fn publish_inventory(&mut self, input: InventoryInput) -> Result<PublishReport, Error> {
        let InventoryInput { fingerprint, files } = input;

        // Sort by path bytes so writes and the digest are deterministic and
        // independent of the caller's input order.
        let mut sorted = files;
        sorted.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
        let digest = snapshot_digest(&fingerprint, &sorted);

        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;

        let previous = load_previous_inventory(&tx)?;
        let incoming: HashSet<&str> = sorted.iter().map(|file| file.path.as_str()).collect();
        let deleted = previous
            .keys()
            .filter(|path| !incoming.contains(path.as_str()))
            .count() as u64;

        // Delete rows that left the eligible set before writing the new ones.
        {
            let mut delete = tx.prepare("DELETE FROM files WHERE path = ?1")?;
            for path in previous.keys() {
                if !incoming.contains(path.as_str()) {
                    delete.execute(params![path])?;
                }
            }
        }

        let mut updated = 0_u64;
        let mut unchanged = 0_u64;
        {
            // A plain INSERT, not an upsert: a duplicate path in the input must
            // abort the whole transaction rather than silently collapse.
            let mut insert = tx.prepare(
                "INSERT INTO files
                     (path, language, mtime_ns, size, content_hash, source, parse_status)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            let mut update = tx.prepare(
                "UPDATE files SET
                     language = ?2,
                     mtime_ns = ?3,
                     size = ?4,
                     content_hash = ?5,
                     source = ?6,
                     parse_status = ?7
                 WHERE path = ?1",
            )?;
            for file in &sorted {
                match previous.get(&file.path) {
                    Some((content_hash, parse_status, language))
                        if *content_hash == file.content_hash
                            && *parse_status == file.parse_status
                            && *language == file.language =>
                    {
                        unchanged += 1;
                        write_file_row(&mut update, file)?;
                    }
                    Some(_) => {
                        updated += 1;
                        write_file_row(&mut update, file)?;
                    }
                    None => {
                        updated += 1;
                        write_file_row(&mut insert, file)?;
                    }
                }
            }
        }

        write_fingerprint(&tx, &fingerprint, &digest)?;
        tx.commit()?;

        Ok(PublishReport {
            updated,
            unchanged,
            deleted,
            digest,
        })
    }

    /// Returns the `files` row for `path`, if any.
    pub fn get_file(&self, path: &str) -> Result<Option<FileRow>, Error> {
        let tx = self.conn.unchecked_transaction()?;
        let row = tx
            .query_row(
                "SELECT path, language, mtime_ns, size, content_hash, source, parse_status
                 FROM files WHERE path = ?1",
                params![path],
                file_row_from,
            )
            .optional()?;
        tx.commit()?;
        Ok(row)
    }

    /// Returns every `files` row ordered by path bytes.
    pub fn list_files(&self) -> Result<Vec<FileRow>, Error> {
        let tx = self.conn.unchecked_transaction()?;
        let rows = {
            let mut stmt = tx.prepare(
                "SELECT path, language, mtime_ns, size, content_hash, source, parse_status
                 FROM files ORDER BY path COLLATE BINARY",
            )?;
            let rows = stmt.query_map([], file_row_from)?;
            rows.collect::<rusqlite::Result<Vec<FileRow>>>()?
        };
        tx.commit()?;
        Ok(rows)
    }

    /// Deletes the `files` row for `path`; dependent facts cascade.
    pub fn delete_file(&self, path: &str) -> Result<(), Error> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM files WHERE path = ?1", params![path])?;
        tx.commit()?;
        Ok(())
    }

    /// Returns the `meta` value for `key`, if present.
    pub fn get_meta(&self, key: &str) -> Result<Option<String>, Error> {
        let tx = self.conn.unchecked_transaction()?;
        let value = tx
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()?;
        tx.commit()?;
        Ok(value)
    }

    /// Inserts or replaces the `meta` value for `key` in a transaction.
    pub fn set_meta(&self, key: &str, value: &str) -> Result<(), Error> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Reports whether `PRAGMA foreign_keys` is enabled on this connection.
    pub fn pragma_foreign_keys(&self) -> Result<bool, Error> {
        pragma_foreign_keys(&self.conn)
    }
}

/// Validates `rivet_dir` and `rivet_dir/index.db` before any write (spec §27).
fn validate_destination(rivet_dir: &Path) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(rivet_dir).map_err(|source| Error::InvalidDestination {
        path: rivet_dir.to_path_buf(),
        reason: format!("cannot inspect destination: {source}"),
    })?;
    if metadata.file_type().is_symlink() {
        return Err(Error::InvalidDestination {
            path: rivet_dir.to_path_buf(),
            reason: "destination is a symbolic link".to_string(),
        });
    }
    if !metadata.is_dir() {
        return Err(Error::InvalidDestination {
            path: rivet_dir.to_path_buf(),
            reason: "destination is not a directory".to_string(),
        });
    }

    let db_path = rivet_dir.join(INDEX_DB_FILE);
    match fs::symlink_metadata(&db_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(Error::InvalidDestination {
            path: db_path,
            reason: "index database is a symbolic link".to_string(),
        }),
        Ok(metadata) if !metadata.is_file() => Err(Error::InvalidDestination {
            path: db_path,
            reason: "index database is not a regular file".to_string(),
        }),
        Ok(_) => Ok(()),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(Error::InvalidDestination {
            path: db_path,
            reason: format!("cannot inspect index database: {source}"),
        }),
    }
}

/// Applies the required per-connection pragmas and verifies foreign keys.
fn configure_connection(conn: &Connection) -> Result<(), Error> {
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "busy_timeout", 5000_i64)?;
    if !pragma_foreign_keys(conn)? {
        return Err(Error::Configuration {
            detail: "PRAGMA foreign_keys did not remain ON".to_string(),
        });
    }
    Ok(())
}

/// Returns whether `PRAGMA foreign_keys` is on for `conn`.
fn pragma_foreign_keys(conn: &Connection) -> Result<bool, Error> {
    let enabled: i64 = conn.pragma_query_value(None, "foreign_keys", |row| row.get(0))?;
    Ok(enabled == 1)
}

/// Creates the schema when empty, or validates an existing format version.
fn initialize(conn: &Connection) -> Result<(), Error> {
    match read_existing_version(conn)? {
        None => create_schema(conn),
        Some(found) if found == INDEX_FORMAT_VERSION => Ok(()),
        Some(found) => Err(Error::IncompatibleIndexFormat { found }),
    }
}

/// Reads `meta.index_format_version`, or `None` when no `meta` table exists.
///
/// A database without a `meta` table is treated as empty. A `meta` table
/// without the version row yields `Some("")`, which is refused as incompatible
/// rather than silently recreated.
fn read_existing_version(conn: &Connection) -> Result<Option<String>, Error> {
    let has_meta: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'meta'",
        [],
        |row| row.get(0),
    )?;
    if has_meta == 0 {
        return Ok(None);
    }
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'index_format_version'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(Some(value.unwrap_or_default()))
}

/// Creates the full version-1 schema and records the format version atomically.
fn create_schema(conn: &Connection) -> Result<(), Error> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(SCHEMA_SQL)?;
    tx.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)",
        params!["index_format_version", INDEX_FORMAT_VERSION],
    )?;
    tx.commit()?;
    Ok(())
}

/// Maps a `files` row to a [`FileRow`].
fn file_row_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileRow> {
    let path: String = row.get("path")?;
    let language: Option<String> = row.get("language")?;
    let mtime_ns: i64 = row.get("mtime_ns")?;
    let size: i64 = row.get("size")?;
    let content_hash: Option<String> = row.get("content_hash")?;
    let source: Option<Vec<u8>> = row.get("source")?;
    let status: String = row.get("parse_status")?;
    let parse_status = status.parse::<ParseStatus>().map_err(|error| {
        // `parse_status` is the seventh selected column (index 6).
        rusqlite::Error::FromSqlConversionFailure(6, Type::Text, Box::new(error))
    })?;
    Ok(FileRow {
        path,
        language,
        mtime_ns,
        size: size as u64,
        content_hash,
        source,
        parse_status,
    })
}

/// Executes one parameterized `files` write (INSERT or UPDATE) for `file`.
///
/// Both statements use the same `?1..?7` column order, so one binding list
/// serves each.
fn write_file_row(stmt: &mut rusqlite::Statement<'_>, file: &FileRow) -> Result<(), Error> {
    stmt.execute(params![
        file.path,
        file.language,
        file.mtime_ns,
        file.size as i64,
        file.content_hash,
        file.source,
        file.parse_status.as_str(),
    ])?;
    Ok(())
}

/// Comparison fields of a previously published `files` row: content hash,
/// parse status, and language. Used to classify updated vs unchanged.
type PreviousFile = (Option<String>, ParseStatus, Option<String>);

/// Loads the current inventory keyed by path, with the comparison fields only.
fn load_previous_inventory(
    tx: &rusqlite::Transaction<'_>,
) -> Result<HashMap<String, PreviousFile>, Error> {
    let raw: Vec<(String, Option<String>, String, Option<String>)> = {
        let mut stmt =
            tx.prepare("SELECT path, content_hash, parse_status, language FROM files")?;
        let rows = stmt.query_map(
            [],
            |row| -> rusqlite::Result<(String, Option<String>, String, Option<String>)> {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            },
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };

    let mut previous = HashMap::with_capacity(raw.len());
    for (path, content_hash, status, language) in raw {
        let parse_status = status.parse::<ParseStatus>().map_err(|error| {
            // `parse_status` is the third selected column (index 2).
            rusqlite::Error::FromSqlConversionFailure(2, Type::Text, Box::new(error))
        })?;
        previous.insert(path, (content_hash, parse_status, language));
    }
    Ok(previous)
}

/// Writes the four fingerprints and the snapshot digest into `meta`.
fn write_fingerprint(
    tx: &rusqlite::Transaction<'_>,
    fingerprint: &Fingerprint,
    digest: &str,
) -> Result<(), Error> {
    let entries = [
        (
            "index_format_version",
            fingerprint.index_format_version.as_str(),
        ),
        (
            "effective_config_fingerprint",
            fingerprint.effective_config.as_str(),
        ),
        ("extractor_fingerprint", fingerprint.extractor.as_str()),
        ("resolver_fingerprint", fingerprint.resolver.as_str()),
        ("snapshot_digest", digest),
    ];
    let mut upsert = tx.prepare(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )?;
    for (key, value) in entries {
        upsert.execute(params![key, value])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        Error, FileRow, Fingerprint, INDEX_FORMAT_VERSION, InventoryInput, PublishReport, Store,
        clamp_mtime_ns, snapshot_digest,
    };
    use rivet_core::{ParseStatus, content_hash};
    use rusqlite::params;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A uniquely named temporary directory removed when dropped.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(label: &str) -> TempDir {
            let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir().join(format!(
                "rivet-store-{label}-{}-{nanos}-{unique}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create temporary directory");
            TempDir { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn sample_file(path: &str) -> FileRow {
        FileRow {
            path: path.to_string(),
            language: Some("php".to_string()),
            mtime_ns: 1_700_000_000_000_000_000,
            size: 5,
            content_hash: Some(content_hash(b"<?php")),
            source: Some(b"<?php".to_vec()),
            parse_status: ParseStatus::Ok,
        }
    }

    fn file_with(path: &str, content: &[u8]) -> FileRow {
        FileRow {
            path: path.to_string(),
            language: Some("php".to_string()),
            mtime_ns: 1_700_000_000_000_000_000,
            size: content.len() as u64,
            content_hash: Some(content_hash(content)),
            source: Some(content.to_vec()),
            parse_status: ParseStatus::Ok,
        }
    }

    fn sample_fingerprint() -> Fingerprint {
        Fingerprint {
            index_format_version: INDEX_FORMAT_VERSION.to_string(),
            effective_config: "cfg-v1".to_string(),
            extractor: "extractor-v1".to_string(),
            resolver: "resolver-v1".to_string(),
        }
    }

    fn inventory(fingerprint: Fingerprint, files: Vec<FileRow>) -> InventoryInput {
        InventoryInput { fingerprint, files }
    }

    fn publish(store: &mut Store, files: Vec<FileRow>) -> PublishReport {
        store
            .publish_inventory(inventory(sample_fingerprint(), files))
            .expect("publish succeeds")
    }

    #[test]
    fn snapshot_digest_is_order_independent_and_excludes_metadata() {
        let fingerprint = sample_fingerprint();
        let forward = vec![sample_file("a.php"), sample_file("b.php")];
        let mut reversed = forward.clone();
        reversed.reverse();
        assert_eq!(
            snapshot_digest(&fingerprint, &forward),
            snapshot_digest(&fingerprint, &reversed)
        );

        // mtime and size are not part of the digest.
        let mut touched = forward.clone();
        touched[0].mtime_ns += 99;
        touched[0].size = 12345;
        assert_eq!(
            snapshot_digest(&fingerprint, &forward),
            snapshot_digest(&fingerprint, &touched)
        );
    }

    #[test]
    fn republish_same_inventory_is_stable() {
        let mut store = Store::open_in_memory().unwrap();
        let files = vec![sample_file("a.php"), sample_file("b.php")];
        let first = publish(&mut store, files.clone());
        assert_eq!((first.updated, first.unchanged, first.deleted), (2, 0, 0));

        let second = publish(&mut store, files);
        assert_eq!(
            (second.updated, second.unchanged, second.deleted),
            (0, 2, 0)
        );
        assert_eq!(second.digest, first.digest);
    }

    #[test]
    fn publish_is_order_independent() {
        let mut store = Store::open_in_memory().unwrap();
        let first = publish(
            &mut store,
            vec![
                sample_file("a.php"),
                sample_file("b.php"),
                sample_file("c.php"),
            ],
        );
        let second = publish(
            &mut store,
            vec![
                sample_file("c.php"),
                sample_file("b.php"),
                sample_file("a.php"),
            ],
        );
        assert_eq!(second.digest, first.digest);
        assert_eq!(
            (second.updated, second.unchanged, second.deleted),
            (0, 3, 0)
        );
    }

    #[test]
    fn changed_content_hash_updates_one_and_changes_digest() {
        let mut store = Store::open_in_memory().unwrap();
        let base = vec![sample_file("a.php"), sample_file("b.php")];
        let before = publish(&mut store, base.clone());

        let mut changed = base;
        changed[0] = file_with("a.php", b"<?php changed");
        let after = publish(&mut store, changed);
        assert_eq!((after.updated, after.unchanged, after.deleted), (1, 1, 0));
        assert_ne!(after.digest, before.digest);
    }

    #[test]
    fn mtime_only_change_is_unchanged_and_same_digest() {
        let mut store = Store::open_in_memory().unwrap();
        let base = vec![sample_file("a.php"), sample_file("b.php")];
        let before = publish(&mut store, base.clone());

        let mut touched = base.clone();
        touched[0].mtime_ns += 12_345;
        let after = publish(&mut store, touched);
        assert_eq!((after.updated, after.unchanged, after.deleted), (0, 2, 0));
        assert_eq!(after.digest, before.digest);

        // Metadata-only updates are still persisted.
        assert_eq!(
            store.get_file("a.php").unwrap().unwrap().mtime_ns,
            base[0].mtime_ns + 12_345
        );
    }

    #[test]
    fn removed_file_counts_deleted_and_changes_digest() {
        let mut store = Store::open_in_memory().unwrap();
        let before = publish(&mut store, vec![sample_file("a.php"), sample_file("b.php")]);

        let after = publish(&mut store, vec![sample_file("a.php")]);
        assert_eq!((after.updated, after.unchanged, after.deleted), (0, 1, 1));
        assert_ne!(after.digest, before.digest);
        assert!(store.get_file("b.php").unwrap().is_none());
    }

    #[test]
    fn effective_config_change_changes_digest() {
        let mut store = Store::open_in_memory().unwrap();
        let files = vec![sample_file("a.php"), sample_file("b.php")];
        let before = publish(&mut store, files.clone());

        let mut fingerprint = sample_fingerprint();
        fingerprint.effective_config = "cfg-v2".to_string();
        let after = store
            .publish_inventory(inventory(fingerprint, files))
            .unwrap();
        assert_eq!((after.updated, after.unchanged, after.deleted), (0, 2, 0));
        assert_ne!(after.digest, before.digest);
    }

    #[test]
    fn failed_publish_rolls_back_inventory_and_digest() {
        let mut store = Store::open_in_memory().unwrap();
        let before = publish(&mut store, vec![sample_file("a.php")]);
        let digest_before = store.get_meta("snapshot_digest").unwrap();
        assert_eq!(digest_before.as_deref(), Some(before.digest.as_str()));

        // Two rows share "b.php"; the second plain INSERT hits the PRIMARY KEY
        // partway through, so the whole transaction must roll back.
        let duplicate = vec![
            sample_file("b.php"),
            sample_file("c.php"),
            sample_file("b.php"),
        ];
        let error = store
            .publish_inventory(inventory(sample_fingerprint(), duplicate))
            .unwrap_err();
        assert!(matches!(error, Error::Sqlite(_)), "{error:?}");

        assert_eq!(store.get_meta("snapshot_digest").unwrap(), digest_before);
        assert_eq!(store.list_files().unwrap(), vec![sample_file("a.php")]);
    }

    #[test]
    fn list_files_after_publish_is_path_sorted() {
        let mut store = Store::open_in_memory().unwrap();
        let input = vec![
            sample_file("z.php"),
            sample_file("B.php"),
            sample_file("aa.php"),
            sample_file("a.php"),
        ];
        publish(&mut store, input.clone());

        let mut expected = input;
        expected.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
        assert_eq!(store.list_files().unwrap(), expected);
    }

    #[test]
    fn schema_creation_sets_format_version_and_foreign_keys() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(
            store.get_meta("index_format_version").unwrap().as_deref(),
            Some(INDEX_FORMAT_VERSION)
        );
        assert!(store.pragma_foreign_keys().unwrap());

        // Every table from ARCHITECTURE "Minimal logical schema" exists.
        let mut names: Vec<String> = store
            .conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<String>>>()
            .unwrap();
        names.retain(|name| !name.starts_with("sqlite_"));
        assert_eq!(
            names,
            vec![
                "bindings",
                "diagnostics",
                "files",
                "meta",
                "scopes",
                "symbols",
                "uses"
            ]
        );
    }

    #[test]
    fn file_row_round_trip_preserves_bytes_and_hash() {
        let store = Store::open_in_memory().unwrap();
        // NUL, 0xff, CRLF, and a UTF-8 BOM: bytes a text pipeline must not
        // rewrite.
        let source: Vec<u8> = vec![0x00, 0xff, b'\r', b'\n', 0xef, 0xbb, 0xbf];
        let hash = content_hash(&source);
        let row = FileRow {
            path: "src/a.php".to_string(),
            language: Some("php".to_string()),
            mtime_ns: 1_700_000_000_000_000_000,
            size: source.len() as u64,
            content_hash: Some(hash.clone()),
            source: Some(source.clone()),
            parse_status: ParseStatus::Binary,
        };

        store.upsert_file(&row).unwrap();
        let got = store.get_file("src/a.php").unwrap().expect("row exists");
        assert_eq!(got, row);
        assert_eq!(got.source.as_deref(), Some(source.as_slice()));
        assert_eq!(got.content_hash.as_deref(), Some(hash.as_str()));
        assert_eq!(content_hash(got.source.as_deref().unwrap()), hash);
    }

    #[test]
    fn upsert_replaces_existing_row() {
        let store = Store::open_in_memory().unwrap();
        store.upsert_file(&sample_file("a.php")).unwrap();
        let mut updated = sample_file("a.php");
        updated.parse_status = ParseStatus::ParseError;
        updated.source = None;
        updated.content_hash = None;
        store.upsert_file(&updated).unwrap();

        assert_eq!(store.list_files().unwrap(), vec![updated]);
    }

    #[test]
    fn list_files_is_byte_ordered() {
        let store = Store::open_in_memory().unwrap();
        for path in ["z.php", "a.php", "B.php", "aa.php"] {
            store.upsert_file(&sample_file(path)).unwrap();
        }

        let paths: Vec<String> = store
            .list_files()
            .unwrap()
            .into_iter()
            .map(|file| file.path)
            .collect();
        // BINARY byte order: 'B' < 'a' < 'aa' < 'z'.
        assert_eq!(paths, vec!["B.php", "a.php", "aa.php", "z.php"]);
        assert!(store.get_file("missing.php").unwrap().is_none());
    }

    #[test]
    fn delete_file_cascades_to_symbols() {
        let store = Store::open_in_memory().unwrap();
        store.upsert_file(&sample_file("a.php")).unwrap();
        store
            .conn
            .execute(
                "INSERT INTO symbols
                     (id, file, name, lookup_name, qualified_name, kind,
                      start_byte, end_byte, start_line, end_line)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params!["sym1", "a.php", "run", "run", "run", "function", 0, 5, 1, 1],
            )
            .unwrap();

        let count = |store: &Store| -> i64 {
            store
                .conn
                .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
                .unwrap()
        };
        assert_eq!(count(&store), 1);

        store.delete_file("a.php").unwrap();
        assert!(store.get_file("a.php").unwrap().is_none());
        assert_eq!(count(&store), 0, "symbols must cascade with the file");
    }

    #[test]
    fn unknown_format_version_is_rejected_and_file_unchanged() {
        let temp = TempDir::new("format");
        let rivet_dir = temp.path();
        {
            let store = Store::open(rivet_dir).unwrap();
            assert_eq!(
                store.get_meta("index_format_version").unwrap().as_deref(),
                Some(INDEX_FORMAT_VERSION)
            );
            store.set_meta("index_format_version", "99").unwrap();
        }

        let db_path = rivet_dir.join("index.db");
        let before = fs::read(&db_path).unwrap();

        match Store::open(rivet_dir).unwrap_err() {
            Error::IncompatibleIndexFormat { found } => assert_eq!(found, "99"),
            other => panic!("expected IncompatibleIndexFormat, got {other:?}"),
        }

        let after = fs::read(&db_path).unwrap();
        assert_eq!(
            content_hash(&before),
            content_hash(&after),
            "a refused database must not be modified"
        );
    }

    #[test]
    fn clamp_mtime_ns_bounds_i128_to_i64() {
        assert_eq!(clamp_mtime_ns(0), 0);
        assert_eq!(clamp_mtime_ns(i64::MAX as i128), i64::MAX);
        assert_eq!(clamp_mtime_ns(i64::MAX as i128 + 1), i64::MAX);
        assert_eq!(clamp_mtime_ns(i64::MIN as i128), i64::MIN);
        assert_eq!(clamp_mtime_ns(i64::MIN as i128 - 1), i64::MIN);
    }

    #[test]
    fn missing_destination_is_rejected() {
        let temp = TempDir::new("missing");
        let error = Store::open(&temp.path().join("absent")).unwrap_err();
        assert!(matches!(error, Error::InvalidDestination { .. }));
        assert!(error.to_string().contains("absent"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_rivet_dir_is_rejected() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new("symlink-dir");
        let real = temp.path().join("real-rivet");
        fs::create_dir_all(&real).unwrap();
        let link = temp.path().join("linked-rivet");
        symlink(&real, &link).unwrap();

        let error = Store::open(&link).unwrap_err();
        assert!(matches!(error, Error::InvalidDestination { .. }));
        assert!(error.to_string().contains("linked-rivet"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_index_db_is_rejected() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new("symlink-db");
        let rivet_dir = temp.path().join("rivet");
        fs::create_dir_all(&rivet_dir).unwrap();
        let target = temp.path().join("elsewhere.db");
        fs::write(&target, b"").unwrap();
        symlink(&target, rivet_dir.join("index.db")).unwrap();

        let error = Store::open(&rivet_dir).unwrap_err();
        assert!(matches!(error, Error::InvalidDestination { .. }));
        assert!(error.to_string().contains("index.db"), "{error}");
    }
}
