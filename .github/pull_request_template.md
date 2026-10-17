## Promotion checklist

This checklist applies only when the pull request targets `main`.

- [ ] The pull request was opened as a draft; no checks ran while it was draft.
- [ ] Exactly one adversarial agent review was completed.
- [ ] Every finding from that review was addressed without requesting a second review, re-review, or panel.
- [ ] The pull request was marked ready only after those findings were addressed, then received the `fast-lane` label.
- [ ] The current head passed `Main promotion gate`, which contains only the fast lane.

For a pull request targeting any other branch, leave this checklist unused: it
has no required adversarial review and no fast lane.

If this PR returns to draft, remove `fast-lane` first.

## Apple TV navigation (when a focusable surface changes)

- Entry target and directional neighbors:
- Select action, Back owner and dismissal target:
- Focus fallback after refresh/removal; empty and failed states:
- Native remote regression and source-bound screenshot/result receipt:
- Confirm shared modal/chrome styles are reused and reporting stays inside its owning controls.

The [TV navigation contract](../docs/clients/APPLE-TV-NAVIGATION-CONTRACT.md#7-navigation-contract--make-regressions-fail-before-release)
owns these rules. Compilation and logical focus requests do not prove actual
native focus. Keep untested device cases explicit.
