# Move Live TV enable, pause/resume and prepared handoff out of Developer

Build: 228
Issue: #987

Settings → Live TV now opens with the Enable Live TV switch and its advisory
prerequisites, which used to wait in Developer. Bounded pause/resume and
prepared quality handoff are ordinary Playback toggles, on the same saved keys
and defaults, with their readiness rows one link away. Developer keeps Cinema
remotes, screen invitations and the shared libraries card. Merge does not
publish or install this build.
