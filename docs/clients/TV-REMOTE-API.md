# Cinema remote HTTP API

**Status:** open — B04 implementation reference; client and physical qualification remain pending.
**Audience:** receiver and companion implementers.

This is the exact server contract for the node-owned v1 relay. Read the
[protocol](TV-REMOTE-PROTOCOL.md) for receiver-local admission and the
[implementation plan](TV-REMOTE-AND-COMPANION-IMPLEMENTATION.md) for ownership.

## Wire contract


Version: cinema.remote.v1. Prefix /api/remote/v1. Every POST request has
`version: "cinema.remote.v1"`; all fields are required unless marked optional.
Unknown fields/types/versions are rejected. JSON request cap 16 KiB; presence state and returned state/list cap 64 KiB. Responses carry version and no-store.
Bearer is exactly one Authorization: Bearer <human native login token> header.
Query parameters, X-Api-Key, machine credentials and duplicate proof headers
are rejected. Proofs are headers only; never in returned state.

Target = {owner_node_id:string, session_id:UUID, receiver_epoch:UUID}.

POST /receivers
  request: {version,name:string<=80 UTF8 bytes,platform:"web"|"apple_tv"|"android_tv"|"desktop"}
  response: {version,receiver_id:UUID,receiver_secret:base64url32bytes}
DELETE /receivers/{receiver_id}
  human bearer only; response {version,revoked:true}; idempotent owned cleanup.
  Revokes installation and all grants, cancels live sessions. Frees active cap20.
GET /receivers
  response: {version,receivers:[{receiver_id,name,platform,target:Target|null,
    available:bool,busy:bool,paired:bool}],unavailable_nodes:[node_id]}
  Same-user installations including offline entries for cleanup. No media/focus.
POST /sessions (X-Cinema-Receiver-Secret)
  request: {version,receiver_id,foreground_id:UUID}; response {version,target}
  foreground_id is client metadata, stable across reconnects in one actual
  app foreground lifetime; new only after background-to-foreground, not auth.
POST /presence (X-Cinema-Receiver-Secret)
  request: {version,target,state:ReceiverState}; response {version,accepted:true}
POST /poll (X-Cinema-Receiver-Secret)
  request: {version,target,after_delivery_id:safe-int,after_response_revision:safe-int,wait_ms:uint<=20000}
  response: {version,target,response_revision:positive-safe-int,delivery_id:safe-int,control:Control|null,commands:[Command],pairings:[PendingPairing]}
  One bounded last batch is retained until after_delivery_id acknowledges its
  receipt. Retrying the prior cursor can repeat that batch after authoritative
  token/grant checks; the receiver sequence guard applies once. Entries expire
  after 500 ms even in retained batches. Never retry an action with a new sequence.
  Receiver polls must be serial; control/pairing changes wake an empty poll.
  Presence is separately refreshed every 5 s idle/every 250 ms while controlling.
POST /ack (X-Cinema-Receiver-Secret)
  request: {version,target,outcomes:[{control_epoch,sequence,outcome}]<=64}
  response: {version,accepted:true}
POST /state (X-Cinema-Grant-Secret)
  request: {version,target,grant_id:UUID,after_revision:uint,wait_ms:uint<=20000}
  response: {version,target,response_revision:positive-safe-int,control:Control|null,state:ReceiverState|null,
    outcomes:[{control_epoch,sequence,outcome}]}; state=null means unchanged.
POST /control (X-Cinema-Grant-Secret)
  request: {version,target,grant_id,action:"acquire"|"renew"|"takeover"|"release",control_epoch:UUID|null}
  response: {version,target,response_revision:positive-safe-int,control:Control|null}
  Renew/release require matching epoch. Same-grant acquire/renew extends 15 s
  idle lease with unchanged epoch. Active phone renews at most every 5 s. Different grant needs explicit takeover.
  Control = {control_epoch:UUID,active_grant_id:UUID,controller_name:string<=80}
POST /commands (X-Cinema-Grant-Secret)
  request: exact B01 Command; response 202 {version,queued:true,control_epoch,sequence}
  UI outcomes come from ack/state; 202 never means applied.

POST /pairing/start (X-Cinema-Receiver-Secret)
  request: {version,target}
  response: {version,target,challenge_id:UUID,code:8-digit-string,expires_in_ms:120000}
POST /pairing/claim (bearer only)
  request: {version,target,challenge_id:UUID|null(optional),code:8-digit-string,controller_name:string<=80}
  response: {version,pending_id:UUID,poll_secret:base64url32bytes}
POST /pairing/approve (X-Cinema-Receiver-Secret)
  request: {version,target,pending_id,approve:bool}; response {version,accepted:true}
  Receiver local UI only; network semantic registry must never activate approval.
POST /pairing/result (bearer + X-Cinema-Pairing-Secret header)
  request: {version,target,pending_id}
  response: {version,status:"pending"|"denied"|"approved",grant_id:UUID|null,
    grant_secret:base64url32bytes|null,receiver_id:UUID|null}
  Approved secret is consumed once; result loss requires pairing again.
GET /grants
  response: {version,grants:[{id:UUID,receiver_id:UUID,name:string,created_at:int}]}
DELETE /grants/{id}
  human bearer only; response {version,revoked:true}; idempotent owned revocation.

PendingPairing = {pending_id:UUID,controller_name:string}; exposed only to receiver.
ReceiverState = {state_revision:positive-safe-int,context_revision:positive-safe-int,
  focus_revision:positive-safe-int,route:"home"|"library"|"details"|"search"|
  "playback"|"tracks"|"restricted",capabilities:[B01 action type names]<=12,
  focused_label:string<=256|null(optional),credits:[{nonce:UUID,kind:"interaction"|"playback"}]<=16,
  text_nonce:UUID|null(optional),playback:PlaybackSummary|null(optional)}
PlaybackSummary = {media:MediaIdentity,title:string<=256,playing:bool,
  position_ms:safe-int,duration_ms:safe-int,tracks:[{kind:B01 TrackKind,
  option_id:string<=128,label:string<=256}]<=64}
MediaIdentity = {type:"item",item_id:positive-safe-int} OR
  {type:"live_channel",channel_id:nonempty-string<=128UTF8bytes}. Strict tagged
  union: unknown/extra/mixed fields reject. Zero duration means unknown for
  Live TV; seek support remains capabilities-based. No play-channel command.
Restricted state is sanitized by server: capabilities/credits empty, label/text/
playback null. Only safe device/route/blocked information remains available.

All owner-bound requests route by Target.owner_node_id; no queue migration.
Signed internal POST /internal/remote/v1/dispatch binds an explicit operation,
native login token digest, expected user_id, and hashed receiver/grant proof.
Owner reauthenticates token digest and joins current receiver/grant authority.
Queued entries retain controller token digest; delivery rechecks it plus grant,
receiver and currently active control. Receiver's own session login is bound
and also rechecked. Account/logout/server changes need a new receiver session.

Owner response_revision advances on credit publication, control/revocation/expiry
and pairing changes. Clients ignore lower response_revision for the same target;
use a local request generation to discard responses across identity/server changes.
Lease expiry/revocation wakes receiver/controller polls with explicit null control.
Live TV uses route=playback. paired means the account has an active grant; only
a phone possessing its own local grant secret can offer/use that grant.

Errors use {version,code,message};401 unauthorized;403 unauthorized;400 invalid;
404 unavailable;409 busy/stale_control/stale_target;429 busy;503 unavailable.
Unavailable peer discovery is separately listed, never an empty authoritative
answer. Feature disabled returns503 unavailable and invalidates owner sessions.



Manual pairing requires only the selected target and eight-digit TV code.
challenge_id can be omitted/null; QR supplies it and requires exact match.

Conditional takeover: action=takeover with control_epoch=expected rotates only if the current live lease has exactly that epoch; otherwise 409 stale_control. Null is explicit unconditional takeover.
Discovery: multiple simultaneous owner summaries for one installation return target:null, available:false until a unique target remains (old foreground sessions expire within 15 s). No arbitrary owner selection.
Unsent queue expiry/authorization/control failures publish bounded expired/unauthorized/stale_control acknowledgements. Already delivered commands without receiver acknowledgement have unknown outcome; timeout never proves failure.

## Authority and storage

Receiver/grant secrets are random 32-byte base64url values disclosed once. Only
SHA-256 digests are durable. Authorization reads use consistent replicated
queries; commands, credits, control and queues stay in owner memory. A signed
peer identifies the transport, while the original human token digest and grant
are revalidated at the owner and again before queued delivery. Learners can
relay these exact routes without claiming ownership or leader authority.

SQLite migration 106 installs Remote tables. Replicated storage installs an
independently versioned Remote adjunct under the existing startup migration
admission, preserving frozen cluster schema 81/82. One transaction creates the
exact tables/index and remote_schema singleton marker (version 1). Consistent
shape and marker verification settles repeat or uncertain commits; partial or
incompatible shapes fail startup. Maintenance open and serving requests never
repair schema. Older peers without the adjunct are unavailable for Remote
routing rather than inferred ready from their cluster version.

Offline snapshot restore revokes every restored receiver and grant and clears
claim budgets before the restored database serves requests. SQLite-to-Hiqlite
import deliberately excludes Remote credential tables; target Remote tables
must be empty. Older snapshots/imports therefore never reactivate old proofs.
Users pair again after restore/import.

## Limits and lifecycle

The account installation limit is 20 active receivers, with 8 active grants per
receiver. Each owner holds at most 100 sessions (20 per account), 32 queued or
retained commands per session, 8 pairing claims per session, and 16 concurrent
approval finalizers. Pair claims consume a durable account-wide 10-per-minute
budget; each 120 s TV challenge permits 5 failed code attempts.

Presence expires after 15 s. Control idle lease expires after 15 s; polls wake with
null control when it expires or loses authority. One receiver poll per session
and one state poll per grant/session are admitted at a time; concurrent polls
return 429 busy and cancellation releases the permit. Outcomes retain at most 64
entries for 10 s. Poll timeouts are capped 20 s and authority is rechecked at least
every second while waiting.

Turning the Developer switch off clears all local sessions. The switch and Save
remain usable irrespective of advisory client qualification. Network cleanup
must not delay receiver-local pause or change platform fullscreen/PiP ownership.

## Verification

Focused daemon regressions cover header-only authentication, manual pairing,
concurrent approval/result, canceled approval cleanup, queue admission during
delivery authorization, logout/revocation, restricted state and cleanup,
duplicate-owner discovery and conditional takeover. Backend contracts cover
proof types, caps, claim budgets, adjunct admission/version/shape and restore
fencing. Release readiness still requires composed client and physical-device
qualification; this reference does not claim it.

Receiver presence metadata focused_label, text_nonce and playback may be omitted
or null. Responses emit null for absent values. Control.control_epoch is
required even when its value is null.
