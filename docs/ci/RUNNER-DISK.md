# Runner disk — what fills a CI runner, and what is bounded

Companion to [CI_EXECUTION_ACCELERATION_PLAN.md](CI_EXECUTION_ACCELERATION_PLAN.md)
(how the lanes are meant to get faster) — this is *where the bytes go, who
deletes them, and what to do when a runner fills anyway*.

Read this before adding a cache to any workflow, and before believing a
`ld terminated with signal 7 [Bus error]` in a CI log.

## A full runner does not report a full disk

It reports a linker crash:

```
error: linking with `cc` failed: exit status: 1
= note: collect2: fatal error: ld terminated with signal 7 [Bus error]
```

The linker was writing when the filesystem filled, and a truncated write
surfaces as SIGBUS. That is the same message a genuine miscompile produces,
so a fleet out of disk looks like a toolchain fault, and the real
`No space left on device` appears hundreds of lines later if at all. On
2026-09-06 three runners hit this in one afternoon and jobs were re-run that
could never have passed.

`scripts/ci-require-disk` runs first in every Cargo lane for exactly this
reason: it fails the job on its first line, names the runner, and lists the
largest directories. It deletes nothing — the work root holds other jobs'
checkouts, and a cleanup that guesses which are finished is how a running job
loses its tree.

## The three things that fill a runner

Measured on the fleet on 2026-09-07, when the whole thing was full:

| Consumer | Where | Measured | Bounded by |
|---|---|---|---|
| Runner cache server | `<runner>/cache` (`bolt.db` + blobs) | 41 G on `gha-m6-general-01`, ~167 G fleet-wide | the janitor timer, and nothing else |
| Cargo caches | `$RUNNER_TOOL_CACHE/plurx-ci/cargo/...` | 30 G budget per runner | `scripts/ci-cache-prune`, before and after every job |
| Docker + BuildKit | `/var/lib/docker` | 101 G images · 58 G build cache on nynuc | `scripts/ci-buildkit-prune` (50 G) and the janitor |

### The cache server evicts nothing. Ever.

A Forgejo runner serves `actions/cache` itself, out of the directory named by
`cache.dir` in its `config.yml` — a `bolt.db` index beside a tree of blobs.
`forgejo-runner` 13.1.0 has **no eviction of any kind** for it. Its own
`generate-config` offers `enabled`, `dir`, `host`, `proxy_port` and the shared
secrets, and nothing else: no size cap, no TTL, no garbage collection. Every
entry the server is ever handed is kept until somebody with root deletes it.

So the rule that follows is not a preference:

> **Nothing that grows per commit may be written to the runner cache server.**

`gha-m6-general-01` is what breaking that rule looks like: 118 entries, 41 G,
every one created in the previous three days, on a 78 G disk — about 13 G/day,
which fills a runner guest in a week. The entries were `Swatinem/rust-cache`
tarballs of the target directory (~380 M–1.1 G each) and `type=gha` BuildKit
layer blobs.

### Where the caches live now

The choice follows the runner, never a configuration variable:

```
 RUNNER_ENVIRONMENT == github-hosted? ── yes ──▶ hosted cache service
        │ no                                     (evicted by the service)
 RUNNER_TOOL_CACHE set? ───────────── no ───▶ hosted cache service
        │ yes
        ▼
 runner-local, bounded:  $RUNNER_TOOL_CACHE/plurx-ci/cargo/<runner>/rust-<v>/
   · CARGO_HOME and CARGO_TARGET_DIR both live here
   · LRU-pruned to 30 G before the job and again after it
   · under a filesystem floor of 20 % of the volume
 and BuildKit:  a named `plurx-<runner>` builder, pruned to 50 G per job
```

This used to be gated on `persistent-eligible: true` *and*
`CI_EXECUTION_MODE` being `shadow` or `accelerated`. That variable has never
been set on this repository, so the condition was false on every job and every
job took the unbounded branch — while a complete bounded implementation sat in
the tree behind a flag nobody had set. **A bound a configuration variable can
switch off is not a bound.** The inputs are gone; there is one path per runner
kind and both are bounded.

## The floor is a share of the disk, not a number

`ci-cache-prune` and `ci-buildkit-prune` both keep a reserve free: **20 % of
the filesystem**, with a 10 GiB minimum that is itself capped at a quarter of
the filesystem.

It was a flat 100 GiB. The runner fleet is not all large disks — the Incus
guests are 78–97 GB volumes — so on those the reserve was larger than the
entire filesystem, `healthy` could never be true, and the pruner would delete
every cache it was permitted to delete and then fail the job anyway. No cache
and no build, which is worse than either.

## How to read a job's cache report

Every Cargo lane writes two blocks into the job summary.

**`### Runner cache server`** — the directory nothing evicts.
`retained: N GiB in M entries` is the number to watch, and `oldest entry` says
how far back it goes. Under ~20 G on a 78 G guest is healthy; at 20 G the
audit raises a `::warning::` naming the runner, and that runner wants a janitor
pass. The audit never deletes: index rows and blobs have to go together —
remove a blob and leave its row and the next job matching that key is told the
entry exists and then fails to download it — and removing them together means
stopping the runner, which a job running *on* that runner cannot do.

**`### Cargo cache`** — `prune decision: within-budget` means nothing was
deleted. `pressure` means the budget or the floor was breached and the pruner
evicted, oldest toolchain first, then least-recently-used lane, then the
registry caches. Repeated `pressure` on the same runner is a real signal: the
budget no longer fits the work, or something outside the budget is eating the
volume.

## Running out anyway — the operator's pass

Read the number first, then act on the largest one. All of it needs root on
the runner host; the Incus guests are reachable from any cluster member with
`incus exec --project github-runners <guest>`.

```bash
# 1. What is actually on this runner
du -sh /opt/forgejo-runner*/cache /opt/forgejo-runner*/_work /home/runner
docker system df                       # images and build cache, if it has docker

# 2. Docker first: it is the biggest and the safest. Running containers keep
#    their images; the build cache is reclaimable in full.
docker builder prune -af
docker image prune -f

# 3. The cache server, only while that runner is idle. Index and blobs go
#    together or the next job is promised an entry that cannot be downloaded.
systemctl stop forgejo-runner-<name>.service   # graceful: finishes a live job
rm -rf /opt/forgejo-runner/cache
systemctl start forgejo-runner-<name>.service
```

Check idle before step 3 — `status` on the runners API, not a guess:

```bash
curl -s -H "Authorization: token $TOKEN" \
  http://192.168.4.7:3000/api/v1/repos/noirr/plurx/actions/runners \
  | python3 -c 'import json,sys; [print(r["name"], r["status"]) for r in json.load(sys.stdin)]'
```

Step 3 costs the next few jobs a cold cache. That is the whole cost; there is
no data in that directory that is not reproducible.

## The janitor is the permanent half

Everything above is enforced by jobs, and a job only enforces it on the runner
it happened to land on. The cache server, the Docker image store, and a runner
that has stopped taking jobs need something that runs whether or not CI does.
That is [`deploy/runner-janitor/`](../../deploy/runner-janitor/): a script, a
systemd unit, an hourly timer, and a one-command installer.

```bash
sudo deploy/runner-janitor/install   # copies, enables the timer, ends in --dry-run
```

Run it on every runner host, and inside every runner guest — an Incus guest is
reachable from any cluster member as
`incus exec --project github-runners <guest> -- …`. It is idempotent; running
it again upgrades the script in place. The ARM runner is a Lima VM on a Mac and
runs systemd, so this is the right installer for it too:
`limactl shell plurx-ci-arm -- sudo bash …`.

**The Apple runner has its own**, in
[`deploy/runner-janitor/macos/`](../../deploy/runner-janitor/macos/): same
numbers, same three invariants, launchd instead of systemd.

```bash
sudo deploy/runner-janitor/macos/install
```

Verified on `gha-mba-apple-01` on 2026-09-07: it read the runner's label and
config out of the launchd plist, measured 4 G, unloaded the daemon, reset the
directory and loaded it again, and the runner was back `idle` in Forgejo
twenty seconds later.

One difference there is not cosmetic. `systemctl stop` drains — the Linux
installer raises `TimeoutStopSec` to thirty minutes so that it can — and
`launchctl bootout` does not: SIGTERM, then SIGKILL about twenty seconds later,
which on that runner is a killed Xcode build. So on macOS the idle check is the
whole safety mechanism rather than a courtesy, and it is stricter: the daemon
must have no child processes **and** its work root must have been untouched for
two minutes.

**What a pass does.** For every `forgejo-runner*.service` on the host it reads
that runner's own `config.yml` for its `cache.dir`, and if the directory is
over the 20 G budget *or* its filesystem is under the 20 % reserve, it resets
it: stop the runner, delete the directory whole, start the runner. In steady
state it decides `within budget` and deletes nothing. Then, only if the Docker
filesystem is still under its reserve, it prunes stopped containers, images
unused for two weeks, and build cache older than a week — not all build cache,
because the named `plurx-<runner>` builders are kept warm on purpose.

**Why the whole directory.** `bolt.db` is the only thing that knows which blob
belongs to which key, and it is not a format a shell script should edit.
Removing index and blobs together is always consistent; removing one without
the other promises the next job an entry it cannot download. The cost is a cold
cache for the next few jobs, and nothing in that directory is not reproducible.

**Three invariants, each with a test.** It never resets a runner that is
working — idleness is "the unit's cgroup holds nothing but the daemon" on Linux
and "the daemon has no child processes" on macOS, a local answer either way
that needs no API token. It never leaves a runner stopped: the restart
is on a `RETURN` trap, so a failed stop or a failed delete still ends with the
runner up. And it refuses any `cache.dir` that is not a runner root ending in
`cache` and holding `bolt.db` — the delete is a whole directory, so the path
check *is* the safety argument.

**What it reports.** Every pass writes a line per runner to the journal and a
record to `/var/lib/plurx-ci-janitor/last-run.json`:

```json
{"finished":"2026-09-07T22:00:04Z","host":"nynuc","instances":4,
 "over_budget":1,"reset":1,"reclaimed_gb":16,"docker_pruned":false,
 "budget_gb":20}
```

`over_budget` and `reset` are deliberately separate: a runner that is over
budget every hour and never idle enough to reset is a real condition, and it
should be visible rather than silently skipped forever.

```bash
systemctl list-timers plurx-ci-janitor.timer     # when it next runs
journalctl -u plurx-ci-janitor -n 50             # what the last passes decided
cat /var/lib/plurx-ci-janitor/last-run.json      # the last pass, in one line
plurx-ci-janitor --dry-run                       # decide out loud, delete nothing
```

The installer also drops `TimeoutStopSec=30min` onto every runner unit. Without
it systemd kills a running job ninety seconds into a graceful stop, and the
janitor becomes the thing that breaks CI.

## What this deliberately does not do

- **No cleanup of `_work`.** It holds other jobs' checkouts on a shared
  runner, several repositories deep, and there is no reliable signal from
  outside a job that a tree is finished. It is also small — under 2 G on every
  runner measured. Not worth the risk it carries.
- **No deletion from a job.** A job cannot stop the runner it runs on, and
  partial deletion from the cache server is worse than none.
- **No cache in the workspace.** `CARGO_TARGET_DIR` deliberately lives outside
  `$GITHUB_WORKSPACE`, so `actions/checkout` never has to delete 21 G of build
  output, and so the budget can be enforced across jobs.
- **No pinning of lanes to hosts by disk size.** The floor is proportional, so
  a small guest bounds itself; a roster that encoded disk sizes would go stale
  the first time a volume was resized.

## Kept honest by

`tests/operations/test_ci_cache.py` — that the runner-local path is what a
self-hosted runner takes, that no rollout input can move it, that the floor
stays satisfiable on a 78 GiB volume, and that the audit reports and never
deletes. `tests/operations/test_ci_janitor.py` — that the janitor leaves a
cache inside its budget completely alone, resets an over-budget one whole,
never touches a working runner, never leaves a runner stopped, and refuses a
`cache.dir` that is not one. `tests/operations/test_contracts.py` pins the same
shape per lane.
