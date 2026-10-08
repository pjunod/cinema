# Native invitation adapters: build and review contract

This document answers how the native B09 adapters enroll notifications, handle a
cold-launch tap and stop resident work without taking over the existing media or
reminder services. It is an open build contract; native invitation delivery is not yet
qualified. Use the home B09 packet's frozen invitation API reference as the exact wire
contract. Build after the Apple/Android couch packets freeze. Native owner owns only
clients/apple and clients/android source/build/resources/tests; request any cross-owner
server/API changes before editing. Parent personally reviews and releases separate PRs
to the batching coordinator. The batching coordinator owns release counters and shared
gates. Physical device use requires authorization; no connected television installation
is included in this packet. Do not claim physical provider acceptance from software
fixtures.

## Shared enrollment and tap state

Use one invitation installation UUID per saved server-instance/account, with its
returned phone proof in existing platform secure storage. Keep invitation and remote
receiver identities distinct. The API's 43-character invitation ID is 32 decoded bytes:
phone UUID then random event UUID. Decode canonically; match the phone prefix only
against locally saved authenticated installations. It never supplies an origin, login,
grant or account switch authorization. Select the saved profile explicitly as needed,
retain the pending tap across cold launch/login, then authenticated lookup revalidates
the entire ID. Show the returned screen through ordinary companion selection. Do not
acquire/take over, Select, tune or start playback on a notification tap.

Visible per-paired-screen invitation choice defaults off and persists enabled even when
provider/permission/global dispatch is unavailable. Surface server readiness as advisory
text. OFF must not need a live grant/provider/global feature. Keep foreground
discovery/use intact. Permission requests occur only after the user asks to enable
notifications. Serialize phone generation, consent generation and transport generation
changes; snapshot profile/account/phone and generation around every await, dropping
stale completions. No recovery of a lost one-time phone proof: offer same-user
delete/new installation. Login rebind is explicit and disables consent per API; never
silently bind an old consent to a new login.

Transport rotation: explicit Start -> direct verified HTTPS broker claim (ticket Bearer
only) -> home Confirm. The authenticated home Start response supplies the ticket
`broker_origin` and `broker_generation` from trusted operator configuration. Require
an HTTPS origin without userinfo, path, query or fragment, freeze it with the ticket,
and verify the single canonical `X-Cinema-Broker-Generation` response header before
accepting the claim. A push never supplies either trust decision. No Cinema credential may go to the broker, no redirects,
arbitrary origin from a push or extra payload fields. The OS token never goes to home.
Missing/unknown issuance does not cause automatic repeated visible notification or
leaked stale ticket. Token callback can schedule consent-preserving transport renewal
when the same current consent remains enabled; reuse exact generations and stop on
identity replacement. Proof hashes, where needed, are SHA256 of canonical base64url
textual bytes, not decoded secret bytes.

## Apple

`OfflineAppDelegate` currently calls `LocalReminders.shared.begin()` at launch, and owns
background-download URLSession callbacks. `LocalReminders` is the existing
UNUserNotificationCenter delegate and registers its reminder category/actions. Extend
one shared notification routing owner; do not install a second delegate or replace
reminder categories. Preserve Watch/Record reminder semantics and download callbacks.
Add didRegisterForRemoteNotifications/didFail callbacks to the existing
UIApplicationDelegate, dispatching to the invitation model with identity/generation
fences. Register APNs only for the explicit notification opt-in flow. Configure
documented aps-environment entitlement/signing prerequisites without pretending an
unsigned/simulator build has a delivered provider token.

Recognize only category CINEMA_REMOTE_INVITATION plus a canonical opaque invitation_id.
Default tap may queue lookup; dismiss/custom unrelated actions must not open a remote.
Cold-launch delivery must queue before views/models are ready. Foreground notification
presentation should honor the same opt-in state and avoid a duplicate prompt from the
existing foreground suggestion. No silent-push loop or fake audio/location background
mode. Keep tvOS receiver compilation working; mobile phone enrollment is iOS/iPadOS UI,
not a tvOS push-service claim.

## Android

Preserve PlaybackService, PlurxDownloadService/OfflineTransferJobService,
ReminderAlarmReceiver, and their channels/identities. Add a separate invitation channel
and immutable explicit PendingIntent with only the opaque invitation ID. Exported
MainActivity treats all extras as untrusted; revalidate via authenticated lookup.
Notification Stop is local, immediate, durable and available offline. It cancels
polling/network callbacks, persists resident=false intent for next authenticated
reconciliation, stops foreground service and cannot cause a
boot/alarm/WorkManager/START_STICKY restart.

FCM is independently optional. Add real token rotation/message adapters with a
documented project/app configuration seam, no checked-in provider secrets or made-up
credentials. Builds without a configured Firebase app remain usable and show
provider-unconfigured readiness; no fake ready state. FCM uses a data-only message
with HIGH Android priority and bounded TTL, containing only the invitation ID and
CINEMA_REMOTE_INVITATION category. The native messaging adapter checks the local
installation, consent, permission and dedupe, then immediately renders a generic
“A paired screen is ready” notification. Do not add an FCM notification block that
bypasses these local checks, or await home lookup before displaying it. APNs visible
alerts already sent to the OS cannot be recalled; taps still reauthenticate.
Duplicate opaque IDs must not
create repeated visible notifications. Persist bounded dedupe/cursor per installation,
advance only through actually processed poll prefix, and fence late messages after
logout/revocation.

Resident option is user-started from visible UI, separate from the saved invitation
consent. Poll only while the real service is active and OS notification
permission/channel availability permits visible invitations. Never borrow
mediaPlayback/dataSync to claim indefinite receiver discovery. Use connectedDevice only
with genuine required networking and documented prerequisite. Existing
INTERNET/ACCESS_NETWORK_STATE/ACCESS_LOCAL_NETWORK alone do not satisfy its prerequisite
list.

A candidate worth evaluating is a scoped Wi-Fi NetworkRequest held for the actual
opted-in home receiver/poll connection: ConnectivityManager.requestNetwork requires
CHANGE_NETWORK_STATE and retains a requested network until unregisterNetworkCallback;
use its Network socket factory/DNS for this invitation client only. Do not bind the
whole process (would disrupt playback/downloads), repeatedly request networks as a
presence probe, turn Wi-Fi on/off, or follow discovered URLs with credentials. Restrict
to the user-selected saved origin and release exactly once on Stop/logout/loss/timeout.
This is a design candidate, not blanket approval of Play eligibility or proof the device
will keep it alive. If no qualifying interaction can be demonstrated, keep the choice
saved with honest unavailable readiness and retain FCM/foreground alternatives.

## Focused acceptance

Use production DTO/identity/state methods and real notification dispatch adapters with
injected synthetic OS/provider inputs. Cover cold-launch tap before model readiness;
reminder and invitation routing coexist;
dismissed/wrong-category/malformed/unknown-server/stale-generation taps do nothing;
valid tap only opens ordinary explicit remote selection; no credential sent to broker;
stale token callback cannot enroll a replacement account; missing provider/permission
preserves enabled choice; OFF with grant gone works; resident offline Stop persists and
cannot restart; late poll/network callback cannot notify after Stop; prefix cursor and
durable local dedupe survive restart. Build iOS/tvOS and Android production/test APKs
and run focused cases only. Record provider token, push delivery, Doze/OEM and physical
notification behavior as unverified until actually exercised.

Official sources checked 2026-10-08:

- https://developer.android.com/develop/background-work/services/fgs/service-types#connected-device
- https://developer.android.com/reference/android/net/ConnectivityManager#requestNetwork(android.net.NetworkRequest,%20android.net.ConnectivityManager.NetworkCallback)
- https://developer.android.com/reference/android/net/Network#getSocketFactory()
- https://developer.apple.com/documentation/usernotifications/registering-your-app-with-apns
- https://firebase.google.com/docs/cloud-messaging/android/receive-messages
- https://firebase.google.com/docs/cloud-messaging/android-message-priority
