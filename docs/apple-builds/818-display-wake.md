# Keep the screensaver off while video plays

Build: 209
Issue: #818

The screensaver no longer comes on over Live TV on Apple TV, in fullscreen or
in the guide's picture, and iOS auto-lock no longer interrupts a playing
picture. The app now owns display wake itself instead of leaving it to
AVPlayer's implicit assertion, which does not survive the picture moving
between surfaces: every player (films and episodes, Live TV, Library Channels)
holds the display awake while its video is playing or buffering toward play,
and lets go the moment it is paused, stopped or fails. Audiobooks never hold
it, and a picture sent to an AirPlay receiver leaves the phone free to lock.
