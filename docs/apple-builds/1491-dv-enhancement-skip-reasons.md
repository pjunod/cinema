# Show the real reason a Dolby Vision enhancement was skipped

Build: 295
Issue: #1491

**Status:** built; focused native tests passed; hardware qualification and device acceptance pending.

Playback Info on iOS and tvOS now shows the server's generation-scoped reason
when a deferred Dolby Vision enhancement is declined or still qualifying (for
example, a GPU recipe not yet measured on this server), instead of generic
text. The reason clears when the play is released or replaced. Enhancement
badges still describe only what is actually being delivered; a late
qualification that activates during the same play updates them normally.

iOS and tvOS builds passed and 15 focused `EffectiveProcessingPresentationTests`
passed on the earlier checkpoint. Android build 194 carries the same wording
(10 `PlaybackEnhancementTest` cases and lint). Physical device acceptance is
not claimed.
