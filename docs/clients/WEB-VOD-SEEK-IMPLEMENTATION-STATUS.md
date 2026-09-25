# Web VOD seek repair — implementation status

**Status:** open — implementation merged; physical Safari acceptance pending ·
**Updated:** 2026-09-24 · **Merged commit:** `c327b2a60110c8353907465287b0f6226d0bf032` ·
**PR:** [#478](http://192.168.4.7:3000/noirr/plurx/pulls/478) ·
**State:** reviewed and fast lane green on the merged candidate.

Companion to [the RCA and fix proposal](WEB-VOD-SEEK-MISSING-MEDIA-RCA-AND-FIX.md).
This page records the client repair and the remaining physical acceptance
check. The pull request contains the implementation, review response, and
fast lane result together.

| Step | State | Evidence or next action |
|---|---|---|
| RCA and Fable design review | Done | Keep VOD local; use the existing 20 s seek deadline and one fenced reopen. |
| Client implementation | Done | Local VOD intents have one 20 s fallback; progress watch, `waiting`, and startup watchdog yield while target media is missing. |
| Focused regression cases | Passed locally | `node --test tests/playback/seek-control.test.js` passed 19/19 after correcting the harness frame floor. `web-control.test.js` passed in the combined run; the docs index passed 4/4. |
| Adversarial implementation review | Done | One static review found a stale `waiting` timer after coverage loss and a needless reopen after proven presentation; both are addressed in source and regression cases. |
| Fast lane | Passed | Final head `043f09521` passed preflight, web syntax, Windows compile, Rust gate, and Main promotion gate. Apple and Android were skipped by affected-surface scope. |
| Merge | Done | PR #478 merged into `main` as `c327b2a6`; its task branch was deleted. |
| Physical playback acceptance | Pending | Replay the missing-target seek in Safari against the intended media node; no deployment or physical replay is claimed here. |

**Scope decision:** This is a direct repair to existing VOD playback behavior.
It does not add a feature flag or a new enable switch. Existing Developer
settings use advisory readiness for optional capabilities; no optional
capability is introduced by this change.

**Focused validation, 2026-09-23:** The first combined Node run found a
fixture error in the new presentation case: its stubbed executed seek had no
`frameFloor`, so the real settlement function accepted a null frame sequence.
The harness now initializes that floor as production does. The rerun of
`node --test tests/playback/seek-control.test.js` passed all 19 tests;
`tests/playback/web-control.test.js` passed in the combined run. The four
`tests/operations/test_docs_index.py` contracts passed, and
`git diff --check` was clean. No physical Safari run or server deploy is
claimed.

**Fast-lane preflight, 2026-09-24:** The first corrected-head run stopped at
`make history-check`, which required explicit retained-check mappings for
commits `ba6e39ca0` and `528316c24`. The
[`regressions.d` entry](../../validation/regressions.d/ba6e39ca-web-vod-seek-deadline.toml)
maps both to the focused web check. See PR #478 for the final-head gate
outcome; earlier runs cannot qualify a newer commit.

The next preflight passed history coverage and failed the doc status audit:
this newly indexed page lacked a `**Status:**` header. That header is now
present and `python3 -m unittest discover -s tests/operations -p
test_doc_status_audit.py` passes 3/3.

**Forgejo draft limitation:** The API accepted PR creation but reported
`draft: false` despite the requested draft flag. The conversion endpoint
returned HTTP 405, and a patch also left `draft: false`. Any fast-lane run
on the initial commit is superseded by the corrected head; only the final
head's result can qualify this PR.

**Final-head gate, 2026-09-24:** Preflight, web syntax, Windows compile,
Rust gate, and Main promotion gate passed for `043f09521`. Forgejo reported
the PR mergeable against unchanged `main`, then merged it as `c327b2a6`.
The remaining acceptance step is a physical Safari replay of the missing
media seek on the intended server.
