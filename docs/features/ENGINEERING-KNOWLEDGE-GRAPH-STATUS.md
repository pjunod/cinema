# Engineering knowledge pilot status — owners, evidence and next work

**Status:** open · **Updated:** 2026-10-21 · **Owner:** Codex integrator

Companion to the [approved pilot plan](ENGINEERING-KNOWLEDGE-GRAPH-PILOT.md)
and [review closure](ENGINEERING-KNOWLEDGE-GRAPH-REVIEW.md). This is the live
execution page: source ownership, milestone acceptance and publication.
The plan specifies behavior; this page records actual work and changes in
execution instructions. No benchmark savings are established.

## Current work — narrow R9 correction ready for same-task recheck

Agent-owned clone, branch `effort/engineering-knowledge-pilot`, based on
`168fe74fb2a47f3ccd56b795741dfe3422307b0d` (current Forgejo main at setup).
Tree: `ea8c9c283a71b67bf773511144eb3ee7ab09ee9c`. The reviewed specification
SHA-256 is `473d391e9b6a1e20ea1ecdd9d93b46339e69ba31923bd85f60bcc6a534d2821c`;
its exact bytes are retained in agent-owned review inputs. Subsequent ledger
updates are implementation status, not a new claim of Fable approval.

| Owner | Scope | State / next action |
|---|---|---|
| Codex integrator | Public manifest, status/docs, source baseline and Git/PR integration; graph-record author | Integrated all eight corrections; same-task recheck next. No private gold access. |
| Sol 6.1 curator A | Private routing gold; no source or graph-record writes | Done. Six routing cases sealed; independent B/C approval. |
| Sol 6.1 curator B | Private evidence gold; no source or graph-record writes | Done. Six evidence cases sealed; independent A/C approval. |
| Sol 6.1 scorer C | Independent gold/rubric adjudication, private corpus projection and trusted study driver; no record writes | Projection independently approved; six generic source files frozen for exact recheck. No gold enters record-author or solver context. |
| Sol 6.1 model/snapshot/query authors | M1–M3 model, adapters, snapshots and boundary-test drafts | Source integrated; final review and qualification pending. No gold access. |
| Luna CLI/capture/skill authors | Thin CLI, capture-test drafts and repository skill | Source integrated; final review and qualification pending. |
| Sol 6.1 evaluation/executor authors | Closed corpus proxy, finite budgets, result analysis and OCI boundary | Source integrated with synthetic/mock test drafts; review and runtime qualification pending. |
| Luna source lookup | Existing governing passage candidates, read-only | Done. Seven bounded citations; no accepted cohort-specific review finding found. |
| Luna | Mechanical skill-source lookup, read-only | Done. Only installed cache located; maintained source/update path unverified. |

M0 manifest: [pilot.json](../../validation/knowledge/pilot.json). It contains
cohorts, subjects, source fingerprints, fixed limits and future commitment
slots now filled. [Safe seal metadata](../../validation/knowledge/evaluation-seal.json)
records 24 approvals, 56 obligations and zero blockers across twelve cases.
Required 4/4/4 task balance, 6/6 cohorts, two empty controls and five
stale/disputed cases are frozen. Gold answers, hidden mutations and rubric content stay outside Git,
record-author and future solver corpora. Only verified digests/counts enter
the public record after independent cross-adjudication.

Astra returned **REQUEST_CHANGES** on candidate `ac7a72c9e` in Hermes
`t_ff338f21`: eight findings concern receipt byte coverage, sealed semantic
visibility, actual graph identity, acceptance infrastructure classification,
thinking protocol settings, per-obligation scorer agreement, historical path
authority and public token evidence. All eight source corrections are prepared for a
same-task exact-source recheck. This is implementation review; the earlier
Fable approval remains specification-only. No test or study result is implied.
Astra R2 subsequently confirmed all eight corrections, then found R9 in the
capability-only source loader. Its verified-buffer execution fix and inert
cache/replacement controls are now frozen for the same-task narrow recheck.
No public graph/query or product source changed for R9.

## Milestone acceptance — distinguish source from qualification

| Stage | Actual state | Remaining acceptance |
|---|---|---|
| M0 baseline | Current main pinned; 68 exact-schema routing rows and six selected IDs verified | Complete. Anchors/signatures revalidated and twelve exact final cases sealed. |
| M0 skill | Installed cache found, maintained source unknown | Carry repository-owned skill and small amendment; do not edit cache. |
| M0 feasibility | Initial three approved by two nonauthor reviewers each, zero blockers | Complete. Initial authoring timestamp gaps are explicit; measured review effort is 5.965 minutes, initial authoring conservative wall-time bound is ten minutes. No execution occurred. |
| M0 seal | Complete. Safe metadata SHA-256 `9c4c69510a157c6bd280c078515f395c258d1ec32d0a75c9600c69dafff23c51` | Runtime isolation and budget activation remain final-harness acceptance; the seal does not claim execution. |
| M1 | Typed schema, read-only adapters and 27 reviewed semantic records authored independently of gold | Final adversarial review and focused checks pending. Actual review-finding/execution evidence gaps are explicit; no invented records. |
| M2 | Snapshot isolation, rebuildable SQLite index and separate evidence dimensions implemented | Final review and qualification pending. Canonical proof closure remains independent of retrieval scope. |
| M3 | Bounded traversal, continuation binding and CLI implemented | Final review, boundary tests and latency measurements pending. |
| M4 | Advisory capture, preflight body wiring and repository skill implemented | Final review and fast lane pending. Maintained personal-skill installation remains unknown; amendment supplied. |
| M5 | Harness/provider, visibility projection, OCI executor and concrete trusted driver source prepared; clean corpus export independently approved | No dry run, provider call, scored run or savings result. Candidate-code acceptance requires verified OS isolation. |
| M5b | Observation not started | Ten real eligible PRs and 14 days after advisory capture reaches main; does not block tooling landing. |

Timing is recorded as shared agent-session wall intervals, including waits,
not exclusive person-time or human labeling cost. Initial authoring lacks
start timestamps; the conservative bound is retained without inventing a
measurement. Later record curation must retain its own timing.

No unit suite, compilation, benchmark run, provider call, CI workflow or hardware
campaign has run for this implementation. Toolchain/image identities were read only. JSON parsing, hashes and source-schema inspection are preparation,
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

**D07, 2026-10-21:** Canonical PR capture is wired into the existing fast-lane
validation step through inert environment data. Missing acknowledgment stays
advisory; invalid supplied identities fail. `--staged` selects staged paths
against explicitly identified worktree knowledge, not staged-tree execution
evidence. No new job or product feature switch is added.

**D08, 2026-10-21:** Sealed source selectors are hard corpus boundaries.
Partial cases expose only line-preserving visible spans after their frozen
synthetic transformations. Neither whole source files nor pristine repair
versions may widen them. Graph excerpts must match actual visible bytes.
Candidate Python/Rust patch execution requires verified filesystem/network
isolation; a separate folder or an instruction alone does not establish it.

**D09, 2026-10-21:** The clean solver export has twelve cases and 71 file
instances (43 whole views, 26 scoped views and two visible overlays), with
independent exact-hash approval. [Projection metadata](../../validation/knowledge/evaluation-projection-seal.json)
records commitments without gold or private paths. Four patch bundles need
real execution; the Rust bundle requires pinned 1.97.1, offline dependencies
and `hiqlite-store`. This Mac's 1.98 is not qualification. Candidate execution
requires verified container containment; a process group cannot prove that
escaped descendants are gone. No new engine/service is installed.

**D10, 2026-10-21:** Evaluation uses a closed source proxy over the sealed
passages. Actual Ripwire execution is unavailable in this owned environment;
common literal search/read/outline tools are an explicit study limitation,
not a claim of full-workflow parity. No outside source, new case constraint
or future answer is introduced to compensate. Model/settings, dry-run usage
and a finite ceiling must be frozen before runtime.

**D11, 2026-10-21:** Current main `957258574` was merged cleanly before
review preparation; the evaluation baseline remains `168fe74fb`. Read-only
inspection found an existing local Linux container engine and cached immutable
1.97.1 runtime images. Toolchain execution, offline dependencies and actual
isolation probes remain unverified until final review. Identical complete
patch/proof inputs may reuse an actual acceptance result; all reuse and
failed runs remain visible. No image pull, container or new service has run.

**D12, 2026-10-21:** Astra R1's eight findings are corrected in source,
with draft negative controls. Semantic projection requires unchanged visible
governing bytes, authority propagates through current paths only, and receipt
witnesses bind source bytes as well as membership. Acceptance requires an
exact-family prerequisite and trusted completed outcome. One inert-byte
bundle shape is supported in source; three imported-Python/Rust shapes remain
unsupported. The full frozen study refuses model spending until every family
qualifies. A capability-refusal witness is separate from OS/dependency proof.
This may yield an explicit inconclusive evaluation; it cannot yield savings.
Main remains `00c74cd8c` at recheck freeze; study baseline remains unchanged.

## Publication — one owner and an honest landing state

Owner: Codex integrator. PR: not opened; this avoids spending draft-CI units
before the owner's requested final review/test point. The branch will carry
coherent commits and one batched main-bound PR, with owner, exact head,
review state, failed/passing evidence and next action updated in its body
and comments. The dated workflow correction in the current development pipeline confirms
main-bound fast-lane promotion after review; older full-promotion prose is
superseded there. The actual workflow still runs paperwork for drafts, so
keeping the branch without a PR avoids that early expenditure.
Generic study-driver sources are retained privately by exact
[review-input commitments](../../validation/knowledge/evaluation-driver-source.json);
they contain no gold literals, and runtime still requires reviewed source,
queued admission, calibrated finite budget and observed isolation/toolchain
proof. Same-task independent source recheck is the next action.
No merge or deployment has occurred. Any pause after a PR
exists must end with a current status update on that PR.
