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
