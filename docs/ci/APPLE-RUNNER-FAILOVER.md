# Apple runners — the M4 primary and M3 Max standby

**Status:** live · **Updated:** 2026-10-26

Companion to [CI_TEST_OVERHAUL_PLAN.md](CI_TEST_OVERHAUL_PLAN.md) (what Apple
CI verifies) and [RUNNER-DISK.md](RUNNER-DISK.md) (runner disk maintenance).
This document owns Apple runner preference, outage detection and installation.

## The M4 accepts Apple jobs while it is available

| Role | Forgejo runner | Host | Account | Toolchain |
|---|---|---|---|---|
| Primary | `gha-maca-apple-01` | M4 MacBook Air, `10.42.5.115` | `githubrunner` | Xcode 27.0, `27A266a`; awaiting 27.1 upgrade |
| Backup | `gha-macb-apple-01` | M3 Max MacBook Pro | `pjunod` | Xcode 27.1, `27A9275` |

The toolchain pin is Xcode 27.1 (`27A9275`). The standby was observed on
27.1 with iOS 27.1 and tvOS 27.0 SDKs on 2026-10-23; the primary still
reports 27.0 and must match the pin before it can serve Apple jobs. This
source change does not install or restart either runner.

Apple jobs keep DerivedData inside their own checkout and reuse it between
that job's build, unit-test and navigation steps. Whole DerivedData is neither
restored nor uploaded by full, effort or main-fast CI: its build database
contains absolute paths, while repeated Forgejo jobs have used different
checkout roots. Path-specific keys would miss those jobs and still upload the
cache. Existing Forgejo cache blobs, Rust caches and tool caches are unchanged.

Generic iOS and tvOS simulator builds still cover arm64 and x86_64. Toolchain
pins, concurrency and promotion gates are unchanged by this cache boundary. This
cache boundary does not establish the cause of a compiler failure or qualify a new
candidate; the existing build and native test checks remain required.
The [workflow regression](../../tests/operations/test_apple_tv_navigation_gate.py)
rejects whole-DerivedData transfer in all three workflows and retains their
build commands, pins and scheduling limits.

The Effort Apple compile budget is 45 minutes because it also
builds both macOS tray architectures before generic iOS and tvOS simulators.
Run 5877 exhausted the old 10-minute budget during the first iOS cold build;
that failure establishes neither iOS nor tvOS compilation. Full CI and main
promotion each allow 120 minutes for the Apple job. Full CI runs iPhone,
iPad and tvOS units before the mandatory 35 native journeys. The native XCTest
summary for fast run 5967 (internal 5996) alone lasted 53 minutes 12 seconds
(34 passes and one
guide timeout); that failed-run timing establishes duration, not acceptance.
Full run 5985 (internal 6014) had passed seven journeys, including the guide
in 124.621 seconds, after its unit and build stages. That observation was made
while the job was active, without a terminal outcome. The former
60-minute full-job budget left insufficient room for this measured pipeline.
The per-test 180/240-second limits and all unit/navigation gates remain.

Both carry `self-hosted`, `macOS`, `ARM64`, `lab`, `apple`, and `xcode-27`.
The primary also carries `primary`, and the backup carries `backup`.
Apple workflows additionally require `xcode-27.1`. Only the backup currently
provides that label; the primary must complete its Xcode upgrade before it
can receive the label and resume Apple jobs. Forgejo matches labels; the role
labels do not make its scheduler prefer one runner. The
[standby controller](../../deploy/apple-runner/plurx-apple-standby) enforces
preference by starting the backup daemon only during an outage. Apple jobs request the common labels plus the exact toolchain label.

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
label `org.forgejo.actions.runner.plurx.gha-maca-apple-01` even though the runner
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
