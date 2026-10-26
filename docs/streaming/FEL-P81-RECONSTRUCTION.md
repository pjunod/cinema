# Reconstructed Profile 8.1 — serving Profile 7 FEL to Dolby Vision clients

**Status:** built on branch `claude/lp-fel-p81` (milestone M2 of
[the Linux player plan](../clients/LINUX-PLAYER-PLAN.md) §3.3) · not
deployed · physical Dolby Vision display acceptance open · **Written:**
2026-10-25

Companion to [DV_PROCESSING_TOOLS.md](DV_PROCESSING_TOOLS.md) (the helpers)
and [the authoring notes](../../tools/dv_processing/AUTHORING.md) (the P8.1
record). This is what made the route serve, what proves the picture is the
composed one, and what is still open.

A Dolby Vision client that claims `dv_profiles:[5,8]` and plays a Profile 7
FEL title, with `playback.dolby_vision_fel_reencode` and conversion on, gets
a P8.1 HLS rendition whose base layer is the FEL-composed picture
(`segment_decode_render … fel`, VA-API decode, Vulkan compose, `hevc_vaapi`
Main10) and whose RPU is the source RPU converted with `To81` and the
applied mapping removed. Without it the client gets the plain conversion:
the original base layer, the enhancement layer dropped.

## 1. Why it was "not available" — the root causes

| Where | What stopped the route | Fix |
|---|---|---|
| `transcode/manager/create.rs` preparation, `dv_segment.rs` `streaming_helper_cpu_threads` | The renderer was debited four software frame-decoder threads per layer plus two (FEL: 10) whatever it decoded with. With the 5-thread encoder share that is 15 against the 11-thread software budget of a 12-thread worker, so on lab6 every FEL graph was refused: `DV processing needs 15 CPU threads (10 renderer, 5 encoder) and this server's software encoder budget is 11`. `segment_decode_render.c` (`open_layer`) runs those workers only when it opens without a VA-API device. | `RendererDecode` sizes the debit: on the GPU one decode context per layer plus two (FEL 4, base 3); in software unchanged. The GPU shape needs the device and a proved HEVC Main10 decode (`ffmpeg::has_hevc_main10_vaapi_decode`, measured once at startup: the decoder inventory cannot see HEVC hardware decode, because FFmpeg's HEVC decoder never prints the `Reinit context` line it reads). A window whose renderer nevertheless opened in software is refused, never served over budget. |
| `http/developer.rs` `dolby_vision_fel_reencode` | A fixed `Unmet` "not yet available on playback workers", keyed only on the saved switch. | Computed rows: helper installed + conversion permitted; the FEL graph's CPU debit against this node's budget; the live slot. Advisory, never read on Save. |
| `dv_processing.rs` `reports_processed` | Admitted `Hdr10 | Sdr` only, so a P8.1 window never produced a processing report; nothing recorded that the served base layer came from composition. | Reports `profile81_reconstructed` with `fel_contributed` from every served frame's EL binding and NLQ residual. The owner relay accepts it only with a `dolby_vision` carrier and never with a display-target mapping. Clients validate HDR10/SDR reports and ignore it. |
| `http/hls/create.rs` `session_enhancement` | `processed_fel.unwrap_or(profile == 7 && has_el)`: with no plan fact, the source's tags lit "FEL used". | FEL comes from the receipt, else the plan, else it is "not confirmed". |
| `vod/serve/delivery.rs` `display_target_activity` | Returned nothing for a P8.1 rendition, so Activity showed "Transcode · 2160p" and no reason. | A bounded `processing` observation (picture, work, reason) beside Stop playback. |

Qualification was not bypassed and not changed: the completed-window
constructor (`dv_plan_completed_window`) and the receipt still decide every
window. The helper's `production_qualified:false` is a constant in its own
trace and was never read by the daemon; the daemon's checked window is the
production authority, for HDR10-E as for P8.1.

## 2. Admission — one live reconstruction per worker

- **The slot.** `LIVE_RECONSTRUCTION_SLOTS = 1` (`transcode.rs`). A graph
  that composes the EL takes it before its CPU/GPU admission and its runtime
  recipe holds it for the graph's life, so a reused root shares it and a
  closed graph frees it. The next play keeps its ordinary route; its FEL row
  says `not used for this play — … this worker already runs its 1 live FEL
  reconstruction; more wait until concurrent reconstruction is measured`.
  One, because only one graph's throughput was measured (M2a: 1.40× and
  1.70× real time sustained on lab6).
- **The budget.** The existing frozen admission
  (`vodencode::admit_frozen_preparation`: one GPU slot plus the summed CPU
  debit) is unchanged; it now debits 9 threads for a GPU-decoded FEL graph.
- **Startup.** The existing preparation already verifies the first window
  before the create answers: 1.50–1.65 s for each 2 s 4K window on lab6, well
  inside the 8 s enhancement share. No further buffer was added; it would
  need a new preparation stage, and the producer runs ahead at about 1.3×
  per window after start.

## 3. Proof

All on lab6 (Ryzen Phoenix1 iGPU, RADV + VA-API, the image's bundle, whose
renderer identity equals this tree's), inside the `plurxd` container, in a
scratch directory deleted afterwards. Two 12 s excerpts of E2 titles, cut
with `ffmpeg -c copy`: Title A (P7 FEL, CM v4.0, 24 fps, origin 0) and
Title B (P7 FEL, CM v4.0, 23.976 fps, origin 83 ms).

**Serving.** `actual_processed_hdr_windows_verify_audio_cadence_native_shapes_and_seek`
(ignored physical test) with `PLURX_DV_WINDOW_DESTINATION=profile81`
selects `ReconstructedProfile81` on both, proves the slot is held and a
second play is refused, and verifies windows 0, 1 and 2: 48/48 frames with
the EL bound, VA-API decode, 9-thread graph. Before the fix it refused at
admission on lab6.

**The served window** (`tools/dv_processing/controls/check_p81_window.py`):

| | Title A | Title B |
|---|---|---|
| Configuration | P8, level 6, BL compat 1, no EL, `dvvC` (no `dvcC`) | same |
| Access units / RPU NALs per AU / extracted RPUs | 48 / 1 / 48 | 48 / 1 / 48 |
| L1, L2, L8 equal to the source, per presented frame | 144 / 144 | 144 / 144 |
| L3, L5, L6, L9, L10, L11 equal | 48 frames each | 48 frames each |

**Composition** (`tools/dv_processing/controls/fel_composition_proof.py`).
Reference: the helper's own FEL master render (`fel`, 16-bit R'G'B').
EL dropped: the window converted with `dovi_tool -m 2` (libdovi `To81`, as
`dvconvert.rs`), its base layer checked bit-identical to the source's, then
rendered by the same helper in `base-rpu` mode — what a Dolby Vision display
reconstructs from the plain conversion. PSNR over every pixel on PQ
R'G'B'; ΔE_ITP (BT.2124, `tools/dv_quality/metrics.py`) on a 16-pixel
lattice, 1 555 200 samples.

| Against the FEL reference | Title A PSNR · ΔE mean · ΔE p99 | Title B PSNR · ΔE mean · ΔE p99 |
|---|---|---|
| Composed, pre-encode 4:2:0 export | 53.16 dB · 1.01 · 6.19 | 57.27 dB · 0.92 · 2.87 |
| EL dropped, same 4:2:0 export | 42.45 dB · 4.68 · 10.58 | 36.15 dB · 3.32 · 10.58 |
| Served window (composed + `hevc_vaapi`, ~18.4 Mb/s) | 47.29 dB · 2.86 · 12.16 | 34.66 dB · 3.95 · 11.36 |
| EL dropped, 4:4:4, unencoded | 43.15 dB · 3.94 · 10.27 | 36.04 dB · 3.53 · 10.27 |

**How to read it.** The first two rows differ only in whether the EL was
composed — same frames, same export, same Y'CbCr step — so they prove
composition: the served base layer is built from the composed picture, and
that picture is 10.7 dB (A) and 21.1 dB (B) closer to the FEL master. The
last two rows add the delivery encode. Title A's served window is still
closer than the plain conversion's untouched base layer; Title B's is not.
Per-frame Y-PSNR of B's served window against its own pre-encode picture is
about 48 dB on every third frame and 29–34 dB on the rest: `hevc_vaapi`
(RADV, compression level 19) quantizes its B-frames far harder than its
anchors, and B's grain is the EL's detail. That is the open "quality at
delivery bitrates" requirement, not a composition fault.

Both titles are letterboxed and the bars are in every number. The EL-dropped
p99 is the same in both titles because it is the bars: the composed picture
puts them at 0.00001 nits, the EL-dropped one at 0.004–0.005 nits, a
constant ΔE_ITP of 10.27 (4:4:4) or 10.58 (4:2:0). The picture area is
where the PSNR and mean differences come from (Title A centre pixel: ΔE 1.98;
Title B: 3.57).

## 4. Truthfulness and attribution

- **`enhancement`.** The reconstructed 8.1 lights FEL only from the
  served-window receipt or the selected plan (which admits P8.1 only for a
  composed FEL picture). The fallback conversion says `FEL: not used for this
  play — <reason>`.
- **`effective_processing`.** `profile81_reconstructed: true`,
  `hdr10_enhanced: false`, `fel_contributed` from every served frame.
- **Activity.** The stream card shows `Reconstructed Dolby Vision Profile 8.1
  · VA-API decode, base and full enhancement layers composed on the GPU,
  hevc_vaapi Main10 encode, a rewritten 8.1 RPU per frame`, then `Why: Dolby
  Vision client playing a Profile 7 FEL title; FEL re-encode is on in
  Settings → Developer · live rendering · FEL composed`, with the serving
  node and Stop playback (which ends the session and its graph). The
  renderer and encoder also appear under Processes as `DV streaming
  reconstruction` and `DV streaming encoder`, each stoppable.

## 5. Reproduce

```bash
# lab6, inside the plurxd container, in a scratch directory
ffmpeg -ss 1800 -i "<P7 FEL title>.mkv" -t 12 -map 0:v:0 -map 0:a:0 -c copy clip.mkv
PLURX_DV_WINDOW_SOURCE=clip.mkv PLURX_DV_WINDOW_DESTINATION=profile81 \
  PLURX_DV_WINDOW_DUMP=dump ./plurxd-test \
  actual_processed_hdr_windows_verify_audio_cadence_native_shapes_and_seek \
  --ignored --nocapture                       # serving, slot, windows 0-2
python3 -I tools/dv_processing/controls/check_p81_window.py check dump/window-000.mp4 clip.mkv 48
python3 -I tools/dv_processing/controls/fel_composition_proof.py proof clip.mkv dump
```

## 6. Open

- **Delivery-bitrate quality.** Title B's served window loses to the plain
  conversion after the encode (§3). The `hevc_vaapi` B-frame quality on
  RADV, and the processed rendition's bitrate when it replaces a full-rate
  converted copy, need their own measurement before this is the better
  picture on grainy titles. The Developer row says so.
- **Concurrency.** One slot until two concurrent graphs are measured.
- **A device whose VA-API driver fails HEVC Main10** decodes in software
  and is debited for it; one whose probe passes but whose renderer still
  opens in software has each first window refused (the failure episode then
  keeps that play on its ordinary route).
- **Physical acceptance** on an Apple TV, Android TV and the Linux player
  (M6): startup, seeking, the badges, and the display's rendering.
