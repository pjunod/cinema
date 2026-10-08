# Cinema remotes — setup, recovery and acceptance

**Status:** implementation setup guide; end-to-end and physical acceptance open ·
**Updated:** 2026-10-08.

Use the [status ledger](TV-REMOTE-AND-COMPANION-STATUS.md) to identify reviewed
source and remaining work. These instructions describe the implementation
being integrated. They do not claim that every packet has reached `main` or
that a particular TV, adapter or notification provider has been qualified.
The [build packets](TV-REMOTE-AND-COMPANION-IMPLEMENTATION.md) contain developer
ownership and compile commands; the [protocol](TV-REMOTE-PROTOCOL.md) defines
pairing and command authority.

## Choose the receiver

| TV setup | Physical remote path | Phone/tablet path |
|---|---|---|
| Android TV | Existing Android D-pad/media input | Cinema receiver and same-account companion |
| Apple TV | Existing Siri Remote/tvOS input | Cinema receiver and same-account companion |
| Linux/Pi over HDMI | Kernel CEC device where available, or separately installed supported libCEC adapter | Foreground Cinema browser receiver |
| Mac over HDMI | Supported USB-CEC adapter and separately installed libCEC | Foreground Cinema browser receiver |
| Windows over HDMI | USB-CEC source adapter; native launcher qualification remains open | Foreground Cinema browser receiver |

An HDMI video connection alone does not establish that the computer exposes a
CEC device. Inspect the actual adapter and operating system. The desktop path
currently uses Chromium, Chrome or Edge; its extension is not a Safari or
Firefox package. Follow the exact source installation and dependency steps in
[the desktop CEC guide](../../clients/desktop-remote/README.md).

The local desktop helper receives CEC while the browser has an explicit active
binding. Phone commands use the Cinema server and an approved grant. These are
separate saved choices. Turning off server remote control does not turn off a
previously enabled local CEC binding.

## Pair a phone with a foreground screen

1. An administrator enables the server's Cinema remote-control switch in
   Settings → Developer. Readiness is advisory: saving the choice succeeds
   even when a client, adapter or provider is unavailable.
2. Sign in to the same Cinema server as the same human user on both devices.
   Pairing is not household-wide authority and does not grant access to another
   user's playback or administrative screens.
3. On Apple/Android, enable Cinema remotes in Developer. On a browser TV,
   open Developer device preferences, register a screen name, and enable
   receiving. Browser receiving needs Web Locks in a secure context. Only one
   tab can own that browser installation at a time.
4. On the TV choose **Pair a phone** or **Show pairing code**. On the phone,
   open the Cinema remote and choose that available screen. Scan its QR code
   or enter the eight-digit code. Codes expire after two minutes. A QR link
   identifies a screen on the already authenticated server; it cannot choose
   a different server to receive your credentials.
5. Physically approve the named phone on the TV. Pairing approval is restricted
   to local input. The network remote cannot approve itself.
6. On the phone choose **Use as remote**. Pairing alone does not acquire
   control or start playback. If another phone has control, **Take over** is
   an explicit separate choice.

Each control is offered only when the current TV screen supports it. Search
text goes to Cinema's current search field. Audio, subtitle and quality choices
use the app's playback owner. Account, administrative, destructive and unknown
system presentations remain outside the network action set.

Closing the phone remote leaves TV playback running. Reopening requires an
explicit control request. A delayed Select is discarded after TV focus, route,
controller or receiver identity changes; it is never replayed on a replacement
screen.

## Make the remote easy to find

Foreground suggestions use grants saved on that phone. One available paired
TV offers its remote; multiple TVs offer a screen picker. A dismissal lasts
for the current app session, and per-screen suggestion preferences can persist.
An available screen means Cinema can currently reach its foreground receiver;
it is not evidence that the phone is physically near the TV.

Background invitations are a separate opt-in implementation packet. Provider
configuration and actual suspended-device delivery are not yet evidenced.
The foreground remote remains useful without them. Do not expect a browser
page or a suspended iPhone app to poll continuously. Final background setup
instructions must name the implemented broker, consent and permission flows
before that packet graduates from Developer.

The notification is only a generic invitation. An OFF choice saved while offline
needs to synchronize with the home server before that server can stop new sends.
A provider alert already sent can also arrive after the server has accepted OFF. Android's offline check can
recognize the local installation and whether invitations are still enabled there;
the opaque payload does not identify an individual screen's consent. A tap always
checks the exact current screen, consent, login and pairing with the home server.
It never acquires control or starts playback automatically.

## Connect home to the invitation broker

This is the B09 configuration contract being integrated, not a provider-delivery
qualification. Build and initialize the separate broker using its
[operator guide](../../crates/plurx-notification-broker/README.md), publish its
loopback listener through verified HTTPS, and configure only the intended home
publisher. The home daemon never stores APNs/FCM device tokens or provider keys.

Set `PLURX_INVITATION_PUBLISHER_CONFIG` in the home daemon's service environment
to an operator-owned JSON file with this exact shape. Values below are examples;
use the identities and generation from the actual broker manifest and home.

```json
{
  "broker_origin": "https://broker.example.com",
  "publisher_id": "f482d5c0-12a4-4b07-9082-7484b60d7c58",
  "broker_generation": "a640bd62-1a69-4f06-8809-b725310bfad5",
  "server_instance_id": "your-exact-home-instance",
  "publisher_secret_file": "/srv/cinema/private/publisher-proof.txt"
}
```

Use an HTTPS origin with no path, query, fragment or user information. Keep the
publisher proof in a separate protected file, containing its canonical
43-character unpadded base64url text. It must match the broker manifest's
SHA-256 hash of those text bytes. Use absolute file paths. Keep this proof and
all provider keys out of the repository and client builds. Restart the home
daemon after changing the configuration or proof; it loads them once per run.
A restart does not discard retained broker cleanup.

Enable the independent server invitation switch in Developer and save the
phone's per-screen invitation choice. Provider or permission readiness remains
advisory and must not reject that saved choice. Apple signing must match the
actual `tv.plurx.app` topic and APNs environment described in the
[native build contract](TV-REMOTE-NATIVE-INVITATIONS-BUILD.md#apple). The phone
claims its short-lived ticket directly with the broker; the home login and
pairing proof never accompany that claim. Home confirmation is required before
provider dispatch becomes eligible. A ready response means setup is eligible,
not proof that a physical notification arrived.

## Build Android push or choose the resident receiver

The Android build accepts public Firebase application configuration through
these Gradle properties. Use the values for your registered Android app;
provider service-account credentials belong only on the broker.

| Gradle property | Firebase value |
|---|---|
| `cinemaFirebaseProjectId` | Project ID |
| `cinemaFirebaseApplicationId` | Firebase application ID |
| `cinemaFirebaseApiKey` | Public application API key |
| `cinemaFirebaseSenderId` | Messaging sender ID |

Pass the properties to the normal Android build, for example from
`clients/android`:

```bash
./gradlew :app:assembleDebug \
  -PcinemaFirebaseProjectId=your-project \
  -PcinemaFirebaseApplicationId=1:123456789:android:abcdef \
  -PcinemaFirebaseApiKey=your-public-application-api-key \
  -PcinemaFirebaseSenderId=123456789
```

The implementation initializes the Firebase messaging SDK from these values
and leaves automatic token initialization off. An explicit push-enrollment
request obtains the token. An incomplete configuration leaves push unavailable
without discarding a screen's saved ON choice. It does not start a resident
service as a fallback. Building with these public values is not proof that
Google accepted a broker credential or delivered a notification.

While Android invitation acceptance is open, registration, permissions,
per-screen choices and push/resident controls live in **Settings → Developer
→ Cinema remotes**. Human installation list/delete and local-record recovery
remain in **Remotes & devices**, including when foreground remotes are OFF.

For the resident alternative, choose **Resident** for each screen that should
invite this phone and save that choice to the home. Separately enable the local
resident receiver choice and press **Start resident receiver**. The service
requires Wi-Fi and notification permission, shows an ongoing notification with
**Stop**, and binds its own requests to the selected Wi-Fi network. Other app
traffic keeps its existing routing. Stopping the service preserves per-screen
choices and records local OFF before trying to synchronize home availability.
The service does not restart at boot or after the app process dies; start a new
run explicitly. A saved receiver choice does not mean a run is active.

## Recover without confusing saved pairing and current control

| Symptom | What to check or do |
|---|---|
| TV absent/offline | Open Cinema on the TV, confirm its receiver choice, account/server and connectivity. Refresh the picker. |
| Screen temporarily unavailable after reconnect | Old and new receiver owners can briefly overlap. Wait for the old presence to expire, then select the uniquely available screen. |
| Browser reports another tab owns this screen | Return to the owning tab or move focus away from it and foreground the intended tab. Do not register duplicate installations to work around the lock. |
| Local CEC stopped after switching tabs or windows | Return to Cinema and explicitly bind the tab in the extension again. |
| Commands stop after another phone takes over | Review the screen, then explicitly request control or take over. Held gestures do not resume automatically. |
| Controls unavailable in a modal or system screen | Use the physical remote to finish or dismiss that presentation. Read the packet's navigation inventory for unfinished ordinary browsing controls. |
| Code expired or approval result is unknown | Close pairing, show a fresh code on the TV and pair again. Inspect the grant list before retaining obsolete grants. |
| Saved pairing no longer works | Revoke the old grant and pair again. **Forget saved pairing** removes a local proof; it does not claim server revocation. |
| An updated native app asks to pair again | Pair again in the current server/account. Apple no longer reuses older origin-less proofs. Android now uses an unambiguous origin/instance/account namespace; older Android records already contained an origin but are not silently migrated into the new namespace. |
| Selling/resetting a receiver | Remove/reset its TV registration. This revokes its paired grants. Merely closing the app does not perform this reset. |
| Invitation transport reports `migration_remediation` after broker replacement | Restore the exact old broker origin, publisher, server identity and generation authority, then drain retained cleanup before changing scope. A compatible proof rotation can preserve that authority. If it is permanently lost, cleanup stays retained; this packet has no exceptional operator-fence command to discard it. A successful response from a new generation is not proof of old-generation revocation. After repairing the exact scope, restart the home to clear its remembered generation-mismatch state; durable cleanup survives restart. |

Network remote availability does not replace local playback controls. On the
reviewed desktop path, local pause and Stop use the existing browser player
without waiting for a server reply. Actual offline behavior with a real CEC
adapter remains a required acceptance case.

## Record device evidence before claiming support

Record the exact source/build, OS, browser/app version, TV and AVR model,
adapter/firmware, connection path, and relevant permission choices. Keep
synthetic credentials and real pairing secrets out of screenshots and logs.
For each platform, record actual outcomes for:

- Pair, decline, expire, revoke and reset; one phone versus two competing phones.
- Navigate Home, library pages, detail/episodes, search and playback choices;
  restore focus on Back and cross at least two lazy-grid viewports.
- Alternate physical and phone input, including held direction, lost release
  and a physical key at a stationary edge. A stale network Select must fail.
- Open a restricted dialog; neither Select nor Home/Back may activate the
  browsing screen behind it. Local approval must still work.
- Suspend/resume, change account/server, lose connectivity and replace the
  receiver owner. Old actions must not execute after reconnect.
- For desktop CEC: native install/uninstall, permission denial, busy adapter,
  unplug/reconnect, explicit rebind, and offline VOD/Live TV pause and Stop.
- For invitations after B09: opt-in/out, permission denial, token rotation,
  duplicate events, cooldown, stale tap, and resident-service Stop where
  legitimately supported. Record real provider/device evidence separately.

Compiler results, injected transports and simulator checks belong in the
ledger, but cannot stand in for these physical results. The parent reviews
software packets; the designated batch coordinator owns combined qualification
and the merge into `main`.
