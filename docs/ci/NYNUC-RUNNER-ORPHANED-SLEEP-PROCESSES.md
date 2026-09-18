# Nynuc runner orphaned sleeps — evidence and cleanup boundary

**Status:** open investigation · **Observed:** 2026-09-08 · **Scope:** the four
Forgejo runner services on `nynuc`

Companion to [VALIDATION.md](../VALIDATION.md) (what CI gates) and
[CI_EXECUTION_ACCELERATION_PLAN.md](CI_EXECUTION_ACCELERATION_PLAN.md) (runner
capacity and isolation) — this note records the stopped processes found in the
`nynuc` runner cgroups, what they do and do not explain, and the safe boundary
for a later cleanup and monitoring change.

## Finding — 50 stopped orphan processes remain in runner cgroups

Two read-only inspections on 2026-09-08 found the same 50 processes. Every
process had command name `sleep`, process state `T`, and parent PID 1. Sampled
members reported the command line `sleep 1.5`.

`T` means stopped by job control or a signal; it is not the ordinary sleeping
state `S`. Parent PID 1 means the process that created the helper has exited and
the host init process has adopted it. The processes still belong to their
original systemd runner cgroups.

| Runner service | Stopped orphan count | Oldest observed age at the second check |
|---|---:|---:|
| `forgejo-runner-gha-nynuc-general-01.service` | 26 | 21h 16m |
| `forgejo-runner-gha-nynuc-general-02.service` | 14 | 20h 52m |
| `forgejo-runner-gha-nynuc-general-03.service` | 6 | 17h 35m |
| `forgejo-runner-gha-nynuc-general-04.service` | 4 | 18h 55m |
| **Total** | **50** | |

The counts did not grow between the two observations. They also did not fall,
so neither normal job completion nor the janitor's ordinary hourly pass had
cleaned them up.

## Service and host evidence — degraded hygiene, not a stopped runner

All four runner units were `active (running)`. Each reported `NRestarts=0`,
`KillMode=control-group`, and zero cgroup OOM kills. The kernel journal showed
no OOM, filesystem I/O, or segmentation-fault event in the inspected
2026-09-08 window.

The cgroups have nevertheless crossed their configured `memory.high` limits
many times:

| Runner | `memory.high` events | `oom_kill` |
|---|---:|---:|
| `general-01` | 8,784 | 0 |
| `general-02` | 10,595 | 0 |
| `general-03` | 7,108 | 0 |
| `general-04` | 4,788 | 0 |

At the first observation the 60 GiB host had 31 GiB available memory, but its
8 GiB swap was effectively full. Current pressure-stall readings were quiet.
Those facts prove historical pressure, not current starvation: a cumulative
`memory.high` counter does not identify which job caused an event.

For the runner originally under investigation,
`gha-nynuc-general-02`, the same snapshot showed:

- service active since 2026-09-07 21:26 UTC, with no restart;
- 107 GiB available on the filesystem containing
  `/opt/forgejo-runner-02`;
- a 4.6 GiB Forgejo cache; and
- no cgroup OOM kill.

## Causality — these processes did not cause PR #156's preflight result

The three PR #156 preflight attempts assigned to
`gha-nynuc-general-02` completed in 62, 69, and 52 seconds. They did not reach
the workflow's three-minute timeout. Each attempt exited on the same six
`test_rolling_producer_ownership_inventory` assertions because source counts
and the reviewed ownership ledger differed.

The orphan processes are therefore a real runner-hygiene defect, but the
available evidence does not make them the cause of those preflight failures.
Likewise, the pairwise shape and stopped state suggest a repeated helper
cleanup gap, but they do not identify the creating test or command. Treat that
origin as unproved until a new process can be correlated with a specific task.

## Read-only inspection — count without changing the host

Run this on `nynuc` to list and count stopped processes in the four runner
cgroups:

```bash
ps -eo pid,ppid,unit,state,etime,comm --sort=unit \
  | awk '$3 ~ /^forgejo-runner-gha-nynuc-general-[0-9]+\.service$/ && \
         $4 ~ /^T/ {
      count[$3]++
      print
    }
    END {
      for (unit in count) print "COUNT", unit, count[unit]
    }'
```

**How to read it:** a healthy idle runner should not retain stopped job helper
processes. A line with `PPID` 1 and state `T` is an adopted, stopped process;
the final `COUNT` lines show the affected service cgroups. This command is
observational only.

Check service state separately:

```bash
systemctl is-active \
  forgejo-runner-gha-nynuc-general-01.service \
  forgejo-runner-gha-nynuc-general-02.service \
  forgejo-runner-gha-nynuc-general-03.service \
  forgejo-runner-gha-nynuc-general-04.service
```

Four `active` lines prove the daemons are running; they do not prove their
cgroups are free of abandoned descendants.

## Cleanup contract — restart only after proving the runner is idle

The user directed the current session to leave these processes in place. A
later cleanup must observe these guardrails:

1. **Prove the exact runner idle before stopping its unit.** A process list is
   not an idle signal; use the same Forgejo task-state check that protects the
   cache janitor.
2. **Restart one runner service at a time.** Its `KillMode=control-group`
   setting makes systemd own every descendant, including the adopted sleeps.
   Do not kill individual PIDs by hand.
3. **Verify both outcomes.** The unit must return to `active (running)`, and
   the stopped-process count for that unit must become zero.
4. **Stop if the count survives.** A surviving process means the cgroup or unit
   assumption is wrong; do not escalate to filesystem deletion or a broad host
   process kill.

No cache directory or runner worktree needs manual deletion for this cleanup.

## Monitoring handoff — distinguish runner faults from failed workloads

The follow-up monitoring change should publish these per-runner signals:

| Metric | Meaning | Initial alert |
|---|---|---|
| `plurx_ci_runner_stopped_processes` | Processes in state `T` inside the runner cgroup | `> 0` for 15 minutes |
| `plurx_ci_runner_orphan_processes` | Runner-cgroup processes whose parent is PID 1 | `> 0` for 15 minutes |
| `plurx_ci_runner_memory_high_events_total` | Cumulative cgroup memory throttling | positive rate over 15 minutes |
| `plurx_ci_runner_oom_kills_total` | Cgroup OOM kills | any increase |
| `plurx_ci_runner_host_swap_used_ratio` | Host swap consumption | `> 0.90` for 15 minutes |

The dashboard should show stopped and orphan counts beside service state, disk
reserve, cache size, and job failures. A failed job is not automatically a
runner fault: runner alerts should be driven by service loss, OOM, I/O errors,
disk reserve, or leaked process state, while job-result panels retain the
workload outcome separately.

## Non-goals — what this note does not authorize

- It does not authorize killing the 50 processes individually.
- It does not authorize restarting a runner while Forgejo has assigned it a
  job.
- It does not attribute PR #156's contract failures to host pressure.
- It does not identify the command that originally leaked the stopped helpers.
- It does not authorize deleting anything under `/opt/forgejo-runner*`.
