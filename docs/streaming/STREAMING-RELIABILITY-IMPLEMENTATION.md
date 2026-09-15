# Streaming reliability — execution plan and integration contract

**Status:** ready for parallel implementation; no behavior shipped by this
plan · **Written:** 2026-09-15 · **Base:** c2216ae75cb2a6f86efabaa4ebc2169a231ecc50
· **Integration branch:** effort/streaming-reliability

Companion to [PLAYBACK.md](../PLAYBACK.md) (the delivery inventory),
[the protocol plan](../playback-control/PLAYBACK-CONTROL-PROTOCOL-PLAN.md)
(the existing control architecture), and
[the development pipeline](../DEVELOPMENT_PIPELINE.md) (commits, task PRs and
the current fast lane). This document controls this effort where historical VOD
cutover prose conflicts with deployed code. Read it before either
[shared-index handoff](STREAMING-SHARED-INDEX-HANDOFF.md) or
[web-recovery handoff](STREAMING-WEB-RECOVERY-HANDOFF.md).

## 1. Outcome — stable playback at the highest supported quality

Repair the demonstrated path without creating another open-ended playback
rewrite. The deliverable is truthful stall evidence, recovery that preserves
quality unless a measured constraint requires changing it, shared preparation
for the actual Dolby Vision recipe, and a valid first-play fallback while
cold VOD preparation remains necessary.

The owner of this task keeps the difficult protocol, pacing, preparation
lifecycle and integration work. Two Sol tasks implement bounded, disjoint
packages. Work in two waves; target one reviewable implementation PR per lane
per wave, at most six planned task PRs plus the final promotion PR. Split only
when a concrete compile or integration dependency requires it, not per helper
function. This is a scope budget, not a reason to combine unrelated changes.

### 1.1 User constraints are binding

- No runtime eligibility switches based on test receipts, device qualification,
  sample counts, benchmark thresholds or an empirical allowlist. Measurements
  inform diagnostics and automatic quality selection; they do not grant
  permission to use a feature. Real format support, authentication, source
  identity, available resources and actual decoder capabilities still describe
  what can execute; report an actual failure honestly.
- Add no rollout flag by default. If an enable/disable control is necessary,
  put it only in Developer settings with requirements and their observed state.
  Requirements are explanatory, not an independent activation veto. Do not
  silently change existing production settings as part of implementation.
- Retain newly imported media playback. Do not remove growing HLS merely to
  complete this effort or turn an index miss into mandatory transcoding.
- Follow the September 13 correction at the top of the development pipeline.
  Task PRs allocate no automatic jobs; effort compiler checks and evidence
  workflows are manual. Keep affected compilation, lint and focused proof
  blocking locally. Repeat only after relevant edits, a failure, or a changed
  integration base. Full CI is a separate manual/release sweep, not a task or
  automatic promotion requirement.
- Make exactly one adversarial agent review of the final main-bound PR, once
  its diff is integrated and ordinary checks are ready. Fix actionable findings
  and verify those fixes directly; do not start another review cycle.
- Production restarts, index backfills, cold-cache manipulation and physical
  playback experiments must not interrupt an active viewer. Local fixtures
  are the default development surface.

### 1.2 What the incident proves, and what it does not

Read-only September 15 telemetry for TRON: Ares, file 5418, on nynuc:

| UTC | Recorded fact |
|---|---|
| 14:40:56 | Copy VOD refused with vod_index_pending; growing HLS started |
| 14:43:22 | The served playlist began sliding after retention |
| 14:44:16 | Reported position changed from 193276 to 188314 ms and stopped |
| 14:44:24 | Persistent stall recorded with 18310 ms of loaded runway |
| 14:44:36 | Automatic recovery chose transcode |
| 14:44:41 | Replacement presented 1080p after original 2160p playback |

The accepted observation said decoder ready, with one dropped frame; the
persistent-wait callback synthesized decoder failed. At the stall, publication
was 214673 ms, target 49 seconds, release threshold 24 seconds and publication
lead 26 seconds. This proves a held producer and a stationary presentation;
it does not prove that resuming production would have fixed Safari.

The current shared resolver rejects preserved Dolby Vision before hydration;
local indexing supports stripped, preserved and converted identities, but the
needed local row was missing and the local indexing cadence was disabled.
An older shared artifact existed. Existence of an artifact is not proof it
matches the selected recipe. Safari's exact internal initiating fault remains
unproven. Do not encode that hypothesis as a decoder rule or a regression
claim. Use synthetic values from this table; do not commit production logs,
capabilities, credentials or complete telemetry databases.

## 2. Fixed contracts — implement these before adding vocabulary

### 2.1 Observation, inference, action and success remain distinct

The current v1 request already contains demand, position_ms,
buffered_from_ms, buffered_through_ms, render_state, playback_rate,
observation and selection. Keep that wire shape for Wave 1. The protocol
uses strict validation; inventing fields without negotiation breaks old
servers. Current decoder states are unknown, ready, starved and failed.

| Evidence | Meaning | Permitted response |
|---|---|---|
| No presentation progress, loaded contiguous range, no media error | Presentation wait; cause unknown | Bounded native recovery or same-recipe replacement |
| Missing/empty loaded range | Supply observation; cause not yet attributed | Compare publication, transfer and producer progress |
| A typed decoder error or existing sustained lost-frame evidence | Decoder pressure/failure, with source attribution | Existing compatible-recipe planning may adapt |
| Sustained transfer capacity insufficient for delivered media | Network pressure | Existing quality planner selects feasible output |
| Producer cannot meet demand while actually running | Producer pressure | Placement/recipe correction; do not label network failure |
| Successful control response or buffered bytes | Intermediate progress | Never alone completes presentation recovery |

Use the existing presentation-progress observer when available, and its
explicit position fallback where frames are unavailable. Do not substitute
readyState or total decoded-ahead frames for proof of displayed progress.
Native-player unknowns stay unknown. Do not require decoder_state failed to
let an active, explicitly stalled player recover from a loaded-media wait.

### 2.2 One recovery episode, existing owners

The client attachment controller owns play, seek and attachment mutation.
The server actor owns producer flow and server-side resource/replacement work.
The protocol coordinates them; neither owner impersonates the other.

An episode captures attachment/generation/epoch, intent generation, first
observation time, original position, immutable evidence snapshot, action and
outcome. Reuse existing counters and budgets. Multiple observers update one
episode; they do not mint fresh deadlines. A new seek, pause, close or
supersession cancels obsolete work. An old callback cannot mutate a successor.

A recovery action completes only on new presentation progress at the intended
position, or settles as cancelled/failed. Server hold describes production;
it cannot prohibit local presentation repair. A passive none response means
no server action, not success and not an additional waiting allowance.
Retain the existing absolute 20-second episode bound for the first patch;
reduce avoidable delay through meaningful action completion, not a new timer.
Do not repeatedly spend the whole bound waiting for a passive answer.

Wave 2 may add bounded episode telemetry to existing log extra fields. Any
wire extension must first document old/new client-server combinations, field
limits and negotiation in this plan. No mandatory v2 migration is planned.

### 2.3 Recovery preserves the delivery contract

Snapshot video codec/profile, resolution, dynamic range, copy/encode mode,
audio selection, subtitle mode, sync offset, quality intent and film position.
For an unattributed presentation or delivery fault, preserve those properties.
Existing supported container/attachment repair may change transport without
changing the selected video recipe.

Do not send a server stall ticket that normalizes Auto one rung down when the
reason is a presentation repair. Preserve the predecessor, ownership fences
and supersession transaction; omitting a quality-degrade reason is not
permission to omit lifecycle identity. Genuine adaptation still uses the
existing planner and prepared replacement machinery. Manual quality intent
is respected, with clear failure when it cannot be delivered.

A recovery attempt cannot write a learned decode limit without qualifying
existing decode evidence. Temporary capacity limitations must retain their
scope and expiry. Reuse existing upward re-evaluation and cooldown where
correct; change demonstrated gaps rather than introducing a second ABR loop.

### 2.4 An immutable plan and produced bytes are different prerequisites

Copy VOD needs an accurate whole-film fragment plan before attachment; media
segments can then be materialized on demand. Encoded VOD uses its defined
output grid and does not need the copy index. Missing preparation must not
select an encode automatically merely to make VOD available.

A canonical recipe identity must distinguish source bytes, source version,
FFmpeg engine identity, copy arguments and output-affecting Rust transforms,
including DV preservation/conversion and transform revision. Ordinary keys
that still name identical output should remain reusable. Two recipes that
produce different fragment sizes or init data must never share an artifact.
The existing fragment-index jobs, leases, source fences, blob checks and
hydration remain the mechanism; do not build a second preparation queue.

### 2.5 First play retains a real usable path

~~~text
selected recipe
    |
    +-- exact plan available locally or on a peer --> immutable VOD
    |
    +-- plan unavailable --> schedule/coalesce exact preparation
                                 |
                                 +--> valid rolling playback for this watch
                                 +--> later watches use the completed VOD plan
~~~

No forced mid-watch rolling-to-VOD migration is required. It would create a
new attachment interruption merely to improve an architectural statistic.
Background indexing disabled must not masquerade as an active preparation
job. Distinguish periodic scheduling from explicit foreground demand; the
coordinator owns that settings semantics correction and its documentation.
Existing explicit administrative disable intent is not silently overwritten.

While the fallback remains, choose its playlist semantics at creation. A
playlist whose retention removes entries must be typeless/sliding from its
first response; do not mutate an EVENT promise into live semantics. Preserve
film-time mapping, sequence/discontinuity rules, accurate target duration,
retention and current start position. This corrects the fallback's contract;
it is not proof that every native-player stall disappears.

### 2.6 Playback resources take priority without disabling work

Use existing admission and scheduling priorities. Foreground segment demand
outranks indexing, downloads and speculative work. Work can wait for actual
capacity; a missing benchmark is not a capacity failure. Bound background
concurrency, coalesce work, and test cancellation and lease loss. No new
resource scheduler or fleet-wide redesign belongs in this effort.

## 3. Ownership — three active tasks, disjoint first-wave files

| Lane | Owner | First-wave files | Exclusions |
|---|---|---|---|
| C: protocol and integration | This task | playback_control.rs, transcode.rs; HTTP settings and core legacy-setting documentation; protocol-focused tests; this plan, docs index and live references | No edits in Sol-owned files until handoff |
| I: shared exact-recipe indexes | Sol task 1 | fragindex.rs, fragment_index_cluster.rs, state.rs, vodserve.rs; core store/fragment_index_cluster.rs if needed; embedded tests; shared-index handoff | No web/client or transcode.rs edits; no migrations without a concrete need |
| W: web recovery and quality | Sol task 2 | web/index.html, web/playback-policy.js, web/playback-control.js; web-control.test.js, web-policy.test.js; web-recovery handoff | No server files or native clients |

All daemon paths above are beneath crates/plurxd/src; tests are beneath
[tests/playback](../../tests/playback). The core store path is beneath
crates/plurx-core/src. Re-verify symbols at the branch base before editing.
The coordinator owns shared validation catalog and root documentation changes;
workers send the required mappings before their PR is considered complete.
Each worker owns its unique per-commit regressions.d fragment; central catalog
changes remain with the coordinator.
Worker-owned handoff documents record behavior in their implementation commit.
The coordinator must apply accompanying live-reference updates before merging
the task PR, without sweeping unrelated documentation into it.

Wave 2 transfers state.rs/vodserve.rs to C after I merges. I then owns the
cold-preparation measurement/harness package, while W owns native client
recovery parity and targeted client tests. Record each transfer in §8 before
editing. Do not concurrently edit a shared file and rely on merging it later.

## 4. Work packages — two implementation waves

### 4.1 Wave 1 can run in parallel now

**C1 — truthful server control and valid rolling presentation.** Verify the
incident predicate against current code. Make explicit stalled presentation
with loaded media independent of an invented decoder failure. Preserve
permanent producer decisions and intentional pause behavior. Correct the
creation-time playlist shape for retention-enabled fallback. Add focused
contract regressions for the incident values, pause, starvation, and native
recovery eligibility. No change to production settings or buffer constants.

**I1 — exact shared DV preparation.** Execute the shared-index handoff. Prove
recipe isolation, conversion metadata round-trip, peer hydration and exact
job deduplication. Remove the broad DV rejection only when the real artifact
can be produced, verified and consumed. Do not merely delete the refusal.

**W1 — truthful web evidence and recipe-preserving recovery.** Execute the
web-recovery handoff. Remove the wait-to-decoder-failed inference; recover
unattributed stalls without downgrading; settle recovery only on progress.
Keep current budgets, generation fencing and real decoder fallback.

**Wave acceptance:** three task PRs compile and pass their focused regressions;
C1+W1 together recover the synthetic loaded wait without asserting decoder
failure or lowering quality. I1 creates and hydrates the converting recipe.
No requirement to complete a library-wide backfill.

### 4.2 Wave 2 integrates the first-play experience and native parity

**C2 — preparation demand, joined outcomes and quality ownership.** After I1,
connect missing exact plans requested by playback to the existing queue;
retain ingestion/discovery scheduling and separate requested work from periodic cadence; expose bounded queued,
running, ready, failed and unavailable reasons through existing status
surfaces. Reuse cancellation, source/recipe fences and job deduplication.
Ensure repeated play requests cannot create duplicate full-file passes.
Audit existing adaptation consumers for false decoder/throughput attribution
and repair demonstrated violations. Use one episode ledger and actual
presentation acknowledgement. Add no receipt-based activation checks.

**I2 — finite first-play and resource measurement.** Extend an existing harness
or one small script to distinguish source scan/index time, peer hydration,
session creation and first frame. Use fixtures first. One nominated idle
production window measures representative SDR, HDR10 and P7-converted remuxes
with sizes and cache state recorded; do not flush the production NAS cache.
Test foreground competition using existing admission seams. Report cached or
uncontrolled cache state honestly instead of claiming a cold measurement.

**W2 — Apple and Android parity.** Port the same evidence/action distinction
through the existing attachment owners. Preserve native media errors and
prepared successors; do not clone the web timer algorithm. Prove same-recipe
repair, intent cancellation, observed-progress completion and no learned
quality restriction from an unknown stall. Update client behavior references
and required build counters using the existing release convention.

**Wave acceptance:** missing-index first play remains playable; next-play VOD
uses the exact completed artifact; observations explain the path and quality;
all three clients preserve quality on an unattributed recovery. Physical
coverage is recorded separately from simulator/unit evidence.

## 5. Retirement is a bounded decision, not an endless prerequisite

At the end of Wave 2, record one decision using measured preparation latency,
recipe coverage and the requirement for prompt original-quality first play:

- Keep the corrected fallback when whole-file indexing still imposes an
  unacceptable first-play wait. Close this effort with that limit stated.
- Retire it only if an agreed usable first-play path already exists for the
  supported catalog. Do not infer agreement to mandatory waiting from agreement
  with the architectural destination.

A faster exact-plan algorithm gets one bounded feasibility spike only if the
measurements show it is needed. Maximum: one working session and a written
feasibility result, not an additional rewrite inside this effort. Container
cues, estimated durations and partial indexes are not accepted as immutable
copy plans without proof of the actual sample/segment contract. A partial
playlist is not a complete immutable playlist.

No runtime feature is gated on this decision record or qualification data.
This section governs what source we choose to ship and the supported product
behavior, not whether an installed user may enable it.

## 6. Validation — finite tests, quality assertions and one review

### 6.1 Per-task proof

Before Rust edits, verify the pinned compiler and establish the local loop.
On this Mac the shell default is Homebrew Rust 1.98; use rustup run 1.97.1.
Each worktree needs a separate target directory; do not let concurrent tasks
serialize behind one Cargo lock or overwrite a source-only extraction.
Limit simultaneous heavy builds to available memory; parallel reasoning and
editing remain useful even when compiles need staggering.

~~~bash
rustup run 1.97.1 rustc --version
rustup run 1.97.1 cargo check -p plurxd --all-targets --locked
rustup run 1.97.1 cargo fmt --all -- --check
rustup run 1.97.1 cargo clippy -p plurxd --all-targets --locked -- -D warnings
node --test tests/playback/web-policy.test.js tests/playback/web-control.test.js
python3 -m unittest discover -s tests/operations -p test_docs_index.py
~~~

Run only the commands appropriate to the task, plus its named focused Rust
or native test. These are not instructions to run every command on every
edit. Record exact commands and exit status in the PR. A named test filter
must actually execute tests, not pass with zero selected. Source-only cloud
compilation remains available via [the compile loop](../ci/AGENT-COMPILE-LOOP.md)
if the pinned local compiler cannot build the relevant target.

### 6.2 Acceptance matrix

| Case | Required assertion |
|---|---|
| Incident replay | Active, loaded, stationary, no decoder error: one bounded repair; no quality loss |
| Actual decoder failure | Correctly attributed failure still reaches compatible recovery |
| Supply loss | Published/fetched/loaded facts identify the boundary; no fabricated decoder limit |
| Pause/seek/close during recovery | Obsolete work cancels; no successor mutates current intent |
| Delayed or rate-limited control | Loaded playback continues; no duplicate action or renewed episode budget |
| DV index on another node | Correct recipe hydrates and opens copy VOD without stripping DV |
| Old/mismatched/corrupt index | Never reinterpreted as another recipe; exact repair is coalesced |
| Index absent, periodic indexing off | Explicit status and a usable first play; no automatic transcode |
| Retention across 3+ minutes | One playlist semantic contract and stable film-time mapping |
| Cache eviction, far seek, refill | No advertised timeline mutation; bounded materialization |
| Background competition | Existing foreground priority is enforced; resources settle on cancel |
| Owner transition | Existing fences and prepared handoff survive; no double active owner |
| Temporary quality adaptation | Reason identifies real pressure; higher quality can be reconsidered |

Physical sweep: one representative long copy-VOD watch through retention/
eviction pressure, one fallback watch beyond the incident window, and focused
seek/pause/recovery operations on Safari, Apple and Android. Record actual
transport and recipe, interruption and quality, not just a green player.
Do not repeat the whole sweep for prose changes. Do not claim physical proof
from mocked HTMLMediaElement or simulator-only tests.

### 6.3 Promotion and adversarial review

Every implementation lane commits normally and opens PRs into the effort.
The coordinator merges only after inspecting the diff and recorded affected
compilation, lint and focused proof on the task candidate. Task PRs allocate
no automatic jobs. The effort workflow remains available for a concrete
compiler diagnostic need; do not dispatch it to repeat successful local work.
Routine diff inspection is not another adversarial-agent campaign.

Freeze task merges, integrate current main, and open the final PR into main
as draft. Dispatch exactly one adversarial agent when that integrated PR is
otherwise ready to merge. Focus on false quality loss, lifecycle races, recipe
identity, missing-index first play, and software gating. Fix valid findings
and verify the affected behavior directly. Do not request another review.
Then mark the PR ready and require the current-candidate main fast lane to
pass before merging. A changed candidate needs fresh affected evidence and
the fast lane again, not another adversarial campaign.

The September 13 correction in DEVELOPMENT_PIPELINE.md and
[main-fast-lane.yml](../../.github/workflows/main-fast-lane.yml) supersede the
older automatic Effort development gate / Main promotion gate / full-suite
receipt descriptions. Do not restore them in this effort. Full CI and runtime
sweeps remain separate manual or release work. No force push to main, bypassed
hooks, skipped current blocking checks or direct implementation commits to
main. Merging does not authorize deployment or an image build.

## 7. Launch and integration procedure

Merge the documentation foundation PR into effort/streaming-reliability. Create two
user-visible Sol tasks with separate worktrees. Each fetches the effort,
creates its own codex branch from that exact commit, reads AGENTS.md and its
handoff, verifies the base SHA, and begins its Wave 1 package. Do not use the
dirty original checkout as either task's starting source.

The coordinator remains in this conversation and its isolated effort
checkout. Before implementation it creates its own codex task branch from the
effort, so C1 also receives a PR. No worker merges the effort into main.
Workers return: branch, commit, PR URL, changed files, proof commands/results,
remaining risks and the exact ownership transfer needed for Wave 2. Communicate
contract conflicts immediately and continue independent work. Do not wait on
optional diagnostics or modify another lane's file to make progress.

Parallelism shortens independent implementation, not final integration,
physical testing or CI capacity. Do not promise a multiplier. Reassess once
after the first wave: if a lane is waiting on shared files, integrate that
handoff and reassign work rather than leaving two tasks polling each other.

## 8. Execution ledger — update outcomes, not aspirations

| Package | Owner | State | Evidence / next step |
|---|---|---|---|
| Plan foundation | Coordinator | merged | [PR #322](http://192.168.4.7:3000/noirr/plurx/pulls/322); commit 05298f1c4, merged as fea5d2451; four docs checks and tracked hook passed |
| C1 | Coordinator | merged | [PR #323](http://192.168.4.7:3000/noirr/plurx/pulls/323); runtime 00a70a7b; 11 focused tests passed |
| I1 | Sol 1 | merged | [PR #325](http://192.168.4.7:3000/noirr/plurx/pulls/325); runtime 2a2ec42f; seven focused identity, conversion and store tests passed |
| W1 | Sol 2 | merged | [PR #324](http://192.168.4.7:3000/noirr/plurx/pulls/324); head e906ba04; passive waits, transfer attribution and presented-frame corrections verified |
| C2 | Coordinator | merged | [PR #326](http://192.168.4.7:3000/noirr/plurx/pulls/326); runtime 298cc69d; four focused tests and combined web proof passed |
| I2 | Sol 1 | implementing | Existing playback-lab measurement extension; no production scan |
| W2 | Sol 2 | implementing | Native evidence/recovery parity from integrated current main |
| Final promotion | Coordinator | pending | One adversarial agent review; current-candidate main fast lane |
| Fallback retirement | Coordinator | retain | Exact copy plans still need complete source passes; preserve prompt first play and close the retirement question for this effort |

Record task IDs and PRs here as they become known. Never record a test pass
without its command and tested commit. Main documents that currently claim
VOD-only production must be reconciled before promotion.

### C1 compatibility note

The updated binary always serves rolling playlists with one typeless shape;
there is no per-session experiment flag. The legacy settings API field stays
accepted for older clients/peers and reports the actual local contract as true.
Remote start v1 does not acknowledge playlist shape, so the existing durable
peer recipe retains its conservative legacy-setting guarantee. Do not assert
the new ingress's local behavior for an older worker. C2's joined-outcome work
should retain or improve that attribution without an unnegotiated wire field.

### C1 focused evidence

Runtime commit 00a70a7b: pinned Rust 1.97.1 daemon all-target check and test
compilation passed. The tracked hook passed catalog lint, workspace formatting,
workspace all-target Clippy with denied warnings, and embedded JavaScript
syntax. The four documentation-index checks passed. No full unit suite ran.

Build the test binary with:

~~~bash
CARGO_TARGET_DIR=/private/tmp/plurx-streaming-target-coordinator \
  rustup run 1.97.1 cargo test -p plurxd --bin plurxd --locked --no-run
~~~

The resulting plurxd-6bd69bb492009d17 test executable was invoked with
`--exact --test-threads=2` and the following full filters. Result: 11 passed,
0 failed, 2237 filtered out in 1.41 seconds. Compilation time is separate.

~~~text
playback_control::tests::loaded_stall_recovery_does_not_require_decoder_failure
playback_control::tests::the_serving_predicate_requires_an_active_stall_and_a_recoverable_boundary
playback_control::tests::lifecycle_refill_loaded_native_wait_outranks_a_producer_hold
playback_control::tests::a_producer_decision_still_outranks_the_predicate
transcode::tests::served_live_playlist_advances_past_the_pruned_prefix
transcode::tests::typeless_sliding_playlist_keeps_one_shape_across_pruning
transcode::tests::rolling_playlist_refuses_an_inconsistent_writer_snapshot
transcode::tests::takeover_playlist_declares_one_monotone_discontinuity
transcode::tests::an_untouched_takeover_playlist_reports_its_epoch_floor
transcode::tests::retention_keeps_a_reload_margin_above_the_observed_fetch_lead
http::tests::seeded_write_surface
~~~

### C2 focused evidence

Runtime commit `298cc69d`: pinned Rust 1.97.1 daemon test compilation and
tracked workspace formatting/Clippy/JavaScript hook passed. Four documentation
index checks passed. The four focused tests below passed using the C1 build
command and its test executable with `--exact --test-threads=2`; the two
unchanged tests were not rerun after correcting only the new state fixture's
missing Dolby Vision metadata. No full unit suite ran.

~~~text
state::tests::playback_preparation_is_durable_exact_and_independent_of_discovery
vodserve::tests::first_play_queues_missing_source_preparation_with_discovery_off
vodserve::tests::a_converting_cluster_artifact_hydrates_without_a_local_v1_index
vodserve::tests::requests_the_vod_presentation_cannot_serve_fail_typed
~~~

The existing scheduler already consumes explicitly requested analysis with
periodic discovery disabled. C2 therefore adds demand at the missing-source
boundary and shares the canonical generation algorithm; it does not add a
scheduler, schema, activation receipt or mandatory import sweep. Existing
status rows supply preparation outcomes. Existing attested-artifact repair
continues using the shared-index queue. A usable local index starts directly.

The first-play rolling fallback remains intentional: exact copy indexing
still requires a complete source pass. Its presentation is now typeless from
the first response (C1), and completing preparation never changes the active
watch. Physical first-play latency and Safari recovery remain separate
observations, not claims made by these fixture tests.

Combined tree `c2770918` includes current main `2699360e7` and merged W1.
The pinned daemon test binary was rebuilt for that tree; all four C2 filters
passed again. The focused web policy, control, settings-section and player-DOM
files passed together with `node --test` (four files, 31.3 seconds), covering
the concurrent playback-info merge. This replaces the older-base evidence for
integration; no full web or Rust suite was repeated.

### Final integration correction

Runtime `8297e553` closes the adjacent native-browser error seam: element
aborts and transfer failures no longer count as media incompatibility when
selecting a rescue transcode. Decode and unsupported-format errors retain
that path. `node tests/playback/web-policy.test.js` passed, including the
shipped callback for all five native error-code cases; the normal workspace
hook passed. This correction will receive the same single final main-PR
adversarial review as the integrated effort.
