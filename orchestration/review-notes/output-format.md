# Output format: JSON envelope overhead

Raised by the user during T34: is JSON the right output for an agent to read,
or would a compact text or Markdown form be cheaper? Measured on the authored
fixture with the post-T33b binary.

| Response | Total bytes | Source bytes | Envelope share |
|---|---|---|---|
| `context` on `SurveyService::launch` | 4,142 | 519 | 88% |
| `context` on `SurveyService::relaunch` | 3,627 | 451 | 88% |
| `refs` on `SurveyService::launch`, 6 results | 4,161 | 0 | 100% |

Where the envelope goes:

- Every symbol and reference carries a 71-byte `blake3:` content hash. In the
  refs response that is 12 hashes and 20% of the bytes, and hex tokenizes
  poorly.
- Each reference embeds its containing symbol as a full object, with a second
  hash.
- Source lives inside JSON strings, so newlines become `\n` and PHP namespace
  separators double; the refs response has 30 doubled backslashes.

`--tokens` bounds only the source, by design, so the envelope is unbounded
relative to the requested budget, and the benchmark measures total input
tokens.

## Decision deferred to data

JSON stays the normative contract. Whether agents should read the compact text
form instead is to be decided from T37's measurements on the pinned real
repository, where larger bodies will lower the envelope's share. T37 measures
the same queries in both forms. Options afterwards, each a spec or
benchmark-treatment change for the user to decide:

1. Point the snippet at the text form. The snippet currently says "Add `--json`
   for structured results", and it is the benchmark treatment with a recorded
   hash, so this must be settled before the pilot.
2. Slim the JSON contract, for example one hash per file rather than per
   reference, and symbol IDs instead of repeated objects.
3. Both.

## T34 text form, measured

Once T34 added the compact human form, the same queries on the authored
fixture:

| Query | JSON bytes | Text bytes | Text as share of JSON |
|---|---|---|---|
| `context` on `SurveyService::launch` | 4,142 | 1,201 | 28% |
| `context` on `SurveyService::relaunch` | 3,627 | 1,055 | 29% |
| `refs` on `SurveyService::launch` | 4,161 | 671 | 16% |
| `symbol` on `SurveyService::launch` | 4,286 | 785 | 18% |

The text form keeps the signals an agent needs: the reference total and tier
breakdown, `?` on name-only matches, a `showing 1-2 of 6; next: --offset 2`
pagination line, and a coverage line whenever coverage is incomplete. Bytes are
a proxy; T37 and T47 should confirm with a real tokenizer on the pinned
repository.
