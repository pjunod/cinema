# Playback start — continuation and merge ledger

**Status:** open · **Owner:** Codex, session
`01a1c166-b5b2-7033-bd74-77816ba09733` · **Updated:** 2026-11-09.

Companion to the [implementation plan](PLAYBACK-START-PLAN.md) and
[Phase 1 build contract](PLAYBACK-START-BUILD.md). Phase 1 merged in
#1454 (`b08b5b8ec`); Apple build 294 landed in #1466 (`09097650f`).
All four fleet nodes report main `8ab61a779`. Build deployment is established;
queue recovery and physical-device acceptance remain open. Media1 still
reports degraded, 3,840 eligible index requests and 0 ready in 24 hours.
Persisted failed-row counts are not new failures over that interval.

## Merged batch — index timestamp repair and Source locate eligibility

S4 removes the index-only fatal muxer-diagnostic policy while retaining
HEVC tracing, configuration and complete-video validation. S12 uses the
canonical locate route policy to exclude learners in the existing media
peer query; ordinary media retains learner capacity. Both repairs come from
the [shared-library RCA](SHARED-LIBRARY-PLAYBACK-RCA.md). GPT 6.1 Sol built
S12; Codex owns S4, integration and merge.

Independent implementation review found no actionable findings. Local
Clippy, syntax checks and both focused regressions passed. PR #1475 merged
as `2ad533907` after exact-candidate promotion run 7052 passed. Deployment
of this batch remains separate from the observed `8ab61a779` fleet. The first
preflight stopped on the intentionally pinned membership file hash. Tables
31/32 were refreshed after confirming both mapped authority bodies are
byte-identical to the base; only the media directory changed. Local macOS Clippy
also exposed existing Linux-only processed-upgrade imports/test helpers;
the batch aligns their declarations with their consuming platform.

Apple build 294 is live in internal TestFlight for iOS and tvOS, both
`VALID` and `IN_BETA_TESTING`. Export and upload succeeded from the M5
with the signed-in Xcode account and Cloud Managed Apple Distribution;
[the publishing recovery notes](../PUBLISHING.md) retain the proven method.
The standing publisher has not been migrated or requalified. Android 193
and native prepared-switch/TTFF measurements remain open.

## Current batch — A1/B1

Codex owns `codex/playback-start-phase2`; no pull request is open yet.
GPT 6.1 Sol extracted the canonical video-only fragment row observation
for reuse by the growing engine; integration and identity design are in
progress. No growing-engine behavior or qualification is claimed yet.
Server/web development and web testing proceed independently of mobile
publication. The deployed Phase 1 web build can be tested now.

The queue stall is traced to an identifier-contract mismatch. Subtitle-source
requests use the 64-character digest returned by
`SubtitleSourceStamp::generation`; both queue backends persist that as the
request ID. `background_owner_key` incorrectly requires a UUID for analysis
custody. Registration rejects a legitimate request before creating custody;
`settle_unstarted_analysis_source` retries the deterministic rejection while
the independent heartbeat retains its lease and maintenance source-I/O
reservation. The live owner logged that rejection at its attempt start and
never reached source probing. Repair the custody validator to match queue
admission and exercise registration/settlement with a real generated ID;
retain the exact owner/fence checks and ambiguous-write recovery protocol.
The fix is being built; the fleet queue has not yet been repaired.

## Native admission — A1-N, PR #1476

**Owner:** Codex, chat `01a1c172-ef6a-77e3-8422-95798308bc5c`.
Implementation and local acceptance are recorded in
[PR #1476](http://forge.lan:3000/noirr/plurx/pulls/1476); that PR is the
current landing-state authority. This is a post-promotion repair to Phase 1,
not completion of A1's growing copy engine.

The fixed target remains 16 seconds. Exact hls.js retains its qualified short
bootstrap; native/absent/unknown transport admits three advertised target
intervals, with one bounded complete cut for initial legacy admission. The
normal download-frontier cap resumes afterward. No actor deadline or feature
switch changes. The shared Gran Turismo fixture pins the actual emitted
first snapshot; a count of three segments plus EVENT alone is insufficient.

Eight focused Rust cases and strict workspace Clippy passed. Matched-source
native WebKit off/cold/warm cases reached playing at 759/745/713 ms; the
24-second positive continued past the first reload. The old short controls
remain non-playing, and the completed short title starts. Successful cases
were retained while failed observer checks alone were rerun. These are local
matched-source receipts; full daemon/fleet acceptance remains open.

## Current process — one final candidate review and validation

Paul's 2026-11-09 direction supersedes the original per-lane/design-review
schedule: batch normal commits into a reviewable PR; adversarial review
when ready to merge; fix findings; run the final-candidate checks once and
repeat failed checks only. Required CI remains a merge condition. Keep PR
ownership, current state and remaining merge requirements explicit. All
task workspaces, scratch, receipts and build output use external agent
storage. No feature gates are added.

A1/B1 and later lanes remain unbuilt. S1–S3/S5–S11 join A1 rather than
introducing a separate shared-library engine. The two remote ownership
follow-ups in the history below remain open.

## Execution history


| Date | Lane | Model / session | Action |
|---|---|---|---|
| 2026-11-07 | plan | claude-fable-5-1 / 01BCWLA2SJPLAmibeaz9L5xb | Rev 1 written from the review; rulings 1–4 recorded. |
| 2026-11-07 | plan | claude-fable-5-1 / 01BCWLA2SJPLAmibeaz9L5xb | Fleet state read (queue wedged at cap; release-as-fence); Fable review applied (rev 2, not what #1401 merged — bridge propagation). |
| 2026-11-07 | plan | claude-fable-5-1 / 01BCWLA2SJPLAmibeaz9L5xb | Astra review applied; rev 3 is the first revision build agents may claim. |
| 2026-11-07 | plan | claude-fable-5-1 / 01BCWLA2SJPLAmibeaz9L5xb | Ruling 3 mechanism settled (M6); Phase 1 build contracts written (QUEUE, CREATE). |
| 2026-11-07 | H2, C2, C3, C1, D2, R1, H1, G1, D1 | claude-opus-5-5 / 01BCWL9xwmpSjitHqZMTRAht | Phase 1 claimed. Lanes integrate on `effort/playback-start`; per Paul (2026-11-07) one adversarial review and one fast-lane run on effort → main rather than per lane. H2/C2/C3 built; H1 built by a Sonnet sub-agent; C1, D2, R1 by Opus sub-agents. |
| 2026-11-08 | D2 (review 1 S4) | claude-opus-5-5 / 01BCWL9xwmpSjitHqZMTRAht | Open gap, not fixed: a remote start whose reply is lost on both the ask and the one re-ask (`start_remote_recovering_lost_reply`) moves to the next candidate under the same incarnation while the first worker may still hold a session for it. That first producer runs until its confirmation watcher sees another owner's activation of the incarnation (≤ 500 ms after it lands) or, when no activation comes, to `REMOTE_ACTIVATION_CONFIRMATION_WINDOW`. Closing it needs the transport to tell "never delivered" from "delivered, reply lost" (`PeerTransportError` does not) so only the first moves on. Pinned by `media_sessions::tests::a_dropped_remote_start_reply_takes_the_session_the_worker_already_holds`. |
| 2026-11-08 | H2, C2, C3, H1, R1, D2, C1, G1 | claude-opus-5-5 / 01BCWL9xwmpSjitHqZMTRAht | Landed on the effort: #1424, #1427, #1428, #1431, #1433, #1434. Fable review 1 (landed lanes) → fixes #1436, #1440; Fable review 2 (C1, G1) → fixes #1438. Combined effort head clippy-clean; paperwork vs main 16/16. D1 building; then D1 review, one fast-lane run, effort → main. |
| 2026-11-08 | D1 (review 3 S7) | claude-opus-5-5 / 01BCWL9xwmpSjitHqZMTRAht | Open gap: remote-owned ordinary sessions never upgrade (the create registers the intent only where the ingress owns the route). Follow-up: carry the intent on the remote start dispatch and trigger from the owner actor's presented phase. |
| 2026-11-09 | continuation | Codex / 01a1c166-b5b2-7033-bd74-77816ba09733 | Phase 1 merge ledger reconciled against main `8ab61a779`. Media1 reports that build; queue still degraded (3,840 eligible, 0 ready in 24 h), so merged implementation is not fleet acceptance. Signing publisher is offline; its last attempted export reported no account and no distribution identity/private key. |
| 2026-11-09 | S4, S12 | Codex; GPT 6.1 Sol for S12 | Root-cause follow-ups from [shared-library RCA](SHARED-LIBRARY-PLAYBACK-RCA.md): index muxer timestamp correction and route-eligible Source locate directory implemented; review and validation pending. No feature gates. |
| 2026-11-09 | process | Paul; executed by Codex | Current direction supersedes the original per-lane/design review schedule: batch normal commits into a reviewable PR, adversarial review when ready to merge, fix findings, then one final-candidate validation run with only failed checks repeated. Keep all task storage on the external agent drive. |
