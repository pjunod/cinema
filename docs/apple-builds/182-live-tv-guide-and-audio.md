# Make the Live TV guide selectable and restore iPhone audio

Build: 121
Issue: #182

The HDHomeRun guide source can now be selected and saved without its settings
card repaint disabling Save or leaving readiness stuck on Checking. Live TV
also establishes the iOS playback audio session used by its separate player,
so an iPhone's silent switch no longer mutes the channel audio.
