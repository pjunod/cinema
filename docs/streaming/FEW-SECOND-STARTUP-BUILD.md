# Few-second startup — review of shorter targets and partial delivery

**Status:** changes requested; protocol corrections in progress, runtime blocked on control experiments ·
**Date:** 2026-10-02 · **Decision:** Paul explicitly authorized building the
few-second startup path, including Low-Latency HLS, in the playback repair chat.

Companion to [the existing startup contract](PLAYBACK-STARTUP-LATENCY-IMPLEMENTATION.md)
and [measurements](PLAYBACK-STARTUP-LATENCY-MEASUREMENTS.md). This extension
supersedes that contract's LL-HLS non-goal for this effort. It preserves source
quality, ownership, admission, cancellation, resource limits and honest evidence.

**Review request:** assess the existing preparation correction separately from
the proposed LL-HLS runtime. In particular, challenge the media-preservation,
native-startup, ownership and resource contracts below. The current Safari
prototype does **not** meet the requested few-second continuous-playback outcome.
Do not approve runtime activation based on passing primitive tests or on part
requests alone. Sections 7–10 identify the exact changes, evidence and decisions
that need review.

Hostnames and local paths in this committed packet are privacy aliases:
`media1`, `forge.lan` and `~/plurx-*` stand for the captured host, forge and
temporary evidence/worktree locations. They do not relocate the evidence.

## 1. Outcome and scope

Start rolling playback with a few seconds of playable media after the requested
position, then continue producing and downloading toward the existing steady
buffer. A successful first frame followed by an avoidable startup stall fails
acceptance. Four seconds at 1× is an initial experiment, not a measured guarantee.
Record click-to-first-frame separately from the amount of media buffered.

The immediate reproduction is Wicked in media1's web UI on native Safari.
The user approved a three-minute local excerpt for testing. That media and raw
private logs stay outside Git in the existing temporary evidence directory.
The preparation-starvation repair in PR #745 remains a separate change; merge
its reviewed candidate after its required gate and integrate main here later.

Use one integration branch, `effort/few-second-startup`. Task branches start
from its current head and target it. No file-disjoint exception applies.
The user's original checkout is untouched. The working checkout is
`~/plurx-few-second-startup-20261002`; Rust 1.97.1 all-target daemon
check passed on main `772b9cf23` before Rust edits.

## 2. Decision and alternatives

| Approach | Decision and reason |
|---|---|
| Lower only the current 48-second publication gate | Insufficient alone: native buffering and holdback scale with the declared target. Keep the current gate until an alternative is measured. |
| Short target per attempt with verified maximum GOP and decodable boundaries | Reopened; evaluate classic two-second control before P2. Unknown sources retain the existing target. Packet keyframe flags and the three-minute excerpt do not establish whole-source closed-GOP safety. |
| Publish bounded fMP4 parts and support blocking reload | Authorized scope, but runtime construction waits for transport and position-continuity qualification. Parts alone have not enabled advancing native playback. |
| Re-encode video to force short GOPs | Rejected as a latency shortcut; preserving source quality is part of the request. |

Protocol references: [Apple's LL-HLS overview](https://developer.apple.com/documentation/http-live-streaming/enabling-low-latency-http-live-streaming-hls)
and [HLS second edition draft 22](https://datatracker.ietf.org/doc/html/draft-pantos-hls-rfc8216bis-22),
especially partial segments, blocking reload, preload hints and the low-latency
server profile. The latter is an active draft, not a published RFC.

The protocol distinguishes complete parent segments from parts. Parts need
truthful duration and independence, a fixed part target, appropriate holdback,
ordered retention and preload handling. Blocking requests specify media and
part sequence numbers. Invalid or excessively advanced requests fail promptly;
valid requests wait within a finite deadline. The server profile also specifies
HTTP/2 or HTTP/3. Native Safari engagement must therefore be observed on the
actual transport; successful ordinary HLS over HTTP/1.1 is insufficient proof.

## 3. Media and lifecycle contract

Freeze the selected delivery mode, parent target and part target at attempt
creation. Initially investigate subsecond parts and a four-second startup
runway; derive the consumed-media requirement from accepted playback rate.
Do not reuse first-presentation protection as a deadline for growing reserve.
Sustainable playback can remain below the steady target without being retired.

Only complete, validated part bytes become available. Reuse the existing init
promotion, Dolby Vision conversion and source timeline. A part boundary inside
a GOP must not acquire the semantics of a new random-access segment or discard
leading pictures. Parent output and its parts must represent the same ordered
samples, including audio tails. Parts cannot escape before the init and first
decodable origin are established. Prove sample identity and decoding continuity
before enabling the path in the daemon.

Keep the parent target honest for unknown GOPs. Bounded fragment emission may
require muxer changes, but no video encoder is introduced. Verify actual part
duration from sample clocks; reject invalid geometry before the first response.
After publication, preserve the existing failure contract rather than silently
replacing the stream. Never advertise a part before its bytes are durable.

Part catalog entries carry producer attempt, parent sequence, part index,
time interval, length and independence. Publish their immutable snapshot through
the current owner. The same attempt fences and final response admission protect
parts, playlists and preload responses locally and through a peer relay.
Relay queries must retain delivery directives. An old waiter cannot release
successor bytes after seek, replacement, cancellation or owner change.

Blocked requests retain bounded permits, obey cancellation, and wake from
publication notifications without spawning detached per-request tasks. A hinted
part request waits for the exact part; it never streams a half-written file.
Part bytes, parent bytes, in-flight writes and retention grace all consume
scratch. Avoid uncharged duplicate representations and account for any actual
duplication. Retention must keep promised parts readable for the required grace.

## 4. Reviewable tasks and ownership

| Task | Owned work | Required focused evidence |
|---|---|---|
| P1: protocol and feasibility | New core LL-HLS types/tests; this contract and docs index; isolated local transport experiment | Part geometry, sequence rollover, bounded future requests and EOF; actual Safari requests and advancing frames establish transport behavior |
| S1: source-qualified short target (priority before P2) | Exact source/recipe boundary proof, immutable per-attempt target, copy limits and publication bootstrap | Whole-source clean-boundary coverage, unchanged decoded samples and audio/DV behavior; no proof retains existing target. Fragment-index presence alone is insufficient if boundaries are dirty. |
| S2: short-target native acceptance | Real daemon and shipped Safari on existing HTTP LAN transport | Repeated cold/resumed Wicked or explicitly refused eligibility; advancing-frame TTFF, no added missing samples, no post-start stall, honest fallback |
| P2: producer and delivery (experimental, deferred) | fMP4/copy producer, rolling publication/flow/scratch, HLS HTTP and peer relay | Same samples through parts and parents; bounded waits, cancellation, ownership changes, retention and accounting |
| P3: client and settings | Startup mode selection, web native/hls.js handling, Developer switch and readiness text | Shipped client starts at requested position, keeps playing while reserve grows, reports actual readiness; switch never blocked by advisory readiness |
| P4: acceptance and promotion | Integration tests, retained measurements, current status and final review fixes | Repeated native Safari Wicked starts, continuity, rate/resume/pause/seek, throttled production and network, ordinary-client compatibility |

Tasks may touch earlier owners only after the preceding task lands on the
effort. Keep unfinished activation in Settings → Developer with advisory
readiness and an explicit graduation condition, following repository policy.
Do not infer native support solely from a transport label or manifest tags.

## 5. Evidence and completion

Use the pinned local compiler before each push, focused behavior regressions,
normal hooks and the required effort gate. Name each changed behavior's
regression in its PR and landing message. Integrate current main before final
qualification. The final main-bound PR receives one independent implementation
review and the required promotion gate/qualification receipt.

Acceptance records actual first-frame time, server-produced coverage,
client-buffered coverage and part requests separately. Cover startup at zero
and a resumed position, stop during a blocked request, replacement, pause,
rate changes, EOF, slow production and a refused scratch grant. The source
sample is a local proof only; physical Apple/Android and fleet claims require
their own observations. Production deployment and observed media1 playback are
separate from merging source. Do not call the original ten-second complaint
fixed before measuring the deployed result.

## 6. Execution record

- 2026-10-02: scope expansion authorized; isolated effort checkout created.
  Rust 1.97.1 `cargo check -p plurxd --all-targets --offline` passed on its base.
- P1 in progress on `codex/ll-hls-startup-contract`. Eight pure protocol tests
  pass locally; the module is not called by production. The loopback Safari
  prototype requests parts and blocking reloads but fails early continuous
  playback. No production activation, LL-HLS PR, or deployment exists.

## 7. Existing preparation repair — separate reviewed change

[PR #745](http://forge.lan:3000/noirr/plurx/pulls/745) targets main. Its
reviewed/tested tree is `b49a28731c13ad55c9e6f3aed5614efe16a2f6a8`; pushed
head `f6c74feef47df4a2d9a1e925122c092279a53da4` has the identical tree after
preserving a remote main merge. Its base is `772b9cf236e989fbb75ac4d1aa760c0ad632395d`.
At this packet's preparation, policy preflight passed; required Rust/Windows
and aggregate gates were still pending. This is not a merge or deploy receipt.

Review follow-up head `7b8954fca` is pushed with B1/B2 fixes and the
transaction-census correction; its remote gates are pending. Sections below describing the
original review tree are historical evidence, not qualification of this head.

**Problem proved:** Wicked's current-engine preparation request stayed queued
with `foreground_preempted` while its live viewer was playing. The daemon
required an idle node before claiming or hashing that viewer's source.

| Changed file under `crates/` | Resulting behavior |
|---|---|
| `plurx-core/src/store/fragment_index_cluster.rs` | Shared live-viewer predicate and capacity-aware claim API; existing ordinary claims keep their behavior. |
| `plurx-core/src/store/sqlite/fragment_index_cluster.rs` | Busy claims require an unexpired pending playback-analysis waiter in both candidate selection and fenced update. |
| `plurx-core/src/store/hiqlite_fragment_index_cluster.rs` | Same admission rule on the replicated Store. |
| `plurx-core/src/store/background_jobs.rs` | Authoritative preparation observation includes whether live viewer interest remains. |
| `plurxd/src/state.rs` | Busy nodes can claim and continue source attestation for any unexpired playback waiter on the request; cancellation and loss of interest stop that exception. |
| `plurx-core/tests/store_contract.rs` | SQLite and three-voter Hiqlite regression for live-viewer admission, expiry, engine compatibility and maintenance attempt accounting. |

Existing source-I/O reservations, leases, claim fences and feature cancellation
remain in effect. Artifact construction and hydration still require their own
idle/heavy-worker admission. Therefore this repairs source-attestation starvation;
it does not guarantee an index finishes on a continuously busy target and does
not itself change the rolling 48-second bootstrap.

The bounded daemon regression adds one retained test task and four timer calls;
`tests/playback/rolling-producer-owners.toml` records their ownership. This
inventory correction fixes the first gate failure. The same independent
reviewer approved the implementation, correction and final main integration
with no actionable findings, while retaining the limits above.

Local evidence: pinned check and workspace Clippy passed; busy/idle daemon wake
and hash cancellation regressions passed; the new SQLite/Hiqlite contract passed;
seven ownership-inventory and four documentation-index tests passed. The precise
commands and regression identifiers are retained in the PR description and the
existing measurements document.

## 8. New LL-HLS code — implemented versus proposed

The LL-HLS branch currently has only these source changes, based on main
`772b9cf23`; it does not yet include PR #745:

| File | Current change and status |
|---|---|
| [transcode/low_latency.rs](../../crates/plurx-core/src/transcode/low_latency.rs) | New pure types and eight tests. Local implementation, not wired into a daemon route. |
| [transcode/mod.rs](../../crates/plurx-core/src/transcode/mod.rs) | Exposes the new module; no FFmpeg arguments or production duration constants changed. |
| This document and [docs index](../README.md) | Build/review contract and its index row. |

`PartPolicy` freezes the part and parent duration contract. It rejects zero or
oversized parts and rejects an undersized dependent part unless that part ends
its parent. It deliberately does not choose a production part target.

`ReloadFrontier` borrows a contiguous published-parent inventory and rejects
invalid ordering or a non-final unfinished parent. `ReloadRequest` distinguishes
whole-parent requests from part requests. Classification returns ready, wait or
bad request; it handles evicted requests, completed-parent rollover, finite
lookahead and completed presentations. Unknown future part requests beyond an
unfinished parent, or across an entirely unpublished intervening parent, fail promptly because this inventory cannot establish their
cross-parent distance; this is a conservative admission policy, not a claim
that the protocol forbids every such request. It owns no tasks, timers, media bytes or
mutable runtime catalog. The daemon must still implement identity fencing,
bounded waits, HTTP parsing and final response admission. Those protections
are not proved by these tests.

Executed on Rust 1.97.1:

```bash
cargo test -p plurx-core --lib transcode::low_latency::tests --offline
```

Eight passed, zero failed. This is a protocol-only test and does not claim
replicated-storage coverage. Workspace Clippy with `--all-targets -- -D warnings` passed on the new
tree. The eight tests were rerun after lint corrections and passed again.
The four documentation-index tests also passed.

The next runtime changes are proposed, not present:

| Existing owner | Proposed integration |
|---|---|
| `crates/plurx-core/src/fmp4.rs` and `transcode/mod.rs` | Produce/assemble bounded parts while preserving encoded samples, init transforms, timing and parent boundaries. |
| `crates/plurxd/src/copyseg.rs` | Atomically write append-only parent byte ranges, charge writes and notify the publication owner; never expose a half-written fragment. |
| `crates/plurxd/src/transcode/rolling/{publication,session,segment_index,flow}.rs` | Attempt-bound part inventory, separate startup/steady coverage, legal publication and retention, exact byte accounting. |
| `crates/plurxd/src/http/hls/{playlist,response}.rs` and media-serving owner | Parse delivery directives, bounded blocking reload/preload, capability auth, part GETs and exact response admission. |
| `crates/plurxd/src/media_sessions.rs` and `http/internal_media_sessions.rs` | Preserve directives, part identity, cancellation and deadlines through peer relay. |
| `crates/plurxd/src/web/player/player.js` and `web/pages/settings-developer.js` | Native/hls.js qualification, requested-position behavior, actual progress telemetry and advisory Developer activation. |

Runtime sequence to review:

1. Create the attempt with frozen delivery policy and its existing owner and
   source/decoder contract.
2. Establish the init and decodable origin; write validated parts under charged
   scratch, then publish their immutable inventory.
3. Release a first playlist when actual contiguous audio/video coverage after
   the requested position satisfies the chosen startup requirement. First-frame
   and advancing-frame measurements remain separate.
4. Block subsequent reloads/preloads on that exact attempt's new inventory,
   cancelling promptly on stop, replacement or owner loss.
5. Keep filling toward the steady buffer under existing demand and capacity
   control. Do not expire healthy playback simply because reserve grows slowly.
6. Retire parts through explicit grace and release every retained/write/body
   charge. A new attempt cannot inherit old publication authority.

## 9. Measurements — what the current experiments actually establish

The production Wicked attempt took **11,019 ms** to first frame. Its live create
request took 2,520 ms; the initial rolling snapshot waited for 48 seconds of
post-position media. The complete phase breakdown is in the
[PR #745 version of the measurements](http://forge.lan:3000/noirr/plurx/src/commit/f6c74feef47df4a2d9a1e925122c092279a53da4/docs/streaming/PLAYBACK-STARTUP-LATENCY-MEASUREMENTS.md),
not reconstructed from the synthetic probe.

On this Mac, the approved three-minute excerpt and real daemon/shipped Safari
web UI measured 3,580 ms with the production 48-second bootstrap and 3,093 ms
with a temporary 32-second candidate. Both reached the excerpt's end without
a recorded post-start stall. This single local pair is not a fleet result;
the experimental binding was restored and is absent from PR #745.

The new **synthetic** feasibility probe is a separate loopback HTTP/1.1 Python
server and generated H.264/AAC fragmented MP4. It is not the daemon or Wicked.
It releases half-second parts at simulated 1.2× production, initially serves
four seconds of media, advertises a 0.550-second part target, and records native
Safari requests, buffered ranges and frame callbacks. The first complete parent
is two seconds. Each row is one exploratory run, not a statistical benchmark.

| Probe | First frame at media time zero | First observed advancing frame | Coverage reported at advancing frame |
|---|---:|---:|---:|
| 16-second parent target, byte ranges, start-offset tag | 3.403 s | 28.421 s | 34 s |
| Same, without the start-offset tag | 3.409 s | 28.443 s | 34 s |
| Valid 6-second parent geometry | 3.386 s | 11.752 s | 14 s |
| 16-second target, three short opening parents | 3.959 s | 31.760 s | 38 s |

Safari issued part GETs and `_HLS_msn`/`_HLS_part` blocking reloads. That proves
request behavior only. The static first frame followed by a long pause fails
the intended outcome. The probe logger also reported a temporary no-space-left
error later in the session; these retained exploratory traces are not a clean
end-to-end environment qualification. Changing byte-range handling or the start-offset tag did
not remove it. Review of the request trace shows Safari consumed one parent of
parts, then switched to `_HLS_msn` without `_HLS_part`, correctly waiting for
a complete parent. The first advancing frames coincide with the opening
coverage plus twice the declared target. This is an observed correlation, not
proof of Safari internals. HTTP/2/profile qualification,
packaging, native buffer heuristics and prototype correctness must be isolated
before claiming a production design works. These results do not prove LL-HLS
itself cannot meet the goal, nor prove HTTP/1.1 caused the pause.

Local evidence root: `~/plurx-startup-evidence-20261002`.
The synthetic probe and event files are under `ll-hls-probe/`; core test output
is `ll-hls-core-tests.log`. The probe has exploratory simplifications and must
not be shipped as the production server. No private media is in this document.

## 10. Questions for the reviewer

Please return blocking findings with file/section references, failure scenarios
and the smallest proof needed to resolve each. Separate a code defect from an
unimplemented requirement or an experiment that has not yet passed.

1. Does PR #745 preserve claim fencing, source-I/O limits, maintenance fairness
   and cancellation on both Store backends? Is its limited outcome stated
   accurately enough to avoid calling the original startup incident repaired?
2. Are the pure reload decisions correct at completed-parent rollover, eviction,
   an empty current parent, EOF, integer limits and malformed directives? Must
   the handling of a part directive without a media sequence differ at EOF?
3. What observation would distinguish the native pause's cause? Is the probe
   faithful enough to guide packaging, or must transport/media defects be fixed
   before using its timings? Part requests alone are not acceptance.
4. Can proposed part and parent construction preserve the exact encoded sample
   sequence, audio alignment, HDR/Dolby Vision and open-GOP leading pictures?
   Where must bytes remain unpublished until a future boundary is known?
5. Is the proposed attempt-fenced delivery/resource lifecycle complete for
   blocking reloads, preload hints, relays, owner changes, cancellation, partial
   writes, retention grace and source exhaustion?
6. What startup/part targets are justified by real native behavior and the
   admitted production/consumption range? Neither four seconds nor a subsecond
   part target is selected for production by this packet.
7. Are the task boundaries and acceptance matrix sufficient before activating
   the Developer switch or graduating it? Identify required physical/client
   evidence explicitly rather than accepting a synthetic run as fleet proof.

## 11. External review response — 2026-10-02

Claude's review requests changes to P1 and blocks P2. The recommended order is
B1/B2 on PR #745, protocol correction and branch backup, classic HLS control,
then a transport decision. No new LL-HLS PR is requested.

| Finding | Disposition and evidence required |
|---|---|
| B1: contention consumes timeout attempts | Repair follow-up tracks observed playback contention for the entire hash. Its timeout becomes uncharged, including after playback ends. Idle-only timeouts remain charged. Production bandwidth and stall impact remain unmeasured. |
| B2: viewer wording | Corrected to any unexpired pending playback waiter on the request, without asserting node/playback affinity. |
| L1: parts fetched without advancing playback | Accepted. Initial static frames track the synthetic four-second publication gate. Run classic target/parent 2 s, trusted TLS+h2 LL, then declared 16 s/actual 6 s. Retain per-run free space and server errors. |
| L2: short target dismissed too early | Reopened before P2. An index must prove full-source boundary coverage; MKV Cues alone require coverage and random-access validation. Preserve leading pictures and sample identity. |
| L3: missing phase budget | Proposed allocations below. Attribute Store calls on media1 separately; current measurements overlap and must not be added as disjoint phases. |
| L4: moving edge can skip content | Runtime blocked on an explicit edge/position contract and physical-client acceptance below. |
| L5: transport choice | Paul selected verified short-segment startup on the existing LAN URL; native LL-HLS stays experimental. Trusted TLS+h2 testing and production transport changes are deferred. |
| L6: protocol and clock geometry | Correct ENDLIST precedence and last-complete-parent bound; reject unknown future part geometry across unfinished parents. Derive part targets from integer video clocks plus a bounded audio-frame allowance; validate actual muxed spans. |
| L7: byte-range representation | Prefer append-only parent bytes. Publish a part range only when complete; parent finalization cannot rewrite advertised bytes or offsets. Use complete moof/mdat fragments and prove sample/decoder continuity. |
| L8: temporary-only branches | Push the protocol and effort branches after local validation, without opening a PR. |

**Retry recovery.** Existing terminal analysis generations are not reopened by
ordinary repeated playback. Explicit forced preparation creates a fresh
generation; source/pipeline identity changes can create new work. PR #745 must
prevent contention from spending the allowance rather than relying on recovery.
It does not silently reset existing terminal rows. The existing
`analysis_admin_retry_resets_attempt_budget_through_dyn_store` regression was
executed against the previously built SQLite/three-voter Hiqlite contract
binary and passed: five charged attempts reach `attempt_limit`, admin retry
creates a successor at zero attempts, and its first claim succeeds. The Store
code is unchanged by the follow-up. Store observation errors
remain fail-closed until a separate bounded tolerance design is justified.

**Holdback precision.** The specified minimum for HOLD-BACK is three target
durations; initial-position guidance is a client SHOULD. Neither statement by
itself mandates a universal server gate of exactly 48 seconds before any
playlist can be served. The existing 48-second gate is conservative policy for
a 16-second target. Changing it requires measured continuity, not just a spec
interpretation. The excerpt's 0.876-second maximum packet-keyframe gap is local
sample evidence only.

### 11.1 Proposed end-to-end budget — acceptance, not a measurement

Target: click to continuously advancing video at or below 4.0 seconds for the
qualified local-network case. Record p50 and p95 over repeated cold and warm
starts; do not substitute a single successful run.

| Non-overlapping boundary | Proposed allocation |
|---|---:|
| Click to route/create complete, including Store and control transport | 1.0 s |
| Create complete to first usable manifest, including remaining producer work | 1.0 s |
| Manifest attach to continuously advancing native video | 2.0 s |

Production work may overlap create; trace spans must retain their causal
boundaries. The measured 2,520 ms create request (33 local reads, 6 mutations),
3,574 ms gated-inventory-to-snapshot wait, and about 2.3 s attach-to-frame are
separate diagnostics. Do not add the create interval to an overlapping route
interval. Instrument each Store operation's count, elapsed time and phase
before attributing the live/local gap to a particular database or network hop.

### 11.2 Position and resource requirements before P2

The current rolling movie is not a wall-clock live broadcast. An LL client must
never seek forward or increase playback rate just because production reaches
2× or the server's steady reserve grows. Runtime design must freeze playlist
type and the requested start position at creation. Investigate an append-only
EVENT presentation with `EXT-X-START` bound to the requested position; this is
an experiment, not yet an approved production choice.

While advertising LL parts, provisionally cap the published edge ahead of the
observed playhead to the selected startup runway plus one part target. Produced
bytes beyond that edge belong to the charged private reserve. Pause or stale
playhead observation must freeze edge advancement. Prove that this bound works
with native holdback/start behavior before committing it to runtime. Define a
legal transition to ordinary parents; do not remove PART-INF mid-attempt without
client and protocol evidence. Restarting or skipping content is not an exit plan.

Blocking reload and preload requests have explicit per-attempt and per-host
limits, share cancellation/owner fences and return 503 after at most three
parent target durations. Capacity rejection and cancellation never spend a media
preparation retry. Measure control/telemetry responsiveness with two players.

Acceptance includes continuous frame timestamps and requested-position error,
actual playback rate, no omitted source intervals under 2× production, pause,
seek and replacement. Test native Safari, hls.js with LL enabled, a physical
Apple TV or iPad over the selected transport, and ExoPlayer on Android. The
Mac synthetic probe cannot qualify those other clients.

### 11.3 Classic control result

A clean HTTP/1.1 run of the same generated H.264/AAC media with no PART-INF,
PART, SERVER-CONTROL or PRELOAD-HINT tags used target 2 s and actual parents
2 s. The unchanged synthetic initial gate released four seconds of media at
1.2× production. The first static frame arrived at 3.409 s; the first advancing
frame arrived at 5.459 s with [0, 6] buffered. Playback ended at 65.398 s for the
60-second source, with no recorded waiting event after advancement began.
Reported playback rate stayed 1.0. The server error log was empty and free
space was recorded before the run (21.7 GB decimal) and checked afterward.

This agrees with opener + 2×target coverage for this fixture, independently of
parts. It supports evaluating short-target native startup. It does not prove a
universal Safari buffering rule or qualify Wicked's copy/open-GOP path. The
four-second publication gate and simulated production rate remain experiment
inputs, so 5.459 s is not a production latency estimate.

### 11.4 Create-path attribution from retained production timestamps

The existing counter records total handler time, not per-operation Store
latency. Therefore 33 local reads and six mutations do not prove that Store
round trips consumed 2.52 seconds. No authority reads were counted.

| Handler interval, UTC 19:16 | Elapsed | What is measured |
|---|---:|---|
| 22.166 inferred handler start → 22.473 catalog start | ~307 ms | Setup before catalog discovery; individual call timings absent |
| 22.473 → 22.614 | 141 ms | Catalog discovery |
| 22.614 → 23.008 inferred transcode-create start | ~394 ms | Selection, admission and placement work; individual timings absent |
| 23.008 → 24.231 | 1,223 ms | Transcode create, including the 448 ms refused VOD attempt and rolling fallback |
| 24.231 → 24.686 handler completion | ~455 ms | Remaining ownership/publication/response work; individual timings absent |

The inferred starts subtract logged elapsed durations from their endpoint;
rounding applies. These adjacent intervals partition the 2,520 ms request.
The 448 ms VOD refusal is nested within create and must not be counted twice.
Attributing the remaining 1,156 ms outside transcode create requires finer
phase timing on the live host; current logs cannot identify one slow Store
operation. This packet does not claim a database root cause or a deployed fix.

### 11.5 Wicked boundary qualification — the short target is not yet safe

After Paul selected the existing-LAN short-target direction, a read-only
`ffmpeg -bsf:v trace_headers` pass classified the approved excerpt's HEVC
packets. It found 4,325 packets, 222 IRAP packets, 39 IDR packets and 366
RASL packets. The largest IDR-to-IDR timestamp gap was 28,821 ms. The packet
clock is milliseconds; the first trace records duration 41. There were two
null-muxer non-monotonic-DTS warnings near the excerpt origin, so this pass is
NAL/boundary evidence, not a decode-continuity certificate.

The earlier maximum keyframe gap of 0.876 s therefore does not establish a
0.876 s gap between clean random-access boundaries. The existing copy segmenter
explicitly avoids open-GOP cuts to preserve leading pictures; its ceiling cuts
are already a known compromise. A two-second ceiling would need proof that it
does not introduce additional loss. Cues or packet keyframe flags cannot alone
supply that proof. This is source-quality evidence against blindly applying the
short target to Wicked, not a reason to reject short targets for qualified
sources.

Next qualification must use the segmenter's actual clean-boundary semantics,
including CRA/RASL relationships, and compare output sample/decode continuity
through each proposed cut. Sources without sufficient complete-source proof
retain the current target. Native LL remains experimental as Paul requested;
no certificate setup, production target change or deployment occurred.

### 11.6 Review follow-up validation

The corrected pure module passes eight focused tests on Rust 1.97.1, including
EOF precedence, completed-parent MSN limits, conservative rejection beyond an
unfinished parent, 24000/1001 cadence and AAC skew. The duration constructor
accepts source clocks; it does not select a production target or establish
variable-frame-rate bounds. Four documentation-index checks pass. No new tasks,
waiters, timers or runtime activation were introduced in this branch.

PR #745's follow-up passes the focused timeout-policy test, busy/idle wake and
hash cancellation regressions, pinned all-target daemon check, workspace
Clippy, seven ownership checks and four documentation-index checks. Independent
review found no Rust correctness issue. The timeout test covers sticky policy
classification; a complete resolver timeout and durable attempt-refund cycle
was not newly exercised. Host/path privacy aliases are explicitly labeled to
preserve provenance. Remote gate results and merge status must be checked on
the updated head; the earlier head's success cannot qualify this follow-up.

### 11.7 Declared target isolated from actual parent length

A subsequent clean HTTP/1.1 LL-tagged probe kept TARGETDURATION at 16 s while
using actual six-second parents after the two-second opener. Safari requested
12 parts (one parent), showed a static frame at 3.419 s, and first advanced at
21.770 s with [0, 26] buffered. The 60-second source ended at 81.718 s, with
no waiting event after advancement. Server error output was empty; free space
was recorded before the run and remained about 15.1 GB afterward.

Compared with the target-6/parent-6 result (11.752 s, 14 s coverage), this
isolates a material effect of the declared target. However, 26 s is neither
2 + 2×16 nor 2 + 2×6. The exact two-target rule that fit the original four
rows is therefore not a general explanation. The remaining native threshold
is unproven. This HTTP/1.1 run is a diagnostic control, not native LL profile
qualification; the trusted TLS/h2 experiment stays deferred by Paul's choice.

### 11.8 PR #745 gate follow-up

The prior full Rust gate on the original repair tree failed the SQLite
transaction census: it still named `claim_analysis_request_compatible` after
the concrete implementation moved to `claim_analysis_request_for_capacity`.
The inventory entry now follows the actual method; transaction mechanism and
shape remain unchanged. The focused `hiqlite-store`-enabled
`every_sqlite_transaction_site_is_classified` regression passed. Independent
review confirmed that no other transaction census entry needed changing at
that revision.

That run also failed 14 broader daemon tests, chiefly the ten-second create
allowance under the full runner load, plus provider and telemetry deadlines.
All 14 failed daemon cases passed in focused local execution (17 tests total
including three neighboring cases). This does not reproduce the runner's full-suite contention or establish a flaky-test root cause; the fresh
required gate remains necessary. No source merge or deployment is claimed by
this packet. The LL contract branch and effort base are backed up on Forgejo;
no LL pull request was opened.


### 11.9 Full-suite failure diagnosis — code cost and local prerequisites

The fourteen daemon deadline failures were not established as environmental
failures. Ten also occurred on the earlier `main` coverage run, which proves
that they predated this PR, not why they failed. A full local run on repair
base `bb1bc4c91` passed 1,391 core tests and 191 Store contracts. Thirteen of
the fourteen daemon cases passed; overlapping creates failed again. Preserving
both errors in that assertion showed two `startup_timeout` responses. The
three-second replacement-gate refusal was considered and ruled out for this
reproduction.

SQLite admission synchronously calls session maintenance. Its first three
updates compile the session trigger graph even when no rows qualify. In a
16-thread HLS group, two overlapping creates spent 7,353.793 ms combined on
those six no-op updates while sharing one writer connection; individual
updates took 1,089–1,500 ms. The same updates took roughly 33–36 ms each in
isolation. The instrumented timing run passed all 234 cases; the preceding
concurrent diagnostic run failed the overlap case. The measurement identifies
avoidable cost under contention, not a deterministic ten-second threshold.

The repair checks each update's candidate predicate inside the existing
SQLite transaction before preparing that update. Update order, deadlines,
batch bounds and the other cleanup statements stay intact. A trace regression
checks an empty store and a current session, then proves that an expired
session is still retired. The transaction census now records this method as
`ReadBranchWrite`. The replicated backend already checks for idle maintenance;
this SQLite measurement does not establish a speedup on the activated cluster.

The first full local daemon run also found thirteen font prerequisites and
two socket-test failures. The default Mac FFmpeg lacks subtitle rendering;
installed full builds render but do not provide the Fontconfig trace this
suite requires. The socket helper had a separate defect: `read_to_string`
discards received response bytes when a later read returns a reset. Reading
bytes before UTF-8 conversion preserves the response, and both unchanged
socket assertions pass. No timeout or success assertion was relaxed.

Independent review found no actionable issue with these changes. The new
SQLite regression, transaction census, four expiration/drain/session
contracts and four documentation-index checks pass. The complete concurrent
daemon rerun finished with 3,115 passed, 13 failed and 16 ignored in 447.74
seconds. All fourteen original daemon failures and both socket regressions
passed. The thirteen remaining failures are the local font prerequisites
described above. The normal commit hook passed catalog lint, formatting,
workspace Clippy with warnings denied and served-JavaScript syntax. The
required Linux fast lane remains pending. The user authorized merge after
that lane passes, with remaining configured checks monitored afterward;
no source merge or deployment is claimed here.

Evidence under the aliased local root above: `unit-failure-classification.json`,
`full-unit-bb1bc4c91-no-incremental.log`, `hls-concurrent-diagnostic.log`,
`maintenance-diagnostic-concurrent-timings.log`, `maintenance-measurement.json`,
`unit-repair-store-contracts.log` and `unit-repair-full-daemon.log`.
