# Browser HDR restart — preserve normal retirement through the DV adapter

**Status:** open · **Updated:** 2026-10-19 · **Owner:** Codex ·
**Branch:** `codex/browser-session-retirement` · fixes implemented locally;
deployment and acceptance open.

Companion to [DV processing contracts](DV_HDR_M1_CONTRACTS.md) and
[terminal-event ownership](../playback-control/PLAYBACK-CONTROL-PROTOCOL-M3-TERMINAL-EVENTS.md).
This record distinguishes reproduced browser failures from the unresolved
Safari startup and stale Activity observations. It does not replace the
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

## Safari startup and the original stale entry remain separate evidence gaps

The original Safari attempt selected native HLS with Profile 7 to Profile 8.1
conversion. Its playlists passed readiness checks, but no first frame was
logged. A later same-title Resume produced a first frame in 2.6 seconds and
Close released it cleanly. That does not diagnose the original hang.

The original enhanced Chrome session eventually expired through the idle
reaper. Its missing terminal log does not establish whether DELETE was absent
or refused. The explicit-Close reproduction above did release successfully.
The independent missing `pagehide` cleanup fixed in `c8cf1ee76` is useful, but
does not explain an operator who remained in the document and used Close.
Neither original observation is declared fixed by this lifecycle change.
