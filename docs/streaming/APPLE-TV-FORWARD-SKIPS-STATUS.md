# Apple TV forward skips — implementation and promotion ledger

**Status:** implementation in progress · **Started:** 2026-09-20 ·
**Branch:** `codex/apple-tv-forward-skips` · **Base:** `79113254`

Companion to
[MKV-DURATION-AND-SLIDING-HLS-IMPLEMENTATION.md](MKV-DURATION-AND-SLIDING-HLS-IMPLEMENTATION.md)
— this page records the exact implementation, review, focused proof, fast-lane
and merge state for the rolling-publication pacing repair.

## Outcome — advertise consumption, not writer speed

The rolling server currently publishes the writer's whole completed tail at
each target. A writer running near 2× can therefore move an Apple TV's live
edge by roughly twice the elapsed playback time, eventually forcing native
recovery and a large forward position discontinuity.

This repair keeps the existing rolling route and actor ownership. It adds a
cumulative publication budget, selects only the earned completed prefix,
protects the segment containing the viewer's back-buffer position, and fences
publication against the exact accepted demand and producer attempt.

There is no feature flag or settings toggle. This is an always-on correctness
repair for an existing path, so an advisory "enable" control would falsely
present safe playlist accounting as optional. Existing resource, lifetime and
failure checks remain authoritative.

## Delivery state — one branch, one main-bound pull request

| Stage | State | Evidence |
|---|---|---|
| Separate working clone | done | Fresh Forgejo clone at `79113254`; the user's existing clones are read-only references |
| Rust compiler loop | done | Rust 1.97.1; baseline `cargo check -p plurxd --all-targets` passed before edits |
| Cumulative budget and bounded prefix | implemented | Cumulative `C + R`, `R <= 124 s`, real-EXTINF prefix selection and carried surplus live in `RollingPublicationClock` and `publication_cycle_at` |
| Actor demand/protected-start fence | implemented | Accepted demand sequence/age ride `RollingLeaseSnapshot`; the actor rejects stale identity, attempt and a first segment past `max(origin, C - G - B)` |
| Retention and EOF preservation | implemented | Download-frontier removal is clamped to the protected segment; EOF retains the protected history and ordered tail |
| Legacy compatibility | implemented | Fixed 1× wall-time bootstrap plus resolved-fetch cap; explicit cutover clears the bootstrap anchor once |
| Focused regression set | passed | 13/13 `rolling_publication_budget*` and 4/4 `mkv_hls_schedule*` tests passed on code candidate `7654a184` |
| Documentation and validation catalog | implemented | This page, `docs/README.md`, the `playback.pipeline` contract, and exact `6f4e279e`/`a157179a`/`5a387be6` regression mappings |
| Adversarial implementation review | complete and addressed | The single review found a variable-duration legacy bootstrap deadlock, backdated commit deadlines and three weak proof seams; commit `5a387be6` addresses all findings |
| Final focused and fast-lane proof | local proof passed; Forgejo pending | Rust 1.97.1 check/clippy/format, both focused filters, four docs-index tests and `git diff --check` passed; the current pushed candidate still needs its Forgejo fast lane |
| Merge to `main` | not started | only after the current fast lane is green |

## Evidence rules — make every green claim reproducible

The final entry records the candidate SHA, Rust version, exact focused test
filters and selected test counts, compile/lint/format results, review findings,
PR number, fast-lane run and merge SHA. A filtered command that selects zero
tests is a failure even if Cargo exits successfully.

The physical Apple TV and actual-server ten-minute continuity runs require
the lab and reference title named in the handoff. If that environment is not
available from this execution host, the page will state the missing evidence;
it will not relabel a synthetic regression as device qualification.

## Implementation snapshot — the existing owners remain authoritative

The actor records only newly accepted demand sequences and acceptance times;
replays and media renewals cannot manufacture freshness. Both the worker and
actor use the same projection helper, which advances only `Active` plus
`Rendering` observations and freezes at 30 seconds.

The clock selects the first real segment end at or after the cumulative target
without exceeding one validated segment of rounding. At a due target with less
than one segment of new credit, it can publish only the next segment within the
124-second safety floor and carries that excess forward. The renderer takes an
explicit final segment, so completed media beyond the selected prefix remains
private and charged instead of leaking through the writer's EVENT tail.

Legacy publication cannot safely round beyond a budget because it has no
accepted playback position. It therefore selects the last completed endpoint
inside the fixed wall/download allowance. This lets a 47-second prefix
bootstrap when the next variable-duration segment ends at 52 seconds, while
keeping that next segment private until earned. Snapshot availability, grace
and the next publication deadlines are stamped only after actor admission.

The reviewed code candidate `7654a184` is green on
`rustc 1.97.1 (8bab26f4f 2026-07-14)`:

```bash
rustup run 1.97.1 cargo test -p plurxd --bin plurxd rolling_publication_budget # 13 passed
rustup run 1.97.1 cargo test -p plurxd --bin plurxd mkv_hls_schedule          # 4 passed
rustup run 1.97.1 cargo check -p plurxd --all-targets
rustup run 1.97.1 cargo clippy -p plurxd --all-targets -- -D warnings
rustup run 1.97.1 cargo fmt --all -- --check
python3 -m unittest discover -s tests/operations -p test_docs_index.py        # 4 passed
git diff --check
```

The first focused execution exposed test-fixture defects: an invalid client
UUID, response commits that omitted the production handoff, and synthetic
policy clocks paired with real actor throttling. Commits `ab2697e3` and
`603687a9` correct those fixtures without bypassing the actor or publication
worker. The final rerun selected 13 tests and passed all 13. Clippy then found
one mechanical manual-clamp warning; `7654a184` applies its behavior-preserving
rewrite and the full denied-warning pass is green.

The core runtime implementation is commit `6f4e279e`; bounded low-rate
recovery and its extra sustained/freshness regressions are commit `a157179a`;
the adversarial-review corrections are commit `5a387be6`; and the final proof
corrections are `ab2697e3`, `603687a9` and `7654a184`. The regression catalog
maps all exact corrective commits to `playback.pipeline` and the Rust gates.

Forgejo briefly treated the initial draft API request as ready because this
server recognizes the `WIP:` title convention rather than the submitted draft
field. Run `2368` was therefore created before any final validation and failed;
it is not promotion evidence. PR #404 was immediately returned to draft and
remains there until the reviewed candidate's one intended fast-lane run.

## Decisions to revisit — only if evidence forces them

1. **Keep the existing flow scheduler.** The bounded publication prefix should
   make its staged-inventory hold effective. A predictive scheduler is out of
   scope unless the 1.2×/1× focused regression misses a deadline.
2. **Keep legacy playback available.** Its fixed 1× bootstrap clock is bounded
   by elapsed time and resolved fetch progress; no client-buffer guess disables
   the route.
3. **No Developer-settings enable section.** The implementation handoff calls
   this an always-on repair and explicitly excludes settings work. The user's
   advisory-enable instruction is applied to optional features, not to this
   correctness invariant.
