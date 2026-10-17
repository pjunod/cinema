# Continuous quality on the installed shipping Apple app

Drives the **installed** `tv.plurx.app` (a Release build from `main`, as
`scripts/ship-physical` installs it) through the continuous-quality campaign:
15 manual quality changes, Auto through fixed links of 100/10/25/10/25 Mb/s,
then a 3 Mb/s downshift and 25 Mb/s recovery.

- `proxy.js` puts the repository's `ShapingProxy` (`scripts/playback-lab`) in
  front of a real server. The UI test logs a `link_request`; the proxy follows
  the test log and applies the stage, because the on-device test runner has no
  local-network permission.
- The app is launched with `-plurx.origin <proxy>`, which overrides the saved
  origin for that launch only and reuses the stored login. Nothing is persisted.
- `Tests/` holds the iOS/iPadOS and tvOS UI tests. Each change records the
  app's own Playback info rows (Frames/Audio at the switch, Buffering
  interruptions, Player display size, method) and any Loading/Buffering surface
  seen in an 18 s window. `shaper-evidence.json` keeps the link stages and the
  app's client-log events.

```
PLURX_DEVELOPMENT_TEAM=<team> scripts/cq-native-shipping/run \
  <device-udid> http://<server>:32400 <this-mac-lan-address> 38411 "<movie title>" campaign <out-dir> [ios|tvos]
```

Run it inside the logged-in GUI session (codesigning needs the login keychain;
over SSH, bootstrap it as a `gui/<uid>` launchd job). The Mac must allow
Automation Mode (`automationmodetool`), and each device needs
Settings → Developer → Enable UI Automation.

`run` exits non-zero, and writes `UNSHAPED=1` to the phase log, unless the
shaper evidence shows media bytes delivered through the proxy in stage 0 and
in every stage the test requested (`check-evidence.js`); an unshaped run can
never read as a pass, whatever the UI test reported.

**Caveats.** Three things about the shipping app behind a proxy that the
harness cannot change:

- A `401`/`403` answered through the proxy makes the Release app's bootstrap
  clear the Keychain login. Point the proxy only at the server the app is
  logged in to, and do not shape the first requests of a cold launch.
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
