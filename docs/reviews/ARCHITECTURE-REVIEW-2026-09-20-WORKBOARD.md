# Architecture review work board — one status for every plan, whoever is executing it

**Status:** live · **Tracks:** the implementation plans for
[ARCHITECTURE-REVIEW-2026-09-20.md](ARCHITECTURE-REVIEW-2026-09-20.md)
(revision 3) · **Written:** 2026-09-20

The plans below will be executed by sessions on different vendors — Claude,
GPT (Astra), and models reached through OpenRouter — at the same time. This
file is the **only** shared status. Nothing else (chat, a vendor's memory, a
PR title alone) counts as having claimed or finished work. It lives in the
repository so every vendor reads and writes it the same way: through git and
the Forgejo API, with no vendor-specific tooling.

## How to claim, work and finish a plan

Every executing session follows this exactly. The reason for each rule is the
failure it prevents.

1. **Read before claiming.** Read the plan, its rows in
   [ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md](ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md),
   and this board. A plan whose row says anything but `unclaimed` is someone
   else's; do not start it, do not "help" on it. Two sessions on one plan
   produce two branches that cannot both merge.
2. **Claim = a draft PR that changes this file.** Branch from current `main`
   as `plan/<board-id>` (for example `plan/S-01`), edit your row here to
   `claimed`, fill in **Model**, **Session** and **Branch**, commit, push, and
   open a **draft** PR titled `WIP: <board-id> <plan title>` whose body starts
   with the same three lines. The push is the atomic claim: if it is rejected
   as non-fast-forward, someone claimed something since you read the board —
   fetch, re-read your row, and only proceed if it is still `unclaimed`.
   The draft PR is the claim's public record; a board edit with no PR is not
   a claim and may be reverted.
3. **Identity is mandatory.** **Model** is the exact model identifier your
   runtime reports (`claude-fable-5-1`, `gpt-5`, `anthropic/claude-opus-4`
   via OpenRouter, …), not a product name. **Session** is your session's own
   id or URL (Claude: the `https://claude.ai/code/session_…` URL; GPT: the
   conversation id; OpenRouter: the generation/thread id your client
   exposes). If your runtime gives you no id, write `none:<vendor>:<date>`
   and say so in the PR body. The same two values go in **every commit** on
   the branch as trailers:

   ```text
   Agent-Model: <model identifier>
   Agent-Session: <session id or URL>
   ```

   (Claude sessions keep their existing `Co-Authored-By` and
   `Claude-Session` trailers as well.) This is what lets a later reader tell
   which model wrote which change without guessing from prose style.
4. **One implementation PR owns the whole plan.** Work only inside the
   plan's milestones, but keep every milestone in the draft PR opened by
   rule 2. Milestones are logical commits and rows in the plan's
   **Execution log**, not separate PRs: record date, model, session,
   milestone, commit and outcome, and update this board's **Status** and
   **Last update** cells in that same plan PR. Evidence that can exist only
   after merge (deployment, fleet or device results) is appended later to
   the same execution log and board row through one evidence-only docs PR;
   it never creates milestone PRs or a second status ledger. A plan that
   tells you to stop and flag something means stop and flag it — in the PR
   body and in the board's **Notes** cell — not "make a reasonable choice".
5. **Statuses** (the only words allowed in the Status column):
   `unclaimed` · `claimed` (branch exists, no code yet) · `in-progress`
   (the plan PR contains implementation commits) · `blocked: <reason>`
   (waiting on a fleet measurement, a device test, or a decision that is
   Paul's — say which) ·
   `in-review` (adversarial review requested; name the reviewer session) ·
   `merged: <landed milestones>` (the one implementation PR landed, but
   post-merge evidence is still owed; name the logical milestones without
   implying separate PRs) · `done` (every milestone landed and its acceptance
   evidence is recorded in the plan) · `abandoned: <reason>` (release the row
   back to `unclaimed` in the same edit, keeping the reason in Notes).
6. **Release what you cannot finish.** If your session is ending with a
   milestone unfinished, set `blocked:` or `abandoned:` with a one-line
   handover in Notes (branch, what is done, what is next, what is broken)
   in the same push. A silently stale `in-progress` row costs the next
   session a day of archaeology. Rows untouched for **7 days** in
   `claimed` or `in-progress` may be reclaimed by anyone after they add a
   note saying so.
7. **Never edit another session's row** except to reclaim a stale one under
   rule 6, or to record objective post-merge evidence and mark it `done`
   after its implementation PR lands. Paul's standing rule is that the
   executing session merges its own implementation PR; if you are recording
   evidence or merging for someone else, say why in Notes.
8. **The PR lifecycle is the repository's**: draft until ready · adversarial
   review · findings folded · fast lane green (`make unit` locally for Rust
   changes; the CI fast lane also runs it on ready Rust PRs — see
   `docs/ci/RUST-TEST-EXECUTION-POLICY.md`) · merge it yourself · delete the
   branch. Un-drafting on Forgejo is a title edit that does not re-trigger
   CI; close and reopen the PR to start the lane.
9. **Fleet and device evidence** that a session cannot produce itself goes
   in the plan's Execution log as `needs: <what>` with the plan's GPT prompt;
   before merge the row's Status becomes `blocked: fleet evidence`. If the
   implementation is safe to merge without that observation, merge the one
   plan PR with Status `merged`, then record the result through the
   evidence-only docs PR from rule 4. Do not mark a plan `done` from a device
   observation nobody has made.

A session that cannot follow rule 2 (no push access) has no business
claiming; it can still review.

## The board

Board ids are stable handles for claims and PR titles; the review ids in the
third column are what the plan executes. **Priority** is from the review's
§5: `week` · `month` · `quarter` · `design` (a design document whose output
is a decision or a follow-on plan, not code). Every row now has a
document (the twelve written second landed on 2026-09-20 as well).

| Id | Plan | Executes | Priority | Status | Model | Session | Branch / PR | Last update | Notes |
|---|---|---|---|---|---|---|---|---|---|
| S-01 | [PROCESS-OUTPUT-CAPTURE-AND-SCAN-PROBE-BOUNDS](../streaming/PROCESS-OUTPUT-CAPTURE-AND-SCAN-PROBE-BOUNDS.md) | §2.1, C12 | week | merged: M1-M3 | gpt-5.6-sol | agent:/root/s01_builder | [PR #396](http://192.168.4.7:3000/noirr/plurx/pulls/396) | 2026-09-21 | M1-M3 merged as `f0af512d` after the sole adversarial finding and fast lane were green. M4 still needs the merged image deployed and §5.4 fleet evidence. |
| S-02 | [MEDIA-BODY-BUFFERS](../streaming/MEDIA-BODY-BUFFERS.md) | §2.4, C1 | week | unclaimed | | | | 2026-09-20 | M1 only this week; M2 (ack batching) after measurement |
| S-03 | [ENCODED-VOD-HOLD-AND-RELEASE](../streaming/ENCODED-VOD-HOLD-AND-RELEASE.md) | §2.6 | week | merged: M1-M3 | gpt-5.6-sol | agent:/root/s01_builder | [PR #412](http://192.168.4.7:3000/noirr/plurx/pulls/412) | 2026-09-21 | M1-M3 merged from PR #412: the 90-second ahead-hold low-water latch, the pure contention table with its explicit `YieldToWaiter` termination, and the `vodserve` wiring for the encoded SIGSTOP hold, VOD registration wake, one-second stopped-encoder poll, success-only latch transitions and closed-label generation/termination metrics. Sole adversarial finding #3230 is disposed by real-`spawn_driver` regressions covering the no-kick poll yield, the low-water re-admission fence, and the session-TTL reap with full-permit reacquisition; that evidence is registered in `validation/regressions.d/39472d6e-encoded-vod-driver-lifecycle.toml`. The plan is **not done**. M0 controlled before-counts and M4 post-deploy fleet traces are **post-merge** evidence and are recorded later through the rule-4 evidence-only docs PR; no count was fabricated from an empty log window. |
| S-04 | [FONT-ATTESTATION-AND-BLOCKING-IO](../streaming/FONT-ATTESTATION-AND-BLOCKING-IO.md) | §2.7 | week (M1) / month (M2) | unclaimed | | | | 2026-09-20 | Needs `ldd \| grep fontconfig` on media1 before M2 |
| S-05 | [FFMPEG-SPAWN-UNIFICATION](../streaming/FFMPEG-SPAWN-UNIFICATION.md) | F-stream-12/13, §4.1 | month | unclaimed | | | | 2026-09-20 | After S-01 |
| S-06 | [ENCODER-RATE-CONTROL-DEFAULTS](../streaming/ENCODER-RATE-CONTROL-DEFAULTS.md) | Q1 | month | unclaimed | | | | 2026-09-20 | Per-family evidence; no universal maxrate cap |
| S-07 | [TONE-MAP-CHAIN-CORRECTIONS](../streaming/TONE-MAP-CHAIN-CORRECTIONS.md) | Q3 | month | unclaimed | | | | 2026-09-20 | M0 is the hwdownload metadata test |
| S-08 | [INTERLACE-IN-THE-MEDIA-CONTRACT](../streaming/INTERLACE-IN-THE-MEDIA-CONTRACT.md) | Q4, Q9 (bitrate half) | month | merged: M1-M4 | gpt-5.6-sol | agent:/root/s01_builder | [PR #417](http://192.168.4.7:3000/noirr/plurx/pulls/417) | 2026-09-21 | M1-M4 merged; the sole adversarial review's two P1 findings in [comment 3298](http://192.168.4.7:3000/noirr/plurx/pulls/417#issuecomment-3298) are answered in `9789ca4ed` and mapped to `rust-gate` evidence in `validation/regressions.d/9789ca4e-final-interlace-routes.toml`. The plan is **not done**: M5 still needs the plan's media1 QSV/VAAPI idet, signalstats and wall-time qualification, and hardware graphs stay declined. That outstanding qualification evidence is **post-merge** and gets recorded later through the rule-4 evidence-only docs PR, not on this branch. |
| S-09 | [AUDIO-RESOLVED-INDEPENDENTLY](../streaming/AUDIO-RESOLVED-INDEPENDENTLY.md) | Q5 | month | merged: M1, M2 partial | gpt-5.6-sol | agent:/root/p01_builder | [PR #418](http://192.168.4.7:3000/noirr/plurx/pulls/418) | 2026-09-21 | M1 landed whole: bounded fail-closed `audio_sinks` and flat `achannels` claims, one v2/legacy profile translation, a route-aware `resolve_audio` that keeps decoder, sink, passthrough, and sample-rate authority separate, and independently serialized `delivered_audio` re-resolved once the playback method is known. M2 is partial: every lossy rolling, copy-conversion, and progressive remux AAC path pins `-ar 48000` and recipe v4 records `arate`, while copied audio stays untouched. Full M2 still needs normalized source channel layout and M4's measured per-layout matrices before downmix, manifests, offline packages, or prepared handoffs can enable surround output, and M3-M5 need Apple TV/AVR/AirPods, Android, and browser device evidence. All of that remaining evidence is post-merge and is recorded later through the rule-4 evidence-only docs PR, so this plan is not done. |
| S-10 | [HONEST-MASTER-PLAYLIST](../streaming/HONEST-MASTER-PLAYLIST.md) | Q7, A11 | month | merged: M1-M2 | gpt-5.6-sol | agent:/root/p01_builder | [PR #419](http://192.168.4.7:3000/noirr/plurx/pulls/419) | 2026-09-21 | M1 rolling-rung geometry and M2 init-derived fMP4 AVC identity landed, together with the sole adversarial review's two corrections: encoded-VOD geometry now resolves width and height from one `output_size` decision, and AVC identity is read structurally from the selected track's sample descriptions rather than the first `avcC` byte match. This plan is **not done**: M3 per-family 23.976/29.97/59.94/60 SPS evidence, M4 named-device SDR `CODECS` re-qualification, M5 transcode plus copy/remux/index peak evidence and M6 Apple-panel before/after evidence are all **post-merge evidence** that can only be gathered on the fleet and on physical devices, and each is recorded later in this row and the plan's execution log through the rule-4 evidence-only docs PR. |
| S-11 | [CODEC-AND-GPU-QUALIFICATION](../streaming/CODEC-AND-GPU-QUALIFICATION.md) | Q12, Q6, Q8 | quarter | unclaimed | | | | 2026-09-20 | M0 encoder inventory on /metrics decides whether NVENC/VideoToolbox milestones exist |
| S-12 | [VOD-BFRAMES-TIMELINE-DESIGN](../streaming/VOD-BFRAMES-TIMELINE-DESIGN.md) | Q2 | design | merged: D0-D3 | gpt-5.6-sol | agent:/root/p01_builder | [PR #423](http://192.168.4.7:3000/noirr/plurx/pulls/423) | 2026-09-21 | D0-D3 plus the sole adversarial finding merged from the one plan PR after a green fast lane. Option C stays deployed; the v2 oracle binds video-track timescale to the plan, refuses edit lists, and separates runtime-resolved facts from raw B0 box evidence. No Rust, recipe, setting, feature gate or enablement changed. **Not done:** B0-B4 are post-merge evidence - real ffmpeg 6/8 box capture, per-family quality/init/restart measurement, Paul's Option decision and client playback evidence - recorded later through the rule 4 evidence-only docs PR. |
| S-13 | [DECODE-FACTS-GATE-AND-FALLBACK](../streaming/DECODE-FACTS-GATE-AND-FALLBACK.md) | C13 | month | unclaimed | | | | 2026-09-20 | Measure before optimising |
| S-14 | [TRANSCODE-DECOMPOSITION-PLAN](../streaming/TRANSCODE-DECOMPOSITION-PLAN.md) | §4.1, §4.2, §4.9 | quarter | unclaimed | | | | 2026-09-20 | Behaviour-preserving moves first; registry unification separate |
| K-01 | [CLUSTER-BACKUP-AND-RESTORE](../cluster/CLUSTER-BACKUP-AND-RESTORE.md) | §2.3, S4 | month | unclaimed | | | | 2026-09-20 | The procedure is the deliverable |
| K-02 | [RAFT-SNAPSHOT-CADENCE-AND-CONSISTENT-CUT](../cluster/RAFT-SNAPSHOT-CADENCE-AND-CONSISTENT-CUT.md) | S2, S5 | month | unclaimed | | | | 2026-09-20 | M0 measurement on each voter |
| K-03 | [REPLICATED-WRITE-RATE-HYGIENE](../cluster/REPLICATED-WRITE-RATE-HYGIENE.md) | S3 | week | unclaimed | | | | 2026-09-20 | Keep the atomic claim |
| K-04 | [BOUNDED-REPLICA-READS-ROLLOUT](../cluster/BOUNDED-REPLICA-READS-ROLLOUT.md) | S1 | month | unclaimed | | | | 2026-09-20 | Consistency-policy change; no auth cache |
| K-05 | [SQLITE-READ-PATH-AND-QUERY-PLANS](../cluster/SQLITE-READ-PATH-AND-QUERY-PLANS.md) | S6, S7, S11 | month | unclaimed | | | | 2026-09-20 | Search predicate is the corrected one |
| K-06 | [CLOCK-SKEW-GUARD-DESIGN](../cluster/CLOCK-SKEW-GUARD-DESIGN.md) | S9 | design | merged: M0-M4 | gpt-5.6-sol | agent:/root/p01_builder | [PR #430](http://192.168.4.7:3000/noirr/plurx/pulls/430) | 2026-09-21 | M0-M4 and the sole-review disposition merged as `02c7760e`: wall/monotonic generation rechecks close local/common-mode step races; admission is distinct from fenced target removal; RTT filtering has a floor and time expiry. **Not done:** runtime and fleet skew measurement is post-merge evidence, recorded later through the rule 4 evidence-only docs PR. |
| K-07 | [STORE-CONTRACT-COVERAGE-AND-PLACEHOLDER-VALIDATION](../cluster/STORE-CONTRACT-COVERAGE-AND-PLACEHOLDER-VALIDATION.md) | S10 | week | merged: M0-M4 | gpt-5.6-sol | agent:/root/c02_builder | [PR #411](http://192.168.4.7:3000/noirr/plurx/pulls/411) | 2026-09-21 | M0-M4 merged as `58e75260` after the sole adversarial review and green fast lane. The semantic 42-site discard guard and actual SQLite/three-voter no-holder contract are live; first-ten-post-merge lane-cost evidence remains pending. |
| K-08 | [HIQLITE-FORK-AND-DEPENDENCY-CLEANUP](../cluster/HIQLITE-FORK-AND-DEPENDENCY-CLEANUP.md) | §4.3 | month | unclaimed | | | | 2026-09-20 | Do not rename (patch-by-name); three edges reach aws-lc, cryptr fix removes one |
| C-01 | [HTTP-LISTENER-TIMEOUTS-AND-ASSET-DELIVERY](../server/HTTP-LISTENER-TIMEOUTS-AND-ASSET-DELIVERY.md) | §2.5, W1, W2, W5, W6 | week | merged: M1-M3 | gpt-5.6-sol | agent:/root/c01_builder | [PR #395](http://192.168.4.7:3000/noirr/plurx/pulls/395) | 2026-09-20 | One plan PR merged as `79113254`; §6.2–§6.4 lab soak, HAR/waterfall, and device-reader evidence remains pending, so this row is not done. |
| C-02 | [SCAN-AND-ENRICHMENT-HYGIENE](../server/SCAN-AND-ENRICHMENT-HYGIENE.md) | C3, C5 | week | merged: M1-M3 | gpt-5.6-sol | agent:/root/c02_builder | [PR #400](http://192.168.4.7:3000/noirr/plurx/pulls/400) | 2026-09-21 | M1-M3 merged as `665b8b5c` after the sole adversarial review and unchanged fast-lane retry were green. Fleet NAS timing and provider-drop acceptance remain post-merge. |
| C-03 | [IMAGE-SERVING-AND-DERIVATIVES](../server/IMAGE-SERVING-AND-DERIVATIVES.md) | C6 | month | unclaimed | | | | 2026-09-20 | Keep `original` backdrops |
| C-04 | [AUTH-HARDENING](../server/AUTH-HARDENING.md) | C7, C8 | month | unclaimed | | | | 2026-09-20 | Fence kept; token expiry is Paul's call |
| C-05 | [DETAIL-READS-AND-STORAGE-AVAILABILITY](../server/DETAIL-READS-AND-STORAGE-AVAILABILITY.md) | C14 | month | unclaimed | | | | 2026-09-20 | M1 marker must land and backfill before M2; cost is the node-local sidecar, not raft |
| C-06 | [TELEMETRY-BACKPRESSURE](../server/TELEMETRY-BACKPRESSURE.md) | C15 | month | unclaimed | | | | 2026-09-20 | Four logical milestone commits in one plan PR; M1 first |
| C-07 | [PLEX-FACADE-PAGING](../server/PLEX-FACADE-PAGING.md) | C4, C9 | month | unclaimed | | | | 2026-09-20 | M0 census decides whether to build; `files_for_items` does not exist yet; C9 is measure-only |
| C-08 | [OBSERVABILITY-BASELINE](../server/OBSERVABILITY-BASELINE.md) | C10, §4.9 | month | unclaimed | | | | 2026-09-20 | M1–M4 parallel; M5 needs M1; there is no access log today at all |
| L-01 | [DVR-SCHEDULER-GUIDE-VIEW-AND-SINK-ISOLATION](../features/DVR-SCHEDULER-GUIDE-VIEW-AND-SINK-ISOLATION.md) | L1, L10 | week (L1) / month (L10) | merged: M1-M3 | gpt-5.6-sol | agent:/root/s01_builder | [PR #407](http://192.168.4.7:3000/noirr/plurx/pulls/407) | 2026-09-21 | M1 immutable owner-side full-guide views (`9a4b83af`), M2 bounded per-sink queues with owned writers and attempt-fenced finalization (`46a1aeec`, `f2127e1a`, `6837d56b`), and M3 operations/status/plan contracts are implemented on `plan/L-01`; every corrective runtime commit now maps to its `rust-gate` evidence in `validation/regressions.d/`. The remaining evidence is post-merge and is recorded later through the rule-4 evidence-only docs PR: the media1 336-hour guide versus 2 MiB public clip comparison, the mixed NAS/local sink-interruption run with metric, attempt and reader-gap numbers, and broad unit qualification after P-01. This plan is not done. |
| L-02 | [LIVE-TV-SESSION-FENCE-PEER-TRANSPORT-AND-START](../features/LIVE-TV-SESSION-FENCE-PEER-TRANSPORT-AND-START.md) | L2, L3, L6, L9 | week (L3) / month | unclaimed | | | | 2026-09-20 | Fence grace bound is Paul's call |
| L-03 | [LIVE-TV-SHARED-TRANSPORT](../features/LIVE-TV-SHARED-TRANSPORT.md) | L4, Q9 (captions) | quarter | unclaimed | | | | 2026-09-20 | Design + caption audit |
| W-01 | [WEB-PLAYER-RECOVERY-AND-LOCAL-SEEK](../clients/WEB-PLAYER-RECOVERY-AND-LOCAL-SEEK.md) | Q10, W3, W7 | week (worker, reporter) / month (seek) | merged: M1-M4 | gpt-5.6-sol | agent:/root/c02_builder | [PR #408](http://192.168.4.7:3000/noirr/plurx/pulls/408) | 2026-09-21 | M1-M4 landed on `plan/W-01`: the hls.js worker at both construction sites with a CSP-blocked inline fallback proof (`b50c4816`), one fenced decoder rescue per attach on the shared retry budget (`3d63b5ac`), bounded early error reporting plus the capture-phase missing-resource reports from the sole adversarial P1 (`3362df6a`, `054ac458`), and covered rolling/progressive seeks routed locally with one fenced reopen at the same target (`aadf9c1c`); each is now mapped to its current check in `validation/regressions.d`. The progressive-remux browser/device matrix and the post-deploy fleet event-rate and journal observation are post-merge evidence, recorded later through the rule-4 evidence-only docs PR, so this plan is NOT done. |
| W-02 | [WEB-TYPE-CHECKING-AND-PLAYER-DECOMPOSITION](../clients/WEB-TYPE-CHECKING-AND-PLAYER-DECOMPOSITION.md) | W8, W9 | month | unclaimed | | | | 2026-09-20 | |
| A-01 | [APPLE-DISPLAY-CRITERIA-AND-AUDIO-SESSION](../clients/APPLE-DISPLAY-CRITERIA-AND-AUDIO-SESSION.md) | §2.8, §2.10 | week | unclaimed | | | | 2026-09-20 | Device tests are GPT prompts in the plan |
| A-02 | [APPLE-PLAYER-CONTROLLER-ATTEMPT-AND-OBSERVATION](../clients/APPLE-PLAYER-CONTROLLER-ATTEMPT-AND-OBSERVATION.md) | A3, A4, A6, A7 | month | unclaimed | | | | 2026-09-20 | After A-01 |
| A-03 | [NATIVE-LIBRARY-PAGING](../clients/NATIVE-LIBRARY-PAGING.md) | A5, D6 (library) | month | unclaimed | | | | 2026-09-20 | Apple + Android |
| A-04 | [NATIVE-ADAPTIVE-QUALITY-DESIGN](../clients/NATIVE-ADAPTIVE-QUALITY-DESIGN.md) | §3.8 | design | unclaimed | | | | 2026-09-20 | Design; recommends revive the wire, delete the Apple helper — Paul decides |
| D-01 | [ANDROID-DISPLAY-MODE-AND-BUFFER-BUDGET](../clients/ANDROID-DISPLAY-MODE-AND-BUFFER-BUDGET.md) | §2.9, D1 | week (measure) / month | unclaimed | | | | 2026-09-20 | Memory measurements first |
| D-02 | [ANDROID-LIFECYCLE-PLAYER-BUILDER-AND-ERROR-CLASSIFICATION](../clients/ANDROID-LIFECYCLE-PLAYER-BUILDER-AND-ERROR-CLASSIFICATION.md) | D2, D3, D4, D7 | month | unclaimed | | | | 2026-09-20 | |
| D-03 | [ANDROID-CREDENTIAL-EXPOSURE-AND-RELEASE-BUILD](../clients/ANDROID-CREDENTIAL-EXPOSURE-AND-RELEASE-BUILD.md) | D5, D6 (release) | week (backup rules) / month | unclaimed | | | | 2026-09-20 | |
| P-01 | [RUST-TEST-EXECUTION-POLICY](../ci/RUST-TEST-EXECUTION-POLICY.md) | §2.2, §4.8 | week | merged: M2-M6 | gpt-5.6-sol | agent:/root/p01_builder | [PR #401](http://192.168.4.7:3000/noirr/plurx/pulls/401) | 2026-09-21 | Option (a), with no schedule, merged as `21eab120` after the sole adversarial P1 and fast lane were green. M1's named runner measurements were blocked by recorded FFmpeg drift; bounded source-only sizing and failed run URLs remain the evidence. |
| P-02 | [SERVICE-LIMITS-CHILD-PRIORITIES-AND-BUILD-HYGIENE](../ci/SERVICE-LIMITS-CHILD-PRIORITIES-AND-BUILD-HYGIENE.md) | §4.6 | month | unclaimed | | | | 2026-09-20 | Observe inherited limits first |
| P-03 | [LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS](../ci/LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md) | §4.4, §4.5 | month | unclaimed | | | | 2026-09-20 | Two decisions are Paul's; `publish_main` is unreachable on current triggers (blocks M7 behind P-01) |
| P-04 | [ARCHITECTURE-DOC-RECONCILIATION](../ci/ARCHITECTURE-DOC-RECONCILIATION.md) | §4.7 | week | merged: M1-M4 | gpt-5.6-sol | agent:/root/p04_builder | [PR #398](http://192.168.4.7:3000/noirr/plurx/pulls/398) | 2026-09-21 | M1-M4 and both adversarial corrections merged as `d77a1afa`; the maintained documentation audit reports no contradictions, missing headers, or unclear statuses. |

Not on the board by design: C16 (scratch reservations) belongs to the
seek-scratch repair effort and is tracked there.

## Reading the board

`unclaimed` rows with `week` priority and no `Notes` dependency are what a
new session should take first, in id order within the `week` set: S-01,
C-01, C-02, K-03, K-07, P-04, then the client `week` items. A row in
`blocked: fleet evidence` is not free work for a session without device or
fleet access — leave it. A row in `in-review` is waiting on the named
reviewer, not on a builder. `in-progress` always names one plan PR, never a
set of milestone PRs; `merged: <landed milestones>` means its one
implementation PR landed and only the post-merge evidence recorded under
rule 4 remains.

This file is kept honest by `tests/operations/test_docs_index.py` (every
linked plan must exist) and by rule 2 above (a claim without a PR is not a
claim).
