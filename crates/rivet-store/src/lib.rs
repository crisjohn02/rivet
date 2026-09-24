//! SQLite schema and minimal store operations (spec §27; ARCHITECTURE
//! "Minimal logical schema" and "Concurrency and source consistency").
//!
//! This crate owns the disposable index cache. [`Store::open`] validates the
//! `.rivet/` destination before touching the database, opens `index.db` with
//! foreign keys enabled, and either creates the current schema, rebuilds a
//! supported older format, or refuses a database whose
//! `meta.index_format_version` is otherwise different. Minimal row operations
//! run inside explicit transactions. [`Store::publish_inventory`] atomically
//! replaces the complete file inventory and writes the configuration
//! fingerprints plus the deterministic snapshot digest. [`Store::begin_write`]
//! takes the writer lock for a whole refresh ([`WriteTxn`]), and
//! [`Store::begin_snapshot_read`] pins one committed snapshot for a query; every
//! read method joins whichever transaction is open. Walking, parsing, and
//! resolution belong to the CLI and index crates.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rivet_core::{ParseStatus, RefKind, Resolution, SymbolKind};
use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

/// The database file name inside `.rivet/`.
const INDEX_DB_FILE: &str = "index.db";

/// The writer-lock busy timeout from ARCHITECTURE "Refresh and invalidation"
/// (`begin immediate transaction (busy timeout: 5 seconds)`).
pub const WRITER_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// [`WRITER_BUSY_TIMEOUT`] in the milliseconds `PRAGMA busy_timeout` takes; the
/// default for every statement on a connection.
const DEFAULT_BUSY_TIMEOUT_MS: i64 = 5_000;

/// The current value of `meta.index_format_version`.
///
/// History, newest first (RELEASING "Index format version": it increments when
/// the SQLite schema changes; supported older formats rebuild, unknown ones
/// are refused):
///
/// - **2** — LR2: the `receiver_classes` table.
/// - **1** — T08: the first schema.
pub const INDEX_FORMAT_VERSION: &str = "2";

/// Older `meta.index_format_version` values this build recognizes. A writable
/// open drops and recreates such a disposable cache, because nothing in it
/// can be migrated more cheaply than it can be rebuilt; a read-only
/// (`--no-refresh`) open refuses it like any other incompatible format.
const SUPPORTED_OLDER_FORMAT_VERSIONS: &[&str] = &["1"];

/// The complete current schema from ARCHITECTURE "Minimal logical schema".
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
CREATE INDEX IF NOT EXISTS symbols_parent ON symbols(parent_id);

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

CREATE TABLE receiver_classes (
  use_id INTEGER PRIMARY KEY REFERENCES uses(use_id) ON DELETE CASCADE,
  class_qname TEXT NOT NULL,
  class_id TEXT REFERENCES symbols(id) ON DELETE CASCADE
); -- LR2: the receiver class a receiver rule determined for an unbound
   -- member/scoped use; class_id is set only for an indexed class-like
CREATE INDEX receiver_classes_class ON receiver_classes(class_id);

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

/// The index on `symbols.parent_id` (PF1).
///
/// `symbols.parent_id REFERENCES symbols(id) ON DELETE SET NULL` makes every
/// symbol delete look up that symbol's children. Without an index whose
/// leading column is `parent_id` each lookup scans the whole `symbols` table,
/// so replacing all N symbols costs O(N²) row checks. Every other foreign key
/// already has an index on its referencing column.
///
/// The index is part of [`SCHEMA_SQL`] for a new database and is added to an
/// existing version-1 database when it opens ([`ensure_additive_indexes`]).
/// It changes no stored fact and no query result, so it needs no
/// index-format version bump: an older build reading a database that has it
/// behaves exactly as before, and a database without it is still valid.
const PARENT_INDEX_NAME: &str = "symbols_parent";

/// Creates [`PARENT_INDEX_NAME`] when missing.
const PARENT_INDEX_SQL: &str = "CREATE INDEX IF NOT EXISTS symbols_parent ON symbols(parent_id)";

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

/// The column list shared by every `symbols` read, in schema order.
const SYMBOL_COLUMNS: &str = "id, file, name, lookup_name, qualified_name, kind, parent_id, \
     start_byte, end_byte, start_line, end_line, signature, doc_comment";

/// One row of the `symbols` table.
///
/// `kind` is the parsed [`SymbolKind`]; `parent_id` is the canonical ID of the
/// enclosing container declaration, or `None` for a top-level definition.
/// `signature` and `doc_comment` are filled by a later task (T14) and are
/// `None` in T12.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolRow {
    /// Canonical symbol ID (`<file>#<qualified name>[#<ordinal>]`).
    pub id: String,
    /// Repo-relative `/`-separated file path (the `files` foreign key).
    pub file: String,
    /// Short declared name.
    pub name: String,
    /// Case-folded name used for short-name lookup.
    pub lookup_name: String,
    /// Language-native qualified name.
    pub qualified_name: String,
    /// Declaration kind.
    pub kind: SymbolKind,
    /// Canonical ID of the parent declaration, if any.
    pub parent_id: Option<String>,
    /// Zero-based inclusive declaration start byte.
    pub start_byte: u32,
    /// Zero-based exclusive declaration end byte.
    pub end_byte: u32,
    /// One-based line containing `start_byte`.
    pub start_line: u32,
    /// One-based inclusive line containing `end_byte - 1`.
    pub end_line: u32,
    /// Collapsed signature (T14), or `None`.
    pub signature: Option<String>,
    /// Attached doc comment (T14), or `None`.
    pub doc_comment: Option<String>,
}

/// The column list shared by every `uses` read, in schema order.
const USE_COLUMNS: &str = "use_id, file, containing_symbol, scope_key, spelling, lookup_name, \
     ref_kind, start_byte, end_byte, line, col, receiver, hint_json";

/// One row of the `uses` table.
///
/// `use_id` is the SQLite-assigned surrogate key: it is `None` for a newly
/// extracted use and `Some` for a row read back from the store. A refresh that
/// reuses an unchanged file's uses carries the stored IDs forward so a
/// no-edit re-index keeps byte-identical rows. `containing_symbol` is the
/// canonical ID of the innermost named container, or `None` for a top-level
/// use. `hint_json` is the serde JSON of a [`rivet_core::UseHint`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UseRow {
    /// SQLite row ID (`None` before insert, `Some` when read).
    pub use_id: Option<i64>,
    /// Repo-relative `/`-separated file path (the `files` foreign key).
    pub file: String,
    /// Canonical ID of the containing declaration, or `None` at file scope.
    pub containing_symbol: Option<String>,
    /// Deterministic lexical scope key.
    pub scope_key: String,
    /// The identifier exactly as written in source.
    pub spelling: String,
    /// Case-folded lookup key following the language's rule.
    pub lookup_name: String,
    /// The syntactic form of the use.
    pub ref_kind: RefKind,
    /// Zero-based inclusive use start byte.
    pub start_byte: u32,
    /// Zero-based exclusive use end byte.
    pub end_byte: u32,
    /// One-based line containing `start_byte`.
    pub line: u32,
    /// One-based UTF-8 byte column of `start_byte`.
    pub col: u32,
    /// Source text of the receiver expression, if any.
    pub receiver: Option<String>,
    /// Serde JSON of the adapter's lexical receiver hint.
    pub hint_json: String,
}

/// The column list shared by every `scopes` read, in schema order.
const SCOPE_COLUMNS: &str = "file, scope_key, parent_scope_key, facts_json";

/// One row of the `scopes` table.
///
/// `facts_json` holds the language-neutral owned scope facts (imports, typed
/// and `new` bindings, and declared symbol IDs) so a later resolver can
/// re-resolve every use without reparsing the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeRow {
    /// Repo-relative `/`-separated file path (the `files` foreign key).
    pub file: String,
    /// Deterministic scope key within the file.
    pub scope_key: String,
    /// The enclosing scope's key, or `None` for the file scope.
    pub parent_scope_key: Option<String>,
    /// Serde JSON of this scope's facts.
    pub facts_json: String,
}

/// One row of the `bindings` table.
///
/// A binding links one persisted use to the single declaration a resolver rule
/// selected. `resolution` is `exact` or `scoped`; the schema rejects
/// `name_match`, which is query-relative evidence rather than a stored link.
/// The row is written by `publish_inventory` after every use has a `use_id`, so
/// `use_id` is always the SQLite surrogate key of a committed use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingRow {
    /// SQLite row ID of the bound use (the `bindings` primary key).
    pub use_id: i64,
    /// Canonical ID of the target declaration.
    pub target_id: String,
    /// Evidence tier for the link (`exact` or `scoped`).
    pub resolution: Resolution,
}

/// One row of the `receiver_classes` table (LR2).
///
/// For a use that no rule bound, the class a receiver rule determined for its
/// member or scoped receiver (`$this`, `self`, a trusted `new` or typed
/// receiver, or a class named before `::`), under the same trust conditions
/// the rule applies before binding. It is never a binding: it links no use to
/// a declaration and carries no tier. Reference mode reads it only to exclude
/// a same-name use whose receiver class is unrelated to the target's class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiverClassRow {
    /// SQLite row ID of the unbound use.
    pub use_id: i64,
    /// The receiver class's fully qualified name, without a leading `\`.
    pub class_qname: String,
    /// The canonical ID of that class when it is indexed, else `None`.
    pub class_id: Option<String>,
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
    /// Every extracted symbol for the current inventory. Symbols are replaced
    /// per file inside the same transaction as the file rows.
    pub symbols: Vec<SymbolRow>,
    /// Every extracted use for the current inventory. Uses are replaced per
    /// file, together with that file's symbols and scopes, in the same
    /// transaction.
    pub uses: Vec<UseRow>,
    /// Every extracted scope for the current inventory. Scopes are replaced
    /// per file, together with that file's symbols and uses, in the same
    /// transaction.
    pub scopes: Vec<ScopeRow>,
    /// Every resolved binding for the current inventory. The whole table is
    /// replaced: bindings are re-resolved for all uses whenever they are
    /// replaced (spec §12.3; always, unless a [`StagePlan`] keeps them). Each
    /// `use_id` must name a use in `uses`.
    pub bindings: Vec<BindingRow>,
    /// Every determined receiver class of an unbound use (LR2). Replaced
    /// together with [`InventoryInput::bindings`], under the same rules.
    pub receiver_classes: Vec<ReceiverClassRow>,
    /// `--force`: delete every stored fact and rebuild it in this same
    /// transaction, so all current file rows count as `updated`.
    pub force: bool,
    /// Paths whose facts this publication regenerated although their content
    /// hash, parse status, and language may be unchanged (for example, every
    /// file reparsed after an extractor fingerprint change). Each counts as
    /// `updated`, never `unchanged`. A path not in `files` is ignored.
    pub regenerated: Vec<String>,
}

/// Which stored facts a [`WriteTxn::stage_refresh`] call may keep instead of
/// rewriting (PF1; ARCHITECTURE "Refresh and invalidation": "replace file
/// facts" only for changed content, and "if content, membership, or resolver
/// fingerprint changed: clear bindings; re-resolve all persisted uses").
///
/// [`StagePlan::full`] keeps nothing, which is what
/// [`WriteTxn::stage_inventory`] and [`Store::publish_inventory`] use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagePlan {
    /// Current paths whose facts were regenerated (reparsed) by the caller.
    /// `None` rewrites the facts of every current file.
    ///
    /// With `Some`, a current file's symbols, uses, and scopes are replaced
    /// when it is listed here, is new, or its content hash, parse status,
    /// language, or source presence differs from the stored row. Every other
    /// current file keeps its stored fact rows untouched, and any rows the
    /// caller supplied for it are ignored: the caller guarantees its stored
    /// facts are still current (a refresh reuses them verbatim).
    pub reparsed: Option<HashSet<String>>,
    /// Whether the whole `bindings` table is cleared and replaced by
    /// [`InventoryInput::bindings`]. When `false` the stored bindings are kept,
    /// `InventoryInput::bindings` must be empty, and the staging fails unless
    /// the publication changes no file's content, status, language, or facts
    /// and deletes no file, since any such change can move a binding in an
    /// unchanged file (T22).
    pub replace_bindings: bool,
}

impl StagePlan {
    /// Rewrite every fact and every binding.
    pub fn full() -> StagePlan {
        StagePlan {
            reparsed: None,
            replace_bindings: true,
        }
    }
}

/// Counts and digest describing one [`Store::publish_inventory`] call.
///
/// `updated + unchanged == files.len()` of the published input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishReport {
    /// New rows, rows whose content hash, parse status, or language changed,
    /// and rows listed in [`InventoryInput::regenerated`].
    pub updated: u64,
    /// Rows present before and after with none of those three values changed
    /// and whose facts were not regenerated.
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
    /// The writer lock (a `BEGIN IMMEDIATE` transaction on `index.db`) was not
    /// acquired within the busy timeout because another process holds it.
    WriterLocked {
        /// The database whose writer lock is held.
        path: String,
        /// The busy timeout that elapsed, in milliseconds.
        timeout_ms: u64,
    },
    /// A transaction was begun while another was already open on the store.
    TransactionState {
        /// What was attempted.
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
            Error::WriterLocked { path, timeout_ms } => write!(
                f,
                "the index writer lock on {path} is held by another process \
                 (not acquired within {timeout_ms} ms)"
            ),
            Error::TransactionState { detail } => write!(f, "invalid transaction state: {detail}"),
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

/// Which existing databases of another format an open may drop and recreate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rebuild {
    /// None: every other format is refused unmodified.
    Never,
    /// Only [`SUPPORTED_OLDER_FORMAT_VERSIONS`].
    Older,
    /// Any other format (`index --force`).
    Any,
}

impl Rebuild {
    /// Whether a database whose stored format is `found` may be rebuilt.
    fn allows(self, found: &str) -> bool {
        match self {
            Rebuild::Never => false,
            Rebuild::Older => SUPPORTED_OLDER_FORMAT_VERSIONS.contains(&found),
            Rebuild::Any => true,
        }
    }
}

/// An open index cache.
#[derive(Debug)]
pub struct Store {
    conn: Connection,
    /// The on-disk database, or `None` for an in-memory store.
    db_path: Option<PathBuf>,
}

impl Store {
    /// Opens the index cache in `rivet_dir`.
    ///
    /// The destination is validated before any write: `rivet_dir` must exist
    /// and be a real directory, and `index.db` (if present) must be a regular
    /// file, never a symlink. The connection enables foreign keys and a five
    /// second busy timeout. An empty database receives the current schema. An
    /// existing database in a supported older format (LR2: version `1`) is a
    /// disposable cache and is dropped and recreated under the writer lock
    /// (RELEASING "Index format version": "supported older formats rebuild");
    /// any other different `meta.index_format_version` is refused without
    /// modifying it. WAL journaling is enabled only after the format is known
    /// to be compatible.
    pub fn open(rivet_dir: &Path) -> Result<Store, Error> {
        Store::open_with_rebuild(rivet_dir, Rebuild::Older)
    }

    /// Opens an existing index cache for a read-only answer (`--no-refresh`):
    /// like [`Store::open`], but a database in any other format, older ones
    /// included, is refused unmodified, because this path must not write.
    pub fn open_cached(rivet_dir: &Path) -> Result<Store, Error> {
        Store::open_with_rebuild(rivet_dir, Rebuild::Never)
    }

    /// Opens the index cache, dropping and recreating the schema when an
    /// existing database has an incompatible format version.
    ///
    /// This is the `index --force` path: the cache is disposable, so after
    /// [`validate_destination`] accepts the destination an incompatible
    /// database is torn down and the current schema recreated (spec §13;
    /// ARCHITECTURE "Concurrency and source consistency"). User source and
    /// configuration are never touched.
    pub fn open_rebuildable(rivet_dir: &Path) -> Result<Store, Error> {
        Store::open_with_rebuild(rivet_dir, Rebuild::Any)
    }

    /// Shared implementation of the three file-backed opens.
    fn open_with_rebuild(rivet_dir: &Path, rebuild: Rebuild) -> Result<Store, Error> {
        validate_destination(rivet_dir)?;
        let path = rivet_dir.join(INDEX_DB_FILE);
        let conn = Connection::open(&path)?;
        configure_connection(&conn)?;
        initialize(&conn, rebuild)?;
        // Set WAL only after the format is accepted so a refused database is
        // left byte-for-byte unchanged (spec §27).
        enable_wal(&conn)?;
        Ok(Store {
            conn,
            db_path: Some(path),
        })
    }

    /// Opens a fresh in-memory index cache with the current schema.
    ///
    /// Intended for tests: no filesystem destination is touched and WAL is not
    /// applicable.
    pub fn open_in_memory() -> Result<Store, Error> {
        let conn = Connection::open_in_memory()?;
        configure_connection(&conn)?;
        initialize(&conn, Rebuild::Never)?;
        Ok(Store {
            conn,
            db_path: None,
        })
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
    /// The whole publication runs in one `BEGIN IMMEDIATE` transaction (see
    /// [`Store::begin_write`]): rows absent from `input.files` are deleted,
    /// present rows are inserted or updated, and the `meta` values are written
    /// before commit. Any failure rolls the transaction back so the previous
    /// complete inventory and meta stay untouched (spec §12.3; ARCHITECTURE
    /// "Refresh and invalidation" and "Concurrency and source consistency"). A
    /// duplicate new path aborts the transaction rather than collapsing two
    /// logically distinct inputs.
    ///
    /// When `input.force` is set, every stored fact (`files`, `symbols`,
    /// `uses`, `bindings`, `scopes`, `diagnostics`) is deleted first inside the
    /// same transaction and rebuilt, so every current file row counts as
    /// `updated` (spec §13). The database file itself is never deleted.
    ///
    /// This is [`Store::begin_write`], [`WriteTxn::stage_inventory`], and
    /// [`WriteTxn::commit`] in one call, for callers that computed the
    /// inventory without holding the writer lock (tests and fixtures). The
    /// refresh path holds the lock for the whole refresh instead.
    pub fn publish_inventory(&mut self, input: InventoryInput) -> Result<PublishReport, Error> {
        let mut txn = self.begin_write(WRITER_BUSY_TIMEOUT)?;
        let staged = txn.stage_inventory(input)?;
        txn.commit(staged)
    }

    /// Takes the writer lock: begins one `BEGIN IMMEDIATE` transaction that
    /// every later read and write on this store joins until the returned guard
    /// commits or is dropped (ARCHITECTURE "Refresh and invalidation": the
    /// refresh begins with `begin immediate transaction (busy timeout: 5
    /// seconds)`).
    ///
    /// `busy_timeout` bounds the wait for another process's writer. When it
    /// elapses the error is [`Error::WriterLocked`]; nothing was read or
    /// written. Dropping the guard without [`WriteTxn::commit`] rolls the whole
    /// transaction back, as does a crash or kill of the process, so no partial
    /// refresh ever becomes visible.
    pub fn begin_write(&self, busy_timeout: Duration) -> Result<WriteTxn<'_>, Error> {
        if !self.conn.is_autocommit() {
            return Err(Error::TransactionState {
                detail: "a transaction is already open on this store".to_string(),
            });
        }
        let timeout_ms = i64::try_from(busy_timeout.as_millis()).unwrap_or(i64::MAX);
        self.conn.pragma_update(None, "busy_timeout", timeout_ms)?;
        let begun = self.conn.execute_batch("BEGIN IMMEDIATE");
        // The busy timeout is a connection setting, not transactional state, so
        // the default is restored for every later statement either way.
        let restored = self
            .conn
            .pragma_update(None, "busy_timeout", DEFAULT_BUSY_TIMEOUT_MS);
        match begun {
            Ok(()) => {
                let txn = WriteTxn {
                    store: self,
                    open: true,
                };
                restored?;
                Ok(txn)
            }
            Err(error) if is_busy(&error) => Err(Error::WriterLocked {
                path: self.lock_path(),
                timeout_ms: timeout_ms as u64,
            }),
            Err(error) => Err(error.into()),
        }
    }

    /// Begins one committed read transaction and returns the `snapshot_digest`
    /// it observes.
    ///
    /// ARCHITECTURE "Concurrency and source consistency": "Readers use a
    /// committed read transaction." The digest is read as the transaction's
    /// first statement, which is what pins the WAL snapshot, so every later
    /// read on this store (rows, stored source bytes, meta) sees exactly the
    /// committed snapshot whose digest this returns, even while another process
    /// publishes (spec §12.4 step 4). The transaction stays open until
    /// [`Store::end_snapshot_read`] or until the store is dropped; it takes no
    /// writer lock and never blocks a writer.
    pub fn begin_snapshot_read(&self) -> Result<Option<String>, Error> {
        if !self.conn.is_autocommit() {
            return Err(Error::TransactionState {
                detail: "a transaction is already open on this store".to_string(),
            });
        }
        self.conn.execute_batch("BEGIN DEFERRED")?;
        let digest = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'snapshot_digest'",
                [],
                |row| row.get(0),
            )
            .optional();
        match digest {
            Ok(digest) => Ok(digest),
            Err(error) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(error.into())
            }
        }
    }

    /// Ends the read transaction begun by [`Store::begin_snapshot_read`]. A
    /// store with no open transaction is left unchanged.
    pub fn end_snapshot_read(&self) -> Result<(), Error> {
        if !self.conn.is_autocommit() {
            self.conn.execute_batch("COMMIT")?;
        }
        Ok(())
    }

    /// Runs `PRAGMA integrity_check` and returns its result rows (`["ok"]` for
    /// a healthy database).
    pub fn integrity_check(&self) -> Result<Vec<String>, Error> {
        let mut stmt = self.conn.prepare("PRAGMA integrity_check")?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<String>>>()?)
    }

    /// The path named by a writer-lock failure.
    fn lock_path(&self) -> String {
        match &self.db_path {
            Some(path) => path.display().to_string(),
            None => ":memory:".to_string(),
        }
    }

    /// Runs `read` in this store's open transaction, or in a new read
    /// transaction of its own when none is open.
    ///
    /// Inside [`Store::begin_write`] or [`Store::begin_snapshot_read`] every
    /// read therefore sees the same snapshot (the writer's own uncommitted rows,
    /// or one committed snapshot).
    fn read<T>(&self, read: impl FnOnce(&Connection) -> Result<T, Error>) -> Result<T, Error> {
        if self.conn.is_autocommit() {
            let tx = self.conn.unchecked_transaction()?;
            let value = read(&tx)?;
            tx.commit()?;
            Ok(value)
        } else {
            read(&self.conn)
        }
    }

    /// Returns the `files` row for `path`, if any.
    pub fn get_file(&self, path: &str) -> Result<Option<FileRow>, Error> {
        self.read(|conn| {
            Ok(conn
                .query_row(
                    "SELECT path, language, mtime_ns, size, content_hash, source, parse_status
                     FROM files WHERE path = ?1",
                    params![path],
                    file_row_from,
                )
                .optional()?)
        })
    }

    /// Returns every `files` row ordered by path bytes.
    pub fn list_files(&self) -> Result<Vec<FileRow>, Error> {
        self.read(|conn| {
            let mut stmt = conn.prepare(
                "SELECT path, language, mtime_ns, size, content_hash, source, parse_status
                 FROM files ORDER BY path COLLATE BINARY",
            )?;
            let rows = stmt.query_map([], file_row_from)?;
            Ok(rows.collect::<rusqlite::Result<Vec<FileRow>>>()?)
        })
    }

    /// Returns every file's path and stored language, ordered by path bytes,
    /// without reading source bytes (T43). The language decides which
    /// identifier comparison and which binding rules apply to a file's rows.
    pub fn list_file_languages(&self) -> Result<Vec<(String, Option<String>)>, Error> {
        self.read(|conn| {
            let mut stmt =
                conn.prepare("SELECT path, language FROM files ORDER BY path COLLATE BINARY")?;
            let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
            Ok(rows.collect::<rusqlite::Result<Vec<(String, Option<String>)>>>()?)
        })
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
        self.read(|conn| {
            Ok(conn
                .query_row(
                    "SELECT value FROM meta WHERE key = ?1",
                    params![key],
                    |row| row.get(0),
                )
                .optional()?)
        })
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

    /// Deletes then inserts every symbol row for one file in one transaction.
    ///
    /// Replacing a whole file's symbols keeps a refresh atomic per file; the
    /// deferred `parent_id` foreign key allows a child row to be inserted
    /// before its parent row within the same transaction. `publish_inventory`
    /// calls the same helper inside its single publication transaction.
    pub fn replace_file_symbols(&mut self, file: &str, symbols: &[SymbolRow]) -> Result<(), Error> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        replace_file_symbols_in_tx(&tx, file, symbols)?;
        tx.commit()?;
        Ok(())
    }

    /// Returns the symbol row with canonical ID `id`, if any.
    pub fn get_symbol(&self, id: &str) -> Result<Option<SymbolRow>, Error> {
        self.read(|conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {SYMBOL_COLUMNS} FROM symbols WHERE id = ?1"),
                    params![id],
                    symbol_row_from,
                )
                .optional()?)
        })
    }

    /// Returns every symbol whose persisted `lookup_name` equals `name`.
    ///
    /// Rows are ordered by `(file bytes, start_byte, id)`.
    pub fn find_symbols_by_lookup_name(&self, name: &str) -> Result<Vec<SymbolRow>, Error> {
        self.select_symbols(
            &format!(
                "SELECT {SYMBOL_COLUMNS} FROM symbols WHERE lookup_name = ?1 \
                 ORDER BY file COLLATE BINARY, start_byte, id COLLATE BINARY"
            ),
            params![name],
        )
    }

    /// Returns every symbol whose persisted `qualified_name` equals `qname`.
    ///
    /// Rows are ordered by `(file bytes, start_byte, id)`.
    pub fn find_symbols_by_qualified_name(&self, qname: &str) -> Result<Vec<SymbolRow>, Error> {
        self.select_symbols(
            &format!(
                "SELECT {SYMBOL_COLUMNS} FROM symbols WHERE qualified_name = ?1 \
                 ORDER BY file COLLATE BINARY, start_byte, id COLLATE BINARY"
            ),
            params![qname],
        )
    }

    /// Returns every persisted symbol ordered by `(file bytes, start_byte, id)`.
    ///
    /// Dotted-path and case-folded qualified-name matching need to normalize
    /// separators and case in Rust, so they scan this ordered list instead of a
    /// single indexed lookup.
    pub fn list_symbols(&self) -> Result<Vec<SymbolRow>, Error> {
        self.select_symbols(
            &format!(
                "SELECT {SYMBOL_COLUMNS} FROM symbols \
                 ORDER BY file COLLATE BINARY, start_byte, id COLLATE BINARY"
            ),
            [],
        )
    }

    /// Runs a `symbols` SELECT and collects every row.
    fn select_symbols(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
    ) -> Result<Vec<SymbolRow>, Error> {
        self.read(|conn| {
            let mut stmt = conn.prepare(sql)?;
            let rows = stmt.query_map(params, symbol_row_from)?;
            Ok(rows.collect::<rusqlite::Result<Vec<SymbolRow>>>()?)
        })
    }

    /// Returns every use row for `file`.
    ///
    /// Rows are ordered by `(start_byte, end_byte, ref_kind, use_id)`.
    pub fn list_uses_for_file(&self, file: &str) -> Result<Vec<UseRow>, Error> {
        self.select_uses(
            &format!(
                "SELECT {USE_COLUMNS} FROM uses WHERE file = ?1 \
                 ORDER BY start_byte, end_byte, ref_kind COLLATE BINARY, use_id"
            ),
            params![file],
        )
    }

    /// Returns every use whose persisted `lookup_name` equals `name`.
    ///
    /// Rows are ordered by `(file bytes, start_byte, end_byte, ref_kind)`.
    /// `use_id` is a final tiebreaker, though `UNIQUE(file, start_byte,
    /// end_byte, ref_kind)` already makes the documented four columns unique.
    pub fn find_uses_by_lookup_name(&self, name: &str) -> Result<Vec<UseRow>, Error> {
        self.select_uses(
            &format!(
                "SELECT {USE_COLUMNS} FROM uses WHERE lookup_name = ?1 \
                 ORDER BY file COLLATE BINARY, start_byte, end_byte, \
                 ref_kind COLLATE BINARY, use_id"
            ),
            params![name],
        )
    }

    /// Runs a `uses` SELECT and collects every row.
    fn select_uses(&self, sql: &str, params: impl rusqlite::Params) -> Result<Vec<UseRow>, Error> {
        self.read(|conn| {
            let mut stmt = conn.prepare(sql)?;
            let rows = stmt.query_map(params, use_row_from)?;
            Ok(rows.collect::<rusqlite::Result<Vec<UseRow>>>()?)
        })
    }

    /// Returns every scope row for `file`, ordered by `scope_key` bytes.
    pub fn list_scopes_for_file(&self, file: &str) -> Result<Vec<ScopeRow>, Error> {
        self.read(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {SCOPE_COLUMNS} FROM scopes WHERE file = ?1 \
                 ORDER BY scope_key COLLATE BINARY"
            ))?;
            let rows = stmt.query_map(params![file], scope_row_from)?;
            Ok(rows.collect::<rusqlite::Result<Vec<ScopeRow>>>()?)
        })
    }

    /// Returns every persisted binding ordered by `use_id`.
    ///
    /// Rows are the whole `bindings` table, so a caller can compare
    /// re-resolution output across refreshes.
    pub fn list_bindings(&self) -> Result<Vec<BindingRow>, Error> {
        self.read(|conn| {
            let mut stmt =
                conn.prepare("SELECT use_id, target_id, resolution FROM bindings ORDER BY use_id")?;
            let rows = stmt.query_map([], binding_row_from)?;
            Ok(rows.collect::<rusqlite::Result<Vec<BindingRow>>>()?)
        })
    }

    /// Returns every persisted receiver class ordered by `use_id` (LR2).
    pub fn list_receiver_classes(&self) -> Result<Vec<ReceiverClassRow>, Error> {
        self.read(|conn| {
            let mut stmt = conn.prepare(
                "SELECT use_id, class_qname, class_id FROM receiver_classes ORDER BY use_id",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(ReceiverClassRow {
                    use_id: row.get(0)?,
                    class_qname: row.get(1)?,
                    class_id: row.get(2)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<ReceiverClassRow>>>()?)
        })
    }

    /// Returns every persisted use ordered by `(file bytes, start_byte,
    /// end_byte, ref_kind, use_id)`: per file, the same order as
    /// [`Store::list_uses_for_file`], in one query.
    pub fn list_uses(&self) -> Result<Vec<UseRow>, Error> {
        self.select_uses(
            &format!(
                "SELECT {USE_COLUMNS} FROM uses \
                 ORDER BY file COLLATE BINARY, start_byte, end_byte, \
                 ref_kind COLLATE BINARY, use_id"
            ),
            [],
        )
    }

    /// Returns every persisted scope ordered by `(file bytes, scope_key
    /// bytes)`: per file, the same order as [`Store::list_scopes_for_file`],
    /// in one query.
    pub fn list_scopes(&self) -> Result<Vec<ScopeRow>, Error> {
        self.read(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {SCOPE_COLUMNS} FROM scopes \
                 ORDER BY file COLLATE BINARY, scope_key COLLATE BINARY"
            ))?;
            let rows = stmt.query_map([], scope_row_from)?;
            Ok(rows.collect::<rusqlite::Result<Vec<ScopeRow>>>()?)
        })
    }

    /// Returns the number of persisted symbols.
    pub fn count_symbols(&self) -> Result<u64, Error> {
        self.count_rows("SELECT COUNT(*) FROM symbols")
    }

    /// Returns the number of persisted uses.
    pub fn count_uses(&self) -> Result<u64, Error> {
        self.count_rows("SELECT COUNT(*) FROM uses")
    }

    /// Returns the number of persisted bindings.
    pub fn count_bindings(&self) -> Result<u64, Error> {
        self.count_rows("SELECT COUNT(*) FROM bindings")
    }

    /// Runs one `SELECT COUNT(*)` statement.
    fn count_rows(&self, sql: &str) -> Result<u64, Error> {
        self.read(|conn| {
            let count: i64 = conn.query_row(sql, [], |row| row.get(0))?;
            Ok(count as u64)
        })
    }

    /// The number of rows inserted, updated, or deleted through this store's
    /// connection since it opened, including foreign-key actions (SQLite
    /// `sqlite3_total_changes64`). Tests use the difference across a refresh to
    /// prove how many rows it wrote without depending on timing.
    pub fn total_changes(&self) -> u64 {
        self.conn.total_changes()
    }

    /// The underlying connection, for tests and diagnostics that inspect the
    /// database directly (for example with temporary triggers). A write through
    /// it bypasses every invariant this store maintains.
    pub fn connection(&self) -> &Connection {
        &self.conn
    }
}

/// The writer lock: one open `BEGIN IMMEDIATE` transaction on a [`Store`].
///
/// Created by [`Store::begin_write`]. Every read made through the store while
/// the guard lives joins this transaction, so a refresh loads the previous
/// inventory, assigns use IDs, and writes the new facts against one state that
/// no other writer can change. [`WriteTxn::commit`] makes the staged snapshot
/// visible; dropping the guard, or the process dying, rolls everything back.
#[derive(Debug)]
pub struct WriteTxn<'s> {
    store: &'s Store,
    open: bool,
}

/// An inventory written inside a [`WriteTxn`] whose `meta` fingerprints and
/// snapshot digest are not written yet.
///
/// Returned by [`WriteTxn::stage_inventory`] and consumed by
/// [`WriteTxn::commit`], which computes the digest, writes `meta`, and commits
/// (ARCHITECTURE "Refresh and invalidation": the recheck comes between the
/// fact writes and "compute deterministic digest and commit").
#[derive(Debug)]
pub struct StagedInventory {
    fingerprint: Fingerprint,
    /// The published file rows without their source bytes, which the digest
    /// does not cover.
    digest_files: Vec<FileRow>,
    updated: u64,
    unchanged: u64,
    deleted: u64,
}

impl WriteTxn<'_> {
    /// The store this transaction belongs to; its reads join the transaction.
    pub fn store(&self) -> &Store {
        self.store
    }

    /// Replaces the complete `files` inventory and every per-file fact inside
    /// this transaction, without writing `meta` or committing.
    ///
    /// Rows absent from `input.files` are deleted, present rows are inserted
    /// or updated, and symbols, uses, and scopes are replaced per current file.
    /// The whole `bindings` table is replaced. See [`Store::publish_inventory`]
    /// for the `force` and `regenerated` semantics. An error leaves the
    /// transaction open for the caller to drop, which rolls it back.
    pub fn stage_inventory(&mut self, input: InventoryInput) -> Result<StagedInventory, Error> {
        self.stage_refresh(input, StagePlan::full())
    }

    /// [`WriteTxn::stage_inventory`] that keeps what `plan` allows (PF1).
    ///
    /// A `files` row is written only when one of its columns changes: a row
    /// whose content hash, parse status, language, source presence, mtime,
    /// and size all equal the stored row is left alone, and a row whose only
    /// change is mtime or size has just those two columns updated. Facts are
    /// replaced only for the files [`StagePlan::reparsed`] selects, and the
    /// `bindings` table only when [`StagePlan::replace_bindings`] is set. The
    /// `meta` values are written in [`WriteTxn::commit`] only when they differ.
    /// A refresh that changes nothing therefore modifies no row at all.
    ///
    /// `force` ignores `plan` and rewrites everything.
    pub fn stage_refresh(
        &mut self,
        input: InventoryInput,
        plan: StagePlan,
    ) -> Result<StagedInventory, Error> {
        let InventoryInput {
            fingerprint,
            files,
            symbols,
            uses,
            scopes,
            bindings,
            receiver_classes,
            force,
            regenerated,
        } = input;
        let plan = if force { StagePlan::full() } else { plan };
        if !plan.replace_bindings && !(bindings.is_empty() && receiver_classes.is_empty()) {
            return Err(Error::TransactionState {
                detail: "bindings were supplied but the plan keeps the stored bindings".to_string(),
            });
        }
        let tx: &Connection = &self.store.conn;
        // OUTPUT-CONTRACT "Administrative commands": "`updated` counts current
        // file rows with changed source/status/language or regenerated facts".
        let regenerated: HashSet<String> = regenerated.into_iter().collect();

        // Sort by path bytes so writes and the digest are deterministic and
        // independent of the caller's input order.
        let mut sorted = files;
        sorted.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));

        let previous = load_previous_inventory(tx)?;
        let incoming: HashSet<&str> = sorted.iter().map(|file| file.path.as_str()).collect();
        let deleted = previous
            .keys()
            .filter(|path| !incoming.contains(path.as_str()))
            .count() as u64;

        // Which current files get their facts rewritten, decided before any
        // write from the stored rows. `None` in the plan selects every file.
        let replace_facts: HashSet<&str> = sorted
            .iter()
            .filter(|file| match (&plan.reparsed, previous.get(&file.path)) {
                (None, _) | (Some(_), None) => true,
                (Some(reparsed), Some(stored)) => {
                    reparsed.contains(&file.path) || !stored.same_content(file)
                }
            })
            .map(|file| file.path.as_str())
            .collect();

        // Keeping the stored bindings is only sound when no fact, content,
        // status, language, or membership changed (T22: a change in one file
        // can move a binding in another). A reparse that reproduced a failed
        // file's empty facts changes nothing and is allowed.
        if !plan.replace_bindings {
            let content_changed = deleted > 0
                || sorted.iter().any(|file| match previous.get(&file.path) {
                    None => true,
                    Some(stored) => {
                        !stored.same_content(file)
                            || regenerated.contains(&file.path)
                            // A rewritten `ok` file gets new use IDs, and
                            // deleting its old uses cascades to their bindings.
                            || (replace_facts.contains(file.path.as_str())
                                && (file.parse_status == ParseStatus::Ok
                                    || stored.parse_status == ParseStatus::Ok))
                    }
                });
            if content_changed {
                return Err(Error::TransactionState {
                    detail: "the plan keeps stored bindings although file content, status, \
                             membership, or facts changed"
                        .to_string(),
                });
            }
        }

        // Bindings are re-resolved for every persisted use whenever anything
        // they depend on changed, so the whole table is cleared before the new
        // facts are written and the fresh rows are inserted after all uses
        // exist (spec §12.3).
        if plan.replace_bindings {
            tx.execute("DELETE FROM bindings", [])?;
            tx.execute("DELETE FROM receiver_classes", [])?;
        }

        // A forced rebuild discards every stored fact before writing the new
        // inventory. Child tables are cleared before their parents so foreign
        // keys never see a dangling reference; `diagnostics` has no foreign key
        // and is cleared explicitly.
        if force {
            tx.execute("DELETE FROM uses", [])?;
            tx.execute("DELETE FROM scopes", [])?;
            tx.execute("DELETE FROM diagnostics", [])?;
            tx.execute("DELETE FROM symbols", [])?;
            tx.execute("DELETE FROM files", [])?;
        } else {
            // Delete rows that left the eligible set before writing the new
            // ones, in path order.
            let mut gone: Vec<&String> = previous
                .keys()
                .filter(|path| !incoming.contains(path.as_str()))
                .collect();
            gone.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
            let mut delete = tx.prepare("DELETE FROM files WHERE path = ?1")?;
            for path in gone {
                delete.execute(params![path])?;
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
            // ARCHITECTURE "Refresh and invalidation": "Update mtime/size even
            // when the new hash equals the old hash."
            let mut update_metadata =
                tx.prepare("UPDATE files SET mtime_ns = ?2, size = ?3 WHERE path = ?1")?;
            if force {
                // Every current row was just deleted, so all of them are new.
                for file in &sorted {
                    updated += 1;
                    write_file_row(&mut insert, file)?;
                }
            } else {
                for file in &sorted {
                    match previous.get(&file.path) {
                        Some(stored) if stored.same_content(file) => {
                            if regenerated.contains(&file.path) {
                                updated += 1;
                            } else {
                                unchanged += 1;
                            }
                            if stored.mtime_ns != file.mtime_ns || stored.size != file.size {
                                update_metadata.execute(params![
                                    file.path,
                                    file.mtime_ns,
                                    file.size as i64
                                ])?;
                            }
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
        }

        // Replace symbols, uses, and scopes for the selected current files in
        // the same transaction. Every selected file is replaced even when it
        // now has no facts, which removes stale facts from a file that became
        // parse_error/resource_limit in this refresh.
        let mut symbols_by_file: HashMap<String, Vec<SymbolRow>> = HashMap::new();
        for symbol in symbols {
            if replace_facts.contains(symbol.file.as_str()) {
                symbols_by_file
                    .entry(symbol.file.clone())
                    .or_default()
                    .push(symbol);
            }
        }
        let mut uses_by_file: HashMap<String, Vec<UseRow>> = HashMap::new();
        for row in uses {
            if replace_facts.contains(row.file.as_str()) {
                uses_by_file.entry(row.file.clone()).or_default().push(row);
            }
        }
        let mut scopes_by_file: HashMap<String, Vec<ScopeRow>> = HashMap::new();
        for row in scopes {
            if replace_facts.contains(row.file.as_str()) {
                scopes_by_file
                    .entry(row.file.clone())
                    .or_default()
                    .push(row);
            }
        }
        for file in &sorted {
            if !replace_facts.contains(file.path.as_str()) {
                continue;
            }
            let symbol_rows = symbols_by_file.remove(&file.path).unwrap_or_default();
            replace_file_symbols_in_tx(tx, &file.path, &symbol_rows)?;
            let use_rows = uses_by_file.remove(&file.path).unwrap_or_default();
            replace_file_uses_in_tx(tx, &file.path, &use_rows)?;
            let scope_rows = scopes_by_file.remove(&file.path).unwrap_or_default();
            replace_file_scopes_in_tx(tx, &file.path, &scope_rows)?;
        }
        if plan.replace_bindings {
            insert_bindings_in_tx(tx, &bindings)?;
            insert_receiver_classes_in_tx(tx, &receiver_classes)?;
        }

        let digest_files = sorted
            .into_iter()
            .map(|file| FileRow {
                source: None,
                ..file
            })
            .collect();
        Ok(StagedInventory {
            fingerprint,
            digest_files,
            updated,
            unchanged,
            deleted,
        })
    }

    /// Sets one `meta` value inside this transaction, writing nothing when the
    /// stored value is already `value`.
    pub fn set_meta(&self, key: &str, value: &str) -> Result<(), Error> {
        upsert_meta_if_changed(&self.store.conn, key, value)
    }

    /// Computes the deterministic snapshot digest of `staged`, writes the four
    /// fingerprints and the digest into `meta`, and commits the transaction.
    ///
    /// On any failure the guard is dropped and the transaction rolled back.
    pub fn commit(mut self, staged: StagedInventory) -> Result<PublishReport, Error> {
        let StagedInventory {
            fingerprint,
            digest_files,
            updated,
            unchanged,
            deleted,
        } = staged;
        let digest = snapshot_digest(&fingerprint, &digest_files);
        write_fingerprint(&self.store.conn, &fingerprint, &digest)?;
        self.store.conn.execute_batch("COMMIT")?;
        self.open = false;
        Ok(PublishReport {
            updated,
            unchanged,
            deleted,
            digest,
        })
    }

    /// Rolls the transaction back explicitly, releasing the writer lock.
    pub fn rollback(mut self) -> Result<(), Error> {
        self.open = false;
        self.store.conn.execute_batch("ROLLBACK")?;
        Ok(())
    }
}

impl Drop for WriteTxn<'_> {
    fn drop(&mut self) {
        if self.open && !self.store.conn.is_autocommit() {
            let _ = self.store.conn.execute_batch("ROLLBACK");
        }
    }
}

/// Whether `error` is SQLite reporting that another connection holds a lock.
fn is_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(code, _)
            if code.code == rusqlite::ErrorCode::DatabaseBusy
                || code.code == rusqlite::ErrorCode::DatabaseLocked
    )
}

/// Deletes then inserts `symbols` for `file` inside the caller's transaction.
fn replace_file_symbols_in_tx(
    tx: &Connection,
    file: &str,
    symbols: &[SymbolRow],
) -> Result<(), Error> {
    tx.execute("DELETE FROM symbols WHERE file = ?1", params![file])?;
    if symbols.is_empty() {
        return Ok(());
    }
    let mut insert = tx.prepare(
        "INSERT INTO symbols
             (id, file, name, lookup_name, qualified_name, kind, parent_id,
              start_byte, end_byte, start_line, end_line, signature, doc_comment)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
    )?;
    for symbol in symbols {
        insert.execute(params![
            symbol.id,
            symbol.file,
            symbol.name,
            symbol.lookup_name,
            symbol.qualified_name,
            symbol.kind.as_str(),
            symbol.parent_id,
            symbol.start_byte,
            symbol.end_byte,
            symbol.start_line,
            symbol.end_line,
            symbol.signature,
            symbol.doc_comment,
        ])?;
    }
    Ok(())
}

/// Maps a `symbols` row (selected with [`SYMBOL_COLUMNS`]) to a [`SymbolRow`].
fn symbol_row_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<SymbolRow> {
    let id: String = row.get("id")?;
    let file: String = row.get("file")?;
    let name: String = row.get("name")?;
    let lookup_name: String = row.get("lookup_name")?;
    let qualified_name: String = row.get("qualified_name")?;
    let kind: String = row.get("kind")?;
    let kind = kind.parse::<SymbolKind>().map_err(|error| {
        // `kind` is the sixth selected column (index 5).
        rusqlite::Error::FromSqlConversionFailure(5, Type::Text, Box::new(error))
    })?;
    let parent_id: Option<String> = row.get("parent_id")?;
    let start_byte: i64 = row.get("start_byte")?;
    let end_byte: i64 = row.get("end_byte")?;
    let start_line: i64 = row.get("start_line")?;
    let end_line: i64 = row.get("end_line")?;
    let signature: Option<String> = row.get("signature")?;
    let doc_comment: Option<String> = row.get("doc_comment")?;
    Ok(SymbolRow {
        id,
        file,
        name,
        lookup_name,
        qualified_name,
        kind,
        parent_id,
        start_byte: start_byte as u32,
        end_byte: end_byte as u32,
        start_line: start_line as u32,
        end_line: end_line as u32,
        signature,
        doc_comment,
    })
}

/// Deletes then inserts `uses` for `file` inside the caller's transaction.
///
/// A `Some(use_id)` is written explicitly so a refresh that reused a stored
/// row keeps the same surrogate key; `None` lets SQLite assign one.
fn replace_file_uses_in_tx(tx: &Connection, file: &str, uses: &[UseRow]) -> Result<(), Error> {
    tx.execute("DELETE FROM uses WHERE file = ?1", params![file])?;
    if uses.is_empty() {
        return Ok(());
    }
    let mut insert = tx.prepare(
        "INSERT INTO uses
             (use_id, file, containing_symbol, scope_key, spelling, lookup_name,
              ref_kind, start_byte, end_byte, line, col, receiver, hint_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
    )?;
    for row in uses {
        insert.execute(params![
            row.use_id,
            row.file,
            row.containing_symbol,
            row.scope_key,
            row.spelling,
            row.lookup_name,
            row.ref_kind.as_str(),
            row.start_byte,
            row.end_byte,
            row.line,
            row.col,
            row.receiver,
            row.hint_json,
        ])?;
    }
    Ok(())
}

/// Maps a `uses` row (selected with [`USE_COLUMNS`]) to a [`UseRow`].
fn use_row_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<UseRow> {
    let use_id: i64 = row.get("use_id")?;
    let file: String = row.get("file")?;
    let containing_symbol: Option<String> = row.get("containing_symbol")?;
    let scope_key: String = row.get("scope_key")?;
    let spelling: String = row.get("spelling")?;
    let lookup_name: String = row.get("lookup_name")?;
    let ref_kind: String = row.get("ref_kind")?;
    let ref_kind = ref_kind.parse::<RefKind>().map_err(|error| {
        // `ref_kind` is the seventh selected column (index 6).
        rusqlite::Error::FromSqlConversionFailure(6, Type::Text, Box::new(error))
    })?;
    let start_byte: i64 = row.get("start_byte")?;
    let end_byte: i64 = row.get("end_byte")?;
    let line: i64 = row.get("line")?;
    let col: i64 = row.get("col")?;
    let receiver: Option<String> = row.get("receiver")?;
    let hint_json: String = row.get("hint_json")?;
    Ok(UseRow {
        use_id: Some(use_id),
        file,
        containing_symbol,
        scope_key,
        spelling,
        lookup_name,
        ref_kind,
        start_byte: start_byte as u32,
        end_byte: end_byte as u32,
        line: line as u32,
        col: col as u32,
        receiver,
        hint_json,
    })
}

/// Deletes then inserts `scopes` for `file` inside the caller's transaction.
fn replace_file_scopes_in_tx(
    tx: &Connection,
    file: &str,
    scopes: &[ScopeRow],
) -> Result<(), Error> {
    tx.execute("DELETE FROM scopes WHERE file = ?1", params![file])?;
    if scopes.is_empty() {
        return Ok(());
    }
    let mut insert = tx.prepare(
        "INSERT INTO scopes (file, scope_key, parent_scope_key, facts_json)
         VALUES (?1, ?2, ?3, ?4)",
    )?;
    for row in scopes {
        insert.execute(params![
            row.file,
            row.scope_key,
            row.parent_scope_key,
            row.facts_json,
        ])?;
    }
    Ok(())
}

/// Maps a `scopes` row (selected with [`SCOPE_COLUMNS`]) to a [`ScopeRow`].
fn scope_row_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScopeRow> {
    Ok(ScopeRow {
        file: row.get("file")?,
        scope_key: row.get("scope_key")?,
        parent_scope_key: row.get("parent_scope_key")?,
        facts_json: row.get("facts_json")?,
    })
}

/// Inserts every resolved binding inside the caller's transaction.
///
/// The caller has already deleted the previous rows and written every use, so
/// each binding's `use_id` foreign key resolves. A duplicate `use_id` aborts
/// the transaction rather than replacing a link silently.
fn insert_bindings_in_tx(tx: &Connection, bindings: &[BindingRow]) -> Result<(), Error> {
    if bindings.is_empty() {
        return Ok(());
    }
    let mut insert =
        tx.prepare("INSERT INTO bindings (use_id, target_id, resolution) VALUES (?1, ?2, ?3)")?;
    for binding in bindings {
        insert.execute(params![
            binding.use_id,
            binding.target_id,
            binding.resolution.as_str(),
        ])?;
    }
    Ok(())
}

/// Inserts every determined receiver class inside the caller's transaction
/// (LR2), after every use exists. A duplicate `use_id` aborts the
/// transaction.
fn insert_receiver_classes_in_tx(tx: &Connection, rows: &[ReceiverClassRow]) -> Result<(), Error> {
    if rows.is_empty() {
        return Ok(());
    }
    let mut insert = tx.prepare(
        "INSERT INTO receiver_classes (use_id, class_qname, class_id) VALUES (?1, ?2, ?3)",
    )?;
    for row in rows {
        insert.execute(params![row.use_id, row.class_qname, row.class_id])?;
    }
    Ok(())
}

/// Maps a `bindings` row to a [`BindingRow`].
fn binding_row_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<BindingRow> {
    let use_id: i64 = row.get("use_id")?;
    let target_id: String = row.get("target_id")?;
    let resolution: String = row.get("resolution")?;
    let resolution = resolution.parse::<Resolution>().map_err(|error| {
        // `resolution` is the third selected column (index 2).
        rusqlite::Error::FromSqlConversionFailure(2, Type::Text, Box::new(error))
    })?;
    Ok(BindingRow {
        use_id,
        target_id,
        resolution,
    })
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
    conn.pragma_update(None, "busy_timeout", DEFAULT_BUSY_TIMEOUT_MS)?;
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

/// Puts an accepted database into WAL mode, waiting out competing openers.
///
/// A database already in WAL mode is left alone: `PRAGMA journal_mode` is only
/// queried. Switching a new database from its rollback journal is a write made
/// by the pragma's own statement after it has taken a read (SHARED) lock. When
/// another connection holds the write lock, as a competing opener does while it
/// creates the schema or switches the mode itself, SQLite refuses to wait on
/// that read-to-write upgrade (deadlock avoidance) and returns `SQLITE_BUSY`
/// ("database is locked") at once, without calling the busy handler. Several
/// processes creating the cache together hit this. The switch is therefore
/// retried until it succeeds or the documented writer busy timeout elapses,
/// which is [`Error::WriterLocked`]. Only a database `initialize` has already
/// accepted as current reaches this, so a refused database is never modified
/// (spec §27).
fn enable_wal(conn: &Connection) -> Result<(), Error> {
    let deadline = std::time::Instant::now() + WRITER_BUSY_TIMEOUT;
    loop {
        let attempt = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
            .and_then(|mode| {
                if mode.eq_ignore_ascii_case("wal") {
                    Ok(mode)
                } else {
                    conn.query_row("PRAGMA journal_mode = WAL", [], |row| {
                        row.get::<_, String>(0)
                    })
                }
            });
        let is_wal = |mode: &String| mode.eq_ignore_ascii_case("wal");
        // Another connection holding a lock makes SQLite either fail with
        // `SQLITE_BUSY` or report the old mode instead of switching; both are
        // waited out.
        let retryable = match &attempt {
            Ok(mode) => !is_wal(mode),
            Err(error) => is_busy(error),
        };
        if retryable && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }
        match attempt {
            Ok(mode) if is_wal(&mode) => return Ok(()),
            Ok(mode) => {
                return Err(Error::Configuration {
                    detail: format!("journal_mode stayed {mode:?} instead of WAL"),
                });
            }
            Err(error) if is_busy(&error) => {
                return Err(Error::WriterLocked {
                    path: conn.path().unwrap_or(":memory:").to_string(),
                    timeout_ms: DEFAULT_BUSY_TIMEOUT_MS as u64,
                });
            }
            Err(error) => return Err(error.into()),
        }
    }
}

/// Creates the schema when empty, or validates an existing format version.
///
/// When `rebuild` allows the stored version, the disposable cache is torn down
/// and recreated instead of being refused.
fn initialize(conn: &Connection, rebuild: Rebuild) -> Result<(), Error> {
    // Fast path without a lock: a current database needs no write, and an
    // incompatible one is refused without being modified (spec §27).
    match read_existing_version(conn)? {
        Some(found) if found == INDEX_FORMAT_VERSION => return ensure_additive_indexes(conn),
        Some(found) if !rebuild.allows(&found) => {
            return Err(Error::IncompatibleIndexFormat { found });
        }
        _ => {}
    }

    // Creating or rebuilding the schema writes it, so it happens under the
    // writer lock and the version is read again inside the lock: a competing
    // process may have created or rebuilt the schema since the unlocked read,
    // and a second `CREATE TABLE` would fail (ARCHITECTURE "Concurrency and
    // source consistency": an index-format mismatch "rebuilds the disposable
    // cache under the writer lock"). `PRAGMA foreign_keys` is a no-op inside a
    // transaction, so a rebuild disables it before `BEGIN IMMEDIATE`.
    let allow_rebuild = rebuild != Rebuild::Never;
    if allow_rebuild {
        conn.pragma_update(None, "foreign_keys", "OFF")?;
    }
    let result = initialize_locked(conn, rebuild);
    if allow_rebuild {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        if !pragma_foreign_keys(conn)? {
            return Err(Error::Configuration {
                detail: "PRAGMA foreign_keys did not remain ON after rebuild".to_string(),
            });
        }
    }
    result
}

/// Creates or rebuilds the schema inside one `BEGIN IMMEDIATE` transaction,
/// deciding from the format version read under the lock.
fn initialize_locked(conn: &Connection, rebuild: Rebuild) -> Result<(), Error> {
    if let Err(error) = conn.execute_batch("BEGIN IMMEDIATE") {
        return Err(if is_busy(&error) {
            Error::WriterLocked {
                path: conn.path().unwrap_or(":memory:").to_string(),
                timeout_ms: DEFAULT_BUSY_TIMEOUT_MS as u64,
            }
        } else {
            error.into()
        });
    }
    let outcome = match read_existing_version(conn) {
        Ok(None) => create_schema(conn),
        Ok(Some(found)) if found == INDEX_FORMAT_VERSION => {
            conn.execute_batch(PARENT_INDEX_SQL).map_err(Error::from)
        }
        Ok(Some(found)) if rebuild.allows(&found) => rebuild_schema(conn),
        Ok(Some(found)) => Err(Error::IncompatibleIndexFormat { found }),
        Err(error) => Err(error),
    };
    match outcome {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

/// Adds the additive indexes a version-1 database created before them lacks
/// (PF1: [`PARENT_INDEX_NAME`]).
///
/// The presence check is a read, so a current database is not written. A
/// missing index is created in its own short write transaction under the
/// connection's busy timeout. The index affects only how fast facts are
/// deleted, never a stored fact or a query result, so when it cannot be
/// created because another process holds the writer lock past the timeout, or
/// the database is read-only, the store opens without it and a later open adds
/// it. Any other failure is an error.
fn ensure_additive_indexes(conn: &Connection) -> Result<(), Error> {
    let present: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?1",
        params![PARENT_INDEX_NAME],
        |row| row.get(0),
    )?;
    if present > 0 {
        return Ok(());
    }
    match conn.execute_batch(PARENT_INDEX_SQL) {
        Ok(()) => Ok(()),
        Err(error) if is_busy(&error) || is_read_only(&error) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Whether `error` is SQLite refusing a write to a read-only database.
fn is_read_only(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(code, _)
            if code.code == rusqlite::ErrorCode::ReadOnly
    )
}

/// Drops every user table and recreates the current schema inside the
/// caller's transaction.
///
/// The caller has disabled foreign keys so tables can be dropped in any order.
/// Used only by [`Store::open_rebuildable`] on a database already accepted by
/// [`validate_destination`].
fn rebuild_schema(conn: &Connection) -> Result<(), Error> {
    let tables: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        )?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        rows.collect::<rusqlite::Result<Vec<String>>>()?
    };
    for name in &tables {
        conn.execute_batch(&format!("DROP TABLE IF EXISTS {}", quote_identifier(name)))?;
    }
    create_schema(conn)
}

/// Quotes a SQLite identifier by doubling embedded double quotes.
fn quote_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
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

/// Creates the full current schema and records the format version inside
/// the caller's transaction.
fn create_schema(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch(SCHEMA_SQL)?;
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)",
        params!["index_format_version", INDEX_FORMAT_VERSION],
    )?;
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

/// The stored columns of a previously published `files` row that decide
/// whether it changed, without its source bytes.
#[derive(Debug)]
struct PreviousFile {
    content_hash: Option<String>,
    parse_status: ParseStatus,
    language: Option<String>,
    has_source: bool,
    mtime_ns: i64,
    size: u64,
}

impl PreviousFile {
    /// Whether `file` has this row's content hash, parse status, language,
    /// and source presence: the row counts as `unchanged`, its source column
    /// needs no write (the hash covers the stored bytes), and its facts are
    /// the stored facts unless the caller regenerated them.
    fn same_content(&self, file: &FileRow) -> bool {
        self.content_hash == file.content_hash
            && self.parse_status == file.parse_status
            && self.language == file.language
            && self.has_source == file.source.is_some()
    }
}

/// Loads the current inventory keyed by path, without source bytes.
fn load_previous_inventory(tx: &Connection) -> Result<HashMap<String, PreviousFile>, Error> {
    let mut stmt = tx.prepare(
        "SELECT path, content_hash, parse_status, language, source IS NOT NULL, mtime_ns, size
         FROM files",
    )?;
    let rows = stmt.query_map([], |row| {
        let status: String = row.get(2)?;
        let parse_status = status.parse::<ParseStatus>().map_err(|error| {
            // `parse_status` is the third selected column (index 2).
            rusqlite::Error::FromSqlConversionFailure(2, Type::Text, Box::new(error))
        })?;
        let size: i64 = row.get(6)?;
        Ok((
            row.get::<_, String>(0)?,
            PreviousFile {
                content_hash: row.get(1)?,
                parse_status,
                language: row.get(3)?,
                has_source: row.get(4)?,
                mtime_ns: row.get(5)?,
                size: size as u64,
            },
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<HashMap<_, _>>>()?)
}

/// Writes the four fingerprints and the snapshot digest into `meta`.
fn write_fingerprint(
    tx: &Connection,
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
    for (key, value) in entries {
        upsert_meta_if_changed(tx, key, value)?;
    }
    Ok(())
}

/// Inserts or updates one `meta` value, modifying no row when the stored value
/// already equals `value` (PF1: a no-change refresh writes nothing).
fn upsert_meta_if_changed(tx: &Connection, key: &str, value: &str) -> Result<(), Error> {
    let mut upsert = tx.prepare_cached(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value
         WHERE meta.value IS NOT excluded.value",
    )?;
    upsert.execute(params![key, value])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        BindingRow, Error, FileRow, Fingerprint, INDEX_FORMAT_VERSION, InventoryInput,
        PublishReport, ScopeRow, StagePlan, Store, SymbolRow, UseRow, WRITER_BUSY_TIMEOUT,
        clamp_mtime_ns, snapshot_digest,
    };
    use rivet_core::{ParseStatus, RefKind, Resolution, SymbolKind, content_hash};
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
        InventoryInput {
            fingerprint,
            files,
            symbols: Vec::new(),
            uses: Vec::new(),
            scopes: Vec::new(),
            bindings: Vec::new(),
            receiver_classes: Vec::new(),
            force: false,
            regenerated: Vec::new(),
        }
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
    fn forced_publish_rebuilds_all_rows_and_keeps_digest() {
        let mut store = Store::open_in_memory().unwrap();
        let files = vec![sample_file("a.php"), sample_file("b.php")];
        let before = publish(&mut store, files.clone());

        let after = store
            .publish_inventory(InventoryInput {
                fingerprint: sample_fingerprint(),
                files,
                symbols: Vec::new(),
                uses: Vec::new(),
                scopes: Vec::new(),
                bindings: Vec::new(),
                receiver_classes: Vec::new(),
                force: true,
                regenerated: Vec::new(),
            })
            .unwrap();
        assert_eq!((after.updated, after.unchanged, after.deleted), (2, 0, 0));
        assert_eq!(after.digest, before.digest, "content is unchanged");
        assert_eq!(store.list_files().unwrap().len(), 2);
    }

    #[test]
    fn open_rebuildable_recreates_incompatible_schema() {
        let temp = TempDir::new("rebuild");
        let rivet_dir = temp.path();
        {
            let store = Store::open(rivet_dir).unwrap();
            store.set_meta("index_format_version", "99").unwrap();
            store.upsert_file(&sample_file("a.php")).unwrap();
        }

        // A plain open still refuses the incompatible database.
        assert!(matches!(
            Store::open(rivet_dir).unwrap_err(),
            Error::IncompatibleIndexFormat { .. }
        ));

        let store = Store::open_rebuildable(rivet_dir).unwrap();
        assert_eq!(
            store.get_meta("index_format_version").unwrap().as_deref(),
            Some(INDEX_FORMAT_VERSION)
        );
        assert!(
            store.list_files().unwrap().is_empty(),
            "the rebuild must discard the old inventory"
        );
        assert!(store.pragma_foreign_keys().unwrap());
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

    /// Whether the PF1 `symbols.parent_id` index exists, and whether SQLite
    /// uses it to find a symbol's children.
    fn parent_index_state(conn: &rusqlite::Connection) -> (bool, String) {
        let present: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' \
                 AND name = 'symbols_parent' AND tbl_name = 'symbols'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let plan: String = conn
            .query_row(
                "EXPLAIN QUERY PLAN SELECT id FROM symbols WHERE parent_id = 'x'",
                [],
                |row| row.get(3),
            )
            .unwrap();
        (present == 1, plan)
    }

    #[test]
    fn a_fresh_store_indexes_symbol_parent_ids() {
        let store = Store::open_in_memory().unwrap();
        let (present, plan) = parent_index_state(&store.conn);
        assert!(present);
        assert!(plan.contains("symbols_parent"), "plan: {plan}");

        let temp = TempDir::new("parent-index-fresh");
        let store = Store::open(temp.path()).unwrap();
        assert!(parent_index_state(&store.conn).0);
    }

    #[test]
    fn opening_a_store_that_predates_the_parent_index_adds_it_without_a_rebuild() {
        let temp = TempDir::new("parent-index-old");
        {
            let mut store = Store::open(temp.path()).unwrap();
            publish(&mut store, vec![sample_file("a.php")]);
            store
                .replace_file_symbols("a.php", &[sample_symbol("a.php", "a.php#A", "A", 0)])
                .unwrap();
            // Recreate the pre-PF1 database: the same version-1 schema without
            // the index.
            store
                .conn
                .execute_batch("DROP INDEX symbols_parent")
                .unwrap();
            assert!(!parent_index_state(&store.conn).0);
        }
        let digest_before = {
            let conn = rusqlite::Connection::open(temp.path().join("index.db")).unwrap();
            conn.query_row(
                "SELECT value FROM meta WHERE key = 'snapshot_digest'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap()
        };

        let store = Store::open(temp.path()).unwrap();
        assert!(parent_index_state(&store.conn).0, "open adds the index");
        // No format bump and no rebuild: version, digest, and facts are kept.
        assert_eq!(
            store.get_meta("index_format_version").unwrap().as_deref(),
            Some(INDEX_FORMAT_VERSION)
        );
        assert_eq!(
            store.get_meta("snapshot_digest").unwrap(),
            Some(digest_before)
        );
        assert_eq!(store.list_symbols().unwrap().len(), 1);
        assert_eq!(store.integrity_check().unwrap(), vec!["ok".to_string()]);
        drop(store);

        // Reopening a current database writes nothing.
        let store = Store::open(temp.path()).unwrap();
        assert_eq!(store.total_changes(), 0);
        assert!(parent_index_state(&store.conn).0);
    }

    #[test]
    fn republishing_an_unchanged_inventory_modifies_no_row() {
        let mut store = Store::open_in_memory().unwrap();
        let files = vec![sample_file("a.php"), sample_file("b.php")];
        let first = publish(&mut store, files.clone());

        // A full-plan republish of identical rows (with no facts) changes no
        // `files` row and no `meta` value.
        let before = store.total_changes();
        let again = publish(&mut store, files.clone());
        assert_eq!(again.digest, first.digest);
        assert_eq!(store.total_changes(), before, "no file, fact, or meta row");

        // A keep-everything plan on the same inventory writes nothing at all.
        let before = store.total_changes();
        let mut txn = store.begin_write(WRITER_BUSY_TIMEOUT).unwrap();
        let staged = txn
            .stage_refresh(
                inventory(sample_fingerprint(), files.clone()),
                StagePlan {
                    reparsed: Some(Default::default()),
                    replace_bindings: false,
                },
            )
            .unwrap();
        let report = txn.commit(staged).unwrap();
        assert_eq!(report.digest, first.digest);
        assert_eq!((report.updated, report.unchanged), (0, 2));
        assert_eq!(store.total_changes(), before);

        // An mtime-only change updates exactly that row's metadata.
        let mut touched = files;
        touched[1].mtime_ns += 1;
        let before = store.total_changes();
        let report = publish(&mut store, touched);
        assert_eq!(report.digest, first.digest);
        assert_eq!(store.total_changes(), before + 1);
        assert_eq!(
            store.get_file("b.php").unwrap().unwrap().mtime_ns,
            sample_file("b.php").mtime_ns + 1
        );
    }

    #[test]
    fn keeping_stored_bindings_is_refused_when_content_or_membership_changed() {
        let keep = StagePlan {
            reparsed: Some(Default::default()),
            replace_bindings: false,
        };
        let cases: Vec<(&str, Vec<FileRow>)> = vec![
            (
                "changed content",
                vec![sample_file("a.php"), file_with("b.php", b"<?php //")],
            ),
            ("deleted file", vec![sample_file("a.php")]),
            (
                "new file",
                vec![
                    sample_file("a.php"),
                    sample_file("b.php"),
                    sample_file("c.php"),
                ],
            ),
        ];
        for (label, files) in cases {
            let mut store = Store::open_in_memory().unwrap();
            let first = publish(&mut store, vec![sample_file("a.php"), sample_file("b.php")]);
            let mut txn = store.begin_write(WRITER_BUSY_TIMEOUT).unwrap();
            let error = txn
                .stage_refresh(inventory(sample_fingerprint(), files), keep.clone())
                .expect_err(label);
            assert!(matches!(error, Error::TransactionState { .. }), "{label}");
            drop(txn);
            assert_eq!(
                store.get_meta("snapshot_digest").unwrap().as_deref(),
                Some(first.digest.as_str()),
                "{label}: nothing is published"
            );
        }

        // Supplying bindings with a keep plan is refused as well.
        let mut store = Store::open_in_memory().unwrap();
        publish(&mut store, vec![sample_file("a.php")]);
        let mut input = inventory(sample_fingerprint(), vec![sample_file("a.php")]);
        input.bindings = vec![BindingRow {
            use_id: 1,
            target_id: "a.php#A".to_string(),
            resolution: Resolution::Exact,
        }];
        let mut txn = store.begin_write(WRITER_BUSY_TIMEOUT).unwrap();
        assert!(matches!(
            txn.stage_refresh(input, keep),
            Err(Error::TransactionState { .. })
        ));
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
                "receiver_classes",
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

    /// One symbol row for `file` with a distinct id.
    fn sample_symbol(file: &str, id: &str, name: &str, start_byte: u32) -> SymbolRow {
        SymbolRow {
            id: id.to_string(),
            file: file.to_string(),
            name: name.to_string(),
            lookup_name: name.to_lowercase(),
            qualified_name: format!("App\\{name}"),
            kind: SymbolKind::Method,
            parent_id: None,
            start_byte,
            end_byte: start_byte + 5,
            start_line: 1,
            end_line: 1,
            signature: None,
            doc_comment: None,
        }
    }

    #[test]
    fn replace_file_symbols_deletes_then_inserts_in_one_transaction() {
        let mut store = Store::open_in_memory().unwrap();
        store.upsert_file(&sample_file("a.php")).unwrap();
        store.upsert_file(&sample_file("b.php")).unwrap();

        store
            .replace_file_symbols("a.php", &[sample_symbol("a.php", "a#one", "one", 0)])
            .unwrap();
        store
            .replace_file_symbols("a.php", &[sample_symbol("a.php", "a#two", "two", 10)])
            .unwrap();
        store
            .replace_file_symbols("b.php", &[sample_symbol("b.php", "b#three", "three", 0)])
            .unwrap();

        assert!(store.get_symbol("a#one").unwrap().is_none());
        assert_eq!(store.get_symbol("a#two").unwrap().unwrap().name, "two");
        assert_eq!(store.get_symbol("b#three").unwrap().unwrap().name, "three");
        assert_eq!(store.list_symbols().unwrap().len(), 2);
    }

    #[test]
    fn symbol_reads_are_ordered_by_file_then_start_byte_then_id() {
        let mut store = Store::open_in_memory().unwrap();
        for path in ["z.php", "a.php", "m.php"] {
            store.upsert_file(&sample_file(path)).unwrap();
        }
        // Deliberately out of order, including a same-file tie.
        let rows = vec![
            sample_symbol("z.php", "z#x", "same", 0),
            sample_symbol("a.php", "a#b", "same", 5),
            sample_symbol("a.php", "a#a", "same", 5),
            sample_symbol("m.php", "m#x", "same", 1),
        ];
        store
            .publish_inventory(InventoryInput {
                fingerprint: sample_fingerprint(),
                files: vec![
                    sample_file("z.php"),
                    sample_file("a.php"),
                    sample_file("m.php"),
                ],
                symbols: rows,
                uses: Vec::new(),
                scopes: Vec::new(),
                bindings: Vec::new(),
                receiver_classes: Vec::new(),
                force: false,
                regenerated: Vec::new(),
            })
            .unwrap();

        let ids: Vec<String> = store
            .find_symbols_by_lookup_name("same")
            .unwrap()
            .into_iter()
            .map(|row| row.id)
            .collect();
        assert_eq!(ids, vec!["a#a", "a#b", "m#x", "z#x"]);

        let by_qname = store.find_symbols_by_qualified_name("App\\same").unwrap();
        assert_eq!(by_qname.len(), 4);
        assert!(
            store
                .find_symbols_by_lookup_name("nope")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn publishing_parse_error_clears_stale_symbols() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .publish_inventory(InventoryInput {
                fingerprint: sample_fingerprint(),
                files: vec![sample_file("a.php")],
                symbols: vec![sample_symbol("a.php", "a#one", "one", 0)],
                uses: Vec::new(),
                scopes: Vec::new(),
                bindings: Vec::new(),
                receiver_classes: Vec::new(),
                force: false,
                regenerated: Vec::new(),
            })
            .unwrap();
        assert_eq!(store.list_symbols().unwrap().len(), 1);

        let mut failed = sample_file("a.php");
        failed.parse_status = ParseStatus::ParseError;
        failed.source = None;
        store
            .publish_inventory(InventoryInput {
                fingerprint: sample_fingerprint(),
                files: vec![failed],
                symbols: Vec::new(),
                uses: Vec::new(),
                scopes: Vec::new(),
                bindings: Vec::new(),
                receiver_classes: Vec::new(),
                force: false,
                regenerated: Vec::new(),
            })
            .unwrap();
        assert!(store.list_symbols().unwrap().is_empty());
    }

    /// A fresh use row (no SQLite ID) with a top-level container and an
    /// unresolved hint; callers mutate the fields they need to vary.
    fn sample_use(
        file: &str,
        spelling: &str,
        lookup_name: &str,
        ref_kind: RefKind,
        start_byte: u32,
        end_byte: u32,
    ) -> UseRow {
        UseRow {
            use_id: None,
            file: file.to_string(),
            containing_symbol: None,
            scope_key: "top:file".to_string(),
            spelling: spelling.to_string(),
            lookup_name: lookup_name.to_string(),
            ref_kind,
            start_byte,
            end_byte,
            line: 1,
            col: start_byte + 1,
            receiver: None,
            hint_json: "{\"kind\":\"unresolved\"}".to_string(),
        }
    }

    /// One scope row for `file` with the given key and JSON facts.
    fn sample_scope(
        file: &str,
        scope_key: &str,
        parent: Option<&str>,
        facts_json: &str,
    ) -> ScopeRow {
        ScopeRow {
            file: file.to_string(),
            scope_key: scope_key.to_string(),
            parent_scope_key: parent.map(str::to_string),
            facts_json: facts_json.to_string(),
        }
    }

    #[test]
    fn bindings_round_trip_and_are_replaced_on_publish() {
        let mut store = Store::open_in_memory().unwrap();
        let facts_json =
            "{\"imports\":[],\"typed_bindings\":[],\"new_bindings\":[],\"declares\":[]}";
        let symbol = sample_symbol("a.php", "a.php#App\\launch", "launch", 0);
        let mut use_row = sample_use("a.php", "launch", "launch", RefKind::Call, 179, 185);
        use_row.use_id = Some(7);
        store
            .publish_inventory(InventoryInput {
                fingerprint: sample_fingerprint(),
                files: vec![sample_file("a.php")],
                symbols: vec![symbol.clone()],
                uses: vec![use_row],
                scopes: vec![sample_scope("a.php", "top:file", None, facts_json)],
                bindings: vec![BindingRow {
                    use_id: 7,
                    target_id: symbol.id.clone(),
                    resolution: Resolution::Exact,
                }],
                receiver_classes: Vec::new(),
                force: false,
                regenerated: Vec::new(),
            })
            .unwrap();

        let bindings = store.list_bindings().unwrap();
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].use_id, 7);
        assert_eq!(bindings[0].target_id, symbol.id);
        assert_eq!(bindings[0].resolution, Resolution::Exact);

        // Re-publishing with no bindings clears the table: every publish
        // re-resolves all uses (spec §12.3).
        store
            .publish_inventory(InventoryInput {
                fingerprint: sample_fingerprint(),
                files: vec![sample_file("a.php")],
                symbols: vec![symbol],
                uses: Vec::new(),
                scopes: vec![sample_scope("a.php", "top:file", None, facts_json)],
                bindings: Vec::new(),
                receiver_classes: Vec::new(),
                force: false,
                regenerated: Vec::new(),
            })
            .unwrap();
        assert!(store.list_bindings().unwrap().is_empty());
    }

    #[test]
    fn receiver_classes_round_trip_and_are_replaced_with_bindings() {
        let mut store = Store::open_in_memory().unwrap();
        let facts_json =
            "{\"imports\":[],\"typed_bindings\":[],\"new_bindings\":[],\"declares\":[]}";
        let class = sample_symbol("a.php", "a.php#App\\K", "K", 0);
        let mut indexed = sample_use("a.php", "save", "save", RefKind::Call, 10, 14);
        indexed.use_id = Some(3);
        let mut vendor = sample_use("a.php", "save", "save", RefKind::Call, 20, 24);
        vendor.use_id = Some(4);
        use super::ReceiverClassRow;
        let rows = vec![
            ReceiverClassRow {
                use_id: 3,
                class_qname: "App\\K".to_string(),
                class_id: Some(class.id.clone()),
            },
            ReceiverClassRow {
                use_id: 4,
                class_qname: "Vendor\\Request".to_string(),
                class_id: None,
            },
        ];
        let input = |uses: Vec<UseRow>, receiver_classes: Vec<ReceiverClassRow>| InventoryInput {
            fingerprint: sample_fingerprint(),
            files: vec![sample_file("a.php")],
            symbols: vec![class.clone()],
            uses,
            scopes: vec![sample_scope("a.php", "top:file", None, facts_json)],
            bindings: Vec::new(),
            receiver_classes,
            force: false,
            regenerated: Vec::new(),
        };
        store
            .publish_inventory(input(vec![indexed.clone(), vendor.clone()], rows.clone()))
            .unwrap();
        assert_eq!(store.list_receiver_classes().unwrap(), rows);

        // Supplying rows with a plan that keeps the stored bindings is refused.
        let keep = StagePlan {
            reparsed: Some(std::collections::HashSet::new()),
            replace_bindings: false,
        };
        let mut txn = store.begin_write(WRITER_BUSY_TIMEOUT).unwrap();
        assert!(matches!(
            txn.stage_refresh(input(vec![indexed, vendor], rows), keep),
            Err(Error::TransactionState { .. })
        ));
        drop(txn);

        // Every publish that replaces bindings replaces these rows too.
        store
            .publish_inventory(input(Vec::new(), Vec::new()))
            .unwrap();
        assert!(store.list_receiver_classes().unwrap().is_empty());
    }

    #[test]
    fn use_row_round_trips_with_null_container_and_non_ascii_receiver() {
        let mut store = Store::open_in_memory().unwrap();
        let facts_json =
            "{\"imports\":[],\"typed_bindings\":[],\"new_bindings\":[],\"declares\":[]}";
        let mut use_row = sample_use("a.php", "launch", "launch", RefKind::Call, 179, 185);
        use_row.receiver = Some("héllo→".to_string());
        store
            .publish_inventory(InventoryInput {
                fingerprint: sample_fingerprint(),
                files: vec![sample_file("a.php")],
                symbols: Vec::new(),
                uses: vec![use_row.clone()],
                scopes: vec![sample_scope("a.php", "top:file", None, facts_json)],
                bindings: Vec::new(),
                receiver_classes: Vec::new(),
                force: false,
                regenerated: Vec::new(),
            })
            .unwrap();

        let rows = store.list_uses_for_file("a.php").unwrap();
        assert_eq!(rows.len(), 1);
        let got = &rows[0];
        assert!(got.use_id.is_some(), "SQLite must assign a use_id");
        assert_eq!(got.containing_symbol, None, "NULL container must persist");
        assert_eq!(got.receiver.as_deref(), Some("héllo→"));
        assert_eq!(got.hint_json, "{\"kind\":\"unresolved\"}");
        assert_eq!(
            UseRow {
                use_id: got.use_id,
                ..use_row
            },
            *got
        );

        let scopes = store.list_scopes_for_file("a.php").unwrap();
        assert_eq!(
            scopes,
            vec![sample_scope("a.php", "top:file", None, facts_json)]
        );
    }

    #[test]
    fn find_uses_by_lookup_name_is_byte_ordered() {
        let mut store = Store::open_in_memory().unwrap();
        let rows = vec![
            sample_use("a.php", "Same", "same", RefKind::Type, 10, 14),
            sample_use("b.php", "same", "same", RefKind::Write, 1, 5),
            sample_use("a.php", "same", "same", RefKind::Call, 5, 9),
            sample_use("a.php", "same", "same", RefKind::Read, 5, 9),
        ];
        store
            .publish_inventory(InventoryInput {
                fingerprint: sample_fingerprint(),
                files: vec![sample_file("a.php"), sample_file("b.php")],
                symbols: Vec::new(),
                uses: rows,
                scopes: Vec::new(),
                bindings: Vec::new(),
                receiver_classes: Vec::new(),
                force: false,
                regenerated: Vec::new(),
            })
            .unwrap();

        let ordered: Vec<(String, u32, RefKind)> = store
            .find_uses_by_lookup_name("same")
            .unwrap()
            .into_iter()
            .map(|row| (row.file, row.start_byte, row.ref_kind))
            .collect();
        assert_eq!(
            ordered,
            vec![
                ("a.php".to_string(), 5, RefKind::Call),
                ("a.php".to_string(), 5, RefKind::Read),
                ("a.php".to_string(), 10, RefKind::Type),
                ("b.php".to_string(), 1, RefKind::Write),
            ]
        );
        assert!(store.find_uses_by_lookup_name("absent").unwrap().is_empty());
    }

    #[test]
    fn deleting_a_file_cascades_to_uses_and_scopes() {
        let mut store = Store::open_in_memory().unwrap();
        let facts_json =
            "{\"imports\":[],\"typed_bindings\":[],\"new_bindings\":[],\"declares\":[]}";
        store
            .publish_inventory(InventoryInput {
                fingerprint: sample_fingerprint(),
                files: vec![sample_file("a.php")],
                symbols: Vec::new(),
                uses: vec![sample_use("a.php", "g", "g", RefKind::Call, 0, 1)],
                scopes: vec![sample_scope("a.php", "top:file", None, facts_json)],
                bindings: Vec::new(),
                receiver_classes: Vec::new(),
                force: false,
                regenerated: Vec::new(),
            })
            .unwrap();

        assert_eq!(store.list_uses_for_file("a.php").unwrap().len(), 1);
        assert_eq!(store.list_scopes_for_file("a.php").unwrap().len(), 1);

        store.delete_file("a.php").unwrap();
        assert!(store.list_uses_for_file("a.php").unwrap().is_empty());
        assert!(store.list_scopes_for_file("a.php").unwrap().is_empty());
    }

    /// A second connection cannot take the writer lock while the first holds
    /// it: it fails with `WriterLocked` naming the database once the busy
    /// timeout elapses, and succeeds after the first commits.
    #[test]
    fn writer_lock_is_exclusive_and_bounded_by_the_busy_timeout() {
        let temp = TempDir::new("writer-lock");
        let mut first = Store::open(temp.path()).unwrap();
        publish(&mut first, vec![sample_file("a.php")]);
        let second = Store::open(temp.path()).unwrap();

        let mut txn = first
            .begin_write(std::time::Duration::from_secs(5))
            .unwrap();
        let started = std::time::Instant::now();
        match second.begin_write(std::time::Duration::from_millis(150)) {
            Err(Error::WriterLocked { path, timeout_ms }) => {
                assert!(path.ends_with("index.db"), "{path}");
                assert_eq!(timeout_ms, 150);
            }
            other => panic!("expected WriterLocked, got {other:?}"),
        }
        assert!(started.elapsed() >= std::time::Duration::from_millis(140));
        let staged = txn
            .stage_inventory(inventory(sample_fingerprint(), vec![sample_file("b.php")]))
            .unwrap();
        txn.commit(staged).unwrap();

        let txn = second
            .begin_write(std::time::Duration::from_millis(150))
            .unwrap();
        txn.rollback().unwrap();
        assert_eq!(second.list_files().unwrap().len(), 1);
    }

    /// Dropping a write transaction after staging rolls every row back, and a
    /// transaction cannot be nested inside another.
    #[test]
    fn dropped_write_transaction_rolls_back_staged_rows() {
        let temp = TempDir::new("drop-rollback");
        let mut store = Store::open(temp.path()).unwrap();
        let before = publish(&mut store, vec![sample_file("a.php")]);
        {
            let mut txn = store
                .begin_write(std::time::Duration::from_secs(5))
                .unwrap();
            txn.stage_inventory(inventory(sample_fingerprint(), vec![sample_file("b.php")]))
                .unwrap();
            // Reads made through the store join the open transaction.
            let inside: Vec<String> = txn
                .store()
                .list_files()
                .unwrap()
                .into_iter()
                .map(|file| file.path)
                .collect();
            assert_eq!(inside, vec!["b.php".to_string()]);
            assert!(matches!(
                txn.store().begin_snapshot_read(),
                Err(Error::TransactionState { .. })
            ));
        }
        let after: Vec<String> = store
            .list_files()
            .unwrap()
            .into_iter()
            .map(|file| file.path)
            .collect();
        assert_eq!(after, vec!["a.php".to_string()]);
        assert_eq!(
            store.get_meta("snapshot_digest").unwrap(),
            Some(before.digest)
        );
        assert_eq!(store.integrity_check().unwrap(), vec!["ok".to_string()]);
    }

    /// A read transaction keeps seeing the snapshot whose digest it returned,
    /// rows and stored source included, while another connection publishes.
    #[test]
    fn snapshot_read_is_isolated_from_a_concurrent_publish() {
        let temp = TempDir::new("snapshot-read");
        let mut writer = Store::open(temp.path()).unwrap();
        let first = publish(&mut writer, vec![file_with("a.php", b"<?php // one")]);
        let reader = Store::open(temp.path()).unwrap();

        let pinned = reader.begin_snapshot_read().unwrap();
        assert_eq!(pinned, Some(first.digest.clone()));
        let second = publish(
            &mut writer,
            vec![
                file_with("a.php", b"<?php // two"),
                file_with("b.php", b"<?php"),
            ],
        );
        assert_ne!(second.digest, first.digest);

        let files = reader.list_files().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(
            reader.get_file("a.php").unwrap().unwrap().source,
            Some(b"<?php // one".to_vec())
        );
        assert_eq!(
            reader.get_meta("snapshot_digest").unwrap(),
            Some(first.digest)
        );

        reader.end_snapshot_read().unwrap();
        assert_eq!(
            reader.get_meta("snapshot_digest").unwrap(),
            Some(second.digest)
        );
        assert_eq!(reader.list_files().unwrap().len(), 2);
    }

    /// The exact failure behind the flaky first-refresh test: opening a
    /// compatible database still in rollback-journal mode while another
    /// connection holds its write lock. `PRAGMA journal_mode = WAL` then
    /// returned `SQLITE_BUSY` ("database is locked") at once, without the busy
    /// handler. The open must instead wait for the lock and then switch to WAL.
    #[test]
    fn open_waits_for_a_held_lock_before_switching_to_wal() {
        let temp = TempDir::new("wal-switch");
        let db = temp.path().join("index.db");
        let holder = rusqlite::Connection::open(&db).unwrap();
        holder.execute_batch(super::SCHEMA_SQL).unwrap();
        holder
            .execute(
                "INSERT INTO meta (key, value) VALUES ('index_format_version', ?1)",
                params![INDEX_FORMAT_VERSION],
            )
            .unwrap();
        let mode: String = holder
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        // Hold the RESERVED (write) lock, as a competing opener does while it
        // creates the schema or switches the journal mode. The pragma's own
        // statement takes a SHARED lock and then tries to upgrade to write;
        // SQLite refuses to wait on an upgrade from a held read (deadlock
        // avoidance), so the busy handler is never called.
        holder.execute_batch("BEGIN IMMEDIATE").unwrap();

        let hold = std::time::Duration::from_millis(300);
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(hold);
            holder.execute_batch("COMMIT").unwrap();
        });
        let started = std::time::Instant::now();
        let store = Store::open(temp.path()).expect("open waits for the lock");
        // It could only have switched once the lock was released.
        assert!(started.elapsed() >= std::time::Duration::from_millis(250));
        releaser.join().unwrap();
        let mode: String = store
            .conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        assert_eq!(store.integrity_check().unwrap(), vec!["ok".to_string()]);
    }
}
