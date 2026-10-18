# Retain the saved position when an Apple resume seek is interrupted

Build: 263
Issue: #1181
PR: #1182

**Status:** built; physical iPad acceptance remains pending.

Resume preparation now checks AVPlayer’s seek completion result. An
interrupted seek gets one retry at the same saved position within the existing
deadline. A newer viewer destination cancels that retry.

If preparation still fails, playback stops and keeps the requested position
for Retry and Close. The initial open, a subsequent reopen, and the visible
detail progress all retain the same destination instead of adopting an
unprepared player’s zero clock or brief introductory playback.

Regression coverage exercises interrupted seeks, successful bounded retry,
superseding commands, permanent VOD failure through initial open and Retry,
and closing before attachment. Physical diagnostics also contained repeated
player creation; isolated presentation tests did not reproduce it, so this
change makes no claim to resolve that separate pattern.
