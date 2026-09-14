# The Live TV cover stops swallowing the remote, on Apple TV

Build: 155
Issue: #306

The Live TV playback overlay drew and nothing on it could be reached: moving
the remote highlighted nothing, and no press activated a control.

The cover carries a transparent full-screen layer because `PlayerSurfaceView`
refuses focus, so an overlay that has auto-hidden would otherwise leave the
remote with nothing to talk to. That layer takes every direction through
`onMoveCommand`, so while it holds focus the focus engine never sees one.

It reported a hardcoded `.fullscreenHidden`, and that state answers left,
right, up, down and Select alike with `reveal`. `focusable(false)` does not
relocate focus until the next focus update and the adapter stays attached
meanwhile, so the layer could still hold focus after the overlay was back —
and through that window every press re-showed an overlay that was already up
and restarted the auto-hide it had just cancelled. The window never closed.
The layer now reports the state it is actually in, so those presses route to
`focusControl`, which the view refuses and the framework answers by moving
focus off it.

The reveal direction of the overlay change assigned focus without waiting,
while the hide direction already waited a yield. The overlay's buttons are
inserted by that same update, so tvOS dropped the assignment; focus stayed on
the layer, and having never changed it did not trip the bounce that would
have pushed it to Play. Both directions defer now.

A programme sheet opened from the guide could let the chrome hide underneath
it, because `detail` was missing from the two auto-hide guards its four
sibling flags are in. It is in both.

Nothing here ran on a television or a simulator in the session that wrote it;
the compile is the first type-check. The behaviour is what wants confirming
on the device: enter fullscreen, let the chrome hide, then press a direction
and check that a control takes the highlight and Select works on it.
