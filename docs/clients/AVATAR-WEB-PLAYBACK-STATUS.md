# Avatar web playback — diagnosis and merge status

**Status:** open · **Owner:** Codex, Avatar web stall chat · **Updated:** 2026-10-20

Companion to [the playback surface contract](PLAYBACK-SURFACE-CONTRACT.md).
This ledger tracks the Safari buffering overlay and false-stall investigation.

## Current state

Isolated branch `codex/avatar-web-stall`; diagnosis in progress. No PR yet.
Lower model GPT-6 Luna completed a bounded, read-only source trace.
Review and test runs are deferred until the implementation is ready, as requested.

## Incident evidence

At 01:24:34 UTC on October 21, Safari attached Avatar: Fire and Ash through
native HEVC HLS on lab6. The server reported ready playlists. At 01:24:45,
the client declared a presentation stall despite 56.4 seconds buffered. It
reopened at 01:24:57; the replacement buffered 51.2 seconds but again reported
no presentation, then exhausted recovery at 01:25:18. No Play permission
rejection was recorded. The viewer reported moving video under the overlay.

## Remaining work

Identify the missing presentation evidence and reproduce the causal chain;
repair the existing observer; add focused regression coverage; obtain one
adversarial review; run focused evidence and the fast lane; merge green.
Physical acceptance and deployment will be reported separately from merge.
