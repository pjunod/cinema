# The playback surface is one projection of the player, not four fields

Build: 147
Issue: #278

`failed`, `playbackError`, `playbackFailureTitle` and `playbackNotice` are
gone. In their place is one `PlaybackSurfaceModel`: a pure reducer over typed
faults, evidence and identities, which every former writer now raises into
instead of painting a string at the view. The classes, the sources and the
timings are a transcription of
`tests/playback/playback-surface-contract.json`, and `AppleClientTests` runs
all 54 of that fixture's ordered-event sequences against the model, so the
Apple presenter and the web one cannot drift.

Three things a viewer can see change, and each closes a reported defect.

A readiness deadline can no longer put a full screen over a picture that is
still playing. When the compatibility ladder has a rung left, the deadline is
a refusal of the change that failed — a banner over a predecessor that keeps
playing. When it has none, `fail()` stops the player first and then says
playback is stalled, which is the one new `player.pause()` this build adds on
that path.

The pre-start black-frame ladder no longer exhausts in silence. Six seconds of
audio over a black picture with no rung left used to do nothing at all; it now
stops the player and offers Try again and Close.

A refusal the server explained is read rather than discarded. `PlurxAPI.check`
keeps `{code, message}` for every non-2xx it can parse, so a 503
`startup_timeout` reaches the viewer as the server's own sentence instead of
"Server returned 503". 401 and 403 stay status-shaped so the sign-in path is
untouched, and a 409 is still a conflict for every matcher that reads one.

Playback debug gains a SURFACE section: what is drawn, which fault class, from
which source, for which media generation and viewer request, and the last
sixteen faults with what cleared each one. The same four events go to the
client log, `surface_disagreement` among them — each occurrence is a place
where something started the player after its owner had stopped it, and the
iOS lock-screen play command is the first documented one.

The presenter has no side effects: it never pauses, resumes, seeks, reopens,
cancels a timer or reports to the control plane. No threshold, budget,
detector or ladder moved.
