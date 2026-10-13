# Prepare growing playlists before native attachment

Build: 237
Issue: #914

Growing playback now waits for the first successful playlist response before
handing its URL to AVPlayer. Master playlists also wait for their media
children; transport failures retain bounded ingress recovery before attachment. This prevents the native 20-second resource wait
from expiring during valid server preparation. Stop and replacement still
retire pending attachment. Ingress failover also retires the previous native
item's surface faults, preventing an old preparation error from covering
recovered video.

The [October 13 root-cause record](../streaming/PLAYBACK-START-FAILURES-2026-10-13.md)
contains the reproduction, server inventory repair and separate audio-engine
failure. Physical deployment and acceptance remain open.
