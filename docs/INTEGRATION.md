# Using Rivet with Coding Agents

> **Status:** integration plan, not a released integration. Rivet has no executable yet. Rivet commands below describe the intended MVP. Host documentation was checked on 2026-09-22; record actual host versions when testing.

## What the user installs

Rivet is a local executable used by the user's existing coding agent through its shell tool. The initial integration has two parts: the binary on PATH and the short [navigation instruction block](AGENT-SNIPPET.md) in the target project's instructions.

| Layer | Purpose | Plan |
|---|---|---|
| Rivet CLI | Index, find definitions/references, return compact context | Required MVP |
| Project instruction block | Tell the existing agent when and how to call Rivet | Default MVP integration |
| `rivet` skill | Reusable workflow, discoverable or explicitly invoked | Optional after the PHP pilot |
| Separate agent | Delegate exploration to another model session | No default integration; introduces another orchestration step |
| MCP server / plugin | Additional tool transport or distribution | Deferred until CLI adoption is measured |

The skill would contain instructions for the same CLI. It would not install a second model or make Rivet itself an agent. Skills and project instructions do not install the binary or override host permissions.

## Normal Codex session

Once a verified Rivet release exists, install it in the environment where Codex runs, following [INSTALL](INSTALL.md). In the **project being worked on**, not the Rivet source repository:

```bash
cd /path/to/your-project
rivet --version
rivet init --write-snippet --snippet-file AGENTS.md
codex
```

An explicit snippet destination is created if missing and updated idempotently if present. Existing content outside the managed block stays intact. Start a fresh Codex session after setup: Codex builds its project instruction chain when a session starts. [Official Codex instruction guide](https://learn.chatgpt.com/docs/agent-configuration/agents-md).

Then ask a normal coding question:

```text
Find why SurveyService.launch can create duplicate jobs, fix it,
and run the relevant tests.
```

The intended agent behavior is to choose the appropriate query directly, for example:

```bash
rivet context SurveyService.launch --tokens 3000 --json
```

If it needs all call sites, it can then request `refs`. If it only needs a definition location, it can start with `symbol`. There is no required `symbol → refs → context` sequence and no routine indexing step for the user. The existing coding agent reads the result, reasons, edits source using its usual tools, and runs tests.

Instructions encourage adoption; they cannot guarantee every agent will choose Rivet. For a first smoke check, explicitly ask: “Use Rivet to find the launch definition and show which command you ran.” For adoption measurement, use the ordinary task wording with no extra nudge.

## Normal Claude Code session

Use the same executable and choose Claude Code's instruction file explicitly:

```bash
cd /path/to/your-project
rivet --version
rivet init --write-snippet --snippet-file CLAUDE.md
claude
```

Ask the same ordinary coding question. Claude Code reads `CLAUDE.md` project guidance. Current versions also document conditional `AGENTS.md` support; explicit `CLAUDE.md` setup avoids depending on that fallback. [Official Claude Code instruction guide](https://code.claude.com/docs/en/memory).

For a team using both tools, either maintain the same managed block in both files, or keep it in `AGENTS.md` and manually add `@AGENTS.md` to an existing `CLAUDE.md`, preserving its other content. Claude documents that import. Choose one strategy to avoid duplicated instruction text; the MVP installer only manages blocks and does not rewrite imports. [Shared instruction files](https://code.claude.com/docs/en/memory#share-one-file-with-other-coding-tools).

## Optional skill experience

A thin, instruction-only skill named `rivet` can make the workflow reusable across projects and explicitly selectable. Author it once in a future `integrations/skills/rivet/SKILL.md` distribution asset; install a copy into the host's supported discovery location. That asset does not exist yet and is not needed for the snippet pilot.

| Host | Project skill location | Personal skill location | Explicit invocation after installation |
|---|---|---|---|
| Codex CLI / IDE | `.agents/skills/rivet/SKILL.md` | `~/.agents/skills/rivet/SKILL.md` | `$rivet Find references to SurveyService.launch` |
| Claude Code | `.claude/skills/rivet/SKILL.md` | `~/.claude/skills/rivet/SKILL.md` | `/rivet Find references to SurveyService.launch` |

Codex can select skills by description or explicit mention, loading the body when selected. Claude Code supports relevant-task activation and `/skill-name` invocation. These are host features; the Rivet skill still needs authoring and testing. See [official Codex skills documentation](https://learn.chatgpt.com/docs/build-skills) and [official Claude Code skills documentation](https://code.claude.com/docs/en/skills).

Keep the skill small and portable: `name`/`description` plus plain instructions, with no host-specific shell interpolation, hooks, subagent launch, or permission grants. Its workflow should:

1. Confirm the binary is available once per session or after a command-not-found error. If absent, explain the setup requirement and use normal tools; do not silently download software.
2. Choose context for understanding a known symbol, symbol for locating it, or refs for its uses. Avoid fetching all three by default.
3. Check ambiguity, coverage, resolution tiers, and truncation; recover with a specific ID/page or a focused text read.
4. Keep output bounded and use live source verification before editing.

Use the full snippet alone for the initial pilot. Evaluate skill-only or a short discovery hint plus skill separately; do not quietly add another prompt treatment to the baseline. No skill installer flag or new Rivet command is required for the MVP.

## Environment requirements and troubleshooting

The executable must be available to the **agent's shell environment**. Installing it on a laptop does not place it in a remote container, cloud runner, or another worktree's environment. The agent needs read access to source and write access to `.rivet/` for automatic indexing; navigation does not edit source, but it does maintain its cache. Normal host sandbox/approval rules continue to apply.

| Symptom | Check / response |
|---|---|
| `rivet: command not found` | Check `rivet --version` inside the actual agent shell; install/provision there through the normal approved setup process. |
| Agent never uses Rivet | Confirm the project instruction block is loaded, restart after instruction changes, and try one explicit request. Inspect overrides/conflicting project guidance. |
| Index cannot be written | Ensure the selected project's cache is writable under the host's policy; fall back to normal reads if unavailable. Do not disable the sandbox as an integration step. |
| Empty or partial references | Inspect coverage and tiers; unsupported/dynamic uses require text tools or other project tooling. |
| Read-only cached workflow | `--no-refresh` requires a compatible prebuilt cache and reports cached data. Never treat it as an automatic substitute for a failed refresh. |
| Skill missing | Check its installed path/name and the host's skill selector; restart if discovery has not updated. The snippet path remains usable. |

For other coding agents, the same CLI can be described in their supported project instructions if they have shell access. Do not advertise compatibility with a particular host until its smoke checks have run.

## Integration acceptance and task placement

The implementation queue tracks this under T33a–T33c; the optional skill is T40a. Installer fixtures are ordinary local checks. Live host smoke runs need installed hosts, appropriate access, and the agreed small usage budget.

For each tested host, record version, OS/environment, binary version, instruction/skill hash, and transcript. Verify fresh setup, repeat setup without duplicate blocks, ordinary task adoption, explicit invocation, ambiguity recovery, partial coverage, missing binary behavior, and refresh after an edit. Use the same PHP fixture and expected source spans for both hosts. Untested hosts remain “planned,” not “supported.”

Integration instructions must be shipped/versioned with the binary's command contract. Keep [AGENT-SNIPPET](AGENT-SNIPPET.md) as the canonical MVP text; compare shipped text to that source in the CLI tests. No user-wide instructions or permissions are modified by `rivet init`.
