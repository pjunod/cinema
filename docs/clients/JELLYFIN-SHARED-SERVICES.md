# Jellyfin service seams — share native policy before mounting routes

**Status:** open · J1 shared-service task implemented; native focused regressions pass ·
**Written:** 2026-10-02.

Companion to [the build contract](JELLYFIN-COMPATIBILITY-BUILD.md). This task
extracts callable operations from native HTTP handlers. It adds no Jellyfin
routes. It now includes transactional compatibility token replacement; play
binding and watch revision fences remain open. Physical Apple TV work remains deferred to final qualification under
Paul's instruction; implementation continues without waiting for that device.

## 1. Authentication — parsing and authority remain separate

`auth::verify_login_password` shares native password-size/device-label validation, trusted
proxy address selection, per-node login throttling, bounded password work,
uniform unknown-user verification and a generation-bracketed password read.
`auth::login_user` keeps native password-match CAS minting and cache-proof
bookkeeping. The native handler converts the returned native user
to its existing response DTO. A compatibility handler must supply the actual
connection peer and apply its own bounded request body before calling the login service.

`extract::authenticate_user_token` shares native expiry/revocation decisions,
typed idle-expiry refusals and proof-generation bookkeeping. Native `AuthUser`
still parses its existing carriers and delegates authority to this function.
The compatibility parser will supply its validated user token independently;
client/device labels and successful wire parsing grant no authority.

`auth::revoke_token_under_exclusion` commits deletion under the exact cluster
cache-revocation claim and finishes the proof invalidation. Its caller must
first authenticate the presented token and acquire that digest's exclusion.
Native logout keeps revoking all user file grants inside that exclusion before
calling the primitive. Compatibility logout can end its own bound plays and
call the token-only primitive without revoking other devices' file grants.
No public compatibility logout route exists in this task.

## 2. Watch operations — retain every native side effect

`watch::apply_progress` keeps item validation, negative-position clamping,
durable previous-state reads, offline timestamp ordering and live coalescing.
It retains direct-play presence, start-attempt activity, watched-time telemetry,
Trakt progress and watched notification on the completion crossing. Protocol
adapters must not replace it with a bare coalescer call.

`watch::apply_watched` retains cascading manual watched/unwatched marks,
notifications for episodes that actually become watched, and Trakt sync for
both directions. Native handlers return their existing JSON forms. Compatibility
binding revisions, terminal durability and queued pre-edit beats are additional
ingress/storage work; these shared functions do not claim those fences exist.

## 3. Focused proof — preserve scope and existing HTTP behavior

The new token-scope regression calls shared login and authority directly, then
revokes one token under the production exclusion primitive. The other device
stays authenticated and its reader grant remains active and unrevoked; the
revoked token's own grant loses source authority. Calling the native logout
route afterward still revokes user file grants under its existing scope.

Existing focused regressions cover setup/login and body/device bounds, typed
idle expiry, concurrent logout, cache-proof generation order, dated/coalesced
progress, direct-play activity, watched-time telemetry and cascading marks.
The new scope regression and ten affected native regressions pass on pinned
Rust 1.97.1. The service task compiles for all daemon targets and its four
document-index checks pass. Workspace denied-lint checks remain required
before review. Keep play-binding lifecycle and cross-node manual-watch
fencing open until their own receipts exist.

## 4. Compatibility login — replacement without intermediate native tokens

`auth::login_jellyfin_user` verifies the password through the same admission,
proxy and throttle policy, then acquires the existing user cache-proof
exclusion. Device IDs are bounded to 256 bytes and retained only as SHA-256
digests. The closed client family and authenticated native user select the
replacement scope; a human device label never selects authority.

The Store's `replace_jellyfin_login` mints the new token under the verified
password CAS, retires the previous compatibility token in that exact scope,
and publishes the new mapping in one transaction. Native tokens, another
family, another device and another user's tokens survive. A collision or
failed transaction rolls everything back. On replicated storage every mutation
also checks the exact committed cache-revocation claim, so cancellation and
claim cleanup cannot admit a delayed replacement. The service completes the
exclusion before recording a fresh authentication proof; it never reuses its
pre-exclusion ticket.

SQLite migration 93 and replicated migration 71 add the digest-only scope
mapping with cascading token/user deletion. The SQLite import plan preserves
this mapping after its token rows, allowing replacement from another node.
The backend regressions prove simultaneous replacements converge to one
winner, stale passwords change no authority, collisions preserve the old
login, unrelated reader grants remain active and absent/cleaned exact cluster
claims refuse the mutation. Those two backend regressions pass on SQLite and
three voters. The daemon service and alternate-node import regressions pass on the combined
task tree. The 15 SQL placeholder checks, transaction/import censuses, fresh
versus frozen-v42 migration parity, five affected daemon regressions, seven
document/source guards and mandatory workspace lint checks also pass.

No public compatibility route is mounted yet. Replaced-play retirement still
requires the binding lifecycle work; token replacement alone is not evidence
that those native playback resources have been released.
