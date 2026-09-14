# DVR visibility status — make capture state explain itself

**Status:** foundation merged; web fidelity follow-up built and reviewed ·
**Branch:** `codex/dvr-ui-fidelity` · **Base:** `0b2490839` (PR #314) ·
**Updated:** 2026-09-14

Companion to [LIVE-TV-DVR-STATUS.md](LIVE-TV-DVR-STATUS.md) (what the shipped
recorder already does),
[LIVE-TV-DVR-IMPLEMENTATION.md](LIVE-TV-DVR-IMPLEMENTATION.md) (the recorder
and scheduler contract), and
[DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) (the one-review fast
lane) — this is the short answer to *what is being changed, what is already
proved, and what remains before the rendered design is delivered?*

## Outcome — recording evidence belongs wherever the programme appears

The effort makes a recording visible in Live TV, Activity and a permanent
Recordings destination on web, Apple and Android. Labels follow recorder
evidence: a request is not a write, a running clock is not saved media, and
an unreachable owner is `Status unavailable`, never a false zero.

The existing recorder, scheduler, household permissions, reminder controls,
shared channel transports and finite-media playback path stay in place. The
new work adds bounded observation, durable lifecycle history and one shared
presentation contract around them.

## Web fidelity follow-up — render the design in the product

The merged foundation exposed the data but omitted much of the proposed
presentation. This follow-up changes the shipped web page, using the existing
DVR APIs. It does not change Rust, Apple, Android, recording storage or the
scheduler. Deployment is a separate release step.

| Surface | Implemented presentation |
|---|---|
| Live TV programme | Bordered recording panel beside the player, named state, confirmed bytes, elapsed capture bar, end time, Activity link and named Stop confirmation; unscheduled programmes offer Record programme and Record series |
| Player and guide | Programme recording badge beside the player title; readable recording state in guide marks; exact channel/airing identity and explicitly named padding |
| Other captures | Also recording strip above the guide, with programme, channel, state and link to that capture in Activity |
| Activity | Selectable recording cards, desktop detail pane, write age/rate, attempts, event timeline and technical disclosure; Needs attention and Recent recordings appear before existing playback and background work |
| Recordings | Compact counts and active-capture strip; Saved uses programme cards with Play only when a media item and file are linked; other task tabs and paged history remain available |
| Phone | Cards stack; selecting a detail brings it above the card list and focuses Close; closing restores focus to the recording; navigation scrolls within its own row |

**How to read it:** capture progress is elapsed wall-clock time within the
capture window, not the duration of playable video. Byte counts describe
successful writes. Rates are bytes per second. A stale overview or stale
owner observation says Status unavailable and suppresses current write
health. A durable recording row alone never proves that a recorder is
writing. Changing channels or leaving Live TV does not stop recording.

One adversarial review found four issues; all were addressed: delayed detail
responses after navigation, detail polling after backgrounding, Live TV
polling losing keyboard focus, and a stale schedule fallback claiming to
record. The browser regression reproduces these cases. Native confirmations
name the programme and known capture window; cancelling sends no mutation.

**Validation:** the browser harness renders the actual shipped HTML and
assets with intercepted fixture API responses. It checks Classic, Catalog
and Theater at 1440, 390 and 320 px, dark/light rendering, no horizontal page
overflow, selection, focus restoration, stale states, padding identity,
linked playback, stop cancellation and preservation of the Live TV video
node during status refresh. It does not prove live tuner capture or native
client behavior. The player region is empty in fixture screenshots because
there is no broadcast stream.

```bash
# Run with a local Playwright installation; no running plurxd is needed.
node tests/web/dvr-ui.browser.cjs
# Optional: point to an existing installation and save rendered evidence.
PLAYWRIGHT_MODULE=/path/to/node_modules/playwright \
DVR_SCREENSHOTS=/tmp/dvr-ui-evidence node tests/web/dvr-ui.browser.cjs

# Focused state, Live TV and unchanged playback/activity regressions.
node --test tests/web/dvr-visibility.test.js tests/web/live-tv.test.js \
  tests/web/activity-node-names.test.js tests/web/layout-containment.test.js \
  tests/web/nav-keyboard.test.js tests/web/player-dom.test.js \
  tests/playback/player-input-contract.test.js
scripts/js-check
```

These checks pass for this follow-up. Screenshots include Live TV, Activity,
Saved, phone details and light mode. The earlier implementation record below
explains the foundation; its compile results do not replace visual checks.

## Foundation implementation record — original promotion packages

| Package | State | Evidence or next boundary |
|---|---|---|
| S00 isolated base and compiler | complete | independent clone at `28ae8163`; pinned Rust 1.97.1 workspace check passed before Rust edits |
| S01 blank-list repair | built; final evidence pending | web, Apple and Android decode the real `{rows, next}` envelope, preserve stale rows and expose independent Load more controls; regression fixtures written but not run |
| S02 capture observation and overview | built; compile passed | successful-write sampling, five-second rate window, bounded peer observations, shared Activity/overview projection and `/api/v1/dvr/overview`; focused fixtures written but not run |
| S03 lifecycle ledger and attention | built; compile passed | append-only SQLite v59 / replicated v39 migrations; atomic and owner-fenced events, bounded pages/pruning, recovery-gap provenance, legacy outcomes, per-user acknowledgments and section/watermark-stable attention traversal; no tests run yet |
| S04 web layouts and controller | built; compile passed | one scope-aware poller feeds chrome, Live TV and Recordings; exact-airing player context, canonical `#/recordings`, six task tabs, mobile detail, real event history and Activity projection |
| S05 Apple clients | built; source type-check passed | additive overview/event/attention decoders; one foreground profile controller; iOS/tvOS Recordings root, capture activity, immediate details, history, manual/skipped/rules/reminders and exact-airing player/guide context; device builds remain pending because this host exposes no simulator runtime |
| S06 Android clients | built; compile passed | additive overview/event/attention decoders; one lifecycle-owned profile controller; phone/Google TV Recordings root, capture activity, immediate detail/history, manual/skipped/rules/reminders, exact-airing context and named stop/delete confirmations; physical D-pad evidence remains pending |
| S07 review and promotion | fast lane retry pending | one adversarial review completed against the current-base candidate; all eleven findings are remediated and the affected source compilers pass; the first real PR #314 promotion run stopped in policy preflight before compiler jobs, and its four history anchors plus Apple build 158 / Android build 98 are now corrected for one synchronization retry |

## Decisions made while implementing

1. **Use one effort branch and one main-bound PR.** The user asked for larger
   batches and one final review/test cycle, so reviewable packages land as
   normal commits on `effort/dvr-visibility`; there are no per-package CI
   runs.
2. **Treat readiness as advice.** DVR remains a plain enable setting. The
   Developer settings section explains each prerequisite and whether this
   process can observe it, but no readiness result blocks enablement.
3. **Use the current workflow, not the superseded label instruction.** Since
   2026-09-13, marking a main-bound draft ready starts `main-fast-lane.yml`;
   the removed `fast-lane` label cannot be applied.
4. **Reserve tests for the frozen candidate.** Compile-only feedback may run
   while building because repository policy forbids using CI as a compiler.
   Focused and fast-lane tests run only after the adversarial review is
   addressed.
5. **Keep the television manual timer operable without a date picker.** tvOS
   does not ship SwiftUI's `DatePicker`, so its existing channel-and-time task
   uses explicit start-delay and duration choices; iOS keeps exact date/time
   selection. Both submit the same server contract.
6. **Use one bounded schedule window on every new surface.** Upcoming defaults
   to the twelve hours before and after now, caps the window at 24 hours, and
   owns a cursor separate from Saved and Needs attention. The legacy 14-day
   query remains compatible for older clients.
7. **Keep internal recording topology out of normal attention views.** The
   attention projection now omits filesystem paths, owner-node identity and
   raw requesting/stopping user ids. It carries only the viewer-facing
   `stopped_early` fact clients need to explain an intentional partial file.

## Adversarial review — one pass, all findings addressed

The requested single adversarial review ran after the integrated branch was
current with `main`. Its six P1 and five P2 findings were handled as one batch:

- foreground overview and filtered schedule reads now push their state,
  24-hour window, ordering and limit into SQLite and replicated-store queries;
  exact active/conflict totals remain separate from bounded row projections;
- conditional Stop, Skip and Delete responses re-read a lost state race and
  report the action that actually applies;
- current attention ends when a conflict clears or a later retry/write event
  resolves an interruption, while fresh runtime evidence adds stalled writes
  to the overview attention count;
- a finishing-event persistence failure marks history incomplete before a
  row may become terminal, so a complete-looking ledger cannot silently lose
  its last observation;
- the narrow web detail layer stays absent until selection, restores focus on
  close, and polling preserves focused controls and manual-form values;
- Apple and Android retain explicitly loaded Upcoming pages across normal
  refreshes and drain every forward lifecycle-event page before stopping;
- Android recording details now confirm the immutable title and capture window
  before Stop; replicated acknowledgement validation now matches SQLite.

## Current compile evidence — no test lane spent

- `rustup run 1.97.1 cargo check -p plurxd --all-targets` passes on the
  integrated Rust tree.
- Android `:app:compileDebugKotlin` passes; its two volume-icon deprecation
  warnings predate this effort.
- Direct Swift type-checks pass for both `arm64-apple-ios17.0` and
  `arm64-apple-tvos17.0`. The full Xcode build still stops in asset compilation
  because this host has no simulator runtimes; no Swift error was emitted.
- `scripts/js-check` accepts both shipped inline script blocks.
- The post-review Rust, Android, iOS, tvOS and inline-JavaScript source checks
  all pass. Only the same pre-existing Apple concurrency warnings and Android
  volume-icon deprecations remain.

No unit or UI test command has run. The shared web/Apple/Android fixture and
focused Rust cases remain owned by the one fast lane, as requested. Forgejo's
initial `ready_for_review` event carried the old draft flag and skipped every
job without allocating a runner; the following status-only synchronization is
the first authoritative promotion attempt for the unchanged reviewed source.
It stopped before the compiler jobs because current `main` requires Apple and
Android release-counter increments and regression-history anchors for four
corrective commits. Those mechanical promotion inputs are now Apple build 158,
Android versionCode 98, two client-fix anchors and two runtime regression
mappings; the next synchronization retries the same fast lane without another
review or a local test cycle. That retry accepted the history ledger and all
198 CI-policy cases, then the operations inventory found the four new DVR
routes were listed but the prose total still said 205. The corrected total is
209; no product code changed and the following synchronization continues the
same failure-fix lane.

## Evidence limits — green source is not a hardware claim

The final fast lane can prove policy, static contracts and affected Rust,
web, Apple and Android compilation. It cannot prove tuner writes, physical
Siri Remote or Google TV focus, shared storage on every node, or two-client
convergence. Those observations remain explicit follow-up evidence; this
page will not turn a simulator, fixture or compiler result into a device
claim.
