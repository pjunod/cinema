# HDHomeRun Live TV on iPhone, iPad and Apple TV

Build: 116
Issue: #902

Live TV is always visible in navigation. A dedicated live player supports
channel selection, pause/resume, mute, fullscreen and explicit stop without
writing library watch progress or invoking the finite-media player.

Settings → Developer exposes saved tuner configuration, readiness, separate
runtime enable/disable actions, and exact old-owner physical-fencing recovery.
Read the safety requirements there before enabling: a scanned HDHomeRun on a
private IPv4 address, a committed voter owner, compatible cluster binaries,
FFmpeg H.264/AAC support, scratch space and a session budget are required.
DRM, DVR, guide scheduling and captions are not supported by this first profile.

One app-wide serialized lease releases the previous capability before opening
another. A token-free durable marker blocks ambiguous starts across app death
and profile changes; playback without media progress stops within 30 seconds.
Fourteen focused tests pass on both iOS and tvOS simulators. Hardware playback
and final effort qualification remain separate acceptance steps; compilation
does not prove a household tuner or decoder works.
