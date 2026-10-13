# Metal HDR acceptance — real hardware results (2026-10-12)

**Status:** accepted for the stated HDR10 comparison; broader playback issues
remain open · **Recorded:** 2026-10-12.

Companion to [the Mac processing ledger](MACOS-VIDEO-PROCESSING-STATUS.md).
This is the complete acceptance record, with retained failures and source-bound
evidence; the ledger tracks the broader processing project.

Physical acceptance passes on `94dee81da`. PR #1038 merged into main as
`0da656d3a3e7d7f0c52387543571d2ff4a265340` at 23:09:38 UTC after the exact-head
promotion gate passed on `33d57cdf9`. The Air remains on the physically tested `94dee81da` build; the Pro was
checked on parser-install repair `f01b8605d`. The [evidence manifest](evidence/macos-metal-hdr-20261012/evidence-manifest.json)
indexes every retained result, failure, capture and receipt by checksum. All
text records are committed in [the receipt archive](evidence/macos-metal-hdr-20261012/text-receipts.tar.gz).
Screenshots are preserved in the [private evidence archive](http://forge.lan:3000/attachments/8e191b9e-898b-4525-8cfb-77897f99c559)
on PR #1038; the
manifest records their archive locations and original checksums. Member names
below identify files inside those archives. Prior screenshots without `5m-presented` are not accepted proof.

## 1. NFS repaired with original options

`nas:/20T` was remounted with `vers=4,tcp,resvport`. Two 16 MiB reads complete
in 0.859956 and 0.774783 seconds. Yuba FFprobe completes in 0.079411 seconds.
The last recorded transfer reprobe timeout is Oct 11 at 19:11:13 UTC. Full Disk
Access granted by the user separately fixes protected network-volume access
for the service. No NAS configuration changed. See `air-remount-20261012.json`
and the NFS checks retained in `acceptance-receipt.json`.

## 2. Fleet deployment and Mac installation

The normal Ansible rollout delivered main containing `f01b8605d`. All four
Linux servers are healthy/ready; concurrent later deployments superseded
that run. Initial memory/concurrent-replacement failures remain recorded in
`fleet-final.json` and earlier logs.

Both standard Mac installers build/install `plurx-source-parser.wasm` plus
`plurx-source-parser.wasm.abi` next to pinned FFprobe. Both modules have SHA256
`36f7e0298eac14d1a664dfc083702bab812f6c83d196c651420af92f1080f21c`.
Original installed version on both was
`plurxd 0.3.0 (attribution-repair-2026-10-06-1601-gf01b8605d)`.
The Air's corrected version is
`plurxd 0.3.0 (attribution-repair-2026-10-06-1640-g94dee81da)`.
See `air-install-receipt.txt`, `laptop-install-receipt.txt`, and
`plurx-hevc-air-install-94dee81da-correct-prefix.log`. The latter standard
install uses `/opt/homebrew`; the LaunchAgent is unchanged. An initial
wrong-prefix install is retained as negative evidence, with its task-created
`/usr/local` duplicates moved aside reversibly.

## 3. Boot probe and bound identity

Both Macs bind FFprobe identity with no missing-parser/identity warning.
Full original pipeprobe lines are in `air-pipeprobe.txt` and
`laptop-pipeprobe.txt`. Corrected fresh Air boot is in `air-94-fresh-boot.log`:

```text
21:51:12.035774Z tone-map graph validated pipeline="vt_tonemap_metal" speedup="2.25x"
21:51:12.035831Z tone-map graph not available pipeline="libplacebo" reason=not available on this platform — this FFmpeg build has no vulkan support
21:51:12.035897Z the platform tone-map graph matched the CPU reference; HDR sessions use it through Mac video processing and otherwise the CPU chain pipeline=vt_tonemap_metal
```

This uses temporarily authorized VideoToolbox preference. Final Auto selects
software, correctly leaves HDR on CPU and skips Metal qualification; its 22:19
restart evidence is `air-94-auto-restored-boot.log`.

## 4. API and expanded System page

The authenticated earlier `air-system-final.json`/`.har` records selected CPU,
Metal passed, and libplacebo unavailable with the Vulkan reason. It is an
earlier source snapshot, not a fresh 94 API capture. Current corrected-source
System expanded proof is `air-94-system-probe-details.jpg`.
`air-4e-system.json` contains an authentication error and is invalid API proof.
Final restored System is `air-94-auto-restored-system.jpg`.

## 5. Physical Metal/CPU sessions and retained failures

American Gangster file 15 is ordinary HDR10, 3840×2160, MaxCLL 1179. Air Chrome was
forced to SDR and 1080p. Corrected Metal starts in 2.363 s and plays continuously
through five minutes, with 0 interruptions and 0 dropped of 7,810 frames. CPU starts
in 4.027 s. Exact paused 300,000 ms receipts prove presented frames in 39 ms / 36 ms,
not just a seekbar label:

| Path | Picture | Playback information | Presented-frame log |
|---|---|---|---|
| Metal94 | air-94-metal-5m-presented.jpg | air-94-metal-5m-presented-info.jpg | air-94-metal-5m-proof.log |
| CPU94 | air-94-cpu-5m-presented.jpg | air-94-cpu-5m-presented-info.jpg | air-94-cpu-5m-proof.log |

Actual producer command (`air-94-metal-producer-command.log`):

```text
scale_vt=w=1920:h=1080:format=p010le,tonemap_videotoolbox=tonemap=hable:tonemap_mode=max:desat=0:peak=117.9:transfer=bt709:matrix=bt709:primaries=bt709:range=tv:format=nv12:apply_dovi=0
```

CPU producer (`air-94-cpu-producer-command.log`) uses
`tonemap=tonemap=hable:desat=0:peak=11.79` between zscale linearization and
BT.709 SDR conversion. Both encode with h264_videotoolbox. Metal's peak option
takes nits/10; CPU takes nits/100. INFO-level VOD logs omit argv, so the actual
running producer command substitutes for the requested node-log argv. Cached
PlaybackInfo omits renderer and encoded frame rows; actual producer plus
browser intrinsic 1920×1080 prove path and raster.

Corrected physical Metal removes the old excess brightness/highlight clipping,
remaining slightly darker than CPU. No pixel identity or display calibration
is claimed. Actual raw 300 s frame luma MAE improves 13.75→5.15, p95 23→9. The broad
synthetic boot threshold accepts both old and corrected options; authored
neutral/real-source diagnostics expose it, and absolute-unit/key regressions
enforce the correction.

The Pro's warmed forced 1080p SDR Heretic 398 now starts: intrinsic 2583×1080,
2,268 frames, 0 drops, 0 interruptions (`laptop-heretic-1080-started-20261012.jpg`).
Cold-start reliability is not claimed. Preserve these negatives:

- Earlier cold/remux placement stalls and a misleading dynamic-range badge.
- Unbuffered paused seeks stall 20 s; a stale picture can remain under a 5:00 label.
- One replicated-store self-fence causes a transient 503 and recovers.
- On 94, a manual 1080p quality switch loses alignment 51 ms and retains HDR remux.
- Abandoned own staging holds VOD capacity; two 500 startup refusals disappear
  after stopping only that test session via Activity. The continuous corrected
  Metal run then passes.

All permitted temporary settings are restored: Auto preferred backend applied
by service restart, Mac processing on, quality Auto, resume 0, Chrome Default
relaunched. Test player closed; idle Air Chrome tab remains because closing the
last grouped tab would delete a synced group, so that prompt was canceled.
Apple TV / phones were not used.

PR #1038 includes conservative selected-stream HEVC scan refinement,
catalog/prepared-source parity, peak-unit conversion and Metal-only affected
cache-key invalidation. Astra review has no remaining blockers. Candidate94
passes pinned Rust 1.97.1 Mac Clippy / release and 101 decoder / 21 pipeline / 46 probe
regressions, Linux workspace check / Clippy, and Main promotion logical 4860 / actual 4886
(Python 1,376 passes). Integrated source 5010b0516 passes 101 decoder, 21 pipeline, 29 HEVC daemon,
62 probe (one ignored) and 25 ownership/documentation checks. Final head
33d57cdf9 changes only two documents after integration; pinned Mac workspace
check (2m 58s) and Clippy (39.80s) pass on that committed head. Integrated
Linux workspace check (8m 20s) and Clippy (5m 44s) pass. Current-head promotion
run 4863 / API 4889 passed; exact-head preflight and complete 1,376-test Python
receipts are saved, with no phase or fixture errors. The landing carries all
six regression anchors. Windows compile
gates were deliberately removed by ee668d0b4; no Windows pass claimed.

Automatic approval review rejected temporary LaunchAgent RUST_LOG debug
configuration as beyond permitted settings. No logging configuration changed.

## 6. Remaining playback work — preserve failures alongside successful frames

The [detailed follow-up plan](../playback-control/MACOS-HDR-ACCEPTANCE-FOLLOWUPS-2026-10-12.md)
specifies F1–F7 evidence, investigation order, reproduction and closure criteria.

| Observation | Status and next investigation | Evidence member |
|---|---|---|
| Paused seek outside buffered video stalls about 20 seconds; timeline can show an old picture at5:00 | Open: qualify presented-frame completion and paused unbuffered-seek recovery under the playback lifecycle work. | air-4e-final-playback.log; air-94-cpu-5m-proof.log |
| Manual1080p switch loses alignment51ms and retainsHDRremux | Open: reproduce prepared quality-switch alignment; kept-incumbent playback is not an applied1080p switch. | air-94-metal-startup.log; air-94-metal-5m-proof.log |
| Laptop cold/remux start stalls or exceeds placement deadline | Open: warmed Heretic success does not qualify cold-start reliability. | laptop-heretic-20261012-failure.txt; laptop-heretic-warm-retry.txt |
| Cached PlaybackInfo lacks renderer/encoded-raster rows; dynamic-range badge disagrees with SDR delivery | Open: reconcile displayed metadata with the delivered rendition. Actualproducer/intrinsic raster supplies acceptance proof here. | air-94-metal-5m-presented-info.jpg; laptop-heretic-diagnostics.txt |
| Abandoned own staging holds VOD capacity and causes two500 refusals | Cleared by stopping only this test session; investigate abandoned-preparation cleanup under playback lifecycle reliability. | air-94-metal-startup-error.log; air-94-metal-retry.log |
| Replicated-store self-fence causes transient503 | Automatically recovered; retain outage/grace observation for serving-fence reliability. | air-4e-final-playback.log |
| Corrected Metal slightly darker than CPU | Broader calibrated fidelity remains open; boot mean-channel threshold does not enforce this real-source regression. | plurx-air-real-frame-check.log; plurx-metal-boot-fixture-check.log |

Successful HDR10 comparison closes this acceptance slice. It does not close the
[continuous-quality](../playback-control/CONTINUOUS-QUALITY-BUILD.md),
[playback lifecycle](../playback-control/PLAYBACK-LIFECYCLE-STATUS.md) or
[HDR fidelity](../performance/HDR-REFERENCE-SCORING.md) work.

## 7. October 13 renewed-access and paused-reopen rerun

Renewed NAS access permits real Air Chrome HDR10-to-1080p-SDR sessions through
the Metal and CPU Hable graphs. Local unbuffered paused targets present; the
forced paused replacement exposed a preparation overlay that remained after
the frame arrived. The exact reviewed surface correction clears it from current
owned target-frame proof without Play. The original failed picture and corrected
paused five-minute picture are both preserved. The
[dated physical rerun](../playback-control/MACOS-HDR-ACCEPTANCE-FOLLOWUPS-2026-10-12.md#october-13-physical-rerun--renewed-nas-access-and-paused-surface-completion)
records source/binary provenance, timing, limits and current completion state.
Standard Screen Sharing carries SDR pixels; this is ordinary HDR10-to-SDR
hls.js acceptance, not calibrated HDR-display or native HLS qualification.
