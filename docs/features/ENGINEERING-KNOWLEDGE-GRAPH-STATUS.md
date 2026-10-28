# Engineering knowledge pilot status — owners, evidence and next work

**Status:** open · **Updated:** 2026-10-21 · **Owner:** Codex integrator

Companion to the [approved pilot plan](ENGINEERING-KNOWLEDGE-GRAPH-PILOT.md)
and [review closure](ENGINEERING-KNOWLEDGE-GRAPH-REVIEW.md). This is the live
execution page: source ownership, milestone acceptance and publication.
The plan specifies behavior; this page records actual work and changes in
execution instructions. No benchmark savings are established.

## Current work — source approved; local qualification recorded

Agent-owned clone, branch `effort/engineering-knowledge-pilot`, based on
`168fe74fb2a47f3ccd56b795741dfe3422307b0d` (current Forgejo main at setup).
Tree: `ea8c9c283a71b67bf773511144eb3ee7ab09ee9c`. The reviewed specification
SHA-256 is `473d391e9b6a1e20ea1ecdd9d93b46339e69ba31923bd85f60bcc6a534d2821c`;
its exact bytes are retained in agent-owned review inputs. Subsequent ledger
updates are implementation status, not a new claim of Fable approval.

| Owner | Scope | State / next action |
|---|---|---|
| Codex integrator | Public manifest, status/docs, source baseline and Git/PR integration; graph-record author | R4 source approved; local qualification recorded. Owns the batched main PR and its current CI/merge state. No private gold access. |
| Sol 6.1 curator A | Private routing gold; no source or graph-record writes | Done. Six routing cases sealed; independent B/C approval. |
| Sol 6.1 curator B | Private evidence gold; no source or graph-record writes | Done. Six evidence cases sealed; independent A/C approval. |
| Sol 6.1 scorer C | Independent gold/rubric adjudication, private corpus projection and trusted study driver; no record writes | Projection independently approved; six exact generic source files independently approved; five affected synthetic checks passed, seven prior passes retained. No gold enters record-author or solver context. |
| Sol 6.1 model/snapshot/query authors | M1–M3 model, adapters, snapshots and boundary-test drafts | R4 source approved; affected qualification recorded. No gold access. |
| Luna CLI/capture/skill authors | Thin CLI, capture-test drafts and repository skill | R4 source approved; affected qualification recorded. |
| Sol 6.1 evaluation/executor authors | Closed corpus proxy, finite budgets, result analysis and OCI boundary | R4 source approved; synthetic checks recorded. Full study unavailable until every runtime prerequisite qualifies. |
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

Astra R1 requested eight source corrections; R2 confirmed those and found
R9 in the generic loader. R3 approved R9's exact-buffer execution fix.
Post-review qualification then exposed warm rebuild cost, one Mock interface
error and nested grader-path wiring. R4 independently approved all three
corrections at `ac0f247b5edfd455a53a3aa35999b9b89cb550ed`, tree
`56669c330f1459444a8f694f2af9ac89d0d306e6`, with all six generic sources
verified. This is source approval, separate from runtime and merge evidence.

Actual qualification after R4: five affected private synthetic checks passed;
seven unaffected prior passes were retained by exact test/behavior AST
commitments. Fresh-cache indexing took **1.296 seconds**; five warm CLI
queries took **0.639–0.696 seconds**, each returning identical 7,991-byte
answers. Both pilot latency targets were met in this bounded local sample;
it does not establish a population P95. Historical failed timings and the
original fixture error remain preserved, rather than overwritten.

The fresh capability-only witness recognized **one of four bundle shapes**
and refused the full study before provider, token-count, credential, container,
staging or acceptance-cache use. The three imported-Python/Rust mechanisms
remain unavailable. This verifies capability refusal, not actual dependency
or OS isolation. No model study or token-saving claim follows.

## Milestone acceptance — distinguish source from qualification

| Stage | Actual state | Remaining acceptance |
|---|---|---|
| M0 baseline | Current main pinned; 68 exact-schema routing rows and six selected IDs verified | Complete. Anchors/signatures revalidated and twelve exact final cases sealed. |
| M0 skill | Installed cache found, maintained source unknown | Carry repository-owned skill and small amendment; do not edit cache. |
| M0 feasibility | Initial three approved by two nonauthor reviewers each, zero blockers | Complete. Initial authoring timestamp gaps are explicit; measured review effort is 5.965 minutes, initial authoring conservative wall-time bound is ten minutes. No execution occurred. |
| M0 seal | Complete. Safe metadata SHA-256 `9c4c69510a157c6bd280c078515f395c258d1ec32d0a75c9600c69dafff23c51` | Runtime isolation and budget activation remain final-harness acceptance; the seal does not claim execution. |
| M1 | Typed schema, read-only adapters and 27 reviewed semantic records authored independently of gold | R4 source approved; actual review-finding/execution evidence gaps remain explicit. Public check evidence is recorded in the owning PR. |
| M2 | Snapshot isolation, rebuildable SQLite index and separate evidence dimensions implemented | R4 warm-cache correction approved. Canonical proof closure remains independent of retrieval scope; check evidence is recorded in the owning PR. |
| M3 | Bounded traversal, continuation binding and CLI implemented | R4 source approved; corrected cold/warm local timings meet pilot targets. Public boundary checks are recorded in the owning PR. |
| M4 | Advisory capture, preflight body wiring and repository skill implemented | R4 source approved; capture remains advisory. Maintained personal-skill installation remains unknown; amendment supplied. |
| M5 | Harness/provider, visibility projection, OCI executor and concrete trusted driver source prepared; clean corpus export independently approved | Capability-only refusal observed (1/4 supported); no provider, scored run or savings result. Full comparison is inconclusive while three families and runtime containment remain unavailable. |
| M5b | Observation not started | Ten real eligible PRs and 14 days after advisory capture reaches main; does not block tooling landing. |

Timing is recorded as shared agent-session wall intervals, including waits,
not exclusive person-time or human labeling cost. Initial authoring lacks
start timestamps; the conservative bound is retained without inventing a
measurement. Later record curation must retain its own timing.

No public unit suite or CI workflow had run at this qualification snapshot.
The owning PR records current fast-lane results, exact candidate identity and
landing state; those live results supersede this dated pre-publication snapshot.
The repository skill format validator passed using an isolated YAML dependency;
initial missing-dependency attempts ran no validation. No installed skill was edited.
No container, provider call or hardware campaign has run.
The approved plan's earlier 28 doc
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
exact-family prerequisite and trusted completed outcome. An earlier static claim of one supported inert-byte bundle was overbroad:
the actual witness observed zero of four because nested grader identities were
flattened. Revised source preserves those identities; its actual supported
count was unset at the source freeze; the subsequent R4 witness observed one supported shape. Three imported-Python/Rust
mechanisms remain unsupported. The full frozen study refuses model spending until every family
qualifies. A capability-refusal witness is separate from OS/dependency proof.
This may yield an explicit inconclusive evaluation; it cannot yield savings.
Main remains `00c74cd8c` at recheck freeze; study baseline remains unchanged.

**D13, 2026-10-21:** Main advanced to `7f5af4524` while the source review
was queued. Merge its already-landed worker-refinement changes, preserving
its workflow/catalog additions. Its P35 backlog identity is retained; this
pilot moves to P36. The frozen study baseline and approved pilot source stay
unchanged. Independent mechanical verification checks the unchanged pilot
source and effective integration before the current-candidate fast lane.
The unstarted R5 integration-only review request was withdrawn to honor the
owner's one-adversarial-review workflow; it grants no new Astra approval.
Actual R4 source approval and unchanged private synthetic/capability evidence
are retained, with exact source and integration identities distinguished.

## Publication — one owner and an honest landing state

Owner: Codex integrator, branch `effort/engineering-knowledge-pilot`. One
batched main-bound PR carries the current head, independent source-review
identity, failed/passing evidence, next action and merge state in its body
and comments. Its authenticated fast-lane journals are authoritative for
public unit and compile evidence. Missing, ambiguous or stale receipts are
not passes; only failed or genuinely invalidated checks repeat. The current
development pipeline's dated correction selects main-bound fast-lane promotion.

Generic study-driver sources are retained privately by exact
[review-input commitments](../../validation/knowledge/evaluation-driver-source.json);
they contain no gold literals, and runtime still requires reviewed source,
queued admission, calibrated finite budget and observed isolation/toolchain
proof. R4 source approval is exact-content; later qualification ledger updates do
not imply an independent review of those historical facts. Runtime still
requires every declared prerequisite; the unavailable study is not run.
At this dated qualification snapshot no merge or deployment had occurred.
The owning PR records subsequent landing state. Any pause with an open PR
ends with its current status update.
