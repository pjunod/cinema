# FFmpeg spawn unification — one producer spawn path, one progress classifier

**Status:** ready for review · **Executes:** F-stream-13, the progress-line
half of F-stream-12, and the "unify the spawn path (fixes the drift)" step of
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

One PR, three commits (builder + A; B; C), each green on its own so a
bisect lands on one site. Tests in `producer_spawn.rs`, using the
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

Acceptance: all of the above green; `make unit` green; `grep -rn
"Command::new(recipe_program\|Command::new(ffmpeg_bin())" crates/plurxd/src/
{transcode,vodserve}.rs crates/plurxd/src/http/stream.rs` shows no
producer site outside `producer_spawn.rs` (probes remain and are listed in
the PR body).

### 5.2 M2 — one `is_progress_line`

Separate small PR after M1. Code: §3.2. Tests: the table in §3.2 as one
parameterised test in `plurx-core`, plus the two fixtures moved from
`stream.rs`. A remux-side test: a tracked progressive stream fed a stderr
transcript containing one bare `filter_units=…` line logs it as `remux
ffmpeg: filter_units=…` and does not advance progress.

Acceptance: `cargo test -p plurx-core progress::is_progress_line`; `cargo
test -p plurxd http::stream::tests::.*progress.*`; `grep -rn "fn
is_progress_line" crates/` returns exactly one definition; `make unit`.

### 5.3 M3 — fleet check

GPT prompt after the M1 deploy:

> On media1 (a Docker deployment): start *Harbor Lights* as an encoded VOD
> session with the text subtitle track burned in. Paste `cat
> /proc/$(pgrep -n -f 'ffmpeg.*dev/fd/3')/environ | tr '\0' '\n' | grep
> -E 'XDG_CACHE_HOME|AV_LOG_FORCE_NOCOLOR'` (both must be present for the
> VOD producer; before this change they were absent), and time-to-first-
> segment from the session's playback-info for this start and for a second
> start of the same title (the second should not pay a fontconfig cache
> rebuild). Then start a progressive remux from the web client (a title
> that direct-plays the video but re-encodes audio) and paste the same
> `environ` grep for its ffmpeg.

Acceptance: both variables present on both producers; no regression in
first-segment time; the M2 deploy shows no new `remux ffmpeg:` lines in
the journal for an ordinary tracked remux (a new-key warn would appear
here, per §3.2's last row).

## 6. Verification and rollout

- Focused: named per milestone above; `make vodencode-restart-check` is
  mandatory for M1 because B is the path it exercises.
- Lane: `make unit` on both PRs; `cargo check -p plurxd --tests --target
  x86_64-pc-windows-msvc` for M1 in the PR body (the builder has Windows
  branches — `descriptors.verify()` and the Job Object — and the Windows
  lane compiles release only).
- Metrics: none added. The observable is environmental (`/proc/<pid>/environ`)
  and the existing first-segment telemetry.
- Rollout: M1 as one draft PR with three commits; M2 after. No setting, no
  gate, no schema, no recipe identity change — `recipe_pipe_args` is
  untouched and the encoded identity hashes argv, not environment
  (`vodencode.rs:172-197`). Rollback: revert; nothing persists.
- Ordering with the other plans: M1 before
  [FONT-ATTESTATION-AND-BLOCKING-IO.md](FONT-ATTESTATION-AND-BLOCKING-IO.md)
  M2 (which uses `SpawnOptions.env`); independent of
  [ENCODED-VOD-HOLD-AND-RELEASE.md](ENCODED-VOD-HOLD-AND-RELEASE.md) (which
  changes when `spawn_generation` is called, not what it does).

## 7. Open questions

1. Should the builder take `program: &Path` or resolve `ffmpeg_bin()` /
   `recipe_program(recipe)` itself? VOD's program is the recipe's attested
   executable path (`EncodedExecutable`), not `ffmpeg_bin()`; the builder
   must not re-resolve it. Proposed: caller passes the path; the builder
   never consults `PLURX_FFMPEG`.
2. `ChildJob` in the VOD slot: `attach_owned` takes `Box<dyn Send>` for
   owned resources today (the permit). Add the job to that box, or a
   second field? Proposed: a second, typed field, so the drop order (job
   after wait) is explicit rather than a property of a tuple.
3. The strict key set is FFmpeg 6.1's `-progress` output. Does the
   deployed jellyfin-ffmpeg 8 emit any additional key (`out_time_ms` is
   deprecated there in favour of `out_time_us`, which is already in the
   set)? A one-line check on media1 before M2: `ffmpeg -progress pipe:1 -f
   lavfi -i testsrc=duration=1 -f null - | cut -d= -f1 | sort -u`.
4. `inherit_file_descriptors`'s assert (`files.len() <= 7`, targets 3..=9)
   versus the rolling path's fixed three: keep the general form as the
   shared `pre_exec` and let the builder pass exactly three? Proposed yes;
   the subtitle extractors pass one.

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
