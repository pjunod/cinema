# Wait for progressing growing-stream preparation on resume

**Status:** built; physical acceptance open

Build: 218
Issue: #914

A resume correction no longer reports that preparation failed after 15 seconds
while its exact growing session is still actively preparing an unpublished
playlist. Fresh, known producer progress can extend that preparation within a
60-second total bound. First publication starts one 15-second item-readiness
budget within the same cap. Native AVPlayer failures remain immediate.
Same-session ingress failover restarts its status poll and rejects late
predecessor responses.

The [root-cause and implementation record](../clients/APPLE-GROWING-RESUME-PREPARATION.md)
separates the proven premature Naked Gun timeout from its later, unclassified
AVPlayer resource error. Build 217 has not been installed on the physical
Apple TV; simulator validation is not physical playback acceptance.
