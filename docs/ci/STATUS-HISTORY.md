# CI, build and validation — status history

**Status:** done · records moved verbatim from `STATUS.md` on 2026-09-24

**Moved here from [STATUS.md](../../STATUS.md) on 2026-09-24**, verbatim, by
[LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md](../ci/LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md)
M6. Each section keeps its original heading under the date it was first
recorded in `STATUS.md`; relative links are re-based to this folder and
nothing else changed. Newest first. These are records: a section here
describes the state on the day it was written, and
`tests/operations/test_status_pr_claims.py` keeps holding it to the same
merged-pull-request rule it held in `STATUS.md`.

## 2026-09-17 · The twenty red tests, and the three live defects three of them were reporting

**[#356](http://forge.lan:3000/noirr/plurx/pulls/356) from
`fix/red-suite-repair` into `main`; adversarial review done (1 blocker, 5
should-fix, 5 notes) and every finding folded or answered.**
`main` was carrying twenty failing tests across `plurxd` and `plurx-core`; all
twenty are green here. Three of them were reporting live production defects
that the inventories around them had stopped being able to show.
**`requeue_cluster_fragment_index` bound twelve parameters against `?13`**, so
rusqlite refused the statement before any I/O and its one caller discards the
error — a holders-unavailable artifact has never been requeued for rebuild on
the SQLite backend. **The forced-supersede guard ignored the incoming
identity**, so the blank-identity force the admin Force rebuild button sends
was blocked by any pre-upgrade row, answering "analysis source changed or the
active request queue is full" and doing nothing; the review then found that
admitting it also admitted a *second* one, so that is fenced too. And
**`Spawn::ffprobe` never piped stdout** — `wait_with_output` collects only what
was piped, so every colour tag the pipeline probe read was `""`, which reads as
"not BT.709", which fails the reference run: **no GPU tone-map graph has ever
been validated on any node**, and every HDR transcode falls back to software
x264 with a validated QSV or VAAPI encoder idle. Measured on `media1` as a
1080p rung presenting at 83% of realtime. The proof was in the container log
the whole time — the three correct `color_*=bt709` lines printed immediately
above "the CPU tone-map reference did not run". Two narrower ones: a response
that came back out of a durable settlement was stamped afterwards, so an answer
and its own replay were different bytes (and the review caught that the fix let
an exchange dispatch a successor while answering `none`, which is a hard reopen
mid-film — dispatch is now gated on composing your own body); and the library
badge read the legacy refusal, which carries `Truncated` for every terminal
code that is not literally `Unsupported`, badging permanently unindexable files
as pending forever. The censuses are restored rather than relaxed: ten SQLite
transaction boundaries and a module named, the migration ladder at 63, the
sidecar at v9, the activity payload's base `dvr` key, and a content seed that
records the 35 bytes it writes instead of 42 — which is what every direct play,
range read and Plex part read of that fixture was answering 404 against. Four
new regressions cover the rules that had none: the length check, the ffprobe
pipe, and both directions of the blank-identity force.

## 2026-09-17 · `make install` is one command on every platform

**[#349](http://forge.lan:3000/noirr/plurx/pulls/349) from `feat/make-install`
into `main`; adversarial review done (3 blockers, 14 should-fix) and every
blocker plus the should-fix set folded; merging once the PR fan-out is green.**
`make install` detects the host and hands off to `deploy/install`: a sandboxed
systemd unit with a dedicated `plurx` user on Linux, a LaunchAgent in the
login session on macOS (VideoToolbox stays available), or the native Windows
service through `deploy\install.ps1`, which elevates itself. `make
install-docker` makes the first Compose run one command; `make install-binary`
puts `plurxd` on `PATH` with no service; `make uninstall` / `uninstall-docker`
reverse them and keep data. Every path builds the same `--locked` release
binary or takes a prebuilt one, installs ffmpeg when it is missing (beside
`plurxd.exe` on Windows, because LocalSystem does not share the user's
`PATH`), runs only the privileged steps through sudo, upgrades in place
without ever overwriting a unit or plist the operator edited, and ends by
waiting for `/readyz` and printing the version the server reports — an
install whose server never answers exits non-zero and names the log.
`tests/operations/test_install.py` (21 cases) executes each path against
stubbed host tools; the Linux path was also run for real as root in a
container (fresh, upgrade, uninstall, readiness timeout). Not run here: a real
launchd bootstrap on a Mac and a real Windows service install — both are
hand-offs in the PR body.

## 2026-09-13 · `make web-check` is green, and `rust-gate` has actually been run

**[#286](http://forge.lan:3000/noirr/plurx/pulls/286), WIP, not merged.**
Two gaps the playback surface effort left behind: its own acceptance command
could not go green, and its Rust gate had never been executed at all.

### `make web-check`

Three tests were red on `main`, none of them for a defect in what the page
does.

- **`layout-containment`** — four bare `1fr` content columns, all four from
  `78c48a95`: at the branch point they are lines 3204 and 3209 of
  `index.html`, and `git blame 3062f3c9` gives that commit for both. (Line
  3210 is `</style>`, from `a9cca3dc`; on this branch the `@media` line sits
  at 3210 because of the rule added above it, which is an easy off-by-one to
  blame against the wrong tree.) `1fr` is `minmax(auto, 1fr)`: content sets
  the track's minimum,
  so a long programme title in `.lc-programme` widens the middle column and
  pushes the "Watch from start" button off the card, and the under-760px
  overrides for `.lc-editor`, `.lc-grid` and `.lc-programme` put that failure
  where the viewport is narrowest. Fixed in the CSS with `minmax(0,1fr)` — the
  floor the other thirty-odd grid declarations in this stylesheet already use,
  and the one that also drops the grid item's automatic minimum size, which a
  track-only fix leaves behind. The title cell gets `min-width:0` and
  `overflow-wrap:anywhere` for the same reason `.specs dd` has them.
- **`page-read-budget`** and **`settings-sections`** — `ReferenceError` in
  both, and the shipped page is correct in both. `shippedSource(name)` hands a
  harness one function, sliced declaration-to-declaration, so the harness must
  declare everything that function calls. `78c48a95` added
  `clearLibraryChannelDraft()` and `LIBRARY_CHANNEL_TUNE.stop()` to
  `clearLocalSession`, and `583bcd1d` added `playbackSurfaceReadinessCard()`
  to `developerPanel`, neither with the stub its harness needed. The two
  sign-out collaborators are stubbed as observable state rather than as
  no-ops — so the test now asserts that signing out really does drop the
  unsaved wizard draft and the tune fence, and deleting either call from
  `index.html` fails it — and the readiness card is evaluated from the shipped
  source rather than stubbed, with an assertion that Developer renders it.

Every test in the target passes afterwards. The target is sixteen node tests
plus `scripts/js-check` and `scripts/contrast-check` (`Makefile:1413`) — it
does **not** contain either fence, so `scripts/playback-surface-fence` and
`scripts/player-input-fence` were run separately; both PASS, with no new
`MIGRATION_BUDGET` entries.

### `rust-gate`, at last

Archived out of the clone and compiled in a container on the pinned `1.97.1`,
the `COMPILE-LOOP.md` route. Run twice, at two shas:

- **`3062f3c9`** — the branch point, and the first sha at which all seven
  playback-surface PRs are in.
- **`ec1c324f`** — this branch's second commit, and the last one that touches
  compiled or embedded source. The branch tip is `08aeedc7`, which adds only
  this `STATUS.md` entry, so the gate result carries to it unchanged.

`main` has since moved on to `a64e28ff`, but `git diff 3062f3c9..a64e28ff --
'*.rs' 'Cargo.toml' 'Cargo.lock'` is empty: no Rust and no dependency has
changed, so this result still describes current `main`.

Identically at both shas: `cargo fmt --all --check` **clean**, `cargo clippy -p
plurxd --all-targets -- -D warnings` **clean**, `cargo test -p plurxd --bin
plurxd` **2192 passed, 18 failed, 6 ignored**.

**Nothing in the playback surface work broke a Rust test.** `index.html` and
`playback-policy.js` are `include_str!`'d into the binary and asserted on by
`http::web::tests`; every one of those passes, before and after the CSS change
here. The 18 are identical on both shas, so this branch introduces none of
them:

- 13 `decode_facts` tests fail at `Spawn("Function not implemented (os error
  38)")` before any assertion — the bound-exec probe's `pre_exec` builds a
  Landlock ruleset and `landlock_create_ruleset` is `ENOSYS` in that
  container, confirmed directly. Environmental.
- `live_tv::one_tuner_get_runs_the_full_hls_lifecycle_and_stop_waits_for_cleanup`
  says "FFprobe is not configured on the tuner owner"; `playback_control::copy_retry_is_unsupported_only…`
  and `vodserve::a_capacity_stall_still_says_no_room_after_its_producer_is_gone`
  both need a real producer process to reach a particular exit. Environmental.
- **Two `live_tv` tests are deterministically red on `main`** — pure in-memory
  assertions, no I/O, so they fail on any machine.
  `live_tv_software_hls_argument_baseline_is_stable` freezes an expected
  FFmpeg argument list containing `bwdif=mode=send_frame:…` and an ordering
  the code no longer emits: the fixture sets `deinterlace: false` so `bwdif`
  cannot appear, and the shipped filter says `send_field`.
  `live_hls_publishes_short_startup_segments_before_steady_cadence` looks for
  `-force_key_frames` in `LIVE_HLS_OUTPUT_ARGS`, which is a 10-element array
  ending at `-hls_delete_threshold`; the flag moved into the encoder block.
  **Both are left alone on purpose** — fixing either means editing a frozen
  Live TV FFmpeg baseline, and neither is playback-surface work. Reported, not
  changed, and still red.

### A trap for the next session: `TMPDIR`

The device VM's `/sessions` — its default `TMPDIR`, and where the clone's own
`$HOME` lives — is **100% full**. Nothing warns you. Two things follow:
`tests/playback/network-shaping.test.js` reports "12 shaping contract
failure(s)" there and "93 shaping contracts hold" with `TMPDIR` pointed
anywhere else, and detached `nohup` jobs die without a message. Export
`TMPDIR` off `/sessions` before believing any red result on that machine.

## 2026-09-07 · The CI fleet filled up because the bound was behind a flag nobody set

**Merged into `main`; the last of it is `5b30eb92`.** Runners kept running
out of disk. The failure never says so: a runner that fills mid-link reports
`ld terminated with signal 7 [Bus error]`, which is what a miscompile looks
like, and the real `No space left on device` is hundreds of lines further down.

A Forgejo runner serves `actions/cache` out of its own `cache.dir`, and
`forgejo-runner` 13.1.0 has **no eviction for it at all** — its own
`generate-config` offers `enabled`, `dir`, `host`, `proxy_port` and the shared
secrets, and nothing else. Everything written there is permanent. This
repository already owned a bounded alternative for both consumers that write
there — an LRU Cargo cache under a 30 G budget, a named BuildKit builder pruned
to 50 G — and both were gated on `persistent-eligible: true` *and*
`CI_EXECUTION_MODE` in {shadow, accelerated}. The variable has never been set
on this repository. So the condition was false on every job ever run, every job
took the unbounded branch, and nothing in the fleet was bounded by anything.

Measured 2026-09-07 on `gha-lab6-general-01`: 118 cache entries, 41 G, every one
created in the previous three days — about 13 G/day on a 78 G disk, which fills
a runner guest in a week. Fleet-wide the cache servers held ~167 G, and media1
carried another 101 G of Docker images (4 of 131 in use) and 58 G of BuildKit
cache. ~340 G was reclaimed by hand the same day: Docker build cache and
dangling images on media1, lab6 and lab4, and the cache servers of the five idle
runner guests reset index-and-blobs together while each was stopped.

The branch removes both gates — where a cache lives follows the runner, not a
rollout flag — and fixes a second defect the flag had been hiding: the pruners'
reserve was a flat 100 GiB, which is larger than the 78-97 GB Incus guests, so
`healthy` could never become true there and the pruner would have deleted every
cache it was permitted to and failed the job anyway. It is 20 % of the
filesystem now, with a floor that cannot exceed a quarter of it. What a job
still cannot bound, it reports: `scripts/ci-runner-cache-audit` writes the
cache server's size and entry count into every Cargo lane's summary and warns
by runner name past 20 G, because deleting from that directory means stopping
the runner and a job cannot stop the runner it is running on.

That last part is `deploy/runner-janitor/`: a script, a systemd unit, an hourly
timer and a one-command installer, on the same numbers — 20 G budget, 20 %
reserve, graceful stop before any delete. It never resets a working runner,
never leaves one stopped, and refuses any `cache.dir` that is not one; each of
those is mutation-proven. Installed and running hourly on media1, lab5, every reachable
Incus runner guest, and the Lima VM that carries `gha-macb-linux-arm-01` — which
was holding 24G of its own and gave all of it back on the first pass.
`deploy/runner-janitor/macos/` is the launchd equivalent for
`gha-maca-apple-01`, the one runner with no systemd, and it is installed and
verified there: it read the runner's label and config out of the launchd plist,
unloaded the daemon, reset a 4 G cache and loaded it again, and the runner was
back `idle` in Forgejo twenty seconds later. **The cache is bounded fleet-wide.**

**The band between two bounds, found and closed 2026-09-08.**
`gha-lab4-general-01` refused two jobs in a row — tasks 3753 and 3780 — with
`::error::gha-lab4-general-01 is out of disk: 18G available at
/opt/forgejo-runner/_work/<hash>/hostexecutor, need 25G`, identical to the
gigabyte across both. **Not because anything pinned the job there** — that
was this session's assumption and it is wrong: eight runners carry the
`general` label, and `gha-lab6-general-02` ran the same lane green with 42 G
free. A runner that refuses in fifteen seconds returns to idle immediately and
is therefore first in line for the next job, so **a full runner starves the
pool precisely because it fails fast**. That is worth its own fix — the
preflight could hold the slot on refusal so healthy runners win the race — and
it is named here rather than built at the end of a long session. The janitor reported that runner healthy the
whole time, and by its own rule it was: **the preflight refuses a job below
25 G and the janitor reserved 20 % of the filesystem, 15.6 G on a 78 GB guest.**
Between those two figures is a band where the fleet turns work away and the
janitor reclaims nothing, and 18 G is inside it. Two bounds, both satisfied,
nobody minding the gap.

The reserve is now `max(20 % of the filesystem, the preflight's own number)`.
A demand the filesystem cannot meet is **reported and then ignored** rather
than chased: capping it to half the disk was tried first and is worse than
doing nothing, because `min(25 G, half)` *is* half on every volume under
50 GiB — the smallest hosts would carry the most aggressive reserve this
script has ever kept and prune hourly forever after a figure the same message
calls unreachable. A host that cannot free enough for a lane is a placement
problem. That keeps the lesson of the original bug in this same function: a
fixed 100 GiB reserve on a 78 GB guest could never be satisfied, so the pruner
deleted everything it was permitted to and failed anyway.

The janitor is installed standalone per host and cannot read the workflow at
run time, so contract tests hold the figures level instead of an import.
**Two of those guards were decorations and the review caught both** — which is
the more useful part of this entry. The first read the `"${DISK_GB:-25}"`
fallback in the step body, and `action.yml` sets `DISK_GB` unconditionally, so
that literal can never fire and the test stayed green while the governing
default moved; it reads `inputs.disk-gb`'s own default now, proven by mutating
it. The second was a "said once per pass" flag set inside a function that is
only ever called as `$(...)` — a command substitution is a subshell, so the
flag never reached the parent, and the assertion counting the message passed
because the fixture had one runner and no Docker. The reporting moved to the
parent shell and the fixture grew a second runner; the sample receipt in
`docs/ci/RUNNER-DISK.md` has said `"instances":4` all along, so one runner was
never the case to test against.

**The band is closed for the default bar and not for every lane:** `disk-gb`
is per-lane and `vod_web` asks for 45 G, which raising `REQUIRED_GB` cannot
cover because 45 G is over half a 78 GB guest. That lane is named as a known
exception, counted rather than merely named — a second lane copying `45` was
the likeliest way another one appears — and the scan no longer misses a value
hidden behind a trailing comment.

**And the loop that reported success while achieving nothing.** The janitor can
free the cache and Docker; the OS, the toolchains and the 30 G Cargo cache are
not its to take. A host whose freeable bytes are smaller than its gap was being
stopped, wiped cold and left short every hour, with `done:` reporting a
successful reset each time. Free space is re-read after a reset now, the
shortfall is said out loud, and `short_after` is in the receipt — gated on a
reset having actually happened, because `reset: 0, short_after: 1` would have
described a runner that was merely busy and sent an operator to re-provision a
machine that is fine. `demand_dropped` counts the filesystems running on the
percentage rule instead of the reserve configured for them, Docker's own volume
included: that path was silent, and it is the one that runs
`docker image prune -af`. A bound that silently cannot be met is the failure
this whole entry is about, and the janitor had two more of its own.

**A wrong fix was built first, and the way it was wrong is the lesson.** The
error message names a path under `_work`, so `_work` was taken to be the full
directory and a reaper for it was written — script, tests, mutations, the lot.
An adversarial review checked the premise instead of the code: `18G available
at .../hostexecutor` is `df` on the **filesystem**, not a size of that
directory, and the preflight's own diagnostic in the same log lists the whole
checkout at about 58 MB (`20M docs`, `19M crates`, `4.6M brand`). The reaper
was withdrawn. What it would have deleted was never the problem, and it could
not have fired anyway — it sat behind the same 20 % reserve that had already
read healthy at 18 G. The measurement is now in `docs/ci/RUNNER-DISK.md` and in
the janitor's own header, next to the instruction to measure `_work` before
writing anything that deletes from it.

**Two things the same review found in code that was already merged.** A failed
`rm -rf` — `EBUSY` on a leftover mount, `EACCES` on another uid's file — aborted
the shell under `set -e` before the restart, and a `RETURN` trap does not run
on shell exit, so the janitor could take a runner out of the fleet with nothing
left to bring it back; the operations doc had listed "a failed delete still
ends with the runner up" as a tested invariant, and it never was. It is now.
And the macOS janitor's quiet window — its stricter half of the idle check, and
the only thing standing between a wrong answer and a SIGKILLed Xcode build —
had never been executed by the suite at all, because the fixture had no `_work`
for it to look at. It has a mutation-proven test now, and a faked `stat`,
because BSD `stat -f '%m'` prints an epoch second while GNU `stat` reads `-f`
as `--file-system` and answers a `File: ...` block that bash then evaluates
arithmetically.

## 2026-09-05 · A deploy that refused itself over an unmaintainable pair

**PR [#37](http://forge.lan:3000/noirr/plurx/pulls/37) merged as
`c661d387`.** `make
docker-up` on media1 refused to change a container: the health start period was
the tracked five minutes, and `/srv/plurx/plurx.toml` sets
`install_snapshot_timeout_secs = 1200`, which with the three named startup
phases requires 1,335 seconds. The preflight was right. The design was not: the
two halves of that budget live in different files on different machines — the
deadline in a production TOML this repository never sees, the grace in
`deploy/.env` — and nothing paired them, so the pairing rule existed only in
prose and the first report of a mismatch was a refused deploy on the host.

`make docker-up` now derives the grace from the same resolved deadline the
refusal is computed from, proves that value, and applies the value it proved. A
grace an operator wrote is passed through untouched and still refused by name
if it is too short, because a deliberately short grace is how a permanently
broken build gets reported instead of waited out. Verified on media1 itself:
the same command that refused now reports `health=1335s, snapshot=1200s
(/srv/plurx/plurx.toml)`. `make operations-check` — 185 tests — passes.

## 2026-09-01 · Artwork repair-fence claim flake (same CI job)

**PR [#753](https://github.com/pjunod/plurx/pull/753) — MERGED to main
(`36da0497`, 2026-09-01).** While running #744's acceptance loop, the other
intermittent failure in `replicated store and topology contracts` fired —
`new leader did not exclusively fence artwork repair: []`, previously seen
on the effort-train qualification. Root cause: `claim_artwork_source_repair`
declines with an explicit `fence: None` when the leader's quorum
acknowledgement is older than 1s at the claim instant; on a loaded runner
that instant can fall in a scheduling gap the drill's own successor proof
already tolerates. The drill now retries only that classified no-op while
the same node reports itself leader in the same term, bounded well below
one repair lease; both exclusivity bails carry the node's raft state and
the durable repair row — CI's first capture of that evidence
(`since_last_ack: 1036 ms`, term stable) confirmed the mechanism and
exposed a self-defeating freshness guard in the first cut, fixed before
merge. Two adversarial review rounds; 21/21 local drill runs green; CI
green (one unrelated `activity proof` flake on the first run, green on
re-run — noted for a future look if it recurs). Also merged today:
[#752](https://github.com/pjunod/plurx/pull/752), the RELEASING.md answer
to the writeup's §7 question.
