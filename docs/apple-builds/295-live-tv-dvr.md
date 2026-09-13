# Record and remind from the guide, on iPhone, iPad and Apple TV

Build: 147
Issue: #295

Every guide cell now carries **Record · Record series · Remind me** beside
Watch, and a Recordings shelf sits on Home beside the libraries. A capture that
has finished plays through the ordinary VOD path, because the server files it
into a `recordings` library like any other media rather than inventing a second
player for it.

The guide grid marks what is already asked for: a filled dot for a scheduled
airing, a ring for a series rule that will match it, and a red dot while a
recording is actually running. The marks come from one `/api/v1/dvr/marks`
read per visible window rather than a per-cell lookup, so scrolling a fortnight
of guide does not become a fortnight of requests.

Reminders are mirrored, not polled. While the app is open it keeps the
server's armed reminders in `UNUserNotificationCenter`, so a phone that is
asleep at the right minute still buzzes; an open app shows the overlay
directly. A reminder created, moved or deleted on another device while this
phone stays closed is not reflected until it next opens — APNs would close
that gap and is not in this change.

The client refuses nothing the server allows. `dvr.enabled` is a server
setting, and when it is off the actions are simply absent rather than present
and disabled, which is the honest rendering of a server that will answer 409.

Forty-one focused tests cover the marks arithmetic, the request bodies, the
reminder mirror's add/move/cancel arithmetic and the overlay's lifecycle on
both the iOS and tvOS simulators. No capture has been played back on real
hardware; raw MPEG-TS through the real decision path is an acceptance step of
its own.
