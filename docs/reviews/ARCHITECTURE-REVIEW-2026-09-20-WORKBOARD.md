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
   changes; the CI fast lane compiles but does not run tests — see
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
| S-01 | [PROCESS-OUTPUT-CAPTURE-AND-SCAN-PROBE-BOUNDS](../streaming/PROCESS-OUTPUT-CAPTURE-AND-SCAN-PROBE-BOUNDS.md) | §2.1, C12 | week | unclaimed | | | | 2026-09-20 | Do first; every other ffmpeg-probe plan assumes M1 |
| S-02 | [MEDIA-BODY-BUFFERS](../streaming/MEDIA-BODY-BUFFERS.md) | §2.4, C1 | week | in-progress | gpt-5.6-sol | agent:/root/c02_builder | [PR #410](http://192.168.4.7:3000/noirr/plurx/pulls/410) | 2026-09-21 | M1 implementation and focused 1.97.1 evidence complete; lab4 before/after acceptance pending. M2 stays pending in this PR until the one-week media1 telemetry prerequisite exists. |
| S-03 | [ENCODED-VOD-HOLD-AND-RELEASE](../streaming/ENCODED-VOD-HOLD-AND-RELEASE.md) | §2.6 | week | unclaimed | | | | 2026-09-20 | M0 fleet count first |
| S-04 | [FONT-ATTESTATION-AND-BLOCKING-IO](../streaming/FONT-ATTESTATION-AND-BLOCKING-IO.md) | §2.7 | week (M1) / month (M2) | unclaimed | | | | 2026-09-20 | Needs `ldd \| grep fontconfig` on media1 before M2 |
| S-05 | [FFMPEG-SPAWN-UNIFICATION](../streaming/FFMPEG-SPAWN-UNIFICATION.md) | F-stream-12/13, §4.1 | month | unclaimed | | | | 2026-09-20 | After S-01 |
| S-06 | [ENCODER-RATE-CONTROL-DEFAULTS](../streaming/ENCODER-RATE-CONTROL-DEFAULTS.md) | Q1 | month | unclaimed | | | | 2026-09-20 | Per-family evidence; no universal maxrate cap |
| S-07 | [TONE-MAP-CHAIN-CORRECTIONS](../streaming/TONE-MAP-CHAIN-CORRECTIONS.md) | Q3 | month | unclaimed | | | | 2026-09-20 | M0 is the hwdownload metadata test |
| S-08 | [INTERLACE-IN-THE-MEDIA-CONTRACT](../streaming/INTERLACE-IN-THE-MEDIA-CONTRACT.md) | Q4, Q9 (bitrate half) | month | unclaimed | | | | 2026-09-20 | Reproduced defect; fixture in review §9 |
| S-09 | [AUDIO-RESOLVED-INDEPENDENTLY](../streaming/AUDIO-RESOLVED-INDEPENDENTLY.md) | Q5 | month | unclaimed | | | | 2026-09-20 | |
| S-10 | [HONEST-MASTER-PLAYLIST](../streaming/HONEST-MASTER-PLAYLIST.md) | Q7, A11 | month | unclaimed | | | | 2026-09-20 | Rolling path emits source geometry; VOD already emits output geometry; BANDWIDTH wrong on both; hard-coded string is `avc1.640034` |
| S-11 | [CODEC-AND-GPU-QUALIFICATION](../streaming/CODEC-AND-GPU-QUALIFICATION.md) | Q12, Q6, Q8 | quarter | unclaimed | | | | 2026-09-20 | M0 encoder inventory on /metrics decides whether NVENC/VideoToolbox milestones exist |
| S-12 | [VOD-BFRAMES-TIMELINE-DESIGN](../streaming/VOD-BFRAMES-TIMELINE-DESIGN.md) | Q2 | design | unclaimed | | | | 2026-09-20 | Design only; `vodgen.rs:399` refuses nonzero CTO today |
| S-13 | [DECODE-FACTS-GATE-AND-FALLBACK](../streaming/DECODE-FACTS-GATE-AND-FALLBACK.md) | C13 | month | unclaimed | | | | 2026-09-20 | Measure before optimising |
| S-14 | [TRANSCODE-DECOMPOSITION-PLAN](../streaming/TRANSCODE-DECOMPOSITION-PLAN.md) | §4.1, §4.2, §4.9 | quarter | unclaimed | | | | 2026-09-20 | Behaviour-preserving moves first; registry unification separate |
| K-01 | [CLUSTER-BACKUP-AND-RESTORE](../cluster/CLUSTER-BACKUP-AND-RESTORE.md) | §2.3, S4 | month | unclaimed | | | | 2026-09-20 | The procedure is the deliverable |
| K-02 | [RAFT-SNAPSHOT-CADENCE-AND-CONSISTENT-CUT](../cluster/RAFT-SNAPSHOT-CADENCE-AND-CONSISTENT-CUT.md) | S2, S5 | month | unclaimed | | | | 2026-09-20 | M0 measurement on each voter |
| K-03 | [REPLICATED-WRITE-RATE-HYGIENE](../cluster/REPLICATED-WRITE-RATE-HYGIENE.md) | S3 | week | unclaimed | | | | 2026-09-20 | Keep the atomic claim |
| K-04 | [BOUNDED-REPLICA-READS-ROLLOUT](../cluster/BOUNDED-REPLICA-READS-ROLLOUT.md) | S1 | month | unclaimed | | | | 2026-09-20 | Consistency-policy change; no auth cache |
| K-05 | [SQLITE-READ-PATH-AND-QUERY-PLANS](../cluster/SQLITE-READ-PATH-AND-QUERY-PLANS.md) | S6, S7, S11 | month | unclaimed | | | | 2026-09-20 | Search predicate is the corrected one |
| K-06 | [CLOCK-SKEW-GUARD-DESIGN](../cluster/CLOCK-SKEW-GUARD-DESIGN.md) | S9 | design | unclaimed | | | | 2026-09-20 | Half the exchange exists (`x-plurx-cluster-time-ms`); auth windows already assume ≤5 s |
| K-07 | [STORE-CONTRACT-COVERAGE-AND-PLACEHOLDER-VALIDATION](../cluster/STORE-CONTRACT-COVERAGE-AND-PLACEHOLDER-VALIDATION.md) | S10 | week | unclaimed | | | | 2026-09-20 | Selector output confirmed 3/16 + 7/24; 29 discarded store results, not 13 |
| K-08 | [HIQLITE-FORK-AND-DEPENDENCY-CLEANUP](../cluster/HIQLITE-FORK-AND-DEPENDENCY-CLEANUP.md) | §4.3 | month | unclaimed | | | | 2026-09-20 | Do not rename (patch-by-name); three edges reach aws-lc, cryptr fix removes one |
| C-01 | [HTTP-LISTENER-TIMEOUTS-AND-ASSET-DELIVERY](../server/HTTP-LISTENER-TIMEOUTS-AND-ASSET-DELIVERY.md) | §2.5, W1, W2, W5, W6 | week | merged: M1-M3 | gpt-5.6-sol | agent:/root/c01_builder | [PR #395](http://192.168.4.7:3000/noirr/plurx/pulls/395) | 2026-09-20 | One plan PR merged as `79113254`; §6.2–§6.4 lab soak, HAR/waterfall, and device-reader evidence remains pending, so this row is not done. |
| C-02 | [SCAN-AND-ENRICHMENT-HYGIENE](../server/SCAN-AND-ENRICHMENT-HYGIENE.md) | C3, C5 | week | unclaimed | | | | 2026-09-20 | |
| C-03 | [IMAGE-SERVING-AND-DERIVATIVES](../server/IMAGE-SERVING-AND-DERIVATIVES.md) | C6 | month | unclaimed | | | | 2026-09-20 | Keep `original` backdrops |
| C-04 | [AUTH-HARDENING](../server/AUTH-HARDENING.md) | C7, C8 | month | unclaimed | | | | 2026-09-20 | Fence kept; token expiry is Paul's call |
| C-05 | [DETAIL-READS-AND-STORAGE-AVAILABILITY](../server/DETAIL-READS-AND-STORAGE-AVAILABILITY.md) | C14 | month | unclaimed | | | | 2026-09-20 | M1 marker must land and backfill before M2; cost is the node-local sidecar, not raft |
| C-06 | [TELEMETRY-BACKPRESSURE](../server/TELEMETRY-BACKPRESSURE.md) | C15 | month | unclaimed | | | | 2026-09-20 | Four logical milestone commits in one plan PR; M1 first |
| C-07 | [PLEX-FACADE-PAGING](../server/PLEX-FACADE-PAGING.md) | C4, C9 | month | unclaimed | | | | 2026-09-20 | M0 census decides whether to build; `files_for_items` does not exist yet; C9 is measure-only |
| C-08 | [OBSERVABILITY-BASELINE](../server/OBSERVABILITY-BASELINE.md) | C10, §4.9 | month | unclaimed | | | | 2026-09-20 | M1–M4 parallel; M5 needs M1; there is no access log today at all |
| L-01 | [DVR-SCHEDULER-GUIDE-VIEW-AND-SINK-ISOLATION](../features/DVR-SCHEDULER-GUIDE-VIEW-AND-SINK-ISOLATION.md) | L1, L10 | week (L1) / month (L10) | unclaimed | | | | 2026-09-20 | |
| L-02 | [LIVE-TV-SESSION-FENCE-PEER-TRANSPORT-AND-START](../features/LIVE-TV-SESSION-FENCE-PEER-TRANSPORT-AND-START.md) | L2, L3, L6, L9 | week (L3) / month | unclaimed | | | | 2026-09-20 | Fence grace bound is Paul's call |
| L-03 | [LIVE-TV-SHARED-TRANSPORT](../features/LIVE-TV-SHARED-TRANSPORT.md) | L4, Q9 (captions) | quarter | unclaimed | | | | 2026-09-20 | Design + caption audit |
| W-01 | [WEB-PLAYER-RECOVERY-AND-LOCAL-SEEK](../clients/WEB-PLAYER-RECOVERY-AND-LOCAL-SEEK.md) | Q10, W3, W7 | week (worker, reporter) / month (seek) | unclaimed | | | | 2026-09-20 | |
| W-02 | [WEB-TYPE-CHECKING-AND-PLAYER-DECOMPOSITION](../clients/WEB-TYPE-CHECKING-AND-PLAYER-DECOMPOSITION.md) | W8, W9 | month | unclaimed | | | | 2026-09-20 | |
| A-01 | [APPLE-DISPLAY-CRITERIA-AND-AUDIO-SESSION](../clients/APPLE-DISPLAY-CRITERIA-AND-AUDIO-SESSION.md) | §2.8, §2.10 | week | unclaimed | | | | 2026-09-20 | Device tests are GPT prompts in the plan |
| A-02 | [APPLE-PLAYER-CONTROLLER-ATTEMPT-AND-OBSERVATION](../clients/APPLE-PLAYER-CONTROLLER-ATTEMPT-AND-OBSERVATION.md) | A3, A4, A6, A7 | month | unclaimed | | | | 2026-09-20 | After A-01 |
| A-03 | [NATIVE-LIBRARY-PAGING](../clients/NATIVE-LIBRARY-PAGING.md) | A5, D6 (library) | month | unclaimed | | | | 2026-09-20 | Apple + Android |
| A-04 | [NATIVE-ADAPTIVE-QUALITY-DESIGN](../clients/NATIVE-ADAPTIVE-QUALITY-DESIGN.md) | §3.8 | design | unclaimed | | | | 2026-09-20 | Design; recommends revive the wire, delete the Apple helper — Paul decides |
| D-01 | [ANDROID-DISPLAY-MODE-AND-BUFFER-BUDGET](../clients/ANDROID-DISPLAY-MODE-AND-BUFFER-BUDGET.md) | §2.9, D1 | week (measure) / month | unclaimed | | | | 2026-09-20 | Memory measurements first |
| D-02 | [ANDROID-LIFECYCLE-PLAYER-BUILDER-AND-ERROR-CLASSIFICATION](../clients/ANDROID-LIFECYCLE-PLAYER-BUILDER-AND-ERROR-CLASSIFICATION.md) | D2, D3, D4, D7 | month | unclaimed | | | | 2026-09-20 | |
| D-03 | [ANDROID-CREDENTIAL-EXPOSURE-AND-RELEASE-BUILD](../clients/ANDROID-CREDENTIAL-EXPOSURE-AND-RELEASE-BUILD.md) | D5, D6 (release) | week (backup rules) / month | unclaimed | | | | 2026-09-20 | |
| P-01 | [RUST-TEST-EXECUTION-POLICY](../ci/RUST-TEST-EXECUTION-POLICY.md) | §2.2, §4.8 | week | unclaimed | | | | 2026-09-20 | Decision is Paul's (§7.1); the two red tests are not |
| P-02 | [SERVICE-LIMITS-CHILD-PRIORITIES-AND-BUILD-HYGIENE](../ci/SERVICE-LIMITS-CHILD-PRIORITIES-AND-BUILD-HYGIENE.md) | §4.6 | month | unclaimed | | | | 2026-09-20 | Observe inherited limits first |
| P-03 | [LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS](../ci/LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md) | §4.4, §4.5 | month | unclaimed | | | | 2026-09-20 | Two decisions are Paul's; `publish_main` is unreachable on current triggers (blocks M7 behind P-01) |
| P-04 | [ARCHITECTURE-DOC-RECONCILIATION](../ci/ARCHITECTURE-DOC-RECONCILIATION.md) | §4.7 | week | in-progress | gpt-5.6-sol | agent:/root/p04_builder | [PR #398](http://192.168.4.7:3000/noirr/plurx/pulls/398) | 2026-09-20 | Both adversarial findings addressed: one-plan protocol aligned; 305-row audit now reports zero contradictions, missing headers or unclear statuses; exact-head validation pending |

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
