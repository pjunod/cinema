# Add text subtitle translation and shared caption reads

**Status:** open · final qualification

Build: 277
Issue: #1292

Continues the subtitle setup work in #1283 with native client support for
translating an existing text subtitle and reading committed AI captions during
shared playback. A translation result is selected only after the viewer uses
it and the client revalidates the current subtitle identity. Shared caption
reads do not start work on the Source.

This build remains in qualification. Packaged model, physical-client and
broader acceptance evidence are still open.
