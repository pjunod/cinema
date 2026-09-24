# Web VOD seek repair — implementation status

**Updated:** 2026-09-23 · **Branch:** `codex/web-vod-seek-deadline` ·
**Base:** `main` at `99d4abf8c` · **State:** implementation written; review and
validation pending in a separate clone.

Companion to [the RCA and fix proposal](WEB-VOD-SEEK-MISSING-MEDIA-RCA-AND-FIX.md).
This page records which parts of the client repair have been built and which
evidence is still owed. The pull request will contain the implementation,
review response, and fast lane result together.

| Step | State | Evidence or next action |
|---|---|---|
| RCA and Fable design review | Done | Keep VOD local; use the existing 20 s seek deadline and one fenced reopen. |
| Client implementation | Written, not yet reviewed | Local VOD intents have one 20 s fallback; progress watch, `waiting`, and startup watchdog yield while target media is missing. |
| Focused regression cases | Written, not yet run | Cover target coverage, `seeked` alone, superseded intents, the 20 s boundary, late target coverage, and a later true 8 s stall. |
| Adversarial implementation review | Pending | Run once when the PR is ready to merge; address findings before fast lane. |
| Fast lane | Pending | Run after review fixes, then record exact commands and results here and in the PR. |
| Merge | Pending | Merge only after the reviewed candidate passes the requested fast lane. |

**Scope decision:** This is a direct repair to existing VOD playback behavior.
It does not add a feature flag or a new enable switch. Existing Developer
settings use advisory readiness for optional capabilities; no optional
capability is introduced by this change.
