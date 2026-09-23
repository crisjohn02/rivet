#!/usr/bin/env python3
"""A fake `rivet` for runner tests: `--version`, `snippet`, and
`init --write-snippet --snippet-file CLAUDE.md --json`. Anything else exits 2.
FAKE_RIVET_SKIP_GITIGNORE=1 makes init skip the .gitignore entry."""

import json
import os
import sys

SNIPPET = "<!-- rivet:start -->\n## Code navigation with rivet (fake)\n<!-- rivet:end -->\n"


def main() -> int:
    args = sys.argv[1:]
    if args == ["--version"]:
        print("rivet 0.0.0-fake")
        return 0
    if args == ["snippet"]:
        sys.stdout.write(SNIPPET)
        return 0
    if args[:4] == ["init", "--write-snippet", "--snippet-file", "CLAUDE.md"]:
        os.makedirs(".rivet", exist_ok=True)
        with open(os.path.join(".rivet", "config.toml"), "w") as handle:
            handle.write("# fake\n")
        with open("CLAUDE.md", "w") as handle:
            handle.write(SNIPPET)
        # Like the real init: ignore the cache in the root .gitignore.
        existed = os.path.exists(".gitignore")
        if not os.environ.get("FAKE_RIVET_SKIP_GITIGNORE"):
            with open(".gitignore", "a") as handle:
                handle.write(".rivet/\n")
        created = [".rivet/", ".rivet/config.toml", "CLAUDE.md"] + ([] if existed else [".gitignore"])
        print(json.dumps({"schema_version": 1, "created": sorted(created), "modified": [".gitignore"] if existed else [], "snippet_file": "CLAUDE.md"}))
        return 0
    print("fake rivet: unsupported", file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main())
