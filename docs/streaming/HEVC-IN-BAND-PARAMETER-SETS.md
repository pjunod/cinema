# HEVC in-band parameter sets — why UNABOMBER plays pink and green

**Status:** open — built on `fix/hevc-inband-retention`, stacked on the #535
re-land (PR #546); device qualification pending · **Written:** 2026-09-26 ·
**Builds on:** [HEVC-COLOR-CORRUPTION-RCA-AND-FIX.md](HEVC-COLOR-CORRUPTION-RCA-AND-FIX.md)

Companion to [STUTTER-4K.md](STUTTER-4K.md), whose parameter-set deletion this
narrows. Read §1 for the cause, §3 for the fix, §5 for what is still
unproven, and §6 for the live copy trace (2026-11-27).

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

1. **Its code never reached `main`.** It merged at 00:13:40 UTC on 2026-09-26
   as `70264bf7`, and two seconds later Forgejo's push mirror to GitHub,
   finishing a sync of an older snapshot, wrote `main` back to `ea215603`
   through its `fetch = +refs/*:refs/*` refspec (the bare repository's reflog
   records it as `Gitea … update by push`). The same race had erased #225,
   #460 and #509. The refspec is removed and PR #546 re-lands #535 and #509.
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
probe comparison ignores it, and so does the shared-library file revision
(`FILE_REVISION_PROJECTION_SQL` projects the probe without it).

**Shared (Source) starts.** A receiver's copy of a shared title is cut on the
Source, which must not move the file revision the grant was witnessed against
and must not start children outside its owner. Both now hold: the Source
actor measures the census itself (`hevc_census::ensure_source_census`), under
its preparation authority and on a held fence of the file, before its growing
copy or cold index reads the probe, and recording it leaves the revision
standing. Before this, the Source read the stored probe as it stood, so the
first play of an uncensused title anywhere, if it was a shared one, copied it
with the in-band sets deleted: the copy's HEVC trace refused the change
(`hevc_configuration_unsupported`) and the title could not be copied from that
Source until a local play or an index pass measured it.

Taking the census out of the revision re-keys, once, every shared file that
already carries one (and any whose stored probe is not SQLite's canonical
text; scans store canonical serde text). While a cluster runs both builds,
old and new nodes compute different revisions for those files, so a Source
start of one may be refused until every node runs the new build; receivers
re-resolve it as any other revision change.

**The decision.** `CopyVideoOptions::from_probe` reads the record, beside the
`extradata_size` fact that already drives empty-`hvcC` promotion, so every
copy path and every node decides from the same replicated row:

| Source | Bitstream filter | `merge` | Index identity |
|---|---|---|---|
| In-band sets repeat the record, or none (Blu-ray remux, most encodes) | unchanged: deletes 32–34 | unchanged: deletes 32–34 | unchanged |
| In-band sets redefine the record (UNABOMBER) | keeps 32–34; DV layers handled exactly as before | keeps them | new (a recipe marker, on every branch) |

The `hvc1`/`dvh1` label is unchanged. Only the second row's files change at
all, so there is no library re-index, no protocol bump and no refusal.

**Composition with #535's proof.** #535 admits a VOD copy that deletes the
in-band sets only when a full original-header trace proves them constant, and
refuses rolling and progressive HEVC copy outright. A copy that keeps them
cannot decode against stale definitions, so this change narrows #535 to the
copies that actually delete something:

| Copy | Parameter sets | Admission |
|---|---|---|
| VOD, census says they vary | kept | no proof needed |
| VOD, census says they repeat the record | deleted (today's bytes) | #535's proof, or the Developer override |
| Rolling (the fallback while a title is unproven) | always kept | no proof, no override |
| Progressive `stream.mp4` | always kept | no proof, no override |

So an unanalysed title plays with correct colour through the rolling
fallback instead of being refused, UNABOMBER plays through VOD once its
retaining index exists, and the Developer override now matters only for a
VOD copy that would delete unproven parameter sets.

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
`::an_unreadable_file_records_nothing_and_is_not_retried_at_once`,
`crates/plurxd/src/transcode/tests/source_actor.rs::source_first_copy_of_an_uncensused_varying_hevc_title_keeps_its_parameter_sets`,
`crates/plurx-core/src/store/sharing_catalogue_details.rs::recording_the_hevc_census_keeps_the_shared_file_revision`.

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

## 6. The live copy trace — Avatar and 3 Body Problem (2026-11-27)

The pre-filter trace that proves a copy's parameter sets (`hevc_configuration::Trace`)
refused two shared titles that copy safely. Fixed by #1996.

### 6.1 Finding — three faults, one message

On 2026-11-27 two shared titles stopped at once with "The sharing host could
not confirm playback preparation", while an H.264 title played. The copy owner
(FFmpeg 8.1) refused both with `hevc_configuration_unsupported`:

- **Avatar: Fire and Ash** (DV Profile 7, converted to 8.1) —
  `HEVC parameter sets change; immutable copy is unsafe`. Its full-source
  index says the sets never change, and they do not. The live copy transcodes
  TrueHD, and FFmpeg writes that output's header dump once the encoder opens,
  in pieces, from another thread. A `trace_headers` line printed between two
  pieces loses its `[trace_headers @ …]` prefix and is glued onto the piece
  (`    title           : 2742        scaling_list_delta_coeff[2][0][54] …`).
  The parser ignored prefix-less lines, so the next repetition of the SPS
  lacked a field and read as a change. Reproduced on the owner node in two of
  three runs; the index pass (`-an`) never hits it.
- **3 Body Problem S01E01** (WEB-DL, 23-byte `hvcC`) — `source extradata
  lacks a complete decoder configuration`. Its copy promotes its parameter
  sets (`hevc_mp4toannexb,extract_extradata` writes the decoder record from
  the first packet), but the trace demanded them in extradata before the
  first packet. The same run also lost a `Slice Segment Header` line to the
  dump.
- **The viewer's server** stored the Source's typed 422 and woke its waiter,
  then began the retirement that cancels the start; the waiter read the
  cancellation first and answered `sharing_start_unresolved` instead of the
  refusal the player turns into "Convert it to play".

### 6.2 Fix

- `hevc_configuration::Trace` reads a prefix-less line that ends in a trace
  field by CBS's own layout (`%-10d  %s%*s = %d`: the position fills the
  twelve columns before the name), and only when it is later in its unit than
  the field before it. A unit whose header line was lost is rebuilt from its
  `nal_unit_type`; a glued `Packet:` line is read by its exact form. Anything
  else trace-like fails the inspection (`trace line lost its prefix`, `trace
  unit header lost`), never the source's configuration; so does a change
  that a recovered field accounts for, since text the file controls (a
  title in the dump) can take a field's form.
- `Trace::for_copy(promotes)`: a promoting copy takes its sets from the first
  packet, before its first slice; later changes are still refused. Observed
  copies and index passes pass their recipe's promotion.
- `ReceiverStartActor::wait_ready` reads a stored result before a stop.

### 6.3 Open

- Index proofs written before this fix kept their refusals: on the Source
  node, 35 `extradata lacks …` and 83 `parameter sets change`. A node now
  records the reader generation on its refusals (`Proof::parser`,
  `hevc_configuration::PARSER = 2`). An older refusal requires a fresh
  analysis. Verified VOD falls back to the local index when a portable copy
  index contains an unstamped refusal. Until the local proof is ready, VOD
  answers `hevc_configuration_unverified`. Existing permits remain valid,
  and portable index bytes and identity retain their existing encoding.
- Replay Avatar and 3 Body Problem S01E01 on the M4 Air after deploy.
