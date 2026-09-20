# Service limits, child priorities and build hygiene — implementation plan

**Status:** ready for review · **Executes:** §4.6 / F-build-4 / F-build-8 /
F-build-11 / F-build-13 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 against `main` @ `88a3957a`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Companion to [OPERATIONS.md](../OPERATIONS.md) (how the fleet is run) and
[RUST-TEST-EXECUTION-POLICY.md](RUST-TEST-EXECUTION-POLICY.md) (which lane
the ffmpeg change in §3.4 lands in). Read review §4.6 and §0's release
profile paragraph, then assessment corrections 18 and rows `4.6`,
`F-build-ops-codehealth-4/8/11/13`, then this document. Five threads
(systemd unit · child priorities · CI ffmpeg · Dockerfile and release
profile · fuzz); each thread's milestones are ordered by their own
"observe first" step, the threads are independent. Line numbers are from
`88a3957a`; re-verify by function name.

The standing instruction: **if a step seems to require changing the
process-supervision contract (`spawn_job_owned`, `kill_on_drop`, the
reap-before-signal select), the scratch-path layout under the data
directory, or `panic = "unwind"`, stop and flag it.** Each directive below
is added beside those mechanisms, validated, and reverted if it breaks
realtime delivery; none replaces them.

**Correction to the review:** two. (1) §4.6 says `plurxd.service` has
"nothing else" beyond three hardening lines; it also has `ReadWritePaths=/var/lib/plurx`
(`deploy/plurxd.service:30`), which is what makes `ProtectSystem=strict`
workable and what any `PrivateTmp` or `ReadOnlyPaths` change has to be
checked against. (2) The `Dockerfile` copies the pinned `rust-toolchain.toml`
into the build context (`COPY . .`, `:15`) and `rustup`-managed `cargo`
honours it, so the compiler is pinned; the float in `rust:1-bookworm` is
the Debian system-library layer and the `rust:` image's own `rustup`
version (assessment 4.6) — this plan pins the image by digest for that
reason and says so in the file.

---

## 1. Objective

1. `plurxd.service` and `docker-compose.yml` carry explicit file-descriptor,
   task, OOM and namespace limits, each set from an observed fleet value
   and validated against a realtime playback run before it stays.
2. ffmpeg children run below the daemon's CPU and I/O priority and with a
   positive OOM adjustment, applied in an async-signal-safe `pre_exec`,
   without hurting realtime delivery — measured.
3. The daemon raises its soft `RLIMIT_NOFILE` to the hard limit at startup
   and logs both.
4. CI's Rust unit lane runs the ffmpeg major the image ships (jellyfin-ffmpeg
   8), with any older-runtime lane retained explicitly.
5. The Dockerfile base is pinned by digest; the release profile carries
   line tables, and any LTO/CGU/overflow change is measured before it
   lands.
6. Four new bounded fuzz targets (fMP4 reader, RPU rewrite, NFO, EPUB) run
   in the nightly campaign beside the PGS one.

Done means: every directive in §3.1 has its observation and validation
row filled; the child-priority change has its realtime measurement; the
`check` job's runner label reads `ffmpeg-8`; the release profile is the
§3.5 one with numbers; four fuzz targets have corpora and artefact upload.

---

## 2. Contract today

Re-verify at build time.

### 2.1 The unit and the Compose service

```ini
# deploy/plurxd.service:6-31 (abridged)
[Service]
Type=simple
User=plurx
Group=plurx
ExecStart=/usr/local/bin/plurxd run
Environment=PLURX_DATA_DIR=/var/lib/plurx
Restart=on-failure
RestartSec=5
NoNewPrivileges=true
ProtectSystem=strict
ReadWritePaths=/var/lib/plurx
ProtectHome=true
```

No `LimitNOFILE`, `TasksMax`, `OOMScoreAdjust`, `MemoryHigh`, `PrivateTmp`,
`ProtectKernelTunables`, `RestrictSUIDSGID`, `LockPersonality`. systemd's
default soft `NOFILE` for services is 1024 (hard 524288 on current
systemd); `DefaultTasksMax` is 15 % of the pid limit.
`deploy/docker-compose.yml:44-147`: no `ulimits`, no `pids_limit`;
`stop_grace_period` and `healthcheck.start_period` are derived by
`make docker-image-up` (comments `:67-98`).

The fleet runs the container path (`PLURX_IMAGE=forge.lan:3000/noirr/plurxd:main`
in `deploy/.env`, OPERATIONS.md §"The fleet registry"), so the Compose
limits are the ones media1 and lab1–lab6 actually inherit; the unit is the
bare-metal install path.

### 2.2 Child spawning

`crates/plurxd/src/process_control.rs:21-33` `spawn_job_owned(command)`:
`configure_suspended` → `spawn` → `ChildJob::attach` → `resume_suspended`;
no-ops on Unix, Job Object on Windows. Every ffmpeg goes through it except
the VOD spawn (`vodserve.rs:6778-6794`, review §4.1). `configure_ffmpeg_runtime`
(`transcode.rs:2373-2383`) sets `XDG_CACHE_HOME` and `AV_LOG_FORCE_NOCOLOR`
on the command. The only `pre_exec` in the tree is
`inherit_file_descriptors` (`ffmpeg.rs:84-115`): its closure does
`fcntl(F_DUPFD_CLOEXEC)`, `dup2`, `close` and nothing else — "the post-fork
closure performs only descriptor syscalls" (`:83`). That is the model for
any new `pre_exec`. `grep -rn "setpriority\|ioprio\|oom_score_adj\|setrlimit"
crates/` in non-test source: none.

### 2.3 CI ffmpeg

`ci.yml:267` `check` runs on `[…, high-cpu, ffmpeg-6]` and calls
`./.github/actions/ffmpeg` with `major: "6"` (`:275-277`); the same label on
`cluster_daemon` (`:848`), `web_layout` (`:882`), `vod_web` (`:930`),
`coverage` (`:1228`). The action's comment (`action.yml:6-14`) records that
ffmpeg 8 ignores `-readrate_initial_burst` (#380/#386) and "CI never caught
it because CI has never run ffmpeg 8". The image installs `jellyfin-ffmpeg8`
and asserts AC-4, `dovi_rpu` and `tonemapx apply_dovi` (`Dockerfile:74-92`),
default engine via `PLURX_FFMPEG=/usr/lib/jellyfin-ffmpeg/ffmpeg`.
Persistent runner VMs are provisioned by Ansible and the action refuses to
mutate them (`action.yml:53-56`).

### 2.4 Dockerfile base and release profile

`Dockerfile:3` `FROM rust:1-bookworm AS build`; `:26` `FROM
debian:bookworm-slim AS runtime-assets`. Neither by digest.
`Cargo.toml:140-142`:

```toml
[profile.release]
lto = "thin"
strip = "symbols"
```

`debug` unset (0), so no line tables; `strip = "symbols"` removes the
symbol table as well. Panic locations print (they are string literals);
native backtraces are addresses. `[profile.transport-recovery]` (`:133-138`)
already shows the house pattern of a profile with `debug = 1` and
`overflow-checks = true` for a diagnostic build.

### 2.5 Fuzz

`fuzz/Cargo.toml`: one target, `inspect_sup` over `plurx-pgs`, own
workspace and toolchain. `validation-nightly.yml:73-125`: `cargo
+nightly-2026-08-01 fuzz run inspect_sup fuzz/corpus/inspect_sup --
-max_total_time=900`, 20-minute job timeout, `continue-on-error` with a
following step that fails the job on a crash, artefacts uploaded 14 days.
`tests/operations/test_evidence_workflows.py:119` pins it as "bounded,
seeded, artifacted and gating". Parser seams with no fuzz target:
`plurx_core::fmp4::FragmentReader::{push,next_unit}` (`fmp4.rs:351-372`),
`plurxd::dvpipe::Converter::{for_init,convert}` (`dvpipe.rs:117,152`),
`plurx_core::scan::nfo::parse(&str)` (`scan/nfo.rs:86`),
`plurx_core::metadata::book::read_epub_facts(&Path)` (`book.rs:121`) and
`plurxd::http::publication::parse_epub(&Path)` (`publication.rs:502`).

---

## 3. Change

### 3.1 Unit and Compose directives — each with its observation and validation

| Directive | Value (proposal) | Observe first | Validate | Reason |
|---|---|---|---|---|
| `LimitNOFILE` / `ulimits.nofile` | `65536:65536` | `cat /proc/$(pidof plurxd)/limits`, `ls /proc/$(pidof plurxd)/fd \| wc -l` on media1 at peak (three sessions + DVR) | the fd count under peak load ≤ 10 % of the new soft limit; no `EMFILE` in the journal for a week | each session holds pipes, the source fd, up to seven sidecar fds (`inherit_file_descriptors` asserts ≤7), segments and sockets; the hiqlite patch ledger records an fd-exhaustion incident |
| `TasksMax` / `pids_limit` | `4096` | `systemctl show plurxd -p TasksCurrent` / `cat /sys/fs/cgroup/…/pids.current` at peak | peak threads + children ≤ 25 % of the limit (tokio workers + blocking pool 512 max + ffmpeg threads × sessions) | a runaway fork bomb in a child must not take the host; too low kills legitimate x265 thread pools |
| `OOMScoreAdjust` | `-500` | `cat /proc/$(pidof plurxd)/oom_score_adj` and each ffmpeg child's | with §3.2's `pre_exec` in place, every child shows `+500` (or the value chosen) and the daemon `-500`; without §3.2 this directive is **not** applied | the killer must prefer a wedged x265 over the daemon; the adjustment is inherited, so a child that keeps `-500` inverts the intent |
| `MemoryHigh` | not set in this plan | `MemoryCurrent` over a week | — | a throttle without a measured ceiling stalls playback silently; revisit after the buffer/scratch work |
| `PrivateTmp` | `true` | `grep -rn "temp_dir\|/tmp" crates/plurxd/src crates/plurx-core/src` (non-test): none in production paths; scratch is under `PLURX_DATA_DIR` (`transcode.rs:27167` `state_root.join("transcode")`) | full playback matrix (direct, copy HLS, transcode, subtitle burn, DVR) on lab1 with the directive; fontconfig cache is `XDG_CACHE_HOME` under the data dir, not `/tmp` | isolates the daemon's `/tmp` from other services; safe only because nothing production reads `/tmp` |
| `ProtectKernelTunables=true` | yes | — | GPU probe still selects QSV/VAAPI on media1 (`/sys/class/drm` reads are unaffected; `/proc/sys` writes are not needed) | blocks sysctl writes from a compromised child |
| `RestrictSUIDSGID=true` | yes | — | scratch and DVR writes still succeed (they never set SUID bits) | prevents creating setuid files in `ReadWritePaths` |
| `LockPersonality=true` | yes | — | ffmpeg starts (no personality change needed) | closes an old exploit class at no cost |
| `SupplementaryGroups=render` | stays a comment | — | — | host-specific; documented at `:24-26` |

Every row lands only after its "observe" column is filled with the fleet
value and its "validate" column with a dated result; the assessment's
"missing explicit limits does not establish the fleet's inherited limits or
measured exhaustion threshold" is why the table has those two columns.

### 3.2 Child priorities and the `pre_exec` contract

```rust
// process_control.rs — Unix only; called from spawn_job_owned before spawn
#[cfg(unix)]
pub(crate) fn lower_child_priority(command: &mut tokio::process::Command, policy: ChildPriority) {
    let nice = policy.nice;                 // i32, e.g. 10
    let ioprio = policy.ioprio;             // u32, IOPRIO_PRIO_VALUE(IOPRIO_CLASS_BE, 7)
    let oom = policy.oom_score_adj;         // &'static [u8], e.g. b"500\n"
    unsafe {
        command.pre_exec(move || {
            // Async-signal-safe only: raw syscalls, no allocation, no logging,
            // no std::fs, no formatting. Errors are ignored on purpose: a
            // priority that cannot be lowered must not prevent a transcode.
            libc::setpriority(libc::PRIO_PROCESS, 0, nice);
            libc::syscall(libc::SYS_ioprio_set, 1 /* IOPRIO_WHO_PROCESS */, 0, ioprio);
            let fd = libc::open(b"/proc/self/oom_score_adj\0".as_ptr().cast(), libc::O_WRONLY | libc::O_CLOEXEC);
            if fd >= 0 { libc::write(fd, oom.as_ptr().cast(), oom.len()); libc::close(fd); }
            Ok(())
        });
    }
}
```

Rules with reasons:

- **Async-signal-safe** means: `setpriority`, `syscall`, `open`, `write`,
  `close` — all in POSIX's safe list; the OOM string is a `&'static [u8]`
  computed before the fork; no `format!`, no `tracing`, no `std::fs::write`
  (which allocates a `PathBuf`). Between `fork` and `exec` in a
  multithreaded tokio process an allocation can deadlock on a lock another
  thread held at fork time (assessment F-build-8: "do not allocate or
  perform arbitrary file/logging work after fork").
- **`oom_score_adj` written by the child, not by the parent after spawn**:
  the parent-side write races the child's exec and needs the pid; the
  child's own `/proc/self` needs neither. A non-root process may raise its
  own `oom_score_adj` freely; lowering below the inherited value is what
  needs `CAP_SYS_RESOURCE`, and `+500` from `-500` is a raise.
- **Applies to every child path**, including the VOD spawn at
  `vodserve.rs:6778` and `output_job_owned` — which is one more reason
  §4.1's spawn unification matters; until then the call is added at each
  `Command::new(ffmpeg_bin())` site (`transcode.rs:2163, 2299`,
  `ffmpeg.rs:2204, 2313, 2425, 2496, 2556, 2611`, `vodserve.rs:6778`) and
  a test greps that every `ffmpeg_bin()` command passes through it.
- **Checked against realtime delivery** before it stays: `nice 10` and
  `ioprio BE/7` are proposals. On media1 under three concurrent transcodes
  plus a 4K direct play plus a DVR recording, measure the copy-HLS
  producer's segment cadence (`plurx_live_tv_starts_total` is not this; use
  the session status `http_wait_count` and the segment materialisation
  interval in the journal) with and without the priorities. If a producer
  that used to keep up now falls behind, lower `nice` to 5 and ioprio to
  BE/4 and re-measure; if it still falls behind, the priorities stay off
  and this document says so. CPU scheduling and I/O scheduling and the OOM
  killer are three different mechanisms (assessment F-build-8) — the
  measurement records all three separately.
- **cgroup v2 group-kill**: with `OOMPolicy=continue` (the default for
  `Type=simple` without `Delegate`), the killer picks a process, not the
  whole unit; the `+500` child is the pick. Document that `OOMPolicy=kill`
  would kill the daemon along with it and is not set.

### 3.3 Raise soft `NOFILE` at startup

In `main.rs` before the runtime starts: `getrlimit(RLIMIT_NOFILE)`, set
`rlim_cur = rlim_max`, `setrlimit`, log both values at `info` once. Reason:
systemd and Docker give a soft limit far below the hard one, and tokio does
not raise it; raising it is free and makes `LimitNOFILE`'s hard value the
operative one on every install path (including the Windows service, where
the call is a no-op). A unit test with `ulimit -n 256` in a child process
asserts the log line shows `256 → <hard>`.

### 3.4 CI unit tests on jellyfin-ffmpeg 8

Two options; the plan takes the first and keeps the second as fallback:

- **Runner provisioning** (preferred): the Ansible role that provisions
  the persistent lab runners installs `jellyfin-ffmpeg8` from the same
  Jellyfin apt repository the `Dockerfile` uses, symlinks it first on
  `PATH` for runner jobs, and relabels those runners `ffmpeg-8`. `ci.yml`'s
  `check`, `cluster_daemon`, `web_layout`, `vod_web`, `coverage` move to
  `ffmpeg-8` with `major: "8"`. The action already fails on a mismatch, so
  a runner that did not get the package fails loudly.
- **Run in the `runtime-assets` image** (fallback): a `container:` job
  built from the Dockerfile's `runtime-assets` stage plus the Rust
  toolchain; slower to start, but byte-identical to what ships.

Either way: **retain an older-runtime lane explicitly** if any test asserts
ffmpeg-6 behaviour (the assessment's "explicitly retain any older-runtime
compatibility lane"). Audit first: `grep -rn "ffmpeg 6\|major 6\|readrate_initial_burst"
crates/ tests/ validation/` and the `#[ignore = "nightly runner capability
contract"]` test in `ffmpeg.rs:4488`. If nothing asserts 6, the label is
retired and VALIDATION.md says why; if something does, that test moves to a
`ffmpeg-6` job of its own. This change lands in whichever lane
RUST-TEST-EXECUTION-POLICY.md §7.1 chooses; if `make unit` joins the fast
lane, the fast lane's `rust_compile` gets the same action and label.

### 3.5 Dockerfile base pin and the release profile

Pin: `FROM rust:1-bookworm@sha256:<digest> AS build` and
`FROM debian:bookworm-slim@sha256:<digest> AS runtime-assets`, with a
comment stating the digest is the system-library pin (the compiler is
pinned by `rust-toolchain.toml`), and a `tests/operations` check that both
`FROM` lines carry a digest. The weekly `rust-audit.yml` gains a step that
prints the current upstream digest beside the pinned one, so drift is
visible without being automatic.

Profile, one line at a time, each its own measured PR:

```toml
[profile.release]
lto = "fat"                 # PR 3: measure build time and binary size against "thin" first
codegen-units = 1           # PR 3: same measurement; lands with or without lto=fat by the numbers
debug = "line-tables-only"  # PR 1: symbolicated native stacks; ~+10–20 % binary, no runtime cost
strip = "none"              # PR 1: "debuginfo" would remove the line tables above.
                            #       If size matters, PR 2 ships split debug files instead.
overflow-checks = true      # PR 4: changes runtime behaviour (a wrapped counter becomes a panic);
                            #       run make test-full and a week on lab1 before the fleet
```

No semicolons, no `strip = "debuginfo"` (assessment 18). PR 1's acceptance
is a symbolicated backtrace from a deliberate `panic!` behind a hidden CLI
flag on the release binary; PR 2 (`split-debuginfo = "packed"` plus
uploading the `.dwp` beside the image) only if PR 1's size delta on the
image exceeds 15 %; PR 3 records `time cargo build --release` cold on the
`high-cpu` runner and `ls -l target/release/plurxd` for thin vs fat and
CGU 16 vs 1, and lands the combination whose build-time cost Paul accepts;
PR 4 records `make test-full` green and seven days of lab1 journal with no
`attempt to add with overflow` panic. `panic = "unwind"` stays (review
§4.6).

### 3.6 Fuzz targets

Four `[[bin]]`s in `fuzz/Cargo.toml`, each a bounded callable seam, each
with a seed corpus of valid and invalid inputs and the same
`-max_total_time=900` budget and artefact upload as `inspect_sup`:

| Target | Seam | Seeds | Bound |
|---|---|---|---|
| `fmp4_reader` | `FragmentReader::new(); push(data); loop next_unit()` | `init.mp4` + two fragments from a real remux (generated fixture, not library media), truncated and bit-flipped variants | `push` in ≤64 KiB slices; stop after 1 000 units; `-rss_limit_mb=1024` |
| `rpu_rewrite` | `Converter::for_init(&init)` then `convert(&mut fragment)` over units from `fmp4_reader`'s corpus with DV NALs | a Profile 7 → 8.1 fixture the `dv-evidence` script already builds | same as above; `Refused` is a valid outcome, a panic is not |
| `nfo_parse` | `scan::nfo::parse(&str)` | the three fixtures in `scan/nfo.rs` tests, plus malformed XML | input ≤1 MiB |
| `epub_facts` | `read_epub_facts(&path)` on a tempfile from the fuzz input; `parse_epub` likewise (a second target if the first is cheap) | a minimal EPUB 2 and EPUB 3, a zip bomb, a missing `mimetype` | `-rss_limit_mb=1024`, tempfile deleted per iteration; the 620 MiB memory-bound proof stays an ignored test, not a fuzz seed |

Rules: filesystem-input seams get a tempfile harness that never reads
outside it (assessment F-build-13); each crash artefact is promoted to a
unit test in the parser's own `mod tests` before the fix merges;
`test_evidence_workflows.py:119`'s assertion extends to all five targets.
Ten minutes per target is a budget, not coverage evidence — the nightly
summary prints executions and corpus growth so a target that stops finding
new edges is visible.

---

## 4. Guardrails (non-goals)

- **Do not set a directive without its observed value.** The table's
  "observe first" column is the gate; a limit chosen from a blog post is
  what the assessment rejected.
- **Do not set `OOMScoreAdjust` without the child `pre_exec`.** Alone it
  makes every ffmpeg as protected as the daemon, which is the opposite of
  the goal.
- **Nothing in `pre_exec` beyond raw syscalls on pre-computed values.** No
  `format!`, no `tracing`, no `std::fs`, no `CString::new` (allocates).
- **Do not keep child priorities that measurably slow realtime delivery.**
  The measurement in §3.2 decides; "sensible defaults" do not.
- **Do not set `MemoryHigh` or `MemoryMax` in this plan.** No ceiling has
  been measured; a throttle stalls playback without a log line.
- **Do not enable `PrivateTmp` until the grep and the playback matrix are
  recorded**, and do not move scratch to `/tmp` to "use" it.
- **Do not widen the ffmpeg action to accept two majors** (`action.yml:120-125`
  says so). One lane, one major; a second major is a second lane.
- **Do not write `strip = "debuginfo"` with `debug = "line-tables-only"`**;
  the first deletes the second.
- **Do not land `lto = "fat"`, `codegen-units = 1` or `overflow-checks`
  without their measurement PRs.** They are trade-offs; the review's
  snippet was corrected to say so.
- **Do not fuzz with library media** or commit any real file as a seed;
  fixtures are generated (`dv-evidence`, `mkpgs`, the nfo test strings).
- **Do not remove the Windows job-object path** while adding Unix
  priorities; the `#[cfg(unix)]` guard is the contract.

---

## 5. Milestones

Draft PRs into `main` under the fast lane; server changes run `make unit`
plus the focused command named.

### 5.1 M1 — observe the fleet (no code)

GPT prompt:

```text
On media1 and lab1 (both run the container), during a busy evening (≥2
transcodes, a direct play, a DVR recording — start them if needed with
Harbor Lights / Night Tide): `docker exec plurxd sh -c 'cat
/proc/1/limits; ls /proc/1/fd | wc -l; cat /proc/1/oom_score_adj; cat
/sys/fs/cgroup/pids.current /sys/fs/cgroup/pids.max 2>/dev/null; for p in
$(pgrep ffmpeg); do cat /proc/$p/oom_score_adj; done'` and on the host
`docker stats --no-stream plurxd`. Repeat idle. Also `systemctl show
plurxd -p LimitNOFILE -p TasksMax -p TasksCurrent -p MemoryCurrent` on any
bare-metal node if one exists. Report each command's output per node,
labelled busy/idle.
```

Acceptance: §3.1's "observe first" column has media1 and lab1 values with a
date.

### 5.2 M2 — `NOFILE` raise at startup and the `ulimits`/`LimitNOFILE` rows

Per §3.3 and the first table row, values from M1.

Acceptance: `cargo test -p plurxd rlimit` (the `ulimit -n 256` child test);
after deploy, `docker exec plurxd cat /proc/1/limits | grep 'open files'`
shows the new soft = hard; a week of journal with no `EMFILE`/`Too many
open files`.

### 5.3 M3 — child priorities behind a measurement

`lower_child_priority`, wired at every `ffmpeg_bin()` site, the grep test,
`OOMScoreAdjust=-500` in the unit and `oom_score_adj` `+500` from the
child, the realtime measurement on media1 recorded in §3.2 with the values
that stayed.

Acceptance: `cargo test -p plurxd child_priority` asserts a spawned `sh -c
'cat /proc/self/oom_score_adj; nice'` child reports `500` and `10` (or the
chosen values) while the test process reports its own; on media1 `for p in
$(pgrep ffmpeg); do cat /proc/$p/oom_score_adj; done` prints the child
value and `/proc/$(pidof plurxd)/oom_score_adj` the daemon's; the segment
cadence table before/after is in the PR.

### 5.4 M4 — `TasksMax`/`pids_limit`, `PrivateTmp`, hardening trio

The remaining table rows, each with its validation run on lab1 (the
playback matrix for `PrivateTmp`, GPU selection for
`ProtectKernelTunables`).

Acceptance: `systemd-analyze security plurxd` score improves and is pasted
in the PR; lab1 plays direct, copy HLS, transcode with burned text
subtitles, and records one DVR programme under the new unit; `make
operations-check` green with an updated unit-file text contract if one
exists.

### 5.5 M5 — jellyfin-ffmpeg 8 in CI

The runner provisioning change (in `plurx-agent`'s runner role, referenced
by path in the PR), the label and `major` changes in `ci.yml`, the audit
result for a retained ffmpeg-6 lane, VALIDATION.md's ffmpeg paragraph.

Acceptance: the `check` job's step summary reads `expected major: 8`
`resolved: ffmpeg version 8…jellyfin`; `make unit` green on that runner;
`grep -n "ffmpeg-6" .github/workflows/ci.yml` returns either nothing or
only the retained lane named in VALIDATION.md.

### 5.6 M6 — Dockerfile digests and release profile PR 1

Digest pins with the drift-print step; `debug = "line-tables-only"`,
`strip = "none"`; the hidden panic flag for the acceptance.

Acceptance: `python3 -m unittest tests.operations.test_contracts -k
dockerfile_base_is_pinned`; `docker build` reproduces; `plurxd
--diagnostic-panic` (hidden) on the release binary prints a backtrace with
`crates/plurxd/src/…:<line>` frames; image size delta recorded.

### 5.7 M7 — release profile PRs 2–4 (each by its numbers)

Split debug only if M6's delta > 15 %; fat LTO / CGU 1 with the build-time
table; overflow checks after `make test-full` and seven lab1 days.

Acceptance: each PR body carries its table; PR 4's acceptance is the
journal grep `attempt to .* with overflow` empty after seven days on lab1.

### 5.8 M8 — four fuzz targets

`fuzz/Cargo.toml` bins, harnesses, seed corpora, nightly steps, the
operations test extension.

Acceptance: `cargo +nightly-2026-08-01 fuzz run <target> -- -max_total_time=60`
runs each target locally without a crash; the next nightly's summary lists
five campaigns with executions and corpus sizes; `python3 -m unittest
tests.operations.test_evidence_workflows` green.

---

## 6. Verification and rollout

Fast lane: `make unit` for M2/M3/M6/M7; `make operations-check` for
M4/M5/M6/M8. Fleet: M1's prompt before anything; M3's realtime measurement
on media1; M4's matrix on lab1 first, then the fleet by the ordinary serial
`make docker-image-up` (the Compose file change rides the repo checkout the
role already does). The unit file changes reach bare-metal installs through
`deploy/install`; there is no bare-metal fleet node today, so the unit is
validated on a lab VM.

Rollback: every directive is one line; reverting the line and `systemctl
daemon-reload && systemctl restart plurxd` (or `make docker-image-up`) is
the whole procedure. Profile changes roll back by the `sha-` image tag.

---

## 7. Open questions

1. **Which node is bare-metal, if any.** The unit is maintained but the
   fleet is containers; if nothing runs the unit, M4's validation is on a
   lab VM and the doc says so.
2. **`nice`/`ioprio` values.** 10 and BE/7 are proposals; §3.2's
   measurement may settle on 5 and BE/4 or on none.
3. **Runner provisioning ownership.** The Ansible runner role lives in
   `plurx-agent`; M5's provisioning half is a PR there, referenced from the
   `ci.yml` PR.
4. **Fat LTO build time on the `high-cpu` runner.** If it doubles the
   release build, Paul may prefer thin + CGU 1; PR 3's table is the input.
5. **`epub_facts` vs `parse_epub`.** Two EPUB parsers exist (metadata facts
   and the reader's publication model); whether both get a target depends
   on the first one's cost.

---

## Execution log

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| | | | | | |
