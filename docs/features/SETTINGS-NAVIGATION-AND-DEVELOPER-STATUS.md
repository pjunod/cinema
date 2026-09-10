# Settings navigation and Developer — implementation status

**Status:** review addressed; promotion gate required · **Owner:** Codex implementation task · **Started:**
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
| Portable UI structure golden | complete | 78 deterministic captures · 7,536 structural facts · settings and Developer drift accepted across all three layouts at desktop and mobile widths · no console or page errors |
| Current-reference documentation | complete | `FEATURES.md` names each control owner and advisory semantics · `API.md` names all three readiness consumers and the moved fencing action |
| Actual-app visual evidence | complete | Isolated daemon at `127.0.0.1:32419` · 1280, 880, 390, and 320 CSS px · light and dark · 640 CSS px as the 200% responsive equivalent · expanded readiness on desktop and phone |
| Adversarial review | complete | The one permitted review reported five findings; all five are addressed on the draft branch without a re-review |
| Fast lane | required | PR #228 is the authoritative record for current-head attempts and corrections; runs begin only after the one review is addressed |
| Merge to `main` | required | Requires a green Main promotion gate on the current head |

No separate unit, integration, browser, simulator, emulator, recovery,
playback, package, or smoke suite has run for this branch. The main-bound
workflow deliberately defers those suites; fast-lane attempts and any
current-head corrections begin only after the single adversarial review is
addressed.

## Actual-app evidence

The branch was compiled and served as an isolated single-node installation on
loopback-only ports. A disposable local admin account was used; no production
data or existing plurx installation was touched.

| Surface | Evidence |
|---|---|
| Developer, desktop | [1280 px dark](../img/settings-developer-1280-dark.png) · [1280 px light](../img/settings-developer-1280-light.png) |
| Developer, responsive | [880 px](../img/settings-developer-880-dark.png) · [390 px](../img/settings-developer-390-dark.png) · [320 px](../img/settings-developer-320-dark.png) |
| Developer, zoom pressure | [640 CSS px](../img/settings-developer-200-percent-equivalent.png), the layout-equivalent viewport for a 1280 px window at 200% browser zoom |
| Control ownership | [Playback quality](../img/settings-playback-1280-dark.png) · [Live TV tuner and guide](../img/settings-live-tv-1280-dark.png) · [Cluster recovery](../img/settings-cluster-recovery-1280-dark.png) |
| Expanded readiness | [1280 px](../img/settings-playback-readiness-expanded-1280-dark.png) · [390 px](../img/settings-playback-readiness-expanded-390-dark.png), including long evidence and status pills |

Manual actual-app exercises also confirmed that:

- saving prepared quality changes updates only that card and renders the
  returned enabled/disabled state;
- changing and saving the guide source preserves an unsaved tuner-address
  draft; and
- leaving Live TV while a saved-configuration check is pending keeps the
  destination route on Developer when the response completes.

The in-app browser harness cannot set browser chrome zoom directly. Its 640
CSS px viewport exercises the same responsive width as a 1280 px window at
200%; the evidence names this limitation instead of claiming a native zoom
gesture was performed.

## Sole adversarial review

The draft pull request received exactly one adversarial agent review. The
author addressed every finding directly; no second review or approval pass was
requested.

| Finding | Disposition |
|---|---|
| Off-route Live TV writes left the shared Settings cache stale | Cache the returned snapshot before suppressing route-local DOM work |
| Guide save and refresh failures could target a detached error node | Re-resolve the owning error slot after each awaited request |
| An older quality-save response could erase a newer edit | Track each card's draft revision and leave newer edits visible and unsaved |
| Guide readiness could be mistaken for validation of an unsaved draft | Label the persistent status and response with the saved source and look-ahead |
| Visual evidence omitted expanded readiness | Add desktop and phone captures with long evidence and nonshrinking status pills |

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
- Current-head fast-lane results will be added only when they exist;
  historical prototype observations are not evidence for this branch.
