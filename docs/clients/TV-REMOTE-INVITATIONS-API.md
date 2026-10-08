# Cinema background invitation API

This document answers how an explicitly paired phone opts into a generic
screen-ready invitation, and what it must revalidate when the user taps it.
The wire contract is fixed for B09 adapter work. The first implementation slice
provides the durable schema, no-touch login authority and atomic admission;
HTTP delivery and provider qualification are still being built. This is not
proof of background execution or physical provider delivery.

Invitations never acquire control or start playback. Ordinary foreground
pairing/control remains the [remote API](TV-REMOTE-API.md).

## Envelope, proofs and generations

The home routes live under `/api/remote/v1/invitations` and
`/api/remote/v1/phones`. Every top-level request/response envelope has
`version:"cinema.invitation.v1"`. Unknown fields/versions are rejected. UUIDs
are canonical lowercase strings; counters are safe JSON integers. HTTP JSON
requests/replies have the same 64 KiB ceiling as the remote API.

Every home request has one exact `Authorization: Bearer <Native login>` header.
No query authentication, duplicate authorization, X-Api-Key, machine token or
compatibility login is accepted. Phone operations also use
`X-Cinema-Phone-Secret` with a canonical base64url 32-byte proof. Consent ON and
provider ticket operations additionally use `X-Cinema-Grant-Secret`. Proofs
are headers; only newly minted phone/ticket proofs are returned once in JSON.
Errors are flat `{version,code,message}`.

`phone_generation` changes on phone availability, login rebind and installation
lifecycle changes. `consent_generation` belongs to one stable phone/TV consent
row; its expected value is 0 only when creating a new consent. Transport start
also advances `transport_generation`. Clients retain the returned values and
never silently overwrite a newer generation. Stale writes return 409
`stale_generation`.

Native clients create a phone installation UUID per saved server instance and
account, and persist its proof in platform secure storage. A notification
contains only fixed generic wording/category and opaque `invitation_id`:
canonical base64url of the 16 installation UUID bytes followed by 16 random
invitation UUID bytes (43 ASCII characters). Its installation prefix selects
the local saved authenticated server. It is not a login, grant, origin or control
capability; the server still verifies the complete identity at lookup.

## Phone routes

| Method and path | Request fields besides version | Response fields besides version |
| --- | --- | --- |
| POST `/phones` | `installation_id`, `platform:"apple"|"android"`, `name` | `phone`, `phone_secret:string|null` |
| POST `/phones/list` | `after_id:UUID|null`, `limit:1..20` | `phones`, `next_cursor:UUID|null` |
| POST `/phones/{id}/availability` | `expected_phone_generation`, `permission_granted:bool`, `resident_active:bool` | `phone` |
| POST `/phones/{id}/rebind` | `expected_phone_generation` | `phone` |
| DELETE `/phones/{id}` | empty body | version only |

`phone` is `{installation_id,name,platform,phone_generation,created_at,
permission_granted,resident_active}`. Labels are nonempty, at most 80 UTF-8
bytes and have no Unicode control characters. Phones/list requires the human
Bearer only, and never returns a phone proof, OS token or broker capability.
The sorted page is a bounded prefix, not an unbounded installation inventory.

New POST `/phones` returns generation 1, readiness flags false and the proof
once. Retrying an existing UUID requires its existing phone proof and returns
metadata with `phone_secret:null`; plaintext cannot be recovered from its hash.
Losing the initial response requires explicit same-user DELETE followed by a
new UUID registration. There is no unproved retry rotation.

Availability flags are reported native OS permission/service readiness, not
proof that a service remains continuously running;
`resident_active:true` requires Android and `permission_granted:true`. It
advances phone generation without changing enabled consent or removing event
history. Native must persist stopped state locally on Stop/offline, reconcile
false on the next authenticated connection without restarting, and stop its service
on logout/server/account change. Saved consent remains on if the OS denies
notifications, but dispatch is ineligible.

Rebind explicitly associates an installation with the current human login,
advances its generation, clears permission/resident readiness and disables
consents until the user opts in again. It preserves live-scope dedupe/cooldown
history. It is required when the original bound login was revoked; this is not
automatic login substitution by the background worker.

DELETE uses same-user human Bearer only, so a lost proof cannot trap the
20-phone cap. It invalidates the phone and all consent/delivery capabilities,
then cleans up the revoked scope's history. No live grant, provider or feature
switch is required to delete an installation.

## Consent and transport

| Method and path | Request fields besides version | Response fields besides version |
| --- | --- | --- |
| POST `/invitations/consent` | `installation_id`, `receiver_id`, `expected_phone_generation`, `expected_consent_generation`, `enabled`, `grant_id:UUID|null`, `transport:null|"apns"|"fcm"|"android_resident"` | `consent` |
| POST `/invitations/consents/list` | `installation_id`, `after_receiver_id:UUID|null`, `limit:1..20` | `consents`, `next_cursor:UUID|null` |
| POST `/invitations/transport/start` | `installation_id`, `receiver_id`, `grant_id`, `expected_phone_generation`, `expected_consent_generation` | `consent`, `ticket:object|null`, `broker_origin:string|null` |
| POST `/invitations/transport/confirm` | `installation_id`, `receiver_id`, `ticket_id`, `expected_phone_generation`, `expected_consent_generation`, `expected_transport_generation` | `consent` |

Nullable fields may be omitted where the request does not need them (OFF).
ON requires nonnull grant_id/transport and the current grant proof, the current
bound phone login, and same-user live receiver/grant. APNs is Apple only; FCM
and resident are Android only. OFF needs current same-user Native Bearer and
phone proof, but no still-live grant, provider, receiver or global feature.
OFF for an absent consent with expected generation 0 returns disabled metadata;
it does not create a phantom grant-bound row.

`consent` is `{receiver_id,grant_id,enabled,transport,consent_generation,
transport_generation,readiness}`. Readiness is
`{eligible:bool,status,provider_delivery_verified:false}`. Status is one of
`disabled`, `ready`, `global_disabled`, `login_changed`, `grant_revoked`,
`permission_unavailable`, `provider_unconfigured`, `transport_pending`,
`transport_unavailable`, `retention_limit`. Provider delivery remains unverified
until separate physical qualification; configured transport is not a delivery
receipt. Consent ON always preserves the user's choice when permission,
provider or global dispatch switches are unavailable.

A transport ticket is `{ticket_id,ticket_secret,expires_at}` with a 120 s
lifetime. Start first advances consent/transport generation and suspends old
dispatch, then requests a scoped broker ticket. Missing provider configuration
returns `ticket:null` plus advisory readiness, without undoing enabled consent.
OS device-token rotation uses the same start/claim/confirm flow. The home server
never receives the OS token.

Phone claims the ticket directly at the configured verified HTTPS broker:
POST `/broker/v1/tickets/claim`,
`Authorization: Bearer <ticket_secret>`, JSON
`{version,ticket_id,platform,device_token}`. It sends no Cinema login, phone or
grant proof to the broker. No redirect or phone-selected origin is accepted.
The topic/project, server, phone and transport generation are fixed ticket
metadata. Confirm asks the home server to query that recorded ticket through
its publisher authority; a phone cannot swap in an arbitrary enrollment.

## Resident poll and notification tap

POST `/invitations/poll` takes
`{version,installation_id,after_revision,wait_ms}` with wait 0–20000 ms and
returns `{version,revision,invitations:[{invitation_id,expires_at}]}`. Pages are
ordered prefixes with at most 16 invitations and the 64 KiB byte limit. One
RAII poll permit per phone prevents unbounded authenticated long polls.
Repeated delivery uses the same identity; native deduplicates its local visible
notification. This endpoint is for an explicitly started Android resident
service, not an iOS background loop.

Worker traffic and resident polling use a consistent no-touch Native verdict
at initial admission and each one-second poll refresh. They apply existing
`TokenIdlePolicy` and audience rules without updating `last_seen_at`; background
work cannot keep a dormant phone signed in. User-initiated consent and actual
tap lookup retain ordinary human authentication activity.

POST `/invitations/lookup` takes
`{version,installation_id,invitation_id}` and returns
`{version,receiver_id,foreground_id,target,expires_at}`. It requires current
human Bearer, phone proof, current bound login, enabled consent, valid stored
grant/receiver and one current live target. Expiry, a new foreground lifetime,
revocation or ambiguous owners refuses the tap. A reconnect in the same
foreground lifetime can resolve to its new unique target. This is a discovery
result, never command retargeting. Native opens the remote and then uses the
normal explicit acquire/takeover controls.

The visible provider/local notification uses category
`CINEMA_REMOTE_INVITATION`, wording “A paired screen is ready”, and opaque
invitation_id only. It carries no title, origin, bearer, grant, media URL or OS
power action. Apple must route through its existing notification delegate;
Android must preserve its download/media owners and user Stop behavior.

## Durable limits, restore and delivery truth

SQLite migration 107 and the admitted Hiqlite Invitation adjunct install exact
STRICT tables/indexes plus independent marker version 1 in one transaction.
Source schema 81/82 and Remote schema 1 are unchanged. Missing/partial/incorrect
markers are refused; serving and maintenance open do not repair schema.

Admission is unique `(receiver_id,foreground_id,enrollment_id)`, with stable
consent identity across live mutations. A separate receiver/phone cooldown is
1800 s and survives grant/token/provider rotation. Event and cooldown commit
atomically; every transaction result is checked. Bound 20 phones and 160
consents/account, and 100000 event tombstones/account. At the event limit no
new invitation is admitted; retention_limit readiness preserves enabled choice.
Live-authority history is not pruned merely by age. Cleanup belongs only to
revoked scopes. Portable restore explicitly clears children and broker
capabilities in dependency order even with foreign_keys OFF; old imports omit
Invitation tables and refuse a nonempty target.

The worker rechecks phone login without touching activity, consent generations,
receiver/grant authority, permission, feature state and owner snapshot before
persisting attempted, then making at most one broker send. Unknown results
consume that attempt. The broker separately persists identity before one
provider attempt. Neither layer promises exactly-once OS delivery. Calls have
bounded concurrency, queues, payloads, reply reads and deadlines.

Broker storage also has bounded tickets, revocation work and retained delivery
identities. Expired/unknown ticket or enrollment IDs are rejected; deleting
expired records never turns them into new capabilities. Delivery identity
history is retained for the publisher generation, with a hard quota; it is not
silently expired into repeat eligibility. Restoring the broker requires an
operator-maintained external generation/key manifest. Missing or mismatched
manifest refuses startup; copying an old database without advancing that
external generation cannot be detected automatically and is not a safe restore
procedure.

## Settings and ownership

`cinema_remote_invitations` defaults false and maps to
`cinema.remote_invitations`. GET/PUT `/api/v1/settings` exposes/saves this boolean
regardless of readiness. Dispatch also requires existing cinema_remote_control.
Developer readiness item/setting is cinema_remote_invitations with requirement
IDs broker, consent and delivery. Web card ownership is B05's builder; server
mapping/worker/store/broker belong to B09. Shared protocol/status/build documents
and native adapters remain their assigned owners.

The home stores no APNs/FCM provider key. The independent broker uses scoped
publisher proof, verified HTTPS, fixed provider hosts/topic allowlists and
encrypted OS tokens. [APNs requires HTTP/2 and TLS](https://developer.apple.com/documentation/usernotifications/establishing-a-connection-to-apns),
[ES256 provider authentication](https://developer.apple.com/documentation/usernotifications/establishing-a-token-based-connection-to-apns),
and [FCM HTTP v1 uses scoped OAuth credentials](https://firebase.google.com/docs/cloud-messaging/send/v1-api).
Android resident eligibility requires actual qualifying work under the
[connected-device foreground-service rules](https://developer.android.com/develop/background-work/services/fgs/service-types);
merely declaring a permission is not evidence. Existing mDNS discovery never
authorizes Cinema credentials at an arbitrary advertised origin.
