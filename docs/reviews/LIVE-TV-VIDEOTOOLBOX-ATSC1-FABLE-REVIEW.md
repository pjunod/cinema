# Review — ATSC 1.0 on VideoToolbox, caption-triggered encoder failure

**Verified against:** origin/main @ `c9e4edf4` (Forgejo, 2026-09-16 19:36 UTC).
**Candidate read from:** Paul's working tree at `~/code/plurx`, HEAD `10f2afe6`
(the doc's stated base) — the only case where the worktree is the source.
**Reviewer:** Fable, 2026-09-16.

## Verdict

**The encoding fix is correct and independently proven. The candidate as it
stands cannot land: it is built against a base 534 commits behind main, and
its hardware test fails deterministically on main.** Ship the one-line
`-a53cc 0` with its two tests as a small PR after a rebase; rework the two
diagnostic additions separately (§ blockers B3, B4).

What I ran, not reasoned:

| Check | Where | Result |
|---|---|---|
| Caption fixture vs plain fixture, FFmpeg 9.0.1 / macOS 27.0 (`maca`, the Mac runner) | mac | plain: exit 0 · captioned, VT default: **exit 183**, `Unexpected end of SEI NAL Unit parsing size` + `Error copying packet data: -1094995529` · captioned, `-a53cc 0`: exit 0 · captioned, libx264: exit 0 · captioned VT into HLS: exit 183 after **1** listed segment |
| Same, VOD shape (`-hwaccel videotoolbox` decode → `h264_videotoolbox`) | mac | **exit 183** — same failure |
| Same with `hevc_videotoolbox` | mac | exit 0 |
| Single frame (`-frames:v 1`) | mac | exit 183 — fails on the first caption-bearing frame, not a rare one |
| Candidate hand-ported onto main `c9e4edf4`; `cargo test … live_caption` | lab4 | 2 passed |
| `cargo test … live_tv::` on the ported tree | lab4 | 134 passed (main has 134, not 75) |
| `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check` on the ported tree | lab4 | pass |
| `tests/operations/test_docs_index.py` with the doc + README row on main | lab4 | 4 passed |
| Hardware test, candidate, on main | mac | **FAILED**, deterministic (3/3 runs) — see B2 |
| Hardware test with the helper returning nothing (old code) | mac | FAILED at `feed MPEG-TS: Broken pipe` — the test does reject the old code |
| Mutation M1 helper → `&[]` | lab4 | `live_caption_forwarding…` fails ✔ |
| Mutation M2 helper applies to every encoder | lab4 | `live_caption_forwarding…` fails ✔ |
| Mutation M3 recognizer matches only the `type` variant | lab4 | `…contains_no_raw_media_metadata` fails ✔ |
| Mutation M4 recognizer *also* sets `decoder_unavailable` | lab4 | **both tests still green** — see B4 |
| Mutation M5 recognizer never receives stderr (window never filled) | lab4 | **both tests still green** — see B4 |

## Blockers for the encoding fix

**B1 — Stale base; half the diff is already on main in another shape.**
`crates/plurxd/src/live_tv.rs`. Since `10f2afe6`, main restructured this file
(+3,423/−354): `live_ffmpeg_command` is now a thin wrapper over
`live_ffmpeg_command_for_input(…, LiveTvFfmpegInput::{Tuner,GraphProbe})`, so
production and the readiness probe already share one builder;
`spawn_live_ffmpeg` returns `(Child, ChildJob)`; `-force_key_frames` already
precedes `encode_args`; the lifecycle test already has a fake FFprobe; and
`LIVE_HLS_OUTPUT_ARGS` is now 1 s segments / list 24 (the start-stall fix).
`git apply --3way` of the candidate conflicts. Consequences: the
`append_live_hls_output` extraction and the second `live_caption_args` call
site in `run_graph_probe` are dead on main (one call site inside the `Encode`
arm of `live_ffmpeg_command_for_input` covers both — I verified the ported
tree emits `-a53cc 0` for the probe shape too); the hardware test needs
`let (mut child, _job) = spawn_live_ffmpeg(…)`; §5's contract table, §6.1 and
§9 describe a file that no longer exists. Correction: rebase onto main, keep
only the helper, one call site, the two portable tests, the test file and the
docs. Everything §9 lists as "pre-existing" is a stale re-derivation of merged
work and should be dropped, not preserved.

**B2 — The hardware test fails on main, deterministically, for a reason
unrelated to captions.** `videotoolbox_tests.rs` per-segment decode loop
(`-map 0:v:0 -map 0:a:0 -f null -` with `-xerror`). With main's 1 s cadence
the tail segment of the 480i case holds one video frame and zero audio
packets (reproduced by hand: `seg-000004.ts v=1 a=N/A` on a 4 s fixture), so
FFmpeg 9.0.1 aborts with `Neither number of channels nor channel layout
specified … Nothing was written into output file`. On the 4 s cadence of the
candidate's base the tail frame shares a segment with audio, which is why the
doc's "4/4 pass in 10.75 s" held there. So §7's evidence does not transfer
to main, and §11 step 1 is not a formality. Correction: decode video with
`-xerror` on every listed segment and assert audio packets on every segment
except the last (or extend the sine to outlast the video and trim with the
output `-t`); then re-run all four cases on main and record the SHA.

## Blockers for the diagnostic additions (not for the encoding fix)

**B3 — The terminal `warn!("Live TV session failed")` fires on the routine
end of every viewing session and duplicates a log main already has.**
`run_live_session`. On main the steady-state loop breaks with
`Err(CapabilityExpired("the live-TV capability became idle"))` at
`CAPABILITY_IDLE_TIMEOUT` (45 s) — the normal way a session ends when the
viewer walks away — and with `CapabilityExpired("the provisional live-TV
start was not activated")`. Both reach the new `warn!`. And since
`e44b5426a` (after the candidate's base) `retire_session` already emits
`info!("Live TV session ended", channel, reason = code, duration_s,
tuner_bytes, user)`. The candidate's line adds one thing that record lacks —
the sanitized `cause` — and everything else is a second, less informative
copy. Correction: drop the new `warn!`; add `cause =
%sanitize_error(&error.to_string())` to the existing "Live TV session ended"
line (that also answers §10 Q6's correlation question: channel, user and
duration are already on it). Privacy: the same message is already returned
to clients via `state.error`, so logging it widens nothing.

**B4 — The recognizer is untested where it lives, and the test that claims
to keep it out of the decoder classification cannot fail.** `capture_live_stderr`
/ `live_caption_encoder_diagnostic_contains_no_raw_media_metadata`. The
assertion `classify_live_source_error(Err(StreamFailed), false)` stays
`StreamFailed` never touches the recognizer; mutation M4 (recognizer sets
`decoder_unavailable`) and M5 (recognizer never fed) both leave the suite
green. The only tested unit is a substring match on a byte slice. Correction,
and the better design: make the diagnostic a session fact, not a log line —
an `Arc<StdMutex<Option<&'static str>>>` beside `decoder_unavailable`, set by
`capture_live_stderr`, consumed by `classify_live_source_error` to enrich the
`StreamFailed` *message* while keeping code `stream_failed`. Then it is
testable by driving `capture_live_stderr` with `tokio::io::simplex` (split
message across two 1,024-byte reads, descriptor-chunk case, once-only), it
reaches the API `state.error` and the end-of-session log for free, and the
"not a missing decoder" claim becomes a real assertion.

## Other findings

**F1 — DVR recordings will hit the identical failure on VOD playback
(medium; follow-up, not this PR).** `dvr.rs` on main records the tuner's
MPEG-TS byte-for-byte ("no encoder, no FFmpeg process"), captions included.
Playing that recording back on a Mac owner goes through
`plurx-core/transcode/encoder.rs` → `h264_videotoolbox` with no `-a53cc`, and
I reproduced the VOD shape (`-hwaccel videotoolbox` decode, VT encode) failing
exit 183 on the first frame. §11's non-goal "VOD caption policy" is right for
scope but the doc should say plainly that every MPEG-2-with-captions file —
DVR output first among them — is broken on VT hosts today, and open the
follow-up.

**F2 — The caption-loss tradeoff is understated (low).** `hls.js` (bundled
in `web/`) and ExoPlayer both render CEA-608 from H.264 SEI by default, and
libx264 / NVENC / QSV keep `a53cc` on. So after this change a Linux host
forwards live captions to the web client and a Mac host does not. "Live
captions are unsupported" is the product position, but the doc should state
the per-host asymmetry rather than "loss … on builds that handle them".

**F3 — §10 Q4: the fixture is representative.** `00 00 01 B2 GA94 03 41 FF
FC 94 20 FF` parses as user_data_type 0x03, process_cc_data=1, cc_count=1,
one valid 608 field-1 RCL pair with odd parity, marker 0xFF; inserted before
each first slice (`00 00 01 01`), which MPEG-2 start-code uniqueness
guarantees is the right place. It triggers the real failure through the real
production command (old-code run above). The `!frames.is_empty()` guard keeps
the all-frames assertion from being vacuous.

**F4 — §10 Q5: the before-EOF check is real but weak.** `write_all` returns
when the last byte enters the pipe, so the check proves only that two entries
appear without a muxer flush. Fine as a guard. The test does not cover 5.1
AC-3 (`-ac 6`, 384k — the reported channel); one 5.1 case is cheap and worth
adding since it is the reported input.

**F5 — When the old code is under test the failure message is `Broken
pipe`, not the SEI line.** The write task panics before `wait_with_output`
runs, so FFmpeg's stderr is never printed — exactly the masking §7 admits.
Correction: on a write error, `child.wait_with_output()` first, then panic
with its stderr.

**F6 — Doc corrections.** §7's "Pass ×4" table and §8's commands describe
the stale base; `cargo +1.97.1` (PLAYBACK-TESTING) vs `rustup run 1.97.1`
(§8) — `rust-toolchain.toml` already pins 1.97.1, the real trap is Homebrew's
cargo ignoring it, say that instead; §4.1 and §5 decision 4 are fine; §6.1
must be rewritten around the existing "Live TV session ended" record. Nothing
in the doc or test violates the public-mirror naming rules.

## Answers to §10

1. Yes; independently reproduced on a second Mac with a second fixture, and
   the claims are not stronger than the evidence — except §7's transferable
   "pass", which B2 refutes on main.
2. VideoToolbox-only live suppression is the right scope for this PR; add
   the per-host asymmetry (F2) and the DVR/VOD follow-up (F1).
3. On main there is one placement, not two; the probe inherits it. Copy,
   other encoders and VOD unchanged — verified by test and by mutation M2.
4. Yes (F3).
5. Meaningful but weak (F4); add one 5.1 case.
6. Not safe as leveled — it warns on every idle-reaped session (B3). No new
   correlation id needed; the existing end-of-session line already carries
   channel/user/duration.
7. Not acceptable as tested (B4); fold the diagnostic into the session
   error and test through `capture_live_stderr`.
8. Yes, separate them. The fix is one helper, one call site, two tests, one
   test file, docs.
9. Keep `stream_failed`; enrich the message, not the code.

## Rerun before pushing

On the rebased branch: `cargo test -p plurxd --bin plurxd live_caption
--locked`, `… live_tv:: --locked` (expect 134), the ignored hardware test on
a Mac with host encoder access (all four cases), clippy `-D warnings`, fmt,
`python3 tests/operations/test_docs_index.py`. The Mac runner now has a warm
`~/vt-review/target` for plurxd tests (built with Xcode's SDK — the
CommandLineTools SDK fails `aws-lc-sys` with "unknown architecture"), so a
re-run there is ~30 s, not a cold build.