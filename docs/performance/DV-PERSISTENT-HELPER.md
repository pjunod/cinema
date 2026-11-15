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
- **Ownership.** The session holds what the graph held: the source custody,
  the source-offset lane and the admission with its worker claim. The
  rendition keeps it between windows (`HelperSessionSlot`). It ends, and the
  next window starts cold, on a seek or discontinuity, a flow stop, idle or
  capacity decision, a reposition, a close, a lost source read authority or
  a configuration change; the old helper is reaped before the next one
  starts. A window failure, or a helper that exits between windows (stopped
  from Activity, a crash), fails the window as before. A lost source
  authority does not fail the episode. The processed trial is a session of
  one; its helper is not handed over.
- **Soundtrack (option C).** The helper carries the selected soundtrack's own
  packets, unchanged, from the decoder preroll start the encoder's lattice
  filter rebases on to half a second past the window, in a per-window
  Matroska pipe (NUT has no TrueHD codec tag). The encoder reads it as input
  1 with no seek and never opens the source. The AAC lattice filter and the
  finalize checks are unchanged.
- **Attribution.** The session is the one "DV streaming reconstruction"
  process in Activity for the rendition's play, stoppable there; its end is
  logged with its cause and window count.

Hand measurement on an idle AMD lab node, inside the plurxd container, the session helper
writing to the window encoder's shape (VA-API HEVC Main10, AAC from the
carried soundtrack), consecutive windows back to back:

| Title | First window (cold) | Later windows |
|---|---|---|
| Axel F (P8 BaseRpu, long GOP), 2 s from 612 s | 1.57 s | 0.85-0.92 s, mean 0.89 s |
| Avatar (P7 FEL, TrueHD 7.1), 2.002 s from 600.6 s | 1.69 s | 0.90-0.96 s, mean 0.93 s |

The daemon adds verification and publication (120-210 ms per window live).
`tools/dv_processing/test_session.py` is the helper control: session windows
are byte-identical to single-shot windows (NUT, RPUs, timing, coded trace),
later windows decode no preroll, the soundtrack is the source's own packets,
and a non-contiguous window is refused.
