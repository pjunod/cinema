# Browser HDR restart — preserve normal retirement through the DV adapter

**Status:** open · **Updated:** 2026-10-19 · **Owner:** Codex ·
**Branch:** `codex/browser-session-retirement` · fixes implemented locally;
deployment and acceptance open.

Companion to [DV processing contracts](DV_HDR_M1_CONTRACTS.md) and
[terminal-event ownership](../playback-control/PLAYBACK-CONTROL-PROTOCOL-M3-TERMINAL-EVENTS.md).
This record distinguishes reproduced browser failures from the unresolved
original stale Activity observation. It does not replace the
[processing project ledger](DV_HDR_PROCESSING_STATUS.md).

## The same-tab Close and Resume sequence reproduces the HDR downgrade

The operator reported enhanced HDR10 with FEL/RPU on the first Chrome play,
ordinary HDR10 after stopping and restarting, a Safari start stuck preparing,
and stale Activity entries. The operator clarified that Chrome stayed in the
same tab and playback was stopped with the player Close button.

Read-only server inspection identified deployed source `e940b75b4`. A second
test against that unchanged deployment reproduced Close followed by Resume in
the same Chrome document. Private captures and the screenshot stay outside the
tracked tree. The decisive events on 2026-10-19 were:

| Time (UTC) | Observation | Conclusion |
|---|---|---|
| 20:18:46 | Chrome started enhanced HDR10 with an accepted processing report. | The initial FEL/RPU processing was real. |
| 20:19:18.674 | Close ended the enhanced session with durable cause `Deleted`. | This reproduction successfully released the session. |
| 20:19:18.676 | The producer reported `no child is attached to this producer slot` as a DV failure. | Expected retirement was classified as failed processing. |
| 20:19:22 | Same-tab Resume was refused processing because it had previously failed for the playback/source/settings/backend. | The shutdown observation poisoned processing eligibility. |

Earlier captures also establish an independent Auto defect: a matching catalog
refresh clears the running processed candidate ID. The next control exchange
then requests unnamed Auto and can stage an ordinary HDR10 copy despite no
viewer selection change. Commit `c8cf1ee76` retains the exact compatible
processed recipe through that refresh.

## The graph adapter loses a distinction the lifecycle already makes

The [processing build contract](DV_HDR_PROCESSING_BUILD.md#5-m1m3--processing-identity-and-safe-lifecycle)
places helpers inside the existing producer/generation lifecycle. Ordinary
VOD already treats a vanished session as normal completion; terminal ownership
rejects late observations after retirement.

The failing path is in the DV adapter:

```text
Close -> durable session end -> exact registered child retirement
                                      |
                                      v
                        in-flight private-window observation
                                      |
                          missing child -> string error
                                      |
                      runtime and generation mark backend failed
                                      |
                           next HDR10-E start excluded
```

The helper supervisor also retires the encoder after genuine renderer,
source-validation or evidence-write failures. A retirement token therefore
cannot, by itself, identify the cause. Treating every retired encoder as
cancelled would erase real backend failures and is not a valid repair.

## Preserve one result through the existing owner

1. **Keep process ownership in the existing registration.**
   [`ProducerRegistration`](../../crates/plurxd/src/prodrun.rs) identifies the
   exact child; its retirement token fences publication. The registered
   writer barrier and confirmed reap retain custody and capacity until cleanup
   finishes. A processing fix does not introduce another reaper or infer
   successful production from a reap receipt.
2. **Preserve the helper supervisor's causal result before cleanup.**
   [`dv_segment`](../../crates/plurxd/src/dv_segment.rs) owns one authoritative
   typed completion: completed, intentionally cancelled, or failed with its
   original cause. A separate mutable failure flag beside a completion channel
   would create two authorities and is rejected.
3. **Carry cancellation as a normal outcome.**
   [`dv_runtime`](../../crates/plurxd/src/dv_runtime.rs) must return cancellation
   without minting failed-backend evidence. Genuine helper, encoder, timeout
   and validation failures retain their causes. Classification must not depend
   on matching the text of an I/O error.
4. **Respect the generation's terminal fence.**
   [`vod/generation`](../../crates/plurxd/src/vod/generation.rs) must not let
   a late completion fail a closed, replaced or cancelled generation. Apply
   the existing generation-epoch and lifecycle checks before publishing a
   prepublication failure, as the postpublication path already does.
   Runtime execution reports outcomes without mutating the failure ledger.
   The current VOD generation records an episode failure only after its
   lifecycle fence accepts it; initial planning records a genuine backend
   failure only while its own preparation remains current. Cancellation never
   enters that ledger.

Cache invalidation, badge retention, retrying enhanced processing regardless
of failures, and increasing idle timeouts do not repair this lost lifecycle
meaning. Genuine failed candidates must remain excluded within their existing
recovery episode.

## Qualification must exercise the actual completion and cleanup path

The regression boundary includes the real helper supervisor, owned child,
private-window capture and registered reap. Constructing a cancellation flag
and testing a cache predicate alone does not qualify the change.

| Case | Required result |
|---|---|
| Deliberate retirement during capture | Normal cancellation; no producer failure or failed-backend evidence; child and writers settled; next start eligible. |
| Helper fails and retires its encoder | Original helper failure retained; failed-backend evidence recorded. |
| Encoder exits unsuccessfully | Encoder failure retained. |
| Deadline expires while the generation remains live | Timeout remains a real failure. |
| Late result after replacement or Close | Cannot change the winning terminal cause or fail its successor. |

Validate the Linux-only adapter with pinned Rust 1.97.1, focused regressions,
all-target compilation and Clippy. Archive committed source without `.git` or
credentials to the compiler host, then check the final committed candidate.
Passing synthetic process tests is not physical playback acceptance.

## Recovery must preserve evidence of the original fault

The operator requires stronger error visibility as successful fallbacks make
faults harder to see. Playback success alone is insufficient acceptance.
Use the existing structured diagnostics pipeline to retain the original fault,
the recovery decision and the resulting delivery, correlated by playback,
session and generation identity where available. Expected retirement,
resource refusal and actual backend failure must remain distinct. A recovered
fault retains its cause; normal Close retains its lifecycle event without
becoming a producer failure. Focused regressions must check this trail as well
as recovery behavior. Failed session-release requests are part of the audit.

The browser audit found two silent paths in
[`directed-change.js`](../../crates/plurxd/src/web/player/directed-change.js):
session DELETE ignored rejected promises and non-success HTTP responses, and
the quality-recovery callback ignored synchronous and asynchronous exceptions.
Both now report failures through `clientLog`, capturing the originating
session, attempt and file before asynchronous work. Safe typed error names,
codes and HTTP status survive; arbitrary exception text and response bodies
are excluded because they can contain delivery credentials.

The server receives an outgoing failure even after replacement. The local
diagnostic mirror accepts it only for its current owner; handleless direct and
progressive playback use the matching attempt and available file/attachment
identity. Regressions check both current handleless faults and late outgoing
faults, as well as successful cleanup remaining quiet. A recovered playback
keeps its original warning, and failed recovery adds its own error.

## Safari startup mistakes an unowned pause for viewer intent

The original Safari attempt selected native HLS with Profile 7 to Profile 8.1
conversion. Persisted playback-event transport fields supplied evidence absent
from the abbreviated journal text:

| Time (UTC) | Observation |
|---|---|
| 19:56:43.310 | Playlists ready; an internal Play requests playback. |
| 19:56:43.316 | Native Play event reports the element unpaused. |
| 19:56:43.417 | Unmatched native Pause, before a first frame, changes desired playback from true to false. |
| 19:56:43.425 | A second unmatched Pause leaves desired playback false. |
| 19:56:43.970 onward | Control reports Hold / Seeking at the same resume position, with no runway. |

The shared transport handler infers viewer intent from an unowned media event.
That moves startup to `paused`, where both the native startup deadline and
stall diagnosis intentionally abstain. The result is indefinite Preparing.
No internal Pause command or user Pause appears between Play and those events.
The initial metadata-driven resume seek occurs in that interval; the evidence
does not identify why WebKit paused. A later same-rendition Resume produced a
first frame in 2.6 seconds and Close released it cleanly.

The [input contract](../clients/PLAYER-INPUT-CONTRACT.md) already distinguishes
viewer intent from an element paused during open or reattachment. Apply that
boundary to the shared handler: explicit UI, keyboard and MediaSession commands
remain authoritative throughout startup. Unmatched native events before proven
presentation by the current attachment remain observations, retaining the
original intent and bounded startup owner. Native/PiP event compatibility after
presentation remains supported. This rule depends on attachment ownership and
presentation, not browser name or a grace-period guess. A rejected `play()` call
must retain its causal diagnostic rather than silently becoming viewer Pause.
Its originating invocation remains identifiable after replacement. An
`AbortError` is normal cancellation only when an owned pause or source change
explicitly cancelled that exact pending invocation; a changed player identity
alone does not prove cancellation. Other rejections remain faults, including
late ones. Cause and disposition must use fields retained by server ingestion.

## The original stale Activity entry still lacks a release outcome

The original enhanced Chrome session eventually expired through the idle
reaper at 19:59:55, five minutes after its last client activity. It has no
terminal event, while neighboring sessions have `client_released` outcomes.
Available logs show neither a DELETE rejection nor a successfully applied
release. They cannot distinguish a dropped/unsent request from release of
another handle. Activity polling did not extend its lifetime; the idle cleanup
worked as designed. The explicit-Close reproduction above released cleanly.

Source tracing identified an independently reproducible handle-loss path:
Close snapshots `sessionId`, then `teardownHls` can cancel an unproven prepared
switch and restore its predecessor. Close clears that restored ID while
releasing only the previously captured successor. The predecessor survives
until idle retirement. The common Close lifecycle must capture its bounded
owned session set before cancellation/rollback, drain the decoders through
their existing owners, then release those handles. Page exit delegates to
that lifecycle rather than owning a special second drain. This addresses the
ownership defect for both player Close and document exit; available original
telemetry does not prove that the reported stale session took this path.

## Local qualification and remaining acceptance

The affected browser suites pass: **191 tests**, covering web control, native
startup, surface policy, Close/pagehide, diagnostics and file context. Run with:

```sh
node --test tests/playback/web-control.test.js tests/playback/native-hls-startup.test.js tests/playback/web-policy.test.js tests/web/cinema-remote.test.js tests/web/file-context.test.js
```

The original transport handler fails the startup pause regression, and the
original Close fails the prepared-rollback drain regression. The Linux process
suite passes all eight private-window outcome tests plus the Source generation
registration/Close/cancel resource-retention regression. Pinned Rust 1.97.1
formatting and all-target Clippy pass on the source-only compiler copy;
workspace/all-target Clippy also passes on the native development host.
Static web checks pass with the existing type baseline unchanged.

Deployment and physical acceptance remain open. Recheck same-tab enhanced
Close/Resume, Safari resume-to-first-frame and Close during a prepared switch.
Verify session release and diagnostic causes, not just the visible picture:
normal shutdown must not exclude processing, genuine failed recovery must
retain its originating fault, and no closed session should await idle expiry
when its release succeeds. The original WebKit pause cause and historical
stale-session release outcome remain unproven.
