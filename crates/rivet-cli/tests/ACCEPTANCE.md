# Correctness acceptance map

Every case of the correctness acceptance matrix in
`docs/IMPLEMENTATION-PLAN.md`, mapped to the tests that prove it. Written for
T36 and kept current as tests are added.

A case is **covered** only when a test was read and found to assert the
matrix's required result for that case, not merely something nearby.
Test names are `file::test`. Files are under `crates/rivet-cli/tests/`
unless a crate path is given; "(unit)" marks a crate unit test, and "(lib)" an
integration test that drives the `rivet_cli` library instead of the binary.

Statuses:

- **covered**: proved before T36.
- **added in T36**: no test proved it before T36, and one does now.
- **gap → T36x**: not proved, or proved wrong; recorded as a subtask in
  `docs/TASKS.md`, with an `#[ignore]`d test where one can be written.
- **n/a**: the case has no second code path to compare, with the reason.

Every T36 test drives the real binary through `support/mod.rs`, which requires
exactly one compact JSON object and one LF on the chosen stream and nothing on
the other, and kills any run that exceeds its deadline.

## Freshness

Required: content-mode results reflect the new source and declaration bindings.

| Case | Tests | Status |
|---|---|---|
| Same-size edit with restored mtime | `refresh::same_size_edit_with_restored_mtime_is_detected`; `freshness_modes::metadata_mode_misses_a_restored_mtime_edit` (content mode catches it, metadata mode is documented to miss it) | covered |
| Deletion | `refresh::deleted_file_loses_its_symbols`; `reresolve::deleting_a_target_file_unbinds_an_untouched_use_and_restoring_rebinds` | covered |
| Rename | `refresh::renamed_file_moves_its_canonical_id` | covered |
| Branch switch | `freshness::a_git_branch_switch_is_reflected_by_the_next_query` (real `git checkout`: same-size edit, add, delete, move; answers equal a cold build; switching back restores the bytes) | added in T36 |
| New duplicate definition | `reresolve::duplicate_declaration_in_a_new_file_invalidates_an_exact_binding` | covered |
| Changed import | `reresolve::changed_import_in_one_file_leaves_another_files_bindings_identical` | covered |

## Cache invalidation

Required: excluded facts removed; affected parses and bindings rebuilt.

| Case | Tests | Status |
|---|---|---|
| Ignore change | `freshness::git_ignore_rule_changes_remove_and_restore_facts` (root `.gitignore`, nested `.gitignore`, `.git/info/exclude`, and `respect_gitignore = false`). The walker's rules alone were covered by `rivet-core/src/walk.rs` (unit) `honors_nested_gitignore`, `git_info_exclude_works`; nothing drove a rule change through refresh. | added in T36 |
| Config change | `freshness_modes::config_invalidation_excludes_and_restores`; `rivet-store` (unit) `effective_config_change_changes_digest` | covered |
| Language change | `freshness_modes::language_invalidation_disables_php`; `coverage_honesty::disabled_typescript_is_an_ordinary_unsupported_file` | covered |
| Grammar change | `rivet-languages/src/lib.rs` (unit) `fingerprint_tracks_locked_grammar_versions` (the extractor fingerprint names the locked grammar versions); `coverage_honesty::updated_counts_files_reparsed_after_an_extractor_change`; `rivet-cli/src/refresh.rs` (unit) `extractor_fingerprint_change_reparses_equal_content`; `coverage_honesty::no_refresh_refuses_a_cache_from_another_extractor` | covered |
| Resolver change | `refresh::stale_bindings_are_re_resolved_without_reparsing`; `coverage_honesty::no_refresh_refuses_a_cache_from_another_resolver` | covered |

## Atomicity

Required: a complete committed snapshot or a bounded explicit error, never
mixed positions or source.

| Case | Tests | Status |
|---|---|---|
| Two concurrent queries | `concurrency::paused_query_reads_one_snapshot_while_another_process_publishes`; `concurrency::publish_between_commit_and_read_is_detected_and_retried`; `concurrency::competing_first_refreshes_create_the_schema_once`. T36 adds `determinism::concurrent_queries_on_an_unindexed_tree_agree_byte_for_byte` (nine concurrent query processes on a never-indexed tree). | covered |
| Killed writer | `concurrency::killed_refresh_rolls_back_to_the_previous_snapshot`; `concurrency::killed_forced_rebuild_and_killed_query_roll_back` | covered |
| Lock timeout | `concurrency::competing_refresh_times_out_on_the_held_writer_lock`; `concurrency::query_against_a_held_writer_lock_is_exit_3_without_a_cached_answer`; `rivet-store` (unit) `writer_lock_is_exclusive_and_bounded_by_the_busy_timeout` | covered |
| Editor changing a file during refresh | `concurrency::race_between_scan_and_recheck_retries_once_and_succeeds`; `concurrency::deleted_file_during_refresh_retries_once`; `concurrency::continued_mutation_is_exit_9_and_keeps_the_previous_snapshot`; `concurrency::stale_scan_is_never_published_over_a_newer_tree` | covered |

## Parser failures

Required: old facts removed; coverage explains omissions; explicit target
queries fail clearly.

| Case | Tests | Status |
|---|---|---|
| Valid file becomes malformed | `failure_boundaries::a_file_that_becomes_a_parse_error_or_resource_limit_loses_its_facts`; `refresh::valid_to_malformed_clears_symbols_and_back`; `failure_boundaries::a_parse_error_or_resource_limit_file_is_exit_6_with_detail` | covered |
| Invalid UTF-8 | `parse_failures::a_file_that_becomes_invalid_utf8_binary_or_oversize_loses_its_facts` (the transition: facts removed, use unbound, exit 3 by ID/path, snapshot restored). Before T36 only a file that was *already* invalid was tested (`failure_boundaries::a_binary_oversize_or_encoding_exclusion_is_exit_3_with_reason_and_hint`, `kind_aware_lookup::invalid_utf8_php_file_suppresses_the_global_fallback`), so "old facts removed" was unproved. | added in T36 |
| Oversize | Same T36 test (also covers the binary transition); before, only an already-oversize file. | added in T36 |
| Deterministic resource limit | `failure_boundaries::lowered_node_limit_records_resource_limit_and_default_indexes_it`; `failure_boundaries::lowered_use_limit_records_resource_limit_only_when_exceeded`; `failure_boundaries::a_file_that_becomes_a_parse_error_or_resource_limit_loses_its_facts`; `rivet-parser` (unit) `node_limit_is_a_count_that_bites_only_when_exceeded` | covered |
| (carried) Valid PHP heredoc containing `<?php` reported as a parse error | `parse_failures::heredoc_and_nowdoc_bodies_containing_the_open_tag_index_normally` (six authored forms, all index). The cause found is the pinned grammar, not rivet's policy: `$v->1` inside a heredoc or double-quoted string is valid PHP but tree-sitter-php 0.24.2 yields an ERROR node, so the file is honestly reported as `parse_error`. `parse_failures::interpolated_arrow_before_a_digit_is_reported_not_mis_indexed` pins today's honest behaviour; `parse_failures::interpolated_arrow_before_a_digit_is_valid_php` is `#[ignore]`d. | gap → T36a |

## Resolution

Required: supported bindings and tiers correct; uncertain uses retained without
false exact claims.

| Case | Tests | Status |
|---|---|---|
| Aliases | `bindings::resolves_exact_imports_and_scoped_receivers_but_not_new_hints` (gold b); `bindings::function_import_alias_binds_and_without_it_stays_unresolved`; `refs_json::aliased_uses_are_retained_through_their_binding` | covered |
| Local shadowing | `resolution_scopes::a_shadowing_variable_never_inherits_the_outer_receiver` (closure parameter, closure without `use`, function body, typed parameter of another class). Name-level shadowing (a namespaced function over a global one) was already covered by `namespace_scopes::top_level_arrow_function_sees_the_file_namespace_and_imports`; variable shadowing was not. | added in T36 |
| Receiver reassignment | `bindings::new_receiver_conservatism_rejects_reassignment_and_control_flow`; `bindings::every_php_rebinding_form_leaves_the_new_receiver_unbound`; `receiver_conservatism::typed_parameter_rebinding_forms_record_no_binding`; `receiver_conservatism::typed_property_survives_reassignment_but_the_promoted_parameter_does_not` | covered |
| Duplicate names | `reresolve::duplicate_declaration_in_a_new_file_invalidates_an_exact_binding`; `symbol_json::duplicate_short_names_are_ambiguous_and_byte_ordered`; `refs_json::candidate_mode_reports_a_use_bound_elsewhere_as_query_relative_name_match` | covered |
| Dynamic/static dispatch | `resolution_scopes::dynamic_dispatch_stays_unbound_and_static_dispatch_binds_scoped` (`A::s()` scoped, `$cls::s()` unbound, `$svc->$name()` and a callable string are not uses). Before T36: `silent_misses::static_and_parent_scopes_bind_nothing` and `silent_misses::explicit_class_references_appear_in_refs` covered `static::`/`parent::` and the class reference only, not the method binding. | added in T36 |
| Top-level calls | `symbol_calls_json::top_level_calls_are_retained_in_called_by`; `bindings::resolves_exact_imports_and_scoped_receivers_but_not_new_hints` (gold c) | covered |
| Interpolation | `bindings::resolves_exact_imports_and_scoped_receivers_but_not_new_hints` (gold h, `{$svc->launch()}`); `rivet-languages/tests/php_uses.rs`; `refs_json::gold_not_a_use_spans_appear_in_neither_mode` (literal text excluded) | covered |
| (found) `extends`/`implements` clauses and the scope of `$expr::$prop` are never recorded as uses, so `refs` on a base class silently omits its subclasses. Listed in `audit-2026-09-22.md` "Follow-ups"; still true on this build. | none | gap → T36d |

Other audit follow-ups, re-checked on this build and **not** matrix gaps
because every one is labelled honestly: an unbound `$x->items()` call
name-matches a property `$items` (noise, labelled `name_match`); an `A|null`
parameter binds nothing (conservative); `rivet symbol items` does not find the
property `$items` (query-form limitation). `RESOLVER_FINGERPRINT` is now
`php-rules-v2`, and the two T32 items are fixed and covered by
`failure_boundaries::invalid_file_line_forms_are_exit_2_outside_any_repository`
and `failure_boundaries::an_unsupported_file_is_exit_7_with_its_nullable_language`.

## Identity

Required: no ID collisions; no arbitrary ambiguity resolution.

| Case | Tests | Status |
|---|---|---|
| Literal `#`/`%` in path | `identity::literal_hash_and_percent_in_paths_are_escaped_without_collision` (`a#b/` and `a%23b/` get distinct IDs; `100%.php`; unescaped spellings are not guessed). Before T36 only `rivet-core/src/id.rs` (unit) `round_trips_escaped_path_and_ordinal` / `round_trips_literal_percent_escape_text`, never through indexing and queries. | added in T36 |
| Duplicates | `identity::duplicate_declarations_get_distinct_ordinals_and_are_never_picked_arbitrarily` (`#1`/`#2` on functions and methods, unsuffixed and out-of-range ordinals not found, the call binds to neither). Before T36: `rivet-core/src/id.rs` (unit) `assigns_ordinals_in_span_order`, `ordinal_ties_break_on_kind_and_include_first_member` only. | added in T36 |
| Nested definitions on one line | `identity::nested_definitions_on_one_line_are_distinct_and_file_line_is_ambiguous`; `rivet-index/src/query.rs` (unit) `file_line_siblings_of_different_lengths_on_one_line_are_ambiguous`. **Defect fixed:** `file:line` picked the shortest span, so of `function h() {} function k() { function inner() {} }` it returned `h` alone. It now returns every innermost symbol (minimal by containment). | added in T36 |
| Case-sensitive paths | `identity::paths_differing_only_in_case_are_never_folded` (queries spelled in another case never resolve; the two-file half runs only on a case-sensitive filesystem and was **not run** on this APFS machine); `rivet-index/src/query.rs` (unit) `paths_differing_only_in_case_are_distinct` (both files in one store, distinct IDs, byte-exact `file:line` and canonical ID). | added in T36 |

## Budget

Required: invalid argument or fitting forms / error 8; no partial body;
truthful omission and cap metadata.

| Case | Tests | Status |
|---|---|---|
| Zero/negative | `context_json::invalid_arguments_fail_before_filesystem_work` (`--tokens 0`, `1000001`, `many`); `context_json::an_out_of_range_configured_budget_is_invalid`. Negative was untested; T36 adds `--tokens -1` in `streams::every_command_failure_is_one_json_error_on_stderr_and_nothing_on_stdout`. | added in T36 |
| Tiny budget | `context_fit::target_fits_exactly_at_the_budget_and_fails_over_one_under` (lib); `context_json::budget_too_small_reports_required_tokens_and_index` | covered |
| Large target | `context_fit::a_target_that_cannot_fit_is_error_8_with_the_allowed_minimum` (lib) | covered |
| All collapse modes | `context_json::collapse_modes_select_forms`; `context_fit::the_three_modes_differ_on_the_fixture_candidates` (lib); `context_fit::no_success_exceeds_the_budget_for_any_target_mode_or_budget` (lib, every target × mode × budget) | covered |
| Parent/child overlap | `context_overlap::a_full_class_target_suppresses_its_members_as_overlap`, `b_the_parent_of_a_full_method_target_is_a_reduced_signature`, `c_a_container_emitted_before_its_member_is_rebuilt_without_it`, `d_the_limit_caps_segments_and_overlap_takes_precedence`, `g_sweep_the_authored_fixture` | covered |
| High fanout | `context_traversal::the_default_candidate_cap_holds_at_scale`; `context_traversal::the_default_examined_use_cap_holds_at_scale`; `context_traversal::the_candidate_cap_is_exact_at_the_boundary`; `context_overlap::candidate_limit_reached_passes_through` | covered |

## Output

Required: parseable bounded objects, correct totals, consistent stdout/stderr
and exit codes.

| Case | Tests | Status |
|---|---|---|
| Empty results | `refs_json::kind_filter_can_yield_an_empty_page`; `human_output::empty_refs_say_the_result_is_not_proof`. T36 adds `streams::empty_results_are_empty_lists_with_zero_counts` (empty repository, empty call lists, target-only context). | covered |
| Limit/offset boundary | `refs_json::pagination_is_ordered_and_counts_before_slicing`; `symbol_query_forms::offset_beyond_the_end_is_an_empty_page`; `symbol_calls_json::call_lists_paginate_independently`; `refs_json::invalid_arguments_are_rejected_before_filesystem_work`. T36 adds `streams::limit_and_offset_are_exact_at_their_boundaries` (limit = total, total − 1, last single page, 1000, u64::MAX offset, overflow). | covered |
| Ambiguity pages | `symbol_query_forms::offset_pages_ambiguous_candidates_deterministically`; `symbol_json::limit_truncates_the_candidate_page`; `context_json::ambiguity_returns_candidates_at_offset_zero`. T36 adds `streams::ambiguity_pages_are_exact_and_agree_across_commands` (symbol and refs page identically; context equals the offset-zero page). | covered |
| JSON errors | `streams::every_command_failure_is_one_json_error_on_stderr_and_nothing_on_stdout` (exits 2–8 for every command that can produce them); `streams::every_command_success_is_one_json_line_on_stdout_and_nothing_on_stderr`; exit 9 in `concurrency::continued_mutation_is_exit_9_and_keeps_the_previous_snapshot`; `rivet-cli/src/transport.rs` (unit) `error_constructors_use_documented_codes_and_exits`. **Defect fixed:** `--help --json` and `--version --json` printed text with exit 0, although the contract says they cannot be combined with `--json`; they are now `invalid_arguments` (exit 2) JSON. **Defect fixed:** every argument error's hint said "Run `rivet index --help`"; it now names the command given. Earlier suites parsed with `serde_json::from_slice`, which accepts whitespace and does not check the single trailing LF; the T36 helper checks both. Exit 1 (`general`) has no reachable trigger to test. | added in T36 |
| init/snippet | `init_json` (22 tests, including `repeated_runs_change_nothing`, the symlink and wrong-kind rejections); `snippet_json` (18 tests, including `both_files_without_snippet_file_is_invalid_arguments`, `malformed_or_duplicate_blocks_are_rejected_without_writing`) | covered |

## Filesystem

Required: root boundary and exclusion rules honored; no unsafe writes or reads.

| Case | Tests | Status |
|---|---|---|
| Worktree `.git` file | `filesystem::a_worktree_whose_git_is_a_file_is_the_root_and_its_git_file_is_not_indexed` (real `git worktree add`); `rivet-core/src/walk.rs` (unit) `worktree_git_file_at_the_root_is_not_eligible`. Root selection was already covered (`init_json::inner_worktree_git_file_is_the_root`, `rivet-core/src/root.rs` (unit) `inner_git_file_beats_outer_git_dir`). **Defect fixed:** the worktree's own `.git` file was indexed as an unsupported file, so a worktree reported one more file and a different snapshot than a clone of the same tree. | added in T36 |
| Nested repo | `rivet-core/src/walk.rs` (unit) `nested_repository_with_git_dir_is_skipped`, `nested_repository_with_git_file_is_skipped`. T36 adds `filesystem::a_nested_repository_and_a_submodule_are_excluded_from_the_outer_root` (end to end, and the nested repository as its own root). | covered |
| Symlink cache | `init_json::symlinked_rivet_dir_is_rejected_and_tree_unchanged`, `symlinked_config_is_rejected_and_tree_unchanged`; `rivet-store` (unit) `symlinked_rivet_dir_is_rejected`, `symlinked_index_db_is_rejected`; `rivet-core/src/root.rs` (unit) `symlinked_rivet_dir_is_rejected`. T36 adds `filesystem::a_symlinked_cache_directory_database_or_config_is_refused_without_writing` (every query command). | covered |
| Outside symlink | `rivet-core/src/walk.rs` (unit) `symlink_to_file_and_dir_are_skipped`; `rivet-core/src/source.rs` (unit) `symlink_to_valid_file_is_skipped`. T36 adds `filesystem::symlinks_are_never_followed_even_to_php_outside_the_root`. | covered |
| FIFO | `filesystem::a_fifo_is_never_opened_and_the_walk_never_blocks` (30 s kill guard); `rivet-core/src/walk.rs` (unit) `a_fifo_git_ignore_file_is_refused_without_being_opened` (thread + timeout guard, releases the FIFO on timeout). Before T36, `fifo_is_skipped` and `fifo_is_not_regular_without_blocking` (unit) covered a FIFO *source* file. **Defect fixed:** a FIFO named `.gitignore` (at any depth) or `.git/info/exclude` hung every refresh forever, because the `ignore` crate opens those files; they are now checked and refused with exit 3. | added in T36 |
| CRLF/Unicode | `filesystem::crlf_and_non_ascii_source_report_utf8_byte_coordinates` (hand-counted bytes, lines, UTF-8 byte columns for a symbol, a reference and `file:line`; non-ASCII path and name kept as UTF-8). Before T36, `symbol_source::crlf_source_is_preserved_byte_for_byte` and `multibyte_prefix_does_not_shift_the_method_bytes` checked bytes only, and `rivet-core/src/span.rs` (unit) `line_and_byte_column_ignore_display_width` checked columns outside the CLI. | added in T36 |
| (extra) Permission denied | `failure_boundaries::an_unreadable_file_fails_refresh_instead_of_keeping_old_facts` (file); T36 adds `filesystem::an_unreadable_directory_fails_refresh_with_exit_3` | covered |
| (carried) Nested `vendor/` excluded | Every default exclusion applies at any depth (`rivet-core/src/walk.rs` (unit) `default_excludes_apply_at_any_depth`), which drops customised package templates under e.g. `resources/views/vendor/`. Spec §26 does not say whether the defaults are root-only. A spec issue, not a code defect. | gap → T36b |
| (found) Git ignore sources outside the root | In a worktree, the shared `info/exclude` in the common Git directory is not read (the main checkout honours it, the worktree does not), and a symlinked `.gitignore` is followed to its target, possibly outside the root (Git ≥ 2.32 refuses to follow it). Both need a decision about reading outside the root. | gap → T36c |

## Determinism

Required: same query bytes for the same supported snapshot and options.

| Case | Tests | Status |
|---|---|---|
| Sequential/parallel extraction | Extraction is sequential; no parallel path exists to compare (IMPLEMENTATION-PLAN: "Start sequentially; add Rayon when measured"). Concurrent processes are compared in `determinism::concurrent_queries_on_an_unindexed_tree_agree_byte_for_byte`. | n/a |
| Clean rebuild versus incremental | `determinism::a_clean_rebuild_and_an_incremental_history_give_identical_query_bytes` (fifteen navigation queries, successes and errors, after an eleven-step history, versus a cold build, `--force`, and a deleted cache). Before T36 only digests and bindings were compared: `refresh_writes::a_no_change_refresh_matches_a_fresh_rebuild_of_the_same_tree` (lib), `force_rebuild::forced_digest_equals_normal_digest_on_the_authored_fixture`, `index_uses::index_persists_gold_uses_scopes_and_reindexes_identically`. | added in T36 |
| Repeated query | `refs_json::repeated_queries_are_byte_identical`; `context_json::repeated_invocations_are_byte_identical`; `symbol_calls_json::repeated_symbol_queries_are_byte_identical`; `context_fit::fitting_is_deterministic_across_snapshots`. T36 adds `determinism::repeated_queries_are_byte_identical_for_every_query_command` (with `--no-refresh`). | covered |

## Summary

Counts are over the matrix's own cases; rows marked (carried), (found) or
(extra) are listed above but not counted.

| Area | Cases | Covered before T36 | Added in T36 | Gap → subtask | n/a |
|---|---|---|---|---|---|
| Freshness | 6 | 5 | 1 | 0 | 0 |
| Cache invalidation | 5 | 4 | 1 | 0 | 0 |
| Atomicity | 4 | 4 | 0 | 0 | 0 |
| Parser failures | 4 | 2 | 2 | 0 (+1 carried: T36a) | 0 |
| Resolution | 7 | 5 | 2 | 0 (+1 found: T36d) | 0 |
| Identity | 4 | 0 | 4 | 0 | 0 |
| Budget | 6 | 5 | 1 | 0 | 0 |
| Output | 5 | 4 | 1 | 0 | 0 |
| Filesystem | 6 | 3 | 3 | 0 (+2: T36b carried, T36c found) | 0 |
| Determinism | 3 | 1 | 1 | 0 | 1 |
| **Total** | **50** | **33** | **16** | **0 (+4)** | **1** |

## Where BUILDING.md's named suites actually live

`docs/BUILDING.md` "Tests" names files that do not exist. Recorded here because
implementors may not edit it.

| BUILDING.md names | Does not exist | Coverage actually lives in |
|---|---|---|
| Snapshot: `crates/rivet-cli/tests/snapshots/`, `insta` | No `snapshots/` directory; `insta` is not a dependency; `cargo insta review` does nothing | `golden/t34-json` checked by `human_output::json_output_is_byte_identical_to_the_pre_t34_binary`; the per-command `*_json.rs` suites assert exact fields and key order |
| Coverage/bindings: `coverage.rs` | No such file | Gold spans: `tests/gold/check_gold.py`, `index_uses.rs`, `bindings.rs`, `refs_json.rs`; coverage counts: `coverage_honesty.rs`, `failure_boundaries.rs` |
| Determinism: `determinism.rs` | Did not exist before T36; now holds the T36 cases | `determinism.rs` plus the repeated-query tests listed above, `force_rebuild.rs`, `refresh_writes.rs` |
| Freshness: `freshness.rs` | Did not exist before T36; now holds the branch-switch and ignore-rule cases | `refresh.rs`, `reresolve.rs`, `freshness_modes.rs`, `refresh_writes.rs`, `concurrency.rs` (concurrent refresh), `freshness.rs` |
