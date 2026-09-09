# Restore converted Dolby Vision and Live TV starts on Apple TV

Build: 127
Issue: #215

A Profile 7 title that reaches growing copy HLS now keeps its Profile 8.1
conversion. The GOP-aware segmenter rewrites each RPU after ffmpeg muxes it,
writes a matching Profile 8 configuration record, and refuses the legacy
retry that could silently turn the same presentation into HDR10.

Apple TV keeps Live TV's 90-second restart-safety marker in tvOS's writable
cache area. The marker still lands before a tuner request leaves the device,
but a missing Application Support directory no longer latches every channel
start into the device-storage error. This removes the client-side refusal;
physical tuner startup and reception remain separate server-side checks.
