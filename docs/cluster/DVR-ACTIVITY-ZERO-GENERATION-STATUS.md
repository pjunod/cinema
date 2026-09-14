# DVR Activity generation zero — repair status

**Status:** reviewed · local qualification complete · hosted fast lane next · **Updated:** 2026-09-14 ·
**Base:** `0b2490839112` · **Branch:**
`codex/fix-dvr-activity-zero-generation`

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
| D02 · correct the peer contract | complete | Removed only the invalid `serving_generation > 0` predicate; the field is an unsigned fence token and zero is its healthy initial epoch | Complete |
| D03 · retain the failure | complete | `initial_dvr_serving_generation_is_a_valid_peer_snapshot` serializes the real peer shape and requires the decoder to answer it | Complete |
| D04 · adversarial review | complete | The single review of `decada4790d8` found no concrete correctness, security, coverage, documentation, or workflow defect; it retained the lack of a multi-daemon case as residual risk | Complete |
| D05 · qualification and promotion | in progress | Pinned formatting, compile, Clippy, and the focused regression pass; pull request #315 is the authoritative hosted receipt | Exact-head fast lane passes, then the pull request merges |

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
| Pinned compiler | complete | `rustc 1.97.1 (8bab26f4f 2026-07-14)` and `cargo 1.97.1`; `cargo check -p plurxd --all-targets --locked` passed |
| Adversarial review | complete | One pass against `decada4790d8`; no concrete findings and no second review requested |
| Static validation | complete | `cargo fmt --all -- --check` and `cargo clippy -p plurxd --all-targets --locked -- -D warnings` passed; Clippy's two current-main DVR style findings were corrected in this batch |
| Focused regression | complete | `cargo test --locked -p plurxd --bin plurxd initial_dvr_serving_generation_is_a_valid_peer_snapshot`: 1 passed, 0 failed, 2,245 filtered out |
| Fast lane | ready | Forgejo pull request #315 is the exact-head authority; making it ready starts the single hosted run |
| Main merge | waiting | Merge only after pull request #315's exact-head fast lane is green |
