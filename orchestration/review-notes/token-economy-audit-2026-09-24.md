# TE1: token-economy audit of the pilot and held-out transcripts (public summary)

**Exploratory.** The held-out tasks are already spent, and nothing here is evidence for a claim.
It changes no preregistered result. Measured numbers come straight from the transcripts. Every
counterfactual is an **estimate**, made by replaying measured request sizes with one item edited;
the arithmetic is shown.

This summary contains only aggregate numbers and descriptions of rivet's own output format.

## Method in brief

- **Runs:** 276 recorded runs from four pilot studies on one project and the held-out study on a
  second project. Arm B used built-in search and read tools; arm C also had rivet and its managed
  instruction block (the snippet).
- **Request sizes are exact.** Each model request's input size is recorded (uncached input, cache
  creation and cache reads). Summed over a run, these sizes equal the preregistered metric in 276 of
  276 runs, to the token.
  - So the prefix, the number of requests and each turn's growth are all measured.
  - A run's total is `N x P + sum over turns of (turn growth x number of later requests)`, where `N`
    is the number of requests and `P` the size of the first one.
- **What is estimated:** how each turn's growth splits between tool results, the model's own
  message and per-call framing.
  - A least-squares fit over 1,159 measured turn increments gives chars-per-token ratios by content
    class (R^2 0.99).
  - Source code is about 2.85 chars per token; paths and qualified names about 2.1; rivet's
    metadata lines about 2.0; column padding about 24; text-search output about 2.35 to 2.5.
- **rivet output per invocation:** every rivet pipeline was re-run with the frozen binary on a
  scratch copy of the corpus. 204 of 211 outputs matched the delivered tool result byte for byte;
  the rest were located by the output grammar.

## Headline numbers

### 1. Where the input tokens come from (held-out, per run)

| | B | C |
|---|---|---|
| Total input tokens (measured) | 65,897 | 63,266 |
| Requests (measured) | 5.43 | 4.98 |
| First request (measured) | 6,854 | 7,537 |
| Prefix x requests (measured) | 37,305 (57%) | 37,609 (59%) |
| of which snippet (measured) | 0 | 3,401 (5.4%) |
| Tool results, replayed (estimated) | 24,568 | 21,578 |
| Model's own messages, replayed (estimated) | 2,045 | 1,762 |
| Per-call framing and unseen harness text (estimated residual) | 1,979 | 2,318 |

- **The snippet costs exactly 683 tokens per request** (684 on the pilot project): this is C's
  first request minus B's, the same for every task.
  - That is 3,401 per C run on average (1,366 to 6,830 across runs) and 5.4% of C's total.
  - By category, it is 4.0% (tests) to 7.9% (callers) of C's total.
  - An estimated 87 to 217 of the 683 tokens are the host's wrapper around the instruction file,
    not the snippet text.
- **Fewer requests against larger items** (task-weighted C minus B, per run):

  | Term | Tokens |
  |---|---|
  | Snippet | +3,401 |
  | Fewer requests (B's prefix x 0.45 fewer requests) | -3,097 |
  | Smaller tool-result growth (estimated) | -2,989 |
  | Model messages (estimated) | -284 |
  | Residual (estimated) | +339 |
  | **Total** | **-2,631 (C/B 0.960)** |

  - C's turns are larger (1,947 measured tokens per turn against 1,846) but fewer (3.98 against
    4.43), and each is carried by fewer later requests (2.90 against 3.16).
  - The request saving beats the snippet only in the callers and trace categories.
  - Locate-category runs never called rivet (0 of 20), so there the snippet is pure overhead.

### 2. rivet output anatomy (held-out, delivered text)

| Command | Calls | Mean chars | Mean est. tokens | Largest parts, by share of est. tokens |
|---|---|---|---|---|
| `context` | 19 | 9,838 | ~3,700 | source excerpts 78%, segment headers 18%, footer and coverage 3% |
| `symbol` | 57 | 4,367 | ~1,530 (median ~780) | call and caller rows about 78% (location column 34%, symbol column 28%), header 11%, coverage line 4% |
| `refs` | 57 | 772 | ~340 | rows 57%, header line 20%, coverage line 18%, count-and-tier line 4% |
| ambiguity error | 13 | ~780 | ~345 | candidate rows, message, hint, coverage line |

- **Name-only (`?`) rows are about 53% of `symbol` output tokens.** None of the 882 name-only
  `symbol` rows in the held-out runs matched an item of the final answer (heuristic match on file
  and symbol name).
- **`refs` rows were used at about 50%.** Callers-category rows were used at 100%.
- **The `context` budget is not the delivered size.** `--tokens 3000` counts source bytes / 3; in
  model tokens (estimated) the delivered output is about 3,700.
  - A sole `context` turn measured 3,785 tokens on average, including about 140 of framing.
- **Two cheap-looking parts:**
  - Column padding is 28% of `symbol` characters but only about 3% of its tokens.
  - The coverage line is about 60 tokens on every call.
- **A format defect:** a multi-line receiver expression in a `calls` row is printed across several
  lines, and it widens every row's padding.

### 3. Re-reading

- **True duplicates are negligible.** In the held-out runs, agents re-read 38 lines that rivet had
  already displayed, in 151 invocations: about 29 replayed tokens per C run (0.05%).
- **Follow-up reads mostly fetch source rivet did not show.**
  - 72 of 151 invocations were followed by a source read within the same call or the next three
    calls.
  - Of 7,102 lines read:
    - 46% were around a call site rivet listed;
    - 27% were inside a span rivet only pointed to (a `symbol` body, or a `context` segment shown as a
      signature);
    - 12% were other lines of files rivet named;
    - 14% were in files rivet did not name.
  - The replayed cost is about 3,850 tokens per C run (6.1% of C's total).

### 4. Errors

- **13 `ambiguous_symbol` exits in 100 held-out C runs.**
  - The runner's heuristic attributed only the 5 that stood alone in their tool call. The other 8
    were inside chained shell commands.
  - 12 of the 13 queries were unqualified short names.
- **No agent re-ran rivet with a canonical ID (0 of 13).** They used the candidates' locations to
  read the source directly, or switched to text search.
- **Cost:** about 635 tokens per C run (1.0%) as an upper bound.
  - An error alone in its call costs the request that read it, plus its carried text: about 9,300
    to 10,700 tokens each.
  - A successful lookup of a candidate would have been 1.5 to 7 times the size of the error.

### 5. Arm B's strategy

- **Per held-out run:** 3.4 Grep calls in content mode (about 980 estimated tokens each), 1.5 Reads
  of line ranges (about 1,860 each), and 0.7 shell searches or range prints.
- **C's rivet invocations** average about 1,290 estimated tokens each.
- **B used fewer tokens on 10 of 20 tasks.**
  - Dependencies is the one category where B wins on average (C/B 1.07). There, a single `symbol`
    or `context` call (about 2,100 tokens per invocation) cost several times one of B's greps
    (about 490).
  - On one trace task, C paged a long call list and filtered out name-only rows by hand.

### 6. Pilot against held-out

Mean of per-task ratios = 1 + the terms below, each averaged over tasks as a fraction of B:

| Study | Mean per-task ratio | Snippet | Fewer requests | Tool-result growth | Other |
|---|---|---|---|---|---|
| Pilot 02 | 0.858 | +0.056 | -0.128 | -0.051 | -0.020 |
| Pilot 03 | 0.842 | +0.057 | -0.080 | -0.121 | -0.014 |
| Held-out | 0.984 | +0.061 | -0.059 | -0.021 | +0.003 |

- **The snippet costs the same on both projects**, and so do rivet's outputs: about 1.5
  invocations, about 1,800 to 2,000 estimated tokens and about 10% of C's total per run.
- **The pilots' larger effect came from arm B's baseline.**
  - On two of five pilot tasks, B needed many more turns or read whole files. There, rivet removed
    1.0 to 1.4 requests per run and large reads.
  - On the held-out project, B's scoped-search pattern was already cheap. C saved only 0.45
    requests, and the snippet cancelled most of that saving.
- **The prefix is about 70% of C's tokens (mean of runs) on both projects.**

## 7. Ranked levers (estimates, replayed on the held-out runs)

The simulator replays each measured C run with one edit, holding the agent's behaviour fixed
unless stated. C/B is task-weighted, as in the primary metric (measured 0.960).

The preregistered upper bound sat 0.051 above the point estimate. So any single lever below that
size would not have cleared the efficiency gate alone, even if its estimated effect held.

| Rank | Lever | Est. saved per C run | Est. C/B | Arithmetic | Honesty and correctness risk | Spec or contract change |
|---|---|---|---|---|---|---|
| 1 | Snippet of about 150 tokens | 1,574 to 2,221 | 0.936 to 0.926 | (683 - 150 - wrapper 87 to 217) x 4.98 requests; for example, (63,266 - 2,221) / 65,897 = 0.926 | Adoption may change. The short text must keep the name-only, coverage and not-proof-of-completeness clauses. | Snippet and its recorded hash only; this is a new benchmark treatment. |
| 2 | `symbol` call lists default to scoped and exact tiers, with a count of hidden name-only sites | 1,228 | 0.941 | Exact re-render of 49 outputs with `--min-resolution scoped`, plus a 20-token count line; sum of (saved x carry) / 100 runs | No tier changes, and hidden rows are counted and one flag away. A dynamic caller seen only by name drops out of the default view. `refs` keeps its name-only rows. | Yes: symbol output in the spec, and the flag default in the contract. |
| 3 | `context` budget of 1,500 instead of 3,000 | 1,127 (2,000: 704) | 0.943 | Exact re-render of 19 calls; mean 1,640 saved per call, carried about 3.4 times | Less source per call may add follow-up reads, which this replay does not model. | Snippet text, or the config default. |
| 4 | `symbol --source` in place of a later body read (the flag exists; agents never used it) | -979 to +1,232 | 0.975 to 0.941 | The body is added to 20 outputs (about 2,540 tokens each); it saves only if the separate read turn disappears (11 turns) | No honesty risk. Whole-declaration bodies can be large, so the sign depends on whether a turn is saved. | Snippet mention; spec only if made a default. |
| 5 | Shorter coverage line with the same facts, and no pointer to `--json` for diagnostics | 520 (text alone 136) | 0.952 | About 26 tokens x 146 outputs x carry; plus 5 `--json` audits that the pointer prompted (about 2,800 tokens each) | None, if the facts stay. Name a parse-error file inline instead of pointing to `--json`. | Human output text. |
| 6 | Better ambiguity output | 0 to 635 (upper bound) | up to 0.950 | 13 exits, each removed as a turn or as text | It must still refuse to guess. A successful output is larger than the error, so the realistic saving is near zero. | Human error text. |
| - | Combined: ranks 1 + 2 + 3 + 5 | 5,096 | 0.883 | Overlaps resolved per turn | As the parts. | As the parts. |
| - | Combined, optimistic: the above + 4 (turn saved) + 6 (upper bound) | 6,718 | 0.858 | | | |

Smaller levers:

- `context --exclude-tests` at the same budget: 141.
- Segment headers without namespaces: 60.

On the pilot runs, the same combination (ranks 1, 2, 3 and 5) replays to an estimated 4,381 tokens
per C run (C/B 0.738 -> 0.682).

### Other observations

- **Flags:** in 211 invocations, agents used only `--tokens`, `--mode candidates`, `--offset` and
  `--json`. They never used `--source`, `--signature-only`, `--min-resolution`, `--limit` or
  `--kind`.
  - The only list-shaping was piping `symbol` output through a filter that dropped name-only rows,
    which is lever 2 done by hand.
- **`--json` was used only after the coverage line's diagnostics pointer:** 5 times, each piped
  into a text search for diagnostics. This mirrors the pilot-03 finding about the exclusion line's
  pointer.
- **The runner's exit-code heuristic undercounts errors inside chained commands**: 5 attributed
  against 13 real exits.
