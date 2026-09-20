# Process output capture and scan-probe bounds — one child-process contract for every short-lived helper

**Status:** ready for review · **Executes:** §2.1, C12 (§3.3.1), §5.1 items 1
and 15 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 against `main` @ `88a3957a`

Companion to [VOD-ENCODING.md](VOD-ENCODING.md) (why the encoded producer
attests its engine before every spawn) and
[STREAMING-RELIABILITY-IMPLEMENTATION.md](STREAMING-RELIABILITY-IMPLEMENTATION.md)
(the supervised producer paths this document leaves alone). Read §2 before
touching `process_control.rs`: the helper's callers were written against
`tokio::process::Command::output` semantics and the fix is to restore those
semantics, not to invent new ones. Work §5 in order; M1 and M2 are one PR,
M3 is a second, M4 is a fleet step with no code. If a step seems to require
changing what any caller *reads* from the output, or adding an output bound
to `output_job_owned`, stop and flag it — those are separate contracts (§4).

**Correction to the review:** §2.1 lists `live_tv.rs:7403` among the callers
whose stderr text is now empty. It is not.
[`live_ffmpeg_command_for_input`](../../crates/plurxd/src/live_tv.rs) sets
`.stdin(piped) .stdout(null) .stderr(piped)` itself (`live_tv.rs:6062-6066`),
so `run_graph_probe` receives the stderr it formats. The review is also
slightly wrong about `ffmpeg.rs:2859`: `probe_burst` reads only the exit
status, never stderr. The two stdout consumers (`dovi_probe_output`,
`probe_media_origin`) and the two stderr consumers in `subtitles.rs` are
broken exactly as described. A third correction: the review says the test
should "also run on the Windows lane". There is no Windows test-execution
lane — `ci.yml:206` `windows_compile` is a Linux-hosted `cargo xwin build
--release`; nothing executes a Rust test on Windows in CI. The test must
*compile* for `x86_64-pc-windows-msvc` and be runnable by hand on a Windows
machine; §6 says how each is checked.

## 1. Objective

1. `output_job_owned` returns the child's stdout and stderr, exactly as
   `Command::output` did before `2e3a3bb5` (2026-09-13), so the four broken
   consumers work again: DV Profile 5 transcodes stop being refused with
   "did not prove that Dolby Vision RPU application changes pixels", the
   copy-video media-origin probe answers again, and subtitle-extraction
   failures carry ffmpeg's reason instead of an empty string.
2. Every caller of the helper is audited and listed, with what it reads, so
   the next refactor of this file cannot silently change a consumer again.
3. The ordinary scan probe (`scan/probe.rs::probe`) gets the same bounds the
   reporter probe beside it already has — a wall deadline, an output bound,
   a kill that is confirmed by a reap, a minimal environment — and a typed
   transient/permanent failure, so one hung ffprobe cannot stall a library
   scan and a dropped scan future cannot strand a child.
4. The in-memory `dovi_proofs` cache of false refusals is proven cleared on
   the fleet after deploy.

## 2. Contract today

Re-verify every line number at build time; they are from `88a3957a`.

### 2.1 The helper

[`process_control.rs:39-44`](../../crates/plurxd/src/process_control.rs):

```rust
pub(crate) async fn output_job_owned(
    command: &mut tokio::process::Command,
) -> io::Result<std::process::Output> {
    let (child, _job) = spawn_job_owned(command)?;
    child.wait_with_output().await
}
```

`spawn_job_owned` (`:21-33`) configures a suspended start on Windows,
spawns, attaches the kill-on-close Job Object, resumes. It sets no `Stdio`.
`wait_with_output` collects only what was piped; an inherited stream comes
back as an empty `Vec`. Tokio's `Command::output` (tokio 1.53.1,
`process/mod.rs:1065-1072`) is:

```rust
self.std.stdout(Stdio::piped());
self.std.stderr(Stdio::piped());
let child = self.spawn();
async { child?.wait_with_output().await }
```

That is the contract every caller below was written against: **both streams
piped unconditionally, whatever the caller configured**. `2e3a3bb5` replaced
nine `.output()` calls with the helper and dropped those two lines. #356
repaired one caller (`pipeprobe.rs`) by piping at the call site and left the
helper as it was.

### 2.2 Every caller of `output_job_owned`, and what it reads

| Site | Function | Pipes set by caller | Reads | `kill_on_drop` | Deadline | State on `main` |
|---|---|---|---|---|---|---|
| `pipeprobe.rs:203` | `Spawn::ffmpeg` | stdout per `Stdout` enum, stderr piped | stdout (`Capture`), stderr | no | none | works (#356) |
| `pipeprobe.rs:224` | `Spawn::ffprobe` | both piped | stdout, stderr | no | none | works (#356) |
| `ffmpeg.rs:2437` | `has_dovi_passthrough` | none | status; stderr in the failure `warn!` | yes (`:2427`) | 20 s | status right, failure text empty |
| `ffmpeg.rs:2513` | `has_hdr10_passthrough` | none | status; stderr in the failure `warn!` | yes | 20 s | same |
| `ffmpeg.rs:2671` | `dovi_probe_output` | none | **stdout** (framemd5 lines), stderr on failure | yes (`:2662`) | 30 s | **broken**: always `Err("produced no frame hashes")` |
| `ffmpeg.rs:2859` | `probe_burst` | none | status only | **no** | **none** | status right; child stdio inherits the daemon's |
| `transcode.rs:9274` | `probe_media_origin` | stdin null | **stdout** (`parse_keyframe_origin`), stderr on failure | yes (`:9268`) | 5 s (`MEDIA_ORIGIN_PROBE_TIMEOUT`, `:9228`) | **broken**: always falls back to `start_seconds` |
| `subtitles.rs:1446` | `extract_vtt_window` | stdin null | status; **stderr** in the error | yes (`:1441`) | outer (`publish_extraction`) | error text empty |
| `subtitles.rs:1749` | `extract_vtt` | stdin null | status; **stderr** in the error | yes (`:1745`) | outer | error text empty |
| `live_tv.rs:7403` | `run_graph_probe` | stdin piped, stdout null, stderr piped (`:6062-6066`) | status, stderr | yes (`:6067`) | 20 s | works |

`status_job_owned` (`ffmpeg.rs:2324, 2573, 2628` — `probe_dovi_reshape_graph`,
`has_hdr10_passthrough_qsv`, `has_dovi_passthrough_with`) reads only the exit
status; its children inherit the daemon's stdout/stderr at `-loglevel error`,
which is the pre-09-13 behaviour of `Command::status` and is unchanged here.

Consequences of the two stdout breaks, from the callers:

- `transcode.rs:14301-14335` `require_dovi_renderer`: on `proved == false`
  returns `unsupported_build_error("this source did not prove that Dolby
  Vision RPU application changes pixels through the production renderer")`
  and inserts `false` into `dovi_proofs: Mutex<HashMap<String, bool>>`
  (`:12767`, keyed `path:size:mtime`, `:14297`). The map lives on
  `TranscodeManager`; it is not persisted, so a daemon restart empties it.
  `transcode.rs:13511-13525` reads the same map for the media-offer probe.
- `transcode.rs:9297-9302`: with empty stdout `parse_keyframe_origin` yields
  `None`, `.unwrap_or(start_seconds)` returns the requested start, and the
  "copy session begins at the preceding keyframe" line never logs.

### 2.3 The bounded primitives that already exist

Two, not one, and they differ in ways C12 cares about.

[`ffmpeg.rs:1904-1943`](../../crates/plurxd/src/ffmpeg.rs)
`bounded_command_output_with_limits(command, timeout, max_bytes, label)`:
pipes both, `kill_on_drop(true)`, `spawn_job_owned`, `join!` of two
`read_bounded_with_limit` (`take(max_bytes + 1)` then `read_to_end`) and
`child.wait()`, the whole thing under `tokio::time::timeout`. Constants:
`ENGINE_PROBE_TIMEOUT = 5 s` (`:1529`), `ENGINE_PROBE_MAX_BYTES = 16 MiB`
(`:1544`). Three properties matter here: (a) on timeout the future is
dropped, so the child is killed by `kill_on_drop` but **not reaped by this
code** — tokio's orphan reaper collects it later; (b) a non-zero exit
returns `Err("engine probe exited {status}")` and **discards stderr**; (c) at
the bound it returns `Err` and closes the pipe, which ends the child by
`SIGPIPE`. Its callers are engine attestation and capability probes, where
all three are fine.

[`bounded_process.rs:86-165`](../../crates/plurxd/src/bounded_process.rs)
`output(program, args, wall_time, max_output_bytes) -> io::Result<Output>`:
`env_clear()` plus a nine-name allow-list and `LC_ALL=C`; both piped;
`kill_on_drop(true)`; `process_group(0)` on Unix; drains **past** the cap
(`drain_capped`, `:169-183`) so a flooding child cannot deadlock on a full
pipe; on timeout or error calls `kill_and_reap` (`:49-55`: SIGKILL to the
process group, `start_kill`, **`child.wait().await`**) before returning; on
success kills any descendant left in the group before waiting for pipe EOF;
returns `status`, `stdout`, `stderr` on non-zero exit. Its callers are
`main.rs:3019` (`-version` at startup) and `decoder_health.rs:1330`. It has
three tests (`:189-231`) covering cap, hang and a backgrounded descendant —
all `/bin/sh`.

C12's remedy named the first primitive. The second is the one whose contract
matches C12's acceptance ("a sleeping probe, an output-flooding probe and a
lease loss each leave no child behind"; "establish reap completion before
releasing admission"; "typed transient/permanent failure", which needs
stderr on a non-zero exit — `probe_failure_reason` at `probe.rs:168-186`
is built on it). §3.3 plans against the second.

### 2.4 The scan probe

[`scan/probe.rs:190-232`](../../crates/plurx-core/src/scan/probe.rs):

```rust
let output = tokio::process::Command::new(ffprobe_bin())
    .args(["-v", "error", "-print_format", "json",
           "-show_format", "-show_streams", "-show_chapters"])
    .arg(path)
    .output()
    .await
    .map_err(|e| ProbeError::Spawn(e.to_string()))?;
if !output.status.success() {
    return Err(ProbeError::Failed { path, code: output.status.code(),
                                    reason: probe_failure_reason(&output.stderr) });
}
let mut json: Value = serde_json::from_slice(&output.stdout)
    .map_err(|e| ProbeError::Parse(format!("ffprobe json: {e}")))?;
```

No deadline, no bound, no `kill_on_drop`, no `process_group`, the daemon's
full environment. `ProbeError` (`error.rs:56-73`) has `Spawn`, `Failed {
path, code, reason }`, `Parse` — no variant says "transient". Callers:
`scan/mod.rs:246` (re-probe), `:1019` (retry of a never-probed file on
rescan), `:1122` (new file). All three treat any `Err` as "record without
media detail, count an error, continue" — the scan already continues past a
failure; what it cannot do is continue past a *hang*. The scan runs under a
job lease; `state.rs:4371-4376` `select!`s the scan against
`lost.cancelled()`, so lease loss drops the future mid-probe, and a dropped
`Child` without `kill_on_drop` is not killed (tokio's documented default).
The reporter probe next to it (`:122-152`) is bounded: 5 s, 64 KiB,
`kill_on_drop`, explicit `start_kill` + bounded `wait`.

`plurx-core` cannot call `plurxd::process_control` (dependency direction).
`plurx-core` already depends on `libc` and, on Windows, `windows-sys` with
`Win32_System_Threading` (`plurx-core/Cargo.toml:84-93`); it lacks
`Win32_System_JobObjects`.

## 3. Change

### 3.1 `output_job_owned` pipes both streams and owns the drop

```rust
pub(crate) async fn output_job_owned(
    command: &mut tokio::process::Command,
) -> io::Result<std::process::Output> {
    // `Command::output` semantics: both streams piped whatever the caller
    // configured, because every caller reads `Output` fields that are only
    // filled for a piped stream, and an inherited stream returns an empty
    // Vec without an error. Restored after 2e3a3bb5 dropped it.
    command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let (child, _job) = spawn_job_owned(command)?;
    child.wait_with_output().await
}
```

Two reasons for `kill_on_drop(true)` inside the helper rather than leaving it
to callers: seven of the ten callers set it already and rely on it for
their `timeout` wrapper to be a kill; the three that do not (`pipeprobe.rs`
×2, `probe_burst`) run at startup with no timeout, and the only time their
future is dropped is runtime shutdown, when killing a probe is right. On
Windows the Job Object already kills on handle close, so this changes
nothing there. The helper still does **not** bound output size; see §4.

`status_job_owned` is unchanged: `Command::status` does not pipe.

### 3.2 The caller audit becomes a test, not a comment

The table in §2.2 is copied into a doc comment on `output_job_owned` and a
`#[test]` in `process_control.rs` asserts the count of `output_job_owned(`
call sites in `crates/plurxd/src` equals the table's row count, with the
file list. The reason is the failure mode: the regression was invisible
because nothing named who depended on the semantics. A count is a blunt
instrument; it exists to make the next person open this table.

`pipeprobe.rs`'s `Stdout::{Discard, Inherit}` variants (`:172-181`) become
capture-overridden, exactly as they were under `Command::output` before
09-13. That is not a regression: `Stdout::Inherit` at `:763` never inherited
under `.output()` either. Leave the enum alone in this PR (§7).

### 3.3 The scan probe goes through a shared bounded primitive in `plurx-core`

Move `bounded_process.rs` to `plurx-core` as
`crates/plurx-core/src/process/bounded.rs` (`pub mod process`), and move the
platform pieces it needs from `process_control.rs` with it: `spawn_job_owned`,
`ChildJob`, `configure_suspended`, `resume_suspended`, `signal`,
`ProcessSignal`. `plurxd::process_control` becomes `pub(crate) use
plurx_core::process::*;` so no `plurxd` caller changes. `plurx-core`'s
Windows dependency gains `Win32_System_JobObjects`. This is a `git mv` plus
re-exports — the same behaviour-preserving move the web-shell split used
(#371) — and it is done *before* the scan probe changes, in its own commit,
so the diff that changes behaviour is small.

Then `scan/probe.rs::probe`:

```rust
const SCAN_PROBE_TIMEOUT: Duration = Duration::from_secs(60);
const SCAN_PROBE_MAX_BYTES: usize = 16 * 1024 * 1024;

let output = crate::process::bounded::output(
    &ffprobe_bin(), &args, SCAN_PROBE_TIMEOUT, SCAN_PROBE_MAX_BYTES,
)
.await
.map_err(|error| match error.kind() {
    io::ErrorKind::TimedOut => ProbeError::Transient { path, reason: "ffprobe exceeded 60 s" },
    io::ErrorKind::NotFound  => ProbeError::Spawn(error.to_string()),
    _                        => ProbeError::Transient { path, reason: error.to_string() },
})?;
```

`ProbeError` gains `Transient { path: String, reason: String }` with the
display text "ffprobe did not finish for {path}: {reason}". `Failed` stays
permanent (ffprobe answered and said no); `Parse` stays permanent (a 16 MiB
JSON that does not parse is a real document problem, and the cap is 16× any
observed `-show_streams -show_chapters` output). The three `scan/mod.rs`
callers keep their current arms and add one: on `Transient` the file is
recorded **without** setting `probed`, so the existing `!ex.probed` retry at
`scan/mod.rs:1004-1041` picks it up on the next scan instead of the file
sitting forever with a permanent-looking failure note. The report line says
"did not finish" rather than "could not read", because the operator's fix
is different (a mount, not a file).

Why 60 s and not the reporter's 5 s: a scan probe reads a media file on a
NAS, and `-show_chapters -show_streams` on a large Matroska over a cold mount
can take tens of seconds honestly. The number is a ceiling for a hang, not a
budget for a healthy probe; it is a constant with this reason attached, not
a setting (Paul refuses gates; nothing here needs an operator switch).

Why the environment allow-list is acceptable for a scan: the primitive's
nine names (`PATH`, loader paths, VA-API/VDPAU/CUDA selectors) plus
`LC_ALL=C` are already what the boot probes run under; ffprobe reads a file
and prints JSON and needs nothing else. `PLURX_FFPROBE` is resolved in the
parent (`ffprobe_bin()`), so it is unaffected.

Reap-before-release: `bounded::output` returns only after `child.wait()`
has completed on every path (success, error, timeout) — `kill_and_reap`
awaits the wait. So when `probe()` returns, the child is gone, and the scan
lease at `state.rs:4371` cannot be released with a child still running. On
**drop** (lease loss mid-probe) the primitive's `OwnedChild::drop` sends
SIGKILL to the process group and spawns the reap onto the runtime
(`bounded_process.rs:67-83`); that reap is not awaited by the dropped
future, so the lease release at `state.rs:4378` can precede it by
milliseconds. The requirement is "no child left behind", which this meets;
"reap completes before the lease is released" on the *drop* path would
need the scan to run the probe in a task it can join after cancellation.
§7 asks whether that stronger ordering is wanted; the acceptance test in
§5.3 checks the observable fact (no surviving pid within 2 s of
cancellation).

## 4. Guardrails (non-goals)

- **No output bound in `output_job_owned`.** Adding one would change a
  second semantic in the same PR the assessment wants isolated ("proceed as
  an isolated cross-platform correctness repair"). Callers with neither a
  timeout nor a bound (`pipeprobe.rs` ×2, `probe_burst`) are listed in §7.
- **No change to what any caller parses.** `dovi_probe_output` still wants
  non-comment framemd5 lines; `probe_media_origin` still filters origins
  after the start. If restoring the bytes makes a parser fail, that is a
  new finding, not something to patch here.
- **`status_job_owned` stays unpiped.** Its callers want an exit status and
  the child's `-loglevel error` output in the journal; piping and dropping
  would hide it.
- **The supervised producer paths are untouched**: `transcode.rs:2217/2311`,
  `stream.rs:1961/3243`, `live_tv.rs:5819/5920`, `decode_facts.rs`,
  `fragindex.rs`, `dv_disk.rs`, `ffmpeg.rs:156/176/501/1916` all use
  `spawn_job_owned` directly with their own readers and reap owners. The
  §6 "already good" list names them as preservation constraints.
- **`bounded_command_output_with_limits` keeps its three properties**
  (§2.3). Engine attestation depends on "non-zero exit is an error"; it is
  not replaced by the moved primitive in this plan.
- **The scan stays sequential.** Bounding a probe does not make it
  concurrent; C3 (walk on a blocking pool) is a separate item.
- **`dovi_proofs` stays in memory.** Persisting a proof is F-stream-9's
  attestation question, withdrawn until the persisted identity is defined.
- **No new spawn pattern.** The review's explicit ask; the plan moves one
  existing primitive rather than writing a fourth.

## 5. Milestones

### 5.1 M1 — the helper pipes both streams, with a portable child test

One PR with M2. Change §3.1. Add `#[cfg(test)] mod tests` to
`process_control.rs` with a child that is **the test binary itself**:

```rust
/// Re-exec `current_exe()` with PLURX_CHILD_MODE set; the `child_main`
/// test below dispatches on it. Portable: no /bin/sh, no /bin/echo, builds
/// and runs on Windows, and the "child" is a binary cargo already made.
fn child(mode: &str) -> tokio::process::Command {
    let mut c = tokio::process::Command::new(std::env::current_exe().unwrap());
    c.args(["--exact", "process_control::tests::child_main", "--nocapture"])
     .env("PLURX_CHILD_MODE", mode);
    c
}

#[test]
fn child_main() {
    match std::env::var("PLURX_CHILD_MODE").as_deref() {
        Ok("echo")  => { print!("out-bytes"); eprint!("err-bytes"); }
        Ok("fail")  => { eprint!("reason"); std::process::exit(3); }
        Ok("sleep") => {
            if let Ok(path) = std::env::var("PLURX_CHILD_PID_FILE") {
                std::fs::write(path, std::process::id().to_string()).unwrap();
            }
            std::thread::sleep(Duration::from_secs(300));
        }
        _ => {} // running as an ordinary test: nothing to do
    }
}
```

Tests (all `#[tokio::test]`, each named for the fact it pins):

| Test | Asserts |
|---|---|
| `both_streams_are_captured` | mode `echo`: `stdout` contains `out-bytes`, `stderr` contains `err-bytes`, `status.success()` |
| `a_failing_child_reports_status_and_stderr` | mode `fail`: `status.code() == Some(3)`, stderr contains `reason` |
| `a_caller_that_configured_inherit_still_gets_bytes` | `.stdout(Stdio::inherit())` before the call; stdout still captured (the `Command::output` override) |
| `dropping_the_future_kills_the_child` | mode `sleep`: the child first writes `std::process::id()` to the file named by `PLURX_CHILD_PID_FILE`, then sleeps; the parent runs the helper under `timeout(200 ms)`, reads the pid file, and asserts `process_control::signal(pid, Terminate)` returns `Ok(false)` (no such process) within 2 s — the same `ESRCH`/not-found mapping on both platforms |
| `output_job_owned_call_sites_are_the_audited_set` | §3.2 count |

The test harness captures its own stdout, which is why the child is run
with `--nocapture`; the parent reads the pipe, not the harness. The child
test's name is stable because `--exact` matches it.

Acceptance: `cargo test -p plurxd process_control` green on Linux, and
`cargo check -p plurxd --tests --target x86_64-pc-windows-msvc` compiles
(run through the same `cargo xwin` toolchain `ci.yml:206` installs; the
release lane does not build tests, so this check is the PR author's, in
the PR body).

### 5.2 M2 — caller audit and the dovi/media-origin proofs

Same PR. Add the table of §2.2 as a doc comment. Add two tests that pin the
consumers the regression broke, using a generated fixture (no real titles):

- `ffmpeg.rs`: `dovi_probe_output`-shaped test with `ffmpeg -f lavfi
  testsrc … -f framemd5 -` through `output_job_owned`, asserting at least
  one non-comment line comes back on stdout. Marked `#[ignore = "needs
  ffmpeg"]` in the same style the repo already uses, and named in the PR's
  focused command.
- `transcode.rs`: `probe_media_origin` against a two-keyframe generated MP4,
  asserting the origin is the *preceding* keyframe, not `start_seconds`.
  Uses `plurx_core::testfixtures` under the `fixtures` feature as the
  copyseg tests do.

Acceptance: `cargo test -p plurxd dovi_probe_output media_origin -- --ignored`
green on a host with ffmpeg; `make unit` green.

### 5.3 M3 — the scan probe through the shared primitive

Second PR. Commit 1: the `git mv` of §3.3 with re-exports, no behaviour
change; `cargo check --workspace --all-targets` and `make unit` unchanged.
Commit 2: `probe()` through `plurx_core::process::bounded::output`, the
`Transient` variant, the three caller arms.

Tests in `crates/plurx-core/src/scan/probe.rs` (`PLURX_FFPROBE` pointed at
the test binary re-exec trick from §5.1, or at a tiny cargo-built helper
under `crates/plurx-core/tests/bin/`; the review's rule stands — not
`/bin/echo`):

| Test | Asserts |
|---|---|
| `a_sleeping_ffprobe_is_killed_at_the_deadline` | `PLURX_FFPROBE` = a child that sleeps; with the timeout constant overridden to 200 ms via a `#[cfg(test)]` hook, `probe()` returns `ProbeError::Transient` within 2 s and the child's pid is gone |
| `an_output_flooding_ffprobe_cannot_wedge_the_scan` | child prints 64 MiB of `{` then exits 0; `probe()` returns `ProbeError::Parse` (not a hang) and the child is reaped |
| `a_dropped_probe_future_leaves_no_child` | start `probe()` in a task, abort it after the child has started, assert the pid is gone within 2 s — this is the lease-loss case at `state.rs:4371` |
| `a_permanent_failure_still_carries_ffprobes_reason` | child writes `path: Permission denied` to stderr and exits 1; `Failed { reason: "Permission denied" }` |
| `a_transient_failure_leaves_the_file_unprobed_for_the_next_scan` | in `scan/mod.rs` tests: a `Transient` result records the file with `probed == false`; a second scan re-probes it |

Acceptance: `cargo test -p plurx-core scan::probe` and `cargo test -p
plurx-core scan::tests` green; `make unit` green; `grep -rn
"Command::new(ffprobe_bin())" crates/plurx-core/src/scan/` shows only the
reporter probe.

### 5.4 M4 — deploy and fleet check

No code. After the M1/M2 image is deployed by the Ansible play to media1
and lab1–lab6 (the deploy restarts `plurxd`, which empties `dovi_proofs`
because it is a field on `TranscodeManager`, not a table):

GPT prompt (paste to a session with fleet access):

> On media1, after the plurxd deploy carrying PR <n>: (1) confirm the
> restart cleared the DV proof cache by playing *Night Tide* (a known Dolby
> Vision Profile 5 title) through a transcode path from the web client at
> 1080p and reading the session's playback-info; it must not show
> `unsupported_build_error` with "did not prove … changes pixels", and
> `journalctl -u plurxd --since "<deploy time>" | grep -c "did not prove
> that Dolby Vision"` must be 0. (2) Start a copy-video HLS session for
> *Harbor Lights* at `start_seconds=1800` and confirm the journal now has a
> "copy session begins at the preceding keyframe" line for it, or an
> explicit "origin equals start" — not "media-origin probe did not answer".
> (3) `journalctl -u plurxd --since "<deploy time>" | grep "subtitle
> extraction failed: $"` must be empty (a failure line ending in `: ` means
> stderr is still empty). Report the three commands and their output.

Acceptance: the three observations, recorded in the PR's status note.

## 6. Verification and rollout

- Focused: `cargo test -p plurxd process_control` (M1), `cargo test -p plurxd
  dovi_probe_output media_origin -- --ignored` (M2, ffmpeg host), `cargo
  test -p plurx-core scan::probe` (M3).
- Lane: `make unit` for every PR; `cargo check -p plurxd --tests --target
  x86_64-pc-windows-msvc` in the M1 PR body, because the Windows lane builds
  release only.
- Windows execution of the M1 tests: not provable in CI or on the named
  fleet (no Windows host). GPT prompt: "On a Windows machine with the
  repo's toolchain, run `cargo test -p plurxd process_control` and paste
  the output." If none is available, the PR says so; the compile check and
  the Linux run are the evidence.
- Rollout: two draft PRs into `main` under the fast lane, M1+M2 first. M3
  after, because it moves a module M1 tests live next to. No setting, no
  gate, no schema. Rollback is a revert; nothing persists.
- Metrics: none added by M1/M2. M3 adds one counter to `/metrics`, rendered
  beside the scan counters:
  `plurx_scan_probe_outcomes_total{outcome="ok"|"failed"|"transient"|"parse"}`
  — four fixed labels, incremented in `scan/mod.rs` where each arm already
  bumps `report.errors`. The reason: the C12 defect was invisible for a
  month because a hung probe looked like a slow scan; a `transient` count
  that is not zero is the observation.

## 7. Open questions

1. `pipeprobe.rs:203/224` and `probe_burst` have no timeout and, after M1,
   no output bound either. They run once at startup. Should they move to
   `bounded_command_output_with_limits` (5 s / 16 MiB) in a follow-up? The
   ffmpeg encodes in `pipeprobe` legitimately take seconds; the number
   would need measuring on lab4.
2. `pipeprobe.rs`'s `Stdout::{Discard, Inherit}` are dead under `Command::
   output` semantics and were before 09-13. Delete the variants (one enum,
   two call sites) in the M1 PR, or leave for the spawn-unification work in
   [FFMPEG-SPAWN-UNIFICATION.md](FFMPEG-SPAWN-UNIFICATION.md)? Proposed:
   leave; this PR is behaviour restoration only.
3. On the *drop* path (§3.3) the reap is spawned, not awaited, so a lease
   release can precede it by milliseconds. Is "no surviving pid within 2 s"
   the requirement, or must the scan join the probe task before releasing
   the lease? The latter is a small change in `state.rs:4371` (run the scan
   in a `JoinHandle`, abort it on lease loss, await the handle) and can be
   added to M3 if wanted.
4. `SCAN_PROBE_TIMEOUT = 60 s` is a ceiling chosen from what a cold NAS read
   can honestly take, not measured. The M4-style check is `journalctl | grep
   "did not finish for"` over a week on media1: if healthy files trip it, it
   is too low.
5. Moving `process_control` into `plurx-core` gives `plurx-core` a Job
   Object dependency it did not have. The alternative — a trait object for
   "spawn owned" injected from `plurxd` — keeps the crate boundary at the
   cost of a second abstraction. The move is proposed because it is a
   `git mv` and the review asked for one primitive, not two layers.
