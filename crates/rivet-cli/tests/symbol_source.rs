//! Integration tests for `rivet symbol --source` and stored signatures (T14).
//!
//! They drive the built binary against temporary copies of the authored PHP
//! fixture so `--source` bytes, persisted signatures/doc comments, and the
//! current refresh behavior are exercised end to end.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

/// The binary under test, supplied by Cargo for integration tests.
const RIVET: &str = env!("CARGO_BIN_EXE_rivet");

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
            "rivet-source-{label}-{}-{nanos}-{unique}",
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

/// A temporary Git root holding a byte-for-byte copy of the authored fixture.
fn fixture_repo(label: &str) -> TempDir {
    let temp = TempDir::new(label);
    fs::create_dir_all(temp.path().join(".git")).expect("create .git");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/php/authored");
    for entry in fs::read_dir(&source).expect("read authored fixture") {
        let entry = entry.expect("fixture entry");
        if entry.path().is_file() {
            fs::copy(entry.path(), temp.path().join(entry.file_name())).expect("copy fixture file");
        }
    }
    temp
}

/// Runs the binary in `dir` and returns the captured output.
fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(RIVET)
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run the rivet binary")
}

/// Parses a success object, requiring exit 0 and empty stderr.
fn parse_success(output: &Output) -> Value {
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "success must not write stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("stdout is one JSON object")
}

const LAUNCH: &str = "App\\Services\\SurveyService::launch";
/// The gold `SurveyService::launch` span from `tests/gold/php-authored.toml`.
const LAUNCH_START_BYTE: u64 = 460;
const LAUNCH_END_BYTE: u64 = 546;

#[test]
fn source_is_the_exact_stored_span() {
    let temp = fixture_repo("exact");
    let output = run(temp.path(), &["symbol", LAUNCH, "--source", "--json"]);
    let value = parse_success(&output);

    assert_eq!(value["symbol"]["start_byte"], LAUNCH_START_BYTE);
    assert_eq!(value["symbol"]["end_byte"], LAUNCH_END_BYTE);
    assert_eq!(value["signature"], "public function launch(): void");
    assert!(value["doc_comment"].is_null(), "{value}");

    // The stored bytes equal the live fixture slice for this span.
    let live = fs::read(temp.path().join("SurveyService.php")).expect("read live fixture");
    let expected =
        std::str::from_utf8(&live[LAUNCH_START_BYTE as usize..LAUNCH_END_BYTE as usize]).unwrap();
    assert_eq!(value["source"], expected);
    assert!(expected.starts_with("public function launch"));
}

#[test]
fn doc_comment_is_persisted_and_returned() {
    let temp = fixture_repo("docblock");
    let output = run(
        temp.path(),
        &["symbol", "App\\Documented\\Documented::run", "--json"],
    );
    let value = parse_success(&output);

    assert_eq!(value["signature"], "public function run(): void");
    assert_eq!(
        value["doc_comment"],
        "/**\n * Runs the documented work.\n */"
    );

    let class = run(
        temp.path(),
        &["symbol", "App\\Documented\\Documented", "--json"],
    );
    let class = parse_success(&class);
    assert_eq!(class["signature"], "final class Documented");
    assert_eq!(class["doc_comment"], "/**\n * A documented service.\n */");
}

/// After a live edit, a fresh query refreshes and sees the new span. This will
/// change in T15/T16, when refresh modes make snapshot-vs-live divergence an
/// explicit option; the assertion here records the current refresh-always
/// behavior, not a durable contract.
#[test]
fn live_edit_refreshes_to_the_new_span() {
    let temp = fixture_repo("edit");
    let before = run(temp.path(), &["symbol", LAUNCH, "--source", "--json"]);
    let before = parse_success(&before);
    let before_hash = before["symbol"]["content_hash"].clone();
    let before_source = before["source"].as_str().unwrap().to_string();

    // Give `launch` a longer body without re-running index explicitly.
    let path = temp.path().join("SurveyService.php");
    let original = fs::read_to_string(&path).expect("read fixture");
    let edited = original.replace(
        "    public function launch(): void\n    {\n        $this->label = self::DEFAULT_LABEL;\n    }",
        "    public function launch(): void\n    {\n        $this->label = self::DEFAULT_LABEL;\n        $this->relaunch();\n    }",
    );
    assert_ne!(edited, original, "the edit must change the fixture");
    fs::write(&path, edited).expect("overwrite fixture");

    let after = run(temp.path(), &["symbol", LAUNCH, "--source", "--json"]);
    let after = parse_success(&after);

    assert_ne!(
        after["symbol"]["content_hash"], before_hash,
        "the symbol object's content_hash must change after the edit"
    );
    assert_ne!(after["symbol"]["end_byte"], LAUNCH_END_BYTE);
    let after_source = after["source"].as_str().unwrap();
    assert!(
        after_source.contains("$this->relaunch();"),
        "{after_source:?}"
    );
    assert_ne!(after_source, before_source);
    assert_eq!(
        after["source"].as_str().unwrap().len() as u64,
        after["symbol"]["end_byte"].as_u64().unwrap()
            - after["symbol"]["start_byte"].as_u64().unwrap()
    );
}

#[test]
fn crlf_source_is_preserved_byte_for_byte() {
    let temp = fixture_repo("crlf");
    // Rewrite the fixture with CRLF line endings; every byte `\n` becomes
    // `\r\n`, so the recorded LF offsets shift.
    let lf = fs::read(temp.path().join("SurveyService.php")).expect("read fixture");
    let mut crlf: Vec<u8> = Vec::with_capacity(lf.len() + lf.len() / 20);
    for byte in lf {
        if byte == b'\n' {
            crlf.push(b'\r');
        }
        crlf.push(byte);
    }
    fs::write(temp.path().join("SurveyService.php"), &crlf).expect("write CRLF fixture");
    assert!(crlf.windows(2).any(|pair| pair == b"\r\n"));

    let output = run(temp.path(), &["symbol", LAUNCH, "--source", "--json"]);
    let value = parse_success(&output);
    let source = value["source"].as_str().expect("source string");
    assert!(
        source.contains("\r\n"),
        "CRLF bytes must survive: {source:?}"
    );
    let live = fs::read(temp.path().join("SurveyService.php")).expect("read CRLF fixture");
    let start = value["symbol"]["start_byte"].as_u64().unwrap() as usize;
    let end = value["symbol"]["end_byte"].as_u64().unwrap() as usize;
    assert_eq!(source.as_bytes(), &live[start..end]);
}

#[test]
fn multibyte_prefix_does_not_shift_the_method_bytes() {
    let temp = fixture_repo("multibyte");
    let path = temp.path().join("SurveyService.php");
    let original = fs::read_to_string(&path).expect("read fixture");
    // Insert a multibyte character on a line before `launch`.
    let edited = original.replace(
        "    private string $label = 'survey';",
        "    private string $label = 'surveyé';",
    );
    assert_ne!(edited, original);
    fs::write(&path, edited).expect("write edited fixture");

    let output = run(temp.path(), &["symbol", LAUNCH, "--source", "--json"]);
    let value = parse_success(&output);
    let source = value["source"].as_str().expect("source string");
    assert_eq!(
        source,
        "public function launch(): void\n    {\n        $this->label = self::DEFAULT_LABEL;\n    }"
    );

    let live = fs::read(&path).expect("read edited fixture");
    let start = value["symbol"]["start_byte"].as_u64().unwrap() as usize;
    let end = value["symbol"]["end_byte"].as_u64().unwrap() as usize;
    assert_eq!(source.as_bytes(), &live[start..end]);
}
