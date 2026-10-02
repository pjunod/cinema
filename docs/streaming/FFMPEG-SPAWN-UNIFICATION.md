# FFmpeg spawn unification — one producer spawn path, one progress classifier

**Status:** M1/M2 merged (PR #415) · M3 PASS in the owned lab on lab3,
2026-10-02, for VOD encode, VOD burn, VOD copy and progressive remux
(§5.3.1); rolling HLS not measurable from library files on this build; the
media1 reading is still owed ·
**Executes:** F-stream-13, the progress-line half of F-stream-12, and the
"unify the spawn path (fixes the drift)" step of
§4.1 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 against `main` @ `88a3957a`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Companion to [VOD-ENCODING.md](VOD-ENCODING.md) (the attested-descriptor
rules the VOD spawn must keep),
[STREAMING-RELIABILITY-IMPLEMENTATION.md](STREAMING-RELIABILITY-IMPLEMENTATION.md)
(§7.1 "the session/producer remains the owner of that handle" — the rolling
path's diagnostics ownership), and
[FONT-ATTESTATION-AND-BLOCKING-IO.md](FONT-ATTESTATION-AND-BLOCKING-IO.md)
(which needs one place to set `FONTCONFIG_FILE`). Read §2 before writing
the builder: the three spawn sites differ on purpose in several places and
by accident in three. The plan is behaviour-preserving — every difference
in §2.3 that is deliberate is carried as a builder option; every accidental
one is listed and closed with its own test. If a step seems to require
changing what a stderr reader does with a line, who reaps a child, or
which descriptor number a source is attached to, stop and flag it.

**Correction to the review:** the review counts "three ffmpeg spawn
implementations" and "two `is_progress_line` copies". Both hold for the
three *producer* sites named in the brief. For the record there are two
more `spawn_job_owned` producers outside this plan's scope
(`fragindex.rs:1707`, which already uses `configure_ffmpeg_runtime`, and
`live_tv.rs:5819/5920`, which uses `env_clear` plus an allow-list — a
different, deliberate environment policy) and two descriptor-inheritance
`pre_exec` implementations (`transcode.rs:2165-2215` inline;
`ffmpeg.rs:84-118` `inherit_file_descriptors`). §4 says what happens to
each. The review's statement that the loose classifier "swallows exactly
the bsf error the strict one's comment warns about" is narrower than it
reads: an ffmpeg error line normally carries a `[component @ 0x…]` prefix,
whose key part contains spaces and uppercase and is therefore **not**
classified as progress by the loose rule. Only a bare
`filter_units=remove_types=…` line with no prefix is swallowed. Real, but
narrow; the assessment says the same ("a bare filter_units line is not
every FFmpeg diagnostic").

## 1. Objective

1. One function builds and spawns a producer `ffmpeg` for the rolling HLS
   path, the encoded/copy VOD path and the progressive remux, so that the
   runtime environment (`configure_ffmpeg_runtime`), job ownership
   (`spawn_job_owned`), piped stdio, `kill_on_drop`, and descriptor
   attachment are decided in one place — and the VOD path stops being the
   one producer that gets none of the first two.
2. One `is_progress_line`, with the strict key set, used by every reader
   that has to separate `-progress` blocks from diagnostics on a shared
   stderr; the loose one is deleted after its callers' accepted inputs are
   carried into the strict one's tests.
3. No change in what any reader logs, reports as a fault, feeds to
   progress, or when; no change in who reaps which child.

## 2. Contract today

Re-verify every line number at build time; they are from `88a3957a`.

### 2.1 The shared pieces

- [`configure_ffmpeg_runtime(command, runtime_cache)`](../../crates/plurxd/src/transcode.rs)
  (`transcode.rs:2373-2383`): `XDG_CACHE_HOME=<runtime_cache>`,
  `AV_LOG_FORCE_NOCOLOR=1`. Callers: `transcode.rs:2164, 2300`,
  `fragindex.rs:1665`, and a test at `:30016`.
- [`spawn_job_owned`](../../crates/plurxd/src/process_control.rs)
  (`process_control.rs:21-33`): Windows suspended start + Job Object;
  no-op setup on Unix. Returns `(Child, ChildJob)`; the `ChildJob` must
  live as long as the child.
- [`inherit_file_descriptors(command, &[(&File, target)])`](../../crates/plurxd/src/ffmpeg.rs)
  (`ffmpeg.rs:84-118`): Unix `pre_exec` that `F_DUPFD_CLOEXEC`s each source
  above 10 then `dup2`s to the target (3..=9). Used by VOD
  (`vodserve.rs:6935-6954` `attach_recipe_descriptors`: source→3, audio→4,
  subtitle→5) and by the subtitle extractors.
- `is_progress_line` (strict): `transcode.rs:2385-2407`, key ∈
  {`frame`, `fps`, `bitrate`, `total_size`, `out_time_us`, `out_time_ms`,
  `out_time`, `dup_frames`, `drop_frames`, `speed`, `progress`} or
  `stream_*_q`. Its comment names the failure the loose rule has: "an error
  message about `filter_units=remove_types=32-34` contains plenty of those,
  and swallowing it would hide exactly the failure a copy session is most
  likely to have."
- `is_progress_line` (loose): `stream.rs:3409-3420`, key after `trim()` is
  non-empty and all `[a-z0-9_]`. Tests at `stream.rs:4383-4394` assert a
  progress fixture is classified and a prose fixture is not.
- `apply_progress_line(progress, generation, line)` (`transcode.rs:1699-1730`):
  generation-fenced; parses `out_time_us|out_time_ms` and `speed`; treats
  `N/A` as nothing. Used by both classifiers' consumers.

### 2.2 The three producer spawn sites

**A. Rolling HLS — `transcode.rs:2148-2278` `spawn_ffmpeg`** and
**`:2289-2360` `spawn_ffmpeg_pipe`** (copy, media on stdout).

```text
argv     = ["-progress", "pipe:1" | "pipe:2"] ++ args
env      = configure_ffmpeg_runtime
fds      = inline pre_exec: dup source→3, output→4, subtitle→5, clear
           FD_CLOEXEC, macOS fchdir(4)          (spawn_ffmpeg only;
           spawn_ffmpeg_pipe: Windows verify, Unix drops descriptors)
stdio    = stdin null, stdout piped, stderr piped, kill_on_drop(true)
spawn    = spawn_job_owned → ObservedFfmpeg { child, child_job, diagnostics }
stdout   = spawn_ffmpeg: owned task, `progress_observer.apply_line` per line
           spawn_ffmpeg_pipe: returned to the caller as the media pipe
stderr   = owned task, decoder_health::read_diagnostics[_reporting] with the
           attempt's grammar; log_ffmpeg_stderr per line; spawn_ffmpeg_pipe
           first filters `is_progress_line` (strict) → apply_line
reap     = the attempt supervisor (ObservedFfmpeg's owner), biased
           reap-before-signal select (out of scope here)
```

**B. Encoded / copy VOD — `vodserve.rs:6733-6861` `spawn_generation`.**

```text
argv     = recipe_pipe_args(recipe, start_seconds, attested)   (no -progress)
env      = NONE — inherits the daemon's environment as is
fds      = attach_recipe_descriptors → inherit_file_descriptors (3, 4, 5)
stdio    = stdin null, stdout piped, stderr piped, kill_on_drop(true)
spawn    = Command::spawn() — NOT spawn_job_owned; no ChildJob
stdout   = run_generation (fMP4 fragments → materialize)
stderr   = ffmpeg::drain_diagnostics (last 8 KiB), logged once at exit as
           "VOD producer diagnostic"
reap     = ProducerSlot (prodrun.rs) via attach_owned; Terminate = kill+wait,
           then permit drop
```

Consequences of the two NONEs: on a Docker deployment with `HOME` unset,
fontconfig has no cache and rebuilds it at every encoded burn launch
(F-stream-13; the `configure_ffmpeg_runtime` doc comment describes the
symptom); ffmpeg may colour its log output when it decides stderr is a
terminal (never on a pipe, but the grammar comment at
`transcode.rs:2378-2382` says why the variable is set anyway); and on
Windows a VOD producer's descendants are not in a Job Object, so a killed
producer can leave a grandchild holding a cache file (the `output_job_owned`
doc comment at `process_control.rs:35-38` names that hazard).

**C. Progressive remux — `stream.rs:3118-3345` `remux`.**

```text
argv     = ["-hide_banner","-loglevel","error"] ++ (tracked ? ["-progress","pipe:2"])
           ++ seek/itsoffset ++ ["-i", path] ++ codec args ++ fMP4 flags ++ "pipe:1"
env      = NONE
fds      = none (input by path; Windows verify_windows_source_path)
stdio    = stdin null, stdout piped, stderr piped, kill_on_drop(true)
spawn    = spawn_job_owned → (child, child_job)
stdout   = the HTTP body (BufReader), through the cancellation-aware stream
stderr   = detached tokio::spawn: if tracked && is_progress_line (loose)
           → apply_progress_line; else tracing::warn!("remux ffmpeg: {line}")
reap     = spawn_remux_process_owner (stream.rs:3026-3060): owns child_job,
           kills on body drop / serving loss, always `wait`s
```

### 2.3 The differences, sorted

Deliberate — each becomes a builder option or stays with the caller:

| Difference | A rolling | B VOD | C remux | Carried as |
|---|---|---|---|---|
| `-progress` destination | `pipe:1` (HLS) / `pipe:2` (copy) | none | `pipe:2` iff tracked | `Progress::{Stdout, Stderr, None}` |
| Descriptor attachment | inline pre_exec 3/4/5 + `fchdir(4)` on macOS | `inherit_file_descriptors` 3/4/5 | none | `Descriptors { source, output, subtitle }` → one pre_exec (§3.1) |
| stderr consumer | `read_diagnostics[_reporting]` + grammar + fault sink | `drain_diagnostics` tail | line logger | **stays with the caller**: the builder returns `ChildStderr`, never a reader |
| stdout consumer | progress task / media pipe | media pipe | HTTP body | **stays with the caller**: builder returns `ChildStdout` |
| Reap owner | attempt supervisor | `ProducerSlot` | `spawn_remux_process_owner` | **stays with the caller**: builder returns `(Child, ChildJob)` |
| `-hide_banner -loglevel error` | in `args` | in recipe args | added by `remux` | stays in each caller's argv (not the builder's business) |

Accidental — closed by the builder, each with a test in §5.1:

| Drift | Where | Closed by |
|---|---|---|
| No `configure_ffmpeg_runtime` | B, C | builder always calls it |
| No `spawn_job_owned` | B | builder always uses it |
| Two descriptor `pre_exec`s | A vs B | one implementation (§3.1) |
| Two `is_progress_line`s | A vs C | one, strict (§3.2) |
| macOS `fchdir(4)` only in A | A | kept, as an option only A sets (`Descriptors.output_is_cwd`) — it is deliberate for A and irrelevant to B/C; listed here because a naive merge would drop it |

## 3. Change

### 3.1 `producer_spawn::spawn(program, args, options) -> Spawned`

New module `crates/plurxd/src/producer_spawn.rs` (the §4.1 decomposition
names `producer/spawn.rs`; this file moves there when that split happens):

```rust
pub(crate) struct SpawnOptions<'a> {
    pub runtime_cache: &'a Path,
    pub progress: Progress,                 // Stdout | Stderr | None
    pub descriptors: Descriptors<'a>,       // { source, output, subtitle: Option<&File>,
                                            //   output_is_cwd: bool }
    pub env: &'a [(&'a str, &'a OsStr)],    // extra child-only variables, e.g. FONTCONFIG_FILE
}
pub(crate) struct Spawned {
    pub child: tokio::process::Child,
    pub child_job: crate::process_control::ChildJob,
    pub stdout: tokio::process::ChildStdout,
    pub stderr: tokio::process::ChildStderr,
}
pub(crate) fn spawn(program: &Path, args: &[String], options: SpawnOptions<'_>)
    -> Result<Spawned, String>;
```

Body, in order: `Command::new(program)`; `-progress pipe:N` prepended per
`options.progress` (a global option, so it may lead — `transcode.rs:2156`);
`args`; `configure_ffmpeg_runtime`; `options.env`; descriptors via **one**
`pre_exec` that is today's `inherit_file_descriptors` extended with the
`FD_CLOEXEC` clear (harmless: `dup2` already clears it) and the
`output_is_cwd` `fchdir` — `inherit_file_descriptors` itself becomes a thin
call into it so the subtitle extractors share the code; Windows
`descriptors.verify()`; `stdin null, stdout piped, stderr piped,
kill_on_drop(true)`; `spawn_job_owned`; take both pipes (`None` is an
`Err`, as `spawn_ffmpeg_pipe` treats a missing stdout today). Nothing is
spawned onto the runtime by this function; the caller owns every task.

Callers after the change:

- `spawn_ffmpeg` / `spawn_ffmpeg_pipe`: build `SpawnOptions`, call
  `spawn`, then attach exactly the readers they attach today to the
  returned pipes and wrap into `ObservedFfmpeg`. `FfmpegDescriptors` maps
  onto `Descriptors` field for field.
- `spawn_generation`: `attach_recipe_descriptors` becomes a `Descriptors`
  value; the bare `.spawn()` becomes `spawn(...)`; the returned
  `child_job` is stored beside the child in the slot (`attach_owned` gains
  the job as part of the owned resources, dropped when the child is
  reaped — the same lifetime `ObservedFfmpeg` gives it). `run_generation`
  and `drain_diagnostics` are unchanged. This is where B gains
  `configure_ffmpeg_runtime` and the Job Object.
- `remux`: `Progress::Stderr` iff tracked; the detached stderr task and
  `spawn_remux_process_owner` are unchanged except that the task now calls
  the strict classifier (§3.2).

### 3.2 One `is_progress_line`

Move the strict function to `plurx_core::transcode::progress::is_progress_line`
(beside `apply_progress_line`'s key knowledge, which lives in `plurxd`
today; moving the classifier to `plurx-core` lets the
[Live TV playlist/progress work](../features/LIVE-TV-DVR-IMPLEMENTATION.md)
reuse it later without a `plurxd` dependency). Delete `stream.rs:3409-3420`.
Before deleting, carry its two fixtures (`stream.rs:4383-4394`) into the
strict function's tests and add the divergence cases:

| Line | loose | strict | after |
|---|---|---|---|
| `out_time_us=1234` | progress | progress | progress |
| `speed=1.02x` | progress | progress | progress |
| `stream_0_0_q=-1.0` | progress | progress | progress |
| `progress=continue` | progress | progress | progress |
| `filter_units=remove_types=32-34` (bare) | **progress** | diagnostic | diagnostic |
| `[AVBSFContext @ 0x55] Option remove_types=32-34 …` | diagnostic | diagnostic | diagnostic |
| `some_future_key=1` | progress | diagnostic | diagnostic — logged once as `remux ffmpeg: …`, which is the documented cost of the strict rule ("a misfiled line costs a log entry") |

The last row is the one behaviour change a remux viewer could notice: an
ffmpeg build that adds a new `-progress` key produces one warn line per
block until the key is added to the set. Acceptable and stated; the
alternative is keeping the rule that hides a bsf failure.

Note on F-stream-12's other half: `hls.rs:10141` `video_frame_rate` is
`#[cfg(test)]` and the "cover-art FRAME-RATE" bug is withdrawn (§0); the
frame-rate parsers are not touched by this plan.

## 4. Guardrails (non-goals)

- **Behaviour-preserving.** Every reader, logger, fault sink, grammar and
  reap owner stays where it is. The builder returns pipes; it never spawns
  a task. A test asserts the builder spawns nothing onto the runtime
  (`tokio::runtime::Handle::current().metrics().num_alive_tasks()` before
  and after, under `tokio_unstable`; otherwise by construction and review).
- **Descriptor numbers do not change:** source 3, output 4, subtitle 5 on
  every path, because the recipe args and rolling args say `/dev/fd/3` etc.
  VOD-ENCODING.md: the source and subtitle descriptors "are duplicated
  before either is assigned its reserved child descriptor number" — the
  unified `pre_exec` keeps the dup-all-then-dup2-all order.
- **The Live TV spawn is not unified here.** It uses `env_clear`
  (`live_tv.rs:5955`) with an allow-list (`:6068-6085`), stdin piped, and
  its own
  `capture_live_stderr`; that environment policy is deliberate (§6 of the
  review lists it as good). It may adopt the builder later with an
  `env: Clear(allow_list)` option; not in this plan.
- **`fragindex.rs:1707` and the boot/capability probes stay on their
  primitives.** They are not producers; the probe paths are
  [PROCESS-OUTPUT-CAPTURE-AND-SCAN-PROBE-BOUNDS.md](PROCESS-OUTPUT-CAPTURE-AND-SCAN-PROBE-BOUNDS.md)'s.
- **No `-hide_banner`/`-loglevel` policy in the builder.** The rolling
  attempt's log flags are part of its diagnostic contract (`grammar` is
  `Some` only "when this attempt asked for the log flags that contract was
  qualified under", `transcode.rs:1916-1920`); the builder must not add or
  remove them.
- **No environment beyond `configure_ffmpeg_runtime` plus the caller's
  `env` list.** The builder does not `env_clear`; that would change the GPU
  driver selection variables every producer inherits today.
- **Not the registry unification or the god-module split.** §4.1 says
  spawn unification comes after the `git mv` decomposition; this plan can
  land before it because it adds one module and touches three functions,
  and the later `git mv` moves the module.

## 5. Milestones

### 5.1 M1 — the builder, adopted by all three sites

One logical implementation commit in the plan PR owns the builder and all
three adoptions; splitting caller adoption across PRs would recreate the drift
this milestone removes. Tests in `producer_spawn.rs`, using the
re-exec-the-test-binary child from
[PROCESS-OUTPUT-CAPTURE-AND-SCAN-PROBE-BOUNDS.md](PROCESS-OUTPUT-CAPTURE-AND-SCAN-PROBE-BOUNDS.md)
§5.1 (portable; no `/bin/sh`):

| Test | Asserts |
|---|---|
| `every_spawn_carries_the_runtime_environment` | child prints `XDG_CACHE_HOME` and `AV_LOG_FORCE_NOCOLOR`; both equal what `configure_ffmpeg_runtime` sets, for `Progress::{Stdout, Stderr, None}` |
| `caller_env_is_added_not_substituted` | `env = [("FONTCONFIG_FILE", "/x")]`: child sees it **and** `XDG_CACHE_HOME` |
| `progress_flag_leads_argv_per_option` | child prints its argv: `["-progress","pipe:1", …]`, `["-progress","pipe:2", …]`, or no `-progress` |
| `descriptors_land_on_three_four_five` (Unix) | three temp files with distinct contents; child reads `/dev/fd/3`, `/dev/fd/4`, `/dev/fd/5` and prints them; matches; and `/dev/fd/6` does not exist |
| `descriptor_dup_survives_an_unlucky_low_fd` (Unix) | open a source whose raw fd is 4 before spawning with `output` set: both land correctly (the dup-all-then-dup2 order) |
| `both_pipes_are_returned_and_owned_by_the_caller` | `Spawned.stdout`/`.stderr` readable; dropping `Spawned` with `child_job` kills the child (pid-file + `signal(pid, Terminate) == Ok(false)`) |
| `the_builder_spawns_no_tasks` | task count unchanged across `spawn` |

Per-site preservation tests (existing suites, must pass unchanged):
`cargo test -p plurxd spawn_ffmpeg progress_observer read_diagnostics`
(A); `cargo test -p plurxd vodserve` and `make vodencode-restart-check` (B —
the restart check is the proof that the encoded pipe still reads its
descriptors and produces identical bytes); `cargo test -p plurxd
http::stream::tests` (C). Plus one new B test:
`a_vod_generation_carries_a_child_job_until_reap` — the slot's owned
resources include the `ChildJob` and it is dropped after `kill().await`,
not before.

Acceptance: all of the above green; the ready-PR fast lane supplies the broad
`make unit` result; `grep -rn
"Command::new(recipe_program\|Command::new(ffmpeg_bin())" crates/plurxd/src/
{transcode,vodserve}.rs crates/plurxd/src/http/stream.rs` shows no
producer site outside `producer_spawn.rs` (probes remain and are listed in
the PR body).

### 5.2 M2 — one `is_progress_line`

Separate logical commit after M1 in the same plan PR. Code: §3.2. Tests: the
table in §3.2 as one parameterised test in `plurx-core`, plus the two fixtures moved from
`stream.rs`. A remux-side test: a tracked progressive stream fed a stderr
transcript containing one bare `filter_units=…` line logs it as `remux
ffmpeg: filter_units=…` and does not advance progress.

Acceptance: `cargo test -p plurx-core progress::tests`; `cargo test -p
plurxd http::stream::tests::progress_blocks_are_distinguishable_from_ffmpegs_prose`;
`grep -rn "fn is_progress_line" crates/` returns exactly one definition. The
ready-PR fast lane supplies the broad `make unit` result.

### 5.3 M3 — fleet check

GPT prompt after the M1 deploy (corrected 2026-10-02: the first text used
`pgrep`, which the image does not have, a placeholder title, and "the
journal", which a Docker node does not write to; the loop selects children by
`readlink $p/exe`, since a `cmdline` match also finds the loop's own
`sh -c`):

> On media1 (a Docker deployment): start any title that has a *text*
> subtitle track as an encoded VOD session with that track burned in
> (choose a quality that transcodes). While it plays, run on the host:
> `docker exec plurxd sh -c 'for p in /proc/[0-9]*; do readlink $p/exe
> 2>/dev/null | grep -q ffmpeg && { echo "== ${p#/proc/}"; tr "\0" "\n"
> <$p/environ | grep -E "XDG_CACHE_HOME|AV_LOG_FORCE_NOCOLOR|FONTCONFIG_";
> tr "\0" " " <$p/cmdline | grep -o "/dev/fd/[345]"; ls -l $p/fd/3
> $p/fd/4 2>/dev/null; }; done'` and paste it (both variables must be
> present for the VOD producer; before this change they were absent). Send
> time-to-first-segment from the session's playback-info for this start and
> for a second start of the same title (the second should not pay a
> fontconfig cache rebuild). Then start a progressive remux from the web
> client (a title that direct-plays the video but re-encodes audio), paste
> the same loop's output for its ffmpeg, and send `docker logs --since 2h
> plurxd 2>&1 | grep -E 'remux ffmpeg:|progress key'`.

Acceptance: both variables present on both producers; no regression in
first-segment time; the M2 deploy shows no new `remux ffmpeg:` lines in
the daemon log for an ordinary tracked remux (a new-key warn would appear
here, per §3.2's last row).

#### 5.3.1 Owned-lab evidence, 2026-10-02 — M3 PASS (rolling not measurable)

**Where.** A throwaway container on **lab3**, from an image built from the
effort branch with the P-02 reproducibility continuation (`9ed98a30c`);
sessions created over the API; children read from the host through `/proc`
of every pid in the container's cgroup, once a second. Every library session
took the encoded-VOD route: on this build a library file cannot start rolling
HLS (any `presentation` other than `vod` answers 410
`live_presentation_removed`), so **no rolling producer was measured**.
**Caveat:** an owned lab on lab3, not media1.

| Child | `XDG_CACHE_HOME` | `AV_LOG_FORCE_NOCOLOR` | argv input | fd 3 / fd 4 |
|---|---|---|---|---|
| VOD producer, encode (12 in the first phase, 3 in the second) | the runtime cache ✓ | `1` ✓ | `-i /dev/fd/3 … -i /dev/fd/4` | both the source file ✓ |
| VOD producer, ASS text burn | ✓ | ✓ (plus `FONTCONFIG_FILE`, `FONTCONFIG_SYSROOT`) | `/dev/fd/3`, `/dev/fd/4` | the source ✓ |
| VOD producer, copy session (`copy:true, aac:true`) | ✓ | ✓ | `-i /dev/fd/3 … -c:v copy -c:a aac … pipe:1` | the source ✓ |
| progressive remux `stream.mp4` (twice) | ✓ | ✓ | `-progress pipe:2 … -readrate 4.00 -i <path>` | fd 3 a pipe, fd 4 the source (the path in argv is by design: §2.3, "descriptor attachment: C remux none") |
| held-source ffprobe at session start | environment cleared — a probe, not a producer | — | `/dev/fd/3` | the source |

**Logs.** Two `remux ffmpeg:` lines in total, both the source's own decoder
complaint (`[h264 …] number of reference frames (0+5) exceeds max (4;
probably corrupt input)`), which is a real diagnostic and not a progress
key. No progress-key line and no progress-key warning anywhere.

**First segment, first versus second start.** SDR film: 717 ms on the
first-ever start (segment 0), 117–125 ms restarted over materialised
segments, 806–1044 ms at fresh positions. Text-burn title: 1458 ms on the
cold-font first start versus 790 ms on the second start (a new font
environment and a fresh encode) — no Fontconfig rebuild penalty on the
second start. Progressive remux time to first byte: 15.8 ms and 16.0 ms
(first and second).

**Verdict: M3 PASS** for VOD encode, VOD burn, VOD copy and progressive
remux; rolling HLS not measurable from files on this build.

## 6. Verification and rollout

- Focused: named per milestone above; `make vodencode-restart-check` is
  mandatory for M1 because B is the path it exercises.
- Lane: the one ready plan PR runs `make unit`; `cargo check -p plurxd --tests --target
  x86_64-pc-windows-msvc` for M1 in the PR body (the builder has Windows
  branches — `descriptors.verify()` and the Job Object — so the server test
  target needs compilation). The current Windows composite action builds all
  workspace targets; the recovered exact-source receipt below includes the
  server test target. The 2026-09-21 author-host cross-check reached
  third-party C builds but could not reach project Rust: this macOS host has
  the Rust target installed but no MSVC C headers/toolchain (`stdlib.h` was
  missing in `libsqlite3-sys`/`onig_sys`, and `assert.h` in `ring`). The exact
  command and environment limitation are recorded on the draft PR rather than
  presented as Windows compile evidence.
- Metrics: none added. The observable is environmental (`/proc/<pid>/environ`)
  and the existing first-segment telemetry.
- Rollout: M1 and M2 as logical commits in one draft plan PR. No setting, no
  gate, no schema, no recipe identity change — `recipe_pipe_args` is
  untouched and the encoded identity hashes argv, not environment
  (`vodencode.rs:172-197`). Rollback: revert; nothing persists.
- Ordering with the other plans: M1 before
  [FONT-ATTESTATION-AND-BLOCKING-IO.md](FONT-ATTESTATION-AND-BLOCKING-IO.md)
  M2 (which uses `SpawnOptions.env`); independent of
  [ENCODED-VOD-HOLD-AND-RELEASE.md](ENCODED-VOD-HOLD-AND-RELEASE.md) (which
  changes when `spawn_generation` is called, not what it does).

**Windows test-target receipt, 2026-09-30 ([#643](http://192.168.4.7:3000/noirr/plurx/pulls/643)).**
The existing successful [Effort gate UI 3635](http://192.168.4.7:3000/noirr/plurx/actions/runs/3635)
(API run 3656), Windows job 38936, checked out exact source
`8a305517ccf702ed4c46496a10d244df112311cf`. Its
[primary log](http://192.168.4.7:3000/api/v1/repos/noirr/plurx/actions/jobs/38936/logs)
records `plurxd` (bin `plurxd` test) with 46 warnings (7 duplicates), then
`Finished dev profile [unoptimized + debuginfo] target(s) in 4m 28s` at
2026-09-30 15:53:32 UTC. The ordinary server binary reported 12 warnings.
This is successful Windows test-target compilation, with warnings disclosed;
it is not strict-warning compliance or execution on a native Windows host.

The [Windows composite action](../../.github/actions/windows-cross/action.yml)
at that exact source runs:

```bash
cargo xwin build --workspace --all-targets --locked \
  --exclude plurx-cluster-check --target x86_64-pc-windows-msvc
```

Source and integrated landing `61bd96c4b831d4aa3ed4377c758ba3adb0aa7315`
have equal tree `aa3cdb768beb254a34d76abd6b359c152bac026e`. This receipt
supersedes the missing Windows compilation evidence, while retaining the
September 21 author-host C-header failure above. No Windows build, test or CI
rerun was needed to recover it. M3's encoded-burn/remux environment,
first/second-start TTFF and progress-diagnostic fleet checks remain open;
S-05 is not done.

## 7. Decisions

1. **The caller passes `program: &Path`.** An encoded VOD recipe names an
   attested executable, so re-resolving `PLURX_FFMPEG` inside the builder could
   launch bytes other than the recipe identifies.
2. **The VOD slot carries `ChildJob` in a typed field.** The field remains
   alive through `kill().await` and is released before a successor can attach;
   hiding it in the permit's type-erased box would make that order accidental.
3. **The strict key set stays closed.** A read-only 2026-09-21 probe of the
   deployed media1 FFmpeg emitted exactly `bitrate`, `drop_frames`,
   `dup_frames`, `fps`, `frame`, `out_time`, `out_time_ms`, `out_time_us`,
   `progress`, `speed`, `stream_0_0_q`, and `total_size`; all are in the set.
   An unknown future key is logged as a diagnostic until deliberately added.
4. **The descriptor primitive stays general while producers use 3/4/5.**
   Subtitle helpers still need an arbitrary target list. The shared primitive
   keeps dup-all-before-dup2 ordering and the producer builder supplies only
   the three recipe-reserved targets.
5. **Head regeneration uses the builder too.** Current `main` had gained a
   fourth direct recipe-program launch after the plan snapshot. Leaving it
   direct would preserve the same runtime-environment and Windows-descendant
   drift inside the VOD lifecycle, so M1 includes it and retains its existing
   cancellation-independent reaper.
6. **There is no enablement setting.** This is invariant consolidation, not a
   behavior operators opt into; no feature gate or Developer toggle is added.

---

## Execution log

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-21 | gpt-5.6-sol | agent:/root/p01_builder | Claim | [#415](http://192.168.4.7:3000/noirr/plurx/pulls/415) | Claimed `plan/S-05` from `f0af512d`; pinned Rust 1.97.1 available locally. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/p01_builder | M1 | [#415](http://192.168.4.7:3000/noirr/plurx/pulls/415) · `e12f0c02` | Shared builder adopted by rolling HLS, VOD generation/head regeneration, and progressive remux. Builder 5, producer-slot 12, head-regeneration 2, and both real-FFmpeg restart tests passed. Host format/check/Clippy passed; Windows cross-check stopped in third-party C builds because the macOS host has no MSVC C headers/toolchain. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/p01_builder | M2 | [#415](http://192.168.4.7:3000/noirr/plurx/pulls/415) · `60b2faa9` | One strict classifier definition; core and remux focused tests passed. Media1's deployed key vocabulary matched the closed set exactly. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/p01_builder | M3 | [#415](http://192.168.4.7:3000/noirr/plurx/pulls/415) | needs: deploy the candidate, run the §5.3 encoded-burn and progressive-remux environment/TTFF checks, and inspect the journal for unexpected progress keys. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/c02_builder | Sole-review disposition | [#415](http://192.168.4.7:3000/noirr/plurx/pulls/415) · this commit | Added executable low-descriptor collision and fd 6 closure coverage, live-child direct-drop and VOD-slot termination/reap coverage through the shared spawn result, and an end-to-end remux stderr-consumer regression that observes the warning while progress remains unchanged. The portable ownership tests exercise the same builder and typed `ChildJob` path on Windows when run there; Windows execution is not claimed on this macOS host. M3 remains pending. |
| 2026-09-30 | gpt-6.1-sol | agent:/root/k08_upstream_receipt_sol61 | Recovered Windows test-target compilation | [#643](http://192.168.4.7:3000/noirr/plurx/pulls/643) | Evidence-only continuation from effort `61bd96c4b831d4aa3ed4377c758ba3adb0aa7315`; public draft claim `0384be61a` preceded receipt edits. Independently read existing successful API run 3656/UI 3635/job 38936 and its exact-source log: source `8a305517ccf702ed4c46496a10d244df112311cf`, equal source/landing tree `aa3cdb768beb254a34d76abd6b359c152bac026e`, all-target Windows build finished in 4m 28s with ordinary/test binary warnings disclosed above. Historical C-header failure and original implementation authorship remain. No Windows execution, fleet acceptance, source/test/workflow edit or new CI run; M3 remains open. Root coordinator manages this continuation's sole independent review, exact-current Effort gate and integration. |
| 2026-10-02 | claude-opus-5-5 | https://claude.ai/code/session_01CAyBrYCQ7PpAtuZwUxKfp7 | M3 owned-lab evidence | branch `opus/encoder-lab-evidence` (evidence-only docs) | Owned lab on lab3 (not media1). M3 PASS (§5.3.1) for VOD encode, burn and copy producers and the progressive remux: `XDG_CACHE_HOME` and `AV_LOG_FORCE_NOCOLOR` on every producer, the burn also with `FONTCONFIG_FILE`/`FONTCONFIG_SYSROOT`, sources on fd 3/4 as designed; two `remux ffmpeg:` lines, both the source decoder's own complaint, no progress-key lines; no Fontconfig rebuild penalty on a second burn start (1458 ms cold versus 790 ms). Rolling HLS is not startable for library files on this build (410 `live_presentation_removed`) and was not measured. §5.3 prompt corrected (no `pgrep`, no placeholder title, `docker logs`). |
