# Curator login — optional household delegation and qualification

**Status:** Astra findings fixed; static recheck and regression execution pending ·
**Updated:** 2026-11-13 · **Owner:** Codex/root ·
**Branch:** `codex/curator-delegation` · **Current-main integration:** `483d2bcf3`.

## At a glance

<!-- agent head: read this section first; keep evidence and remaining work distinct. -->

- **What this is:** An optional, explicit application login connection. Cinema
  and Curator retain independent local accounts, credentials and permissions.
- **Where it stands:** Store, HTTP and web consent/registration code exists.
  Pinned 1.97.1 compile checks pass on the integrated source and authored core
  fixtures. Astra requested changes on `6f12ce9d`; the fixes and new fixtures
  await a targeted static recheck. Regression suites, PR/CI qualification and merge
  remain pending. No deployment or real-account linking has been performed.
- **Read further only for:** the authority contract, endpoint/setup workflow,
  storage/migration boundaries and the evidence ledger below. The canonical
  route reference is [API.md](../API.md#optional-curator-household-login).

## Account ownership and authority

Cinema owns its native user directory and login sessions. Curator owns permanent
local accounts, owner/member permissions and request history. An explicit link
uses Cinema's stable instance UUID and nonrecycled user wire UUID. Names and
emails never merge accounts. Reusing the wire identity adapter does not require
turning on the Jellyfin HTTP facade.

A delegated grant carries only `household-requests` scope, issuer, permanent
subject, registered audience, display name and expiry. It never carries a Cinema
bearer, password hash, administrator flag or watch history. Curator intersects
that scope with local membership. A linked local owner still receives only
household access through Cinema. Local owner work requires local sign-in.

The native source login remains authoritative. Consent, code redemption and
introspection check its current existence, live user incarnation and current
idle-expiry policy. Grant introspection never refreshes native login activity.
Logout/device revocation, password reset and user deletion invalidate derived
grants. Client disconnect advances its generation and invalidates its codes and
grants. Cinema outages fail closed for integration sessions; Curator's local
accounts remain usable. Unlink in Curator preserves local account/history,
revokes its integration sessions locally and durably retries remote revocation.

## Register and consent

In Cinema, a local administrator opens **Settings → Integrations → Manage
optional Curator sign-in connections**. Register the exact HTTPS Curator callback
ending `/api/v1/auth/cinema/callback`. The new client secret is shown once; store
it in Curator's **Access → Cinema sign-in connection**, separately from existing
calendar, scan-notification and watch-state integration credentials. Both
applications' new delegation routes must be deployed before linking can work.
This change does not deploy them or configure a real household connection.

A person signs in locally to Curator and requests linking after a local password
proof. Curator binds state and S256 to that local session/account and the expected
Cinema issuer. Cinema's own origin presents consent and identifies its signed-in
user. Its native bearer stays there. A 120-second single-use code returns to the
registered callback. Confidential client proof, exact redirect and S256 verifier
are required to exchange it for an opaque grant.

An unlinked Cinema sign-in returns to local sign-in/invitation enrollment; it
never creates or silently selects a Curator account. Native Curator uses the
system browser and a second one-use S256 handoff before receiving a device-scoped
Curator session. Browser callback state does not contain a bearer or owner key.

## Storage and migration

SQLite migration **132** installs the independently recognized extension. The
same schema is recognized/installed on Hiqlite's fresh/current-schema paths;
partial or incompatible shapes are refused. Clients, codes and grants live in
`delegation_*` tables. Grants never enter `tokens`, so older native-token lookups
cannot accept them. Code claims and grant creation use the shared Backend
transaction primitive on both stores, with a unique claim nonce rather than a
matching timestamp. Expiry cleanup is bounded during consent minting.

The standalone-to-replicated import inventory preserves these records starting
at schema 132. Source-native tokens and stable user identities remain their
revocation/incarnation authorities. Client/grant secrets are stored only as
SHA-256 digests in Cinema; the plaintext client secret is returned at registration
and the plaintext grant only at successful redemption. These responses are
`no-store` and `no-referrer`. Curator stores its backend connection/grants in its
private database and returns only its own per-device sessions to native clients.

## Review and regression ledger

Astra reviewed the design, standalone-account clarification and the exact-scope
request-admission contract during development. The final implementation review
is still pending. No new delegation regression has run before that final gate.
Compile checks are not execution evidence.

Authored core regressions run the same behavioral scenarios through the existing
standalone/replicated contract harness: one-use redemption races, wrong proof and
client secret, audience isolation, unchanged native activity, source revocation,
client disconnect and deleted/recreated names. Unit fixtures cover idle expiry
and partial extension shape. Real-router fixtures exercise exact HTTPS callbacks,
confidential proof, S256, replay and source logout. Curator independently authors
web/native account, request and outage/unlink regressions and isolated desktop/
390px acceptance flows.

Before merge: complete final Astra review and fixes, run pinned compiler/Clippy/
format checks and focused regressions (including `hiqlite-store` and the replicated
contract feature), run paperwork and the ordinary main-lane checks on the actual
candidate. Record PR, exact checks, merge and owned-storage finish receipt here.
No live deployment, mobile signing or physical-device acceptance is claimed.

## Final review correction ledger

The reviewed fixes mount the shipped layout on cold consent/management routes,
require the exact Curator callback path, enforce an atomic 100-active-client
limit matching the complete disconnect inventory, order replicated expiry
cleanup by expiry and digest, and recheck code expiry after queued commit and
post-commit source validation before returning a grant. SQLite v132 preflights
existing objects and verifies final shape inside the migration transaction.
Historical v43/v14 helpers first validate and remove only an exact empty v132
extension; registrations, codes, grants, altered objects or missing markers
refuse without mutation. The census includes v132.

New core/backend, HTTP and actual shipped-shell Chromium fixtures were authored.
Only pinned compile checks have run; static fix recheck precedes all runtime
regressions. Curator separately requires app-verifier plus consenting-device
completion proof, retaining standalone local login during upstream outages.
