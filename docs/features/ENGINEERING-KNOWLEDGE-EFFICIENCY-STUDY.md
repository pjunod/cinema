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
batch; it never becomes zero consumption. The solver partition is fixed at 1,500,000 tokens rounded down to equal
shares for all 108 contexts. The scorer pool receives the remaining ceiling
after that reservation and conservatively charged calibration; all 216
scorer shares must fit before answers. No model switch, budget increase or
case replacement follows an unfavorable outcome.

The initial Sonnet 5.5 nomination was corrected before calibration or answers:
account capability metadata reports that model cannot disable thinking, which
the current transcript adapter requires. Sonnet 5 supports that mode. This
compatibility correction precedes outcomes and applies to every context.

Calibration counts all twelve complete static scoring envelopes, a
representative 2,048-UTF-8-byte synthetic answer/edit increment over the
largest measured static token envelope, and the three initial solver
envelopes. Every fixed scorer share must cover its static input, the measured
increment and its output reservation before any study answer is generated.
This representative measurement does not guarantee that every later answer
fits. The complete actual scoring payload is counted against its frozen
per-case input ceiling. An oversized payload is retained as unscored without
truncating required evidence, borrowing another share or suppressing later
contexts. Unknown remote usage or failed durable accounting still stops all
further account calls.

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

## Independent source review before execution

Astra reviewed the complete generic harness at
`6fde3602b997cbdac1db504201661a1c68625df5` on 2026-10-22 and returned
**REQUEST_CHANGES**. The exact execution closure includes 35 Python modules
and 11 immutable assets. The reviewer verified those bytes without reading
private case inputs, Gold, documentation propositions or credentials. Review
is separate from actual runtime qualification and comparative evidence.

| Finding | Required correction | State |
|---|---|---|
| F1 | Give isolated Python roles their separate verified source import roots while retaining isolated mode. | closed by Astra R2 |
| F2 | Classify Rust setup consistently in both phases; setup earns no assertion credit. | closed by Astra R2 |
| F3 | Mount only the immutable wrapper's protected log directory as writable runtime storage. | closed by Astra R2 |
| F4 | Stop later account calls after a count or durable-accounting failure. | closed by Astra R2 |
| F5 | Advertise the actual pinned Ripwire selector contract in every arm. | closed by Astra R2 |
| F6 | Require successful provenance-bound navigation before calibration. | closed by Astra R2 |
| F7 | Preserve later independent scorer shares after known local exhaustion; validate measured envelopes before answers. | closed by Astra R2 |
| N1 | Reproduce the compiler-job setting in the effective controller environment. | closed by Astra R2 |

The revised controller preserves isolated Python startup and separate source
mounts. Only the immutable wrapper's fixed log directory is writable through
protected runtime storage; its source tree remains read-only for assertion
steps and the worker has no access to those logs. Effective environment,
mounts, probes and source bytes participate in qualification identity.
Count and checkpoint failures stop later account work; known local scorer
share exhaustion retains an unscored context and preserves later shares.
The two explicitly enabled actual OCI controls passed: 2 tests, no skips,
8.973 seconds. They exercise isolated role imports and the protected log
mount. Their original receipts remain intact; the later six-line synthetic
engine repair changes neither production source nor the actual OCI class
and its seven methods/helpers. This scoped retention does not claim that
the entire test file stayed byte-identical.

The actual R1 report SHA-256 is
`35c9ef188ca9062f4b0e5697b7881d660aab530f272aeb9f8fdd7e46c58b1dc5`;
its non-authorizing verdict SHA-256 is
`7d8a3ec94058ec704e25c4bb0a5778f81fc93058d11aadaec087c2554049298d`.
Revisions require independent closure over their exact final source map.
No new behavioral tests, token-count calls or inference calls had run when
this review returned. The frozen cases, grader bytes/commands and acceptance
thresholds remain unchanged.

## Final source closure and public verification

Astra R2 closed F1–F7 and N1. The final narrow R4 review approved execution
source at `e39859ea8a7ccb9df34717f68608d549077c38f1`, tree
`d8ab8210c6bbc5c271743d994c0f37c1e0c2f53b`, with no blocking findings.
Its exact map binds all 35 modules and 11 assets. Fixture and ledger commits
through `8caee3d6` leave those 46 execution inputs unchanged. The measured
build-profile correction below changes the isolation module and therefore
requires a new exact-map review before further qualification.

| Independent artifact | SHA-256 |
|---|---|
| R4 review report, actual model `gpt-6-astra`, task `t_633fcc38` | `d3bc2ef19dcb94de0a834aa6346a9592ff3e935a625a341b12ae10e8e663e1b6` |
| R4 authorizing verdict | `82fe72f68c14b8fe2c1d5220ba48a1ef9342fde8efbb7837fc62a2acd08abd98` |
| Exact execution map | `88bd599948abd6952ffef6b97ef008d9a0a2417d4760f03e58c3b412ed1eb302` |
| Source/asset closure | `51f335cb473a0ec16188e5aa75a046a1ab10dc89065409b64c4ba18b5569bdb8` |

The [isolation executor](../../validation/knowledge/evaluation_isolation.py)
now declares the image packaging environment explicitly and verifies kernel
link state, routes and actual network denial. Its immutable Linux probe
reports a versioned guest errno table and semantic labels. Host validation
checks table/label/number consistency and each boundary's permitted outcomes;
it does not interpret Linux errors with macOS constants. All observations,
probe bytes and executor bytes participate in receipt identity. Unknown or
contradictory observations still refuse qualification.

| Public verification | Actual evidence and limit |
|---|---|
| Required CI run 5783 | 2,108 Python passes retained; three failures and two OCI skips explicitly recorded. |
| Three failed synthetic checks | Passed on their own after the fake engine honored Docker's exact owned-container-name filter; 0.869 seconds. Production code was unchanged. |
| Explicit actual OCI controls | Both passed with no skips. Original receipt SHA-256 `f8306ed5b6d96000eb2c30f298fee0bae686990bda58c1ef9dcb980a293534e8`; scoped unchanged-control proof `8bfff8ff0ea817c342e561169a3a1fb4431a6f92ca16a4a430ac978a9087a8d2`. |
| Required CI on `8caee3d6` | Passed, including Rust and Main promotion. It retained 2,066 passes and selected 47 checks: 45 new passes and two OCI skips, in 3.639 seconds. Fresh runs are required; the receipt policy rejects job reruns. |
| First private qualification | Refused at a Rust candidate build: 0/4 family seals, 0/12 navigation views, and zero count/inference calls or study answers. The measured OOM cause is recorded below. |

CI adopts only attributable successes whose declared inputs remain valid;
it must not turn historical passes or the three local repairs into an
unverified whole-tree success. Required CI and actual four-family acceptance
and five-mode navigation qualification remain prerequisites to calibration.
The frozen cases, model settings, original thresholds and 3,000,000-token
ceiling are unchanged. Source approval and prerequisite checks establish no
comparative token savings or documentation-reliability result.

## Pre-answer compiler policy — fit the measured workload

The first actual private qualification stopped after 208.053 seconds, before
calibration. A focused replay of only the failed build measured compiler
exit 101 with SIGKILL, no Rust error codes, and cgroup event deltas of
`oom=1`, `oom_kill=1`, `max=3795`. Docker also reported an OOM kill. The
container's memory and swap limits were both 4,294,967,296 bytes, with two
CPUs and 128 PIDs. This is measured container memory exhaustion; it is not
evidence of a model, source-logic or comparative-arm regression.

The earlier public compiler proof used `CARGO_PROFILE_DEV_DEBUG=0`; the
failed actual build left it unset and emitted `-C debuginfo=2`. Those public
proofs also omitted `hiqlite-store`, so they did not establish resource fit
for the actual feature-enabled prerequisite. The [candidate driver](../../validation/knowledge/study_driver/rust_driver_factory.py)
retains that required feature and its exact frozen commands. Actual Docker
capacity was 8,318,709,760 bytes, so an 8 GiB container limit was not a sound
local repair. No global Docker configuration was changed.

The common study environment will default both `CARGO_PROFILE_DEV_DEBUG`
and `CARGO_PROFILE_TEST_DEBUG` to `0`, alongside `CARGO_BUILD_JOBS=1`, only
for Rust-required execution. Explicit frozen environment values take
precedence. The immutable probe must observe every effective value, and
configuration/probe/source identities must bind them. The memory, CPU and
PID bounds remain unchanged. Cargo documents [debug information](https://doc.rust-lang.org/cargo/reference/profiles.html#debug)
and [debug assertions](https://doc.rust-lang.org/cargo/reference/profiles.html#debug-assertions)
as separate settings; this amendment changes debug symbols, not assertions,
optimization level or feature selection.

This correction was chosen before any count, inference, answer or scorer
existed and applies identically to every arm. It changes generic executor
policy rather than frozen case data or the closed resource-options API.
Actual Astra R5 approved the exact 46-input closure at `9afea577e`; the
focused policy regression passed and required CI completed successfully,
including Rust and Main promotion. Fresh actual qualification remains
required; reducing debug information has not yet been shown sufficient for
the full chain.
The failed stage, warm state, compiler output and cgroup proof are retained.
The safe OOM diagnostic receipt SHA-256 is
`fa744cb45fef6c6798cb6852f9a73a12f0dcb6fbd8b564dd99c3325dc423df8b`.

## Pre-answer environment recreation — explicit new identity

The pinned base and dependency images were absent from the configured Docker
store when the two affected OCI controls reached image lookup. Both controls
refused before creating a container. Exact original image configurations and
build provenance were recovered, but executable layers were unavailable in
the checked store, build cache and agent-owned archives. The previous daemon
identity was not recorded; these observations do not establish why images
became unavailable.

The human authorized the additional public image/tool acquisition, build and
durable archive. The recorded Ubuntu material remained available; the old
Rust material digest returned `manifest unknown`. The replacement uses the
same official Rust **1.97.1** version at its newly resolved native ARM digest.
The original package list is retained; its historical APT versions were not
pinned, so this is a newly identified environment, without an equivalence
claim for the old executable bytes.

| Input or artifact | Replacement identity / observation |
|---|---|
| Ubuntu material | `ubuntu:24.04@sha256:a853f94d226358a79c740cfc7bce0c289748f3fe3488d921d038ccd752c61b60` |
| Rust material | `rust:1.97.1-bookworm@sha256:6e957ef098dcc77d33e310261e4ed5843bb108d5c3b5dc2b476cbc8b6caf53fa` |
| Base image | `sha256:769ee1d1e2205ca810a1f281106a7d30f7aec9c4e375231027c62542ecb896fa` |
| Dependency image | `sha256:198b4ef49a5b5ac641c0b28eedcd30a6a397c337b92fb3b1f0b6d17a976e2741` |
| Dependency receipt SHA-256 | `315feaafc0a9f30bf321811bf1e09b6794a8d5f21afef86876ec6f81196860fa` |
| Native inventory | AArch64; Rust/Cargo 1.97.1, Python 3.12.3, FFmpeg 6.1.1; 405 package versions recorded |
| Durable Docker archive | 2,495,707,136 bytes; SHA-256 `834f8445603fd5c97e3a703c82dda061ff8bdda9b02254208acecffe6fdb3f77` |

The network build contains only the public tool recipe, without candidate
code, private study inputs or credentials. The derived build runs offline,
copying the existing 626 checksum-verified crate sources. Its image
configuration equals the base configuration and its root filesystem adds
exactly one COPY layer; the crate file/mode/checksum manifest is unchanged.
Historical failed recipes, lookup failures and acquisition receipts remain
separate from the replacement receipts. Study execution also requires a retained,
verified durable image export.

The strict base-image admission pin is updated to this exact identity;
receipt schema, baseline/lockfile checks and layer-lineage checks remain.
The changed source and environment require independent narrow review, the
two invalidated OCI controls and required current-candidate CI, followed by
fresh containment, all four acceptance families and all navigation modes.
Every arm and repeat must then use this same replacement image. No frozen
case, prompt, Gold, assertion, acceptance command, model setting, resource
limit, schedule or token ceiling changes. This environment revision precedes
all calibration, count, inference, answer and scoring calls; it provides no
comparison result or resource-fit conclusion on its own.

## Results and interpretation

**Pending.** No numerical savings, comparative reliability or dollar-cost
claim exists yet. The complete report will provide actual usage by arm,
all-run and correct-paired comparisons, uncertainty, failure counts, applied
documentation scores and the immutable run/score metadata commitments.
Calibration and scoring overhead will be reported separately from solver
usage. The ten-eligible-PR/14-day process observation remains a separate
post-landing measure.
