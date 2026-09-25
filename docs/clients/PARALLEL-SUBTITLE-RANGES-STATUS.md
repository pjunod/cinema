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

The implementation milestones are `ccb795c8`, `e4cbdb7c` and `ad7dda80`.
Their normal commit hooks passed catalog lint, pinned workspace/all-target
Clippy, formatting and JavaScript syntax. After the single adversarial
review, `ff7d1835` fixed cancellation during publication and all 10 range
regressions plus the unsigned-handler regression passed; §6 and §7 record
the review disposition and commands.

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

Both bounded runs made three input seeks and exposed two timestamp bases.
The sparse-cue correction in §5 supersedes this initial output-seek variant
and removes timestamp-base inference from the indexed path. These synthetic
results establish input seeking for this container and codec, not a fleet
latency measurement or a physical byte bound for every subtitle layout.

## 3. Implementation and verification

| Work | State |
|---|---|
| Pinned compiler loop and bounded-seek experiment | complete |
| Authenticated peer range execution and source fencing | implemented; signed request/response, digest and cue checks, per-peer/node limits |
| Parallel current/next window publication and playback integration | implemented; current extraction starts before peer discovery and publishes independently |
| Named regressions and documentation | complete; 10 range tests and 1 unsigned-handler test passed after review fixes |
| Single adversarial review | complete; cancellation finding fixed and acceptance limits recorded in §6 |
| Current candidate fast-lane qualification | pending; PR is ready for the non-draft qualification run |
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

The indexed path preserves absolute timestamps with `-copyts -start_at_zero`
and an input seek 60 seconds before the window. It uses output `-to` only;
output `-ss` is omitted because FFmpeg 5 rebases timestamps while newer
versions do not. Rust then filters preroll by exact cue timestamps, retaining
cues whose end crosses the anchor. Cues that began more than 60 seconds
before the anchor may remain unavailable until the complete track arrives.
PGS cannot use this path because palette and object state can cross display
sets. ASS/SSA display still uses VTT where already supported; styled burns
continue to require the complete Matroska representation.

Named regression anchors (passed after review fixes):

- `subtitle_ranges::tests::range_identity_rejects_stale_stamp_bad_grid_and_bitmap`
- `subtitle_ranges::tests::peer_range_rejects_malformed_nonfinite_and_wrong_timeline`
- `subtitle_ranges::tests::same_range_deduplicates_and_cancel_releases_claim`
- `subtitle_ranges::tests::absolute_window_keeps_spanning_cue_and_sparse_first_cue`
- `subtitle_ranges::tests::current_range_is_readable_before_slow_peer_and_cancel_drops_prefetch`
- `subtitle_ranges::tests::indexed_late_window_matches_full_scan_with_nonzero_source_start`
- `subtitle_ranges::tests::indexed_text_starts_window_after_midpoint_while_whole_track_is_healthy`
- `http::internal_media::tests::subtitle_range_rejects_unsigned_work_before_reading_file`

A second synthetic fixture on local FFmpeg 9.0.1 had video start 7.500 s and
subtitle start 8.500 s. Direct extraction returned cues at 1–4, 102–106 and
132–136 s. The initial candidate `-copyts -start_at_zero -ss 40 -i source
-ss 100 -to 200` returned 102–106 and 132–136 s exactly, with no normalization
shift. That experiment validated the initial output-seek candidate only.
The final command omits output `-ss`; §5 records its cross-version and
sparse-track evidence.

The time range selects overlapping cues found by the bounded preroll; it does
not bound the exact physical bytes read.
A sparse subtitle stream can make FFmpeg read past the nominal endpoint
until it encounters its next subtitle packet. The measured read ratios above
are specific to a cue-every-ten-seconds fixture. Input seeking avoids the
prefix scan; the 25-second peer deadline and bounded output remain the hard
limits when sparse media cannot complete as quickly.

## 5. Deterministic timestamp correction before review

The initial output-seek variant exposed a sparse-track ambiguity: a relative
first cue at 220 s in the 200 s window could be mistaken for absolute 220 s
instead of 420 s. The indexed path therefore never calls the first-cue
normalizer. Its final command omits output `-ss` and filters absolute cues
in Rust.

A disposable container from the existing nuc3 image ran both Debian FFmpeg
5.1.9 and Jellyfin 8.1.2, without starting the stopped service. The synthetic
fixture used a 7.5-second output timestamp offset and two text tracks: one
with near and sparse cues, one with only sparse cues. For both binaries,
full extraction and `-copyts -start_at_zero -ss 140 -i source -to 460` agreed
exactly on cues at 217.500–221.500 and 427.500–431.500 s. Preroll at
157.500–161.500 s remained absolute for Rust to discard. A sparse-only track
still returned 427.500–431.500 s, proving no first-cue inference is needed.
The disposable container was network-disabled, read-only apart from its
private temporary filesystem, and automatically removed afterward.

## 6. Adversarial review disposition

The single adversarial review found a cancellation race during publication:
blocking write/fsync/rename could finish after the async range owner settled.
Publication now stages through the held secure directory and orders its final
rename against owner cancellation with the same mutex. If cancellation wins,
the staged file is removed without publishing; if rename wins, cancellation
cannot settle until that rename finishes. A deterministic regression pauses
an actual staged write, cancels its owner, publishes a successor, then releases
the obsolete writer and checks that the successor survives.

Additional regression anchors (passed after review fixes):

- `subtitle_ranges::tests::cancellation_during_blocking_publish_prevents_obsolete_rename`
- `subtitle_ranges::tests::source_replacement_and_attestation_mismatch_reject_publication`
- `subtitle_ranges::tests::peer_refusal_and_timeout_preserve_readable_current_range`

The source regression uses actual sampled attestation and replaces the path
under a held descriptor. The peer-failure regression uses the production
fan-out seam and the cache reader used by HLS, covering refusal and the
30-second speculative deadline without losing the current local range.

Live signed handler/transport success and body-mutation rejection remain a
deployment qualification gap. The ordinary HTTP fixture has no replicated
membership and cannot sign a non-self request. Proving that path requires a
controlled two-member cluster, a long indexed Matroska source, and observable
cold HLS range requests through the real peer transport. The unsigned-handler
and payload-validation regressions do not establish that end-to-end proof.
No service was deployed or started for this change.

## 7. Verification after review fixes

At `ff7d1835`, Rust 1.97.1 passed `cargo check -p plurxd --all-targets
--offline`, formatting, and the normal hook's workspace/all-target Clippy
with warnings denied. The following focused commands passed on the checkout
host after review fixes:

```sh
rustup run 1.97.1 cargo test -p plurxd --bin plurxd subtitle_ranges::tests --offline
rustup run 1.97.1 cargo test -p plurxd --bin plurxd subtitle_range_rejects_unsigned_work_before_reading_file --offline
```

The range module ran 10 tests successfully; the HTTP filter ran one. The
actual FFmpeg fixture is included in those 10 tests. The macOS linker emitted
a large `__eh_frame` compact-unwind warning; neither compilation nor tests
failed. Windows has the matching secure-write API but was not exercised by
this host run. The main fast lane and live two-member acceptance remain
separate verification steps; these focused results do not claim either.
