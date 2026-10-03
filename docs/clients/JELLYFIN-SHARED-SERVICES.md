# Jellyfin service seams — share native policy before mounting routes

**Status:** open · J1 shared-service task implemented; native focused regressions pass ·
**Written:** 2026-10-02.

Companion to [the build contract](JELLYFIN-COMPATIBILITY-BUILD.md). This task
extracts callable operations from native HTTP handlers. It adds no Jellyfin
routes and does not complete token replacement, play binding or watch revision
fences. Physical Apple TV work remains deferred to final qualification under
Paul's instruction; implementation continues without waiting for that device.

## 1. Authentication — parsing and authority remain separate

`auth::login_user` owns native password-size/device-label validation, trusted
proxy address selection, per-node login throttling, bounded password work,
uniform unknown-user verification, password-match CAS token minting and
cache-proof bookkeeping. The native handler converts the returned native user
to its existing response DTO. A compatibility handler must supply the actual
connection peer and apply its own bounded request body before calling it.

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
before review. Keep repeated compatibility login, play-binding lifecycle and cross-node
manual-watch fencing open until their own receipts exist.
