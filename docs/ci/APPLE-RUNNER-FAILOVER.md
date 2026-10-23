# Apple runners — the M4 primary and M3 Max standby

**Status:** live · **Updated:** 2026-10-23

Companion to [CI_TEST_OVERHAUL_PLAN.md](CI_TEST_OVERHAUL_PLAN.md) (what Apple
CI verifies) and [RUNNER-DISK.md](RUNNER-DISK.md) (runner disk maintenance).
This document owns Apple runner preference, outage detection and installation.

## The M4 accepts Apple jobs while it is available

| Role | Forgejo runner | Host | Account | Toolchain |
|---|---|---|---|---|
| Primary | `gha-maca-apple-01` | M4 MacBook Air, `10.42.5.115` | `githubrunner` | Unverified; offline at the 2026-10-23 inspection |
| Backup | `gha-macb-apple-01` | M3 Max MacBook Pro | `operator` | Verified 2026-10-23: Xcode 27.1, `27A9275` |

The workflow and health-command pin is Xcode **27.1**, build **27A9275**.
The verified backup has iphoneos SDK **27.1**, appletvos SDK **27.0**, and
XcodeGen **2.46.0**. Full promotion still requires the existing iOS 26.5 /
23F77 and tvOS 26.5 / 23L470 simulator runtimes; the SDK upgrade does not
change that runtime contract. The primary's installation and deployment of
the updated health command remain unverified. The backup observed on
2026-10-23 runs a user-level controller at `~/.local/bin/plurx-apple-standby`
with `~/.config/plurx/apple-standby.json`, supervised by the user LaunchAgent
`~/Library/LaunchAgents/tv.plurx.apple-standby.plist` in that account's GUI
domain. The system-service installation paths below were absent. Update that existing supervised deployment rather
than starting a second controller.

Apple jobs require `self-hosted`, `macOS`, `ARM64`, `lab`, `apple`, and
`xcode-27.1`. Grant the new version label only after verifying the selected
installation and runner prerequisites. Keep the unverified primary excluded
from that label until its toolchain and installed health command are checked.
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
match the exact workflow version and build, and its host to reach Forgejo.
Only after all three checks succeed does it print exactly:

```text
Xcode 27.1
Build version 27A9275
```

Both lines end with a newline. The controller requires exit status zero and
stdout exactly equal to the required nonempty `probe_expected_stdout` string
in its configuration (`Xcode 27.1\nBuild version 27A9275\n` in JSON).
It does not trim whitespace or accept exit status alone. A missing, empty or
non-string expectation refuses configuration. An old installed health command
that silently exits zero therefore cannot trigger backup drain. A busy
primary remains available: the backup never starts merely to shorten a queue.

## Three failed probes enable the backup; two successful probes drain it

The [configuration](../../deploy/apple-runner/standby.json) waits 20 seconds
between probes and bounds each probe to 12 seconds. Three consecutive failures
start the backup. Two consecutive successes send one `SIGTERM` to it, which
stops polling and permits an active job to finish. The runner's
`shutdown_timeout: 3h` bounds that drain; the controller never force-kills a
job. If the primary fails again during a drain, the existing job finishes
before a new backup daemon starts.

```text
 M4 service + exact pin + Forgejo healthy ──▶ M3 standby, no job polling
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

## Migrate eligibility and health together before restoring preference

Source changes do not update either installed role. During the 27.1 migration,
keep the primary without `xcode-27.1`; the verified backup can provide Apple
execution while the primary is unavailable. This is degraded backup-only
operation until the primary installation and health deployment are verified.

1. Preserve the installed controller, configuration and health command for
   rollback outside compiler caches. Verify the backup's identity and that it
   is idle; gracefully drain any active job through its existing supervisor
   before unloading the controller. Never force-kill a job.
2. Persist `xcode-27.1` for the verified backup through its existing runner
   registration/configuration mechanism, preserving other labels and private
   credentials. Verify eligibility survives service restart. Do not grant the
   new label to the unverified primary.
3. Install the reviewed [controller](../../deploy/apple-runner/plurx-apple-standby)
   and [configuration](../../deploy/apple-runner/standby.json) together through
   the existing user-level supervision, preserving private connection values.
   Use the system installer below only for a deliberate service migration
   after unloading the old supervisor and draining its child. Record their
   deployed hashes. The new configuration requires the explicit health output;
   an old silent primary or an SSH timeout remains unavailable. Verify one
   backup daemon starts after three failed probes and does not spuriously drain.
4. When the primary is reachable, install the reviewed health command and
   verify its selected Xcode 27.1/build 27A9275, required host prerequisites,
   and exact two-line health output. Only then grant `xcode-27.1`. Two
   successful probes allow the existing graceful backup drain.

Rollback must keep workflow pins, runner labels and health-output expectations
coherent. Restoring the exit-only controller while an old primary can report
healthy for new-pin jobs would restore the premature-drain failure.

## System-service installation — an alternative to the observed user deployment

This recipe installs a system LaunchDaemon; it does not describe the observed
user-level backup deployment. Do not run it alongside that controller. A
migration must first unload the actual existing supervisor after its child
drains, or a second supervisor could duplicate job polling.

Install `plurx-apple-primary-health` as root-owned mode `0755` in
`/usr/local/bin` on the M4 when it is reachable. The health command's launchd
service target has not been verified against the offline primary; compare it
with the actual installed service before restoring eligibility. A runner
display name alone does not establish its service identifier.

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

**How to read it:** `primary available` means the health command exited zero
and its stdout exactly matched the configured toolchain identity.
`starting backup daemon` means the third failed probe enabled polling.
`draining backup` means new polling is stopping; a job may still be finishing. Forgejo shows the backup offline while it is deliberately in
standby. The controller runs as `pjunod`, as authorized for the backup runner;
CI jobs can read that account's files. A system LaunchDaemon survives logout
and reboot while the Mac is awake. It does not prevent laptop sleep.

The [focused tests](../../tests/operations/test_apple_runner_standby.py)
exercise exact output and exit status, old silent health, version/build
mismatch, malformed configuration, timeout, outage/recovery thresholds,
busy-primary behavior, daemon restart, and an active process that finishes its
job after a drain signal:

```bash
python3 -m unittest tests.operations.test_apple_runner_standby
```
