//! Integration tests for refresh under concurrency, races, and interruption,
//! and for snapshot-consistent query reads (T31; spec §12.3, §12.4 steps 3 and
//! 4; ARCHITECTURE "Refresh and invalidation" and "Concurrency and source
//! consistency").
//!
//! Every test drives real `rivet` processes. Timing is controlled only through
//! the debug-build hooks in `src/debug_hook.rs`: a process stops at a named
//! point when `<hooks>/<point>.pause` exists, announces it with
//! `<hooks>/<point>.reached`, and continues once `<hooks>/<point>.go` exists.
//! Each process also appends every point it passes to `<hooks>/trace`, so the
//! exact sequence of attempts is asserted rather than inferred. No test sleeps
//! and hopes; the only waits are for a named condition, bounded by a deadline
//! that fails the test.

use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

use rivet_store::Store;

/// The binary under test, supplied by Cargo for integration tests.
const RIVET: &str = env!("CARGO_BIN_EXE_rivet");

/// The longest any test waits for a process to reach a point or exit.
const DEADLINE: Duration = Duration::from_secs(60);

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
            "rivet-concurrency-{label}-{}-{nanos}-{unique}",
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

/// One process's hook directory: pause points, reached markers, and trace.
struct Hooks {
    dir: TempDir,
}

impl Hooks {
    fn new(label: &str) -> Hooks {
        Hooks {
            dir: TempDir::new(&format!("hooks-{label}")),
        }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Makes the process stop at `point`.
    fn pause(&self, point: &str) {
        fs::write(self.path().join(format!("{point}.pause")), b"").expect("write pause file");
    }

    /// Lets a process stopped at `point` continue.
    fn release(&self, point: &str) {
        fs::write(self.path().join(format!("{point}.go")), b"").expect("write go file");
    }

    fn reached(&self, point: &str) -> bool {
        self.path().join(format!("{point}.reached")).exists()
    }

    /// Every point the process has passed, in order.
    fn trace(&self) -> Vec<String> {
        fs::read_to_string(self.path().join("trace"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// Waits until the process stops at `point`, failing if it exits first.
    fn wait_reached(&self, point: &str, child: &mut Running) {
        let started = Instant::now();
        while !self.reached(point) {
            if let Some(status) = child.try_wait() {
                panic!(
                    "process exited with {status} before reaching {point}; trace {:?}; stderr {}",
                    self.trace(),
                    child.stderr_now()
                );
            }
            assert!(
                started.elapsed() < DEADLINE,
                "timed out waiting for {point}; trace {:?}",
                self.trace()
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Waits until the process has recorded `point` in its trace.
    fn wait_traced(&self, point: &str, child: &mut Running) {
        let started = Instant::now();
        while !self.trace().iter().any(|line| line == point) {
            if let Some(status) = child.try_wait() {
                panic!(
                    "process exited with {status} before tracing {point}; trace {:?}",
                    self.trace()
                );
            }
            assert!(
                started.elapsed() < DEADLINE,
                "timed out waiting for trace {point}; trace {:?}",
                self.trace()
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

/// A spawned `rivet` process, killed if a failing test abandons it.
struct Running {
    child: Option<Child>,
}

impl Running {
    fn child(&mut self) -> &mut Child {
        self.child.as_mut().expect("process not yet collected")
    }

    fn try_wait(&mut self) -> Option<std::process::ExitStatus> {
        self.child().try_wait().expect("poll child")
    }

    fn still_running(&mut self) -> bool {
        self.try_wait().is_none()
    }

    fn stderr_now(&mut self) -> String {
        let mut text = String::new();
        if let Some(stderr) = self.child().stderr.as_mut() {
            let _ = stderr.read_to_string(&mut text);
        }
        text
    }

    /// Sends SIGKILL and reaps the process.
    fn kill(mut self) -> std::process::ExitStatus {
        let mut child = self.child.take().expect("process not yet collected");
        child.kill().expect("kill the process");
        child.wait().expect("reap the killed process")
    }

    /// Waits for exit and collects the output. The pipes are drained while
    /// waiting, so a large answer cannot block the child; a paused child
    /// gives up its pause after the hook's own bound.
    fn finish(mut self) -> Output {
        let child = self.child.take().expect("process not yet collected");
        child.wait_with_output().expect("collect output")
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Spawns the binary in `dir` with its hooks in `hooks` and extra variables.
fn spawn(dir: &Path, args: &[&str], hooks: &Hooks, env: &[(&str, &str)]) -> Running {
    let mut command = Command::new(RIVET);
    command
        .args(args)
        .current_dir(dir)
        .env("RIVET_DEBUG_PAUSE_DIR", hooks.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in env {
        command.env(key, value);
    }
    Running {
        child: Some(command.spawn().expect("spawn the rivet binary")),
    }
}

/// Runs the binary in `dir` to completion without hooks.
fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(RIVET)
        .args(args)
        .current_dir(dir)
        .env_remove("RIVET_DEBUG_PAUSE_DIR")
        .output()
        .expect("run the rivet binary")
}

/// Parses the single stdout JSON object of a successful run.
fn success_json(output: &Output) -> Value {
    assert_eq!(
        output.status.code(),
        Some(0),
        "expected success; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty(), "success writes nothing to stderr");
    serde_json::from_slice(&output.stdout).expect("stdout is one JSON object")
}

/// Asserts a failure with `exit` and `error`, empty stdout, and returns the
/// stderr error object.
fn error_json(output: &Output, exit: i32, error: &str) -> Value {
    assert_eq!(
        output.status.code(),
        Some(exit),
        "expected exit {exit}; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "a failure never answers on stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let value: Value = serde_json::from_slice(&output.stderr).expect("stderr is one JSON object");
    assert_eq!(value["error"], error, "error object: {value}");
    assert!(
        value.get("index").is_none(),
        "no answer from any snapshot: {value}"
    );
    value
}

fn digest(value: &Value) -> String {
    value["index"]["snapshot"]
        .as_str()
        .expect("index.snapshot is a string")
        .to_string()
}

/// The committed digest read straight from the store.
fn stored_digest(repo: &Path) -> Option<String> {
    let store = Store::open(&repo.join(".rivet")).expect("open store");
    store.get_meta("snapshot_digest").expect("read meta")
}

fn assert_integrity_ok(repo: &Path) {
    let store = Store::open(&repo.join(".rivet")).expect("open store");
    assert_eq!(
        store.integrity_check().expect("run integrity_check"),
        vec!["ok".to_string()]
    );
}

/// Rewrites `file` with `extra` appended; the size changes, so the new
/// metadata differs from any earlier observation.
fn append(repo: &Path, file: &str, extra: &str) {
    let path = repo.join(file);
    let mut text = fs::read_to_string(&path).expect("read fixture file");
    text.push_str(extra);
    fs::write(&path, text).expect("rewrite fixture file");
}

fn strings(points: &[&str]) -> Vec<String> {
    points.iter().map(|point| point.to_string()).collect()
}

/// (a) A second refresh started while the first holds the writer lock waits
/// for it and times out with exit 3 when the pause outlasts its busy timeout.
/// It never acquires the lock, so it publishes nothing, and the first refresh
/// then commits normally.
#[test]
fn competing_refresh_times_out_on_the_held_writer_lock() {
    let repo = fixture_repo("compete-timeout");
    let baseline = digest(&success_json(&run(repo.path(), &["index", "--json"])));

    let first_hooks = Hooks::new("compete-timeout-a");
    first_hooks.pause("refresh-staged-1");
    let mut first = spawn(repo.path(), &["index", "--json"], &first_hooks, &[]);
    first_hooks.wait_reached("refresh-staged-1", &mut first);

    // The first refresh has written every fact inside its transaction and has
    // not committed; the second cannot take the lock.
    append(
        repo.path(),
        "SurveyService.php",
        "\n// edited by the second writer\n",
    );
    let second_hooks = Hooks::new("compete-timeout-b");
    let second = spawn(
        repo.path(),
        &["index", "--json"],
        &second_hooks,
        &[("RIVET_DEBUG_BUSY_TIMEOUT_MS", "200")],
    )
    .finish();
    let error = error_json(&second, 3, "repository_unavailable");
    let message = error["message"].as_str().expect("message");
    assert!(message.contains("writer lock"), "names the lock: {message}");
    assert!(
        message.contains("index.db"),
        "names the database: {message}"
    );
    assert_eq!(second_hooks.trace(), strings(&["refresh-before-lock-1"]));
    assert_eq!(stored_digest(repo.path()), Some(baseline.clone()));

    // The edit landed after the first refresh's scan, so its recheck rolls
    // back and retries once, and the retry publishes the edited tree.
    first_hooks.release("refresh-staged-1");
    let first = success_json(&first.finish());
    assert_eq!(
        first_hooks.trace(),
        strings(&[
            "refresh-before-lock-1",
            "refresh-locked-1",
            "refresh-staged-1",
            "refresh-raced-1",
            "refresh-before-lock-2",
            "refresh-locked-2",
            "refresh-staged-2",
            "refresh-committed-2",
        ])
    );
    assert_ne!(digest(&first), baseline);
    let after = success_json(&run(repo.path(), &["index", "--json"]));
    assert_eq!(digest(&after), digest(&first));
    assert_eq!(after["updated"], 0);
}

/// (a) A second refresh waits on the lock while the first is paused inside
/// its transaction, acquires it only after the first commits, and computes
/// from the first's committed inventory plus the tree as it is then, never
/// from the state the first began with.
///
/// The first refresh publishes an edit to `boot.php`. The second's reparse
/// record must then name only `SurveyService.php`, edited while the second
/// already held the lock: had it loaded the inventory before the first
/// committed, `boot.php`'s stored hash would be stale and it would reparse
/// `boot.php` too; had it walked before taking the lock, it would miss the
/// `SurveyService.php` edit.
#[test]
fn competing_refresh_waits_and_publishes_after_the_first_commits() {
    let repo = fixture_repo("compete-wait");
    let baseline = digest(&success_json(&run(repo.path(), &["index", "--json"])));
    append(
        repo.path(),
        "boot.php",
        "\n// published by the first writer\n",
    );

    let first_hooks = Hooks::new("compete-wait-a");
    first_hooks.pause("refresh-staged-1");
    let mut first = spawn(repo.path(), &["index", "--json"], &first_hooks, &[]);
    first_hooks.wait_reached("refresh-staged-1", &mut first);

    let second_hooks = Hooks::new("compete-wait-b");
    second_hooks.pause("refresh-locked-1");
    let reparsed = second_hooks.path().join("reparsed");
    let reparsed_env = reparsed.to_str().expect("UTF-8 temp path").to_string();
    let mut second = spawn(
        repo.path(),
        &["index", "--json"],
        &second_hooks,
        &[
            ("RIVET_DEBUG_BUSY_TIMEOUT_MS", "60000"),
            ("RIVET_DEBUG_REPARSED", reparsed_env.as_str()),
        ],
    );
    second_hooks.wait_traced("refresh-before-lock-1", &mut second);
    // The first still holds the lock: the second cannot have acquired it.
    assert!(second.still_running());
    assert!(!second_hooks.reached("refresh-locked-1"));

    first_hooks.release("refresh-staged-1");
    let first = success_json(&first.finish());
    let first_digest = digest(&first);
    assert_ne!(first_digest, baseline);
    assert_eq!(first["updated"], 1);

    // Only now does the second hold the lock. Edit before it walks.
    second_hooks.wait_reached("refresh-locked-1", &mut second);
    append(
        repo.path(),
        "SurveyService.php",
        "\n// edited while the second holds the lock\n",
    );
    second_hooks.release("refresh-locked-1");
    let second = success_json(&second.finish());
    assert_eq!(
        second_hooks.trace(),
        strings(&[
            "refresh-before-lock-1",
            "refresh-locked-1",
            "refresh-staged-1",
            "refresh-committed-1",
        ])
    );
    assert_eq!(
        fs::read_to_string(&reparsed).expect("reparse record"),
        "SurveyService.php\n"
    );
    assert_ne!(digest(&second), first_digest);
    assert_eq!(second["updated"], 1);
    assert_eq!(second["unchanged"], 9);
    let after = success_json(&run(repo.path(), &["index", "--json"]));
    assert_eq!(digest(&after), digest(&second));
    assert_eq!(after["updated"], 0);
}

/// (a) The out-of-order case the lock exists for: a first refresh that
/// scanned the old tree and a second started after an edit. The first's
/// stale scan is never published over the newer tree; both end on the
/// edited tree's snapshot.
#[test]
fn stale_scan_is_never_published_over_a_newer_tree() {
    let repo = fixture_repo("stale-scan");
    let baseline = digest(&success_json(&run(repo.path(), &["index", "--json"])));

    let first_hooks = Hooks::new("stale-scan-a");
    first_hooks.pause("refresh-staged-1");
    let mut first = spawn(repo.path(), &["index", "--json"], &first_hooks, &[]);
    first_hooks.wait_reached("refresh-staged-1", &mut first);

    append(repo.path(), "boot.php", "\nfunction t31_newer(): void {}\n");
    let second_hooks = Hooks::new("stale-scan-b");
    let mut second = spawn(
        repo.path(),
        &["index", "--json"],
        &second_hooks,
        &[("RIVET_DEBUG_BUSY_TIMEOUT_MS", "60000")],
    );
    second_hooks.wait_traced("refresh-before-lock-1", &mut second);
    assert!(
        !second_hooks
            .trace()
            .contains(&"refresh-locked-1".to_string())
    );

    first_hooks.release("refresh-staged-1");
    let first = success_json(&first.finish());
    let second = success_json(&second.finish());
    let first_trace = first_hooks.trace();
    assert!(first_trace.contains(&"refresh-raced-1".to_string()));
    assert!(!first_trace.contains(&"refresh-committed-1".to_string()));
    assert_eq!(
        first_trace.last().map(String::as_str),
        Some("refresh-committed-2")
    );

    let fresh = success_json(&run(repo.path(), &["index", "--json"]));
    assert_ne!(digest(&fresh), baseline);
    assert_eq!(
        fresh["updated"], 0,
        "the committed snapshot is the edited tree"
    );
    assert_eq!(digest(&first), digest(&fresh));
    assert_eq!(digest(&second), digest(&fresh));
    let found = success_json(&run(repo.path(), &["symbol", "t31_newer", "--json"]));
    assert_eq!(found["symbol"]["name"], "t31_newer");
}

/// (b) A query against a held writer lock fails with exit 3 naming the lock
/// after the documented 5 second busy timeout, with empty stdout and no answer
/// from the existing snapshot. Explicit `--no-refresh` still reads the cache,
/// labelled `cached`, because readers never wait for the writer.
#[test]
fn query_against_a_held_writer_lock_is_exit_3_without_a_cached_answer() {
    let repo = fixture_repo("lock-timeout");
    let baseline = digest(&success_json(&run(repo.path(), &["index", "--json"])));

    let holder_hooks = Hooks::new("lock-timeout-holder");
    holder_hooks.pause("refresh-locked-1");
    let mut holder = spawn(repo.path(), &["index", "--json"], &holder_hooks, &[]);
    holder_hooks.wait_reached("refresh-locked-1", &mut holder);

    let query_hooks = Hooks::new("lock-timeout-query");
    let started = Instant::now();
    let query = spawn(
        repo.path(),
        &["symbol", "SurveyService", "--json"],
        &query_hooks,
        &[],
    )
    .finish();
    let waited = started.elapsed();
    let error = error_json(&query, 3, "repository_unavailable");
    let message = error["message"].as_str().expect("message");
    assert!(message.contains("writer lock"), "names the lock: {message}");
    assert!(message.contains("5000 ms"), "names the timeout: {message}");
    assert!(
        waited >= Duration::from_millis(4500),
        "waited the documented busy timeout, not {waited:?}"
    );
    assert_eq!(query_hooks.trace(), strings(&["refresh-before-lock-1"]));

    // The same with the shortened timeout for refs and context.
    for args in [
        &["refs", "SurveyService", "--json"][..],
        &["context", "SurveyService", "--json"][..],
    ] {
        let hooks = Hooks::new("lock-timeout-short");
        let output = spawn(
            repo.path(),
            args,
            &hooks,
            &[("RIVET_DEBUG_BUSY_TIMEOUT_MS", "100")],
        )
        .finish();
        error_json(&output, 3, "repository_unavailable");
    }

    let cached = success_json(&run(
        repo.path(),
        &["symbol", "SurveyService", "--json", "--no-refresh"],
    ));
    assert_eq!(cached["index"]["freshness"], "cached");
    assert_eq!(digest(&cached), baseline);

    holder_hooks.release("refresh-locked-1");
    success_json(&holder.finish());
}

/// (c) A file changed between the scan and the pre-commit recheck causes
/// exactly one retry, which succeeds, and the answer comes from a snapshot
/// that reflects the changed file.
#[test]
fn race_between_scan_and_recheck_retries_once_and_succeeds() {
    let repo = fixture_repo("race-retry");
    let baseline = digest(&success_json(&run(repo.path(), &["index", "--json"])));

    let hooks = Hooks::new("race-retry");
    hooks.pause("refresh-staged-1");
    let mut query = spawn(repo.path(), &["symbol", "t31_added", "--json"], &hooks, &[]);
    hooks.wait_reached("refresh-staged-1", &mut query);
    append(repo.path(), "boot.php", "\nfunction t31_added(): void {}\n");
    hooks.release("refresh-staged-1");

    // The first attempt's facts do not contain the function; only the retry's
    // snapshot can answer.
    let answer = success_json(&query.finish());
    assert_eq!(answer["symbol"]["name"], "t31_added");
    assert_eq!(answer["symbol"]["file"], "boot.php");
    assert_eq!(
        hooks.trace(),
        strings(&[
            "refresh-before-lock-1",
            "refresh-locked-1",
            "refresh-staged-1",
            "refresh-raced-1",
            "refresh-before-lock-2",
            "refresh-locked-2",
            "refresh-staged-2",
            "refresh-committed-2",
            "query-snapshot-pinned",
        ])
    );
    assert_ne!(digest(&answer), baseline);
    let fresh = success_json(&run(repo.path(), &["index", "--json"]));
    assert_eq!(digest(&fresh), digest(&answer));
    assert_eq!(fresh["updated"], 0);
    assert_eq!(stored_digest(repo.path()), Some(digest(&answer)));
}

/// (c) A file deleted between the scan and the recheck is a change to the
/// eligible path set: one retry, which publishes the tree without it.
#[test]
fn deleted_file_during_refresh_retries_once() {
    let repo = fixture_repo("race-delete");
    success_json(&run(repo.path(), &["index", "--json"]));

    let hooks = Hooks::new("race-delete");
    hooks.pause("refresh-staged-1");
    let mut index = spawn(repo.path(), &["index", "--json"], &hooks, &[]);
    hooks.wait_reached("refresh-staged-1", &mut index);
    fs::remove_file(repo.path().join("Trait.php")).expect("delete a file");
    hooks.release("refresh-staged-1");

    let report = success_json(&index.finish());
    assert_eq!(report["deleted"], 1);
    assert_eq!(report["index"]["coverage"]["files_seen"], 9);
    let trace = hooks.trace();
    assert_eq!(
        trace
            .iter()
            .filter(|p| p.starts_with("refresh-raced-"))
            .count(),
        1
    );
    assert_eq!(
        trace.last().map(String::as_str),
        Some("refresh-committed-2")
    );
}

/// (d) A file that changes again during the retry yields `repository_changed`
/// (exit 9), empty stdout, and leaves the previous snapshot committed.
#[test]
fn continued_mutation_is_exit_9_and_keeps_the_previous_snapshot() {
    let repo = fixture_repo("race-twice");
    let baseline = digest(&success_json(&run(repo.path(), &["index", "--json"])));
    let before = run(
        repo.path(),
        &["symbol", "SurveyService", "--json", "--no-refresh"],
    );

    let hooks = Hooks::new("race-twice");
    hooks.pause("refresh-staged-1");
    hooks.pause("refresh-staged-2");
    let mut query = spawn(
        repo.path(),
        &["symbol", "SurveyService", "--json"],
        &hooks,
        &[],
    );
    hooks.wait_reached("refresh-staged-1", &mut query);
    append(repo.path(), "boot.php", "\n// first edit\n");
    hooks.release("refresh-staged-1");
    hooks.wait_reached("refresh-staged-2", &mut query);
    append(repo.path(), "boot.php", "\n// second edit\n");
    hooks.release("refresh-staged-2");

    let output = query.finish();
    error_json(&output, 9, "repository_changed");
    assert_eq!(
        hooks.trace(),
        strings(&[
            "refresh-before-lock-1",
            "refresh-locked-1",
            "refresh-staged-1",
            "refresh-raced-1",
            "refresh-before-lock-2",
            "refresh-locked-2",
            "refresh-staged-2",
            "refresh-raced-2",
        ])
    );
    assert_eq!(stored_digest(repo.path()), Some(baseline));
    let after = run(
        repo.path(),
        &["symbol", "SurveyService", "--json", "--no-refresh"],
    );
    assert_eq!(after.stdout, before.stdout);
    assert_integrity_ok(repo.path());
}

/// Kills a refresh stopped at `point` and checks the previous snapshot,
/// database integrity, and that the next refresh succeeds.
fn assert_kill_rolls_back(label: &str, args: &[&str], point: &str) {
    let repo = fixture_repo(label);
    let baseline = digest(&success_json(&run(repo.path(), &["index", "--json"])));
    let queries: [&[&str]; 3] = [
        &[
            "symbol",
            "SurveyService",
            "--json",
            "--no-refresh",
            "--source",
        ],
        &["refs", "SurveyService", "--json", "--no-refresh"],
        &["context", "SurveyService", "--json", "--no-refresh"],
    ];
    let before: Vec<Vec<u8>> = queries
        .iter()
        .map(|args| success_json_bytes(&run(repo.path(), args)))
        .collect();

    // Real pending changes, so the killed transaction had rows to write.
    append(
        repo.path(),
        "SurveyService.php",
        "\nfunction t31_killed(): void {}\n",
    );
    fs::remove_file(repo.path().join("Trait.php")).expect("delete a file");

    let hooks = Hooks::new(label);
    hooks.pause(point);
    let mut writer = spawn(repo.path(), args, &hooks, &[]);
    hooks.wait_reached(point, &mut writer);
    let status = writer.kill();
    assert!(!status.success());
    assert!(
        !hooks
            .trace()
            .iter()
            .any(|p| p.starts_with("refresh-committed"))
    );

    assert_integrity_ok(repo.path());
    assert_eq!(stored_digest(repo.path()), Some(baseline.clone()));
    for (args, expected) in queries.iter().zip(&before) {
        let output = run(repo.path(), args);
        assert_eq!(
            &success_json_bytes(&output),
            expected,
            "{args:?} answers from the previous snapshot"
        );
    }

    let next = success_json(&run(repo.path(), &["index", "--json"]));
    assert_ne!(digest(&next), baseline);
    assert_eq!(next["deleted"], 1);
    let found = success_json(&run(repo.path(), &["symbol", "t31_killed", "--json"]));
    assert_eq!(found["symbol"]["name"], "t31_killed");
    assert_integrity_ok(repo.path());
}

/// The stdout bytes of a successful run.
fn success_json_bytes(output: &Output) -> Vec<u8> {
    success_json(output);
    output.stdout.clone()
}

/// (e) A refresh killed after writing every fact inside its transaction
/// leaves the previous snapshot, a passing integrity check, and a next
/// refresh that succeeds.
#[test]
fn killed_refresh_rolls_back_to_the_previous_snapshot() {
    assert_kill_rolls_back("kill-staged", &["index", "--json"], "refresh-staged-1");
}

/// (e) The same for `index --force`, whose transaction first deletes every
/// stored fact, and for a query killed while holding the lock.
#[test]
fn killed_forced_rebuild_and_killed_query_roll_back() {
    assert_kill_rolls_back(
        "kill-force",
        &["index", "--json", "--force"],
        "refresh-staged-1",
    );
    assert_kill_rolls_back(
        "kill-query",
        &["refs", "SurveyService", "--json"],
        "refresh-locked-1",
    );
}

/// (f) A query paused after its read transaction pinned a snapshot, while
/// another process publishes a different snapshot, still answers entirely
/// from the pinned one: rows, positions, stored source, and digest are
/// byte-identical to the answer before the publish.
#[test]
fn paused_query_reads_one_snapshot_while_another_process_publishes() {
    for no_refresh in [false, true] {
        let repo = fixture_repo("snapshot-read");
        let baseline = digest(&success_json(&run(repo.path(), &["index", "--json"])));
        let mut commands: Vec<Vec<&str>> = vec![
            vec!["symbol", "SurveyService", "--json", "--source"],
            vec!["refs", "SurveyService", "--json"],
            vec!["context", "SurveyService", "--json"],
        ];
        if no_refresh {
            for args in &mut commands {
                args.push("--no-refresh");
            }
        }
        let before: Vec<Vec<u8>> = commands
            .iter()
            .map(|args| success_json_bytes(&run(repo.path(), args)))
            .collect();

        let mut paused = Vec::new();
        for args in &commands {
            let hooks = Hooks::new("snapshot-read");
            hooks.pause("query-snapshot-pinned");
            let mut query = spawn(repo.path(), args, &hooks, &[]);
            hooks.wait_reached("query-snapshot-pinned", &mut query);
            paused.push((hooks, query));
        }

        // Shift every position in both files and add a reference, then
        // publish. The writer is not blocked by the paused readers.
        let prefix = "<?php\n// shifted by a concurrent edit\n";
        for file in ["SurveyService.php", "ReportService.php"] {
            let path = repo.path().join(file);
            let text = fs::read_to_string(&path).expect("read");
            let shifted = text.replacen("<?php\n", prefix, 1);
            fs::write(&path, shifted).expect("write");
        }
        append(
            repo.path(),
            "boot.php",
            "\n$other = new \\App\\Services\\SurveyService();\n",
        );
        let published = success_json(&run(repo.path(), &["index", "--json"]));
        assert_ne!(digest(&published), baseline);
        assert_eq!(stored_digest(repo.path()), Some(digest(&published)));

        for ((hooks, query), expected) in paused.into_iter().zip(&before) {
            hooks.release("query-snapshot-pinned");
            let output = query.finish();
            assert_eq!(
                String::from_utf8_lossy(&success_json_bytes(&output)),
                String::from_utf8_lossy(expected),
                "a paused query answers from its pinned snapshot"
            );
            assert_eq!(digest(&success_json(&output)), baseline);
        }

        // And a fresh query now sees the published snapshot.
        let fresh = success_json(&run(repo.path(), &["refs", "SurveyService", "--json"]));
        assert_eq!(digest(&fresh), digest(&published));
        assert_ne!(
            fresh["references"],
            success_json_value(&before[1])["references"]
        );
    }
}

fn success_json_value(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).expect("stored JSON")
}

/// (f) Another process that publishes between a query's commit and its read
/// transaction would pair this refresh's report with another snapshot's rows.
/// The pinned digest then differs from the committed one, which spends the
/// single retry, and the answer and digest come from the newer snapshot.
#[test]
fn publish_between_commit_and_read_is_detected_and_retried() {
    let repo = fixture_repo("snapshot-moved");
    let baseline = digest(&success_json(&run(repo.path(), &["index", "--json"])));

    let hooks = Hooks::new("snapshot-moved");
    hooks.pause("refresh-committed-1");
    let mut query = spawn(
        repo.path(),
        &["symbol", "t31_between", "--json"],
        &hooks,
        &[],
    );
    hooks.wait_reached("refresh-committed-1", &mut query);
    append(
        repo.path(),
        "boot.php",
        "\nfunction t31_between(): void {}\n",
    );
    let published = success_json(&run(repo.path(), &["index", "--json"]));
    assert_ne!(digest(&published), baseline);
    hooks.release("refresh-committed-1");

    let answer = success_json(&query.finish());
    assert_eq!(answer["symbol"]["name"], "t31_between");
    assert_eq!(digest(&answer), digest(&published));
    assert_eq!(
        hooks.trace(),
        strings(&[
            "refresh-before-lock-1",
            "refresh-locked-1",
            "refresh-staged-1",
            "refresh-committed-1",
            "query-snapshot-moved-1",
            "refresh-before-lock-2",
            "refresh-locked-2",
            "refresh-staged-2",
            "refresh-committed-2",
            "query-snapshot-pinned",
        ])
    );
}

/// Competing first refreshes on a repository with no cache yet all succeed
/// with one digest. Creating the schema happens under the writer lock and
/// re-reads the format version inside it; before that, every process but one
/// failed with "table meta already exists". This is the one test here that
/// relies on processes overlapping rather than on a pause point, because the
/// schema is created before any refresh point; its pass condition does not
/// depend on how they interleave.
#[test]
fn competing_first_refreshes_create_the_schema_once() {
    for _ in 0..3 {
        let repo = fixture_repo("first-create");
        let hooks = Hooks::new("first-create");
        let running: Vec<Running> = (0..10)
            .map(|_| spawn(repo.path(), &["index", "--json"], &hooks, &[]))
            .collect();
        let digests: std::collections::BTreeSet<String> = running
            .into_iter()
            .map(|process| digest(&success_json(&process.finish())))
            .collect();
        assert_eq!(digests.len(), 1, "one snapshot: {digests:?}");
        assert_integrity_ok(repo.path());
    }
}
