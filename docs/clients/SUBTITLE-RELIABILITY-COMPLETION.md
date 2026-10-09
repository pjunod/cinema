# Subtitle reliability — completion ledger

**Status:** active; fixes built locally, integration and remaining acceptance open.  
**Updated:** 2026-10-09 · **Base:** `1088d7529` · **Integration:** `effort/cinema-subtitles`

Cinema subtitles fail across clients: selection may produce nothing, cues can
arrive late, and a selected track can stop displaying. WebVTT is already
implemented; the remaining defects concern when its bytes become available,
what playback engines retain, and which subtitle timeline they follow.

This ledger owns the current repair and reconciles the older subtitle plans.
It supersedes their stale build/PR status, not their unearned acceptance.
The user authorized testing on available hardware except the physical Apple TV
and iPhone 18. Neither excluded device is used for this effort.

## 1. Confirmed defects and repairs

| Defect | Evidence | Repair |
|---|---|---|
| All three readiness reporters require seeing warming before ready | Actual macOS AVPlayer buffers five empty VTT segments; old source observes ready first and renders zero cues for 22 seconds while video advances | Retry the first ready for the current selection/seek intent, then suppress repeated identical ready observations |
| Ready window and ready complete track are indistinguishable | Focused server test publishes a window, a different window, then the complete track without a warming edge | Optional opaque `delivery.subtitle_revision` identifies the source/track/window representation; a new revision authorizes one retry |
| Media3 off/on retry can be coalesced before the playback thread consumes it | Physical Android: old retry fails 2 of 5 runs, five empty reads, zero ready reads, text still selected and video advancing | Wait for `onTracksChanged` to acknowledge text deselection; then restore the still-current selection. Cancellation/timeout cannot strand the current intent disabled |
| Web disables native captions while waiting for a whole-track sidecar even when only a window is ready | Recovery path inspection and browser regression | Retain the native rendition until usable script cues arrive; deduplicate pending requests and fence completion by intent |
| Subtitle playlists discard video discontinuity tags | New takeover/sliding-playlist regression fails against the old generator | Preserve discontinuity sequence and every segment boundary in the VTT rendition |
| A session can join the wrong extraction after a language/source change at the same playhead | New regression holds the old window flight, changes track or source stamp, and observes the new publication | Include complete cache identity in the session window owner; displace obsolete work |

The first-ready mechanism is demonstrated against the old and new Apple
source; Media3's additional race is demonstrated on a physical Pixel Fold.
These findings explain concrete failure modes across clients. Retained fleet
logs do not identify every selection from the reported evening, so this is
not a claim that every reported failure had only one cause.

`subtitle_revision` is an optional, opaque string. Clients compare equality
only. Absent and unknown readiness values never imply readiness. Old peers
may omit the revision; first-ready recovery still works. The server observes
cache state without starting extraction from a control request. A full-track
revision stays stable as the playhead crosses windows. Off, newer selection,
newer seek, replacement and teardown continue to fence delayed work.

## 2. Local evidence

| Check | Result |
|---|---|
| Rust 1.97.1 baseline and modified `cargo check -p plurxd --all-targets --offline` | pass |
| Pinned `cargo clippy -p plurxd --all-targets --offline -- -D warnings` | pass |
| `cargo test -p plurxd --bin plurxd --offline subtitle` | 178 passed |
| Apple playback-control reporter/session XCTest on iPad simulator | 87 passed |
| Android playback-control, subtitle-policy and acknowledgment JVM tests | 177 passed |
| `node tests/playback/web-control.test.js` | 39 passed |
| `scripts/subtitle-readiness-browser-check` with real Chrome/hls.js | zero cues before readiness; visible cue after; same video element and session |
| `scripts/subtitle-readiness-native-check` with actual macOS AVPlayer | old source fails with zero cues; fixed source refetches and continues through cue 21 without replacing the item |
| `SubtitleReadinessRenderingTest` on physical Pixel Fold, isolated application | old retry fails 2/5; acknowledged retry passes 10/10 |

Reproduce the native fixture with `python3 scripts/subtitle-readiness-native-check`.
For Android, start that command with `--serve`, reverse its printed localhost
port with ADB, and pass `-e subtitleFixture http://127.0.0.1:PORT` to the
instrumentation runner. Build with `-PplurxIsolatedBudgetProbe=true` and install
only the `capabilityProbe` application and test packages. The fixture creates
synthetic media, uses no library credentials and does not replace the viewer's
installed Cinema application. The test is skipped without an explicit fixture.

The synthetic harnesses prove decoder/renderer recovery. They do not replace
an end-to-end test of a deployed server, UI selection, seeks and quality
handoffs against actual titles. AVPlayer testing also showed that answering
503 while captions warm can stall video; a blanket 503 change is not a fix.

## 3. Project reconciliation and remaining work

| Work | Current state | Completion requirement |
|---|---|---|
| [Original reliability handoff](SUBTITLE-RELIABILITY-HANDOFF.md) | Earlier implementation is on main; residual defects above were missed | Integrate repairs; repeat selection/off/seek/quality cases on available clients |
| [Cluster extraction](SUBTITLE-CLUSTER-EXTRACTION-STATUS.md) | M0–M5 built in #507 | Fleet cold/warm/off/slow, peer hydration and backfill observations |
| [Parallel ranges](PARALLEL-SUBTITLE-RANGES-STATUS.md) | #517 landed as `38f61dfe6`; its pending-PR status was stale | Current/next ranges and complete-track takeover stay visible without a video restart |
| [Online downloads](../features/SUBTITLE-DOWNLOADS-IMPLEMENTATION.md) | #498 landed as `0e2c3fd47`, implementation `843ed9bb5`; not an unbuilt feature | Provider configuration/quota smoke where configured; durable caption playback on clients |
| [PGS startup and overlay](PGS-SUBTITLE-START-PATH-RCA-AND-PLAN.md) | Two-device startup bar passed September 24; broader corpus still open | Color/geometry/alpha, intervals, active-cue seeks and late data across clients |
| [Mac E2 subtitle processing](../streaming/MACOS-VIDEO-PROCESSING-STATUS.md) | CPU composition after native processing is integrated; HLG text/bitmap checks exist | Reconcile E2 acceptance; GPU composition is conditional on measured benefit, not a required second renderer |
| [Offline transcription](../performance/PERF2-PLAN.md#102-subtitles-on-demand--the-whisper-queue) | Genuinely unimplemented; separate from online downloads | Bounded offline command job, Stop/lease ownership, durable caption provenance, settings and tests |
| [Physical verification](SUBTITLE-RELIABILITY-PHYSICAL-VERIFICATION-PROMPT.md) | Historical cases remain useful; build numbers and PGS refusal expectations are stale | Record each current case separately; excluded hardware stays untested |

Task branches target the effort. The first task owns shared WebVTT serving,
readiness/retry code, regressions and this reconciliation. Later tasks start
from its integrated result. Every task requires its focused regressions and
the manually dispatched Effort development gate. Main promotion requires the
current main merged into the frozen effort, full promotion qualification and
the exact-tree receipt. No production deployment or completion of the whole
backlog is claimed by local test results.
