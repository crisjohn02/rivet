# TE1: token-economy audit of the pilot and held-out transcripts (exploratory)

Read `AGENTS.md` first. Then read only:

- `benchmark/results/heldout-01/RESULT.md`, `benchmark/results/pilot-03/RESULT.md` and
  `benchmark/results/pilot-04/RESULT.md`.
- `docs/AGENT-SNIPPET.md` and `docs/OUTPUT-CONTRACT.md`, the human (text) output sections of
  `symbol`, `refs` and `context`.
- `benchmark/runner/transcript.py` and `benchmark/runner/extract.py`, to reuse their parsers.
- `benchmark/runner/TASK-FORMAT.md`, "runs.csv", for the metric definitions.

## Why

The held-out study ended `inconclusive`: C/B input tokens 0.960, with an upper bound of 1.011.
Arm C made 21% fewer tool calls at equal success, but used about the same tokens.

A first breakdown by the orchestrator (arm C against arm B) suggests four causes:

- **Snippet prefix:** arm C's context starts about 680 tokens larger, and every model request
  re-sends it.
- **Output size:** a `rivet context` call averages about 2,500 tokens, and a `rivet symbol` call
  about 1,070.
- **Re-reading:** agents still read the same code with `sed` or `Read` after rivet.
- **Errors:** there were 5 `ambiguous_symbol` exits.

Your job is to measure all of this properly and rank the levers by estimated token saving. This
is **exploratory**: the held-out tasks are already spent, and nothing here is evidence for a
claim.

## Inputs (private; read only)

- Runs:
  - `~/pilot-runs-rivet/pilot-01` … `pilot-04` (pilot project);
  - `~/pilot-runs-rivet/heldout-01` (held-out project).
  - Each has `runs/<attempt>/record.json` and `transcript.jsonl`, and a `runs.csv`.
- The frozen rivet binary, `~/pilot-runs-rivet/frozen/rivet-9838fc5`. You may run it only on a
  scratch copy of a corpus project that you make under `~/pilot-runs-rivet/analysis/TE1/scratch/`.
  Never run it in `~/rivet-corpus`.

## Rules

- **Nothing is modified.** No model, `claude` or agent process. No change to rivet, the runner,
  the corpus, the gold, or any run directory.
- **Write only under `~/pilot-runs-rivet/analysis/TE1/`:** scripts, data and the private report.
  Write nothing into any git repository.
- **Two outputs, both in `~/pilot-runs-rivet/analysis/TE1/`:**
  - `REPORT-private.md` may use task IDs and anything else.
  - `SUMMARY-public.md` must contain only aggregate numbers and generic descriptions of rivet's
    own output format, which is public. It must have no identifiers, paths, code, domain words or
    task text from either private project. The orchestrator will commit it after review.
- **Label every estimate as an estimate.** Show its arithmetic, and keep measured and
  counterfactual numbers apart.

## Questions to answer, each with numbers per arm, per project and per category

1. **Where the total input tokens come from.** The primary metric counts input, cache reads and
   cache creation over every request, so the context is re-counted on each request. Split each
   run's total into:
   - the fixed prefix (system prompt, tool definitions and, in C, the managed CLAUDE.md), multiplied
     by the number of requests;
   - tool results, each multiplied by the number of later requests that carry it;
   - the model's own messages, replayed the same way.

   Give the snippet's exact overhead per request and per run, and its share of arm C's total.
   Show how the fewer requests in arm C trade against its larger items.
2. **rivet output anatomy.** Take each rivet invocation's actual tool result from the transcripts:
   - size, as characters and estimated tokens, distributed by command;
   - the share of each output part: header, count and tier lines, reference or call rows,
     containing-symbol and path columns, source excerpts, coverage notes, hints, and the
     exclusion line;
   - the number of rows returned against the number the agent's final answer used, where you
     can determine it.
3. **Re-reading.** After each rivet call, did the agent read the same file or lines within the
   next three tool calls? Measure the overlap in lines and the duplicated tokens. Separate reads
   that fetch source rivet did not show (the `symbol` default shows no body, for example) from
   true duplicates.
4. **Errors and retries.** For every rivet non-zero exit, give the command form, the error, what
   the agent did next, and the tokens that retry cost.
5. **Arm B's strategy for comparison.** The Grep, Read and rg pattern, with sizes. Where does B
   win, and on which categories?
6. **Pilot against held-out.** Explain the pilot's larger effect (per-task ratio about 0.85)
   against the held-out one (0.98), using the measures above: output sizes, request counts and
   prefix share.
7. **Ranked levers with counterfactual estimates.** Each lever gets estimated tokens saved per C
   run, and the resulting C/B ratio if it were applied to the held-out runs as replayed
   arithmetic. Candidates, plus any you find:
   - a snippet of about 150 tokens;
   - leaner default `symbol` or `refs` text output;
   - a smaller default `context` budget or a leaner layout;
   - an option for `symbol` to return the body in place;
   - better ambiguity output, so the next call succeeds.

   State each lever's risk to honesty and correctness, and whether it needs a spec or contract
   change.

## Report (your final message; public-safe, as in SUMMARY-public.md)

Give the ranked levers table and the headline numbers for questions 1–6. The private details go in
`REPORT-private.md`.
