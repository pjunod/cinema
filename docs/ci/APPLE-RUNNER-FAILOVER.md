# Apple runners — the M4 primary and M3 Max standby

Companion to [CI_TEST_OVERHAUL_PLAN.md](CI_TEST_OVERHAUL_PLAN.md) (what Apple
CI verifies) and [RUNNER-DISK.md](RUNNER-DISK.md) (runner disk maintenance).
This document owns Apple runner preference, outage detection and installation.

## The M4 accepts Apple jobs while it is available

| Role | Forgejo runner | Host | Account | Toolchain |
|---|---|---|---|---|
| Primary | `gha-maca-apple-01` | M4 MacBook Air, `192.168.5.115` | `githubrunner` | Xcode 27.0, `27A266a` |
| Backup | `gha-macb-apple-01` | M3 Max MacBook Pro | `pjunod` | Xcode 27.0, `27A266a` |

Both carry `self-hosted`, `macOS`, `ARM64`, `lab`, `apple`, and `xcode-27`.
The primary also carries `primary`, and the backup carries `backup`.
Forgejo matches labels; those last two labels describe the roles and do not
make its scheduler prefer one runner. The
[standby controller](../../deploy/apple-runner/plurx-apple-standby) enforces
preference by starting the backup daemon only during an outage. Apple jobs
continue to request the common label set.

The controller probes the primary over existing `pjunod` SSH access, with
strict host-key verification and no interactive authentication. The
[primary health command](../../deploy/apple-runner/plurx-apple-primary-health)
requires the M4 runner's launchd service to be running, the active Xcode to
match the workflow pin, and its host to reach Forgejo. A busy primary remains available: the backup never starts merely to
shorten a queue.

## Three failed probes enable the backup; two successful probes drain it

The [configuration](../../deploy/apple-runner/standby.json) waits 20 seconds
between probes and bounds each probe to 12 seconds. Three consecutive failures
start the backup. Two consecutive successes send one `SIGTERM` to it, which
stops polling and permits an active job to finish. The runner's
`shutdown_timeout: 3h` bounds that drain; the controller never force-kills a
job. If the primary fails again during a drain, the existing job finishes
before a new backup daemon starts.

```text
 M4 service running + Forgejo reachable ──▶ M3 standby, no job polling
 three consecutive failed probes       ──▶ M3 starts one Apple job slot
 two consecutive successful probes     ──▶ M3 stops polling, finishes its job
 backup daemon exits                   ──▶ standby, or restart during outage
```

**Limits:** availability is observed from the M3, not from Forgejo's runner
database. A network partition between the two Macs can enable both runners
even if the M4 can still reach Forgejo. A running daemon with invalid
registration credentials can also appear healthy; investigate Forgejo's
runner page if jobs queue despite a successful probe. This setup does not
transfer an interrupted M4 job to the M3. New queued jobs can use the backup;
the interrupted run still follows Forgejo's own failure/retry policy.

## Install the health command on the M4, then the controller on the M3

Install `plurx-apple-primary-health` as root-owned mode `0755` in
`/usr/local/bin` on the M4. Its existing launchd service retains the historical
label `org.forgejo.actions.runner.plurx.gha-mba-apple-01` even though the runner
display name is `gha-maca-apple-01`.

On the M3, stop any directly launched Forgejo daemon and its Homebrew service
before installing the controller. The installer refuses a duplicate daemon.
The existing private runner configuration and token remain in their current
locations; this installation contains no credential.

```bash
sudo deploy/apple-runner/install                 # install the system service
/usr/bin/python3 /usr/local/bin/plurx-apple-standby \
  --config /usr/local/etc/plurx-apple-standby.json --probe  # read primary health
sudo launchctl print system/tv.plurx.apple-standby # verify the controller runs
tail /opt/homebrew/var/log/plurx-apple-standby.log  # read transitions
```

**How to read it:** `primary available` means the primary health command
succeeded. `starting backup daemon` means the third failed probe enabled
polling. `draining backup` means new polling is stopping; a job may still be
finishing. Forgejo shows the backup offline while it is deliberately in
standby. The controller runs as `pjunod`, as authorized for the backup runner;
CI jobs can read that account's files. A system LaunchDaemon survives logout
and reboot while the Mac is awake. It does not prevent laptop sleep.

The [focused tests](../../tests/operations/test_apple_runner_standby.py)
exercise outage/recovery thresholds, busy-primary behavior, daemon restart,
and an active process that finishes its job after a drain signal:

```bash
python3 -m unittest tests.operations.test_apple_runner_standby
```
