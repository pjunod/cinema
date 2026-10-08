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
| POST `/invitations/transport/start` | `installation_id`, `receiver_id`, `grant_id`, `expected_phone_generation`, `expected_consent_generation` | `consent`, `ticket:object|null` |
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
`transport_unavailable`, `retention_limit`, `migration_remediation`. Provider delivery remains unverified
until separate physical qualification; configured transport is not a delivery
receipt. Consent ON always preserves the user's choice when permission,
provider or global dispatch switches are unavailable.

A transport ticket is `{ticket_id,ticket_secret,expires_at,broker_origin,broker_generation}` with a 120 s
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

SQLite migrations 107/108/109/110 and the admitted Hiqlite Invitation adjunct install
exact STRICT tables/indexes plus independent marker version 4. Fresh replicated
installation is transactional; an exact version-1 image upgrades in one
transaction. Durable phone high-water survives receiver/event cleanup and
advances only with successful admission. Transport readiness binds the current
phone generation.
Source schema 81/82 and Remote schema 1 are unchanged. Missing/partial/incorrect
markers are refused; serving and maintenance open do not repair schema.

Admission is unique `(receiver_id,foreground_id,enrollment_id)`, with stable
consent identity across live mutations. A separate receiver/phone cooldown is
1800 s and survives grant/token/provider rotation. Event and cooldown commit
atomically; every transaction result is checked. Bound 20 phones and 160
consents/account, and 100000 event tombstones/account. At the event limit no
new invitation is admitted; retention_limit readiness preserves enabled choice.
Live-authority history is not pruned merely by age. Cleanup belongs only to
revoked scopes. Portable restore exact-upgrades old shapes, clears live children and phone
authority in dependency order even with foreign_keys OFF, and preserves
globally owned cleanup-only obligations. Over-capacity images refuse restore
with migration remediation rather than prune work; old imports omit
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

## Standalone broker contract

Every top-level envelope has version:"cinema.invitation.v1". All objects reject unknown fields; UUIDs are canonical lowercase; positive generation counters <=9007199254740991. JSON request/reply ceiling 64 KiB; nested strings/arrays have explicit tighter bounds. Flat errors {version,code,message}. Credentials are headers only; no query, X-Api-Key, duplicate Authorization or lexical whitespace alternatives. Broker HTTP listener loopback by default, served through operator verified HTTPS; home/phone clients refuse redirects and arbitrary origin changes.

Publisher authority: exactly one X-Cinema-Publisher-Id canonical UUID header plus Authorization: Bearer <canonical base64url 32-byte publisher proof>. External operator config maps hashed proof to publisher ID, exact server_instance_id, current startup generation and fixed platform/topic/project allowlist. The server instance is bounded 128 UTF-8 bytes/no controls. Publisher cannot enumerate another scope. Ticket claims use only Authorization Bearer ticket proof and no publisher/Cinema identity headers.

POST /broker/v1/tickets request fields:
{version,ticket_id,ticket_secret_hash,server_instance_id,installation_id,receiver_id,consent_id,phone_generation,consent_generation,transport_generation,platform}
Home generates fresh UUID ticket_id and random 32-byte ticket_secret; sends only its lowercase 64-hex digest. Platformapple maps to fixed configured APNs topic; android maps to fixed configured FCM project. Response {version,ticket_id,expires_at,status:"pending"}. TTL 120 s from first issuance; retry only exact same binding+hash returns metadata without extending expiry. Mixed binding collision rejects. Home retains secret only for the current Start response, never recoverable later. Unknown/lost Start response requires explicit fresh Start/generation; no implicit secret rotation.

POST /broker/v1/tickets/claim request {version,ticket_id,platform,device_token}, Bearer ticket_secret. Atomic single use before enrollment; expired, used, revoked, missing, wrong proof/platform IDs reject. Response {version,status:"claimed"}. Phone receives no enrollment/publisher capability. Token bound: APNs even-length ASCII hex, nonempty <=512bytes, not fixed token length; FCM nonempty <=4096bytes, no ASCII controls/whitespace. No client topic/project/URL/payload input.

POST /broker/v1/tickets/status publisher request {version,ticket_id}. Response {version,ticket_id,status:"pending"|"claimed"|"revoked"|"expired",expires_at,enrollment_id:null|UUID (equal to ticket_id when claimed),server_instance_id,installation_id,receiver_id,consent_id,phone_generation,consent_generation,transport_generation,platform}. Home verifies every immutable field against its recorded ticket and current generation before marking ready. Unknown ticket returns scoped404. Missing provider configuration refuses ticket issuance (503provider_unconfigured); no fake readiness.

DELETE /broker/v1/enrollments/{id}, publisher authority, empty body ->{version,status:"revoked"}. Idempotent scoped revoke; status and DELETE never reveal cross-publisher existence. Keep durable generation/revocation tombstones; no capability resurrects through reuse/pruning. Phone token rotation uses new Start/claim/confirm generation; old enrollment becomes ineligible immediately at home and receives bounded durable revoke work.

POST /broker/v1/deliveries publisher request {version,enrollment_id,installation_id,phone_generation,consent_generation,transport_generation,invitation_id,expires_at}. The immutable binding must match active enrollment/current publisher generation. invitation_id canonical 43-character base64url of installation UUID 16 bytes + event UUID 16 bytes, matching installation prefix. Expiry must be future and <=120 s; reject expired IDs before pruning. Response {version,status:"accepted"|"duplicate"|"denied"|"unknown"}. accepted means provider accepted its HTTP request, never physical delivery. Persist scoped dedupe and attempted state BEFORE provider I/O; duplicate never makes a second provider request. Unknown network outcome consumes attempt. Fixed generic visible body "A paired screen is ready", category CINEMA_REMOTE_INVITATION, opaque invitation_id only; no title, URL, grant, login or OS power action. Invalid provider token revokes its enrollment.

Bounds: <=64 retained publisher IDs (including inactive); <=4096 pending tickets/publisher, expired unclaimed tickets stop counting as pending but retain their ID tombstone (unknown/expired claims always deny); <=4096 active enrollments/publisher; <=100000 durable enrollment revocation identities and <=100000 delivery dedupe identities/publisher generation. No age pruning of live-generation dedupe/revocation; capacity returns 429 retention_limit. Prefix/pages for any diagnostic listing, no unbounded arrays. Global <=16 in-flight HTTP tasks/provider calls and <=4 provider calls/publisher, with immediate rejection or a bounded queue rather than unbounded semaphore waiters; one overall 5 s provider deadline includes OAuth fetch and send, provider response 64 KiB max, Google OAuth response 16 KiB max. Bounded per-publisher admission rate and per-installation 30-minute cooldown. Unknown response/restart cannot reset attempts.

At-rest device tokens encrypted ChaCha20Poly1305 with independent 32-byte broker-only master key, fresh 12-byte nonce and AAD publisher+startup generation+enrollment+platform+transport generation. Provider private keys exist only in broker-only files. Existing ring primitives sign APNs ES256 (P-256 fixed 64-byte signature), FCM RS256; fixed Apple production/sandbox HTTP2 TLS hosts/topic and fixed Google OAuth+configured project HTTPv1 hosts. No general JWT-auth parser, no client-controlled host or arbitrary notification.

Restore: external operator generation/key manifest is authoritative and separate from broker DB. Missing manifest/key or DB generation mismatch refuses serving. Raw old DB copy with the SAME external manifest cannot self-detect rollback: operator must rotate generation, master key and publisher secrets BEFORE restore/startup, then run explicit offline restore-fence command that clears/revokes all old ticket/enrollment/delivery capabilities and binds new marker. Normal startup must not silently repair mismatch or recreate keys over ciphertext. Document these real guarantees and the operator procedure, not magical rollback detection.

Focused tests: strict duplicate/lexical proof failures; publisher scope; ticket exact retry/non-extension/single use/expired+pruned deny; bound quotas; ciphertext tamper/AAD; delivery duplicate/concurrent/crash-unknown consume; revoke/generation stale; external manifest missing/mismatch and explicit restore fence; synthetic APNs/FCM exact payload/signing/fixed-host/status formation. No real provider success claim. Provider/native physical acceptance remains unverified/advisory.


Cleanup identity and capacity refinement: enrollment_id equals ticket_id, the fresh canonical UUID chosen by home. DELETE durably tombstones that publisher-scoped ID even if claim/issuance has not completed, preventing later claim or issuance resurrection. Claimed status remains resolvable while enrollment is live; ticket expiry only ends unclaimed admission. Keep all issued/expired/revoked IDs in the current external publisher generation's bounded 100000-identity budget; expiry may release pending capacity but cannot erase the identity and permit reuse. Every issuance reserves its eventual revoke slot. Reject new issuance at retention_limit, rather than make a later authorized revoke require new capacity.

Home admission likewise reserves a single 100000/account budget covering queued revoke work plus every outstanding issued/uncertain broker reference before external issuance. OFF/rebind/DELETE moves an existing reservation into durable revoke work and can complete with the broker offline. These operations never discard unqueued capability references. If imported/legacy state violates the invariant, fence and retain it with explicit migration remediation rather than silently prune authority.

Proof digests are SHA-256 of the canonical 43-character base64url proof text bytes, matching `plurx_core::auth::hash_token`, not a hash of decoded raw bytes. All expires_at values are Unix seconds and bounded safe JSON integers.


## Broker scope replacement and durable cleanup

A home broker reference binds the configured HTTPS broker origin, publisher ID
server instance and broker generation. This scope is immutable while any held ticket reference or
queued revocation remains. Rotating the publisher proof within the same scope
is allowed when the broker accepts that new proof for the existing publisher.
Replacing the origin, publisher ID, server instance or broker generation requires draining every
old reference to a durable broker tombstone acknowledgement first. A mismatch
preserves pending work, fences new issuance and reports `migration_remediation`;
it never sends an old ID under a new publisher or treats a new generation's
unknown-ID tombstone as proof that the old enrollment was revoked.

If old authority is lost, an operator must deliberately revoke the old publisher
generation at its broker and record that fence before replacing home scope.
Broker restore generation/key rotation is a separate operator procedure; a raw
old database copy with its old external manifest cannot detect its own rollback.

Invitation schema 3 makes broker cleanup globally owned: `user_id` remains
accounting metadata but does not reference a deletable user. Consent deletion
transfers held references through an exact-shape trigger, including phone,
receiver and user cascades. Migration preserves existing queued rows. The
reserved capacity covers held plus queued identities, bounded both per account
and globally at 100000. Deletion transfers existing capacity and works while
the broker is offline. Work does not expire with ticket admission and is removed
only after broker tombstone acknowledgement. Portable restore retains copied cleanup-only obligations after fencing all
invitation authority. A matching authenticated old broker scope may drain them;
scope mismatch requires explicit remediation. Broker database restore retains
its separate mandatory offline generation/key rotation procedure.


Broker generation headers: every publisher request requires exactly one
`X-Cinema-Broker-Generation` containing a canonical UUID equal to the broker's
verified external manifest generation. Missing, duplicate or noncanonical values
fail before any effect; mismatch returns versioned 409 `stale_broker_generation`.
Every broker JSON response, including errors and native claim responses, carries
exactly one header with the current verified generation. Native claim requires
no request generation header. Home requires the response generation to equal
its requested trusted configured `broker_generation` before parsing capability
results or acknowledging cleanup. Missing, duplicate, malformed or mismatched
response generation retains pending work as unknown/remediation. A new publisher
proof under a new broker generation cannot acknowledge old-generation cleanup.
Home includes `broker_generation` in its scope hash. Ordinary publisher-proof
or provider-signing-key rotation preserves generation; operator restore or an
explicit external generation fence changes it.


Schema 4 adds a destructive-cleanup guard for incompatible legacy references.
An enrollment without its ticket/scope reference, malformed reference or
mismatched known enrollment refuses restore before mutation and refuses ordinary
phone/receiver/account cascades transactionally with `migration_remediation`.
The obligation remains intact; no missing old broker authority is invented.
OFF may disable consent while retaining that reference. Valid reserved ordinary
deletions still work at capacity with the broker offline. Exact schema 3 is
preserved and upgrades through an additive migration.


A nonnull home TransportStart ticket includes `broker_origin` and
`broker_generation`. The origin comes solely from trusted validated home
operator config, is HTTPS with no userinfo/path/query/fragment, and remains
frozen to that ticket scope. The authenticated home Start response establishes
this trust seam. Native claims only that exact origin, refuses redirects and
sends only its ticket proof. It accepts a claim response only with exactly one
canonical `X-Cinema-Broker-Generation` equal to the issued ticket generation.
No request generation header is required for claim. Notification payloads never
carry broker or home origins.


Android FCM invitations use data-only `invitation_id` and the fixed category,
HIGH priority and bounded TTL. The native app checks the current local installation, account, login and
permission, at least one enabled FCM consent, and dedupe before displaying the
fixed generic local notification; it makes no home fetch before display. This
is an installation-wide gate: the two-field payload does not identify a TV or
consent generation. Home and broker fence unsent stale work, but a generic alert
already accepted by the provider may arrive after one TV consent is OFF while
another remains ON. The client must not claim exact per-TV local fencing. Notification-plus-data auto-display would
bypass those local checks and is not used. APNs remains a visible alert. Already
sent OS notifications cannot be recalled; every tap still reauthenticates and
checks current home authority.


This packet does not implement an operator-fence CLI or API for permanently
lost broker authority. Supported recovery restores the old exact configured
scope authority (compatible publisher-proof rotation is allowed) and drains
its authenticated same-generation tombstones. If that authority cannot be
restored, held and queued obligations remain retained with
`migration_remediation`; a response from a replacement broker generation
cannot acknowledge or discard them. An exceptional recovery surface requires
a separate reviewed design on the daemon-owned activated store.
