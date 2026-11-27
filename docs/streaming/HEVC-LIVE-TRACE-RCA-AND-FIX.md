# HEVC live copy trace — why Avatar and 3 Body Problem were refused

**Status:** open — fixed on `claude/hevc-live-trace`; replay on the shared
library pending · **Written:** 2026-11-27 · **Builds on:**
[HEVC-IN-BAND-PARAMETER-SETS.md](HEVC-IN-BAND-PARAMETER-SETS.md)

Read §1 for the causes, §2 for the fix, §3 for what is still open.

## 1. Finding — three faults, one message

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

## 2. Fix

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

## 3. Open

- Index proofs written before this fix keep their refusals (`current_on_node`
  treats a refusal as current): on m6, 35 `extradata lacks …` and 83
  `parameter sets change` refusals. Re-examining refusals written by the old
  parser needs a proof field, not a `REVISION` bump, which would re-index
  every HEVC file.
- Replay Avatar and 3 Body Problem S01E01 on the M4 Air after deploy.
