
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
