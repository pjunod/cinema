# Cinema vs LibreELEC CB1 — what CB1 does better, and what to take from it

**Status:** open — analysis complete; adoption is tracked per project in the
[project backlog](../features/PROJECT-BACKLOG.md) · **Written:** 2026-11-23 ·
**Owner:** Claude

Companion to [CINEMA-END-TO-END-REVIEW-2026-11-07.md](CINEMA-END-TO-END-REVIEW-2026-11-07.md)
(Cinema reviewed against itself) — this is Cinema reviewed against another
project. LibreELEC CB1 is an unofficial LibreELEC/Kodi build that plays Dolby
Vision, including Profile 7 FEL, on x86 PCs over HDMI. The owner asked what it
does better than Cinema, what it does that Cinema does not, and what Cinema
can take from it. Two plans came out of it:
[FEL reference composition](../streaming/FEL-REFERENCE-COMPOSITION-PLAN.md) and
[Playback info provenance rows](../clients/PLAYBACK-INFO-DV-PROVENANCE-PLAN.md).

**Sources:** CB1 `CroqueMr/libreelec-CB1` at `8d173a5`; plurx main at `c27701a`.
**Method:** six read-only comparison passes, then spot checks of the claims
that drive the recommendations. Paths are relative to each repository root.

## Verdict

Cinema's code is stronger in most areas: parsing safety, container output,
display mapping, encoding, tests, CI and reproducible builds. CB1 is ahead in
four specific places, all worth taking:

1. Profile 7 FEL reconstruction fidelity (reference filters, clamp, ordering).
2. Enhancement-layer pairing, recovery and container association.
3. Dolby Vision → HDR10+ generation, which is an open Cinema backlog item.
4. Deciding the output route once, up front, with the reason attached.

## Dolby Vision profile gap

CB1 decides from the RPU's contents; Cinema decides from the profile number
the scan recorded and the numbers each client claims
(`crates/plurx-core/src/playback/mod.rs:1344`).

| Profile / layout | CB1 | Cinema today | Gap |
|---|---|---|---|
| 5 | Direct IPT path | Direct on Apple, web, Linux; rendered otherwise | None |
| 7 FEL | Paired and composed in the player | Linux player composes; others get P8.1 conversion (FEL dropped) or optional server reconstruction | Fidelity, not support (see below) |
| 7 MEL | Handled without decoding the EL | Converted to P8.1; title scan now records FEL vs MEL (`scan/dv_title.rs`) | Small: skip EL decode for MEL |
| 8.1 | Handled | Direct where claimed, else HDR10 base | None |
| 8.2 (SDR base) | Listed test class | Re-encoded for non-claiming clients; processing preflight refuses it | Minor, rare content |
| 8.4 (HLG base) | Stays DV, full reconstruction | Strips to HLG base on non-claiming clients; processing refuses it | Real for broadcast/phone content |
| 10.x (AV1) | Software-decoded 10-bit AV1 DV, raw T.35 RPU kept | Detected, but no client claims 10 and the strip filter is HEVC-only | Real, growing |
| Dual-track MKV/MP4, dual-PID m2ts/ISO | Associated through FFmpeg patches | Scan reads the first video stream only; ISO not scanned | Real, uncommon |
| 4, 9, 20 | Not found | 4 is named; 9 and 20 not handled | No gap: neither does them |

So "CB1 supports profiles mine doesn't" comes down to three things: AV1
Dolby Vision (10.x), dual-track/dual-PID layouts, and processing of 8.4. The
headline profiles (5, 7, 8.1) are covered on both sides.

Closing each:

- **AV1 DV:** a caps claim on AV1 entries, AV1 (OBU) handling in copy and
  strip, a client that decodes it, and a guard so the label matches what is
  delivered. Cinema's behaviour on an AV1 DV file today was not verified.
- **Dual-track / dual-PID:** make scan and stream mapping aware of the second
  layer. CB1's three FFmpeg patches (`projects/Generic/patches/ffmpeg/`
  `…-04-ts-dovi-layer`, `…-05-matroska-dovi-association`,
  `…-07-mov-dovi-hevc-entry`) show the association rules.
- **8.4:** admit it in the processing preflight
  (`crates/plurx-core/src/transcode/dv_processing.rs`).

## Code approaches by area

| Area | Better | Why |
|---|---|---|
| RPU parsing and validation | Cinema | Hardened vendored parser, CRC always enforced, fuzz target with pinned findings; CB1 relies on FFmpeg and CRC checking appears off |
| RPU conversion and container output | Cinema | Full round trip with all levels kept; correct `dvcC`/`dvvC` handling. CB1 never writes RPUs or config records |
| Container input (layer association) | CB1 | Handles dual-track MKV/MP4 and dual-PID TS; Cinema handles single-track only |
| FEL reconstruction | CB1 | Integer reference arithmetic, reference filters, composition at chroma resolution before upsampling |
| EL pairing and recovery | CB1 | Pairs by parser picture order, recovers at the next shared keyframe; Cinema requires exact PTS equality and fails the job |
| Display mapping | Cinema | Closed-form CM v4.0/v2.9 honouring L1/L2/L3/L8 and more; CB1 applies L2 only on its HDR10 route and disclaims equivalence |
| Dynamic metadata from pixels | CB1 only | Cinema generates none; but CB1's "AI" is measurement plus a frozen first-second bias |
| Dynamic mapping of a DV title for an HDR10 display | Cinema, when the display peak is known | Corrected 2026-11-23, see "Enhanced HDR10 versus HDR10+" below |
| HDR10+ metadata in the output stream | CB1 only | Cinema writes none, and an HDR10+ source loses its metadata on re-encode |
| Encoding | Cinema only | CB1 has no encoder |
| State and seek invalidation | Split | CB1's core is a small candidate/commit design; its Kodi glue is about 36 booleans. Cinema's is correct but large |
| Fallback selection | CB1 | One pure selection per playback with the reason attached; Cinema's are reactive and per client |
| Caching | Cinema | Content-addressed, with eviction and pinning |
| Tests and CI | Cinema | Far larger suite, 17 workflows, fuzzing; CB1's C tests appear not to run in CI |
| Reproducible builds | Cinema | Byte-identical rebuild required; CB1 says hashes vary |
| Provenance per file | CB1 | Machine-checked per-file manifest and licence map |
| Verification on real content | Cinema | ΔE_ITP against a licensed renderer; CB1 states it has no certified-player comparison |

## Enhanced HDR10 versus HDR10+ (correction)

The first version of this document said only that Cinema "drops HDR10+ on
re-encode". That is true of an HDR10+ source, but it understated what Cinema
does with a Dolby Vision source.

Cinema's enhanced HDR10 (HDR10-E) reconstructs the picture with the FEL
residual and the RPU reshaping, then, when the display peak is known, applies
Dolby's per-shot L1 mapping and the L8/L2 creative trims on the server for
that peak (`docs/streaming/DISPLAY-TARGETED-DV-BUILD.md`, promoted through
#1271, trims through #1276). The dynamic mapping is baked into the pixels and
the stream is signalled as static HDR10 with the bucket as its mastering peak.

| | Enhanced HDR10, peak known | HDR10+ generated from DV |
|---|---|---|
| Per-shot mapping | Dolby's own L1 curve, applied by Cinema | TV's HDR10+ curve, driven by converted statistics |
| Creative trims (L2/L8) | Applied exactly | No direct equivalent; approximated by a knee |
| FEL and reshaping | Included | Separate question; needs the same reconstruction |
| Works on | Any HDR10 display | HDR10+ displays only |
| Needs the display's peak | Yes | No; the TV knows its own panel |
| One encode for all displays | No; one per peak bucket | Yes |

So with a known peak, enhanced HDR10 should match or beat HDR10+ generated
from the same title, because it uses the authored trims. Not measured here.

The gap is the unknown-peak case. Buckets are 400 · 600 · 1000 · 1600 · 2000
· 4000; "none / unknown" maps to **master**, which is static HDR10 with no
per-shot mapping, and the TV then applies one curve to the whole film. The
build doc's own table records that the peak comes from the platform or from a
viewer setting. That is where HDR10+ metadata would still help, and it is the
only case item 5 below addresses.

## What to adopt, ranked

1. **Reference EL filters as a pre-pass.** Cinema upsamples every plane
   bilinearly (`tools/dv_processing/fel_renderer.h:596`). CB1 implements the
   specified EL luma and chroma filters (`CB1/patches/libplacebo/`
   `libplacebo-9916-fel-filter.patch`, filter code in `…-9913-processing`).
   Smallest change that improves FEL fidelity.
2. **`vdr_in_max` clamp in the float NLQ.** Cinema's renderer lacks it and so
   admits only titles where it cannot bind (`tools/dv_processing/`
   `nlq_clipping.h:8`). CB1's patches `…-9917-native-bounds` and `…-9910-4`
   show the clamp and a noise guard. Widens the set of titles FEL works on.
3. **An independent integer FEL oracle.** Cinema's FEL reference shares
   libplacebo's composition with the thing it tests, so it cannot see filter
   or ordering deviations. CB1 has a Python integer oracle
   (`CB1/tests/reference/dovi_master_reference.py`).
4. **EL pairing by picture order, with recovery.** Replace strict PTS equality
   (`tools/dv_processing/segment_decode_render.c:730`) with CB1's parser-order
   pairing and shared-keyframe recovery (`CB1/src/dvbridge_fel.c`,
   `dvbridge_fel_seek.h`). Add MEL skip.
5. **DV → HDR10+ generation, for displays whose peak is unknown.** Lower
   value than first stated: with a known peak, enhanced HDR10 already does
   better. Open in
   `docs/streaming/DV-ENHANCEMENTS-NOT-YET-BUILT.md` §1. CB1's fit, validation
   and fallbacks are in `CB1/src/cb1_dv_hdr10plus.c`,
   `cb1_hdr10plus_curve.c`. Keep full SEI precision; CB1 quantises for HDMI.
6. **One up-front route selection with its reason.** CB1's
   `SelectOutput` (in `kodi-…-dv-startup-hdr10-recovery.patch`) returns
   `{route, policy, reason}` once per playback and latches fallback one way.
   This removes the stale-reason class recorded in `docs/PLAYBACK.md`.
7. **Candidate/commit for replacement sessions.** CB1's core
   (`CB1/src/dvbridge_core.c:135-226`) builds a candidate, commits only if the
   revision has not moved, and leaves nothing behind on failure. A fit for
   client recovery and quality replacement in place of one-shot latches.
8. **RPU padding tolerances.** Cinema's strict FFmpeg patch fails decode on
   any RPU parse error while its Rust parser tolerates padding; CB1's
   `ffmpeg-9900-09-dovi-aligned-padding.patch` shows the tolerance. Check
   first whether the pinned FFmpeg already has it.
9. **Cheap practice items:** a bug-report template with required build,
   hardware and display-chain fields; a per-file provenance manifest for
   adapted third-party code; diagnostics that state what a fact does not prove.

## What not to adopt

- The HDR10 → DV "AI": a tiny model adding a bias computed from the first
  second of playback, with no published training data or accuracy.
- CB1's Kodi-side state: about 36 booleans, with a true presentation watchdog
  and an unexplained flip timeout left open.
- Its patch layout: ordering by runs of "z" in filenames, duplicated and
  diverged patch trees, and a private ABI fork of an FFmpeg struct.
- VAAPI as the default EL scaler (driver-defined, with a silent fallback).

## Licence

CB1's engine, libplacebo additions and Kodi changes are GPL-2.0-or-later (some
LGPL-2.1-or-later per file). Items 1–5 above live in libplacebo or helper code
that the plurx server's FFmpeg path also uses. Reimplement from the published
specification and CB1's documentation rather than copying code, unless the
owner accepts GPL for that component.

## Limits of this analysis

- Static reading only. No performance or quality figure was reproduced.
- Upstream libplacebo and mpv sources are in neither tree, so Cinema's float
  composition was inferred from its helper code, patches and docs.
- Several large files were sized or searched, not read in full: CB1's
  `dvbridge_render.c` and big Kodi patches; Cinema's `ffmpeg.rs`, `vod.rs`,
  `dv_runtime*`.
- Counts of timers and flags are floors.
- Not verified: Cinema's behaviour on AV1 DV and dual-track sources; whether
  CB1 enables CRC checking elsewhere; whether CB1's C tests run in any CI.
- One doc was stale: `DV-ENHANCEMENTS-NOT-YET-BUILT.md` §3 says FEL vs MEL is
  unknown at scan, but `scan/dv_title.rs` already records it.

## Subtitles (added 2026-11-23)

Static reading; one comparison pass plus spot checks. Not device-verified.

| Area | Better | Why |
|---|---|---|
| Subtitles over HDR and Dolby Vision | CB1 | Converts the subtitle layer into the output's own domain and blends there; Cinema refuses the burn |
| Formats | Different goals | CB1 inherits Kodi's full set; Cinema is narrower on image and styled subtitles by design |
| Track selection, download, generation, translation | Cinema | One shared policy plus OpenSubtitles, whisper.cpp captions and translation; CB1 adds nothing beyond Kodi |
| Timing and seek | Different goals | CB1 shares Kodi's clock; Cinema keeps cues on the source timeline and shifts per client |
| Text rendering | CB1 live, Cinema reproducible | Kodi's libass renders over HDR; Cinema's libass runs only inside an SDR burn with frozen fonts |
| Parser safety and tests | Cinema | Bounded Rust PGS adapter with a fuzz target; CB1 patches unfuzzed C parsers |

**What Cinema does today over HDR.** `burn_would_discard_hdr`
(`crates/plurx-core/src/playback/mod.rs:1005`) refuses any burn when the
delivery is Dolby Vision, HDR10 or HLG, including "HDR source, transcode
negotiated to HDR10". The viewer keeps HDR and gets no subtitle. Only the
`pgs-v1` overlay on Apple and Android shows PGS over HDR, as sRGB images with
no brightness control. Styled ASS over HDR is not possible on any client.

**What CB1 does.** It takes the sRGB subtitle and menu layer to linear light,
to BT.2020, then to PQ at a 203-nit reference white, and blends in the
output domain. For bitmaps it adds an "authoring" setting (SDR or PQ,
because PGS carries no transfer tag) and separate brightness and saturation
controls applied to the palette in linear light.

**Worth adopting, ranked:**

1. **HDR-aware burn on the HDR10 transcode route.** Convert the subtitle
   plane sRGB → linear → BT.2020 → PQ at a fixed reference white, then
   overlay in 10-bit. The burn graph already has an HDR10 branch
   (`transcode/mod.rs` near line 2049); only the guard and the conversion are
   missing. This lifts the refusal for transcoded HDR, including styled ASS.
   It does not help copy, remux or direct play, where the server never
   touches pixels.
2. **A BT.2020/PQ authoring option and brightness control for PGS** in
   `plurx-pgs` and the overlay clients.
3. **Position, offset and size controls for bitmap overlays.**
4. **mpv could render embedded PGS itself on Linux direct play** instead of
   reporting it unavailable.

**Found in passing, not a CB1 idea:** `dvb_subtitle` is not listed in
`is_bitmap_subtitle` (`crates/plurx-core/src/tracks.rs:174`), so a DVB image
subtitle classifies as text. Downstream effect not traced.

**Cannot be adopted (architectural):** blending subtitles into the Dolby
Vision source domain so the TV maps them with the picture needs ownership of
the final scan-out. A server delivering Dolby Vision by copy cannot do it.

## Interface (added 2026-11-23)

From CB1's screenshots (`docs/cb1/images/`), its interface docs and a
read-only inventory of Cinema's web, Apple, Android and Linux player UI.
Nothing was run on either side.

**What CB1's interface consists of.** A Player Information panel (three
columns: input, processing, output), a Quick Switch list for changing the
processing path during playback, a Display Details dialog showing what the TV
advertises, and a per-format default-playback block in settings. The look is
stock Kodi; the content and grouping are what is worth studying.

| CB1 piece | Cinema today | Gap |
|---|---|---|
| Player Information: input / processing / output columns | "Playback info" on web, Apple, Android with four density modes and one shared field catalog; Linux player has one flat list | Linux list has no group headings |
| Metadata provenance: in the file · used · sent to the TV, as level lists | FEL / RPU / DYN rows with a reason each | No per-level list (L1, L2, L5, L6, L8 …) and no "left for the display" column |
| Live metadata values: mastering, MaxCLL, RPU max/min, current L1 | Tone-map peak note on web | Not shown |
| Quick Switch with unavailable choices greyed and a reason | Quality menu; unusable rungs are omitted | Omitted choices give no reason |
| Input → output header on the switcher | Source → delivered chip with arrow | Equivalent |
| Display Details (peak, black, primaries, formats) | Nothing viewer-facing; Android reads HDR capabilities and sends them without showing them | Not shown |
| Display target used for this playback | Admin Activity page only | Viewer cannot see their peak bucket |
| Per-format default playback | Global server switches | No per-format choice |
| "Show playback mode at start" notice | None found | Optional |
| Render load bar, drop/skip | Drops shown; no load figure | Minor |
| Upscaler, SDR presets, subtitle brightness/position | Not found | Covered in earlier sections |

**Where Cinema is already ahead:** four density modes, one field catalog
shared by every client, typed refusal messages, a quality change that keeps
the old stream until the new one is ready, and explicit "display output
unverified" labelling.

**Worth taking, ranked:**

1. **Metadata provenance as three lists.** In the file · applied by Cinema ·
   left for the display, each a list of levels. It answers "did my trims get
   used" at a glance and fits the existing Enhancement rows.
2. **Show the display target in Playback info:** reported peak, whether it
   came from the platform or the viewer's setting, the bucket chosen, and
   "this title already fits" when the bucket was elided. The server already
   records all of it.
3. **Greyed choices with a reason in the Quality menu,** instead of omitting
   them. The server's quality candidates already carry the reason.
4. **Group headings in the Linux player's info list:** Input · Processing ·
   Output, matching the other clients.
5. **A display details view beside the HDR display peak picker,** showing
   what the client can read about the display. Helps the viewer pick a peak.
6. **Live per-shot values** (current L1 min/max/avg, mastering peak) in the
   details mode.
7. **Per-format defaults** in Settings → Playback, if you want different
   handling for Dolby Vision, HDR10 and HLG. Lower priority.

**Not worth taking:** the rounded panel container (conflicts with the
flat-sections rule), engine and circuit version strings as headline
information, and CB1's mode naming, which has drifted between releases.

**Found in passing:** the Apple client sends only HDR and Dolby Vision
booleans and has no peak control, and the Linux player sends no peak, so
their enhanced HDR10 sessions cannot be display-targeted. Not traced further.
