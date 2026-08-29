# Windows port — implementation plan

**Status:** ready to build · **Target:** native `plurxd.exe` on Windows 10
1809+/Server 2019+ x64, NTFS · **Baseline:** v0.2.7, `main` @ 2026-08-29 ·
**Written:** 2026-08-29

Companion to [ARCHITECTURE.md](ARCHITECTURE.md) (how the server is built),
[SECURITY.md](SECURITY.md) (the threat model the port must not silently
weaken), and [OPERATIONS.md](OPERATIONS.md) (the env vars and paths this plan
extends). This document is the handoff for the agent executing the port: the
complete inventory of what is Unix-specific today, the decision for each
piece, and the milestones with their acceptance checks.

## 1. Objective

A Windows user should be able to install plurx, launch it at boot, point it at
media, and get hardware-accelerated playback — the same promise the Linux and
macOS deploys make. Concretely, by the end of this plan:

- `plurxd.exe` builds, boots, scans, direct-plays, and transcodes on Windows.
- It installs and launches as a Windows service (`plurxd service install`),
  with a console mode (`plurxd run`) identical to other platforms.
- NVENC and QuickSync hardware transcode work, admitted through the same
  boot probes and measured-admission policy every other family obeys.
- CI builds and gates the Windows target so it cannot silently rot.

What already works and needs no port: the web client (a browser), the Android
and Apple clients (they speak HTTP to the server), Docker Desktop/WSL2 as a
stopgap (runs the Linux image today, software transcode only — WSL2 exposes
neither `/dev/dri` nor NVENC to containers). The port is the server.

**Read §6 (non-goals) before writing any code.** Several plausible
"improvements" are explicitly out of scope, and one decision (§4 D2) needs
Paul's sign-off before its milestone starts.

## 2. How to work this plan

Milestone by milestone, one PR per milestone, in order — each milestone's
acceptance check is a runnable command or observable fact, and a milestone is
not done until its check passes. During development use the fast lane (`make
unit`, `make validate-staged`); the full gate (`make check`, `make
test-full`) runs once per milestone before its PR. Code references below
(file, function, line) were taken from `main` on 2026-08-29 — **re-verify
each against the tree at build time**; the shape is stable, the line numbers
are not.

The standing instruction: if a step seems to require changing behavior on
Linux or macOS, or weakening a guarantee `SECURITY.md` documents, stop and
flag it instead of doing it. The port adds a platform; it does not refactor
the existing ones.

## 3. The Unix surface, inventoried

About 168 `cfg(unix)`/`std::os::unix`/`libc` sites in `crates/`, but they
cluster into seven areas. Everything else in the workspace — axum, tokio,
`rusqlite` (bundled), the scanner, the web app, the Plex façade, GDM's UDP
sockets — is portable Rust and compiles for `x86_64-pc-windows-msvc` as-is.

| Area | Where | What it uses | Windows answer (§4) |
|---|---|---|---|
| Capability filesystem | `plurx-core/src/fs_secure.rs` (~3,570 lines) | `openat(O_NOFOLLOW)` walks, `unlinkat`/`renameat`/`fstatat`, `DIR*` streams, dev+ino identity, `geteuid` ownership, `memfd`+seals | D1 |
| ffmpeg descriptor handoff | `plurxd/src/transcode.rs` (`pre_exec`, `F_DUPFD_CLOEXEC`→`dup2` to fds 3/4/5; `/dev/fd/3`, `/dev/fd/4/<part>`, `/dev/fd/5`) | fd inheritance + `/dev/fd` | D2 |
| Pause/resume | `transcode.rs` `apply_ahead_window` (SIGSTOP/SIGCONT), `prodrun.rs` `signal()` | POSIX signals | D3 |
| Kill/reap/orphans | `child.kill()` everywhere; no process groups | portable already; orphan gap | D4 |
| Delete-while-open | `cachekeep.rs` sweeps, `fs_secure` scratch clearing | Unix unlink semantics | D5 |
| Small syscalls | `statvfs` (membership.rs), `gethostname` (membership.rs), `getrusage` (http/publication.rs), `posix_fadvise` (storeprobe.rs), `renameatx_np` (macOS branch) | one-liners each | D6 |
| Shutdown signals | `plurxd/src/main.rs` ~2236 (`SignalKind::terminate/interrupt`) | tokio unix signals | D7 |

Plus two areas outside `crates/`:

- **Vendored cluster storage** — `vendor/hiqlite`, `vendor/hiqlite-wal` have
  six Unix sites: `PermissionsExt` 0o600/0o700 chmods (`helpers.rs:264`,
  `log_store.rs:37`), `MetadataExt` in `client/backup.rs`, `cfg(unix)` mmap
  `Advice::Sequential` in `wal.rs` (already correctly gated — memmap2 itself
  is portable), and `cfg(unix)`-gated tests. This is NOT optional for a
  minimal port: M2 activation selects one-voter Hiqlite on every node
  (`docs/CLUSTERING-PLAN.md`), so hiqlite must build and run on Windows even
  for a standalone server. → D9
- **Install/launch** — `deploy/` has systemd, launchd, Docker, Unraid; no
  Windows lane. `ci.yml` builds `x86_64/aarch64-unknown-linux-gnu` only.
  → D10–D12

Hardware acceleration is its own story (§4 D8) because most of it is *not*
Unix-specific: encoder families live in `plurx-core/src/transcode/encoder.rs`
as ffmpeg argument builders, detection is `ffmpeg -encoders` at startup, and
admission is gated by the boot probes (`plurxd/src/pipeprobe.rs`,
`plurxd/src/ffmpeg.rs`) — all of which run anywhere ffmpeg runs.

## 4. Decisions

Each decision below is the contract for its milestone. The recommendation is
chosen; alternatives are recorded so the executing agent doesn't relitigate
them, and the one open sign-off is marked.

### D1 — fs_secure gets a Windows backend behind the same API

Keep the public API exactly as it is (`open_read_nofollow_blocking`,
`open_directory_nofollow_blocking`, `SecureDirectory`, `FileIdentity`,
`claim_and_clear_owned_scratch_blocking`, `anonymous_memory_file`, …) and add
a `cfg(windows)` implementation in a sibling module. Do not adopt `cap-std`:
the API here is bespoke (identity checks, ancestry-alias detection, scratch
claiming) and wrapping a second abstraction under it buys nothing but a
dependency. The mappings:

| Unix primitive | Windows implementation |
|---|---|
| component-wise `openat(O_NOFOLLOW\|O_CLOEXEC)` | `NtCreateFile` with `OBJECT_ATTRIBUTES.RootDirectory` = parent handle, one component at a time, `FILE_OPEN_REPARSE_POINT`; if the opened object has `FILE_ATTRIBUTE_REPARSE_POINT`, close and refuse (symlinks *and* junctions *and* mount points — junctions are the Windows symlink-swap) |
| `O_DIRECTORY` | `FILE_DIRECTORY_FILE` create option |
| `FileIdentity` (st_dev, st_ino) | `GetFileInformationByHandleEx(FileIdInfo)` → 64-bit volume serial + 128-bit file ID; `same_inode` compares both |
| `fstatat(AT_SYMLINK_NOFOLLOW)` | open child as above, then `FileBasicInfo`/`FileStandardInfo` on the handle |
| `unlinkat`, `AT_REMOVEDIR` | open child, `SetFileInformationByHandle(FileDispositionInfoEx)` with `FILE_DISPOSITION_POSIX_SEMANTICS \| FILE_DISPOSITION_IGNORE_READONLY_ATTRIBUTE` |
| `renameat` | `SetFileInformationByHandle(FileRenameInfoEx)` with `FILE_RENAME_POSIX_SEMANTICS \| FILE_RENAME_REPLACE_IF_EXISTS`, `RootDirectory` = destination parent handle |
| `closedir`/`readdir` on a dup'd cursor | `GetFileInformationByHandleEx(FileFullDirectoryInfo)` enumeration on a re-opened `.` (open `.` relative to the held handle, same reasoning as `independent_directory_stream`) |
| `geteuid()` == owner uid | `GetSecurityInfo(OWNER_SECURITY_INFORMATION)` owner SID `EqualSid` current token user SID |
| `memfd` + `F_ADD_SEALS` (`anonymous_memory_file`, `seal_anonymous_memory_file`) | temp file in owned scratch with `FILE_ATTRIBUTE_TEMPORARY \| FILE_FLAG_DELETE_ON_CLOSE`; sealing becomes a no-op (documented: seals defend against later writes through *other* descriptors to the same anonymous file — on Windows the file is unnamed-equivalent and plurxd holds the only handle) |

POSIX-semantics delete/rename need NTFS (or ReFS) and Windows 10 1809+ —
that is why §1 pins the floor. The **data and cache directories must be
NTFS**; refuse at boot with a clear error otherwise (a `statvfs`-style probe
at startup, D6, can carry this check). Media *libraries* may live on SMB/
exFAT — they are opened read-only and never take the delete/rename paths.

Use `windows-sys` for the bindings (add as a `[target.'cfg(windows)'.
dependencies]` entry). Every `unsafe` block gets the same one-line SAFETY
comment discipline the Unix side already has.

### D2 — the ffmpeg I/O handoff ⚠ needs Paul's sign-off before M2

Today (Linux): plurxd holds validated descriptors and, in `pre_exec`, dups
them to fixed child fds — source → 3 (`/dev/fd/3`), output directory → 4
(the muxer writes `/dev/fd/4/<part>`), burned-subtitle file → 5
(`/dev/fd/5`). ffmpeg never sees a pathname an attacker could swap; the held
descriptor *is* the authority. macOS already deviates (fdesc can't resolve
`/dev/fd/4/child`, so the child's cwd is anchored to descriptor 4 and the
muxer uses a relative path) — proof the handoff is per-OS routed already.

Windows has no `/dev/fd` and no fd inheritance a stock ffmpeg build can
address beyond stdio. Options considered:

1. **CRT `lpReserved2` fd-passing** — Windows' MSVCRT convention can hand a
   child a full fd table at spawn, and ffmpeg's `pipe:N` would then work,
   seeks included. Rejected for M2: it depends on which CRT the ffmpeg build
   links, it is undocumented enough to break across builds, and no shipping
   media server does it. Recorded as a possible later hardening.
2. **plurxd pumps bytes over stdin/named pipes** — breaks seekable input;
   MKV/MP4 demuxing needs seeks. Rejected.
3. **Path handoff + held-handle identity re-verify (chosen).** plurxd keeps
   the validated handle; immediately before spawn it re-resolves the
   canonical path and confirms `FileIdentity` matches the held handle,
   then passes the real path to ffmpeg. Output: plurxd creates the part
   directory itself under owned scratch (it already does) and anchors the
   child's working directory to it, muxer on relative paths — the same shape
   as the macOS branch. Subtitles: written to owned scratch, passed by path,
   removed after reap (a delete-on-close handle can't be reopened by ffmpeg,
   which doesn't request `FILE_SHARE_DELETE`).

The honest cost of option 3: between plurxd's re-verify and ffmpeg's own
`CreateFile` there is a window in which a swap redirects ffmpeg — the exact
TOCTOU `fs_secure.rs` closes on Unix. The mitigations are that scratch and
cache directories are created by plurxd with a restrictive DACL (owner +
SYSTEM only, no inheritance), so only an attacker already running as the
service account can race them, and media sources are read-only inputs whose
worst-case swap is "ffmpeg transcodes the wrong file the attacker could
already read." This is a *documented platform difference*, and it goes in
`SECURITY.md` in the same PR that implements it — which is why this decision
needs Paul's explicit OK before M2 begins. If the answer is no, option 1 is
the fallback and M2 grows by the CRT-spelunking.

```
 Linux                                Windows (option 3)
 ┌─────────┐ dup2→3  ┌────────┐       ┌─────────┐ verify id,   ┌────────┐
 │ plurxd  │────────▶│ ffmpeg │       │ plurxd  │ pass path ──▶│ ffmpeg │
 │ held fd │ /dev/fd │  opens │       │ held    │              │ opens  │
 │ = truth │         │  fd 3  │       │ handle  │  DACL'd      │ path   │
 └─────────┘         └────────┘       └─────────┘  scratch     └────────┘
   no pathname exists                   pathname exists; race window
                                        bounded by directory ACLs
```

### D3 — pause/resume via NtSuspendProcess

`apply_ahead_window` (transcode.rs ~12014) SIGSTOPs a session that is far
enough ahead and SIGCONTs it on release; `prodrun.rs` does the same for
producers. The semantics that must survive: suspension is *process-wide and
immediate*, and on resume `session.progress.touch()` runs before the
suspended flag flips so the stall watchdog never reads "running" beside a
motion clock spanning the suspension (the comment at transcode.rs:462 is the
scar). Windows: `NtSuspendProcess`/`NtResumeProcess` from ntdll — formally
undocumented, stable since XP, what Process Explorer uses. Declare the two
externs; do not thread-enumerate (`SuspendThread` loops race ffmpeg's thread
creation). ESRCH-equivalent (`STATUS_INVALID_HANDLE` after exit) maps to the
same "already reaped" tolerance the Unix code has.

### D4 — kill, reap, and the orphan gap

`tokio::process::Child::kill()` already works on Windows (TerminateProcess).
Add what Unix never needed spelled out: assign every ffmpeg child to a Job
Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` (create suspended → assign
→ resume) so a plurxd crash cannot strand transcoders holding cache files
open — on Unix a dead parent's children die with the session or get swept;
on Windows an orphan ffmpeg with an open segment blocks every delete under
D5. The job handle lives beside the child in the session.

### D5 — delete-while-open becomes skip-and-retry

Rust's std opens files with `FILE_SHARE_READ|WRITE|DELETE`, so plurxd's own
handles never block a delete. ffmpeg's CRT opens share read/write but NOT
delete: any sweep that unlinks a file ffmpeg still holds —
`cachekeep.rs`'s crash-leftover and budget sweeps, `fs_secure`'s
`clear_scratch_blocking` — gets `ERROR_SHARING_VIOLATION`. Map exactly that
error to "in use: skip this entry, it stays in the sweep set for next pass"
rather than failing the sweep; every other error stays loud. This is a
behavioral difference worth a comment at each site: on Unix "unlink then let
the fd drain" is one operation, on Windows it is a retry loop that converges
when the child exits.

### D6 — the one-liners

`statvfs` → `GetDiskFreeSpaceExW` (same free/total shape membership.rs
wants, plus the NTFS check from D1). `gethostname` → `GetComputerNameExW
(ComputerNameDnsHostname)`. `getrusage(RUSAGE_SELF)` →
`GetProcessMemoryInfo` + `GetProcessTimes`. `posix_fadvise(DONTNEED)` →
no-op with a comment (Windows has no equivalent hint worth faking;
`FILE_FLAG_NO_BUFFERING` changes I/O contracts and is not it).
`renameatx_np` is already inside a macOS branch — untouched.

### D7 — shutdown and the service boundary

main.rs's `SignalKind::terminate/interrupt` block becomes `cfg(unix)`; the
Windows arm listens on `tokio::signal::ctrl_c()` (console mode) plus a
`windows-service` control handler mapping `SERVICE_CONTROL_STOP`/`SHUTDOWN`
into the same shutdown notify the signals feed. One graceful-shutdown path,
three doorbells. `plurxd run` stays byte-identical in behavior across
platforms; service mode is new subcommands: `plurxd service install`,
`plurxd service uninstall`, and the SCM entry point. Service defaults that
differ from console: `data_dir` falls back to `%ProgramData%\plurx\data`
when the config leaves it at the relative `"./data"` default (a service's
cwd is System32 — a relative default there is a footgun, refuse it loudly
and say what to set).

### D8 — hardware acceleration: two families ride, one dies, one is born

The families (`encoder.rs`) and their Windows fate:

| Family | Encoder | Decode side (transcode/mod.rs ~839) | On Windows |
|---|---|---|---|
| Software | `libx264`/`libx265` | — | works as-is |
| Nvenc | `h264_nvenc` | `-hwaccel cuda` | same names, same flags — expected to work unchanged; must still pass boot probes |
| Qsv | `h264_qsv`/`hevc_qsv` | `-hwaccel qsv` | `-init_hw_device qsv=hw` derives from D3D11 on Windows automatically (jellyfin-ffmpeg ≥6); flags unchanged, but this is an *unmeasured claim* until M4 runs it on hardware |
| Vaapi | `h264_vaapi` | `-hwaccel vaapi` | never detected — Windows ffmpeg builds don't ship VAAPI encoders, so startup detection (`ffmpeg -encoders`) skips the family naturally; ensure `vaapi_device()`/`PLURX_VAAPI_DEVICE` is only read when the family is selected (it is today — keep it that way) |
| VideoToolbox | — | — | macOS-only, irrelevant |
| **Amf (new, M5)** | `h264_amf` | D3D11 decode | AMD's only Windows path; a full new family — rate control, `forced_idr` spelling research, measured admission |

The port's real safety net is that admission is already probed, not
declared: startup detection, the forced-IDR probe (the difference between
2-second and 10.4-second segments — encoder.rs's `forced_idr_flag` comment),
and the four-way tone-map gate in `pipeprobe.rs` (runs, tags BT.709,
matches the CPU reference picture, beats it on speed) all execute per node.
A Windows driver that can't reproduce the claim falls back to software —
the same rule as everywhere. **No family is enabled on Windows by editing a
table; it is enabled by running the probes on real hardware and recording
the measurement in the PR**, exactly like `hevc_qsv`'s nynuc numbers.
Recommend `PLURX_FFMPEG` pointed at jellyfin-ffmpeg's Windows build in all
docs; stock gyan/BtbN builds carry NVENC/QSV/AMF too but not the tonemapx
filter the HDR chain uses.

### D9 — vendored hiqlite: port the six sites

`PermissionsExt` chmods → on Windows, skip with a comment (the data dir DACL
from D7 covers it; per-file POSIX modes have no faithful equivalent and
0o600-via-DACL on every WAL segment is noise). `MetadataExt` in backup →
`FileIdInfo` as in D1. The mmap advice sites are already `cfg(unix)`. Gate
the Unix-only tests. Keep the diff minimal and carry it as a vendor patch —
these files are a fork already.

### D10 — packaging: zip + service first, MSI later

M3 ships: a `plurxd-windows-x86_64.zip` from the release matrix (binary +
`plurx.example.toml` + a README pointing at OPERATIONS), `plurxd service
install` as the launch story, and documented `netsh advfirewall` rules for
TCP 32400 and UDP 32414 (GDM discovery fails silently without the UDP rule —
say so where the rule is documented). winget/MSI/chocolatey are explicitly
deferred until the zip path has survived real users; an installer that
automates a broken flow just breaks faster.

### D11 — paths, config, and the scanner

Media filenames are arbitrary user data: the scanner must tolerate reserved
names (`con.mkv`), trailing dots/spaces, and >260-char paths. Embed the
`longPathAware` application manifest at link time and keep every open going
through std (which handles `\\?\` when the manifest is present). The NFO
reader already case-insensitively matches `.NFO` "as Windows-authored
libraries are full of" them (scan/nfo.rs:197) — the codebase has been
reading Windows-made libraries for a while; now it just reads them locally.
Cache/scratch names are hashes and part numbers — already safe.

### D12 — CI: a Windows lane that gates

Extend `ci.yml`: one `windows-2025` job (GitHub-hosted regardless of
`CI_RUNNER_MODE` until the lab has a Windows runner) running `cargo build
--workspace --exclude plurx-cluster-check --target x86_64-pc-windows-msvc`
plus the unit lane with Unix-only tests cfg-gated. Add
`x86_64-pc-windows-msvc` to the release-build matrix (ci.yml ~601). A
`.github/actions/ffmpeg` Windows variant installs the pinned jellyfin-ffmpeg
build for the M2 smoke job. `plurx-cluster-check` stays excluded on Windows
(§6).

## 5. Milestones

### M1 — it compiles, and CI proves it forever

The `cfg` split: D1's Windows backend module (compiling, unit-tested where
testable without media), D3/D4/D5/D6/D7 stubs or implementations behind the
existing call sites, D9 vendor patches, D12's build job. No behavior change
on Unix — the Linux/macOS object code paths must be untouched by inspection
of the diff.

**Acceptance:** the new CI job passes `cargo build --workspace --exclude
plurx-cluster-check --target x86_64-pc-windows-msvc`; `make unit` and `make
validate-staged` stay green on Linux; `make check` green at the PR gate.

### M2 — it boots, scans, and plays (software) ⚠ blocked on D2 sign-off

fs_secure Windows backend live end-to-end; D2 option 3 handoff; suspend/
resume (D3); job objects (D4); sharing-violation sweeps (D5); one-voter
Hiqlite activation on Windows (D9); console-mode shutdown (D7's ctrl_c arm).
SECURITY.md gains the Windows platform-differences section in this PR.

**Acceptance:** on a Windows runner with pinned ffmpeg: `plurxd run` against
a fixture library → create admin, add library, scan completes, direct-play
request streams, an HLS software-transcode session starts, suspends when
far enough ahead (log line `suspending transcode: far enough ahead`),
resumes, and a ctrl-C shuts down cleanly with no leftover ffmpeg (job-object
check). Scripted, in CI, as a new smoke job.

### M3 — it installs and launches like it belongs there

Service subcommands + SCM handler (D7), `%ProgramData%` defaults, the zip
artifact in the release matrix (D10, D12), firewall documentation, and the
docs: `deploy/README.md` gains the Windows section, OPERATIONS the env-var
and path notes, README's platform list updated.

**Acceptance:** on a clean Windows VM: unzip → `plurxd service install` →
service starts, survives a reboot, serves on 32400, GDM answers on 32414
with the firewall rules applied; `plurxd service uninstall` leaves nothing
running. Recorded as a transcript in the PR (no CI reboot lane — this one is
a documented manual check).

### M4 — hardware transcode, measured

NVENC and QSV on real Windows hardware: boot probes green, forced-IDR probe
confirms 2-second segments (not 10.4), tone-map gate selects or refuses the
hw graph honestly, and the numbers go in the PR the way hevc_qsv's nynuc
numbers did. Needs a Windows box with an NVIDIA card and one with an Intel
iGPU — see §8.

**Acceptance:** settings page hardware pills show the family active on the
test box; `pipeprobe` results and encode-speed measurements recorded in the
PR; a played session probes as 2-second segments.

### M5 — AMF for AMD (optional, separately approved)

New encoder family end-to-end: flags, rate control, forced-IDR spelling,
detection, probes, measured admission. Scoped and priced only after M4
teaches us what Windows hardware validation actually costs.

**Acceptance:** same shape as M4, on AMD hardware.

## 6. Non-goals — guardrails, each with its reason

- **No cluster voters on Windows.** hiqlite must *build* (single-voter
  activation needs it) but multi-node membership is unvalidated there:
  `plurx-cluster-check` — the only thing that proves cluster behavior — is
  built on SIGSTOP fault injection and stays Linux-only. Joining a cluster
  from a Windows node refuses with a clear error naming this document.
- **No MediaFoundation/DirectShow pipeline.** ffmpeg remains the only media
  engine; a second engine doubles every probe and every codec bug.
- **No ARM64 Windows, no 32-bit.** Nobody has the hardware to validate them;
  an unvalidated target is a standing lie in the README.
- **No weakening of the Unix fs_secure paths** — the Windows backend is an
  addition; if sharing code would change Unix behavior, don't share it.
- **No refactor of the fs_secure or encoder APIs** while porting. Same
  functions, new backend. API changes are their own PRs with their own
  review.
- **No installer automation (MSI/winget) in this plan** — zip + service
  install first (D10).
- **Don't touch the mobile build-claim surfaces** (project.yml, client
  READMEs, STATUS.html build tiles) — nothing here changes a client build.

## 7. Validation additions

Unix-only tests get `cfg(unix)` gates in the same commit that makes their
subject conditional — a test that can't compile on Windows is M1's problem,
not M2's. New Windows-specific tests mirror the scars this plan inherits:
identity re-verify catches a swapped file (D2), a sharing violation marks an
entry in-use instead of failing a sweep (D5), suspend/resume keeps the
watchdog honest across the gap (D3), reparse points are refused in a
component walk (D1 — the junction case is the one Unix tests can't cover).
`docs/VALIDATION.md` gains the Windows lane description; the pinned ffmpeg
version for Windows runners lives with the existing pins in
`.github/actions/ffmpeg`.

## 8. Open questions for Paul

1. **D2 sign-off** — accept path-handoff + identity re-verify + DACL'd
   scratch as the documented Windows difference, or require the CRT
   fd-passing spike first? M2 cannot start without this answer.
2. **Hardware for M4** — which Windows box(es) with NVIDIA/Intel graphics
   can run the probes? (An existing lab machine dual-booting counts; the
   measurements are per-node anyway.)
3. **Does M5 (AMF/AMD) get approved now or after M4's cost is known?**
4. **Lab Windows runner or GitHub-hosted** for the permanent CI lane —
   GitHub-hosted is the default in D12; a lab runner would match the
   `CI_RUNNER_MODE` pattern and speed up the smoke job.
