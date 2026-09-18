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
