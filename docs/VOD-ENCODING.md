# Encoded VOD — produce the immutable recipe the playlist names

Companion to [VOD-M3-HANDOFF.md](VOD-M3-HANDOFF.md): this describes the
encoded producer behind the same immutable manifest, GET admission, and
eviction model. Copy still uses its source fragment index. Transcode and
subtitle burn use a frame-grid plan and a real FFmpeg encoder.

## One recipe fixes the bytes for every restart

The manager resolves the encoder, output grade, geometry, selected audio,
manual A/V correction, subtitle pixels, rate control, thread budget, and
output cadence before attachment. The FFmpeg build and length-delimited
production arguments form the encoded source fingerprint. A changed recipe
gets a different rendition key; a restart cannot silently substitute a
different hardware encoder or tone-map graph under the existing init URI.
The source's held device/inode, length, and nanosecond mtime/ctime identity
also enters that fingerprint and the burn-sidecar key. Attachment rechecks
that object identity; equal scanner size and whole-second mtime cannot make
a replaced film inherit old segments or subtitle pixels.

```text
normalized request
       │
       ├── copy ─────── source index / verified demuxer landing
       │
       └── encode ──── frozen recipe / rational frame grid
                              │
                       foreground admission
                              │
                       attested FFmpeg pipe
                              │
                    init + physical grid verification
                              │
                     immutable manifest / GET files
```

## Timing decisions and their costs

1. **Preserve a rational output cadence.** The probe's usable average or
   nominal rate defines an explicit output grid; it is not evidence that
   the input is constant-rate. The FPS filter samples VFR onto that grid.
   A GOP uses the nearest whole number of frames to two seconds: 48 frames
   at 24000/1001 occupy 2.002 seconds. The final entry rounds outward to a
   whole frame. There is no forced 30 FPS conversion.
2. **The published plan wins.** Each encoded fragment must start at a clean
   random-access frame and match the planned video DTS and duration before
   publication. Encoder keyframe flags alone are not proof. The source's
   final frame may be cloned to fill the last planned frame; video is then
   trimmed to the exact planned frame count.
3. **Burn on the source clock.** Libass and bitmap overlay see absolute film
   time before the generation is rebased. Otherwise a seek would restart
   subtitle cue time at zero. Extracted text is held by an open descriptor;
   the source and subtitle descriptors are duplicated before either is
   assigned its reserved child descriptor number.
4. **Audio owns a global sample lattice.** Output is AAC-LC at 48 kHz. An
   AAC frame is 1024 samples, while a 2.002-second video GOP is 96096 samples.
   Each generation therefore starts audio encoding on a film-global AAC
   boundary before the requested video cut. The runner discards encoder
   priming and preroll, then restores the same global audio phase. An
   independently regenerated entry must have the same audio interval as
   the original entry. Per-generation edit lists are disabled so a local
   priming offset cannot change the immutable init or shift audio twice.
   Source audio is decoded from film origin on a separate, independently
   opened and identity-checked descriptor, then trimmed on its corrected
   48 kHz sample clock. A seeked Matroska packet's millisecond timestamp
   cannot identify its exact decoded-sample ordinal: a tone/impulse join
   regression caught an eight-sample error in that attempted shortcut.
5. **No reordered video across the boundary.** Encoded VOD requests closed
   GOPs and disables B frames. This sacrifices some compression efficiency
   to make random-access and decoder-time ownership explicit. A later
   quality improvement can restore reordering only with production decode
   tests proving that leading pictures, init identity, and every planned
   boundary remain correct.

## Capacity belongs to a process, not a rendition

Each running encoded generation holds the shared foreground hardware or
software permit. The producer slot releases it only after confirmed child
reap, including restart, cancellation, and slot destruction. Head-only init
regeneration transfers its permit to the cancellation-independent reaper.
A dormant rendition and an already materialized cache hit hold no permit.

One rendition driver owns a queued foreground wait. It checks external pool
releases at most every 250 ms because non-VOD background jobs do not notify
the VOD registry. Holds, terminal failure, and close remove that queue
ownership. No new retry task is spawned for every GET.
Each new admission reads current hardware/software pool limits. Changing an
operator limit affects the next child, without confiscating a running
child's already-held permit. Encoder recipe choices remain immutable.
The related policy values come from one Store snapshot under a one-second
deadline. A hung read returns no permit and leaves no foreground waiter;
only the existing driver owns its bounded retry.

The executable is a captured canonical path and content digest. Its loaded
codec/filter dependency closure is hashed once and every object identity is
rechecked before spawn and publication. Text burn also fingerprints active
Fontconfig rules and every discoverable font object; the retained sidecar's
actual bytes cover embedded attachments. Encoded identities include a random
daemon-process identity, so two nodes, driver stacks, or daemon restarts never
regenerate bytes into one another's durable keyspace merely because their
encoder names match. Any dependency or font replacement in the running daemon
withdraws the recipe with a typed engine-changed result.

## Long seeks are correct, not yet qualified as transparent

The explicit two-hour benchmark creates a real indexed Matroska source with
5.1-channel 48 kHz AAC. On the local warm filesystem, fetching two entries at
7195.188 seconds and reaping the producer took 12.273 seconds alongside the
focused VOD tests, and 7.629 seconds in a separate serial run. The source was
576,959,395 bytes; the serial run consumed 12.189 seconds of child CPU between
preparation and confirmed reap. Both runs fit the
current 30-second materialization budget but is **not** product acceptance
for transparent seeking. Cold NAS scans and heavier audio codecs remain
unqualified; the HTTP wait may return Pending while preparation continues.

The next performance step is a source/track-specific seekable canonical
audio artifact, or an exact decoded-sample index with validated decoder
preroll. Its identity must include the strong source version, selected
track, channel transform and resampler recipe. Video generations can then
seek that exact sample lattice without rescanning film-origin source audio.
A packet-PTS index alone is insufficient. Preparation must expose its real
headroom/deadline, and handoff must leave the old stream alive until the
new video and audio are physically ready. The benchmark remains explicit
and repeatable; short-media unit tests cannot substitute for that evidence.

## Scope and qualification

This milestone supplies executable recipe production. It does not itself
implement client-ready handoff, Original wire semantics, or the development
settings UI. Those are separate consumers of the same media contract.

The production-linked regressions live in
[vodencode_tests.rs](../crates/plurxd/src/vodencode_tests.rs) and
[vodencode_manager_tests.rs](../crates/plurxd/src/vodencode_manager_tests.rs).
They read the files returned by VOD GETs and decode their bytes. Frame-grid
and process-ownership tests supplement that evidence; they do not replace
physical playback qualification on each supported hardware family and
client. The task PR records the exact compiler, FFmpeg build, commands, and
results for its frozen tree.

Producer and head regeneration drain stderr alongside stdout in one
cancellation scope with a bounded 8 KiB diagnostic tail, so a filter/codec
failure is visible operationally without letting an unlimited diagnostic line
block media production. A sidecar destination create, write, or flush error
stops and reaps the exact child before returning; cancellation drops both pipe
readers together and transfers that child to the runtime-owned reaper.

## Current task checkpoint — 2026-09-08

The preserved implementation has been recovered onto current streaming effort
`c60ca53b` on `codex/vod-recipe-production-recovery`. The original worktree
lost its Git common directory; recovery compared file contents against the
effort history, restored only the 18 changed VOD files, and then integrated the
current effort. Three overlaps were resolved in favour of current live-session
semantics while retaining the immutable recipe work.

The current combined source has passed:

- pinned Rust 1.97.1 daemon all-target compilation;
- workspace all-target Clippy with warnings denied;
- 91 VOD-serving tests with the explicit two-hour benchmark ignored;
- 13 fragment-generation, 17 admission, 18 FFmpeg, 18 subtitle, and 11
  producer-ownership tests;
- 214 daemon and 137 core transcode tests;
- the public typed-refusal regression and seven lifetime-inventory contracts.

The first broad daemon transcode run saw the recent-delivery boundary test
miss once under load. It passed alone and all 214 transcode tests passed on the
immediate broad rerun. The three process-state tests require permission for
Darwin `ps` to inspect their own children; all 11 pass together outside the
restricted process sandbox. Neither event was hidden by weakening an
assertion.

Draft Forgejo PR [#173](http://192.168.4.7:3000/noirr/plurx/pulls/173) passed
the Effort development gate at reviewed head `44abf337`. The one adversarial
review found no P0s, three P1 merge blockers, and one P2 status error. Correction
commit `c7b87df5` closes all four:

- renderer identity now covers the FFmpeg dependency closure, active font
  inputs for text burn, and a process-local boundary that prevents unsafe
  cross-node or post-restart cache reuse;
- recipe preparation ffprobes the held source descriptor and compares the
  canonical document with the stored scan before using its geometry, tracks,
  cadence, or color facts;
- subtitle plus Matroska-attachment extraction writes through a daemon-owned
  bounded pipe, kills at exactly 64 MiB, confirms child reap, and publishes no
  overflow artifact;
- this checkpoint records the reviewed and corrected heads rather than
  implying review was still pending.

The first corrected draft gate then refused stale process/task ownership
counts. Auditing that failure found one more lifecycle problem: destination
I/O errors could return while a separately spawned diagnostic reader remained
detached. Commit `d8806467` joins stdout and stderr in one cancellation scope,
stops and reaps the exact child before returning any create/write/flush error,
adds the pre-existing-destination regression, and records every changed owner
in the enforced inventory.

The correction fast lane passes pinned Rust 1.97.1 all-target compilation,
workspace all-target Clippy with warnings denied, 93 VOD-serving tests with the
explicit two-hour benchmark ignored, 18 subtitle-cache tests, and focused
manager, dependency/font, process-isolation, disk-bound, and production
oversized-attachment regressions. The default Homebrew FFmpeg 9.0.1 formula
lacks libass and zscale; full burn/HDR checks used the supported
`ffmpeg-full` 9.0.1 binary instead of weakening those tests. The PR remains
draft, and its one post-correction full-suite run remains unspent. The prior
7.561–12.273-second warm two-hour measurements remain a performance nonclaim:
correctness inside the 30-second materialization budget is not transparent
seek latency.

## Historical recovery checkpoint — 2026-09-05

The following records the original capture-era worktree and evidence. It is
retained to explain provenance, not to describe the current checkout or grant
approval to the recovered tree. No old path, commit, test binary, or review is
current task-PR evidence.

- Own clone: `/private/tmp/plurx-streaming-vod-recipes.nE72nh/repo`.
- Branch: `codex/vod-recipe-production`.
- HEAD: `79f7f9eca0efbf085cc39be9369efd9b9b8cf6c0`, including effort
  `6efec8d9b1b4a27e36a6e0a3c5af8aa773d73909` (capture PR20), plus the
  prior main/PR5 integration. The merge and patch restoration were clean.
- Recovery backup: Git stash `3e2c66a8ba28b13413ec9d333b0818c57f4ffe97`
  contains the complete patch, including untracked source, before this last
  integration. It remains retained; it is not the authoritative current tree.
- Preserve every modified and untracked file reported by `git status`.
  In particular, the untracked `transcode/vod.rs`, `vodencode.rs`,
  `vodencode_tests.rs`, `vodencode_manager_tests.rs`, and this document are
  required source, not disposable build output.
- Source was frozen for review under the 18-file content-manifest SHA256
  `00988814a677192357753f5d128f6b7558525d45e116e8da8c6543232a611a2d`.
  Adding this checkpoint and serial benchmark numbers changes documentation
  only; the tested Rust and inventory bytes remain unchanged.
- No generated media is required to resume. Tests create their fixtures in
  temporary directories. Preserve `/private/tmp/plurx-astra-vod-recipes-*`
  logs as evidence; diagnostic spike directories are optional.

The pinned compiler is `rustc 1.97.1 (8bab26f4f 2026-07-14)`. The warm build
target is `/private/tmp/plurx-streaming-server.5u1DYC/repo/target`; do not edit
that other checkout. Cargo strips the external DYLD path when launching test
children, so build with `cargo test --no-run` and execute the freshly built
binary directly with the media environment below.

Prior complete evidence against the original frozen source, before the two
final corrections and capture integration:

| Check | Result | Log under `/private/tmp/` |
|---|---|---|
| Pinned daemon all-target check after integration | Pass | `plurx-astra-vod-recipes-12be-check.log` |
| Pinned daemon all-target Clippy, warnings denied | Pass | `plurx-astra-vod-recipes-final-clippy.log` |
| Fresh daemon test compilation | Pass | `plurx-astra-vod-recipes-final-build.log` |
| VOD serving / physical media | 78 pass, 1 explicit benchmark ignored | `plurx-astra-vod-recipes-final-vodserve.log` |
| Fragment generation | 13 pass | `plurx-astra-vod-recipes-final-vodgen.log` |
| Admission / FFmpeg / subtitles / producer ownership | 17 / 16 / 18 / 11 pass | `plurx-astra-vod-recipes-final-{admission,ffmpeg,subtitles,prodrun}.log` |
| Manager/transcode | 212 pass | `plurx-astra-vod-recipes-final-transcode.log` |
| Public typed refusal mapping | 1 pass | `plurx-astra-vod-recipes-final-api-errors.log` |
| Core transcode | 137 pass | `plurx-astra-vod-recipes-final-core-run.log` |
| Validation policy and reviewed lifetime inventory | 121 pass | `plurx-astra-vod-recipes-validation-final.log` |
| Explicit serial two-hour audio restart | 1 pass; 7.629 s preparation/reap, 12.189 s child CPU | `plurx-astra-vod-recipes-final-two-hour.log` |
| FFmpeg 8.1.2 supported non-burn, non-HDR subset | 8 pass, benchmark ignored | `plurx-astra-vod-recipes-final-ffmpeg8-supported.log` |
| Pinned workspace all-target Clippy, warnings denied | Pass | `plurx-astra-vod-recipes-final-workspace-clippy.log` |

Pinned `cargo fmt --all --check`, explicit rustfmt of both included test
files, and `git diff --check` also pass. The FFmpeg module retains one
pre-existing ignored test. The standalone core build reports pre-existing
dead-code warnings without daemon feature unification; daemon all-target
Clippy with `-D warnings` is green.

Repeatable compatibility command: run the FFmpeg 8.1.2 supported cases. That
installed build lacks libass and zscale, so this deliberately excludes burn
and the correctly converted HDR fixture. An initial run excluding only burn
failed at HDR fixture generation with `No such filter: zscale`; see
`/private/tmp/plurx-astra-vod-recipes-final-ffmpeg8-compat.log`. Do not report
that as passing HDR compatibility. Full burn and HDR proofs above ran with
the existing full FFmpeg 9.0.1 build.

```bash
cd /private/tmp/plurx-streaming-vod-recipes.nE72nh/repo
env PLURX_FFMPEG=/opt/homebrew/bin/ffmpeg \
  PLURX_FFPROBE=/opt/homebrew/bin/ffprobe \
  DYLD_LIBRARY_PATH=/opt/homebrew/Cellar/x265/4.2/lib \
  /private/tmp/plurx-streaming-server.5u1DYC/repo/target/debug/deps/plurxd-77893b3017870dbe \
  encoded_vod --skip burn --skip hdr10 --nocapture
```

After any source edit, rebuild with the pinned toolchain and re-run affected
focused tests before reviewing the resulting tree. For the full-featured
media environment, set `PLURX_FFMPEG` and `PLURX_FFPROBE` to the binaries in
`/opt/homebrew/opt/ffmpeg-full/bin/`. Do not substitute an older test binary
merely because its filename matches: `final-build.log` proves the current
binary was rebuilt after integration and the final fixture correction.

Outstanding work and nonclaims:

- Both final review blockers are implemented and structurally accepted by
  the reviewer. BoundedDiagnosticChild drains only an 8 KiB tail, discards
  stdout, and retains exact-child reap ownership across cancellation.
  The driver releases its manifest lock before capacity I/O, first reaps a
  superseded sole-permit child, then re-reads latest demand before spawning.
- Post-correction proof on the previous base: extractor 2/2; held-capacity
  cached-GET/reap/latest-seek regression 1/1; VOD 79/79, FFmpeg 18/18,
  subtitles 18/18; lifetime inventory 7/7. Logs are
  `/private/tmp/plurx-astra-vod-recipes-extractor-correction.log`,
  `plurx-astra-vod-recipes-driver-admission-correction.log`, and
  `plurx-astra-vod-recipes-correction-{vodserve,ffmpeg,subtitles}.log`.
  These prove the fixes before integration, not the final combined tree.
- Exact integrated proof on `79f7f9e` is complete: workspace all-target check
  (48.68 s), workspace all-target Clippy with warnings denied (39.67 s), fresh
  daemon test build (18.32 s), fresh core test build (7.36 s), VOD 79, generation
  13, admission 17, FFmpeg 18, subtitles 18, producer ownership 11, daemon
  transcode 212, public refusal mapping 1, core transcode 137, and validation
  policy 121 all pass. Formatting and diff checks pass. Logs use prefix
  `/private/tmp/plurx-astra-vod-recipes-6efe-` with suffixes `check`, `clippy`,
  `build`, `core-build`, `vodserve`, `vodgen`, `admission`, `ffmpeg`, `subtitles`,
  `prodrun`, `transcode`, `every_vod_ineligibility_reaches_the_wire_as_a_typed_refusal`,
  `core`, and `validation`, each ending in `.log`.
- The explicit two-hour benchmark also passes on this integrated source:
  7.561 s preparation/first two GETs/reap and 11.882 s child CPU; log
  `/private/tmp/plurx-astra-vod-recipes-6efe-two-hour.log`. This confirms the
  same performance limitation, not transparent-latency acceptance.
- Current next step: obtain final exact-source adversarial approval,
  coordinate the final base with root after its docs-only PR21, then perform
  the proper effort fast-lane task commit and explicit post-commit history
  mapping. Root handles PR publication and its development gate. No full
  suite runs in this subtask.
- Independent review by `astra_server_adversary` remains incomplete. The
  previously reported strong source/sidecar identity, live capacity policy,
  bounded Store read, frozen executable, and public refusal cases have fixes
  and green regression evidence, but are not yet a final approval.
- Two-hour warm seeking is correctness evidence, not seamless preparation
  latency acceptance. Canonical seekable audio, cold NAS, expensive codecs,
  and physical hardware/client qualification remain follow-on work.
- Executable fingerprinting does not attest dynamic codec/libass libraries
  or font resolution. The operational limitation above is intentional and
  must not be represented as complete engine attestation.
- Protocol handoff, Original mapping, and Dev-tab enablement belong to other
  effort milestones. There are no hidden/compile feature gates in this one.
