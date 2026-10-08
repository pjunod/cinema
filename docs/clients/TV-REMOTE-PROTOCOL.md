# Cinema remote protocol — authority, freshness, and receiver ownership

**Status:** build contract, not a shipping API · **Written:** 2026-10-07 ·
**Source base:** `8e242787c` · **Version:** `cinema.remote.v1`.

Read [the implementation handoff](TV-REMOTE-AND-COMPANION-IMPLEMENTATION.md)
for task ownership and acceptance. This document supersedes the proposed
cross-account, leader-relay, WebSocket, and clock-deadline choices in
[the original proposal](TV-REMOTE-AND-COMPANION-PLAN.md). New names below are
interfaces to implement, not claims about endpoints that already exist.

## 1. Authority — same account, explicit approval, semantic actions

V1 pairs only installations authenticated as the **same user on the same
server instance**. Existing `User` records have no profile/library ACL model;
do not invent a permissions intersection which the server cannot enforce.
Use human bearer authentication in the `Authorization: Bearer` header only.
Reject the general extractor's alternate query and `X-Api-Key` paths.
Machine API keys, URL tokens,
mDNS, a device name, an installation UUID, and shared Wi-Fi confer no control.

A receiver installation has a random UUID and a random 256-bit secret. A
controller grant has its own UUID and random 256-bit secret, bound to a user
and receiver installation. Store SHA-256 hashes, never plaintext secrets.
Return a newly generated secret once in a no-store response. Clients keep
secrets in Keychain/Keystore; web uses the existing origin-scoped credential
storage policy. This is not protection against a compromised same-origin
script. Logout/server switch cancels sessions, clears local secrets, and
requires explicit registration/pairing again. Never silently reclaim a device
UUID without possession of its secret.

Both bearer and installation/grant proof are required. A same-account grant
still cannot administer Cinema through a TV logged in as an administrator.
Receivers expose typed actions for browsing, authorized media playback, and
player controls only. Settings, user management, server/cluster actions,
sharing changes, delete/edit/download actions, credentials, external links,
and OS control are excluded. A physical local user may open such a route;
remote commands then return `restricted_surface` and publish no sensitive
state. Return only the route category, blocked status, and safe device name.
A paired phone cannot approve another pairing or revoke grants via Select.

There is no generic key injection, DOM selector command, arbitrary URL,
JavaScript evaluation, accessibility automation, or blind `element.click()`.
Register an explicit semantic action for every remotely reachable control.
Validate that action again at activation, including when physical input has
changed the focused control or opened a modal.

## 2. Cluster ownership — one queue on the receiver's chosen node

```text
phone -> ingress node -> signed bounded HTTP -> receiver owner actor
                                                  |
                                          receiver long poll
                                                  |
                                             app UI owner
TV CEC -> native host -> extension -> bound Cinema tab (local only)
```

Use `(owner_node_id, session_id, receiver_epoch)` as the complete target.
Session and epoch are random UUIDs. Each foreground receiver holds exactly
one active tuple. Cancel the old channel and invalidate its credits before
activating a replacement. Duplicate tabs are separate receivers; never
broadcast a command to every tab. An owner restart loses ephemeral sessions;
a reconnect creates a new tuple. **No queue migration or automatic takeover.**

Resolve owner IDs through `MembershipManager::operations_peers()` and use
`PeerTransport::request` with
`PeerAuthMode::ExactRequestAndMemberResponse`. Never accept an HTTP base URL
from a phone. Sign exact requests and replies using the existing internal
peer helpers. Authenticated peer transport proves the peer, not the user:
the owner rechecks the current bearer/session validity, user, receiver, and
grant in authoritative storage before delivery. Preserve the request token
digest for this check; a cached user ID cannot survive token revocation. Revocation prevents subsequent admitted delivery;
it cannot undo an action already applied to a screen.

`may_run_cluster_jobs()` permits multiple committed voters. It is NOT a
leader guard. Do not use it for this directory. Use local serving and
maintenance admission, including explicit learner-route classification.
Discovery aggregates bounded node-local summaries, with a single two-second
fan-out deadline and at most eight concurrent peer requests. An unavailable
node is unavailable, not an empty authoritative answer. Listing a stale
summary never grants authority or transfers ownership.

Use ordinary bounded JSON HTTP rather than adding a WebSocket proxy. The
existing signed peer transport supports that path. Long polls hold no store
transaction or actor mutex across an await. Idle receiver heartbeat is five
seconds; presence expires after fifteen seconds of owner-local monotonic
silence. Poll wait is at most twenty seconds. Retry with jitter from one to
thirty seconds; changing servers or auth cancels retries immediately.

## 3. Wire envelope — explicit revisions and receiver-issued credits

All bodies are UTF-8 JSON, max 16 KiB except discovery/state responses at
64 KiB. Unknown protocol versions/actions and extra action fields are
rejected; no permissive fallback into keyboard events. Integers are unsigned
and limited to `Number.MAX_SAFE_INTEGER` for identical JavaScript semantics.
Sequence and initialized context/focus/state revisions are positive; zero is
reserved for uninitialized local state, never an active command. IDs/nonces
use UUID strings, except secrets which use unpadded base64url.

```json
{
  "version": "cinema.remote.v1",
  "target": {
    "owner_node_id": "node-identity-from-server",
    "session_id": "80a14912-80c4-43d6-b626-8c50e43c41ae",
    "receiver_epoch": "2242c9bb-8315-4948-85b5-74341b265bac"
  },
  "grant_id": "1ef35d51-11f3-455e-bbe5-57318cb14b63",
  "control_epoch": "50f26cd2-7cb2-422d-9bbe-317f34724041",
  "sequence": 1,
  "credit": "9a2a81a3-68b7-44d2-811c-dfa9990a2940",
  "context_revision": 4,
  "focus_revision": 7,
  "action": { "type": "navigate", "direction": "down" }
}
```

Bearer, `X-Cinema-Receiver-Secret`, and `X-Cinema-Grant-Secret` are headers,
not fields copied to state or logs. CSRF defenses and origin checks follow
the native API policy; state-changing GETs are forbidden. Responses are
`Cache-Control: no-store`; diagnostics contain only redacted IDs and reasons.

**Freshness belongs to the receiver.** The receiver mints random credits and
retains their deadlines using its own monotonic clock. Navigation, Select,
Back, Home, and text credits live 1,000 ms; playback credits live 3,000 ms.
Each advertised credit is `{ "nonce": UUID, "kind": "interaction" }` or
`{ "nonce": UUID, "kind": "playback" }`; the envelope echoes the nonce.
`interaction` applies to navigate/select/back/home/text_replace/open_tracks.
`playback` applies to set_playing/seek_relative/seek_absolute/stop/choose_track/
play_item. A credit cannot authorize another class. State exposes the class,
never the receiver's monotonic timestamp. Mint every 250 ms while an active
controller is connected. Keep a ring of at
most sixteen credits; replacing a context, control epoch, or foreground
session clears it. No wall-clock timestamp crosses the wire for expiry.
An actor also expires queued work after 500 ms of actor-local time. That is
an additional bound, not a replacement for the final receiver check.

Before applying a command, on the UI owner: validate protocol, active target,
active grant and control epoch, credit identity and local deadline, sequence,
context/focus, allowed semantic action and parameters. Then atomically consume sequence on the UI owner before invoking its effect.
Rejected commands do not consume sequence. This admission is synchronous;
never yield between semantic authorization and consumption/application. A command arriving after a stall does not get a fresh TTL.
At the deadline it is expired (`now >= deadline`). Clock offsets across
phone/server/TV are irrelevant. UI stall time counts through a monotonic
clock which continues advancing; resume/background invalidates all credits.
If a supplied local clock moves backward, invalidate and reject, never extend
old credits. Monotonic clock values are local guard inputs, not wire fields.

`context_revision` changes on route, modal, playback-item, text-context, or
permission changes. `focus_revision` changes when the focused semantic item
changes. Select requires both; directional moves require context but may
advance focus themselves. Text requires an additional current text nonce.
Playback commands require the current playback context, never a previous
movie's duration or position. Physical input cancels remote holds, pending
scrubs and credits before its own action, then publishes fresh state.

One controller holds a fifteen-second idle control lease. Acquire through
an explicit Use as remote tap. A second controller sees who is controlling
and must explicitly Take over; takeover creates a new random control epoch
and clears queues, credits, dedup and pending gestures. Physical input always
works and does not need this network lease. Revocation or lease loss clears
unapplied commands. Sequence begins at 1 per control epoch; duplicate/lower
sequences never apply again. Bounded results are keyed by `(control_epoch, sequence)` and retain the
last 64 entries for ten seconds; after eviction return `duplicate_or_old`, never execute to recover
a missing acknowledgement. Clients do not retry non-idempotent commands
with a new sequence number.

## 4. Action and state vocabulary

| Action | Fields | Application rule |
|---|---|---|
| `navigate` | `direction`: up/down/left/right | One move in the active semantic scope; no route escape |
| `select` | none | Activate current registered item only with matching focus revision |
| `back` | none | Dismiss one owned modal, otherwise pop one Cinema route |
| `home` | none | Cinema Home, never OS Home |
| `set_playing` | `playing`: bool | Desired state through existing playback owner; not toggle |
| `seek_relative` | `seconds`: -30/-10/10/30 | Existing policy, clamp to authorized seek range |
| `seek_absolute` | `position_ms`: safe nonnegative integer | Current item/range only, one committed scrub |
| `stop` | none | Pause immediately through the playback owner; preserve fullscreen/PiP teardown ordering without waiting for server cleanup |
| `open_tracks` | `kind`: audio/subtitles/quality | App-owned scoped menu |
| `choose_track` | `kind`, `option_id`: max 128 UTF-8 bytes | ID must be in current authorized menu options |
| `text_replace` | `text_nonce`, `text`: max 512 UTF-8 bytes | Search only in v1; reject password/login/settings fields |
| `play_item` | `item_id`: positive safe integer | Receiver requests normal authorized item playback |

Volumes, power, HDMI input switching and OS wake are not v1 actions. TV/AVR
volume remains the TV remote's job. An unsupported action returns
`unsupported`, never a guessed keycode. Desktop keyboard mappings remain
as documented in [PLAYER-INPUT-CONTRACT.md](PLAYER-INPUT-CONTRACT.md).

State includes target, safe receiver name (80 UTF-8 bytes), platform,
capabilities, available/busy status, active grant ID, control epoch, state revision,
context/focus revisions, safe focused label, current credits, optional search
nonce, and optional authorized playback summary. Never include filesystem
paths, playable stream URLs, bearer tokens, pairing code, passwords, or an
administrator screen's labels. Apply state monotonically per target/epoch;
ignore older replies and erase state on identity change. Cap a state at 64
KiB and labels at 256 UTF-8 bytes. Do not publish a full DOM or library dump.

Acknowledgements name sequence and one of `applied`, `duplicate_or_old`,
`expired`, `stale_target`, `stale_control`, `stale_context`, `stale_focus`,
`restricted_surface`, `unauthorized`, `unsupported`, `busy`, `unavailable`,
`invalid`. Acknowledgement means the UI accepted/applied the action, not that
a media load has finished. Media outcomes arrive through ordinary state.
Enqueue response is HTTP 202 `queued`, never `applied`.

**Holds:** the phone emits bounded one-step navigation at no more than 8 Hz,
after a 350 ms initial repeat delay. No server-side endless key-down state.
Pointer-up/cancel, blur, lease/context change and physical input stop repeats.
The native CEC adapter translates callback press/release similarly and clears
held state after 750 ms silence, unplug, focus loss, or reconnect. A stuck
key cannot continue after disconnection.

## 5. Endpoint contract and persistent records

Prefix `/api/remote/v1`. Public requests use normal human auth plus the proof
required in the table. Owner-specific paths carry `owner_node_id` in the
JSON target; ingress routes internally to `/internal/remote/v1/dispatch`.
Internal dispatch is an explicit tagged request enum, not an arbitrary proxy.

| Method/path | Request/result | Additional proof |
|---|---|---|
| POST `/receivers` | name/platform -> receiver_id + secret | Fresh human login; capped 20 active installations per user |
| DELETE `/receivers/{id}` | Revoke own installation and its grants, cancel its sessions; idempotent | Human auth; not a remote semantic action |
| POST `/sessions` | receiver_id -> target | Receiver secret |
| POST `/presence` | target + bounded state -> accepted | Receiver secret |
| POST `/poll` | target + last delivery ID -> commands or empty | Receiver secret; max 20 s |
| POST `/ack` | target + command outcomes -> accepted | Receiver secret |
| GET `/receivers` | Safe same-user available device list | No media state before grant |
| POST `/pairing/start` | target -> code + challenge_id + expires_in_ms | Receiver secret; 120 s challenge |
| POST `/pairing/claim` | target + code + controller_name -> pending_id | Human same-user login |
| POST `/pairing/approve` | target + pending_id + approve -> result | Receiver secret; local physical UI approval only |
| POST `/pairing/result` | pending_id + poll secret -> pending/grant | Original claimant proof; grant secret returned once |
| GET `/grants` | Own grants, names, created time | Human auth |
| DELETE `/grants/{id}` | Revoke own grant; idempotent | Human auth; cannot be remotely activated on receiver |
| POST `/control` | target + acquire/takeover/release -> control epoch | Grant secret |
| POST `/state` | target + after revision -> state/unchanged | Grant secret; max 20 s |
| POST `/commands` | Envelope from §3 -> 202 queued or rejection | Grant secret |

Use an eight-digit random pairing code, keyed hash in owner memory, 120-second
expiry, max five failed claims per challenge, and per-user throttling of ten
claims/minute across challenges. Never permit an unbounded code lookup scan.
Pairing screen QR contains server instance ID, target and challenge ID; put
any code in a URL fragment, remove it after parsing, and never auto-approve.
No long-lived secret in a QR. Every claim creates a fresh random poll secret,
held by the claimant; an account peer cannot collect another claimant's
returned grant. Consume approved results once and erase after sixty seconds.
If delivery is lost, pair again rather than disclose an old secret.

Proposed SQL (follow actual migration mechanisms, not a second database):

```sql
CREATE TABLE remote_receivers (
  id TEXT PRIMARY KEY,
  user_id INTEGER NOT NULL REFERENCES users(id),
  name TEXT NOT NULL,
  platform TEXT NOT NULL,
  secret_hash TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  revoked_at INTEGER
);
CREATE TABLE remote_grants (
  id TEXT PRIMARY KEY,
  receiver_id TEXT NOT NULL REFERENCES remote_receivers(id),
  user_id INTEGER NOT NULL REFERENCES users(id),
  controller_name TEXT NOT NULL,
  secret_hash TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  revoked_at INTEGER
);
CREATE INDEX remote_grants_receiver ON remote_grants(receiver_id);
```

Verify the actual users table identifier/type and timestamp convention in
both backends at implementation. Implement a typed `RemoteStore` composed
into `Store`, SQLite and Hiqlite. Authorization reads join live receiver,
grant and user with **consistent** reads; replica-local cached rows do not
authorize remote actions after revocation. Never use settings KV for grants.
Presence, credits, pairing attempts, command queues and control leases are
memory-only and bounded: 100 sessions/node, 20/user, 8 paired controllers per
receiver, 32 queued commands/session, 64 recent acknowledgements/session.
Reject capacity with 429/`busy`; do not evict an unrelated active controller.
One actor per owner processes ordering; no unbounded task per keypress.

## 6. Stable receiver adapter interfaces

Implement equivalent native types; do not build a second media player:

```text
RemoteNavigationCoordinator.snapshot() -> SafeNavigationState
RemoteNavigationCoordinator.dispatch(action, context) -> RemoteOutcome
RemotePlaybackAdapter.dispatch(action, playback_context) -> RemoteOutcome
RemoteReceiver.apply(envelope, monotonic_now) -> RemoteOutcome
RemoteReceiver.invalidate(reason) -> void
```

Web exposes one `CinemaRemote` global with `snapshot()`,
`dispatch(action, context)`, `invalidate(reason)`, `registerScope(...)` and
`registerAction(...)`. B02 owns semantic routing; B05 owns authenticated
`apply(envelope, now)` admission. B08 local CEC invalidates pending network
work first, then dispatches against the new snapshot/revisions so physical
input does not reject its own action. Concrete registration parameters are
owned by B02 and recorded in the same commit as the implementation. Source names are
routing metadata, never an authorization credential. Native code calls
existing UI/main-thread owners. Receive tasks parse and validate bounded data
before hopping to the UI; they never mutate player state on a network thread.

## 7. Required evidence before claiming this protocol works

Delay a navigation packet past its receiver credit: focus stays unchanged.
Replay Select: one activation. Change route or physical focus before Select:
reject. Switch account/server during a long poll: no old state appears.
Revoke during a queued command: subsequent admitted deliveries fail. Lose
owner and register elsewhere: old queue cannot apply to the new epoch.
Open admin modal locally: remote cannot activate or read it. Deny machine
API-key auth. Exhaust queue/pairing limits: bounded memory and 429 response.

These are named behavioral regressions for implementation tasks, not unit
tests to add or run for this documentation-only commit. Actual evidence and
remaining hardware limits belong in the implementation status ledger.
