# Live TV reliability — build status

What is built, what is pushed, and what is still open on the
`effort/live-tv-reliability` lane. Companion to
[LIVE-TV-RELIABILITY-IMPLEMENTATION.md](LIVE-TV-RELIABILITY-IMPLEMENTATION.md)
(the *what*, exactly) and
[LIVE-TV-GUIDE-AND-START-RELIABILITY.md](LIVE-TV-GUIDE-AND-START-RELIABILITY.md)
(the *why*). Read this one to answer "how far along is it" without asking.

**Lane:** `effort/live-tv-reliability`, based on `main` at `75edcb4` ·
**Pull request:** one, into `main`, opened `WIP:` ·
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
| Apple | — | no Swift toolchain in the session that built this; see §5 |
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

- **Nothing Apple was compiled.** There is no Swift toolchain in the session
  that built this; `make apple-test` on the lane is the first real type-check.
  The constructs most likely to need a one-line fix, in order: the async
  `recoveryRoutesAvailable()` witness, `nextRefreshAt`'s implicit `nil` in
  `LiveTvGuide`'s memberwise init, `LiveTvResumeAnswer`'s explicit
  `CodingKeys`, the nested generic `read<T: Decodable>` in
  `LiveTvGuideReadiness.init(from:)`, and `StartBody`'s nil `requestId` being
  omitted by the synthesised encoder — that last one is the mechanism that
  stops an older `deny_unknown_fields` ingress answering 400.
- **No Android instrumented test ran.** The hint store's real `AtomicFile`
  behaviour, the Developer tab's guide panel and the resume-to-playback path
  are compile- and lint-verified only.
- **Nothing ran against a live server or a tuner.** The physical script in the
  plan's §7 is what settles the tvOS Caches-purge path, the resume-to-picture
  transition, and cases (1) through (6) generally.

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
