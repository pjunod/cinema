# Playback lifecycle — states, buffers, messages, and proof

**Status:** open — use-condition map and coverage audit, 2026-09-12 UTC
(2026-09-11 evening in New York). **Source baseline:** `efd54247adddeb3978812e55ebbb6a7f08adc9d9`.
**Scope:** continuous movie/episode playback across direct delivery, immutable
VOD, and rolling HLS; explicit differences for Live TV and library channels.

Companion to [PLAYBACK.md](../PLAYBACK.md) (routing and delivery), the
[protocol plan](PLAYBACK-CONTROL-PROTOCOL-PLAN.md) (design), and
[PLAYBACK-TESTING.md](../PLAYBACK-TESTING.md) (harness commands). This document
answers what must happen at each lifecycle transition, how buffering changes
that answer, which messages coordinate it, and what evidence is still missing.
It does not authorize another independent watchdog or change production policy.

The companion [rewrite remainder](PLAYBACK-REWRITE-REMAINDER.md) separates
unimplemented work, restricted coverage, physical evidence debt, and stale
status claims, and maps their intersection with these lifecycle cases.

Build from the [implementation contract](PLAYBACK-LIFECYCLE-IMPLEMENTATION.md):
four bounded packages, advisory Developer settings, and the revised CI/CD
process. Its current no-software-gating policy supersedes historical rollout
prerequisites; the coverage/evidence inventory here is not runtime permission.

## 1. The current implementation is a hybrid, with incomplete acceptance

All three client implementations contain playback-control reporters and action
handling. The observed Apple session used explicit control, so the current
freeze cannot be explained by a wholly unimplemented protocol. Prepared
successor priming also merged as Forgejo #203; the older status table saying
that its merge is pending is stale. Implementation is not physical acceptance,
and it does not mean the original M9 deletion contract is complete.

The current [create path](../../crates/plurxd/src/transcode.rs),
`create_session_inner`, tries immutable VOD first. With live recovery enabled,
exactly four typed prerequisite refusals admit rolling recovery:
`vod_index_pending`, `vod_transcode_unavailable`,
`vod_subtitle_burn_unavailable`, and `vod_source_unsupported`. Public clients
request VOD; an internal requested-live/takeover path is separately counted.
The old VOD cutover document's statement that no fallback exists is historical.
Do not infer presentation from a movie title, the HLS extension, or `remux`:
record the returned `vod` flag and actual playlist shape.

| Path | What is buffered and requested | What normal production means | Distinct boundary to test |
|---|---|---|---|
| Direct file/range | Player asks for byte ranges of an existing file | No HLS segment producer or HLS control bootstrap | Slow storage/range transfer versus decoder waiting; do not invent a producer hold |
| Progressive remux | Player consumes a progressive media response | A remux pipeline streams bytes; not an immutable segment catalog | Backpressure, EOF, seek/reopen, cancellation of the response |
| Immutable VOD HLS | Full timeline names segments; only some bytes may be materialized | A missing eligible segment creates bounded materialization demand; ready bytes are served from the working set | Planned is not materialized; a far-ahead island is not contiguous runway |
| Rolling HLS recovery | Growing/event playlist becomes a retained sliding window | Actor starts/holds/resumes producer; client reloads playlist and fetches published segments | Publication gate, playlist reload delay, pacing hysteresis, retention and film/item clock conversion |
| Live TV | Tuner feed with a live window and no finite film EOF | Source advances independently of a viewer's pause | Live-edge policy, tuner ownership, lost feed and bounded rejoin |
| Library channel | Server-time schedule chooses finite files | Follow mode uses finite HLS session lifecycle | Buffering must retain play intent; progress must keep moving; schedule rollover differs from ordinary watch history |

## 2. Keep intent, observation, producer state, and ownership separate

A single `playing` boolean cannot represent the system. Use these orthogonal
coordinates; the lifecycle names below are analysis labels, not new wire enums.

| Coordinate | Values/facts | Authority |
|---|---|---|
| Viewer intent | `active`, `hold`, `end`; desired recipe and seek target | Client controller; buffering does not change play intent to pause |
| Render observation | `starting`, `rendering`, `waiting`, `stalled`, `seeking`, `ended`, `failed` | Playback framework observed by the controller |
| Buffer/supply | Planned, materialized, published, transferred, loaded, decodable, rendered | Each component reports its own boundary; no stage certifies the next |
| Production | Starting/running, intentionally held, complete, failed; reason and physical signal acknowledgement | Server producer/control actor |
| Ownership | Current generation/epoch, reserved successor, committed successor, draining predecessor, ended | Durable session transaction plus exact local lifecycle owner |

For example, `active + waiting + producer held + 22 seconds loaded` is a real
observed state. It is neither an intentional pause nor proof that playback is
healthy. Likewise `active + rendering + producer held` can be healthy when
existing buffers cover the wait.

## 3. Every buffer boundary needs a demand and progress contract

```text
 source/storage -> reader/decoder/encoder -> muxer/segment publication
                                            |
                         VOD plan -> materialized cache
                                            |
                           playlist/catalog -> HTTP/relay -> player loader
                                                                  |
                                                        contiguous loaded range
                                                                  |
                                                        decoder/audio/video queues
                                                                  |
                                                           presented frames

 demand and backpressure travel upstream; progress facts travel downstream.
 Control exchanges join client intent/render facts with server supply facts.
```

| Boundary | Demand, buffer, and backpressure | Resume/progress proof | What must be monitored |
|---|---|---|---|
| Storage → reader | File reads, NAS latency, read-ahead, blocked read; cache-hit/miss changes cost | Required bytes read before the inherited budget expires | Read start/finish, bytes, latency, error; distinguish read starvation from a held child |
| Reader → decode/encode → muxer | Pipes and codec queues; hardware/CPU admission; decoder reorder and keyframe dependencies | Output for the required media interval, not merely a live PID or CPU activity | Input/output timestamps, producer attempt, speed, decode error, running/held state |
| Muxer → published media | Incomplete segment/init data must not be advertised as ready; GOP length affects first publication | Exact complete segment/init is available under the authorized generation | Planned versus complete versus published frontiers; publication delay and segment duration |
| VOD materializer → working set | Missing segment demand, shared production, finite disk/memory; readers pin needed windows | Requested immutable segment becomes readable; retries retain one overall materialization budget | Requested segment, ready state, waiter age, reader window, eviction reason, working-set capacity |
| Catalog/playlist → loader | VOD can name future work; rolling reloads can expose too short a future window | Client learns about and requests the next needed segment | Actual playlist shape/window, reload cadence, target duration, stale response, retention boundary |
| HTTP/relay → client | Socket backpressure and bounded body pumps; a partial body is not a completed fetch | Bytes cross the serving boundary; exact successful EOF commits the segment frontier | First byte, transfer stalls, response completion/drop, owner/epoch, upstream and downstream progress |
| Loader → loaded range | Range continuity, append/demux, track alignment; downloaded bytes can sit unusable | Contiguous range covers current target on the attached item's clock | Range start/end/gaps, timeline origin, append errors, track readiness, sample age |
| Loaded range → decoder → output | Resume policy, decode queues, audio/video sync; AVPlayer may wait with loaded media | First frame and continuing presentation at the intended rate; audio-only uses the appropriate clock | Waiting reason, actual rendered progress, buffer-empty/likely-to-keep-up, dropped frames, audio versus video progress |
| Predecessor → successor | Two separately accounted buffers and resource sets | Successor readiness, authorized commit, observed first presentation, predecessor retirement | Both identities, both origins, buffer through switch point, acknowledgement, drain and released resources |

**Accounting rules.** Keep bytes and media time separate. Convert media seconds
to wall-time runway using intended playback rate. Never sum disjoint buffered
ranges or subtract film position from a session-relative frontier. Record the
measurement's generation, timestamp and origin before comparing components.
A successful HTTP response proves neither decoder readiness nor presentation.

### 3.1 The pacing/refill loop needs a liveness proof

The buffering phases have different exit conditions:

| Phase | Buffer needed | Who permits exit |
|---|---|---|
| Cold startup | Init, keyframe/track alignment and initial complete publication | Server publication gate, then player first presentation |
| Steady refill | Useful media ahead of current playhead at intended rate | Producer/materializer and loader remain able to satisfy demand |
| Rebuffer | Enough usable, aligned data for the framework to resume | Framework resume policy followed by actual presentation |
| Paused | Retained buffer may remain; new production is not ordinary active demand | Viewer resume intent, then authority and buffer revalidation |
| Seeking | Buffer around the latest destination | Target landing on the current intent/item generation |
| Successor preparation | Separate successor buffer covering the switch interval | Readiness and commit transaction, then first successor presentation |
| Stopping/draining | No new consumer demand; only authorized in-flight work remains | Exact terminal/retirement owner and confirmed resource release |

The rolling startup grant is a specific exception: before the one-time
publication floor is reached, `evaluate_flow` exempts demand-hold and time
pacing so the playlist can become playable. End and hard byte/capacity limits
still apply. This exception must not rearm when retention advances the window
or a stale predecessor publication arrives. Startup and rebuffering therefore
cannot be tested as if they share identical pacing rules.

The current rolling policy in
[transcode.rs](../../crates/plurxd/src/transcode.rs) derives a production target
from reported client runway plus 30 seconds of intended-rate reserve, capped
by configured limits. Its held release threshold has hysteresis. Apple asks
for a 60-second forward buffer on growing HLS; that is a preference, not a cap.
[PlayerController.swift](../../clients/apple/Sources/PlayerController.swift)
also enables `automaticallyWaitsToMinimizeStalling`. Immutable/direct items use
a zero forward-buffer preference, leaving the normal framework policy in place.

For a positive configured time cap, the current steady-state calculation is:

```text
anchor = seek target if seeking, otherwise reported film position
R = ceil(max(0, buffered_through - anchor) / 1000)       media seconds
T = clamp(R + ceil(intended_rate * 30), 1, configured_time_cap)
A = (media_origin + published_end - anchor) / 1000      media seconds
enter time hold when A > T
while held, time release requires A <= max(T - 30, floor(T / 2), 1)
```

A nonpositive configured time cap disables this time limit. Resume also
requires the byte/global limits to clear; clearing time alone is insufficient.
Byte limits use their held release thresholds, and global entry counts total
scratch while global release counts drainable ahead bytes. A held producer
still serves already published bytes. The analogous VOD constraint is ready
materialization and reader-window/working-set capacity, not this rolling time
formula.

These individually reasonable policies must work together. Test the closed
loop, including stale reports, playlist reloads and variable segment duration:

1. The player waits for enough usable media to resume.
2. The server decides it has produced enough and holds.
3. A hold must have a reachable release condition while the film clock is
   stationary. Requiring that frozen clock to move first is circular.
4. If the server has fetchable media, the loader must have a bounded path to
   request and consume it. If it does not, additional production/materialization
   must have a bounded path to make it available, subject to real capacity.
5. After refill, actual presentation clears the wait. A new request, fresh
   heartbeat or a bigger byte counter alone cannot clear it.

A useful test state vector is `(playhead, loaded_through, published_through,
fetched_through, render_state, demand, producer_state, hold_reason)` plus the
current generation, timeline origin, sample ages and capacity facts. Hold the
playhead fixed and execute the real policy repeatedly. Verify useful progress
or a classified capacity/terminal result; do not let a test pass solely because
a later watchdog reopens the title.

## 4. State transitions and their buffering/communication contracts

```text
idle -> opening -> admission -> starting -> rendering
                    |             |           |  ^
                 refusal       wait/refill <--+  |
                                  |              |
                                  +-- usable media + presentation
                                  |
                              persistent stall -> classified recovery

rendering/waiting <-> viewer pause -> resume/revalidate -> render or refill
rendering/paused  -> seek latest target -> land -> render or stay paused

current remains live -> reserve successor -> prime -> client readiness
                              |                          |
                              +--- abort/expiry <--------+
                                                         |
                                        commit -> successor presents
                                                         |
                                                 predecessor drains

any state -> stop/end -> fence ownership -> finish cleanup -> idle
```

A terminal failure is a classified exit from an attempted operation, not a
loop back to opening. A newer viewer command supersedes pending operations;
stop must remain reachable even during admission, refill or preparation.

Coverage IDs in the final column refer to §6. **Partial** means relevant source
regressions exist; it does not claim every path/client combination passes.
**Open** means the named end-to-end contract is not established by this audit.

| ID | Transition / trigger | Buffer and resource contract | Communications and completion | Evidence |
|---|---|---|---|---|
| L01 | Idle → opening: Play or resume a title | No current buffer assumed; retain requested position/recipe; reserve only the owned create | Decision/capabilities, idempotent create, returned actual presentation and control bootstrap | C01/C02, partial |
| L02 | Opening → waiting for admission | Capacity reservation is not a playable stream; bound wait/cancel without leaking CPU/GPU slots | Typed pending/refusal and retry information; create identity survives ambiguous response | C03, partial |
| L03 | Opening → starting | Acquire init and first playable media around the requested target; distinguish metadata-ready from first-frame-ready | Media requests plus `active/starting`; exact generation attached | C04, partial |
| L04 | Starting → normal play | First frame/audio clock at target, then sustained progress; reset per-item establishment | `active/rendering`, current contiguous buffer; TTFF carries actual path | C04/C05, partial |
| L05 | Normal play → normal play | Refill before usable runway runs out; preserve bounded memory, disk and retention | Fresh control and media progress; producer hold is allowed while rendering remains continuous | C06, open closed-loop/device proof |
| L06 | Normal play → buffering wait | Preserve current item and useful buffer; demand stays active even if framework rate becomes zero | `active/waiting`, intended rate, exact ranges, waiting reason, timestamps | C05/C07; observed failure |
| L07 | Buffering wait → refill | Classify missing production, unavailable segment, transfer gap, unusable loaded data or decoder wait; resume only the blocked component | Control supplies client need; server supplies publication/capacity/hold facts; media requests obtain bytes | C06/C08/C09; open combined proof |
| L08 | Buffering refill → rendering | Same item/session when repair is in place; first resumed frame, not a heartbeat, closes the stall | Urgent updated render/buffer observation then periodic reporting; one measured stall episode | C07; open physical proof |
| L09 | Waiting → persistent stall | Preserve elapsed wait across cause flapping; detectors report facts to one decision path | `active/stalled`, classified error/wedge evidence; current server verdict | C07/C10, partial |
| L10 | Stalled with published bytes → fetch recovery | Published-but-unfetched is separate from encoder starvation; preserve recipe where it is a delivery wedge | Hold must not veto proven fetch recovery; generation-fenced bounded reconnect when needed | C10, partial; empty-buffer fix does not cover every loaded-but-waiting case |
| L11 | Stalled with loaded bytes → render recovery | Do not call loaded bytes proof of healthy decoding or sustained bandwidth; inspect gaps, tracks, resume policy | Frame/wait/decoder observations joined with server supply; one classified action | C07/C10, open exact Ronny case |
| L12 | Rendering/waiting → viewer pause | Hold intent; retain useful loaded media and session lease; no automatic recovery because position is stationary | New intent `hold`, rate zero allowed; continue eligible control renewals | C05/C11, partial |
| L13 | Paused → resume | Resume current buffer if valid; otherwise refill/rejoin around preserved target, without false end | New `active` intent even before framework rate changes; confirm presentation | C05/C11, partial |
| L14 | Foreground → background/sleep → return | Distinguish supported playback/PiP, intentional hold and unrenewable absence; stale cache cannot imply ownership | Valid renewals within granted lease; on return revalidate epoch/session and current intent | C11/C12, partial; OS/device proof open |
| L15 | Playing/paused → seek in available range | Buffer is evaluated at target, not old playhead; paused seek stays paused | New intent/sequence with seek target, then observed landing; no unnecessary session replacement | C13, partial |
| L16 | Seek beyond range / VOD cold segment | VOD materializes target within same timeline; rolling may reopen near keyframe with a new origin | Coalesce latest target; typed pending; attach/land only if request still current | C08/C13, partial |
| L17 | Seek storm / selection race | Abandoned destinations cannot leave competing producers, subtitle windows or stale attachment | Latest intent wins; stale callbacks and create replies are fenced | C02/C13, partial |
| L18 | Current → reserve successor | Keep predecessor and buffer alive; reserve second recipe/worker without freeing occupied resources | One durable preparation/action identity; reject unadmitted pair without breaking current playback | C14/C15, partial |
| L19 | Reserved → primed | Reservation does not imply bytes; attach successor producer and expose only authorized staged media | Server prime/ledger precedes `prepare`; predecessor remains current | C14/C15, partial |
| L20 | Primed → client ready | Load metadata, align timeline, fill successor's own contiguous buffer to switch point | Matching action acknowledgement: readiness stages; no first-frame claim from metadata alone | C16, partial |
| L21 | Ready → committed → switched | Change durable/current ownership exactly once; retain predecessor until successor presentation/drain contract permits release | Fenced commit/first-frame and supported switched acknowledgement; lost reply replay is idempotent | C15/C16, partial; physical switch evidence open |
| L22 | Preparing → aborted/expired/refused | Free only successor resources; current buffered playback survives; stop cancels both | Failed/aborted acknowledgement or expiry; durable cleanup settles slot before reuse | C14/C16, partial |
| L23 | Current → restart/reopen | Carry current position, pause, tracks, quality/HDR intent; discard only obsolete item buffer; prevent old replies attaching | New idempotent request, typed cause/predecessor where required; new bootstrap and first-frame receipt | C02/C10/C13, partial |
| L24 | Quality/audio/subtitle change | In-place operation when supported; otherwise prepare or bounded reopen; subtitle work cannot stall video unnecessarily | Desired/effective selection, capability/admission facts, preparation or replacement result | Routing inventory + C16/C17, partial |
| L25 | Any live state → stop/end | Fence fresh media/control actions immediately; dispose client buffer; reap child before returning its permits | Final `end`/DELETE, idempotent settlement; cancel reporter, observers, pending starts/preparations | C02/C18, partial |
| L26 | Ended → immediate restart/new title | Fresh generation cannot inherit stale wait/recipe/seek state; old cleanup cannot kill new owner | Fresh create identity and bootstrap; reject stale timer/callback/control results | C02/C18, partial |
| L27 | Near EOF → completed | Drain playable tail before natural completion; producer EOF is not rendered EOF | Verify title endpoint and media completion; final progress/end once | C04/C19, partial |
| L28 | Unexpected EOF → bounded recovery | Preserve position; truncated playlist/file is not a watched-to-end event | Failed/truncated observation and classified reopen; repeated same-position end is bounded | C19, partial |
| L29 | Control outage while media works | Consume/refill usable media where authorized; avoid destroying it just because control failed | Retry within lease; distinguish auth refusal, stale sequence and owner change from transport outage | C12, partial |
| L30 | Media outage while control works | A heartbeat is not video progress; preserve active demand and identify missing boundary | Render starvation plus server delivery/producer diagnostics; one recovery decision | C08/C09/C10, partial |
| L31 | Owner loss / handoff | Buffered media may bridge outage; never mix owner epochs or let old owner resume production | Owner-change/rejoin path, durable fence, new bootstrap; planned drain needs separate acceptance | C12/C20, partial |
| L32 | Resource pressure / multiple viewers | Per-reader needed windows and predecessor+successor capacity count; one viewer cannot strand another | Typed capacity/working-set hold, independent client demand/lease, reason clearing | C03/C08/C14, partial |
| L33 | Live/library-channel rollover | Follow schedule/live policy, not movie EOF/resume semantics; wait keeps active demand | Channel progress and startup decision, tuner/session release, bounded next attachment | C21, partial; separate physical proof |

### 4.1 A transition is unfinished until its messages settle

For each row test: happy path; duplicate; delayed/out-of-order response; lost
request; lost response after server commit; cancellation; newer viewer intent;
expired lease; and capacity failure where applicable. At each interruption,
assert current item, current durable generation, active producers, retained
buffer and acquired/released permits. A response-code assertion alone is not
a lifecycle test.

Pause, producer hold, buffering wait, reserved successor and stopped are five
different conditions. Neither `hold` on the wire nor an idle decoder means
that all five should share resource or recovery behavior.

## 5. Communications that facilitate, monitor, and maintain playback

The [current wire source](../../crates/plurxd/src/playback_control.rs), rather
than historical pseudocode, owns names and validation. HLS routes are under
`/api/v1/hls/{session}`. The protocol is control-plane coordination; media
continues over playlist/segment requests.

| Message / fact | What it facilitates | What it maintains and proves |
|---|---|---|
| Decision + create | Select recipe, request position, admit resources, bind request identity | Actual `vod` result, session/playlist/origin and control bootstrap; timeout is not proof create failed |
| POST control snapshot | Report intent, render state, film position, contiguous buffer, intended rate, seek, selection and capability changes | Generation, epoch, instance and monotone sequence fence stale work; accepted fresh control renews lease |
| Server control reply | Join effective selection, delivery and lease facts with one supported action | Accepted sequence, expiry, producer state and hold cause; reply receipt does not establish rendered progress |
| `none` / `hold` / `retry_resource` / `terminal` | Explain or classify current server state | Current shipped advisory actions; a production hold is not a command to pause the player or deny published bytes |
| `prepare` + acknowledgement | Stage successor media without immediately destroying predecessor | The actor retains `prior_action` internally for replay; clients send the supported acknowledgement schema. The older plan's example `commit_replacement` name is not the shipped action API |
| Media GET / body completion | Move init, media and subtitle bytes | Exact completion advances appropriate delivery frontier; abandoned/partial response cannot claim full delivery |
| GET status | Diagnose publication, transfer, pacing and producer state | Observation only; it is not a substitute for demand/lease renewal or rendered progress |
| Final end / DELETE | End stream and release ownership | Replays settle same operation; cancelled HTTP caller must not cancel admitted cleanup |
| Client/server events | Explain waits, transitions, reopens and first frames after the fact | Correlate one episode across clocks/generations; avoid capability URLs and raw credentials |

**Cadence and bounds at this source revision.** Normal control cadence is
5 seconds. Rolling explicit lease is 30 seconds, legacy rolling lease
60 seconds, VOD lease 300 seconds. Use bootstrap/response values rather than
client hard-coding. Immediate intent/evidence notifications are coalesced by the
reporter. Exact duplicate control does not extend the lease. The Apple stall
hold deferral ceiling is 20 seconds; that bounds a recovery decision and is
not an acceptable normal-play freeze target.

**Timeout ownership.** The protocol design allows a server producer-progress
deadline, one client playback-progress observation deadline, and a VOD
segment-materialization deadline. Lease expiry, HTTP/body limits, preparation
expiry and drain/reap budgets are separate lifecycle limits. Tests must assert
what each timer can mutate. A media-request timeout cannot independently
replace a producer; a fresh heartbeat cannot reset a render-stall episode;
an expired successor cannot end its predecessor. Current client source still
has several detectors and bounded reopen paths, so this desired ownership
contract is not a claim that M9 deletion has finished.

The existing protocol does **not** explicitly communicate a framework's
minimum desired refill quantity/resume threshold. Its loaded range, render
state and decoder observations do not establish that threshold. Determine
whether current facts suffice through the coupled test before adding a wire
field. AVPlayer's internal threshold may not be directly observable: mark it
unknown and measure behavior instead of inventing a number.

**Required diagnostic record per transition:** episode ID; old/new lifecycle
state; trigger; viewer intent; presentation/recipe; client/app/OS and server
builds; session generation and owner epoch in sanitized form; film/item origin;
source/read/publication/fetch/loaded/render frontiers with sample ages; intended
and actual rate; waiting reason; active hold and release predicate; request and
action identities; deadlines remaining; capacity reservations; chosen recovery;
first resumed frame and final cleanup. Fields not collected today remain an
observability gap, not zeros. Existing events are useful but do not supply this
entire joined record.

## 6. Existing coverage and its limits

These are inspected regression anchors, not claims that all suites ran in this
session. Tests can prove bounded recovery while users still see regular
freezes. Coverage must therefore distinguish policy correctness, integrated
supply progress, and physical continuous presentation.

| Ref | Existing source/test evidence | What it does not establish |
|---|---|---|
| C01 | [Routing catalog](../../tests/playback/routing-decisions.toml); [HLS tests](../../crates/plurxd/src/http/hls.rs): `every_vod_ineligibility_reaches_the_wire_as_a_typed_refusal` | Both engines must be deliberately selected in lifecycle runs; a successful remux verdict does not identify engine |
| C02 | [Apple operation ownership](../../clients/apple/Tests/PlayerOperationOwnershipTests.swift): `testStartStopStartRestoresOnlyTheNewTitlesPlaybackIntent`, `testStopStartWhileReadingControlSequenceCannotCreateOldRecipeForNewFile` | Cross-client, real-server ordering and resource accounting |
| C03 | [Transcode tests](../../crates/plurxd/src/transcode.rs): `live_admission_fails_retryably_when_background_does_not_release`, `aborting_a_waiting_live_admission_releases_its_yield_signal` | Real workload contention while preserving render continuity |
| C04 | [Apple tests](../../clients/apple/Tests/AppleClientTests.swift): `testEveryOpenedItemMustEstablishItsOwnBufferingRecoveryWindow`, `testAppleTTFFRebasesGrowingCopyOriginWithoutRestartingClock` | Physical first-frame and long-GOP startup latency |
| C05 | [Apple mapper](../../clients/apple/Tests/PlaybackControlSnapshotMapperTests.swift): `testAPausedPlayerHoldsRatherThanEnding`, `testAShortWaitIsWaitingAndALongOneIsStalled`; [Android mapper](../../clients/android/app/src/test/java/tv/plurx/app/player/PlaybackControlSnapshotMapperTest.kt); [web control](../../tests/playback/web-control.test.js) | Actual player observation plumbing can still report the wrong intent; mapper fixtures alone do not cover it |
| C06 | [Transcode tests](../../crates/plurxd/src/transcode.rs): `explicit_flow_uses_reported_runway_and_preserves_capacity_bounds`, `a_client_fetch_releases_a_held_session_and_restarts_progress` | Coupled AVPlayer wait/refill versus dynamic hold-release liveness with the playhead frozen |
| C07 | [Apple tests](../../clients/apple/Tests/AppleClientTests.swift): `testBufferingWaitHasABoundedRecoveryTimer`, `testRegimeFlappingCannotResetTheStallClock`, `testAppleBufferedRunwayStopsAtTheFirstGap` | A bounded reopen is not prevention of short freezes; loaded-but-waiting needs its own integrated case |
| C08 | [VOD tests](../../crates/plurxd/src/vodserve.rs): `a_blocking_get_materializes_the_segment_and_a_re_get_serves_the_same_bytes`, `materialize_watchdog_spans_http_retries_and_fails_typed`, `make_room_never_evicts_inside_a_live_readers_window` | Physical VOD refill/resume with real segment durations, multiple readers and decoder behavior |
| C09 | [HTTP HLS tests](../../crates/plurxd/src/http/hls.rs): `segment_bytes_are_counted_as_the_body_drains_not_at_open`, `an_abandoned_vod_body_counts_nothing_it_did_not_hand_over`, `driven_local_body_rejects_queued_data_after_terminal_failure` | Server-written bytes are not client-decoded frames; real relay/storage stalls still need joined evidence |
| C10 | [Control tests](../../crates/plurxd/src/playback_control.rs): `a_stalled_starved_client_with_fetchable_media_is_not_told_to_hold`; [Apple tests](../../clients/apple/Tests/AppleClientTests.swift): `testAHoldDoesNotGetToDecideAStallThatOnlyAReopenCanFix` | Positive loaded runway can exclude the empty-buffer repair; no universal pacing diagnosis follows from the message |
| C11 | [Control tests](../../crates/plurxd/src/playback_control.rs): `thirty_minute_foreground_hold_remains_live_on_delivered_heartbeats`; Apple mapper in C05 | OS sleep, Wi-Fi changes and paused buffer eviction on physical devices |
| C12 | [Apple reporter tests](../../clients/apple/Tests/PlaybackControlReporterTests.swift): `testAnOwnerChangeAdoptsTheNewGenerationAndRestartsTheSequence`, `testAnExchangeThatNeverAnswersEndsAtTheDeadline`; [session tests](../../clients/apple/Tests/PlaybackControlSessionTests.swift): `testAVerdictDoesNotOutliveTheLeaseItWasGivenUnder` | Live media consumption across owner loss on every client |
| C13 | [Apple tests](../../clients/apple/Tests/AppleClientTests.swift): `testSeekRouteSeeksInsideALaterRangeInsteadOfSnappingToAnEarlierEdge`, `testSeekRouteReopensOutsideTheAdvertisedWindow`; [operation tests](../../clients/apple/Tests/PlayerOperationOwnershipTests.swift): `testPauseDuringResumePreparationRetainsTheResumeDestination` | Simultaneous seek, buffered gap, late publication and real device resume |
| C14 | [Store contracts](../../crates/plurx-core/tests/store_contract.rs): `media_session_prepare_stages_a_successor_that_changes_nothing`, `media_session_prepare_does_not_discount_the_predecessor_against_the_cap`, `media_session_maintenance_reaps_an_abandoned_preparation` | A reserved row alone provides no playable media |
| C15 | [Control tests](../../crates/plurxd/src/playback_control.rs): `preparation_executor_commits_a_staged_successor`, `preparation_executor_aborts_only_the_successor`; [HLS tests](../../crates/plurxd/src/http/hls.rs): `a_rolling_preparation_commit_reaches_the_store` | Every recipe axis and real first-frame continuity |
| C16 | [Apple prepared tests](../../clients/apple/Tests/PreparedReplacementTests.swift): `testEndingThePlayerSettlesAndFreesWhateverIsLive`; [Android prepared tests](../../clients/android/app/src/test/java/tv/plurx/app/player/PreparedReplacementTest.kt); web control in C05 | Physical two-player capacity, timeline alignment and gap at cutover |
| C17 | [HLS tests](../../crates/plurxd/src/http/hls.rs): `one_session_traverses_anchors_joins_refuses_and_releases`; [Apple tests](../../clients/apple/Tests/AppleClientTests.swift): `testSelectSubtitleRoutingReopensOnceForABurnAndStaysInPlaceForNative` | Subtitle readiness and audio/video synchronization under real refill pressure |
| C18 | [HLS tests](../../crates/plurxd/src/http/hls.rs): `cancelling_public_delete_does_not_cancel_its_admitted_cleanup_owner`; [transcode tests](../../crates/plurxd/src/transcode.rs): `supersession_cancellation_finishes_vod_and_every_rolling_victim` | Full resource return with real child processes and both delivery paths under every interruption |
| C19 | [Apple tests](../../clients/apple/Tests/AppleClientTests.swift): `testEarlyEndGetsOneReopenButCannotLoopAtTheSamePosition`; [VOD tests](../../crates/plurxd/src/vodserve.rs): `every_terminal_cause_answers_gone_and_supersession_spares_the_keeper` | Correct rendered tail and watch history on hardware |
| C20 | [Media session tests](../../crates/plurxd/src/media_sessions.rs): `release_reconciliation_elects_once_and_retries_with_one_fence`, `expired_active_route_is_retryable_owner_transition_not_terminal` | M8 planned drain and fleet cutover acceptance |
| C21 | [Apple Live TV/channel tests](../../clients/apple/Tests/LiveTvTests.swift), [library channel source](../../clients/apple/Sources/LibraryChannels.swift) | Separate controller: ordinary movie coverage cannot be credited to channel buffering/progress automatically |

### 6.1 Use-condition cross product

Run every applicable L-row against both **forced immutable VOD** and **forced
typed rolling fallback**. Assert the path in the response/playlist so fallback
cannot silently turn a VOD test into a passing rolling test. Direct/progressive
runs use their applicable subset and explicitly mark producer-control operations
not applicable, with a reason. Live TV and library channels get separate runs.

The highest-risk combinations need explicit cases, not random sampling:

- Apple TV AVPlayer first; then iPhone/iPad, Android TV/phone Media3, Safari
  native HLS and Chromium/hls.js. Record exact installed builds.
- Copy/remux versus hardware and software transcode; 1080p SDR and high-bitrate
  4K HDR/DV; long/variable GOP and segment duration; audio/text/bitmap subtitle
  modes and track changes. Preserve requested quality and dynamic range.
- Cold/warm cache, index unavailable, segment absent, cache eviction, slow NAS,
  read failure, partial HTTP body, relay hop and owner loss.
- Stable network, jitter/bursts, bandwidth drop and restoration, transport loss
  with live control, control loss with live media, and total disconnection.
- One viewer, multiple readers of one rendition, competing transcodes, occupied
  CPU/GPU slots and a prepared successor requiring a second allocation.
- Buffer empty, short, apparently sufficient but waiting, disjoint ranges,
  stale ranges from old item, video blocked with audio moving, and vice versa.
- Stop/pause/seek/restart at each asynchronous seam, especially during admission,
  metadata readiness, materialization, commit and retirement.

Use pairwise combinations for lower-risk dimensions after the explicit cases.
Every exclusion records why that transition cannot occur on that path.

## 7. Tonight's failure is an acceptance case, not a guessed root cause

[Sanitized retained observations](../../tests/playback/lifecycle-observation-2026-09-11.json)
contain 17 events for **Ronny Chieng: Speakeasy** on m6, including 13 stall
reports. Multiple reports may describe one episode; 13 is not a count of
independent freezes. The viewer identifies Apple TV; telemetry identifies
Apple AVPlayer but does not establish the installed client build. m6, nuc4 and
nynuc reported server build `v0.3.0-2059-gefd54247` when inspected.

| New York time, September 11 | Observation | What it establishes |
|---|---|---|
| 20:13:52 | Buffering for 12.31 s; loaded runway 63.64 s; server `time` hold; outcome `server_hold` | Waiting with considerable loaded media is a real use condition |
| 20:18:06 → 20:18:14 | Same film position 742.798 s; runway 0.149 s; hold then reopen at recorded 20 s | The bounded recovery ceiling is active; it does not prevent freezes |
| 20:18:19 | First-frame report after reopen: 5.704 s | Reopening adds visible recovery cost; this is not proof of continuous playback |
| 20:54:35 | Buffering 12.355 s, runway 22.58 s; `AVPlayerWaitingToMinimizeStallsReason`; server pacing hold | Client refill policy and server pacing must be examined together |
| 20:54:46 | Access-log self-recovery; recorded stall duration 19.139 s | This episode eventually recovered, consistent with the viewer's report |

The observed stalls with server status were rolling/sliding HLS using explicit
control. This does not explain the viewer's separate VOD failures. The receipt
is a bounded extract, not a full session/fleet census. Snapshot ages and
frontier time bases limit causal claims. `recorded_ms: 0` means the access-log
path did not supply a duration, not that no freeze happened.

## 8. Close coverage in this order

1. **G1 — coupled buffering liveness (L05–L11).** Replay tonight's two distinct
   conditions: near-empty buffer with held production, and loaded-but-waiting.
   Use the real flow policy, loader request/reload behavior and controller
   observation path. Track useful progress at every boundary. A watchdog reopen
   does not pass the normal-play contract. Reproduce on Apple TV with the same
   title/path before changing policy; compare a forced VOD run.
2. **G2 — transition fault harness (L01–L04, L12–L17, L23–L30).** Drive each
   asynchronous boundary with delay/drop/cancel/duplicate injection. Join client
   state with real server session/resource state. Require correct demand,
   retained intent, bounded cleanup, and first resumed presentation.
3. **G3 — reservation and switch accounting (L18–L22, L31–L32).** Exercise
   predecessor plus successor under actual capacity limits. Prove readable
   successor before switch, exact commit replay, abort cleanup and no old-owner
   resurrection. Keep unsupported recipe/capability combinations explicit.
4. **G4 — physical continuity matrix.** Run steady playback beyond repeated
   pacing and playlist-window cycles, plus refill/seek/pause/restart transitions,
   on both engines and each client family. Measure stall count, total/max wait,
   first-frame latency, recovery count, quality changes and leaked resources.
   Healthy-network steady playback must have zero involuntary presentation
   stalls; startup, user pause, seek and injected outage are separate intervals.
   Existing bounded recovery limits do not redefine a healthy play pass.
5. **G5 — ownership and observability closure.** Inventory every active recovery
   entry point against one arbiter, distinguish observation from mutation, and
   remove competing owners only after equivalent transition evidence exists.
   Add missing per-boundary facts needed to explain G1, with bounded storage;
   do not build a new polling/restart loop just to collect them.

For buffering recovery, record recovery time from supply restoration to actual
presentation. Establish the per-platform envelope from measured segment/loader
behavior before imposing an invented universal numeric threshold. All cases
still require a finite outcome: resume, explicit terminal/capacity result, or a
recorded failed acceptance test. Indefinite silent waiting never passes.

**Definition of covered:** an L-row has an exact source owner, an automated
transition receipt for each applicable engine, failure/race evidence, and the
required physical receipt for platform-dependent behavior. Test presence,
compilation, or an HTTP 200 cannot mark the row complete. This audit establishes
the map and existing anchors; G1–G5 remain open. No playback fix or physical
acceptance is claimed by this documentation change.

### 8.1 Validation of this map

At `efd54247` plus this documentation/receipt change:

- `python3 -m unittest tests.operations.test_docs_index tests.validation.test_playback_routing_inventory`: 8 tests passed.
- `node tests/playback/web-control.test.js`: passed the existing web reporter/control regression program.
- `make validation-lint`: passed path ownership/catalog validation.
- All 33 lifecycle IDs are unique; all 44 named test anchors and relative links
  in this map resolve in the inspected source.

These checks validate the map and the existing web control baseline. No Rust,
Swift, Android or physical playback suite was run for this documentation-only
change; none of those outcomes is implied by these receipts.
