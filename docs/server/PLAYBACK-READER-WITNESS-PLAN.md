# Playback reads keep a valid recovery witness

**Status:** proposed · revised after Fable review · plan only, nothing built ·
**Updated:** 2026-11-12 · **Source baseline:** `origin/main` at `54aebebf9`;
in-flight `effort/playback-start-phase2` at `50fa67f41`; census stopgap
PR #1561 (`claude/playback-census-recovery`)

On 2026-11-12 three playback ("foreground") source owners on media1 stayed
`custody_held=1` after the media1 daemon restarted while each stream played.
No storage domains are configured, so every analysis claim needs the
cluster-wide `source_io` slot, and those three rows held it. Each owner's v2
reader witness (`<data_dir>/source-custody-recovery/v2/foreground-<owner_id>.{json,anchor,armed}`)
had a poisoned anchor (first byte rewritten to `!`). A dead owner with a
poisoned witness cannot settle through the ordinary replay. The only way
out is the root-run host census, which PR #1561 extends to foreground
owners. That stopgap needs a person to run a root helper after every
unclean restart during playback.

This plan is the root-cause fix: a production child launch poisons a
witness only if it could keep reading media after the daemon dies. After it,
a restart during playback settles itself through the existing v2 path: the
kernel lock is released when the daemon dies, the kernel is the same, and
the anchor is untouched.

Companion: [source custody recovery](SOURCE-CUSTODY-RECOVERY.md) §1.1–1.3,
[job queue RCA](JOB-QUEUE-ROOT-CAUSE-AND-FIX.md),
[storage domains deferred apply](STORAGE-DOMAINS-DEFERRED-APPLY-PLAN.md).

## 1. Why every playback witness is poisoned

| Fact | Where |
|---|---|
| Every production spawn goes through `spawn_job_owned` / `output_job_owned*`, which call `source_lifetime::before_spawn(descriptor_handoff)` | `crates/plurx-core/src/process/mod.rs` ≈86, ≈223; enforced by `process::census::every_production_spawn_goes_through_the_launcher` |
| `before_spawn` reports mediated only inside a caller-declared task-local scope (`mediated` / `mediated_spawn`) and only without a tagged descriptor handoff | `crates/plurx-core/src/process/source_lifetime.rs` |
| The observer poisons **every** armed witness for any launch that is not mediated, whichever owner it belongs to | `crates/plurxd/src/source_custody/recovery/reader.rs` `observe_children` ≈441, `Witness::observe_launch` ≈68 |
| A declared raw child also poisons the anchor; only the in-memory live ledger proves its join | `observe_children` comment; SOURCE-CUSTODY-RECOVERY §1.2a |
| Any inherited descriptor through the producer builder is tagged a source handoff, including output pipes and the session directory | `crates/plurxd/src/ffmpeg.rs::inherit_raw_file_descriptors_with_cwd` ≈111 |
| Several sites install descriptors in their own `pre_exec` without tagging, so `before_spawn` never sees them | `dv_disk.rs` ≈1581, ≈5820 (fd 198, `/proc/self/fd/198`); `fragindex.rs` ≈2795; `ffmpeg/output_pipe.rs` ≈95; `source_probe/fel_qualification.rs` ≈390; `state.rs` ≈3590; `subtitle_ride_along.rs` ≈1153; `subtitle_translation/runtime.rs` ≈111; `decode_facts.rs` ≈3175 |
| Only six call sites declare mediation | `producer_spawn::brokered_cache_part`, `ffmpeg.rs` ≈1034, `source_probe.rs` ≈327, `dv_title.rs` ≈59, `fel_qualification.rs` (×4) |

So the rolling playback producer poisons every witness even though, with a
replicated Store, it already reads through the broker:
`rolling/session.rs::spawn_and_install_prepublication_child` (≈2123) opens
`SourceInput::open(request)` and passes its URL, but the spawn is not in a
mediated scope. So do capability probes, Live TV, the DV mux stage and other
children that never touch media.

The witness rule is right. A child holding an original path or descriptor
can outlive the daemon and keep reading, and the anchor lock cannot see
that. The defect is that the daemon declares almost nothing about what its
children can reach. And today's `mediated` scope is trusted without any
check, so even the declared sites are unverified (and some hand descriptors
through `pre_exec` that the observer cannot see).

## 2. Launch-site census

Labels are the `ChildWork` purposes. "Brokered today" means the argv already
carries a `SourceInput` URL on a replicated Store, where `source_identity`
is configured (`state.rs` ≈1297).

### 2.1 Playback and playback-start path

| # | Launch (purpose) | Site | Input today | Custody | Plan |
|---|---|---|---|---|---|
| P1 | playback transcode | `transcode/manager/start.rs` ≈1666 via `rolling/session.rs` ≈2123 | broker URL; raw path + fd 3 only without a request; subtitle fd 5 when burning | yes | declare `Brokered`; fd 5 classified (§4.3, Q1) |
| P2 | playback transcode / copy HLS (pipe child) | `start.rs` ≈2407, ≈2461; `transcode/producer/prepublication_executor.rs` ≈229, ≈404 | as P1 | yes | as P1 |
| P3 | Source / Analysis observed copy | `copyrange/pipes.rs` ≈238 | broker URL, one `-i` enforced; copy-measurement pipes fds 6, 7 (`producer_spawn.rs` ≈66–76) | yes | declare; pipes classify as `Pipe` |
| P4 | playback remux | `http/stream.rs` ≈4356 | broker URL | yes | declare |
| P5 | VOD transcode | `vod/generation.rs` ≈675 | broker (`open_companion`) when custody, else fd 3; audio fd 4; subtitle fd 5 | when companion | declare; classify fds (Q1, Q2) |
| P6 | VOD init segment | `vod/init.rs` ≈64 | **raw fd 3**, audio fd, subtitle fd | session's | `open_foreground_lent` (effort) |
| P7 | VOD landing probe | `vod/generation.rs` ≈1223 | verify | — | lent, probe policy |
| P8 | DV segment / streaming reconstruction (renderer) | `dv_segment.rs` ≈854, ≈1274 | **raw fd 3** (`renderer_args` → `fd(3)`) + output dir fd | session's | broker URL as `args[0]`, as `fel_qualification.rs` ≈590 already does |
| P9 | DV timestamped NUT mux | `dv_segment.rs` ≈898 | temp files and a pipe | — | `NoSource` |
| P10 | DV segment / streaming encoder | `dv_segment.rs` ≈935, ≈1314 | broker when `source_input`, else fd 3 | session's | always brokered |
| P11 | chapter list probe | `http/stream.rs` ≈2132 | **raw path** to ffprobe, no custody | none | lent from the session's custody; without a session, its own short `SourceInput::open` (off the start path) |
| P12 | playback start media probe | `transcode/media_origin.rs` ≈58 | **raw path**, keyframe seeks, no custody | none | lent, probe policy |
| P13 | subtitle window for the playhead | `subtitles.rs` ≈3402, ≈3447, ≈3525 | broker via `open_current_held` when a request is current, else **raw path** | when current | lent, small window |
| P14 | subtitle track a viewer turned on | `http/stream.rs` ≈2917 | verify (whole-track extraction) | verify | lent, sequential |
| P15 | text subtitle for a session start | `start.rs` ≈993 | verify | session's | lent, sequential |
| P16 | held source / decode-fact probes for a session start | `transcode.rs` ≈189; `transcode/planning.rs` ≈30 → `decode_facts.rs` ≈5536 | broker via `open_current_held`; `decode_facts.rs` ≈3175 installs a `pre_exec` descriptor | yes | move the descriptor into the builder; declare |
| P17 | HEVC parameter-set census; PGS subtitle demux | `hevc_census.rs` ≈227; `pgs_overlay.rs` ≈735 | verify | verify | broker |
| P18 | DV completed output header probe | `dv_runtime.rs` ≈612 | output file | — | `NoSource` |
| P19 | Live TV stream and probes | `live_tv.rs` ≈7130, ≈8320, ≈10254; `caption_probe.rs` | network tuner URL | — | `NoSource` with the declared tuner scheme (§4.2); otherwise `Unknown`, which poisons — see Q4 |

### 2.2 Everything else that runs while playback custody is armed

| Class | Launches |
|---|---|
| No media → `NoSource` | ffmpeg version, build identity, engine / DV / VA-API capability probes, media support inventory, media processing smoke, GPU inventory, tone-map pipeline probe, video processing / Mac processing compatibility probes, read-rate burst probe, private sharing host setup, whisper.cpp (extracted audio), progressive subtitle translation (text) |
| Directory metadata → `MetadataOnly` | library root reachability probe (`media_pool.rs` ≈1854: `find -H <root>/. -mindepth 1 -maxdepth 1 -print -quit`) |
| Reads media, mediated today | fragment index probes, complete-title DV scan, FEL qualification, brokered cache parts — re-verified under §4.2 |
| Reads media raw → broker | chapter thumbnail (`http/chapter_thumbs.rs` ≈402), subtitle source extraction (`state.rs` ≈3623, ≈10241), transcription audio, subtitle translation input, content-aware sample encode / VMAF, DV conversion and its source probes (`dv_disk.rs`), artwork derivative with input under a library root (`http/images.rs` ≈1023), offline transcode (`http/offline.rs` ≈1579), transcode produce (`transcode/manager/produce.rs` ≈1693), PGS ride-along probe |

Rows marked "verify" need their argv and descriptors read before they
change. The census test (§8.1) makes the list complete by construction.

## 3. The options for a child that reads raw

### 3.1 Declared raw retirement joined in memory — does not survive the daemon

The live completion ledger (§1.2a) counts declared raw children and settles
once each exit is observed. That proof is in daemon memory. If the daemon
dies first nobody observed the exit, and the anchor was poisoned at launch.

### 3.2 PID-namespace witness — rejected

The idea: if the daemon is PID 1 of its PID namespace, the kernel kills every
member when it dies; record the namespace at activation and let the next
daemon prove it gone.

Within one container this is a real guarantee: `zap_pid_ns_processes` keeps
the namespace's init from finishing its exit until every member is gone, so
a same-container restart cannot begin while an old reader lives. It is
rejected for two decisive reasons:

- **Native macOS and Linux have no such guarantee.** The daemon is not PID 1;
  orphans reparent to launchd or systemd and keep running. The v2 protocol
  must work natively (SOURCE-CUSTODY-RECOVERY §1.1).
- **A fresh container cannot verify that the old namespace emptied.** It
  sees only its own `/proc/self/ns/pid`; without the host `/proc` it cannot
  enumerate other namespaces, and nsfs inode numbers are recycled. A host
  `/proc` bind would turn it back into a host census.

`PR_SET_PDEATHSIG` (`priority.rs` ≈165) covers only direct children on Linux
and is no witness either.

### 3.3 Inherited lifetime lock — survives the daemon

`flock` locks belong to the open file description, are inherited across
`fork`/`exec` by every descriptor that refers to it, and are released only
when the last such descriptor closes (Linux and macOS alike). So:

- each witness gets a `.lifetime` sidecar (empty, mode 0600), opened
  **read-only** by the daemon and held with `LOCK_SH`. It must pass the same
  `local_filesystem` check as the anchor (`reader.rs` ≈132–150);
- a declared raw child of that owner inherits a duplicate of that
  description at fd 9 and holds the shared lock for its whole life, as does
  every descendant that keeps it;
- recovery opens a new description and takes `LOCK_EX | LOCK_NB`. It
  succeeds only when no process — daemon, child or grandchild — still holds
  the description.

A child blocked in uninterruptible NFS I/O keeps its descriptors, so it
keeps the lock until the read returns. That is exactly why this works where
the PID-namespace idea does not. Its limits, stated plainly:

- any holder of the shared description can `flock(LOCK_UN)` or convert the
  lock, or close fd 9. The child cannot touch the anchor (it has no
  descriptor to it), but it *can* release the lifetime lock. Safety rests on
  the audited programs not doing so; §8.2 tests that against the deployed
  tool builds, not a stub;
- the child's lifetime is charged to owner *o*. A declared-raw launch whose
  path check fails must never fall to a class that skips the lock (§4.2).

### 3.4 Recommendation

1. **Declare every launch's source access and verify it at spawn against an
   allowlist** (§4). This alone stops no-media and already-brokered
   launches from poisoning witnesses.
2. **Read media through the broker everywhere** (§2 "broker" rows). After
   the daemon dies an orphan holds only a URL whose loopback listener is
   gone; its capability path is random, so a later daemon on the same port
   answers 404. This is the design SOURCE-CUSTODY-RECOVERY §1.1 already names.
3. **Use the inherited lifetime lock only for a child that cannot be
   brokered** (phase B3, only if B2 leaves one).
4. **Keep "unknown launch poisons every witness"** as the conservative
   fallback, and make it attributable (§9).

## 4. Design

### 4.1 Source access classes

In `crates/plurx-core/src/process/source_lifetime.rs`, replace the boolean
task-local with:

```rust
#[derive(Clone, Debug)]
pub enum SourceAccess {
    Unknown,                         // default: no declaration in scope
    NoSource,                        // reaches no media
    MetadataOnly,                    // directory metadata of a library root, no file contents
    Brokered,                        // media only as live loopback broker URLs
    DeclaredRaw(Arc<OwnerLifetime>), // raw media of one owner; always inherits its lifetime lock
}
tokio::task_local! { static ACCESS: SourceAccess; }
pub async fn no_source<T>(work: impl Future<Output = T>) -> T;
pub fn no_source_spawn<T>(spawn: impl FnOnce() -> T) -> T;
pub async fn metadata_only<T>(work: impl Future<Output = T>) -> T;
pub async fn brokered<T>(work: impl Future<Output = T>) -> T;   // `mediated` stays as an alias
pub fn brokered_spawn<T>(spawn: impl FnOnce() -> T) -> T;       // `mediated_spawn` alias
pub async fn declared_raw<T>(owner: Arc<OwnerLifetime>, work: impl Future<Output = T>) -> T;
```

Detached tasks inherit nothing and stay `Unknown`, as today.

### 4.2 Verification at spawn — an allowlist that fails closed

`before_spawn` becomes `before_spawn(command: &std::process::Command, inherited: &[InheritedFd], purpose: &'static str)`.
`InheritedFd` comes from the one descriptor builder (§4.3); the census test
forbids any other way of handing a descriptor to a child. The observer
receives `LaunchObservation { declared, verified, purpose }`
(`type Observer = fn(&LaunchObservation) -> io::Result<()>`).

**Daemon-owned roots.** `source_lifetime::set_daemon_roots` is called once at
start with the canonical data directory, transcode/cache directories,
runtime cache, scratch directories and the tool binary directories. These
are node-local and fixed for the process lifetime, so no replicated library
list is involved and nothing can go stale across nodes.

A launch verifies as **`NoSource`** or **`Brokered`** only if all of these
hold. Anything else is `Unknown`.

| Check | Rule |
|---|---|
| Program | canonical path under a tool binary directory |
| Child cwd | set explicitly, or inherited from the daemon, and resolves under a daemon-owned root; a directory descriptor used as cwd (`producer_spawn.rs` `output_is_cwd`, `ffmpeg.rs` ≈146 `fchdir`) must be a builder-registered daemon-owned directory |
| Path-like tokens | every token that is absolute, starts with `./` or `../`, or contains `/` and is not a URL — including fragments of option values split on `=`, `:`, `,` and quotes (filter graphs such as `subtitles=filename=…` or `movie=`) — resolves (canonicalised against the child cwd, so symlinks and mount aliases are followed) under a daemon-owned root |
| Script and list inputs | `-filter_script`, `-filter_complex_script`, `-f concat`, `.ffconcat`, `.m3u8`, `.txt` inputs must be files the daemon wrote and registered; otherwise `Unknown` |
| URLs | every `http://` / `https://` token on loopback is a live broker capability (`source_lifetime::register_broker` / `unregister_broker`, called by `SourceInput::launch` and its close paths); non-loopback URLs are allowed only for `NoSource` when the declaring site names the scheme (Live TV, Q4) |
| Descriptor tokens | every `/proc/self/fd/N`, `/dev/fd/N` or `pipe:N` names a builder-registered descriptor whose kind is not `Source` |
| Descriptors | every inherited descriptor, stdin included, is builder-registered with a non-`Source` kind (§4.3) |

**`MetadataOnly`** verifies only for program `find` with exactly the argv
shape `-H <dir>/. -mindepth 1 -maxdepth 1 -print -quit`, no descriptors and
null stdio. It reads no file contents whatever the directory, so no root
list is needed.

**`DeclaredRaw(o)`** always installs `o`'s lifetime descriptor, whether or
not the rest verifies. It verifies when every path-like token outside
daemon-owned roots, and every `Source` descriptor, is `o`'s source (same
`dev`/`ino` as `o`'s held source fence). A failure is reported as
`Unknown` (every other witness is poisoned too) but keeps the lifetime lock.

A failed verification logs `error!` with the purpose and the failing check
(never the path), increments `plurx_source_launch_total{access="unverified"}`,
and panics in debug and test builds, so a wrong declaration fails CI rather
than production recovery. Disc folders, extensionless inputs and cwd-relative
media paths are simply not under a daemon-owned root and fail closed.

### 4.3 Descriptor classification is mechanical

`producer_spawn::Descriptors` becomes the only descriptor builder in the
daemon. `FfmpegDescriptors` and every site in §1's untagged `pre_exec` row
move to it (`dv_disk.rs` fd 198 included). At spawn the builder `fstat`s
each descriptor it will install, stdin included:

| `fstat` result | Kind |
|---|---|
| FIFO or socket | `Pipe` |
| regular file or directory whose `(dev, ino)` the builder registered when it created or opened it under a daemon-owned root (`tempfile_in`, cache output, session directory) | `Output` / `Auxiliary` |
| `o`'s lifetime file | `Lifetime` |
| anything else — including a regular file on the data device that the builder did not create | `Source` |

Callers no longer assert kinds. A `Source` descriptor tags the handoff, as
today. A subtitle or audio handle is non-`Source` only if the daemon itself
wrote it (an extracted sidecar in the cache); a sidecar next to the media is
`Source`, so it is brokered or the launch is `Unknown`.

### 4.4 Witness changes

`reader.rs`, after PR #1561 (which prefixes witness and sidecar names with a
`CensusFamily` and adds a foreground `.source` sidecar written at arming):

- **B0 `.poisoned`:** `observe_launch` writes `<family-prefixed name>.poisoned`
  (0600, ≤128 bytes: the first poisoning purpose and boot tick) before
  invalidating the anchor.
- **B3 `.lifetime`:** `Witness` gains `lifetime: std::fs::File` (read-only,
  `LOCK_SH`), created in `prepare_at` (≈160) under the directory lock.
  Evidence moves to `version: 3`, `reader_model: "daemon_mediated_v2"`, with
  `lifetime_device` and `lifetime_inode`. Older binaries skip version 3, so a
  downgrade leaves the owner held, never released. `replay_at` (≈507) also
  requires the lifetime file (matching device/inode, `nlink == 1`,
  `local_filesystem`) and a non-blocking exclusive lock on it, held through
  settlement like the anchor. `observe_children` does not poison any witness
  for a verified `DeclaredRaw(o)` launch.
- **Directory bounds:** an owner now has up to seven entries (`.json`,
  `.anchor`, `.armed`, `.source`, `.unmediated`, `.poisoned`, `.lifetime`).
  Raise `MAX_ENTRIES` (≈471) from `4 × MAX_FILES` to `8 × MAX_FILES` and the
  per-owner reservation (≈500) from 3 to the full sidecar count, and add
  `poisoned` and `lifetime` to `complete_at`'s suffix list (≈284).

The live ledger (§1.2a) is unchanged.

## 5. Playback-start and throughput risk

### 5.1 What the broker costs

`source_input.rs` on `main`: one loopback listener and axum task per Input
(`launch` ≈559–580); reads serialised under the budget lock (≈831) with one
window per Input; a read at a new offset discards the window and reads up to
`window_bytes` (default 4 MiB, ≈870–885), so every open-ended `Range` seek
pulls a full window unless the Input is `without_prefetch()` or
`with_read_window(n)`; a `spawn_blocking` `source.unchanged()` check per
256 KiB chunk served from the window (≈846); for non-rolling foreground
Inputs, `transfer_epoch`/`idle`/`refill` observer calls in the read path that
can sleep until a grant expires (≈905–945).

### 5.2 Per-site policy

| Sites | Input | Window | Budget |
|---|---|---|---|
| P11, P12, P7, P17 probes (seek several times, read little) | `open_foreground_lent` (effort `0294b75ab`), one Input per probe | `without_prefetch()` plus `seal_read_ceiling(64 MiB)` | charged to the owner's custody; a lent Input never calls `transfer_epoch`/`idle`/`refill` — the owner's primary Input already holds the grant. That rule is new in `source_input.rs` and must be agreed with the effort's author |
| P13 playhead windows | lent | `with_read_window(1 MiB)` | as above |
| P14, P15 whole-track extraction | lent | default | as above |
| P6, P8, P10 producers | lent or own Input | default 4 MiB | unchanged |
| §2.2 background readers | `open_background_held` | default | background budget, unchanged |

Two readers never share one URL: a probe that shares the producer's URL
would discard its window on every seek. Each reader gets its own lent Input
under the same custody.

### 5.3 Gates

Measured with the B0 bench (`source_input::tests::brokered_read_rate`,
ignored perf test, plus a lab script) on lab4, nas and local NVMe:

| Site type | Pass when |
|---|---|
| Realtime producers (P1–P6, P8, P10) | brokered sustained rate ≥ 4× the title's peak bitrate on nas and on NVMe, for a 4K HEVC remux of at least 40 Mbit/s and a P8 FEL title; and with a second concurrent reader (probe) on its own lent Input, the producer still meets it |
| Throughput jobs (DV conversion, VMAF, offline transcode, subtitle extraction, transcription audio) | real-job wall clock within 5% of raw on the same title |
| Playback start | over 20 starts per title: median grows by at most 20 ms and p95 by at most 50 ms, including a cold-cache nas title (not read in 24 h, or evicted on the lab host) |

A site that fails waits for a broker change (a second read slot for disjoint
ranges, a larger sequential window), never for a raw fallback. The
implementer of each B2 task — or the GPT lab session for lab runs — records
the numbers in the Execution log below and in a "Measurements" table in
`docs/server/PLAYBACK-READER-WITNESS.md`.

## 6. Fit with in-flight work

| Work | Overlap | Handling |
|---|---|---|
| PR #1561 census stopgap (`claude/playback-census-recovery`) | `reader.rs` (`CensusFamily` names, `record_source_identity`, foreground `.source`), `source_custody.rs` (`note_process_source_boot`, `process_source_boot`, `source_generation`), `SOURCE-CUSTODY-RECOVERY.md`, `deploy/docker-compose.yml`, `deploy/plurxd.service` | B0 starts after #1561 merges; `.poisoned` and `.lifetime` use family-prefixed names; the census stays (§10) |
| `effort/playback-start-phase2` | touches `source_input.rs` (`open_foreground_lent`, `open_foreground_with_custody`), copy ranges, VOD, `growing.rs`; touches none of `process/`, `ffmpeg.rs`, `producer_spawn.rs`, `source_custody/` | B1 lands on `main` independently. P6, P7, P11, P12, P13 and P15 hard-depend on the effort merging, because they use `open_foreground_lent` |
| `claude/a1b1-range-serving` | same files as the effort | same |
| `effort/standalone-sqlite` | standalone installs (§7) | coordinate only |
| Storage-domain deferred apply | none in code | independent |

`with_copy_measurements` (fds 6, 7) is already on `main` (`producer_spawn.rs` ≈66–76).

## 7. Standalone SQLite installs

A non-replicated Store has no reader witnesses (§1.2a: the ledger uses its
own start as the escape tick), and `source_identity` is not configured, so
P1/P2 read the raw path and fd 3. Self-settling after a restart is a
replicated-store property, and this plan does not change standalone
recovery. B2 brokers a standalone site only where the site already opens a
`SourceInput` without a request; P1/P2 stay raw on standalone. Decision
noted for the owner to review; if standalone should get the same guarantee,
that is a follow-up on `effort/standalone-sqlite`.

## 8. Tests

Each test fails on today's code unless marked *guard*.

### 8.1 Core (`crates/plurx-core/src/process/`)

| Test | Proves |
|---|---|
| `source_lifetime::tests::no_source_launch_with_daemon_paths_verifies` | cache paths, a registered pipe and a daemon cwd verify `NoSource` |
| `source_lifetime::tests::brokered_declaration_with_a_media_path_is_unknown` | a token outside daemon roots fails `Brokered` (today `mediated` is trusted) |
| `source_lifetime::tests::cwd_relative_path_is_unknown` | `-i movie.mkv` with cwd in a library root fails |
| `source_lifetime::tests::aliased_root_is_unknown` | a symlink in the cache pointing into a library root fails after canonicalisation |
| `source_lifetime::tests::directory_and_list_inputs_are_unknown` | a disc folder and an unregistered concat list fail |
| `source_lifetime::tests::untagged_descriptor_is_unknown` | a descriptor not registered through the builder fails |
| `source_lifetime::tests::dead_broker_url_is_not_a_live_capability` | an unregistered loopback URL fails |
| `source_lifetime::tests::declared_raw_failure_keeps_the_lifetime_lock` | B3: a failed `DeclaredRaw` launch is `Unknown` and still inherits fd 9 |
| `census::every_production_launch_declares_source_access` | source scan: each production launcher call is inside a §4.1 scope; no descriptor-installing `pre_exec` and no file-backed `Stdio::from` stdin outside `producer_spawn` (reviewed allowlist) |

### 8.2 Daemon (`crates/plurxd/src/`)

| Test | Proves |
|---|---|
| `producer_spawn::tests::fstat_classifies_pipes_outputs_and_sources` | §4.3 table, including an unregistered regular file on the data device → `Source` |
| `ffmpeg::tests::output_and_pipe_descriptors_are_not_a_source_handoff` | only `Source` kinds tag |
| `source_custody::recovery::reader::tests::brokered_rolling_producer_keeps_the_armed_witness` | arm a foreground witness, spawn through `spawn_and_install_prepublication_child`: anchor byte 0 is not `!`, no `.poisoned` |
| `…::reader::tests::capability_probe_during_playback_keeps_the_witness` | a `NoSource` engine probe leaves an armed witness valid |
| `…::reader::tests::unknown_launch_still_poisons_every_armed_witness` | *guard* |
| `…::reader::tests::poisoned_witness_names_the_launch` | `.poisoned` holds the purpose |
| `…::reader::tests::witness_directory_bound_counts_seven_sidecars` | admission reserves the full sidecar count; `complete_at` removes `.poisoned` and `.lifetime` |
| `transcode::media_origin::tests::probe_reads_a_lent_input_without_prefetch` | P12 argv has only a broker URL; the Input is lent and unprefetched |
| `http::stream::tests::chapter_probe_argv_names_only_a_broker_url` | P11 |
| `vod::init::tests::init_segment_inherits_no_source_descriptor` | P6 |
| `dv_segment::tests::renderer_reads_a_broker_url` | P8 |
| `subtitles::tests::playhead_window_never_passes_the_raw_path` | P13 with and without a current request |
| `source_input::tests::lent_probe_input_makes_no_grant_calls` | §5.2 budget rule |
| `…::reader::tests::declared_raw_child_holds_the_lifetime_lock_after_daemon_death` | B3: fixture arms, launches a declared raw child holding fd 9, is SIGKILLed; replay retains while the child lives and settles after it exits |
| `…::reader::tests::deployed_tools_keep_fd_9_open` | B3, run against the deployed FFmpeg, ffprobe and DV renderer builds: each still holds the inherited description mid-run |

### 8.3 Crash test — the acceptance test

`source_custody::recovery::reader::tests::daemon_killed_mid_playback_self_settles_foreground_custody`
(Linux and macOS, replicated test Store as the existing v2 fixtures use; the
subprocess fixture driven by `config.mode` in `reader/tests.rs`):

1. The fixture registers a foreground owner through `SourceInput::open`,
   arms its witness, and starts the rolling producer through
   `spawn_and_install_prepublication_child` with a stub FFmpeg that reads
   the broker URL in a loop.
2. The test SIGKILLs the fixture. The stub's next read fails and it exits.
3. A fresh process runs `replay_verified_recovery`.
4. Assert: the owner is settled (`foreground_source_permissions.custody_held = 0`),
   its v2 files are gone, no census proof exists, and an analysis claim on
   `source_io` is admitted.

Today step 4 fails: the producer launch poisoned the anchor.

### 8.4 Lab qualification (GPT prompt)

Paste to a GPT session with lab access once B2 is deployed to lab3:

> On lab3 (noirr cinema test node, Docker deployment), with no storage
> domains configured: start playback of five titles from the web client
> (one 4K HEVC remux copy, one transcode with burned subtitles, one DV P8
> FEL processed, one VOD, one with chapters). While all five play, run
> `docker kill` on the plurxd container, then start it again. Within two
> minutes, check Settings → Developer → Playback restart recovery and the
> Activity page: every foreground owner from before the kill must be
> settled, "witnesses waiting for the host census" must be 0, and the
> undeclared-launch count must be 0. Do not run any census helper. Repeat
> three times. Then run the start-time gate of §5.3 on lab4 (20 starts per
> title, one cold-cache nas title). Record per run: settle time, any
> `.poisoned` purposes, median and p95 start time before and after, in the
> plan's Execution log.

## 9. Developer advisory and attribution

New card in Settings → Developer, **Playback restart recovery**, fed by
`GET /api/v1/developer/source-recovery` (admin, `crates/plurxd/src/http/developer.rs`).
Nothing is enabled or gated; each row says what self-settling needs and
whether it is met. The card names the node it describes with the node's own
name from the response (`node_name`), never a fixed host.

| Row | Met when |
|---|---|
| Reader witnesses supported on this node | kernel instance readable and the data directory on a qualified local filesystem |
| Every child launch since start declared its source access | undeclared launches = 0; otherwise "3 undeclared launches since start; last: chapter list probe at 14:02" |
| Daemon is PID 1 or a child subreaper | needed by the live ledger after an undeclared launch |
| Witnesses waiting for the host census | 0; otherwise the count, the poisoning purpose, and "Run scripts/source-custody-source-census on {node_name}" |

Activity's child-process rows (which list `ChildWork` purposes) gain the
verified source-access class, so a reader of real storage is attributable:
what it is, which owner it serves, and the existing stop control for that
work. Metrics: `plurx_source_launch_total{access}` and
`plurx_source_witness_poisoned_total{purpose}` (fixed `&'static str`
purposes, bounded cardinality).

## 10. The census stopgap

Keep it. After this plan it is the recovery path only for witnesses still
poisoned: written before the upgrade, poisoned by a deliberately `Unknown`
launch, or by a declaration that failed verification. The §9 card tells the
operator when it is needed. SOURCE-CUSTODY-RECOVERY §1.3 says that ordinary
restarts during playback settle by themselves and the census is the fallback.

## 11. Phasing

Large effort `effort/source-reader-witness`, task branches into it, per
AGENTS.md.

| Phase | Content | Depends on | Behaviour change |
|---|---|---|---|
| B0 | `.poisoned`, launch counters, Developer card, broker bench, lab4 measurement | PR #1561 merged | none; shows which launches poison |
| B1 | §4.1–4.3: classes, allowlist verification, single descriptor builder (all `pre_exec` sites moved), declarations for P1–P5, P9, P16, P18, §2.2 `NoSource`/`MetadataOnly` | — | most witnesses stop being poisoned; no bytes path changes |
| B2 | every "broker" row moves to the broker with the §5.2 policy; one task PR per site with its §5.3 numbers | `effort/playback-start-phase2` merged for P6, P7, P11, P12, P13, P15 | raw media readers disappear |
| B3 | §4.4 lifetime lock, only if B2 leaves a raw site | B2 | residual raw children self-settle |
| B4 | §8.3 green on Linux and macOS, §8.4 lab run, docs | B1–B3 | qualification, then effort → `main` |

## 12. Open points the implementer resolves in B1

- **Q1** Is `session.subtitle_handle` / `recipe.encoding.subtitle` ever the
  original media or a sidecar beside it? §4.3 classifies it mechanically; if
  it is `Source`, burn-in reads through a broker URL in the `subtitles=`
  filter (escape `:` and `\`) or the launch stays `Unknown`.
- **Q2** What file does `vod/init.rs::reopen_encoded_audio` open?
- **Q3** Is the streaming DV renderer the binary `fel_qualification.rs`
  already drives with a broker URL?
- **Q4** Live TV: should a declared non-loopback tuner URL verify as
  `NoSource`? The plan allows it only for the declaring sites, by scheme.

## 13. Docs touched

- `docs/server/SOURCE-CUSTODY-RECOVERY.md` §1.1 (classes, allowlist), §1.2a
  (declared raw and the lifetime lock), §1.3 (census as fallback).
- `docs/OPERATIONS.md`: the Developer card and when a census is needed.
- `docs/API.md`: `GET /api/v1/developer/source-recovery`.
- This plan moves to `docs/server/PLAYBACK-READER-WITNESS.md`, linked from
  `docs/server/README.md`.

## 14. Review

Fable reviewed the first draft against `origin/main` at `54aebebf9`.

| Finding | Resolution |
|---|---|
| B1 most severe: denylist verification fails open | §4.2 rewritten as an allowlist over daemon-owned roots, cwd, canonical paths, list/script inputs, broker URLs and builder-registered descriptors; §4.3 classifies descriptors by `fstat` plus builder registration (tighter than "other device", which fails open when media and data share a disk); census forbids `pre_exec` descriptors and file stdin outside the builder; negative tests in §8.1 |
| B2: wrong API | probes use `open_foreground_lent`; hard dependency on the effort stated in §6 and §11 |
| B3: broker costs | §5.1 facts, §5.2 per-site window and budget policy, one Input per reader, two-reader bench case |
| B4: gate | absolute criteria, p95, cold-cache nas title, who records results (§5.3) |
| B5: witness directory bounds | §4.4 raises bound and reservation; `complete_at` removes both new sidecars; test |
| B6: #1561 overlap | §6; B0 sequenced after #1561; family-prefixed names; `with_copy_measurements` noted on `main` |
| B7: PID-namespace wording | §3.2 reworded around the two decisive reasons |
| B8: lifetime lock limits | §3.3: NFS D-state keeps the lock; children can release it; tests against deployed builds; `local_filesystem`; failed `DeclaredRaw` keeps the lock |
| B9: standalone SQLite | §7: P1/P2 stay raw on standalone and its recovery is unchanged (no witnesses there); follow-up named |
| B10: placeholder host | card renders `node_name` from the response |
| Line drift | every reference re-pinned to `54aebebf9` |

## Execution log

| Date | Agent model | Agent session | Step | Result |
|---|---|---|---|---|
| | | | | |
