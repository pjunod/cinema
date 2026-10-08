# TV remotes and companion control — Cinema from the couch

**Status:** proposed design, implementation not started · **Written:**
2026-10-07 · **Source inspected:** `890bca0fc` · **Executes:** Paul's request
for TV-remote input and automatic phone/tablet remote discovery.

Companion to [CLIENTS.md](../CLIENTS.md) (platform strategy),
[PLAYER-INPUT-CONTRACT.md](PLAYER-INPUT-CONTRACT.md) (button behavior), and
[WEB-SHELL-LAYOUT.md](WEB-SHELL-LAYOUT.md) (web source ownership). This plan
adds screen discovery, authenticated control, and desktop CEC input. It must
preserve the existing playback owner, preparation, and recovery protocols.
All new APIs, settings, filenames, limits, and milestones below are proposals.
Existing-code observations are distinguished in §3. Recheck them before build.

**Build successor, 2026-10-07:** execute
[the implementation handoff](TV-REMOTE-AND-COMPANION-IMPLEMENTATION.md) and
[protocol contract](TV-REMOTE-PROTOCOL.md). They replace this proposal's
cross-account pairing, leader relay, WebSocket and expiry-clock choices.
[The status ledger](TV-REMOTE-AND-COMPANION-STATUS.md) records implementation
and hardware evidence; this proposal remains the original design record.

## 1. The experience to build

Turn on a TV, open Cinema, and navigate Home, libraries, details, search,
settings, and playback using Up/Down/Left/Right, Select, and Back. Keep a
visible focus indicator, restore the selected item when returning from its
detail page, and put every required action within directional reach. A
full-screen desktop Theater session should not need a mouse after setup.

When a signed-in phone or tablet opens Cinema, show a compact card:

```text
Living Room TV · Cinema is open
[ Use as remote ]                         [ Dismiss ]

While connected:
Controlling Living Room TV                 [ Change screen ]
                 ↑
             ←   OK   →
                 ↓
        Back     Play/Pause     Home
        −10 s                  +10 s
        Search / Keyboard      Audio / Subtitles
```

The connection is explicit: discovery never redirects the phone's own
playback or starts a film. A persistent Remote button opens the device list.
One available previously paired screen gets a named suggestion; several get
“Choose a screen.” Do not call a screen “nearby” based on server membership.
Dismissal suppresses that screen's suggestion for the current phone app
session; a per-device preference disables automatic suggestions permanently.

The TV's “Connect a remote” action shows a QR code and a short pairing code.
The QR opens the installed app where supported, with a mobile web remote as
fallback. Keep manual code entry available. After pairing, remember the
phone/device relationship and reconnect without another pairing ceremony.
An initially unpaired target remains available through the device picker
or the TV's pairing screen; it must not reveal another user's viewing title.

“Play on Living Room TV” belongs on media details as a follow-on to the
remote. The TV opens its own authorized playback; the phone is the controller
and is not required to upload or relay the media. Closing the phone leaves
the movie playing. The remote also works while the TV is browsing, so it
cannot be implemented as a list of active streaming sessions alone.

## 2. Platform support and honest boundaries

| Target screen | TV remote path | Phone/tablet path | Priority |
|---|---|---|---|
| Android TV / Google TV | OS-delivered D-pad, Back, and media keys; CEC handled by device firmware/OS | Native receiver inside Cinema | First release |
| Apple TV | tvOS focus and remote events; compatible TV remotes can arrive through HDMI-CEC | Native receiver inside Cinema | First release |
| Linux PC | Cinema Theater browser plus companion service and supported CEC interface; USB-CEC when necessary | Browser receiver | First desktop target |
| Raspberry Pi | Same Linux path, preferring supported native HDMI CEC | Browser receiver | First hardware acceptance target |
| Mac | Cinema Theater browser plus companion service and USB-CEC adapter | Browser receiver | Alongside Linux |
| Windows PC | Same browser receiver and companion protocol, packaged for Windows with USB-CEC | Browser receiver | Same design; package after Linux/Mac |

Android's direct HDMI control service is restricted to system/privileged
apps. Cinema should consume normal input events, not ask users for root or
try to claim that service. Apple documents TV-remote control through CEC;
Cinema still needs correct focus and input handling. These are platform
capabilities, not evidence that every television forwards every button.
[Android CEC service](https://source.android.com/docs/devices/tv/hdmi-cec),
[Apple TV remote support](https://support.apple.com/en-ca/guide/tv/atvb0410f604/27/tvos/27).

HDMI video output alone does not establish a usable CEC interface. Detect
the interface and show its status. libCEC supports Linux, macOS, Windows,
USB adapters, and supported Raspberry Pi hardware. Validate the actual Pi,
kernel, adapter, TV, and AVR chain before calling a combination supported.
Mac's built-in HDMI or a USB-C dock is not the assumed CEC path.
[Pulse-Eight hardware and OS guide](https://support.pulse-eight.com/support/solutions/articles/30000053002-what-is-the-pulse-eight-cec-adapter-and-libcec-).

**Power and volume:** let the TV/AVR retain its ordinary volume keys. Expose
phone volume only when the target advertises an actual working control,
labelled “TV volume” or “Player volume.” Power, input switching, and wake
are separate capabilities and off by default. Do not promise that the phone
can launch a suspended tvOS app, drive the OS home screen, or wake every PC.
Remote Home means Cinema Home; Back follows Cinema's navigation stack.

**Automatic appearance:** include both the foreground card and opt-in
background invitations. Paul explicitly requested investigation of a small
background service on 2026-10-07; background discovery is part of this plan,
not deferred out of scope. The supported mechanism differs by phone OS, as
§2.1 specifies. Do not promise the Apple system remote popup or an app
opening itself over another app. Suspended TV receivers disappear from the
available list when their lease expires; background phone invitation delivery
does not make a suspended TV receiver controllable.
[Apple background execution](https://developer.apple.com/documentation/xcode/configuring-background-execution-modes),
[Android background activity restrictions](https://developer.android.com/guide/components/activities/background-starts).

### 2.1 Background invitations — listen for a TV becoming available

The TV already reports availability to Cinema's server. Subscribe to that
event rather than waking every phone repeatedly to scan for TVs. After the
user enables “Offer remote when this TV opens Cinema,” show an ordinary
notification: “Living Room TV is ready · Open remote.” Tapping opens the
paired target and revalidates its current presence and grant. Discovery alone
never brings an app to the foreground or grants control.

| Phone platform | Proposed background mechanism | Constraint to prove |
|---|---|---|
| Android, local-only option | User-started foreground service subscribed to server presence, with an ongoing service notification and Stop action | Validate the `connectedDevice` service type/prerequisites for actual device interaction, background-start rules, Doze behavior, OEM battery policies and Play distribution requirements |
| Android, push option | FCM invitation from a server notification broker | Requires Internet delivery and notification permission; offers a lower-maintenance alternative to a resident service |
| iPhone/iPad, general distribution | Visible APNs alert when a paired TV enters Cinema | OS delivers the invitation while the app is suspended; no continuous polling or silent-push timing dependency |
| iPhone/iPad, local-only investigation | Local Push Connectivity extension using `NEAppPushProvider` on configured Wi-Fi networks | Requires Apple's approved `app-push-provider` entitlement; suitability and approval for this product are unproven |

Android explicitly supports foreground services for interaction with external
devices over a network. That makes a small resident service a credible path,
but not an invisible, unkillable daemon. Start it through an explicit user
action, surface its status, and recover on next app open if the OS/user stops
it. Do not claim arbitrary server polling qualifies without the M0 platform
and distribution check.
[Android foreground service types](https://developer.android.com/develop/background-work/services/fgs/service-types).

iOS background refresh is scheduled by the OS, so a periodic timer cannot
promise a prompt when the TV starts. Visible push is the normal solution;
silent pushes are opportunistic and must not underpin timely invitations.
Apple also provides a genuine persistent local-network extension, intended
for restricted networks without APNs access. Investigate it because it
resembles the requested background service, but do not assume entitlement
approval or misuse audio, location, VPN, or VoIP modes to stay alive.
[Apple background strategies](https://developer.apple.com/documentation/backgroundtasks/choosing-background-strategies-for-your-app),
[Apple Local Push Connectivity](https://developer.apple.com/documentation/networkextension/local-push-connectivity).

**Notification broker:** proposed optional Cinema-operated service that owns
the shipping app's APNs/FCM credentials. A self-hosted server enrolls only
devices that explicitly opt in, with scoped, revocable delivery grants.
Never distribute the app-wide push signing key to home servers. Send an
opaque invitation identifier and minimal display text; no media titles,
playback capabilities, pairing secrets, or login credentials. Control remains
between the phone, its Cinema server, and the TV; the broker only delivers
invitations. A custom-build self-hosted provider is a later packaging option.
No broker is needed for the Android local service or foreground discovery.

**Prompt policy:** notify only previously paired, opted-in phones for an
explicitly selected TV, once per TV foreground session; suppress reconnect
flaps and impose a proposed 30-minute per-phone/TV cooldown. User-requested
“Invite my phone” is a separate bounded action. Default to quiet delivery,
respect notification permissions/Focus, and provide a per-TV mute. Server
membership is not proof the phone is home: settings must explain that APNs
invitations can arrive away from home. Any future “only when home” mode needs
a supported presence signal; do not infer it from public IP equality.

Use a 60-second push expiry and a per-target collapse key to reduce stale
notifications. Tapping an expired/unavailable invitation shows the device
picker with the reason, never silently controls another TV. Validate locked,
suspended, offline, permission-denied, user-stopped/force-quit, battery-saver,
and notification-disabled cases; notification delivery is best effort.

## 3. Reuse the existing code, fill the actual gaps

| Existing source | What was observed | Work to add |
|---|---|---|
| [PlayerRemoteAdapter.swift](../../clients/apple/Sources/PlayerRemoteAdapter.swift) | tvOS adapters route playback and Live TV events through the shared input rules; transport also uses SwiftUI focus | A receiver action entry point and app-owned navigation coordinator |
| [RemoteCommandOwner.swift](../../clients/apple/Sources/RemoteCommandOwner.swift) | Ownership of Apple's system media commands | Preserve one active playback owner; this is not a phone-to-TV transport |
| [PlayerKeyAdapter.kt](../../clients/android/app/src/main/java/tv/plurx/app/player/PlayerKeyAdapter.kt) | Android keys map to player contract inputs | Semantic network actions and full-app focus reachability |
| [ServerDiscovery.swift](../../clients/apple/Sources/ServerDiscovery.swift), [ServerDiscovery.kt](../../clients/android/app/src/main/java/tv/plurx/app/data/ServerDiscovery.kt) | `_plurx._tcp` discovers servers | List screen receivers after server authentication; do not confuse server discovery with screen discovery |
| [keyboard-reach.js](../../crates/plurxd/src/web/core/keyboard-reach.js) | Posters/episodes gain tab stops and Enter/Space activation | Spatial direction navigation, modal focus, Back, and focus restoration |
| [theater.js](../../crates/plurxd/src/web/layouts/theater.js) | Theater uses that shared keyboard wiring | A deliberate “Use this screen as a TV” mode, receiver lifetime and identity |
| [project.yml](../../clients/apple/project.yml) | iOS and tvOS app targets | Mac initially uses the existing browser UI, avoiding a new native playback stack |

No companion-screen directory or HDMI-CEC bridge was found in the inspected
client and HTTP routing code. The existing player input contract is a useful
foundation, but does not prove whole-app navigation or network remote control.

## 4. One receiver per screen, one route for remote actions

```text
TV remote → TV/OS → native input adapter ─────────────┐
TV remote → desktop CEC helper → server relay ───────┤
Phone/tablet → authenticated server relay ───────────┤
                                                    ▼
                                      Cinema receiver action router
                                      ├─ navigation / focus / text
                                      └─ existing player / Live TV actions
                                                    │
                         receiver state and results ─┘ → phone
```

Use the configured Cinema server as the discovery and control rendezvous.
Both sides make outbound connections; a TV on Ethernet and phone on Wi-Fi
work if they can reach that server. Do not require Wi-Fi multicast between
clients or run a new HTTP listener on every TV. Retain existing Bonjour for
finding the server and existing manual/QR server setup for blocked multicast.
Apple's local-network permission still applies to LAN connections.
[Apple local-network privacy](https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy).

**Receiver identity:** a persisted installation identifier plus a per-window
screen identifier, bound to the authenticated server identity and current
user/profile. A fresh unpredictable `receiver_epoch` identifies each active
connection. Name screens explicitly (“Living Room Mac”). Browser tabs may
not share an active receiver identity; duplicate-tab handling is an M0 spike.
Changing profile ends the old epoch and rechecks grants.

**Presence:** receivers register when Cinema is active on a TV, or when the
user enables TV mode on a desktop. Proposed heartbeat: 5 seconds, presence
expiry: 15 seconds. Explicit logout/close removes presence immediately when
possible. Background/suspension updates capabilities or withdraws presence;
a timer is the fallback for abrupt loss. Availability never depends on a
movie being active. Phone suggestions update within 2 seconds of a presence
event under the local acceptance conditions.

**Transport:** proposed authenticated WebSocket control channel, separate
from the playback-control protocol. Native clients use existing bearer
authentication; browser connections use an authenticated short-lived,
single-use upgrade ticket, delivered in the first bounded auth frame.
Validate browser Origin and allow no subscription or control before auth.
Keep long-lived credentials out of URLs and QR codes. Use the configured
server transport; plaintext deployments retain their existing LAN exposure
and must not be described as encrypted. Public remote access uses TLS.

**Cluster ownership:** durable pairing grants live in replicated storage.
For the initial implementation, the current cluster leader owns the remote
directory and relay actor; standalone uses its local actor. Followers proxy
through the existing authenticated cluster transport so clients keep their
configured origin. Leadership changes disconnect/re-register receivers with
new epochs, discard pending commands, and refresh controller state. Fence
the actor with current leadership authority and stop dispatch when it is
lost. Do not add distributed per-keypress database writes. M0 must prove
stream proxying and leadership fencing before M1 chooses this wire path.

## 5. Pairing, messages, and command ownership

Same server or same LAN is discovery context, never control authorization.
Require signed-in controller and receiver, a target-bound pairing grant, and
the current profile's permission to control the screen. First pairing uses
TV approval or a code deliberately displayed by opening “Connect a remote.”
Different user accounts additionally require explicit receiver approval.
Remember the grant per device, user, server, and target. Either side can
revoke it. Logout, user removal, or profile change invalidates current
control until authorization is checked again. Child/library restrictions
must hold on both sides of “Play on TV.” Pairing confers no server-admin
access, general remote desktop access, or hidden library access.

Proposed pairing bounds: a single-use 128-bit QR token, a six-digit manual
code, both expiring after 2 minutes; at most five failed code attempts per
challenge, plus controller/account/address rate limits. Code consumption
must be atomic across cluster nodes. QR carries server identity and the
pairing challenge, never a login token. Validate any scanned origin before
sending credentials; use the existing server-identity/authentication flow.

Proposed endpoints under `/api/v1/remote`:

| Endpoint | Contract |
|---|---|
| `GET /receivers` | Only discoverable authorized screen summaries; no private viewing details before pairing |
| `POST /pairings` | Receiver creates an expiring challenge |
| `POST /pairings/{id}/claim` | Controller supplies token/code; returns grant or pending TV approval |
| `POST /pairings/{id}/approve` | Target receiver approves the named controller |
| `DELETE /grants/{id}` | Either authorized party revokes a device grant |
| `POST /tickets` | Existing authentication mints a single-use channel ticket |
| `GET /channel` | WebSocket; role-specific register, presence, command, result and state messages |

Versioned command envelope, with illustrative identifiers:

```json
{
  "v": 1,
  "type": "command",
  "command_id": "random-uuid",
  "receiver_id": "living-room-screen-id",
  "receiver_epoch": "current-connection-nonce",
  "control_epoch": "current-controller-lease",
  "context_revision": 42,
  "action": "navigate",
  "args": { "direction": "left" }
}
```

The server stamps controller identity and a monotonic receipt deadline;
client timestamps are not authority. Receiver actions are typed: `navigate`,
`select`, `back`, `home`, `play`, `pause`, `toggle_play`, `seek_to`,
`skip_by`, `select_audio`, `select_subtitle`, `set_text`, and `play_item`.
`play` and `pause` are idempotent desired states; never translate both into
a toggle. UI navigation shares the existing input contract. Explicit
transport actions call the current player owner, not synthetic key events.

State reports include supported actions, navigation context/revision,
playback identity, play state, position/duration, track choices, and text
entry context. Seek/track commands carry playback identity; text carries a
field-context nonce and is limited to eligible search fields, never passwords.
Reject stale contexts rather than typing into or seeking a different screen.
Do not expose arbitrary DOM selectors, JavaScript, shell commands, or keycodes.

One phone holds a renewable 15-second control lease; renew every 5 seconds
while its remote screen is active. Another paired phone can explicitly take
over, advancing `control_epoch`; the old phone becomes read-only. Physical
input always works and cancels a pending network hold/scrub. The receiver's
single action queue determines order; new context revisions invalidate old
context-sensitive commands. Disconnecting a phone does not pause playback.

Proposed bounds: 16 KiB message maximum, 20 commands/second/controller with
a burst of 40, and 32 pending commands/receiver. Navigation expires after
1 second, other interactive commands after 3 seconds. Direction holds use
explicit start/renew/end with a 500 ms dead-man timeout; never repeat Select
or toggle. Deduplicate command IDs for the connection epoch, retain up to
2,048 outcomes for 2 minutes, and reject overload instead of evicting live
deduplication entries. Acknowledge `applied`, `rejected`, or `expired` only
from the receiver. A transport timeout means outcome unknown, not success;
refresh state and never replay a toggle, seek, or navigation after reconnect.

## 6. Desktop CEC helper and navigation adapters

Build a small separately installed companion, proposed name
`cinema-remote`, running as the logged-in user on Linux, Mac, and Windows.
Its first responsibility is adapter discovery and normalized CEC input.
It pairs to one desktop browser receiver using a scoped input-source grant
and sends actions through the same outbound relay. This avoids a browser
trying to open CEC devices or an HTTPS page connecting to an insecure local
WebSocket. The extra server hop is an accepted first-release trade-off;
measure latency before considering a local authenticated IPC transport.

Pin the supported libCEC release/API after M0. Prefer its structured API
over parsing `cec-client` diagnostic text. Keep it outside the server's
required dependencies; a headless media server may be in another room.
The workspace declares Apache-2.0 and libCEC documents GPL/commercial
licensing: record the chosen distribution/licensing arrangement before
bundling or linking it. Process separation alone is not a licensing decision.
[libCEC upstream](https://github.com/Pulse-Eight/libcec),
[libCEC API and licensing statement](https://pulse-eight.github.io/libcec/).

Expose “adapter detected,” “waiting for TV,” “receiving buttons,” and the
last decoded key in setup diagnostics. Missing hardware leaves the saved
choice enabled and explains what is missing. Handle hot-unplug, sleep/wake,
reconnect, duplicate presses, stuck holds, multiple HDMI ports, and a second
CEC application holding the adapter. Only forward to the selected active
Cinema receiver. Never inject desktop-wide keys. Provide opt-in launch at
login, full-screen startup, a room name, and idle screen behavior as desktop
conveniences; do not force an OS-wide kiosk or suppress sleep permanently.

On web, build spatial focus inside the active route/modal, with stable item
keys, scroll-into-view, focus restoration, and text-input ownership. On tvOS,
phone arrows must drive app-owned focus state through supported APIs;
SwiftUI's physical-remote focus engine is not an injectable network API.
Android likewise exposes semantic navigation through the app's coordinator.
M0 proves these paths on real screens before protocol work assumes they exist.
Preserve native accessibility, keyboard Tab order, and physical remote input.

Unfinished receiver, companion, and CEC controls belong in Settings →
Developer with an explicit switch and advisory `devGraduation` text. Saving
must work without hardware or successful readiness checks. On completion,
graduate permanent controls to a “Remotes & devices” settings section and
remove temporary implementation switches, following the
[Developer lifecycle](../features/SETTINGS-NAVIGATION-AND-DEVELOPER-STATUS.md#developer-lifecycle--every-card-graduates).

## 7. Implementation sequence and evidence

Use one `effort/cinema-remotes` branch; tasks target its current head because
they share auth, settings, navigation, and wire contracts. This is not the
disjoint-file exception. Establish the pinned Rust 1.97.1 compiler loop
before any Rust edits, following the
[development pipeline](../DEVELOPMENT_PIPELINE.md) and
[agent compile loop](../ci/AGENT-COMPILE-LOOP.md).

| Milestone | Ownership / concrete work | Acceptance |
|---|---|---|
| M0 — prove constraints | Client focus prototypes, cluster channel spike, desktop adapter probes, background invitation/service probes, distribution decision; record exact devices and dependencies | Phone-originated arrows/select/back work on one tvOS and Android TV screen; browser duplicate-tab IDs stay distinct; leader loss stops dispatch; Pi and Mac receive real CEC presses; Windows build/install probe recorded; Android service and visible iOS push demonstrated, local-push entitlement feasibility recorded |
| M1 — server contract | Remote HTTP module, relay actor, replicated grants, settings and native contract fixtures | Unauthorized enumeration/control, replay, wrong epochs, code brute force, revocation, takeover, backpressure and leadership-change tests pass; no stale command replays |
| M2 — browser receiver | Web navigation, Theater TV mode, action router, receiver settings | Complete Home → library → detail → playback → tracks → Back using six navigation buttons; modal/search/focus restoration tests; phone commands do not create a second player |
| M3 — native receivers | Apple app model/focus/player adapters; Android navigation/player adapters | Same route walk on physical Apple TV and Android TV; network text search; VOD and Live TV behavior matches the shared input contracts; suspend/resume refreshes epochs |
| M4 — companion UI | iPhone/iPad, Android phone/tablet, mobile web; pairing, prompt, remote, target picker, text/track controls | iOS → Android TV and Android → Apple TV both work; Ethernet TV discovered; dismissal, multiple TVs, logout, profile restrictions and tablet layouts pass |
| M4b — background invitations | Android opt-in local service, APNs/FCM broker and device enrollment, notification deep links, per-TV preferences | TV entering Cinema produces an invitation on locked/suspended test phones under supported conditions; tapping reaches the correct receiver; stale/revoked invitations cannot control it; offline/Doze/Focus and force-quit behavior documented; no reconnect notification storms |
| M5 — desktop CEC | Helper packages, Linux/Pi then Mac then Windows; scoped source pairing and diagnostics | Real TV remote completes M2's route walk on each advertised OS/hardware combination; unplug, stuck hold, reconnect, sleep/wake and adapter contention recover visibly |
| M6 — convenience and promotion | Play on TV, opt-in desktop startup/fullscreen, final settings, operator docs and hardware support table | Phone can select a film and disconnect while TV continues; measured latency and full acceptance matrix recorded; settings graduate only with their evidence |

Each milestone should be split into reviewable task PRs when necessary; do
not land a large cross-platform change under one unverified claim. Existing
source anchors in §3 define integration seams, not exclusive file ownership.
New web files require their asset row, shell tag, and layout-document row in
the same commit. Extend the existing input fixtures/fences rather than
creating contradictory button semantics.

Run focused behavior tests locally, affected Rust/Apple/Android compilation,
formatting, and Clippy before pushing. Name each actual regression as
`Regression-Test: <path>::<test name>` in the PR and landing commit; proposed
tests in this plan are not evidence. Behavioral corrections use `fix(` or
`perf(` subjects. Replicated-storage coverage requires `make unit-core` or
`--features hiqlite-store`. Existing checks to retain include:

```bash
python3 -m unittest discover -s tests/operations -p 'test_docs_index.py'
node tests/web/asset-order.test.js
node tests/web/asset-load.test.js
node tests/web/asset-layout.test.js
make apple-build
make android-test
```

Add focused receiver/relay/UI tests as those modules exist. Record physical
acceptance separately: automated focus fixtures cannot establish CEC support.
Proposed latency target on a healthy LAN: p95 below 150 ms from controller
send to receiver application over 100 button actions, plus separately measured
visible response; do not claim photon-to-photon latency from an ACK alone.
Record TV/AVR model, adapter, OS, route, sample count, and disconnect behavior.

At completion freeze task merges, integrate current main, and qualify the
exact promotion candidate under the repository's current effort/main gates
and qualification receipt rules. A changed base needs fresh evidence. This
plan does not authorize bypassing those gates or deploying incomplete work.

## 8. Scope limits and decisions to revisit

First release controls Cinema on active receivers, including browsing and
playback. It does not remotely administer the computer, control unrelated
TV apps, provide synchronized multi-room playback, or guarantee wake/power
control. Native TV CEC remains the operating system's responsibility.

Foreground discovery and background invitations are both planned. The
iOS local-only service remains conditional on entitlement approval; visible
APNs invitations are the general-distribution route. Revisit local desktop
IPC if measured server relay latency misses the target;
revisit sharding the relay only if household-scale traffic warrants it.
Finalize libCEC packaging, TV focus APIs, browser fullscreen/autoplay limits,
and cluster streaming behavior in M0. Browsers may require one local user
gesture before remote-initiated playback/fullscreen works: setup must make
that clear rather than promising unattended launch on every browser.

The Linux/Pi and Mac paths have priority. Windows shares the design and
ships when its package and real adapter acceptance are complete. A passing
cross-compile alone never earns a supported-hardware claim.
