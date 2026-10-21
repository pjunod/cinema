# Engineering knowledge pilot status — owners, evidence and next work

**Status:** open · **Updated:** 2026-10-21 · **Owner:** Codex integrator

Companion to the [approved pilot plan](ENGINEERING-KNOWLEDGE-GRAPH-PILOT.md)
and [review closure](ENGINEERING-KNOWLEDGE-GRAPH-REVIEW.md). This is the live
execution page: source ownership, milestone acceptance and publication.
The plan specifies behavior; this page records actual work and changes in
execution instructions. No benchmark savings are established.

## Current work — M0 independent curation

Agent-owned clone, branch `effort/engineering-knowledge-pilot`, based on
`168fe74fb2a47f3ccd56b795741dfe3422307b0d` (current Forgejo main at setup).
Tree: `ea8c9c283a71b67bf773511144eb3ee7ab09ee9c`. The reviewed specification
SHA-256 is `473d391e9b6a1e20ea1ecdd9d93b46339e69ba31923bd85f60bcc6a534d2821c`;
its exact bytes are retained in agent-owned review inputs. Subsequent ledger
updates are implementation status, not a new claim of Fable approval.

| Owner | Scope | State / next action |
|---|---|---|
| Codex integrator | Public manifest, status/docs, source baseline and Git/PR integration; later graph-record author | Active. Must not read private gold answers. |
| Sol 6.1 curator A | Private routing gold; no source or graph-record writes | P02 feasibility case in progress. |
| Sol 6.1 curator B | Private evidence gold; no source or graph-record writes | E02/E06 feasibility cases in progress. |
| Luna | Mechanical skill-source lookup, read-only | Done. Only installed cache located; maintained source/update path unverified. |

M0 manifest: [pilot.json](../../validation/knowledge/pilot.json). It contains
cohorts, subjects, source fingerprints, fixed limits and future commitment
slots. Gold answers, hidden mutations and rubric content stay outside Git,
record-author and future solver corpora. Only verified digests/counts enter
the public record after independent cross-adjudication.

## Milestone acceptance — distinguish source from qualification

| Stage | Actual state | Remaining acceptance |
|---|---|---|
| M0 baseline | Current main pinned; 68 exact-schema routing rows and six selected IDs verified | Revalidate selected anchor/signature provenance; initial three gold lists, then all twelve independently sealed. |
| M0 skill | Installed cache found, maintained source unknown | Carry repository-owned skill and small amendment; do not edit cache. |
| M0 feasibility | Three cases assigned to independent curators | Cross-adjudication, measured curation effort and pass/fail feasibility receipt. |
| M0 seal | Not sealed | Freeze all cases, corpus identities, gold/rubric commitments and evaluator-role isolation before M1. |
| M1–M5 | Not started | M0 acceptance first; implementation then final review/fixes/fast lane. |
| M5b | Observation not started | Ten real eligible PRs and 14 days after advisory capture reaches main; does not block tooling landing. |

No unit suite, compiler, benchmark run, CI workflow or hardware campaign was
run for M0. JSON parsing, hashes and source-schema inspection are preparation,
not unit-test or qualification claims. The approved plan's earlier 28 doc
checks remain historical evidence for that reviewed document revision.

## Execution decisions — latest owner instructions govern

**D01, 2026-10-21:** The owner requests lower models for simple tasks,
proper commits batched into one larger main-bound PR, adversarial review only
when ready for main, then fast-lane tests once on the candidate. Repeat only
failed or genuinely invalidated evidence. This supersedes the plan's per-task
PR/test timing in §8. M0–M5 remain milestones and separate coherent commits
on one owned effort branch. Do not manufacture task PRs or run unit suites
at each milestone. Preserve mandatory final merge gates; if CI imposes an
unavoidable conflicting workflow, report it rather than weakening policy.

**D02, 2026-10-21:** All implementation uses an agent-owned clone. The two
pilot documents and their index/backlog entries created during planning were
copied and hash-verified there; only those assistant additions were removed
from the owner's checkout. Unrelated owner work was preserved. Durable
artifacts stay in owned storage; no temporary-directory storage.

**D03, 2026-10-21:** Gold authors and scorers do not author graph records.
The integrator receives only metadata, commitments and feasibility results.
A later solver must receive an explicit corpus export with no gold files or
future-answer Git objects and tools restricted to that export; a separate
folder without tool isolation alone is not sufficient.

**D04, 2026-10-21:** The 108-run comparison remains behind a measured dry
run, finite aggregate ceiling and frozen model/budget. M0 does not purchase
credits, use a reset, start a paid benchmark or treat unset budget as zero.

## Publication — one owner and an honest landing state

Owner: Codex integrator. PR: not opened; this avoids spending draft-CI units
before the owner's requested final review/test point. The branch will carry
coherent commits and one batched main-bound PR, with owner, exact head,
review state, failed/passing evidence and next action updated in its body
and comments. No merge or deployment has occurred. Any pause after a PR
exists must end with a current status update on that PR.
