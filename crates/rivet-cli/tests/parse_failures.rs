//! T36 acceptance matrix, "Parser failures": the transitions the earlier
//! suites did not drive (a formerly valid file that becomes invalid UTF-8,
//! binary, or oversize), and the heredoc parse-error report carried into
//! T36 (spec §§26–27; ARCHITECTURE "Parse and coverage policy").
//!
//! Valid-to-malformed and resource-limit transitions are covered by
//! `failure_boundaries.rs` and `refresh.rs`; see `ACCEPTANCE.md`.

mod support;

use serde_json::{Value, json};
use support::{failure, git_repo, run, success, write};

const DECL_PHP: &[u8] = b"<?php\nnamespace App;\nfunction helper(): void {}\n";
const USER_PHP: &[u8] = b"<?php\nnamespace App;\nfunction user(): void { helper(); }\n";

/// A formerly valid file that becomes invalid UTF-8, binary, or larger than
/// `max_file_size_kb` loses its facts: its symbol is gone by name, its ID is
/// an exit-3 direct-target error with the reason, the use that bound to it is
/// left unbound, coverage counts the skip with a diagnostic, and restoring the
/// bytes restores the original snapshot.
#[test]
fn a_file_that_becomes_invalid_utf8_binary_or_oversize_loses_its_facts() {
    let temp = git_repo("exclusion-transitions");
    let root = temp.path();
    write(
        root,
        ".rivet/config.toml",
        b"[index]\nmax_file_size_kb = 1\n",
    );
    write(root, "decl.php", DECL_PHP);
    write(root, "user.php", USER_PHP);

    let baseline = success(&run(root, &["index", "--json"]));
    assert_eq!(baseline["index"]["coverage"]["complete"], true);
    let bound = success(&run(root, &["refs", "App\\helper", "--json"]));
    assert_eq!(bound["references"][0]["resolution"], "exact");

    let mut invalid = DECL_PHP.to_vec();
    invalid.extend_from_slice(b"// \xff\n");
    let mut binary = DECL_PHP.to_vec();
    binary.extend_from_slice(b"\0\0\n");
    let mut oversize = DECL_PHP.to_vec();
    oversize.extend(std::iter::repeat_n(b'/', 1100));
    oversize.push(b'\n');

    for (label, bytes, skip, code, reason) in [
        (
            "invalid UTF-8",
            invalid,
            "encoding",
            "invalid_utf8",
            "UTF-8",
        ),
        ("binary", binary, "binary", "binary_file", "binary"),
        (
            "oversize",
            oversize,
            "size",
            "file_too_large",
            "max_file_size_kb",
        ),
    ] {
        write(root, "decl.php", &bytes);
        let index = success(&run(root, &["index", "--json"]));
        let coverage = &index["index"]["coverage"];
        assert_eq!(coverage["complete"], false, "{label}");
        assert_eq!(coverage["files_seen"], 2, "{label}");
        assert_eq!(coverage["files_indexed"], 1, "{label}");
        assert_eq!(coverage["skipped"][skip], 1, "{label}");
        assert_eq!(
            index["index"]["diagnostics"]["items"],
            json!([{
                "file": "decl.php",
                "code": code,
                "detail": index["index"]["diagnostics"]["items"][0]["detail"].clone(),
            }]),
            "{label}"
        );

        let error = failure(&run(root, &["symbol", "App\\helper", "--json"]), 4);
        assert_eq!(error["error"], "symbol_not_found", "{label}");
        for args in [
            vec!["symbol", "decl.php#App\\helper", "--json"],
            vec!["refs", "decl.php:3", "--json"],
            vec!["context", "decl.php#App\\helper", "--json"],
        ] {
            let error = failure(&run(root, &args), 3);
            assert_eq!(error["error"], "repository_unavailable", "{label} {args:?}");
            let message = error["message"].as_str().unwrap();
            assert!(
                message.starts_with("decl.php is not indexed: ") && message.contains(reason),
                "{label} {args:?}: {message}"
            );
        }
        // The call in user.php is still a use, but binds to nothing.
        let user = success(&run(root, &["symbol", "App\\user", "--json"]));
        let calls = user["calls"]["items"].as_array().expect("calls");
        assert_eq!(calls.len(), 1, "{label}");
        assert_eq!(calls[0]["resolved_target"], Value::Null, "{label}");
        assert_eq!(calls[0]["resolution"], "name_match", "{label}");

        write(root, "decl.php", DECL_PHP);
        let restored = success(&run(root, &["index", "--json"]));
        assert_eq!(restored["index"], baseline["index"], "{label}");
        assert_eq!(restored["bindings"], baseline["bindings"], "{label}");
    }
}

/// Valid PHP whose heredoc or nowdoc body contains `<?php` indexes normally.
///
/// T36 was asked to reproduce a report of such a file being a parse error.
/// `<?php` in a heredoc or nowdoc body is not itself the trigger: none of
/// these authored forms fails with the pinned tree-sitter-php 0.24.2 grammar,
/// and they are kept as regressions. A heredoc template that contains `<?php`
/// *and* an interpolation such as `$v->1` does fail, in the grammar; see
/// `interpolated_arrow_before_a_digit_is_valid_php`.
#[test]
fn heredoc_and_nowdoc_bodies_containing_the_open_tag_index_normally() {
    let temp = git_repo("heredoc-open-tag");
    let root = temp.path();
    let files: [(&str, &str); 6] = [
        (
            "stub.php",
            "<?php\nnamespace App;\nfunction stub(string $ns): string\n{\n    return <<<EOT\n<?php\n\nnamespace {$ns};\n\nclass Foo\n{\n    public function bar(): void {}\n}\nEOT;\n}\n",
        ),
        (
            "nowdoc.php",
            "<?php\nnamespace App;\nfunction nowdoc(): string\n{\n    return <<<'PHP'\n<?php\n\nreturn ['key' => $value];\nPHP;\n}\n",
        ),
        (
            "flexible.php",
            "<?php\nnamespace App;\nclass Flexible\n{\n    public function body(): string\n    {\n        return <<<EOT\n            <?php echo $x; ?>\n            <?= $y ?>\n            EOT;\n    }\n}\n",
        ),
        (
            "argument.php",
            "<?php\nnamespace App;\nfunction argument(): void\n{\n    file_put_contents('x.php', <<<PHP\n<?php\ndeclare(strict_types=1);\nPHP);\n}\n",
        ),
        (
            "top.php",
            "<?php\nnamespace App;\n$template = <<<\"EOT\"\n<?php\n\\$x = {$y['k']};\nEOT;\nfunction after(): void {}\n",
        ),
        (
            "crlf.php",
            "<?php\r\nnamespace App;\r\nfunction crlf(): string\r\n{\r\n    return <<<EOT\r\n<?php\r\n\r\nclass Foo {}\r\nEOT;\r\n}\r\n",
        ),
    ];
    for (name, source) in files {
        write(root, name, source.as_bytes());
    }
    let index = success(&run(root, &["index", "--json"]));
    assert_eq!(index["index"]["coverage"]["complete"], true, "{index}");
    assert_eq!(index["index"]["coverage"]["files_indexed"], 6);
    assert_eq!(index["index"]["diagnostics"]["total"], 0);
    for name in [
        "App\\stub",
        "App\\nowdoc",
        "App\\Flexible::body",
        "App\\argument",
        "App\\after",
        "App\\crlf",
    ] {
        success(&run(root, &["symbol", name, "--json"]));
    }
    // Code inside the string is text, not declarations.
    failure(&run(root, &["symbol", "App\\Foo", "--json"]), 4);
    failure(&run(root, &["symbol", "bar", "--json"]), 4);
}

/// `$x->1` inside a double-quoted string or heredoc is valid PHP (the
/// interpolation is `$x`, followed by the literal text `->1`; `php -l`
/// accepts it), but the pinned tree-sitter-php 0.24.2 scanner treats any
/// alphanumeric after `->` as a property name, so the grammar produces an
/// ERROR node and rivet's policy reports the whole file as `parse_error`.
/// The report is honest (no facts are invented), but a valid file is lost.
#[test]
#[ignore = "T36a: pinned tree-sitter-php 0.24.2 rejects valid `$x->1` interpolation; needs a grammar fix, a pin bump, or a documented limitation"]
fn interpolated_arrow_before_a_digit_is_valid_php() {
    let temp = git_repo("arrow-digit");
    let root = temp.path();
    write(
        root,
        "version.php",
        b"<?php\nnamespace App;\nfunction version(object $v): string\n{\n    return <<<EOT\n<?php\n$v->1\nEOT;\n}\n",
    );
    write(
        root,
        "quoted.php",
        b"<?php\nnamespace App;\nfunction quoted(object $v): string { return \"$v->1\"; }\n",
    );
    let index = success(&run(root, &["index", "--json"]));
    assert_eq!(index["index"]["coverage"]["complete"], true, "{index}");
    success(&run(root, &["symbol", "App\\version", "--json"]));
    success(&run(root, &["symbol", "App\\quoted", "--json"]));
}

/// The current, honest behaviour for the defect above: the valid file is a
/// `parse_error` with no facts, direct targets fail with exit 6, and the other
/// file stays queryable with partial coverage. When T36a lands this test must
/// change together with the ignored one.
#[test]
fn interpolated_arrow_before_a_digit_is_reported_not_mis_indexed() {
    let temp = git_repo("arrow-digit-now");
    let root = temp.path();
    write(
        root,
        "quoted.php",
        b"<?php\nnamespace App;\nfunction quoted(object $v): string { return \"$v->1\"; }\n",
    );
    write(
        root,
        "stub.php",
        b"<?php\nnamespace App;\nfunction stub(object $v): string\n{\n    return <<<EOT\n<?php\n$v->1\nEOT;\n}\n",
    );
    write(
        root,
        "ok.php",
        b"<?php\nnamespace App;\nfunction ok(): void {}\n",
    );
    let index = success(&run(root, &["index", "--json"]));
    assert_eq!(index["index"]["coverage"]["skipped"]["parse_error"], 2);
    let files: Vec<&str> = index["index"]["diagnostics"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["file"].as_str().unwrap())
        .collect();
    assert_eq!(files, vec!["quoted.php", "stub.php"]);
    let error = failure(&run(root, &["symbol", "quoted.php:3", "--json"]), 6);
    assert_eq!(error["file"], "quoted.php");
    let ok = success(&run(root, &["symbol", "App\\ok", "--json"]));
    assert_eq!(ok["index"]["coverage"]["complete"], false);
}
