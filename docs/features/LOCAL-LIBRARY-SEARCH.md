# Local library search and reusable classification

**Status:** implementation complete; preparing commits and one draft PR.
Adversarial review and final fast-lane validation pending. Not deployed.
**Updated:** 2026-09-17.

## Delivery status

| Stage | Status |
|---|---|
| Default local text search and explicit channel rules | Implemented |
| Background metadata classification and durable corrections | Implemented |
| Optional embedded CPU model, default off | Implemented; actual embedding smoke check passed |
| Channel creation layout and search feedback | Implemented; desktop and 390 px browser checks passed |
| Agent workspace isolation | Standalone clone at `/private/tmp/plurx-search-agent` |
| Commits and combined PR | Preparing |
| Final adversarial review | Pending; one review when ready for main |
| Fast lane and main merge | Pending review fixes |

The owner requested one batched PR, no repeated unit-test runs, one final
adversarial review, then the fast lane and merge. That order applies from
2026-09-17. Focused tests listed below were run before this instruction.
The current pipeline's workflow correction already specifies this order;
older effort/qualification paragraphs are superseded. No feature capability
is compile-gated. Developer settings owns the explicit semantic toggle;
readiness is advisory and cannot disable it.


The default search path uses catalogue text, explicit channel rules, and
background metadata classification. Embedded semantic search is optional and
off by default. This replaces the channel subject worker's Ollama dependency.

## Data flow

```text
Title / overview / genres / existing tags
                 |             TMDB keywords (existing key, optional)
                 +-----------------------+
                                         v
                              finite metadata classifier
                                         |
                              labels + evidence + corrections
                                         |
                     +-------------------+------------------+
                     v                                      v
              local FTS5 search                    exact channel rules

Optional embedded MiniLM -> node-local vectors -> related search suggestions
```

Generated labels are separate from user-editable tags. Each record stores the
source metadata snapshot, classifier version, provider observations, manual
corrections and a revision. Conditional writes fence both source edits and
concurrent corrections. The index excludes stale classification snapshots.

The classifier uses a finite, inspectable vocabulary rather than arbitrary
inferred tags. It distinguishes format (stand-up, documentary, sitcom,
talk-show, concert, animation) from topics and existing genres. Missing
labels do not establish membership. Negative rules and manual exclusions
remain authoritative. Show-level format labels are not blindly inherited by
every episode.

The optional model is
[sentence-transformers/all-MiniLM-L6-v2](https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2),
revision `1110a243fdf4706b3f48f1d95db1a4f5529b4d41` (Apache-2.0).
Its 384-dimensional mean-pooled, normalized embeddings use at most 256 tokens.
Model files have fixed sizes and SHA-256 hashes. CPU-only Candle loads the
weights inside the daemon. Model or index failure cannot replace ordinary
search with an error. Disabling the setting releases the runtime and keeps
its disposable files for reuse.

## API and compatibility

- `GET /api/v1/search` retains its existing response contract.
- `GET /api/v1/search/related` provides optional semantic suggestions.
- `GET /api/v1/search/settings` reports configuration and node observations;
  `PUT` requires an administrator.
- `GET /api/v1/items/{id}/classification` returns labels, evidence, corrections
  and revision; `PUT` requires an administrator and `expected_revision`.
- SQLite migration 61 and Hiqlite migration 41 add classification persistence.
  Backup import carries the durable rows and rebuilds FTS.

Old clients keep using ordinary search. Exact legacy shipped preset strings
map to local rules. Arbitrary prose has no general natural-language parser;
review custom subjects before replacing their existing schedules. Semantic
suggestions are currently exposed by the web search page; native clients can
adopt the additive endpoint independently.

## Validation and decisions

Before the latest CI timing instruction, focused checks passed for local
matching, phrases/aliases/exclusions, correction persistence, metadata-source
and revision fences, index rebuilds, provider keyword parsing, API defaults
and authorization, and web search/settings behavior. The shared storage
contract passed on SQLite and three temporary Hiqlite voter processes. An
actual MiniLM inference check ranked the space description above the cooking
description. Rust 1.97.1 check and Clippy passed on the prior isolated source.
These results are development observations, not final evidence for the new
base or merge candidate.

No further unit suites will run before the final review. The fast lane will
validate the final candidate. Fleet model installation and deployment remain
separate from the requested main merge.

Decisions made without blocking on the owner:

- Use a finite metadata taxonomy and existing TMDB keywords by default; no
  inference service, video decoding, or new metadata API key is required.
- Keep semantic suggestions separate from exact results and channel admission.
- Use a pinned 91 MB CPU model with a disposable per-node cache, default off.
- Preserve manual classification corrections separately from original tags.
- Keep native clients on their existing exact-search API; the semantic API is
  additive, and legacy shipped channel preset text maps to local selectors.
