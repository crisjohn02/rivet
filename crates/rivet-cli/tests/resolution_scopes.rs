//! T36 acceptance matrix, "Resolution": local shadowing of a receiver
//! variable and dynamic versus static dispatch, through `refs` on the real
//! binary (spec §§11.3–11.5). The other cases of the row (aliases, receiver
//! reassignment, duplicates, top-level calls, interpolation) live in
//! `bindings.rs`, `receiver_conservatism.rs`, `reresolve.rs`,
//! `symbol_calls_json.rs`, and the gold suite; see `ACCEPTANCE.md`.
//!
//! Required result: supported bindings and tiers are correct, and uncertain
//! uses are retained without a false `exact` claim.

mod support;

use serde_json::Value;
use support::{git_repo, run, success, write};

const SOURCE: &str = "<?php
namespace App;
class A { public function m(): void {} public static function s(): void {} }
class B { public function m(): void {} }
$svc = new A();
$f = function ($svc) { $svc->m(); };
$g = function () { $svc->m(); };
$h = fn () => $svc->m();
function top(): void { $svc->m(); }
function param(B $svc): void { $svc->m(); }
$name = 'm';
$svc->$name();
$cls = 'App\\A';
$cls::s();
A::s();
call_user_func([$svc, 'm']);
$svc->m();
";

/// `(line, resolution, resolved_target)` for each listed reference.
fn by_line(value: &Value) -> Vec<(u64, String, Value)> {
    value["references"]
        .as_array()
        .expect("references")
        .iter()
        .map(|item| {
            (
                item["line"].as_u64().unwrap(),
                item["resolution"].as_str().unwrap().to_string(),
                item["resolved_target"].clone(),
            )
        })
        .collect()
}

/// A variable named like an outer receiver but bound in another scope (a
/// closure parameter, a closure without `use`, a function body, a typed
/// parameter of another class) never inherits the outer `new A()`; only the
/// same-scope call binds `scoped`. No receiver call is ever `exact`.
#[test]
fn a_shadowing_variable_never_inherits_the_outer_receiver() {
    let temp = git_repo("shadowing");
    let root = temp.path();
    write(root, "a.php", SOURCE.as_bytes());

    let target = "a.php#App\\A::m";
    let candidates = success(&run(
        root,
        &["refs", "App\\A::m", "--mode", "candidates", "--json"],
    ));
    let rows = by_line(&candidates);
    let lines: Vec<u64> = rows.iter().map(|row| row.0).collect();
    // Line 12 (`$svc->$name()`) has no static method name and line 16 is a
    // string, so neither is a use of `m`.
    assert_eq!(lines, vec![6, 7, 8, 9, 10, 17], "{candidates}");
    for (line, resolution, resolved) in &rows {
        assert_ne!(
            resolution, "exact",
            "line {line}: a receiver call is never exact"
        );
        match line {
            // Other scopes: no evidence for A, so unbound.
            6 | 7 | 9 => {
                assert_eq!(resolution, "name_match", "line {line}");
                assert!(resolved.is_null(), "line {line}: {resolved}");
            }
            // An arrow function captures `$svc` by value; binding it is
            // allowed, but only to A, and never above `scoped`.
            8 => assert!(
                resolved.is_null() || resolved == target,
                "line 8: {resolved}"
            ),
            // The typed parameter shadows the outer variable with B.
            10 => {
                assert_eq!(resolution, "name_match", "query-relative");
                assert_eq!(resolved, "a.php#App\\B::m");
            }
            // Same scope as `$svc = new A()`.
            17 => {
                assert_eq!(resolution, "scoped");
                assert_eq!(resolved, target);
            }
            _ => unreachable!(),
        }
    }

    // Reference mode drops the use bound to B and keeps everything else.
    let references = success(&run(root, &["refs", "App\\A::m", "--json"]));
    let lines: Vec<u64> = by_line(&references).iter().map(|row| row.0).collect();
    assert_eq!(lines, vec![6, 7, 8, 9, 17]);
    assert_eq!(references["by_resolution"]["exact"], 0);

    // B::m sees only its own typed call as a reference.
    let b = success(&run(root, &["refs", "App\\B::m", "--json"]));
    let b_rows = by_line(&b);
    assert!(b_rows.contains(&(10, "scoped".to_string(), Value::from("a.php#App\\B::m"))));
    assert!(!b_rows.iter().any(|row| row.0 == 17), "{b}");
}

/// A static call on a literal class name binds `scoped`; a call through a
/// class-name variable (`$cls::s()`) is dynamic and stays an unbound name
/// match; a callable string is not a use.
#[test]
fn dynamic_dispatch_stays_unbound_and_static_dispatch_binds_scoped() {
    let temp = git_repo("dispatch");
    let root = temp.path();
    write(root, "a.php", SOURCE.as_bytes());

    let refs = success(&run(
        root,
        &["refs", "App\\A::s", "--mode", "candidates", "--json"],
    ));
    assert_eq!(
        by_line(&refs),
        vec![
            (14, "name_match".to_string(), Value::Null),
            (15, "scoped".to_string(), Value::from("a.php#App\\A::s")),
        ],
        "{refs}"
    );
    assert_eq!(refs["by_resolution"]["exact"], 0);
}
