# HEVC in-band parameter sets — why UNABOMBER plays pink and green

**Status:** open — built on `fix/hevc-inband-parameter-sets`; device qualification pending ·
**Written:** 2026-09-26 · **Supersedes:** the containment in PR #535, which never
reached `main`

Companion to [STUTTER-4K.md](STUTTER-4K.md), whose parameter-set deletion this
narrows. Read §1 for the cause, §3 for the fix, and §5 for what is still
unproven.

## 1. Finding — the copy path deletes definitions the pictures need

Every copied HEVC stream is tagged `hvc1`/`dvh1`, and every copy deletes the
in-band VPS/SPS/PPS (NAL types 32–34) with `filter_units`, then deletes them
again in `fmp4::merge`. That is lossless only when the in-band sets repeat the
decoder configuration record (`hvcC`). A Blu-ray remux does; deleting its
repeats is what fixed the 4K boundary stutter.

A chunk-encoded streaming WEB-DL does not. `UNABOMBER (2026) [WEB-DL
2160p].mkv` (Dolby Vision Profile 8.1, 10.6 GB, 6006 s) redefines PPS 0 at shot
boundaries and repeats the current definition at every keyframe. A full-film
census of every keyframe's in-band parameter sets, taken before any filter:

| Measurement | Value |
|---|---|
| Keyframes carrying VPS/SPS/PPS | 1,170 of 1,170 |
| Distinct in-band configurations | 4 |
| Transitions between them | 301 |
| `hvcC` PPS chroma QP offsets (cb/cr) | −6 / −8 |
| In-band at 0 s, 600 s, 3000 s, 6000 s | −4/−5, −1/−2, −1/−2, 0/0 |

With the in-band sets deleted, every picture is decoded against the record's
stale PPS, and chroma is dequantized with the wrong offsets: pink and green
blotches over a picture whose samples were copied unchanged. The RCA on the
PR #535 branch proved the mechanism with matched decoded-frame hashes; this
document adds the whole-film shape, which is what rules out any single
replacement `hvcC`.

## 2. PR #535 — why "it didn't fix it"

Two independent reasons.

1. **Its code never reached `main`.** It merged at 00:13 UTC on 2026-09-26 as
   `70264bf7`. PR #538 merged seven minutes later as `71d1c1ec` with first
   parent `ea215603` — the `main` from before #535 — so #535's change was
   dropped. `git merge-base --is-ancestor b7f47c16 71d1c1ec` is false, and the
   fleet was deployed from `71d1c1ec`.
2. **Even landed, it was containment.** For this title its own RCA states the
   expected outcome as "a clear refusal of unsafe copy". By default it also
   refused every HEVC copy without a full `trace_headers` proof, which a large
   4K film may not finish within the index budget.

## 3. Fix — keep the in-band sets when, and only when, they redefine the record

**The fact.** `transcode::hevc_census` records, per source revision, whether
any in-band parameter set disagrees with the `hvcC`. The daemon measures it
(`plurxd::hevc_census`) by copying seven single keyframes spread over the film
— about a second on a cold NAS-mounted 2160p film — and grafts the record onto
the file's stored probe JSON under `plurx_hevc_parameter_sets`, fenced to the
file's size and mtime. It runs on the playback copy decision path the first
time a file without a current record is played as a copy; encodes, direct
plays and background index discovery do not measure, so the library is never
swept. A run in which any sample failed or overran can record a disagreement
it saw but never agreement; otherwise it records nothing and the copy keeps
the historical behaviour. The record is not a source fact, so the held-source
probe comparison ignores it.

**The decision.** `CopyVideoOptions::from_probe` reads the record, beside the
`extradata_size` fact that already drives empty-`hvcC` promotion, so every
copy path and every node decides from the same replicated row:

| Source | Bitstream filter | `merge` | Index identity |
|---|---|---|---|
| In-band sets repeat the record, or none (Blu-ray remux, most encodes) | unchanged: deletes 32–34 | unchanged: deletes 32–34 | unchanged |
| In-band sets redefine the record (UNABOMBER) | keeps 32–34; DV layers handled exactly as before | keeps them | new (a recipe marker, on every branch) |

The `hvc1`/`dvh1` label is unchanged. Only the second row's files change at
all, so there is no library re-index, no protocol bump and no refusal.

**VOD stays available.** The index compares each clean fragment's promotion
inputs to decide whether one init can describe the film. A type the record
already configures is never promoted, so `PromotionInputs::from_fragment` no
longer counts it; retained redefinitions therefore do not make the film look
non-presentable, and each VOD segment carries its own definitions because the
source repeats them at every keyframe.

## 4. Evidence

| Check | Result |
|---|---|
| Software decode of the 84 s cut, frame 104 | source `670dfeb9…`; kept (DV and HDR10) `670dfeb9…`; deleted (DV and HDR10) `d7776804…` |
| Regression: a real two-pass x265 encode with PPS 0 redefined, through the production argv and segmenter | kept copy decodes frame-for-frame to the source; historical copy does not |
| Regression: the same film indexed with retention | `parameter_sets_constant = true`, no promotion inputs |
| Store contract | the graft is fenced to size/mtime and keeps the rest of the probe, on every backend |

Regressions: `crates/plurxd/src/hevc_census.rs::a_redefined_pps_is_measured_retained_and_decodes_to_the_source_pixels`,
`::a_retained_film_still_indexes_as_vod_presentable`,
`::an_unreadable_file_records_nothing_and_is_not_retried_at_once`.

## 5. Open — what is not yet proven

- **Apple and browser decoders honouring in-band redefinitions under
  `hvc1`/`dvh1`.** ISO/IEC 14496-15 says `hvc1` carries parameter sets only in
  the sample entry; FFmpeg's decoder applies in-band ones regardless. Chrome,
  Safari, tvOS and iOS need a physical check on UNABOMBER after deploy; the
  84 s cut from §4, served as four A/B variants, is the quickest check. If
  AVFoundation ignores them, the conformant follow-up is to carry all four
  PPS variants in `hvcC` under distinct IDs and renumber each slice header's
  `slice_pic_parameter_set_id`.
- **Chrome boundary stutter on retained titles.** STUTTER-4K attributed a
  per-boundary stutter to in-band parameter sets. Retained titles accept that
  risk in exchange for correct colour; unaffected titles keep today's bytes.
- **Coverage of the census.** Seven keyframes catch a film that redefines its
  parameter sets throughout. A film that redefines them only in a short
  stretch between samples is still copied the historical way.
