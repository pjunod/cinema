# Avatar web playback — diagnosis and merge status

**Status:** open · **Owner:** Codex, Avatar web stall chat · **Updated:** 2026-10-20

Companion to [the playback surface contract](PLAYBACK-SURFACE-CONTRACT.md).
This ledger tracks the Safari buffering overlay and false-stall investigation.

## Current state

Isolated branch `codex/avatar-web-stall`; [PR #1247](http://forge.lan:3000/noirr/plurx/pulls/1247)
is owned by Codex for this investigation and remains draft. A bounded observer
repair and regression coverage are implemented. The single adversarial review
of `fa492e1` found no actionable issues. Focused evidence passed; fast lane and
merge are next.
Lower model GPT-6 Luna completed a bounded, read-only source trace.
Review and test execution followed implementation, as requested.

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

## Review and focused evidence

The adversarial reviewer checked the diff, touched outlines and this ledger.
No actionable findings. Ownership guards remain before receipt counting, and
seek settlement still requires valid timestamps and current identity/epoch.

`node --test tests/web/hitch-lifecycle.test.js`: **7 passed**, October 20.
The production detector and progress sampler retain presentation for 20 seconds
of moving video with future callback timestamps, retire a media wait, and do
not enter persistent recovery. Missing callbacks and a frozen element clock
still enter stall handling. Invalid PTS cannot enter seek or hitch evidence.
This is a deterministic callback-metadata regression, not a reproduction of
the original Safari attempt.

Browser tracing was removed; diagnostic playback was closed and the resume
point restored conservatively to the original displayed 1:13.

## Remaining work

Run the fast lane and merge green.
Physical acceptance and deployment will be reported separately from merge.
