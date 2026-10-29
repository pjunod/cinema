# Remote diagnostics — build a report that explains a Cinema failure

**Status:** open — archive export is the current release scope; device acceptance pending ·
**Written:** 2026-10-12 · **Source baseline:** `8d7bc723e` ·
**Executes:** Paul's request for remote diagnostics on web, Apple and Android,
with archive attachments, including the TV’s own archive on a phone ·
**Builder:** GPT-6.1 Sol.

This is the implementation contract. Read the
[execution ledger](REMOTE-DIAGNOSTICS-STATUS.md) for ownership and evidence,
[development pipeline](../DEVELOPMENT_PIPELINE.md) for integration, and
[web source map](../clients/WEB-SHELL-LAYOUT.md) before changing web code.
Re-verify source anchors against the actual task base. Work through §12 in
order; record a contract change and its reason before implementing it.
Preserve playback authority, account isolation and sharing boundaries. A
diagnostic observer must never become another playback controller.

## Current direction — attach a ZIP to a bug report

Paul changed the release direction on 2026-10-13: the diagnostic tool should
produce an archive a viewer can attach to a bug or issue report. This supersedes
the inbox, direct-send and hosted phone-handoff requirements below. Their built
source is retained as deferred work; it is not a release dependency and the
prepared private service will not be installed.

The ordinary flow is Report a problem → describe the issue → review optional
private content → Save / share diagnostic ZIP → attach it to an issue. Web
saves a file; iPhone/iPad and Android phones use the system share sheet. Export
must work when Cinema is unreachable, and must not enroll an installation or
contact a support service. A ZIP contains `summary.txt`, `manifest.json`, the
user's `description.txt`, bounded structured client events, and available
user-scoped server evidence. Title and platform attachments stay opt-in.

Credential sign-in forms omit the reporting action (issue #1321). Server
selection, first-account setup, saved-session connection recovery, and
signed-in Help/player reporting retain their existing entry points. Anonymous
capture remains available from the connection/setup paths.

Native TVs expose their selected ZIP only after the physically controlled
report screen's “Save TV ZIP on a phone” action. A temporary listener binds a
private IPv4 interface, uses a random capability with at least 128 bits of
entropy, expires after five minutes and permits one explicit download. The QR
opens a minimal phone page; only its Download button's POST consumes the
archive, so previews do not consume it. No support service, enrollment, DNS or
TLS configuration is required. Cinema may be unavailable.

The screen warns that local HTTP is unencrypted and requires trusted Wi-Fi;
anyone possessing the link can download its optional private content. The ZIP
is frozen from the TV's selected incident and privacy choices, not captured on
the phone. Closing, backgrounding, changing incident/privacy/description,
deleting or changing authorization revokes the listener and active sockets.
The endpoint accepts no file path or playback command. Requests, connections,
headers and archive bytes are bounded; cross-origin forms, body framing and
wrong capabilities are refused. A stopped or expired download can be restarted
with a fresh link. Transfer requires phone-to-TV LAN reachability; guest-network
isolation can block it. Physical and signed-device evidence stays distinct from
owned simulator/emulator evidence in the execution ledger.

The sections below retain the original implementation design and receipts.
Hosted delivery requirements are historical until a new user instruction
reopens that work.

## 1. First release — capture, explain and deliver one incident

A remote viewer can report a failure without a terminal, device developer
mode, administrator account or access to the server filesystem. A report
contains what the client actually observed and whatever server evidence the
viewer is authorized to collect. Missing evidence is explicit.

The first release includes all of these, rather than treating native clients
or TV delivery as follow-up work:

| Surface | Required first-release behavior |
|---|---|
| Desktop/mobile web | Local recorder; Report a problem from Help, player and login/connection errors; download ZIP; send to inbox; pending reports |
| iPhone/iPad | Same capture and delivery; system share sheet; restart recovery; phone web handoff works without installing an app |
| Apple TV | Native recorder, focusable report flow, direct send, QR/manual phone handoff, restart recovery |
| Android phone/tablet | Same capture and delivery; system share sheet; process-death recovery |
| Android/Google TV | Native recorder, D-pad report flow, direct send, QR/manual phone handoff, process-death recovery |
| Browser TV/desktop wrapper | Web reporter in the TV presentation, including phone handoff |
| Cinema daemon/cluster | User-scoped incident snapshots, attributed worker evidence, honest partial results and bounded collection |
| Separate support service | HTTPS upload ingress, temporary phone handoffs, authenticated operator inbox, download/delete/retention, deployment package |
| All clients | Five-minute reproduction capture; bounded connection checks; local previous-run evidence and available platform diagnostic attachments |

The first release is not complete when these are merely buttons or mocks.
At least one real report must travel from each platform family to the
operator inbox, and both native TV families must complete the phone flow.

**Deliberate exclusions:** remote shell/control; unattended screen recording;
media-file uploads; full databases or system-wide log dumps; cross-household
log access; automatic submission of every failure; a general-purpose file
hosting service; a new analytics/advertising identity; automatically opening
issues or emailing users. Third-party crash SaaS and advanced population
analytics can be considered later. Connection checks do not change DNS,
firewalls, certificates, saved origins or playback settings.

## 2. Existing foundations — extend their observations, not their authority

These are existing source anchors, not claims that the new report exists:

| Existing surface | Reuse and limit |
|---|---|
| [HTTP system diagnostics](../../crates/plurxd/src/http/system.rs) | Client-log ingestion, admin system/log/playback-event views. Ordinary report callers must not gain the admin endpoints |
| [Log rings](../../crates/plurxd/src/logbuf.rs) and [telemetry writer](../../crates/plurxd/src/telemetry.rs) | Bounded observation and loss accounting. In-memory rings disappear on restart; playback retention can be disabled |
| [Cluster bundle](../../crates/plurxd/src/http/cluster_operations.rs) | ZIP/manifest/redaction precedent. Its aggregate status and local cluster-log tail are not a playback incident spanning every worker |
| [Session log correlation](../../crates/plurxd/src/transcode/session_log.rs) | Non-secret session digest. Raw stream session identifiers are bearer capabilities and must not enter a report |
| [Web early errors](../../crates/plurxd/src/web/core/errors.js) and [API helper](../../crates/plurxd/src/web/core/api.js) | Bootstrap failures and request outcomes; current authenticated best-effort delivery is not offline evidence |
| [Apple player](../../clients/apple/Sources/PlayerController.swift), [surface model](../../clients/apple/Sources/PlaybackSurfaceModel.swift), [session](../../clients/apple/Sources/Session.swift) | Playback diagnostics, action/state evidence and account lifetime |
| [Android telemetry](../../clients/android/app/src/main/java/tv/plurx/app/player/PlaybackTelemetry.kt), [network client](../../clients/android/app/src/main/java/tv/plurx/app/data/Net.kt), [Live TV diagnostics](../../clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvTerminalDiagnostics.kt) | Existing structured measurements; persist locally before optional best-effort delivery |
| [Notification broker](../../crates/plurx-notification-broker/README.md) | Study narrow credentials, deployment and revocation patterns. Do not add support storage or report authority to this fixed-purpose binary |
| [Remote companion implementation](../clients/TV-REMOTE-AND-COMPANION-IMPLEMENTATION.md) | Reuse QR/focus presentation utilities where suitable. Diagnostic handoff is a separate capability, never a playback-control grant |

Add a small shared Rust diagnostics contract module and a separate
`plurx-support` workspace binary. Exact new file names are proposals below;
all paths linked above already exist. No raw logs should be exported simply
because a regex sanitizer happens to accept them.

## 3. Architecture — the support path survives a Cinema outage

```text
 app actions / player / network / bootstrap
                     |
                     v
 bounded local recorder --> frozen report --> local ZIP/share
                                  |
                  authorized Cinema snapshot (optional)
                                  |
                                  v
                  independent HTTPS support service
                    |                           |
              staged TV report             submitted report
                    |                           |
          phone claim + TV approval        operator inbox
                    |
              phone adds context
                    |
               explicit Send
```

1. **Record locally before attempting delivery.** A broken login or server
   connection is a reportable incident, not a reason to discard evidence.
2. **Separate support hosting from the media daemon.** The service has its
   own process, storage and public HTTPS origin. It needs no route into a
   user's LAN and accepts no arbitrary fetch/callback URLs.
3. **Keep capture and submission separate.** Freezing/exporting is local;
   sending or staging names the recipient and requires an explicit action.
   Ordinary existing client-log behavior is unchanged by this consent.
4. **Use one report format and state machine.** Swift, Kotlin, JavaScript and
   Rust consume shared fixture meanings, including malformed inputs.
5. **Keep operational data node-local.** Recorder events and ZIPs never go
   through Raft. Only configuration/authorization that already belongs in
   replicated settings may use the existing store contracts.
6. **Use partial success.** A five-second server collection timeout cannot
   prevent a local report. A down support service cannot prevent export.

### 3.1 User and administrator boundaries

Every local report belongs to an exact server-instance/account namespace.
Pre-login capture has a separate anonymous namespace. Never replay one
account's queued evidence or grants under another account. On sign-out,
cancel scoped requests, invalidate in-memory grants, and clear that account's
unsubmitted local captures; warn only if the user has deliberately saved a
pending report they would otherwise lose. Account switching locks old
namespaces until that same account authenticates again.

A normal viewer gets only their own client data and positively attributed
server events. Administrator reports can explicitly include a bounded system
snapshot and the existing cluster support projection. The support operator
role exists only on the support service and confers no Cinema administration.

Shared-library playback keeps source/receiver trust boundaries intact.
Release one collects receiver evidence and already-authorized source facts.
A source-household administrator may separately export source diagnostics;
do not add cross-household diagnostic access to the sharing protocol.

## 4. Report contract — readable evidence with explicit limits

### 4.1 Version and identity

Use version `cinema.diagnostics.v1`. Generate a random report UUID when
capture freezes. Also record a random process/run UUID, monotonically
increasing sequence per run, UTC timestamp and monotonic elapsed time.
Encode potentially 64-bit counters/identifiers as canonical decimal strings
where JavaScript could lose precision. Millisecond durations are bounded
integers. UUIDs and digests use a canonical encoding in all clients.

Use non-authorizing correlation identifiers for playback, request, attempt,
worker and shared-receiver context. Derive session correlation through the
existing server contract; never export raw HLS IDs, URL capabilities,
invitation tokens or account tokens. A client-provided correlation label is
evidence, never authorization to read server data.

Clock alignment records a bounded request/response time estimate and its
uncertainty. Present a per-run ordered timeline; do not claim globally exact
ordering between devices whose clocks cannot be aligned. Distinguish
observed facts, capability predictions and diagnostic inferences.

Proposed normalized manifest shape:

```json
{
  "version": "cinema.diagnostics.v1",
  "report_id": "827432ee-5d67-4dcf-a111-f607d8426a41",
  "captured_at": "2026-10-12T16:00:00Z",
  "category": "playback",
  "scope": "viewer",
  "client": {
    "platform": "tvos",
    "app_version": "example",
    "build": "example",
    "os_version": "example",
    "device_model": "example"
  },
  "capture": {
    "mode": "ordinary",
    "window_ms": 600000,
    "dropped_events": "0",
    "persistence": "available"
  },
  "components": [
    {"name": "client", "state": "complete"},
    {"name": "server", "state": "unavailable", "reason": "connection_failed"}
  ],
  "privacy": {"titles_included": false, "attachments_included": false},
  "files": [
    {"name": "summary.txt", "size_bytes": 1234, "sha256": "<digest>"}
  ]
}
```

The manifest example is illustrative; B01 makes exact enums, bounded fields,
required/null semantics, duplicate-key rejection and schema fixtures
executable. Unknown schema versions are rejected with a useful update
message. Optional components from older software remain explicitly absent.

### 4.2 Archive contents and bounds

| Entry | What it answers |
|---|---|
| `manifest.json` | Version, build, coverage, limits, privacy selections, file sizes and SHA-256 checksums |
| `summary.txt` | What was reported, when, by which build/device, and which evidence is missing |
| `description.txt` | User-entered problem and reproduction steps, explicitly identified as user content |
| `client-events.jsonl` | Ordered actions, state transitions, request outcomes and errors |
| `playback.json` | Source/delivery/render observations, selected tracks, buffering, frame progress and recovery |
| `connection.json` | Passive network observations and optional active test results |
| `server-events.jsonl` | Only events the current caller may read, with node aliases and correlation |
| `server-snapshot.json` | Applicable component versions, relevant effective settings, producer state and resource evidence |
| `diagnostics/` | Sanitized supported crash/hang data with origin and availability metadata |

All entries are data. No HTML, executable files, scripts, symlinks, absolute
paths, traversal names, nested archives or duplicate names. No automatically
collected media, screenshots or free-form request/response bodies.

Use initial hard limits of 16 MiB compressed, 64 MiB expanded, 32 files and
8 MiB per file. Count bytes while streaming and enforce both declared and
actual sizes. The final manifest is excluded from its own checksum list.
Budgets include framing/encoding overhead. Drop low-priority samples before
incident markers and terminal events, and name every truncation in coverage.

Local frozen captures target at most 8 MiB each; larger combined reports
must still satisfy archive limits. Submission description is at most 4,000
Unicode characters and 16 KiB UTF-8. Optional title inclusion is per report.
First-release screenshot attachment is optional implementation scope; if
included, it needs preview, explicit selection, metadata removal and the same
bounds. It is never necessary for first-release acceptance.

**How to read it:** complete means the named component finished collection
within its declared scope. It does not prove all historical events survived.
Partial means usable evidence with a named gap. Unavailable means no claim
can be made about that component. A zero count is never substituted for
unavailable evidence.

## 5. Recorder — bounded, persistent and independent of playback

### 5.1 Recording contract

Provide equivalent small interfaces in each language:

```text
record(typedEvent) -> accepted | coalesced | dropped
freeze(category, incidentMarker) -> reportHandle
startReproduction(deadline <= 5 minutes) -> captureHandle
stopReproduction(captureHandle) -> reportHandle
export(reportHandle, privacySelection) -> bounded archive
delete(reportHandle)
```

Proposed budgets: ten minutes, 10,000 events and 2 MiB serialized active
history, whichever limit is reached first; 2 KiB per event. Reserve 20% of
event capacity for errors/actions/terminal transitions. Sample steady
playback at most once per second; coalesce unchanged state. No frame-by-frame
or segment-body recording. Use a bounded producer queue with loss counters;
never wait on filesystem/network work in a player callback.

Native apps flush batches off the UI thread at most once per second, with an
urgent flush request on freeze/terminal events. Use a checksummed append or
transactional store that recovers a valid prefix after process death. Browser
persistence uses IndexedDB, with memory-only fallback and a visible
persistence-unavailable field. Storage denial, quota exhaustion and private
browsing do not break the application. Account namespaces remain separate.

Keep at most three frozen unsubmitted reports for seven days and a 32 MiB
total local budget including the active recorder. Before displacing a
user-saved pending report, show the storage decision; ordinary rolling samples
may be discarded automatically. Backgrounding stops reproduction sampling
and flushes what the platform permits. TTL cleanup also runs at next launch.
TV storage may be purged by the OS; do not promise durable recovery where the
platform does not guarantee it.

### 5.2 Required observations

| Domain | Required facts, when observable |
|---|---|
| Bootstrap/UI | Run/build, app foreground/background, route/screen class, startup errors, navigation action category, main-thread stall evidence if available |
| Network | Route template, method, duration, status/typed failure, retry count, cancellation, connectivity transition; no URL query, headers or body |
| Playback | User intent; decision reason; direct/remux/transcode; source and delivered codecs/range; renderer observation separately; selected audio/subtitle types |
| Progress | Playhead, loaded ranges/headroom, frame/clock progress, dropped frames if available, TTFF, stall start/end, recovery outcome |
| Producer | Attributed owner/worker, recipe/encoder identifiers, readiness, production progress, delivery failures, typed process exit, safe stderr classification |
| Context | Build/OS/model, relevant allowlisted effective settings, capability predictions, available resource-pressure signals |
| Coverage | Overflow, rate limiting, storage loss, restart boundary, unsupported field, collection timeout, missing peer, telemetry disabled |

Do not infer hardware decoding from a capability prediction, actual HDR from
the source tag, or a crash from an unclean previous shutdown marker. Preserve
`unknown` and reasons. Observers must not extend a playback lease, trigger a
retry, keep a player alive or override recovery policy.

### 5.3 Freeze and reproduction

Opening Report a problem immediately freezes the preceding window and
selected playback snapshot, before the user types. Snapshot server evidence
for the same incident time. A reproduction capture adds up to five minutes
of more detailed transitions, displays a persistent indicator and offers
Stop/Cancel. It does not globally enable debug logging, collect secrets, or
automatically send. Cancel deletes its unsubmitted draft; upload cancellation
semantics are specified separately in §7.

## 6. Cinema collection — user-scoped evidence across serving nodes

Add proposed routes under `/api/v1/diagnostics`, integrated with existing
authentication and listener classification. Exact module names can follow
the current router layout.

| Route | Authority and behavior |
|---|---|
| `GET /capabilities` | Public, small non-sensitive schema/feature response; no configuration secrets or user facts |
| `POST /snapshots` | Signed-in caller; freeze their incident projection; body includes bounded time window and authorized context; 202 with opaque owner-bound job ID |
| `GET /snapshots/{id}` | Same caller only; pending/complete/partial/unavailable and authorized component data |
| `DELETE /snapshots/{id}` | Same caller only; cancel/erase temporary snapshot |
| `POST /upload-grants` | Signed-in caller; obtain a single-report support grant through configured publisher identity; no implicit upload |

Root the context in server-established user/session ownership, not the
client's claimed file ID, timestamp, user ID or attempt string. If existing
ownership indexes cannot authorize a stopped session, add a minimal bounded
node-local diagnostic ownership receipt with a 24-hour TTL. Bind it to
server instance, authenticated principal, safe correlation and serving-node
identity; do not retain playback capabilities. Its lookup is authorization,
not a client-searchable event index. Expired/unprovable ownership yields
`context_unavailable`, never a general-log fallback.

The collecting node derives participant nodes from server-owned playback
routing evidence. It contacts at most eight relevant nodes with concurrency
four, a two-second per-peer budget and five-second total budget. Evidence
from additional nodes is explicitly omitted. Peer RPC authenticates the
cluster transport and carries a collector-derived scope, bounded time range
and safe correlation; it is not an arbitrary filter/SQL/path request. Peer
authorization rechecks that the scope can be served under the current
identity/revocation model.

Keep snapshots for 15 minutes, at most two active jobs per user and 32 per
node, with an explicit byte cap of 64 MiB total node-local snapshot storage.
Rate-limit creation to three per user per five minutes. Reject excess jobs
with a typed retryable response; never evict an unrelated running job
silently. Cancellation/timeout owns every spawned collector and temporary
file. Shutdown, orphan cleanup and ENOSPC have regression coverage.

Use a structured diagnostic projection for normal viewers. The existing
general log ring is not reliably attributable to one person; do not scan it
by substring and release matching lines. Admin-only richer evidence remains
a separate projection. If a component cannot supply a safe projection, name
it as unavailable and implement the missing instrumentation in its packet.

## 7. Independent support service — upload, retain and inspect

### 7.1 Hosting, identities and configuration

Build `crates/plurx-support/` as a separate binary with SQLite metadata and
bounded blob storage. Add workspace/lockfile/license checks, packaging, a
health/readiness endpoint, and a reverse-proxy deployment example using a
neutral hostname. Reuse established cryptography libraries, not custom crypto.

Configure Cinema's support origin and publisher credential through an
operator-owned secret file outside exported settings. Proposed configuration
keys are `support.origin`, `support.publisher_id` and
`support.publisher_secret_file`; store only non-secret references in any
replicated settings. Never distribute a publisher secret to an app.

Publisher enrollment is an operator CLI action on the support service.
Each publisher represents one Cinema installation and receives a revocable
credential that can mint single-report upload grants, not read inbox reports.
Store credential digests. Prefer 256-bit random opaque capabilities, compared
in constant time, scoped by operation/report/publisher/expiry. Enforce
revocation on every operation, including retries and finalization.

Keep the upload public endpoint separate from the operator administration
origin/listener. The inbox uses named operator accounts with passkeys and
short-lived server sessions; bootstrap the first enrollment through an
operator-only CLI token. No public operator registration or universal shared
admin password. Cookie sessions are Secure/HttpOnly, protected against CSRF
and checked against an exact origin allowlist. Log access/deletion metadata
without copying report contents or secrets into access logs.

### 7.2 Pre-login and disconnected clients

A client that previously connected may cache only the non-secret support
origin plus a deliberately provisioned, upload-only enrollment capability,
bound to its diagnostic installation and capped by publisher quotas. Add an
explicit endpoint to provision/rotate/revoke this capability under the
signed-in account; define it in B03. Store native capabilities in
Keychain/Keystore-backed storage, and treat browser storage as weaker:
browser upload enrollment is memory-only by default and export remains the
durable fallback. This capability cannot read account/server data, retrieve
reports or create remote-control grants.

Enrollment must remain tied to the original principal when Cinema is down:
the service can validate its publisher/generation/expiry locally, but must
not claim immediate knowledge of a Cinema account revocation it has not
received. Use a 24-hour maximum enrollment lifetime, explicit publisher
revocation synchronization when reachable, and disclose this bounded
upload-only revocation delay. No cached authority retrieves server evidence.

On a completely new installation, Help accepts an operator-issued support
invitation, containing the trusted support origin and a one-use upload
authorization. For TV first-login failures it can be entered as a code on the
TV using the configured support origin; the inbox operator creates the code.
A short code has bounded attempts and expiry and grants only one upload.
No secret baked into a publicly distributed app is treated as authentication.

An unknown or user-entered support origin requires explicit destination
confirmation; a normal Cinema login token is never sent to that origin.
The normal direct route obtains a fresh single-report grant from Cinema.
The disconnected route uses the restricted enrollment/invitation. If neither
is available, save/export still works and explains how to obtain an invitation.

### 7.3 Upload protocol and durable states

Proposed service routes are rooted at `/support/v1`:

| Route | Authority / result |
|---|---|
| `POST /grants` | Publisher credential or restricted enrollment/invitation; report-bound, upload-only grant, 30-minute lifetime |
| `POST /uploads` | Grant; idempotently reserve report ID, exact length/hash and privacy schema; return upload ID |
| `PUT /uploads/{id}/chunks/{index}` | Same grant; fixed 1 MiB chunks except final; verify chunk hash/offset; repeated identical write succeeds, conflicting write is 409 |
| `GET /uploads/{id}/status` | Same grant; received chunk indexes and expiry only |
| `POST /uploads/{id}/complete` | Same grant; verify full hash and bounded archive/schema, commit immutable blob, return staged or ready state |
| `POST /reports/{id}/submit` | Bound report-management grant; finalize description/privacy selections and mark submitted exactly once |
| `GET /reports/{id}/receipt` | Bound management grant; delivery state/reference only, no report enumeration |
| `DELETE /reports/{id}` | Bound unexpired management grant or operator; erase draft/report and revoke grants |

Use separate upload and report-management grants; an upload-only credential
must never read report contents. The server creates the management grant
only for that capture's authorized creator and later its approved phone.
Document authorization for every state transition in executable fixtures.

```text
local frozen -> reserved -> uploading -> validating -> staged -> submitted
                   |           |            |           |
                   +-----------+------------+-----------+--> cancelled/expired
submitted -> resolved (operator label) -> deleted/retention-expired
```

Local upload state survives app suspension/process death where storage
permits. Clients retry with jitter and exponential backoff, at most five
automatic attempts while foregrounded per action; a Retry button remains.
Retry only an already-consented upload, not an arbitrary pending capture.
Grant refresh preserves the same report/publisher/content identity and never
extends a draft's absolute 24-hour lifetime. No background task reliability
is assumed. Reopening the app can resume.

A receipt means the validated blob and metadata are durably committed;
it does not mean a human has read the report. An ambiguous HTTP timeout is
resolved through status/receipt with the same identity, not a duplicate report.
Changing content requires a new content revision before submission; a
submitted archive is immutable. Description completion is allowed before
submission and bounded separately from the immutable archive.

Use temp-file write, fsync, atomic publication and transactional metadata,
with startup reconciliation of partial/orphaned files. Crash at every
publication boundary must yield a resumable upload, a committed receipt, or
a typed terminal failure, never a phantom successful receipt.

### 7.4 Service security, quotas and lifecycle

Verify the archive incrementally in a bounded worker; never extract it into
a web-served directory. Enforce §4 limits and parser depth/string/event count
limits. Reject forged checksums, unsupported entry types, decompression
bombs, duplicated fields and bad encodings. Rate-limit before expensive work.
Render description and all evidence strictly as escaped text.

Initial quotas: two concurrent uploads per enrollment, five per publisher;
ten submissions per enrollment per day, 100 per publisher per day; 1 GiB
stored per publisher, 20 GiB global. Anonymous invitation redemption gets
five failures per code and bounded IP buckets; do not trust arbitrary
forwarded-IP headers. Full storage rejects new reservations without damaging
existing reports. All limits are operator-configurable within documented
safe bounds and included in readiness.

Encrypt blobs at rest with an established AEAD, unique nonces, report/publisher
identity as associated data and versioned keys stored outside the database.
Encrypt sensitive description/diagnostic metadata as well. TLS terminates at
the deployed ingress; test that raw HTTP public access and credential-bearing
cross-origin redirects are refused. This is service-readable encryption,
not a claim of end-to-end encryption from the support operator.

Incomplete uploads expire after one hour of inactivity, bounded by 24 hours
absolute. Unsubmitted staged reports expire after 24 hours; phone claims
expire much sooner (§8). Submitted reports expire after 30 days by default,
with explicit operator extension up to 90 days. Delete active blobs, chunks,
metadata containing report content and associated grants; retain only minimal
non-content audit tombstones. Apply a documented bounded backup retention
and restore deletion journal so a restore does not silently resurrect deleted
reports or revoked capabilities. On generation/key rollback, refuse serving
until the documented operator recovery step fences old grants.

No report-reference number, filename or search term is a download credential.
All UI/API caches use no-store for sensitive responses. Redact proxy access
logs; no analytics, third-party scripts or referrers on report/handoff pages.

### 7.5 Operator inbox

Deliver a working responsive browser inbox with passkey sign-in, filters by
date/platform/build/category/state, free-text user descriptions shown as text,
and a detail view with summary, evidence coverage, ordered timeline and
download/delete actions. State labels are new, investigating and resolved.
Reference search is exact and authenticated; do not implement arbitrary
full-text indexing of sensitive raw evidence in release one.

Show source/delivery/render facts separately, and explain missing worker,
disabled telemetry, recorder overflow and unknown clocks. Provide a
standalone local bundle inspection command so downloaded reports can be
read without the hosted UI. It validates the same bounds and checksums;
it never runs embedded content. Operator comments/labels do not mutate the
submitted evidence. Audit viewing, downloading, deletion and retention changes.

## 8. TV phone handoff — one report, confirmed on the television

The phone uses the support service's mobile web page. It needs neither the
Cinema native app nor access to the TV's LAN. Do not require Cinema account
login just to finish an explicitly approved report. TV and phone need
internet reachability to the support service; no network means local capture
and a truthful pending state, not an allegedly working QR transfer.

### 8.1 User flow and consent

1. The user opens Report a problem on TV. Freeze locally and show categories,
   privacy choices, recipient, Direct send and Continue on phone.
2. Continue on phone explains that the selected redacted report will be
   temporarily uploaded to that recipient's service. Only this action starts
   staging. A phone cannot add title/screenshot data the TV did not approve.
3. When staging is durable, show a QR and a short manual code with a
   ten-minute expiry. No cinema/server credential appears in either.
4. The phone scans/enters the code and requests a claim. Both screens show
   the same short comparison phrase/code; the TV asks Approve this phone.
   Before TV approval, the phone sees no description, preview or report data.
5. Approval atomically binds the report to that phone's ephemeral session.
   The phone can review a safe summary, add a description and explicitly Send.
   It can further redact optional content, never widen the TV-approved scope.
6. Phone and TV receive the same durable receipt/reference. Cancel/expire
   revokes the handoff. The original local capture remains available for a
   fresh attempt until the user deletes it or its retention expires.

Direct send is fully usable without a phone. Back/dismiss on TV should
explain whether a staged report remains; cancelling the handoff deletes the
unsubmitted server draft unless the creator explicitly keeps it. Cancelling
after submission requires the report deletion action rather than pretending
the submission never occurred.

### 8.2 Handoff protocol

Add `POST /handoffs` with creator management authority, then separate
claim/status/approve/reject/cancel endpoints. A handoff is bound to one
publisher, report ID, content hash and privacy revision. Use an atomic
server state machine:

```text
offered -> claim_pending -> approved -> submitted
   |            |              |
   +------------+--------------+--> revoked/expired
```

QR carries a 256-bit one-use claim secret in the URL fragment. The page
removes the fragment from history before loading anything else and exchanges
it by POST. Never put it in query strings, server logs or referrers. The
manual code is ten unambiguous base32 characters, valid for ten minutes,
with five failed claims and per-IP/global limits. Either path still needs
physical TV approval; the comparison phrase is not itself a bearer secret.

The phone creates a random session proof at claim time. Approval binds its
digest atomically; only that session can receive the report-management grant.
Two phones racing cannot both win. A second claimant cannot evict a pending
claim; TV can reject and regenerate. Creator status/revoke and claimant
polling use separate scoped credentials, not the displayed code. Poll no
faster than every two seconds, stop on background/expiry, and apply bounded
backoff. SSE is optional, not a delivery dependency.

Offer/claim windows are ten minutes; an approved phone editing session lasts
30 minutes, bounded by the draft's absolute lifetime. Once approval succeeds,
TV process death must not invalidate an already authorized phone, but explicit
TV revoke must. Reject stale approvals and callbacks after account changes.
Consumption, expiry, revision change and revocation are durable and checked
on every endpoint. Failed attempts cannot restore an old token's authority.

The handoff page is a first-release deliverable in `plurx-support`. It has
accessible text instructions, manual-code entry, loading/failure states and
no camera permission requirement. Test a phone on cellular while the TV is
on another network.

## 9. Client implementation — one contract, native ownership

### 9.1 Web and browser TV

Proposed new classic scripts: `core/diagnostics.js`,
`core/diagnostics-storage.js`, `core/diagnostics-upload.js` and
`pages/help.js`. Follow the
[served-order rules](../clients/WEB-SHELL-LAYOUT.md): file, WEB_ASSETS row,
shell tag and map row belong in the same commit. No import/export syntax.
The earliest minimal recorder must be dependency-free and available before
other sidecars; bridge the current early error reporter without recursive
reporting or turning diagnostic failures into further diagnostic requests.

Instrument semantic action and existing player/network seams, never log
DOM contents, text fields or full URLs. Capture global errors, rejections,
resource/boot failures and non-playback navigation failures as well as
playback. Do not patch every console method to scrape arbitrary messages.
Freeze/export must work before login and when authenticated API calls fail.

Add a non-admin Help destination independent of admin Settings data loads;
integrate player and bootstrap-error entry points. Pending Reports supports
retry/export/delete with correct account scoping. Use download as baseline;
use Web Share only after feature detection and keep download when unavailable.
Browser TV has remote-focusable categories, Send and QR views.

Uploads use a dedicated support transport; never reuse a helper that attaches
Cinema Authorization to a different origin. Explicit origin validation,
CORS policy, redirect rejection and request cancellation are required.
Support safe localhost HTTP only in an explicit test/development mode.

Register allowed browser Cinema origins per publisher, validate exact origins
before permitting credential-bearing CORS, and omit ambient cookies from
cross-origin client uploads. The phone page uses its own service origin.
A browser whose Cinema shell has never loaded cannot record a cold-load
network failure: direct the user to the separately hosted support invitation
page and identify its evidence as phone/browser observations, not a recovered
Cinema recording. Do not promise offline web availability without an
independently qualified offline shell.

### 9.2 Apple: iOS, iPadOS and tvOS

Proposed files under `clients/apple/Sources/`:
`DiagnosticRecorder.swift`, `DiagnosticStore.swift`,
`DiagnosticReport.swift`, `DiagnosticTransport.swift`,
`DiagnosticReportView.swift` and `DiagnosticHandoffView.swift`.

Use a bounded serial/actor-owned recorder and off-main persistence/export.
Keep hooks in PlayerController, playback surface, Live TV, Session and
navigation thin; never retain player/session objects in a pending report.
Integrate SettingsView plus login and player error menus. On iOS/iPadOS,
export through the system share sheet with temporary-file cleanup and
correct iPad presentation. tvOS uses direct send and TV-focusable handoff.

Persist ordinary events and an unclean-run marker early enough for next-launch
inspection. Attach platform crash/hang diagnostics only when actually
available and sanitized. Check MetricKit/API availability against the
shipping deployment targets; do not assume tvOS exposes iOS APIs. System
delivery can be delayed or absent. Call the previous run interrupted unless
there is actual crash evidence. Preserve dSYM/build UUID artifacts through
the release process.

Use protected local storage where supported, exclude pending reports from
cloud backup, and keep upload/enrollment secrets in appropriate Keychain
items scoped to server/account. Handle lock/unavailable storage explicitly.
Do not add a background entitlement solely to promise uninterrupted upload.
Foreground retry after suspension is sufficient and must work.

### 9.3 Android: phone, tablet and TV

Proposed package `tv.plurx.app.diagnostics` adds bounded recorder/store,
report schema/exporter, dedicated transport and Compose report/handoff
screens. Integrate Net outcome hooks, PlaybackTelemetry, Controller,
PlaybackSurfaceOwner, LiveTvTerminalDiagnostics and app navigation without
changing player recovery ownership.

Use a bounded channel and an IO-owned writer. Store drafts in app-private
storage, excluded from backup; protect persistent authorization material with
the app's established Keystore pattern. The share sheet uses FileProvider
content URIs and temporary read grants, never world-readable paths.

Handle process death, coroutine cancellation and account replacement.
WorkManager may resume a previously consented upload with unique work keyed
by report/account, but is optional; it must never silently initiate a new
report. TV focus must work entirely from a D-pad without requiring a keyboard.

Use supported process-exit/ANR/crash information where available, marked with
API/version limitations. Do not ship a global uncaught-exception handler
that allocates, performs network I/O or masks the original failure. Preserve
R8 mappings and native symbols when applicable; verify the signed release
variant, not only debug instrumentation.

### 9.4 Connection checks and local crash evidence

First-release guided checks use the configured Cinema endpoint and
authorized playback route only. Check reachability, TLS, authentication,
server readiness/version and a bounded delivery probe when a safe existing
resource is available. No LAN scanning or unrestricted URL fetches.
Cap a run at 15 seconds and an optional transfer at 2 MiB. Obtain consent
before a cellular transfer and allow cancellation.

Native clients can distinguish DNS/connect/TLS phases when their API exposes
them. Browser failures are often opaque; say browser network failure rather
than inventing a DNS or certificate diagnosis. Record unsupported phases.
Store results in the incident with timestamps; a later healthy probe does
not erase the original failure.

A crash-report adapter's first-release obligation is truthful availability
plus preserved local breadcrumbs and build identity on every platform.
Native stack capture must use supported mechanisms. Store/provider crash
portals are supplemental; a user should not need their own developer account
or device-console export to report an ordinary incident.

## 10. Privacy — enforce the schema at recording and export

Define one field classification table in B01 and fixture it across languages.
Safe typed enums/measurements are recorded directly. Opaque correlation uses
per-report pseudonyms or the established non-secret digest contract.
Potentially private free text is never captured by default. User-authored
description is visibly user-supplied and previewed before sending.

Exclude authorization headers, cookies, passwords, API keys, pairing proofs,
raw stream capabilities, account names/emails, home paths, hostnames, raw IPs,
Wi-Fi names, request queries, library listings and media contents. Network
type and route role may be kept; use stable report-local aliases for nodes.
Optional media title inclusion starts off and cannot enable path inclusion.

Redact structured fields at the producer and enforce a second schema/safety
pass during export. Do not rely on a final regex over arbitrary logs as the
primary boundary. Server-side ingestion repeats structural checks; if a
forbidden field appears, reject it and show the error locally rather than
silently storing it. Free-form crash diagnostics need an explicit adapter
with a tested sanitizer before being attachable.

Tests seed unique canary credentials, path/user/host markers, URL fragments,
nested error strings and FFmpeg argument echoes. Assert their absence from
local recorded structured data, export, upload body, proxy/service logs,
phone preview and inbox rendering. Protect user-entered descriptions as
sensitive text; warn about detected obvious secrets without claiming to
detect every possible private fact.

Local capture belongs in ordinary Help/privacy controls. Explain its bounded
local retention and offer Clear diagnostics. A meaningful Disable local
history preference is permitted in Help/Privacy; explicit reports still
capture their current snapshot and disclose that history was disabled.
There is no gratuitous master Developer toggle for internal instrumentation.
Unqualified capability cards, if present, follow
[Developer graduation](SETTINGS-NAVIGATION-AND-DEVELOPER-STATUS.md#developer-lifecycle--every-card-graduates)
and cannot block saving choices or exporting evidence.

## 11. Ownership and integration — one owned clone and batched main PR

**Maintainer ruling, 2026-10-12:** “Use the latest workflow; replace those
earlier requirements.” This section supersedes the original project effort
workflow, per-push focused tests, per-task effort gates, and full promotion
qualification for this work.

Use an independently owned clone, never the user's existing checkouts or a
managed worktree sharing their Git administration. Preserve adopted planning
files and implementation before cleaning only the builder's former workspace.
Keep all source, canonical status, the visible status page and build outputs
in the owned clone. Use `codex/remote-diagnostics` for normal packet commits
batched into one larger main-bound draft PR. Keep that PR's owner/chat, scope,
source head, review/check state, blockers and next step current. If work is
interrupted, the last action updates its body accurately.

Verify the pinned Rust 1.97.1 toolchain and compile/type/format checks before
pushing. Compiler checks remain local; CI is not a compiler. Normal repository
hooks remain. Behavior changes retain corrective subjects, landing regression
anchors and native source/test mappings; do not execute unit suites per
packet or commit. Shared fixtures and regression tests remain deliverables.

Only when the main PR is ready, obtain one adversarial agent review, address
findings, and run the fast lane. Every required check needs an applicable
passing result. Rerun failed checks, and rerun additional checks only when a
later fix invalidates their coverage; record the impact. Resolve review and
required fast-lane failures before merge. Another process owns full unit
failures in later batches. No full-suite qualification wait is imposed here.

No code feature gates. A meaningful enable control lives in Developer while
unfinished, with prerequisites/readiness advisory only. Ordinary local
privacy preferences and explicit upload consent are user choices, not release
gates. Use existing architectural ownership and diagnose causes; do not add
watchdogs or playback retry owners. Upload retry ownership stays separate.

The GPT-6.1 Sol chat owns all packets, implementation, decisions and evidence.
Do not spawn routine reviewers before the final PR is ready. Production
support DNS, TLS, operator/publisher credentials and physical devices remain
external deployment/acceptance inputs. Build with synthetic isolated fixtures
and keep implementing independent work when those inputs are unavailable.
Do not invent a public origin, purchase hosting or publish store builds.

## 12. Build packets — deliver all first-release surfaces

Paths below are proposed ownership, not existing-file assertions. Each packet
updates the ledger, API/operations references for implemented behavior, and
the backlog summary. B01 owns the schema; later revisions require fixtures
and compatibility checks on every consumer.

| Packet | Owned implementation surface | Dependencies |
|---|---|---|
| B00 | Workspace/compile loop, baseline audit, plan and ledger adoption | None |
| B01 | Core diagnostic schema, privacy table, shared fixtures, archive validator and local inspector | B00 |
| B02 | Daemon typed recorder projection, ownership receipts, snapshot/peer APIs | B01 |
| B03 | Support service auth, publisher/enrollment/grant model, bounded storage/upload lifecycle, configuration | B01 |
| B04 | Support handoff protocol, mobile completion page, operator inbox, service packaging | B03 |
| B05 | Web recorder/storage, early errors/API/player hooks, Help/report/export/upload/handoff | B01–B04 |
| B06 | Apple recorder/store/transport, iOS/iPadOS report/share, tvOS report/handoff, platform diagnostics | B01–B04 |
| B07 | Android recorder/store/transport, mobile share/report, TV report/handoff, platform diagnostics | B01–B04 |
| B08 | Reproduction and connection-check adapters on all clients; crash/symbol evidence gaps | B05–B07 |
| B09 | Adversarial integration, resource/privacy measurements and real browser/device acceptance | B02–B08 |
| B10 | Operational runbook, release packaging, exact-tree qualification and promotion handoff | B09 |

**B00 acceptance:** record the exact starting commit and isolated workspace,
confirm relevant source hooks, run a successful pinned check on the affected
Rust baseline and compile a representative native baseline. Record any
pre-existing failures with commands before attributing them to this work.

**B01 acceptance:** one valid shared fixture and malicious/overflow/missing
field fixtures decode equivalently in Rust, JavaScript, Swift and Kotlin.
Archive inspection refuses traversal, bombs, duplicate files and bad hashes.
A golden readable summary matches coverage without exposing canaries.

**B02 acceptance:** real HTTP tests with two users and two serving nodes:
caller gets their own evidence, cannot get another user's stopped session,
worker loss yields partial export, and timeout/cancellation leave no workers
or files. Include revoked authorization and telemetry-disabled cases.

**B03 acceptance:** real HTTP upload from an external client reaches a
durable receipt; kill/restart at each commit boundary, resume, repeat
finalize, exhaust quotas, revoke credentials and inject ENOSPC. Prove no
enumeration/read through enrollment or upload grants. Implement the dedicated
provision/rotate/revoke enrollment routes and their account-scoped tests.

**B04 acceptance:** two independent browser contexts and a TV simulator
complete staging, claim, physical approval, description and submit. Two-phone
race/replay, expired/manual code, TV cancel, phone refresh and service
restart retain the contract. A passkey-authenticated operator can read,
download and delete the result; unauthorized callers cannot.

**B05 acceptance:** real browsers capture login/network/boot/player failures,
export offline, survive reload where storage allows, handle denied IndexedDB,
and send/handoff without leaking the Cinema token. Verify keyboard and TV
remote focus and all three existing layouts.

**B06 acceptance:** iOS and tvOS compile; focused tests prove ring/recovery/
privacy/account fencing and transport state. Physical iPhone/iPad share plus
Apple TV-to-phone-on-cellular produce real inbox receipts. Simulator
evidence is labeled as such until physical cells pass.

**B07 acceptance:** Android app/test artifacts compile and release symbol
handling is checked; focused tests prove recorder/store/transport and
Compose/D-pad behavior. Physical mobile share and Android TV-to-phone-on-
cellular produce real inbox receipts. Process death/resume is observed.

**B08 acceptance:** stop/expiry/background behavior ends reproduction on
every client; controlled DNS/TLS/auth/service outage cases produce only
supported diagnoses. No-network capture and interrupted-run recovery remain
usable. Real or synthetic platform payloads are labeled correctly.

**B09 acceptance:** execute §13 against the composed candidate, fix findings
through their owning packet, and retain sanitized receipts and raw evidence
privately. Measure observer overhead rather than assuming it is negligible.

**B10 acceptance:** new operator can deploy the isolated service from the
runbook, enroll Cinema, create a pre-login invitation, collect a TV report,
inspect and delete it. Build the final exact-candidate receipts; update
status/index/backlog without describing pending deployment as shipped.

## 13. Validation — behavior, failure and physical evidence

### 13.1 Required matrix

| Scenario | Web | Apple mobile / TV | Android mobile / TV | Server/service |
|---|---|---|---|---|
| Normal report and ZIP/inbox receipt | All layouts | Both form factors | Both form factors | Hash/manifest/identity verified |
| Playback never starts / stalls / seek / subtitle / wrong range | Real browser | Native observation | Native observation | Correct worker correlation and source/delivery distinction |
| Login refused / server offline / support offline | Export preserved | Draft preserved | Draft preserved | Typed partial/retry states |
| App/process dies before report | Persistence or explicit absence | Prefix recovery/availability | Prefix recovery/availability | Upload crash recovery |
| Account switch/sign-out/revocation | No cross-account evidence | Same | Same | No cross-user snapshot/grant |
| TV QR and manual-code phone transfer | Browser TV | Physical Apple TV | Physical Android TV | Different networks, same submitted report |
| Two-phone claim/replay/cancel/expiry | Phone browsers | TV confirmation | TV confirmation | Atomic single winner and revocation |
| Storage full / quota / oversized archive | Bounded failure | Bounded failure | Bounded failure | No OOM/disk runaway |
| Redaction canaries and hostile text | No credential leak/XSS | Same | Same | Validator, inbox and proxy logs |
| Delete/retention/backup restore | Local cleanup | Local cleanup | Local cleanup | No resurrected report/grant |
| Reproduction/connection checks | Stops; opaque errors honest | Stops; supported phases | Stops; supported phases | No arbitrary network target |
| Old server/new client and converse | Export fallback | Same | Same | Version error/absent evidence, not crash |

Check both playback-connected and pre-login TV support invitations. A
connected happy-path report does not prove that the separate support service
helps during a Cinema outage.

### 13.2 Performance and resource acceptance

On representative phone, Apple TV, Android TV and browser hardware, retain
matched recorder-on/off runs: 30-minute playback with seeks/stalls plus an
idle interval. Initial target is under 1% of one CPU core average added
recorder work, no sustained recorder growth beyond configured caps, and
no attributable playback stalls. Record event-hook p99 latency (target under
1 ms), storage write bytes, queue drops and upload/export peak memory.
Report measured values and device/build; changing a target needs a reason,
not a retrospective green label.

Diagnostic export/upload must yield to playback work and never hold playback
locks while serializing. A bounded native worker and a browser worker or
incremental serializer prevent large ZIP work on UI threads. Concurrent
reports cannot exceed global service reservation limits, including temporary
chunks and validation scratch. Failure metrics are fixed-cardinality:
no report IDs, tokens or user identifiers in metric labels.

### 13.3 Commands and evidence

Existing commands below must be rechecked against the current Makefile.
B01–B08 add focused behavioral tests and record their exact runnable names
in each PR; do not substitute these broad examples for a focused regression.

```bash
rustc --version                       # Must report the repository pin
make fmt-check                        # Rust formatting
make effort-rust-check                # Affected Rust compilation baseline
make web-unit-check                   # Existing Node contracts
make apple-build                      # Both Apple targets compile
make android-instrumentation-build    # App and UI-test APK compilation
python3 -m unittest discover -s tests/operations -p 'test_docs_index.py'
scripts/scrub-infra-names --check      # Public mirror naming
```

Add focused `plurx-support` tests and new diagnostics browser contracts to
the build targets and validation scopes in the same packet as the feature.
Use `make unit-core` or `--features hiqlite-store` for any replicated-store
change. Focused Swift/XCTest and Kotlin/JVM or instrumentation commands belong
in the ledger with actual target/destination/test IDs. Native build numbers
and notes follow the existing release tooling.

Before main merge, apply the 2026-10-12 maintainer fast-lane ruling in §11,
including the new binary/routes/assets and affected native platforms.
Windows compile jobs remain subject to the current maintainer waiver; do
not invent Windows evidence. One hardware family passing does not qualify
another. Use exactly the configured review/gate process, not a new gate
created solely for this plan.

## 14. Operations, rollout and completion

Ship a service README, deployment sample, enrollment/rotation/revocation/
backup/restore/deletion commands and the local inspection tool's usage.
Document which secrets live where, what the inbox can read, ingress limits,
disk/quota alerts, retention and safe upgrade/rollback. Secret values are
operator inputs, never examples copied from the real fleet.

Rollout order: deploy qualified support service and exercise a synthetic
report; enroll a test Cinema installation; distribute candidate clients;
perform per-platform/TV acceptance; then enable the configured support
destination for intended external users. Missing support configuration
disables only delivery and explains it; local reporting remains usable.
Deployment approval, actual DNS/TLS and device availability can be recorded
as remaining inputs while implementation proceeds.

Rollback turns off new delivery/minting while preserving local export and
already submitted reports. Revoke publisher/enrollment authority if exposed.
Keep a reader for v1 archives and existing unexpired grants during compatible
upgrades; incompatible changes use a new version/generation, not reinterpretation
of old capabilities. Deletion remains available during normal drain.

A first-release completion receipt names the final source commit, schema,
service build/deployed origin (private evidence when needed), all client
builds, required tests, physical TV/phone receipts, privacy/performance
measurements and unresolved limitations. The
[ledger](REMOTE-DIAGNOSTICS-STATUS.md) separates code built, software verified,
physical verified, promoted and deployed. Update its backlog row at every
material transition. Do not mark the project done while any required
first-release client, phone handoff or inbox capability is still missing.

## 15. Platform references — verify availability during implementation

Use official documentation for API/platform behavior, checked again against
the actual deployment targets during B06–B08:

- [Apple MetricKit](https://developer.apple.com/documentation/metrickit):
  diagnostic payload availability is platform/version dependent.
- [Apple crash report acquisition](https://developer.apple.com/documentation/xcode/acquiring-crash-reports-and-diagnostic-logs):
  symbolicated crash evidence complements local breadcrumbs.
- [Android native symbols](https://developer.android.com/build/include-native-symbols):
  retain matching symbols for release diagnostics.
- [Android ApplicationExitInfo](https://developer.android.com/reference/android/app/ApplicationExitInfo):
  process exit facts have API and retention limits.
- [Web Share](https://developer.mozilla.org/en-US/docs/Web/API/Navigator/share):
  feature-detect and retain download fallback; do not assume TV availability.

## 16. Build decisions — recorded before dependent implementation

**2026-10-12 · operator passkey verification:** Use the stable established
`webauthn-rs` 0.5.5 server implementation, with attestation disabled. Its
passkey ceremony checks are reused instead of implementing WebAuthn crypto.
The existing dependency policy assumed that all native crypto meant TLS;
this library requires OpenSSL for credential verification and has MPL-2.0
licensed library files. Add narrowly named library-license exceptions and
allow OpenSSL only through the WebAuthn library wrappers in the independent
support binary. Native TLS remains forbidden; the daemon/client HTTP graph
continues to use rustls. Retain dependency notices and verify the resolved
graph and license policy before final merge. This is an explicit dependency
policy decision, not an incidental transitive relaxation. References:
[passkey API](https://docs.rs/webauthn-rs/0.5.5/webauthn_rs/),
[wrapper policy](https://embarkstudios.github.io/cargo-deny/checks/bans/cfg.html).

**2026-10-12 · support durable storage:** Store encrypted chunks, immutable
validated report blobs and encrypted sensitive metadata inside SQLite's
transactional blob storage. `synchronous=FULL`, WAL, byte reservations,
secure deletion and bounded reclamation provide one publication transaction.
This removes a filesystem/blob-metadata split where a receipt could outlive
its blob. Keep encryption keys and the rollback/deletion fence outside the
database. Backups still need deletion-journal replay and generation fencing.
No Cinema operational data enters Raft.
