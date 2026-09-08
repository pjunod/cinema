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
| Runner work root | `<runner>/_work` (checkouts) | ~58 M on `gha-nuc4-general-01`, 2026-09-08 | nothing, and it does not need to be |
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

**On a host with no checkout**, `deploy/runner-janitor/bootstrap` fetches the
four files from the forge and runs the same installer — **and it needs a token,
which is not optional**:

```bash
sudo PLURX_TOKEN=<forgejo token> bash -euc 'f=$(mktemp); curl -fsSL \
  -H "Authorization: token $PLURX_TOKEN" \
  http://192.168.4.7:3000/noirr/plurx/raw/branch/main/deploy/runner-janitor/bootstrap \
  -o "$f"; bash "$f"; rm -f "$f"'
```

**The shape of that command matters as much as the header.**
`bash -c "$(curl …)"` expands the substitution in the *calling* shell, where
`PLURX_TOKEN` is not set — a `sudo VAR=x` assignment applies only to the
command sudo runs. The header goes out empty, the forge answers 404, `--fail`
makes curl exit 22, the substitution yields nothing, and `bash -c ""` exits 0:
a silent no-op that reports success, holding a perfectly valid token. That
version was written, reviewed and published here before this one, which is why
`test_the_documented_bootstrap_command_works` now extracts this very code block
from this file and runs it.

`noirr/plurx` is private: every raw URL answers 404 to an anonymous request and
200 with that header. The bootstrap shipped without it, so the one-command
install it documented could never have worked on any host — it was published,
handed to an operator, and failed on first use with
`curl: (22) ... 404` followed by `bash: /dev/fd/63: Bad file descriptor`, which
names neither the private repository nor the missing credential. The check that
was supposed to guard it asserted the URL *string* was present in the file,
which it was, correctly, the whole time. `test_the_bootstrap_can_actually_fetch`
now runs the fetch loop against a fake forge that refuses unauthenticated
requests, and against a missing file, because without `--fail` curl writes the
404 body to the destination and exits 0, so the installer is chmod +x'd and
exec'd as root over whatever the forge said. This forge answers `Not found.`,
eleven bytes with no shebang, so that exec fails — but "the install silently
did nothing" is the outcome either way, and nothing about the next endpoint is
promised.

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

**Four invariants, each with a test.** It never resets a runner that is
working — idleness is "the unit's cgroup holds nothing but the daemon" on Linux
and "the daemon has no child processes, and `_work` has been quiet for two
minutes" on macOS, a local answer either way that needs no API token. It never
leaves a runner stopped: the restart is on a `RETURN` trap for a failed stop,
and a failed *delete* is caught rather than allowed to abort the shell — `set
-e` would exit before the restart, and a `RETURN` trap does not run on shell
exit, which is how an `EBUSY` on a leftover mount would have taken a runner out
of the fleet with nothing left to bring it back. It refuses any `cache.dir`
that is not a runner root ending in `cache` and holding `bolt.db` or a blob
tree — the delete is a whole directory, so the path check *is* the safety
argument. **And its reserve is never below what a job is refused for not
having**, which is the invariant that was missing; see below.

**What it reports.** Every pass writes a line per runner to the journal and a
record to `/var/lib/plurx-ci-janitor/last-run.json`:

```json
{"finished":"2026-09-07T22:00:04Z","host":"nynuc","instances":4,
 "over_budget":1,"reset":1,"short_after":0,"demand_dropped":0,
 "reclaimed_gb":16,"docker_pruned":false,"budget_gb":20,"required_gb":25}
```

`over_budget` and `reset` are deliberately separate: a runner that is over
budget every hour and never idle enough to reset is a real condition, and it
should be visible rather than silently skipped forever.

**`short_after` is the field to watch, and it exists because the alternative
was invisible.** The janitor can free the cache and Docker; the OS, the
toolchains and the 30 G Cargo cache are not its to take. A host whose freeable
bytes are smaller than its gap gets stopped, wiped cold and left short — every
hour, with the summary reporting a successful reset each time. The heuristic
cannot predict that from disk size, so the janitor re-reads free space after a
reset and says so when it did not reach the floor. A `short_after` that is
non-zero hour after hour is a host to give more disk or fewer lanes, not a
janitor to tune.

It is only ever counted **after a reset actually happened**. `reset: 0,
short_after: 1` is not a state this script can write, and it should not be: a
runner merely busy at the top of every hour is the normal condition of a
working CI host, and reporting it as short would send an operator to
re-provision a machine that is fine.

**`demand_dropped` non-zero means something on this host is running on the
percentage rule rather than the reserve configured for it** — a demand larger
than half the volume is dropped rather than met (below), which is the original
defect narrowed to one filesystem. Read it as a flag, not as a census: it
counts *checks*, one per runner instance plus one for Docker, so four runners
sharing one short volume read `5` rather than `1`. Deduping by mount point
would cost a `df` per instance to change a number nobody acts on differently.

It is a count and not a reserve figure on purpose. An earlier version reported
the largest reserve seen in the pass, which on a host with one big volume and
one small one read as healthy while hiding the small one entirely. The message
naming the filesystem is printed once per pass; the counter counts every check.

### The reserve has to clear the bar jobs are held to

The janitor keeps a reserve; the preflight refuses a job that has less free
space than `disk-gb` (25 G). Those were two different numbers, and **between
them was a band in which the fleet refused work and the janitor reported every
runner healthy.** 20 % of a 78 GB guest is 15.6 G, so at 18 G free both rules
were satisfied at once — one by turning jobs away, the other by doing nothing
about it.

`gha-nuc4-general-01` sat in that band on 2026-09-08: two jobs refused with
`18G available … need 25G`, identical to the gigabyte across both, because the
job's labels pin it to that guest and a re-push lands on the same disk. So the
reserve is now `max(20 % of the filesystem, REQUIRED_GB)`.

A demand **larger than half the filesystem** is reported and then ignored — it
does not become the reserve. Capping it to half was tried and is worse than
doing nothing: `min(25 G, half)` *is* half for every volume under 50 GiB, so
the smallest hosts in the fleet would carry the most aggressive reserve this
script has ever kept, and prune hourly forever chasing a figure the same
message calls unreachable. **A host that cannot free enough for a lane is a
placement problem, not a pruning one.** The underlying lesson is the original
bug here: a fixed 100 GiB reserve on a 78 GB guest could never be satisfied, so
the pruner deleted everything it was allowed to and failed anyway.

*Larger than* half, not *at least* half: 25 G on a 50 GiB volume is entirely
attainable — the host need only hold under 25 G — and dropping the demand there
would return that host to the 20 % rule while printing that it cannot run the
lane, which would be false.

The janitor is installed standalone on each host and cannot read the workflow
at run time, so the figures are held level by contract tests rather than by an
import. `test_the_janitor_reserve_and_the_preflight_default_are_one_number`
reads `inputs.disk-gb`'s **default**, not the `"${DISK_GB:-25}"` fallback in
the step body — `action.yml` sets `DISK_GB` unconditionally, so that literal
can never fire, and a test reading it stays green while the governing number
moves.

### The band is closed for the default bar, not for every lane

`disk-gb` is a per-lane input and the janitor has one number for the host, so
a lane asking for more than the janitor reserves keeps a band of its own.
**`vod_web` asks for 45 G** — Playwright browsers plus an ffmpeg build on top
of the workspace — and nothing reclaims the 25–45 G gap on its behalf. Raising
`REQUIRED_GB` to 45 would not fix it either: that is over half a 78 GB guest,
so it would be reported and ignored by the rule above. That lane needs a runner
with the room. `test_no_lane_asks_for_more_disk_than_the_janitor_reserves`
names it as a known exception and fails on any new one, so a second such lane
is a decision rather than an accident.

### Two side effects of raising the reserve, stated rather than discovered

- **Docker prunes sooner.** The Docker block calls the same `floor_kb_for`, so
  on a 78 GB host `docker image prune -af --filter until=336h` and the builder
  prune now fire below 25 G free instead of below 15.6 G. Nothing new is
  deleted; an existing deleter fires across a wider range.
- **Cache resets are less rare.** This page records ~13 G/day of accumulation
  on a 78 G disk, so with 25 G reserved rather than 15.6 G, expect the timer to
  reset caches regularly instead of almost never. That is the intent — a cold
  cache costs a few slow jobs, and the band it replaces cost the fleet the
  whole lane — but the timer's own "in steady state it deletes nothing" note is
  now less true than it was.

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
  outside a job that a tree is finished. It is also small: the preflight's own
  diagnostic on `gha-nuc4-general-01` on 2026-09-08 listed the whole checkout
  at about 58 MB — `20M docs`, `19M crates`, `4.6M brand`, and down from there.

  A reaper for it was written and withdrawn on that measurement, and the
  mistake is worth recording because the message invites it: `18G available at
  /opt/forgejo-runner/_work/<hash>/hostexecutor` is `df` on the **filesystem**,
  not a size of that directory. Reading it as one turns "this disk is full"
  into "this directory is full" and builds the wrong fix. Measure `_work`
  before writing anything that deletes from it.
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
