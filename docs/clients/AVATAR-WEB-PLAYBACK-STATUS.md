# Avatar web playback — diagnosis and merge status

**Status:** open · **Owner:** Codex, Avatar web stall chat · **Updated:** 2026-10-20

Companion to [the playback surface contract](PLAYBACK-SURFACE-CONTRACT.md).
This ledger tracks the Safari buffering overlay and false-stall investigation.

## Current state

Isolated branch `codex/avatar-web-stall`; [PR #1247](http://forge.lan:3000/noirr/plurx/pulls/1247)
is owned by Codex for this investigation and remains draft. A bounded observer
repair is implemented; regression coverage is being added. Review has not run.
Lower model GPT-6 Luna completed a bounded, read-only source trace.
Review and test runs are deferred until the implementation is ready, as requested.

## Incident evidence

At 01:24:34 UTC on October 21, Safari attached Avatar: Fire and Ash through
native HEVC HLS on lab6. The server reported ready playlists. At 01:24:45,
the client declared a presentation stall despite 56.4 seconds buffered. It
reopened at 01:24:57; the replacement buffered 51.2 seconds but again reported
no presentation, then exhausted recovery at 01:25:18. No Play permission
rejection was recorded. The viewer reported moving video under the overlay.

## Confirmed implementation defect and incident limits

The viewer confirmed that the picture visibly moved under the overlay.
`armHitchDetector` discarded callbacks with suspicious future media timestamps
before incrementing `controlPresentedFrames`. The existing progress sampler
requires both the element clock and that count to advance. Thus a diagnostic
metadata filter could turn real delivered frames into false no-presentation
evidence, retain a buffering surface, and spend the automatic recovery budget.

The repair counts current owned callbacks before filtering their timestamps.
Suspicious timestamps still cannot settle a seek or enter hitch/rate history.
Startup presentation uses the existing element-clock-plus-callback proof.
No fallback signal, recovery timer, or configuration switch is added.

Same-tab and cold-load Safari resumes during diagnosis delivered about 24 fps
and cleared their startup surfaces normally. A bounded visibility experiment
also retained frame callbacks while hidden and did not report a stall.
The original failed attempt did not retain its hitch/metadata counters, so
its exact trigger is unconfirmed. The regression must prove the identified
counter defect without claiming the original physical failure was reproduced.

## Remaining work

Complete focused regression coverage; obtain one
adversarial review; run focused evidence and the fast lane; merge green.
Physical acceptance and deployment will be reported separately from merge.
