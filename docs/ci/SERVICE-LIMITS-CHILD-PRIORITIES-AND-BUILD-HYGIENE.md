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

**Corrected 2026-09-24 (review of PR #457).** Two claims above were already
false when this plan was written. `inherit_file_descriptors` is one of five
production `pre_exec` registrations (`ffmpeg.rs:131`, `fragindex.rs:1754`,
`dv_disk.rs:1578` and `:5811`, `decode_facts.rs:2472`), and not every ffmpeg
reaches `spawn_job_owned`: `plurx-core` spawns its own. §3.2.1 has the
survey. `setrlimit` now has one production caller, §3.3's
`process::rlimit`.

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

### 3.1.1 What the fleet actually inherits — observed 2026-09-23

M1's measurement, as far as this session could take it. Every number below
was read, not proposed. The hosts reachable from the executing session were
**nuc3** (192.168.4.7), **nuc4** (192.168.4.8) and **nynuc** (192.168.5.236);
no host named `media1` or `lab1` was reachable, and **every reading is idle**
— see "What is still unobserved" below.

All three run the container path. `systemctl show plurxd -p LoadState`
answered `LoadState=not-found` on all three, so `deploy/plurxd.service` is
installed nowhere on them; that answers open question 1 for this slice of the
fleet (§7.1).

```text
$ ssh <host> "docker exec plurxd sh -c 'cat /proc/1/limits'"     # 2026-09-23
Limit                     Soft Limit           Hard Limit           Units
Max open files            1024                 524288               files
Max processes             unlimited            unlimited            processes
Max stack size            8388608              unlimited            bytes
Max locked memory         8388608              8388608              bytes
```

Identical on nuc3, nuc4 and nynuc. The pair is systemd's own default handed
through Docker unchanged — `systemctl show -p DefaultLimitNOFILE
-p DefaultLimitNOFILESoft` reads `524288` / `1024` on all three hosts — and
`docker inspect plurxd` confirms nothing in the Compose file narrows or
widens it:

```text
$ ssh <host> 'docker inspect plurxd --format "Ulimits={{.HostConfig.Ulimits}} PidsLimit={{.HostConfig.PidsLimit}} OomScoreAdj={{.HostConfig.OomScoreAdj}}"'
Ulimits=[] PidsLimit=<nil> OomScoreAdj=0
```

| Reading | nuc3 | nuc4 | nynuc | Command |
|---|---|---|---|---|
| `/proc/1/limits` open files (soft / hard) | 1024 / 524288 | 1024 / 524288 | 1024 / 524288 | `docker exec plurxd cat /proc/1/limits` |
| pid 1 descriptors held (idle, two samples) | 45–46 | 56–61 | 46–47 | `docker exec plurxd sh -c 'ls /proc/1/fd \| wc -l'` |
| pid 1 threads (idle) | 28 | 32 | 28 | `docker exec plurxd sh -c 'ls /proc/1/task \| wc -l'` |
| pid 1 `oom_score_adj` | 0 | 0 | 0 | `docker exec plurxd cat /proc/1/oom_score_adj` |
| cgroup `pids.current` / `pids.max` | 30 / 37262 | 34 / 37300 | 30 / 74782 | `docker exec plurxd cat /sys/fs/cgroup/pids.current /sys/fs/cgroup/pids.max` |
| cgroup `memory.max` | `max` | `max` | `max` | `docker exec plurxd cat /sys/fs/cgroup/memory.max` |
| cgroup `memory.current` | 833658880 (795 MiB) | 21234450432 (19.8 GiB) | 9531957248 (8.9 GiB) | `docker exec plurxd cat /sys/fs/cgroup/memory.current` |
| host `DefaultTasksMax` | 37262 | 37300 | 74782 | `systemctl show -p DefaultTasksMax` |
| `docker.service` own limits | `LimitNOFILE=infinity`, `TasksMax=infinity`, `OOMScoreAdjust=-500` | same | same | `systemctl show docker.service -p LimitNOFILE -p TasksMax -p OOMScoreAdjust` |

`memory.current` is the cgroup's charge, page cache included, not the
daemon's resident set; it is here because §3.1's `MemoryHigh` row asks for
a week of it, and one idle sample is not that.

Three things follow.

1. **The soft limit is the one that runs out, and it is 512× lower than the
   ceiling the deployment actually chose.** Nothing narrowed the hard limit:
   524288 is available on every one of these hosts today, and the daemon may
   have it for the asking. That is §3.3, and it is implemented — it needs no
   deployment-file change and no peak-load number to justify, because it
   cannot lower anything.
2. **`pids.max` is the host's slice default, not a decision.** It varies with
   host RAM (37262 on the two NUCs, 74782 on nynuc) because systemd derives
   `DefaultTasksMax` from the pid limit. A `pids_limit` in Compose would be
   the first deliberate value; §3.1's row still needs its peak number first.
3. **The OOM killer has no preference to act on.** pid 1 sits at
   `oom_score_adj=0` and Docker resets the container to 0 regardless of
   `docker.service`'s own `-500`. Whatever ffmpeg children inherit is also 0,
   so under memory pressure the kernel is as free to take the daemon — and
   every session with it — as one wedged x265. That is §3.2's case, and this
   session did not implement it; see §3.2.1.

#### What is still unobserved

- **Every reading above is idle.** A `/proc` walk inside each container found
  exactly two processes — `plurxd` and the healthcheck's `sh` — so no
  transcode, no DVR recording and no ffmpeg child was running when these were
  taken. The peak descriptor count §3.1's first row is gated on, and the peak
  `pids.current` the second is gated on, are **not** in this document.
- **No `media1` or `lab1`.** The three hosts above are the ones this session
  could reach. If those names are other machines, their limits are unread.
- **`EMFILE` history is a weak negative.** `docker logs plurxd | grep -ciE
  'too many open files|EMFILE'` returned 0 on all three, but the containers
  had been up 2 h, 11 h and 2 h respectively, which is nothing like the week
  §3.1 asks for, and `docker logs` only covers the current container.
- **No child `oom_score_adj` or `nice` reading**, because no child existed.

M1's GPT prompt in §5.1 remains the way to close all four. It is unchanged
and still needs running on a busy evening.

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

### 3.2.1 Flagged, not taken: the child priorities need a decision first

The standing instruction at the head of this plan says that a step which
seems to require changing the process-supervision contract must be stopped
and flagged rather than decided by the executing session, and the work
board's rule 4 says the same. §3.2 is such a step. It is flagged here and in
[PR #457](http://192.168.4.7:3000/noirr/plurx/pulls/457); nothing in §3.2 or
its `OOMScoreAdjust` companion row in §3.1 is implemented.

**The wiring §3.2 prescribes does not cover the tree.** §3.2 says to add the
call "at each `Command::new(ffmpeg_bin())` site" and to have "a test [grep]
that every `ffmpeg_bin()` command passes through it". A survey of every
`Command::new(` in `crates/plurxd/src` outside test modules (2026-09-23; it
did not look at `plurx-core`, which the correction below does)
finds fourteen sites that name `ffmpeg_bin()` literally — in `ffmpeg.rs`,
`subtitles.rs`, `fragindex.rs`, `pgs_overlay.rs` and `pipeprobe.rs` — and
these, which spawn ffmpeg through a value instead:

| Site | Program expression | Why the grep misses it |
|---|---|---|
| `producer_spawn.rs:182` | `Command::new(program)` | the copy-HLS and transcode producers, the two longest-lived ffmpeg children there are, reach this through a `&Path` argument |
| `live_tv.rs:6083` | `Command::new(&system.ffmpeg)` | the probed system binary, resolved at boot |
| `http/images.rs:863` | `Command::new(ffmpeg_bin)` | a parameter that happens to share the function's name |
| `ffmpeg.rs:2115` | `Command::new(&bin)` | `let bin = ffmpeg_bin();` three lines earlier |
| `dv_disk.rs:5752,5805,5853` | `Command::new(program)` | the Dolby Vision conversion tools |
| `decode_facts.rs:3095,3219,…` | `Command::new(snapshot_execution_path(…))` | a snapshotted executable path |

A test keyed on the literal `ffmpeg_bin()` would be green while the producer
that carries realtime playback spawns unadjusted children. That is a test
which proves nothing, and this campaign does not land those.

**Correction (2026-09-24, review of #457): no seam covers the tree today.**
The first version of this section said that every child reaches
`plurx_core::process::spawn_job_owned`, so one `pre_exec` registration there
"would be complete by construction and provable by one test". That was
false. Its survey covered only `crates/plurxd/src`, and it missed a spawn
there too. Re-surveyed on the branch after merging main, across
`crates/plurx-core/src` and `crates/plurxd/src` (every production
`Command::new(`, then every production `.spawn()` / `.output()` /
`.status()` on a command, each traced to the call that starts the child),
these production children never reach `spawn_job_owned`:

| Site | Child | Started by |
|---|---|---|
| `plurx-core` `metadata/local.rs:402` (`generate_thumb_with`, from `:365` and `:812`) | ffmpeg: library-scan thumbnail extraction | `cmd.spawn()` `:419` |
| `plurx-core` `metadata/book.rs:678` (`extract_attached_picture`, from `:199` and `:610`) | ffmpeg: embedded cover extraction | `.spawn()` `:693` |
| `plurx-core` `transcode/encoder.rs:781` (`detect_video_decoders`) | ffmpeg: decoder inventory | `.output()` `:783` |
| `plurx-core` `transcode/encoder.rs:886` (`try_encode`) | ffmpeg: test-encode validation | `.output()` `:891` |
| `plurx-core` `transcode/encoder.rs:951` (`try_encode_yielding`) | ffmpeg: test-encode validation | `command.spawn()` `:958` |
| `plurx-core` `transcode/encoder.rs:1184` (`detect_encoders`) | ffmpeg: encoder inventory | `.output()` `:1186` |
| `plurx-core` `transcode/decoder_inventory.rs:376`, `:451`, `:509` (`advertised_backends`, `probe_clip`, `probe_decode`) | ffmpeg: decoder inventory probes | `.output()` `:379`, `:470`, `:526`, under the file's own timeout `bounded()` (`:483`), not `process::bounded` |
| `plurx-core` `scan/probe.rs:177` (`probe_reporter_identity`) | `ffprobe -version` | `.spawn()` `:183` |
| `plurxd` `media_pool.rs:910` (`spawn_library_root_probe`) | `find`: library-root reachability probe | `.spawn()` `:918` |
| `plurxd` `subtitle_ride_along.rs:1755` (`run_ffmpeg`; arrived with the main merge) | ffmpeg: PGS ride-along self-test | `command.output()` `:1763` under `tokio::time::timeout` |
| `plurxd` `decode_facts.rs:2314` (`spawn_configured_probe`, from `:3112`, `:3868` and `:4038`) | the held-source decode-fact probes | `command.spawn()` `:2319` inside `spawn_blocking` |

Every other production `Command::new(` in the two crates does reach
`spawn_job_owned`: directly, through `output_job_owned` /
`status_job_owned`, through `process::bounded::output`, through `ffmpeg.rs`'s
`bounded_command_output*` (`:2383`) or `BoundedDiagnosticChild` (`:194`,
`:214`), or through `dv_disk.rs`'s `run_tool_command_with_timeout`
(`:5885`). The three builders that return a `Command` (`live_tv.rs:6071`,
`http/chapter_thumbs.rs:339`, `http/images.rs:855`) are spawned by job-owned
callers. `plurx-pgs` and `plurx-compat-plex` spawn nothing in production, and
`plurx-cluster-check` is a separate harness binary, not a daemon child.

The rows above are exactly the background ffmpeg work that child
priorities exist to push below playback: scan thumbnails, cover extraction,
encoder and decoder inventory, and test encodes. So **neither option is
complete as it stands**:

- **(a) a call at each `Command::new(ffmpeg_bin())` site plus a grep test**
  misses the value-spawned producers in the first table, as above.
- **(b) one `pre_exec` inside `spawn_job_owned`** misses every row of this
  table. Its one test would be green while scan thumbnails and encoder
  probes run at the daemon's priority and `oom_score_adj`, which is the same
  defect that rules out (a).

Each option therefore carries its own migration list. (b) must first move
the rows above onto `spawn_job_owned` or an equivalent owned spawn, and needs
an audit test that fails on any production `.spawn()` / `.output()` /
`.status()` outside `process/`. The existing
`process::tests::output_job_owned_call_sites_are_the_audited_set` has that
shape. (a) needs every literal and value-spawned ffmpeg site in both crates,
including the `plurx-core` rows. The choice between them remains the
decision this section flags, together with whether an OOM preference should
reach non-ffmpeg children such as `find` at all. This PR does not take it.

**`pre_exec` composition is answered.** std's `CommandExt::pre_exec` appends
each closure, and the child runs every registered closure in registration
order before `exec` ("multiple closures can be registered and they will be
called in order of their registration"). tokio's `Command::pre_exec`
delegates to std. This was checked on nuc3 with rustc 1.97.1: two closures
on one `Command` printed `first` then `second`. A second registration
composes with `inherit_file_descriptors` and does not replace it. Two things
follow for whichever seam is chosen:

- There are five production `pre_exec` registrations, not one (§2.2's
  correction lists them).
- Order matters where a caller's closure does not return. In production,
  `decode_facts.rs:2472` installs seccomp filters and then execs the probe
  from inside the closure (`execute_held_probe`, `SYS_execveat`, `:2395`).
  A closure registered after it would never run. That is where a
  registration inside `spawn_job_owned` would land, because callers
  configure the command first. A closure registered before it would run
  ahead of the seccomp filter. That path does not reach `spawn_job_owned`
  today anyway (the last row above).

**Independently, §3.2's own acceptance is out of reach here.** The `nice` and
`ioprio` values are explicitly proposals that §3.2's realtime cadence
measurement on a busy media host is supposed to settle, up to and including
settling on "none". No such host was reachable from this session, and §4's
guardrail ("do not keep child priorities that measurably slow realtime
delivery — the measurement decides") is not satisfiable without it.

**So the `OOMScoreAdjust` row in §3.1 also stays out**, under §4's guardrail
that it must not be set without the child `pre_exec`: alone it would make
every ffmpeg exactly as protected as the daemon, which inverts the intent.

What the next session needs, in order:

1. A decision on the seam: (a) with its full site list across both crates;
   (b) with the rows above migrated first and an audit test over every
   production spawn; or §4.1's spawn unification first.
2. For (b), where the registration goes relative to a caller's own
   `pre_exec` (the decode-facts path above), pinned by a test.
3. §3.2's measurement, on a host running real sessions.

### 3.3 Raise soft `NOFILE` at startup

In `main.rs` before the runtime starts: `getrlimit(RLIMIT_NOFILE)`, set
`rlim_cur = rlim_max`, `setrlimit`, log both values at `info` once. Reason:
systemd and Docker give a soft limit far below the hard one, and tokio does
not raise it; raising it is free and makes `LimitNOFILE`'s hard value the
operative one on every install path (including the Windows service, where
the call is a no-op). A unit test with `ulimit -n 256` in a child process
asserts the log line shows `256 → <hard>`.

**Landed 2026-09-23** (`claude-opus-5`). The raise is
`plurx_core::process::rlimit::raise_open_file_limit`, called from `run()` in
`crates/plurxd/src/main.rs` immediately after `init_logging()` and reported by
`report_open_file_limit` beside it. Two departures from the paragraph above,
both deliberate:

- **Not "before the runtime starts".** `#[tokio::main]` builds the runtime
  before any of `main`'s body runs, so that position does not exist. The
  limit is consulted when a descriptor is opened, and the call runs before
  store activation, the system probe and every listener open theirs.
- **The child lowers its own soft limit rather than inheriting `ulimit -n
  256` from a shell.** The test re-execs the test binary — the pattern the
  sibling process-ownership tests already use — and the child calls
  `setrlimit` on itself, so the test needs no shell and cannot disturb the
  shared test process. It asserts the raise both as the function reports it
  and as an independent `getrlimit` reads it back, and it fails loudly rather
  than vacuously on a host whose hard limit is too low to demonstrate
  anything.

**Corrected 2026-09-24 (review of #457): macOS.** As first landed, the raise
always asked for `rlim_cur = rlim_max`. launchd gives a LaunchAgent such as
`deploy/com.plurx.plurxd.plist` `256` soft against an **unlimited** hard
limit, and macOS enforces `kern.maxfilesperproc` whatever the hard limit
says. The review reasoned from `setrlimit(2)`'s COMPATIBILITY note that
`rlim_cur = RLIM_INFINITY` is refused with `EINVAL`, leaving the daemon at 256
with a WARN on every boot and the unit test red. On the lab Mac `mba`
(macOS 27.0; soft `256`, hard `unlimited`, `kern.maxfilesperproc` `122880`)
that does **not** reproduce. `setrlimit` accepts the infinite soft limit,
`getrlimit` reports it back, the kernel still stops the process at 122877
open descriptors with `EMFILE`, and the unfixed test passes. The defect on
this macOS is therefore a daemon that logs `soft 256 -> 9223372036854775807`
for a limit it does not have. On the older versions the man page describes,
the defect is the one the review names; that was not run.

On either behaviour the target should be the per-process ceiling. The
raise now clamps it to `kern.maxfilesperproc` on Apple targets, as Go's
runtime does, and falls back to `OPEN_MAX` (10240) if the sysctl cannot be
read. The Linux behaviour is unchanged: Linux caps the hard limit at
`fs.nr_open`, so a Linux hard limit is always an acceptable soft limit. The
clamp is a pure function (`raised_soft_limit`), pinned on every host by
`an_unlimited_hard_limit_is_clamped_to_the_platform_ceiling`. The re-exec
test, now `the_soft_limit_is_raised_as_far_as_the_platform_allows`, compares
against `min(hard, sysctl -n kern.maxfilesperproc)` on macOS and against the
hard limit elsewhere. On `mba` it fails when the Apple ceiling is removed
and passes with it; the disposition comment on #457 has the output.

The `LimitNOFILE` / `ulimits.nofile` row in §3.1 did **not** land with it, and
deliberately: the observed hard limit is already 524288, so the raise takes
the fleet from 1024 to 524288 with no deployment-file change at all, while
writing `65536:65536` into Compose would *lower* that ceiling eightfold on
the strength of a number nobody has measured. That row still waits on §3.1.1's
peak descriptor count.

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

**Audit result, 2026-09-23** (`claude-opus-5`; read-only, no `ci.yml` change
in this slice). Five jobs carry the `ffmpeg-6` label and `major: "6"`:
`check` (`ci.yml:267,278`), `cluster_daemon` (`:848,861`), `web_layout`
(`:882,902`), `vod_web` (`:930,955`) and `coverage` (`:1228,1255`).

Nothing found in `crates/`, `tests/` or `validation/` *asserts* ffmpeg-6
behaviour — but one test quietly *loses* coverage on 8, which the audit
question as §3.4 words it would have missed:

- The one test that asserts a runtime capability,
  `nightly_runner_has_ffmpeg_readrate` (`crates/plurxd/src/ffmpeg.rs:5189`,
  `#[ignore = "nightly runner capability contract"]`), requires only
  `-readrate`, which landed in 5.1 and is present in 8.
- Most of what names `-readrate_initial_burst` either builds an argument list
  and asserts on the strings (`plurx-core/src/transcode/mod.rs:3959`,
  `plurxd/src/ffmpeg.rs:5034,5077`) or exercises the daemon's own capability
  detection against a stub (`ffmpeg.rs:4971,5006,5148`). Those are
  major-independent and stay green on 8.
- **The exception, and it matters.** The live session test in
  `crates/plurxd/src/transcode/tests/chunk_05.rs` branches on
  `pacing_caps().await.initial_burst` at `:1337`: on a build that declares
  the option without honouring it — which the comment at `:1331` names as
  ffmpeg 8 — it prints `INFO: … does not honour -readrate_initial_burst` and
  **skips** the ahead-window and suspend-resume assertions (`:1346-1352`).
  It does not fail, so moving every lane to 8 would retire that coverage
  silently, with a green suite and an INFO line nobody reads.

So the answer to §3.4's audit question is neither of the two it offers. No
test *requires* ffmpeg 6, so no lane has to be retained to keep the suite
green; but one test *is* weaker on 8, so retiring the last burst-honouring
runner is a real coverage decision, not bookkeeping. Whoever executes M5
should either keep one lane on a burst-honouring build for that test, or
change `chunk_05.rs` to assert the paced-rate behaviour on 8 instead of
skipping — and say which in VALIDATION.md, as §3.4 requires.

This is a source audit only. It does **not** establish that `make unit` is
green on a jellyfin-ffmpeg 8 runner, which is M5's actual acceptance and
needs the runner-provisioning change in `plurx-agent` first.

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

No semicolons, no `strip = "debuginfo"` (assessment 18).

**Pin landed 2026-09-23** (`claude-opus-5`); **the release profile is
untouched.** `Dockerfile:9` and `:35` now read:

```dockerfile
FROM rust:1-bookworm@sha256:93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e AS build
FROM debian:bookworm-slim@sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251 AS runtime-assets
```

Both are **index** digests (`application/vnd.oci.image.index.v1+json`), not
per-platform manifest digests, because the image is built for amd64 and arm64
from the same `FROM`. They were read with `docker buildx imagetools inspect`
on nuc3 on 2026-09-23, and each was verified to be the SHA-256 of the
manifest bytes `--raw` returns. `tests/operations/test_contracts.py`'s
`test_dockerfile_base_images_are_pinned_by_digest` rejects any registry
`FROM` in `Dockerfile` that carries no digest, and
`test_base_image_pin_drift_is_reported_weekly_and_gates_nothing` pins the
drift report onto `rust-audit.yml`'s weekly `scheduled` job, where
`scripts/image-base-drift` prints each pinned digest beside the current
upstream one under `continue-on-error` and `if: ${{ !cancelled() }}`. The
condition was added after review: `continue-on-error` only keeps the step's
own failure from failing the job and does not make it run after an earlier
step failed, so without the condition the report was skipped in every week
the audit it rides on went red. The contract test now asserts it. That step
has **not** been observed
running on a CI runner; it is non-gating by construction, but the claim here
is only that it is wired, not that it has reported.

`Dockerfile.store-shard:8` pins `rust:1.97.1-bookworm` by version tag and is
left alone: the contract test covers `Dockerfile`, which is what ships.

The profile block above — `lto`, `codegen-units`, `debug`, `strip`,
`overflow-checks` — is **unchanged in `Cargo.toml`**. Each of PRs 1-4 is
gated on a measurement (image size delta, a symbolicated backtrace from a
release build, cold build time on the `high-cpu` runner, seven days of lab1
journal) and none of those was run here. PR 1's acceptance
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
Fifteen minutes per target (the 900 s budget) is a budget, not coverage
evidence — each job's summary prints executions and corpus growth so a
target that stops finding new edges is visible.

**As built, 2026-09-24/25 (`plan/P-02-2`, claude-fable-5-1).** The four
targets, `scripts/fuzz-seeds`, `scripts/fuzz-campaign`, the nightly
`parser-fuzz` matrix job and the operations-test extension landed. Where the
table above and the code differ, the code is right and the reason is here:

- **A second fuzz package.** The four targets are `fuzz/parsers/`
  (`plurx-parser-fuzz`: its own `Cargo.toml`, lock, `fuzz_targets/`,
  `corpus/` and `artifacts/`), not `[[bin]]`s beside `inspect_sup` in
  `fuzz/`. A package's dependencies are built for every one of its bins, so
  adding `plurx-core` to the PGS package would have put ~240 crates under
  AddressSanitizer in front of the PGS job's 20-minute step. `inspect_sup`
  and `fuzz/Cargo.lock` are untouched. A campaign is
  `cargo +nightly-2026-08-01 fuzz run --fuzz-dir fuzz/parsers <target>
  fuzz/parsers/corpus/<target>`; `fuzz/parsers/Cargo.lock` was seeded from
  the root lock so the fuzzer resolves the versions that ship.

- **`rpu_rewrite`'s seam.** `Converter::for_init` / `convert` do not exist;
  the module header of `transcode/dvconvert.rs` explains why the
  between-two-ffmpegs form was withdrawn. The seam is
  `convert_length_prefixed(sample, nal_length_size, &mut out)`, and the
  target drives it directly over a length-prefixed sample, with the first
  input byte choosing the `hvcC` width (the three legal widths get seven
  slots in eight, an illegal width one). Its seeds are the repository's own
  Profile 7 RPU (`tests/playback/dv-p7-rpu.hex`) framed as samples, not
  `dv-evidence` output, which needs a server and a real title.
- **Bounds.** Input caps are 4 MiB (`fmp4_reader`) and 1 MiB (the other
  three). The reader target's unit cap is a termination guard at 2²⁰, not
  1 000: a well-formed 4 MiB input cannot reach it, so it fires only for a
  reader that yields without consuming. `-rss_limit_mb` stays at libFuzzer's
  2048 default rather than the 1024 written above: `epub_facts` peaks at
  991 MiB under AddressSanitizer with the reader's own 12 MiB cover cap in
  force, so 1024 would report the sanitizer's overhead as a finding.
- **Tempfile.** `epub_facts` reuses one `NamedTempFile` per thread, truncated
  and rewritten per input, exactly as `inspect_sup` does; the reader is
  handed that path and nothing else. Deleting and recreating per iteration
  bought nothing the reuse does not.
- **`parse_epub`.** No such function; `read_epub_facts` is the catalogue
  seam and the only one a scan reaches. No second EPUB target.
- **Seeds are generated, committed and reproducible.** `scripts/fuzz-seeds`
  writes 13 / 14 / 20 / 17 seeds (≈ 140 / 60 / 80 / 550 KiB); the fMP4 ones
  come from a one-second 64×64 lavfi pattern through libx264, so an x264
  build change can move bytes in them, which is why regeneration is a PR
  that names its build and not a nightly step. `scripts/fuzz-seeds --check`
  runs in the job so an emptied corpus fails before the campaign.
- **The fuzz workspace patches its own `dolby_vision` and `bitvec_helpers`.**
  `fuzz/parsers/Cargo.toml` is a separate workspace and does not inherit the
  root `[patch]` table; the first campaign after the fix below was still
  fuzzing the registry crate until the rows were added there too. Keep the
  two tables in step.
- **Debug assertions are on in the nightly.** `cargo fuzz run` without `-O`
  builds `--release` and adds `-Cdebug-assertions`, so overflow checks are on
  in the campaign and off in the shipped binary. That is wanted (an overflow
  is a finding) and it is why `bitvec_helpers` is vendored too: its `read_ue`
  shifted `1 << 64` on sixty-four zero bits, a panic under the campaign and a
  wrong value in production. The local acceptance runs below used `-O`.

**Findings, first hour.** `fmp4_reader`, `nfo_parse` and `epub_facts` ran
60 s, then 450–300 s, clean (1.1 M / 1.0 M / 21 K executions in the first
minute; 3.2 M / 76 K in the longer runs). `rpu_rewrite` found three ways for
a malformed RPU to stop the conversion inside its first ten thousand
executions, all in the `dolby_vision` 3.4.0 dependency: an allocation sized
from an unbounded ue(v) count (`num_ext_blocks` in the hundreds of millions
→ a 0x603c2cfd0-byte, ~25.8 GB `Vec::with_capacity` → the process aborts),
an `unimplemented!()` two bits away from any valid RPU, and an
`unreachable!()` reached by any level 8/9/10 block with an unlisted length
(those two unwind the converting task, not the process). The crate is now
vendored under `vendor/dolby_vision` with refusals in place of those (its
`PLURX-PATCH.md` has the table, five patches after the review added capped
capacity hints and a per-component mapping-method check that closes a
write-side index panic), `dvconvert` refuses any RPU NAL over 64 KiB before
parsing, and each fuzz input is a fixture under
`tests/playback/dv-p7-rpu-hostile-*.hex` with a test in `dvconvert`'s own
`mod tests`, per the rule above. `bitvec_helpers` 4.0.2, the bit reader
under it, is vendored for two Exp-Golomb overflows the review found. The
vendoring follows `vendor/rust_decimal`: workspace exclude,
`[patch.crates-io]`, `scripts/vendor-audit-lock`, THIRD-PARTY-NOTICES §3.
After the patches `rpu_rewrite` ran 900 s clean: 11.4 M executions, 5 103
new corpus units, 523 MiB peak RSS.

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

**Partial, 2026-09-23** (`claude-opus-5`). §3.1.1 records idle values for
nuc3, nuc4 and nynuc with the command that produced each. No busy-evening
sample and no host named media1 or lab1; the prompt above is unchanged and
still has to be run.

### 5.2 M2 — `NOFILE` raise at startup and the `ulimits`/`LimitNOFILE` rows

Per §3.3 and the first table row, values from M1.

Acceptance: `cargo test -p plurxd rlimit` (the `ulimit -n 256` child test);
after deploy, `docker exec plurxd cat /proc/1/limits | grep 'open files'`
shows the new soft = hard; a week of journal with no `EMFILE`/`Too many
open files`.

**The startup raise landed 2026-09-23** (`claude-opus-5`); **the table row
did not** — see §3.3. The test is `cargo test -p plurx-core rlimit`, not
`-p plurxd`: the raise lives in `plurx_core::process::rlimit`, and
`cargo test -p plurxd open_file_limit` covers the daemon's reporting of it.
The two post-deploy halves of the acceptance — `/proc/1/limits` showing soft
= hard, and a week without `EMFILE` — are **outstanding**, because this
session deployed nothing.

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

**Digest half landed 2026-09-23** (`claude-opus-5`), with the drift step; the
test is `-k dockerfile_base_images` (§3.5 names both new tests). **The
release-profile half did not land at all**: no `debug = "line-tables-only"`,
no `strip = "none"`, no hidden panic flag, no image-size delta, and no
`docker build` was run from the pinned Dockerfile. Those are the parts that
need a release build to mean anything.

**Release-profile half measured 2026-09-25** (`plan/P-02-2`,
claude-fable-5-1) **and not shipped as written.** The hidden flag landed as
the subcommand `plurxd diagnostic-panic` (a subcommand is the shape this CLI
gives every action; `Command::DiagnosticPanic`, `#[command(hide = true)]`).
Four profiles were built from the same tree (`4dd1cfcc`, identical in
content to `830d1197`) on nuc3 with the pinned 1.97.1, 16 threads, thin LTO,
`cargo build --release -p plurxd --bin plurxd` after `cargo clean`, and
`RUST_BACKTRACE=1 plurxd diagnostic-panic` run on each binary:

| Profile | `plurxd` bytes | vs A | gzip (image-layer proxy) | vs A | Build wall / peak RSS | Backtrace |
|---|---|---|---|---|---|---|
| A `strip = "symbols"` (main) | 83 637 528 (79.8 MiB) | — | 31 187 341 | — | 6 m 27 s / 8.9 GB | panic line only; **"stack backtrace:" is empty** |
| B `debug = "line-tables-only"`, `strip = "none"` (PR 1 as written) | 440 675 232 (420 MiB) | **+427 %** | 103 867 528 | +233 % | 6 m 55 s / 10.6 GB | full: `plurxd::diagnostic_panic at ./crates/plurxd/src/main.rs:372:5`, `{async_fn#0} at ./crates/plurxd/src/main.rs:413:37`, `main at …:363:7`, tokio and std frames with paths |
| C B + `split-debuginfo = "packed"` (PR 2 as written) | 241 166 712 (230 MiB) + `plurxd.dwp` 185 212 632 (177 MiB) | +188 % (binary alone) | 61 732 532 + 38 458 907 | +98 % (binary alone) | 7 m 12 s / 11.3 GB | full, as B, with the `.dwp` beside the binary |
| D `debug = "line-tables-only"`, `strip = "debuginfo"` | 113 813 160 (109 MiB) | **+36 %** | 34 786 422 | **+11.5 %** | 6 m 48 s / 11.2 GB | named frames, no lines: `plurxd::diagnostic_panic`, `plurxd::dispatch::{closure#0}`, `plurxd::main` |

`size(1)` shows A and B within 20 KiB of each other in `text` + `data`; the
whole difference is debug sections. The image delta is the binary delta:
`Dockerfile` copies `plurxd` and `plurx-cluster-check` into a
`debian:bookworm-slim` base and nothing else changes, so B's image grows by
~357 MB uncompressed and ~73 MB as a compressed layer; no `docker build` was
run because nuc3's disk was at 95 % and the binary is the whole of the delta.
On the 2-CPU cloud host that measured A first (83 553 536 bytes, an 84 KiB
path-string difference from nuc3's), the B compile of `plurxd` was killed at
6 GB RSS by a ~7 GB cgroup where A's had taken 2.7 GB — line tables through
thin LTO roughly double the linker's peak, a number the §5.7 table wants.

**Outcome.** §3.5's premise, "~+10–20 % binary", is off by an order of
magnitude for this dependency graph (candle, tokenizers, hiqlite, reqwest
and the rest carry most of the line tables), and §5.7's own rule — split
debug only if the delta exceeds 15 % — is met by every option, so PR 1 as
written is **not** the right thing to ship and `Cargo.toml`'s profile stays
main's. The choice is Paul's (§7, question 6): D is the cheap step (named
frames, +11.5 % on the wire, nothing to publish); C is the way to line
numbers without a 420 MiB binary, at the price of a 177 MiB `.dwp` per
release that the release cut (P-03's `scripts/release-cut`) would have to
upload beside the image and an operator would have to fetch to
symbolicate; B is honest and simple and five times the download. Whichever
lands, `plurxd diagnostic-panic` on the deployed image is the check.

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

**Status (2026-09-25):** built on `plan/P-02-2`, in `fuzz/parsers/`. Local
60 s runs with debug assertions on, as the nightly runs them, against the
review-round tree: `fmp4_reader` 544 092, `rpu_rewrite` 1 085 140,
`nfo_parse` 817 151 and `epub_facts` 21 156 executions, no finding, peak
RSS 428–527 MiB; the three earlier crash inputs run clean through the new
binary. Before the `dolby_vision` refusals `rpu_rewrite` crashed within its
first minute (§3.6). `tests.operations.test_evidence_workflows` green. The
five-campaign nightly summary is **post-merge** evidence: the job has not
run on a runner yet, and its first night's summaries belong in the
execution log through the evidence-only docs PR.

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

1. **Which node is bare-metal, if any.** **Answered 2026-09-23, for the
   three reachable hosts: none.** `systemctl show plurxd -p LoadState`
   returns `LoadState=not-found` on nuc3, nuc4 and nynuc, all of which run
   the container (§3.1.1). `deploy/plurxd.service` is an install path with
   no node behind it today, so M4's validation is a lab VM, as the question
   anticipated. Hosts outside those three are unchecked.
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
   **Answered 2026-09-25:** there is one, `read_epub_facts`; `parse_epub`
   does not exist. One target (§3.6 as built).
6. **Which release profile ships** (§5.6's table, 2026-09-25): A (today:
   empty backtraces), D (`strip = "debuginfo"`: named frames, +36 % binary,
   +11.5 % compressed), C (packed split debuginfo: file:line frames, 230 MiB
   binary plus a 177 MiB `.dwp` the release cut must publish), or B (file:line
   frames, 420 MiB binary). The executing session's recommendation is D now
   and C when line numbers are worth the artefact plumbing; §5.7's PR 2 is
   C. **Paul's call.**

---

## Execution log

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-23 | claude-opus-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M1 (partial) | [#457](http://192.168.4.7:3000/noirr/plurx/pulls/457) | Idle limits read on nuc3, nuc4 and nynuc and recorded in §3.1.1 with their commands: `/proc/1/limits` open files **1024 soft / 524288 hard** on all three, `Ulimits=[] PidsLimit=<nil> OomScoreAdj=0`, `pids.max` = the host slice default, `memory.max` unset. `plurxd.service` is `LoadState=not-found` on all three, answering §7.1. **No busy-evening sample and no media1/lab1**; §5.1's prompt still has to be run. |
| 2026-09-23 | claude-opus-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M2 (§3.3 only) | [#457](http://192.168.4.7:3000/noirr/plurx/pulls/457) | `plurx_core::process::rlimit::raise_open_file_limit` raises soft to hard and never touches hard; `run()` reports both values at `info`. Proof it is load-bearing: with the `setrlimit` call removed from the function, `cargo test -p plurx-core rlimit` fails `left: 256, right: 524288`; with the reporter's message and error arm reverted, both `cargo test -p plurxd open_file_limit` tests fail. The §3.1 `LimitNOFILE`/`ulimits` row deliberately did **not** land: the observed hard limit is already 524288, and the plan's 65536 would lower it. Post-deploy `/proc/1/limits` evidence outstanding. |
| 2026-09-23 | claude-opus-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M5 (audit only) | [#457](http://192.168.4.7:3000/noirr/plurx/pulls/457) | §3.4's audit run read-only; `ci.yml` unchanged. Five `ffmpeg-6` jobs listed; no test *requires* ffmpeg 6, but `transcode/tests/chunk_05.rs:1337` **skips** its ahead-window and suspend-resume assertions on a build that ignores `-readrate_initial_burst`, so moving every lane to 8 silently retires that coverage. Recorded as a decision M5 owes, not as a clean bill. |
| 2026-09-23 | claude-opus-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M6 (digest half) | [#457](http://192.168.4.7:3000/noirr/plurx/pulls/457) | Both `Dockerfile` bases pinned to their index digests, read on nuc3 and each verified against the SHA-256 of `imagetools inspect --raw`. `test_dockerfile_base_images_are_pinned_by_digest` fails on the unpinned Dockerfile (`[('rust:1-bookworm', 'build')] != []`) and `test_base_image_pin_drift_is_reported_weekly_and_gates_nothing` fails with the workflow step removed. `scripts/image-base-drift` exits 1 on an unpinned base. The release profile is untouched; the drift step has not been observed on a runner. |
| 2026-09-23 | claude-opus-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M3 — **flagged, not implemented** | [#457](http://192.168.4.7:3000/noirr/plurx/pulls/457) | §3.2.1. §3.2's prescribed wiring (each `Command::new(ffmpeg_bin())` site plus a grep test) does not reach the producers that carry realtime playback, which spawn through a value; the seam that does is `spawn_job_owned`, which the plan's standing instruction says to stop and flag rather than change. §3.2's realtime measurement is also unreachable from here, so the `OOMScoreAdjust` row stays out under §4's guardrail. |
| 2026-09-23 | claude-opus-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M4, M7, M8 — not started | [#457](http://192.168.4.7:3000/noirr/plurx/pulls/457) | M4 needs a lab VM playback matrix and a GPU-selection check under the new unit; M7's three PRs are each gated on a measurement; M8's four fuzz targets need the nightly toolchain and generated corpora. None was attempted, and nothing in the branch pretends otherwise. |
| 2026-09-24 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | Review of #457 (M2, M3 flag, M6) | [#457](http://192.168.4.7:3000/noirr/plurx/pulls/457) | Three findings, all addressed on the branch after merging main. (1) §3.2.1's claim that every child reaches `spawn_job_owned` was false. It has been re-surveyed across `plurx-core` and `plurxd`: eleven production sites spawn without it, including scan thumbnails, cover extraction, the encoder/decoder inventory, `media_pool.rs`'s `find`, the PGS ride-along self-test and the held decode-fact probes. Both M3 options now carry a migration list, and the decision stays open. `pre_exec` composition is answered: std runs every closure in registration order, checked on rustc 1.97.1. §2.2's "only `pre_exec`" claim is corrected to five. (2) The open-file raise now clamps to `kern.maxfilesperproc` on Apple targets. On `mba` (macOS 27.0) the review's `EINVAL` did not reproduce: the old code set and reported an infinite soft limit that the kernel does not enforce. `an_unlimited_hard_limit_is_clamped_to_the_platform_ceiling` fails on Linux without the clamp, and `the_soft_limit_is_raised_as_far_as_the_platform_allows` fails on `mba` without the Apple ceiling (`9223372036854775807` vs `122880`). (3) The drift step is `if: ${{ !cancelled() }}`, and `test_base_image_pin_drift_is_reported_weekly_and_gates_nothing` fails without it. M3 is still not implemented. |
| 2026-09-24 | claude-fable-5-1 | https://claude.ai/code/session_01MuSahCpDVTu88LbxWMUMwS | Claim (second pass: M8, M6 release-profile half) | [#510](http://192.168.4.7:3000/noirr/plurx/pulls/510) | Branch `plan/P-02-2` from `f600d2823`. M3 stays on Paul's seam decision, M4 on the lab1 matrix, M5's lane and M7's PRs 2-4 untouched. |
| 2026-09-24 | claude-fable-5-1 | https://claude.ai/code/session_01MuSahCpDVTu88LbxWMUMwS | M8 (targets, seeds, nightly, contract) | [#510](http://192.168.4.7:3000/noirr/plurx/pulls/510) | `d51a11d2`, then `33706bd3` after review: `fuzz/parsers/` with `fmp4_reader`, `rpu_rewrite`, `nfo_parse`, `epub_facts`; `scripts/fuzz-seeds` (13/16/20/17 seeds); `scripts/fuzz-campaign`; the `parser-fuzz` matrix job; `test_parser_fuzzers_are_bounded_seeded_artifacted_and_gating`. §3.6 "as built" records every departure from the table. 60 s runs clean on all four (see §5.8). |
| 2026-09-24 | claude-fable-5-1 | https://claude.ai/code/session_01MuSahCpDVTu88LbxWMUMwS | M8 finding: `dolby_vision` 3.4.0 | [#510](http://192.168.4.7:3000/noirr/plurx/pulls/510) | `9024fd63`: `rpu_rewrite` found a ~25.8 GB `Vec::with_capacity` from an unbounded ue(v) (process abort), an `unimplemented!()` and an `unreachable!()` (task unwinds) inside 10 000 executions. Crate vendored at `vendor/dolby_vision` with refusals; fixtures `tests/playback/dv-p7-rpu-hostile-*.hex`; three tests in `dvconvert`; ledger row `9024fd63-dv-rpu-hostile-sizes.toml`. After the patches: 900 s, 11.4 M executions, clean. |
| 2026-09-25 | claude-opus (subagent adc9b30ebfcfc08d9) | https://claude.ai/code/session_01MuSahCpDVTu88LbxWMUMwS | Adversarial review | [#510 comment 4575](http://192.168.4.7:3000/noirr/plurx/pulls/510#issuecomment-4575) | 3 P1 (catalog lint, PGS job cost, input-scaled bounds), 5 P2, 8 P3. All folded: `5d18326b` (patches 4-5, `MAX_RPU_NAL_BYTES`, `vendor/bitvec_helpers` for two Exp-Golomb overflows, ledger row `5d18326b-dv-rpu-size-ceiling.toml`) and `33706bd3` (the `fuzz/parsers` split, catalog rows, campaign summary, seeds, audit-lock dedupe, test slices). Disposition on the PR. |
| 2026-09-25 | claude-fable-5-1 | https://claude.ai/code/session_01MuSahCpDVTu88LbxWMUMwS | M6 release-profile half — measured, profile not shipped | [#510](http://192.168.4.7:3000/noirr/plurx/pulls/510) | `830d1197` adds `plurxd diagnostic-panic`; four profiles built on nuc3 (§5.6 table): PR 1 as written is +427 % binary; `strip = "debuginfo"` is +36 % (+11.5 % gzipped) with named frames; packed split is 230 MiB + a 177 MiB `.dwp`. `Cargo.toml` keeps main's profile; which one ships is §7 question 6, Paul's. |
