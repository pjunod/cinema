# Engineering knowledge efficiency study — measured cost and documentation reliability

**Status:** open · **Updated:** 2026-10-22 · **Owner:** Codex integrator

Companion to the [pilot contract](ENGINEERING-KNOWLEDGE-GRAPH-PILOT.md)
and [execution ledger](ENGINEERING-KNOWLEDGE-GRAPH-STATUS.md). This document
records the bounded comparison and its preregistered measurement amendment.
There are no token-savings or documentation-reliability results yet.

## The frozen comparison

The original pilot landed in PR #1256, merge
`92b259b257cc9abd7d764e78c7b4fff9dd7a0445`. This continuation uses an
isolated agent-owned clone. The **study corpus remains at frozen source
`168fe74fb2a47f3ccd56b795741dfe3422307b0d`**, independently of changes to
the generic harness. Prompts, Gold, mutations and acceptance argv remain
unchanged. No private answers, labels, grader bodies or solver transcripts
enter this document or the record-author context.

| Dimension | Frozen comparison |
|---|---|
| Cases | Twelve: four investigations, four planning/reviews, four small patches; six cases in each cohort |
| Runs | Three arms × three fresh repeats × twelve cases = 108 solver contexts |
| Arm A | Current source/document corpus, common Ripwire and corpus tools; no curated knowledge records |
| Arm B | Same corpus/tools plus curated records at traversal depth zero |
| Arm C | Same corpus/tools plus curated records at traversal depth six |
| Scoring | Two independent fresh blind contexts for every answer: 216 scorer contexts |
| Model | Existing-account Sonnet 5, thinking disabled; identical pinned model/settings in every arm |
| Budget | Existing hard ceiling of 3,000,000 aggregate tokens, including calibration, solver and scoring use; final split must be calibrated and sealed before scored runs |
| Schedule | Seeded randomized order; repeats are independent contexts, without invented deterministic sampling seeds |
| Analysis | Frozen missed-obligation and correctness rules; task-clustered, cohort-stratified uncertainty; failed, exhausted, unscored and incomplete runs remain visible |

Model identity, common instructions, tool schemas, source/record/corpus digests,
navigation assets and limits must be committed in the final execution receipt.
Token reservations include counted input plus maximum output before each
request. Missing or uncertain usage retains its reservation and halts the
batch; it never becomes zero consumption. Scoring receives an allocation
before the solver share is assigned. No model switch, budget increase or
case replacement follows an unfavorable outcome.

The initial Sonnet 5.5 nomination was corrected before calibration or answers:
account capability metadata reports that model cannot disable thinking, which
the current transcript adapter requires. Sonnet 5 supports that mode. This
compatibility correction precedes outcomes and applies to every context.

## Every arm receives actual Ripwire over the same visible corpus

The previous generic proxy offered source reads/search/outline but did not
execute the repository's pinned Ripwire baseline. The comparison now requires
a concrete navigation backend in every arm. Each run receives a fresh
synthetic Git repository containing only its projected visible source and
document bytes, with no ambient history or remotes. Omitted lines are blank
before indexing. Pinned tool bytes are copied into a separate operational
area; these files are unavailable through the corpus interface. Cache state
starts fresh per run and may warm within that run.

Allowed operations are `find`, `outline`, `body`, `callers` and `impact`.
Responses have a byte bound, report truncation and retain the heuristic
coverage warning. An empty result does not establish absence. Query receipts
bind the binary, synthetic source identity and configuration. Operational
setup/download and arbitrary shell execution are unavailable to the solver.

## Acceptance assertions stay outside candidate code

Three frozen acceptance mechanisms require imported Python or compiled Rust.
An outer signed report cannot prove that assertions ran when candidate code
can replace their in-process test framework. The continuation separates
protected assertions from candidate execution. The immutable assertion code
uses a reviewed typed RPC facade to request candidate behavior; candidate
responses are bounded inert data. Different roles, mounts and process
channels keep candidate code away from the protected result channel.

Ordered setup and acceptance steps retain their exact argv, cwd and per-step
environment. Setup completion never counts as passed assertions. Source and
runtime transitions invalidate earlier probes; new probes must cover the
state actually checked. Empty checks, dependency failures, protocol errors
and unavailable containment are unavailable evidence, never passing results.
All four frozen acceptance bundles must qualify before any paid study run.

## Preregistered secondary measure — applied documentation identification

This amendment was defined **before calibration, solver answers or model
calls**. It adds a descriptive documentation metric without changing the
original frozen Gold, honored-obligation labels, denominators or keep/drop
thresholds. The independent custodian derives expected source-path and
heading units from the immutable authority references and visible baseline
sources, without consulting the curated graph. A nonauthor auditor must
check authority, applicability and visibility before the derivation is sealed.
The independently approved packet contains **32 grouped documentation-role
units, 43 propositions, 46 acceptable alternatives and 62 source spans across
twelve cases**. Two cases have undefined empty denominators. A role unit asks
whether the answer locates and applies its governing documentation; it is
not a count of unique filenames. Partial support from an overlapping
alternative does not earn full unit credit.

Independent audit closed all findings before any count request or model
answer. Approved packet SHA-256:
`703fc9afaf2dc7e1d524add3d166dbe48d70c0f9006abb68218264670209cc97`;
embedded policy:
`00a5e45652640282bfe3f20803db2a3e33ec4af63f66f74a13a765fb6f0afda4`;
safe audit receipt:
`5dff121ac1d8e587d6111671c2d60297f935cee177ba2deda9117c14c3212c36`.
This approves the reference derivation and rubric. It does not establish
operational scorer independence or comparative results.

The same citation request appears in the common instructions for every arm:
identify the governing document and section and apply the supported constraint
to the answer or proposed edit. Listing a path in a tool dump, mentioning a
filename incidentally, or making an unsupported authority claim earns no
finding credit. Two fresh blind scorers judge this separate vector alongside
the original rubric, without arm identity, other answers or peer judgments.

| Measure | Interpretation |
|---|---|
| Applied-reference recall | Correctly attributed and applied expected document/heading units divided by the sealed nonempty denominator |
| Document-authority errors | Unsupported source or authority claims, counted separately from omissions |
| Complete documentation success | Every expected unit correctly applied and attributed, with no unsupported authority claim |
| Empty denominator | Undefined recall; excluded from recall averages and reported separately, never automatic success |
| Valid alternative sources | Independently supported visible alternatives can be correct; absence from the expected set alone does not make a claim false |

The final report must distinguish scorer agreement, adjudication and missing
scores. It must publish counts and arm differences, not equate a governing
requirement score with exact-document identification. This small fixed sample
supports a bounded pilot result, not a population-wide accuracy claim.

## Offline dependencies — immutable image, unchanged checks

The acquisition step verified all **626** locked registry archives against
`Cargo.lock`; 27,193 complete source files totaling 522,397,189 bytes then
matched their archived bytes. The derivative toolchain image contains only
those public dependency sources above the pinned base. Source workspaces,
build targets, private assertions and account credentials are separate.
Public example certificates remain ordinary checksum-bound dependency source.
They require no exception to the runtime credential-name guard.

The runtime Cargo home contains a deterministic configuration pointing to
`/opt/locked-crates` in that immutable image. The exact frozen commands and
lockfile remain unchanged. This uses Cargo's documented
[directory source replacement](https://doc.rust-lang.org/cargo/reference/source-replacement.html)
and [offline vendoring](https://doc.rust-lang.org/cargo/commands/cargo-vendor.html).
Image acquisition is separate from actual compilation, containment probes
and acceptance qualification.

## Results and interpretation

**Pending.** No numerical savings, comparative reliability or dollar-cost
claim exists yet. The complete report will provide actual usage by arm,
all-run and correct-paired comparisons, uncertainty, failure counts, applied
documentation scores and the immutable run/score metadata commitments.
Calibration and scoring overhead will be reported separately from solver
usage. The ten-eligible-PR/14-day process observation remains a separate
post-landing measure.
