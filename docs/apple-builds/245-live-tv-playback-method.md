# Show what Live TV copies and transcodes

Build: 136
Issue: #245

Live TV shows the server-selected playback method in the Apple TV preview,
fullscreen overlay, and Info sheet. Direct stream means both tracks are copied
without transcoding; audio-only, video-only, and combined transcoding are named
explicitly. Info separates delivered codecs, dimensions, audio channel count,
packaging, and the reported video encoder. The start response supplies these
facts immediately, and stopping or changing channels clears them.

Typed HTTP 409 responses from ordinary playback and Library channels display
the server's reason and error code. An untyped response still shows the status;
authentication errors retain their existing sign-in handling.

The companion server correction accepts measured FFprobe schema additions
without mistaking them for changed media. Non-default metadata and codec,
geometry, stream identity, duration, and reported-profile changes still fail
source verification. Build 136 has not yet been installed on a physical device.
