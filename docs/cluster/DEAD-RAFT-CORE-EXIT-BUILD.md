# Dead Raft core exit — a node whose core died does not serve fenced for hours

**Status:** built — fleet verification pending · **Executes:** audit item C7
(not yet a row of `CLUSTER-LAYER-AUDIT-2026-11-14.md`) · **Design:**
coordinator; build: Claude (Opus 5.5), session
`session_01BCWLA3JD1HCNMEid3AZrc3` · **Verified against:** `main` with
C2 (PR 1642) · **Written:** 2026-11-15

## 1. Objective

On 2026-11-15 lab3 (a learner) filled its root filesystem. Snapshot admission
deferred for ten minutes, then built anyway. The build failed with "database
or disk is full", and OpenRaft quit the database Raft core. plurxd stayed up
for hours, fenced, refusing every AppendEntries with the stored error and
keeping a 144 MB `.temp` and `.temp-journal` on the full disk. Only a manual
restart recovered it.

After this change:
- a process whose core died drains and exits non-zero, so its supervisor
  restarts it;
- a disk that cannot fit a build never starts one;
- a failed or abandoned build leaves no temporaries behind.

## 2. Contract (re-verify)

| Fact | Where | What it says |
|---|---|---|
| Terminal state | openraft 0.9.25 `core/raft_core.rs:245` | The core publishes `running_state = Err(err)` when `main` returns: `Fatal::Stopped` after `Raft::shutdown`, `Fatal::StorageError` after a storage-error quit. A panic drops the watch without a terminal state. |
| Build error is fatal | `raft_core.rs:1381` | `command_result.result?` on the state-machine notification: a `build_snapshot` error ends the core. No existing mechanism lets a build fail without that. |
| Build runs off the loop | `core/sm/worker.rs:186` | The build is a spawned task. A pending build sets only `building_snapshot` (`snapshot_handler/mod.rs:34`), which blocks the next build trigger. Log purge waits for a new snapshot; receive and install stay live. |
| Observer | `vendor/hiqlite/src/client/mgmt.rs:165` `LocalDbRaftMetrics::core_stopped`, `:189` `core_fatal` (patch 33) | `core_stopped` waits on the watch: `Stopped` or `Fatal(text)`. `core_fatal` reads the same state without waiting, for loops that already wake on their own. |
| Monitor | `crates/plurx-core/src/cluster/membership.rs:5687` `MembershipManager::raft_core_failure`, reached through the daemon facade's `core_failure` (`crates/plurxd/src/cluster.rs:98`) | Resolves only on `Fatal`. With no replicated store it stays pending forever. It is the only copy; the core regression calls it through `membership_manager()`. |
| Startup | `crates/plurx-core/src/cluster/migration.rs:4121` `startup_core_died` | The admission, quorum-watermark and catch-up waits check `core_fatal` on every wake and end startup with the core's error, instead of waiting out the catch-up deadline (about 23 minutes by default) behind the fence. |
| Call site | `crates/plurxd/src/main.rs:2655` | The last arm of the drain's biased `select!`, after the signal and the leave. `exit_after_drain` (`:2748`) returns a typed `RaftCoreFatal`, which wins over a serving error in the same drain (that error is logged); the store close after it is bounded at 30 s (`:2057`), and `daemon_main` exits 1 after its runtime shuts down (`:664`). |
| Admission | `vendor/hiqlite/src/snapshot_admission.rs:97` | Three bands, below. Re-probed every 10 s; ends with an error once the SQL writer has gone. |
| Temporaries | `snapshot_builder.rs:123`; `state_machine.rs:346` (patch 34) | A failed build removes `<uuid>.temp` and `-journal`. Startup removes abandoned ones, after the lock check and before the writer. |
| Lock file | `client/mgmt.rs:896`, `:1179` (patch 34) | Both shutdowns stop the SQL writer, which removes the lock, whatever the WAL's shutdown returned; a dead core's WAL writer has exited, so that shutdown errors. A stopped SQL writer is an error there, not a panic. The daemon test checks the lock is gone after the exit. |

The passive metrics loop (`migration.rs:7203`) is not the authority: it
stops on any invalid observation. The core's own terminal state is.

## 3. Design

1. **Exit on a dead core.** `raft_core_failure` waits for the core's own
   terminal state; this is not a symptom watchdog. On `Fatal`, the drain
   logs one `error!` naming the error and runs as for SIGTERM. Its sessions,
   WAL and writer stop as far as they can. A dead leader core's last
   metrics still name it leader, so `handoff_for_shutdown` checks
   `running_state` first and returns `core_stopped` (not settled) at once.
   The store close is bounded at 30 s, the runtime shuts down, and the
   process exits 1. An ordinary `Raft::shutdown` (`Stopped`) and a store
   without Raft never fire; a single voter does. Nothing is observed before
   `boot` builds the drain.
2. **No doomed build.** In 0.9.25 a failed build always kills the core, and
   deferring blocks only the next snapshot (so log purging, and a
   snapshot-anchored backup, which has its own deadline). One build writes
   the copied image, the WAL it pins, and a journal:
   - below `image + max(WAL, 32 MiB) + 64 MiB` it cannot fit and waits with
     no bound; `snapshot_storage_deferral` does not bound it, and 0 does not
     make it attempt;
   - below `2 × image + max(WAL, 32 MiB) + 64 MiB` but able to fit, or on a
     probe error, it waits as before: at most `snapshot_storage_deferral`
     (ten minutes), 0 attempting at once. One clock covers both; a
     cannot-fit answer restarts it.

   A deferral warns at most once a minute and counts
   `plurx_raft_snapshot_deferrals_total{reason="storage"}`, which the
   cadence plan's alert (§3.5 there: rate > 0 for 5 min, page) watches.
3. **No stranded temporaries.** See §2.

What restarts the process:
- `deploy/docker-compose.yml:82`: `restart: unless-stopped`.
- The systemd unit: `Restart=on-failure`.
- The launchd plist: `KeepAlive` with `SuccessfulExit` false.
- The Windows service reports exit code 1 to the service control manager
  (`windows_service.rs:257`). No failure actions are configured, so it
  stays stopped and visible.

On a disk still full, the result is a restart loop, accepted on purpose:
it is visible, and the node heals itself once the cause clears. No
supervisor give-up limit is added. Docker doubles its delay from 100 ms but
resets it after 10 s of running, so longer cycles restart at once; systemd
waits `RestartSec=5` with no `StartLimit`. Each cycle costs a full startup,
system probe included, and a rejoin.

Residual, for the audit: admission covers builds only. Installing a
received snapshot on a full disk fails inside the core and is fatal (item 1
exits); a failed chunk write while receiving returns to the sender's RPC.

## 4. Decided edge cases

| Case | Behaviour |
|---|---|
| Ordinary stop | `Fatal::Stopped`: no trigger, exit 0. |
| Core panics | The watch closes still `Ok`: treated as fatal. |
| Fatal during startup | Startup's admission and catch-up waits see it on their next wake and fail with the core's error; the supervisor restarts the process. |
| Fatal before serving | The drain's first poll sees it and drains at once. |
| Fatal during a SIGTERM drain | The select already resolved: one drain, signal's verdict. |
| Fatal and signal both pending | Biased to the signal or leave: exit 0. |
| Dead leader core | Handoff returns `core_stopped`, no network. |
| Disk cannot fit for hours | No build, no purge, logs grow; if an append then fails, item 1 exits. |
| Can fit, or probe error | Bounded at ten minutes, then the existing path. |
| Writer gone mid-deferral | The build ends with an error. |

## 5. Non-goals

No in-process Raft restart, no serving-fence change, no setting or flag, no
disk-space watchdog, nothing about why the disk filled.

## 6. Tests

- `vendor/hiqlite/src/client/mgmt.rs::the_core_stop_names_a_fatal_quit_and_ignores_an_ordinary_stop`,
  `the_lock_file_is_released_whatever_the_wal_shutdown_returns`.
- `crates/plurx-core/src/cluster/migration/membership_api.rs::a_dead_raft_core_is_reported_and_an_ordinary_stop_is_not`.
  A real voter whose snapshots path is replaced by a file is asked to build;
  the core quits and both observers see it, and its handoff returns
  `core_stopped` at once. An ordinary stop does not fire.
- `crates/plurxd/src/startup_tests.rs::a_dead_raft_core_drains_like_a_signal_and_exits_non_zero`.
- `crates/plurxd/tests/cluster_activity.rs::a_follower_whose_raft_core_dies_exits_non_zero_and_rejoins_after_restart`.
- `vendor/hiqlite/src/snapshot_admission.rs`: `a_build_that_cannot_fit_is_never_attempted`,
  `storage_that_can_fit_but_is_short_of_the_floor_is_bounded_at_ten_minutes`,
  `unknown_storage_is_bounded_at_ten_minutes`,
  `a_zero_deferral_attempts_a_build_that_can_fit_at_once`,
  `a_cannot_fit_answer_restarts_the_bound`,
  `unknown_and_short_of_the_floor_share_one_bound`,
  `a_deferred_build_ends_when_its_writer_is_gone`,
  `the_real_probe_sees_a_disconnected_writer_before_measuring`.
- `.../sqlite/state_machine.rs::off_writer_copy_failure_preserves_published_generation_and_live_integrity`:
  a real failed copy leaves no `.temp`; a restart sweeps an abandoned pair.
  The builder's `abandoned_build_temporaries_are_removed_and_nothing_else`
  and source guard back it up.

## 7. Execution log

| Date | Step | Agent-Model | Result |
|---|---|---|---|
| 2026-11-15 | Design | coordinator | brief |
| 2026-11-15 | Build | claude-opus-5-5 | Patch 33 (built as 31; renumbered twice as main took rows 29 and 32) plus the daemon exit. On media1 the follower test passed 3/3: the core died on the blocked snapshot path, the process exited non-zero inside the 90 s bound, the other two kept a leader, and the restarted follower rejoined. It fails with the drain arm removed, as does each other test with its subject mutated. C2 holds: `leader-shutdown` passed, and M3 leader SIGTERM passed 3/3, with the successor named 131–133 ms after SIGTERM. |
| 2026-11-15 | Review fixes | claude-opus-5-5 | Not recorded. This row was left as a placeholder, and what it covered was not recovered at handoff. |
| 2026-11-18 | Pre-landing review and fixes | claude-opus-5-5 | Whole-diff adversarial review on current main: approve, seven minor findings. Fixed all seven. Startup fails at once on a dead core. The duplicate `ReplicationMonitor::raft_core_failure` is removed, and the core test calls the production path. The daemon test checks the lock file is released. The main shutdown maps a stopped SQL writer to an error. A dead-core verdict wins over a serving error. The generic teardown repairs moved to Hiqlite row 34 (`pending M6`). This doc is refreshed. |
