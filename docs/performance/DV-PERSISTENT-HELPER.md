# Persistent DV helper: one helper per rendition's contiguous play

**Status:** built (phase 1) · 2026-11-15 · branch `claude/dv-persistent-helper`.

Context: [DV_HDR_PROCESSING_STATUS.md](../streaming/DV_HDR_PROCESSING_STATUS.md), [backlog §7](../streaming/DV-ENHANCEMENTS-NOT-YET-BUILT.md).

A processed rendition produced each 2 s window with a fresh helper: it opened
and probed the source, created its VA-API and Vulkan/libplacebo state and
decoded the keyframe preroll before every window's first picture, and the
window's encoder opened the source a second time for its soundtrack. Live on the
lab node that cost 2.6-3.1 s per BaseRpu window and 3.2-3.7 s per FEL window,
against the 1.33 s the client's seam needs.

- **Session.** `segment_decode_render --session 5` keeps one helper for
  consecutive windows (`dv_segment/session.rs`). Each window command on the
  helper's SOCK_SEQPACKET control socket carries its exact bounds, picture
  cap and soundtrack interval, with the window's NUT pipe, private evidence
  directory and soundtrack pipe as SCM_RIGHTS. Demuxer, decoders and GPU
  state carry over, so only the session's first window seeks and decodes
  preroll. A window must start where the previous one ended; any other
  command is refused.
- **Each window is still its own proof.** Its own encoder, private spool,
  evidence (`renderer.jsonl`, `timing.tsv`, `stages.tsv`, `rpus/`, opened
  with `*at()` in that window's directory), deadline, observation, finalize,
  receipt and publication. The read envelope (128 MiB), packet and picture
  bounds and the 3 s window cap are per window.
- **Ownership.** The session holds the source custody and the admission
  with its worker claim (the helper reads through its custody URL, so it
  holds no source-offset lane). The rendition keeps it between windows
  (`HelperSessionSlot`); every retirement, a dropped session's included, is
  counted in the rendition's `SessionHome`, and a cold window waits for the
  reap.
- **Lifetime.** The session survives the self-clearing pacing hold
  (`Hold::Ahead`, the ahead stop included) and `MakeRoom`, which frees
  segment bytes the session does not hold. It ends on idle and the
  indefinite holds, a seek or discontinuity, a restart, a terminate, a
  handoff to a successor, a close, a lost read authority or a configuration
  change. Restart, terminate and the handoff wait for the reap, so the
  capacity handed on is free. A window failure, or a helper that exits
  between windows (stopped from Activity, a crash), fails the window as
  before. A lost read authority, even one that makes the helper exit
  mid-window, does not fail the episode. The processed trial is a session
  of one.
- **Soundtrack (option C).** The helper carries the selected soundtrack's own
  packets, unchanged, from 200 ms before the point the encoder's lattice
  filter rebases on to half a second past the window. The pipe keeps the
  source's time base: Matroska for a Matroska source, NUT otherwise, and
  Matroska for a codec NUT cannot tag (TrueHD); the encoder probes which.
  The daemon passes the probed channel count (`--audio-channels`), which
  the helper's FFmpeg (no audio decoders or parsers) cannot read from an
  MP4's AAC configuration. Before a picture blocks on the video pipe, the
  helper demuxes the soundtrack half a second past it, so a source that
  stores audio behind its video does not stall the encoder. The encoder
  never opens the source; the AAC lattice filter and finalize's checks are
  unchanged.
- **Attribution.** Activity's process list names what the helper serves
  ("Processed DV for <title> · rendition <key>"), and the playback's
  processing line shows the helper's pid, start, windows and whether one is
  in flight. Stopping it there fails the next window, which falls back.
  Start and end are logged with the rendition and pid.
- **Stage rows.** `helper_spawned` is when the helper was handed the
  window's command; `helper_exited` is its acknowledgement, after the
  window's soundtrack tail is written.

Hand measurement on an idle AMD lab node, inside the plurxd container, the session helper
writing to the window encoder's shape (VA-API HEVC Main10, AAC from the
carried soundtrack), consecutive windows back to back:

| Title | First window (cold) | Later windows |
|---|---|---|
| Axel F (P8 BaseRpu, long GOP), 2 s from 612 s | 1.58-1.78 s | 0.85-0.91 s, mean 0.88 s |
| Avatar (P7 FEL, TrueHD 7.1), 2.002 s from 600.6 s | 1.69 s | 0.89-1.04 s, mean 0.93-0.95 s |

The daemon adds verification and publication (120-210 ms per window live).
`tools/dv_processing/test_session.py` is the helper control: session windows
are byte-identical to single-shot windows (NUT, RPUs, timing, coded trace),
later windows decode no preroll, the soundtrack is the source's own packets,
and a non-contiguous window is refused. `--pcm-join` compares the encoder's
lattice PCM from the carried pipe with main's source read per window and
across joins (bit-identical for FLAC, PCM and Opus; sample-aligned within
main's own start dependence for E-AC-3, AC-3 and AAC in MP4; within 1 ms for
DTS and TrueHD on a millisecond clock, as on main). `--session-only --growth`
checks descriptors and RSS over a long session (300 windows: flat). The
physical probe `PLURX_DV_WINDOW_SESSION=1` runs the daemon's own path,
`execute_with_source_input`, over consecutive windows with the real helper
and encoder through finalize.
