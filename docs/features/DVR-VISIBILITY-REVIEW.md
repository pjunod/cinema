# DVR visibility review — findings and their disposition

**Status:** one adversarial design review complete; findings addressed in
the implementation contract, not yet in product code · **Reviewed:**
2026-09-14 · **Source baseline:** cached `origin/main`
`5b0456058804ac2ced4d642c6d8c8d7de8321642`.

Companion to [the Sol implementation handoff](DVR-VISIBILITY-IMPLEMENTATION.md).
This is the record of what the independent reviewer challenged and what
changed in the design. It is separate from the
[earlier base-DVR review](LIVE-TV-DVR-AND-REMINDERS-REVIEW.md), whose recorder,
airing and shared-transport concerns were already incorporated into the
merged DVR. This effort must build on those contracts.

## Review scope and verdict

The user requested an adversarial agent review of three DVR integration
renderings: Live TV programme status, recording Activity and a permanent
Recordings destination. One independent agent reviewed the HTML proposals
and the merged DVR source. It returned **changes required before
implementation**, with six P1 blockers and four P2 refinements. The main
agent independently found the recordings-response envelope defect while
tracing the Activity consumer.

This was a source/design review, not a running-fleet inspection. No product
tests or hardware experiments were performed by the reviewer. The corrected
handoff was authored and checked by the main agent; there was no second
adversarial pass and no claim that the reviewer approved unreviewed code.

## Findings — concrete failure and accepted correction

| ID | Priority | Finding and failure example | Resolution in the handoff |
|---|---|---|---|
| R1 | P1 | A wall-clock percentage was labelled saved duration. An attempt can lose minutes or restart while the bar still advances. Current sink bytes are attempt-local; durable progress updates every 30 seconds. | §3.2 and §4.2 rename the bar Recording window elapsed and bytes Written. Saved duration requires finalized/probed media. Add successful-write age, sample freshness and explicit attempt byte baselines; rate resets across attempts. |
| R2 | P1 | Durable `recording` and browser connectivity were treated as sufficient evidence of healthy capture. Starting/Finishing are not existing durable states; missing-owner data could leave a false active count. | §4 separates durable state, owner phase, capture health and observation freshness, defines label precedence, and expires runtime evidence after 20 seconds. Stop requested remains distinct from Finishing. Unknown observation enums degrade safely. |
| R3 | P1 | A programme could inherit another show's badge through a shared title, channel switch, rerun or tail padding. Manual channel recordings do not necessarily match a guide airing. | §3.1 uses exact `(channel_id, airing_start)` for guide joins and recording ID for actions. Watched/selected identities stay separate. Head/tail padding and manual capture get explicit channel-level copy. |
| R4 | P1 | The timeline fabricated first-video/tuner/actor facts from rows that do not contain those events. Webhook delivery is best-effort, not durable history. A browser timeout is not a tuner interruption. | §6 defines bounded durable events, atomic accepted transitions, idempotency, attempt identity, sequence ordering, history gaps, retention and pagination. First bytes written replaces First video saved. No synthetic history for old recordings. |
| R5 | P1 | Stop can race completion; an automatic retry with `delete_file=1` could delete the result. The proposed saved-portion action could expose media before library linking. User-stopped `done` could misleadingly say Complete. | §3.3 and §7.1 bind confirmation to ID/action, distinguish 202 intent from terminal state, forbid Stop-to-Delete escalation, and condition Play on item/file linking. Stopped early and Preparing playback are explicit. |
| R6 | P1 | Node-local Activity can be empty when capture is on another owner. Cached snapshots during handoff can double-count or resurrect an old attempt. | §5 pins the public projection, optional internal snapshot, existing peer authentication/read gates, durable/runtime join, owner/config/fence checks, truncation and partial-response semantics. Counts distinguish sinks from transports. |
| R7 | P2 | The DVR mockup hid other Activity work and left an old incomplete-recording alert permanently prominent. | §3.2 preserves existing active background sections and prioritizes current problems. §6.2 bounds historical summaries and adds per-user explicit review acknowledgment; new failures can resurface without changing media. |
| R8 | P2 | The new tabs omitted rules, manual recordings and skipped restore, while speculative edit/search actions implied missing APIs. | §3.3 retains these destinations and reminders, preserves deep links, exposes only existing rule controls, and defers arbitrary per-airing edits and Find another airing. Airing and padded capture times remain separate. |
| R9 | P2 | CSS stacking left phone details below a long list, hiding future columns removed navigation, and full HTML replacement lost focus. The prototype toast was not a production confirmation dialog. | §3.4 specifies phone detail navigation, desktop stable selection, each native TV layout, remote focus and return behavior, future-guide navigation, real modal confirmation, accessible progress/tabs and transition-only announcements. |
| R10 | P2 | Independent badge polling could create redundant network/store work and leak diagnostics through shared caches. Existing household recording access must not be silently replaced with an invented ACL. | §5.3 specifies one scoped controller, single-flight refresh, lifecycle/backoff, pagination and shared raw collection with per-request projection. §7.2 records actual household/rule authorization and limits new diagnostics server-side. |
| L1 | P1 | The actual web Activity and Saved consumers expect an array from `/dvr/recordings`, which returns `{rows,next}`. Both discard valid results as empty, and error fallback also masquerades as an empty list. | §1.2 and S01 make the envelope/error/pagination repair the first package. Rules and schedule retain their own response shapes. Add a regression against a real-shaped nonempty page. |

## Decisions that supersede the original rendering copy

- `24 min saved` becomes elapsed-window progress plus confirmed written
  bytes. Playable duration appears only when the finalized file is probed.
- `Receiving normally` becomes a precisely scoped recent-write statement.
  It establishes application write activity, not stream decodability.
- `First video saved` becomes a real `first_bytes_written` event, if that
  event was observed and persisted.
- `2 recording` is used only for confirmed capture observations. Mixed
  retry/unknown states carry separate counts and expired evidence is marked.
- Stop first produces `Stop requested`; `Finishing` needs owner evidence.
  A result without bytes may fail rather than produce a saved portion.
- Saved files awaiting library linking say `Preparing playback`. No active
  capture receives a Play button.
- Recordings retains Series rules, manual creation, skipped restore and
  reminder entry points. Proposed controls without an API are omitted.
- Phone details open immediately. TV navigation follows the existing
  focus/player contract; shrinking desktop HTML is not native design proof.

These are contract corrections, not production implementation claims. The
original visual hierarchy remains useful; its sample numbers and simplified
interactions are not an executable specification for Sol. The corrected
wireframes in the handoff are the build reference.

## What remains to prove during implementation

The handoff's §8 maps every finding to regression scenarios. Sol must provide
runtime evidence for non-owner Activity, idle guide start/finish updates,
attempt accounting, stop/completion races, delayed media linking, bounded
history and actual phone/TV navigation. The independent design review does
not replace the repository-required code review of the implemented tree.

**Disposition:** all ten agent findings and the additional envelope defect
have explicit corrections and acceptance criteria in the handoff. No
finding is waived, no second recorder is proposed, and no product fix is
claimed complete by this documentation pass.
