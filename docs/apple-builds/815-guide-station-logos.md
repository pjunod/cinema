# Draw station logos in the Live TV guide

Build: 209
Issue: #815

The Apple TV and iOS Live TV guides now draw HDHomeRun's station artwork
wherever a station is named: channel list rows, grid channel headers,
programme details, the picture badge and the fullscreen identity tile. Which
address is used is the shared `station_logo` rule in
`tests/playback/live-tv-guide-cases.json`, the same one the web and Android
guides answer. The callsign stays until a logo decodes, and artwork is fetched
on a connection that never carries the account token. Merge does not publish
or install this build.
