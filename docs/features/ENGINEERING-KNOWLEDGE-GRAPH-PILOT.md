# Engineering knowledge graph — trace code to intent and evidence

**Status:** M0 frozen; M1–M5 source prepared, qualification pending · **Written:** 2026-10-21 ·
**Executes:** the owner's request for a bounded plurx pilot incorporating
Astra's assessment and Fable 5.1's independent review · **Owner:** Codex integrator

Companion to [VALIDATION](../VALIDATION.md) (behavior and check ownership),
[the development pipeline](../DEVELOPMENT_PIPELINE.md) (review and landing),
and [the documentation router](../README.md) (where documents live). This
is the implementation contract and canonical pilot ledger. Work through the
milestones in §8; record changes and acceptance in §11. If a step appears to
require changing runtime behavior, evidence-adoption policy, or running a
hardware campaign, record the proposed scope change before proceeding.
The [plan review and dispositions](ENGINEERING-KNOWLEDGE-GRAPH-REVIEW.md)
record the independent review; conceptual advice alone is not plan approval.

## 1. Outcome — an agent can explain what governs a change

Build a small engineering knowledge graph that connects code to reference
sections, decisions, plans, individual review findings, open obligations,
check definitions, and execution evidence. Support both starting from an
arbitrary piece of code and asking what applies to a proposed diff. A list
of document names alone does not satisfy this plan.

The pilot answers with relationship paths, supporting passages, authority
and applicability limits. Required paths include code → contract → decision
→ review finding → obligation, and contract → check → execution receipt;
plans connect through `delivered-by` and reference sections through
`specified-by`. File adjacency alone does not satisfy either path.

The graph model is the product. Storage starts as a rebuildable SQLite
property graph, with explicit node and edge tables and bounded traversal.
This is a concrete implementation choice for the pilot, not a decision that
graph databases cannot help. A backend-neutral API leaves that choice open.
§9 separates the value of curated knowledge from traversal, and defines when
to evaluate an external graph engine.

**Success means:** fewer missed governing constraints, honest answers about
unknown or disputed knowledge, and a measured maintenance and execution
cost. There is no promised token-saving percentage.

## 2. Existing contracts — adapt them without duplicating ownership

The source baseline inspected for this plan is `d1f61377096074b915856ae5c913e1dc68885738`.
Re-verify these interfaces and current document status at M0; the repository
will move before implementation. The examples below describe existing code,
whereas §4–§9 specify new interfaces that do not exist yet.

| Existing artifact | Existing contract | Pilot integration |
|---|---|---|
| [Routing decisions](../../tests/playback/routing-decisions.toml) | Each row has exactly `id`, `source`, `source_anchor`, `test`, `test_anchor`. | Read-only adapter; namespace the existing ID rather than add fields. |
| [Routing inventory test](../../tests/validation/test_playback_routing_inventory.py) | Checks exact row fields, documentation parity and live anchors. | Preserve it; new knowledge tests check adapter identity and provenance. |
| [Functionality catalog](../../validation/points.toml) and [runner](../../validation/runner.py) | `Point(id, title, contract, paths, checks, depends_on)` owns behavior-to-check selection. | Import point/check identities and dependency direction; do not replace CI selection. |
| [Documentation indexes](../README.md) and [index tests](../../tests/operations/test_docs_index.py) | Folder ownership, discovery and link integrity. | Retain navigation; add semantic links separately. An index status is a claim, not automatic authority. |
| [Rust source expansion](../../validation/rust_modules.py) | Existing inventories resolve split product modules through `module_source`. | Validate anchors consistently, preserving actual physical file provenance. |
| [Python receipt policy](../ci/PYTHON-UNIT-PR-RECEIPTS.md) | Authenticated history is adopted only when applicable; unadoptable history runs again. | Read metadata through existing validators; graph output cannot authorize adoption. |
| [Preflight adoption](../../validation/main_preflight_adoption.py) and [input descriptor](../../validation/main-preflight-inputs.json) | Source/input, environment and run identity constrain reuse. | Import proof scope and missing prerequisites; never infer pass from a pathname. |
| [Ripwire](../ci/RIPWIRE.md) | Bounded, heuristic source navigation and impact; exclusions are explicit. | Compose with its output when available; heuristic impact never proves exhaustive scope. |

For example, the existing routing adapter must accept this unchanged row:

```toml
[[decisions]]
id = "server.learned-decode-limit-expiry"
source = "crates/plurx-core/src/playback/caps.rs"
source_anchor = "pub fn applies_at("
test = "crates/plurx-core/src/playback/tests.rs"
test_anchor = "an_expired_limit_hands_the_decision_back_to_the_client_that_can_retest"
```

Its derived identity is `plurx:route:server.learned-decode-limit-expiry`.
Record the source manifest, row ID and content digest as provenance. This
derived node is not an architecture decision merely because the manifest
calls its rows decisions. It becomes a contract/check relationship; a
reviewed semantic record supplies governing design and plan relationships.

For the evidence adapter, the existing
`main_preflight_adoption.input_digest(commit, row)` binds selected Git
objects/modes, input patterns, pathset membership and optional history.
`python_unit_receipts.SourceApplicability` explicitly limits its guarantee
to method/local-fixture applicability, not arbitrary production inputs.
Preserve that distinction when exposing either result through the graph.

## 3. Boundaries — two cohorts, no repository-wide backfill

### 3.1 Playback cohort: one routing family

Seed only these six existing routing IDs and the source/test anchors they
name: `server.capability-profile`, `server.learned-decode-limit`,
`server.learned-decode-limit-expiry`, `server.verdict`,
`server.manual-quality`, and `server.execution-plan`. M0 verifies that each
still exists. Use [the playback inventory](../PLAYBACK.md#routing-inventory--every-fork-has-one-owner-and-one-regression-pin)
and the existing functionality-point relationships as initial evidence.

Find and review the actual governing reference, design, implementation plan
and review passages for this family. Link passages that exist; where a role
has no reviewed match, return `not-recorded`. Do not invent a design document
or treat a title match as proof. Bound semantic curation to this family and
its directly relevant decisions, including explicit constraints against
unsupported capability or expiry assumptions.

### 3.2 Evidence cohort: Python/main-preflight receipt applicability

Use the source-applicability and authenticated-receipt boundary in the
[receipt policy](../ci/PYTHON-UNIT-PR-RECEIPTS.md),
[main receipt code](../../validation/main_unit_receipts.py),
[effort receipt code](../../validation/python_unit_receipts.py), and
[preflight adoption code](../../validation/main_preflight_adoption.py).
Read only the selected input-digest, identity, applicability and restoration
functions plus their focused tests. The cohort covers these four claims:

1. A named check is not an execution receipt.
2. Source compatibility, authenticated origin and environment are distinct.
3. Missing or unusable historical evidence gives no adoption credit; it
   does not create a new blocker in an otherwise valid current lane.
4. Compile evidence cannot stand in for behavior or physical acceptance.

Use synthetic metadata and existing public-safe fixtures. Do not fetch
private CI artifacts, credentials, media, or device data. Selectively index
approved receipt metadata and cited historical passages; broad archive,
Apple-build and evidence-directory ingestion stays outside this pilot.
Metadata exclusions must remain visible in every answer.

### 3.3 Fixed limits and non-goals

- At most 60 authored semantic records and 12 evaluator tasks. Imported
  source anchors and manifest-derived nodes do not consume that limit.
  Record counts and curation minutes; exceeding the cap needs a documented
  scope revision before adding work.
- No new source parser or replacement for Ripwire. Only the pilot anchors
  and their reviewed dependencies enter the derived graph.
- No running Neo4j/FalkorDB service, embeddings, autonomous agents, remote
  MCP service, UI graph explorer, or whole-repository AI back-extraction.
  These are optional follow-on work, not pilot prerequisites.
- No Rust, Swift, Kotlin or product-web behavior change; no Developer card
  or toggle. This is internal engineering tooling with no runtime setting.
- No replacement of receipt validators, CI selection, merge gates,
  regression fields, or qualification policy. Existing gates remain decisive.
- No blanket doc-link requirement for every source file, and no claim that
  graph consistency proves prose truth or complete test coverage.
- Do not edit the owner's unrelated consolidated acceptance matrix.

## 4. Graph contract — explicit meaning, provenance and disagreement

### 4.1 Authored records and derived nodes

Put new data under `validation/knowledge/`: `records.toml`, `coverage.toml`,
`conflicts.toml`, and a schema-versioned pilot manifest. Use one repository
identity, `plurx`, and globally unambiguous IDs `plurx:<kind>:<local-id>`.
Preserve imported catalog identities with prefixes such as `point`, `route`
and `check`; authored IDs are stable even if titles and paths change.

| Authored kind | Required information |
|---|---|
| `contract` | Behavior, source scope, authoritative document section, check IDs. |
| `obligation` | Required or forbidden action, scope, originating authority, disposition, responsible role. |
| `decision` | Rationale, applicable scope, decision status, source section, explicit supersession links. |
| `finding` | Review identity, finding ID, severity, scope, accepted/rejected/unresolved disposition and resolution claim. |
| `check` | Existing check reference or exact test selector, kind, governed IDs; definition only. |
| `receipt` | Execution identity, check ID, source tree/revision, environment, result, scope descriptor, provenance and artifact digest/reference. |

`receipt` is a normalized view emitted/imported from an execution record,
not a human-authored success assertion. A local statement about a run is
retained as `reported`, with no authenticated credit. Larger artifacts stay
in existing evidence storage; Git holds reviewed metadata and safe locators.
Do not publish internal URLs or local absolute paths in public-mirrored data.

Plans, reviews, reference documents and build ledgers are derived artifact
nodes with a `role` and section anchors; they need not become a seventh
authored kind. Modules, symbols, checks imported from catalogs, source
snapshots and document sections are derived nodes too. This preserves a
queryable graph without requiring authors to maintain a second file tree.

Use reviewed stable section IDs mapped to an exact heading path and digest.
An explicit `<!-- knowledge-id: ... -->` marker can replace that mapping
when the document is next edited. Missing, ambiguous or duplicate anchors
fail validation; numeric line locations are display hints, not identities.
Source anchors must retain both the physical path and owning module; a
split-module match is not permission to omit the child from invalidation.

### 4.2 Relations, authority and lifecycle

Schema v1 permits `implements`, `specified-by`, `constrained-by`,
`delivered-by`, `assessed-by`, `creates`, `checked-by`, `executed-as`,
`supersedes`, `resolved-by`, `depends-on`, and `mentions`. Enforce valid
endpoint kinds. A `mentions` edge never grants governing authority. Inferred
candidate edges live outside the accepted graph and are not CI obligations.

Every accepted semantic edge carries its author/reviewer attribution,
originating section/record, source digest and revision. Attribution records
what was actually reviewed; a generated reviewer name grants no trust.
Keep acquisition method (`parsed`, `imported`, `reviewed`, `candidate`)
separate from authority and from evidence authentication.

Contract applicability is `active`, `superseded` or `withdrawn`. Obligations
have `open`, `implemented-awaiting-evidence`, `satisfied`, `waived` or
`superseded` dispositions. A finding can be `unresolved`, `accepted`,
`rejected` or `resolved`; resolution names a corrective change and any
remaining evidence requirement. A merge or commit reference alone cannot
produce `satisfied`. Waivers include reason, owner and bounded scope.

Status assertions remain claims with source revision and attribution. Use
explicit accepted supersession/precedence to determine the governing claim;
neither newest date nor index wording wins automatically. Return unresolved
disagreement as `conflicted`. Synthetic fixtures exercise contradictory
headers; real ambiguities require adjudication before resolution is encoded.
Never label a whole document superseded because one section was amended.

### 4.3 Coverage, freshness and evidence are independent

Coverage declarations apply only to reviewed scopes and name an owner and
reviewed revision. Levels are `none`, `structural`, or `curated`; an empty
answer always means **no known applicable records**, never proof that no
obligation exists. Overlapping declarations report their combined boundaries
and unresolved conflicts rather than selecting the broadest assertion.

Each query separates these dimensions:

| Dimension | Values and interpretation |
|---|---|
| Anchor integrity | `valid`, `changed`, `missing`, `ambiguous`. |
| Semantic applicability | `reviewed`, `needs-review`, `unknown`, `conflicted`. |
| Evidence origin | `authenticated`, `reported`, `unavailable`. A hash establishes integrity, not trusted origin. |
| Source compatibility | `exact-tree`, `compatible-within-reviewed-scope`, `stale`, `unknown`. |
| Execution | `pass`, `fail`, `skip`, `not-run`, plus original check kind and environment. |

Path stability is a rejection filter, not an if-and-only-if proof. For a
receipt at R and target H, compare additions, removals, renames, file modes,
symlinks and bytes over its reviewed dependency closure, input descriptors,
check definition and applicable configuration/toolchain identities. Include
dirty and relevant untracked files. A changed dependency yields `stale`;
an incomplete closure, missing Git object, changed environment or untrusted
receipt yields `unknown` or `unavailable`, never an assumed pass.

Use existing receipt validators' stricter verdicts when present. The graph
can explain why evidence appears applicable, but cannot mint a production
qualification receipt, skip a test, authenticate a run, or weaken an existing
validator. Preserve failures and skips alongside passes. Do not convert
`compatible-within-reviewed-scope` into a claim of current-tree execution.
`authenticated` is copied only from an existing validator's verdict, with
validator identity and scope retained; graph code cannot assign it. Synthetic
fixtures exercise that path without claiming real authenticated evidence.
Real receipts unavailable to this pilot remain `unavailable`.

Each check/receipt names its canonical dependency descriptor and digest.
Import the applicable row of the existing input descriptor, or catalog point
paths and transitive `depends_on` providers. Catalog impact scope alone is
not a complete evidence-input proof. Authored records may select a narrower
retrieval view but cannot remove inputs from an imported proof closure or
invent broader qualification. Missing completeness remains `unknown`; a
proposed closure change belongs to the existing descriptor and its review.

## 5. Index and queries — a real graph with bounded answers

Implement new modules in `validation/knowledge/` for models, adapters,
snapshot capture, index construction, queries, lint and evaluation. Use
Python 3.11+ standard library (`tomllib`, `sqlite3`, `unittest`); no service
or network dependency. `scripts/knowledge` is the thin CLI entry point.

The derived database has `nodes`, `edges`, `claims`, `source_anchors`,
`receipts` and `coverage` tables, with repository/snapshot keys and provenance
references. Parameterized SQL and bounded recursive traversal are internal
implementation details. Cyclic dependencies are allowed with visited sets;
supersession cycles, dangling endpoints and duplicate accepted IDs fail lint.
No model-generated SQL or free-form Cypher execution is exposed initially.

The cache identity includes repository identity, HEAD tree, relevant index
and working-tree bytes, selected untracked paths, adapter/schema versions,
and record/input-descriptor digests. Capture a stable snapshot and verify
inputs before publishing an index atomically. If a file changes during a
query, retry once then return `snapshot-changed`; never mix revisions.
Separate caches by worktree identity and snapshot. Read historical Git blobs
without checkout mutation or network fetch; missing history is explicit.
Only the explicit cohort/input allowlist selects untracked files; ignore
credentials, generated caches and unrelated owner work. No credential scan
or directory-wide untracked ingestion is required.

Use bounded excerpts for documents and symbols. Treat imported prose as
source data, never as tool instructions. Refuse paths escaping the repository,
unsafe artifact references and symlink escapes. Do not follow arbitrary
document URLs during indexing. Keep databases and run logs out of Git.

The following commands and signatures are **proposed interfaces**, to be
implemented and tested in M1–M3:

```bash
scripts/knowledge index --snapshot worktree             # rebuild local graph
scripts/knowledge context --path <file> --symbol <name> # start from code
scripts/knowledge obligations --staged                  # staged change only
scripts/knowledge obligations --base <ref> --worktree   # full proposed change
scripts/knowledge evidence <id> --at <revision>          # inspect execution scope
scripts/knowledge why <id>                              # trace governing reasons
scripts/knowledge conflicts --scope <cohort>             # inspect disagreement
scripts/knowledge lint --scope pilot                    # deterministic contracts
```

`--symbol` is optional; default scope is the supplied file. Diff modes use
Git's NUL-delimited paths, include deleted/renamed source and report unknown
symbol resolution. Impact expansion can consume Ripwire's receipt and
candidates, but failure or heuristic coverage is explicitly reported; direct
path/record matching remains available without installing Ripwire.

Query commands accept `--max-depth` (default 6, range 0–8),
`--format json|text` and `--max-bytes` (default 8192 UTF-8 bytes).
Depth 0 returns only direct record/anchor matches, without relationship
expansion; this also applies to `why` and `evidence`. Depth 6 enables the
pilot's multi-hop paths. Both modes use the same precomputed per-record
metadata, ordering, excerpts and response envelope. Byte limits are
deterministic, not exact token limits; actual tokens are measured in §9.
JSON is the canonical
response, containing snapshot, coverage, included/excluded scopes, records,
relationship paths, excerpts, conflicts and evidence dimensions from §4.3.

**How to read it:** constraints and conflicts appear before historical
context. Every answer names omitted counts and a bounded continuation handle;
the JSON does not list thousands of omitted IDs outside the size limit.
If mandatory identity/coverage/conflict information cannot fit, return
`budget-too-small`. Errors never return an apparently complete empty result.
At normal depth, `why` must demonstrate a multi-edge code/contract/decision/review
or contract/check/receipt path; a list of adjacent filenames fails M3.

## 6. Capture and enforcement — maintain the knowledge as work happens

Keep all existing inventories canonical. Read-only adapters normalize IDs;
authored relationships refer to them without adding columns to the exact
routing-row schema or changing the `Point` dataclass. Update adapter tests
when an upstream schema changes rather than accepting unknown fields silently.

Review output should include structured finding records and dispositions.
Decision authors record scope and explicit supersession when they decide.
Normalize existing receipt output read-only; new tooling checks may emit a
small local receipt, clearly distinguished from authenticated CI evidence.
No CI execution or adoption policy changes are necessary for the pilot.

For the two curated scopes, add optional PR-body fields:

```text
Knowledge-Obligation: plurx:obligation:<id>
Knowledge-Decision: plurx:decision:<id>
Knowledge-Impact: reviewed-no-change; <reason>
```

These fields record acknowledgment, not proof of satisfaction. One ID per
line; multiple lines are allowed. `reviewed-no-change` needs a concrete reason
and does not suppress matched obligations. Commit trailers may carry the
same fields, but the PR body is the pilot's canonical capture surface; do not
introduce another landing-message requirement beside existing regression
fields. Missing fields are initially advisory. Invalid supplied IDs fail
`scripts/knowledge lint --body-file <file> --base <ref>`, invoked by the
existing PR preflight with its locally captured body. Local
`make validation-lint` checks graph data/schema; without a body it explicitly
reports PR capture as unchecked. Paperwork integration only moves the same
check earlier. Do not auto-edit a user's PR description.

Add behavioral tests under `tests/validation/test_knowledge_*.py` and a small
operations smoke test for the CLI and real pilot records. Existing validation
test discovery already includes the new unit modules; explicitly register
the pilot command/data ownership in the catalog and affected scope descriptors
so receipt reuse cannot hide changes to graph validation. Integrate the fast
schema/reference lint with `scripts/paperwork` only after timing proves its
incremental cost is at most one second on the current runner; otherwise keep
it in ordinary preflight. Do not add a separate expensive CI lane.

Hard checks cover schema, ID/anchor/endpoint integrity, reviewed scopes,
conflict dispositions, and evidence labeling. An unresolved conflict is
usable data, not an unconditionally red repository. Track each recognized
conflict with responsible role, reason and revisit condition; lint fails new
unregistered contradictions or invalid dispositions. Warn on review due
dates; never silently expire or resolve a claim by date.

An eligible PR is a landed PR whose diff touches a curated scope in
`coverage.toml`; freeze that scope set when M4's advisory workflow reaches
main. Count distinct PRs once, recording absent/invalid capture as failures.
The process window starts then and closes after both ten eligible PRs and
14 calendar days. M5 can land comparative results with capture advisory;
later M5b may promote acknowledgment to failure only if §9's process
threshold passes and a dated §11 decision records promotion. Until then
retain advisory mode. A field cannot prove semantic correctness.

### 6.1 Documentation skill and repository workflow

Add a repository-owned skill at
`.agents/skills/engineering-knowledge/SKILL.md`, linked from `AGENTS.md`,
with these steps: query context/obligations before pilot changes, inspect
authority and coverage limits, maintain accepted findings/decisions in the
same change, and preserve execution-vs-definition distinctions. The skill
contains the workflow, not the full record corpus. Read-only investigations
start with `context`; patches and reviews also use `obligations`.

Update the user's project-documentation house skill to discover and honor
this repository's knowledge contract. M0 locates that skill's maintained
source and supported install path; do not edit a versioned plugin cache as
the durable source of truth. If that source is unavailable, deliver the
exact small skill amendment with M4 and record installation as incomplete;
the repository skill/AGENTS integration remains independently usable. This
plan does not modify the installed personal skill or add enforcement today.

## 7. Tests — prove the failure boundaries, not prose spelling

Use temporary isolated Git repositories and synthetic receipts for unit
fixtures. No real lab, remote CI account or historical private artifact is
required. Name the regression cases in task PRs as the pipeline requires.

| Test family | Required behavior |
|---|---|
| Adapters | Existing row IDs/fields remain valid; missing and ambiguous symbols refuse; split children participate in provenance/invalidation. |
| Multi-hop answers | Code → contract → decision → review → open obligation and contract → check → receipt survive bounded traversal; `mentions` cannot grant authority. |
| Revision isolation | Two worktrees, dirty staged/unstaged changes, untracked inputs, deletion, rename, changed modes, missing history and mid-index mutation cannot share a falsely fresh graph. |
| Evidence | Check-only, skip, failure, reported success, wrong tree, changed dependency/input membership/environment and forged origin never become authenticated current behavioral evidence. |
| Lifecycle/conflicts | Unresolved findings, partial implementation, bounded waivers, explicit supersession and contradictory sections retain their distinct claims. |
| Coverage/budgets | Uncurated and curated empty answers both expose limits; output stays bounded, JSON is valid and omissions/continuations are explicit. |
| Capture | Warning/enforcement modes differ only as specified; IDs must resolve; no-change reasons do not erase obligations; unrelated scopes stay unaffected. |
| Safety | Path traversal, symlink escapes and prose containing agent instructions remain inert; queries do not mutate source or fetch evidence. |

## 8. Delivery — integrate one effort with six reviewable tasks

Use `effort/engineering-knowledge-pilot` for implementation. Base each
`codex/knowledge-mN` task branch on the current effort and target that effort;
shared catalog/Makefile/AGENTS ownership makes the disjoint-main exception
inapplicable. One integrator owns shared files. Task file ownership below
prevents overlapping writers; it does not authorize spawning workers.
Follow the live [pipeline](../DEVELOPMENT_PIPELINE.md) and governing
[contributor rules](../../AGENTS.md); manually dispatch the effort development
gate for each candidate.
Final promotion needs the current main-promotion gate and qualification
receipt. Do not claim development-gate success as release qualification.

All commands below are future acceptance commands once their task exists.
Use `scripts/quiet` for test/build suites and preserve full failure logs.

| Task | Owned output and dependencies | Acceptance before task landing |
|---|---|---|
| M0 · Freeze pilot | This ledger; cohort manifest; task outlines and hidden evaluator gold. Confirm base and maintained skill location. | Three gold lists establish feasibility, then all twelve are independently sealed before M1; record authors cannot read gold answers. Freeze scoring and corpus identities. |
| M1 · Records/adapters | `validation/knowledge/` model/adapter/lint modules and records; adapter unit tests. Depends M0. | `scripts/quiet python3 -m unittest discover -s tests/validation -p 'test_knowledge_model.py'`; exact-source and bad-reference controls pass. |
| M2 · Snapshot graph | Snapshot/index/applicability modules and isolated-Git tests. Depends M1. | `scripts/quiet python3 -m unittest discover -s tests/validation -p 'test_knowledge_snapshot.py'`; all revision/evidence refusal controls in §7 pass. |
| M3 · Agent queries | Query modules, `scripts/knowledge`, bounded-response/CLI tests. Depends M2. | `scripts/knowledge lint --scope pilot`; query suite passes and both cohorts produce a provenance-bearing multi-hop answer under 8192 bytes. |
| M4 · Capture/integration | Integrator owns catalog, Makefile, relevant input descriptor, paperwork, AGENTS and repository skill; capture tests and skill amendment. Depends M3. | Focused capture/operations tests; `make validation-lint`; `make paperwork`; receipt-input closure and diff selection checked; missing PR fields still warn. |
| M5 · Evaluate | Evaluator harness, comparative result, hashes/metrics and backlog update. Depends M4. | §9 runs complete or explicitly inconclusive; land qualified tooling/results with advisory capture. No dependency on the ten-PR window; no unmeasured savings claim. |
| M5b · Observe/promote | Dated §11 process decision; no new implementation milestone. Starts after advisory M4 reaches main. | Observe §6 window; only then decide hard capture enforcement or cohort expansion. No circular prerequisite for M5 or tooling promotion. |

M0 starts by curating only three gold lists: one routing case, one evidence
case and one disagreement case. Stop expansion and report if that takes more
than eight person-hours; reduce scope in a new plan revision rather than
quietly spending weeks labeling history. For the completed pilot, target a
maximum 16 person-hours of semantic/gold curation and record actuals.

The overall planning allowance is 2–4 person-weeks for a bounded prototype
and measured pilot, with low confidence until M0. Re-estimate after those
three lists and again after M3. No repository-wide semantic coverage or
graph-service operations are included in that allowance.

## 9. Evaluation — separate knowledge capture from graph traversal

### 9.1 Freeze twelve cases without leaking answers

Use four investigations, four change-planning/review tasks, and four small
patch tasks. Six tasks belong to each cohort. Before running, M0 records the
exact revision, prompt, visible source/doc corpus, expected obligations,
forbidden conclusions, task type and patch acceptance checks for each case.
At least three cases contain stale/superseded/disputed guidance and two
correctly have no known applicable obligation. The empty cases retain an
explicit coverage limit. Include at least one genuinely multi-hop finding
and one source-compatible-but-unauthenticated receipt.

Start with these case categories; M0 turns each into a frozen runnable case:

| Cohort | Six case subjects |
|---|---|
| Routing | Explicit capability precedence; learned limit; expiry; verdict ownership; manual-quality policy; execution-plan derivation. |
| Evidence | Definition vs execution; input-path membership change; environment mismatch; unauthenticated reported pass; unusable history vs current-run integrity; compile vs behavioral qualification. |

Some cases combine categories to meet the stale/empty/multi-hop requirements.
Use historical pre-change snapshots with only contemporaneous records, or
clearly labeled synthetic mutations. Never expose the future fix, future
review, or gold answer through graph records. Seal all twelve gold lists and
their rubric before M1 record authoring; record only their digests in §11.
Gold authors/scorers are independent of record authors, who see only cohort
definitions and case subjects. All are separate from solver sessions. Gold
and future answers stay outside solver-accessible files, Git objects and
tool/network permissions; a historical checkout alone is not isolation.

### 9.2 Compare three arms on equal information and budgets

| Arm | Available knowledge access |
|---|---|
| A | Current Ripwire, indexes, repository instructions and source/doc corpus. No new semantic records or graph tool. |
| B | A plus curated records served by §5 at enforced `--max-depth 0`: direct record/anchor matches only, same JSON/text envelope, ordering, metadata, excerpts and byte limit as C. |
| C | Same records/interface as B at fixed `--max-depth 6`; only relationship expansion differs. No extra summaries, evidence or privileged presentation. |

Between B and C only enforced traversal depth and its documented skill
instruction differ; A lacks both curated records and their query endpoint.
Keep the same frozen model/version, reasoning settings, maximum task budget,
available test tools and common instructions in all arms. Run three fresh
sessions per case/arm: 108 runs. Where a provider supports seeds, pair them;
otherwise randomize order and label repeats rather than invent seeded
determinism. Do not switch models mid-comparison or share solver transcripts.

Record run IDs, cache state, prices/date if cost is calculated, all available
input/cached-input/output/reasoning usage without double-counting provider
fields, tool outputs, errors, retries and elapsed time. Missing usage is
`unavailable`, never zero. Include failed runs and budget exhaustion. Graph
indexing, semantic curation and maintenance are separate measured costs;
show amortization at 10, 100 and 1000 tasks, without projecting savings as fact.

Run a dry check before starting the 108-run batch. M0 records a finite
aggregate spend/token ceiling from that measured dry run and existing usage
constraints; the harness refuses a batch without one. Stop at the ceiling
and report incomplete pairs; no automatic credit purchase, reset, new paid
provider, or background recurrence is part of this plan.

### 9.3 Scoring and predeclared decisions

Two independent scorers adjudicate gold obligations before execution, then
score anonymized answers against frozen gold IDs. Count an obligation as
honored only if it constrains the proposed answer or patch correctly; merely
listing its ID is insufficient. Record scorer disagreements. Patch tasks
need focused executable checks plus blind review for missed constraints;
tests passing alone do not establish acceptance.

These are chosen pilot thresholds, not claims of statistical power:

- **Keep the records:** B reduces the mean missed-obligation rate by at least
  20% relative to A, or 5 percentage points absolute, with no new critical
  unsupported-authority claim or critical patch regression. When A misses
  nothing, report the benefit question as inconclusive rather than divide by
  zero or claim improvement.
- **Keep traversal:** C improves over B by the same obligation threshold,
  **or** lowers median total task tokens by at least 10% on cases where all
  three repeats in both B and C score correct, while not worsening the overall
  missed-obligation rate. Report all-run completion and usage so excluding failed
  cases cannot manufacture savings. Critical correctness regressions veto
  expansion; analyze ordinary differences case by case. With fewer than
  three qualifying cases in either cohort, token evidence is inconclusive.
  Zero-gold cases score false obligations/unsupported claims separately;
  exclude them from missed-obligation denominators, not correctness checks.
- **Maintainability:** at least 90% of ten eligible pilot PRs capture valid
  knowledge impact without reviewer prompting; median extra authoring effort
  at most ten minutes per PR. Until ten PRs exist this is unproven, not a
  reason to create artificial PRs. Persistent capture cost above the bound
  triggers redesign before adding a third subsystem.
- **Tooling:** warm queries p95 at most one second and cold pilot indexing
  at most five seconds on the recorded reference machine, excluding first
  Ripwire setup; measure and report scope/size. Timeouts return honest partial
  status. These are usability targets, not production service objectives.

Report per-case and per-cohort results, paired differences and uncertainty
intervals clustered by task, plus raw anonymized observations. Twelve cases
cannot justify a precise repository-wide savings estimate. If uncertainty
crosses the chosen margin, call the result inconclusive and design the next
bounded sample; do not keep rerunning until a favorable number appears.

**Disposition:** if B beats A but C does not beat B, retain capture/search
and defer traversal expansion. If C earns its cost, retain the graph API and
expand one reviewed cohort at a time. If B fails or capture does not stick,
stop rollout, preserve useful records and report the failure.

This comparison tests retrieval/traversal, not SQLite versus Neo4j. Consider
a graph-engine bakeoff only after traversal earns value and the same frozen
query workload misses the local latency/memory/concurrency budget or needs
queries the current engine cannot express safely. Feed identical graph
snapshots to candidate engines and compare answer parity, rebuild/update
cost, branch isolation and operating burden before selecting a service.

## 10. Decision rationale — where Astra and Fable changed the design

1. **Graph semantics precede the engine.** Both reviews emphasized authority
   and applicability. The pilot still builds explicit graph traversal, but
   avoids service operations before there is a demonstrated retrieval need.
2. **Keep both code-entry and diff-entry queries.** Fable's obligations-for-
   this-diff query captures change constraints; the owner's arbitrary-code
   navigation requirement remains first-class through `context` and `why`.
3. **Capture at creation, backfill only a bounded cohort.** Fable's structured
   findings and PR fields make relationships maintainable. Astra's reuse of
   inventories avoids independently maintained competing catalogs.
4. **Treat evidence as scoped executions.** Astra separated tests from runs.
   Fable proposed path-stability applicability; §4.3 narrows that proposal
   because unchanged paths cannot authenticate a receipt or prove complete
   dependency/environment compatibility.
5. **Preserve contradictions and knowledge gaps.** Both reviews require
   conflict visibility. Explicit coverage, claim provenance and owned conflict
   dispositions prevent a compact answer from appearing exhaustive.
6. **Measure knowledge quality before declaring efficiency.** The A/B/C
   comparison comes from Astra and Fable. Smaller repeated tasks follow
   Fable; blind scoring, all-in cost and the two different cohorts preserve
   Astra's demand for useful evidence rather than a curated demonstration.

The alternatives are skill/index changes alone (cheap but no relationship
query), records/search alone (arm B), this local property graph (arm C), and
a service-backed graph with broad AI extraction (deferred because authority
and maintenance remain unsolved). The pilot records which alternative wins.

The conceptual reviews are advice, not implementation approval or benchmark
evidence. Fable's conceptual review used the brief without repository access.
Prior graph/context research does not establish plurx savings; §9 must test
them. Earlier percentage estimates remain unmeasured hypotheses.

## 11. Pilot ledger — implementation and acceptance remain separate

| Milestone | Implementation | Acceptance / remaining evidence |
|---|---|---|
| M0 | Frozen; Codex integrator, owned effort branch. | Twelve gold/rubric commitments sealed with 24 independent approvals and zero blockers. Source, cases and scoring frozen; runtime budget/isolation remain M5 acceptance. See [live status](ENGINEERING-KNOWLEDGE-GRAPH-STATUS.md). |
| M1–M2 | Implemented, unqualified. | Typed schema, 27 records, adapters, isolated snapshots and evidence dimensions; final review/checks pending. |
| M3–M4 | Implemented, unqualified. | Bounded queries, scoped advisory capture and repository skill; final review/checks pending. |
| M5 | Source prepared. | Corpus projection/isolation, calibrated finite budget and frozen comparative runs remain; no savings established. |
| M5b | Process window not started. | Begins when advisory M4 reaches main; enforcement/expansion decision after §6 window. Does not block M5 landing. |

Implementation and evidence live on the [status page](ENGINEERING-KNOWLEDGE-GRAPH-STATUS.md); this ledger does not claim runtime qualification or savings. On task landing, update this ledger and
[the project backlog](PROJECT-BACKLOG.md) together with exact revision,
focused regression command, review disposition, gate and retained evidence.
Keep missing implementation separate from missing pilot/process acceptance.
