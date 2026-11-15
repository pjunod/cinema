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
- a disk known to be short never starts a build;
- a failed or abandoned build leaves no temporaries behind.

## 2. Contract (re-verify)

| Fact | Where | What it says |
|---|---|---|
| Terminal state | openraft 0.9.25 `core/raft_core.rs:245` | The core publishes `running_state = Err(err)` when `main` returns: `Fatal::Stopped` after `Raft::shutdown`, `Fatal::StorageError` after a storage-error quit. A panic drops the watch without a terminal state. |
| Build error is fatal | `raft_core.rs:1381` | `command_result.result?` on the state-machine notification: a `build_snapshot` error ends the core. No existing mechanism lets a build fail without that. |
| Build runs off the loop | `core/sm/worker.rs:186` | The build is a spawned task. A pending build sets only `building_snapshot` (`snapshot_handler/mod.rs:34`), which blocks the next build trigger. Log purge waits for a new snapshot; receive and install stay live. |
| Observer | `vendor/hiqlite/src/client/mgmt.rs:165` `LocalDbRaftMetrics::core_stopped` (patch 31) | Waits on the watch; `Stopped` or `Fatal(text)`. |
| Monitor | `crates/plurx-core/src/cluster/migration.rs:7256` `raft_core_failure` | Resolves only on `Fatal`. For SQLite or a remote client it stays pending forever. |
| Call site | `crates/plurxd/src/main.rs:2622` | A third arm of the drain's `select!`, beside the signal and the leave. `exit_after_drain` (`:2721`) returns an error after the drain, so `daemon_main` exits 1. |
| Admission | `vendor/hiqlite/src/snapshot_admission.rs:59` | Known short: wait and re-probe every 10 s, with no bound. Unknown: bounded by 10 min of continuous unknown. |
| Temporaries | `snapshot_builder.rs:129`; `state_machine.rs:349` | A failed build removes `<uuid>.temp` and `-journal`. Startup removes abandoned ones, after the lock check and before the writer. |

The passive metrics loop (`migration.rs:7174`) is not the authority: it
stops on any invalid observation. The core's own terminal state is.

## 3. Design

1. **Exit on a dead core.** `raft_core_failure` waits for the core's own
   terminal state; this is not a symptom watchdog. On `Fatal`, the drain
   logs one `error!` naming the error and runs as for SIGTERM. Its sessions,
   WAL and writer stop as far as they can, and `handoff_for_shutdown`
   returns `NotLeader` at once. The process then exits 1. An ordinary
   `Raft::shutdown` publishes `Stopped` and never fires. A store without
   Raft never fires. A single-voter Raft does fire: a dead core is dead there
   too. Nothing is observed before `boot` builds the drain: activation and
   staged startup own their own failures.
2. **No doomed build.** In 0.9.25 a failed build always kills the core, and
   deferring blocks only the next snapshot (so log purging, and a
   snapshot-anchored backup, which has its own deadline). So known-short
   storage now defers without a bound. The ten-minute bound stays only for a
   probe error, counted over a continuous unknown stretch.
3. **No stranded temporaries.** See §2.

What restarts the process:
- `deploy/docker-compose.yml:82`: `restart: unless-stopped`.
- The systemd unit: `Restart=on-failure`.
- The launchd plist: `KeepAlive` with `SuccessfulExit` false.
- The Windows service reports exit code 1 to the service control manager
  (`windows_service.rs:257`). No failure actions are configured, so it
  stays stopped and visible.

On a disk that is still full, the result is a visible restart loop under
Docker's backoff. That is the intended, honest state, and once the cause
clears the node recovers by itself.

## 4. Decided edge cases

| Case | Behaviour |
|---|---|
| Ordinary stop | `Fatal::Stopped`: no trigger, exit 0. |
| Core panics | The watch closes still `Ok`: treated as fatal. |
| Fatal before serving | The drain's first poll sees it and drains at once. |
| Fatal during a SIGTERM drain | The select already resolved: one drain, signal's verdict. |
| Disk short for hours | No build, no purge, logs grow; if an append then fails, item 1 exits. |
| Probe error | Bounded at ten minutes of continuous unknown, then the existing path. |

## 5. Non-goals

No in-process Raft restart, no serving-fence change, no setting or flag, no
disk-space watchdog, nothing about why the disk filled.

## 6. Tests

- `vendor/hiqlite/src/client/mgmt.rs::the_core_stop_names_a_fatal_quit_and_ignores_an_ordinary_stop`.
- `crates/plurx-core/src/cluster/migration/membership_api.rs::a_dead_raft_core_is_reported_and_an_ordinary_stop_is_not`.
  A real voter whose snapshots path is replaced by a file is asked to build;
  the core quits and both observers see it. An ordinary stop does not fire.
- `crates/plurxd/src/startup_tests.rs::a_dead_raft_core_drains_like_a_signal_and_exits_non_zero`.
- `crates/plurxd/tests/cluster_activity.rs::a_follower_whose_raft_core_dies_exits_non_zero_and_rejoins_after_restart`.
- `vendor/hiqlite/src/snapshot_admission.rs::known_insufficient_storage_never_attempts_the_build`,
  `a_known_answer_restarts_the_unknown_bound`; the existing ten-minute test
  is unchanged.
- `vendor/hiqlite/src/store/state_machine/sqlite/snapshot_builder.rs::abandoned_build_temporaries_are_removed_and_nothing_else`,
  `a_failed_build_removes_what_it_wrote_and_startup_removes_what_was_abandoned`.

## 7. Execution log

| Date | Step | Agent-Model | Result |
|---|---|---|---|
| 2026-11-15 | Design | coordinator | brief |
| 2026-11-15 | Build | claude-opus-5-5 | Patch 31 plus the daemon exit. On media1 the follower test passed 3/3: the core died on the blocked snapshot path, the process exited non-zero inside the 90 s bound, the other two kept a leader, and the restarted follower rejoined. It fails with the drain arm removed, as does each other test with its subject mutated. C2 holds: `leader-shutdown` passed, and M3 leader SIGTERM passed 3/3, with the successor named 131–133 ms after SIGTERM. |
