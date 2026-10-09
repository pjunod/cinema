# Subtitle reliability — physical verification

**Status:** current case contract; individual results belong in the completion
ledger · **Reconciled:** 2026-10-09.

Companion to the [subtitle completion ledger](SUBTITLE-RELIABILITY-COMPLETION.md),
which records source versions, engine observations and the finite remaining
work. Use current clients and an identified server tree. The original Apple
171/Android108 target and unconditional PGS-refusal expectations are historical.
The user excludes physical Apple TV and iPhone18 from this effort. Available
Apple handhelds, Android devices and browsers may be used; simulator or engine
fixtures must be labelled as such.

Record each case separately: platform/build, source/file/track, outcome,
visible cue behavior, video continuity and why any case was not run. A
synthetic engine check proves that engine mechanism; it does not establish
library UI selection, provider operation or a different hardware display.

## 1. Automatic selection preserves the delivered video range

Use an HDR/Dolby Vision title with PGS and SRT. Start without touching the
subtitle menu and record the selected track against the viewer's saved
language/mode preference. A native client advertising `pgs-v1` may select PGS
as an overlay. A client without that capability may select deliverable text
or Off, according to policy. Neither path may silently spend the requested
video range on an implicit SDR burn or show a refusal for an unrequested pick.

## 2. Manual PGS selection follows client capability

On overlay-capable Apple and Android, select PGS: cues must show with correct
placement/timing, and video session/range must stay unchanged. Switch PGS A
to B and then Off; stale manifests/images must not restore a previous choice.
On a client without overlay support, record its explicit burn/refusal policy.
An HDR refusal on that client must preserve the requested range. The old
rule that every native PGS pick must be refused is superseded.

Include a warm track and a cold/late manifest, an active cue seek, a clear
followed by a gap and authored colors/alpha. Preserve known corpus and
output-mode limitations as individual results.

## 3. Text selected during play recovers after preparation

Start an SRT title with Off, then select its track. Preparation may finish
before the first control observation; the first ready response must therefore
restore cached empty rendition data without requiring an observed warming
edge. Cues must continue through later ready windows and complete-track
publication. Seek forward and back and confirm source timing; Off remains Off.
Record cue availability separately from preparation duration.

## 4. A quality/item handoff preserves text intent

With cues visibly showing on Apple, perform a quality handoff and confirm
that the same selected text continues. Repeat with Off and verify it stays
Off. A response/callback from the superseded item cannot apply to its
replacement. Record whether the switch actually occurred.

## 5. Dual-audio styled subtitles preserve their routing policy

Use a Japanese/English anime file with ASS. Record the selected audio and
subtitle route. Direct container playback may render an embedded styled track
without a re-encode. Session-mode ASS needs a burn and is not automatically
chosen when the policy would spend the video range. A manual supported choice
must produce visible authored cues; a refused choice must explain the limit.
This case is not an unconditional promise that every ASS default is on.

## 6. Failed captions notify without stalling video

Use a synthetic failure fixture or an existing unreadable subtitle; never
remove or corrupt real library media to create a case. Control reports
`delivery.subtitle_readiness=unavailable`. The client shows one nonfatal
notice per current selection/seek intent, with the video and selected track
remaining attached. Repeated unavailable exchanges do not repeat the notice;
a delayed response cannot notify after Off, a new intent or teardown.

The terminal subtitle-503 experiment is retired. Actual macOS AVPlayer stayed
at time zero for 18 seconds on permanent 503+`Retry-After:2`, while an empty 200
control advanced to 17.75 seconds. Physical Pixel Fold Media3 advanced to 14.22
seconds in a 15-second 503 run. Failure belongs to the control contract; it
must not become a caption transport refusal that blocks the picture. Do not
re-enable a historical Developer switch or treat readiness advice as a gate.

## 7. Direct-play styled selection does not reopen video

On Android, select embedded ASS or `mov_text` in direct container playback.
Cues must show without creating another server session or changing quality.
Record server session/media-item identity as well as visible continuity.

## 8. Each rendition displays its selected language

Use a title with two or more subtitle renditions. Select each in turn and
confirm the displayed language. The master declares `CLOSED-CAPTIONS=NONE`;
selecting an ordinal must not resolve to a phantom CEA-608 option. If the old
symptom cannot be reproduced, say so. Keep available handheld results
separate from excluded TV hardware.
