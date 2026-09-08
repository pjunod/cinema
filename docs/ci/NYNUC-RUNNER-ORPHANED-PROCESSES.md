# Nynuc runner orphans — kill the owned group, then prove the child is gone

**Status:** fix in review · **Observed:** 2026-09-08 · **Scope:** the four
Forgejo runner services on `nynuc` · **Issue:**
[#175](http://192.168.4.7:3000/noirr/plurx/issues/175)

Companion to [VALIDATION.md](../VALIDATION.md) for timeout semantics,
[RUNNER-DISK.md](RUNNER-DISK.md) for the idle-runner contract, and
[CI_EXECUTION_ACCELERATION_PLAN.md](CI_EXECUTION_ACCELERATION_PLAN.md) for
runner capacity and isolation.

## Finding — 56 stopped children were retained by completed preflights

The initial inspection found 50 processes. A later 2026-09-08 census found 56,
so this was an active leak rather than residue from one historical failure.
Every process was named `sleep`, had command line `sleep 1.5`, state `T`, parent
PID 1, and remained in one of the four runner service cgroups.

| Runner service | Initial stopped orphan count |
|---|---:|
| `forgejo-runner-gha-nynuc-general-01.service` | 26 |
| `forgejo-runner-gha-nynuc-general-02.service` | 14 |
| `forgejo-runner-gha-nynuc-general-03.service` | 6 |
| `forgejo-runner-gha-nynuc-general-04.service` | 4 |
| **Total** | **50** |

The six later processes were two each from runs 1031 and 1041 on runner 01 and
run 1041 on runner 03. All four runner services remained active with no cgroup
OOM kills. This is a runner-hygiene failure, not proof that a workload failed
because of resource pressure.

## Root cause — the failure-path regression leaked its own fixture

Safe `/proc` fields on all 56 processes identify the same source:

- repository `noirr/plurx`;
- workflow `effort ci`, job `preflight`, action 2;
- a deleted Python temporary directory as the working directory; and
- exactly two stopped children for each affected workflow run.

Action 2 runs the validation catalog unit tests. Its
`test_timeout_cleanup_failure_kills_root_group_and_aborts` regression executes
two subcases whose command is `sleep 1.5`, then deliberately makes process
census fail during timeout cleanup.

`validation.runner._terminate_process_tree` first sent `SIGSTOP` to the new
root process group. When the injected census error prevented normal pidfd
teardown, its fallback called `process.kill()`. That killed only the shell
leader. The stopped child survived in the same group, PID 1 adopted it, and
the test passed because it asserted only that a later marker did not appear.
A permanently stopped child also never writes that marker, so the assertion
mistook a leak for cleanup.

The one-to-one evidence is decisive: two injected cleanup-failure subcases,
two `sleep 1.5` orphans per affected preflight, matching workflow step and
deleted fixture directory on every retained process.

## Fix — retain group ownership through the fatal fallback

The fatal fallback now sends `SIGKILL` to the root process group. It is safe to
use that numeric group at this boundary because `_run_shell` created a new
session whose root group is the owned shell PID, and that leader remains live
and unreaped while fallback runs. The group cannot be reused in that state.

The regression now writes the child PID before sleeping and requires that PID
to be gone or a non-executable zombie after cleanup. The old marker assertion
remains, but no longer stands in for process-lifecycle evidence.

Focused proof on the task branch:

```text
python3 -m unittest \
  tests.validation.test_runner.CatalogCase.\
test_timeout_cleanup_failure_kills_root_group_and_aborts

Ran 1 test in 3.648s
OK
```

## Monitoring and cleanup boundary

The companion observability change publishes per-runner stopped/orphan counts,
`memory.high` events, OOM kills, host swap ratio, and collector freshness from
`nynuc` every minute. Stopped or orphan counts alert after 15 minutes and have
a dedicated Grafana dashboard. This monitoring is in the observability
infrastructure PR and is not represented as live until deployment verifies its
series.

Cleanup is a unit operation, never a PID sweep:

1. Read the repository-scoped Forgejo runner status and require the exact
   runner to report `idle`.
2. Restart one runner service. Its `KillMode=control-group` owns every retained
   child; systemd also continues stopped tasks so its termination signal can be
   handled.
3. Require the unit to return to `active`, its stopped/orphan counts to be zero,
   and its Forgejo registration to return to `idle` before touching the next
   unit.
4. Stop if any count survives. Do not kill individual PIDs or delete runner
   worktrees or caches.

Until the fix reaches the shared effort branch, a new preflight can reproduce
the leak. Cleanup therefore follows the merge and monitoring deployment, not
just the first diagnosis.
