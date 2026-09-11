# Apply the playback decision to Library channels

Build: 138
Issue: #245

Apple Library channels now request the per-file playback decision before
creating their scheduled session. Direct and remux decisions copy video;
audio conversion and Dolby Vision handling follow the same helpers as ordinary
library playback. Incompatible sources retain the server's transcode decision.
The create request carries the exact capability snapshot used for the decision.

Playback failures include the Apple error domain and code, including the first
underlying error, so a generic “Cannot Complete Action” has actionable detail.
No capability URLs or error user-info dictionaries are displayed.
