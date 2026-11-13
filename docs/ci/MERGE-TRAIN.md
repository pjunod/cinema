# The merge train — how a pull request lands on `main`

**Status:** built (the train service is in [noirr/ci-fleet](http://forge.lan:3000/noirr/ci-fleet), `ci-fleet train`); off until the owner switches it on · **Updated:** 2026-11-12 · **Owner:** Claude (claude-opus, session 01BCWLA2)

Any agent — Claude, GPT, OpenRouter — or person can follow this page. It says
how a pull request into `main` lands with the train **off** and **on**, the
four controls, and what the bot's comment on your PR means.

## Is it on?

One switch: the repository Actions variable `MERGE_TRAIN`. `on` means on;
unset or any other value means off. `ci-fleet train status` (from a ci-fleet
checkout) prints it with the queue; every bot comment also names the mode.
Only the owner flips it (`ci-fleet train on` / `ci-fleet train off`).

## Train off — what you do today

Nothing changes: open your PR, take `WIP:` off the title when it is ready, the
`main fast lane` runs on it, and you merge it yourself once its
`Main promotion gate` passes (AGENTS.md "Large efforts" for the merge message).
The train service writes nothing to your PR while it is off.

## Train on — what you do instead

1. **Open your PR.** If its title has no `WIP:`, the bot adds it and posts one
   comment (it edits that comment from then on; it never adds a second).
   While it is a draft only the `paperwork` job runs.
2. **Take `WIP:` off when you want it merged.** The PR joins the queue. Only a
   PR the bot has already seen as a draft can board: a PR that is ready when
   the bot first sees it — including every ready PR at the moment the switch
   goes on — is drafted, never boarded, so you take `WIP:` off again.
3. **Do not merge it yourself.** The train takes up to eight queued PRs in
   order, merges them onto `main` in a branch `train/<n>`, and opens a pull
   request for that branch. The fast lane runs on that train PR only — with
   the switch on, the lane's jobs after `paperwork` do not run on ordinary PRs,
   because a per-PR run would test a tree that never lands.
4. **Green:** the bot merges each PR through its own PR, in order, pinned to the
   commit that was tested, and checks that `main` now has exactly the tested
   tree. Your PR shows as merged; the train PR is closed.
5. **Red:** the bot re-runs only the failed jobs on `main` alone. If they fail
   there too, the failure was already on `main`: the train lands anyway and the
   bot opens one issue for it. Otherwise it narrows the failure down to the PR
   that caused it (re-running only those jobs), puts `WIP:` back on that PR with
   a comment linking the failing job's log, and lands the rest after they pass
   the whole lane again without it.
6. **Conflicts:** a PR that does not merge onto `main` plus the PRs ahead of it
   gets `WIP:` back and a comment naming what it conflicts with.

Pushing to a PR that is on a running train is allowed; the train is rebuilt
with your new commit before anything lands.

## The four controls

| Control | Effect |
|---|---|
| `WIP:` at the start of the title | A draft. Remove it to ask for a merge; put it back to leave the queue or a running train (a running train then lands without you). |
| label `hold` | Keeps an undrafted PR out of the train until you remove the label. |
| label `train:solo` | A hotfix: it goes next as a train of one, ahead of the queue. It does not cancel a running train, and it runs the same lane. The comment records who set the label. |
| `MERGE_TRAIN` (owner only) | `on` / off, above. Switching off lets a running train finish and land, then nothing more boards. |

## The bot comment

The first line gives the mode, the PR's owner and, for a hotfix, who set
`train:solo`. **State** is one of:

| State | Meaning |
|---|---|
| drafted | Seen as a draft. Remove `WIP:` when ready. "Switched on while this PR was ready" means it was drafted by the rollout rule. |
| queued, position k of m | Waiting for a train. |
| ready, held / waiting for the forge to report it mergeable | Not boarding: remove `hold`, or resolve the conflict with `main`. |
| on merge train n (phase) · run | Being tested with the PRs ahead of it; the link is the lane run. |
| landed by merge train n | Merged. |
| bounced: … | Back to you with the reason (conflict, or the failing job's log). Fix it and remove `WIP:` again. |
| halted: … | The train stopped itself (for instance `main` did not end up with the tested tree). Nothing lands until the owner runs `ci-fleet train resume`. |

## How the lane knows

`.github/workflows/main-fast-lane.yml` reads `vars.MERGE_TRAIN` in its `scope`
and `Main promotion gate` conditions. A train PR's head starts with `train/` and
its author is the bot (`vars.MERGE_TRAIN_BOT`, default `train-bot`). The bot's
re-runs of only some jobs are train PRs too, with a `Train-Jobs:` line in the
description; `validation/merge_train.py` turns that line into the scope
outputs, and the promotion gate does not run on them. The train lands
through pull requests and never pushes to `main`.
