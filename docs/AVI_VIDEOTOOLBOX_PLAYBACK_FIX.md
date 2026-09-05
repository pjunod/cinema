# AVI VideoToolbox playback fix — keep cheap legacy decode on the CPU

**Status:** proposal for review; no implementation authorized · **Source report:**
[GitHub issue #913](https://github.com/pjunod/plurx/issues/913) · **Delivery:**
doc-only Forgejo branch `codex/issue-913-avi-review` · **Written:** 2026-09-05

Companion to [VALIDATION.md](VALIDATION.md) (what evidence a change must
produce) and [DEVELOPMENT_PIPELINE.md](DEVELOPMENT_PIPELINE.md) (how that
evidence reaches Forgejo) — this document explains why one SD MPEG-4 AVI was
garbled, the proposed decode policy correction, and the proof an implementation
must produce. The original media file is not present in this worktree, so the
planned proof pins the reported media facts and generated FFmpeg argument
boundary; final acceptance still includes replaying that file after deployment.

## 1. Decision — software-decode light VideoToolbox sessions

Use VideoToolbox to decode only a source that
[`heavy_source`](../crates/plurx-core/src/transcode/mod.rs) classifies as heavy:
HEVC (`hevc` · `h265` · `hevc10`) that is HDR or at least 2160 pixels high.
Decode every other VideoToolbox-encoded session in software, including the
reported 624×352 Xvid/MPEG-4 Part 2 AVI. Continue using
`h264_videotoolbox` for the output encode.

The proposal changes one boundary:

```text
                         ┌──────────────────────────────┐
 source ──▶ heavy HEVC? ─┤ yes: VideoToolbox decode     ├──▶ filters
             │           └──────────────────────────────┘
             no
             ▼
        software decode ───────────────────────────────────▶ filters
                                                                    │
                                                                    ▼
                                                        VideoToolbox encode
```

This is a decode correction, not a hardware-transcoding rollback. The output
encoder remains on the hardware path in both branches.

## 2. Reported failure — accepted input produced corrupt frames

The source report describes this file:

| Property | Reported value | Why it matters |
|---|---|---|
| Container | AVI, written by Nandub 1.0rc2 | Legacy interleaving and packet layout |
| Video | MPEG-4 Part 2, XVID fourcc | Not HEVC and therefore not a heavy source |
| Profile | Advanced Simple Profile | Contains tools older hardware paths may mishandle |
| Pixel format | `yuv420p` | Ordinary 8-bit software decode is inexpensive |
| Frame | 624×352 at 25 fps | Far below the size that needs hardware decode |
| Video bitrate | 1,599 kbit/s | Does not create CPU decode pressure |
| Audio | MP3, 48 kHz stereo | Produces separate malformed-packet warnings |
| Observed output | Macroblocking and large green regions | Consistent with bad decoded video frames |
| Control players | VLC and Plex render correctly | The stored media is decodable |

The plurx session selected Apple VideoToolbox and emitted repeated FFmpeg video
decoder failures:

```text
[dec:mpeg4] Error submitting packet to decoder: Unknown error occurred
[mpeg4] No frame decoded?
```

The session nevertheless produced a first frame and remained playable enough
for AVPlayer to display corrupted output. That distinction matters: this was
not an encoder startup failure or an HLS delivery failure. FFmpeg admitted the
hardware decoder, the decoder returned unusable results, and the rest of the
pipeline faithfully packaged them.

The MP3 `Header missing` warnings are real but do not explain green video
planes or MPEG-4 decoder failures. They remain outside this fix so video
correctness is not coupled to an unrelated audio-repair policy.

## 3. Root cause — encoder selection accidentally forced decoder selection

[`hls_args`](../crates/plurx-core/src/transcode/mod.rs) asks `decode_setup` for
input-side flags and independently asks the selected encoder for output-side
flags. Before this change, selecting `Encoder::VideoToolbox` always added:

```bash
-hwaccel videotoolbox               # decode every input through VideoToolbox
```

That happened for every codec, resolution, bit depth, and container. The
resulting command had this effective shape:

```bash
ffmpeg \
  -hwaccel videotoolbox \
  -i episode.avi \
  -vf <software-filter-chain> \
  -c:v h264_videotoolbox \
  <hls-output-options>
```

The coupling violated the policy already expressed by `heavy_source` and the
surrounding comments: hardware decode is worth its compatibility risk only
when software decode cannot produce segments fast enough. An SD MPEG-4 stream
is the opposite case. The CPU can decode it cheaply, while the issue provides
direct evidence that the VideoToolbox decoder cannot handle this particular
stream correctly.

The causal chain is therefore:

```text
VideoToolbox chosen as H.264 encoder
        │
        ▼
unconditional `-hwaccel videotoolbox` added before the AVI input
        │
        ▼
VideoToolbox accepts the MPEG-4 Part 2 stream but repeatedly fails frames
        │
        ▼
FFmpeg filters and encodes damaged decode output
        │
        ▼
valid-enough HLS reaches AVPlayer with macroblocks and green planes
```

## 4. Proposed correction — make VideoToolbox obey the heavy-source gate

The implementation would add the same `heavy` match guard used by the Intel
decode paths:

```rust
Encoder::VideoToolbox if heavy => (
    vec![arg("-hwaccel"), arg("videotoolbox")],
    None,
),
```

When the guard does not match, `decode_setup` returns no decode flags. FFmpeg
then uses its software decoder but still receives the VideoToolbox encoder
arguments later in `hls_args`:

```bash
ffmpeg \
  -i episode.avi \
  -vf <software-filter-chain> \
  -c:v h264_videotoolbox \
  <hls-output-options>
```

The classification is deliberately based on decode cost, not the `.avi`
extension. Containers do not determine decoder safety, and a container-only
special case would leave the same legacy MPEG-4 bitstream exposed after a
remux. The issue fixture retains the AVI field because it is part of the
reported regression, while the production rule follows codec and workload.

### Heavy decode behavior is unchanged

A VideoToolbox session keeps hardware decode when both conditions hold:

1. The source codec is `hevc`, `h265`, or `hevc10`.
2. The source is HDR or its height is at least 2160 pixels.

That preserves the path built for 4K 10-bit HEVC, where software decode can
miss the first-segment deadline and leave the player gray. The proposal therefore
trades no known heavy-source throughput for compatibility on sources that do
not need accelerated decode.

### Explicit pipeline ownership still wins

`hls_args` has two higher-priority rules that remain unchanged:

- A pipeline that requires software decode gets no hardware decode flags.
- A vendor GPU pipeline with its own decode arguments continues to own its
  matching hardware surface path.

Light sources do not select those heavy GPU pipelines, so the reported AVI
would reach the guarded `decode_setup` branch. The proposal does not reorder
pipeline selection or alter filter graphs.

## 5. Alternatives — why the correction lives at decode policy

1. **Deny only `mpeg4`.** This is smaller by line count, but it keeps
   unnecessary hardware decode for every other light VideoToolbox source and
   contradicts the existing heavy-source contract. The observed failure is
   evidence of the broader risk that contract was written to contain.

2. **Deny only AVI.** This mistakes a container for a codec capability. The
   same video packets can appear in another container, and a different codec
   can appear in AVI.

3. **Disable all hardware work for the session.** Software encoding would
   avoid VideoToolbox entirely, but the issue implicates input decode. Keeping
   `h264_videotoolbox` preserves output throughput and limits the change.

4. **Set `PLURX_HWDECODE=off`.** The existing escape hatch is a useful
   operator workaround, not a product fix. It applies globally and would also
   disable the heavy HEVC decode path that exists to meet startup deadlines.

5. **Ignore or discard corrupt frames.** Decoder error flags can suppress
   failures or drop packets, but they cannot reconstruct frames the hardware
   decoder returned incorrectly. Hiding the warning would preserve the broken
   picture.

6. **Require an FFmpeg upgrade.** Decoder behavior can change between FFmpeg
   builds, but the product can make a deterministic routing decision today.
   A dependency upgrade is broader, harder to roll back, and does not encode
   the rule that hardware decode must earn its compatibility risk.

## 6. Regression proof — pin both sides of the boundary

The implementation should add two integration tests in
`crates/plurx-core/tests/videotoolbox_decode_policy.rs`.
Keeping the retained tests outside the production module would let
[`scripts/prove-fix`](../scripts/prove-fix) restore only the old production
file and demonstrate that the regression test turns red.

| Test | Fixture | Required assertion |
|---|---|---|
| `videotoolbox_software_decodes_light_mpeg4_avi` | AVI · MPEG-4 ASP · 624×352 · 8-bit · 1,599 kbit/s | No `-hwaccel videotoolbox`; yes `h264_videotoolbox` |
| `videotoolbox_hardware_decodes_heavy_hevc` | HEVC · HDR10 · 3840×2160 · 10-bit | Yes `-hwaccel videotoolbox`; yes `h264_videotoolbox` |

The first test must reject the current implementation because it
unconditionally inserted `-hwaccel videotoolbox`. The second test prevents a
future compatibility fix from widening into a performance regression for the
heavy source the hardware-decode path exists to serve.

The focused proof command is:

```bash
PATH=/Users/pjunod/.cargo/bin:/opt/homebrew/bin:/usr/bin:/bin \
  cargo test -p plurx-core --test videotoolbox_decode_policy
```

**How to read it:** exactly two tests should run and pass in the named test
binary.

The mutation-style proof restores only the production module from Forgejo
`main` while retaining the integration tests:

```bash
PROVE_CMD='cargo +1.97.1 test -p plurx-core \
  --test videotoolbox_decode_policy {filter}'

scripts/prove-fix \
  --command "$PROVE_CMD" \
  origin/main \
  videotoolbox_ \
  crates/plurx-core/src/transcode/mod.rs
```

**How to read it:** the proposed tree must pass both tests; after the script
restores the pre-fix production file, the light-AVI test must fail because
`-hwaccel videotoolbox` returns. The script reports `PASS` only when it sees
both halves.

## 7. Validation — evidence required before delivery

The repository pin is Rust 1.97.1. The unqualified Homebrew `rustc` on the
checkout host reports 1.95.0, so every Rust command in this review uses the
rustup shim first in `PATH` and verifies the selected compiler.

| Check | Command | Review meaning |
|---|---|---|
| Toolchain | `rustc --version` | Must report `rustc 1.97.1` |
| Focused regression | `cargo test -p plurx-core --test videotoolbox_decode_policy` | Both decode-policy tests pass |
| Formatting | `cargo fmt --all -- --check` | Changed Rust is canonical |
| Daemon compile | `cargo check -p plurxd --all-targets` | Core API and every daemon target compile together |
| Denied lints | `cargo clippy -p plurxd --all-targets -- -D warnings` | No new warning enters the shipping surface |
| Daemon suite | `cargo test -p plurxd --bin plurxd` | Transcode consumers retain behavior |
| Patch hygiene | `git diff --check` | No whitespace or conflict-marker defects |

The pull request should record exact results rather than saying “tests pass.”
If Forgejo `main` moves before push, rebase the branch onto the new base and
run this matrix again; evidence against the old tree does not qualify the new
one.

## 8. Acceptance — replay the reported file after deployment

The original file is not available in this worktree, so unit and compile proof
cannot claim that the exact episode has been watched. After an approved
implementation reaches the affected macOS node:

1. Start the reported episode from the beginning and seek to at least two
   later positions. Seeking matters because legacy AVI keyframe and packet
   boundaries can expose a decoder problem that a cold start misses.
2. Confirm the image has no macroblocking or green regions during motion.
3. Confirm the transcode log names `Apple VideoToolbox` as the encoder.
4. Confirm the FFmpeg command or debug log does not contain
   `-hwaccel videotoolbox` for this MPEG-4 source.
5. Confirm repeated `[dec:mpeg4] Error submitting packet` and
   `No frame decoded?` messages are absent.
6. Confirm audio remains present and in sync. Existing isolated MP3 header
   warnings may remain; treat audible loss or drift as a separate defect.
7. Play one known 4K HDR HEVC title and confirm it still starts through
   VideoToolbox hardware decode within the normal first-frame window.

**Pass:** the AVI picture is clean, H.264 encoding remains on VideoToolbox, and
the heavy HEVC control still hardware-decodes.

**Fail:** green/macroblocked frames persist after software decode, or the
heavy HEVC control loses its hardware decoder. Preserve the complete generated
FFmpeg argument list and decoder log in either case; those distinguish a
routing failure from a different corrupt-frame cause.

## 9. Rollout and rollback — one local policy change

The proposal has no schema, API, cache-format, client, or configuration
migration. Existing transcode sessions keep the arguments they started with;
new sessions receive the corrected routing after the daemon is replaced.

Roll back by reverting the production guard and its two tests if a measured
light-source CPU regression is worse than the compatibility gain. Do not use a
global `PLURX_HWDECODE=off` rollback for that case: it also removes required
acceleration from 4K HDR HEVC and changes more behavior than this patch.

Watch these signals during acceptance:

| Signal | Good | Investigate |
|---|---|---|
| AVI decoder log | No repeated MPEG-4 decode errors | `No frame decoded?` continues |
| AVI picture | Stable, correctly colored frames | Macroblocks or green planes |
| AVI startup | Comparable first-frame latency | New timeout or sustained CPU saturation |
| Heavy HEVC startup | Existing hardware-decode latency | Gray screen or missed first segment |

## 10. Non-goals — keep review scope honest

- **Repair malformed MP3 packets.** Audio warnings were reported beside the
  video defect, but they require their own reproduction and policy.
- **Change AVPlayer compatibility routing.** The client correctly requested a
  transcode for MPEG-4; the corruption arose inside server decode.
- **Change HLS muxing, pacing, or session retirement.** The session produced
  consumable HLS and retired normally when released.
- **Disable VideoToolbox encoding.** Hardware output encode is retained and
  explicitly asserted.
- **Redefine `heavy_source`.** Its HEVC/HDR/2160p threshold is existing policy;
  this change makes VideoToolbox follow it.
- **Generalize the correction to NVENC.** CUDA decode remains unchanged because
  issue #913 supplies no NVIDIA evidence. A separate change can reconcile that
  exception with the written light-source policy if production evidence calls
  for it.

## 11. Reviewer checklist — the questions that can reject this change

- [ ] Does the issue evidence support a decode failure rather than an encode
  or HLS packaging failure?
- [ ] Is `heavy_source` the right compatibility boundary for VideoToolbox, or
  is there measured evidence that a light source requires hardware decode?
- [ ] Does the light AVI test preserve hardware encode while removing only
  input-side VideoToolbox decode?
- [ ] Does the heavy HEVC test prevent accidental loss of the startup-critical
  hardware path?
- [ ] Are MP3 warnings correctly excluded instead of being silently declared
  fixed?
- [ ] Were all commands in §7 run with Rust 1.97.1 against the exact branch
  proposed to Forgejo?
- [ ] Was the original episode replayed according to §8 before calling the
  incident resolved?
