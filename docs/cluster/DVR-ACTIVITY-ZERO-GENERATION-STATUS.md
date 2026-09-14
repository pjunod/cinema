# DVR Activity generation zero — repair status

**Status:** implementation in progress · **Updated:** 2026-09-14 · **Base:**
`0b2490839112` · **Branch:** `codex/fix-dvr-activity-zero-generation`

Companion to the [Activity peer-read plan](ACTIVITY_PEER_READ_FIX_PLAN.md)
(the peer snapshot contract) and the
[DVR visibility status](../features/LIVE-TV-DVR-VISIBILITY-STATUS.md) (the
runtime observation contract) — this page records the diagnosis, repair,
review, qualification, and promotion of the generation-zero interoperability
fix.

The work runs from an isolated Forgejo clone under `/private/tmp`; the existing
developer checkout is not used for source or validation evidence. Rust uses
the repository-pinned 1.97.1 toolchain. Tests remain deferred until the code
and regression are complete and the one adversarial review has been
addressed, so the final candidate consumes the fast lane once.

## Delivery ledger — one corrective pull request

| Work order | State | Evidence | Exit condition |
|---|---|---|---|
| D01 · reproduce and isolate | complete | `m6` recorded 76 `invalid_response` outcomes only while `nynuc` published an active DVR sink; the count stopped when the recording ended | Root cause names the exact rejected field |
| D02 · correct the peer contract | building | A zero serving generation is the healthy initial loss epoch, not a missing fence | Generation zero is admitted without weakening identity, size, or DVR field bounds |
| D03 · retain the failure | building | The current unit coverage constructs no DVR observation in `snapshot_is_bounded` | A focused regression fails before the repair and accepts generation zero afterward |
| D04 · adversarial review | waiting | Review is deliberately deferred until the implementation is merge-ready | One adversarial pass completed and every accepted finding addressed |
| D05 · qualification and promotion | waiting | No test suite has run during implementation | Focused regression and fast lane pass on the reviewed candidate, then the pull request merges |

## Root cause — a valid initial epoch was treated as absent

`ServingFence` initializes `loss_generation` to zero and increments it only
when a ready process loses serving authority. A healthy process may therefore
admit a DVR transport under generation zero for its entire lifetime.

The DVR Activity decoder added `serving_generation > 0` as an input bound.
That predicate rejects the owner's otherwise valid snapshot whenever an active
recording exposes the initial epoch. The owner does not decode its local
snapshot through the peer boundary, which is why `nynuc` remained correct
while another voter displayed `sent an invalid response` for `nynuc`.

## Guardrails — repair the assertion, not the feature

1. **Keep DVR visibility always on.** This is a decoder correctness repair;
   no feature flag or runtime gate belongs on the path.
2. **Retain every meaningful peer bound.** Node identity, non-empty recording
   and channel identities, configuration generation, attempt, phase, reason,
   cardinality, and byte budgets remain fail closed.
3. **Do not renumber the serving fence.** Generation zero is already a valid
   token consumed throughout playback and DVR fencing. Changing its origin
   would widen the fix into unrelated lifecycle code.
4. **Validate once after review.** The final reviewed tree receives the
   focused Rust regression and the main fast lane; full unit-suite repair is
   outside this corrective pull request.

## Qualification ledger — receipts attach to an exact candidate

| Evidence | State | Receipt |
|---|---|---|
| Pinned compiler | ready | `rustc 1.97.1 (8bab26f4f 2026-07-14)` and `cargo 1.97.1` are available through `rustup run 1.97.1` |
| Adversarial review | waiting | Requested only after implementation and regression are committed |
| Focused regression | waiting | Run once on the reviewed candidate |
| Fast lane | waiting | Apply `fast-lane` only after review findings are resolved |
| Main merge | waiting | Merge only after the fast lane is green |
