# Reconstructed Profile 8.1 — serving Profile 7 FEL to Dolby Vision clients

**Status:** built on branch `claude/lp-fel-p81` (milestone M2 of
[the Linux player plan](../clients/LINUX-PLAYER-PLAN.md) §3.3) · not
deployed · served-picture quality acceptance not met (§3, §6) · physical
Dolby Vision display acceptance open · **Written:**
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
reconstructs from the plain conversion. PSNR over every pixel on PQ R'G'B',
frames paired by index; ΔE_ITP (BT.2124, `tools/dv_quality/metrics.py`) on a
16-pixel lattice. Each cell is whole frame / active picture (inside the
letterbox: Title A rows 274–1884, Title B rows 40–2118).

| Against the FEL reference | Title A PSNR · ΔE mean · ΔE p99 | Title B PSNR · ΔE mean · ΔE p99 |
|---|---|---|
| Composed, pre-encode 4:2:0 export | 53.16 · 1.01 · 6.19 / 51.94 · 1.22 · 6.98 | 57.27 · 0.92 · 2.87 / 57.14 · 0.93 · 2.92 |
| EL dropped, same 4:2:0 export | 42.45 · 4.68 · 10.58 / 48.48 · 2.69 · 7.65 | 47.00 · 3.32 · 10.58 / 48.60 · 3.05 · 7.05 |
| EL dropped, 4:4:4, unencoded (what the fallback shows) | 43.15 · 3.94 · 10.27 / 52.48 · 1.81 · 5.98 | 46.07 · 3.53 · 10.27 / 47.22 · 3.27 · 7.34 |
| Served, current recipe (20 Mb/s rung, VBR, compression level 19) | 46.77 · 3.03 · 13.82 / 45.51 · 3.90 · 14.79 | 45.37 · 4.00 · 12.89 / 45.21 · 4.12 · 13.00 |
| Served, VBR 50 Mb/s, `-qmax 16`, compression level 0 | 48.46 · 2.50 · 10.40 / 47.21 · 3.20 · 11.12 | 49.16 · 2.67 · 8.12 / 49.01 · 2.74 · 8.18 |
| Served, VBR 60 Mb/s, `-qmax 14`, compression level 0 | 48.77 · 2.40 · 9.92 / 47.52 · 3.06 · 10.65 | 49.47 · 2.61 · 7.43 / 49.32 · 2.68 · 7.49 |

The served rows encode the window's pre-encode composed picture with the
production argument list for this route (`vod_completed_reconstructed_pipe_args`
plus the radeonsi policy), varying only the rate-control arguments. The
current-recipe reproduction scores what the daemon's own dumped window
scored. Throughput is unaffected: compose + encode over five consecutive
2 s windows runs at 35.2 fps (1.47×) with the current recipe and 35.4–36.2
fps (1.48–1.51×) at 60 Mb/s.

**How to read it.** Rows one and two differ only in whether the EL was
composed, so they prove composition, in the active picture too (A: ΔE 1.22
vs 2.69; B: 0.93 vs 3.05). Whether the *served* picture beats the
fallback is a different question: the fallback serves the disc's own base
layer untouched (~80 Mb/s), the reconstruction re-encodes the whole picture.

- **Title B** (large EL contribution: the fallback is 47.22 dB from the
  reference inside the picture): the current recipe loses (45.21 dB, ΔE
  4.12). At 60 Mb/s with `-qmax 14` it wins PSNR (+2.1 dB) and ΔE mean
  (−18%) inside the picture, with p99 at parity (7.49 vs 7.34).
- **Title A** (small EL contribution: the fallback is 52.48 dB, ΔE 1.81
  inside the picture): no hevc_vaapi setting tried beats it inside the
  picture, up to 106 Mb/s with `-qmax 6` (48.89 dB, ΔE 2.50, p99 9.15).
  Its whole-frame lead comes from the letterbox, where the composed picture
  puts the bars at 0.00001 nits and the fallback at 0.004–0.005 nits (a
  constant ΔE 10.27 per bar pixel, which is also why the fallback's p99 is
  the same on both titles).

**What limits the encoder** (measured, `trace_headers` slice QPs and per-frame
Y-PSNR against the pre-encode picture):

1. **There are no B-frames.** This route already sets `-bf 0`. The earlier
   "29–34 dB on two frames in three" was the measurement: FFmpeg's `psnr`
   pairs frames by timestamp, and a 23.976/24 fps window in a millisecond
   time base was paired with the previous reference frame two times in
   three. The proof now restamps both inputs to their frame index
   (fixed 2026-10-26); the earlier PSNR column in this document was wrong,
   the ΔE columns were always index-paired.
2. **Each window's IDR is its worst frame.** Every window is a fresh encoder,
   so VBR starts the IDR at QP 24 while the P frames settle at 18 or below;
   the IDR scores 48.5 dB at every bitrate from 18 to 60 Mb/s. In CQP mode
   radeonsi encodes the IDR at QP 26 whatever `-qp` says (`-i_qfactor` /
   `-i_qoffset` change nothing). `-qmax` is the one argument that reaches
   it (IDR 53–55 dB at `-qmax 14–16`).
3. **The 20 Mb/s rung.** `ladder::bitrate_for_height(2160)` is the transcode
   rung; this route replaces a full-rate copy. Bitrate moves ΔE mean
   steadily (B active: 4.12 at 20, 2.74 at 50+qmax, 2.68 at 60+qmax).
4. **VBAQ** (compression level bit 4, on in the radeonsi policy) costs p99 at
   the 18 Mb/s rate (13.4 vs 11.7 on B), and nothing measurable at 50 Mb/s
   with `-qmax`; the radeonsi policy for ordinary HDR10 encodes is not shown
   wrong by this.
5. **The ceiling.** P frames reach 57.8 dB against the pre-encode picture at
   CQP 4, so a picture whose EL changes it by less than that (Title A) cannot
   be served closer to the FEL master than the untouched base layer at any
   practical rate.

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

- **Served quality (M2 acceptance not met).** The current recipe serves a
  picture further from the FEL master than the fallback on both titles. A
  60 Mb/s `-qmax 14` recipe for this route wins on Title B and cannot win
  inside Title A's picture at any rate tried (§3). Choosing the rate (and
  whether to reconstruct a title whose EL changes the picture less than the
  encode loses) is an owner decision; no recipe change is committed. A
  `-qmax` floor can push a hard scene past `-maxrate`, which the advertised
  peak would then understate, so a recipe with it needs a VBV check.
- **Concurrency.** One slot until two concurrent graphs are measured.
- **A device whose VA-API driver fails HEVC Main10** decodes in software
  and is debited for it; one whose probe passes but whose renderer still
  opens in software has each first window refused (the failure episode then
  keeps that play on its ordinary route).
- **Physical acceptance** on an Apple TV, Android TV and the Linux player
  (M6): startup, seeking, the badges, and the display's rendering.
