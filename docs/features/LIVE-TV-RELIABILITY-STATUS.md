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
| M0 | `tests/playback/live-tv-start-cases.json`, six `live.timings` keys, contract embed | built |
| M1 | Durable guide, `age_offset`, `next_refresh_at`, event-driven refresh loop, relay memory, Developer rows | built |
| M2 | Web · Apple · Android poll the guide on the owner's clock | pending |
| M3 | `request_id`, retire/resume, stray eviction, one public start budget, typed envelope, session-end logging | built |
| M4 | Barrier removal on all three clients, from the one fixture | pending |
| M5 | Developer-tab enable section, release counters, deploy, physical verification prompt | pending |

## 2. Deviations from the plan, and why

Recorded as they are made, for Paul to overrule at the end rather than be
interrupted for.

| # | Plan said | Built instead | Why |
|---|---|---|---|
| D1 | One task PR per milestone, each adversarially reviewed (§5) | One lane PR into `main`, reviewed once when it is ready to merge | Paul's session directive: batch commits into a bigger PR, one adversarial review at merge time, fast lane run once. His directive is newer than the plan and he authored both. |
| D2 | `plurx_live_tv_session_ends_total` keyed by `error.code()`, with a clean release "distinguished by the cleanup path" (§3.12) | Keyed by `error.code()` **except** a session with no terminal error, which counts as `released` | A stop, a drain, a shutdown and an idle reap all end with the synthesised `capability_expired`. Counting them under that code buries every real failure in the one bucket an operator would look at first. |
| D3 | `request_id` validated as 32 lower-case hex (§3.6) | Same, and the ingress mints `Uuid::new_v4().simple()` — hyphenless — when a client sends none | So every id on the wire has one spelling. The owner treats it as opaque either way. |
| D4 | `GET /live-tv/starts/{id}` reads the owner's registry (§3.10) | On a non-owner ingress it is answered by folding a `resume` exchange | The registry is process-local to the owner; there is no third internal path for a read a resume already answers, and `resume` on a live session is idempotent. |

## 3. Evidence

Recorded per milestone as it lands; the full suites run once, at the lane's
promotion, per [AGENTS.md](../../AGENTS.md).

| Milestone | Command | Result |
|---|---|---|
| M1 · M3 | `cargo check -p plurxd --all-targets` | clean |
| M1 · M3 | `cargo clippy -p plurxd --all-targets -- -D warnings` | clean |
| M1 · M3 | `cargo fmt --all` | clean |

## 4. What is deliberately not in this lane

The guardrails in the plan's §4, unchanged: no `DeviceAuth` at rest or in a
log line, no field added to a signed internal body, no lifecycle constant
moved, no client-side refusal to start, nothing but `{request_id, touched_at}`
persisted on a client, no auto-tune on open, no guide in the replicated store,
no second tuner GET, no DVR and no reminders.

Nothing in this lane is gated in code. The Developer tab's Live TV section
says what a safe enable needs and whether each part is met right now; it is
advisory and never refuses the enable.
