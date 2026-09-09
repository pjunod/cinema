# Live TV says what is on, and keeps playing in picture-in-picture

Build: 120
Issue: #912

Channel rows now carry the programme on now, a bar running to its end, and
what is next. A segmented control switches between that list and a half-hour
guide grid with a red now line; the choice is remembered. Search matches the
number, the callsign, and the programme on now, so typing what you can see on
screen finds the channel showing it.

Picture-in-picture works on the live surface. Entering it backgrounds the app,
which previously released the tuner — the release now happens only when
picture-in-picture is not running, so the window keeps playing and its lease
keeps being renewed.

A future programme opens details and nothing else. There is no recording and
no scheduling behind the guide, so nothing offers them.

On Apple TV the overlay routes every remote press through the shared input
contract: a direction on a hidden overlay reveals it rather than changing
channel, the channel list is preview-then-commit, and four seconds of quiet
hides the overlay while playing.
