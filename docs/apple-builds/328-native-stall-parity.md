# Preserve native quality through unattributed stalls

Build: 162
Issue: #328

Apple playback now treats a stationary film clock as an unknown presentation
stall instead of a decoder failure. Its bounded repair keeps the current
quality, HDR, audio, subtitle, offset, and film position; only an actual
AVPlayer item error may enter the compatibility ladder.
