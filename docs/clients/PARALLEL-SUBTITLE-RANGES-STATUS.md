# Parallel subtitle ranges — playback work shared across nodes

**Status:** open · **Updated:** 2026-09-25 · **Model:** gpt-6-astra ·
**Session:** none:openai:2026-09-25 · **Branch:** codex/parallel-subtitle-ranges ·
**PR:** [#517](http://192.168.4.7:3000/noirr/plurx/pulls/517)

## 1. Intended behavior

For embedded text subtitles during HLS playback, the serving node prepares
its current window while peers prepare the next two windows concurrently.
Each completed range becomes readable immediately in the existing window
cache. A range never becomes a whole-track publication. Current and next
ranges have separate file-stamp, ordinal, anchor and span identities.

One existing playback window owner bounds the fan-out to three ranges. Peer
requests use authenticated internal transport, sampled source attestation,
held descriptors and a source replacement check. Peers limit concurrent
work and time; failed peer work leaves the ordinary playback fallback
available. The current range has precedence over speculative ranges.

The existing full-track job still prepares durable complete tracks. PGS
palette/object dependencies and ASS/SSA burn styling require their existing
complete representations. This change accelerates text display windows;
it does not claim distributed PGS extraction or styled burn startup.
The client retry ladder is unchanged. No deployment is part of this work.

## 2. Evidence before implementation

The first milestone is `ccb795c8`. Its normal commit hook passed catalog
lint, pinned workspace/all-target Clippy, formatting and JavaScript syntax.
Behavioral test execution remains after the single adversarial review.

Pinned local Rust 1.97.1 was verified with `rustup run 1.97.1 rustc --version`;
`rustup run 1.97.1 cargo check -p plurxd --all-targets --offline` passed on the
unchanged base before Rust editing.

A private synthetic 300-second Matroska fixture on nuc3 had one video stream
and text cues every ten seconds. The bounded command used input seek to 90 s,
`-copyts`, and output bounds `-ss 100 -to 140`. It returned exactly cues at
100, 110, 120 and 130 s. Temporary files were removed after each experiment.

| Binary executed inside `plurxd` container | Full-source bytes | Bounded bytes | Timestamp base |
|---|---:|---:|---|
| `/usr/bin/ffmpeg`, Debian 5.1.9 | 11,836,386 | 2,065,526 | relative to output start |
| `/usr/lib/jellyfin-ffmpeg/ffmpeg`, configured production 8.1.2 | 11,836,410 | 2,458,742 | absolute source time |

Both bounded runs made three input seeks. Existing cue normalization accounts
for the two timestamp bases. These synthetic results establish a bounded
seek for this container and codec, not a fleet latency measurement. Boundary
cues and other formats still need explicit verification.

## 3. Implementation and verification

| Work | State |
|---|---|
| Pinned compiler loop and bounded-seek experiment | complete |
| Authenticated peer range execution and source fencing | implemented; signed request/response, digest and cue checks, per-peer/node limits |
| Parallel current/next window publication and playback integration | implemented; current extraction starts before peer discovery and publishes independently |
| Named regressions and documentation | written; execution deferred until the one adversarial review |
| One adversarial review, then focused/fast-lane tests | pending |
| Deployment and physical client measurements | outside this change |

The implementation owns `subtitle_ranges.rs`, subtitle window plumbing in
`subtitles.rs`, `subtitle_source.rs`, the HLS production adapter, internal
HTTP routing, and this status page. The owning agent does not edit client
retry policies or the existing subtitle extraction plan's historical record.

## 4. Scope and bounds

This is automatic playback work, with no new enablement switch or readiness
probe gate. It uses the existing session window owner and cache. Indexed
Matroska text windows can run beyond the file midpoint; unsupported
containers retain the older scan and its midpoint rule. The worker resolves
file IDs from the library; it accepts no caller-supplied path.

One requester can use one worker slot per peer; a node accepts at most two
range jobs. Identical in-flight range identities are deduplicated. The peer
worker has a 25-second deadline and the caller's complete speculative
fan-out has a 30-second deadline. A seek drops outstanding requester futures,
so late responses cannot publish there. A peer whose HTTP disconnect is not
observed immediately can finish within its worker deadline; this is a
bounded remote cancellation delay, not an immediate remote kill guarantee.

The window timeline keeps the existing ownership by cue start. Cues that
began before a window are not recovered by this path; normal contiguous
playback uses the preceding window's slack. This implementation does not
claim a complete arbitrary interval for very long crossing cues. PGS cannot
use this path because palette and object state can cross display sets.
ASS/SSA display remains converted to VTT exactly as before; styled burns
continue to require the complete Matroska representation.

Named regression anchors (written, not yet executed):

- `subtitle_ranges::tests::range_identity_rejects_stale_stamp_bad_grid_and_bitmap`
- `subtitle_ranges::tests::peer_range_rejects_malformed_nonfinite_and_wrong_timeline`
- `subtitle_ranges::tests::same_range_deduplicates_and_cancel_releases_claim`
- `subtitle_ranges::tests::current_range_is_readable_before_slow_peer_and_cancel_drops_prefetch`
- `subtitle_ranges::tests::indexed_late_window_matches_full_scan_with_nonzero_source_start`
- `subtitle_ranges::tests::indexed_text_starts_window_after_midpoint_while_whole_track_is_healthy`
- `http::internal_media::tests::subtitle_range_rejects_unsigned_work_before_reading_file`

A second synthetic fixture on local FFmpeg 9.0.1 had video start 7.500 s and
subtitle start 8.500 s. Direct extraction returned cues at 1–4, 102–106 and
132–136 s. The production-shaped `-copyts -start_at_zero -ss 40 -i source
-ss 100 -to 200` returned 102–106 and 132–136 s exactly, with no normalization
shift. This verifies the nonzero-origin choice on that version; the named
regression retains it for the pinned build environment.

The time range bounds output cue starts, not the exact physical bytes read.
A sparse subtitle stream can make FFmpeg read past the nominal endpoint
until it encounters its next subtitle packet. The measured read ratios above
are specific to a cue-every-ten-seconds fixture. Input seeking avoids the
prefix scan; the 25-second peer deadline and bounded output remain the hard
limits when sparse media cannot complete as quickly.
