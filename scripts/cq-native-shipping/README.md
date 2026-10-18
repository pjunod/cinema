# Continuous quality on the installed shipping Apple app

**Suspended, 2026-10-17.** Do not run this harness or its UI tests against
the installed Release app. The iPad Pro reached the server picker and the
Apple TV reached Sign in; Paul reports that those screens cannot be escaped.
The exact startup request is unconfirmed. The `run` entry point exits before
building, starting a proxy or launching an app. The
[investigation and product bug](../../docs/playback-control/APPLE-AUTO-LOW-LINK-IMPLEMENTATION.md)
defines the evidence and session-preservation checks needed before resuming.

Drives the **installed** `tv.plurx.app` (a Release build from `main`, as
`scripts/ship-physical` installs it) through the continuous-quality campaign:
15 manual quality changes, Auto through fixed links of 100/10/25/10/25 Mb/s,
then a 3 Mb/s downshift and 25 Mb/s recovery.

- `proxy.js` puts the repository's `ShapingProxy` (`scripts/playback-lab`) in
  front of a real server. The UI test logs a `link_request`; the proxy follows
  the test log and applies the stage, because the on-device test runner has no
  local-network permission.
- The app was launched with `-plurx.origin <proxy>` and reused the stored
  login. Although the argument override is temporary, Release bootstrap can
  delete the saved token or persist a rediscovered server on startup failure.
  The launch does not preserve saved state on every path.
- `Tests/` holds the iOS/iPadOS and tvOS UI tests. Each change records the
  app's own Playback info rows (Frames/Audio at the switch, Buffering
  interruptions, Player display size, method) and any Loading/Buffering surface
  seen in an 18 s window. `shaper-evidence.json` keeps the link stages and the
  app's client-log events.

```
PLURX_DEVELOPMENT_TEAM=<team> scripts/cq-native-shipping/run \
  <device-udid> http://<server>:32400 <this-mac-lan-address> 38411 "<movie title>" campaign <out-dir> [ios|tvos]
```

**Historical operation; suspended above.** The command ran inside the
logged-in GUI session because codesigning needs the login keychain. SSH
operation used a `gui/<uid>` launchd job, Mac Automation Mode
(`automationmodetool`) and Settings → Developer → Enable UI Automation
on each device.

Before suspension, `run` exited non-zero and wrote `UNSHAPED=1` to the phase
log unless shaper evidence showed media bytes delivered through the proxy in
stage 0 and in every requested stage (`check-evidence.js`). An unshaped run
could not pass, whatever the UI test reported. The suspension exits 2 before
creating a phase log.

**Caveats.** Three things about the shipping app behind a proxy that the
harness cannot change:

- A `401`/`403` answered through the proxy makes the Release app's bootstrap
  clear the Keychain login. Even an initially unshaped launch against the
  intended server does not prove saved-session safety on all failure paths.
- A failure that is not an authentication one can make the app rediscover
  the real server and persist that origin, after which the rest of the run
  goes around the proxy. `check-evidence.js` is what catches this: a stage
  with no media through the proxy fails the run.
- Peer origins for media failover are not rewritten. A session the server
  relays or places on another node may be fetched from that node directly;
  the shaper sees nothing of it and the run fails the same check.

**Limits.** This measures the shipping binary's own reports and visible wait
surfaces, not optical, audio or menu continuity; "Not measured" switch rows
are reported as such. The 25 Mb/s recovery stage assumes a 1080p encode rung;
an original above 25 Mb/s legitimately stays on a lower rung there.
