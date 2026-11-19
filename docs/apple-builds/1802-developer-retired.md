# Developer settings retired; per-playback reusable copy

**Status:** built — compiles on the fast lane; no physical-device check claimed.

Build: 307
Issue: #1802

Settings no longer has a Developer category on iPhone, iPad or Apple TV.
Shared libraries moved to Sharing, the Cinema remote switch and screen
invitations to Remotes & devices, and F+ curves (with spool delete and
reload) to Playback, with detail in a collapsed disclosure. Pause/resume and
quality handoff now sit under Playback on Apple TV too. The player's
settings gain "Keep a reusable copy of this playback": off sends
`overrides.retain_output=false` on every create of that playback and stops
the running session's copy. Saved settings keep their keys.
