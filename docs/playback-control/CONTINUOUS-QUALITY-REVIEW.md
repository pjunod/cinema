# Continuous quality review — adversarial findings and dispositions

**Status:** reviewed; approved to start isolated CQ0 · **Written:** 2026-09-30

Companion to the [build contract](CONTINUOUS-QUALITY-BUILD.md). Independent
review was performed by the `adversarial_quality_review` agent against the
initial draft and source anchor `1b2ae4f62`. Its first verdict was **revise
first**. This record separates that review from changes made in response;
no implementation or device acceptance is implied.

## 1. Initial review — three contract blockers and two corrections

| ID | Priority | Finding and failure scenario | Disposition in the revised build |
|---|---|---|---|
| R1 | P1 | Cancellation cannot undo appended media. The draft allowed scheduled → retained/superseded while forbidding buffer removal. An appended 1080p interval could still appear after a 30-second timeout claimed 720p was retained. | Addressed in §4.1–§4.3, §5.3 and §6.2. Added irreversible `appended` state, interval provenance, preserved pins, separate latest intent, long-buffer scheduling versus observation deadlines, and bounded three-rung producer behavior. Post-append timeout is unknown observation, never false retention. |
| R2 | P1 | CQ1 required negotiated cancel-before-offer but CQ2 introduced negotiation later. Cancellation was also tied to continuous-family support, although ordinary prepared replacements need it. | Addressed in §6.1 and §9. CQ2a precedes CQ1, cancellation is independently negotiated, all strict bootstrap/request/response/relay readers are covered, and epoch changes invalidate support. After the concurrent-work audit, CQ2a must reuse the upstream negotiation floor rather than invent a duplicate. |
| R3 | P1 | A stable autonomous master could reselect a rendition after the five-second rule retired its producer. A cold request/refusal cannot reliably become an application-level retain-current after native selection. | Addressed in §5.4. Controlled and autonomous lifetimes differ. Autonomous two-rung mode reserves delivery capacity for both advertised rungs through attachment, seeks and reselection. If not admitted, choose the legacy presentation before attaching. Test pressure after the first switch. |
| R4 | P2 | The draft overstated legacy cleanup: the durable 330-second deadline begins during staging and does not bound all detached planning. | Addressed in §4.3. No universal legacy deadline is claimed; late planning after restored selection is a required case. Immediate cleanup belongs to negotiated cancellation. |
| R5 | P2 | Proposed response states did not explain scheduled/presented/recovery settlement, retry receipts or local-only observation states. | Addressed in §6.1–§6.2. The authority/replay table defines every state, owner reservation precedes append, committed intervals survive acknowledgement loss, and terminal replay cannot allocate again. |

The reviewer explicitly accepted feasibility-led execution: native API,
loader-frontier, decoder overlap and real audio/display behavior belong to
CQ0 experiments. The plan need not claim those experiments passed before the
builder begins them. It must preserve their result as a dependency on later
production implementation.

## 2. Concurrent-work correction — user-directed scope separation

After the initial review, Paul required non-overlap with **Check Cinema
transcoding resolutions**, session
`01a0f484-4b71-76d2-acfe-0b0975249d9c`. A read-only audit of that session and
its independent clone found direct overlap, including active uncommitted
changes to web prepared handoff, native controllers, server preparation,
manifest metadata, candidate routing and resource admission.

**R6 / P1, self-review:** a different branch would not prevent duplicate
implementation. Build §1.2 now assigns existing Auto/display/candidate and
voluntary-handoff fixes to that session. This effort consumes their landing
and adds only missing manual behavior and continuous-media semantics. CQ0
can start in newly owned investigative files; shared production changes wait
for upstream landing, ownership release and a reviewed integration delta.
The builder must not edit either the user's checkout or the other session's
clone. No coordination message or transfer of ownership was inferred.

## 3. Delta review and document checks

The independent reviewer's second verdict was: **Approved to start CQ0 now,
within §1.2's investigative file allowance. No remaining blocker to that
work. This is not approval of media acceptance or shared production edits.**
R1–R5 were accepted as addressed; R6 was accepted as preventing overlap after
rechecking the upstream clone's extensive uncommitted work.

Two final clarifications were requested and applied:

- §9.2 now explicitly defers edits to the existing playback lab/cases until
  after §1.2's dependency boundary; immediate CQ0 owns only its new probe.
- §6.1 now requires the append-completion versus cancellation fixture with a
  lost `appended` acknowledgement. Cancel carries current committed facts,
  and the owner retains reserved dependencies when append absence is unknown.

Documentation checks: all four `tests.operations.test_docs_index` checks
passed with the new documents staged; all local links in the build document
resolved; `git diff --cached --check` passed. The normal tracked commit hook
is run with explicit Rust 1.97.1. These are documentation/source checks,
not device acceptance. Hardware evidence remains empty.

The builder starts CQ0 in the independent clone identified in the build.
Before shared production edits it must record upstream landing/release,
reconcile the implementation delta, and obtain the required adversarial
integration review. Existing-session scope is not reassigned by this review.

## 4. CQ0 implementation delta — investigative approval only

The builder's `cq0_adversarial_review` agent reviewed the new isolated lab,
its six final Chrome receipts and the focused checks on 2026-09-30. Verdict:
**approve the isolated investigative prototype; revise the evidence record
before claiming CQ0 complete.** All nine receipt checks passed. No shared
production work or media/native acceptance was approved.

The build's §10.1 now records every qualification requested by the review:

| Finding | Recorded disposition |
|---|---|
| SourceBuffer range growth cannot exclude overlapping replacement samples | The measured frontier and range extension are retained; exact sample bounds, general overwrite exclusion and complete interval provenance remain unproved. |
| Separately decoding aligned segments is not alternating-resolution join verification | Encoded joins, burned frame identities and midpoint joins remain unqualified. |
| Git HEAD did not include uncommitted prototype source | Source content hashes and the staged candidate tree are retained; the normal commit is blocked by missing catalog registration. HEAD in the receipt is explicitly a planning anchor. |
| Final cases exercise `loadLevel`, not the failed `nextLoadLevel` trial | Prior receipts remain historical diagnostics without complete source identity; the limitation is also supported by the read-only vendored setter behavior. |
| Safari WebDriver failed before playback | Native selection, autonomous pressure/reselection and real Safari media remain not measured. |

The same record explicitly keeps audible/browser-output continuity and
external display capture not measured. CQ0 is a runnable partial milestone;
its missing physical/API and media evidence is not converted into a pass.
The upstream ownership/integration delta review required by §1.2 is still
outstanding and cannot be replaced by this isolated prototype review.

## 5. Fable source review — corrections awaiting qualification

**Updated:** 2026-10-15. Fable reviewed `ca00c3ed8` against
`6e01ef879`, including the Apple 245 source package. The corrections are recorded in `f94f73211` (Apple 248), following
the `e2315d3a4` Apple 247 checkpoint. Source review does not qualify a different hardware build.

| Finding | Current disposition | Remaining evidence |
|---|---|---|
| F1, major: a busy cached-reader or accepted-wait lock cancels a sealed recovery under foreground pressure | Authored tri-state probes. Unknown preserves only an existing unexpired witness. Definitive negative checks cancel; fresh authority requires definite success. | Pinned Rust 1.97.1 all-targets compilation passed. Three actual-lock watcher/control regressions are authored, unrun. |
| F2, minor: physical incumbent protection bridges telemetry's positive-gap tolerance | Lease acquisition explicitly requests zero gap tolerance; ordinary runway telemetry retains its existing tolerance. | Real-controller gap, contiguous admission and restoration regression authored; Apple compilation and execution pending. |
| README build mismatch | Already corrected by Apple 247 before this review arrived. | Current README and project both report 247. |
| N6, request-bound coverage | Added an injected-fetch regression with valid many-fragment media; it requires refusal after exactly 64 requests and includes a valid small control. | Authored, unrun; no request-budget change. |
| N4, shared-controller parity | Recorded as a separate parity gap outside this correction batch. | No shared-controller acceptance claimed. |

The new regression anchors are:

- `crates/plurxd/src/http/hls/preparation.rs::cached_accepted_wait_survives_contended_session_lock_during_foreground_pressure`
- `crates/plurxd/src/http/hls/preparation.rs::cached_accepted_wait_survives_contended_reader_lock_during_foreground_pressure`
- `crates/plurxd/src/http/hls/preparation.rs::cached_accepted_wait_contention_keeps_control_witness_without_renewal`
- `clients/apple/Tests/PlayerResumeTests.swift::testIncumbentFetchPreferenceLeaseRejectsPositiveGapInsideProtectedWindow`
- `clients/apple/Tests/HLSFragmentCadenceTests.swift::testFragmentIndexWalkFailsClosedPastSixtyFourRequestsOnManyFragments`

### Source context requested by Fable

`http/hls/control.rs` returns HTTP 429 with `control_rate_limited` when
`media_sessions.admit_control` rejects the ingress control budget. This
source path explains a possible response, not the cause of a particular
metadata-ready receipt: that still requires its exact response and request
chronology. It does not establish a media HTTP 503 cause or authorize a
client retry.

`PlayerController` converts native transfer completion dates into a
`PlaybackControlSession.monotonicMs()` timestamp when observing the metrics.
`AutoCompletedTransfer.ageMs` subsequently subtracts that timestamp from the
same uptime clock, rejecting a backwards value with `Int.max`. The initial
age conversion uses wall-clock dates; it is not an independent monotonic
native completion timestamp. Provide these source contexts in the next
review package rather than claiming them present in the original package.

Catalog counts describe distinct recorded `commits` values, not distinct
regression ID prefixes. At the committed Apple 247 checkpoint there were
712 rows, 712 unique IDs and 588 distinct recorded `commits` values.

### Hardware and owner-policy hold

Phone 247's clock diagnostic was stopped before READY and before native
launch after a new owner instruction prohibited app builds and installs
from unmerged source, including diagnostics. All 159 recorded owned
process identities were absent at independent closure. Three cost proofs
completed; no native frames or quality changes were captured. This is a
policy-stopped attempt, not media acceptance or a reproduced playback
failure. Earlier failed captures remain preserved.

The owner subsequently clarified that the main-only build/install instruction
applies to mobile apps only. iPhone and Android builds/installations stay held;
Apple TV qualification resumes on the corrected source. No merge is authorized.
Unit bodies remain unexecuted under the explicit deferred-test instruction.


## 6. Fable follow-up — Apple 248 accepted with one admission risk

**Updated:** 2026-10-15. Fable verified the Apple 248 source and final hardware
packages, accepting F1/F2/N6 with no blocker. Desk execution of regression
bodies is not test execution. The review identifies the following corrections:

| Finding | Disposition | Evidence still required |
|---|---|---|
| S1: stale comment says unknown cancels | Clarify that unknown preserves only an existing unexpired witness and cannot mint/renew it. | Compile and source inspection. |
| S2: strict gap tolerance also rejects membership rounding | Apple 249 splits membership and gap tolerances; the lease retains 0.25-second membership while requiring zero inter-range gap. Clock-startup remains strict for both. | Extended actual-controller regression, compilation and changed-source TV campaign. |
| S3: duplicate tombstone check | Remove the redundant inner condition; retain the pre-lock rejection. | Compile. |
| S4: ledger overstates revocation observable behind a busy sessions table | §10.326 now names lock-free revocations and the accepted/preparation bound while sessions remain busy. | Sessions-held invalidation regression where authored; unit execution stays deferred. |
| S5: missing contexts | Include `playback_control.rs` (`AcceptedControlFence::still_live`) and Apple `PreparedReplacement.swift` (`preparedAcquisitionURL`) in the next package. | Rehash exact committed inputs. |
| E1: previous TV pass does not prove corrected paths executed | Add attributable watcher-unknown and lease acquire/refuse diagnostics without altering authority or acceptance. | Preserved TV248 logs subsequently yield 22 acquisitions and 22 successful restorations; no refusal count exists in old source. Count future observations separately; do not force contention on shared hardware. |

E2–E8 are accepted limits: no lock-contention hardware cause is established;
SDK exit 1 remains a false wrapper, with completed measurements adjudicated
separately. App absence at a census is not clean-shutdown proof. TV measurements
cover public source under an inverse-verified observer patch, not the shipping
binary. Phone 247 does not qualify changed mobile source. Recorded settled
known-session HLS GET refusal absence is not a blanket route/lifetime claim.
Optical/audio/menu continuity remain unmeasured. No unit bodies or CI lanes have
run on correction commits; final batched qualification remains mandatory before
an authorized merge. The owner still prohibits merging this work now.

The mobile policy is physical-only: new mobile builds intended for installation
and installs require committed main source; Simulator/emulator/CI/test-only
mobile builds are exempt and cannot later be reused physically. TV work is
exempt. The earlier broad policy-stop remains historical, not the current rule.


Apple 249 extends the existing actual-controller lease regression and adds
`crates/plurxd/src/http/hls/preparation.rs::cached_accepted_wait_defers_playing_revocation_until_session_lock_releases`.
The latter holds the real sessions mutex while accepting Rendering, proves
bounded witness survival without renewal, then requires cancellation after
release. No unit body has run; compilation is reported separately.


### S2 follow-up — current coverage precedes rounding fallback

Source inspection after the split identifies an actual new corner: a nearby
range ending behind the playhead shadows a later range containing it. Apple
250 prefers exact membership, then preserves the existing tolerant fallback.
The same actual-controller regression covers current coverage, the stale
preceding range, a positive gap ahead, admission and restoration. No body ran.
The review context and bounded evidence are recorded in build §10.328.

Superseded 249 r4 stopped before READY and SDK; it is not a media failure.
Changed-source compilation and qualification remain pending. Final Fable
follow-up must assess the Apple 250 source, rather than treating the compiled
Apple 249 packet as the final candidate. No merge is authorized.


## 7. Fable Apple250 re-review — follow-ups for direct promotion

**Updated:** 2026-10-15. The re-review clears S1–S5 and E1 with no blocker in
the correction delta and accepts TV250 evidence within its stated decoded-output
scope. Apple251 closes R1 by preferring the greatest-end tolerant fallback and
P1 by allowing automatic zero buffering to take a protected lease, restoring
zero without overwriting later ownership. Both use actual-controller regressions.
S4 is corrected: owner epoch, desired intent and attachment revoke through the
atomic accepted fence without waiting for sessions; Playing snapshots may wait.
R2 extra HTTP-path contention telemetry is optional and is not added: the bounded
watcher-tail count cannot establish all contention-path execution or absence.
F1 remains covered by authored real-lock fixtures, now due for final unit execution.

The owner authorized direct merge after each required unit passes once, with
only failures rerun. No batched merge session exists. This supersedes the earlier
merge hold and deferred-unit checkpoint; it does not restart full review or the
hardware campaign. TV250 does not qualify changed251 hardware; new physical
mobile builds and installs await main. Canonical ledger: §10.329.
