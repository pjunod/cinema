# Few-second startup — publish parts while the steady buffer grows

**Status:** review packet; protocol primitives implemented locally, runtime path not implemented ·
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

## 1. Outcome and scope

Start rolling playback with a few seconds of playable media after the requested
position, then continue producing and downloading toward the existing steady
buffer. A successful first frame followed by an avoidable startup stall fails
acceptance. Four seconds at 1× is an initial experiment, not a measured guarantee.
Record click-to-first-frame separately from the amount of media buffered.

The immediate reproduction is Wicked in nynuc's web UI on native Safari.
The user approved a three-minute local excerpt for testing. That media and raw
private logs stay outside Git in the existing temporary evidence directory.
The preparation-starvation repair in PR #745 remains a separate change; merge
its reviewed candidate after its required gate and integrate main here later.

Use one integration branch, `effort/few-second-startup`. Task branches start
from its current head and target it. No file-disjoint exception applies.
The user's original checkout is untouched. The working checkout is
`/private/tmp/plurx-few-second-startup-20261002`; Rust 1.97.1 all-target daemon
check passed on main `772b9cf23` before Rust edits.

## 2. Decision and alternatives

| Approach | Decision and reason |
|---|---|
| Lower only the current 48-second publication gate | Rejected for this requirement: the fixed 16-second discovery cadence can exhaust a few-second buffer. |
| Shorten every ordinary segment and its target | Rejected as the general solution: unknown GOPs and open-GOP leading pictures cannot be fixed by changing a duration constant. |
| Publish bounded fMP4 parts and support blocking reload | Selected for implementation and actual Safari qualification; retains larger parent segments and steady buffer. |
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
| P2: producer and delivery | fMP4/copy producer, rolling publication/flow/scratch, HLS HTTP and peer relay | Same samples through parts and parents; bounded waits, cancellation, ownership changes, retention and accounting |
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
their own observations. Production deployment and observed nynuc playback are
separate from merging source. Do not call the original ten-second complaint
fixed before measuring the deployed result.

## 6. Execution record

- 2026-10-02: scope expansion authorized; isolated effort checkout created.
  Rust 1.97.1 `cargo check -p plurxd --all-targets --offline` passed on its base.
- P1 in progress on `codex/ll-hls-startup-contract`. Six pure protocol tests
  pass locally; the module is not called by production. The loopback Safari
  prototype requests parts and blocking reloads but fails early continuous
  playback. No production activation, LL-HLS PR, or deployment exists.

## 7. Existing preparation repair — separate reviewed change

[PR #745](http://192.168.4.7:3000/noirr/plurx/pulls/745) targets main. Its
reviewed/tested tree is `b49a28731c13ad55c9e6f3aed5614efe16a2f6a8`; pushed
head `f6c74feef47df4a2d9a1e925122c092279a53da4` has the identical tree after
preserving a remote main merge. Its base is `772b9cf236e989fbb75ac4d1aa760c0ad632395d`.
At this packet's preparation, policy preflight passed; required Rust/Windows
and aggregate gates were still pending. This is not a merge or deploy receipt.

**Problem proved:** Wicked's current-engine preparation request stayed queued
with `foreground_preempted` while its live viewer was playing. The daemon
required an idle node before claiming or hashing that viewer's source.

| Changed file under `crates/` | Resulting behavior |
|---|---|
| `plurx-core/src/store/fragment_index_cluster.rs` | Shared live-viewer predicate and capacity-aware claim API; existing ordinary claims keep their behavior. |
| `plurx-core/src/store/sqlite/fragment_index_cluster.rs` | Busy claims require an unexpired pending playback-analysis waiter in both candidate selection and fenced update. |
| `plurx-core/src/store/hiqlite_fragment_index_cluster.rs` | Same admission rule on the replicated Store. |
| `plurx-core/src/store/background_jobs.rs` | Authoritative preparation observation includes whether live viewer interest remains. |
| `plurxd/src/state.rs` | Busy nodes can claim and continue source attestation for their live viewer; cancellation and loss of interest stop that exception. |
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
| [transcode/low_latency.rs](../../crates/plurx-core/src/transcode/low_latency.rs) | New pure types and six tests. Local implementation, not wired into a daemon route. |
| [transcode/mod.rs](../../crates/plurx-core/src/transcode/mod.rs) | Exposes the new module; no FFmpeg arguments or production duration constants changed. |
| This document and [docs index](../README.md) | Build/review contract and its index row. |

`PartPolicy` freezes the part and parent duration contract. It rejects zero or
oversized parts and rejects an undersized dependent part unless that part ends
its parent. It deliberately does not choose a production part target.

`ReloadFrontier` borrows a contiguous published-parent inventory and rejects
invalid ordering or a non-final unfinished parent. `ReloadRequest` distinguishes
whole-parent requests from part requests. Classification returns ready, wait or
bad request; it handles evicted requests, completed-parent rollover, finite
lookahead and completed presentations. It owns no tasks, timers, media bytes or
mutable runtime catalog. The daemon must still implement identity fencing,
bounded waits, HTTP parsing and final response admission. Those protections
are not proved by these tests.

Executed on Rust 1.97.1:

```bash
cargo test -p plurx-core --lib transcode::low_latency::tests --offline
```

Six passed, zero failed. This is a protocol-only test and does not claim
replicated-storage coverage. Workspace Clippy with `--all-targets -- -D warnings` passed on the new
tree. The six tests were rerun after lint corrections and passed again.
The four documentation-index tests also passed.

The next runtime changes are proposed, not present:

| Existing owner | Proposed integration |
|---|---|
| `crates/plurx-core/src/fmp4.rs` and `transcode/mod.rs` | Produce/assemble bounded parts while preserving encoded samples, init transforms, timing and parent boundaries. |
| `crates/plurxd/src/copyseg.rs` | Atomically write parts, charge writes and notify the publication owner; never expose a half-written fragment. |
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
[PR #745 version of the measurements](http://192.168.4.7:3000/noirr/plurx/src/commit/f6c74feef47df4a2d9a1e925122c092279a53da4/docs/streaming/PLAYBACK-STARTUP-LATENCY-MEASUREMENTS.md),
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
not remove it; the remaining cause is unresolved. HTTP/2/profile qualification,
packaging, native buffer heuristics and prototype correctness must be isolated
before claiming a production design works. These results do not prove LL-HLS
itself cannot meet the goal, nor prove HTTP/1.1 caused the pause.

Local evidence root: `/private/tmp/plurx-startup-evidence-20261002`.
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
