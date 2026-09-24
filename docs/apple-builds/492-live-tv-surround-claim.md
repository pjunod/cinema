# Live TV claims the audio route's channel count

Build: 182
Issue: #492

The Live TV start envelope now tells the server how many channels the active
audio route carries (between stereo and 5.1) instead of a fixed stereo, so a
5.1 broadcast the server has to convert arrives as 5.1 AAC on a surround route
and is no longer folded to stereo.
