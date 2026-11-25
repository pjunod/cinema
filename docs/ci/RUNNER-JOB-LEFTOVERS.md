# Runner job leftovers — what a job that dies never deletes

**Status:** live · **Reconciled:** 2026-11-25

Companion to [RUNNER-DISK.md](RUNNER-DISK.md). Some per-job state is created
when a job starts and deleted in the job's last step. A job that never
reaches that step leaves it behind in full, under names no later job reuses.
Runner restarts, watchdog VM restarts, cancellations, timeouts, and jobs that
fail because the disk is already full all end that way. Until 2026-11-25
nothing else deleted this state, and every other deleter reported its host
within budget while it filled.

| Leftover | Where | Measured 2026-11-25 | Reclaimed by |
|---|---|---|---|
| Container job workspace | Docker volumes `<32 hex>` and `<32 hex>-env` | 47 G on `gha-lab6-general-02` (one volume 36 G), 13 G on lab5, 6 G on `gha-lab4-general-01` | the Linux janitor, every pass |
| Apple job simulators | the runner user's `CoreSimulator/Devices` | 10 G of `gha-maca-apple-01`'s last 11 G | the macOS janitor, while that user's runners are idle |

## Container job workspace volumes

The runner mounts a container job's checkout and its whole `target/` from a
named volume, plus a `-env` twin, and removes both when the job ends.
[`plurx-ci-janitor`](../../deploy/runner-janitor/plurx-ci-janitor) removes
a pair once it is dangling (no container uses it) and more than an hour old
(`PLURX_JANITOR_ORPHAN_VOLUME_AGE_SECONDS`).

It does this on every pass, whether or not the disk is short and whether or
not a job is running. The Docker reserve pass is skipped while any container
job runs, so on a busy host it never ran, and that is how one guest built up
47 G. The volume pass is safe beside a running job: Docker refuses to remove
a volume any container uses, and a job's container exists from its first
step to its last. The hour covers the seconds between the runner creating a
job's volumes and creating the container that mounts them. Other volumes,
including the named BuildKit `_state` volumes and 64-hex anonymous volumes,
are never touched.

## Apple job simulators

Each Apple job creates `plurx CI iPhone|iPad|Apple TV <run>-<attempt>`, and
`scripts/apple-tv-navigation-test` creates `Plurx navigation isolated`. They
are 1–3 G each, and the job deletes them in its last step. The workflow's
own sweep only takes earlier attempts of the same run. On 2026-11-25 run 9462
failed on a full disk before that step, and its simulators kept the disk full
for the next job.

The [macOS janitor](../../deploy/runner-janitor/macos/plurx-ci-janitor)
deletes simulators with those names, acting as the runner daemon's
`UserName`. It takes only devices whose directory has been untouched for
three hours (`PLURX_JANITOR_SIMULATOR_AGE_SECONDS`), longer than any Apple
job's 120-minute timeout. It also requires every runner daemon of that user to
pass the idle test the cache reset uses (no child processes, `_work` quiet for
two minutes). That test covers every runner plist, including ones the cache
pass skips, and it runs again before each delete. A user with a busy daemon
keeps its simulators until the next pass.

## Reading it

`last-run.json` carries `orphan_volumes` (Linux) and `orphan_simulators`
(macOS), the number removed in the last pass. `--dry-run` names each one it
would remove.
