# `effort/live-tv-guide` review — four passes, sixty-one defects

**Reviewed:** 2026-09-08 · one adversarial pass per milestone PR (#120
server, #126 web, #130 Apple, M5 Android) · **Verdict: every finding
implemented and proved before the effort PR was opened.**

Companion to the PR#79–#98 review docs in this folder. Each pass ran against
the real tree with the reviewer free to write and execute proofs, and was
told to assume the author was competent but rushed and over-confident, and
that the commit messages might not be true. That last instruction earned its
keep three times.

---

## What the passes found

| Pass | Confirmed | Critical | Proved by execution |
|---|---:|---:|---|
| Server (#120) | 8 | 2 | 5 |
| Web (#126) | 14 | 3 | 8 |
| Apple (#130) | 19 | 4 | 0 (Swift does not compile here) |
| Android (M5) | 20 | 4 | 0 (Kotlin does not compile here) |
| **Total** | **61** | **13** | **13** |

Eleven of the thirteen criticals concern one thing: the physical tuner. That
is not a coincidence — it is the only resource in this feature that cannot be
retried, shared, or recreated, and every milestone found a new way to hold it
or drop it at the wrong moment.

## The four that mattered most

**Making readiness advisory quietly unfenced a live tuner owner.**
(`crates/plurxd/src/http/system.rs`) Paul's "don't gate features in the code"
ruling turned the Live TV readiness verdict from a refusal into advice. The
readiness check was also — unnoticed — the only thing refusing an enable
while a *previous* owner was still fenced: `validate_static` checks that the
barrier is well-formed, never that it is resolved. So enabling over an
undrained owner became a 200, and the next disable overwrote that barrier
with the current owner, erasing the only record that the first node was never
drained while it may still have been holding the tuner. Two nodes ingesting
one physical tuner is the system being wrong, not the feature being
unavailable, so admission is now a structural refusal in its own right — the
line the advisory ruling actually draws — and a standing barrier is never
overwritten.

**The web page's tuner watchdog died the moment the picture docked.**
(`crates/plurxd/src/web/index.html`) The poll's liveness test asked one
predicate three questions: is this still my session, is this still the render
on screen, and are we still on `#/live-tv`. Docking falsifies the last two by
definition — which is the entire point of the milestone. The interval went on
firing and doing nothing: no keepalive, no status poll, and no thirty-second
no-progress release, so a stalled dock held the household's only tuner until
the server's idle expiry, and returning to the page never repaired it because
the render generation had moved on for good. **The commit message said the
watchdog was untouched.** It was not. The reviewer proved it by driving the
shipped `watchLiveTv` under the repo's own harness with a mutable `location`:
one keepalive on the page, one keepalive after two minutes docked.

**Both Apple picture-in-picture rules were wrong, in opposite directions.**
(`clients/apple/Sources/LiveTvView.swift`) Automatic PiP starts on
*background*, and iOS publishes `.inactive` first — so at the edge the old
rule fired on, `isActive` was still false and the session was stopped before
the PiP window it was starting for could appear. That is the exact scenario
the build note claimed to fix. And nothing observed PiP *ending* while the
screen was gone, so a PiP window closed after navigating away left the lease
renewed by the heartbeat forever.

**The Android module did not compile, and the TV surface was never wired.**
(`clients/android/.../livetv/`) `PictureInPictureModeChangedInfo` was
imported from `androidx.activity`; it lives in `androidx.core.app`. Separately,
`LiveTvInputPolicy.route` had exactly one call site in the repository and it
was its own test — so the carefully transcribed routing table governed
nothing: no key handling, no auto-hide, no channel up/down, and a television
showing the phone layout with a Fullscreen button in front of it.

## One fix caused the next defect, and the review is how that was found

Closing the "a guide refresh can talk to the tuner" finding meant reading the
lineup from cache only. On a freshly restarted owner that cache is cold until
somebody opens Live TV — so the refresh matched nothing, *stored* the empty
result over a perfectly good guide, and reported it as the guide source's
fault. A cold lineup is now a refusal that leaves the cache alone and retries
in a minute rather than twenty. Worth recording plainly: a review pass is not
only a filter on the original work, it is a filter on the repairs.

## And one test that proved nothing

The `DeviceAuth` test asserted that a locally-built fixture did not contain a
string that had never been given to anything. It would have passed with every
credential precaution in the feature deleted. Being told that was worth more
than the test was. It now puts the credential on the real request path first
and asserts it is there, so the absences that follow have something to be
absent from — then checks the served document, the settings tuple, the value
the relay actually holds, and the copy a failed refresh produces.

The same shape appeared twice more. The web milestone's new tests matched
*source text* — `assert.match(SHIPPED_UI, /…/)` — which is why the docked
watchdog and the application-wide Space capture both passed CI while broken;
and every Apple parity test against the shared fixture was throwing on its
first line, because the document is decoded with `.convertFromSnakeCase`
while the test's own structs declared the snake_case originals. A parity
claim had been written into `APPLE-CLIENT-PARITY.md` on the strength of tests
that had never run.

## Method

Four independent passes, one per milestone, each given: the diff, the
invariants that must hold (tuner lease, one tuner GET per tune, the input
contract's hidden-overlay ruling, untrusted guide content), the shared
fixtures to check reducers against by hand, and an explicit instruction to
report "no finding" for areas checked and found clean rather than padding the
list. Server and web findings were proved by execution where possible —
`cargo test`, node harnesses over the shipped functions. Swift and Kotlin
cannot be compiled in this sandbox, so those passes traced call graphs and
walked fixture cases by hand, and their compile-level findings (a tvOS-
unavailable `toggleStyle`, a wrong import package) are the ones the runners
would otherwise have found an hour later.

Every finding was implemented on the milestone branch it belonged to, with a
test where a test could exist, before the effort PR into `main` was opened.
