# ci-fleet — CI placement and capacity console

**Status:** open · **Updated:** 2026-11-01 · **Owner:** Claude (claude-opus-5-5, session 01BCWL9y) · **PR:** [#1344](http://forge.lan:3000/noirr/plurx/pulls/1344)

ci-fleet answers four questions the forge cannot: which runner is executing
which job, why a waiting job is waiting, how much capacity each label pool
and host has left, and where the roster and the live fleet disagree. It is a
collector beside the forge, a small agent on every runner host, a console,
plain-text answers for agent sessions, and Prometheus metrics. It changes no
placement; its controls go through the tools that already own each runner.

Read [Using it](#using-it) to operate it, [Installing](#installing) to put it
on the fleet, and [Design](#design) before changing it.

## Using it

| Need | Do |
|---|---|
| Why is my PR's job not running? | `scripts/ci-fleet why --pr 1340` |
| Everything waiting, with reasons | `scripts/ci-fleet status` |
| Roster vs live fleet | `scripts/ci-fleet drift` (`--check` exits 1 on a blocking finding) |
| Raw data | `scripts/ci-fleet snapshot`, or `GET /api/snapshot` on the collector |
| The console | `http://forge.lan:8790/` — Fleet, Queue, Pools, Drift, Job detail |
| Alerts | Prometheus scrapes `http://forge.lan:8790/metrics` |

`scripts/ci-fleet` reads `scripts/ci-fleet.fleet.json` (git-ignored; copy the
`.example.json`). It asks the collector, or with `--direct` (and whenever the
collector does not answer) builds the same snapshot from the forge API and this
checkout's workflows and roster.

**Why-waiting classes.** Each waiting job gets the first that matches:

| Class | Meaning | What to do |
|---|---|---|
| BLOCKED | A job in its `needs` has not finished. | Nothing; it follows its needs. |
| HELD | Its needs finished; a concurrency group holds it. | Wait for the holder. |
| UNSATISFIABLE | No registered runner has every label it asks for. The card lists the closest runners and what each lacks. | Register the label, or change the workflow. |
| OFFLINE ONLY | Eligible runners exist; all are offline or paused, with each one's reason. | Start VM / Resume, or repair the runner. |
| BUSY | Every eligible runner is at capacity; shows what they run and an ETA from p50. | Wait, or add capacity in that pool. |
| NOT PICKED UP | An eligible runner has been idle for 60 s and has not taken it. | Look at that runner: it is not polling. |
| QUEUED | Just became ready; a free runner should take it on its next poll. | Nothing. |

Flags: **dead on pickup** (a need did not succeed, so a runner would only skip
it), **superseded** (a newer run of the same workflow exists for the PR),
**loaded host** (an eligible host is above 40 % non-CI CPU).

**Controls** need your own Forgejo token with write access (Sign in on the
console), from an address in `control_networks`. Every action is audited
with the signer's login.

| Control | What actually happens |
|---|---|
| Cancel run | `POST /actions/runs/{id}/cancel`. Forgejo cancels whole runs, never one job. |
| Drain (Incus VM runner) | Writes `/var/lib/plurx-runner-watchdog/paused/<vm>` at once, so the watchdog leaves it alone; once the guest has no `ACTIONS-TASK-` container and no runner child process, `systemctl stop --no-block` inside the VM. A busy runner is never stopped: stopping a container runner can cancel its job. |
| Drain (host-mode systemd runner) | Writes `/var/lib/ci-fleet/paused/<runner>`; stops the unit once it has no child process. |
| Drain (launchd Apple primary) | `launchctl disable` (the Apple janitor honours it) and a marker; SIGTERM once the runner has no child process. |
| Drain (Apple standby backup) | Writes the standby controller's `pause_marker`; it drains the backup and offers no slot until Resume. |
| Resume | Removes the marker; Incus runners are started by `plurx-runner-watchdog --vm <vm>`. |
| Start VM | `plurx-runner-watchdog --vm <vm>`: the watchdog's own start path with its headroom check and backoff. The console shows its receipt, including refusals. |
| Delete registration | Only for runners that are offline, not in the roster, and with no agent unit. |

## Installing

Paul deploys. Each step is one command on the host, from a checkout of `main`.

1. **Collector, on lab3 (beside the forge):** put a forge token with repository
   read, and a random shared agent token, in `/etc/ci-fleet/forge.token`
   and `/etc/ci-fleet/agent.token` (mode 0600), copy
   `deploy/ci-fleet/collector.example.json` to a host config, then
   `sudo deploy/ci-fleet/install collector <config>`. It runs as a
   `DynamicUser` with `CPUQuota=20%`, `MemoryMax=300M`, and listens on 8790.
2. **Agents, on every runner host and inside every Incus runner guest:** copy
   `deploy/ci-fleet/agent.example.json` (its `_examples` show each runner
   kind), put the agent token in `/etc/ci-fleet/agent.token`, then
   `sudo deploy/ci-fleet/install agent <config>` (macOS: the same command;
   it installs a LaunchDaemon). Agents run with `CPUQuota=10%`, `MemoryMax=100M`.
3. **Runner watchdog and Apple standby:** re-run their existing installers
   (`deploy/runner-recovery/install`, `deploy/apple-runner/install`) so the
   pause marker and `--vm` are live before anyone uses Drain or Start VM.
4. **Prometheus:** add a scrape job for `lab3:8790/metrics`; the alert rules
   are in [Alerts](#alerts).

## Design

Reviewed proposal, Fable adversarial review (verdict *rework*), and the
revision that addressed every P1/P2 finding: the claude.ai document
"ci-fleet — CI placement & capacity console (proposal)" and the renderings
canvas "CI Fleet Console" (2026-11-01). The decisions it settled:

**Sources.** Forgejo 16.0.3 gives runners and status, the waiting/running
queue with `runs_on`, runs and their jobs, and paged tasks. It does not record
placement on any task or job; line 1 of each job log does
(`Runner <uuid> (version:…) received task <id>`), so the collector reads the
first 512 bytes of each new task's log once and joins the runner by `uuid`.
Without `page=`, `/actions/tasks` and `/actions/runs` ignore `limit` and
return every row (25,040 tasks, 11.6 MB, 15 s, measured); `Forgejo.get`
refuses a list call without `page`, and a test pins it. Blocked jobs never
appear in `/actions/runners/jobs`; the collector lists active runs and their
jobs. Jobs carry no timestamps: a job is ready when its needs' tasks stopped.

**Pools** are the distinct `runs-on` label sets in `.github/workflows/*.yml`
on `main`, parsed by `validation/runner_fleet.py` — the same module
`tests/operations/test_contracts.py` uses — plus any set a live job asks for.
Each pool shows the next `schedule:` trigger that requests it.

**Agents** report resources only: host CPU and memory by owner (CI, plurx
production containers, other), disks against the runner reserve, GPUs, and
per-runner cgroup CPU, RSS, swap, pids, OOM kills and throttling against the
job caps; task containers are tied to their task by the `ACTIONS-TASK-<id>`
name. An Incus runner is reported by its host's agent (VM state, controls)
and by its guest's agent (usage, hygiene); the collector merges both.

**No second runner manager.** Drain/Start VM extend the watchdog (pause marker,
`--vm`) and the Apple standby controller (`pause_marker`); ci-fleet never starts
a VM or a daemon itself.

**Root cause, not just visibility.** The Apple jobs stuck for hours on
2026-11-01 were roster drift: macb re-registered with `xcode-27.2` while the
roster said `xcode-27.1`, and the contract test only checks workflows against
the roster. Drift (roster vs live registrations) is now computed every poll,
exported as `ci_fleet_drift_findings`, and `scripts/ci-fleet drift --check`
fails on a blocking finding. A PR job cannot run that check: the token
Actions jobs receive has no runner-admin scope.

**Bounds.** Polls every 5 s (runners, queue, active runs and their jobs,
running tasks), finished tasks every 30 s, workflows and roster only when
`main` moves; backfill of 90 days runs every third poll at five runs per step.
SQLite on lab3: tasks 90 days, pool minutes 30 days, usage at 5 s for 24 h then
1 min for 30 days, a hard row cap per series, 10,000 audit rows — all constants
in `tools/ci-fleet/cifleet/store.py`. Metrics are labelled by pool, host,
runner, class and severity only.

### Alerts

```yaml
- alert: CIJobsUnsatisfiable
  expr: ci_fleet_unsatisfiable_jobs > 0
  for: 2m
- alert: CIPoolOffline
  expr: ci_fleet_pool_runners{state="registered"} > 0 and ci_fleet_pool_runners{state="online"} == 0
  for: 10m
- alert: CIFleetCollectorDown
  expr: up{job="ci-fleet"} == 0 or ci_fleet_forgejo_up == 0
  for: 5m
- alert: CIFleetAgentSilent
  expr: ci_fleet_agent_age_seconds > 120
  for: 5m
```

### Files

| Path | What |
|---|---|
| `validation/runner_fleet.py` | Roster, workflow jobs, label sets, cron — shared with the contract tests |
| `tools/ci-fleet/cifleet/forgejo.py` | Paged forge client, job-log placement |
| `tools/ci-fleet/cifleet/fleet.py` | Snapshot builder and why-waiting classifier (pure) |
| `tools/ci-fleet/cifleet/collector.py` | Poll loop, agent intake, controls, HTTP API |
| `tools/ci-fleet/cifleet/store.py` | Bounded SQLite history |
| `tools/ci-fleet/cifleet/agent.py` | Host/guest agent (Python 3.9+, Linux and macOS) |
| `tools/ci-fleet/cifleet/metrics.py`, `explain.py` | `/metrics`; plain-text answers |
| `tools/ci-fleet/ui/` | The console (static HTML/JS/CSS, no build step) |
| `scripts/ci-fleet` | CLI for people and agent sessions |
| `deploy/ci-fleet/` | systemd units, LaunchDaemon, installer, example configs |
| `tests/operations/test_ci_fleet.py` | Classifier on captured 2026-11-01 responses, paging, store bounds, agent, collector |

## Not done yet

- **Live agent verification.** The cgroup-to-task tie is proved by tests on
  fixtures; on hardware it is proved once agents are installed (one Incus guest
  on lab6, one host-mode runner on lab5): the task id an agent reports must
  equal the forge's placement.
- **Concurrency holds.** HELD is inferred from a blocked job whose needs all
  finished; how Forgejo 16 reports a job-level concurrency hold has not been
  observed live.
- **Overlap with the existing runner exporter.** Runner hosts already run
  `plurx-ci-runner-metrics` (a five-minute timer outside this repository that
  writes runner up/down and janitor health to the lab Prometheus through a
  textfile). ci-fleet's `ci_fleet_runner_up` and the agents' janitor receipts
  overlap those series; the queue, why-waiting, pools, placement, drift and
  per-job usage are new. Once ci-fleet is installed, retire the overlapping
  series in one place or keep both deliberately — Paul's call.
- **`scripts/ci-failures`** still reads logs over SSH on the old assumption
  that the log API is broken; on 16.0.3 it works, so that path can go.

## Execution log

| Date | Model · session | Step | Result |
|---|---|---|---|
| 2026-11-01 | claude-opus-5-5 · 01BCWL9y | Proposal, renderings, Fable review, revision | Approved by Paul to build |
| 2026-11-01 | claude-opus-5-5 · 01BCWL9y | M0–M3 built on `effort/ci-fleet` | PR #1344 (draft) |
