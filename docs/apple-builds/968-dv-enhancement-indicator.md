# Show which Dolby Vision enhancements are active

Build: 268
Issue: #968

The player's badge row now carries a compact teal pill for each Dolby Vision
enhancement the server reports as active — **FEL**, **RPU** and **DYN** — right
after the dynamic-range badge. Inactive enhancements get no pill and no text in
the row. Playback information gains an **Enhancement** row (Now decoding) with
one plain line per enhancement saying what it does or why it is not in use.

The answer is computed by the server on the HLS session response, so every
client shows the same thing. For processed HDR10 (HDR10-E) the pills and the row
appear only while the player holds the current processing report for the active
generation, so they clear on seek, fallback or replacement together with the
HDR10-E label. Non-Dolby Vision sources and older servers show nothing new.
This build note records source behavior; it does not claim a TestFlight upload
or device installation.
