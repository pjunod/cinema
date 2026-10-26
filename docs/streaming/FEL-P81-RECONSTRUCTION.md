# Reconstructed Profile 8.1 — serving Profile 7 FEL to Dolby Vision clients

**Status:** built on branch `claude/lp-fel-p81` (milestone M2 of
[the Linux player plan](../clients/LINUX-PLAYER-PLAN.md) §3.3) · not
deployed · served only for a title whose own measurement admits it (§7);
on lab6 neither measured title is admitted · physical Dolby Vision display
acceptance open · **Written:** 2026-10-25 · **Updated:** 2026-10-26

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
admission on lab6. Since §7 it first proves an unmeasured title keeps the
plain conversion ("… is being measured …"), then stores an admitting
verdict for the excerpt and serves it.

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

- **No measured title is admitted on lab6** (§7). The route, its recipe and
  its qualification ship gated; a title is reconstructed only when its own
  verdict says the reconstruction is the better picture. A better encoder
  (another GPU, a two-pass or longer-GOP window encode) changes the recipe
  identity and re-measures every title.
- **A node that cannot measure** (no radeonsi VA-API) may still run a
  title's scan job; the verdict then stays missing and the next play on a
  capable node asks again (at most once per title per node every 6 h).
- **Concurrency.** One slot until two concurrent graphs are measured.
- **A device whose VA-API driver fails HEVC Main10** decodes in software
  and is debited for it; one whose probe passes but whose renderer still
  opens in software has each first window refused (the failure episode then
  keeps that play on its ordinary route).
- **Physical acceptance** on an Apple TV, Android TV and the Linux player
  (M6): startup, seeking, the badges, and the display's rendering.

## 7. Route recipe and per-title qualification

**Decision record.** Coordinator decision, 2026-10-25 (the owner had
delegated decisions while away): the reconstruction is served only where it
is the better picture, decided per title by measurement, and the route gets
its own encode recipe. §3 showed why a single answer cannot exist: the
fallback serves the disc's base layer untouched, the reconstruction
re-encodes everything, so it wins only on a title whose enhancement layer
changes the picture by more than the encode loses.

### Recipe

`RECONSTRUCTED_P81_RECIPE` (`p81-vbr:b=min(source,60000k):maxrate=1.5b:bufsize=2b:bf=0:radeonsi-compression_level=0:v1`):
VBR at the source's average bitrate capped at 60 Mb/s
(`reconstructed_p81_video_kbps`), the ordinary VBV (`-maxrate` 1.5×,
`-bufsize` 2×), `-bf 0`, and radeonsi compression level 0 (VBAQ off). It is
applied by `CompletedReconstructedOutputPlan::with_reconstructed_profile81`
for this route only; every other radeonsi encode keeps compression level 19,
and Software and NVENC are untouched (pinned by
`the_reconstructed_profile81_route_alone_drops_vbaq_and_never_caps_qp`).

**VBV check.** The decision asked for `-qmax 14`. radeonsi obeys `-qmax`
over `-maxrate`, so a QP cap lets a hard scene run past the rate the variant
advertises. The processed variant advertises `BANDWIDTH` = video × 1.5 +
audio (`vbv_maxrate_kbps`), 90 Mb/s + audio at the 60 Mb/s target. 2 s
window encodes at b = 60 Mb/s on lab6:

| Rate control | Worst 2 s segment | Within the advertised 90 Mb/s |
|---|---|---|
| VBR, `-qmax 14` | 153 Mb/s (grain-heavy film), 149 Mb/s (bright 70 mm) | no |
| VBR, `-qmax 18` | 94 Mb/s | no |
| QVBR quality 16 | 72.7 Mb/s | yes, but a worse picture than VBR at every point tried |
| **VBR, no `-qmax` (chosen)** | 57 Mb/s on those titles (worst 1 s: 58.8); 50.0–56.8 Mb/s on 8 windows of Titles A and B; 52.9–57.5 Mb/s in the production graph | yes |

So the recipe has no `-qmax`; `maxrate` and `bufsize` stay the ordinary
1.5× and 2×, and the advertised peak follows the route's own rate
(`processed_candidate` reads the processed options). The numbers are pinned
in `the_window_encode_is_the_routes_recipe` (`-b:v 60000k -maxrate 90000k
-bufsize 120000k -compression_level 0 -bf 0`, no `-qmax`).

**Throughput** on lab6 with this recipe, the production graph
(`actual_processed_hdr_windows_verify_audio_cadence_native_shapes_and_seek`,
`profile81`, a 12 s excerpt of Title B, 9-thread graph, VA-API decode):
1.40, 1.46 and 1.54 s per 2.002 s window (1.43×, 1.37×, 1.30×; mean
1.37×), above the 1.25× floor. The windows carried 52.9, 57.2 and 57.5
Mb/s of video (worst 1 s: 59.5 Mb/s) against the advertised 90 Mb/s.

Two faults this physical run found, both fixed with it: the streaming
graph's encoder check accepted only compression level 19, so every 8.1
window would have been refused (`check_encoder_budget` now admits the
route's 0 and nothing else); and a stored verdict made the held-source
probe comparison see an unknown field and refuse every later start of the
title as `vod_source_rescan_required` (the verdict is now application
metadata there, like the title aggregate).

### Qualification

- **Where.** Inside the title's Dolby Vision title scan job
  (`JobKind::DvTitleScan`, `source_probe/title_work.rs`), after the
  complete-title scan and before its aggregate is published. That job is
  already the once-per-title analysis of every Dolby Vision title; it is
  listed in Activity with the title, its node, a Cancel and (while running)
  the reason "Complete-title Dolby Vision scan. A Profile 7 FEL title is
  also measured on the GPU …"; its children appear under Processes as `Dolby
  Vision FEL qualification render / encode / sample`, each stoppable. No new
  job type, thread or schedule.
- **When.** For a Profile 7 FEL title, with HDR processing and FEL re-encode
  on, on a node whose encoder is radeonsi VA-API with its Main10 graph
  proved and whose renderer decodes on the GPU, when the title has no
  verdict for this renderer, recipe and encoder.
- **Admission.** The title job's own admission is extended
  (`extend_fragment_admission`) with a background GPU bundle: the renderer's
  debit plus `CONSUMER_THREADS` (3). It is a claim on that admission, so
  `fragment_worker_idle` counts it as the job's own capacity; without that
  the job's idle check read its own GPU step as foreground pressure and
  `run_admitted_probe` cancelled the qualification within 250 ms, so no
  verdict was ever stored (review B1). Foreground work still preempts it:
  its children are killed and reaped and the aggregate still publishes.
- **The slot, preemptibly.** The qualification takes the node's
  reconstruction slot registered as preemptible. A live reconstructed 8.1
  that finds it taken cancels the qualification and waits up to 5 s
  (`QUALIFICATION_SLOT_YIELD`) for it; one that does not let go in time
  gives the play "this worker's reconstruction slot is held by a FEL
  qualification measurement that did not yield within 5 s", distinct from
  the live-play reason. The slot is for the Profile 8.1 destination only;
  FEL HDR10-E and SDR processing keep their ordinary admission, as before
  this route existed.
- **CPU.** Every stage child is capped to what it is charged: `-threads 1`
  before its input and `-filter_threads 1` (one main, one decoder, one
  filter thread). A window whose helper reports a software decode under the
  GPU admission is refused, as the streaming route refuses it.
- **What.** K = 8 windows of 48 pictures at (i+1)/9 of the title, snapped to
  the picture grid with the held source origin. A window whose reference
  has no lit picture (a fade, a title card) is replaced by its alternate,
  half a spacing later; the letterbox is taken from the window's brightest
  picture, not its first. A title is admitted only on at least 6 lit
  windows; one with fewer gets a stored refusal ("too little of this title
  is lit to measure …"), so it is not measured again on every play. Each is rendered by the
  deployed helper (the streaming protocol: source fd 3, evidence fd 4, NUT
  fd 5) three ways: `fel` 16-bit R'G'B' (the reference), `base-rpu` 16-bit
  R'G'B' (what the plain conversion shows), and `fel` 10-bit 4:2:0 piped
  into the route's encode (`-r` on the picture grid, as the route's own pipe
  recipe reads it; without it the encoder drops and repeats pictures, ΔE
  10.7 instead of 3.1 on the same window), decoded back with the BT.2020
  limited-range matrix. Delta E ITP (BT.2124, the equations of
  `tools/dv_quality/metrics.py`, unit-tested against it) on a 32-pixel
  lattice inside the reference's letterbox (16-bit PQ code > 1024), pooled
  over all windows.
- **Rule.** Admit only if the served mean is at least 10% below the
  EL-dropped mean **and** the served p99 ≤ EL-dropped p99 + 2σ.
- **σ = 0.** σ is the run-to-run spread of the served p99 over repeated
  encodes of one window. hevc_vaapi on radeonsi is deterministic: three
  encodes of each of six windows (both titles, final recipe and the earlier
  `-qmax 14` one) were byte-identical. So the tolerance is 0.
- **K = 8, from the variance.** On Title B, whose windows vary most, one
  window's mean gain ranged −26% to +17% with a standard deviation of 15%,
  so K windows give the title's gain a standard error of 15%/√K. At K = 4
  (7.5%) a title the EL does not help at all would clear the 10% bar about
  one time in eleven; at K = 8 (5.3%) about one in thirty-four. A title is
  measured once, in the background, in about two minutes.
- **Stored** in the title's probe document beside its Dolby Vision facts
  (`files.probe_json.plurx_dv_fel_v1`, a guarded `json_set`: same file id,
  size and mtime, P7 with an EL), keyed by the source size/mtime and an
  identity hash of the renderer bundle identity (the scanner digest), the
  recipe string and the encoder: the FFmpeg executable's digest, the VA
  driver's own description from the startup probe (vendor, Mesa version,
  GPU) and the render device with its PCI identity. A changed source,
  helper, recipe, FFmpeg build, driver or GPU reads as no verdict. The held
  source probe comparison treats the key as application metadata.
- **Re-measuring** rotates the title's scan demand to a binding no scanner
  equals, so the scan worker rebinds and reruns the title job; at most once
  per title per node every 6 h, and never while a title job for it is
  queued, running or cancelling (rotating the demand would cancel that
  attempt, which qualifies the title anyway if it is due). A separate
  qualification-only demand would avoid rescanning the title, but the title
  demand, its triggers and the job kind are a shape-verified extension; the
  rescan runs once per title per renderer, recipe and encoder.
- **Gated** where the ingress offers the route (`prepare_processed_hdr10`)
  and again where every start builds its processed graph
  (`prepare_vod_encoding`), before the slot or any render. Until a verdict
  exists the play gets the plain conversion, its FEL row reading "whether
  this title's enhancement layer adds more than re-encoding loses is being
  measured on this server's GPU"; a refused title's reads "the enhancement
  layer adds less than re-encoding loses for this title (measured ΔE ITP
  mean … reconstructed vs … without the EL, p99 … vs …)"; a node without a
  qualified encoder says so. A start whose own processing is declined inside
  the worker (verdict, busy slot, refused window) carries that reason to the
  session's enhancement detail (`StartInfo::processing_note`), not a generic
  backend failure.
- **VA-API Main10 decode proof.** Measured off the boot path at startup by
  decoding a shipped two-picture clip (no libx265 needed), 5 s bound. A
  success is kept for the process; a failure is retried on later need after
  30 s, doubling to at most an hour. Until proved, the renderer is debited
  its software shape and the qualification does not run.
- **Developer tab.** The "Per-title quality qualification" row
  (`dv_fel_bitrate_quality`) is computed: the rule, whether this node's
  encoder is qualified, and how many titles hold a verdict for this renderer,
  recipe and encoder and how many are admitted, counted over the DV title
  demand (one row per DV title), not the whole catalogue. Advisory; never
  read on Save.

### Results (lab6, the production measurement)

`physical_title_job_qualifies_a_p7_fel_title_and_stores_its_verdict` with
`PLURX_FEL_QUALIFY_WITHOUT_SCAN=1`: the title job's qualification step
(`qualify_if_due` under `run_admitted_probe`, holding the job's own
admission), on the whole files, with a helper bundle built from this tree;
each verdict was stored in the title's probe document and read back as
current for this renderer, recipe and encoder (60 Mb/s target, GPU decode):

| | Title A | Title B |
|---|---|---|
| Served (reconstructed, encoded) ΔE mean / p99 | 3.10 / 10.17 | 3.02 / 9.68 |
| EL dropped (plain conversion) ΔE mean / p99 | 1.69 / 7.45 | 2.94 / 6.61 |
| Mean gain (needs ≥ +10%) | −84% | −2.8% |
| Windows the reconstruction wins on the mean | 0 of 8 | 3 of 8 |
| Verdict (stored) | not admitted | not admitted |
| Time | 127 s | 162 s |

Before the review fix the same step was cancelled by its own GPU admission
within 250 ms and stored nothing. Title B's numbers are identical to the
store-less run (the encode is deterministic); three of Title A's windows
moved because their letterbox now comes from the window's brightest picture.
The full job (complete-title scan first) cannot run on a 60 s excerpt: the
scanner refuses a cut that starts inside a GOP ("contiguous decoded title
coverage"), and on a whole feature it decodes every picture, hours on lab6.

Title B's earlier single-window win (§3: one 12 s excerpt at `-qmax 14`)
does not hold across the title: without the QP cap the VBV-honest encode
loses at p99 in every window, and on the mean the title is a wash. Neither
title is reconstructed on lab6; both play the plain conversion with the
measured reason.

Reproduce (inside the `plurxd` container, scratch directory deleted
afterwards; `PLURX_DV_PROCESSING_DIR` names a bundle built from the tree
under test):

```bash
PLURX_FEL_QUALIFY_WITHOUT_SCAN=1 PLURX_FEL_QUALIFY_SOURCE="<P7 FEL title>.mkv" \
  PLURX_FEL_QUALIFY_SCRATCH=$PWD/scratch \
  ./plurxd-test physical_title_job_qualifies_a_p7_fel_title_and_stores_its_verdict --ignored --nocapture
```
