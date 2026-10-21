# Engineering knowledge pilot status — owners, evidence and next work

**Status:** open · **Updated:** 2026-10-21 · **Owner:** Codex integrator

Companion to the [approved pilot plan](ENGINEERING-KNOWLEDGE-GRAPH-PILOT.md)
and [review closure](ENGINEERING-KNOWLEDGE-GRAPH-REVIEW.md). This is the live
execution page: source ownership, milestone acceptance and publication.
The plan specifies behavior; this page records actual work and changes in
execution instructions. No benchmark savings are established.

## Current work — M0 frozen; M1 next

Agent-owned clone, branch `effort/engineering-knowledge-pilot`, based on
`168fe74fb2a47f3ccd56b795741dfe3422307b0d` (current Forgejo main at setup).
Tree: `ea8c9c283a71b67bf773511144eb3ee7ab09ee9c`. The reviewed specification
SHA-256 is `473d391e9b6a1e20ea1ecdd9d93b46339e69ba31923bd85f60bcc6a534d2821c`;
its exact bytes are retained in agent-owned review inputs. Subsequent ledger
updates are implementation status, not a new claim of Fable approval.

| Owner | Scope | State / next action |
|---|---|---|
| Codex integrator | Public manifest, status/docs, source baseline and Git/PR integration; later graph-record author | Active. Must not read private gold answers. |
| Sol 6.1 curator A | Private routing gold; no source or graph-record writes | Done. Six routing cases sealed; independent B/C approval. |
| Sol 6.1 curator B | Private evidence gold; no source or graph-record writes | Done. Six evidence cases sealed; independent A/C approval. |
| Sol 6.1 scorer C | Independent static gold/rubric adjudication; no record writes | Done. Twelve final revisions independently approved, exact hashes sealed. |
| Luna | Mechanical skill-source lookup, read-only | Done. Only installed cache located; maintained source/update path unverified. |

M0 manifest: [pilot.json](../../validation/knowledge/pilot.json). It contains
cohorts, subjects, source fingerprints, fixed limits and future commitment
slots now filled. [Safe seal metadata](../../validation/knowledge/evaluation-seal.json)
records 24 approvals, 56 obligations and zero blockers across twelve cases.
Required 4/4/4 task balance, 6/6 cohorts, two empty controls and five
stale/disputed cases are frozen. Gold answers, hidden mutations and rubric content stay outside Git,
record-author and future solver corpora. Only verified digests/counts enter
the public record after independent cross-adjudication.

## Milestone acceptance — distinguish source from qualification

| Stage | Actual state | Remaining acceptance |
|---|---|---|
| M0 baseline | Current main pinned; 68 exact-schema routing rows and six selected IDs verified | Complete. Anchors/signatures revalidated and twelve exact final cases sealed. |
| M0 skill | Installed cache found, maintained source unknown | Carry repository-owned skill and small amendment; do not edit cache. |
| M0 feasibility | Initial three approved by two nonauthor reviewers each, zero blockers | Complete. Initial authoring timestamp gaps are explicit; measured review effort is 5.965 minutes, initial authoring conservative wall-time bound is ten minutes. No execution occurred. |
| M0 seal | Complete. Safe metadata SHA-256 `9c4c69510a157c6bd280c078515f395c258d1ec32d0a75c9600c69dafff23c51` | Runtime isolation and budget activation remain final-harness acceptance; the seal does not claim execution. |
| M1 | Ready to start after this freeze commit | Implement model/read-only adapters and independently author records. |
| M2–M5 | Not started | Implementation then final review/fixes/fast lane. |
| M5b | Observation not started | Ten real eligible PRs and 14 days after advisory capture reaches main; does not block tooling landing. |

Timing is recorded as shared agent-session wall intervals, including waits,
not exclusive person-time or human labeling cost. Initial authoring lacks
start timestamps; the conservative bound is retained without inventing a
measurement. Later record curation must retain its own timing.

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

**D05, 2026-10-21:** M0 freezes source, cases and scoring before graph authoring.
The measured benchmark budget cannot be calibrated against a harness that does
not exist until M5. Calibration and activation therefore occur on the final
reviewed candidate, within an explicit finite envelope, before any scored
run. This clarifies §9.2 timing without authorizing unset or unlimited spend;
all runtime evaluation remains blocked until that receipt exists. Runtime
files use durable owned storage and bytecode writes are disabled.

**D06, 2026-10-21:** The refreshed main baseline already assigned P34 to
operation-progress verification. This pilot uses P35; the existing project
identity and row remain untouched.

## Publication — one owner and an honest landing state

Owner: Codex integrator. PR: not opened; this avoids spending draft-CI units
before the owner's requested final review/test point. The branch will carry
coherent commits and one batched main-bound PR, with owner, exact head,
review state, failed/passing evidence and next action updated in its body
and comments. The dated workflow correction in the current development pipeline confirms
main-bound fast-lane promotion after review; older full-promotion prose is
superseded there. The actual workflow still runs paperwork for drafts, so
keeping the branch without a PR avoids that early expenditure.
No merge or deployment has occurred. Any pause after a PR
exists must end with a current status update on that PR.
