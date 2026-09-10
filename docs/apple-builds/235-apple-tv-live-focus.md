# Make Apple TV Live TV focus deterministic and readable

Build: 133
Issue: #235

Apple TV Live TV now processes guide moves in one ordered focus state, gives
the toolbar sole focus ownership when leaving the first row, and uses the
platform focus-scrolling list for On now. Guide cells show an explicit focus
ring, while the toolbar, fullscreen controls, More sheet, and Layout sheet own
readable foreground and background colors instead of inheriting red-on-red
tvOS tinting.
