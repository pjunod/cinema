# Media I/O isolation: a hung media mount must not stop the daemon

**Status:** built; awaitable fence checks bounded (A-0A22BF6A); media identity reads bounded (E-AA7C9C6A48) · owned by Claude · **Updated:** 2026-11-27 · **Source baseline:** `origin/main` at `5eccbf85e`

Effort E-08DAF9B91B, related to E-E154B131A2 (Raft cluster rebuild, audit C4/R1). Identity reads: E-AA7C9C6A48.

## 1. What happened

On 2026-11-23 at 20:22:54Z, voter-a's host (media1) lost carrier on its only
route to the NFSv4.1 media server (`hard,timeo=600,retrans=2`). From 20:23:06
to 20:24:30Z, plurxd stopped running everything for 84.85 s. Raft receive
handlers did not run while the leader kept sending. The serving monitor woke
84.78 s after its sleep deadline. `/metrics` timed out on every scrape. When
the node came back, it deposed the healthy leader (audit R1; pre-vote fixes
that half in E-E154B131A2).

What the C4 collector shows:

- Scrapes answered in about 50 ms until 20:23:05. From 20:23:07, every scrape
  timed out. The stop was one sudden, complete event, not a gradual drain.
- The serving monitor's lateness is measured from its own sleep deadline. An
  84.78 s late wake means no worker thread polled the task for that long.
  Every worker of the runtime was blocked.
- 84 s is one 60 s NFS timeout plus a TCP reconnect. The onset 12 s after
  carrier loss fits the NFS attribute cache expiring. Until then, `stat` and
  `fstat` are answered locally. After that, every revalidation goes to the
  server and blocks.

plurxd runs one multi-thread Tokio runtime for Raft, HTTP, the serving monitor
and all media work (`daemon_main`). A blocking syscall that runs on one of its
workers takes that worker away for as long as the kernel holds it. Enough of
them at once take every worker, and the whole daemon stops.

## 2. Root cause

Synchronous filesystem calls on media storage ran directly on the async
worker threads. There are two shapes:

1. **Inline path calls in async code**: `statvfs`, `canonicalize`,
   `is_file`, `std::fs::read` and `File::open` on library and DVR paths.
2. **Inline `fstat` of held source descriptors**: `SourceFence::drift` /
   `unchanged`, and `File::metadata` after an open. These are reached from
   about 150 decision sites in playback, VOD, subtitles, DV and fragment
   indexing, many of them inside synchronous closures. On NFS each one
   revalidates attributes with the server once the cache has expired. Every
   concurrent check hits that expiry at the same moment, which explains the
   sudden onset.

One lock made this worse: `SourceInput::source_object_version` took the input's
`std::sync::Mutex<Option<SourceFence>>` on a worker. The source reader holds
that mutex across its positioned `read_at` on the blocking pool. On a hung
mount, the worker waits for the whole hang.

Ruled out:

- No memory-mapped media.
- No inotify watcher.
- The scanner walk, the root-readability probe (a child process), the
  shared-cache mount I/O, `storeprobe`, the playback cache and scratch
  sampling were already off the workers.

The exact call that fired first on voter-a at 20:23:06 cannot be identified
from the evidence we have. There is no journal excerpt of plurxd and no
playback telemetry for that minute. The fix therefore covers both shapes, at
every site the inventory found (§4).

## 3. Design

Everything goes through one module, `plurx_core::media_io`. It follows the
rule the codebase already uses for local work (`fs_secure`, `tokio::fs`:
blocking pool, never a worker) and adds a bound for media storage.

- **`media_io::blocking(f)`** is for code that can await. It runs `f` on
  Tokio's blocking pool behind a fixed pool of 64 permits
  (`MEDIA_IO_CONCURRENCY`). The permit moves into the blocking closure. A
  caller that is cancelled while its call is in the kernel therefore keeps
  the bound true until the kernel returns. A hung mount can hold at most 64 of
  the pool's 512 threads, and the rest of the pool keeps serving local
  `tokio::fs` and the store. Only the tasks that await media I/O wait.
- **`media_io::in_place(f)` / `media_io::fstat(file)`** is for synchronous
  decision code that checks a held descriptor and cannot await. On a
  multi-thread worker it is `tokio::task::block_in_place`. The worker's
  scheduler core moves to another thread before the call enters the kernel,
  so the runtime keeps running. Anywhere else (a blocking-pool thread, a
  plain thread, a current-thread test runtime) it calls `f` directly. This is
  the Tokio primitive for exactly this case. It is not bounded the way
  `blocking` is: each parked check holds one thread. On a long hang, clients
  retrying playback could in principle use up the 512-thread blocking pool.
  The `plurx_media_io_in_place` gauge makes that visible.
- **Fence checks are bounded where the caller can await.**
  `SourceFence::unchanged()` and `drift()` are `async` and run the `fstat` on
  a duplicated descriptor through `blocking` (`dup` stays in the local kernel
  and never reaches the server). The `HeldSource` helpers cover the
  `Option<&SourceFence>` call shapes (`held_unchanged`, `missing_or_changed`,
  `held_and_changed`, `held_unchanged_as`, `held_drift`). The slot is taken
  before the `dup`, so a queued check holds no extra descriptor. Making the
  check `async` by default made the compiler list every caller, on macOS and
  under the Linux-only `cfg` gates alike. Callers that cannot await use
  `unchanged_in_place()` / `drift_in_place()`, which take the `in_place` path
  above: synchronous decision code, `filter`/`is_some_and` closures, and code
  already on the blocking pool under a media slot. So do the checks made
  while a lock is held (the `VodServe` preparation readers and manifest
  locks, the retained collector and validation locks, the manifest lock in
  generation start, the publication clock). A lock must never be held while
  its holder queues for a media slot.
- **Metadata calls on a media descriptor are media I/O.** `fstat`,
  `File::metadata` and `fs_secure::std_file_identity` on a descriptor that
  can be on media storage revalidate with the server on NFS just as a read
  does, so they go through `media_io` like any other media call. Identity has
  its own pair: `media_io::identity(file).await` (slot first, then `dup`, then
  the `fstat` under the slot) and `media_io::identity_in_place(file)` for the
  same callers that may use `in_place`. Descriptors on the node's own disk
  (engines, models, private scratch, the playback cache) use
  `fs_secure::local_file_identity`, so each call site's name says which kind
  of descriptor it holds. `media_io_guard_tests` fails on any
  `std_file_identity` outside `fs_secure` and `media_io`, other than the
  one pinned call in `fragment_index_cluster::windows_object_version`, which
  only runs inside a fence check that is already on the media path.
- **Sanctioned in-place exceptions.** `in_place`, `fstat`,
  `identity_in_place`, `unchanged_in_place` and `drift_in_place` are allowed
  only where the caller cannot await without changing what it protects:
  - synchronous decision code and `filter`/`is_some_and` closures (§4);
  - code already on the blocking pool, under a media slot or not, where the
    call runs directly (`SourceHash::new`/`step` inside the hash workers);
  - checks made while a lock is held that cannot be restructured without
    changing what the lock protects (the `VodServe` preparation readers and
    manifest locks, the retained collector and validation locks, the
    manifest lock in generation start, the publication clock);

  Where a lock can be released across the check, it is: snapshot, drop the
  lock, await the bounded check, re-lock and revalidate (the subtitle read
  attestation cache).
- **Cheap state is re-read after an awaited check that guards a destructive
  step.** The check can wait for a slot, so a lease or cancellation read
  before it may be stale by the time it returns. The DV disk publication
  re-reads its lease, and the source-probe spawn re-reads its cancellation
  and authority.
- **The bound is node-wide, not per mount.** While one mount is hung and
  holds all 64 slots, an awaited check of a source on a healthy mount or
  local disk queues behind it. That is the trade-off of one global bound. It
  keeps the blocking pool (local `tokio::fs`, the store) and the async
  workers serving, which matter more than a fence check on another mount.
  The fleet's media sits on one NFS server. A per-device budget, keyed by the
  `st_dev` already in each object version, is the follow-up if that changes
  (ask A-45050A2C).
- **`media_io::slot()`** is for code already on the blocking pool, such as
  the source reader. It takes the same permit around the media calls only.
  The reader revalidates its grant (store and Raft work) before it takes the
  slot, so healthy waits cannot pin media slots.
- **Observability, not control:** `plurx_media_io_{in_flight,capacity,waiting,
  completed_total,in_place}` on `/metrics`. These read atomics only, so the scrape
  answers while the mount is hung. Nothing acts on these numbers: there is
  no watchdog, deadline or feature gate.

Not chosen:

- **A separate runtime for Raft.** It would protect Raft and leave HTTP,
  playback and the serving monitor stalling. It treats the symptom.
- **Converting every fence check to `async`.** The sites inside synchronous
  state machines and closures over lock guards would need those to become
  `async` or to drop their locks across the check, which changes what the
  check protects. They stay in place; the awaitable ones are bounded (above).
- **Timeouts around media calls.** A timed-out `stat` on a hard mount is still
  in the kernel. A timeout bounds only the caller's wait, which the async path
  already gives.

## 4. Sites changed

The inventory covered `crates/plurxd` and `crates/plurx-core`. Each site
below ran on a worker before this change.

| Site | Call | Now |
|---|---|---|
| `live_tv::free_space_bytes` (DVR loop every 15 s, recording start, `/dvr/status`, developer page) | `statvfs` on the DVR root | async, `media_io::blocking` |
| `scan::usable_source_directory` (movie and show placement) | `canonicalize` of the directory and every root | `media_io::blocking` |
| `scan::home::relative_dirs` (home and recordings placement) | `canonicalize` of each root | `media_io::blocking` |
| `scan::home::seed_from_nfo` | `is_file` ×3 plus `std::fs::read` of the NFO | `media_io::blocking` |
| `scan::home::ladder_date` | EXIF `File::open` plus read | `media_io::blocking` |
| `scan::recordings::read_sidecar` | `std::fs::read` of `<media>.json` | async, `media_io::blocking`; read once per placement instead of twice |
| `metadata::local::adopt_{local,folder}_art` | `is_file` ×6 | `media_io::blocking` |
| `source_census::replay` / `apply` (schedule tick, 60 s) | `canonicalize` plus `metadata` of every root and the source | `media_io::blocking` |
| `SourceFence::drift`, `clone_held`, `clone_current_held`, the playback fence open | `fstat` of the held source | `media_io::fstat` |
| `subtitle_source::Live::stamp`, `source_custody::recovery::reader::record_source` | `fstat` of a held descriptor | `media_io::fstat` |
| `SourceInput::source_object_version` | std mutex held across `read_at`, then `fstat` | `media_io::in_place` |
| `source_input` length checks, `source_direct`, `shared_source_direct`, `pretranscode::source`, `dv_segment` ×2, `subtitle_source` ×3, `subtitle_ride_along` ×2 | `fstat` of a held descriptor | `media_io::fstat` |
| `source_input::read_source` | already `spawn_blocking` | `media_io::slot()` around lock, fence and `read_at`, after the grant revalidation |
| Source readers (`source_input` close and valid checks, `source_custody` direct reader), developer DVR probe, playback fence open | already `spawn_blocking` | `media_io::blocking`, so they count against the bound |

Media identity reads (E-AA7C9C6A48). Each ran on a worker, outside the bound,
before this change. The full audit, including the local-descriptor sites left
direct, is in the PR description.

| Site | Call | Now |
|---|---|---|
| `subtitle_translation::runtime::VerifiedSource::unchanged` | in-place fence check plus raw `fstat` | `async`: awaited `unchanged()` plus `media_io::identity` |
| `runtime::VerifiedSource::recheck`, `runtime::verify_source`, `TranslationSources::cached` | raw `fstat` in async code | `media_io::identity` |
| `read_attestation::ReadAttestations::lookup` | in-place fence check plus raw `fstat` while holding the `entries` std mutex, on a worker | `async`: snapshot the `Arc` fence, release the lock, await both checks, re-lock and act only if the entry still holds that fence |
| `read_attestation::authorize` | raw `fstat`, `unchanged_in_place`, and `SourceHash::new` on a worker | awaited checks; `SourceHash::new` under a media slot, after the non-blocking hash lane and admission checks so a refused request does no media work |
| `read_attestation::final_source` | raw `fstat` | `media_io::identity` |
| `subtitle_text_translation::manager::current_source` | two raw `fstat`s | `media_io::identity` |
| `subtitle_transcription::pipeline` (source before decode, after recognition) | raw `fstat` | `media_io::identity` |
| `SourceHash::new` / `step` (hash workers on the blocking pool) | raw `fstat` | `media_io::identity_in_place` |
| Windows-only source handoffs: `ffmpeg` held-source probes, `producer_spawn` path handoffs, `decode_facts::source_observation_windows`, `pretranscode::source` | raw `fstat` | `media_io::identity_in_place`: an interim step, not a sanctioned exception (§6) |

Left as they are, because they already run off the workers:

- the scan walk;
- `library_root_available`;
- `removed` / `containment`;
- `fs_secure` opens;
- `tokio::fs` metadata;
- the root-readability child process;
- the shared-cache mount I/O;
- `storeprobe`;
- the playback cache;
- the scratch sampler.

Other async-reachable sync calls touch the backup destination or the local data
directory. Those are not media storage and are not part of this change.

## 5. Regression test

`plurx_core::media_io::hung_path` builds a FIFO. Opening it for reading blocks
in the kernel until a writer appears, just as an `open` or `read` blocks on a
hard NFS mount whose server is gone. `worst_heartbeat_lateness` then runs
four concurrent operations on that path on a two-worker runtime, beside a
10 ms heartbeat. It reads the verdict from outside the runtime: if every
worker is parked, nothing inside the runtime can run, timers included.

- `scan::recordings::tests::a_hung_recording_sidecar_does_not_stall_the_runtime`
  runs the real `read_sidecar` against a hung sidecar. Before this change it
  ran inline and the heartbeat stalled. Now the heartbeat stays on time.
- `media_io::tests::the_fixture_detects_a_hung_open_on_a_worker` runs the same
  stall inline and requires the fixture to report it. That proves the fixture
  can fail.
- `media_io::tests::hung_media_opens_do_not_stall_a_heartbeat` covers the
  primitive.
- `media_io::tests::an_awaited_identity_read_queues_on_the_media_bound` and
  `read_attestation::tests::a_cache_hit_recheck_queues_on_the_media_bound_without_the_entries_lock`
  hold every media slot and require an awaited identity read, and the
  attestation cache's recheck, to wait for a slot. The second also requires
  the `entries` lock to be free while the recheck waits. Before
  E-AA7C9C6A48 both read in place and completed.
- `media_io_guard_tests::media_identity_reads_go_through_media_io` fails on a
  direct `std_file_identity` call outside `fs_secure` and `media_io`.
- `fragment_index_cluster::tests::an_awaited_source_check_queues_on_the_media_bound`
  holds every media slot, as a hung mount leaves them, and requires an awaited
  `unchanged()` to wait for a slot while `unchanged_in_place()` still
  completes. Before A-0A22BF6A both completed.

The fixture also reports how many operations were still parked when the
heartbeat finished. The tests of the fix require all four, so a heartbeat that
is on time only because nothing hung fails the test.

## 6. Not in scope

- `VodServe` checks `rendition.source.unchanged()` while it holds the tokio
  `sessions` lock (`vod/serve/create.rs`). With this change the workers keep
  running, but a hung mount holds that lock for the length of the hang. This
  predates this change; it is a follow-up.

- The subtitle hash workers (`verify_source`, the read-attestation task) read
  source chunks on plain `spawn_blocking`, outside the media bound. The
  read-attestation worker is capped by its one-permit `HASH_IO` lane and
  read-attestation admission; `verify_source` by the fragment admission its
  job holds. The eventual shape is a slot per chunk, taken after the lane
  and admission, as `source_input::read_source` does. That is a follow-up,
  not part of the identity change.
- The Windows-only source handoffs use `identity_in_place` inside async
  functions and still open the source path inline
  (`open_read_nofollow_blocking`). Both are unbounded, and `block_in_place`
  also stalls sibling branches of the same task. They are debt, not finished
  work. Moving them onto `media_io::blocking` needs a Windows build to verify
  (`cargo xwin check --target x86_64-pc-windows-msvc`), and the lane does not
  run one.

- Pre-vote, so that a stalled voter cannot depose a healthy leader (R1,
  E-E154B131A2).
- media1's `enp46s0` link flapping. That is physical infrastructure (Paul).
- The 23:19 leader stall. enp46s0 was up at the time, so it has a different
  cause.
