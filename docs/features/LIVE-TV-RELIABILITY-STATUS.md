# Live TV reliability — build status

**Status:** open · **Reconciled:** 2026-09-20

What is built, what is merged, what is deployed, and what is still open on
the Live TV reliability effort. Companion to
[LIVE-TV-RELIABILITY-IMPLEMENTATION.md](LIVE-TV-RELIABILITY-IMPLEMENTATION.md)
(the *what*, exactly) and
[LIVE-TV-GUIDE-AND-START-RELIABILITY.md](LIVE-TV-GUIDE-AND-START-RELIABILITY.md)
(the *why*). Read this one to answer "how far along is it" without asking.

**Lane:** `effort/live-tv-reliability`, merged into `main` at `755195a` ·
**Pull request:** [#281](http://forge.lan:3000/noirr/plurx/pulls/281), merged ·
**Fast lane:** all eight jobs green on `644316f` ·
**Last updated:** 2026-09-13

---

## 1. Where it stands

| Milestone | What it delivers | State |
|---|---|---|
| M0 | `tests/playback/live-tv-start-cases.json`, seven `live.timings` keys, contract embed | built |
| M1 | Durable guide, `age_offset`, `next_refresh_at`, event-driven refresh loop, relay memory, Developer rows | built |
| M2 | Web · Apple · Android poll the guide on the owner's clock | built |
| M3 | `request_id`, retire/resume/start-state, stray eviction, one public start budget, typed envelope, session-end logging | built |
| M4 | Barrier removal on all three clients, from the one fixture | built |
| M5 | Developer-tab enable section, release counters, deploy, physical verification prompt | built |

All five are merged. `main` carries the effort as of `755195a`.

One adversarial review has run against the whole lane (§4). Everything it
found that would bite in production is fixed.

## 2. Deviations from the plan, and why

Recorded as they are made, for Paul to overrule at the end rather than be
interrupted for.

| # | Plan said | Built instead | Why |
|---|---|---|---|
| D1 | One task PR per milestone, each adversarially reviewed (§5) | One lane PR into `main`, reviewed once when it is ready to merge | Paul's session directive: batch commits into a bigger PR, one adversarial review at merge time, fast lane run once. His directive is newer than the plan and he authored both. |
| D2 | `plurx_live_tv_session_ends_total` keyed by `error.code()`, with a clean release "distinguished by the cleanup path" (§3.12) | Keyed by `error.code()` **except** a session with no terminal error, which counts as `released` | A stop, a drain, a shutdown and an idle reap all end with the synthesised `capability_expired`. Counting them under that code buries every real failure in the one bucket an operator looks at first. |
| D3 | `request_id` validated as 32 lower-case hex (§3.6) | Same, and the ingress mints `Uuid::new_v4().simple()` — hyphenless — when a client sends none | So every id on the wire has one spelling. The owner treats it as opaque either way. |
| D4 | `GET /live-tv/starts/{id}` answered by the owner's registry (§3.10) | Its own internal path, `/_internal/v1/live-tv/start-state` | The first build folded it into a `resume` exchange. The review found that `resume` is not a read — it selects one session and cancels the others, touches what it keeps, and retires an id it has never seen — so *looking at* a start fenced it. A status read gets its own operation. |
| D5 | The ingress retires the request id in the background when a start fails (§3.13) | Deleted; the client's hint is the handle | The two rules fought. A not-owner-decided answer tells the client to keep its hint and replay the same id, and the ingress fencing that id underneath turned the replay into a 409 — removing the recovery the retire was meant to provide. The client retires it itself on the next press; the owner reaps an unheld session at 45 s. |
| D6 | `PUBLIC_START_DEADLINE` = 40 s (§3.13) | 35 s | A start that fails after a provisional was issued still has to stop it, and that stop is awaited on the response path with its own five seconds. 40 + 5 is exactly the clients' 45 s timeout; 35 + 5 is inside it. The test now asserts the sum, not the budget. |
| D7 | "plurx has 190 routes" (§3.10) | 192 | Three public routes, three internal ones (the plan counted two), and `test_api_doc_routes` is the arbiter. |
| D8 | A guide that is `unavailable` is polled on a flat 30 s (§3.16) | The owner's `next_refresh_at` wins whenever it is present, on unavailable answers too; the flat constant is the fallback for a document that says nothing | The owner now serves `next_refresh_at` on an unavailable answer precisely so a client can ask once the answer exists. All three clients do the same thing, and a failed read is paced like an unavailable one. |
| D9 | Every client runs the liveness probe before retiring a hint (§3.16) | Web only | The probe exists because several browser documents share one origin's storage. One app process owns its hint file outright and forgets what it held on release, so any hint still on disk at press time is an orphan. Applying a freshness guard there would have made the plan's own required test — a hint left seconds ago by a killed process *is* retired — impossible. Both constants are still transcribed and pinned. |
| D10 | — | `guide_poll_ceiling_s` added to `live.timings` | Apple had invented a local twenty-minute clamp so a far-future `next_refresh_at` could not park the grid for hours. The clamp is right; a client-local number the shared fixture cannot pin is not. |

## 3. Evidence

The full suites run once, at the lane's promotion, per
[AGENTS.md](../../AGENTS.md); everything below is the focused regression for
the changed behaviour.

| Surface | Command | Result |
|---|---|---|
| Server | `cargo check -p plurxd --all-targets` | clean |
| Server | `cargo clippy -p plurxd --all-targets -- -D warnings` | clean |
| Server | `cargo fmt --all` | clean |
| Server | `cargo test -p plurxd --bin plurxd live_tv::` | 95 passed, 3 failed — see below |
| Web | `node --test tests/web/live-tv.test.js` | green, 50 tests |
| Android | `./gradlew testDebugUnitTest lintDebug` | green, 571 tests; lint findings unchanged |
| Apple | fast lane `fast Apple compile` on `644316f` | **green** — the lane type-checks on a real Swift toolchain |
| Docs | `python3 -m unittest tests.operations.test_api_doc_routes tests.operations.test_docs_index tests.operations.test_apple_build_claims` | green |

**The three Rust failures are pre-existing on `main`.** Checked out `75edcb4`
in a separate worktree and ran exactly those three: 0 passed, 3 failed. They
are `live_tv_software_hls_argument_baseline_is_stable` (the expected argument
vector disagrees with the builder over deinterlace placement),
`live_hls_publishes_short_startup_segments_before_steady_cadence`, and
`one_tuner_get_runs_the_full_hls_lifecycle_and_stop_waits_for_cleanup`. This
lane touches no FFmpeg argument construction; `git diff 75edcb4..HEAD` over
that code is empty.

**`make web-check` is red on `layout-containment`, and not on our rules.** The
Live TV rule it named (`.lt-signal span`) is fixed here; the four that remain
are library-channel rules that `git blame` to `224fd25`, before this lane.
`page-read-budget` fails the same way, on `clearLibraryChannelDraft` missing
from that file's own sign-out harness. Both Live TV assertions in it pass.

## 4. The adversarial review, and what it changed

One review of the whole lane against the plan's §6 attack list. Eight findings
would have bitten in production; all eight are fixed, each with a test that
fails without the fix.

| Finding | Fix |
|---|---|
| `owner_peer`'s three failures were stamped `owner_decided: true`, so a resume during a deploy blip destroyed the hint for a session still feeding a tuner | They are `Decided::Ingress`: nothing that fails before a request leaves this node can speak for the owner |
| `GET /starts/{id}` mutated state on any non-owner ingress | D4 |
| The web probed liveness before `DELETE` but not before `resume`, so a second tab could adopt the first tab's capability and kill its picture on navigation | One probe, two callers |
| The ingress's detached retire fenced the id the contract tells the client to replay | D5 |
| `PUBLIC_START_DEADLINE` did not bound the public start; the awaited stop pushed the worst case to exactly the clients' timeout | D6 |
| `invalidate()` could race an in-flight persist and leave a discarded guide back on disk | The write is gated and checks a discard epoch before its rename |
| `is_retired` never checked its own TTL | Checked on read |
| A failed lineup read permanently disabled resume for a web document | The document is marked resumed only once a resume was actually possible |

Two tests were replaced rather than fixed: the eviction cases asserted against
a *copy* of the predicate instead of the predicate, so changing the real one
left them green. The choice is now a named function on the registry and the
five capacity fixtures exercise it. The `DeviceAuth`-absence tests were
extended to the persisted file and the session-end line.

Three divergences between the client reducers were closed: a failed guide read
is paced as `unavailable` everywhere, the four ingress-decided codes count as
such only at a 4xx everywhere, and a refused resume forgets the hint only when
an owner decided it.

## 5. What is not proven here

- **Nothing Apple was *run*.** The fast lane compiles the Apple target on a
  real Swift toolchain and it is green, so the five constructs §5 used to
  worry about are settled: the async `recoveryRoutesAvailable()` witness,
  `nextRefreshAt`'s implicit `nil` in `LiveTvGuide`'s memberwise init,
  `LiveTvResumeAnswer`'s explicit `CodingKeys`, the nested generic
  `read<T: Decodable>` in `LiveTvGuideReadiness.init(from:)`, and `StartBody`'s
  nil `requestId` being omitted by the synthesised encoder. No XCTest run.
- **No Android instrumented test ran.** The hint store's real `AtomicFile`
  behaviour, the Developer tab's guide panel and the resume-to-playback path
  are compile-, unit- and lint-verified only.
- **Nothing ran against a live server or a tuner.** The physical script in the
  plan's §7 is what settles the tvOS Caches-purge path, the resume-to-picture
  transition, and cases (1) through (6) generally. §7 below records where that
  stands.
- **Three Rust tests fail, all pre-existing on `main`.** Verified against
  `75edcb4` in a separate worktree before this lane existed: 0 passed, 3
  failed, the same three. They are
  `live_tv_software_hls_argument_baseline_is_stable`,
  `live_hls_publishes_short_startup_segments_before_steady_cadence` and
  `one_tuner_get_runs_the_full_hls_lifecycle_and_stop_waits_for_cleanup`, all
  in FFmpeg argument construction this lane does not touch. They belong to the
  batch pass over full-suite failures, not here.

## 6. What is deliberately not in this lane

The guardrails in the plan's §4, unchanged: no `DeviceAuth` at rest or in a
log line, no field added to a signed internal body, no lifecycle constant
moved, no client-side refusal to start, nothing but `{request_id, touched_at}`
persisted on a client, no auto-tune on open, no guide in the replicated store,
no second tuner GET, no DVR and no reminders.

Nothing in this lane is gated in code. The Developer tab on every client has a
Live TV section that says what a safe enable needs and whether each part is
met right now — `start_recovery` on the readiness document, and `guide_age`,
`guide_next_refresh`, `guide_persisted` and `guide_last_error` on the guide
readiness document. It is advisory: no readiness value reaches a disabled
control on any client, and `start_recovery` is excluded from the overall
verdict so a fleet mid-rollout is never shown as broken for lacking it.

---

## 7. Shipping it

| Step | Who can do it | State |
|---|---|---|
| Merge into `main` | this session | done — [#281](http://forge.lan:3000/noirr/plurx/pulls/281) at `755195a` |
| Server deploy to `media1`, `lab6`, `lab4`, `lab3` | this session | run from `media/deploy.yml`, `-e only=plurx` |
| Apple build 147 → the Apple TV and the iPhone | Paul's Mac | not done — needs the Xcode signing identity |
| Android `versionCode` 90 → the Google TV and the phones | Paul's Mac | not done — needs the paired devices |
| Physical verification, cases (1)–(6) | Paul's Mac | not done — §8 |

### The server deploy

`media/deploy.yml` with `--limit noirr -e sync=false -e only=plurx`. `sync=false`
skips the play that fast-forwards the Mac's own `~/code/*` checkouts; it changes
nothing about what the nodes deploy, because each node fetches from its own
`origin` and computes its own `BUILD_REF` from its own `git describe` after the
reset. The four nodes are the whole `noirr` group.

One operational note, recorded because it cost four minutes of downtime on
`media1`: the playbook stops the stack to copy a consistent database before it
rebuilds, so a controller that dies mid-run leaves that node down. Run it
somewhere that survives — a `screen` on a node, not a shell that can be
reaped. `docker compose start` in `/opt/noirr/plurx/deploy` brings a node back
without waiting for the rebuild.

### What the deploy proved, on the fleet

All four nodes report `v0.3.0-2236-g3062f3c9` and `ready`. `media1` writes
`/srv/plurx/cache/runtime/live-tv/guide.json` - 564 KB, 1772 programme rows.

Then the objective itself, measured by restarting `plurxd` on `media1` and
polling `/metrics` every five seconds:

| | `guide_age_seconds` | `guide_programmes` |
|---|---|---|
| before the restart | 1059 | 1772 |
| t+10 s | **1080** | 1772 |
| t+15 s | **1** | 1771 |
| t+60 s | 46 | 1771 |

The first row is objective 1: the process came back and served the previous
guide at its *true* age - 1059 plus the twenty-odd seconds it was gone - not
zero, not `NaN`, not absent. The second row is objective 2: the first refresh
after a restart landed fifteen seconds later. The behaviour this effort exists
to fix was an empty grid and a twenty-minute wait.

### What the clients need

Neither client artifact can be built anywhere but the Mac that owns the Xcode
signing identity and the device pairings. `scripts/ship --apple` and
`scripts/ship --android` drive the Ansible `mobile_release` role; the
standalone `scripts/ship-physical` does the same work for plurx alone when
Ansible is not healthy. Until one of them runs, the fleet is a new server and
three old clients — which is a state this lane is explicitly built to survive:
an older client sends no `request_id`, the ingress mints one, and the start
behaves exactly as it did before.

## 8. Physical verification — the handoff

The prompt is [LIVE-TV-RELIABILITY-IMPLEMENTATION.md §7](LIVE-TV-RELIABILITY-IMPLEMENTATION.md#7-physical-verification--the-prompt-for-the-gpt-session-at-pauls-mac),
with one change now that the lane has merged: build from `main`, not from
`effort/live-tv-reliability`. Cases (1) through (6) are what settle the tvOS
Caches-purge path, the resume-to-picture transition, and whether the words
"Wait 90 seconds" can still be produced by any sequence.

Case (2) is the one that matters most and the easiest to run wrong: the kill
has to happen while the app is playing in the foreground. Pressing Home first
is a *clean* release on Apple — the `scenePhase` handler stops the session and
a confirmed release forgets the hint — so it tests case (4) instead. On
Android, `am force-stop` runs `onStop` and is likewise not abrupt enough;
`kill -9` on the pid is.
