# Keep Library channel failures visible and controls readable

Build: 137
Issue: #245

Library channels observe player startup and playback failures and show the
error below the preview. Guide refreshes preserve that error; stopping or
starting a new tune clears it. Failures from a replaced player item cannot
overwrite the current channel. The join-position adjustment waits for a ready
player with a finite media time.

Apple TV uses the existing readable button style throughout the channel view,
including programmes, favourites, playback actions, and layout controls.
Selected programmes use the contrasting foreground for the accent background.

The companion server fix waits for the first completed copy segment before
validating its initialization file. FFmpeg can create that file empty before
writing its contents; file existence alone is not publication readiness.
