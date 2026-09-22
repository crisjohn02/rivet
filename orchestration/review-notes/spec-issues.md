# Spec and architecture inconsistencies found during implementation

These are places where the documents disagree with each other or with
behaviour they elsewhere require. None is a code defect: each was handled by
following the governing document. They are collected here for whoever next
revises the spec, since implementors are not allowed to edit it.

- **The config template's `exclude` list is decorative.** Spec §22's template
  writes `exclude = ["vendor/**", "node_modules/**", "dist/**", "build/**"]` as
  if these were editable defaults. §26 makes all six default exclusions
  mandatory and says configured exclusions are *additional*, and the walker
  applies them unconditionally. So deleting `vendor/**` from the file has no
  effect. Found in T33a. Either §22 should ship an empty `exclude = []` with a
  comment naming the mandatory six, or §26 should let configuration disable a
  default.
- **Format mismatch on a normal refresh.** ARCHITECTURE says "An index-format
  mismatch rebuilds the disposable cache under the writer lock", but a plain
  refresh refuses any mismatch with exit 3, even an older format, and only
  `index --force` rebuilds. Not observable while format `1` is the only one.
  Found in AF6.
- **`init` and `.gitignore` outside Git.** §25 says to append `.rivet/` "only
  if needed" and does not say whether `init` creates a `.gitignore` outside a
  Git repository. T33a creates one, because `index.db` stores source bytes and
  must not be committed after a later `git init`. A new `.gitignore` is itself
  an eligible unsupported file, so it changes the snapshot digest and makes
  coverage report one more unsupported file.
