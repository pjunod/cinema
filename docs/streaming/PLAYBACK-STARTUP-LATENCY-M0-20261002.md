# Native startup incident — evidence and amendments to the existing contracts

**Status:** diagnosis approved by review; amendments proposed for review ·
**Revised:** 2026-10-02 EDT · **Inspected runtime:**
`v0.3.0-5385-g189286828` · **Implementation:** pending

This records the lab6 native Safari failure for reference film / file **120**
and folds the proposed repair into the existing
[Track N contract](NATIVE-HLS-STARTUP-IMPLEMENTATION.md#10-authorized-amendment--reuse-track-n-and-preflight-the-compatible-route)
and [startup-latency contract](PLAYBACK-STARTUP-LATENCY-IMPLEMENTATION.md#11-authorized-amendment--incident-evidence-for-m0m1-and-a-measured-m2-choice).
It supersedes the standalone fast-start proposal. Read the
[review](PLAYBACK-STARTUP-LATENCY-REVIEW-20261002.md) alongside the
[sanitized receipt](PLAYBACK-STARTUP-LATENCY-M0-20261002.json).

## 1. Root cause — ready media was withheld while native readiness timed out

At resume **3273.778 s**, the intended route copied HEVC with Dolby Vision
Profile 7 → 8.1 conversion and converted TrueHD to AAC. Exact VOD returned
`vod_index_pending`, so this attempt selected rolling HLS.

Complete media existed 2.25 s after session registration. The 12 s writer
gate opened with 16.475 s of media at 4.468 s. The rolling first-snapshot
predicate still required the steady 48 s reserve and delayed publication
until 15.113 s, with 56.432 s produced. Native master init inspection was
fenced behind that snapshot but waited under a five-second budget. Two
master requests returned 503 before publication. Safari error 4 then
selected codec rescue; no compatible encode route was available, yielding
`candidate_encode_route_unavailable` / 409.

```text
missing exact index → rolling copy → media ready
                                      │
                             48 s first-snapshot gate
                                      │
                     master init waits → 5 s timeout / 503
                                      │
                         native error 4 → rescue → 409
```

The review independently verified the source claims and log sequence.
This supports a preparation/publication failure and incorrect recovery
attribution; it does not prove a Safari decoder limit or an initial FFmpeg
failure. The existing native-startup contract already addressed this
failure signature for reference film G, but Track N had not shipped at the
inspected commit. My original proposal missed that contract and designed
an overlapping after-error mechanism.

## 2. Timeline — server observations, not click-to-frame measurements

All times below are **2026-10-02 UTC** (EDT = UTC − 4 h). Elapsed values
start at actor-owned copy registration. Timing alone does not synchronize
browser and daemon clocks.

| UTC | Elapsed | Observation |
|---|---:|---|
| 04:14:31.245 | before creation | Daemon start, reported in review; listening at 31.719 |
| 04:14:40.805496 | before creation | Intended copy/DV route selected, 9.56 s after reported daemon start |
| 04:14:43.217789 | before creation | Client preparing file 120 |
| 04:14:47.242127 | before creation | Exact VOD refused after 793 ms |
| 04:14:47.286328 | before creation | `vod_index_pending`; rolling recovery |
| 04:14:49.162748 | 0 s | Actor-owned copy session registered |
| 04:14:51.412305 | 2.250 s | First complete segment, duration 7.8 s |
| 04:14:53.631200 | 4.468 s | Writer gate opens, 16.475 s produced |
| 04:14:56.830505 | 7.668 s | Master init inspection 5000 ms timeout; 503 |
| 04:14:56.895979 | 7.733 s | Native error 4; codec rescue selected |
| 04:14:59.562267 | 10.400 s | Rescue candidate unavailable; 409 in original trace |
| 04:15:01.867270 | 12.705 s | Second master init inspection timeout; 503 |
| 04:15:04.275583 | 15.113 s | First served snapshot, 56.432 s produced |
| 04:15:29.085821 | 39.923 s | Retirement settles with winning reason `idle` |

The review also quotes the corresponding refusal at 04:14:59.629. Retain
both event timestamps; neither changes the sequence. No successful first
presentation for this file was observed.

Achieved origin was **3273.103 s**, 0.675 s before the requested position.
The **48.675 s** first-publication arithmetic applies to an explicit lease
whose accepted position is not advancing. The original receipt did not
retain the lease object; record it in M0 rather than treating that arithmetic
as direct lease telemetry. Under legacy wall-clock budgeting, desired
coverage would instead be about **58.6 s** at this point, so that model does
not explain the observed 56.432 s snapshot. The observed writer-to-snapshot
wait is explicitly logged as **10,589 ms**.

A later comparison film / file 9 presented after **16,114 ms**, via the
direct media-playlist path. This confirms some playback could be served;
it neither qualifies this DV stream nor establishes a warm-daemon control.

## 3. Added context — digest backlog and boot contention

The reviewer inspected a copied replicated database and daemon logs. These
are **review-reported findings**, not a fresh query in this documentation
revision. The source mechanism for executable/dependency digest identity
was rechecked at the pinned commit.

| Digest census in the review | Ready files | Queued files |
|---|---:|---:|
| Previous `953a…` | 1,523 | 1,037 |
| `85e6…`, current since 2026-09-28 in that snapshot | 310 | 909 |

File 120 reportedly had six ready artifacts under old digests and zero
claimed current-digest jobs, with queued and `foreground_preempted`
requests. Playback did not promote them to foreground. This gives M1 a
concrete starting diagnosis: digest keyspace churn plus the observed
promotion/preemption gap. It does not by itself prove which scheduler or
resource rule is defective, and the counts need reproducible queries and
exact recipe identity. Indexed VOD cannot be assumed to be the majority
path in that snapshot.

The digest deliberately covers the executable and loaded dependencies.
Keep that source/engine correctness fence. Investigating a narrower identity
is a separate proposed effort, not a fix authorized here. The review also
reports a September 4 DV conversion failure on a read-only filesystem;
that historical failure does not prove future conversion is impossible.

The incident began about nine seconds after daemon start. The review
reports a 335 MB/s boot storage probe on the source mount, 20 caption-graph
proof encodes during 04:14:34–04:14:59, and 149–171 ms Raft applies. These
may affect the producer and creation timings; they do not invalidate the
verified gate and deadline mechanism. M0 must separate warm-daemon and
immediately-after-restart results.

The decision-to-registration interval was **8.36 s**; the reviewer reports
**2.714 s** for creation itself. Decompose these costs before claiming a
few-second click-to-first-frame result. The measured burst produced media
at roughly **3.75–3.9×**; it is not sustained-capacity evidence.

## 4. Proposed disposition — amend two contracts and preserve the requirement

| Review finding | Change in this revision |
|---|---|
| B1: existing native repair | Track N's pre-source master/selected-child readiness owns recovery. Add only compatible-route preflight before its existing fallback; retain one same-session reload and truthful ambiguous attribution |
| B2: duplicated latency effort | Fold evidence into M0/M1 and keep M2 selection before M3. Retain ordinary HLS scope, the existing candidate sweep and the 12 s writer gate |
| B3: four-second geometry claim | Withdraw the four-second ordinary-HLS default and the unsupported 2–4 s overall acceptance claim. Evaluate ~24 s coverage as an interim candidate; measure continuity |
| B4: index keyspace | Incorporate the dated census and promotion/preemption evidence, with provenance and reproducibility requirements |
| B5: restart window | Require warm and after-restart traces, separately reported |
| N1/N2: lease and rate | State the explicit nonadvancing-lease condition; record actual lease and effective sub-1× bootstrap scaling in M0/M2 |
| N3/N4: extra reader and writer gate | Drop the proposed prepared-init reader; keep writer gate at 12 s for ≥16 s candidates |
| N5/N6: probe and patience | Use Track N's existing master/child readiness, not the incomplete sample probe. Keep native hold tolerance unmeasured; application fetch owns its deadline |
| N7: preparation interval | Add the pre-creation decomposition to M0/M4; remove the unsupported total-startup promise |
| N8: repository hygiene | Review copies use lab6, forge.lan and reference-film naming. Package a sanitized receipt alongside the evidence document |

The original private evidence file exists in this workspace; it was not in
the repository. The proposed repository packet now includes a receipt, so
its evidence link survives landing without relying on the private file.
The baseline contracts are retained with appended amendments; old statuses
and build authorization remain historical context, not a new dispatch.

### 4.1 Ordinary HLS is a measurable interim candidate

With the fixed 16 s target, the server normally publishes again at
`available_at + 16 s`; the early arm requires Rendering plus low runway
and cannot act before +8 s. Native changed-playlist discovery may also wait
16 s. These waits overlap; use the actual worst next-discovery interval,
then add transfer/append and scheduling allowances. Do not sum overlapping
server and client waits or substitute the 250 ms worker poll.

The review expects roughly 20 s usable coverage at 1× to cover the next
reload/transfer, rounding to the third completed segment (~24–25 s) here.
At this incident's burst rate, ~24 s suggests **~6.5 s server readiness**
after creation, versus **15.11 s observed** today. This is extrapolated;
client fetch/decode and the earlier 8.36 s interval are additional costs.
A predicate plus scratch-sizing change is a candidate seam, not proof of
slow-producer continuity or completion of the integrated policy contract.

The existing **12/16/24/32/48 s** sweep selects values from physical transport
evidence. Test native alignment, unchanged reloads, accepted rate changes,
0.25×–4×, 1.05× sustained production and long ActiveLowReserve operation.
The present steady runway scales upward and clamps to at least 48,000 ms;
M2 must explicitly choose bootstrap down-scaling rather than inheriting
that clamp by accident.

### 4.2 The user's few-seconds-buffered requirement remains open

The user asked to start after a few seconds are buffered, then keep filling.
**About 24 s buffered is not that behavior**, even if wall-clock waiting is
much shorter than today. This revision preserves that requirement and
labels ordinary HLS as an interim improvement pending the user's choice.
It does not reinterpret the request as merely a few seconds of waiting.

LL-HLS remains excluded from the existing effort. A separate protocol
design, actual part/blocking-reload implementation and device qualification
would be needed before claiming that a very small bootstrap is safe.
This revision neither selects nor authorizes that work. A smaller target
also needs source-cut and client qualification; no quality downgrade or
mid-session target change is proposed.

## 5. Review decisions — concrete changes, with unresolved scope stated

1. **Fold into the existing owners:** recommended. The packet supplies full
   review copies of both contracts with appended amendments; production
   behavior and historical authorizations are unchanged.
2. **Ordinary HLS interim improvement:** awaiting the user's choice. The
   ~24 s/~6.5 s estimate is not a selected threshold, measured result or
   fulfillment of the strict few-seconds-buffered requirement.
3. **Master/child preparation alignment:** proposed after Track N, pending
   reconciliation with the native contract's non-goal. Wait under the
   existing 55 s playlist cap, clipped to the outer deadline; retain 5 s
   final admission. No new init authority or renewed request clock.
4. **Digest-scope investigation:** proposed separately. Keep current engine
   identity intact; no database migration, forced requeue or digest change
   is authorized by this packet.

## 6. Verification and delivery — documentation only

Source was checked at `189286828ca0be981a8820d3191856f4e216364a`; this is
not a fresh assertion about the remote branch head. The original base
compile passed on Rust 1.97.1. The prior draft exact-name test selected zero
tests and supplies no regression evidence; its temporary clone/build cache
was removed. No implementation test or broad suite was run for this revision.

Reviewers should inspect Track N §10 and startup-latency §11 against their
baseline contracts. The next implementation must run only necessary new,
affected or failed tests, retain applicable passing receipts and qualify
actual Safari playback. Observe resume **3273.778 s** and zero through
first frame and at least 30 minutes of continuity, distinguishing produced,
advertised, buffered and presented frontiers. Warm and after-restart cases
must be separate. No runtime setting, restart, requeue, PR or deployment
was performed; this playback has not been accepted as repaired.

**Pinned source anchors:**

- [Writer gate and copy geometry][writer] · [writer publication][copyseg]
- [Initial reserve and explicit budget][budget] · [first snapshot and cadence][snapshot]
- [Snapshot-gated init][init] · [master preparation][master] · [child preparation][child]
- [Native immediate attachment][attach] · [error-to-rescue branch][error]
- [Incomplete sample probe][probe] · [engine digest identity][digest]

[writer]: http://forge.lan:3000/noirr/plurx/src/commit/189286828ca0be981a8820d3191856f4e216364a/crates/plurx-core/src/transcode/mod.rs#L258
[copyseg]: http://forge.lan:3000/noirr/plurx/src/commit/189286828ca0be981a8820d3191856f4e216364a/crates/plurxd/src/copyseg.rs#L479
[budget]: http://forge.lan:3000/noirr/plurx/src/commit/189286828ca0be981a8820d3191856f4e216364a/crates/plurxd/src/transcode/rolling/publication.rs#L266
[snapshot]: http://forge.lan:3000/noirr/plurx/src/commit/189286828ca0be981a8820d3191856f4e216364a/crates/plurxd/src/transcode/rolling/session.rs#L948
[init]: http://forge.lan:3000/noirr/plurx/src/commit/189286828ca0be981a8820d3191856f4e216364a/crates/plurxd/src/transcode/manager/publication.rs#L1938
[master]: http://forge.lan:3000/noirr/plurx/src/commit/189286828ca0be981a8820d3191856f4e216364a/crates/plurxd/src/http/hls/playlist.rs#L148
[child]: http://forge.lan:3000/noirr/plurx/src/commit/189286828ca0be981a8820d3191856f4e216364a/crates/plurxd/src/http/hls/playlist.rs#L330
[attach]: http://forge.lan:3000/noirr/plurx/src/commit/189286828ca0be981a8820d3191856f4e216364a/crates/plurxd/src/web/player/player.js#L1280
[error]: http://forge.lan:3000/noirr/plurx/src/commit/189286828ca0be981a8820d3191856f4e216364a/crates/plurxd/src/web/player/transport.js#L837
[probe]: http://forge.lan:3000/noirr/plurx/src/commit/189286828ca0be981a8820d3191856f4e216364a/crates/plurxd/src/web/player/stall-diagnosis.js#L814
[digest]: http://forge.lan:3000/noirr/plurx/src/commit/189286828ca0be981a8820d3191856f4e216364a/crates/plurxd/src/ffmpeg.rs#L2107
