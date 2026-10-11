# Read a prepared successor's frame cadence from large and MPEG-TS segments

Build: 231
Issue: #978

A prepared quality handoff needs the successor's frame duration as its
one-frame tolerance, and the segment itself is the only source for HLS.
The cadence reader refused fragments over 16 MB and every successor without
an `EXT-X-MAP`, which is every rolling transcode, so those handoffs fell back
to an in-place reopen. It now reads an fMP4 segment's `moof` boxes through
bounded range requests, skipping the media, and reads an MPEG-TS segment's
video timestamps in chunks, refusing rather than guessing when a packet does
not carry exactly one frame.
