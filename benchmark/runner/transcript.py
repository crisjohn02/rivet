"""Claude Code stream-json transcript parsing (T38b).

Everything here reads one transcript (`transcript.jsonl`, one JSON event per
line as written by `claude -p --output-format stream-json --verbose`) and
derives the per-run metrics documented in `TASK-FORMAT.md` "runs.csv".
Nothing is inferred: a value the transcript does not support is returned as
`None` and written as `unavailable`.

Heuristic extractors are marked HEURISTIC below and in TASK-FORMAT.md:
rivet command parsing, rivet error attribution, text-tool fallbacks and
the arm-B contamination scan.
"""

from __future__ import annotations

import json
import os
import re
import shlex

# Tool names that edit files. The pilot denies all of them; an attempt is
# still counted if the transcript shows one.
EDIT_TOOLS = ("Edit", "MultiEdit", "NotebookEdit", "Write")
TEXT_TOOLS = ("Glob", "Grep", "Read")

# Objective provider/infrastructure markers (TASK-FORMAT.md "Failure
# classification"). Matched against the result text and assistant text.
PROVIDER_ERROR_RE = re.compile(
    r"API Error: (?:429|5\d\d)\b|overloaded_error|rate_limit_error|\"type\":\s*\"api_error\"|"
    r"authentication_error|Invalid API key|OAuth token has expired|Please run /login",
    re.IGNORECASE,
)

HARNESS_ERROR_PREFIXES = ("API Error", "Invalid API key", "OAuth token has expired", "Please run /login")

SHELL_SPLIT_RE = re.compile(r"\|\||&&|;|\||\n|\$\(|`|\(|\)|&")
ENV_ASSIGN_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*=")
WRAPPERS = {"command", "env", "exec", "nice", "nohup", "time", "timeout", "xargs", "sudo"}
# rivet flags that take a value, so the value is not mistaken for the query.
RIVET_VALUE_FLAGS = {"--tokens", "--offset", "--limit", "--mode", "--snippet-file", "--kind", "--depth", "--root"}
EXIT_CODE_RE = re.compile(r"^\s*Exit code (\d+)")
FRESHNESS_RE = re.compile(r"\"freshness\"\s*:\s*\"([A-Za-z_]+)\"")
# Output only rivet produces: the JSON index block with a blake3 snapshot.
RIVET_OUTPUT_RE = re.compile(r"\"snapshot\"\s*:\s*\"blake3:[0-9a-f]{16,}")
RIVET_WORD_RE = re.compile(r"(?<![\w.-])rivet(?![\w.-])")
# rivet's human error text (crates/rivet-cli/src/human.rs `error_text`): the
# line `rivet <command>: <message>`, the line `hint: <hint>`, then one line
# group per extra field. Captures the command, the message and the line after
# the hint (a lookahead, so a following error's header is not consumed).
RIVET_ERROR_RE = re.compile(r"^rivet (index|init|symbol|refs|context): (.*)\nhint: .*(?=(?:\n(.*))?)", re.MULTILINE)
# The human text does not print the error code. Each entry is (exit, message
# form, form of the first line after the hint) for a code whose text the CLI
# fixes; either form identifies it. Exits are OUTPUT-CONTRACT "Errors".
# `general` and messages that wrap I/O, lock or config errors have no fixed
# form, so they are recognised as rivet errors with no exit.
RIVET_ERROR_FORMS = [
    (exit_code, re.compile(message), re.compile(extra) if extra else None)
    for exit_code, message, extra in (
        ("5", r"query '.*' matched \d+ symbols", r"candidates:"),  # ambiguous_symbol
        ("4", r"query '.*' matched no symbols|no symbol encloses .*", r"did you mean:"),  # symbol_not_found
        ("6", r".* is not indexed: it (?:has a syntax error|exceeds a parser resource limit)", r"detail: .*"),  # parse_failure
        ("7", r".* is .*, which is not indexed|.* is not in a supported language", None),  # unsupported_language
        (
            "8",
            r"the context target needs at least \d+ estimated tokens, but the budget is \d+",
            r"required_tokens: \d+ \(budget_tokens: \d+\)",
        ),  # budget_too_small
        ("9", r"the repository changed while it was being indexed.*", None),  # repository_changed
        ("2", r"invalid value for `.*|.* cannot be combined with .*|`--offset` is not supported by `rivet context`", None),  # invalid_arguments
        (
            "3",
            r"no committed index at .*|the cache has no committed snapshot|incompatible cache: .*|"
            r".* is not indexed: (?:it is binary|it exceeds max_file_size_kb|its content is not valid UTF-8).*",
            None,
        ),  # repository_unavailable
    )
]


def load_events(path: str) -> tuple[list[dict], int]:
    """Returns (events, invalid_line_count). Blank lines are ignored."""
    events, invalid = [], 0
    if not os.path.exists(path):
        return events, invalid
    with open(path, encoding="utf-8", errors="replace") as handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            try:
                value = json.loads(line)
            except json.JSONDecodeError:
                invalid += 1
                continue
            if isinstance(value, dict):
                events.append(value)
            else:
                invalid += 1
    return events, invalid


def _text_of(content) -> str:
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        parts = []
        for block in content:
            if isinstance(block, dict):
                if block.get("type") == "text" and isinstance(block.get("text"), str):
                    parts.append(block["text"])
                elif isinstance(block.get("content"), (str, list)):
                    parts.append(_text_of(block["content"]))
            elif isinstance(block, str):
                parts.append(block)
        return "\n".join(parts)
    return ""


class Transcript:
    """A parsed transcript. Construct with `Transcript.from_file`."""

    def __init__(self, events: list[dict], invalid_lines: int = 0):
        self.events = events
        self.invalid_lines = invalid_lines
        self.init: dict | None = None
        self.result: dict | None = None
        self.messages: dict[str, dict] = {}  # assistant message id -> message
        self.message_order: list[str] = []
        self.tool_uses: list[dict] = []  # {id, name, input}
        self.tool_results: dict[str, dict] = {}  # tool_use_id -> {is_error, text}
        seen_tools: set[str] = set()
        anon = 0
        for event in events:
            kind = event.get("type")
            if kind == "system" and event.get("subtype") == "init" and self.init is None:
                self.init = event
            elif kind == "result":
                self.result = event
            elif kind == "assistant" and isinstance(event.get("message"), dict):
                message = event["message"]
                mid = message.get("id")
                if not isinstance(mid, str):
                    anon += 1
                    mid = f"<anonymous-{anon}>"
                if mid not in self.messages:
                    self.messages[mid] = {"model": message.get("model"), "usage": None, "content": []}
                    self.message_order.append(mid)
                record = self.messages[mid]
                # stream-json repeats a message's usage on every content-block
                # event; keep the last one seen rather than summing them.
                if isinstance(message.get("usage"), dict):
                    record["usage"] = message["usage"]
                if message.get("model"):
                    record["model"] = message.get("model")
                for block in message.get("content") or []:
                    if not isinstance(block, dict):
                        continue
                    record["content"].append(block)
                    if block.get("type") == "tool_use":
                        tid = block.get("id") or f"<anonymous-tool-{len(self.tool_uses)}>"
                        if tid in seen_tools:
                            continue
                        seen_tools.add(tid)
                        self.tool_uses.append(
                            {"id": tid, "name": block.get("name") or "", "input": block.get("input") or {}}
                        )
            elif kind == "user" and isinstance(event.get("message"), dict):
                for block in event["message"].get("content") or []:
                    if isinstance(block, dict) and block.get("type") == "tool_result":
                        self.tool_results[block.get("tool_use_id")] = {
                            "is_error": bool(block.get("is_error")),
                            "text": _text_of(block.get("content")),
                        }

    @classmethod
    def from_file(cls, path: str) -> "Transcript":
        events, invalid = load_events(path)
        return cls(events, invalid)

    # ---- basic facts -------------------------------------------------------

    def assistant_turns(self) -> int:
        return len(self.messages)

    def final_text(self) -> str | None:
        """The result event's text, else the last assistant message's text."""
        if self.result is not None and isinstance(self.result.get("result"), str):
            return self.result["result"]
        for mid in reversed(self.message_order):
            text = _text_of(self.messages[mid]["content"])
            if text:
                return text
        return None

    def all_assistant_text(self) -> str:
        return "\n".join(_text_of(self.messages[mid]["content"]) for mid in self.message_order)

    def models_observed(self) -> list[str]:
        models = {m["model"] for m in self.messages.values() if isinstance(m.get("model"), str)}
        if self.result and isinstance(self.result.get("modelUsage"), dict):
            models.update(k for k in self.result["modelUsage"] if isinstance(k, str))
        return sorted(models)

    # ---- usage -------------------------------------------------------------

    def usage(self) -> dict:
        """Provider-reported token usage summed over every model request.

        Source preference: the result event's `modelUsage` (all models the
        run called), then the result event's `usage`, then the de-duplicated
        per-message usage of assistant events (a run killed before its result
        event; labelled partial). A component missing from the source is
        `None`, and so is any total that needs it.
        """
        out = {
            "usage_source": "none",
            "input_tokens": None,
            "cache_creation_input_tokens": None,
            "cache_read_input_tokens": None,
            "output_tokens": None,
        }
        rows: list[dict] = []
        keys = {
            "input_tokens": "input_tokens",
            "cache_creation_input_tokens": "cache_creation_input_tokens",
            "cache_read_input_tokens": "cache_read_input_tokens",
            "output_tokens": "output_tokens",
        }
        result = self.result or {}
        model_usage = result.get("modelUsage")
        if isinstance(model_usage, dict) and model_usage and all(isinstance(v, dict) for v in model_usage.values()):
            out["usage_source"] = "result.modelUsage"
            keys = {
                "input_tokens": "inputTokens",
                "cache_creation_input_tokens": "cacheCreationInputTokens",
                "cache_read_input_tokens": "cacheReadInputTokens",
                "output_tokens": "outputTokens",
            }
            rows = [model_usage[k] for k in sorted(model_usage)]
        elif isinstance(result.get("usage"), dict) and result["usage"]:
            out["usage_source"] = "result.usage"
            rows = [result["usage"]]
        else:
            rows = [m["usage"] for m in (self.messages[i] for i in self.message_order) if m["usage"]]
            if rows:
                out["usage_source"] = "assistant_messages_partial"
        if not rows:
            return out
        for field, key in keys.items():
            values = [row.get(key) for row in rows]
            if all(isinstance(v, int) and not isinstance(v, bool) and v >= 0 for v in values):
                out[field] = sum(values)
        return out

    # ---- tools -------------------------------------------------------------

    def tool_calls_by_name(self) -> dict[str, int]:
        counts: dict[str, int] = {}
        for use in self.tool_uses:
            counts[use["name"]] = counts.get(use["name"], 0) + 1
        return dict(sorted(counts.items()))

    def bash_command(self, use: dict) -> str | None:
        if use["name"] != "Bash":
            return None
        command = use["input"].get("command") if isinstance(use["input"], dict) else None
        return command if isinstance(command, str) else None

    def edits(self) -> tuple[int, int]:
        attempts = [u for u in self.tool_uses if u["name"] in EDIT_TOOLS]
        failed = sum(1 for u in attempts if self.tool_results.get(u["id"], {}).get("is_error"))
        return len(attempts), failed

    def permission_denials(self) -> int | None:
        value = (self.result or {}).get("permission_denials")
        return len(value) if isinstance(value, list) else None


# ---- rivet command parsing (HEURISTIC) ---------------------------------------


def _segments(command: str) -> list[list[str]]:
    """Splits a shell command into simple commands' word lists. HEURISTIC:
    it does not implement shell grammar (quoted operators split wrongly)."""
    out = []
    for piece in SHELL_SPLIT_RE.split(command):
        piece = piece.strip()
        if not piece:
            continue
        try:
            words = shlex.split(piece)
        except ValueError:
            words = piece.split()
        if words:
            out.append(words)
    return out


def rivet_invocations(command: str) -> list[dict]:
    """Every `rivet` executed by a command, as {subcommand, query, argv}.

    A word counts as the executable when it is the first word of a simple
    command after env assignments and common wrappers, or follows
    `-exec`/`-execdir`/`--pre`, with basename `rivet`.
    """
    found = []
    for words in _segments(command):
        expanded = []
        for word in words:
            if word.startswith("--pre="):
                expanded.extend(["--pre", word[len("--pre="):]])
            else:
                expanded.append(word)
        words = expanded
        positions = []
        i = 0
        while i < len(words) and (ENV_ASSIGN_RE.match(words[i]) or words[i] in WRAPPERS):
            i += 1
        positions.append(i)
        for j, word in enumerate(words):
            if word in ("-exec", "-execdir", "-ok", "--pre") and j + 1 < len(words):
                positions.append(j + 1)
        for pos in positions:
            if pos < len(words) and os.path.basename(words[pos]) == "rivet":
                args = words[pos + 1 :]
                sub, query, skip = None, None, False
                for arg in args:
                    if skip:
                        skip = False
                        continue
                    if arg.startswith("-"):
                        if arg in RIVET_VALUE_FLAGS:
                            skip = True
                        if sub is None and arg in ("--version", "--help", "-h", "-V"):
                            sub = arg
                        continue
                    if sub is None:
                        sub = arg
                    elif query is None:
                        query = arg
                found.append({"subcommand": sub or "(none)", "query": query, "argv": words[pos:]})
    return found


def rivet_calls(t: Transcript) -> list[dict]:
    """Bash tool uses that invoke rivet, in order, with attribution data."""
    calls = []
    for index, use in enumerate(t.tool_uses):
        command = t.bash_command(use)
        if command is None:
            continue
        invs = rivet_invocations(command)
        if not invs:
            continue
        result = t.tool_results.get(use["id"])
        calls.append(
            {
                "index": index,
                "id": use["id"],
                "invocations": invs,
                "sole": len(invs) == 1 and len(_segments(command)) == 1,
                "result": result,
            }
        )
    return calls


def rivet_error_texts(text: str) -> list[tuple[str, str | None]]:
    """(command, exit or None) for each rivet human error in a tool result,
    in order. HEURISTIC: see `RIVET_ERROR_RE` and `RIVET_ERROR_FORMS`."""
    found = []
    for match in RIVET_ERROR_RE.finditer(text):
        message, extra = match.group(2), match.group(3) or ""
        exit_code = None
        for code, message_re, extra_re in RIVET_ERROR_FORMS:
            if message_re.fullmatch(message) or (extra_re is not None and extra_re.fullmatch(extra)):
                exit_code = code
                break
        found.append((match.group(1), exit_code))
    return found


def call_errors(call: dict) -> list[str]:
    """The rivet errors of one rivet call, one key per error (TASK-FORMAT.md
    "Heuristic extractors", Exit codes). HEURISTIC."""
    result = call["result"]
    if result is None:
        return ["no_result"]
    match = EXIT_CODE_RE.match(result["text"])
    if call["sole"] and result["is_error"] and match:
        # One rivet and one exit status: its error text is the same error.
        return [match.group(1)]
    unmatched = [inv["subcommand"] for inv in call["invocations"]]
    keys = []
    for command, exit_code in rivet_error_texts(result["text"]):
        if command in unmatched:
            unmatched.remove(command)
            keys.append(exit_code or "unattributed")
    if not keys and result["is_error"]:
        # The failing status may be another program's, so it is not rivet's.
        keys.append("unattributed")
    return keys


def rivet_metrics(t: Transcript) -> dict:
    """Arm-C rivet adoption metrics. HEURISTIC error attribution, one count
    per error (`call_errors`): a sole rivet command's `Exit code N`, else
    each rivet human error text in the result, else `unattributed` for a
    failed result. A call without a tool result is `no_result`."""
    by_command: dict[str, int] = {}
    errors: dict[str, int] = {}
    freshness: set[str] = set()
    calls = rivet_calls(t)
    for call in calls:
        for inv in call["invocations"]:
            by_command[inv["subcommand"]] = by_command.get(inv["subcommand"], 0) + 1
        for key in call_errors(call):
            errors[key] = errors.get(key, 0) + 1
        if call["result"] is not None:
            freshness.update(FRESHNESS_RE.findall(call["result"]["text"]))
    return {
        "rivet_calls": len(calls),
        "rivet_invocations_by_command": dict(sorted(by_command.items())),
        "rivet_errors_by_exit": dict(sorted(errors.items())),
        "refresh_mode": "|".join(sorted(freshness)) if freshness else None,
        "rivet_to_text_fallbacks": fallbacks(t, calls),
    }


def _targets(query: str | None) -> tuple[set[str], set[str]]:
    identifiers, files = set(), set()
    if not query:
        return identifiers, files
    match = re.match(r"^(.+?):(\d+)$", query)
    if match and ("/" in match.group(1) or "." in match.group(1)):
        files.add(match.group(1))
        return identifiers, files
    identifiers.add(query)
    for part in re.split(r"\\\\|\\|::|->|\.|#", query):
        if len(part) >= 3:
            identifiers.add(part)
    return identifiers, files


def fallbacks(t: Transcript, calls: list[dict] | None = None) -> int:
    """HEURISTIC (BENCHMARK.md "Collected artifacts"): a rivet call counts
    one fallback when either of the next two tool calls is a text tool
    (Read, Grep, Glob, or Bash that does not invoke rivet) whose input
    mentions the rivet query's identifier (whole word, or a `::`/`.`/`\\`
    component of at least three characters) or, for a `file:line` query,
    that file. Files that appear only in rivet's output are not targets."""
    if calls is None:
        calls = rivet_calls(t)
    rivet_indexes = {c["index"] for c in calls}
    count = 0
    for call in calls:
        identifiers, files = set(), set()
        for inv in call["invocations"]:
            ids, fs = _targets(inv["query"])
            identifiers |= ids
            files |= fs
        if not identifiers and not files:
            continue
        for j in (call["index"] + 1, call["index"] + 2):
            if j >= len(t.tool_uses) or j in rivet_indexes:
                continue
            use = t.tool_uses[j]
            if use["name"] not in TEXT_TOOLS and use["name"] != "Bash":
                continue
            text = json.dumps(use["input"], sort_keys=True, ensure_ascii=False).replace("\\\\", "\\")
            hit = any(re.search(r"(?<![\w$])" + re.escape(i) + r"(?![\w])", text) for i in identifiers)
            hit = hit or any(f in text for f in files)
            if hit:
                count += 1
                break
    return count


def contamination_scan(t: Transcript, workspace: str | None = None) -> list[dict]:
    """Arm-B rivet evidence in a transcript. HEURISTIC and deliberately
    conservative: any of these flags the run.

    - `transcript_invocation`: a Bash command executes `rivet` (see
      `rivet_invocations`).
    - `transcript_mention`: a Bash command contains the word `rivet`
      anywhere (after replacing the workspace path), which also catches
      invocations the parser cannot see.
    - `transcript_rivet_output`: a tool result contains rivet's JSON
      snapshot marker.
    The runner adds `workspace_rivet_dir` when `.rivet/` appears in a B
    workspace after the run.
    """
    evidence = []
    prefixes = []
    if workspace:
        prefixes = sorted({workspace, os.path.realpath(workspace)}, key=len, reverse=True)
    for use in t.tool_uses:
        command = t.bash_command(use)
        if command is not None:
            scrubbed = command
            for prefix in prefixes:
                scrubbed = scrubbed.replace(prefix, "<workspace>")
            if rivet_invocations(scrubbed):
                evidence.append({"kind": "transcript_invocation", "tool_use_id": use["id"], "command": command})
            elif RIVET_WORD_RE.search(scrubbed):
                evidence.append({"kind": "transcript_mention", "tool_use_id": use["id"], "command": command})
        result = t.tool_results.get(use["id"])
        if result and RIVET_OUTPUT_RE.search(result["text"]):
            evidence.append({"kind": "transcript_rivet_output", "tool_use_id": use["id"]})
    return evidence


def isolation_violations(t: Transcript, model: str, allowed_builtin_tools: list[str]) -> list[str]:
    """Checks the harness's own init event against the pinned isolation.
    An absent init event is reported, not assumed clean."""
    if t.init is None:
        return ["no_init_event"]
    problems = []
    init_model = t.init.get("model")
    if init_model != model:
        problems.append(f"init_model:{init_model}")
    tools = t.init.get("tools")
    if isinstance(tools, list):
        extra = sorted(set(map(str, tools)) - set(allowed_builtin_tools))
        if extra:
            problems.append("unexpected_tools:" + ",".join(extra))
    else:
        problems.append("init_tools_unavailable")
    servers = t.init.get("mcp_servers")
    if isinstance(servers, list) and servers:
        problems.append("mcp_servers:" + ",".join(sorted(str(s.get("name", s)) if isinstance(s, dict) else str(s) for s in servers)))
    key_source = t.init.get("apiKeySource")
    if key_source not in (None, "none"):
        problems.append(f"api_key_source:{key_source}")
    observed = [m for m in t.models_observed() if m != model]
    if observed:
        # Claude Code may make small auxiliary requests on another model;
        # recorded, not hidden.
        problems.append("other_models:" + ",".join(observed))
    return problems


def provider_error(t: Transcript) -> str | None:
    """The first provider/auth error marker, looked for only where the
    harness reports its own failures: the result event when it is an error,
    and assistant text blocks that begin with a harness error prefix. Agent
    prose and tool output are never scanned, so source code that mentions an
    error name cannot turn a task failure into an infrastructure failure."""
    texts = []
    if t.result is not None and (t.result.get("is_error") or t.result.get("subtype") != "success"):
        texts.append(str(t.result.get("result", "")))
        texts.append(json.dumps(t.result.get("error", ""), sort_keys=True))
    for mid in t.message_order:
        for block in t.messages[mid]["content"]:
            if block.get("type") == "text" and isinstance(block.get("text"), str):
                text = block["text"].lstrip()
                if text.startswith(HARNESS_ERROR_PREFIXES):
                    texts.append(text)
    for text in texts:
        match = PROVIDER_ERROR_RE.search(text)
        if match:
            return match.group(0)
    return None
