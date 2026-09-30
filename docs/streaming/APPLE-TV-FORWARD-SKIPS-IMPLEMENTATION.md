# Apple TV forward skips — Sol implementation handoff

**Status:** historical build handoff; cumulative pacing is merged into main,
physical Apple TV acceptance remains open · **Written:** 2026-09-20 ·
**Updated:** 2026-09-29.

Companion to the [root-cause report](APPLE-TV-FORWARD-SKIPS-RCA.md) and the
existing [sliding-HLS contract](MKV-DURATION-AND-SLIDING-HLS-IMPLEMENTATION.md),
§§9.1–9.5. This document specified the cumulative accounting that §9.2
required. The [delivery status](APPLE-TV-FORWARD-SKIPS-STATUS.md) records what
landed, its tests and the remaining physical acceptance. The instructions
below are the original build contract, not current unfinished work.

Paul's latest instructions govern scope: finish this as one bounded repair,
add no software feature gates, and use the current fast development lane.
There is no new enablement flag, client capability requirement, settings UI,
route-disable switch, or opt-in rollout. Existing resource/lifetime checks
still apply. Runtime invariants are correctness checks, not feature gating.

Read §§2–6, implement in order, run §7, then follow §8. Do not start a new
research campaign, build a second control state machine, or rewrite unrelated
playback paths. Report a concrete failing contract if one forces more scope.

## 1. Outcome and evidence anchors

On a rolling movie at 1× playback, a 2× writer must fill a bounded reserve and
then alternate production and holds. Advertised media must track consumption
rather than writer speed. The playlist must retain the protected playback
segment; published objects must keep their existing availability promises.

Production `c70390bc3` showed repeating native-recovery events roughly every
72 wall seconds, with reported positions advancing 144 seconds each cycle.
The isolated macOS AVPlayer replay showed sampled position advances of
75.240 and 72.883 seconds in approximately one wall second. Subtracting
elapsed playback gives discontinuity residuals of 74.240 and 71.883 seconds.
The paced comparison had neither event.

The two 12-second request gaps measure media never requested, not total
playback loss. Position continuity is the primary qualification metric. The
capture does not directly identify every rendered frame, and its pre-stall
`loadedTimeRanges` contain a six-second interval around 264 seconds while
`currentTime` is around 207 seconds. Do not turn that into proof that a
56-second contiguous buffer was discarded. Exact rendered loss needs decoded
frame/sample evidence; it is not required to prove this pacing defect or
justify fixing the position discontinuity.

The [RCA](APPLE-TV-FORWARD-SKIPS-RCA.md) contains the code and production
anchors. This handoff was checked against local
`0afefd92ac79ca4945bf8ea4ae1ecdf1936b3037`. Fable reported checking the same
relevant functions at `175c8ad53`; that later remote revision was not
independently fetched for this document. Start implementation on current
main and re-check the named functions there.

## 2. Keep the repair in the existing ownership model

### 2.1 Files and exact responsibilities

| File | Work |
|---|---|
| [transcode.rs](../../crates/plurxd/src/transcode.rs) | Extend `RollingPublicationClock`; compute a cumulative frontier; render a bounded prefix in `publication_cycle`; retain existing flow control; add integration tests and diagnostics |
| [playback_control.rs](../../crates/plurxd/src/playback_control.rs) | Expose accepted-demand identity/age in `RollingLeaseSnapshot`; extend `RollingPublicationObservation`; validate current demand and protected start in `observe_publication_at`; add focused actor tests |
| This document, the [RCA](APPLE-TV-FORWARD-SKIPS-RCA.md), and [index](../README.md) | Record actual implementation, commands and results |
| Existing validation regression catalog | Add the focused regression mapping required by repository conventions |

No Apple, Android, web UI, workflow or codec changes are expected. Additive
server diagnostics can use the existing Activity/status serialization; do
not make a dashboard redesign part of this repair.

### 2.2 Budget stays in `RollingPublicationClock`

Use the existing `self.publication` mutex and `RollingPublicationClock`
(the actual type name, not `PublicationClock`). Keep the existing commit path:

```text
build exact candidate from staged media and accepted demand
    -> acquire child_transition
    -> revalidate producer attempt
    -> existing observe_publication actor command validates candidate
    -> existing publication lock installs that exact immutable snapshot
    -> request_flow re-evaluates the existing hold rule
```

The clock owns the cumulative budget. The actor remains authoritative for
accepted demand, attempt, expiry, failure, and whether publication may commit.
Do not add a proposal/authorization/commit API, a second budget owner, or a
second producer-signaling loop.

Preserve lock order and the existing worker's cancellation/lifetime behavior.
A rejected observation cannot change served revision, budget accounting or
removal promises. A successful publication cannot substitute a newer writer
playlist for the exact candidate accepted by the actor. Test a newer demand
and a replacement arriving between candidate construction and acceptance.
Do not assume that demand-sequence equality alone fences a later replacement.

## 3. Constants, state and arithmetic

### 3.1 Parameter decisions

Use existing constants rather than adding configuration:

| Quantity | Definition | Captured route |
|---|---|---|
| `T` | Frozen `ROLLING_PRESENTATION_TARGET_SECS` | 16 s |
| `S` | Validated presentation segment upper bound, `T` on this route | 16 s |
| `W` | `ROLLING_SERVED_WINDOW_MS` | 180 s |
| `B` | `CLIENT_BACK_BUFFER_SECS` | 30 s |
| `G` | `2 * NEXT_EXCHANGE_MS` | 10 s |
| Observation horizon | `ROLLING_EXPLICIT_LEASE_TIMEOUT_MS`, measured since accepted demand | 30 s |
| `Rmax` | `W - G - S - B` | 124 s |
| `R(rate)` | `min(max(3*T, rolling_initial_runway_ms(rate)), Rmax)` | 48 s at ≤1×, 96 s at 2×, capped at 124 s |

Keep units explicit and use checked integer milliseconds. The `3T` minimum
corrects Fable's proposed reserve below 1×: its raw `48 * rate` formula gives
12 seconds at 0.25×, contradicting the existing 48-second unfinished startup
requirement. The upper cap also means 4× startup cannot advertise the old
192-second rate-scaled inventory all at once. Select a served prefix within
`R + S`, and reconcile the existing initial-readiness calculation with that
prefix. Preserve at least `3T`; do not retain a readiness check that requires
192 seconds while a new hold prevents ever producing it.

`S = T` is a server presentation contract, not an invitation to treat any
arbitrary HLS segment as strictly bounded by its integer header. Preserve
this route's existing validation of actual segment durations. Reuse actual
EXTINF/end times for accounting; never assume every segment is 16 seconds.

The steady-state window inequality is:

```text
F <= C + R + S
R <= W - G - S - B
therefore F - W <= C - G - B
```

The final start-segment check remains necessary: segment rounding and the
independent retention request can alter the selected first segment. During
startup, clamp the protected position to the presentation origin. Test both
the initial `3T` minimum and the mature window; a 180-second maximum window
does not by itself prove the startup minimum.

### 3.2 Minimal additional state

Add fields to the existing clock as needed for:

- Current producer attempt and accepted demand sequence used for the budget.
- Normalized settled position, its actor acceptance time, applicable rate and
  render/demand state, and the last accounted active instant.
- Reserve, cumulative desired end and carried surplus. Prefer deriving
  surplus from frontier minus desired end over storing redundant mutable
  totals that can diverge.
- A fixed legacy bootstrap anchor/time, cleared once on entry into explicit
  mode or replacement, not on every media fetch.

The existing served/staged endpoints, target and hard-deadline fields remain
in place. Add a single source of monotonic `now` to the policy calculation;
pass it through the existing code rather than inventing a clock service.

`RollingLeaseSnapshot` does not currently expose an accepted-demand sequence
or acceptance timestamp suitable for this check. Add internal snapshot
fields sourced from actor acceptance. Use `Option` for the pre-explicit
state; do not invent sequence zero as a new wire convention. Only a newly
accepted control updates the timestamp. Media, playlist and status requests,
or replayed sequences, do not refresh demand age.

This is distinct from lease renewal: media responses can keep a lease alive
after explicit observations stop. Fable's assertion that the lease must
expire when the last demand reaches 30 seconds does not hold on this path.
Freeze extrapolation at the demand horizon even when the lease stays live.

### 3.3 Cumulative calculation

Normalize attempt-relative endpoints using the actual media origin before
comparing with movie position. Do not mix a post-seek zero-based segment end
with an absolute client position.

```text
C(now) = latest settled position
         + accepted-rate integral over fresh, eligible active time
D(now) = C(now) + R(rate)
surplus = max(0, F - D(now))
```

Advance the integral only for Active/rendering time. Hold, known buffering,
End and stale observations contribute no new consumption. At a fresh accepted
observation, replace the old estimate with the reported settled position;
do not add it to already projected consumption. Rate changes split intervals
at acceptance. Replayed controls create no credit.

For regular publication, choose the first completed segment end at or after
`D(now)` when available, subject to `F_new <= D(now) + S`; otherwise choose the
available safe completed prefix. Do not expose extra completed segments just
because they exist. Keep surplus across publication and heartbeats. A settled
seek or replacement rebases once and invalidates old-attempt calculations.

Example at 1× with six-second segments and 48 seconds initially served:

| Target wall time | Desired end | Selected end | Added media |
|---|---|---|---|
| 16 s | 64 s | 66 s | 18 s |
| 32 s | 80 s | 84 s | 18 s |
| 48 s | 96 s | 96 s | 12 s |

The 48 seconds added across three cycles equal elapsed consumption. Checking
`now >= next_publish_at` while granting a fresh rounded 18 seconds every
cycle still fails and must have a negative-control test.

## 4. Publication selection and actor validation

### 4.1 Timing and selected prefix

Use `now >= next_publish_at` for ordinary eligibility. Keep `T/2` only as the
lower bound for early publication when fresh reported buffer is inside `G`
of the current position, and only within the same cumulative budget. Early
publication must not reset the consumption accounting.

Keep the `1.5T` deadline and record availability when the snapshot actually
becomes readable. A timer firing late does not authorize publishing all
staged media. Recompute the ordinary next target from actual availability;
retain cumulative media accounting independently of that timing reset.

`served_live_playlist(raw, Some(first_segment), ...)` currently copies the
whole writer tail. Add bounded-end selection to the existing renderer or
prepare a prefix before calling it. Preserve initialization/discontinuity
metadata, sequence numbers, subtitle alignment, and the existing playlist
header contracts. The observed produced endpoint remains the full physical
endpoint; published endpoint is the selected prefix. Never repurpose one
field to mean the other.

### 4.2 Extend the existing internal observation

Add these fields, using existing coordinate types:

```rust
// Proposed additions to RollingPublicationObservation; internal only.
pub demand_sequence: Option<u64>,
pub published_first_segment: Option<i64>,
pub published_start_ms: Option<i64>,
```

Update all callers and test literals in one compiler pass. A non-publication
observation must remain distinguishable from an actual served snapshot; do
not use missing boundaries to bypass validation on a real publication.

At actor acceptance, preserve all existing attempt/expiry/failure/completion
checks and additionally reject:

1. A candidate whose demand sequence differs from the current accepted one,
   including Legacy-to-Explicit cutover during construction.
2. A candidate whose advertised first segment passes the segment covering
   the protected position after normalization.

Use the same pure position-estimation helper in the worker and actor to
avoid diverging definitions. The actor need not own a second budget to
recompute a protected position from its own demand facts. No new public
control protocol or client capability is required.

Protect the segment covering `max(origin, C(now) - G - B)`. For explicit
non-rendering or stale state, use the frozen estimate; do not keep projecting
a paused viewer forward. Preserve the source of all coordinates in the
observation so the actor can validate independently of a caller-supplied
"safe" boolean.

### 4.3 Apply protection after every prefix-removal request

Build the final first-segment candidate from the normal `F_new - W` floor
and the download-frontier retention request, then validate the combined
result. Physical production end must no longer be the window anchor.

If retention requests a newer first segment than protection allows, defer
that logical removal while the required objects exist and reservations allow
it. Do not retire an otherwise healthy session merely because a player fetched
far ahead. Existing storage limits still win if retaining the media is not
possible; never advertise files already removed. Exercise the actual cleanup
path so this is not just a safe-looking manifest over deleted objects.

## 5. Exceptional states without new gates or a new scheduler

### 5.1 Keep `evaluate_flow` for the first cut

Preserve its existing demand and byte/global precedence and staged-inventory
hold. Once publication consumes only earned inventory, that hold can finally
keep a fast producer stopped. Do not implement the predictive run-budget /
resume-by scheduler from §9.2 unless the focused throughput test demonstrates
that the simpler loop misses a deadline.

The staged threshold is rate-scaled, not always 16 seconds:
`rolling_publication_batch_ms(rolling_playback_rate(demand))`. It is 64 seconds
at 4×. State the bound as the rate-scaled threshold plus a segment/in-flight
allowance; do not promise the review's fixed 16-second staged tail at all
rates. Preserve the existing byte accounting for every staged segment.

Test a 1.2× producer at 1× playback for 30 simulated minutes. Short intentional
holds can bias the current speed estimate downward. If that test falsely
classifies insufficient capacity, exclude held time from that estimate in
this repair; otherwise leave the estimator alone. No speculative scheduler
rewrite or global change to producer timeouts.

### 5.2 Low rates, pause and deadline floor

At the next target, a live unfinished playlist still needs a new segment even
when rounding or low consumption leaves less than one segment of ordinary
credit. Publish the minimum required next segment only if its resulting
frontier and protected-start check are safe. Treat its excess as carried
surplus, not a replenishable grant:

```text
floor_candidate_end - C(now) <= W - S - G - B  // 124 s here
```

Do not wait forever for a full `rate*T` batch, silently violate the deadline,
or append fake ENDLIST. If the floor segment cannot fit, preserve existing
objects and use the existing rolling failure/retirement and position-based
recovery machinery. This is a runtime inability to continue that presentation,
not a disabled feature or eligibility gate.

Keep the existing 180-second pause grace as an upper bound; capacity/window
safety can require earlier retirement. Repeated Hold/reloads do not reset
it. Resume keeps the user's settled position; a held client remains paused.
Do not repeatedly reopen while Hold persists.

Reuse the established lifecycle instead of adding a new protocol. Actual
pause expiry retains `PauseExpired`. An active low-rate window exhaustion
must not be mislabeled as an elapsed user pause: use the existing publication
failure path with an internal `rolling_window_budget_exhausted` reason and
prove the existing active client recovery preserves position. If the current
error classification cannot do that, make only the minimal classification
correction and test its clients/relays; do not add an enablement flag.

At regular target-time updates the worst-case sustainable rate is `S/T`;
the absolute deadline-limited floor is `S/(1.5T)`. Do not confuse the two.
Low-rate/long-segment combinations remain usable through bounded recovery;
there is no software check that disables the playback-rate control.

### 5.3 Legacy remains available with bounded publication

Do not adopt the review's assumed 90-second client buffer ceiling. The
comment above `CLIENT_FORWARD_FETCH_SECS` records a physical iPad fetching
about 120 seconds despite a 60-second preference. That preference is not a
hard cap. `48 + 120 + 16 + 10` already exceeds the 180-second window.

Keep the Legacy route available. Until the first accepted explicit report,
use the existing 1× compatibility rate with a fixed bootstrap origin/time
and both bounds:

```text
legacy desired end = origin + elapsed legacy wall time + 3T
legacy served end <= legacy desired end + S
legacy served end <= resolved fetched end + 3T
```

Before any resolved fetch, use the origin for the second bound, permitting
the initial 48 seconds. Downloads cannot reset the bootstrap clock or turn
1× compatibility publication into 2×. On Explicit cutover, rebase once on
accepted position and apply explicit state/freshness rules.

This is a compatibility assumption of normal 1× playback, not proof of an
unreported legacy viewer's position. Legacy cannot infer pause or a different
rate from fetches. Preserve its existing finite no-activity lease and known
resume behavior; do not claim new guarantees for an indefinitely fetching,
unreported paused client. All shipped clients use explicit control. Test
legacy bootstrap, paced fetches, a rapid-fetch burst and explicit cutover;
no client-buffer-size assumption or route-disable switch is required.

### 5.4 EOF, replacement and failure

A completed source may publish its remaining ordered tail plus ENDLIST, but
must preserve the protected start even if the final window must retain more
history than the ordinary rolling selection. Use already admitted objects
and reservations; do not trim directly to the last 180 seconds or strand
intermediate staged segments. A finished short source retains its existing
startup exception.

Replacement, settled seek, terminal failure and expiry invalidate old budget
state with the existing attempt identity. Preserve object grace and resource
settlement. Holding the producer is not evidence of failure, and repeatedly
resuming it cannot erase an already won deadline.

## 6. Small, useful diagnostics

Add to the existing publication/flow event and server Activity/status data:
`budget_anchor_sequence`, `allowed_end_ms`, `carried_surplus_ms`, and demand
observation age. Reuse existing fields for rate, position, produced end,
published end, first segment, target, deadline and hold reason.

All endpoints must be clearly normalized or labeled attempt-relative. Do
not log bearer session credentials or real media paths in committed evidence.

**How to read it:** at 2× production / 1× playback, lead oscillates within
`R + S`, surplus is carried and production periodically holds. Increasing
lead on every publication remains a failure even if all responses are 200
and the explicit lease stays active.

## 7. Focused proof — one regression set and one native confirmation

### 7.1 Write the failing sustained regression first

Drive real `publication_cycle`, actor observation acceptance and served
snapshots; use synthetic segment metadata and controllable monotonic time.
The production method currently uses `std::time::Instant::now()`, so merely
pausing Tokio time does not make it a deterministic hour. Extract an internal
`publication_cycle_at` or pass `now` into its policy core, while leaving the
ordinary entry point to supply real time. Do not wait an actual hour or test
only a separate simulation of the intended formula.

Group the new tests under an identifiable prefix such as
`rolling_publication_budget`. The name is proposed, not an existing command
that already proves anything.

| Test | Required result |
|---|---|
| 2× writer, 1× playback, 6 s segments, T=16, one simulated hour | Existing code fails sustained lead/window assertions; repaired code keeps `F-C <= R+S` and the protected segment in every snapshot |
| Target-only negative control | Fresh independently rounded 18 s per 16 s accumulates surplus; test rejects it without hard-coding a guessed failure time |
| 16 s and variable segment durations | Real duration accounting, bounded surplus and no skipped advertised segment |
| Writer burst / delayed poll | Only selected prefix appears; all extra staged media remains charged |
| 1.2× writer, 1× playback, 30 simulated minutes | No false insufficient-capacity retirement; publication meets deadlines |
| Rates 0.25× / 1× / 2× / 4× and changes | Parameter inequality, startup `3T`, high-rate reserve cap and expected bounded low-rate recovery all hold |
| Pause, buffering, missing explicit reports with continued HTTP | No invented consumption or freshness renewal; grace cannot reset; no paused reopen storm |
| Seek with nonzero origin, stale sequence and replacement | One correct rebase; old candidate rejected without budget/snapshot mutation |
| Download-frontier retention ahead of playback | Protected prefix remains available; cleanup cannot remove it behind the manifest |
| EOF with slow viewer | Final snapshot preserves current position and all remaining ordered segments |
| Legacy bootstrap and rapid fetch | Fetch cannot make publication outrun the fixed compatibility clock; cutover is correctly fenced |

Retain the existing timing, object-grace and attempt-fencing regressions.
Do not count repeated retirement/reopen as successful uninterrupted 1×
playback. Reuse fixtures; do not invent a new test framework.

### 7.2 Compiler loop before push

Use Rust 1.97.1 on the checkout host or the source-only loop in
[AGENT-COMPILE-LOOP.md](../ci/AGENT-COMPILE-LOOP.md). Never use CI to discover
compiler errors. Do not broaden into unrelated full suites after these checks
pass unless changed code or an observed failure requires it.

```bash
# Verify the pin before Rust edits.
rustup run 1.97.1 rustc --version

# Run the focused regressions after adding the named prefix.
rustup run 1.97.1 cargo test -p plurxd --bin plurxd rolling_publication_budget
rustup run 1.97.1 cargo test -p plurxd --bin plurxd mkv_hls_schedule

# Compile/lint the affected package and check formatting.
rustup run 1.97.1 cargo check -p plurxd --all-targets
rustup run 1.97.1 cargo clippy -p plurxd --all-targets -- -D warnings
rustup run 1.97.1 cargo fmt --all -- --check

# Check documentation registration and patch hygiene.
python3 -m unittest discover -s tests/operations -p test_docs_index.py
git diff --check
```

Record actual test names/counts, compiler version and candidate SHA. Check
that filters selected tests rather than reporting a misleading zero-test
success. Recheck the exact source after moving to a new base.

### 7.3 One actual-server native run, then physical-TV confirmation

The retained isolated replay is supporting evidence; it does not run plurxd.
Do not repeat it as a substitute for testing the repaired server. Paul's Mac
holds its untracked scripts and logs under
`Claude outputs/apple-tv-skip-2026-09-20/replay/`; those paths are not portable
repository prerequisites, and raw logs include private library paths.

Against the patched server, run the controlled 2× writer / 1× AVPlayer
scenario for ten minutes, crossing multiple 180-second windows. Capture
actual served manifests, request sequence, accepted demand, publication
frontiers and one-second player samples. For steady rendering outside
explicit seeks, require:

```text
abs(delta_position - accepted_rate * delta_wall) <= 1 second per sample
unexpected player stall count == 0
unexplained segment-request gaps == 0
published lead and protected start satisfy the server invariants
```

Mark startup, authorized seeks/rate changes and intentionally tested recovery
separately; do not discard samples around an unexplained jump to make the
metric pass. Request gaps supplement continuity; they do not measure all
rendered-content loss. If content loss is quantified, use frame/sample IDs
or packet-to-render correlation rather than assuming a buffer flush.

Confirm the repaired candidate on a physical Apple TV forced through the
same rolling route for one comparable ten-minute session. Record source
alias, file ID, tvOS/app/server versions and attempts. This is a targeted
release confirmation, not a new multi-device qualification campaign or a
software activation gate. Live TV remains a separate diagnosis; do not
promise this movie-path fix resolves it.

## 8. One PR using the current fast lane

Use one implementation branch and one main-bound draft PR. A single cohesive
fix does not require task branches plus an integration train. If Sol chooses
an `effort/` name, it is still one branch and one PR for this repair, not
permission to split it into a multi-task project.

The correction at the top of
[DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) supersedes that document's
older automatic full-promotion instructions. The checked-in
[main-fast-lane.yml](../../.github/workflows/main-fast-lane.yml) confirms:

1. Compile locally and obtain the old-fails/new-passes focused evidence.
2. Open a draft PR. Draft PRs allocate no lane jobs.
3. Obtain exactly one adversarial review of the implementation, then address
   concrete findings. Fable's design review is not that code review.
4. Mark ready to start the fast lane. No `fast-lane` label is needed.
5. Merge only after the current candidate's required fast-lane checks pass.
   A base change requires checking the actual updated candidate, not citing
   an older run.
6. Report merge separately from release: merging neither builds nor deploys
   an image. Use the established release path for the qualified candidate;
   do not create a second deployment workflow.

No full CI dispatch, full platform matrix, repeated adversarial-review loop,
or manual promotion receipt is added to this repair's development lane.
Full CI remains manual or release-triggered under the current policy. The
focused playback proof remains required because the fast Rust job compiles
all targets without executing this regression for the author.

## 9. Completion checklist and scope stop

- [ ] The sustained regression fails on the original behavior.
- [ ] Budget, selected prefix, actor validation and retention protection are
  implemented in the existing path, including startup and explicit cutover.
- [ ] Focused tests and pinned compiler/lint/format checks pass.
- [ ] Draft PR received one adversarial implementation review; findings fixed.
- [ ] Current fast lane passes and the one PR merges.
- [ ] Actual-server native continuity proof and physical-TV confirmation are
  recorded for the release candidate, with deployment status stated honestly.

Stop there. Do not add a predictive scheduler, feature flags, settings page,
client watchdog, codec rewrite, index-backlog repair or Live TV overhaul
without a concrete failure making that work necessary. If a required check
cannot run, record exactly which evidence is missing rather than silently
claiming release qualification or expanding the project.

## 10. Disposition of Fable's review

| Finding | Decision used by this handoff |
|---|---|
| F1 severity / metrics | Correct the 12-second-total-loss claim; use ~72-second position discontinuity and continuity as primary metric. Exact discarded/rendered duration is not established by loaded-range endpoints. |
| F2 implement existing §9.2 | Accepted; this is the missing cumulative implementation plus bounded integration decisions. |
| F3 clock owner / existing commit path | Accepted; `RollingPublicationClock` owns budget, actor validates the extended observation. No new commit protocol. |
| F4 floor / low rate | Accepted with target-vs-hard-deadline distinction and truthful active/paused retirement classification. No rate gate. |
| F5 constants | Use existing T/G/horizon and 124-second cap; add the necessary 3T lower bound and reconcile high-rate startup. Demand freshness is separate from media-renewed lease freshness. |
| F6 90-second legacy bound | Rejected as contradicted by recorded 120-second fetch lead; retain route with a fixed wall-time compatibility cap plus fetch cap. |
| F7 EOF | Accepted; preserve protected start and tail. Staged bound must scale with rate. |
| F8 evidence scope | Movies supported; physical tvOS confirmation remains; no Live TV or platform-exclusivity claim. |
| F9 intermediate-speed test | Accepted; fix hold-biased estimation only if this test shows it is needed. |
| F10 minimal first cut | Accepted; retain existing flow policy unless focused regression exposes a required small correction. |
| F11 hygiene | Use lab6 and reference title S; keep raw logs local/untracked and mark local replay paths explicitly. |

The only delivery for this task is this implementation handoff and the
corrected RCA/index. No Rust changes, branch, PR, review request or deployment
was made while preparing it.
