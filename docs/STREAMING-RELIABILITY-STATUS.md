# Streaming reliability — review, repair, and promotion status

**Status:** corrective implementation active · **Effort:** `effort/streaming-reliability` ·
**Started:** 2026-09-04 · **Effort fork:** `48615baf` · **Latest integrated Forgejo main:** `9c934391`

Companion to [PLAYBACK.md](PLAYBACK.md) (the shipped playback contract),
[PLAYBACK-CONTROL-STATUS.md](PLAYBACK-CONTROL-STATUS.md) (the control-protocol
chronicle), and [VOD-CUTOVER.md](VOD-CUTOVER.md) (the immutable-HLS cutover) —
this page answers *what the current reliability effort is doing, what has been
proved, and what remains before it may reach `main`*.

The detailed, ranked result is
[STREAMING-RELIABILITY-REVIEW.md](STREAMING-RELIABILITY-REVIEW.md). The review
and evidence tasks were merged before the Forgejo cutover; all current task
branches and reviews use `noirr/plurx` on Forgejo.

The working copy is an isolated clone under `/private/tmp`; the existing
developer checkouts are not used for implementation or validation. Task
branches merge into the effort branch through the compile-only development
gate. The effort reaches `main` only after one frozen-candidate full
qualification run, an exact-head adversarial approval, and green unit tests.

## Outcome — playback changes must preserve the player

The effort has four product outcomes:

- ordinary playback does not freeze or stutter because a producer, playlist,
  session owner, or client recovery loop disagreed about progress;
- a seek stays on the same immutable film timeline and does not recreate the
  stream;
- a quality, resolution, track, dynamic-range, or placement change uses one
  make-before-break transaction when the destination is compatible;
- an unsupported transition fails explicitly and leaves the working stream
  intact.

The review may change any component or contract needed to reach those
outcomes. It will not preserve the retained live-HLS engine as an unannounced
production fallback, hide an unfinished capability behind a compile-time gate,
or call a green unit suite physical playback evidence.

## Workstreams — the review decides the build order

| Workstream | State | Current evidence | Exit condition |
|---|---|---|---|
| Last-seven-days change review | **Complete** | 1,193 commits · 217 first-parent changes · 512 files · 220,101 insertions · 50,655 deletions since 2026-08-28; method and disposition are in the findings document | Exact-head review verifies scope and evidence |
| End-to-end architecture review | **Complete — adversarially approved** | Independent server, client, and operations passes converge on the missing prepare/commit transaction, incomplete VOD recipe coverage, and unbounded client freezes; exact-head re-review approved every correction at `b3179001` | Keep the approved contract authoritative while implementation evidence accumulates |
| Review and status PR | **Merged — [#903](https://github.com/pjunod/plurx/pull/903)** | Exact-head adversarial approval at `2fd62daf`; Effort development gate green; merged as `be088173` | Complete |
| VOD demand and deadline isolation | **Merged — [Forgejo #10](http://192.168.4.7:3000/noirr/plurx/pulls/10)** | Exact-approved `da291af5`, 103 focused Rust tests, 115 policy tests, and green Effort development gate; merged into the effort as `ebe71eb4` | Complete for this slice; not a prepared-handoff or release qualification |
| Client recovery and intent | **Merged — [Forgejo #6](http://192.168.4.7:3000/noirr/plurx/pulls/6)** | Exact-approved `263c69f0`, green development gate, and verified merge `ad79ae81`; includes current main `9c934391`. Integrated proof: 148 focused Rust tests, 121 validation tests, 123 focused Android tests, complete web static checks, and iOS/tvOS builds | Complete for this recovery/ownership slice; canonical captures and cold inputs remain separate tasks |
| Direct-file ranges and pacing | **Merged — [Forgejo #14](http://192.168.4.7:3000/noirr/plurx/pulls/14)** | Exact-approved `9239999a`, 45 focused stream tests, 121 validation tests, and green development gate; merged as `56e4f6eb`. Includes the reviewed merge-aware history-audit correction, with no gate bypass | Complete for this slice |
| Atomic client captures | **Review corrections active** | Initial Apple 67, Android 98, and complete web checks passed. Adversarial review then found delayed enqueue reordering, cross-attachment source-slot reuse, and prematurely consumed subtitle-readiness edges; corrections and new boundary regressions are underway | Production-boundary regressions cover urgent, cadence, retry, reset, and superseded capture interleavings |
| Apple cold input ownership | **Committed; PR publication next** | Adversarially approved source committed as `3155bfe9`; exact source passes 50 focused Apple tests (including 19 ownership regressions), iOS/tvOS compilation, and tracked effort hook with Rust 1.97.1 all-target check | Exact committed-head review and green development gate before effort merge; this slice does not claim a whole-pipeline deadline or prepared handoff |
| Serving-proof continuity | **Approval required — other tasks continue** | Independent design review approved retaining only an already-valid proof until its original expiry; automated safety review refused the implementation as a cluster authority-boundary change. No source changes applied | Explicit user approval for this safety-boundary change, then implementation, adversarial review, and deterministic/cluster qualification; no bypass |
| Immutable VOD transcode and burn | **Implementation active** | Real production create → GET → decode passes NTSC 0 → 90.09 → 3 seconds, VFR normalization, ±250 ms audio correction, and a PGS cue beginning before the seek. End-of-film frame overrun corrected; independent adjacent AAC decode and text-burn proof still running on explicit FFmpeg Full 9.0.1 | Production transcode/burn recipes pass demand, cancellation, timestamp, initialization, and decoded-output regressions |
| Final qualification | **Not started** | The candidate is not frozen | Current `main` merged into the effort; unit suite and full promotion gate green once on the final fixed tree; qualification receipt inspected |
| Promotion and cleanup | **Not started** | No main-bound PR exists | Effort PR merged; task and effort branches removed only after the merge is proven |

## Known facts — claims already verified against the baseline

1. **The documented VOD-only cutover is not the production default.** Omitted
   presentation means VOD and an explicit live request is refused, but the
   default Cargo feature retains growing live HLS and a missing runtime setting
   enables fallback. VOD refuses transcode and subtitle burn, so those common
   classes silently enter the old mutable engine. Tests invert the runtime
   default and therefore miss the shipped configuration.
2. **The control plane stages but does not yet complete transparent change.**
   The 2026-09-04 M6 slice can write a prepared successor from an accepted
   in-session selection change. Its own handoff records the commit trigger as
   the next slice, and the current clients still perform several changes as a
   replacement open.
3. **A passing source test is not the playback claim.** The VOD cutover records
   automated Chrome coverage but still carries named-device and two-hour 4K
   physical evidence debt. This effort will preserve that distinction.
4. **The required compiler is available.** Rust 1.97.1 is installed and is
   invoked explicitly because Homebrew Rust 1.95.0 precedes it on `PATH`.
5. **The strongest freeze gates are not running.** Pull-request CI explicitly
   omits VOD steady, seek-storm, and stall-recovery cases. All eight inspected
   nightly runs from 2026-08-28 through 2026-09-04 failed before playback
   because the installed Playwright Chromium executable was not passed to the
   playback lab.
6. **The fleet reproduces an unbounded-hold class.** Seven-day node-local
   telemetry includes repeated Apple `server_hold` outcomes at one frozen
   position for many minutes. The current Apple and web paths allow an advisory
   producer hold to defer a decoder recovery without spending a finite budget.

## Decisions made without waiting

1. **Use the existing VOD and control foundations.** Their core direction is
   sound: immutable film-time resources remove entire freeze categories, and
   prepare/commit/abort is the right shape for compatible stream changes. The
   effort will repair or replace faulty details instead of introducing a third
   playback architecture.
2. **Expose unsafe enablement in Developer settings.** A capability that needs
   prerequisites will name them beside its control. No new compile-time or
   hidden configuration feature gate will be added.
3. **Prefer explicit refusal over destructive fallback.** If a destination
   cannot be prepared, the current stream remains authoritative. Recreating a
   session or silently changing presentation is not a transparent handoff.
4. **Use focused tests during development and qualify once.** Each task PR gets
   the smallest regression that proves its behavior plus the effort gate. The
   final fixed candidate gets the unit suite and one complete main-promotion
   qualification, in that order.
5. **Use Forgejo as the write authority.** GitHub is retained as a read-only
   historical remote. Current branches, task reviews, merges, and build status
   are published through `noirr/plurx` on Forgejo; final reporting records the
   exact branch and head so no GitHub status is mistaken for current evidence.

## Update log — newest entry first

| At (America/New_York) | State change |
|---|---|
| 2026-09-04 — 20:26 EDT | Apple cold-input fix `3155bfe9` is committed after adversarial corrections and the tracked effort hook; 50 focused simulator tests and both Apple builds pass. Atomic capture review found three additional cross-queue/attachment/readiness races being corrected. VOD burn/VFR/offset production tests pass, but strict adjacent AAC waveform comparison found an eight-sample drift that remains under repair; identical init and timestamps alone were insufficient proof. Main separately advanced to `15f88e53` with PR #5 durable settlement and CI fixes; it is not yet integrated into this effort. |
| 2026-09-04 — client task merged | Client PR #6 merged as `ad79ae81` after exact-head approval and all development checks. Atomic capture envelopes and Apple cold input ownership started in separate worktrees from that base. The VOD task proved actual generation, GET, and FFmpeg decode at 0 → 90.09 → 3 seconds with unchanged initialization data; further burn/audio/VFR proof is active. Existing FFmpeg Full 9.0.1 has libass and is being used explicitly for burn tests, without installing or changing host software. Quorum proof-retention implementation is deferred for explicit approval after automated safety rejection; no authority code changed. |
| 2026-09-04 — 19:49 EDT | HTTP PR #14 merged into the effort as `56e4f6eb`; exact-approved head ancestry verified. Web correction `0a70d6fb` is exact-commit approved. Client integration now includes that effort and main `9c934391` without conflicts; the exact integrated web suite, iOS/tvOS compilation, and focused Android run are green. Rust Clippy passed; fresh Rust test compilation and focused regressions are running before the client merge commit and PR update. No full qualification has started. |
| 2026-09-04 — 19:40 EDT | HTTP [PR #14](http://192.168.4.7:3000/noirr/plurx/pulls/14) opened at exact-approved `9239999a`; its development gate is running. Independent review repeated 45 HTTP, 66 VOD, 21 history, and 7 ownership-inventory tests. The final web preparation/retained-observer patch passed `make web-check` and independent source review; its scoped commit is running. Main advanced separately to `9c934391` through layout PR #4; client integration will retain that work and repeat exact-tree checks. |
| 2026-09-04 — server task merged | VOD [PR #10](http://192.168.4.7:3000/noirr/plurx/pulls/10) passed its corrected Effort development gate and merged as `ebe71eb4`. Remote ancestry verified that the effort contains exact-approved `da291af5`; main remained `4ce33f95`. No main promotion occurred. The live next tasks are client web completion, HTTP range/pacing integration, and real immutable VOD recipe production. |
| 2026-09-04 — 19:00 EDT | Android follow-up `310a226b` committed with its tracked effort gate green. Final local source passed 123 focused JVM tests and all 115 validation-contract tests; its four-file source manifest was independently approved. Apple `856d9a0a` is also exact-commit approved. The detailed report now retains the executable-handoff implementation sequence and the remaining atomic snapshot/intent-envelope audit lead. Web preparation ownership remains active, so client #6 is still not ready to merge. |
| 2026-09-04 — 18:55 EDT | Apple continuation fix `856d9a0a` is committed and independently approved at its exact three-file tree: 43 focused simulator tests and iOS/tvOS compilation passed. Android follow-up addresses Pause revoking a create, orphaned stale responses, predecessor failures stealing a newer recipe, and a fired stall latch surviving an awaited control exchange; final focused verification is running. Web is adding an absolute decision/create/body deadline and preservation of the working predecessor on preparation failure. Client #6 remains WIP. |
| 2026-09-04 — 18:50 EDT | Server [PR #10](http://192.168.4.7:3000/noirr/plurx/pulls/10) is open into the effort at `b904b92c`, with exact-commit adversarial approval and 103 focused tests. Its [development run 56](http://192.168.4.7:3000/noirr/plurx/actions/runs/56/jobs/1) found eight ownership-inventory count changes introduced by the new regression code. The added task/timer/process lifetimes are being explicitly audited and documented before rerunning the gate; the failing gate has not been bypassed. |
| 2026-09-04 — executable handoff audit | A fresh source audit of main `4ce33f95` and concurrent PR #5 head `20951a9` confirms that durable acknowledgement settlement still creates an `encoder: staged` placeholder, not a running successor. The remaining contract, real-media producer, priming authority, client-ready/server-commit, switch, and predecessor-drain work is now itemized in the findings document. Existing ownership corrections are not a claim that transparent handoff is complete. |
| 2026-09-04 — focused ownership fixes | Android full-recipe correction committed as `c5504641`; its compile, 105 focused tests, and effort commit gate passed. Server demand correction committed as `abb872ba`; integration with main is being checked before publishing a task PR. Independent web review found partial-open recipe loss, a native-seeking watchdog latch, stale media-event counters, and unfenced diagnostic callbacks; corrections remain active. Apple lifecycle/offline/subtitle follow-ups are also active. No final qualification or main promotion has started. |
| 2026-09-04 — integration | Web correction `ca0913c1` passed the effort commit gate and complete static web checks. Current Forgejo main `4ce33f95` merged cleanly into client head `737952e8`, whose pinned Rust effort gate passed. Generic iOS/tvOS compilation passed with Xcode service access. The server reviewer independently repeated 103 focused tests (66 VOD including FFmpeg, 11 wait-pool, 26 scheduler); Clippy and formatting also passed on the reviewed server patch. That slice is being committed and integrated with main before exact-tree re-verification. New Apple review blockers cover unfenced subtitle awaits, missing fallback recipe ownership, stale stop/start demand and decisions, and offline seek routing. They remain active, alongside Android's full-recipe follow-up. Related [Forgejo #5](http://192.168.4.7:3000/noirr/plurx/pulls/5) implements durable handoff acknowledgements and Developer enablement guidance but explicitly does not start a real successor encoder; this effort will consume it after integration and complete the remaining media/client transaction. |
| 2026-09-04 — Astra follow-up | Apple commits `c50b6025` and `96065b89`, and Android commit `0248fa68`, passed their effort commit gates. Latest focused evidence is 31 Apple simulator tests and 98 Android unit tests. Subsequent review found Android audio/burn/offset composition and retry-budget ownership gaps, so further corrections are active. Web's static fast lane passed, then review found delayed HLS/direct metadata callbacks could suppress startup or overwrite a newer seek; exact-attachment fixes and regressions are being verified. Server review is closing cancellation, admission-clock, mixed-client fairness, publication, and zero-progress eviction races. Forgejo PR #6 has a fresh [progress comment](http://192.168.4.7:3000/noirr/plurx/pulls/6#issuecomment-29). No intermediate result is final approval. |
| 2026-09-04 — Astra implementation | Apple correction commit `c50b6025` passed its effort commit gate and 29 focused simulator regressions. Android's final focused compile and 98 tests passed; its effort commit is running. Cross-platform review then found additional Apple recipe-composition, pause-during-open, readiness, and target-deferral gaps; follow-up implementation separates desired recipe from the attached revision and is being compiled/tested locally. Web's new command-race tests pass; its complete static fast-lane gate is running. A second, independent Astra server reviewer is examining the VOD task. No current task has final approval or merge authority from these intermediate results. |
| 2026-09-04 — CI diagnosis | Forgejo run 26's Rust job failed before compilation because a relative action URL resolved to a missing repository; latest main fixes the URL. The web job found an obsolete three-argument `seekTo` source assertion after the corrected call added explicit intent preservation; that contract is being updated while retaining the behavioral regression. The source-only focused loop remains the build authority before pushing. |
| 2026-09-04 — Astra pass | The current published client head is **not approved**. Forgejo PR #6 is marked WIP with a progress comment. Additional review found web `/decision` responses overwriting newer commands, paused playback resuming during internal replacements, missing execution marking on copy-HLS fallback, leaked abandoned sessions, audio-offset routing loss, and an event-only `playing` recovery cancellation. These are active fixes, not closed findings. Android is also correcting quality-then-seek composition and an independent presentation deadline. Apple’s latest focused 27-test simulator run passed, Android’s intermediate focused 90-test run passed, and web’s cadence/epoch focused suite passed; later edits require fresh proof. The final full suite has not started. |
| 2026-09-04 — Astra pass | VOD correction now gives each admitted segment/init request a bounded deadline and file pin, isolates timeout failure to its entry, preserves retry deadlines, and schedules each viewer’s nearest request against accepted control demand. Pinned local compilation passed; focused regressions and lint are still running. The old PR #6 CI run has failed compile/static jobs and is not merge evidence; those failures will be inspected against the Forgejo pipeline. Latest notified main is `a013458f` after Forgejo PR #8. |
| 2026-09-04 | Exact-head client re-review found two additional Apple presentation gaps: subtitle replacements had no destination generation, and delayed healthy audio could run beyond the landing tolerance and remain pending forever. Subtitle commands now publish an exact destination on every route, in-place selection settles only after its exact mutation, reopen selection waits for destination presentation, and audio applies ±250 ms only to its first landing sample before requiring monotonic advance. Focused simulator proof is green; the follow-up commit and another exact-head pass are in progress. |
| 2026-09-04 | The second adversarial client pass rejected eleven P1 and two P2 race/evidence gaps. All are closed at `78223cef`: actual-output callbacks carry intent generations; Android relative seeks accumulate; failover gets fresh ownership without resuming a pause; Apple/web honor the absolute deferral deadline; presentation waits are bounded; replacements publish exact destinations; overlay mutation follows publication; native seeks are item-fenced; late terminal replies cannot re-arm; Original copy explicitly refuses Auto; audio requires advancing presentation; leases are monotonic; and tests map production seams. Focused Android, Apple, web, and Rust proof plus the effort gate are green. Exact-head re-review is next. |
| 2026-09-04 | Committed the client corrections as `3b5387e5` and their exact regression mapping as `fb3424ac`. Both effort-mode commit gates passed. The first attempt was correctly refused because a historical Android seek regression still named a production helper the new coalescer had bypassed; the seek now routes through one production-linked viewer-seek invalidation seam, its focused regression passes, and the history audit is green. Exact-head adversarial re-review is next. |
| 2026-09-04 | Moved current work to Forgejo as the sole write authority. The first exact-head adversarial client review rejected nine gaps: Android could still freeze before establishment; deadlines counted control latency incorrectly; quality state went stale; Apple did not publish the exact seek target before mutation; request sequence could be lost; seek completion was not presentation; stale actions crossed awaits; terminal verdict lifetime was too broad; and Original collapsed into Auto. All nine are corrected locally. Android units, the web control contract, Rust 1.97.1 all-target compilation and focused units, 359 Apple simulator tests, and generic iOS/tvOS compilation are green. The first Apple test run exposed one stale fixture that asserted an implicit seek target; the fixture now supplies the explicit target and the entire rerun passes. |
| 2026-09-04 | Opened historical GitHub client-recovery PR #911 against the effort before the Forgejo cutover. It is no longer an authority or merge path. The reviewed branch is now published only to Forgejo; its Forgejo task PR must name the new exact head before merge. |
| 2026-09-04 | Client recovery is implemented locally without a feature flag. Android now detects a still-open established buffering freeze at 8 seconds, gives control at most one deferral inside a 20-second absolute deadline, and turns exhausted recovery into a visible failure. Apple and web use the same finite-authority rule. Owner changes settle old asks immediately; web terminal diagnoses now survive same-title replacement only for their lease; Android carries stable playback identity, exact quality, and explicit seek intent across controller replacement. Focused proof: web control passed, Android JVM units passed, and Apple ran 274 selected tests with zero failures. |
| 2026-09-04 | Playback evidence PR #908 passed exact-head adversarial review at `73d9bab7`, passed the Effort development gate, and merged into the effort as `7db47b35`. PR #905 was superseded and auto-closed when its old head branch was removed. |
| 2026-09-04 | PR #905 third-pass review also found that one 200 ms Auto hitch could pass the unimplemented p95 target, and that Auto never checked its first target frame against the film-position boundary. The singleton nightly transition now must stay within 100 ms, the 250 ms absolute ceiling remains explicit, and target-tagged frame evidence rejects both forward and backward jumps over 250 ms. |
| 2026-09-04 | PR #905 third-pass review found API presence could be mistaken for an operative frame oracle when Chromium never invoked its callback. Auto now requires a finite post-cliff callback before suppressing sampled-clock evidence; zero-callback and stopped-callback regressions both fail closed. |
| 2026-09-04 | PR #905 second-pass adversarial review rejected four P1 evidence races and one P2 seam despite a green gate: outgoing frames could satisfy target readiness, hold-window gaps could be reset, Auto moves between polls could collapse, non-finite Auto runway could pass, and final pre-replacement faults could escape sampled rollup. The local correction tags frames, closes each transition after its hold, uses a monotonic timestamped switch ledger, fails runway closed, and increments lifetime faults at their source. |
| 2026-09-04 | Addressed every first-pass PR #905 finding: the Auto cliff is a strict nightly point; rendition completion needs the expected method, exact height, and a new frame; the boundary snapshot and page-lifetime frame/counter probes cover seams and silent freezes; 60-second post-downshift stability is mandatory; identity, runway, position, per-transition p95/max gaps, and partial-browser scope are explicit. |
| 2026-09-04 | Opened corrective PR #905. Its first adversarial pass rejected unscheduled Auto evidence, stale-rendition acceptance, silent-freeze and first-sample seams, bad hold-time math, incomplete identity/dwell checks, and overclaimed browser SLO coverage. All findings are being implemented; audio remains explicitly device evidence. |
| 2026-09-04 | PR #903 merged into the effort as `be088173`. Started the first corrective task in the isolated clone: repair the playback evidence path before changing the behavior it judges. |
| 2026-09-04 | PR #903 exact-head adversarial re-review approved the architecture and findings at `b3179001` with no remaining merge-blocking factual or design errors. The required Effort development gate passed; this status-only accuracy update is the final pre-merge change. |
| 2026-09-04 | PR #903 re-review found three remaining acceptance/evidence holes: supported transitions could all refuse, Auto could still visibly stutter, and retained fleet/receipt claims lacked executable detail. The contract now requires 20/20 commits for qualified tuples, applies continuity SLOs to Auto, and retains the commands and named receipts. |
| 2026-09-04 | PR #903 exact-head adversarial review found eight documentation/architecture defects: adaptation authority, seek intent, owner-loss acceptance, gated-feature inventory, numerical SLOs, retained evidence, completion scope, and inconsistent status. All eight were corrected for re-review. |
| 2026-09-04 | Opened draft review PR [#903](https://github.com/pjunod/plurx/pull/903); completed independent client, server, and operations passes; recorded four P0 blockers and the target immutable-rendition/transaction architecture. |
| 2026-09-04 | Created isolated clone and `effort/streaming-reliability`; confirmed authenticated GitHub access and the pinned Rust 1.97.1 toolchain; began independent server, client, and operations/architecture reviews. |
