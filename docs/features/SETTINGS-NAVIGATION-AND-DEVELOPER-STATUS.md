# Settings navigation and Developer — implementation status

**Status:** building · **Owner:** Codex implementation task · **Started:**
2026-09-09

Companion to [FEATURES.md](../FEATURES.md) (current settings behavior),
[API.md](../API.md) (settings and readiness endpoints), and
[DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) (the main-bound pull
request workflow) — this page answers what is built, reviewed, and proved for
the settings navigation and Developer-page redesign.

## Delivery state — one bounded web change

| Phase | State | Evidence |
|---|---|---|
| Isolated implementation branch | complete | `codex/settings-navigation-developer`, based on Forgejo `main` at `4cef0da7` |
| Navigation and control ownership | complete | Live TV owns tuner and guide cards · Playback owns quality switching · Cluster owns transport guidance · Developer owns compatibility and experiments |
| Readiness layout and responsive treatment | complete | Shared nonshrinking rows · neutral static throughput support · closed native disclosures · deliberate stacking below 360 CSS px |
| Interaction and stale-response correctness | complete | Independent save payloads · returned-value badges · Live TV route fencing · card-local repaint and sibling-draft preservation |
| Current-reference documentation | queued | `FEATURES.md` and `API.md` remain to be updated with final behavior |
| Actual-app visual evidence | queued | Desktop, narrow, zoomed, and expanded-readiness captures remain to be recorded |
| Adversarial review | queued | Exactly one review will run only after the branch is ready to merge |
| Fast lane | queued | Runs only after review findings are addressed and the PR is marked ready |
| Merge to `main` | queued | Requires a green Main promotion gate on the current head |

No unit, integration, browser, simulator, emulator, recovery, playback,
package, or smoke suite has run for this branch. The main-bound workflow
deliberately defers those suites; the current fast lane runs once, after the
single adversarial review is addressed.

## Decisions — preserve operator choice

1. **Readiness remains advisory.** Missing, unmet, stale, failed, or
   unobservable evidence will never disable a toggle, reject its Save, or
   silently replace a saved choice. Existing input validation, authorization,
   generation conflicts, and real operation failures remain intact.
2. **Every control has one owner.** Live TV owns tuner, enablement, fencing,
   and guide configuration · Playback owns prepared quality switching ·
   Cluster owns automatic transport-recovery guidance · Developer owns
   compatibility controls and decoder/delivery experiments.
3. **The prototype is a reference, not a base.** The implementation starts
   from current Forgejo `main`; only relevant ideas are ported from the local
   prototype, and unrelated work in the original checkout is left untouched.
4. **No feature gate is added.** Experiment toggles stay explicit and
   editable. Developer explains what safe enablement needs and which evidence
   the daemon can currently observe.

## Remaining limits — evidence must name what it cannot prove

- Daemon readiness cannot certify physical first-frame handoffs, fleet memory
  stability, or measured fallback interruption when those facts are not
  reported. The UI must label them **not observable**, not pass or fail.
- Static throughput support proves the reporting path exists; it does not
  prove that a particular stream, client, or network has enough throughput.
- Guide readiness describes saved configuration. It must not be presented as
  validation of an unsaved XMLTV draft.
- Actual-app screenshots and current-head fast-lane results will be added only
  when they exist; historical prototype observations are not evidence for this
  branch.
