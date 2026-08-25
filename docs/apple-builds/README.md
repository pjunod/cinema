# Apple per-build release notes

One file per Apple change. This directory exists because the narrative used to
live in three shared documents at a shared insertion point, so any two
concurrent Apple branches conflicted on all three and re-conflicted every time
`main` moved. Issue #509 records the decision and the rejected alternatives;
[`history-through-build-78.md`](history-through-build-78.md) holds everything
those documents said through build 78, verbatim.

## Add a fragment

Name the file after the **issue**, never the build:

```
docs/apple-builds/<issue>-<short-slug>.md
```

Issue-keyed names are the whole point. Two concurrent Apple changes write two
different paths, so the narrative cannot collide, and neither branch has to
guess which build number it will end up shipping as.

```markdown
# Say when a Skip Credits button is an estimate

Build: 79
Issue: #351

Skip Credits now says when its end-credits boundary is estimated rather than
detected, so a viewer can tell a confident jump from a guess.
```

`Build:` and `Issue:` are required and each appears once. `Build:` is the only
number in the fragment that the tooling rewrites, and it is rewritten for you.

## Claim or re-claim the build number

```
make apple-build-bump
```

That reads `clients/apple/project.yml` and the merge target, picks the next
number above both, and rewrites every place the number is allowed to appear:
`project.yml`, the anchored `> Status:` lines in
[`clients/apple/README.md`](../../clients/apple/README.md) and
[`APPLE-CLIENT-PARITY.md`](../APPLE-CLIENT-PARITY.md), the viewers tile and the
`👤 Paul` TestFlight item in [`STATUS.html`](../STATUS.html), and the `Build:`
line of the fragments this branch adds.

Run it again after `main` moves under an open branch, **in that order**: merge
`main` in first, then claim.

```
git merge origin/main      # conflict-free: both sides claimed the same number
make apple-build-bump      # now claims one above what main actually holds
```

The counter has to beat the merge target, not the commit the branch was cut
from. Claiming before the sync puts the branch two above the base while `main`
holds one above it, on the same line — an ordinary same-line conflict, and the
one thing the tool cannot merge for you. Syncing first costs a merge Git
resolves by itself and leaves exactly one mechanical commit behind.

Claiming is idempotent. A branch already above the merge target keeps the number
it holds, so running the tool again when nothing moved changes nothing and
prints so.

## What still fails the gate

`tests/operations/test_mobile_build_claims.py` and `validation/mobile_versions.py`
are unchanged. The number still has to agree across `project.yml`, both
`> Status:` lines, and every unmarked `Apple build <n>` mention in `STATUS.html`,
and it still has to increase whenever Apple release inputs change.
`tests/operations/test_apple_build_claims.py` adds the fragment rules, pins the
generated form of every surface, refuses to let per-build narrative move back
into either status blockquote, and merges two branches cut from one base in
sequence to prove they no longer collide.
