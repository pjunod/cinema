# Shared libraries — the Tailscale build contract for Opus and Sol

**Status:** implementation in progress; S1 integrated; S2 topology
qualification pending; S3–S7 implemented in part with completion work active;
S8 live validation open · **Revised:** 2026-10-06 ·
**Current status:** [build and acceptance ledger](SHARED-LIBRARIES-STATUS.md) ·
**Source rechecked:** `d4d2ec8e4` ·
**Executes:** the Cinema-to-Cinema and private-Tailscale decisions in
[SHARED-LIBRARIES-DESIGN.md](SHARED-LIBRARIES-DESIGN.md) ·
**Implementation lane:** `effort/shared-libraries`, created from current main
`9a719fcb73da195676a6dea5378871d64547bb8b`; isolated implementation checkout.

Read the decision record first. This document is the proposed implementation
contract: authority, state, wire shapes, code seams, work packages, and the
observations that make the feature complete. Opus reviews it before Sol
builds; record findings and their dispositions here, then change the status.
The [original Opus review](SHARED-LIBRARIES-REVIEW.md) records the rejected
first draft; the [re-review](SHARED-LIBRARIES-RE-REVIEW.md) accepts the prior
dispositions subject to its corrections. §16 owns current dispositions. No runtime qualification or
deployment is implied by this file. Existing symbols below were inspected; all new symbols, paths, tables,
and routes are proposed. Re-verify the seams against the implementation base.

**Standing instruction:** preserve local playback semantics. Do not turn a
sharing peer into a cluster member, publish Cinema on the internet, make a
hidden user account to impersonate a remote viewer, forward account tokens,
or bypass the repository's compiler and merge gates. Escalate a required
change to these boundaries as a design finding rather than silently making it.

## 1. Product contract — one household watches another's selected libraries

### 1.1 Confirmed decisions and implementation defaults

| Topic | Contract | Authority |
|---|---|---|
| Scope | Independent Cinema servers share libraries; either may internally be a cluster | Paul |
| Transport | Tailscale; no router port forwarding or public Cinema endpoint | Paul |
| Sequencing | Plex library compatibility later; Watch Together is a separate feature | Paul |
| Delivery | Opus reviews this document; Sol implements the reviewed contract | Paul |
| Initial media | Movies and shows, including anime | Proposed v1 default |
| Local audience | B's administrator assigns individual local users; initially nobody | Proposed v1 default |
| Trust | A grants B access and trusts B's administrator to enforce local assignments | Proposed v1 default |
| Playback | A serves/transcodes; B streams the response to its own player | Proposed v1 default |
| Progress | Per-user remote history resides on B; A's history is untouched | Proposed v1 default |
| Discovery | Explicit Tailscale machine share and Cinema invitation, no global directory | Proposed v1 default |

A is the exporting server; B is the importing server. A grant is directional.
Reciprocal sharing creates a second grant. Accounts with identical names on
A and B are unrelated. A trusts B with the media bytes it grants; software
cannot prevent an authorized administrator from retaining those bytes.

### 1.2 Complete user journey

1. Both administrators install/sign in to Tailscale on their server hosts.
   A grants network access to its sharing service; B verifies private reachability.
2. A selects local movie/show libraries and creates an invitation. B pastes
   it into Settings → Sharing. Both administrators compare the pairing code,
   and A approves the pending recipient.
3. B assigns viewers. Each sees a source-labelled library under Shared
   Libraries, browses/searches it, opens details, and starts playback.
4. Existing volume, pause, seek, quality, audio and subtitle controls work.
   Continue Watching and next episode work for the remote source.
5. A can remove a library or revoke the entire grant. B can remove a viewer
   or disconnect the import. Revocation stops new delivery within §9's bound.
6. A source outage leaves local browsing/playback usable. The shared library
   reports the actual failure and offers retry without signing the user out.

Native clients need remote-library browsing and playback. Administrator
pairing/assignment UI is web-first; native settings may open the authenticated
local web settings route. Do not require users to install Tailscale on TVs
or phones that already reach B. A viewer outside B's household still needs
an independent way to reach B; this feature does not create that connection.

### 1.3 Explicit exclusions

No Plex/Jellyfin/Emby adapter in this effort. No Watch Together rooms, remote
file management, re-export of imported libraries, cross-server title
merging, global search aggregation, offline packages, download UI, books,
photos/home libraries, Live TV, recordings, or library channels. Do not
silently expose remote items through the existing Plex façade. A future
adapter must implement this authorization contract before doing so.

## 2. Tailscale deployment — keep the HTTP service private

### 2.1 First supported topology: host Tailscale Serve

```text
B Cinema → B host tailscaled ══ private tunnel ══ A host tailscaled
                                                    TCP Serve :32443
                                                          ↓
                                               host 127.0.0.1:32444
                                                          ↓
                                               A sharing-only TLS router
```

Use the existing host Tailscale daemon and **raw TCP Serve**, preserving
Cinema's pinned TLS end to end. No embedded daemon, coordination service,
tailnet-wide API key, public reverse proxy, or Funnel. A Docker bridge
container does not own the host's Tailscale interface. Follow the shipped
[Compose deployment](../../deploy/docker-compose.yml), with an opt-in sharing
override; do not change the ordinary app's network mode.

Implemented node-local bare-host configuration:

```toml
[sharing]
listener_profile = "host_loopback"
bind = "127.0.0.1:32444"

[sharing.egress]
mode = "interface"
name = "tailscale0"
```

For Linux Docker bridge hosting, select `listener_profile = "docker_bridge"`,
bind the process to `0.0.0.0:32444` or its static container IPv4 address, and
set `sharing.egress.mode = "local_address"` with that concrete RFC1918 address.
Publish only host `127.0.0.1:32444:32444`. The
[opt-in deployment recipe](../../deploy/README.md#sharing-uses-an-explicit-linux-bridge-profile)
and normal startup preflight check the rendered configuration; running-host
isolation and private egress remain qualification requirements. Treat other
containers as network peers without application authority. Do not use host
`100.x` publication: Docker must start before Tailscale obtains an address.
TLS files remain under the daemon's data directory at `sharing-tls`; `transport`,
`peer_port` and `key_directory` are not configuration fields.

Proposed initialization command and existing Tailscale commands:

```bash
plurxd sharing init-tls --key-directory /var/lib/plurx/sharing-tls
tailscale serve --bg --tcp=32443 tcp://127.0.0.1:32444
tailscale serve status --json
```

`init-tls` is implemented; it creates a node key and self-signed
certificate with owner-only access and refuses to overwrite an existing key.
Initial issuance and renewal set `notBefore = now - 1 hour`, allowing bounded
receiver clock skew without disabling validity checks.
The operator configures Serve, then Cinema reads readiness. Raw `--tcp`
forwards TLS without terminating it; `--bg` retains Serve configuration over
restarts. See [Serve CLI](https://tailscale.com/docs/reference/tailscale-cli/serve).
Do not mount the privileged tailscaled socket in the application container.
Read-only host diagnostics may be supplied by an operator-run probe; absent
probe data means `unknown`, not permission to change Tailscale configuration.

**Outbound B matters too.** Serve provides A's ingress, not B's egress.
For the Linux bridge profile, B's outbound connections use the host's
Tailscale route with the deployment's forwarding/SNAT policy. S2 must qualify
this from inside the actual container, not just from a host shell. Resolve
names on that host through Tailscale and provision the verified numeric hint
through the endpoint-edit API if the container cannot reach its resolver.
No public DNS fallback is permitted. Record the exact host forwarding and
firewall configuration in the qualification receipt. If this path is not
available, report the topology unsupported; do not quietly add a SOCKS proxy,
public route, or host networking. Linux bare-host Serve is the initial
fallback profile. Other host/container modes need their own qualification.

For separate tailnets, use a user-owned B node accepting a machine share.
Cross-tailnet tagged recipients are unsupported. A is quarantined: every
Cinema operation, including claim polling, is B-initiated. A may use its
own cluster transport internally, but never calls into B. A restricted common
tailnet for tagged nodes is a separate qualified profile, not unrestricted
household membership. Network access is user-granular: another device owned
by the accepting user may reach this port. The Cinema credential supplies
per-logical-server authorization. See
[machine sharing](https://tailscale.com/docs/features/sharing).

### 2.2 Endpoints, TLS identity and readiness

The persisted cluster-wide `sharing_enabled` switch defaults to false.
Saving enabled always succeeds; readiness never changes that saved choice.
A failed sharing prerequisite makes the affected operation unavailable while
local Cinema remains usable. When disabled, the peer router returns
`sharing_disabled` and cancels existing sharing bodies under §9.

Endpoint hints contain `{ipv4, ipv6?, ts_fqdn, port, spki_sha256}`. Shared
machines can have a different IPv4 address in the recipient tailnet; use the
full `host.tailnet.ts.net` name, not its short name. B resolves only `*.ts.net`
through Tailscale's resolver, accepts only `100.64.0.0/10` or
`fd7a:115c:a1e0::/48` answers, and pins the resulting address for that dial.
Do not resolve untrusted URLs from metadata. B's administrator can supply a
recipient-side numeric address override at import or through §5.2. Changing
an address retains the approved pin and both source identity checks.

Disable redirects, proxy environment inheritance and public DNS fallback.
Use socket interface binding where the supported OS/network namespace makes
it possible. The Docker bridge profile binds to its qualified egress instead;
do not pretend it has `tailscale0`. A route preflight is diagnostic, not a
race-free security boundary. If a CGNAT packet escapes toward an ISP, it
cannot complete the pinned TLS handshake; send no invitation, grant secret
or application request until pin and handshake verification succeeds.
A no-WAN packet guarantee additionally requires the qualified host egress
firewall; do not infer that guarantee from SPKI verification alone.

Use TLS 1.2+ and the repository's Rustls family. Verify SHA-256 SPKI,
certificate validity, algorithms and handshake signature with a dedicated
verifier; never enable a general invalid-certificate escape. The SPKI is the
identity authority, not a claim of public-CA hostname validation. Generate a
one-year certificate, renew automatically with the **same key** with 30 days
remaining, atomically replace it, and hot-reload it. An expired certificate
fails closed and reports a repair action; keep local Cinema up. Test clock
skew, renewal failure and recovery. Route new crypto dependencies through the
ring-only dependency checks. Keys stay node-local, owner-readable, outside
Raft and normal database snapshots.

Up to four source endpoints may be trusted. `GET /sharing/v1/endpoints` over
an already-pinned, active-credential connection returns server/catalogue IDs,
a monotonic endpoint revision and the administrator-approved endpoint set.
B accepts additional pins only through that channel; validates all hints;
then atomically replaces its set with generation CAS. An older endpoint
manifest cannot undo a newer one. A new node must be approved by A's local
administrator before it appears. Retain a reachable old endpoint during key
rotation. If all old keys/endpoints are lost, B's administrator explicitly
confirms new pins out of band via the endpoint-edit API; no discovery-based
trust reset. Possession of a compromised old key and credential remains a
trust compromise, not a problem discovery can repair.

Readiness reports local listener, certificate/renewal, Serve configuration,
actual pinned outbound reachability, protocol compatibility, and node-key
expiry when observable. Unknown host state is labelled unknown. Recommend
disabling Tailscale node-key expiry on dedicated A/B servers through the
operator's admin console, or document a tested renewal procedure and expiry
alerts. Cinema does not change that setting automatically.

### 2.3 Network policy and qualification

The sharing listener mounts only `/sharing/v1`; no login, local API, web
assets, setup, metrics, Plex or internal cluster router. Ordinary listeners
never mount peer routes. A loopback connection still requires peer credentials:
Serve source IP loss does not weaken authorization. PROXY protocol is not
required and must not become authority.

Allow the receiving user only the sharing TCP port. Tailscale grants are
additive; a broad existing allow defeats a narrow rule. Verify effective
permissions, including SSH, ordinary HTTP and cluster-port denial to
`autogroup:shared`. Non-recipient users must be denied network access;
recipient-user devices without a Cinema credential must be denied catalogue
and playback. Do not promise device-level denial within that user's devices.
See [grant semantics](https://tailscale.com/docs/reference/syntax/grants).

No router forwarding, UPnP, NAT-PMP, PCP, Funnel, public IPv6 opening, exit
node or household subnet route is part of setup. Require Docker Engine 28+
for the first supported loopback-publish profile and reject routed or
unprotected publication modes for the sharing port. Older Docker releases
can expose localhost publishes to same-L2 hosts. Verify from another LAN
machine even on a recent release. See
[Docker port publishing](https://docs.docker.com/engine/network/port-publishing/).

S2 qualification must demonstrate all of these with versioned receipts:

1. An externally shared-in user reaches A's **TCP Serve** service using B's
   recipient-side address/name. This exact combination is not assumed proven
   by the individual features' documentation.
2. B's actual container resolves or uses an admin-provided hint and completes
   pinned TLS through its host. Host-only success is insufficient.
3. LAN/public addresses do not serve the sharing API; non-recipient and
   forbidden-port probes fail. No production firewall changes for a test.
4. Reboot Docker before tailscaled: local Cinema stays healthy. Restart
   tailscaled: Serve restores its configured path.
5. Stop Tailscale on either end: new peer connections fail within 5 seconds;
   open bodies stop under §9. Observe connections, not disappearance of
   `tailscale0`. Restore the daemon and verify recovery without re-pairing.

Literal Tailscale-address binding remains a future, separately qualified
bare-host mode; it is not a fallback in the first build. Direct and encrypted
relay paths both need throughput measurement. Show direct/relayed/unknown
only from diagnostics, never latency inference. No unconditional 4K promise.
See [connection types](https://tailscale.com/docs/reference/connection-types)
and [DERP](https://tailscale.com/docs/reference/derp-servers).

## 3. Trust and pairing — two authorizations, no account delegation

### 3.1 Identifiers and secret handling

Generate UUIDv4 installation `server_id` and `catalogue_epoch` once and
persist them independently of serving nodes. Do not reuse Plex's advertised
node identity. A database rebuilt from scratch gets new identities; a restore
requires the procedure in §10. Human names are untrusted display labels.

All invitation and grant secrets contain 32 cryptographically random bytes,
encoded base64url without padding. Hash verification uses domain-separated
SHA-256 with fixed-time comparison. Separate domains: `sharing-invite-v1`,
`sharing-grant-v1`, and `sharing-delivery-v1`. A stores verification hashes;
B stores its outbound grant credential encrypted. Redact invitation blobs,
Authorization, delivery tokens, URLs carrying capabilities, and remote error
bodies. Logs record only non-secret IDs and bounded typed outcomes.

Extend `CredentialKey` with sharing-specific seal/open methods whose AEAD
additional data binds purpose, local server ID and import ID. Do not call
`seal_trakt` with a fabricated user ID. Ciphertext may replicate; wrapping
keys follow the existing node-local provisioning and backup rules. Generalize `SealedRowCensus::observe_row` to purpose-tagged envelopes,
including sharing credentials, pending claims, rotations and upstream session
secrets. Preserve the existing startup refusal when sealed rows outlive the
key or its key ID does not match. This follows Trakt's existing safety rule;
it is **not** per-import degradation. This is the contract author's engineering
default, matching Opus's recommendation, not a decision explicitly attributed
to Paul. Retain it for S1; a request to isolate failure by secret purpose
would change startup/restore semantics and needs its own contract and tests. Joining nodes receive the credential
key through the existing admission mechanism before serving sealed state.
Test both purposes, backups and join-token provisioning; never regenerate a
key over ciphertext. Runtime peer/key failures cannot authorize delivery.

### 3.2 Exact invitation and claim protocol

An invitation is `cinema-share-v1:` followed by base64url-encoded JSON, at
most 8 KiB decoded. No account tokens or local file paths appear in it:

```json
{
  "version": 1,
  "server_id": "<A UUID>",
  "catalogue_epoch": "<A catalogue UUID>",
  "name": "Paul's Cinema",
  "endpoints": [{"ipv4":"100.101.102.103", "ts_fqdn":"cinema.example.ts.net",
                 "port":32443, "spki_sha256":"<hex>"}],
  "invitation_id": "<UUID>",
  "secret": "<32 random bytes, base64url>",
  "expires_at_ms": 1790985600000
}
```

The timestamp is an example, not an operational value. Default lifetime is
24 hours, maximum 7 days. Parse version, size, address family, port, endpoint
count and fingerprint before opening a connection. Verify TLS against the
pin before sending the invitation secret. A's live identity must match both
IDs in the invitation; a difference is terminal until repaired by an admin.

B first completes pinned TLS and reads the source identity; no bootstrap
secret is sent yet. Only after both IDs match does B insert the import in
`claiming`, with random `claim_id` and sealed random outbound credential,
before its first **claim** attempt. An existing `(source_server_id,
catalogue_epoch)` returns `already_imported`; explicit admin re-pair selects
the existing import. An unverified invitation cannot reserve that uniqueness
slot, overwrite its endpoints or replace its credential.
Keep the invitation secret in a separate sealed claim envelope until A confirms
claim creation, then erase that envelope. This permits restart/retry before
consumption without retaining the bootstrap secret for the grant's lifetime.
B sends `POST /sharing/v1/claims` over pinned TLS:

```json
{
  "invitation_id": "<UUID>", "invitation_secret": "<secret>",
  "claim_id": "<UUID>", "recipient_server_id": "<B UUID>",
  "recipient_name": "Friend's Cinema", "grant_credential": "<secret>"
}
```

A atomically consumes the invitation and creates a pending grant containing
the recipient, request digest and credential hash. One invitation cannot
activate two recipients. Exact retries with the same claim, recipient and
credential digest return the same `grant_id` and state. A different digest
or claimant gets `invitation_consumed`; do not return existing grant details.
Identical retries remain recoverable after invitation expiry if the original
claim committed before expiry. Retain this claim identity until grant deletion.
A must never store the request body or plaintext credential.

Both sides show the first 16 hex characters of SHA-256 over a length-prefixed
encoding of `cinema-pair-v1`, A ID, B ID, invitation ID, claim ID and credential
hash. The administrator compares that code through the channel used to invite
B. A's approval is explicit and names the stored claim. A's UI treats the
recipient name as an unverified label, not proof of identity.

B polls `GET /sharing/v1/grant` every 5 seconds using
`Authorization: CinemaShare <grant_credential>`. Pending credentials may read
only this grant state; they cannot browse, start playback, or read identities
of other grants. Approval changes `pending → active` atomically; denial or
cancellation changes it to `revoked`. Pending grants expire after 24 hours.

B stops polling after a terminal result or pending expiry; exponential
backoff to 60 seconds applies during outages with jitter, and never extends
the pending lifetime.

A grant is authenticated by possession of its scoped credential. Its stored
recipient ID selects the principal; a body field cannot change that binding.
This does not claim hardware attestation of B. B's administrator is trusted
with the credential and may operate it from configured B cluster nodes.

### 3.3 Scope changes, rotation, and revocation

Each grant has positive monotonic `scope_generation`, `credential_generation`
and `catalogue_generation`, plus `mutation_generation` for whole-record admin
CAS. Narrowing, disable and revoke increment scope generation; additions or
removals increment catalogue generation; rotation increments credential
generation only. Every mutation increments mutation generation atomically.
Catalogue data changes also advance library revisions (§6).

A scope mismatch forces current authorization revalidation. Cancel sessions
whose grant is inactive or whose item's library is no longer allowed; update
the observed scope generation for unaffected sessions. Scope additions and
credential rotation do not retire playback. B atomically changes its
per-import `assignment_generation` on whole-set assignment replacement and
re-pair; a pre-repair matrix cannot save after the import becomes active again.
Revalidate affected viewers and cancel only those who lost access.

Rotation uses a new B-generated sealed credential and idempotent request ID.
A atomically swaps its hash while retaining the old hash for **rotation-status
only**, for 10 minutes. The old credential cannot read metadata/media or
create sessions after the swap. A response lost after commit can be recovered
by either credential on the status route; new-token confirmation completes B's
swap. A second, different rotation while one is unresolved returns conflict.
No indefinite grace period for old credentials. If neither side can recover
the secret, revoke and re-pair.

A and B can revoke independently. Revocation is idempotent and durable;
revoked credentials are never resurrected. Removing a local viewer increments
B's assignment generation, without claiming to revoke the entire A-to-B grant.

## 4. Persistence — local user IDs never become remote item IDs

### 4.1 Shared schema contract

Put new types and `SharingStore` in a focused core module. Use the same schema
constants in SQLite and Hiqlite migrations. Allocate migration numbers from
current HEAD when building; do not reserve a stale number in this document.
The following DDL fixes the key shapes; append indexes and migration guards
without weakening these constraints. Millisecond timestamps use INTEGER.

```sql
CREATE TABLE sharing_identity (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    server_id TEXT NOT NULL UNIQUE,
    catalogue_epoch TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
) STRICT;
CREATE TABLE sharing_invitations (
    id TEXT PRIMARY KEY,
    token_hash TEXT NOT NULL UNIQUE,
    library_ids_json TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    expires_at_ms INTEGER NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('open','consumed','cancelled')),
    claim_id TEXT UNIQUE,
    claim_digest TEXT,
    CHECK (expires_at_ms > created_at_ms)
) STRICT;
CREATE TABLE sharing_exports (
    id TEXT PRIMARY KEY,
    invitation_id TEXT NOT NULL UNIQUE REFERENCES sharing_invitations(id),
    recipient_server_id TEXT NOT NULL,
    recipient_name TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    scope_generation INTEGER NOT NULL CHECK (scope_generation > 0),
    credential_generation INTEGER NOT NULL CHECK (credential_generation > 0),
    catalogue_generation INTEGER NOT NULL CHECK (catalogue_generation > 0),
    mutation_generation INTEGER NOT NULL CHECK (mutation_generation > 0),
    state TEXT NOT NULL CHECK (state IN ('pending','active','disabled','revoked')),
    pending_expires_at_ms INTEGER NOT NULL,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
) STRICT;
CREATE TABLE sharing_export_libraries (
    grant_id TEXT NOT NULL REFERENCES sharing_exports(id) ON DELETE CASCADE,
    library_id INTEGER NOT NULL REFERENCES libraries(id) ON DELETE RESTRICT,
    PRIMARY KEY (grant_id, library_id)
) STRICT;
CREATE TABLE sharing_imports (
    id TEXT PRIMARY KEY,
    source_server_id TEXT NOT NULL,
    catalogue_epoch TEXT NOT NULL,
    source_name TEXT NOT NULL,
    claim_id TEXT NOT NULL UNIQUE,
    remote_grant_id TEXT,
    credential_envelope TEXT NOT NULL,
    claim_envelope TEXT,
    endpoints_json TEXT NOT NULL,
    assignment_generation INTEGER NOT NULL CHECK (assignment_generation > 0),
    lifecycle_generation INTEGER NOT NULL CHECK (lifecycle_generation > 0),
    endpoint_generation INTEGER NOT NULL CHECK (endpoint_generation > 0),
    observed_scope_generation INTEGER,
    observed_credential_generation INTEGER,
    observed_catalogue_generation INTEGER,
    observed_endpoint_revision INTEGER,
    state TEXT NOT NULL CHECK
      (state IN ('claiming','pending','active','disabled','revoked')),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    UNIQUE (source_server_id, catalogue_epoch)
) STRICT;
CREATE TABLE sharing_viewers (
    user_id INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    viewer_id TEXT NOT NULL UNIQUE
) STRICT;
CREATE TABLE sharing_assignments (
    import_id TEXT NOT NULL REFERENCES sharing_imports(id) ON DELETE CASCADE,
    remote_library_id TEXT NOT NULL,
    user_id INTEGER NOT NULL REFERENCES sharing_viewers(user_id) ON DELETE CASCADE,
    enabled INTEGER NOT NULL CHECK (enabled IN (0,1)),
    PRIMARY KEY (import_id, remote_library_id, user_id)
) STRICT;
CREATE TABLE sharing_watch (
    source_server_id TEXT NOT NULL,
    catalogue_epoch TEXT NOT NULL,
    remote_library_id TEXT NOT NULL,
    remote_item_id TEXT NOT NULL,
    user_id INTEGER NOT NULL REFERENCES sharing_viewers(user_id) ON DELETE CASCADE,
    position_ms INTEGER NOT NULL CHECK (position_ms >= 0),
    duration_ms INTEGER CHECK (duration_ms >= 0),
    watched INTEGER NOT NULL CHECK (watched IN (0,1)),
    sequence INTEGER NOT NULL CHECK (sequence >= 0),
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (source_server_id, catalogue_epoch, remote_item_id, user_id)
) STRICT;
```

A source item belongs to exactly one library. Store operations reject a change
of `remote_library_id` for an existing watch identity without an explicit
source move/reconciliation result. Validate JSON schemas before insertion;
JSON text does not authorize arbitrary endpoints or library IDs. Import
`lifecycle_generation` changes on disable/revoke/re-pair, fencing old attempts;
`assignment_generation` owns whole-set CAS, even when the assignment set is
empty, and also advances on re-pair; `endpoint_generation` owns local endpoint-edit CAS. Observed source
counters are remote observations, never local authorization. All counters
start at 1, use checked increment, and reject overflow. Invitation
library IDs are snapshotted choices; recheck existence/type on approval.

Keep an import after disconnect as `disabled`/`revoked`; it cannot expose
cached content. Re-pairing the same source/epoch updates that import by explicit
admin action. Preserve watch history so a legitimate re-grant resumes, but
never surface it in Home without current authorization. A new catalogue epoch
starts a distinct identity; no automatic provider-ID watch migration.

Additional durable records, using the same strict constraints and bounded
retention, are required in the same storage work package:

| Table | Required identity and state |
|---|---|
| `sharing_rotations` | Grant/import, request UUID, old verification hash, new hash/sealed credential as appropriate, state, created time, 10-minute expiry; no plaintext |
| `sharing_catalogue_revisions` | Library ID and positive membership/order revision; bump only on membership or sort-key changes, not metadata-only writes; no per-browse writes |
| `sharing_relay_upstream` | Existing B media-session incarnation FK; import, lifecycle/assignment observations, remote item/file/revision, A request/session/incarnation, approved endpoint revision, sealed upstream capability, source position; one row per incarnation |
| `sharing_delivery_grants` | File-grant-style hash verifier bound to B relay/session incarnation and source login-token hash, active/revoked state, deadline; no second player token |
| `sharing_source_session_bindings` | Existing Source incarnation; immutable canonical principal/request/fingerprint/playback and Source/epoch/library/item/file/revision binding, held reservation, dispatch/resolution fence and exact release receipt; FK-free retained capacity ledger, not another session lifecycle |
| `sharing_endpoint_manifest` | Singleton approved node endpoint set and monotonic revision, modified only by local admin; node private keys excluded |

There is no `sharing_media_*` family, separate relay lifecycle table,
per-segment resource map, or replicated browse snapshot. The existing media
session family owns both A's share principal and B's local-user relay (§7). The
Source binding adjunct cannot carry a second route or producer state machine.
It preserves uncertain remote obligations across ordinary request/route cleanup
until an exact never-dispatched CAS or confirmed producer-reap and writer
settlement releases them. Expiry, pointer displacement and a terminal control
receipt alone do not prove physical capacity was returned.

Library deletion uses a single store transaction: advance each affected
export's scope/catalogue/mutation counters, remove export links, then delete
the library. Restrict the FK to catch callers that forget this step; both
backends and every deletion caller must implement it. Inventory the actual
scan/delete transactions before adding catalogue revisions.

B's viewer UUID is generated atomically on first use. Deleting the user
cascades it; recreating the same numeric user ID never inherits the old UUID,
assignments or progress. Password changes do not erase history, but existing
login-token revocation still cancels delivery. B sends a per-import pseudonym,
SHA-256 of length-prefixed import UUID and viewer UUID, rather than a username,
email, local user ID, password hash, or account token.

Integrate every durable table with the SQLite import inventory, snapshots,
retention, admin deletion, and backend contract tests. These records belong
to each server's own database; no Raft membership or database is shared with
another household. Never dump decrypted credentials for import verification.

### 4.2 Store operations and authority reads

Proposed Rust interface outline; outcome enums must distinguish replay,
conflict, expired, revoked and successful creation rather than return `bool`:

```rust
#[async_trait]
pub trait SharingStore: Send + Sync {
    async fn sharing_identity(&self) -> Result<SharingIdentity, StoreError>;
    async fn claim_share(&self, claim: NewShareClaim)
        -> Result<ShareClaimOutcome, StoreError>;
    async fn approve_share(&self, grant_id: &str, expected_mutation_generation: i64)
        -> Result<ShareMutationOutcome, StoreError>;
    async fn mutate_share_scope(&self, change: ShareScopeChange)
        -> Result<ShareMutationOutcome, StoreError>;
    async fn authorize_export(&self, request: ExportAuthorization)
        -> Result<ExportAuthorizationOutcome, StoreError>;
    async fn authorize_import_viewer(&self, request: ImportAuthorization)
        -> Result<ImportAuthorizationOutcome, StoreError>;
    async fn claim_relay_start(&self, request: NewRelayStart)
        -> Result<RelayStartOutcome, StoreError>;
    async fn save_remote_progress(&self, update: RemoteProgress)
        -> Result<RemoteProgressOutcome, StoreError>;
}
```

Authorization reads must be consistent with committed revocations. Catalogue
replica reads may supply allowed metadata only after authority validation.
Use short, bounded authority leases (§9) for open media responses, not one
replicated write per byte or segment. A disconnected minority cannot renew
an authority lease by rereading stale local state.

## 5. API — separate administration, local viewers, and private peers

### 5.1 Common wire rules

Protocol major version is in `/sharing/v1`; advertise min/max supported
major in identity and grant responses. Unsupported versions return 426 with
`sharing_protocol_unsupported`. Optional additive response fields are allowed;
unknown action names and authorization/request fields are rejected. JSON
limits are checked before allocation; all IDs and labels have byte limits.

Canonical source IDs are decimal strings on the sharing wire, at most 20
bytes, representing nonnegative signed 64-bit IDs. UUIDs are canonical lower
case strings. Do not serialize remote IDs as JavaScript numbers. Positions
are finite integers in milliseconds within the JS safe integer range.
Remote media revision is `{size_bytes: "<decimal>", mtime_ms: "<decimal>",
revision: "<opaque source revision>"}`; the source revision changes whenever
media/track facts used by a recipe change, including same-size replacement.

Error body: `{code, message, retry_after_ms?, request_id?}`. Peer errors are
mapped to bounded local messages; never proxy raw upstream bodies. A's 401
becomes B's 502 `sharing_peer_auth_failed`, not a local 401 that signs out B's
viewer. Missing local login remains 401. Unassigned or unexported objects
return 404 to an ordinary viewer; admins receive explicit grant state.

### 5.2 Administration routes on the ordinary local API

All paths below are under `/api/v1/sharing` and require `AdminUser`:

| Method/path | Contract |
|---|---|
| GET/PUT `/settings` | Read/save `enabled`; readiness returned separately and never vetoes Save |
| GET `/status` | Listener/Serve/pin/expiry status, per-node import availability, bounded diagnostics; no secrets |
| POST `/invitations` | `{library_ids:[decimal strings], ttl_seconds?}`; return invitation once |
| DELETE `/invitations/{id}` | Cancel open invitation; idempotent |
| GET `/exports` | Pending/active/revoked grants and selected local libraries |
| POST `/exports/{id}/approve` | `{expected_mutation_generation, pairing_code}` |
| PUT `/exports/{id}/libraries` | Replace allowed local set with mutation-generation CAS; cancel sessions losing access |
| DELETE `/exports/{id}` | Revoke, not merely hide |
| POST `/imports` | `{invitation, endpoint_overrides?}`; parse, verify pinned identity, persist, claim; return import state, never echo invitation |
| GET `/imports` | Local import status and pairing code |
| GET `/imports/{id}/libraries` | Administrator fresh pinned Source scope for assignment bootstrap, independent of viewer assignments; return exact import/Source/epoch/lifecycle/assignment generation and active state with at most 64 movie/show libraries |
| GET `/imports/{id}/assignments` | Complete bounded current import/Source/epoch/lifecycle/state and `expected_assignment_generation` with grouped Source library strings and exact local user IDs; refuse corrupt/oversized matrices |
| PUT `/imports/{id}/assignments` | `{expected_assignment_generation, assignments:[{library_id,user_ids}]}`; explicit whole-set replacement |
| PUT `/imports/{id}/endpoints` | `{expected_endpoint_generation, endpoints, confirm_new_pins:false}`; address edits retain pins; new pins require explicit out-of-band admin confirmation or authenticated manifest proof |
| POST `/imports/{id}/re-pair` | Explicit verified invitation replacement; lifecycle CAS, retire prior attempts, preserve private history |
| POST `/imports/{id}/rotate` | Start/recover the bounded rotation protocol |
| DELETE `/imports/{id}` | Disable local access and cancel local sessions; retain private history |

Limit exports/imports to 32 each, invitations to 32 live per server, and
libraries to 64 per grant as initial operational bounds. Return typed capacity
errors; the admin UI displays them. They are tunable only after resource tests.

### 5.3 Viewer routes on B

Require `AuthUser` and assignment authorization at every entry point:

| Method/path under `/api/v1/shared` | Contract |
|---|---|
| GET `/libraries` | Assigned active libraries, source label, opaque import/library reference and availability |
| GET `/imports/{i}/libraries/{l}/items` | Paginated browse; `query` searches that library only |
| GET `/imports/{i}/items/{item}` | Details, hierarchy, playable files, tracks, chapter/skip metadata |
| GET `/imports/{i}/items/{item}/children` | Paginated season/episode children |
| GET `/imports/{i}/art/{resource}` | Mapped artwork resource; authorization before cache lookup |
| GET/POST `/imports/{i}/files/{opaque}/decision` | Existing decision types; pre-session, assigned viewer and current source membership required |
| POST `/imports/{i}/files/{opaque}/hls/sessions` | Existing start request/response types through remote-source admission; same idempotent start path as playback below |
| GET/HEAD `/imports/{i}/files/{opaque}/...` | Closed file-resource suffixes in §7.3, including pre-session subtitles/chapter thumbnails and admitted media delivery |
| POST `/imports/{i}/playback` | `{item_id,file_id,revision,playback_id,request_id,start_ms,caps,selection}` |
| GET `/playback/{id}` | Recover start result or terminal state without a new transcode |
| POST `/playback/{id}/control` | Existing playback-control semantics through B's session adapter |
| POST `/playback/{id}/selection` | User quality/audio/subtitle/seek intent; supersession fenced to this viewer |
| DELETE `/playback/{id}` | Idempotent retirement |
| POST `/imports/{i}/items/{item}/progress` | Local watch state; active-session observation binding and ordered sequence |
| POST `/imports/{i}/items/{item}/watched` | `{watched:boolean}`; explicit local override, not an A write |
| GET `/continue-watching` | Authorized remote history with source-labelled references |

B serves remote-source sessions on the **ordinary**
`/api/v1/hls/{session_id}/...` namespace. Its existing HLS handlers dispatch on
producer kind after local ownership/capability checks: local producer or
`remote_source` relay. This includes playlists, media, `/control`, `/status`,
and `DELETE /api/v1/hls/{session_id}`. Prepared payloads carry ordinary HLS
paths, preserving server/web/Apple/Android reporter validation. The session
ID is B's UUID capability, never A's handle; §7.3 binds it to the live login.
A sharing principal remains forbidden on this ordinary router. B's relay is
a local principal, which is why this dispatch is allowed.

Each playable file in B's shared detail includes a `file_base` such as
`/api/v1/shared/imports/{i}/files/{opaque}`. It is a B-relative locator, not a
bearer capability. B MAC-signs a bounded opaque reference over import ID,
import lifecycle, source identity/epoch, library, item, file and revision;
verify that binding and current viewer authority on every call. No raw source
ID is looked up in B's local files table. The suffix builder accepts only
§7.3's file grammar. Details, decision and thumbnails need no relay session;
bytes that create playback use admission before delivery. The authenticated
`hls/sessions` and `/playback` starts share one idempotency implementation.

Catalogue/detail/pre-session file calls require `AuthUser`. Native media
requests without account headers use the B relay capability: ordinary HLS
paths embed it; file media URLs add the bounded `session=<B UUID>` query.
That session must bind the exact opaque file reference, revision, recipe,
viewer and live login. B's authenticated start returns these delivery URLs;
the client file-context helper appends the same session query for later
media/subtitle requests. A `file_base` alone never authorizes delivery.
Pre-session calls can use existing local authenticated requests; no second
long-lived capability or account credential is sent to A. Management,
selection intent and progress endpoints also require B's `AuthUser`.

### 5.4 Private peer routes on A

Only the Tailscale listener serves these. Except identity and claim, require
`Authorization: CinemaShare <credential>`; pending status is the one exception
to the requirement for an active grant.

| Method/path under `/sharing/v1` | Contract |
|---|---|
| GET `/identity` | Server/catalogue IDs, name, protocol versions; no libraries or paths |
| POST `/claims` | §3.2; strict 16 KiB body and per-peer admission bounds |
| GET `/grant` | Own pending/active/disabled/revoked state, named generation counters and supported media |
| GET `/endpoints` | Active grant only; approved source endpoint manifest, revision and both source IDs |
| POST `/grant/rotation`, GET `/grant/rotation/{id}` | §3.3; old credential status-only after swap |
| GET `/libraries` | This grant's local movie/show libraries |
| GET `/libraries/{id}/items` | §6 paginated browse/search |
| GET `/items/{id}`, GET `/items/{id}/children` | Grant-checked metadata and hierarchy |
| POST `/items:batch` | At most 200 IDs; per-ID current metadata/authorization outcome, no out-of-scope metadata; §6 |
| GET `/art/{resource}` | Grant-bound opaque artwork resource; no filename lookup supplied by B |
| POST `/playback` | Share principal + actual client caps → source decision and idempotent start |
| GET `/playback/requests/{id}` | Recover exact start outcome, scoped by grant and viewer identity |
| POST `/playback/{id}/control`, POST `/playback/{id}/selection` | Existing typed lifecycle and selection semantics |
| GET/HEAD `/hls/{session}/...`, POST `/hls/{session}/control` | Closed existing HLS resource/control grammar; verify share principal before internal relay |
| GET/HEAD `/items/{item}/files/{file}/subs/{n}[.vtt]`, `.../subs/{n}/overlay.json`, `.../subs/{n}/overlay/{generation}/objects/{png}`, `.../chapters/{n}/thumb` | Typed subtitle/overlay/chapter suffixes before start; the complete Source reference in one bounded `CinemaShare-Reference` header; active grant, item/file/library membership and exact revision before and after the Local per-file work; no raw file lookup, no session. `direct` and `stream.mp4` are not asset suffixes |
| GET/HEAD `/playback/{session}/files/{file}/...` | Direct/progressive and other admitted file bytes under §7.3; exact principal/session/recipe |

| GET/HEAD `/items/{item}/files/{file}/...` | Typed subtitle/overlay/chapter suffixes before start; active grant and item/file/library membership, bounded extraction; no raw file lookup |
| POST `/items/{item}/files/{file}/sessions/{request}/direct` | Admitted direct-play bytes under §7.3, following the `resources` precedent: the exact published Start lineage plus `{method GET\|HEAD, range, if_range}`; Local's 200/206/416 plan and header set. Progressive `stream.mp4` is not served for shared files |
| GET/POST `/items/{item}/files/{file}/decision` | Existing decision request/response types, grant-check item/file relationship before invoking shared decision service |
| DELETE `/playback/{id}` | Retire only this grant's session; repeated deletion succeeds |

Peer start includes `viewer_key`, `playback_id`, `request_id`, source item/file
revision, caps and selection. Derive recipient identity from the credential,
not the body. Every resource request checks item→library→grant membership,
including after a library deletion; deleting an exported local library must also atomically
advance affected scope/catalogue/mutation generations in the same store
transaction (§4); the foreign key blocks a bypass. No route accepts a local path, upstream
URL, shell argument, unbounded filter, or another grant's session identifier.

## 6. Catalogue and identity — stable pages, bounded caches

Remote item reference:

```json
{
  "source":"shared", "import_id":"<B UUID>",
  "server_id":"<A UUID>", "catalogue_epoch":"<UUID>",
  "library_id":"12", "item_id":"9876543210987654"
}
```

Use a new `SharedItemDto` composed from the existing presentation vocabulary,
not a changed meaning for `ItemDto.id`. It contains this reference, title,
kind, parent reference, sort title, year, genres, overview, tracks, media facts,
B-relative artwork URLs, each playable file's `file_base`, and B's watch
overlay. Never include source paths,
source account state, internal nodes, raw capabilities or filesystem errors.

A returns `{items,next_cursor,catalogue_revision,scope_generation}`. Default
page is 60, maximum 200. Use opaque signed keyset cursors, not offset pages or
replicated ID snapshots. Cursor payload: version, grant ID, library ID,
canonical sort key, item ID tie-breaker, filter digest, scope generation,
catalogue generation, library catalogue revision, expiry. MAC with a
cluster-shared purpose-separated key provisioned through the credential-key
mechanism. Bound cursor length to 4 KiB and lifetime to 5 minutes; reject
invalid signature/fields/expiry rather than accepting client SQL.

All nodes use identical collation, NULL ordering and sort vocabulary. Validate
authority and read the current revision/page in one consistent read boundary.
Only membership or sort-key changes advance the library's order revision;
metadata-only artwork, analysis and description updates do not. Filtering
uses the current item values on each page. Metadata changes that alter filter
eligibility have live-list semantics, not a frozen browse snapshot.

An order-revision change **resumes** from the signed last `(sort_key, item_id)`
under the new revision; it does not restart the list. The response's next
cursor records the new revision. Grant scope/catalogue changes trigger current
authorization revalidation: deny removed libraries, continue a still-allowed
library from its prior boundary, and refresh observed counters. Never use an
old scope counter as authority. Expired/invalid cursors or changed query/sort
require a fresh open, with a typed result; ordinary scans do not.

B deduplicates the logical browse by full source reference. The guarantee is
no duplicate displayed items, and forward progress while scans run. An item
whose sort key moves behind the boundary, or which newly matches a filter
behind it, can be missed until the next open. Items moved ahead may be sent
again by A but are suppressed by B. With unchanged membership/order/filter
eligibility, keyset traversal has no skips. Do not claim snapshot isolation
across mutations or completion of an unbounded continuously growing library.
B retains seen references for the current logical browse, including evicted
page payloads; destroying that browse also discards its cursor. Empty pages
after dedup must still advance from A's cursor, not B's last rendered item.

This avoids per-browse Raft writes and removes the 100,000-item snapshot cap.
An index supports `(library, sort_key, item_id)`. S4 pages a large finite
fixture to exhaustion while metadata updates run continuously, then exercises
sort/membership mutation, duplicate suppression and forward cursor movement.
Record latency and seen-reference memory on large libraries; no unbounded
server-side snapshot or automatic restart loop.

B caches at most 32 MiB of metadata in memory and 256 MiB of artwork on disk
per node initially, with LRU eviction and no media file cache. Metadata TTL
is 30 seconds and is keyed by import/lifecycle/reference/revision plus
observed scope/catalogue counters. Assignment authority is checked separately. Cached
artwork still needs current authorization; tokenless public cache URLs are
forbidden. Clear displayable metadata on explicit revoke/disconnect. A
transient source outage may show the library name and offline state, but v1
does not expose stale title browsing as if authorized and current.

Search occurs within a selected shared library. Continue Watching is a
separate B-owned list filtered by current assignments and remote authorization.
A slow/offline import gets its own status and cannot delay local Home data.
Use `POST /sharing/v1/items:batch` for at most 200 source IDs per request,
returning authorized metadata or a per-ID unavailable/not-found outcome with
no out-of-scope detail. Group by import; permit one outstanding batch per
import and four across B. Use the 30-second metadata TTL, and revalidate
authority before displaying cached entries. Return local Home immediately;
remote sections complete independently with timeout/retry. Do not issue one
serial metadata call per Continue Watching entry.

## 7. Playback — reuse the engine without impersonating a source user

### 7.1 The ownership prerequisite is a real work package

Current session structs and `MediaSessionStore` use integer `user_id`, and
session requests/pointers/preparations use `(user_id, playback_id)` keys.
An `AuthUser` wrapper is not enough to admit a remote household safely.
Introduce an internal explicit principal:

```rust
enum PlaybackPrincipal {
    LocalUser { user_id: i64 },
    Sharing { grant_id: String, viewer_key: String },
}
```

Keep one existing media-session family and one session-ID namespace. All
ownership-bearing rows carry a canonical `owner_key TEXT NOT NULL`, built
only by a typed `PlaybackPrincipal::owner_key()` constructor:

- Local: `local:<canonical decimal user_id>`.
- Sharing: `share:<canonical grant UUID>:<64 lowercase hex viewer_key>`.

The fixed UUID/hex grammars exclude delimiters; user IDs use canonical signed
64-bit decimal representation. Request bodies cannot supply `owner_key`.
Use this column for every principal composite key and conflict target; keep
separate principal projections for authorization and explicit local filtering.
Required columns/constraint in rebuilt ownership-bearing tables:

```sql
owner_key TEXT NOT NULL,
principal_kind TEXT NOT NULL DEFAULT 'local'
    CHECK (principal_kind IN ('local', 'sharing')),
user_id INTEGER,
share_grant_id TEXT,
share_viewer_key TEXT,
CHECK (
    (principal_kind = 'local' AND user_id IS NOT NULL
     AND share_grant_id IS NULL AND share_viewer_key IS NULL
     AND owner_key = 'local:' || CAST(user_id AS TEXT))
 OR (principal_kind = 'sharing' AND user_id IS NULL
     AND share_grant_id IS NOT NULL AND share_viewer_key IS NOT NULL
     AND owner_key = 'share:' || share_grant_id || ':' || share_viewer_key)
)
```

This is a table-definition fragment, not an ALTER migration. The store
validates UUID/pseudonym grammar and real owner existence before admission;
the CHECK prevents the stored key/projections from disagreeing. Keep rebuilt
session tables **FK-free**, matching today's lifecycle: do not add references
to users or export grants. Ended rows must not block user/grant deletion.
On B, a remote-source session's principal is local; its remote grant belongs
only to its upstream binding, not B's export principal projection.

Use the table-rebuild precedent `MEDIA_SESSIONS_V10_SCHEMA` in
[hiqlite_sessions.rs](../../crates/plurx-core/src/store/hiqlite_sessions.rs).
Backfill local owner keys; preserve non-ownership columns, terminal records,
leases and required triggers/indexes. Apply this ownership change to requests,
pointers, preparations, desired selection and producer recovery, not only
`media_sessions`. Explicit key/query contract:

| Operation | Required ownership key |
|---|---|
| Request claim/replay/upsert | `PRIMARY KEY (owner_key, request_id)`; `ON CONFLICT(owner_key, request_id)` |
| Current pointer, desired selection, preparation per playback | `PRIMARY KEY (owner_key, playback_id)`; matching conflict target |
| Recovery/attempt ledgers | Replace the `user_id` component with `owner_key`, retaining all epoch/attempt/decision fences |
| Admission counts and supersession | Predicate `owner_key = ?`, plus source grant/server caps for sharing; prepared and unresolved slots count |
| Incarnation/session lookup | Existing unique ID plus returned/validated principal; one namespace |

No nullable principal column belongs to a composite PK, and no partial-index
conflict target is needed. Both backends use the same key encoding and
transaction predicates. Exact retries must resolve the original incarnation;
repeat-request tests must prove that only one encoder is allocated.

Owner existence checks remain authoritative after deletion. User deletion
invalidates login-bound delivery, ends active local/remote-source sessions,
and leaves retained terminal rows readable until ordinary cleanup; reusing a
numeric ID cannot resurrect their authority. Grant deletion first revokes and
cancels sharing sessions, then prunes dependent records in a store transaction
under the retention policy. Retained terminal rows carry immutable owner keys
but confer no access to a missing grant. Test deletion with active, ended and
terminal-ack rows on both backends; do not add cleanup FKs as a shortcut.

The recheck at `15e36f7f4` confirms Opus's 36 `MediaSessionStore` operations:
16 explicitly user-keyed and 20 keyed by session/incarnation/owner. Recount
at implementation HEAD. The S3 PR carries
a finite census of **every `user_id` predicate, key, conflict target and
decoder**, both backends, plus
activation structs, direct/progressive registries, telemetry and admin paths.
Local-only queries explicitly require `principal_kind = 'local'`; common
principal-aware queries select the appropriate branch. No sentinel user ID,
fake account or implicit conversion to `AuthUser`.

Reuse session-keyed activation, preparation rejoin, handoff arm/complete,
terminal projection/acks, route lookup, renewal, expiry, takeover, end,
maintenance and owned-session enumeration. They keep one lifecycle algorithm;
their row/result types must still carry the principal, so “reuse” does not
mean no edits or tests. The ordinary router refuses sharing principals before
`relay_if_remote`; the peer router verifies grant/viewer/session ownership
before using that same internal relay. Receiving cluster nodes recheck the
forwarded typed authority, not a user ID supplied by a foreign peer.

Admin activity/stop can deliberately cover both kinds behind `AdminUser`;
user history and household playback enumeration stay local. Source share
sessions never update A's watch state. Migration, mixed-version admission and
rollback are mandatory S3 acceptance under §10, not a later cleanup task.

### 7.2 Start and replay contract

On B the viewer is a local user. Create an ordinary local-principal
`media_sessions` incarnation with producer kind `remote_source`; keep only
upstream binding/sealed material in `sharing_relay_upstream`. Reuse existing
request claims, placement, leases, terminal state, admin stop and cleanup.
Adapt admission/activation so this producer needs no local file FK: the
validated import/reference side row replaces the local-file recipe. Do not
insert a fake local file or repurpose a foreign integer file ID.

Persist the claim and remote-source recipe before the first A start. B's
idempotency key remains its existing local principal/playback/request key;
the fingerprint includes import lifecycle, source identity, item/file revision,
selection and caps. Revalidate current permissions on replay without treating
benign credential rotation as different intent. A independently claims the
sharing principal/playback/request identity before allocating an encoder.
Same key/different intent is conflict; unknown outcomes query the same request
ID. Never allocate a replacement merely because a response was lost.

B forwards a bounded `CinemaShare-Viewer` header on playback operations; A
validates its pseudonym grammar and combines it with the credential-bound
grant. Session IDs and request-status lookups cannot cross that principal.
The header carries no local user identity and is never trusted on the
ordinary app router.

The peer response reuses the existing
[StartResponse](../../crates/plurxd/src/http/hls/session_guard.rs) and decision
DTOs, with protocol/authority metadata in a peer envelope. B unwraps and
returns the existing player type with B session IDs and ordinary
`/api/v1/hls/{B-session}/...` paths. Client-side file URL builders separately
use the `file_base` context (§8); changing response URL fields alone is
insufficient. Preserve `quality_catalog_status`,
`ladder`, `quality_candidates`, `quality_candidate_id`,
`display_aware_auto_protocol`, `prior_kbps`, `vod`, `height`, `encoder`,
`start_seconds`, `media_origin_ms`, duration, delivered dynamic range/Dolby
Vision profile, control bootstrap and plan notes. Decision method selection
and direct/progressive fields keep their existing types too. Shared fixtures
compare all fields; an engine DTO change must update the fixture, not silently
strip a field in the adapter.

For v1 source sharing starts set `prior_kbps = None` and bypass A's node-local
LAN throughput-prior lookup/update. B must not inject its LAN prior either.
The player's normal ABR samples pass-through delivery end to end. A later
per-grant prior needs its own reviewed measurement policy.

A uses the actual player's caps and selection and performs any transcode;
B never decodes/re-encodes or lies about HDR, audio or subtitle capabilities.
Provisional starts use the engine start deadline plus §9's transport margin;
cleanup reconciles an uncertain result before retiring the exact attempt.
A's cluster assigns the source producer normally. B places `remote_source`
only on nodes with working Tailscale egress and key access; other B ingress
nodes forward through B's existing internal cluster relay (§10).

### 7.3 Resource and lifecycle translation

Preserve the engine's relative HLS URIs under B's ordinary HLS prefix. Examples already
emitted are `init.mp4`, `seg…m4s` and `subs/{index}/index.m3u8`; the closed
sub-grammar comes from the actual HLS router and producers. Map the prefix
and session once; do not create a resource UUID or replicated row per segment.
A bounded playlist validator checks URI lines and URI-bearing attributes;
no general URL fetch or rewrite engine. Reject absolute/root-relative URLs,
authorities, traversal, encoded separators/escapes, unknown URI kinds and
unexpected keys/encryption as `sharing_resource_unsupported`. Explicitly
allow only the current engine's query keys with typed bounds. S5 inventories
all emitted playlist forms so legitimate renditions are not silently dropped.

File resources are not children of the HLS producer. B mounts these typed
suffixes under `file_base`; A separates pre-session item/file routes from
admitted playback/file routes (§5.4):

| Suffix | Contract |
|---|---|
| `decision` | Existing GET/POST request/decision types, available before a session; authorize exact file reference |
| `hls/sessions` | Existing authenticated start semantics, creates B's remote-source session through one idempotent admission path |
| `direct`, `stream.mp4` | GET/HEAD and Range/If-Range/416; authenticated admission or exact existing session binding before bytes; no uncounted streaming |
| `subs/{subtitle}` | Existing sidecar/VTT track grammar; pre-session authenticated preview or exact session delivery |
| `subs/{index}/overlay.json` | Pre-session PGS manifest; timing/generation preserved |
| `subs/{index}/overlay/{generation}/objects/{object}` | Exact PGS generation/object grammar; same file/track authorization |
| `chapters/{index}/thumb` | Available on shared details before start; exact file/chapter membership |

Decision, subtitle extraction and chapter-thumbnail requests check current
item/file/library/assignment authority without requiring a fabricated session.
They remain bounded by extraction/RPC limits. Direct/progressive requests
that initiate delivery must use the same session admission/accounting and
login binding as HLS; HEAD and Range cannot bypass those checks. Prefer the
authenticated `/playback` start before handing a native player its returned
file media URL with `session=<B UUID>`. Repeated resource requests must never
create an extra source producer. A source file revision change returns a
typed reopen result, not another local-file lookup.

The single file-context builder (§8) handles all suffixes and the optional
session binding; raw source file numbers are never B API identifiers.
Typed decision/start/PGS-manifest URL fields still need explicit translation,
even though HLS playlist bodies remain unchanged. Inventory and test every
URL field, control bootstrap, status/stop and prepared replacement. Never leak
an A address, A handle, account token or raw filesystem path.

B's `session_id` is a fresh cryptographically random UUIDv4, following the
existing local HLS capability format. Server and native prepared-payload
validators require UUID session IDs; a 256-bit base64url ID would break them.
Invitation/grant secrets remain 256-bit. Keep the UUID grammar and reporter
rules unchanged, hash the delivery verifier, rate-limit guesses, redact URL
capabilities and enforce parent-login/session authority. Model `sharing_delivery_grants` on
[file_grants.rs](../../crates/plurx-core/src/store/file_grants.rs): hash verifier,
`source_token_hash`, `source_active`, incarnation binding and revocation.
Existing lifecycle storage may retain the session ID as it does for local
HLS; do not claim every capability is hash-only in that storage. Keep A's
upstream handles/capabilities sealed on B. No additional 6-hour token or
renewal protocol: admission/owner leases and the parent login bound lifetime.
Native media requests need no account header; possession authorizes only this
session's media and bounded existing control exchange. Other authenticated
management routes still verify the local owner.

Logout/token revocation, user deletion, import disable and assignment loss
must invalidate the delivery grant and cancel open bodies, including through
other ingress nodes. Reusing local sessions does **not** by itself prove
login-token invalidation; S5 explicitly wires and tests the file-grant pattern.

Preserve origin, discontinuities, ranges, ETag, MIME and subtitle timing.
Playlists/manifests are bounded at 1 MiB; streaming responses use at most
512 KiB application buffering each. Forward only an enumerated response
header set; strip hop-by-hop headers, Set-Cookie, Location and upstream auth
challenges. Error responses use §5.1 mappings.

Control observations remain on A's source timeline. Prepared replacements
remain source-owned; B allocates the corresponding local relay incarnation,
stages its B URL, and carries the real control epoch/receipt through commit
or abort. Fence both sides' ownership and delayed responses; never fabricate
an epoch or let stale selection reverse current intent. An uncertain control
commit reconciles its original request identity. Retire the exact predecessor
only after confirmed commit. B's durable pointer and upstream side record
must atomically identify the same accepted incarnation after recovery.

### 7.4 Watch state and next episode

B receives position from the local player, never by copying A's user state.
Apply existing 95-percent watched behavior and explicit unwatched semantics.
A progress event names the relay and monotonic sequence. Accept only current
viewer/session generations; reject an old session's late beat after a new
playback attempt or manual watched-state override. Coalesce writes at the
existing cadence and persist a final update on orderly teardown.

Store source positions, not HLS-local timestamps. Do not save position zero
before the first presented frame. Preserve progress when the source fails.
Next episode uses source hierarchy/order but creates a new authorized start;
it cannot guess the next local integer item ID. B's local Trakt integration
is not automatically extended to remote history in v1; document that limit.

## 8. Clients and settings — remote context stays explicit

Use one `PlaybackFileContext` contract on web, Apple and Android:

```text
source_ref     local reference | full shared reference
file_base      /api/v1/files/{local_id}
               | /api/v1/shared/imports/{import_id}/files/{opaque}
session_id     optional B UUID, assigned by playback admission
```

All file URL construction goes through one platform helper that appends only
the typed suffix/query vocabulary in §7.3. For shared files, accept `file_base`
only from B's authenticated detail response and validate its B-relative
shared prefix/import; reject absolute URLs, traversal and a local `/files`
base for a shared reference. The opaque reference is a locator, never proof
of authorization. The helper carries normal B authentication or the exact
admitted session query for headerless media delivery. Retain source IDs only
as remote metadata; never coerce them to a local numeric file model.

Session URLs continue to be built from the B UUID under ordinary
`/api/v1/hls/{session}/...`. Existing control/prepared reporters and their
validation remain unchanged. This avoids a parallel playback engine, but
**does require converting every file-keyed call site**. Start/progress adapters
alone are insufficient. Shared detail, resume, quality/seek replacement,
stall reopen, subtitle selection and next episode must preserve the context.
Account origin remains B; watch writes dispatch by the full source reference.

S6/S7 must turn this seed census into an exhaustive per-platform inventory
at their implementation HEAD:

| Surface | Required converted call sites / regression |
|---|---|
| Apple `PlurxAPI.swift` | Decision, overlay manifest/objects, HLS starts and every subtitle/chapter/direct/progressive builder use `file_base` |
| Apple `PlaybackControlReporter.swift` | Ordinary HLS master/index/control and prepared successor UUID accepted unchanged |
| Android `PlurxApi.kt`, player `Controller.kt` | Decision, overlay manifest/objects, HLS start, direct/progressive reopen; HLS status/DELETE/control use B session UUID |
| Android `PlaybackControlReporter.kt` | Ordinary HLS path and UUID validation preserved |
| Web `decode-tiers.js`, `audio-sync.js`, `watch-browser.js` | Every progressive fallback, subtitle fetch and pre-session chapter thumbnail uses file context |
| Web `playback-control.js` | Ordinary B HLS control/prepared path; no source ID or shared HLS prefix |

Add executable source-contract tests that fail on file-path interpolation
outside the platform's approved context helper; inventory deliberate unrelated
admin/download routes explicitly rather than weakening the check. Behavioral
fixtures use the same numeric file ID on A and B with different media: shared
decision/subtitle/thumbnail/start/reopen must always reach A. Also exercise no
local file, details before playback, prepared successors, HDR/Auto and logout.

Web needs settings/shared-library/detail, file-context and progress integration.
New plain scripts obey [WEB-SHELL-LAYOUT.md](../clients/WEB-SHELL-LAYOUT.md):
file, `WEB_ASSETS`, shell tag and documentation row in one commit. Reuse the
existing card/player renderers through explicit view models. Apple and Android
need matching reference/DTOs, source-labelled browse, shared details, Continue
Watching and next episode. Remote/lock-screen/PiP actions retain the same
context. Unsupported offline/edit actions are absent. Qualify physical
Apple TV and Google TV focus and playback, not only simulators.

Settings → Developer initially contains the explicit sharing switch and
advisory readiness: Tailscale/Serve and egress · certificate · peer access · protocol
compatibility · outstanding qualification. Never disable the switch or reject
Save because one item is unmet. Capability and authorization failures still
refuse the specific operation; advisory readiness is not an access bypass.

Graduation: after S8's evidence and promotion, move the permanent switch and
management cards to Settings → Sharing. Name that criterion in `devGraduation`.
Show connecting/pending approval/ready/offline/revoked/version mismatch/capacity
as separate states, and show who shared the library. Do not expose protocol
IDs or internal topology in the normal watching flow.

## 9. Budgets, revocation and concurrency — explicit bounds

Initial constants are build requirements and reviewable tuning choices:

| Constant | Value | Meaning |
|---|---|---|
| `SHARING_AUTHORITY_LEASE` | 10 s | Maximum cached successful consistent authorization, monotonic locally |
| `SHARING_REVOKE_BOUND` | 30 s | Maximum from committed revoke to stopping new source/receiver bytes |
| `SHARING_READINESS_POLL` | 5 s | Read-only diagnostic refresh; Serve owns network-path lifecycle |
| `SHARING_CONNECT_TIMEOUT` | 5 s | Pinned TLS/private connection establishment |
| `SHARING_CATALOGUE_TIMEOUT` | 10 s | Bounded browse/detail RPC; failure isolated to that import |
| `SHARING_SOURCE_START_TIMEOUT` | `START_DEADLINE + 10 s` (currently 60 s) | Unknown outcome until reconciled |
| `SHARING_MEDIA_IDLE_TIMEOUT` | `MEDIA_BODY_NO_PROGRESS_TIMEOUT` (currently 30 s) | No incoming media bytes, not total film duration |
| `SHARING_CONTROL_TIMEOUT` | `EXCHANGE_DEADLINE + 6 s` (currently 10 s) | Covers held control exchange and transport |
| `SHARING_SESSIONS_PER_SERVER` | 8 | Source-wide remote slots, atomic across nodes; preparations also count |
| `SHARING_SESSIONS_PER_GRANT` | 4 | Atomic source admission; lower existing source capacity still wins |
| `SHARING_SESSIONS_PER_VIEWER` | 2 | Receiver admission; prepared successor consumes a real slot |
| `SHARING_STARTS_IN_FLIGHT` | 2/grant | Atomic bounded pending starts, including ambiguous outcomes |
| `SHARING_MEDIA_REQUESTS` | 16/session | Bounds segment/rendition parallelism and memory |
| `SHARING_PAIRING_BODY` | 16 KiB | Checked before parsing or allocating secrets |
| `SHARING_JSON_BODY` | 128 KiB | Maximum normal request; cap response JSON separately at 4 MiB |

Check A's grant/library membership and B's import/assignment/login validity
on admission and before response headers. Long-running bodies hold a 10-second
authority lease with a cancellation/deadline branch **while blocked on reads
or downstream writes**. A renew requires current consistent authority. On
quorum failure, lease expiry cancels output. Never reset its deadline from a
stale cached success. Per-byte blocking queries are unnecessary.

A scope change triggers current membership revalidation (§3.3), cancelling
only sessions that lost authority. Credential rotation and scope expansion
keep authorized playback alive; B refreshes the sealed credential atomically
and retries an auth failure with the confirmed current credential and same
operation ID. Do not pin session lifetime to a credential-generation value.
B removes delivery on lost assignment/login/user/import/feature authority.

Derive timeout constants from the engine symbols in
[media_sessions.rs](../../crates/plurxd/src/media_sessions.rs) and
[playback_control.rs](../../crates/plurxd/src/playback_control.rs); test the
ordering, not just today's numeric literals. Enforce grant and server session
caps in one admission transaction. Prepared successors and uncertain starts
hold real reservations, including the source-wide cap; saturated viewers may
need a typed capacity response. No replacement exception bypasses encoder or
upload limits.

Cancellation must drop socket bodies and release producer/relay ownership,
including response headers already sent, an encoder stuck during startup,
and a client that stopped reading. Already buffered video may continue playing;
acceptance measures last **new byte served**, not the last visible frame.
Retiring a shared session must never retire a local session with a colliding ID.

Store admission counters/claims atomically across cluster nodes. A process-local
semaphore alone cannot enforce a grant's cluster-wide session cap. Keep
per-node request/memory semaphores as a second bound. Declared source capacity
and operator resource limits take precedence over these upper bounds.

## 10. Cluster lifecycle, upgrades and rollback

Sharing never replicates data between households. Within each household,
identity, grants, assignments, claims, relay mapping and watch state replicate
through the existing Store. A node may serve sharing only with the TLS key,
Tailscale reachability, credential unwrap key and protocol implementation
required for that operation. Readiness reports absence without altering the
operator's saved feature setting.

Use up to four approved source endpoints; refresh pins only under §2.2.
B retries transport failures with the same request identity and reconciles
unknown starts. Each A serving node needs its own Serve path and network
share. A's non-owner peer ingress authorizes the sharing principal before
relaying to A's producer through the existing internal route.

B ingress uses the existing local-user media-session route to find its
`remote_source` owner. An ingress node needs no direct A access. Owner
placement/takeover requires node readiness for Tailscale egress and secret
unwrapping; if no eligible node exists, fail unavailable. A takeover winner
uses the fenced owner epoch and durable upstream record, reconciles the same
A request/session, and never blindly starts another transcode. Stable B
session URLs survive ingress changes with unchanged suffix meaning. Loss of
an unrecoverable producer returns an honest reopen result with source position;
this is not a promise of uninterrupted playback.

**Mixed-version rollout:** use the repository's existing schema-marker and
newer-schema refusal mechanisms. The runtime invariant is that no
sharing-principal row on A, or `remote_source` producer row on B, exists while
any participating session reader/writer runs an incompatible binary. Check
the schema/protocol floor of **every member** at sharing admission; unknown
or stale member capability means unavailable. Fence incompatible rejoin;
roll failed nodes forward after a newer marker commits. Preserve the saved
Developer choice and refuse only operations whose prerequisites are absent.

Do not mandate a full-cluster stop without testing compatibility. S3 runs
actual old-binary named-column reads, local inserts/upserts, counts and
cleanup against the candidate rebuilt schema on both backends, before any
remote rows are admitted. `DEFAULT 'local'` is not sufficient by itself:
§7.1's explicit non-null `owner_key` and changed conflict targets mean an old
writer that omits the key fails on the bare fragment. The compatibility
receipt must test that case; it cannot claim rolling safety from reads alone.

If a staged migration/compatibility bridge preserves those old local writes,
use it with the admission floor and document its removal boundary. Otherwise
record the failed old-writer case and use a coordinated drain/backup/upgrade
for this schema transition: stop incompatible writers before rebuild, restart
only compatible members, verify the floor, then admit sharing. The full stop
is the tested fallback, not a standing requirement for every installation or
release. Do not weaken `owner_key`, duplicate replay claims, or synthesize user
IDs merely to pass an upgrade test. S3's migration receipt fixes which path
is supported before the implementation PR can land.

**Rollback:** disable sharing, drain/cancel remote sessions and stop members.
Retain ciphertext/history for forward repair. There is no assumed in-place
old-binary rollback across this schema change; use a pre-upgrade backup and
the supported restore procedure with new identities/re-pairing. S3 records
which binary/schema combinations were exercised. A feature switch cannot
make a nullable-user row readable by an old binary.

A database restore/clone can roll back revocation state. Extend the existing
restore/import workflow to mark sharing disabled and require explicit
re-pairing/credential replacement before export/import traffic resumes. A
cloned installation must receive new server/catalogue identities and TLS keys.
Ordinary process restart does not rotate identities. Document that copying a
live data directory and keys outside the supported restore workflow cannot
be made safe by a database flag stored in the same copy.

## 11. Code ownership — where the implementation belongs

Every proposed new path is a work assignment, not an existing file claim.
Re-verify current definitions at the implementation base before editing.

| Work | Existing seam | Proposed additions |
|---|---|---|
| Domain/store | [domain.rs](../../crates/plurx-core/src/domain.rs), [store/mod.rs](../../crates/plurx-core/src/store/mod.rs), [sqlite/mod.rs](../../crates/plurx-core/src/store/sqlite/mod.rs), [hiqlite.rs](../../crates/plurx-core/src/store/hiqlite.rs) | `sharing.rs`, `store/sharing.rs`, `store/sharing_schema.sql`, SQLite/Hiqlite sharing modules |
| Secrets/import | [secrets.rs](../../crates/plurx-core/src/secrets.rs), [hiqlite_import.rs](../../crates/plurx-core/src/store/hiqlite_import.rs) | Purpose-bound secret methods, inventory and restore handling |
| Listener/runtime | [config.rs](../../crates/plurx-core/src/config.rs), [main.rs](../../crates/plurxd/src/main.rs), [state.rs](../../crates/plurxd/src/state.rs), [http/mod.rs](../../crates/plurxd/src/http/mod.rs) | `sharing/transport.rs`, `sharing/listener.rs`, distinct peer router and runtime |
| HTTP | [http/extract.rs](../../crates/plurxd/src/http/extract.rs), [http/error.rs](../../crates/plurxd/src/http/error.rs) | `http/sharing_admin.rs`, `http/sharing_peer.rs`, `http/shared_library.rs`, scoped extractor |
| Catalogue | [http/browse.rs](../../crates/plurxd/src/http/browse.rs), [http/dto.rs](../../crates/plurxd/src/http/dto.rs) | Shared DTOs, typed references, signed keyset cursors, catalogue revisions and cache |
| Playback | [http/hls.rs](../../crates/plurxd/src/http/hls.rs), [http/stream.rs](../../crates/plurxd/src/http/stream.rs), [playback_control.rs](../../crates/plurxd/src/playback_control.rs), [store/mod.rs](../../crates/plurx-core/src/store/mod.rs) | Explicit principals, existing-family ownership migration, remote-source producer and prefix relay |
| Web | [web layout map](../clients/WEB-SHELL-LAYOUT.md), [settings-developer.js](../../crates/plurxd/src/web/pages/settings-developer.js) | Settings/shared-library/shared-detail scripts, player context adapter |
| Apple | [Models.swift](../../clients/apple/Sources/Models.swift), [PlurxAPI.swift](../../clients/apple/Sources/PlurxAPI.swift), [PlayerController.swift](../../clients/apple/Sources/PlayerController.swift), [LibraryView.swift](../../clients/apple/Sources/LibraryView.swift) | Shared models/library/detail and playback adapter |
| Android | [Models.kt](../../clients/android/app/src/main/java/tv/plurx/app/data/Models.kt), [PlurxApi.kt](../../clients/android/app/src/main/java/tv/plurx/app/data/PlurxApi.kt), [PlayerScreen.kt](../../clients/android/app/src/main/java/tv/plurx/app/player/PlayerScreen.kt), [LibraryScreen.kt](../../clients/android/app/src/main/java/tv/plurx/app/ui/LibraryScreen.kt) | Shared models/library/detail and playback adapter |

Do not reuse [peer_transport.rs](../../crates/plurxd/src/http/peer_transport.rs)
with cluster credentials for the foreign-server leg. A dedicated sharing
client may reuse low-level connection-budget ideas. Server-local cluster RPC
continues to use its own authority inside A or B.

## 12. Build sequence — reviewable milestones on one effort

Create `effort/shared-libraries` from current intended main after review.
Each `codex/sharing-sN-*` branch starts from the current effort and targets it.
Files overlap, so the disjoint-file/main exception is not applicable. Integrate
milestones serially against the effort's current head.

| Step | Scope and boundary | Required acceptance |
|---|---|---|
| S0 | contract ready to build | Original `4e81bc38c`, revision `eb9038755`, PR #740 | Opus re-review corrections incorporated in §16; runtime evidence is recorded per milestone |
| S1 | merged into effort | [PR #746](http://forge.lan:3000/noirr/plurx/pulls/746), `aaafc1a0f5` | SQLite v92 / Hiqlite v70; purpose-bound secrets; pairing/rotation/assignment transactions. Five store contracts, 11 sharing unit tests, schema migration parity and 31 import-inventory tests passed; workspace Clippy and catalog lint passed. Run 3890 and the final Effort development gate passed on the exact candidate; landed in the effort as `971265536a` with all seven regression fields. |
| S2 | Serve/loopback deployment, TLS provisioning/renewal, pinned egress, peer/admin routes, Developer switch | Two NATed servers over Tailscale; no public/LAN listener; TLS mismatch, VPN loss, wrong port/route, redirect/proxy and broad-grant cases |
| S3 | Playback principal migration in existing family and cluster schema floor | Owner-key PK/upsert/count parity; local/shared replay races; user/grant deletion; old-writer migration receipt; prepared successor, cluster ownership and ordinary-router refusal |
| S4 | Shared catalogue, live keyset paging, batch metadata, scoped art, assignments and local history | Huge IDs, same IDs on two sources, paging completes during continuous metadata writes, moved sort keys, expiry/deletion, denied cache reads, progress ordering |
| S5 | Source start/decision, B relay, full playback wire/prefix relay, cleanup and limits | Direct/HLS/transcode, Range/416, subtitles/HDR, seek/quality/track replacement, ambiguous start, logout/revoke during open body, bounded memory |
| S6 | Web settings/library/detail/player/Continue Watching integration | Complete browser journey, file-context call-site census/collision fixture, pre-session thumbnails; assets/order/input/surface contracts |
| S7 | Apple and Android integrations, platform tests and builds | Matching full-wire/file-context fixtures and call-site census; UUID/prepared/status/stop controls; actual Apple TV/Google TV playback |
| S8 | Cluster/NAT/resource qualification, operator docs and promotion | Matrix in §13 complete; current candidate qualified under the current development pipeline; Developer graduation |

S1–S5 may carry executable server fixtures before any client UI. A milestone
is not complete because it compiles. Each PR names the smallest behavior
regressions it executed and includes `Regression-Test: <path>::<test name>`
lines; preserve those in landing commits. Behavior changes use `fix(` or
`perf(` per [AGENTS.md](../../AGENTS.md), including this new visible behavior.

Use the actual current [development pipeline](../DEVELOPMENT_PIPELINE.md),
including its amendments, for draft review, effort checks and promotion.
Do not infer automatic shipping from a passing task gate. Refresh source and
requalify after integrating current main; evidence from an older base does
not qualify the final candidate. No deploy is part of this document task.

## 13. Validation — prove the trust boundary and the watching experience

### 13.1 Shared fixtures and focused regressions

Create `tests/sharing/protocol-cases.json` and consume it from Rust, web,
Swift and Kotlin tests. Cases carry identity, grant generations, input, expected
outcome and expected source position. Fixtures contain synthetic secrets and
addresses only. Create a disposable two-daemon runner using isolated state
and generated test media; no production account, tailnet, media path or
credential is assumed.

Required regression cases, with stable test names in the implementation:

| Group | Cases that must fail on the broken behavior |
|---|---|
| Pairing | Two simultaneous claimants; retry after committed response loss; expired/cancelled invite; wrong TLS pin; unconfirmed recipient; pending credential browsing |
| Authorization | Unexported library/item/file/art; cross-grant session ID; assignment removal; scope narrowing vs expansion/rotation; deleted/recreated user; access through legacy/Plex/cluster routes |
| Secrets | Redacted debug/log/error/body; ciphertext cross-import/server/purpose substitution; key absent; rotated old token denied except status; snapshot contains no cleartext |
| Catalogue | IDs above 2^53; same ID from two sources; source epoch change; paging liveness during continuous metadata writes; sort-key movement and display dedup; deleted item; cursor substitution; cursor revision races/cache bounds; denied cached art |
| Playback | Duplicate request after timeout; concurrent viewers; same playback ID in different grants; source revision changes; seek origin; direct Range/416; rendition map; malformed/foreign playlist URI |
| Recovery | Old prepared response; replacement cancel/commit; source loss; receiver ingress change; VPN removal; revoked session on a stale/minority node; no local-user fallback |
| Progress | No frame means no zero save; stale sequence; old session after new one; manual unwatched vs late beat; separate households; next episode reauthorizes |
| Resources | Slow reader; blocked upstream; 16-request cap; aggregate multi-node start cap; retired-generation resource denial; cancelled encoder and relay cleanup |

Proposed focused suite anchors: Rust test modules `sharing` and
`sharing_playback`, web `tests/web/shared-libraries.test.js`, Swift
`SharedLibrariesTests`, Kotlin `SharedLibrariesTest`. These do not exist yet.
Sol must record exact executed paths/names and nonzero test counts; a filter
matching zero tests is not evidence.

#### Shared protocol fixture parity matrix (2026-10-04)

Audit of `tests/sharing/protocol-cases.json` (version 2) against the table
above and the wires added since. The §13.1 groups are behaviour, not wire
shapes: Pairing, Authorization, Secrets, Progress and most of Catalogue,
Recovery and Resources are proved by the Store contract and daemon suites
(`store_contract sharing_*`, plurxd `sharing_*`) and have no fixture row. What
a row can carry is the wire every implementation must agree on: IDs above
2^53 (Catalogue), malformed or foreign playlist URIs and the seek origin
(Playback), old prepared answers and Source loss (Recovery, as preparation and
refusal rows), and same-viewer concurrent sessions (per-session playback ids).

A row's `layer` names who validates it: `b` the receiver, `client` a player,
`both` both. "yes" means the named test reads every row of the group for that
layer; "B-only" means no client parses that wire.

| Fixture group | Rows | Rust | Web | Swift | Kotlin |
|---|---|---|---|---|---|
| `cases` Source IDs | 12 | yes, core `sharing_shared_protocol_fixture_validates_exact_wire_ids` | yes, `playbackFileDecimal` | yes, `SharedProtocolFixtureTests` | yes, `SharedProtocolCasesTest` (`canonicalId`) |
| `status_tokens` × `shared_status.word_fields` | 12 × 9 | yes, `sharing_protocol_fixture_status_grammar` | yes, `sharedPlaybackStatusMetrics` | yes | yes, `SharedPlaybackStatus.isToken` and `decode` |
| `shared_status` accepted + mutations | 1 + 21 (b 10, client 17) | decode and b rows: yes. B's emitted envelope: yes, `sharing_protocol_fixture_receiver_status_envelope` (`shared_status_body`) | yes, including the exact key set, `incarnation_id` and `control_epoch` | yes (client 17) | yes, `SharedPlaybackStatus.decode` |
| `control_refusals.source` | 14 (7 valid) | validity and Source minting: yes, `sharing_protocol_fixture_source_control_refusals`. B's mapped answer (`b`): yes, `sharing_protocol_fixture_receiver_refusals` (`refusal_response`) | client outcome (`client`): yes | client outcome: yes | client outcome: yes, `SharedControlChannel` |
| `control_refusals.b_precheck` | 4 | yes, `sharing_protocol_fixture_receiver_control_precheck` (`receiver_control_precheck`) | client outcome: yes | client outcome: yes | client outcome: yes, `SharedControlChannel` |
| `control_preparation` (`none`) | 3 | yes, `sharing_protocol_fixture_receiver_preparation_rebind` (`rebind_to_receiver`) | yes, `settlePreparedOfferWaiter` | yes (`SharedControlStep`, `PreparedOfferWait`) | yes, `PreparedOfferWait` with the Shared rule |
| `hls_start` public + mutations | 1 + 17 (b 15, client 16) | yes, `sharing_protocol_fixture_hls_start_projection` | yes, `sharedPlaybackStartContext` | yes (client 16; fixed `unsafe-duration`) | yes, `SharedStart.bindInitial` |
| `direct` MIME set, public + mutations | 13; 1 + 16 (b 9, client 13) | yes, `sharing_protocol_fixture_direct_start` | yes, `sharedPlaybackDirectStartContext` | yes (client 13) | yes, `SharedStartedDirect.decode` |
| `direct_session_query` | 11 | yes, `sharing_protocol_fixture_direct_session_query` | yes | yes | yes |
| `file_suffixes` (route class `b`) | 25 | yes, core `sharing_protocol_fixture_file_suffixes` | yes, `playbackFileUrl` | yes (5 rows fixed) | yes, `PlaybackFileContext.path` (fixed 2026-10-04) |
| `asset_session_query` | 14 | yes, `sharing_protocol_fixture_asset_session_query` | B-only parser; web composes the bound query (next row) | B-only | B-only |
| `presession_assets` | 4 | yes, same test | yes, `playbackFileUrl` bound and unbound | yes, bound and unbound | yes, `PlaybackFileContext.path` |
| `resource_unsupported` (typed 422) | 1 | yes, `sharing_protocol_fixture_resource_unsupported` | B-only | B-only | B-only |
| `playback_ids` (per-session) | 1 | yes, `sharing_protocol_fixture_per_session_playback_ids` | B-only (B to Source) | B-only | B-only |
| `receiver_recovery` | 1 + 4 rendered | yes, `sharing_protocol_fixture_receiver_recovery_status` | yes, `sharingRecoveryHTML` | B-only (web node card) | B-only |

Executed 2026-10-04 on the pinned toolchain: plurx-core `--lib` filters
`sharing_protocol sharing_shared_protocol sharing_file_resources
sharing_wire_ids` 4 passed; plurxd `--bin plurxd -- sharing_protocol_fixture`
9 passed (27 with the touched modules' existing tests); `node --test
tests/web/sharing-protocol-cases.test.js` 9 passed; clippy `-D warnings`
clean.

The three Rust cells this audit left **open** are closed (2026-10-04, see
"Web Shared prepared successor and fixture-driven B wire" below):
`shared_status_body` and `receiver_control_precheck` are pure functions in
`http/shared_receiver_control.rs`, and the fixture drives them,
`refusal_response` and `rebind_to_receiver`.

No Swift test reads the fixture yet. `subs/[0-9]{1,6}` and
`chapters/[0-9]{1,6}/thumb` in `PlaybackFileContext.swift` accept
`subs/4096`, `subs/4096.vtt`, `subs/4096/overlay.json`,
`chapters/4096/thumb` and `subs/01`, which the Shared grammar refuses, so
those five `file_suffixes` rows fail there until the Shared context bounds
canonical indexes at 0..4095. Kotlin had the same hole; it was fixed and the
Kotlin column filled on 2026-10-04 (see "Android Shared prepared successor,
catalogue actions and fixture parity"). The web status binding
was looser than Swift's (no exact outer key set, `incarnation_id` or
`control_epoch`); it now matches, with four client rows for it.

Fixes this audit made: `project_shared_start` accepted a rolling-lease Start
that every client refuses, so a Source answering `vod: true` with a 60 s lease
reached the player as a Start it then rejected; it now requires the VOD lease
and `vod: true` itself (rows `rolling-lease`, `vod-false`; its callers already
refused `vod: false`). Web `playbackFileUrl` built Shared track and
chapter indexes above 4095, and web Shared status accepted prose, paths and
markup in word fields.

### 13.2 Commands and compiler discipline

Before Rust edits, verify the repository pin, not Homebrew's default:

```bash
rustup run 1.97.1 rustc --version
rustup run 1.97.1 cargo check --workspace --locked --all-targets
```

On the documentation host, the pinned compiler was found at
`~/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin/rustc` while
unqualified `rustc` selected Homebrew 1.98.0. That is a dated observation, not
a portable path requirement. Use [AGENT-COMPILE-LOOP.md](../ci/AGENT-COMPILE-LOOP.md)
when the build host cannot run the pin. Archive committed source only; no
repository credentials or `.git` enter the compiler container.

For affected milestones, with PATH selecting the pinned toolchain:

```bash
cargo fmt --all --check
cargo check --workspace --locked --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --locked -p plurx-core --features hiqlite-store sharing -- --nocapture
cargo test --locked -p plurxd sharing -- --nocapture
python3 -m unittest discover -s tests/operations -p test_docs_index.py
```

For S3 and promotion, run the relevant existing playback regressions as well
as new tests, and the repository's required broader gates. `make unit-core`
is the full core fallback. Run backend contract tests against both SQLite
and actual Hiqlite, not only mocked Store returns. For S6 use the focused web
suite and required web asset/settings/input contracts. For S7 use `make
apple-build`, focused XCTest runs via the existing test workflow, `make
android-test` and affected Android compilation; the Makefile is authoritative
for current destinations and dependencies.

### 13.3 Physical and multi-server acceptance matrix

| Dimension | Required observations |
|---|---|
| Network | Two separately NATed homes; direct Tailscale path; forced relay path; source Tailscale stopped; recipient Tailscale stopped; non-recipient denied; recipient device without Cinema credential denied media |
| Accounts | Separate tailnet user-owned server share; supported restricted common-tailnet tagged setup; reciprocal grants without LAN access |
| Hosting | Linux bare-host Serve and Docker 28+ bridge/loopback/host egress; startup ordering, LAN denial; other hosts separately qualified |
| Player | Browser; iPhone/iPad; physical Apple TV; Android phone; physical Google TV |
| Delivery | Direct play; remux/HLS; video transcode; native subtitles; burned subtitles; audio selection; seek; quality replacement; HDR-capable and SDR clients |
| Cluster | A and B standalone; clustered A; clustered B; both clustered; serving-node loss; minority/quorum loss; authenticated endpoint addition; unknown pin rejected; remote-source takeover only on eligible B nodes |
| Revocation | A revoke; A removes library; B removes viewer; B logout/user delete; feature off; each during long direct response and HLS playback |
| Capacity | Per-grant four and source-wide eight slots across nodes; preparations count; slow reader memory bound; high-bitrate stream measured on direct and relay paths |

Use reproducible test media with known duration, keyframes, subtitles and
source timeline. Do not claim Dolby Vision/audio behavior from codec labels
alone. Measure 10-minute sustained playback per transport path, source/relay
bytes, drift of source position after seek, time to first frame, stalls,
peak relay buffer memory, CPU and active encoder count. Report actual bitrate
and throughput; there is no unconditional 4K promise on relay fallback.

For each revoke drill record commit timestamp, node authority-lease expiry,
last new source byte, last new B byte and cleanup time. Both serving bounds
must be ≤30 seconds. Previously buffered playback is explicitly excluded.
All evidence identifies source/receiver/client build revisions, transport
mode, topology, fixture, command and result. Store sanitized evidence using
the repository's existing evidence conventions; index new prose documents.

## 14. Observability and operations — tell the operator what failed

Expose local admin status for listener availability, Serve configuration, egress
readiness, node-key expiry, certificate expiry/fingerprint, supported protocol, import state, last
successful peer RPC, active sessions and last typed failure. Remote peer
names are escaped and length-bounded. Normal users see actionable availability,
not peer addresses, keys, IDs or infrastructure details.

Proposed metrics use fixed low-cardinality labels: RPC totals by operation
and outcome; active sharing sessions by direction; relay bytes; bounded queue
utilization; authority-lease expiry/cancellation totals; start reconciliation
outcomes. Do not label Prometheus metrics with user, title, address, grant,
capability or peer name. Admin activity may show a redacted peer label and
session ID behind existing administrator authorization.

Update [API.md](../API.md), [SECURITY.md](../SECURITY.md),
[OPERATIONS.md](../OPERATIONS.md), [FEATURES.md](../FEATURES.md) and
[CLIENTS.md](../CLIENTS.md) as behavior lands, not as though this plan were
already shipped. The operator guide needs two distinct setup steps (network
share, Cinema grant), effective network policy checks, pin renewal, recovery
from key loss, reconnect/re-pair, disable/revoke, backups/restores and the
supported host/client matrix. No credentials in command examples or screenshots.

## 15. Opus review brief and Sol handoff

### 15.1 Opus — review the design before implementation

Review this file against the cited code and the current base. Return ranked,
actionable findings with section references, a concrete failing sequence and
a proposed correction. Separate blocking protocol/security/correctness gaps
from tuning and later scope. In particular, attack:

1. Cross-tailnet Tailscale sharing, tagged-node restrictions, ingress binding,
   proxy/DNS/route escape, and whether VPN loss really closes every path.
2. Pinned TLS provisioning/renewal, invitation theft, claim races and uncertain
   outcomes, owner confirmation, encrypted secret replication and rotation.
3. The typed playback-principal seam in the existing storage family: completeness,
   migration cost, local behavior preservation, prepared replacement and
   accidental access through legacy capabilities.
4. Resource translation across native HLS variants, direct ranges, captions,
   source positions, malformed metadata and capacity exhaustion.
5. Revocation while blocked in an open response, stale clustered readers,
   receiver ingress change, source endpoint failover and rollback/restore.
6. Live keyset pagination and continuous-scan liveness, remote identity collision, per-user history,
   late progress and next-episode authorization.
7. Whether the milestones are buildable and acceptance tests expose the above
   failures; identify every unresolved interface that would force Sol to invent
   a second protocol halfway through the effort.

The deliverable of this review brief is findings and contract corrections,
not an implementation.
Record finding ID, severity, section, disposition and evidence in a review
section appended here, or a companion review document indexed in the same
commit. Resolve blockers and reconcile contradictory text before marking
this ready to build. Paul's confirmed transport/scope decisions remain fixed.

### 15.2 Sol — implement the reviewed contract

Start from the reviewed commit and current intended main. Read
[AGENTS.md](../../AGENTS.md), this document, its review dispositions and the
[development pipeline](../DEVELOPMENT_PIPELINE.md). Establish the pinned
compiler loop first, create the effort, and execute S1–S8 in order. Use each
milestone's acceptance criteria as its completion boundary. Record exact
base/head, focused test names, commands and outcomes in PRs and the ledger.

Do not mark a feature complete from static source tests, a mock transport,
a single browser, or compilation alone. Do not widen authority or publish
Cinema to obtain a green network test. Report a necessary contract change
with the failing case and proposed revision. Preserve unrelated work, commit
normally with the tracked hook, and leave no task-created temporary files or
uncommitted documentation. No deployment or production Tailscale changes are
implied by the build handoff.

### 15.3 Execution ledger

| Milestone | State | PR / commit | Evidence / outstanding work |
|---|---|---|---|
| S0 | contract ready to build | Original `4e81bc38c`, revision `eb9038755`, PR #740 | Opus re-review corrections incorporated in §16; runtime evidence is recorded per milestone |
| S1 | merged into effort | [PR #746](http://forge.lan:3000/noirr/plurx/pulls/746), `aaafc1a0f5` | SQLite v92 / Hiqlite v70; purpose-bound secrets; pairing/rotation/assignment transactions. Five store contracts, 11 sharing unit tests, schema migration parity and 31 import-inventory tests passed; workspace Clippy and catalog lint passed. Run 3890 and the final Effort development gate passed on the exact candidate; landed in the effort as `971265536a` with all seven regression fields. |
| S2 | implementation in progress; topology qualification open | [draft PR #759](http://forge.lan:3000/noirr/plurx/pulls/759) | Dedicated loopback TLS transport, pinned direct dialing and fixed Tailscale DNS, isolated peer/admin routes, durable claim/rotation recovery, authenticated endpoint refresh and advisory Developer switch implemented. Two-NAT, shared-machine Serve and Docker isolation/egress receipts remain open; S2 is not complete. |
| S3 | implementation started; ownership migration open | `codex/sharing-s3-principals` (unpublished) | Canonical Local writers, complete principal reads and seven-table candidate rebuild implemented; ownership and actual-voter lifecycle regressions passed. Caller refusal and retained-read census are qualified; Shared grant/scope admission, migration installation and coordinated upgrade qualification remain open; §16.4 records the boundaries. |
| S4 | source catalogue and private history candidates implemented; qualification pending | `codex/sharing-s4-catalogue` | Consistent live keysets and batch metadata, candidate order maintenance and durable item identities, peer metadata routes and receiver-only ordered history. Viewer/cache/artwork/details, activation floor and qualification remain open. |
| S5 | bounded resource grammar candidate built; relay and admission open | `codex/sharing-s3-principals` (unpublished) | Current-engine relative HLS grammar and generator/escape regressions passed; no Shared producer, relay or signed file locator is enabled. |
| S6 | file-context foundation integrated; shared UI open | `codex/sharing-s6-file-context` through `4865de285` | Exact Local IDs and immutable full-reference, account-scoped B file contexts cover current file URL callers; full web checks pass. Shared browse/settings/detail/player integration and actual relay playback remain open. |
| S7 | native ownership census and baseline compiler loops started | `codex/sharing-s7-native-file-context` | Apple/Android file-context and authorization-generation implementation is in progress; no native Shared playback receipt exists. |
| S8 | not started | — | — |

## 16. Opus S0 dispositions — corrections are not runtime evidence

The [2026-10-02 review](SHARED-LIBRARIES-REVIEW.md) remains the record of the
first draft. The [re-review](SHARED-LIBRARIES-RE-REVIEW.md) accepts its
dispositions subject to SL-18–24. Those corrections are incorporated below,
meeting its condition to mark the contract ready to build. This is design
readiness, not a claim that implementation or S2 topology experiments passed.
No additional agent review is required by this contract. SL-08 is explicitly
an engineering default preserving current behavior, not a recorded Paul ruling.

| Finding | Disposition and acceptance owner |
|---|---|
| SL-01 · blocker | Revised §2 to raw TCP Serve and loopback publication; S2 must prove shared-user Serve reach, Docker isolation/start order and real container egress. Topology qualification remains open. |
| SL-02 · blocker | Revised §2/§3/§5: FQDN/address hints, recipient-side override, generation-CAS endpoint edit, pin before secrets. S2 tests address remap and resolver/route failure. |
| SL-03 · blocker | Revised §7.1/§10: one session family, canonical owner key, exact-one principal and full key/query/upsert census; §10 qualifies upgrade compatibility. S3 proves local isolation, relay and takeover. |
| SL-04 · blocker | Revised §5/§7: complete existing StartResponse/decision wire, ordinary B HLS namespace and file_base for all file/PGS/chapter routes. S5/S7 prove all player fields and side resources. |
| SL-05 · major | Revised §3/§4/§9: separate scope, credential, catalogue and admin-CAS counters; only loss of effective authority stops playback. S1/S5 exercise rotation and expansion mid-film. |
| SL-06 · major | Revised §7.2/§10: B local-user session with remote_source producer and upstream side row; eligible-node placement/takeover. Login revocation is explicit S5 work, not assumed inherited. |
| SL-07 · major | Revised §6: signed keyset cursors, no replicated snapshots. SL-20 refines liveness: resume on order changes, suppress duplicates at B, and allow moved items to appear on the next open. S4 tests continuous scans. |
| SL-08 · major | Engineering default (a), §3.1, not an explicit Paul decision: preserve whole-server startup refusal for missing/mismatched wrapping key; generalize purpose census. S1 proves sharing and Trakt behavior. |
| SL-09 · major | Revised §2: network reach is user-granular; Cinema credential authorizes logical server; corrected deny cases and operator key-expiry guidance. S2 records actual policy results. |
| SL-10 · major | Revised §2: generated node key, same-key certificate renewal, credentialed pinned endpoint manifests and explicit lost-pin recovery. S2 tests renewal and added/replaced nodes. |
| SL-11 · major | Revised §5/§6: batch metadata ≤200 IDs, per-ID authorization, bounded concurrency and independent Home sections. S4 tests large/offline history lists. |
| SL-12 · medium | Revised §7/§9: disable source/receiver LAN priors for shared starts; atomic source-wide eight-slot cap plus existing limits. S5 measures ABR and admission races. |
| SL-13 · medium | Revised §7.3: B relay ID is capability; file-grants login binding and hash verifier; no separate delivery renewal. S5 proves logout on every ingress/open body. |
| SL-14 · medium | Revised §3/§4/§5: explicit assignment/lifecycle/endpoint counters, verify identity before uniqueness insertion, explicit re-pair, restrictive library FK plus atomic scope change. S1 tests collisions and delete bypass. |
| SL-15 · low | Revised §9: derive deadlines from engine symbols plus named margins; S5 tests ordering. |
| SL-16 · low | Revised §2.3/§13: observe failed connections and bounded body cancellation, not interface disappearance. S2 records VPN stop/restart. |
| SL-17 · process | Documents isolated on codex/shared-libraries-review from main 15e36f7f4 for their own docs PR; no unrelated player changes. Portable home-path example. Landing remains subject to review and repository gates. |

### 16.1 Re-review corrections — SL-18 through SL-24

| Finding | Disposition and acceptance owner |
|---|---|
| SL-18 · blocker | Corrected §5/§7/§8: B uses ordinary HLS routes and UUIDs; all file URLs use the shared file_base context, with pre-session routes and explicit native delivery binding. S5–S7 prove prepared/control/status/stop and A/B numeric-ID collisions. |
| SL-19 · blocker | Corrected §7.1: non-null canonical owner_key in every principal PK, conflict target, count and replay path; checked projections, no partial-index/null ownership trick. S3 proves concurrent retries allocate one incarnation and admission counts sharing rows. |
| SL-20 · major | Corrected §6: metadata-only changes do not bump order revision; changed order resumes after the signed boundary. B suppresses duplicate references; moved-behind items may wait for next open. S4 proves paging liveness under continuous scan writes. |
| SL-21 · major | Corrected §7.1: no new session ownership FKs; explicit revocation/retention cleanup. S3 proves user and grant deletion with active/ended/ack rows and no B export lookup for a local relay. |
| SL-22 · medium | Corrected §10: all-member admission floor and old-binary read/write/upsert tests. Full stop only on failed compatibility evidence; explicit owner_key means default-local alone is not compatibility. S3 records the supported migration path. |
| SL-23 · low | Corrected §2.1: certificate notBefore is backdated one hour on initial issue and renewal; validity still enforced. S2 tests a receiver behind by several minutes, expiry and excessive skew. |
| SL-24 · low | Corrected §8: one file_base context and per-platform census shared by S6/S7; source-contract tests and behavioral file-ID collision fixtures required. |

Process correction: `eb9038755` was already pushed as draft PR #740 when the
re-review was supplied; its statement that the branch was unpushed is stale.
The draft contains only sharing docs and their index. No implementation,
merge, deployment, or production network change is part of these corrections.

### 16.2 Implementation receipt — S1, 2026-10-02

This implementation starts from main `9a719fcb73da195676a6dea5378871d64547bb8b`.
Only the four reviewed sharing documents at `db1ea8a20` and their index rows
were carried into the task; no historical player branch is its base. Primary
checkout changes and the documentation review checkout remain untouched.
Rust `1.97.1 (8bab26f4f 2026-07-14)` passed the untouched workspace compiler
loop before implementation evidence was collected.

**Built:** canonical decimal source identities; endpoint validation; bounded,
redacted invitations; shared SQLite/Hiqlite SQL for claim, exact retry,
approval, expiry, scope CAS, rotation, import and viewer assignments. New
outbound envelopes bind purpose, local server and import; startup and backup
key censuses include sharing. The portable restore path disables old sharing
authority and resets sharing identity while retaining private watch history.
Library deletion advances export counters atomically before removing links.
The restrictive FK rejects deletion that bypasses that transaction.

**Observed:** five store contracts passed with nonzero counts; the four
backend-neutral scenarios ran against both SQLite modes and three real Hiqlite
voters, and a populated SQLite-to-Hiqlite import preserved sharing identity,
sealed credentials and viewer authorization. Eleven sharing unit tests passed,
including ciphertext substitution, redaction, huge IDs, invitation bounds,
reused numeric user ID, restore fencing, FK refusal and actual startup refusal
with only sharing credentials. Bootstrap and the frozen-v42 migration chain
produced identical schemas. Thirty-one import tests and twenty-nine census tests
passed. Workspace Clippy with denied warnings and validation catalog lint passed.

The focused commands were:

```sh
cargo test --locked -p plurx-core --features hiqlite-contract-tests --test store_contract sharing -- --nocapture
cargo test --locked -p plurx-core --features hiqlite-store --lib sharing -- --nocapture
cargo test --locked -p plurx-core --features hiqlite-store --lib store::hiqlite_import::tests -- --nocapture
cargo test --locked -p plurx-core --features hiqlite-store --lib census -- --nocapture
cargo test --locked -p plurx-core --features hiqlite-contract-tests --test store_contract fresh_bootstrap_matches_the_migration_chain_from_a_frozen_v42_tree -- --exact --nocapture
cargo clippy --workspace --all-targets --locked -- -D warnings
make validation-lint
python3 -m unittest discover -s tests/operations -p test_docs_index.py
```

The replicated harness needed the local sandbox's loopback port-bind allowance.
No production process, firewall or Tailscale setting was used. Regression tests
also prove a consumed-invitation replay cannot widen narrowed scope, and an
unchanged scope does not advance the catalogue generation.

**Task:** [S1 PR #746](http://forge.lan:3000/noirr/plurx/pulls/746) targets
`effort/shared-libraries`. Manual effort run 3884 refused its first catalog
mapping: the two new store adapters must also select `cluster.auth` in the
CI scope resolver. That mapping was corrected, and all 253 validation tests
passed locally (one platform-specific skip). The runtime source is unchanged.

Run 3887 passed policy, web, Apple and Android checks, but its Rust job
rejected the isolated Hiqlite spike lockfile before compilation. Commit
`aaafc1a0f5af031c3adbfe6760ad4edcff7cb082` synchronizes that lockfile;
`make spike-lock-check` and the normal hook passed locally. Exact-candidate
[run 3890](http://forge.lan:3000/noirr/plurx/actions/runs/3890) has passed
scope, policy, Rust, web, Apple and Android. Windows attempt 1 reached its
30-minute runner deadline without a compiler error. The individual retry on
the same commit passed in 20 minutes 17 seconds; the final Effort development
gate passed. PR #746 integrated that candidate into the effort as
`971265536a259dea38b0f7a9a8752a5a74e8c025`, preserving all seven regression fields.

**Still owed:** S2 requires disposable two-NAT/Tailscale
and Docker profiles; S7/S8 require physical Apple TV/Google TV and the
cluster/resource matrix. No network, shared playback, native client, promotion
or Developer graduation evidence is claimed by S1. S2–S8 remain work after
the completed S1 integration; no deployment is authorized.

### 16.3 Implementation progress — S2, 2026-10-02

The unpublished S2 worktree preserves the primary checkout. S1 passed the
Effort development gate on `aaafc1a0f5` and landed through PR #746 as
`971265536a`, carrying all seven regression fields. S2 integrated that exact
effort base in `9db1949b4`; its normal tracked hook passed. The final S2
candidate still needs its own exact-tree checks and task gate before landing.

**Implemented:** node-local TLS key provisioning and same-key renewal;
strict SPKI, validity, server-auth and signature checks before capabilities;
a bounded loopback listener isolated from ordinary Cinema routes; fixed
Tailscale DNS with bounded binary parsing and same-resolver TCP fallback;
validated numeric fallback and qualified egress binding; closed HTTP requests
that never follow redirects or consult proxy environment variables. The
admin endpoint manifest is read and updated through
`GET` / `PUT /api/v1/sharing/endpoints`; updates require `expected_revision`.
A source manifest is accepted only through an authenticated active peer and
uses both the source revision and the local endpoint generation.

Recipient claim metadata persists the original recipient name and confirmed
pending expiry inside the purpose-bound credential envelope. Repeated claims
retain the same digest, restart recovery retains expiry, and delayed replies
cannot replace a newer lifecycle's credential or downgrade active authority.
Rotation persists the replacement before the upstream swap and recovers
through the old credential's status-only route. Private HTTP responses carry
`Cache-Control: no-store`, including errors and unsupported protocol majors.
The Developer switch saves either choice while readiness remains advisory.

**Observed so far:** the core sharing filter passed 20 tests, including seven
TLS cases and two DNS cases. The updated endpoint/re-pair/receipt contract passed
against both SQLite modes and three real Hiqlite voters (one named scenario,
9.76 seconds). The focused daemon filter passed eight tests (seven sharing
cases and one pre-existing playback case), including secret-safe errors,
private-route isolation, cache headers, invalid library selections, approval,
rotation and sealed restart metadata. Developer section tests passed 36/36;
`web-types` preserves the existing baseline. Workspace Clippy with denied
warnings passed. The native test linker warns about its large unwind table;
the tests completed successfully. Final exact-tree commands and nonzero
counts will be recorded again after porting to the gated effort. These
component tests do not constitute a topology or physical-device receipt.

```sh
cargo test --locked -p plurx-core --features hiqlite-contract-tests --test store_contract sharing_endpoint_cas_and_re_pair_preserve_private_viewer_identity -- --nocapture
cargo test --locked -p plurxd --bin plurxd sharing_ -- --nocapture
cargo test --locked -p plurx-core --features hiqlite-store --lib sharing_ -- --nocapture
cargo clippy --locked --workspace --all-targets -- -D warnings
scripts/web-types
node --test tests/web/settings-sections.test.js
make spike-lock-check
python3 -m unittest discover -s tests/operations -p test_docs_index.py
```

**Pinned transport receipt:** local checkpoint `6c5f7b8d7` passed the explicitly
invoked `sharing_pinned_transport_recovers_committed_claim_and_rotation_after_restart`
case: one test, zero ignored, 0.55 seconds. An internal-only Docker CGNAT
network supplied the numeric fixture address; no ports were published and
no production Tailscale setting was changed. The production endpoint
validator, DNS fallback, pin/identity checks and HTTP client handled source
commits whose responses were deliberately lost. Recreated recipient managers
recovered the exact claim despite a recipient rename, observed approval,
and settled a committed rotation through the old status-only credential.
The SQLite stores remain in the test process; this is not an actual daemon
process restart or a two-NAT/Tailscale receipt.

The source-only compiler verified Rust 1.97.1. Linux core Clippy, formatting
and workspace compilation passed on `3dfdf1a15`; the Linux-only fixture
subsequently type-checked. Its initial ARM test build required `+fp16` for the
existing GEMM dependency, and a two-job build was killed by the disposable
VM. The successful run used one job, `RUSTFLAGS='-C target-feature=+fp16'`,
`CARGO_PROFILE_TEST_DEBUG=0`, `--locked --offline`, and the exact committed
archive. These flags provide functional component evidence, not resource
qualification.

```sh
PLURX_SHARING_FIXTURE_IP=100.127.88.2 cargo test --locked --offline -p plurxd --bin plurxd http::tests::sharing_pinned_transport_recovers_committed_claim_and_rotation_after_restart -- --ignored --exact --nocapture
```

**Still owed:** complete per-node availability
and deployment diagnostics;
shared-machine raw TCP Serve, two NATs, Docker bridge egress/isolation and
startup ordering; the final task gate. The current listener refuses unqualified
non-loopback profiles. S3–S8, native playback and promotion remain unfinished.


**Capacity and node attribution:** invitation/import limit refusals now expose
`sharing_capacity` with HTTP 429. Admission remains inside the atomic store
write; a subsequent diagnostic read never grants an extra slot. The focused
`sharing_capacity_refusals_preserve_existing_authority_and_reopen_expired_slots`
regression passed on both SQLite modes and three real Hiqlite voters (one
scenario, zero ignored, 9.44 seconds). It covers duplicate authority at the
limit and canceled/expired invitation slot reuse. Admin status identifies
`node_id`, `observed_at_ms` and `observation_scope: local_node`; it does not
present a process-local observation as another member's readiness. Host
Serve and node-key expiry stay unknown without operator evidence.


On the integrated effort base, the native daemon `sharing_` filter passed
8/8 (zero ignored, 1.27 seconds), including the node-attribution assertions.
The core `sharing_` filter passed 20/20 (zero ignored, 1.19 seconds) after
allowing the disposable TLS loopback listeners; the sandboxed first run
refused those listeners and is not the passing receipt. Rust 1.97.1 was
verified explicitly. Docs index passed 4/4 and Developer section tests 36/36.

The complete sharing store filter passed 7/7 scenarios (zero ignored, 63.80
seconds), covering both SQLite modes, three-voter authority, populated import,
endpoint CAS/re-pair and the new capacity refusal contract.

**S2 ownership review, 2026-10-02:** run 3924 on `aeb0d7b91` stopped at the
module-wide task/timer/process-shape inventory before compilation. The new
transport sites are now inventoried: daemon listener/claim loops belong to
shutdown; blocking TLS work is awaited; the Hyper connection driver belongs
to its abort-on-drop peer; eight timers bound those owners. Eight additional
status-shaped calls launch no process. The response-loss fixture now owns its
accepted connections in a JoinSet, and client fixture servers are aborted and
awaited. The seven ownership-inventory tests passed after this review. The
changed candidate must pass its focused transport tests and a fresh effort gate.

**Separate-process restart regression:**
`crates/plurxd/tests/sharing_daemon_restart.rs` starts the shipped daemon twice
with separate disposable stores, pairs through a raw TCP forwarding fixture
while retaining the production TLS pin, and restarts the recipient with a
pending grant. It compares the persisted pairing identity, approves the grant,
rotates its credential, restarts both processes and authenticates a new
endpoint manifest revision with the recovered credential. It checks that source identity,
catalogue epoch, grant/import IDs and SPKI remain stable and that no duplicate
export appears. Rust 1.97.1 native compilation and denied-warning Clippy passed.
The explicit Linux CGNAT execution passed on committed `440f5370a` with
Rust 1.97.1 (one test, zero ignored, 34.17 seconds). The response-loss fixture
also passed on that same candidate (one test, zero ignored, 0.42 seconds). The revision is published
after both restarts, so retained state cannot satisfy the authentication
assertion. An earlier fixture attempted a second rotation during the source's
ten-minute receipt window and correctly encountered conflict; the Store
contract expressly refuses that request. Client dispatch readiness is now
awaited within its existing deadline, and rotation diagnostics expose only
fixed phases and typed errors. No Tailscale or topology receipt is implied by
this forwarding fixture.

```sh
PLURX_SHARING_FIXTURE_IP=100.127.88.2 cargo test --locked -p plurxd --test sharing_daemon_restart sharing_separate_daemons_preserve_pending_pairing_and_rotation_across_restart -- --ignored --exact --nocapture
```

### 16.4 Implementation progress — S3, 2026-10-02

The unpublished S3 worktree starts from local S2 checkpoint `269900c6a`;
its final review branch must be ported to the current gated effort. The
primary checkout remains untouched. The S2 disabled-import fix was subsequently
ported into this worktree, and its exact `6b2c69dbb` archive passed the pinned
Linux recovery and disabled-import refusal tests (0.49 and 0.20 seconds).

`PlaybackPrincipal` now distinguishes a real local numeric user from a grant
UUID and validated 64-character viewer pseudonym. Only its constructor
builds an owner key. The row-projection decoder rejects mixed local/shared
columns, noncanonical UUIDs, malformed pseudonyms and mismatched keys; it
never adopts a foreign local user. Debug output redacts the pseudonym.
Two focused tests passed; core Clippy with denied warnings passed using
Rust 1.97.1 and `hiqlite-store`.

The Store recount confirms 36 `MediaSessionStore` operations: 16 explicit
ownership arguments now take a borrowed `PlaybackPrincipal`, and 20 retain
session/incarnation/owner keys or structured input. Activation, preparation,
route, desired-selection, staged-generation, recovery-request/reservation and
owned-lease results carry the principal. Preparation/control and producer
recovery handoffs pass it through; route comparisons compare the principal.
Local authenticated ingress constructs an explicit local principal.

This interface conversion still uses a temporary local-only adapter in the
common SQL implementation. That adapter refuses a sharing principal before
any statement runs; it cannot create a numeric user for a share. Existing
runtime tables retain their old numeric ownership keys. Legacy source-worker
and VOD dispatch, cluster forwarding, ordinary-router authorization, telemetry
and account/grant deletion still need the complete principal-aware conversion.
The legacy process-local test entry points now pass an absent recovery
identity explicitly. No zero user ID stands for an unbound producer or VOD
viewer; cluster starts still require a complete typed recovery identity. The
focused no-budget regression passed (0.05 seconds).

Durable request hashing now takes the principal. Local hashes retain their
existing JSON/numeric encoding; sharing hashes use the canonical owner key.
Producer supersession and takeover gates share the typed namespace helper,
with the established local encoding preserved. The daemon intent-identity
regression passed after this conversion and proves local hash/gate stability
and separation by grant and viewer.
 These changes do not qualify
sharing admission. The complete predicate/conflict-target/decoder census,
runtime table rebuild, writer floor and migration acceptance remain open.
The typed local session adapter passed all 28 `media_session_` contract
regressions against SQLite and the actual three-voter fixture in 258.12 seconds.
The first activation run caught a Rust helper mistakenly substituted into SQL;
all such substitutions were removed from both backends before this successful
lifecycle run. The ownership conversion still needs the runtime key switch.

The current numeric-ownership SQL inventory contains 140 statement literals
in the session implementations (75 SQLite, 65 Hiqlite). Each must be switched
or explicitly justified as local-only; this statement inventory does not yet
include the remaining decoders, schema definitions, other stores or callers.

```sh
cargo test --locked -p plurx-core --features hiqlite-store --lib playback_principal::tests -- --nocapture
cargo test --locked -p plurx-core --features hiqlite-contract-tests --test store_contract media_session_ -- --nocapture
cargo test --locked -p plurxd --bin plurxd the_grade_is_part_of_a_request_identity -- --nocapture
cargo clippy --locked -p plurx-core --features hiqlite-store --lib -- -D warnings
```

**Candidate ownership rebuild:** a frozen seven-table rebuild is now exercised
against SQLite v92 and the actual three-voter replicated v70 schema. It
backfills canonical local keys, changes all six ownership composite primary
keys, keeps the incarnation/session namespace, adds checked nullable principal
projections, and preserves every pre-existing column in the populated fixture.
The Library-channel session-recipe table is the seventh ownership table.
Terminal acknowledgements and job leases remain unchanged and retained rows
stay FK-free. Existing desired-revision, drain, publication, recipe-retirement
and background-viewer triggers are recreated; the latter also require
principal-aware joins across the rebuilt tables.

Three SQLite tests passed. The replicated case passed in 9.39 seconds after
one fixture correction: Hiqlite requires explicit user `is_admin`/`created_at`
values where SQLite has defaults. A failing final write rolls the entire
replicated DDL/backfill transaction back, then the successful transaction
retains the original data. Both engines refuse an old local insert which
omits `owner_key` and an upsert using the old `(user_id, request_id)` conflict
target. These are SQL-shape probes, not execution of an old daemon binary.
They provide evidence for the coordinated-drain fallback; they do not qualify
a live upgrade, the membership floor or backup restoration.

The populated fixture now includes `sharing_relay_upstream` and
`sharing_delivery_grants`. A retention regression exposed DROP TABLE cascading
into both children. The candidate rebuild evacuates and restores them inside
the same transaction, with coverage for SQLite's migration FK setting and the
replicated writer's enabled FK setting. The source capability envelope, remote
IDs, position, delivery deadline and generation coordinates must remain exact;
backup tables must disappear and foreign-key integrity must hold. The three
SQLite migration regressions passed in 2.64 seconds, including preservation
with foreign keys both enabled and disabled. The source still uses SQLite v92
and replicated v70; this is a candidate rebuild, not an installed migration. The
updated three-voter transaction test passed in 9.38 seconds and proves both
children retain every original column after success and after a failed final
write rolls the complete rebuild back.

The rebuild is not installed in the runtime migration chain. The remaining
S3 work is the principal-aware Store/runtime conversion, owner-existence and
admission checks, full predicate/decoder census, cluster floor and forwarding,
actual old-binary and rollback qualification. Shared playback remains unavailable.

```sh
cargo test --locked -p plurx-core --features hiqlite-store --lib sharing_principal_rebuild -- --nocapture
cargo test --locked -p plurx-core --features hiqlite-contract-tests --test store_contract session_principals::sharing_principal_rebuild_is_atomic_and_preserves_rows_on_three_voters -- --exact --nocapture
```

**S3 checkpoint, 2026-10-02:** background analysis/artifact interest and daemon
viewer demand now carry `PlaybackPrincipal`. Local consumer hashes retain their
old numeric encoding; shared consumers separate grant and viewer identities.
The temporary local-only admission adapter remains explicit, so these typed
interfaces do not admit shared background work before the runtime migration.
The hash regression passed, and all eight `viewer` store contracts passed
against SQLite and the three-voter fixture (74.75 seconds).

The candidate rebuild also retires matching authority on local-user deletion,
export revocation and export deletion: sessions end, their leases expire,
delivery grants revoke, requests fail and current pointers/preparations/desired
selections disappear. Terminal rows remain retained. Tests prove another grant
and its viewer remain active, and reuse of a deleted numeric user ID cannot
reactivate old sessions. Four SQLite candidate tests passed (5.03 seconds),
and both replicated candidate tests passed (18.69 seconds). These triggers
remain candidate DDL; runtime installation and upgrade qualification are open.

```sh
cargo test --locked -p plurx-core --features hiqlite-store --lib sharing_background_consumer_keys -- --nocapture
cargo test --locked -p plurx-core --features hiqlite-store --lib sharing_principal_rebuild -- --nocapture
cargo test --locked -p plurx-core --features hiqlite-contract-tests --test store_contract session_principals -- --nocapture
cargo test --locked -p plurx-core --features hiqlite-contract-tests --test store_contract viewer -- --nocapture
```

**Forwarded ownership checkpoint:** worker start/preparation requests now carry
`PlaybackPrincipal` in Rust. The closed wire adapter keeps the existing local
`user_id` encoding for retained recipes and old local peers, and encodes a
shared request exclusively as a validated principal. Two namespaces, absent
ownership, zero users, malformed viewers and unknown fields are rejected.
Preparation/takeover comparisons use the complete principal; producer recovery
retains it. Current worker ingress explicitly refuses sharing before placement
or producer creation while Store/grant admission is unfinished. All 66
media-session/internal-relay tests passed (zero ignored, 3.68 seconds), including
`sharing_forwarded_principal_preserves_local_wire_and_refuses_mixed_ownership`;
Rust 1.97.1 all-target compilation passed. Shared worker admission remains open.

```sh
cargo test --locked -p plurxd --bin plurxd media_sessions::tests -- --nocapture
```

**Request cleanup ownership checkpoint:** HLS request and started-session
cleanup guards retain the complete `PlaybackPrincipal`, including forwarded
worker ownership, through bounded failure settlement and cancellation. Local
callers explicitly construct a principal from their authenticated user. Rust
1.97.1 all-target compilation passed; the three `started_session_` tests,
`replayed_start_guard_owns_neither_worker_nor_original_claim` and
`pre_worker_request_guard_releases_an_owned_claim_for_immediate_retry` passed
with zero ignored. These local cleanup regressions do not qualify shared
worker admission, which remains refused while grant-aware Store work is open.

```sh
cargo test --locked -p plurxd --bin plurxd started_session_ -- --nocapture
cargo test --locked -p plurxd --bin plurxd replayed_start_guard_owns_neither_worker_nor_original_claim -- --nocapture
cargo test --locked -p plurxd --bin plurxd pre_worker_request_guard_releases_an_owned_claim_for_immediate_retry -- --nocapture
```

**Route decoding checkpoint:** both existing session readers decode and
validate the complete canonical owner projection. The replicated reader
returns typed errors for missing or wrongly typed row data instead of
panicking. Legacy SELECTs explicitly project real local ownership until
the guarded rebuild switches them to the persisted columns; this does not
admit a shared session on the legacy schema. Three decoder tests passed
(zero ignored), preserving sharing grant/viewer identity and refusing mixed,
missing, zero/negative local and noncanonical ownership. Runtime key queries,
installation and member-floor qualification remain open.

```sh
cargo test --locked -p plurx-core --features hiqlite-store --lib sharing_route_ -- --nocapture
```

**Rebuilt route reader checkpoint:** production route queries now select
stored principal columns from a complete rebuilt table, retaining the explicit
local projection on the legacy table. A partial rebuild is refused. SQLite
checks the schema within its connection; Hiqlite caches the quorum-observed
shape for the Store lifetime, which is fixed between coordinated restarts.
That cache proves neither grant authority nor member compatibility. Five
route tests passed (zero ignored, 0.71 seconds), including actual rebuilt
rows in memory and the pooled SQLite store. Both three-voter candidate
contracts passed (zero ignored, 18.42 seconds), now exercising production
route reads for two distinct grants with no local user ID. All 28 existing local/replicated media-session contracts passed
(zero ignored, 256.32 seconds), including preparation, rejoin, replay,
cap enforcement and terminal retention; denied-warning Clippy passed with
`hiqlite-contract-tests`. Writer queries, installation and the mixed-version
floor remain open.

```sh
cargo test --locked -p plurx-core --features hiqlite-store --lib sharing_route_ -- --nocapture
cargo test --locked -p plurx-core --features hiqlite-contract-tests --test store_contract session_principals -- --nocapture
```

**Principal pointer checkpoint:** the production current-playback lookup uses
the canonical owner key on rebuilt tables and retains real local-user lookup
on legacy tables. It verifies that the returned session belongs to the
requested principal. Two grants can therefore use the same playback ID
without sharing pointers, and a corrupt cross-grant pointer returns no route.
Other viewers receive no route; legacy tables refuse shared lookups.
Six route tests passed with zero ignored (0.93 seconds), covering memory and
pooled SQLite. Both actual three-voter candidate contracts passed with zero
ignored (18.50 seconds), including the same cross-grant refusal. These reads
do not admit shared workers or qualify grant authority, schema installation
or the mixed-version floor; those remain open.
The existing `media_session_contract_runs_through_dyn_store` regression also
passed on SQLite and the three-voter backend (zero ignored, 13.25 seconds);
denied-warning Clippy passed with `hiqlite-contract-tests`.

**Desired-selection read checkpoint:** current desired selections are read
by canonical owner key on rebuilt tables, with the complete persisted
principal decoded and validated. Legacy tables retain real local-user
selection and refuse a shared principal. Both grants can retain a distinct
revision for the same playback ID, and another viewer receives no selection.
The six route/principal tests passed on memory and pooled SQLite (zero
ignored, 0.92 seconds), and both actual three-voter candidate contracts passed
(zero ignored, 18.51 seconds). The existing local session lifecycle contract
and denied-warning Clippy also passed. Writers, schema installation, grant
admission and cluster floor remain open; this checkpoint adds no shared
worker admission.

**SQLite desired-writer checkpoint:** a rebuilt local desired-selection write
inserts the complete canonical principal projection and uses `(owner_key,
playback_id)` for conflict resolution. Insert and exact replay retain revision
1; a changed digest advances it to 2. The transaction checks that the real
local user still exists, so a deleted user cannot recreate desired state.
Other grants' selections survive local deletion. The six focused tests passed
on memory and pooled SQLite (zero ignored, 0.99 seconds), and affected
denied-warning Clippy passed. Shared desired writers still refuse admission;
other writer operations, installation and the cluster floor remain open.


**SQLite request-writer checkpoint:** rebuilt local request claims, replay,
recipe persistence, owner assignment and failure cleanup use canonical owner
keys and persist the complete local principal. The real user must exist in
the claiming transaction. A deleted owner receives the existing overloaded
refusal; a resolved request pointing to another principal returns conflict.
Shared claims remain refused while grant admission and the member floor are
unfinished. The focused regression passed in both SQLite modes (one test,
zero ignored, 1.85 seconds). The 28 existing local/replicated session contracts
also passed after the request SQL change (zero ignored, 257.86 seconds),
before the final deleted-owner refusal alignment and positive-owner DDL check.

The candidate rebuild now rejects zero and negative local IDs in all seven
owner tables. Five candidate rebuild tests passed (zero ignored, 5.62 seconds),
and both actual three-voter candidate contracts passed (zero ignored,
18.49 seconds). Denied-warning Clippy passed with `hiqlite-contract-tests`.
The changed DDL requires a fresh historical-store qualification; the schema
is still uninstalled and these results do not qualify shared admission.

```sh
cargo test --locked -p plurx-core --features hiqlite-store --lib sharing_request_rebuilt_local_keys_replay_and_deleted_owner_refusal -- --nocapture
cargo test --locked -p plurx-core --features hiqlite-store --lib sharing_principal_rebuild -- --nocapture
cargo test --locked -p plurx-core --features hiqlite-contract-tests --test store_contract session_principals -- --nocapture
```


**Recovery decoder checkpoint:** the shared recovery-row converter now
receives and preserves a complete validated principal; each legacy caller
explicitly supplies its real Local owner. It refuses invalid Local IDs rather
than manufacturing a valid owner from zero. The focused converter regression
passed (one test, zero ignored), and the four existing recovery behavior
regressions passed through SQLite and actual three-voter stores. The broader
`producer_recovery_` filter also selected an older-schema migration fixture,
which failed during its synthetic downgrade because a newer background
projection trigger referenced a column being dropped. That fixture needs
repair; this is not a passing full filter or migration qualification. Shared
recovery SQL, schema installation and admission remain open.


**SQLite recovery ownership checkpoint:** rebuilt recovery reservations use
canonical owner keys and persist the complete Local projection. Reservation
checks real user existence in its transaction. Replay returns the same budget;
another failed incarnation receives none, and settling the budget prevents
reuse. Recovery reads decode complete persisted principals and isolate two
grants sharing the same playback/epoch; another viewer receives no row. Shared
reservation writes remain refused while authority admission is unfinished.
The focused rebuilt regression passed in memory and pooled SQLite (one test,
zero ignored, 0.73 seconds); the four existing recovery behavior regressions
passed on SQLite and actual three voters (zero ignored, 37.88 seconds). The
older-schema migration fixture remains separately under repair.

```sh
cargo test --locked -p plurx-core --features hiqlite-store --lib sharing_recovery_rebuilt_keys_preserve_grants_and_one_local_budget -- --nocapture
cargo test --locked -p plurx-core --features hiqlite-contract-tests --test store_contract producer_recovery_ -- --skip replicated_v32_store_migrates_the_producer_recovery_ledger_on_daemon_open --nocapture
```


**Parallel integration checkpoint:** the integrated branch includes the
replicated Local request/desired writers, complete staged/owned principal
decoders, canonical inventory joins, member-floor checker and atomic SQL
predicate. The capability remains unadvertised. The checker covers the
committed bounded voter/learner roster, fresh coupled heartbeats, join/removal
fences and quorum failure; its read result alone grants no write authority.
The dedicated three-voter test covers actual quorum loss and a heartbeat
change within the admission transaction. Learner cases use projected SQL
state, so a live fourth learner and promotion still need qualification.

Pinned all-target compilation passed on the integrated tree. Five floor unit
regressions passed (zero ignored, 0.01 seconds), all 11 sharing contracts
passed through their SQLite/actual-voter fixtures (zero ignored,
100.76 seconds), and the repaired v32 migration passed (zero ignored,
9.99 seconds). The fixture now removes later background/sharing schema
objects before setting its historical marker and verifies full background
trigger restoration after forward migration. Production migrations are
unchanged. Earlier v5/v27 checks also passed on the agent's exact committed
fixture. This resolves the stale migration-filter failure above; the new
schema is still uninstalled and Shared writer admission remains unfinished.

The next integrated checkpoint adds the closed catalogue/session floor
variants and replicated recovery reads and Local reservation/settlement
writes. Complete recovery principals survive decoding; rebuilt writes use
canonical owner keys and check the current Local user within their admission
transaction. Shared reservation and settlement remain refused.

On that combined tree, pinned all-target compilation passed (31.25 seconds),
both actual-voter floor contracts passed (13.70 seconds), the rebuilt Local
runtime contract passed (18.14 seconds), and all five producer-recovery
contracts, including the repaired historical v32 fixture, passed
(45.76 seconds). All tests had zero ignored cases. Feature Clippy with denied
warnings passed (36.74 seconds); documentation indexing and catalog lint
also passed. These checks do not qualify schema installation or Shared
writer admission.

**Preparation integration checkpoint:** SQLite staged lookups and worker
inventories decode complete principals and correlate canonical owner keys.
Two grants sharing the same viewer/playback retain independent staged slots;
a starting request suppresses only its own owner's inventory entry. SQLite
Local prepare/replay/rejoin/abort now writes complete metadata, checks current
user existence within its transaction, and requires predecessor ownership
agreement. Retirement and dependent pin/lease cleanup each require the
ledger's principal. The matching replicated preparation checkpoint is also
integrated. Shared mutation ingress remains refused.

On the combined tree, all 13 SQLite ownership cases passed (11.79 seconds),
both actual-voter candidate runtime cases passed (27.27 seconds), and all 28
existing lifecycle contracts passed (257.78 seconds). Every test had zero
ignored cases. The SQLite preparation case executes a corrupt Local ledger
pointing at a foreign Shared route and verifies the route, cache pin and
complete job-lease revision/expiry/update tuple survive the abort attempt in
both memory and pooled disk stores. Pinned all-target compilation and
denied-warning feature Clippy passed; schema installation, Shared admission
and upgrade/rollback qualification remain open.

**Activation integration checkpoint:** both backends now persist complete
Local principal metadata on activation and canonical pointer conflicts,
requests and publication. SQLite checks current user existence within its
transaction and refuses a current pointer that names another owner/playback.
Both implementations preserve a foreign Shared route's job lease when a
Local activation collides with its incarnation. Confirmation and exact replay
retain the committed route; request-backed publication retains its canonical
request identity. Shared activation remains explicitly refused.

Pinned all-target compilation passed on the combined tree (33.49 seconds).
All 14 SQLite ownership cases passed (14.43 seconds), all three actual-voter
candidate runtime cases passed (36.41 seconds), and all 28 existing lifecycle
contracts passed (257.61 seconds), with zero ignored tests. Denied-warning
feature Clippy passed (39.02 seconds). The candidate schema remains
uninstalled; this receipt does not qualify Shared admission or rollout.


**Commit and membership-admission integration checkpoint:** both backends bind
predecessor receipts to the actual predecessor session and canonical principal
inside the pointer compare-and-swap. A receipt naming an actual foreign Shared
session cannot advance a Local pointer or create an acknowledgement. Correct
Local commit, predecessor drain and exact replay preserve complete principals.
SQLite replay without a preparation ledger also requires its retained Local
predecessor rather than treating an unrelated acknowledgement as authority.

The candidate membership factory records token-bound declarations and durable
transition intents. Both application redemption and raw replicated-management
RPCs refuse incompatible admission after partial installation. A real fourth
learner exercises promotion. Ambiguous transition outcomes retain their intent;
a timer does not release the fence. Installation, startup capability advertising
and coordinated upgrade qualification remain unfinished.

Pinned combined-tree core and cluster-check all-target compilation passed
(33.53 and 32.51 seconds), all 15 ownership unit cases passed (15.60 seconds),
all four rebuilt-schema actual-voter cases passed (45.60 seconds), all four
membership-floor contracts passed (13.83 seconds), and six existing preparation
commit contracts passed (59.29 seconds). Denied-warning feature Clippy passed
(33.59 seconds). All executed regression cases had zero ignored tests. An
initial overly narrow membership unit filter selected no tests and is excluded
from this evidence; the exact application membership cases are recorded below.

The exact application redemption/rejoin/promotion regression passed (one test,
9.31 seconds); the existing occupied/expired-target daemon-join regression
passed (one test, 31.55 seconds). Both used the same integrated tree and real
replicated fixtures, with zero ignored tests. These are executed application
join paths rather than historical-process or production rollout evidence.


**Worker authority integration checkpoint:** SQLite and replicated renewal and
expired-owner takeover correlate canonical principal keys. Rebuilt-schema
session, physical lease and pin authority extensions require the stored Local
principal and its current user. Active handoff arming/completion and completion
replay also refuse Shared or deleted Local rows. Shared authority extension
still needs the server-side grant/scope/member-floor proof; terminal retirement
and complete Shared reads remain separate supported operations.

The new SQLite regression passed in memory and pooled stores (0.76 seconds),
including legitimate Local renewal/takeover, Shared refusal with the complete
foreign job-lease tuple unchanged, and a deliberately restored active Local
row after its user was deleted. On the integrated tree, all 16 ownership units
passed (16.33 seconds), all five actual-voter candidate cases passed (54.72
seconds), the existing dyn-store lifecycle passed (12.82 seconds), committed
successor renewal passed (9.66 seconds), and staged deadline refusal passed
(9.63 seconds). All had zero ignored tests. Pinned all-target compilation and
denied-warning feature Clippy passed. This checkpoint also integrates the
replicated confirmation/publication and foreign abandoned-activation cleanup
fences; coordinated installation and Shared admission remain open.


**Terminal and maintenance integration checkpoint:** exact-owner and public
capability terminal cleanup use the actual route's canonical owner key on
both layouts, including the original Local projection. Shared retirement no
longer calls the Local numeric adapter. An expired preparation can retire only
its same-principal, same-playback staged session; canonical pointer, ledger and
request retention joins cannot borrow another principal's row.

The SQLite corruption/cleanup regression passed in memory and pooled stores
(0.95 seconds): an expired Local ledger pointing at a Shared route leaves that
route and its complete lease tuple unchanged, removes the corrupt ledger,
then legitimate exact-owner and capability cleanup retires only the correct
grant. The second grant with the same viewer/playback remains active. The
replicated candidate also verifies preservation of the foreign physical pin.

On the combined tree, pinned all-target compilation passed (26.27 seconds),
all 17 ownership units passed (18.13 seconds), all six rebuilt-schema actual
voter regressions passed (64.02 seconds), abandoned-preparation maintenance
passed (9.61 seconds), and terminal-ack/takeover retention passed (9.70 seconds).
Denied-warning feature Clippy passed (32.93 seconds); every executed regression
had zero ignored tests. These are compatibility and ownership-boundary proofs,
not Shared writer admission or released migration qualification.


**Caller and retained-read census checkpoint:** VOD session creation, copy
queueing and cluster index demand reject Shared execution before allocating a
session, queue entry or pool demand. Canonical SQLite desired-selection reads
now decode the complete retained principal and reject corrupt metadata. Both
backends correlate terminal pointer removal with the exact ended session's
principal and playback ID, preserving foreign-owner pointers even when a
corrupt pointer names that incarnation. The indexed
[principal census](SHARED-LIBRARIES-PRINCIPAL-CENSUS.md) records the remaining
Source admission and proof-bearing worker boundaries.

On the combined caller/SQLite tree, pinned daemon and feature-enabled core
all-target compilation passed. All 19 SQLite ownership regressions passed
(19.65 seconds); the Shared VOD no-allocation regression passed (0.16 seconds)
and durable preparation queue regression passed (1.51 seconds). Core and daemon
denied-warning Clippy passed. These checks do not authorize Shared admission.

The exact committed transport/ownership integration `33c0fb2f0` also passed
`sharing_pinned_transport_recovers_committed_claim_and_rotation_after_restart`
on pinned Rust 1.97.1 in an isolated Linux CGNAT container (one test, zero
ignored, 0.46 seconds). Its source-only archive carried neither Git metadata
nor repository credentials. This is restart/rotation protocol evidence for
that snapshot; it does not qualify real Tailscale, two NATs or a physical TV.

The combined tree also passed the actual three-voter Shared terminal
acknowledgement/projection regression (one test, zero ignored, 9.22 seconds),
feature-enabled all-target check (25.37 seconds) and denied-warning Clippy
(30.40 seconds). Wrong owner tuples fail, retained exact acknowledgements
replay, and retirement preserves a foreign pointer and physical pin.

### 16.5 Parallel S4 implementation — catalogue boundaries and private progress

The isolated task starts from S1 effort landing `971265536`, using the pinned
Rust 1.97.1 compiler. It does not enable a peer or viewer route.

**Built:** [catalogue values](../../crates/plurx-core/src/sharing_catalogue.rs)
keep full import/server/catalogue/library/item identities and canonical string
IDs. Cursor MACs derive a catalogue key from the provisioned credential key and
use a fixed catalogue purpose prefix; key bytes are not exported. Cursors last five minutes, are
bounded to 4 KiB, and bind the grant, library, query and exact sort boundary.
A changed order revision retains that boundary. Current authority still has
to be read with the page; a signed cursor is never an authorization grant.
Batch parsing stops before retaining more than 200 IDs. Logical browse
identity retention suppresses duplicates across evicted pages.

The [private watch Store](../../crates/plurx-core/src/store/sharing_catalogue.rs)
uses the existing receiver-only table. A single guarded write checks current
import, viewer assignment and captured lifecycle/assignment generations.
Higher sequences replace progress; identical retries replay; lower sequences
and conflicting same-sequence values cannot overwrite a newer position.
A changed item/library identity is refused. Reads check current assignments,
so disconnect or unassignment hides retained progress. No statement writes
local household watch state or source-side history. The HTTP caller still
has to verify a live local login and current source item membership; these
Store methods do not attest foreign metadata.

**Observed:** four catalogue/progress unit regressions passed with zero ignored
tests (0.10 seconds). The private-watch contract passed on both SQLite modes and
three actual Hiqlite voters (9.33 seconds, one contract, zero ignored tests).
The reference/dedup fixture retained 40,000 distinct references; this is not
a process-memory measurement. Documentation indexing passed four tests and
catalog lint passed. Clippy passed all core targets with denied warnings
and the replicated contract feature (39.10 seconds). The focused
pinned-toolchain commands were:

```sh
cargo test --locked --offline -p plurx-core --features hiqlite-store --lib sharing_catalogue -- --nocapture
cargo test --locked --offline -p plurx-core --features hiqlite-contract-tests --test store_contract sharing_private_watch -- --nocapture
cargo clippy --locked --offline -p plurx-core --features hiqlite-contract-tests --all-targets -- -D warnings
```

**Still owed:** consistent source catalogue reads and order-revision updates,
closed presentation DTOs, peer/viewer route wiring, scoped artwork, bounded
metadata/art caches, batch concurrency, source revalidation of Continue
Watching, and measured paging under actual continuous scan writes. Cursor
unit tests that advance revisions are not the scan-liveness receipt. S4 is
partial; no Tailscale, playback, client or promotion qualification is claimed.


### 16.6 Parallel S4 source catalogue and item identity candidate

**Built, awaiting activation:** the [source Store](../../crates/plurx-core/src/store/sharing_catalogue_source.rs)
reads active credential/grant, current library membership, revision and page in
one consistent SQL boundary. It uses fixed BINARY indexes and numeric item
IDs with tuple seeks; child order includes numeric season/episode keys and
explicit NULL ordering. Pages are bounded to 201 rows including lookahead.
Batch requests retain at most 200 IDs and return no metadata for missing or
out-of-scope IDs. The [peer routes](../../crates/plurxd/src/http/shared_library.rs)
compose the existing private listener guard and expose closed library/item
metadata, children, batches and signed live cursors. Internal artwork names
cannot be serialized by these records. The routes refuse service while
catalogue order or item identity maintenance is absent or an import is active.

The [candidate order DDL](../../crates/plurx-core/src/store/sharing_catalogue_schema.sql)
indexes title and child order and maintains revisions on library/item insert,
item deletion, library moves and actual sort/parent/kind/ordinal changes.
Descriptions, artwork and analysis updates do not advance order revisions.
The finite production writer census covers ordinary media writes in
`sqlite/media.rs` and `hiqlite_media.rs`, lease-fenced scanner/publication
writes in `sqlite/publication.rs` and `hiqlite_publication.rs`, reconciliation
and pruning in those same modules, and explicit-ID backend import in
`hiqlite_import.rs`. Old table rebuild migrations and test SQL are not runtime
writers. Membership/order maintenance is implemented in storage triggers, so
the enumerated writer paths do not have independent counter updates to omit.

An actual SQLite `insert_item` → delete highest item → unrelated `insert_item`
reproduced ID reuse (`old=1`, `new=1`). Because shared references and retained
private history bind source server/epoch/item IDs, reuse would make retained
state refer to another title. The [candidate allocator DDL](../../crates/plurx-core/src/store/item_identity_schema.sql)
keeps a durable high-water mark and rejects every implicit or explicit insert
at or below it outside a dedicated fresh-target import mode. Ordinary writers
allocate the next ID within the insert statement. The replicated fenced
writer replaces random high-i64 allocation with a consistent candidate read
and an atomic lease-checked insertion; competing allocation rolls back and
retries at most four times. Failed inserts do not spend an ID. All inserts
observe explicit source IDs. The immutable import actor additionally preserves
a source watermark above live IDs; interrupted imports retain unavailable
import mode and require the existing discard-incoming-target recovery.
Catalogue triggers suppress derived revision changes during import and seed
missing older-backup revisions only after parity succeeds.

**Observed:** eight focused core tests passed with zero failures/ignored tests
(2.80 seconds), including actual metadata writes while 5,000 finite items are
paged to exhaustion on memory and disk SQLite. This is a live-list receipt;
it is not snapshot isolation. Isolated native test-process measurements were
0.10 seconds and 24,690,688-byte peak RSS for the 40,000-reference dedup
fixture, and 1.85 seconds and 41,500,672-byte peak RSS for the complete live
scan fixture. These include the test runtime, source fixtures and SQLite;
they do not attribute heap bytes solely to retained references. Two additional
contracts passed on three actual Hiqlite voters (18.95 seconds, zero ignored):
current scope removal, live boundary continuation after metadata/sort changes,
per-ID batch denial, a real prepared SQLite backup with spent watermark 1000,
post-import monotone allocation, deleted-highest allocation, stale scanner
lease refusal, old writer refusal and concurrent distinct IDs.

```sh
cargo test --locked --offline -p plurx-core --features hiqlite-contract-tests --lib sharing_catalogue -- --nocapture
cargo test --locked --offline -p plurx-core --features hiqlite-contract-tests --test store_contract sharing_catalogue_ -- --nocapture
cargo test --locked --offline -p plurxd --bin plurxd sharing_catalogue_http_ -- --nocapture
cargo clippy --locked --offline -p plurxd -p plurx-core --all-targets --features plurx-core/hiqlite-contract-tests -- -D warnings
```

Two daemon HTTP regressions also passed (0.18 seconds, zero ignored):
unknown/duplicate/oversized query rejection and live-grant/maintenance
refusal on every catalogue route, with private headers and no out-of-scope
names or source paths.

All core/daemon targets passed denied-warning Clippy (1 minute 42 seconds).
Documentation indexing passed four tests and catalog lint passed (28 points,
35 checks and 2,609 audited files).

**Still owed:** the schema remains candidate-only and uninstalled. Activation
requires coordinated migration versions and the separate
`sharing_catalogue_item_identity_v1` floor on every active voter and learner;
the old random explicit publication writer must not remain admitted.
No capability is advertised by this candidate. Rejoin/promotion must treat an
installed allocator table conservatively as requiring the new floor. Viewer
routes, closed playable-file details, bounded receiver metadata/art caches,
current source revalidation of Continue Watching, scoped artwork and blocked
body revocation remain open. The earlier private-watch Store still requires
live login and current foreign item membership at its HTTP caller. This is
not S4 completion, playback admission or Tailscale/promotion qualification.


The isolated S4 branch also integrated the verified member-floor dependency
chain through `18999f044`, retaining its principal fixtures and all catalogue
registrations. The resulting tree passed core/daemon all-target check
(1 minute 22 seconds), 46 focused sharing unit tests (13.81 seconds) and
14 focused sharing contracts (110.23 seconds, zero ignored) on SQLite and
actual three-voter fixtures. Those contracts include both independent floor
capability cases and the source/allocator/private-history cases. Serving
admission integration is the next change; these observations do not install
schemas, advertise capabilities or authorize a new shared writer.


#### S4 receiver runtime checkpoint (candidate, open)

The candidate now mounts authenticated receiver library, browse/search,
children, batch, item metadata and a closed private-progress refusal route. Every operation
uses a verified pinned source connection, a closed bounded response and fresh
local assignment authority. Admission retains no wait queue: four metadata
operations process-wide and one per import. Permit ownership releases both
limits on errors and cancellation. Assignment filtering uses one consistent,
generation-bound query, bounded at the existing 64 libraries per grant, instead of one Store lookup
per remote item. Captured source/epoch, import lifecycle, endpoint, grant and
assignment generations are checked after the network operation. The private-progress Store enforces those generations atomically. Its HTTP
writer remains unavailable until S5 supplies current active-session observation
binding; a fresh source batch alone cannot authorize playback progress.

Catalogue success bodies have an explicit 4 MiB client cap; management and
error bodies retain a 128 KiB cap. Unknown response fields, mismatched batch
IDs/order, noncanonical IDs, excessive fields and oversized streams refuse
without retaining peer error text. Query components are percent-encoded and
malformed percent encodings or UTF-8 refuse before cursor/filter processing.
Source metadata serving now requires the actual CatalogueItemIdentity member
floor on replicated nodes, with a one-second read deadline. That read check
is not a write admission predicate and does not install a schema or advertise
capabilities.

This checkpoint remains open: playable-file details, metadata/art caches,
scoped art with blocked-socket revocation and current-source Continue Watching
are still owed. Private progress refuses a changed stored library identity;
a later explicit reconciliation must prove the same durable source/epoch/item
in its new library and recheck the new assignment. A progress body cannot
perform that reconciliation. Neither this checkpoint nor the earlier Store
primitives constitutes S4 completion or S5 playback qualification.


The exact receiver checkpoint passed six focused daemon catalogue regressions
(0.59 seconds, zero ignored), including authenticated-route refusal, malformed
UTF-8/query fields, 64-versus-65 library response bounds, the management versus
catalogue body budgets, mismatched per-ID results and cancelled admission.
The generation-bound private-watch/assignment contract passed memory, pooled
SQLite and actual three-voter storage (9.37 seconds, zero ignored).
Core/daemon all-target denied-warning Clippy passed on the same source; its
reported 5 minutes 4 seconds includes waiting for the focused test compiler.
Commands: `cargo test --locked --offline -p plurxd --bin plurxd sharing_catalogue_`,
`cargo test --locked --offline -p plurx-core --features hiqlite-contract-tests
--test store_contract sharing_private_watch_orders_updates_and_isolates_sources_and_assignments`
and `cargo clippy --locked --offline -p plurxd -p plurx-core --all-targets
--features plurx-core/hiqlite-contract-tests -- -D warnings`, with the pinned
1.97.1 compiler and the isolated S4 target directory.


#### S4 accepted content transport checkpoint (candidate, open)

Source catalogue responses now retain captured grant, source/epoch, library
and item authority through accepted-connection completion. The Store check
uses one consistent current query; credential rotation alone preserves an
already accepted response, while removed grants, narrowed library scope,
item moves/deletion, unavailable qualified layout and import mode refuse.
The actual catalogue query also checks import mode after its earlier readiness
read, closing a transition between those reads. Replicated serving continues
to require the real catalogue member-floor read check. This does not authorize
writes or activate the candidate schemas.

Superseded 2026-10-04 (Root ownership review, D1). A guarded response's
authority monitor now lives exactly as long as its response body.
`MonitoredBody` releases the node-wide permit and cancels a per-response child
of the accepted connection's token when the body drops. Fully buffered
responses (catalogue JSON, details, decisions, artwork) are authorized
immediately before they are returned and start no monitor. A streaming Source
media body still re-observes authority every second while it is alive and
cancels the connection on refusal. Replicated revocation has no change feed,
so this bounded observation is the authority path for bytes that are still
being produced.

The earlier design kept every monitor until the connection closed, because
Hyper may still own queued DATA after Body Drop. On keep-alive connections that
held a slot per completed response. The 33rd guarded response on a node was
refused with 429, the 33rd on one connection cancelled the connection, and an
art lease held by its monitor refused the fifth poster. Revocation no longer
cuts bytes from a completed buffered response that are already queued in the
transport. Those bytes were authorized when the response was built, and no new
bytes follow. Ordinary handlers do not register monitors or cancel the sharing
token.

This checkpoint covers source catalogue JSON. Scoped artwork, receiver login
and assignment body monitors, full file details, caches and Continue Watching
remain open; these results do not qualify S4 or S5. The transport seam is
available to subsequent scoped artwork/relay handlers, which must retain their
own captured authority and bounded monitor admission through the same lifetime.


Ten daemon catalogue regressions passed on this checkpoint (6.40 seconds,
zero ignored), including actual HTTP/1 blocked transport revocation for grant,
scope and item mutations, delayed HTTP/2 stream windows after Body Drop and
another stream flush, credential rotation, collateral multiplex cancellation,
global admission refusal, 32 idle response monitors and owned registry cleanup.
The source Store contract passed on actual three-voter storage (8.93 seconds,
zero ignored), checking source identity, current item moves, import mode and
scope removal. The interposed SQLite query test also passed: import activation
after readiness refuses each actual catalogue query (0.16 seconds, zero ignored).
Commands: `cargo test --locked --offline -p plurxd --bin plurxd sharing_catalogue_`,
`cargo test --locked --offline -p plurx-core --features hiqlite-contract-tests
--test store_contract sharing_catalogue_source_three_voters_refuse_removed_scope_and_preserve_live_boundaries`
and `cargo test --locked --offline -p plurx-core --features hiqlite-contract-tests
--lib sharing_catalogue_actual_query_refuses_import_started_after_readiness`.
The socket/voter tests use local loopback fixtures and the pinned 1.97.1 compiler;
no production schemas, network settings or installations are changed.

Core/daemon all-target Clippy with `-D warnings` passed (1 minute 51 seconds,
including compiler lock wait); docs-index tests passed. The tracked commit
hook must also pass before this checkpoint is integrated.


#### S4 stable file revisions and existing-key census (candidate, open)

The cancellation checkpoint was integrated with the full principal-storage
ancestry through `cac78a97e` and verified transport ancestry through
`c99c3769e`; the resulting `139d30b` passed ten daemon catalogue/cancellation
regressions (7.30 seconds), two actual-voter catalogue/allocator contracts
(18.95 seconds), eight core catalogue units (3.16 seconds), all with zero
ignored, plus feature-enabled core/daemon all-target denied-warning Clippy.
Those results describe that frozen integration tree, not later additions.

The [closed revision type](../../crates/plurx-core/src/sharing_catalogue_details.rs)
contains a canonical 64-hex opaque file revision and a server-only file witness.
The witness has no Serialize or Debug implementation; only a current-authorized
Store query constructs its private canonical projection. The
[Source witness reader](../../crates/plurx-core/src/store/sharing_catalogue_details.rs)
correlates current grant, source/epoch, effective movie/show library,
movie/episode item, file and import state in its actual query. It returns a
closed authorized/unavailable/capacity outcome. Admission must regenerate the
same persisted-column projection and compare the captured witness within its
atomic write. A wire digest or an earlier read cannot authorize that write.

The projection includes exact persisted path, size/mtime, selected media
facts, streams, probe input, scan time, audio offset, DV and luminance values.
Caption resource metadata participates; caption body bytes do not. S5 must
revision the actual caption resources separately. Nested SQL CASE branches
refuse oversized raw private fields and a serialized projection above 2 MiB
before constructing the canonical witness; no truncation hides changes.
The following details checkpoint extends these single-file primitives with
a consistent all-file read and closed presentation facts. Its additional
bounds and remaining delivery limits are recorded below.

File revisions derive an HMAC purpose from a stable random Source/epoch key.
The [candidate key table](../../crates/plurx-core/src/store/sharing_catalogue_keys_schema.sql)
seals that random material under the distinct CatalogueRevision context.
Peer credential rotation does not change it, and replacing the sealing master
rewraps the same material rather than replacing it. The cryptographic factory
writes no database state; catalogue readers never initialize a missing key.
Existing-key reads are bound to the requested Source and epoch and refuse
absent, malformed or incompatible state.

The startup sealed-row census includes this optional candidate table and
refuses wrong object/column/type/nullability/primary-key shapes, excess rows,
foreign Source/epoch bindings, malformed envelopes and oversized rows. SQLite
reads table shape and rows in one read transaction. Hiqlite currently performs
separate consistent reads; this is not an atomic schema/data snapshot.
Coordinated schema creation and key rewrap must remain quiescent around its
startup census and key selection. No runtime initializer is installed.

Activation remains open: the qualified coordinator must provision the purpose
key before publishing its marker, compose the Source capability floor and
same-write intents, and preserve the key through backup/import. The earlier
session coordinator receipt does not qualify this new table. File details,
receiver caches, scoped artwork, receiver body authority and Continue Watching
remain required S4 work; these revision primitives do not complete S4 or
qualify Source playback admission.


The revision checkpoint passed twelve focused core catalogue units (4.85
seconds, zero ignored). The existing-key census and current-file witness each
passed actual three-voter contracts (8.87 seconds each, zero ignored).
The witness regression covers private file replacement, huge canonical IDs,
capacity refusal before malformed caption JSON is evaluated and current scope
removal. Memory/pooled tests also cover read-only absent-key refusal, wrong
epoch, caption body exclusion and metadata revision changes. Feature-enabled
core/daemon all-target Clippy with `-D warnings` passed (2 minutes 7 seconds,
including compiler lock wait); docs-index tests passed.
Commands: `cargo test --locked --offline -p plurx-core --features hiqlite-contract-tests
--lib sharing_catalogue_`, and the same features with `--test store_contract`
filtered to `sharing_catalogue_revision_key_census_three_voters_refuses_partial_and_foreign_state`
and `sharing_catalogue_file_witness_three_voters_binds_current_file_and_refuses_capacity`.
These are pinned Rust 1.97.1 results, with key provisioning confined to temporary
fixtures. Production schemas, activation and Source advertisements remain unchanged.


**Combined ownership/catalogue integration:** the retained-read and caller
checkpoint `f65eaace4` integrates S4 through stable revision/current witness
checkpoint `ae626d8e6`. Pinned daemon and feature-enabled core all-target checks
passed (1m06s and 27.72 seconds). All 13 core catalogue units passed (4.47
seconds), all five catalogue/private-history contracts passed through their
SQLite and actual three-voter fixtures (45.50 seconds), all ten daemon
catalogue/connection cancellation regressions passed (6.20 seconds), and all
19 SQLite principal regressions passed (18.99 seconds). Denied-warning
feature Clippy passed (31.49 seconds); every executed test had zero ignored
cases. The integration preserves canonical ownership fixes while adding the
bounded catalogue, allocator/import identity, private history, transport body
cancellation and stable existing-key-only file-witness candidates. It does
not install their schemas or enable Shared producer admission.


**Opaque Source membership observation candidate:**
`MembershipManager::observe_source_admission_members` derives the serving
Raft identity from its actual replicated state and returns no wire-decodable
proof. It quorum-checks both closed writer capabilities plus unresolved
membership/join declarations, and compares committed membership before and
after that read. Its write guard must be embedded with installed-schema and
current-file authority in the mutation; the observation expires after five
seconds and refuses a backwards clock. The existing advisory readiness APIs
retain their original floor semantics. No capability is advertised and no
Shared ingress is enabled by this API.

Pinned feature-enabled all-target check passed (25.07 seconds), the guarded
SQLite mutation race regression passed (one test, zero ignored), and all four
existing actual-voter/fourth-learner floor contracts passed (13.77 seconds).
The narrow test-only proof seams additionally passed check (28.66 seconds),
the mutation regression (0.01 seconds) and denied-warning feature Clippy
(33.73 seconds). SQLite unit fixtures construct only a test-build observation;
the contract-only wrapper must execute the actual quorum observation path.
Neither constructor is present in production builds. These checks qualify the
closed guard and readiness refactor; a Source binding write using the opaque
observation still needs its own actual-voter admission regression.


**Completed-transition membership fence candidate:** the coordinated membership
factory now includes `cluster_sharing_membership_generation`, a monotonic,
non-replaceable singleton. Membership-intent insertion, mutation and exact
resolution advance it, as do cluster-directory insertion, deletion and identity
or removal-state changes. Ordinary heartbeats preserve it. A Source observation
captures the generation in its quorum floor snapshot; its conditional write
checks both that exact number and the complete generation table/trigger shape.
A restored visible roster or an empty intent table cannot revive an observation
from before a completed transition. Missing/partial factory state refuses; no
read initializes or repairs it.

Pinned feature-enabled all-target check passed (28.12 seconds). Both focused
SQLite guarded-mutation regressions passed (zero ignored), including completed
intent/identity transitions, missing trigger and reset/replacement/deletion
refusals. All four actual-voter/fourth-learner membership-floor contracts passed
(13.76 seconds, zero ignored), and denied-warning feature all-target Clippy
passed (34.08 seconds). The actual Source binding transaction needs integration
qualification with this newer factory. Clock freshness is enforced when the
trusted server submits the mutation; delayed Raft execution has no independent
wall-clock expression in deterministic SQL. Shared worker/frontdoor admission
remains closed pending its full execution and producer-release proof. Earlier
upgrade receipts do not qualify this newer factory or install it in production.


**Coordinated qualification integration:** the indexed
[daemon/rollback receipt](SHARED-LIBRARIES-COORDINATED-UPGRADE.md) records the
actual source-only `9b99fcd0f` qualification on catalogue integration `a8bf5ce1d`:
four historical/current future-schema refusals, three historical voters,
bounded full stop, whole-topology backup, membership factory before the frozen
principal rebuild, three current daemon restarts and historical restore. The
separate SQLiteStore drill checks closed-file backup, injected rollback,
current candidate reopen and byte-identical legacy restore. Strict inventory
compares all eleven tables immediately across rebuild; post-restart predicates
separately classify expected terminal maintenance and runtime worker leases.
It remains an empty-media workload, with no active producer drain or production
schema activation. This receipt does not qualify later Source admission code.

The helper/runner checkpoint `95314e279` is integrated after opaque membership
checkpoint `f6ee7a1bd`. On this combined source, pinned feature all-target check
passed (0.62 seconds), both helper/SQLite regressions passed (0.69 seconds),
denied-warning helper Clippy passed (25.67 seconds), and the six runner plus
four docs-index tests passed. The exact combined runtime archive must be
qualified again before its upgrade receipt can cover a later candidate.


#### S4 bounded Source details and current file tuples (candidate, open)

The Source item route reads its presentation record and file snapshots in one
consistent grant/token/library/item/import-state query. A 65th file is a refusal
sentinel, never a truncated listing. The query checks the aggregate serialized
private projection estimate at 8 MiB before constructing any canonical file
snapshot; each file also retains the strict raw-field and 2 MiB projection
bounds. Public presentation fields are fenced by SQL CASE before JSON
construction. Existing sealed revision-purpose material is read without
initialization. Missing or incompatible material keeps details unavailable.

The closed SourceItemDetails vocabulary permits media facts, bounded audio and
subtitle tracks, chapters, canonical string sizes/IDs and opaque file revisions.
Presentation is copied explicitly from the same private snapshot as its HMAC;
paths, probe documents, downloaded caption bodies and Source accounts have no
wire field. The peer client applies its explicit 4 MiB success budget and
validates both nested facts and the requested item identity. Receiver details
recheck import lifecycle and current assignments after that fresh pinned read.
They expose full shared references and delivery_status=unavailable. There is
no file_base placeholder, raw-ID delivery path or local playback fallback.
The future opaque B locator and delivery require S5 session binding and key
lifetime qualification. Timeline skip-region presentation remains open until
current analysis fingerprint/generation authority is established; the present
candidate returns no regions and does not claim that part of the detail contract.

A details response captures at most 64 library/item/file tuples. Its accepted
connection monitor checks current grant, Source/epoch, import state, effective
scope and every tuple. File deletion or movement revokes the response even
when its socket writer is blocked. Ordinary metadata/revision changes preserve
metadata delivery; actual Source playback admission must still compare the
received immutable revision with its current private witness. JSON serialization
uses a writer that refuses growth beyond 4 MiB before extending its buffer.
Details are buffered, so they are authorized once before return and start no
monitor (see the 2026-10-04 supersession above).

Temporary SQLite and actual three-voter fixtures cover 64-versus-65 files,
aggregate capacity refusal, current tuple authority, large canonical IDs and
private-field exclusion. Wire checks refuse injected fields, malformed sizes,
duplicate file identities and excess chapters. The HTTP fixture exercises the
real accepted connection with blocked DATA and file deletion/movement, including
body/data/permit cleanup. All eleven daemon catalogue/cancellation tests passed
(11.52 seconds, zero ignored), including the existing HTTP/2 delayed-window and
multiplexed-stream proof. Twelve core catalogue units passed (4.86 seconds) and
the expanded actual three-voter file/details contract passed (9.00 seconds),
with zero ignored. Docs-index tests passed. Commands use pinned Rust 1.97.1:
`cargo test --locked --offline -p plurxd --features plurx-core/hiqlite-contract-tests
sharing_catalogue`; `cargo test --locked --offline -p plurx-core --features
hiqlite-contract-tests --lib sharing_catalogue_`; and the same core features
with `--test store_contract sharing_catalogue_file_witness_three_voters`.
All-target feature-enabled Clippy with `-D warnings` passed (1 minute 21 seconds).
The tracked commit hook remains required before integration.
Source schema installation, revision-key initialization, capability advertisement,
receiver body authority/cache/artwork/Continue Watching and S5 delivery remain
open. This checkpoint does not complete S4 or qualify an admission write.

#### S4 receiver body scope and bounded metadata cache (candidate, open)

The candidate adds POST /sharing/v1/current-scope as an authenticated control
operation. Its closed 32 KiB request binds the actual grant credential and
recipient to the expected Source UUID/epoch, at most 64 library IDs, 200
library/item tuples and 64 library/item/file tuples. One consistent query
checks every current effective tuple and importing=0; the real closed catalogue
member floor is checked before and after. The response contains only a current
authority boolean. It proves neither cached bytes/revisions nor worker or write
admission. Source control admission is a separate nonqueued pool of 32 permits,
with a one-second deadline including body reads. No receipt, MAC key, schema
initializer or capability advertisement is added.

Receiver metadata bodies capture only a hashed local login and bounded authority
references. A read-only Store query checks the current token/user, idle-expiry
policy, import lifecycle, Source/epoch, claim/grant and effective assignments;
monitor reads never renew last_seen_at. Assignment additions and endpoint refresh
may advance counters while preserving captured effective tuples; generation
rewind, identity/lifecycle replacement or a lost assignment refuses the proof.
The control client reloads the current sealed credential and approved endpoints,
verifies TLS SPKI and Source identity before sending credentials, and checks
current Source scope. Its whole operation has a one-second deadline and a
separate 32-permit nonqueued admission pool. Approved numeric hints now race
bounded Quad100 resolution rather than waiting behind its one-second lookup.
Each endpoint permits at most four distinct numeric targets, sixteen overall;
DNS cannot duplicate an in-flight hint or add a generic fallback. Losing dials
and connection drivers remain owned and are cancelled.

Receiver authority is checked before bounded 4 MiB serialization and again at
accepted-body admission. Receiver catalogue responses are buffered, so that
admission check is their authority point and they start no monitor; the former
32-slot receiver monitor pool is gone (2026-10-04 supersession above).

The receiver metadata cache charges serialized payload plus conservative entry
overhead against 32 MiB, permits at most 2048 entries and uses LRU eviction and
an absolute 30-second insertion TTL. Its key separates the current B user,
import/Source/epoch/claim/grant and all lifecycle/assignment/endpoint generations
plus the complete request and a digest of the closed received bytes (including
revisions and observed scope/catalogue counters). Every lookup still performs the full fresh pinned
catalogue/details read and current assignment checks. Matching bytes can reuse
a cached decode; changed bytes replace the entry. Failed or unavailable fresh
reads cannot serve cached content. The new scope boolean deliberately cannot
replace that byte-freshness evidence.

Native Source HTTP and typed control-response fixtures pass, including recipient,
foreign Source/grant/tuple refusal, capacity cleanup, oversized bodies and closed
response parsing. The current Source scope query passed against actual three
Hiqlite voters (9.03 seconds, zero ignored). The native receiver fixture passed
with actual blocked HTTP/1 and HTTP/2 DATA, login revocation, connection/data/
permit cleanup and unchanged last_seen_at (5.05 seconds, zero ignored).
An explicit Linux-only fixture retains the real CGNAT Source TLS/SPKI/identity
path, both production routers and accepted-connection servers. It covers Source
grant/scope/item/file changes, B login/import/assignment loss, unavailable fresh
reads with a populated cache, benign real credential rotation and assignment/
endpoint expansion, a refused numeric target with a reachable approved hint,
and collateral HTTP/2 stream closure. That fixture remains unqualified until
executed from the committed candidate in the disposable CGNAT source-only loop;
its ignored declaration is not acceptance evidence.

Continue Watching grouping, scoped artwork/cache delivery, import move history
reconciliation, current skip analysis and S5-bound progress writes remain open.
The current cache does not provide offline fallback, and the scope check does
not qualify playback, file locators, schema installation or promotion.

The final native source passed 66 core sharing units (35.96 seconds), 26 daemon
sharing units (18.93 seconds), the receiver authority contract across SQLite
and actual three-voter Hiqlite (9.30 seconds), and the expanded replicated
file/scope/import-fence contract (9.07 seconds), each with zero ignored.
Commands use pinned Rust 1.97.1 and `--locked --offline`: `cargo test -p
plurx-core --features hiqlite-contract-tests --lib sharing_`; `cargo test -p
plurxd --bin plurxd --features plurx-core/hiqlite-contract-tests sharing_`;
and `cargo test -p plurx-core --features hiqlite-contract-tests --test
store_contract` with `sharing_receiver_content_authority` and
`sharing_catalogue_file_witness_three_voters`. Native all-target core/daemon
Clippy with the same features and `-D warnings` passed; the tracked hook checks
the final committed tree again. Docs-index tests passed. These native results
exclude the explicit Linux CGNAT fixture and do not replace its execution.

### S4 bounded Continue Watching candidate

The receiver now exposes `GET /api/v1/shared/continue-watching?limit=200`
for a local group index and `GET
/api/v1/shared/imports/{i}/continue-watching?limit=200` for each independent
Source group. One current Store read selects the current user, enabled imports,
effective library assignments and B-private unfinished history. Compound
Source/epoch/library/item references remain distinct even when two Sources use
the same item ID. Recent ordering is deterministic. The read refuses more than
200 eligible rows or 32 imports before applying a smaller caller limit; each
import is bounded to 64 libraries. It omits watched items and removed
assignments. A moved item's changed library does not reconcile old history.

The index contains only current B-configured Source labels and B-owned counts.
A per-import request resolves its IDs through one fresh pinned Source batch,
using the existing one-per-import/four-global admission limits. Its
`availability` is `online`, `unavailable` or `busy`; unavailable groups return
no stale title, artwork or cached item. Source metadata and B history are joined
only on the full captured identity and current library. Group requests are
independent, so one unavailable Source cannot erase other configured groups.

Accepted-body authority now separates mandatory current B login/import/effective
assignment checks from optional Source tuple checks. Index, configured-library
labels and offline group status need only B authority. Responses carrying
Source metadata also require current Source authority. Both retain the bounded
connection-owned monitor through actual transport completion. The native
blocked HTTP/1 and HTTP/2 fixture now covers B-only login, import and assignment
loss with blocked DATA and monitor cleanup. The Linux CGNAT fixture additionally
seeds 200 B-owned history rows, performs the production Continue Watching batch
and revokes Source scope during a blocked response; this addition is pending
execution of the exact committed archive and is not a Linux receipt.

The focused Store contract exercises memory SQLite, pooled SQLite and actual
three-voter Hiqlite: two Sources with identical large IDs, another user's
isolation, current assignment filtering, deterministic ordering, the 201st-row
sentinel even when `limit=1`. Production import creation separately enforces
the 32-import bound. A raw reader-only corruption fixture additionally proves
that a 33rd import cannot hide behind `limit=1`, and that oversized configured
labels and malformed canonical item IDs refuse the entire projection. That
fixture runs on memory and pooled SQLite and never decodes its synthetic
credential envelope. The HTTP regression
checks independent offline groups, configured labels/counts without stale item
metadata, closed query parsing and current logout/assignment denial.

Projection and output are bounded, and the handler imposes a one-second Store
read deadline. The existing watch primary key does not provide a dedicated
user/recent index; high-cardinality scan qualification and a coordinated index
migration remain open. This checkpoint does not qualify automatic schema
installation, scoped artwork delivery, history move reconciliation, current
skip analysis, S5 session-bound progress writes or promotion.

Final native checks passed the three-backend Continue Watching contract (9.49
seconds), the raw corruption/projection reader test (0.32 seconds), the real
blocked B-only writer test (14.98 seconds) and all 27 daemon sharing tests
(28.92 seconds), each with zero ignored. Focused commands use pinned Rust
1.97.1, `--locked --offline` and `--features hiqlite-contract-tests` for core
or `--features plurx-core/hiqlite-contract-tests` for daemon. Filters are
`sharing_continue_groups_isolate_sources_filter_assignments_and_refuse_hidden_overflow`
under core `--test store_contract`, `sharing_continue_reader` under core
`--lib`, and `sharing_` under daemon `--bin plurxd`. These native checks
exclude the Linux-only pinned transport fixture.


**Bounded-detail combined integration:** Source details checkpoint `7b85efec8`
is integrated with the ownership, opaque observation and resource grammar
checkpoints through `325617428`. On that exact combined source, pinned Rust
1.97.1 core feature/daemon all-target checks passed (26.85 seconds and 1m08s).
All twelve core catalogue units passed (5.05 seconds), all five catalogue and
private-history store contracts passed (45.65 seconds), all eleven daemon
catalogue/blocked-connection regressions passed (12.44 seconds), and all four
HLS resource regressions passed. Every executed test had zero ignored cases.
Feature-enabled all-target Clippy with denied warnings passed (1m23s), and
all four docs-index tests passed. These results qualify the combined candidate;
Source playback admission, receiver cache authority and signed file delivery
remain open.


**Source reservation / receiver body combined integration:** Source binding
checkpoints `1bb19f392` and `4a82e9cbc`, receiver scope/body checkpoint
`5939dc2b2` and native caller checkpoint `8d7845b76` are integrated with the
receiver locator crypto/census candidates. The combined core all-target check
passed (26.16 seconds), and daemon all-target check passed (1m03s), both with
pinned Rust 1.97.1 and the replicated contract feature.

The fully qualified Source reservation, receiver login/import and Source file
witness voter contracts passed individually (10.07, 9.26 and 9.03 seconds), each
with a nonzero-test guard. All 87 sharing core units passed (52.90 seconds), and
all 27 native-platform daemon sharing regressions passed (19.28 seconds), with
zero ignored cases. Final feature-enabled all-target core/daemon Clippy passed
with denied warnings (1m33s). The Linux-only real CGNAT fixture is outside those native
results and must run against the committed combined source archive. No Shared
producer start, locator exposure or session delivery route is enabled.

**Source assignment / first activation / Continue Watching integration:**
Checkpoints `7b8d25fd7`, `5982f541b` and `a888e6356` are integrated on the
complete receiver projection and native-wire candidate `ca879be53`.
Assignment uses the actual local voter identity and retains the original opaque
membership observation. First blocked Source activation reuses the existing
Hiqlite session algorithm with a Source ownership selector, literal NULL local
user metadata, and a current-authority assertion at the front of the same write
transaction. Its standalone assertion also protects read-only replay. Neither
handle is a physical encoder permit; Source publication, renewal, recovery,
replacement, worker admission and post-reap release remain open.

On the exact combined candidate, pinned Rust 1.97.1 feature-enabled core/daemon
all-target check passed (1m23s). The actual three-voter Source contract passed
one case (10.54 seconds); ten Source/membership units passed (12.67 seconds);
the Continue Watching three-backend contract passed one case (9.50 seconds);
and its malformed raw-reader regression passed one case (0.31 seconds).
The existing Local dynamic-Store activation/preparation/settlement contract
also passed one case (11.88 seconds). All 31 daemon sharing regressions passed
(30.05 seconds). Every executed case
had zero ignored tests. Feature-enabled all-target Clippy with denied warnings
passed (1m31s), and all four docs-index tests passed. The Linux CGNAT transport
fixture remains pending execution from this newly committed combined source.

### 16.6 S5 resource grammar foundation

The candidate [relative HLS grammar](../../crates/plurx-core/src/sharing_resources.rs)
is derived from the actual
[router](../../crates/plurxd/src/http/mod.rs),
[master/subtitle generator](../../crates/plurxd/src/http/hls/playlist_text.rs),
[rolling start/discontinuity projection](../../crates/plurxd/src/transcode/rolling/segment_index.rs),
[init-object naming](../../crates/plurxd/src/transcode/session_request.rs), and
[TS muxer arguments](../../crates/plurx-core/src/transcode/mod.rs).
It preserves validated playlist bytes without rewriting URLs or fetching them.

| Resource | Closed relative grammar |
|---|---|
| Main playlists | `master.m3u8`, `index.m3u8`, `video.m3u8`; only bounded, nonduplicate `native`, `subtitle` and the existing diagnostic query vocabulary |
| Video objects | `init.mp4`, positive `init-e{epoch}.mp4`, `seg{number}.ts` and `seg{number}.m4s`; numeric values fit the engine's signed range |
| Native subtitles | `subs/{index}/index.m3u8`, `subs/{index}/seg{number}.vtt`; subtitle playlist children resolve within that exact track |
| URI-bearing tags | Existing subtitle `EXT-X-MEDIA` and init `EXT-X-MAP`; closed attribute names and exact resource kinds |
| Unsupported forms | Absolute/root-relative URLs, authorities, traversal, encoded separators, fragments, unknown query/URI attributes, keys/encryption, LL-HLS and unexpected tag/resource kinds |

Validation bounds one playlist to 1 MiB, a line to 8 KiB and the line/attribute
counts independently. It checks master/media/subtitle URI placement and
pending segment declarations. Four focused tests passed with zero ignored
cases, including the actual fMP4 generator output through segment index
100000 and explicit proxy/encryption/duplicate-attribute/body-bound refusals.
Pinned feature all-target check passed (28.27 seconds), and final denied-warning
feature Clippy passed (31.23 seconds). The hand-authored master/subtitle fixtures
record current emitted shapes; they do not replace an end-to-end producer
receipt. This helper does not authorize a grant, session, response publication
or a network fetch, and is not yet wired into a Shared relay.

Remaining S5 work includes complete start/decision envelope translation,
Source admission and producer-stop release, the B remote-source lifecycle,
signed file locators, Range/416/file/subtitle resources, current-login and
Source revocation cancellation, transport/resource budgets and real playback.


**Actual-generator and file-resource grammar follow-up:** the closed file suffix
parser now recognizes current decision/start, direct/progressive, VTT, PGS
manifest/object and chapter thumbnail paths. Track/chapter indices are canonical
and bounded; PGS generation/object digests keep their exact immutable grammar.
It refuses authorities, traversal, encoded separators, queries embedded in a
suffix and unknown resource forms. Authentication, immutable file/revision
membership, query validation and B session delivery binding remain independent
handler requirements; parsing a suffix grants none of them.

The HLS validator now accepts literal backslashes in bounded subtitle metadata,
which the actual master generator can emit, while every URI still rejects them.
A daemon regression runs the actual master generator across SDR/HDR10/HLG/Dolby
Vision, selected/forced/hearing-impaired subtitles, Unicode and quoted/comma
names, all current diagnostic masters and actual subtitle timeline generation
through segment sequence 100000. It preserves playlist bytes and refuses a URI
containing the same backslash.

On the combined `8cf6bfb24` base, pinned feature all-target core/daemon check
passed (1m15s). The file suffix regression, all four existing HLS regressions
and actual daemon generator regression passed with zero ignored cases. Final
feature-enabled all-target Clippy with denied warnings passed (1m26s).
This expands tested resource compatibility; no Shared relay/resource route,
Source worker, file locator or response-publication authority is enabled.


**Receiver-signed locator crypto candidate:**
[file locators](../../crates/plurx-core/src/sharing_file_locators.rs) bind the
receiver identity and epoch, import lifecycle generation, complete Source item
reference, exact file ID and file revision in a fixed, versioned HMAC-SHA256
frame. Canonical base64url encoding yields a bounded B-relative file base.
IDs remain exact through the signed integer maximum. Purpose-separated sealed
signing material survives master-key rewrap without changing issued locators.

Three focused regressions passed with zero ignored cases: complete reference
round trips and Source collision separation; every-byte tampering, malformed
encoding and signed invalid fields; master rewrap and deliberate signing-key
reuse across receiver identity/epoch boundaries. Final pinned feature-enabled
all-target Clippy passed with denied warnings (34.42 seconds), and the existing
file-revision regression also passed. Catalog lint covers the new module.

This is crypto evidence only. Durable receiver key installation, sealed-row
census, restart/clone qualification and authenticated file routes remain open.
A verified locator identifies a reference; every use still needs current B
viewer/import/assignment and Source grant/file authority, plus current B session
delivery authority for media. No locator is exposed by current item details.

**Durable key-read and census follow-up:** the optional candidate receiver key
table now participates in the startup sealed-row census. Both backends bound
its shape, identity and envelope reads; SQLite performs shape and data reads
in one snapshot. Replicated activation/rewrap still requires coordinated
quiescence, as with the existing catalogue-purpose key. These reads never
initialize, repair or replace material. Missing material returns unavailable;
partial tables, views, foreign identity, unsealed and oversized rows fail.

Five locator regressions passed with zero ignored cases (0.60 seconds), including
actual file-backed SQLite close/reopen, explicit master rewrap with an unchanged
issued locator, and independent malformed-state refusals on memory and pooled
stores. Pinned feature-enabled all-target check passed (28.16 seconds) and final
denied-warning Clippy passed (32.37 seconds). This qualifies those SQLite reads
and crypto only; the candidate table is uninstalled, and replicated key/restart,
clone and complete activation qualification remain open.

**S5 complete engine-envelope projection candidate:**
[response projection](../../crates/plurxd/src/http/sharing_playback_wire.rs)
preserves the actual serialized decision fields across direct, remux and
transcode. It replaces numeric file identity with its exact Source string and
the complete detail-style compound reference, and translates only closed engine
file URLs to the signed B file base. HLS starts retain quality/catalogue, ladder,
VOD, timing/origin, HDR/Dolby Vision, plan notes and actual control generation,
epoch and lease fields while replacing session/playlist/control URLs with the
ordinary B UUIDv4 namespace. PGS retains cue timing and geometry with string
identity/full reference and closed relative generation/object names.

Three focused regressions compare complete actual typed engine serialization
before and after projection, including IDs above the JavaScript safe range,
all decision methods, retained control epoch, URL/query/foreign-file refusals,
PGS generation and geometry escape refusals, and invalid start timing. They
passed with zero ignored cases (0.01 seconds). The projection module has no
registered routes. Pinned feature-enabled daemon all-target check passed
(50.62 seconds), and final all-target Clippy passed with denied warnings (1m33s).
It grants no current viewer, Source, worker or delivery authority. Peer decision decoding, current timeline/analysis proof, session
binding and actual relay/resource publication remain open.

The first exact combined Linux archive (`352b5644a`, SHA-256
`3e9008b55953063eb64f0cbdee2e5b7d1745aa208f5b6bb109ec1d5642f5b7b1`)
failed compilation in its Linux-only fixture because token deletion's boolean
was used as a unit match arm. The fixture now asserts successful token deletion
and yields unit. That archive has no Linux runtime qualification; the corrected
committed tree must be archived and executed again before any such claim.

**Source SQLite activation and current-owned renewal integration:**
Checkpoints `2925a231b` and `07e193f11` extend the same blocked first-activation
contract to SQLite and add opaque current-owned renewal to both stores. Renewal
requires the exact resolved Source binding, route, lease, pin, file revision and
fresh actual membership proof in the acquired write transaction. An expired
assignment observation cannot authorize renewal, while a fresh observation may
renew its unchanged assignment. Ordinary Shared renewal remains refused.

The combined pinned feature-enabled core/daemon all-target check passed (1m10s).
Twelve Source/membership units passed (22.25s); the actual three-voter Source
assignment/activation/renewal case passed (10.69s), and the unchanged Local
committed-successor renewal case passed (9.59s), all with nonzero-test guards and
zero ignored cases. Final denied-warning all-target Clippy passed (1m24s), and
four docs-index tests passed. Thirty-two daemon sharing regressions, including
the body-tracker correction below, passed on the combined candidate before the
renewal integration. Physical readiness/publication, resolved claim replay,
worker admission, producer settlement and post-reap release remain open.

### 16.7 S6 file-context integration foundation

The verified parallel checkpoint `4865de285` is integrated after catalogue
checkpoint `db220b18a` and membership fence `a6a60d53f`. The new plain-script
[file-context helper](../../crates/plurxd/src/web/core/file-context.js) has its
asset registration, shell tag and layout index in the same checkpoint.
Existing file-resource callers retain exact Local decimal identities; generated
onclick/onchange calls preserve IDs above JavaScript's safe integer range.
Shared contexts require the full Source reference and a B-relative opaque
file base from the detail adapter. Their keys include account generation, and
logout retires them. No numeric Source ID becomes a Local route or cache key.

Typed resource/query builders preserve the current engine vocabulary, including
named profiles, codec/container lists, Dolby Vision HLS and per-codec height
limits. Shared native media URLs carry only an exact bound B UUIDv4 session;
ordinary HLS/control/prepared paths retain their existing namespace. Progress
and continuation refuse unsupported Shared authority before a Local item
lookup. Current Source details have no file base, so Shared factories refuse
that unavailable delivery contract. This foundation does not expose Shared UI
starts or qualify a producer/relay.

All twelve focused context/caller regressions and the full `make web-check`
lane passed on the parallel final checkpoint. The combined tree independently
passed the same full web lane, all four docs-index tests and pinned Rust 1.97.1
`cargo check --locked --offline -p plurxd --all-targets` (1m04s). The lane
includes actual shipped caller/control fixtures, asset order/layout, settings,
JavaScript/TypeScript contracts and the existing documented contrast allowance.
Shared navigation/settings/details/Continue Watching and end-to-end player
behavior still require their S4/S5 authority and delivery integrations.


**S6 Shared browse candidate:** The web app now has separate Shared routes in
all three layouts, keyed by import, Source server, catalogue epoch, library and
item without converting Source IDs to JavaScript numbers. Assigned groups load
independently with four concurrent Source reads. Continue Watching is a separate
B-private group index and fresh per-Source request; unavailable groups publish
no stale items. Library search stays within the chosen Source/library, cursors
remain opaque, and full-reference deduplication bounds one rendered browse to
5,000 items. Shared details display current Source file descriptions without
calling Local numeric item/file/history routes. Playback is explicitly
unavailable until its session and delivery integration is qualified.

Settings → Sharing displays the current imports/exports page and node status.
The existing saved switch remains in Developer with advisory readiness; this
checkpoint adds no pairing, scope, assignment or credential-rotation mutations.
Every Shared catalogue and management read bounds the received body to 4 MiB
and checks the current login generation, bearer, B origin and page after each
stream read and before publication. The file, served row, shell tag, layout map
and generated JavaScript file list are updated together. Eight executed web
regressions cover identities above the safe Number range, escaping, account and
origin replacement during streaming, body bounds, independent Source failure,
opaque cursor encoding/repetition, duplicate items, concurrency and unavailable
Continue Watching. The complete web lane, its layout/shell checks, all 36
Settings section cases, and the unchanged type baseline pass.

**Management assignment snapshot candidate:** Administrator GET
`/api/v1/sharing/imports/{id}/assignments` returns the complete current import/
Source/epoch/lifecycle/assignment generation and state with grouped Source library
strings and exact local user integers. One bounded database read distinguishes
an existing empty matrix from a missing import; it rejects corruption, duplicate
pairs, more than 64 groups, more than 256 users per group or row overflow.
Whole-matrix writes remain generation guarded. Re-pair now advances assignment
generation in the same transaction as lifecycle and endpoint generation, so an
old matrix cannot save after a repaired import returns to active. Administrator
Source-library enumeration and client editors remain separate work.

The bounded raw-decoder regression passed (one case, 0.01s). The complete matrix
and user-lifetime contract passed on memory, pooled SQLite and three actual
voters (one case, 9.40s), as did the re-pair/private-viewer contract including a
pre-repair stale Save after activation (one case, 9.31s). The actual HTTP route
passed its administrator/login isolation, complete/empty/missing import,
no-store and full lossless response checks (one case, 0.20s). All 33 daemon
sharing regressions passed (29.12s), with zero ignored cases. The web ID parser
also accepts canonical zero consistently with SourceId and both native clients;
its eight browse regressions and unchanged type baseline pass. Pinned feature-enabled core/daemon all-target Clippy passed with denied warnings (1m26s), and all four docs-index cases pass.

**Administrator Source-library bootstrap candidate:** GET
`/api/v1/sharing/imports/{id}/libraries` reads current Source scope before B has
any viewer assignments. This is a separate administrator surface: a live token,
current administrator role and exact import lifecycle/claim/grant are read in
one consistent authority query. The existing viewer query still requires its
own assignments. The fresh pinned Source response has at most 64 unique
movie/show libraries and 256 UTF-8 bytes per name. Empty scope is a successful
empty read, while unavailable Source authority refuses the read.

The response is buffered and authorized against the current Source tuple at
admission; it starts no monitor. Role,
login, import, Source grant or sharing-switch loss cancels current connection
authority. The isolated Linux H1/H2 regression is added but still awaits an
exact committed archive run. No cached scope or Local library set bootstraps the
editor. Backend administrator bootstrap/current-role proof passed on memory,
pooled SQLite and three actual voters (one case, 9.28s); unchanged viewer proof
passed (one case, 9.26s). HTTP management authentication isolation passed
(one case, 0.19s); the unchanged idle-expiry/bad-policy reader passed
(one case, 0.32s). All 33 daemon sharing cases passed (29.07s), zero ignored.
Pinned feature-enabled all-target check passed (1m30s), final denied-warning
Clippy passed (1m21s). These native checks exclude the Linux-only new fixture.

**Linux fixture observation correction:** The exact `c220f6270` source archive
(SHA-256 `0072b4b224ab34d05be670b21a7bcc9bfb6a0cbed8b23d479a7cc0d3c4d36c3f`)
compiled with pinned Linux Rust 1.97.1, but its CGNAT fixture failed twice while
awaiting the test body's drop flag (7.28 and 7.27 seconds). Its test decorator
tracked item paths but omitted the newly tested Continue Watching path, so that
flag was never attached. The decorator now includes the exact per-import
Continue Watching route, with a focused path regression. No observation timeout
or production cancellation code changes. The corrected committed archive still
requires execution before any Linux runtime qualification claim. The corrected
`4ebb85226` archive (SHA-256
`d2cd30d31dfab1b67c59b71b1aa25db67834296397f58527814748980cfe7988`)
compiled in 3m54s, then reached the details fixture and failed because its three
seeded files reused a unique path (6.38s). Each file now has its own fixture path;
no production file or serving logic changes. The independent pinned transport
claim/rotation restart case passed on that archive (one test, zero ignored,
0.43s). The combined blocked-response fixture still requires an exact corrected
archive run; neither result proves actual Tailscale or two-NAT operation. The exact
`719fde0bd` archive (SHA-256
`b8698779b2f356c81c9bebe11c78b75acb973f8ff7979b368300031a6cf5c314`)
first lost its compiler to signal 9 without a Rust diagnostic. The same frozen
source then compiled in 2m40s after coordinating heavy Docker workloads: the
blocked scope fixture passed all 18 actual HTTP/1 and HTTP/2 scenarios in one
case (23.06s), and the independent claim/rotation restart case passed (0.37s),
both zero ignored. Log: `/private/tmp/plurx-sharing-719f-linux-retry.log`.
This qualifies the isolated protocol fixture on that commit; it does not
qualify the subsequently added administrator or artwork routes, actual
Tailscale, two-NAT operation or physical playback devices.

### 16.8 S7 native file-context foundation

The parallel native checkpoint `52b27c2b4` adds immutable full-reference contexts
and account-generation seams to Apple and Android. Shared factories fetch the
actual B detail through the authenticated B origin, capture authorization before
the await, and refuse changes afterward. They validate canonical string file
identities/revisions, the complete import/Source/epoch/library/item reference
and the exact B-relative opaque locator. Private constructors provide no raw
Source-to-Local conversion. Optional delivery binding uses B's existing UUIDv4
session grammar, and logout/account replacement retires captured contexts.
Local Apple Int/Android Long identities retain the existing exact i64 wire.

Four iPhone context XCTest cases passed, including actual mock HTTP origin and
bearer assertions, large IDs, reference/revision mismatch, malformed locators,
account ABA and authorization change during fetch. Four Android context and
five existing Session JVM cases passed. Unsigned iOS and tvOS builds passed;
Android used the pinned source-only JDK25/SDK compiler image, not the host JDK21.
The checkpoint's normal catalog/format/workspace Clippy/JavaScript hook passed.
Its source-only archive hash is
`787a598af7a07cf6058c07582d66b3be2e5824a27de34e85e2ed0b5778d2c6c1`.

Integration preserves that tested native source but is not a combined native
qualification receipt. Caller propagation, full resource/query vocabulary,
PGS/reopen/prepared adapters, Shared UI/decision models and physical TV playback
remain open. Current B details omit the locator, so the native Shared factories
refuse that unavailable contract rather than selecting a Local file.

**Native caller integration follow-up:** checkpoint `8d7845b76` propagates
immutable file contexts through actual Apple/Android decision, HLS retry,
progressive/remux, reopen and PGS callers. Captured context survives asynchronous
work; current account generation and exact Local IDs are checked before requests
and continuation. Shared contexts are refused before numeric Local decision,
PGS, progress and next-item adapters. Full query vocabulary is translated through
closed builders, and no Shared start is exposed.

The checkpoint's exact source-only archive SHA-256 is
`aa7fb6a40bb78d89611cb2a7d81a0cbe24ee6649f1a0c6933d52f56197070a45`.
Its affected iOS 106 and tvOS 101 tests and builds passed with zero ignored cases.
Android's identical native source tree passed 26 focused tests; the exact archive
also passed fresh `lintDebug` (4m21s) with pinned Temurin 25.0.4.1 and Gradle 9.7.1.
The normal hook passed. Integration has no native diff against that qualified
checkpoint. These are caller/context regressions, not Shared producer, device
playback, new Shared wire-model or UI qualification.

### S4 opaque artwork candidate (2026-10-03, integration qualification open)

Source catalogue rows may now carry at most eight closed artwork facts, each a
kind, variant and 143-character opaque resource. Source paths are
`/sharing/v1/art/{resource}`. B translates fresh facts into user/import-bound
272-character resources under `/api/v1/shared/imports/{import}/art/{resource}`;
`poster_url` (poster/w300) and `backdrop_url` (backdrop/w780) exist only with the
corresponding mapped resource. Missing existing purpose material leaves artwork
fields absent. There is no numeric Local image route or original fallback for
an unpublished canonical variant.

The MAC domains are distinct from file revision, cursor and media locator
signatures. They use the existing stable Source/epoch catalogue key and B/epoch
file-locator key material; neither key is exported or initialized by a read.
Resources bind the full Source/epoch/grant/library/item/kind/variant, and B also
binds current user and import lifecycle. Expiry uses the actual captured server
clock, with a maximum five-minute lifetime and thirty-second mint buckets.
Credential rotation and sealing-master rewrap preserve purpose material. Current
Store authority is still required on every request; the opaque resource is no
replacement for current login, effective assignment, grant, catalogue floor or
import fence.

Source resolves only the current private persisted artwork name, opens through
the no-follow descriptor helper, and hashes the actual bounded bytes every time.
A canonical derivative requires a current published local Store location; the
shared reader bypasses the existing twenty-four-hour location cache. Current
item/grant authority is checked before demand and again after asset selection.
The existing worker retains its fragment admission, derive permit, claim and
joined child cancellation through settlement and cleanup. Its demand table is
bounded to 4,096 entries/30-second deduplication, candidate pages to 128, and
encoder work to five seconds/15 MiB output. This preserves the existing worker
and governor path; it adds no assertion of a new hard encoder RSS limit.

An absent local original uses a Shared-specific collector: at most three current
trusted roster destinations are tried sequentially within two seconds, with one
preallocated 15 MiB object plus sentinel and a separately charged object-sized
transport workspace. Its HTTP/1-only client uses no proxy or redirect, node proof,
actual content signature and digest validation. It rechecks current authority
before moving its allocation directly into the held-directory blocking writer.
Source, local-image and peer permits remain owned through actual write settlement.
The ordinary three-peer racer and derivative scheduler are unchanged.

A successful asset response identifies the exact opened-byte snapshot with its
SHA-256, byte count, closed MIME and variant. Source and B each admit at most
four operations without queuing, with an independent 64 MiB byte budget and a
15 MiB payload limit. Foreground Source derivatives reserve original plus
variant; B reserves bounded fetch/disk working space. Reservations include the
read sentinel and shrink only after work settles. Blocking reads, disk hashers
and atomic writers retain actual lease ownership after their async caller is
cancelled. The art lease now lives inside the response bytes and returns when the writer
drops them, so idle connections no longer hold artwork capacity.
Revocation closes the accepted connection, including unrelated multiplexed H2
streams. A benign asset replacement affects the next fresh read while an already
accepted snapshot remains under current grant/item body authority.

B's private `sharing-art-v1` managed disk namespace is bounded to 256 MiB and
2,048 entries. Serialized publication evicts deterministically by current file
age/name, uses held-directory atomic writes, and refuses malformed files,
symlinks or an invalid cache shape. Each reuse hashes the current opened cache
file only after a fresh pinned Source response proves that exact digest. The
response uses fresh Source bytes; offline, busy or revoked Source authority
never serves disk bytes. Cache keys include user, import lifecycle, full Source
identity, grant, item, kind, variant and digest. Artwork RAM has its own byte
owners and is not retained in the 32 MiB catalogue metadata cache.

Focused native checks cover purpose separation/rewrap, large decimal IDs,
malformed and oversized resources, closed optional artwork arrays, current
memory/pooled Store scope and private-name bounds, opened inode replacement,
same-inode changes, symlinks, binary digest/size/encoding/variant and redirect
refusal, nonqueued byte capacity, disk LRU concurrency and user isolation, and
actual blocked H1/H2 Source response cleanup, completed idle H2 capacity, and
refused variant demand with existing worker deduplication. The three-voter candidate Store
reader and owned atomic-write cancellation test exercise the production Store
and filesystem boundaries. Final exact-tree receipts are appended after
integration. The opt-in pinned CGNAT fixture now includes B artwork cases 9–15:
Source grant/item deletion/move, B login/import/assignment loss and Source
unavailability, with accepted DATA, operation/byte cleanup and H2 collateral
checks. Its artwork extension remains unqualified until the exact committed
Linux source archive runs. Source schema/key installation, capability
advertisement, actual Tailscale/two-NAT topology and physical-device artwork
qualification remain open, as do the separately recorded S5 history-progress
binding and explicit history-move reconciliation dependencies.

The pre-integration candidate passed pinned Rust 1.97.1 all-target core/daemon
checking; core artwork tests (2 passed); daemon artwork tests (6 passed, zero
ignored, 13.87 seconds); existing worker publication/child-cancellation tests
(2 passed); the actual three-voter artwork Store test (1 passed, zero ignored);
and the owned filesystem writer cancellation test (1 passed). Documentation
index checks passed (4 tests). These receipts qualify the candidate on its
recorded base, not the newer integrated effort or the Linux-only artwork cases.
**Native Shared wire-model candidate:** integrated checkpoint `5d42f292c`
adds separate, lossless Shared decision/start envelopes and string-ID PGS
models, plus a typed Local/Shared subject. Validation binds all import/Source/
epoch/library/item/file/revision fields to the captured current-account context.
Descriptive file URLs do not confer media authority; actual delivery still
requires the bound B UUIDv4 session and its exact playlist/control namespace.
The models preserve the full received B payload without interpreting private
recipe-looking metadata as authority, and retain the PGS containment checks.

The exact native checkpoint passed 10 iOS, 10 tvOS and 10 Android tests with
zero ignored cases, Android lint (4m16s) and its normal hook. The source-only
archive SHA-256 is
`53a9c0b966fd9f1eed0f6fd232348a445d025efc13e973e0671d20c7ddcece8d`.
Integration has no native source diff against that qualified tree. Shared
navigation, settings, playback starts and progress/history remain separate
unfinished integrations.

**S7 Shared browse integration:** Native checkpoint `cee083a8e` adds separate
Shared groups, library pages and details on Apple and Android, preserving the
complete import/Source/epoch/library/item reference throughout navigation.
Each Source loads and retries independently. Details display B-owned remote
watch state and Source files without Local item/history/image fallbacks.
Settings shows current sharing summaries, while Developer retains the saved
choice with advisory readiness. An in-flight read or Save cannot overwrite a
newer user edit. Shared playback, pairing/scope/assignment mutations, optional
artwork rendering and physical-device qualification remain open.

The exact source-only archive SHA-256 is
`81a72a8d3f270ab65875e2467d6fc77855c3a5c4c2c06f161580a17caf36cd54`.
Its 381 tracked native files byte-match the qualification input. Fifteen iOS,
fifteen tvOS and fifteen Android focused tests passed with zero failures, all
affected Apple UI sources compiled, and exact-head Android lint passed (4m33s).
Integration has no native source differences against that qualified checkpoint;
its normal tracked hook passed before integration. Native parity checkpoint `b41e3f8e8` also integrates unchanged: authenticated
Shared details use the same 4 MiB bound as B, including a body with three actual
maximum-sized chapter lists. Seven iOS, seven tvOS and seven Android tests pass;
exact 4 MiB passes, one extra byte and a missing locator refuse, and absent
Content-Length cannot bypass the streaming bound. Android lint passed (5m56s),
its normal hook passed, and all 381 native files byte-match the tested source.
Archive SHA-256 is
`01aa335f5dee172a5d7f3eccb7651cffd3b445d81215afb9e5e758734362330f`.

The artwork candidate integrated the full `bf9747e6` effort ancestor and passed
pinned Rust 1.97.1 all-target core/daemon checking (49.41 seconds), feature
Clippy with denied warnings (1 minute 28 seconds), core sharing regressions
(95 passed, zero ignored, 48.07 seconds), daemon sharing regressions (39 passed,
zero ignored, 43.49 seconds), and the exact fully qualified three-voter artwork
reader regression (1 passed, zero ignored, 9.00 seconds). Documentation index
checks passed (4 tests). The merge retains the current admin authority seam,
Continue Watching tracker and unique Linux fixture paths. Linux artwork cases
remain open until the archive of this committed integrated candidate runs.

Artwork's integrated Linux qualification completed on Root candidate
`5b64da576d15eace7ad28390b77f9d0e89691c39`, from a credential-free `git archive`
with SHA-256 `7d9c986370db07e52cf54c1122408104212370c14796fcfc56b61b66a3631077`.
Pinned Rust 1.97.1 compilation passed (5 minutes 33 seconds); the opt-in CGNAT
fixture passed all 32 H1/H2 scope/artwork cases (one test, zero ignored,
55.40 seconds), the independent admin fixture passed its ten transport cases
(one test, 7.46 seconds), and claim/rotation restart passed (one test,
0.37 seconds). This proves the actual pinned transport and accepted-body cleanup
in the isolated fixture; actual Tailscale, two-NAT and physical-device claims
remain open.

### S4 purpose factory and restore foundation

The candidate purpose factory uses a separate `sharing_purpose_keys_v1` floor
and an opaque observation of the current Raft roster, membership generation,
and each member's heartbeat-coupled selected sealing-master fingerprint. The
fingerprint is a fixed-domain 256-bit HMAC derived from the actual selected
master and has exactly 64 lowercase hex digits; the existing eight-digit key
ID remains diagnostic. Actual AEAD opening of every active and archived
envelope remains mandatory. Its actual write
checks that proof again, both active rows are empty, import mode is absent or
closed, and no census intent exists. It installs the Source catalogue revision
key and B file locator key together with a singleton installation marker.
Reopening verifies existing material and preserves the winning ciphertext;
partial, empty, malformed, foreign-identity, and mixed-master states refuse.
Checkpoint `7f03fea23` did not advertise the capability or run an installer at
startup. Its distributed startup census, removal cleanup, actual one-voter
factory, replicated restart and daemon coordinator were unqualified; the newer
coordinator receipt below records those paths separately.

The existing offline restore fencing transaction now moves the complete old
purpose pair into a sealed archive before deleting active keys and the old
sharing identity, retaining the old purpose/Source/epoch AAD. Archive capacity
is 128 retired epochs (256 rows); overflow refuses rather than deleting keys.
The explicit `restore_pending` marker permits a later guarded factory for the
new identity. Normal empty active tables have no such disposition. Backup
verification includes this bounded strict active/archive census and opens all
purpose material with the supplied sealing key. Complete purpose rewrap
checks the captured active/archive ciphertexts in one guarded write and keeps
the clear signing bytes and original AAD identities. This is purpose-material
rewrap within the existing convention; a global runtime master-file replacement
operation is not implemented.

Focused evidence uses pinned Rust 1.97.1: the actual three-voter factory/rewrap
contract passed with zero ignored tests, and the single/pooled SQLite offline
restore regression passed with foreign keys enabled. These are foundation
receipts, not startup installation or full shared-library qualification.

### S5 authenticated receiver decision adapter — live delivery remains open

B's POST file decision route accepts an authenticated current account, a
current signed file locator, and the actual version-2 device capabilities.
It checks the saved switch, active import, full Source identity and current
library assignment before opening the existing sealed credential and dialing
an approved pinned endpoint. Source uses its ordinary decision engine after a
fresh grant-authorized complete file witness and revision check, then checks
that authority again. Neither side creates a media session for this read.

The peer reply binds the exact requested Source/epoch/library/item/file/revision
and protocol. Its decision decoder retains every current engine field,
including audio/subtitle selection and ladder entries. Unknown fields, type or
method/delivery disagreement, duplicate JSON keys, excessive nesting and an
excessive tree refuse. The response has a 4 MiB wire limit, a 16,384-node limit
and depth 32. B rewrites only the closed file URL suffixes into its signed
namespace and publishes the string file identity with the full shared
reference. Source file zero is valid. No Source URL, Local file route or LAN
bandwidth observation supplies receiver authority.

Pinned Rust 1.97.1 all-target checking and denied-warning Clippy passed on the
adapter. Five complete decision/start/PGS projection tests and the dedicated
JSON allocation-budget regression passed with zero ignored tests. The peer
client regression also passed ten envelope/reference/redirect/body-bound
cases (one test, zero ignored). The opt-in pinned Source/B H1/H2 decision
fixture passed on committed adapter `5fd1b3af16f228829c8d6da8683149517fa813be`,
from source-only archive SHA-256
`1585de991b511d1bf954743011211d73642b9a5de739b675c14e51f81f10735e`.
Pinned Linux compilation passed in 3m47s and the exact regression passed
(one test, zero ignored, 2.06 seconds): complete engine projection, forged
locator, changed Source revision, removed viewer assignment and revoked B
login, over both H1 and H2. Both session tables remained empty. The daemon's
normal features already include `hiqlite-store`; a daemon-only test profile
with 256 code-generation units and one build job kept this qualification
within the isolated compiler's memory allowance. Earlier attempts with
unneeded contract-helper exports were killed before tests and supply no pass.
Actual Tailscale, two-NAT and physical media evidence remains open. Source
starts, receiver sessions, media relay and progress remain separate open work.

### S7 native artwork integration — physical device evidence remains open

Apple and Android Shared browse/details now display only advertised poster and
backdrop descriptors. Immutable authenticated subjects retain the full shared
reference and accept only the matching B-relative artwork namespace. Requests
use the current B account, refuse redirects and unsupported content, and bound
the actual stream even without a trustworthy Content-Length. Account change,
view retirement and task cancellation cannot publish an old completion.

Each process admits four artwork operations without queuing and accounts for
64 MiB of compressed ownership, with a 15 MiB limit per object. A separate
64 MiB allowance covers owned decoded pixels. Swift's actual image provider
release callback retires backing ownership. Android's private bitmap owner
requires confirmed recycle before returning pixel credits; a collected or
retired view alone does not return them. These allowances do not claim to
measure opaque decoder internals or physical GPU/render caches.

Checkpoint `1c7cff4065370c74843d3cafabdb566ee9307723` was qualified from source
archive SHA-256
`a1cfd55ac48427caab3fe4188627b493ae219ca5125f10b3f67af2653bfb4e0f`.
All 395 native files in the integrated tree byte-match that input. The affected
iOS, tvOS and Android suites each passed 35 tests, including seven artwork
regressions. Exact-archive Android lint and instrumentation APK compilation
passed, as did the normal pinned Rust 1.97.1 tracked hook. Android's actual
native bitmap decode/recycle instrumentation is compiled only: no emulator
or physical-device execution is claimed. Shared playback remains open.
### S4 coordinated purpose startup — candidate qualification

The coordinator candidate integrates the complete effort ancestor
`bf9edf9d6`. Actual legacy and replicated master-opening paths claim a
node/raft/boot-attempt census intent before inspecting material or selecting a
key file. Shape and count refusal precede potential key-file creation, then
actual AEAD opening precedes exact-attempt release. A failed or cancelled boot
retains its intent. Same-node restart advances the durable generation under
the existing startup ownership convention; an old attempt cannot release the
new one. Replicated claims require the actual admitted SQL node/raft and clear
that node's old purpose/master proofs. Fenced node removal clears only that
node/raft's census intent in the same removed-at write. Census-only schema is a
recognized pre-factory state; partial active-key installation still refuses.
The first activation also repeats this census against the actual replicated
store after its admitted node row exists, before publishing a purpose proof.
Initial legacy SQLite row import refuses installed or partial purpose material
before changing migration artifacts because its fixed inventory cannot retain
those optional rows. The existing replicated restore path copies the full
image and keeps its explicitly archived ciphertext.

Actual daemon startup prepares its selected master and publishes the protocol
capability and full fingerprint in a serialized heartbeat. This proof-changing
heartbeat cannot coalesce with an earlier heartbeat that lacked the proof.
The capability means binary protocol support and actual selected-master proof,
not installed-key readiness. A five-second startup/heartbeat coordinator runs
only for the saved `sharing_enabled=true` choice; it never changes the choice.
SQLite recovery has no actual replicated membership observation and cannot
mint purpose installation authority. Normal standalone startup uses the same
actual one-voter replicated path as a larger cluster.

First installation uses a separate opaque bootstrap observation of the actual
current Raft roster and selected master. Complete absence permits plain CREATE
of the exact closed membership guard schema and generation zero in a guarded
transaction; partial, incompatible or legacy two-capability shapes refuse
without ALTER or repair. A competing loser can adopt only a complete validated
winner. Existing nonzero generation is retained. The key factory then obtains
a fresh normal purpose observation. Same-write predicates include the exact
SQL/Raft roster, current generation/master proof, no membership transition,
closed import mode and no census owner. Final transaction CHECK assertions
verify the actual key rows and installation marker, so ignored insertion or
rewrap updates cannot leave a partial commit. The purpose transaction guard
refuses attached triggers; complete active/archive rewrap checks exact captured
and replacement ciphertexts in the same write.

Source admission preserves its legacy Principal/Catalogue floor only while
all purpose-material/installation markers remain absent in the actual write.
Even malformed marker presence invalidates an older cached legacy observation.
Presence requires all three capabilities and the privately captured selected
master fingerprint coupled to every current heartbeat; a missing prepared
master or lost purpose/master proof prevents a new observation or guarded
mutation. Census-only boot metadata is excluded from this Source marker, while
join and promotion retain their broader purpose-protocol fence. Actual Raft
voter proof governs key installation; nullable legacy SQL voter roles keep the
existing non-learner interpretation.

Focused qualification records below distinguish actual replicated startup and
concurrent factory tests from the SQL tombstone cleanup regression. The latter
proves that a removed-at write retires only the exact node/raft census claim;
it does not independently qualify the complete Raft removal protocol. The
existing data-directory startup lock supplies local boot ownership. Global
runtime master-file replacement, actual Tailscale/two-NAT transport and final
shared-library promotion remain outside this receipt.

On the `bf9edf9d6` ancestor with this coordinator and the Source fixture updates,
pinned Rust 1.97.1 core/daemon all-target checking passed (1 minute 15 seconds).
The purpose unit filter passed 13 tests with zero ignored (15.01 seconds),
including actual one-voter store selection, install, nullable legacy voter role,
selected-master refusal and durable readdress/restart ciphertext preservation.
The actual three-voter purpose contract passed with zero ignored (9.82 seconds):
competing schema/key factories preserve their winner, complete legacy
two-capability and partial schemas remain untouched, census and stale master
proofs refuse, and an ignored locator rewrap update rolls the entire active/
archive/marker transaction back. Source admission unit checks passed 17 tests
with zero ignored (28.22 seconds); its actual three-voter claim/replay/capacity/
release regression passed with zero ignored (11.60 seconds) using the real
fixture master and heartbeat-coupled fingerprint. These tests do not claim
physical producer or public network qualification.
Feature-enabled all-target Clippy with denied warnings also passed (1 minute
27 seconds), and the documentation index suite passed all four tests.

### S4 Source schema startup — bounded nonrolling factory

The normal daemon calls the single-use `SelectedStore` Source coordinator
before constructing `State`, opening application listeners or probing media
workers. SQLite recovery cannot obtain its authority. Normal standalone uses
an actual one-voter replicated store, with the same master and member-proof
factory as a larger topology. The current binary advertises Principal,
Catalogue and Purpose protocol support after its selected master passes the
actual sealed-material census; this advertisement does not claim installed
schema or permission to create a producer.

Replicated v70 remains the ordinary readable baseline. The binary also
supports v71, whose installation is one guarded Raft transaction: the frozen
seven-family principal rebuild, catalogue membership/order maintenance,
monotone item allocator, Source binding adjunct and final installation marker.
The factory requires the exact current Raft/SQL roster and generation,
heartbeat-coupled selected-master proof, all three sharing capabilities,
no membership or census transition, current saved Sharing=true, and fresh
startup attempts from every active member. Each member advertises only its
current boot UUID; its heartbeat deletes older boot labels. A retained row
from a stopped process does not prove that process is still in startup.
The same-write assertions validate the complete admission schema, captured
boot attempts, exact installed definitions and successful final guard row.
A competing coordinator adopts only the complete validated winner.

The predecessor census freezes the thirty affected table/index/trigger
objects, including the refresh trigger on the background command table.
Only explicit known SQLite ALTER-built and replicated declarations are
accepted; the historical v10 session ALTER chain has its own frozen variant.
Unknown persisted columns, changed constraints, extra indexes, substituted
triggers, partial principal/allocator state and nonpositive Local user
ownership refuse the rebuild. Existing Local rows retain their original
columns through the frozen copy projections. Active sessions, starting
requests, preparations and live worker leases prevent installation; the
coordinator never drains or deletes them.

A twenty-second incomplete-floor wait leaves Shared readiness pending only
after the exact own boot attempt has been confirmed released and the unchanged
legacy layout has been checked again. Startup also retires its own previous
crashed process's intent after the actual selected-master heartbeat has
withdrawn the old boot label. Unknown release outcome or an ignored deletion
prevents serving. A complete recognized two-capability guard is a Local-only
pending state: its saved Sharing choice and schema remain untouched, with no
key mint, ALTER or rebuild. Unknown or partial guards and unreadable material
refuse startup even when Sharing is disabled. The advisory switch remains
saved; readiness never substitutes a different choice.

After commit, the node's durable activation file records the actual committed
schema version, rather than the binary's maximum. A restart verifies every
installed definition before serving and does not repair missing objects.
The media-session projection cache is initially uncaptured: the normal boot
performs this transition before its first session reader. There is no cache
reset or schema rebuild during serving, and the heartbeat coordinator never
installs Source DDL.

These startup fixtures qualify schema authority and bounded Local-only
readiness, not active playback drain, interruption during the rebuild,
production backup tooling, a global runtime master replacement, physical
Source dispatch, Tailscale/two-NAT operation or final effort promotion. The
historical daemon/whole-topology receipt above remains scoped to its recorded
candidate; it does not become evidence for this v71 factory merely because
both use the same seven-family copy statements.

The focused startup and schema regressions are:

```sh
cargo test --locked -p plurx-core --features hiqlite-contract-tests --lib actual_source_schema_coordinator -- --nocapture
cargo test --locked -p plurx-core --features hiqlite-contract-tests --lib sharing_source_schema -- --nocapture
cargo check --locked -p plurx-core -p plurxd --all-targets --features plurx-core/hiqlite-contract-tests
cargo clippy --locked -p plurx-core -p plurxd --all-targets --features plurx-core/hiqlite-contract-tests -- -D warnings
```

On the initial `75882facb8d` development ancestor, the final `source_schema`
filter passed nine tests with zero ignored (109.43 seconds): five schema units
and four actual replicated startup cases. These include one-voter install,
restart and missing-index refusal; retained Local lease and crashed-intent
retirement with Sharing disabled; complete legacy guard pending readiness and
disabled partial-guard refusal; and three-voter single-boot refusal followed by
full-stop competing installation. Feature-enabled core/daemon all-target
Clippy with denied warnings passed in 1 minute 24 seconds. The schema migration
chain regression and existing compatibility regressions also passed. This is
a development checkpoint; qualification must run again after integrating the
current effort and canonical-zero Source adjunct correction.

The full intended-base integration with `b4e04e417` retains the canonical-zero
Source binding checks and incoming receiver/client changes. On that exact tree,
`--lib source_schema` passed nine tests (104.41 seconds),
`--lib sharing_source` passed twenty-two tests (39.03 seconds), and
`--lib purpose` passed fourteen tests (15.48 seconds), all with zero ignored.
The actual three-voter Source reservation contract passed in 11.78 seconds;
the three-voter purpose factory/master-proof contract passed in 9.81 seconds.
Affected feature-enabled core/daemon all-target compilation passed in
1 minute 8 seconds and Clippy with denied warnings passed in 1 minute 38 seconds.
The actual fourth-learner raw admission/promotion capability-race regression
passed in 11.44 seconds. These remain startup and Store receipts with the limits
above; they do not qualify a live relay or promote the complete effort.

### S5 B remote activation checkpoint — delivery remains closed

The receiver now has a distinct `remote_source` recipe with complete import,
Source server/epoch/library/item/file/revision, lifecycle, canonical bounded
playback request, and a persisted Source request UUID equal to its B incarnation.
Its request fingerprint includes the complete remote intent and excludes only
that planned UUID, so an exact request can recover its already-persisted UUID.
No B media file, decoder or encoder is created by this storage path.

A Store-produced receiver authority captures the current B login, expiry policy,
user and effective import/library assignment. Its nonserializable handle lasts
at most five seconds; the committing transaction repeats the login, switch,
policy and complete scope checks. The existing ordinary Local activation refuses
`remote_source` recipes. The dedicated receiver activation commits the blocked
ordinary B session, pointer, lease and exact pending upstream binding atomically.
Lost authority aborts every activation write. Exact replay checks the stored
binding; a missing or mismatched binding refuses without repair.

Ordinary retention sweeps keep unresolved upstream bindings, their session and
request records, and associated lease metadata. Request expiry cannot reacquire
that incarnation while the binding remains. This records uncertainty; it does
not prove Source shutdown, renew an expired serving lease, or authorize a new
Source allocation. Explicit proof-bearing retirement is still required before
those bindings can be removed by the delivery owner.

The focused SQLite regression passed across in-memory and pooled storage, both
original and rebuilt principal layouts, and forty current-authority/identity
cases (one Rust test, zero ignored, 14.17s). The actual three-voter regression
passed with zero ignored (9.32s), covering atomic admission, replay, revoked
login/assignment, changed policy/Source epoch, expired unresolved retention,
refused replacement, and missing-binding refusal. The low-level fixture's
synthetic request is never dispatched to a Source worker. Pinned Rust 1.97.1
feature-enabled affected all-target Clippy passed with denied warnings (1m23s).
The Source unit filter passed seventeen tests, zero ignored (28.32s).

This is the receiver storage/admission checkpoint. The Source-owned live start,
B ownership/renewal/retirement actor, authenticated stream relay, delivery grants,
ordered session-bound progress, and client playback remain open. No public
Shared start is enabled by this checkpoint.

On the same receiver checkpoint, the corrected `purpose` test census passed
fourteen tests, zero ignored (15.46s); it includes the process-priority purpose
regression as well as the thirteen sharing/key cases. The registered Local
rebuilt-principal activation regression passed (0.94s), and the existing actual
three-voter Source reservation and purpose factory contracts passed with zero
ignored (11.56s and 9.78s). All four documentation-index tests passed.

### S5 receiver decision context — authenticated file alias

Authenticated receiver item details now carry the current positive import
`lifecycle_generation`, repeat it in each complete file reference, and advertise
`file_base` only when the receiver's actual sealed locator key is available.
The receiver signs the complete Source/import/library/item/file/revision and
lifecycle tuple; Source metadata cannot choose the receiver URL. A missing key
keeps details browseable without advertising a decision context. Decision and
PGS projections carry the same lifecycle field so native clients can compare
responses against their captured authenticated context.

The focused alias regression passed with zero ignored, including Source ID zero,
lossless `i64::MAX` identity/lifecycle, stale lifecycle refusal, foreign Source
refusal, and browse-only key unavailability. All five complete engine/PGS wire
projection tests passed with zero ignored on pinned Rust 1.97.1. The disposable
CGNAT HTTP/1 and HTTP/2 fixture now reads the actual authenticated item alias
before requesting a decision; its changed-tree Linux execution is still pending.
This does not enable playback or grant delivery authority.

### S5 original-login binding and pending receiver renewal

The persisted remote recipe now records its original B login hash. A current
login for the same user cannot adopt another login's film; the Store authority
factory compares the actual login hash against that durable recipe binding.
A dedicated pending-renewal writer extends the exact blocked session, its
original starting request and matching job lease in one guarded transaction.
It requires fresh original-login/policy/import/library authority, the existing
owner epoch and pointer, the complete pending Source tuple, an unresolved
Source binding, and live existing request/lease deadlines. It neither replaces
the Source request nor publishes the session. Refused authority rolls every
write back; expired or differently owned obligations are never resurrected.
Ordinary incarnation-only Local handoff, renewal and takeover paths refuse a
remote recipe and require the dedicated receiver proof instead.

The expanded SQLite regression passed across both storage modes and principal
layouts (14.51s, one test, zero ignored), including a second valid login for the
same user, exact owner/request refusal and six current-authority/upstream/lease
races with unchanged durable renewal metadata. The actual three-voter contract
passed (10.02s, one test, zero ignored), including guarded pending renewal and
atomic revoked-assignment refusal. The focused existing Local worker and
rebuilt activation regressions passed (0.85s and 0.98s). Pinned Rust 1.97.1
feature-enabled affected all-target Clippy passed with denied warnings (1m39s).
This supplies a pending ownership writer; the B actor, Source attachment,
publication, delivery and ordered progress remain open.

### S3 Source decision marker evidence before physical admission

Shared decisions now use the common engine with an explicit stored-evidence
marker policy. Existing persisted annotations win; otherwise only bounded
scan-time chapters supply chapter evidence. Missing stored chapters never
launch a live FFprobe, backfill a probe document, or create annotations during
Source decision/preparation. The ordinary Local derivation path is retained.
This closes the unadmitted chapter-probe path that the actual Source preparation
fixture exposed when a live fallback changed its captured file revision.

The focused actual-engine regression passed (0.22s, one test, zero ignored).
It proves that a Source decision does not enter the paused live-probe seam,
that stored probe/annotation data stays unchanged, that existing chapters and
persisted annotation evidence are preserved, and that Local fallback still
enters its original seam. The existing complete Source preparation/principal
engine regression passed on the same source. This marker policy grants no
producer or playback authority.

### S6/S7 decision clients — playback remains unavailable

The native clients and web shell now consume the authenticated receiver file
alias and captured lifecycle/full reference when requesting the actual shared
decision. They retain runtime v2 capabilities and complete existing engine
fields, refuse foreign or stale identity, and cancel work when their captured
account/context is retired. Responses are bounded to four MiB, redirects and
implicit retries are refused, and Source media identities stay lossless strings.
These adapters do not invoke Local start APIs or enable Shared playback.

The native checkpoint passed forty affected tests each on iOS, tvOS and Android,
plus Android lint. The integrated receiver tree's 399 client files are byte-equal
to the tested native archive. The web checkpoint passed twenty focused tests,
the complete web gate and four documentation-index tests; the current combined
receiver/client tree passed that web gate and index suite again. The known
TypeScript diagnostic baseline is unchanged. Both agent checkpoints used the
normal hook and exact source-only archives for their compiler evidence.

The exact committed receiver alias checkpoint `f5d4fecef` separately passed the
actual disposable CGNAT HTTP/1 and HTTP/2 alias-consuming decision fixture
(one test, zero ignored, 2.10s) on pinned Rust 1.97.1. A daemon-only compiler
wrapper reduced debug bookkeeping and serialized the backend after measured
memory-limit failures; the successful fresh compilation recorded zero OOM
kills. This proves the pinned Source/B HTTP path and advertised alias, not an
actual Tailscale, DERP, hardware-decoder or live playback scenario. Later
combined candidates still require their own affected qualification before push.

### Current playback-base startup integration qualification

The actual Source startup factory checkpoint `ec2f82a1d` was integrated on
`1ba0814b9`, which includes complete Source response construction, canonical
zero preparation, strict Start decoding and B control projection. Pinned Rust
1.97.1 passed Source schema tests (9, zero ignored, 109.76 seconds), Source
binding regressions (22, zero ignored, 31.11 seconds), purpose regressions
(14, zero ignored, 15.24 seconds) and the actual Source three-voter contract
(1, zero ignored, 11.70 seconds). Feature all-target Clippy with denied
warnings passed in 1 minute 36 seconds, and all four docs-index tests passed.
This qualifies the integrated startup/storage tree; live Source/B actor,
transport/body relay, hardware and real Tailscale acceptance remain open.

### S5 receiver Source attachment, publication and attached renewal

The dedicated receiver Store now attaches an exact Source result to an existing
blocked B owner, publishes its canonical projected response and resolves the
original starting request in one transaction, and renews an attached blocked
or published owner. These are B metadata writers. The caller must verify the
actual Source published response and seal/open its upstream capability with the
selected B key and identity/import AAD; none of these methods admits Source
physical work or infers Source authority from a persisted tuple.

Every write repeats current original-login, idle policy, saved switch, import
lifecycle, Source server/epoch and effective library authorization together with
the exact B session/incarnation, owner node/epoch, pointer, request fingerprint,
recipe and matching live job lease. The Source binding retains the complete
reference, file/revision, request/session/incarnation UUIDs and exact sealed
envelope. Envelope size is checked before parsing (four KiB), including its
nonce/tag structural minimum; projected responses are canonical JSON objects
bounded to sixty-four KiB. Partial or different bindings refuse without repair,
replacement or rebinding. Exact replay performs no writes. Expired owners are
not resurrected and leases are never shortened. Attached renewal updates the
starting claim only while blocked; an already resolved request stays unchanged.
The original null-only pending renewal remains separate.

The existing receiver activation, adjunct and pending-renewal assertions also
refuse before an INSERT trigger can suppress their failure. Their refused
branch evaluates a fixed SQL expression error before the trigger runs, while
the allowed branch emits no assertion row; the older named NOT-NULL error
remains recognized. Both ordinary backend publication methods now exclude a
remote recipe before returning a ready/resolved reply, requiring the dedicated
current-login publication path. Other Source and general lifecycle assertions
are outside this change. Ownership is confined to the receiver DTO/Store and
its existing tests, plus the two publication methods in
`store/sqlite/sessions.rs` and `store/hiqlite_sessions.rs`.

On the complete intended-base integration with `726ed039e`, pinned Rust 1.97.1
affected feature-enabled all-target compilation passed in 1 minute 10 seconds
and Clippy with denied warnings passed in 1 minute 37 seconds. The
`--lib sharing_receiver` filter passed three tests with zero ignored
(19.72 seconds), including both SQLite storage modes and both principal
layouts, stale/current-scope races, original-login loss, malformed/oversized
envelopes, partial binding refusal, exact replay and ignored assertion/update
rollback. The extended actual three-voter receiver contract passed in
9.53 seconds; the existing Local activation/publication contract through
`dyn Store` passed in 11.53 seconds. These Store fixtures do not qualify the
HTTP actor, a live Source/B relay, delivery, progress ordering or two-NAT work.

```sh
cargo test --locked -p plurx-core --features hiqlite-contract-tests --lib sharing_receiver -- --nocapture
cargo test --locked -p plurx-core --features hiqlite-contract-tests --test store_contract sharing_receiver_three_voters_atomic_admission_replay_scope_and_unresolved_retention -- --nocapture
cargo test --locked -p plurx-core --features hiqlite-contract-tests --test store_contract media_session_activation_prepare_settle_contract_runs_through_dyn_store -- --nocapture
cargo check --locked -p plurx-core -p plurxd --all-targets --features plurx-core/hiqlite-contract-tests
cargo clippy --locked -p plurx-core -p plurxd --all-targets --features plurx-core/hiqlite-contract-tests -- -D warnings
```

### B Source Start dispatch — retained owner before sending

`SharingManager::start_file_source` accepts the complete retained receiver
intent and its existing blocked owner. Before peer lookup it compares the
canonical private wrapper against the original complete playback request,
allowing only replacement of `request_id` with the retained Source request
UUID. The original B login, import, Source identity, library assignment and
lifecycle must match the retained intent. The ordinary client request remains
unchanged in the durable remote recipe.

After the pinned peer handshake, the caller repeats current import authority,
prepares fresh original-login authority, and atomically renews the existing
blocked owner, pending claim and matching lease before the first Source send.
The pseudonymous viewer comes from B's current assignment Store. The exchange
uses the qualified bounded pinned Source Start transport without a retry.
Once a complete Source result arrives, this method returns it to the owning
task even if B's scope changed during the exchange: guarded B publication must
repeat authority, while the known Source result remains a cleanup obligation.
An error or dropped waiter proves no Source rollback.

The method is a candidate owner seam; no viewer Start route, relay, control,
delivery grant or restart cleanup is registered by this change. The focused
recipe test checks retained nested playback selections, Source ID zero and an
item ID above JavaScript's exact integer range, exact private request identity,
unchanged original client request and refusal of a different parent login.
It does not prove a real Source/B playback exchange.

```sh
cargo test -p plurxd --bin plurxd sharing_source_dispatch_retains_complete_receiver_recipe_and_private_request
```

### Candidate B owned Start task — no HTTP registration yet

`shared_receiver_playback.rs` adds a bounded registry on the actual
`SharingManager`. It captures the original B login and complete request before
spawning a detached owner. An exact client retry joins the retained owner and
keeps its original Source request UUID; a changed recipe, login, player or
client key refuses. A failed or timed-out attempt remains an obligation and
continues occupying its registry entry.

The owner uses the actual Local principal to claim and assign B's request,
then commits the guarded blocked remote session before sending Source Start.
While waiting, it renews the pending request, owner and lease through fresh
original-login authority. Immediately after receiving Source's complete
result, it retains the actual authenticated Source credential, pseudonymous
viewer, Source incarnation and response before any B Store await. The upstream
capsule is sealed under the selected B master with the Upstream purpose and
B-server/import AAD; its exact ciphertext is retained through attachment,
publication and renewal. The public response projects session, playlist and
control identity to the actual B owner.

This finite owner accepts only the implemented full-film VOD lane with
Source origin zero. Resume remains in the original engine request. It does
not reinterpret an encoded origin or force an unsupported Source lane into
copy. After guarded attachment and publication it renews the attached B
metadata independently of Local worker renewal. A received Source result or
an uncertain send is never released because a waiter times out or B loses
its original login.

The candidate is deliberately unregistered pending real Source HTTP/B Start
qualification, fresh delivery/body grants, Source status/end settlement,
relay, control and progress. Its registry tests qualify exact retry identity,
complete request conflict, captured login, retained Source request identity,
capacity and waiter-timeout retention; they are not physical Source/B proof.

```sh
cargo test -p plurxd --bin plurxd sharing_receiver_registry
cargo test -p plurxd --bin plurxd source_copy_
```

### Combined startup stack regression

The combined real Source actor fixture initially overflowed the default test
thread stack during one-voter schema startup, before Source admission. A batch
debugger identified `HiqliteAuthStore::migrate_schema` reserving 1,635,152 bytes
in its polling frame. Raising the test stack would have hidden this startup
failure. Migration transactions now collect their statements into one fixed
vector representation and box the transaction future through a synchronous
helper. SQL, parameter order, validation, deadlines and commit-unknown handling
remain in the existing transaction and migration settlement paths.

The learner/voter regression now uses the ordinary baseline version 70, while
the Source-only installer retains version 71. Before the voter arm, the fixture
removes migration-70 tables and restores version 69, so it exercises a real
migration rather than only reopening an already-current schema. This regression
passed on pinned Rust 1.97.1 with the default stack: one test, zero ignored,
9.05 seconds. The actual Source response Body guard also checks its retained
physical file fence when observing cancellation; a path replacement cannot
remain authorized merely because the database still reports the old revision.

```sh
cargo test -p plurx-core --features hiqlite-store,hiqlite-contract-tests --lib a_learner_refuses_a_behind_schema_that_a_voter_migrates
```

On this combined tree, the six Source copy/admission/actor tests passed with
zero ignored in 26.33 seconds, including physical file drift cancellation
while a returned Body still holds settlement capacity. The two retained B
registry tests and complete Source-dispatch recipe test also passed with zero
ignored. Documentation index checks passed all four tests. These are focused
integration receipts; the unregistered B candidate still requires the live
HTTP, relay, delivery and cleanup qualification listed above.

### Source HTTP Start on the combined receiver tree

The private file-scoped Source Start route now owns an eight-entry registry on
the actual transcode manager. It retains the full canonical ordinary request,
stable grant, pseudonymous viewer and complete Source reference before
preparation, claim, assignment or physical admission. A disconnected HTTP
waiter leaves the detached owner intact. Exact retries join that owner through
fresh grant/file checks and the actual published actor; stored response JSON
or a missing process-local actor cannot establish readiness.

The reusable real fixture selects a one-voter daemon Store, saves the explicit
sharing choice and runs the actual pre-serving Source schema factory. It uses
real scanned file facts, FFmpeg, fragment index and catalogue revision key.
On the combined startup-fixed tree, all seven Source HTTP tests passed with
zero ignored in 12.24 seconds. The actual TCP-disconnect regression verifies
exact replay, changed-recipe refusal, stable-grant credential rotation,
restart-style registry absence refusal, held response Body retirement and
revocation followed by actual producer/body/SQL settlement.

This checkpoint does not cover bytes already queued by Hyper after Body EOF.
The accepted-connection writer completion barrier remains an explicit next
qualification, along with Source resource/status/End routes, B live Start,
relay, delivery grants and control. No viewer playback is enabled here.

```sh
cargo test -p plurxd --bin plurxd http::shared_source_playback::
```

### Combined retained binding, progress and dispatch qualification

The binding reader, Local-only lease inventory and ordered B progress
checkpoints are integrated with the actual Source HTTP/actor tree. On pinned
Rust 1.97.1, the receiver suite passed three tests with zero ignored in 20.33
seconds; it includes the two-owner progress preimage race across both SQLite
modes and both principal layouts. The actual three-voter receiver contract
passed one test in 9.79 seconds, and the affected actual three-voter
Local/Source/B inventory regression passed one test in 17.93 seconds. The
SQLite inventory regression passed one test in 0.69 seconds. Both B registry
tests, the complete dispatch recipe test and all seven Source HTTP tests also
passed with zero ignored; the HTTP suite took 12.37 seconds.

The candidate B owner now retains the exact sent credential and viewer before
the first Source send, so a lost response retains cleanup authentication.
Pending renewal failure stops renewal but continues awaiting the owned Source
exchange instead of dropping its eventual physical handle. Fresh B authority
still gates attachment and publication. Attached renewal updates the retained
owner's exact lease for subsequent guarded reads. These remain unregistered
candidate seams awaiting real B transport, delivery and cleanup qualification.

```sh
cargo test -p plurx-core --features hiqlite-store,hiqlite-contract-tests --lib sharing_receiver
cargo test -p plurx-core --features hiqlite-store,hiqlite-contract-tests --test store_contract sharing_receiver_three_voters_atomic_admission_replay_scope_and_unresolved_retention
cargo test -p plurx-core --features hiqlite-store,hiqlite-contract-tests --test store_contract sharing_principal_runtime::sharing_rebuilt_local_request_writes_preserve_owner_and_refuse_cross_principal_replay -- --exact
cargo test -p plurx-core --features hiqlite-store,hiqlite-contract-tests --lib sharing_staged_and_worker_inventory_preserve_distinct_owners
```
### S5 retained receiver binding reader and Local worker separation

`receiver_source_binding` reads the original retained sealed upstream envelope
and optional resolved projected response through the current original-login,
import/library, exact B owner/session/request/pointer and live lease proof. It
uses one bounded consistent query, limits UUIDs to their canonical 36-byte
form, envelopes to four KiB and canonical response objects to sixty-four KiB
before returning private data. An unbound blocked owner returns no binding;
partial, malformed or oversized binding material refuses without repair. The
reader neither opens the upstream capability nor establishes Source physical
authority. The actor must authenticate the retained envelope with its exact
Upstream AAD and obtain fresh Source evidence before using it.

Both backend `owned_media_sessions` implementations now enumerate only genuine
Local sessions for the generic Local lease loop. Shared Source principals and
B `remote_source` recipes belong to their dedicated actors and cannot enter
Local renewal or stale-recipe settlement. Staged, expired and cleanup ownership
inventories remain separate; this exclusion is not proof that no actor owns a
resource. Regression fixtures retain genuine Local visibility, exclude both
actor classes, and keep Shared expired ownership visible.

Pinned Rust 1.97.1 receiver reader regressions passed three tests with zero
ignored (19.19 seconds) across both SQLite storage modes and both principal
layouts. The reader recovers the retained envelope/reply and refuses changed
assignment, owner and file tuples or malformed/oversized binding material.
The actual three-voter receiver contract passed (9.56 seconds). The SQLite
actor inventory regression passed in both memory and pooled storage
(0.71 seconds), and the actual three-voter Local/Source/B inventory regression
passed for both principal layouts (17.94 seconds). Affected Core/daemon
feature-enabled all-target compilation passed in 1 minute 9 seconds; the
docs index passed four tests and the catalog passed 2702 audited files.

```sh
cargo test --locked -p plurx-core --features hiqlite-contract-tests --lib sharing_receiver -- --nocapture
cargo test --locked -p plurx-core --features hiqlite-contract-tests --lib sharing_staged_and_worker_inventory_preserve_distinct_owners -- --nocapture
cargo test --locked -p plurx-core --features hiqlite-contract-tests --test store_contract sharing_receiver_three_voters_atomic_admission_replay_scope_and_unresolved_retention -- --nocapture
cargo test --locked -p plurx-core --features hiqlite-contract-tests --test store_contract sharing_rebuilt_local_request_writes_preserve_owner_and_refuse_cross_principal_replay -- --nocapture
```

The existing Local activation/publication contract through `dyn Store` also
passed (11.52 seconds), and affected feature-enabled Clippy with denied
warnings passed in 1 minute 28 seconds. The final reader test additionally
revokes the original login while another valid login remains: that alternate
login cannot authorize recovery of the retained binding.

Final original-login refusal coverage passed in the same three-test reader
filter (19.19 seconds), followed by exact-tree affected compilation and Clippy.

### S5 session-bound ordered receiver progress

`SharingReceiverProgressStore::save_receiver_progress` accepts server-only
progress with an exact retained attachment and opaque fresh receiver authority.
The body supplies position, optional duration, watched state and sequence;
identity comes from the current B session and the Store captures the timestamp.
The transaction repeats original-login/idle-policy, effective import/library,
Source identity/epoch, exact published B owner/session/request/pointer/lease,
recipe/file/revision and retained Source session/incarnation/envelope checks.
Blocked, expired, replaced or revoked owners cannot write history. This is B
metadata authority; the caller still owns fresh Source actor evidence.

The existing history key remains `(source_server_id, catalogue_epoch,
remote_item_id, user_id)`. It spans B sessions and import recreation for the
same durable Source identity; simultaneous imports of the same Source/epoch
are already prohibited. A higher sequence advances the row. An exact duplicate
returns Replay without rewriting its timestamp; older sequences return Stale,
and equal-sequence differences or changed-library history return Conflict.
Neither outcome mutates history. A new session cannot reset the global sequence.
Library moves still require explicit reconciliation, and no Local item ID,
`watch_state`, `watched_outbox` or A household history is written.

The leader reads a bounded watch preimage, rechecks captured proof/owner
freshness after that read, and asserts the exact preimage in the mutation
transaction. A concurrent history change therefore refuses for fresh retry,
without overwriting newer data. The transaction then proves the exact accepted
postimage and current authority before committing. Replay, stale and conflict
transactions issue no history INSERT or UPDATE. Refusal evaluates before an
INSERT trigger; ignored history writes and in-write scope revocation roll back.
Replicated SQL uses no connection-local `changes()` or database clock. Position,
duration and sequence use the existing nonnegative JavaScript-safe integer
bound, while private stored library identifiers are capped before projection.

Ownership is confined to the two receiver-progress Core modules, direct
trait/module/catalog registrations, receiver guard helper visibility and the
existing SQLite/replicated receiver fixtures. The older assignment-bound
`save_remote_watch` remains a historical metadata fixture helper: all current
daemon callers are test-only. The live HTTP progress route remains closed until
the parent receiver actor integrates this session-bound writer and qualifies
actual Source evidence and control delivery.

Pinned Rust 1.97.1 final receiver regressions passed three tests with zero
ignored (20.18 seconds), including both SQLite storage modes and principal
layouts, invalid numeric bounds, stale/expired owners, complete original-login
and attachment refusal, read-only duplicate timestamp preservation, sequence
ordering across two published B sessions, ignored history writes, and scope
revocation inside the write with rollback of both scope and history. The actual
three-voter contract passed with zero ignored (9.80 seconds), including the
replicated ignored-write and in-write revocation cases. A supplemental
interleaving regression passed in all four SQLite combinations (2.65 seconds):
a second published B owner advances history after the first owner's snapshot;
the first write refuses and preserves the newer sequence/position. Affected
feature-enabled compilation then passed in 1 minute 13 seconds; docs index
passed four tests and the catalog passed 2704 audited files.

```sh
cargo test --locked -p plurx-core --features hiqlite-contract-tests --lib sharing_receiver -- --nocapture
cargo test --locked -p plurx-core --features hiqlite-contract-tests --lib sharing_receiver_source_binding_publication_and_renewal_are_guarded_and_exact -- --nocapture
cargo test --locked -p plurx-core --features hiqlite-contract-tests --test store_contract sharing_receiver_three_voters_atomic_admission_replay_scope_and_unresolved_retention -- --nocapture
cargo check --locked -p plurx-core -p plurxd --all-targets --features plurx-core/hiqlite-contract-tests
cargo clippy --locked -p plurx-core -p plurxd --all-targets --features plurx-core/hiqlite-contract-tests -- -D warnings
```

Final affected feature-enabled Clippy with denied warnings passed in
1 minute 30 seconds. These regressions qualify the B Store ordering and
metadata boundary; they do not qualify a live HTTP progress/control route,
Source physical work, two-NAT delivery or hardware playback.

### S5 retained receiver cleanup transport

The receiver retains the complete original private Source session request,
the exact pinned endpoint selected for Start, and the credential and viewer
hash used at dispatch. It records them before the first Source send. Removing
an import or losing the original B login therefore does not erase the means
to clean up an already dispatched Source request. The selected endpoint is
also included in the bounded sealed upstream capsule.

The cleanup connection exposes only the fixed authenticated file-scoped End
exchange. It validates the retained private endpoint and SPKI without an
identity-read preflight, since sharing-off can disable identity reads while
an existing request still needs retirement. It exposes no catalogue, Start,
or resource operation. The complete original session recipe and all known
Source incarnation/session/control identifiers accompany End; a lost Start
response retains the original request identity instead of inventing a session.

A bounded duplicate-aware closed reply must echo the full Source file
reference and request identity. A known Source tuple must match exactly;
unknown lineage may contain the actual assigned tuple or omit both session
and control epoch. Only a settled reply with a canonical nonnil v4 confirmation
is accepted. The receiver retains that authenticated receipt before any later
Store await. The receipt records RPC facts: it cannot alone create retirement
authority. The independently owned B retirement task must also join its actual
accepted bodies, readers and writers before using the Core retirement witness.
The receiver playback ingress and that ownership integration remain closed
while their end-to-end regressions are outstanding.

The combined cold-index and cleanup candidate passed pinned Rust 1.97.1
Core/daemon all-target compilation, thirteen actual Source copy/index tests
(40.14 seconds), the Core guarded index-evidence test (1.07 seconds), and
seven actual Source HTTP tests (12.19 seconds), with zero ignored tests.
The four documentation index tests and validation catalog also passed. The
cleanup wire regressions exercise full recipe preservation, exact known
lineage, lost-Start settlement, duplicates, oversized replies, unsettled
states and noncanonical identifiers. These are wire validation regressions;
actual authenticated End transport and B physical retirement remain open.

### S5 accepted writer completion and owned receiver Start join

The accepted-connection closure observer completes after the actual Hyper
connection and socket writer are dropped. A Source response retains its
producer guard through that observer; body EOF and cancellation are not
settlement. The actual Source HTTP regression blocks the incarnation JSON
DATA at the accepted HTTP/1 and HTTP/2 writer and requires connection/DATA
closure before the actor can report settled. Incomplete resource streams and
independent blocking read jobs still require their own ownership tests.

The receiver registry retains the exact Start task handle. Cleanup seals
dispatch before joining that task; a private joined token binds the result
to the same registry entry and is required before its Source End exchange.
A delayed send cannot follow cleanup's closed gate. An already dispatched
credential/endpoint obligation survives closure, and an absent or failed
task cannot synthesize a joined token. Pending renewal stops when cleanup
requests closure, while an already sent Start remains awaited so its eventual
Source lineage is retained. The planned activation is stored before its SQL
await, and the actual B owner/lease is retained immediately after activation
and successful renewal. Activation commit-unknown still requires exact route
reconciliation; registry absence or a planned epoch does not settle it.

The exact combined tree passed pinned Rust 1.97.1 feature-enabled all-target
Core/daemon compilation (53.42 seconds), four receiver ownership/retry tests,
and eight actual Source HTTP tests (24.04 seconds), all with zero ignored.
The receiver ownership fixture proves the real spawned task remains joined
behind its release signal; it does not model a physical Source producer.
The production B ingress, accepted B body/read/writer joins and private Core
retirement witness integration remain outstanding.
### B confirmed retirement metadata (S4 follow-up)

The receiver retirement Store takes a `ReceiverRetirementWitness` implemented
by a private daemon factory. That factory must retain an actual settled Source
End lineage receipt, or an owned never-dispatched compare-and-set, and join all
accepted B bodies and tasks before presenting the witness. Core fixtures
implement metadata witnesses only; they do not qualify physical termination.
Expiry, terminal JSON, missing pointers and missing leases cannot manufacture
this witness. Daemon factory and HTTP wiring remain separate outstanding work.

The cleanup transaction repeats the original RemoteSource recipe and login
hash, B request/session/incarnation/node/epoch, captured lease and retained full
Source attachment. An all-NULL pending attachment accepts either confirmed
Source request retirement or an owned no-send disposition; a partial attachment
refuses. Current login enablement and import assignment are deliberately not
required for this exact cleanup, so logout, import revocation and the existing
user-delete trigger cannot strand confirmed obligations. Foreign pins, lease
owners and pointers refuse; a displaced successor is preserved.

A bounded canonical terminal receipt replaces only the ended RemoteSource
route response. It binds a purpose-separated context digest, the stable actual
confirmation identity, disposition and reason. It is terminal metadata, never
Source proof or a live Start/delivery response. The original resolved Start
request reply and terminal acknowledgement remain intact; a starting request
becomes failed, while existing trigger-produced NULL replies stay NULL. Matching
lease, pins, old pointer and upstream adjunct are removed atomically. Exact
receipt retry uses assertions only and does not mutate timestamps; a different
context or confirmation refuses. Commit-unknown is returned as an error and the
actor must retain its receipt for retry. Local route responses are unaffected.

After confirmed Source/body/task settlement the daemon may refresh terminal B
route metadata, checking the same original incarnation, session, node, epoch
and recipe. This refresh is metadata only, never settlement evidence. A live
route requires lease deadline equality; an already-ended route permits only a
matching-owner/fence job deadline at or before the captured terminal route
deadline. The actual maintenance sweep advances the terminal route timestamp
while retaining an older expired job deadline, and user deletion can set both
to zero. Future lease extensions and foreign owners remain refusal. The first
existing terminal cause is preserved.

The focused Core matrix uses actual guarded same-playback successor activation
before retiring the old owner, and compares every other route/request/lease and
pointer before and after cleanup. It also retains an acknowledgement written by
the real terminal-ack API, then checks that Source-confirmed metadata retirement
and retry preserve it. A later user-delete trigger may clear the original Start
reply; that current reply is repeated as a transaction preimage and is never
restored. The receipt identity binds immutable lineage rather than this mutable
trigger result.

Qualification uses pinned Rust 1.97.1 on the exact `06c3f4c60` intended ancestor:

- `cargo test -p plurx-core --features hiqlite-store,hiqlite-contract-tests --lib sharing_receiver -- --nocapture`:
  three tests, zero ignored; pending and attached retirement matrices run in
  memory/pooled SQLite with both retained and rebuilt principal layouts.
- `cargo test -p plurx-core --features hiqlite-store,hiqlite-contract-tests --test store_contract sharing_receiver_three_voters_atomic_admission_replay_scope_and_unresolved_retention -- --nocapture`:
  actual three-voter metadata matrix. Both features are required for test
  registration; a zero-test filter is not qualification.
- Core/daemon feature compilation, all-target Core feature Clippy with denied
  warnings, documentation index and catalog lint precede the normal tracked
  commit hook. These checks do not qualify the private physical witness
  factory, HTTP retirement wiring, production deployment or a two-NAT fleet.

Final native receipt: Core receiver filter **3 PASS, 0 ignored, 22.66s**;
actual three-voter receiver filter **1 PASS, 0 ignored, 9.85s**. Core/daemon
feature check passed in 48.45s and all-target Core feature Clippy in 33.83s.
Documentation index passed four tests; catalog lint covered 2708 files. The
same-playback successor, real terminal acknowledgement, trigger-cleared reply,
partial pending attachment, foreign pin epoch and ignored cleanup-write cases
are inside the named Core receiver regressions above.
### S5 retained receiver cleanup transport

### S5 private receiver retirement owner and resource admission boundary

The receiver now has a private detached retirement owner. It closes dispatch
and resource admissions, joins the exact owned Start task, waits for the last
registered resource/job guard, and then obtains the authenticated Source End
confirmation. A never-dispatched outcome requires the same closed gate and
joins. Neither a timer, a stored response nor registry absence creates either
outcome. The confirmation is purpose-hashed with the full Source reference,
private request and actual Source lineage before the private Core witness is
constructed. Known-source confirmations and sealed bindings remain retained
across Store errors and uncertain commits.

Cleanup captures the actual B route only after these joins and checks its
original session/incarnation, principal, playback, owner node/epoch, recovery
epoch, complete recipe, fingerprint and source timeline. Terminal lease
metadata can refresh only after Refused and through the same immutable checks.
The exact retained binding is tried first. Only Refused permits the separate
all-NULL pending transaction; an error never means no attachment. Successful
retirement is the only path marking an activated entry retired. Later registry
registration evicts such completed entries into a bounded non-authorizing
retry tombstone list; unresolved entries continue to consume the eight slots.

The resource registry allows at most 32 active opens per receiver actor.
Admissions close before cleanup waits. Each accepted response, independent
read and upstream client job must retain its actual guard through completion;
body EOF cannot release a guard still held by a job or writer. The ownership
test uses a real spawned job held behind a release signal and proves cleanup
waits after the response's guard is dropped. This is an ownership regression,
not an HTTP or physical Source fixture.

Pinned Rust 1.97.1 Core/daemon all-target compilation passed in 75 seconds
and affected feature-enabled Clippy passed in 95 seconds. Two resource
ownership tests and the six-test receiver ownership group passed with zero
ignored. The combined Core receiver group passed three tests (21.33 seconds)
and the actual three-voter contract passed one test (9.88 seconds), zero
ignored. Public receiver ingress remains unregistered. Actual accepted B
response/read/client-job guard wiring, authenticated pinned End exchange,
request-only no-activation cleanup and automatic failure-triggered retirement
must be qualified before it opens. Core metadata and the ownership fixtures
alone do not qualify physical end-to-end playback.
### B session delivery grant Store checkpoint (metadata qualified)

The receiver delivery Store issues only hashed verifiers for an exact published
B owner and retained Source attachment. Its same-write predicate repeats the
original login and policy, current import and effective assignment, full recipe,
Source reference and revision, resolved original request, current playback
pointer, node and owner epoch, and matching live job/session lease. The grant
expires within 30 seconds of the actual Store clock and no later than that lease
or the original login deadline. Exact issuance retries preserve the deadline;
revoked verifiers cannot be reinstated or rebound. The existing exact attached
owner renewal extends active grants for that incarnation and original login in
the same transaction, capped by the new lease and login deadline. It leaves
revoked grants unchanged and asserts every active grant reached the exact cap;
a suppressed grant update rolls back the whole lease/request renewal. This
keeps the existing static B session capability and adds no player token or
renewal protocol. No raw bearer enters Core.

The bounded reader and individual revocation require the same fresh opaque
receiver authority and exact attachment. The reader returns grant metadata only;
it does not prove Source authority, opened bytes, or accepted-body ownership.
Confirmed retirement revokes all active grants for the exact retired incarnation
in the same guarded transaction and asserts that none remains active. Successor
and foreign-incarnation grants are outside that mutation. Retirement replay
remains read-only. Cleanup after login loss uses the private retirement witness,
not a weaker grant revocation caller.

This schema-free candidate uses the existing `sharing_delivery_grants` table.
It does not enable HTTP delivery or change Local `file_grants`. Production B
relay must independently retain fresh actual Source evidence and accepted B
connection/read/writer ownership. The exact candidate on the full `54792d26f` ancestor, merged with the retained
retirement checkpoint, passes the following pinned Rust 1.97.1 commands:

```sh
cargo test -p plurx-core --features hiqlite-store,hiqlite-contract-tests --lib sharing_receiver -- --nocapture
cargo test -p plurx-core --features hiqlite-store,hiqlite-contract-tests --test store_contract sharing_receiver_three_voters_atomic_admission_replay_scope_and_unresolved_retention -- --nocapture
cargo check -p plurx-core -p plurxd --features plurx-core/hiqlite-store,plurx-core/hiqlite-contract-tests
cargo clippy -p plurx-core -p plurxd --features plurx-core/hiqlite-store,plurx-core/hiqlite-contract-tests --all-targets -- -D warnings
```

The first command reports 3 passed, zero ignored in 21.88 seconds, including
actual memory/pooled receivers in both legacy and rebuilt layouts. The registered
three-voter regression reports 1 passed, zero ignored in 10.11 seconds. It covers
canonical verifier and deadline refusal, exact issuer/revocation retries,
foreign binding and current-login/assignment/owner/pointer/publication loss,
suppressed grant INSERT/UPDATE assertions, owner lease renewal coupled to active
grant extension, revoked-verifier preservation, and whole renewal rollback when
grant deadline UPDATE is ignored. Retirement includes an expired retained grant
fixture and refuses a suppressed revocation before any metadata settlement.
These are real B storage fixtures; their retirement witnesses remain explicitly
metadata fixtures, without synthetic Source physical evidence.

The affected feature check passes in 50.18 seconds and feature Clippy in
1 minute 22 seconds. Documentation index checks pass 4/4; the catalog audits
2,711 files. The normal tracked commit hook remains mandatory. Live HTTP ingress,
Source transport evidence, and accepted-body/read/writer ownership are separate
qualification boundaries.

### Candidate authenticated B Start ingress

The isolated `shared_receiver_ingress` module supplies two aliases beneath the
actual opaque B file base: `hls/sessions` and `playback`. Its router is deliberately
unregistered. Public installation still needs the actual pinned B-to-Source
Start/relay/control fixture and accepted Start-response ownership through the
connection, read jobs and writers. It adds no Play button or Local lookup.

Both aliases require the actual B account token and authenticated user, current
active import, signed file locator and a fresh opaque original-login receiver
Store authority after the bounded body read. The signature binds the retained
Source/epoch/library/item/file/revision and import lifecycle. Decimal Source IDs,
including zero and values beyond JavaScript's safe integer range, remain strings.
The complete provided ordinary CreateSession body is retained: omitted fields,
explicit nulls and integer resume positions are not normalized into guessed
intent. Duplicate keys, unknown nested fields, trailing JSON and oversized input
are refused. A new private Source request UUID changes only that wrapper field;
the retained B request and its fingerprint stay stable across retries.

Initial play/resume preserves `start`, initial quality and device capabilities.
Non-null `previous_session_id`, `control_sequence`, `reopen_reason` and `intent`
are refused before actor admission while Source-safe predecessor/control/intent
planning remains unqualified. Null or omitted values are preserved. Unknown
staged-recovery fields are rejected by the closed DTO grammar. B session UUIDs
are never forwarded as Source predecessors or passed to the Local manager.

The existing Root receiver actor owns claim, assignment, activation, Source
Start and cleanup; this handler starts no second actor. Its 305-second ready
wait is independent of HTTP waiter cancellation, and unresolved/error replies
are not producer or retirement evidence. The candidate JSON response still
awaits the Root accepted-body registry before public enablement.

The focused pinned Rust 1.97.1 ingress command is:

```sh
cargo test -p plurxd --features plurx-core/hiqlite-store,plurx-core/hiqlite-contract-tests --bin plurxd sharing_receiver_ingress -- --nocapture
```

It reports 4 passed, zero ignored in 0.19 seconds on the retained `ebd390b20`
base. The tests cover complete provided DTO preservation, lossless signed-context
construction and stable retry fingerprint, refusal of each non-null recovery or
intent field without rewriting the body, closed malformed/duplicate/oversized
inputs, and actual B account-authentication on both aliases. The ordinary public
router returns 404 for both candidate aliases. These fixtures neither create a
Source producer nor claim physical Start/retirement qualification. Documentation
index checks pass 4/4 and the catalog audits 2,712 files.

Affected feature Clippy passes with `cargo clippy -p plurxd --features
plurx-core/hiqlite-store,plurx-core/hiqlite-contract-tests --all-targets -- -D
warnings` in 1 minute 35 seconds. The normal tracked hook also runs before the
finite checkpoint. Moving the intended base requires the exact combined tree
qualification again.

### Exact pending request cleanup before B route creation

`ReceiverPendingRetirementWitness` is an immutable interface implemented by the
private daemon factory after the owned no-send CAS and actual Start/body/job
joins. It exposes the retained full receiver intent, original request/playback
IDs, exact captured request owner (`Unassigned` or `Assigned(node)`), and bounded
canonical confirmation identity. There is no concrete production constructor,
wire encoding, Debug output or boolean physical assertion in Core.

`retire_pending_receiver_request` repeats the exact original positive B user,
request fingerprint (including original login and whole recipe), private
incarnation, playback ID, owner node and null reply in one transaction. NULL
owner is exact, never a wildcard. Every route, current pointer, session job
lease, upstream attachment, media pin, delivery grant and terminal acknowledgment
for that incarnation must be absent. Even an expired lease refuses cleanup.
The actual owned starting claim becomes failed with the trusted Store clock;
postconditions verify both state and written deadlines/timestamp. An exact failed
retry is assertion-only and cannot rewrite a reply or timestamp.

This cleanup alone bypasses current-login enablement, so logout, import
revocation and deletion of the original user do not strand a safely joined
no-send owner. A foreign takeover, partial resource, resolved reply or missing
claim refuses; no successor is deleted. Commit-unknown remains an error and the
caller must retain its private witness. A failed row or empty route lookup never
constructs the witness or proves Source termination. Generic Local failure APIs
remain insufficient for this exact ownership settlement.

Pinned Rust 1.97.1 focused qualification on the finite ingress ancestor:

```sh
cargo test -p plurx-core --features hiqlite-store,hiqlite-contract-tests --lib sharing_receiver_pending_retirement -- --nocapture
cargo test -p plurx-core --features hiqlite-store,hiqlite-contract-tests --test store_contract sharing_receiver_pending_retirement_three_voters_refuses_takeover_and_ignored_writes -- --nocapture
cargo clippy -p plurx-core -p plurxd --features plurx-core/hiqlite-store,plurx-core/hiqlite-contract-tests --all-targets -- -D warnings
```

The SQLite regression reports 1 passed, zero ignored in 3.29 seconds, exercising
eight memory/pooled × legacy/rebuilt × unassigned/assigned combinations. It covers
original-login/whole-recipe mismatch, actual claim/assignment, foreign takeover,
ignored assertion and UPDATE rollback, expired resource refusal, real B
import/assignment revocation, logout and original-user deletion, successful
starting settlement and exact read-only failed retry. The layout-specific user
deletion triggers may leave starting metadata or fail it already; the fixture
also uses the actual generic failure writer to qualify the failed-state retry.
That state never supplies physical evidence.

The registered actual three-voter regression reports 1 passed, zero ignored in
9.15 seconds, covering real retained NULL/assigned claims, foreign takeover,
ignored assertion/UPDATE, expired-resource refusal, logout and exact read-only
retry. Both tests explicitly use metadata-only witness implementations; they
cannot qualify the daemon's owned no-send CAS or joined jobs. Feature Clippy
passes in 1 minute 36 seconds. The normal tracked hook remains mandatory, and
moving the intended base requires exact combined ingress/cleanup qualification.

### Shared web initial Start and ordered receiver progress

The existing Shared detail page keeps Play unavailable until a freshly read B
item reports `delivery_status: available`. The prepared caller refetches that
item before launch, retains the full import/Source/epoch/library/item/file
identity and opaque B file base, and resumes only the fresh B-owned watch
position. Source IDs remain canonical strings, including values beyond the
JavaScript integer range. The saved Developer Sharing choice remains unchanged.

Initial playback uses session-first Copy HLS or unburned SDR encoded HLS when
the actual decision and browser capability support it. Unsupported burn, HDR,
DV or absent HLS capability refuses. No direct/progressive fallback opens
pre-session bytes. The initial signed-file Start retains the complete original
caps, request UUID, initial quality and resume, including explicit null fields.
Non-null predecessor, control sequence, reopen reason and initial intent remain
unsupported. The ordinary complete B Start reply is validated against its own
B session playlist/control URLs, incarnation and control epoch before its full
compound context is attached to the Player. This is routing metadata, never
Source physical evidence. Generic Source controls/recovery remain pending.

Progress uses the existing B item endpoint with only B session UUID, ordered
sequence, position, optional duration and watched flag. Sequence state follows
the actual Source/epoch/item/user history key across imports and sessions.
An uncertain network send retains its exact sequence and payload for retry.
A typed stale/conflict 409 discards that old beat, reads fresh authorized B
history, and seeds a later new beat above the current sequence; it never
renumbers and replays old user state. A failed resync blocks new beats until a
fresh read succeeds. Zero position requires the existing current attachment
having reached a timeline. Accepted context survives browsing under the same
login/origin and is rejected after account replacement.

The opaque receiver authority also exposes a finite delivery deadline helper:
checked Store observation plus 30 seconds, capped by captured owner lease and
original login expiry. Overflow or an already expired bound returns no
deadline. This helper does not replace the issuer's same-transaction guards.

Web tests use explicitly synthetic B protocol envelopes, not a physical
end-to-end fixture. Public ingress/relay/control enablement still requires the
Root-owned real pinned Source/B qualification and actual accepted connection,
body and job ownership through cleanup.

Finite caller qualification on the pending-retirement ancestor:

```sh
node --test tests/web/shared-libraries.test.js tests/web/shared-decision.test.js tests/web/file-context.test.js
make web-check
cargo test -p plurx-core --features hiqlite-store,hiqlite-contract-tests --lib receiver_delivery_deadline -- --nocapture
cargo clippy -p plurx-core -p plurxd --features plurx-core/hiqlite-store,plurx-core/hiqlite-contract-tests --all-targets -- -D warnings
```

The focused web run reports 35 passed, zero skipped. The full web lane passes,
including TypeScript without baseline increases and 484 contrast pairs with
zero failures. The pinned deadline unit reports 1 passed, zero ignored;
feature Clippy passes in 1 minute 30 seconds. These are caller/metadata
regressions only. Moving the intended base requires requalification, and the
candidate ingress remains unregistered until the actual physical relay receipt.

### S5 joined pending-request cleanup and Source native intent identity

The receiver owner now records its actual claim stage before each claim or
owner-assignment await. Its Start task waits for its own JoinHandle to be
installed before it can run, so automatic retirement after a Start failure
cannot race the handle it must join. Owner failures and loss of authority elect
one independently owned retirement task; cancelled HTTP waiters still leave
that owned operation alive.

After dispatch is sealed and the actual Start and accepted body/job owners have
joined, an attempt that never claimed a row can release only its inert registry
slot. A claimed, never-dispatched attempt instead constructs the private pending
retirement witness from its complete original intent, client request, playback
identity and retained NULL or attempted node owner. The Store's exact transaction
requires the same genuine starting/failed request and absence of every route,
pointer, session lease, upstream binding, pin, delivery grant and terminal ack.
An uncertain commit keeps the same witness. Only Refused can change the expected
attempted owner to the legitimate original NULL owner; foreign or partially
activated metadata stays retained. A planned activation can use the full route
retirement witness only after the exact pending transaction refuses it.

Initial Shared Start independently refuses non-null previous_session_id,
control_sequence, reopen_reason and intent before receiver registration and
before Source dispatch. Explicit null and omitted fields retain their original
representation. These fields remain parseable for closed cleanup recipes; this
change does not qualify shared recovery or controls.

Source prepared playback now freezes native subtitle enablement and the actual
selected subtitle ordinal into its own SHA-256 intent domain. Its claim,
assignment matching and fingerprint getter consume that same value. Disabled
native presentation normalizes the optional choice away; enabled no-choice,
ordinal zero and another ordinal are distinct. Local durable fingerprints keep
the existing calculation. Focused qualification for this combined candidate
follows before the normal commit.

Pinned combined qualification on `9aa4e8a39` plus pending checkpoint `04b5f45aa`
and the private factories: Core/daemon all-target check passed in 70 seconds,
denied-warning feature Clippy in 95 seconds. The receiver owner group passed
eight tests (including two resource-owner tests), zero ignored, in 0.15 seconds;
after lint-safe test scoping it passed eight again in 0.16 seconds. Candidate
ingress passed four tests in 0.17 seconds; Source native fingerprint identity
passed one test. Actual Source copy/index passed thirteen tests in 40.93 seconds,
encoded media nine in 31.16 seconds and existing Source HTTP eight in 23.93
seconds, all zero ignored. The exact pending request memory/pooled matrix passed
one test in 2.95 seconds, and its actual three-voter contract passed one test in
9.16 seconds with both replicated-store features. Documentation index checks
passed four tests. The tracked merge hook remains required before commit.

The inert preclaim test runs the actual owned task against the real fixture
Store, waits for joined retirement and confirms that no route was created. It
qualifies that early failure case; it does not claim full pinned B-to-Source
playback or a physical Source cleanup result from registry absence.

### S5 exact Source status and HTTP resource integration

The receiver checks the retained Source session through a fixed authenticated
status RPC before its first publication and each attached renewal. The bounded
strict decoder requires the complete VOD Start response and the original
Source incarnation, session and control epoch. Cleanup-only status, changed
lineage and presentation origin cannot authorize publication. Each subsequent
receiver writer still checks the original login and current receiver authority.

On the integrated Source HTTP checkpoint `c8596f4b9` and receiver checkpoint
`1621b4967`, pinned Rust 1.97.1 all-target check passed in 58.29 seconds after
making only the decoded fact accessor crate-visible. Denied-warning feature
Clippy passed in 63 seconds. The strict status wire test passed one test;
actual Source HTTP passed sixteen in 38.26 seconds, receiver ownership eight
in 0.14 seconds, strict Start decoding five and End decoding two, all zero
ignored. These results qualify Source HTTP and receiver status seams. The
receiver streaming relay and full physical B-to-Source playback remain open.
The caller is also qualified on the complete joined-pending receiver ancestor
`1621b4967`. Additive integration retains its independent Unsupported refusal
and private cleanup factory. The exact combined focused web run reports 36
passed, zero skipped; the supplemental case starts two synthetic B sessions
through different assigned imports of the same Source/epoch/item and sends
sequences 5 then 6 from an initial watch sequence 4. The complete web lane and
four documentation index tests pass. The pinned deadline unit reports one
passed, zero ignored, after the combined Core test compile in 1 minute 3
seconds. These results remain protocol/metadata evidence, not native hardware
or physical B-to-Source playback qualification.
The same combined feature all-target Clippy passes in 1 minute 43 seconds.
The actual daemon ingress filter reports four passed, zero ignored in 0.19
seconds, preserving complete request fields, recovery refusal, account-authenticated
aliases and the unregistered public route. The normal merge hook remains
required for the exact resulting tree.

### Native authenticated initial Start adapter

Apple and Android retain the complete ordinary CreateSession request through an
authenticated initial Shared Start, with the original v2 caps, resume, quality
and canonical client request UUID. They post only to the captured signed opaque
B file base. Non-null predecessor, control sequence, reopen reason and initial
intent are refused before network dispatch, as are unsupported burn/HDR/DV
requests. Original Local API guards remain intact.

The complete ordinary B reply binds a new context only after current account,
B session playlist/control paths, finite VOD timeline, incarnation, epoch and
bootstrap cadence validation. Additive reply fields remain available alongside
the ordinary playback projection. No numeric Source ID is converted into a
Local file identity. Playlist query permits only unique bounded native,
canonical subtitle and closed diagnostic values. The web caller accepts this
same ordinary closed query grammar; arbitrary, duplicate and encoded fields
refuse.

The established native baseline uses Xcode 27 / Swift 6.4 and the pinned Android
JDK 25 image. iOS Debug builds pass. The actual iOS simulator Shared client/model
filter reports 11 passed, zero failures, including complete synthetic B Start
through authenticated URLProtocol I/O. The source-only Android baseline reports
40 passed across six Shared/context suites, zero failures/skips, with Kotlin
compilation and lint passing in 4 minutes 30 seconds. The committed Native
initial Start adapter then passes 42 tests with zero failures/errors/skips,
Kotlin main/unit compilation and lint in 4 minutes 52 seconds. These are protocol
fixtures, not physical Source production or hardware playback qualification.
Native player launch and ordered B progress remain the next integration slice.


### Native ordered progress adapter

The native client progress body contains only the admitted B session UUID,
sequence, position, optional duration and watched state. It posts to the
captured account's compound Shared item route. It repeats current account and
complete admitted B context validation, permits only bounded numeric values,
and caps ACK/conflict responses at 16 KiB. It exposes only typed acknowledged
or stale/conflict resync outcomes; Source authority stays server-owned.

An ordered beat journal retains an uncertain beat unchanged. Swift JSON uses
sorted keys so retry bytes stay identical. A typed conflict discards that beat
and blocks the next beat until a fresh authorized watch read seeds the next
sequence. It never renumbers the old payload after another device advances
history. Actual iOS simulator tests pass 12 with zero failures, including
zero-position network body, identical retries, numeric-only conflict sequence,
oversized response refusal and journal exhaustion/resync. The committed progress
primitives then pass Android Kotlin main/unit compilation, 43 tests with zero
failures/errors/skips, and lint in 4 minutes 50 seconds. Player handoff remains
explicit follow-up work. These synthetic protocol fixtures do not prove
physical B-to-Source playback or hardware acceptance.


The native progress pool keys orders by captured account authorization, Source
server, catalogue epoch and item, including across import/library aliases. It
holds at most 256 orders and four nonqueued operations globally, with one busy
operation per watch key. Only idle acknowledged entries can be evicted; uncertain
beats retain their sequence and payload. Account replacement clears the old
namespace while old operations retain their own counted admission until actual
network settlement. A changed B session with an uncertain predecessor beat
requires fresh authorized detail/watch state before a new session beat.

Actual iOS simulator tests pass 12 with zero failures on the bounded-pool source,
including separate authenticated imports/libraries/B sessions for the same
Source item observing sequences 8, 9 and 10 rather than restarting the order.
The corrected pool and tagged plan pass their exact source-only Android run
on `091415fbe`: Kotlin main/unit compilation, 43 tests with zero
failures/errors/skips, and lint pass in 5 minutes 17 seconds.
This bounded metadata journal never grants Source production or retirement.


### Tagged native Shared player plan foundation

The initial Shared subject is a full authenticated compound reference and
opaque B context, title, safe resume position and watch sequence. It cannot
carry a Local item/file ID. Its fixed HLS plan requires the same validated
Shared decision and exact v2 caps as the retained whole CreateSession request.
It refuses unsupported predecessor/intent/recovery/burn/HDR/DV asks and an
inconsistent copy/encode choice. The original raw height, quality mode,
track ask and resume stay intact; eventual Source control comparison must use
that original ask rather than normalized Start output dimensions.

The actual iOS simulator client/model filter passes 12 tests with zero failures,
including retained raw 720-height/12.5-second resume, largest signed-i64 Source
file string, and refusal of Local context/wrong copy/predecessor plans. The
corrected progress pool plus tagged plan pass their exact Android run above.
Explicit native controller/view/browser handoff remains unfinished; Shared
status must use a tagged B-bound common-VOD adapter, never the numeric Local
status decoder. No physical producer or hardware qualification is implied.


The same frozen Native runtime source `091415fbe` passes both actual iOS and tvOS
Debug simulator client/model filters: 12 tests each, zero failures. Both iOS
and tvOS Release simulator builds succeed using Xcode 27 / Swift 6.4 and owned
DerivedData. The prior Android pool snapshot failed compilation on the
fresh-detail auth/transport argument order; the corrected exact snapshot is the
qualified result, not the failed run. The normal tracked hook passes for each
finite source checkpoint. These tests use synthetic authenticated B DTOs and
actual native HTTP clients; physical Source playback, native hardware and the
unfinished controller/browser handoff remain separate qualification boundaries.

Root integration of web/delivery checkpoint `b2da6e0e7` onto Source HTTP/status
checkpoint `850bd67c5` passed pinned all-target check in 79 seconds and denied
feature Clippy in 88 seconds. The three Shared web suites passed 33 tests,
file-context passed 13, receiver ingress four in 0.19 seconds and the replicated
feature delivery deadline unit one, all zero skipped/ignored. The normal tracked
hook and documentation index checks run before the merge commit. This remains
a caller/authority checkpoint; full receiver streaming is still under construction.

### S5 guarded Source resource client and native HTTP integration

The receiver's fixed Source resource RPC retains the complete original private
request and exact Source reference/session/incarnation/epoch. Replies must echo
that whole tuple and the requested closed resource, use single canonical length
and MIME headers, and have no content encoding, transfer encoding or trailers.
Playlists are bounded and validated as relative HLS before publication. Media
streams preserve exact declared length and the original finite deadline, split
coalesced transport frames into at most 64KiB chunks without copying the whole
file, and retain the actual upstream connection driver.

The driver's private lifetime guard survives response EOF and drops only after
the socket future is destroyed, including cancellation. Actual receiver Source
Start and status calls carry the same kind of owned connection barrier. After
B publication, the actor issues the hash of its actual B session UUID under
fresh original-login authority with a finite Store-derived deadline. It retains
the exact lease used by attachment/publication before readers can run. The
candidate resource opener checks the original B login, exact published route,
full retained binding and delivery grant both before Source IO and after it.
It remains unregistered pending accepted B writer and physical relay tests.

Native Source HTTP checkpoint `1d290681d` is integrated with these changes.
Pinned Rust 1.97.1 denied-warning feature Clippy passed on the final tree in
65 seconds. Strict resource headers and bounded framing passed two tests;
the real Hyper/socket lifetime test passed one; receiver ownership passed eight,
all zero ignored. Actual Source HTTP passed nineteen in 45.78 seconds, Native
Copy/encoded actors ten in 33.63 seconds, and the actual three-family preparation
FD-close regression one in 33.95 seconds, all zero ignored. These tests qualify
Source resources and client/ownership seams; they do not qualify full physical
B playback, accepted B writers, controls or live Tailscale/hardware behavior.
Documentation checks and the normal tracked hook remain required before commit.

### Native tagged player draft (2026-10-03)

The existing Apple and Android browser/controller files now dispatch a Shared
subject through the authenticated signed-file Start client. The player retains
its complete raw initial request and compound file context; it never supplies a
numeric Local item/file sentinel or calls Local history, recovery or autoplay.
Fresh detail delivery readiness controls the Play action. The saved Developer
Sharing choice remains authoritative.

Progress retains exact uncertain bytes and distinguishes an acknowledged old
beat from the newly requested final position. Renderer resources close before
best-effort B End. Original-login replacement refuses retained Start, progress,
and End requests. Pending renderer controls are hidden until B can translate a
server-accepted current-rendition control response; quality, tracks and recovery
remain explicitly unavailable.

On integrated Root `9a9109602`, the iOS Debug simulator client/model filter passed
12 tests with no failures. The Android controller draft still needs its exact
source-only compiler, unit-test and lint loop; this checkpoint does not qualify
physical Source playback, B relay, device hardware, or the unfinished controls.

Regression-Test: clients/apple/Tests/SharedDecisionClientTests.swift::testAuthenticatedInitialStartRetainsWholeRequestAndBContext
Regression-Test: clients/android/app/src/test/java/tv/plurx/app/data/SharedDecisionClientTest.kt::initialStartRetainsWholeRequestAndBoundBContext

The Native Shared model also projects the frozen current-rendition selection
from its retained raw Start request. Auto keeps an optional raw height without
inventing a candidate; Manual keeps the original ask rather than delivered
encoder dimensions; Original, raw audio, offset and native subtitle index stay
explicit. Missing codec/range policies remain Auto. This is a pure projection,
not a control dispatch or proof of server acceptance. The existing Local enums
and guards remain unchanged. Current control grammar requires Manual height at
least 144; the regression distinguishes raw 144 from delivered 72.

Receiver transport candidate after the upstream socket checkpoint
----------------------------------------------------------------

The B HLS adapter now dispatches actual retained receiver actors before Local
HLS handlers. An explicit durable `remote_source` recipe whose actual actor is
absent receives an unavailable response; it never creates a Local producer.
GET resources use the closed relative resource parser, original B login and
exact current binding/delivery grant, current Source lineage, and another B
observation after the Source request. Source credentials and private lineage
headers are omitted from the B response. Range requests currently receive the
complete representation. Status, controls, End, remote-worker forwarding and
progress are still separate integration work.

Accepted connections have a weak opaque identity, so multiplexed resource
streams for one actor reuse one counted writer guard. The connection monitor
retains its guard until actual accepted Hyper writer closure, including when
the body finishes with queued bytes. Loss of the original login/binding or
actor retirement closes the actual accepted connection. Its monitor captures
only cancellation tokens and a closure observer, avoiding a monitor-owner
reference cycle. Independent upstream resource jobs retain their own guards.
The actor polls fresh Source/B authority each second, while lease renewal stays
at ten seconds; one owner guard spans those Source requests and remains held
by an actual upstream driver until its socket is dropped.

This is candidate implementation. The full B-to-Source physical fixture and
queued accepted B writer regression remain required; resource parser tests or
an ownership-count test alone cannot supply that evidence.

The candidate authenticated Start response now obtains fresh B delivery and
Source lineage observations after its actor reports ready, then retains the
same accepted writer guard as media resources. It requires the actual
`serve_http` connection extension. An in-memory Router response cannot stand
in for accepted-writer ownership. The preceding relay tree passed pinned
all-target check in 62 seconds, denied feature Clippy in 83 seconds, and nine
receiver ownership tests with zero ignored. The final Start-writer tree passed
denied feature Clippy in 77 seconds; its focused regression and normal tracked
hook remain required before committing this candidate.

Native player qualification update: committed `03b77807a` passed the source-only
Android main/unit Kotlin compilation, 43 focused Shared/context tests (zero
failures, errors or skipped), and lint in 6m54 using the bounded owned compiler
container. On full Root `61820efe8`, iOS Shared client/model/catalogue tests passed
17/17 and the isolated Local/Shared context class passed 7/7. The broader Apple
client class passed 335 tests. One combined run also exposed an inconsistent
synthetic transcode fixture (method changed without delivery mode); that fixture
was corrected before the 17-test pass. The combined context failure did not
reproduce in its complete isolated seven-test class, so the combined run is not
reported as a pass. Exact Android qualification of the additional frozen-control
projection and current base remains required.

The combined Apple failure was traced to the existing pure presenter fixture
creating `AppModel()` and leaving its discovery/bootstrap task running. That
unjoined task could replace the global account during a later authenticated
context read. The model initializer now accepts `startServices` with the normal
application default `true`; only that presenter fixture passes `false`.
Original-account refusal remains intact. The complete combined iOS set then
passed 359 tests with zero failures. The preceding exact tvOS Shared/context
set passed 24 tests with zero failures.

Regression-Test: clients/apple/Tests/AppleClientTests.swift::testTheRowEighteenSitesLogOnceAndDrawNothing

Receiver End and progress candidate after `61820efe8`
---------------------------------------------------

The accepted-writer checkpoint completed thirteen focused receiver/ingress
regressions with zero ignored in 0.42 seconds, documentation checks, and the
normal tracked hook (workspace Clippy 74 seconds, catalog and 75 served scripts).

B End now waits for a private completion receipt. Its only factory consumes
the existing actual joined Start/body proof and physical Source End receipt,
then records the exact Store retirement Applied/Replay. The mutable `retired`
flag and an absent/terminal SQL row cannot acknowledge End. The bounded retry
tombstone may retain this already-created receipt; it retains no actor or
registry Arc and cannot reopen delivery. End is a cleanup operation on the
original opaque B session capability even after original authorization loss.
An unknown or unfinished cleanup returns unavailable after 35 seconds rather
than an acknowledgement. The same connection may close before its End reply;
a retry on another connection can observe the actual completion receipt.

Ordered progress derives Source identity and original login from the actual
retained B actor. It accepts only the closed 1 KiB client beat, validates safe
integer positions and sequence, verifies the exact B session/import/item/user
and original login, observes current Source/B authority, then uses the guarded
production progress writer. Replay preserves the original payload. Stale or
conflicting progress returns 409 with a freshly fenced current sequence; it
does not renumber the client's old position or send history to Source.

B resource opening now runs in a bounded independently owned task. Cancellation
of its HTTP waiter does not abandon a sent Source request or its nested dial
and body jobs. The actual upstream driver retains the same counted custody
through socket closure. Capacity refusal returns 429 without retiring an
otherwise authorized actor. Source filesystem job custody is a separate audit
being repaired and tested before whole-resource qualification.

The pinned all-target compiler passed the End/progress/owned-open candidate in
58.83 seconds. Focused regressions, denied Clippy and the normal hook remain
required, and positive physical B playback, queued writers and End/progress
integration still require the genuine paired-server fixture.

The exact End/progress/owned-open tree passed fifteen focused receiver/ingress
regressions with zero ignored in 0.46 seconds and denied all-target feature
Clippy in 79 seconds. The End refusal regression uses a paused clock and proves
that marking/pruning a metadata-only tombstone cannot create the private
completion receipt. The progress wire regression rejects duplicate/foreign
fields, noncanonical session identity, fractional or unsafe numbers. These are
finite refusal/ownership tests, not positive paired playback evidence. The
existing guarded Store publication/renewal/progress matrix and normal tracked
hook are the remaining pre-commit checks for this checkpoint.

Combined genuine pairing checkpoint on `cdc567b86`
-------------------------------------------------

The full `7ad15ba89` history is integrated, including retained Source start
stages and the genuine invitation helper. The Source fixture uses selected
identity/catalogue/replication fields and an actual Invitation-domain secret
whose hash binds invitation creation and claim. The B fixture uses its own
selected voter and production startup factory, real password login, purpose-
sealed actual import/claim credentials, settlement and viewer assignment.
No Source locator, readiness row or schema is supplied by a manual test write.

This exact combined tree passed pinned all-target check in 54.39 seconds,
Source HTTP twenty-one tests with zero ignored in 54.48 seconds, receiver and
pairing sixteen tests with zero ignored in 20.64 seconds, and denied feature
Clippy in 71 seconds. The transport/media fixture is being added separately;
this pairing checkpoint does not establish pinned B playback, queued B writer
closure or positive End/progress over the network.

### Native tagged player qualification on current receiver base

Frozen `1d5354f81` includes full Root `cdc567b86`, including actual-receipt End
and ordered progress candidates. Its source-only Android archive passed main,
unit and instrumentation Kotlin compilation, 47 focused Shared/context and
Local player-policy/surface tests (zero failures, errors or skipped), and lint
in 5m52. Instrumentation was compiled, not executed on hardware. The bounded
owned Docker compiler/container was released on completion.

The same Native source passed 359 combined iOS simulator tests after the
bootstrap fixture correction and 349 combined tvOS simulator tests. The tvOS
runner reported `TEST SUCCEEDED` after its idle verbose diagnostic collector
was stopped; its recorded testcase results were preserved. Both iOS and tvOS
Release simulator builds reported `BUILD SUCCEEDED`. The full `cdc567b86`
merge changed no Apple bytes relative to the iOS test tree; the exact combined
branch was rebuilt for both Release targets and tested on tvOS. Documentation
index checks passed four tests; the merge's normal hook passed formatting,
workspace Clippy (1m16), catalog and 75 JavaScript syntax checks.

This qualifies Native compilation and synthetic authenticated protocol
contracts. It does not qualify a physical Native renderer against paired B/A,
Tailscale/NAT or device hardware. Initial launch still depends on freshly
available B delivery. Directed rendition/recovery controls remain unavailable;
server-accepted current-rendition controls and the Shared-only status reader
remain the next adapters. Swift's current language mode reports a non-Sendable
Start task-result warning; the next controller slice will retain an owned Void
join rather than add an unchecked Sendable claim.

### Native Shared status candidate

The existing Shared models and clients now read only the agreed B status
endpoint, `/api/v1/hls/{Bsid}/status`, with the original captured login. The
closed `subject: shared` envelope must match the retained full file reference,
B session, incarnation and control epoch. Telemetry accepts the bounded Source
VOD fields; it rejects Local identity fields, producer-failure prose, unknown
fields, malformed or negative counters, oversized bodies and foreign lineage.
The renderer shows a passive Shared summary and does not enter Local starvation,
recovery or rendition-change policy. The Apple Start owner now joins a `Void`
task through completion rather than exporting a non-Sendable result.

The final Apple status client/model/context filters passed 19 tests on each of
iOS and tvOS, with zero failures, using the established simulator compiler.
The prior Shared task-result concurrency warning is absent from these builds.
These are synthetic authenticated protocol tests, not Source physical or
hardware evidence. Android status compilation is pending the server fixture's
reserved Docker window; the preceding clean Native checkpoint remains
`ecebec1d6`. No directed controls or Local fallback are enabled by this slice.

Native status qualification: frozen `1e4b05b39` passed the exact source-only
Android main/unit/instrumentation Kotlin compiler, all 47 focused Shared,
context and Local player-policy/surface tests (zero failures, errors or skipped),
and lint in 5m43. The owned Docker container was released afterward. Both Apple
Release simulator builds also reported `BUILD SUCCEEDED`; iOS and tvOS each
passed the final 19-test client/model/context filter. Logs are
`/private/tmp/sharing-s4-native-status-android.log`,
`/private/tmp/sharing-s4-native-status-final-ios.log`,
`/private/tmp/sharing-s4-native-status-final-tvos.log`, and the two
`/private/tmp/sharing-s4-native-status-release-{ios,tvos}.log` files. The source
archive is `/private/tmp/plurx-sharing-s4-native-status-source.tar`. These checks
qualify compilation and synthetic authenticated protocol behavior; the real B
status relay, paired renderer, directed controls and hardware remain separate.

### Root B status/control relay, public Start and real pinned qualification (2026-10-04)

Root integrated S3's Source controls, status, resource custody and HTTP
adapters (`0eec7beaa`) and S2's physical fixture (`379803e8d`). It then added
the B half of §3.3:

- strict bounded clients for `/vod-status` and `/control`;
- an owned task per sent exchange, so a cancelled B waiter loses only the
  answer;
- original login and exact binding re-observed before and after Source IO;
- the agreed Shared status grammar at `GET /api/v1/hls/{B}/status`;
- control translated onto the received Source tuple and rebound to B.

B offers the Source only the advisory actions it can rebind. A Prepare
action, a Source owner hash or raw terminal prose never reaches the client.

One amendment to the Source control adapter: a definitive actor refusal is
answered with a closed `SharedControlRefusal` code in the body instead of a
409, 429 or 503. That lets B tell an ended Source session (B retires and the
client stops) from a stale fence or a rate limit. A client `End` control on a
shared session goes through B's single retirement owner. B answers `410
session_ended` only after the confirmed Source End.

§3.4 installed the authenticated Shared Start on the public media group. A
shared item detail read freshly from the Source reports `delivery_status:
available` exactly when B issued a signed locator. That is launch
capability, not producer readiness.

The real pinned fixture found three defects. None could be reproduced by the
component tests.

1. **Same-owner Source lease renewals raced.** The renewal write guard and the
   owned-route proof both pin the exact lease revision a read observed. Status,
   resources, control and the actor heartbeat all renew the same session, so a
   renewal or proof that lost to another renewal by the same owner was refused
   as a loss of authority. Effects: every B Start answered `503
   sharing_start_unresolved`, resource opens failed mid-playback, and the same
   race in the heartbeat retires a healthy session.

   The optimistic protocol now completes on both sides. It re-observes and
   retries only when the route identity is unchanged and the lease revision
   advanced (`6276ee34d` for the write, `49eb0352c` for the read). Regression:
   `source_status_survives_a_concurrent_same_owner_lease_renewal`.
2. **The receiver dispatch layer never saw a public path.** It sits under the
   `/api/v1` nest, and axum strips the nest prefix from the URI a nested layer
   sees. So every shared playlist, media, status and control request fell
   through to the Local handlers and answered 404. It now matches the
   original URI (`3cf89cbac`).
3. **The fixture's own expectations.** The public router carries the Plex
   colon routes, and segments use the Local `video/iso.segment` MIME.

Evidence on lab4 (Ubuntu 26.04, rustc 1.97.1) at `49eb0352c`:

- `sharing_receiver_real_pinned_source_h1_b_h1_h2_start_resources_and_confirmed_end`
  passed with one executed test, in a disposable Docker CGNAT namespace
  (`100.127.90.2`), over both HTTP/1 and HTTP/2. It covered:
  - fresh details reporting `available`;
  - B Start through the real Source actor;
  - playlist, init and segment relay with real FFmpeg boxes and exact lengths;
  - the Shared status grammar;
  - accepted control, its exact replay, and the stale and owner-changed
    refusals;
  - DELETE 204 and its exact retry.
- Focused filters passed:
  - the receiver control, ingress and fixture tests, plus the Source adapter
    tests (11);
  - Source status (8);
  - Source resource custody (13).

`source_resource_init_open_job_retains_actual_fd_and_guard_after_waiter_cancellation`
passes alone but can fail beside other tests. It compares a raw descriptor
number after close, which a concurrent test may reuse.

This qualifies the Copy lane on one pinned Linux namespace. It does not
qualify:

- encoded or Native text lanes through B;
- queued or backpressured writers;
- revocation while writes are parked;
- real Tailscale/NAT/DERP;
- clusters;
- devices.

### Root ownership review of the sharing lanes (2026-10-04)

Every task, timer and process shape that the S2/S3/S4/Root lanes added was reviewed
against its owner and its exit before the inventory counts in
`tests/playback/rolling-producer-owners.toml` were raised. Eight defects were
found and fixed at their cause:

| Defect | Cause | Fix |
|---|---|---|
| D1 content monitors | Monitor lifetime was the connection, not the body | `MonitoredBody`; buffered responses start no monitor (see supersession above) |
| D2 receiver liveness | The receiver dialled the Source every second for status; each call wrote the Source lease twice | One Source status probe per 10 s lease renewal; local revocation stays with per-resource validation |
| D3 retirement and settlement | Retry loops ran every 5 s with no cap, no shutdown token, and retried refusals that cannot change; failed starts never left the registry | Bounded scratch-owner schedules (receiver: 6 attempts, 5 to 80 s; Source: 24 attempts at 5 s). Definitive refusals are final; every exit frees its slot; failed starts move to the settled cache; the receiver End waits 315 s, covering the Source End budget |
| D4 copy admission | 100 ms polls dropped the live waiter between attempts, so background work could take the capacity | `admit_source_copy` holds one live waiter for the whole wait and reads policy once |
| D5 init readiness | `wait_ready` kicked the driver every 100 ms | Registers init demand and waits on `init_notify` |
| D6 abandoned writer | A dropped writer barrier parked the reaper forever, holding the encoder admission | `WriterSettlement::Abandoned`; the reap and release complete |
| D7 sharing loops | Fixed 1 s and 5 s pacing; enabling sharing locally did not wake the disabled loop | Sleep to the next due import or a local wake, bounded at 10 s for writes made on another node; healthy refresh 60 s |
| D8 rendition failure | Only the 10 s lease tick noticed a failed rendition | The Source actor also wakes on the rendition failure signal and on the drain token |

`state.shutdown` is now cancelled when a process signal starts the drain as well
as by a committed cluster leave, so these owners observe one drain token.

Open after this review:

- Nothing sweeps an expired receiver row that still has a relay binding; maintenance keeps such rows on purpose. This is part of the section 4 crash and restart work. Addressed by "B orphaned receiver crash recovery" below.
- `WriterSettlement::Abandoned` is recorded but not yet read. A panicked writer should become a rendition failure.
- A stuck retirement or settlement is reported only in the log. It belongs in Settings, Developer.

The Encoded and Native text lanes now run through the real pinned B
(`sharing_receiver_real_pinned_source_encoded_and_native_lanes_through_b`). The
Source fixture media is 320x180, so both recipes stay inside the v1 control
height contract (144 to 2160).

### B orphaned receiver crash recovery (2026-10-04)

Closes the first open item of the ownership review above. A receiver owner
that crashed, or whose in-process retirement stalled, left its RemoteSource
route, relay binding, job lease and request behind for good. Maintenance keeps
bound rows on purpose. The local takeover CAS excludes `remote_source` recipes.
The credential, viewer and endpoint needed to authenticate the owed Source End
lived only in the dead process.

**Durable dispatch record.** `sharing_relay_upstream.dispatch_envelope` is new
in the unreleased sharing schema (no marker bump; v70/v92 are not on `main`).
Activation writes `none`. `start_file_source` seals `{credential, viewer,
endpoint, Source request}` under the Upstream purpose and B-server/import AAD.
`record_receiver_dispatch` stores it after `retain_dispatch` and before
`file_start`. The record needs fresh original-login authority and the exact
live blocked owner, epoch and lease. It replaces `none` once, replays exactly
and refuses anything else. A refusal or an error sends nothing. NULL means
unknown. The sealed census, cluster import plan and migration census carry the
column; `none` is not a credential. A never-dispatched retirement now also
requires the durable `none`.

**Inventory and claim (Core, both backends).** `orphaned_receiver_sessions`
is a read-only keyset page (at most 16) of routes whose lease is 60 s past
expiry, or whose owner node carries a removal key, with a matching job lease
and pending or attached binding. Partial bindings, unknown dispatch and
undecodable rows are reported and never claimed.

`claim_orphaned_receiver_session` asserts the exact observed row, then moves
owner, epoch+1 and a 180 s lease onto this node in one transaction. It
revokes delivery grants and drops old-epoch pins. It never changes state,
publication or the discontinuity sequence. Commit-unknown is decided by an
exact re-read. Every old-epoch writer matches node and epoch, so the claim
fences them all:

- pending renewal;
- dispatch record;
- attach, publish and renew;
- retirement.

**Retirement reuse, not adoption.** `receiver_recovery_loop` is spawned
beside the claim loop, not in the maintenance tick. It runs with sharing off,
ticks every 30 s, takes batches of 8 and runs two attempts at a time. Drain is
observed between attempts. For each orphan:

1. A live local registry actor for the same Source request is its owner, so the
   loop skips the route.
2. End material is opened **before** claiming, so a node that cannot open a
   capsule never takes a route it cannot settle.
   - Attached: the upstream capsule's Source session and incarnation must equal
     the binding columns. `SourcePeerLineage::from_capsule` applies the
     `from_start` checks.
   - Pending with a sealed dispatch: a lost-Start End without lineage.
   - Pending with `none`: the committed claim is the no-send proof, because the
     dead owner's record needs its own epoch.
3. The claim runs.
4. `CleanupPeerConnection::end` runs, with `RetirementBudget` and
   `RetirementStep::from_source_end` as the live owner uses them.
5. The exact `retire_receiver_session` witness is applied. A refusal refreshes
   only a lease that maintenance moved while node, epoch and session stay ours.

Recovery never calls `start_file_source`, never creates a registry actor and
never attaches, publishes, renews or delivers. A Source refusal, an exhausted
budget, an unopenable capsule or unknown dispatch keeps the rows and reports
them as stranded.

`receiver_source_wrapper` is shared by ingress and recovery, so a recovered End
names exactly the dispatched Start.

**Visibility.** `/sharing/status` gains `receiver_recovery`:

- `last_scan_at_ms`;
- `in_flight`;
- `retired_total`;
- `stranded`, with incarnation and reason, bounded at 32.

The Sharing settings "This node" card renders it. The new metric is
`plurx_sharing_receiver_orphan_total{outcome=retired|stranded|lost}`. A
stranded route is logged once as a warning, with no credential material. The
design's pause setting is deliberately absent.

Evidence on lab4 (rustc 1.97.1):

- Core `--lib sharing_receiver_orphan`/`sharing_receiver_dispatch`: six tests
  over memory/pooled SQLite × retained/rebuilt principal layouts (grace and
  removal key, foreign fence and recipe, exclusive claim and fenced old-epoch
  writers, maintenance-ended row, dispatch record replay/refusal and census,
  attached retirement with other routes unchanged and read-only replay,
  keyset paging and stranded/unreadable rows).
- `--test store_contract sharing_receiver_orphan_three_voters_exclusive_claim_fence_and_confirmed_retire`:
  two concurrent claims through the actual Raft log, exactly one wins; old
  renewal, dispatch record and retirement refuse; confirmed retirement and
  read-only replay.
- Daemon: `receiver_source_wrapper_matches_ingress_prepare`,
  `receiver_orphan_capsule_rejects_foreign_aad_and_lineage`,
  `receiver_orphan_sweeper_skips_live_local_actor`,
  `receiver_orphan_sweeper_never_retires_without_end_receipt` (claimed, End
  unanswered, drain: binding and lease kept), `receiver_orphan_sweeper_never_adopts_producer`
  (never-dispatched route retired with no actor, publication or delivery;
  unknown dispatch kept and reported), `receiver_orphan_sweeper_observes_drain`.
- The real pinned CGNAT fixtures (`..._h1_b_h1_h2_start_resources_and_confirmed_end`,
  `..._encoded_and_native_lanes_through_b`) pass with the dispatch record in
  the Start path.

Not qualified: a process-level SIGKILL of a published or pending B daemon
followed by restart. `tests/sharing_daemon_restart.rs` restarts paired
daemons but has no shared-playback harness. Clusters, NAT/DERP and devices
remain open, as above.

### Shared pre-session assets and typed file-suffix closure (2026-10-04)

The Assets lane of the direct/progressive/pre-session design. Subtitles,
PGS overlays and chapter thumbnails now reach a shared viewer before any
session exists, and every shared file suffix B does not serve refuses typed.

**Source (A).** `shared_source_assets` mounts the four asset suffixes of
§5.4 on the peer router under `source_content_guard`, with the 30 s resource
deadline and a 16-permit admission (429 `sharing_asset_capacity`). The
complete `SourcePlaybackTarget` travels in one `cinemashare-reference` header
of at most 1 KiB; path item/file must equal it. Authority is the decision's,
now factored as `shared_playback::source_file_authority` /
`SourceFileAuthority::still_current` and used by both: active grant,
grant-visible item/file witness, signed revision, switch on — checked before
the work and again after it, then `attach_source_file_authority` scopes the
body to the file. Only an admitted witness turns the Source file number into
a Local `get_file`. The work is the Local per-file code, split out without
behaviour change for Local callers: `stream::subtitle_vtt_for_file` (bitmap
tracks refused), `pgs_overlay::manifest_for_file`/`object_for_file` (switch,
pgs-v1 track check, 202 while cold, generation bound to track and revision)
and `chapter_thumbs::serve_for_file` (switch, cache, failure memo, permits).

Design deviations, following the code:

- The Source `MediaFile` comes from `get_file`, not
  `playback_planning_snapshot`: assets need no planning settings, and the
  pre/post witness already binds the revision.
- No extra detached task was added. Subtitle extraction and overlay
  preparation already run in owned, cached flights that outlive the request.
  A chapter extraction is bounded in-request (10 s permit wait + 15 s
  extraction) inside the 30 s route deadline, so it is never cut short by it.

**Receiver (B).** `shared_receiver_assets` mounts the same four suffixes
under each signed file base on the media group, under
`receiver_content_guard`. The caller is the signed-in viewer (header or
`?token=`) or `?session=` naming a live receiver session whose recipe binds
exactly this import, lifecycle, item, file and revision
(`recipe_binds_file`) and whose delivery attachment is current
(`ReceiverStartActor::bound_file_viewer`); an account sent with a session
must be that session's viewer. The query is closed (one `token`, one
canonical `session`). `SharingManager::read_file_asset` shares the
import/assignment/pinned-connection preflight with `read_file_decision`
(`assigned_file_peer`) and asks the Source through
`PeerConnection::file_asset`. That client accepts only 200 with exactly one
`Content-Type` from the closed set and one `Content-Length` within the cap:

- WebVTT: 2 MiB, `text/vtt; charset=utf-8`, leading `WEBVTT`, UTF-8.
- Manifest: 1 MiB, `application/json`.
- PNG objects and JPEG thumbnails: 1 MiB, with their magic bytes checked.

Identity encoding only, exact length, and 202 only for a manifest. Refusal
bodies are drained and never relayed. B projects the manifest through
`project_shared_overlay`, which keeps the generation and relative object
names and replaces the file ID. Bodies carry a file-scoped
`ReceiverContentAuthority` for the reaching login
(`attach_receiver_file_authority`) and `Cache-Control: no-store`. There is
no B cache. B admission is its own 16-permit semaphore, not
`catalogue_admission`, whose one-per-import rule would refuse a watch page's
parallel chapter thumbnails.

**Typed closure.** `shared_receiver_assets::closure_router` answers
`stream.mp4` (permanently) and `direct` (one marked arm for the direct lane
to replace) with `422 sharing_resource_unsupported`. It does so for every
method, HEAD included, before any import, locator, account or Local lookup.

Evidence on lab4: see the commit message for the exact test run. The ignored
CGNAT fixture (`actual_pinned_playback`) now also fetches a WebVTT through
B in the native modes, a missing chapter thumbnail, and the `stream.mp4`
closure; it was not run here.


### Shared direct play lane (2026-10-04)

Direct play of a shared file now runs through the Source session machinery
and is relayed by B with the same Range and HEAD behaviour as Local direct
play. Progressive `stream.mp4` stays unsupported for shared files (Root
decision; the assets lane answers it with a typed 422).

**Start.** A viewer asks with the ordinary `CreateSession` body and
`"presentation":"direct"`. B accepts only `vod` or `direct`. Because the value
lives inside the retained request, it is part of B's recipe fingerprint and
of the Source's canonical request identity: the same request ID can never
replay as HLS (409).

On the Source, `own_start` branches only at preparation.
`prepare_source_direct` repeats the HLS authority reads (grant, item/file
witness, revision, planning snapshot) and runs
`stream::decision_for_source_file` with the player's real caps. Anything but
`DirectPlay`, a burn or native-subtitle ask, or a media type outside Local's
audio/video table is refused with `422 sharing_start_unsupported`. B is never
trusted. Intent, claim, assignment and activation authority are the HLS path's
own code, so a direct session takes the same per-grant 4 and Source 8 slots.

The owner (`transcode/source_direct.rs`) is a `SourceViewerInner` with a
`direct` file and no producer. It shares the registry, body ledger,
`SourceResponseGuard`, retirement and settlement with VOD owners. The
settlement tail of `run_source_owner` became `finish_source_owner`, used by
both. It activates and publishes its route like the VOD owner. Its 10 s tick
renews the lease and retires the session after 300 s with no open body and no
byte open (the VOD idle bound). Status renews the lease but never counts as
activity.

The fence is the file. `open_source_playback_fence` opens without following
links, requires a regular file with the scanner's size and mtime, and pins the
object version (dev, inode, size, mtime, ctime). Every byte open and every
Start or status replay re-proves that exact object, and the guard holds the
fence, so a change mid-body ends the body. A relinked file is refused even when
its bytes are unchanged; a fresh start plans the new object.

**Source byte route.** `POST …/sessions/{request}/direct` follows the
`resources` precedent: the exact published lineage plus
`direct: {method, range, if_range}`. `stream::plan_file_range` and
`file_range_head` were extracted from `serve_file_range`; Local and Source
both use them, so 200/206/416 and "If-Range present means ignore Range" cannot
drift. The exchange is a POST, so the planned Content-Length rides in
`cinemashare-content-length` and the real Content-Length is the body sent
(zero for HEAD and 416). The body is `source_file_body` with a start offset
behind `hold_source_body` and `guard_source_response`. Its read slots
(`SOURCE_READ_JOBS`) are now awaited rather than tried: a long direct body
beside segments must queue, not fail mid-stream. Retirement still ends the
wait at once. A watch state is never written.

**B relay.** `GET/HEAD {file_base}/direct?session=<B UUID>` sits on the public
media group. The only accepted query is one canonical `session`; a missing
one is 400 and an unknown one 404. Nothing on this route admits a session or
dials a Source. The actor must be a direct actor whose recipe equals the
verified locator's import, lifecycle, item, file and revision. Account headers
are optional; when present they must authenticate as the intent's viewer. Then
the HLS relay's own gates apply: `current_delivery_attachment` (original login
and delivery grant), `retain_accepted_connection`, and an owned counted open
task. `SourceDirectHead::parse` recomputes B's own plan from the viewer's
request and the published length, and requires the Source head to equal it
exactly, lineage echo included. The relayed body has a 30 s idle deadline per
frame, not a fixed total. It re-chunks to 64 KiB and pulls one upstream frame
only after the last is handed on. A direct session answers its HLS paths,
status and control with `422 sharing_resource_unsupported`; DELETE retires it
as usual.

Range values relay exactly as Local parses them. A value that is not visible
ASCII travels as the empty value, which Local's parser refuses the same way.
Only the first two values travel, because a second one already makes a bytes
Range invalid.

**Deviations from the design, with reasons:**

- No RemoteSourceRecipe v2. The presentation is already in the recipe's
  `request_json`. A version bump would touch both Core backends and the
  retirement witness for no new fact.
- No ControlBootstrap in the direct reply. No control route exists for direct
  play, Local or shared, so advertising one would be false. The reply carries
  `session_id` and `control_epoch`, which is all the lineage needs.
- B refuses a Range value over 16 KiB with 431 before any Source IO. Local
  would parse it. This is pathological input only.
- The peer link is HTTP/1, so it has no h2 window. B's public HTTP/2 send
  buffer is Hyper's default, 400 KiB per stream.

**Proof boundaries.** No settlement is inferred from body EOF, abort or row
absence; End still waits for the actor's body ledger. A parked read job keeps
its guard after the response is dropped. A cancelled B waiter loses only the
answer; the open task keeps the sent request. Status never moves activity.

Evidence on lab4 (rustc 1.97.1):

- New focused tests, 15 passed, 0 failed:
  - Source: actual decision required, grant/Source slots,
    Range parity with Local, If-Range parity, revocation mid-body, read join
    before the End receipt, symlink/relink/resize refusal (7);
  - wire: relay preserves Local's plan over 0/1/10/4096-byte files,
    206/416/HEAD heads exact, closed Start envelope (3);
  - B: exact session query, binding required, HEAD/Range cannot bypass,
    idle-deadline body outlives the 30 s resource deadline, slow reader
    memory bound (5).
- Affected existing filters (`sharing`, `source_`, `direct_range`,
  `receiver_`): 306 passed, 8 ignored (the opt-in CGNAT fixtures).
- Clippy with denied warnings on plurxd and plurx-core, all targets.

Not qualified here: `sharing_receiver_real_pinned_source_direct_range_head_through_b`
(registered as an opt-in fixture) covers the real pinned B relay, the single
Source claim across repeated ranges, and logout ending an open body. It needs
the disposable CGNAT namespace and has not run. Clients, clusters, NAT/DERP
and devices remain open.

### Shared web direct play (2026-10-04)

The web client now uses the direct play lane above. Copy HLS stays the route
for everything else, and Local paths are unchanged.

**Route.** `choosePlayRoute` picks `shared_direct` only when the Shared
decision's method is `direct_play` and the ordinary initial route is `direct`.
That means the container is in this browser's own caps, the audio is the
default track, there is no A/V offset and no burn. A direct decision the
browser cannot take as a raw file, such as a non-default audio track, goes
to Copy HLS, never a progressive remux. The grade does not matter here: the
bytes are untouched, and the Source repeats the decision with the same caps
at Start.

**Start.** `openSession` sends the ordinary `CreateSession` with
`presentation:"direct"`. It drops the HLS-only fields (segment budget,
transport, rung, `copy`) and the subtitle ask, because the Source refuses
those for raw bytes. `SHARED_DECISION.start` accepts only `vod` or `direct`.
It validates the reply with `sharedPlaybackDirectStartContext`
(`core/file-context.js`), which requires exactly five fields:

- `presentation:"direct"`;
- a lowercase v4 B session;
- a safe, non-negative `length`;
- one of B's `DIRECT_MIMES`;
- a `url` equal to this context's own `{file_base}/direct?session=<id>`.

An HLS reply to a direct ask is refused, and so is a direct reply to an HLS
ask.

**Attach.** `attachSharedDirect` binds the player to the returned context
and puts the URL on the element with no `token` query. It keeps the B session
in `PLAYER.sharedDirect`, owned by the attachment it creates. `sessionId`
stays null, so no status poll runs and no control reporter starts, the same
as Local direct play. Progress uses the existing Shared beat path through
the bound context.

**Release.** `DELETE /api/v1/hls/{session}` is sent when a new attachment
begins on the player (`beginPlaybackMediaAttachment`), when the predecessor
player is retired, and on close.

**Expiry.** B retires a direct session after 300 s with no byte request.
When the element raises a network error (code 2) on a Shared direct
attachment that has reached its metadata, it starts one fresh direct play at
the current position through `requestPlaybackMediaChange`. Doing so uses up
the allowance. The new attachment earns another only when it reaches its own
timeline, so there is no timer and no loop. The stall Try again on a Shared
direct play uses the same fresh Start instead of the old URL. A direct-bound
context may start again, direct or Copy HLS, under the accepted login, the
same rule progress follows. Other route changes on a direct play (audio,
quality, decode rescue) go to Copy or encoded HLS. An HLS-bound context still
has no generic replacement. (Superseded by the P0 reopen below.)

Evidence on lab4 (node 22.22.1): `tests/web/file-context.test.js` passes 17
of 17 and `tests/web/shared-decision.test.js` passes 16 of 16, including the
new direct cases. `make web-check` passes, and `scripts/web-types` is
unchanged at 518 diagnostics. Two browser checks in that lane skip because
Playwright is not installed on lab4. None of this has been checked in a
physical browser against a real pinned Source/B pair.

### Shared directed change reopen — P0 (2026-10-04)

A quality, audio or subtitle change on a shared HLS session is now a clean
make-before-break reopen. No Source successor is staged yet (that is P1/P2);
the interim drives the change from the existing control answer and the
existing client fallback, with no new task or timer.

**Source (A).** `SourceViewerActor::control` accepts a changed selection on a
new sequence and records it like any other ask. Every composed Source answer
(`source_control_response`) says `delivery.preparation: "none"`: a Source owner
has no preparation slot, so nothing is ever being built for the current ask.
A changed ask that reuses an accepted sequence is the ordinary stale fence.
Still refused with `Unsupported` (422 at the adapter): an acknowledgement (it
can only name a slot this Source never offered), an intent envelope, End
(B's retirement owner sends End), and any control on a direct owner. The
frozen `original_selection` and its create-side derivation are removed; they
existed only for the old refusal.

**B relay.** The answer is relayed and rebound as before. Because B never
offers the Source `prepare_replacement` and cannot carry a Source successor,
any evaluated `preparation` becomes `"none"` at B; an older Source that did
not evaluate the field stays absent. An acknowledgement is refused at B with
`422 shared_control_unsupported` (field `acknowledgement`) before any Source
exchange is owned or sent.

**Reopen.** The client's one reopen is a fresh shared Start of the same file
at the position sampled when the decline arrived, with the new selection and
no lineage fields. B publishes it first. Only then, from the publication
point in `run_owner`, `supersede_predecessors` hands every older live attempt
of the same user and viewer playback id to its single retirement owner with
reason `Superseded`. That owner sends the Source its End, waits for the
receipt, and frees the B and Source slots. Nothing deletes a row or abandons
an obligation. A registration ordinal keeps an older Start that publishes
late from retiring its replacement. An unpublished, failed or already retiring
successor supersedes nothing, so its predecessor keeps serving. The web client
also DELETEs the predecessor when the new attachment begins; both paths meet
in the same idempotent `begin_retirement`.

**Deviation: per-session playback identity.** The design assumed the reopen
could share the viewer's playback id. It cannot. Both the B activation and
the Source activation use the playback-pointer CAS: one current session per
(principal, playback id), and with no predecessor named the pointer must be
absent. Naming the predecessor instead (`expected_predecessor_incarnation_id`)
would move B's pointer at activation. Every B attach, publish, renew and
delivery predicate requires that pointer, so the predecessor would stop
serving before the successor published: break-before-make. Each B session is
therefore its own playback on both sides, `receiver_playback_id` =
`shared-<source_request_id>`. B claims and activates its route under it,
`receiver_source_wrapper` and `receiver_source_request` put it in the Source
session in place of the viewer's (as they already did for `request_id`), and
the pending retirement witness names it. The viewer's playback id stays in the
retained request and fingerprint; B groups one player's sessions by it. The
same conflict also blocked the direct play lane's move from direct to Copy HLS
while the direct session was still live; that move now works the same way.
The sharing schema is unreleased, so no dispatched route from an older build
needs the old wrapper.

**Web.** `SHARED_DECISION` keeps a `bases` map from every bound context (HLS or
direct) to its file context. `decision` and `start` accept a bound context
under the accepted login (the rule progress and the direct restart already
follow), so `fallBackDirectedChange` reopens through the ordinary
`play()` path immediately on `preparation: none` instead of after the 12 s
offer bound. Audio changes, which reopen through `requestPlaybackMediaChange`,
use the same fresh Start. The new bound context keeps the same accepted
record, so the ordered watch state carries on: no detail re-read, no re-seed,
and the next beat names the new B session with the next sequence. Lineage
fields, burned subtitles and HDR/DV asks stay refused for a shared Start, as
before.

**Proof boundaries.** Supersession is decided from B's process-local registry
and needs B publication; it is not inferred from a row. The predecessor's
retirement is the existing owner and still needs the confirmed Source End.
The `preparation: none` answer is a statement about the Source owner, not
evidence of any physical state.

Evidence on lab4 (rustc 1.97.1, node 22.22.1):

- New focused tests:
  - Source: `source_control_changed_selection_declines_preparation`,
    `source_control_ack_refused_without_slot`.
  - B: `sharing_receiver_control_changed_selection_relays_none`,
    `sharing_receiver_new_start_supersedes_same_playback_after_publication`,
    `sharing_receiver_supersede_never_precedes_successor_publication`.
- Affected daemon filters (`sharing`, `source_`, `direct_range`,
  `receiver_`): 322 passed, 9 ignored (the opt-in CGNAT fixtures). Four
  failed on the first run. Three were expectations of the old refusal or the
  old wrapper and were updated; the fourth,
  `sharing_artwork_blocked_http1_http2_bytes_own_their_lease_without_a_monitor`,
  answered 503 for 429 under load. All four then passed on a rerun.
- Clippy with denied warnings on plurxd and plurx-core, all targets.
- Web: `tests/web/shared-decision.test.js` 17/17, including the new reopen
  and sequence carry-over case. `tests/playback/web-control.test.js` 23/23;
  its new Shared case is a decline that reopens at once from the bound
  context. `scripts/web-types` unchanged at 518.

Not qualified here:
`sharing_receiver_real_pinned_quality_reopen_preserves_position_and_releases_slot`
(registered as an opt-in fixture) covers a real pinned B relaying a changed
selection with `none`, and a reopen at the sampled position published beside
its predecessor. It also checks that the predecessor is superseded (route
ended, Source back to one active session, exact DELETE replaying 204), and
that the successor serves and controls at the new rung. It needs the
disposable CGNAT namespace and has not run.

Native clients (separate lane) need the same three things the web now does:

- treat `preparation: "none"` on a shared session as a decline and reopen at
  once with a fresh Start, with no lineage fields;
- carry the ordered progress sequence across the reopen;
- release the predecessor after the new attach. B supersedes it on
  publication either way.

Stall recovery that reopens with `previous_session_id` is still refused for
shared sessions. Clusters, NAT/DERP and devices remain open.

### Native Apple Shared controls, directed reopen and direct play (2026-10-04)

The Apple Shared player now does the three things the web does, plus
server-accepted controls. Android is a separate lane and still has the
earlier passive player.

**Status.** The word fields of the Shared status grammar use B's
`status_token` rule exactly (ASCII letters, digits, `_`, `-`, `.`, at most
32 bytes). The passive summary is composed only from those tokens and bounded
integers (`Shared HLS · 720p · h264_videotoolbox · running · 42 s ahead`). A
status answer is shown only while the player still holds the session it was
read for; direct play has none.

**Controls.** `SharedControlChannel` (`clients/apple/Sources/SharedPlaybackControl.swift`)
keeps B's exact tuple, one client instance id per player and one ordered
sequence. Capabilities ride sequence 1; the selection is the frozen raw ask
from the Start. Seek, pause and play change the renderer only after B accepted
that exact sequence (`SharedControlStep`). 425/429/503 replay the identical
bytes once after `retry_after_ms`. A 409 is never adopted, because B's tuple
does not move. A 410 during a pause pauses locally and the next play or seek
starts fresh; a 410 on play or seek starts one fresh session per attachment.

**Directed change.** Quality, audio and subtitle go out as a changed selection
on the next sequence. `preparation: "none"` reopens at once: a fresh decision
and Start at the position sampled when the answer arrived, with the same
playback id and no lineage fields. The predecessor is DELETEd only after the
new media is attached. The ordered progress order is keyed by the Source item,
so the next beat names the new session with the next sequence. Absence of the
field is not a decline. Subtitles are native WebVTT renditions only; a track
that would need a burn is not offered.

**Direct play.** Chosen when the decision is `direct_play` with delivery mode
`direct`, the source is not Dolby Vision (AVPlayer's progressive-DV black
plane, as Local), the planned audio is the default track and there is no
offset or subtitle ask. The Start carries `presentation: "direct"` and no HLS
field, and only B's five-field reply bound to this context's own
`{file_base}/direct?session=<B>` is accepted. AVPlayer reads that URL with no
account header. After the item fails past its own timeline, a header-free HEAD
that answers 404 or 410 earns one fresh direct Start at the last position. Any
change from direct play goes to copy or encoded HLS.

**Ownership.** One owned `Void` operation (Start, exchange or reopen) holds the
player; `stop()` cancels and joins it. Every B session the controller started
and no longer plays stays listed until its DELETE was sent, so a release that
`stop()` interrupted is finished by `stop()`. No timer, watchdog or unchecked
`Sendable` was added. Local paths and guards are unchanged.

Evidence on the lab Mac (Xcode 27 simulators, owned DerivedData): `make
apple-test` passed 780 iOS and 764 tvOS tests, zero failures (nine new in
`SharedPlaybackControlTests`). Two deliberate mutations, dropping the
accepted-sequence check and applying a pause on an ended session, each failed
their test. `tests.operations.test_playback_surface_fence` passes with the
Shared player's published inventory updated. These are synthetic
authenticated protocol tests. Physical playback against a real B/Source pair,
direct play on device, and the 300 s direct-expiry restart are not qualified
here.

Regression-Test: clients/apple/Tests/SharedPlaybackControlTests.swift::testRendererMovesOnlyAfterAcceptanceAndDirectedChangeReopensOnNone

### Android Shared controls, directed reopen and direct play (2026-10-04)

The Android client now does the three things the P0 section above asks of a
native client, plus current-rendition controls and direct play. Local paths,
the numeric Local guards and the original-account checks are unchanged.

**Status.** `SharedPlaybackStatus` decodes to a typed `SharedVodMetrics` DTO
bound to the exact started playback (`sessionId`). Every word must match the
server's own `status_token` grammar (`[A-Za-z0-9_.-]{1,32}`), so a sentence or a
path cannot render. The summary is fixed words and bounded numbers only. A
status read for a session the player has since replaced is dropped.

**Controls.** `SharedControlChannel` speaks `plurx-playback-control-v1` on the
Start's own route with the B tuple (generation = B incarnation, epoch = B
epoch), one stable `client_instance_id` per player across reopens, and ordered
sequences restarting at 1 per B session, with capabilities on sequence 1 (SDR,
no dual-player preparation). It declares no actions, so B may answer only
`none`. The selection on every exchange is the frozen raw ask from the Start
request: Manual 144 stays 144 whatever the encoder delivers. A deferred answer
(425, 429, 503) is resent byte for byte after `retry_after_ms` (bounded 250 ms
to 5 s, at most twice). An uncertain one (no answer) is resent once. A 400, 409
or 422 is a typed refusal and is not retried. 410 means the session ended. An
answer for another tuple or sequence is not an acceptance.

`SharedPlaybackOwner` runs every viewer command under one fair mutex. Seek
(`seeking` + `seek_target_ms`), pause (`hold`) and play (`active`) reach
ExoPlayer only after B accepts. A refusal leaves the picture where it is and
says why. A play or seek after a 410 is a fresh Start at the position. A
cadence job owned by the owner sends a renewal at B's 5 s interval, and
progress plus status every second tick. It skips a tick while a command runs.

**Directed change.** A quality, audio or subtitle choice is sent as an ask on
the current session. B answers `preparation: "none"`, because it never carries
a Source successor (absence is an older relay saying the same). The owner then
asks `/decision` again with the plan's retained caps. It makes a fresh Start
at the sampled position with the same viewer `playback_id`, a new
`request_id` and no lineage fields. It attaches that Start, and only then
DELETEs the predecessor. The ordered progress pool is keyed by the watch key,
so the next beat names the new session with the next sequence. A refused ask
keeps the current session.

**Direct play.** `sharedPlaybackPlan` picks `presentation: "direct"` only when
the Source's decision is `direct_play` for these caps and the ask is the file
as it is: default audio, no subtitle, no A/V offset, no AAC transcode. The
Start drops `height`, `copy` and the subtitle fields. The five-field reply is
decoded exactly, and its URL must equal this context's own
`{file_base}/direct?session=<B>`. ExoPlayer plays it through a progressive
source on `Net.capabilityClient`, with no account header. There is no status
or control exchange, and the renderer is local. Stop sends DELETE. A 404 or 410
from the renderer earns one fresh direct Start at the current position, once
per attachment that reached its timeline: no timer and no loop. A reply whose
type ExoPlayer cannot read as a file (WMA, octet-stream) is released and the
same decision is played as Copy HLS. A directed change from direct play goes
to Copy or encoded HLS.

Evidence on lab3 (pinned `plurx-android-build` image, JDK 25):

- New suites: `SharedPlaybackSessionTest` (4) and `SharedPlaybackOwnerTest`
  (4). Together with the existing Shared, context and decision suites: 51
  passed, 0 failed, 0 skipped.
- Full `testDebugUnitTest`: 869 tests, one failure, `CreateRetryTest.onlyANotYetAnswerIsRetriedAtAll`. It fails the same way on the unchanged base: its expected code set predates `quality_catalog_unavailable`.
- `lintDebug`: no errors. `compileDebugAndroidTestKotlin` passed.
- Python fences: player builder, playback surface, control wire, credential
  exposure, test markers, caps wire and contracts all pass. So do
  `scripts/playback-surface-fence` and `scripts/player-input-fence`.

Not qualified here: a physical Android device against a real pinned
Source/B pair, the native subtitle rendition on reopen, rate limiting under a
real seek storm, and the Apple half of these slices.

### Shared prepared successor — P1/P2 (2026-10-04)

A directed change on a shared HLS session can now be a prepared handoff: B
builds the successor beside the session the viewer is watching, offers a
`prepare` naming only its own URLs once that successor is published, and
settles the client's commit or abort exactly. A client that does not ask for
it keeps the P0 reopen.

**Deviation: B owns the successor; the Source is unchanged.** The design
staged the successor on the Source (a `media_session_preparations` row for the
Source session, a new `…/sessions/{pred}/successor` route, a B↔Source mapping
row). The code says otherwise. Since P0 every B session is its own Store and
Source playback (`shared-<source_request_id>`), and every Source guard (claim,
worker authorization, owned routes, settlement and retirement in
`store/sharing_source_sessions.rs`) refuses any preparation row naming the
incarnation and requires the session's own playback pointer. A Source-staged
successor would have meant reopening every one of those guards on both Store
backends. Instead the successor is what the P0 reopen already is, an ordinary
second B session of the same viewer, file and player playback id, started by
B through the same owner, claim, Source Start, attachment, publication and
delivery grant as any shared Start. That one path already counts the Source
slot, the grant slot and the B slot, replays a lost Start by request id,
renews the Source lease every period, and retires through the single
retirement owner with a confirmed Source End. No Store schema, Source route or
Source code changed. "P1" (Source staging) therefore has no code of its own;
both halves land as one B change.

**Negotiation.** A client asks for a Shared successor by declaring
`shared_prepare_replacement` beside `prepare_replacement`, plus
`dual_player_preparation`. The Local promise alone is not enough. Today's web
client declares `prepare_replacement` everywhere, but it would keep beating
Shared progress on the bound context of the session it left (and Apple and
Android Shared channels declare no actions). Without the new name those
clients get `preparation: "none"` and reopen exactly as in P0. The name
follows the precedent of `prepare_replacement`: the server half ships first,
and an older server ignores an unknown action name. B never forwards either
name, or any acknowledgement, to the Source. The `PREPARED_QUALITY_HANDOFF`
switch is read exactly as Local reads it (default on); off means `none`. There
is no new gate.

**Staging (`shared_receiver_successor.rs`).** For each Source-accepted
exchange B applies Local's dispatch rule (`take_preparation_dispatch`). The
first exchange records the ask the session was created for. A different ask
dispatches once. An ask that arrives while the slot is busy waits for the
first exchange after the slot frees. A staged successor for an ask the client
has since left is withdrawn (reason `Replaced`), and the new ask is dispatched
only once that successor has fully retired and freed its slots, so one player
never holds three Source sessions. The successor request is the predecessor's
retained request with the new selection, a fresh request id, the sampled
film position (clamped to the film) and no lineage fields. A selection a
shared Start cannot carry (a burn, an explicit codec or grade, a negotiated
candidate, direct play) is a typed decline. So is a full B registry, or a
Source that refuses the Start (cap full): no slot, `none`, and the client
reopens. Nothing answers 5xx for it. An uncommitted successor stages nothing
of its own.

**Offer.** While the successor is unpublished the answer is `staging`. Once it
is published, and no outranking action is on the exchange, it is `offered`
with `Prepare { action_id, session_id, playlist_url, control, media_origin_ms,
effective_selection }`, taken only from the successor's own projected B Start.
The B session, `/api/v1/hls/{B}/index.m3u8` and B's control bootstrap
(generation = the successor's B incarnation) are the only identities named.
One action id is minted per staging and reused by every later offer. The
response is re-validated against the relay contract before it leaves. The
successor's playlist, segments, status and control dispatch through
`by_session` like any published B session, so its media is served before
commit.

**Settlement.** An acknowledgement is decided against the slot before
anything is sent to the Source. These are refused `409 stale_control`, with
no Source exchange:

- an action id the slot never offered;
- an acknowledgement past the deadline (VOD lease + 30 s; this also withdraws
  the successor);
- a commit whose `committed_media_origin_ms` or current ask no longer matches
  the offer;
- a second commit.

The exchange then goes to the Source as an ordinary one: same sequence, no
acknowledgement. Only after the Source accepts it does B settle. A commit
marks the successor committed and supersedes the predecessor, plus any older
attempt of the same player, through the make-before-break path. The
predecessor's retirement owner sends the Source its End. An abort or failure
withdraws the successor, and its owner frees both slots. A deadline is
enforced on the successor owner's existing renewal tick (no new timer).
Retiring the predecessor for any reason withdraws an uncommitted successor.
Progress beats on an uncommitted successor are refused; after commit the
predecessor is retiring and refuses them.

**Exact replay.** The exact answer bytes of each acknowledgement exchange are
kept per session (bounded to eight). They are served before any authority
read, because a commit retires the session it was sent to. The same request
replays byte-identical; a changed body under an answered sequence is
`stale_control`; neither writes or sends anything. When the retired
predecessor is pruned from the registry, its answers move to its bounded
tombstone, so a lost commit answer still replays. A waiter cancelled between
the Source's acceptance and B's settlement leaves nothing half done. The
client's retry is a same-sequence Source replay, and B settles then.

**Proof boundaries.** The successor relation is process-local, like every
other piece of B playback state. B playback does not survive a B restart:
orphan recovery retires both routes with their owed Source Ends and never
adopts. That is why no durable mapping row is written, and why "restart
between commits recovers the same incarnation" does not apply. Supersession
and withdrawal are decided from B's registry, never from row absence or lease
expiry. Retirement still needs the confirmed Source End.

Evidence on lab4 (rustc 1.97.1):

- New B tests, all passing: fourteen in `shared_receiver_successor_tests.rs`
  and `sharing_receiver_control_never_forwards_an_acknowledgement`.
  - Request building:
    `sharing_receiver_successor_request_carries_the_ask_at_the_sampled_position`.
  - Publication and commit: `…_prepared_publication_never_supersedes_its_predecessor`,
    `…_commit_supersedes_exact_predecessor_and_settles_the_slot`.
  - Withdrawal: `…_abort_withdraws_successor_and_keeps_predecessor`,
    `…_predecessor_retirement_withdraws_uncommitted_successor`.
  - Offer: `sharing_receiver_prepare_names_only_b_urls_and_bootstrap`, including
    the Local-only client that keeps `none`;
    `…_prepare_offered_only_after_successor_publication`.
  - Replay: `…_ack_replays_exactly_and_refuses_a_changed_replay`,
    `…_lost_commit_reconciles_after_predecessor_retired`.
  - Stale and dispatch: `…_stale_acknowledgements_are_refused_before_the_source`,
    `…_observe_ask_dispatches_once_and_withdraws_a_left_ask`.
  - Deadline and media: `…_successor_deadline_and_progress_follow_the_commit`,
    `…_successor_media_relays_before_commit`.
  - Capacity: `sharing_receiver_viewer_cap_declines_typed`.
- Affected daemon filters (`sharing`, `source_`, `direct_range`, `receiver_`):
  333 passed, 10 ignored (the opt-in CGNAT fixtures), 4 failed. None of the
  four is in code this change touches. Three were load or host-port flakes
  that passed on an exact rerun: a Source voter's API port in use, a Source
  actor Deadline, and an fd-close race. The fourth,
  `sharing_artwork_blocked_http1_http2_bytes_own_their_lease_without_a_monitor`,
  answered 503 for 429 under load, as in P0, and passed alone.
- Clippy with denied warnings on plurxd and plurx-core, all targets.
- `tests/validation` (253, including the ownership inventory: +1 test-only
  spawn, +3 test-only fixture waits), `make validation-lint`,
  `tests.operations.test_docs_index` and `tests.operations.test_known_red`
  pass. `make history-check` cannot read history in this partial clone (a
  promisor blob of an earlier commit); these commits are not corrective.

Not qualified here:
`sharing_receiver_real_pinned_prepared_handoff_commit_and_abort` is
registered as an opt-in fixture and has not run, because it needs the
disposable CGNAT namespace. Over H1 and H2 it covers:

- staging, then an offer naming only B URLs;
- two Source sessions;
- the successor playlist served before commit;
- a byte-identical commit replay, before and after the predecessor retired;
- the predecessor's confirmed End;
- an abort freeing the successor's Source slot;
- a late commit refused.

It shares its pair setup with the P0 reopen fixture, which now declares only
`prepare_replacement` and so still exercises the reopen.

Client work still owed before a client may declare
`shared_prepare_replacement`:

- **Web.** Done; see "Web Shared prepared successor and fixture-driven B
  wire" below. The owed work was: on commit, rebind `t.fileContext` to a
  bound context for the successor B session (`SHARED_DECISION` `bases`/`accepted` keyed by it), so
  progress beats and any later reopen name the successor. Then send the next
  beat with the next sequence, and drop the P0 predecessor DELETE for a
  committed handoff, since B supersedes it. Then add
  `shared_prepare_replacement` to `SUPPORTED_ACTIONS` for Shared sessions
  only.
- **Apple.** Done; see "Apple Shared prepared successor, watched state, next
  episode and fixture" below.
- **Android.** Shared channels declare no actions and no dual-player
  preparation today. They need a second player on the offered B playlist, the
  acknowledgement sequence on the predecessor channel, a switch of the Shared
  control channel and progress pool to the successor's tuple after commit,
  and then the two action names.

### S4 catalogue, history and artwork audit (2026-10-04)

This audit compares the S4 catalogue, Continue Watching, history and artwork
paths with §6, §7.4 and the Catalogue/Progress rows of §13.1. Each row names
the code that enforces the rule and the tests that prove it. Rows marked
**fixed** were wrong or missing. Each has a regression test that fails on the
old code.

| Row | Code anchor | Tests | Status |
|---|---|---|---|
| IDs above 2^53, up to `i64::MAX`, end to end | `SourceId::parse` (`plurx-core/src/sharing.rs`). The keyset seek in `source_catalogue_page` casts the boundary ID with `cast(... AS INTEGER)`, and `RECORD` emits `cast(i.id AS TEXT)`. Web: `sharedCatalogueId` | `sharing_wire_ids_preserve_huge_values_and_reject_aliases`, `sharing_catalogue_source_keyset_survives_boundary_deletion_and_moves_with_exact_huge_ties` (now pages ties at `i64::MAX-1`/`i64::MAX` with no wrap or repeat), `sharing_catalogue_source_pages_complete_during_continuous_metadata_writes`, `sharing_current_scope_rejects_oversized_and_malformed_inputs`, `sharing_art_resources_bind_full_identity_user_lifecycle_and_expiry_through_rewrap`. Web: "contexts copy source authority and retain exact unsafe-for-Number source IDs" | proven. Coverage added for the top of the range |
| Same numeric ID on two Sources and on Local | `SharedReference` and `BrowseSeen` key on the full reference. History is keyed `(source_server_id, catalogue_epoch, remote_item_id, user_id)` and is separate from Local `watch_state` | `sharing_catalogue_live_cursor_resumes_across_revisions_and_deduplicates_sources`, `sharing_private_watch_orders_updates_and_isolates_sources_and_assignments`, `sharing_continue_groups_isolate_sources_filter_assignments_and_refuse_hidden_overflow`, `sharing_file_locator_binds_full_source_and_import_lifecycle_without_numeric_collision`, `sharing_manual_watch_override_takes_next_global_sequence_and_refuses_late_beats` (Local `watch_state` untouched). Web: "same numeric source file cannot collide with local playback resources or source keys" | proven |
| Live keyset paging while sort keys move | `(sort_key COLLATE BINARY, i.id) > boundary` in `source_catalogue_page`. B deduplicates by full reference (`BrowseSeen`, web `seenItems`) | `sharing_catalogue_source_keyset_survives_boundary_deletion_and_moves_with_exact_huge_ties` (new): an item moved ahead is resent and displayed once, an item moved behind waits for the next open, and nothing unchanged is skipped. Also `sharing_catalogue_source_pages_complete_during_continuous_metadata_writes`. Web: "shared pagination encodes opaque cursors, keeps full-reference deduplication and refuses repeated cursors" | proven. Test added: no test combined moves with a boundary deletion |
| Item/file deletion and catalogue epoch change mid-paging | A deleted boundary is a value boundary, not a row lookup. The cursor context binds `server_id`, `catalogue_epoch`, `grant_id`, `library_id` and the filter digest. B `peer_failure` | Keyset test above (the boundary item itself is deleted). `sharing_catalogue_item_identity_does_not_recycle_after_deletion`. `sharing_catalogue_cursor_authenticates_boundary_and_refuses_replay_context` (now covers server, epoch and library substitution). `sharing_catalogue_page_failures_keep_typed_reopen_and_absence_results` | **fixed**: B turned the Source's 404 for a deleted or unexported item into 503 `sharing_source_unavailable`. It is now 404 `sharing_not_found` |
| Cursor substitution across imports, Sources, sorts and accounts | `CatalogueCursor::resume` gives `QueryChanged`, `Expired` or `Invalid`, and the Source answers 409, 410 or 400. B's new `page_failure` keeps those typed. Web: `sharedCatalogueLoadPage` reopens once. Accounts: B checks that the account is assigned the cursor's library (`read_catalogue_inner`) before forwarding. The library is part of the signed context. A cursor is a position, never authority | Cursor test above, plus `sharing_catalogue_page_failures_keep_typed_reopen_and_absence_results`, `sharing_private_watch_orders_updates_and_isolates_sources_and_assignments` (an outsider gets no assigned libraries) and `sharing_catalogue_cache_key_separates_full_authority_and_request`. Web: "an expired or substituted Source cursor reopens the list once instead of retrying a dead cursor" | **fixed**: B mapped every cursor refusal to 503, so the page kept a Load-more button on a dead cursor. B now returns 410 `sharing_cursor_expired`, 409 `sharing_query_changed` or 400 `sharing_cursor_invalid`. The web page reopens from the start exactly once and does not loop |
| Artwork cache denial and bounds | `shared_artwork::receiver` verifies the resource token before and after a fresh pinned `read_artwork`, which checks lifecycle, grant and current assignment. The disk cache is write-only, keyed by user, lifecycle, grant and digest, and never serves bytes | `sharing_art_resources_bind_full_identity_user_lifecycle_and_expiry_through_rewrap`, `sharing_art_snapshot_refuses_wrong_scope_moves_and_private_names`, `sharing_art_disk_serializes_lru_rehashes_and_isolates_full_references`, `sharing_art_nonqueued_byte_capacity_survives_held_owners`, `sharing_artwork_blocked_http1_http2_bytes_own_their_lease_without_a_monitor`. Web: "unconfirmed bitmap retirement remains charged and current403 retires only its Source" | proven by existing tests |
| Global progress order across sessions and devices | One `sharing_watch.sequence` per Source/epoch/item/user. The guard `sequence < excluded.sequence` is in both `save_remote_watch` and receiver progress. Web `SHARED_DECISION.progress` drops a 409'd beat and resyncs | `sharing_private_watch_orders_updates_and_isolates_sources_and_assignments`. Web: "ordered Shared beats retry identical payload then resync409 without replaying old position", "two imports of the same Source item share ordered beats across B sessions", "a declined shared HLS change reopens as a fresh Start of its base file and carries the progress sequence" | proven by existing tests |
| Manual mark unwatched (and watched) | New `SharingCatalogueStore::set_remote_watched`: a single guarded upsert takes `sequence+1` inside the write, under import, assignment and captured generations. New `POST /api/v1/shared/imports/{i}/items/{item}/watched` `{watched}` (§5.3) after a fresh Source membership read. New web Mark watched/unwatched control on movie and episode details | `sharing_manual_watch_override_takes_next_global_sequence_and_refuses_late_beats` (both backends: a late beat conflicts, an older one is stale, the position clears, the item leaves Continue Watching, watched keeps the position, and refusals write nothing). `sharing_catalogue_viewer_routes_require_login_and_fail_closed_without_import` (login, body grammar, canonical IDs). Web: "manual Shared watched state posts only to the B-private Shared route" | **fixed**: there was no route, Store operation or control |
| Next episode reauthorizes against current scope | Web `sharedCatalogueNextEpisode` follows Source hierarchy and order only through B viewer routes, which check current assignment and Source scope. `sharedCatalogueLaunch` reads fresh details and makes a new Start. `playNextEpisode` dispatches non-Local contexts to `playNextSharedEpisode` | Web: "Shared next episode follows Source order through B and mints a fresh authorized start" and "Shared next episode dispatches to the Source-order resolver and a fresh authorized start, never a Local route" | **fixed**: Shared playback had no next episode |
| Show and season pages (found during the audit) | `viewSharedCatalogue` binds the children page to the library reference | Web: "a show page renders its seasons instead of refusing children as a changed source" | **fixed**: children were checked against the parent's item reference, so every child was refused as "Shared source changed" |

Still open:

- Android has neither the manual watched control nor Shared next episode. The
  web client is the reference implementation; Apple has both (see "Apple Shared
  prepared successor, watched state, next episode and fixture", below).
- B answers a refused artwork read (assignment removed, import inactive) with
  503 rather than a typed 404. No bytes are served either way.
- None of this is physically qualified against a real pinned Source/B pair.

### Shared prepared successor — commit transport and cancelled change (2026-10-04)

Two defects in the P1/P2 handoff above, found on the integrated
`claude/sharing-batch4` tree.

**Commit answer cut by its own predecessor.** The real pinned fixture failed
over B H2: the commit exchange got `BrokenPipe`. The commit settled at B and
called `supersede_predecessors` inside the control handler, before the answer
was returned. That started the predecessor's retirement, which cancels `stop`.
The predecessor's connection monitor (`retain_accepted_connection`) then cut
every transport it was retained on, including the one still carrying the
commit answer. Over H1 the body is written inside the connection's own poll,
so the race was usually won; over H2 the stream task hands the frame to the
connection driver, and the cut won.

The fix is ordering and ownership, with no delay or retry:

- `commit_successor` still marks the successor committed at once. The
  predecessors it replaces now travel in a `CommittedHandoff` owned by the
  commit answer's writer (`CommitAnswerWriter`).
- The writer drops its connection guard first, then the handoff. Only then is
  each predecessor handed to its retirement owner with reason `Superseded`.
  The handoff runs on every path, including a client that left before reading
  the answer, so a settled commit always supersedes.
- A connection monitor whose session retires as a hand-over to the same
  viewer's successor (`Superseded`, or a withdrawn successor's `Replaced`)
  releases its custody without cutting when none of that session's writers is
  still on the transport. Admission is already closed, so no writer can join
  after the count.
- A transport with a writer still in flight, and every revocation, deletion
  or administrative stop, is cut exactly as before.

Without that last distinction the commit answer, or the successor's own media
on a shared H2 connection, could still be cut after its body ended.

**A cancelled change restaged the original rendition.** The dispatch rule
compared each ask with the last *dispatched* ask. After a change to X was
withdrawn (the client returned to its original ask A) or aborted, A differed
from X, so B staged a successor for the rendition it was already delivering.
An idle player then held a second Source and B slot until the deadline. Now
each session records its own ask (`own_digest`): the first accepted exchange,
or the ask a successor was staged for. An ask equal to it never stages.
`dispatched_digest` now only keeps a refused or failed ask from being
redispatched. It is cleared when the successor is withdrawn for a left ask or
aborted, so a renewed change stages again.

Evidence on lab4 (rustc 1.97.1), integrated tree:

- New regressions, each failing on the old code and passing now:
  - `sharing_receiver_commit_answer_writer_is_released_before_the_predecessor_retires`;
  - `receiver_handover_releases_idle_transports_and_cuts_busy_or_revoked_ones`;
  - `sharing_receiver_cancelled_change_never_restages_the_sessions_own_ask`.

  The commit tests now assert the predecessor keeps serving until the
  handoff runs.
- Real pinned CGNAT fixtures in the disposable namespace:
  - `sharing_receiver_real_pinned_prepared_handoff_commit_and_abort` passed
    over H1 and H2, twice (85.5 s and 96.3 s);
  - `sharing_receiver_real_pinned_quality_reopen_preserves_position_and_releases_slot`,
    whose superseded predecessor now takes the release-or-cut path, passed
    (34.3 s).
- Affected daemon filters (`sharing`, `source_`, `direct_range`, `receiver_`):
  348 passed, 10 ignored, 2 failed. The failures were
  `sharing_artwork_blocked_http1_http2_bytes_own_their_lease_without_a_monitor`
  (503 for 429 under load) and
  `source_resource_media_open_job_retains_actual_fd_and_guard_after_waiter_cancellation`
  (an fd-close race). Neither is in changed code, and both passed on an exact
  rerun.
- Clippy with denied warnings on plurxd and plurx-core, all targets.
  `tests/validation` (253) passes, with the ownership inventory at +1
  test-only wait.

### Apple Shared prepared successor, watched state, next episode and fixture (2026-10-04)

The Apple client now covers the four client items the B prepared successor,
the S4 audit and the fixture parity matrix left open. Android remains open
for all four.

**Prepared successor.** The Shared channel declares `dual_player_preparation`
at sequence 1 and both `prepare_replacement` and `shared_prepare_replacement`
when the existing Developer switch enables two-player preparation (the switch
Local reads; a source with no video codec keeps the P0 reopen, since there is
no frame to prove a switch with). B records a session's first exchange as the
ask it was started for, so a session that has not spoken states its own ask
before the change. A `prepare` binds only whole: another canonical B session,
its own `/api/v1/hls/{B}/index.m3u8` or master playlist and its own control
bootstrap (`/api/v1/hls/{B}/control`, 5 s cadence, 300 s lease)
(`SharedPreparedOffer`). It is bound to the player's context through the
ordinary Shared Start grammar before anything is primed; one that does not
bind is settled `failed` and reopens.

The machinery is Local M6's, reused rather than copied: `PreparedOfferWait`
reads `staging`, the offer, a later `none` and its 12 s bound;
`PreparedReplacementCoordinator` owns the ledger; the commit uses Local's
rendezvous, 4 s alignment bound, 6 s first-frame bound and 250 ms tolerance.
The second AVPlayer is muted, layer-less and never played, primed at the film
position (a shared session is VOD, so its zero is the film's, per Local's
`sessionMediaOriginMs`). Every acknowledgement rides the predecessor's B
channel on its own ordered sequence: progress at most once a second and not
resent, and the terminal one always; a lost answer resends the identical bytes
(at most three sends), which B replays byte-identically. Only an accepted
committed exchange makes the successor the player's session: a fresh ordered
channel on its B tuple, its status, and the shared progress beat, which names
it from the next beat on with the item's next sequence. The predecessor is not
DELETEd; B retires it. A paused viewer, a failed or late successor and a
switch without a frame take the P0 reopen. A commit B did not accept after
the item already moved reopens fresh and ends both sessions. A viewer action
while the change is still waiting or priming takes the player back (`aborted`);
the switch and its settlement do not yield. `stop()` also ends a staged
successor it never adopted.

**Manual watched.** Movie and episode details offer Mark watched/unwatched,
posting `{watched}` to `POST /api/v1/shared/imports/{i}/items/{item}/watched`
and adopting only an answer for the state asked; the page then re-reads its
details. The next progress beat resyncs on its typed conflict.

**Next episode.** At a natural end with autoplay on, the player resolves the
next episode in Source order through B's viewer routes only (next in the
season, else the first episode of the next season; bounded cursors, a repeated
cursor refuses, never another library) and starts it from fresh B details: a
new authenticated context, decision, plan and playback id in a new player
session. The lookup is owned by the player view.

**Fixture.** `SharedProtocolFixtureTests` reads every client-parsed group of
`tests/sharing/protocol-cases.json` (matrix above). It exposed the five
`file_suffixes` rows and `hls_start` `unsafe-duration`; the Shared context now
refuses a non-canonical or out-of-range (above 4095) subtitle or chapter index
(the Local route keeps its pattern), and the Start binder refuses a
`duration_ms` past 2^53 - 1.

Evidence on the lab Mac (Xcode simulators, owned DerivedData): `make
apple-test` passed 796 iOS and 780 tvOS tests, zero failures (sixteen new:
`SharedProtocolFixtureTests` 8, `SharedPreparedHandoffTests` 5,
`SharedCatalogueActionsTests` 3). Reverting the two fixes failed exactly the
six fixture rows; dropping the successor control-URL check failed its test.
`tests/validation` (253) and `tests.operations.test_playback_surface_fence`
(the Shared player's published inventory gains `preparing`) pass. These are
synthetic authenticated protocol tests: the two-player switch, its first-frame
proof and the B commit/abort against a real pinned Source/B pair are not
qualified here, and B's `observe_ask` stages a successor for a session's own
original ask after a withdrawn change (the client aborts such an unrequested
offer the next time it exchanges, but nothing exchanges while the viewer is
idle).

Regression-Test: clients/apple/Tests/SharedPreparedHandoffTests.swift::testCommittedSuccessorBecomesThePlayersSessionAskAndProgress

### Android Shared prepared successor, catalogue actions and fixture parity (2026-10-04)

The Android client now covers the client work the P1/P2 and S4 audit sections
above leave owed, and reads the protocol fixture. Local paths, the numeric
Local guards and the original-account checks are unchanged; `versionCode` is
not bumped.

**Prepared successor (A).** When Settings → Developer leaves prepared
replacement on (the same switch Local uses, default on), the Shared channel
declares `prepare_replacement` and `shared_prepare_replacement`, plus
`dual_player_preparation` in its sequence-1 capabilities. Off, it declares no
actions and keeps the P0 reopen. `SharedPlaybackOwner` drives the handoff
with the Local M6 pieces rather than new ones: `PreparedOfferWait` (with
`noneOnTheAskDeclines`, because B decides on the exchange that carries the
ask), `PreparedReplacementLedger` for the acknowledgement grammar, and
`RendezvousHold` for the meeting point. Every exchange on the predecessor
while the handoff lives carries the new ask, since an exchange carrying the
old one tells B the viewer left it. `staging` and absence keep waiting (12 s
bound); `none` reopens at once. On `offered`, `SharedStart.successor` binds
the `prepare` like a Start: only B's successor session, its
`/api/v1/hls/{successor}/…` playlist and its control bootstrap (v4
generation, 5 s cadence, VOD lease) are accepted. The renderer primes a
second ExoPlayer (`buildSuccessorPlayer`, muted, no surface, parked). Then:

- `metadata_ready` once it is playable, then a park at the rendezvous;
- `buffer_ready` once the park has landed with runway through it;
- the switch at the rendezvous: the successor takes the surface, volume and
  transport intent, and the predecessor is retained;
- `committed`, echoing the offer's `media_origin_ms`, once the successor
  renders a real frame (5 s bound).

Every acknowledgement rides the predecessor's channel. A commit with no
answer is asked again byte for byte under its own sequence
(`SharedControlChannel.replayLast`), so B replays its answer or settles once.
After an accepted commit, control (a new channel from sequence 1, same
`client_instance_id`), status and Shared progress move to the successor's B
session. The predecessor is not DELETEd, because B retires it. A withdrawn
successor (`none` after the offer), a readiness or rendezvous failure, or an
offer the client cannot bind is acknowledged `failed`. A frameless switch is
`failed` after the predecessor is put back. A refused commit puts the
predecessor back. Each of these takes the P0 reopen exactly once, with the
decision already asked for the successor. A seek while unswitched
acknowledges `aborted` and reopens at the target with the new selection. A
commit whose outcome stays unknown reopens and ends the successor too. The
view releases a pipeline it has moved off from its own update, and `stop`
releases whatever is left. There is no timer for either.

**Manual watched (B).** Shared movie and episode details show Mark watched /
Mark unwatched, which posts `{watched}` to
`POST /api/v1/shared/imports/{import}/items/{item}/watched` (§5.3). The answer
must be `updated: 1` with the asked state.

**Next episode (C).** `SharedLibraryClient.nextEpisode` follows Source order
through B's viewer routes only: the next episode of the season, else the
first episode of the next season. Every child page is validated against the
import's library, and the cursor walk is bounded and refuses a repeated
cursor. When a Shared episode ends and autoplay next is on, the details screen
starts the next one with `prepareNextSharedEpisode`. That is a fresh detail
read, context, decision and playback id under the current login, never an
inherited context.

**Fixture (D).** `tests/sharing` is a JVM test resource directory, and
`SharedProtocolCasesTest` reads every Kotlin column group of the parity
matrix above through the shipped decoders. It exposed two real bugs, now
fixed: the Shared file-suffix grammar accepted indexes above 4095 and leading
zeros (`PlaybackFileContext.sharedFileSuffix`), and a Shared Start accepted a
duration above 2^53-1 ms.

Evidence on lab3 (pinned `plurx-android-build` image):

- New suites: `SharedProtocolCasesTest` (8), `SharedCatalogueActionsTest` (3)
  and `SharedPreparedHandoffTest` (5). The handoff suite covers a commit
  whose answer is lost and replayed with identical bytes, the move of control
  and progress, `none` on the ask, a withdrawn successor, a refused commit,
  and a seek while staging. Each mutation of the two grammar fixes fails its
  fixture row.
- Full `testDebugUnitTest`: 885 tests, 0 failures, 0 skipped. `lintDebug`: no
  errors.
- Python fences pass: player builder, credential exposure, playback surface,
  player input, control wire, caps wire, test markers, contracts. So do
  `scripts/playback-surface-fence`, `scripts/player-input-fence` and
  `make validation-lint`.

Not qualified here: any physical device against a real pinned Source/B pair.
That includes a real two-pipeline prime and switch on a phone and on a
television (where Local M5.5 measured dual prime failing), first-frame proof
on the surface, the next episode on a real series, and the watched control
against a real B.

### Web Shared prepared successor and fixture-driven B wire (2026-10-04)

The web client now consumes the B prepared successor, and the B wire the
parity matrix listed as open is driven from the fixture in Rust.

**Negotiation.** A Shared reporter (`startPlaybackControl` on a Shared file
context) declares `shared_prepare_replacement` beside `prepare_replacement`
(`PlurxPlaybackControl.Reporter` option `sharedSuccessor`; Local reporters are
unchanged). `dual_player_preparation` stays the Settings → Developer "Allow a
second player" switch every session honours, so B stages nothing with it off
and the change keeps the P0 reopen. There is no new gate.

**Offer.** A `prepare` on a Shared session binds whole before a second
pipeline is primed: `SHARED_DECISION.successor` binds it under the accepted
login through `sharedPlaybackSuccessorContext`, which requires another B
session than the predecessor's, its own `/api/v1/hls/{successor}/index.m3u8`
or `master.m3u8` (the Shared playlist query grammar), and its own control
bootstrap through the ordinary Shared Start grammar
(`/api/v1/hls/{successor}/control`, a new v4 incarnation, 5 s cadence, 300 s
lease) on the same file. An offer that does not bind is acknowledged `failed`
on the predecessor's channel (B withdraws it), nothing is primed, and the
change takes its one P0 reopen. `none` still reopens at once, as in P0.

**Switch and commit.** The existing web prepared-replacement player
(`player/prepared-replacement.js`, `player/directed-change.js`) primes,
aligns, proves the frame and switches, exactly as for Local. Every
acknowledgement rides the predecessor's reporter. Only B's acceptance of the
`committed` exchange moves the player's Shared context: `p.fileContext` and
`p.meta.fileContext` become the successor's bound context (same accepted
record, so the ordered watch sequence carries on and a later reopen starts
from it), and the successor's reporter starts on its own bootstrap. Status
then reads the successor's session and incarnation. The predecessor is never
DELETEd after a committed handoff; B supersedes it. A Shared beat retained for
one session is never resent under another
(`SHARED_DECISION.progress`), so a lost predecessor beat cannot wedge the
successor's progress. A change that settled before its offer waiter confirmed
keeps its settled outcome (`requestQualityChange`).

**Status reader.** `sharedPlaybackStatusMetrics` refuses an envelope whose
top-level keys are not exactly B's six, or whose `incarnation_id` and
`control_epoch` are not the started session's control tuple
(`withPlaybackFileSession` now carries it for HLS sessions; a direct session
has none and gets no sample). Fixture rows `envelope-foreign-incarnation`,
`envelope-foreign-control-epoch`, `envelope-missing-incarnation` and
`envelope-unknown-key` (client layer) cover it; Swift and Kotlin already
refuse all four, and the Swift exact row count moved from 13 to 17.

**B wire extraction (no behaviour change).** `shared_status_body` (the
envelope `receiver_status` emits) and `receiver_control_precheck` (the bounded
v1 check, generation, owner epoch and acknowledgement plan, returning a typed
`PrecheckRefusal`) are pure functions; `receiver_control` calls them. The
fixture now drives the envelope, `refusal_response`, the precheck (against a
real registered actor with no staged successor) and `rebind_to_receiver`.
That exposed one stale fixture row: since P1/P2 an acknowledgement with no
staged successor is `409 stale_control`, not `422 shared_control_unsupported`
(P0's answer). The row now says so; every client's outcome for it is still
`stop`.

Evidence on lab4 (rustc 1.97.1, node 22.22.1):

- New Rust tests, all passing: `sharing_protocol_fixture_receiver_status_envelope`,
  `sharing_protocol_fixture_receiver_refusals`,
  `sharing_protocol_fixture_receiver_control_precheck`,
  `sharing_protocol_fixture_receiver_preparation_rebind`.
- Affected daemon filters (`sharing`, `source_`, `direct_range`, `receiver_`):
  353 passed, 10 ignored (the opt-in CGNAT fixtures), 1 failed:
  `sharing_artwork_blocked_http1_http2_bytes_own_their_lease_without_a_monitor`
  (503 for 429 under load, as before), which passed on an exact rerun.
- Clippy with denied warnings on plurxd and plurx-core, all targets.
- Web: `tests/web/shared-decision.test.js` 18/18 (new: the successor binds
  whole, the next beat names it), `tests/web/file-context.test.js` 18/18,
  `tests/web/sharing-protocol-cases.test.js` 9/9,
  `tests/playback/web-control.test.js` 23/23 (new: a bound Shared offer
  commits and moves the context only on B's acceptance; an unbindable offer
  is acknowledged `failed` and reopens once; a Shared reporter declares
  `shared_prepare_replacement`), `tests/playback/seek-control.test.js` 24/24.
  `scripts/web-types` unchanged at 518.
- `tests/validation` (253), `make validation-lint` and
  `tests.operations.test_docs_index` pass. `make history-check` cannot read
  history in this partial clone (a promisor blob of an earlier commit).

Not qualified here: a real browser handoff against a pinned Source/B pair
(two pipelines, frame proof, commit, status and progress moving to the
successor), and the Apple and Android fixture suites with the four new rows.

### Source-owned burns and HDR — completion design (2026-10-06)

**Status:** implemented in `d4d2ec8e4`, with Rust check and denied Clippy passed; runtime
qualification deferred under Paul's 2026-10-06 test policy. Track the batch
and unproved boundaries in [the status page](SHARED-LIBRARIES-STATUS.md).

The existing Source actor refuses burns and HDR encodes because the Local
preparation path can start shared-cache work whose lifetime is not owned by
the Source session. Removing those refusals alone would let Source retirement
release its admission while extraction or font work was still running.

1. **The admitted Source probe owns burn preparation.** Resolve the embedded
   subtitle from the Source's own scanned file and prepared request. Extract
   one subtitle-only Matroska stream, including attachments, from the already
   held source descriptor. Bound it to 64 MiB and the existing Start deadline.
   Every FFmpeg and Fontconfig child uses the Source command executor, which
   retains admission, cancellation, reap and pipe joins.
2. **The rendition owns its artifacts.** Keep the extracted sidecar in an
   anonymous file and the text renderer's frozen Fontconfig directory in the
   existing reference-counted engine. Hand the encoding its own descriptor.
   Last-reference release removes the artifacts; no sweep or watchdog is
   introduced. Local playback keeps its existing cache behavior.
3. **The Source decides the grade.** Reuse the existing planner with the
   player's actual capability document and Source file facts. Burns always
   encode. SDR-only players get the supported tone-map route; preserving HDR
   requires the existing encoder and display capability proofs. B validates
   the delivered range against its retained request, including the precise
   PQ/HLG presentation range, before publishing the session.
4. **Dolby Vision conversion stays a typed limitation.** Its per-source RPU
   proof is not yet owned by Source preparation. Refuse DV preservation,
   conversion and re-encoding before dispatch with
   `sharing_start_dolby_vision_unsupported`. Direct play retains untouched
   bytes and its normal capability decision. Do not silently claim an HDR10
   or SDR conversion of a DV source.
5. **Retain regression definitions, defer execution.** Cover text and bitmap
   burn selection, bounded extraction/settlement, HDR grade binding, and DV
   refusal. Compile test targets now; execute required regressions only after
   the main-promotion adversarial review. A generated fixture without rendered
   output inspection is not evidence of subtitle or HDR fidelity.

Native subtitles beside HDR and downloaded subtitle burns remain typed
unsupported shapes until their Source-owned preparation is implemented.
They do not justify a hidden feature gate or a disabled Developer switch.

### Fresh Source invocation custody before preparation (2026-10-06)

**Status:** implemented in `e0fd7b898`; pinned compile and Clippy passed on
the builder checkpoint. Runtime regression execution remains deferred.

Preparation could reject a valid, authenticated Start before the Source had
claimed the invocation. B had already dispatched the request, so the Source
could neither prove a fresh refusal nor confirm End. The Source now claims
the complete canonical reference and request under the domain
`plurx.sharing-source-invocation.v2` before fallible preparation. Only a fresh
`Acquired` result owns the undispatched g0 cleanup receipt. An old claim,
commit-unknown result or missing actor remains unresolved.

The durable invocation fingerprint is separate from the planner's normalized
recipe and engine/cache identity. A private binder checks the complete
principal, request, playback and Source file tuple before factory admission.
Both HLS and direct factories require that explicit binding; a historical
normalized fingerprint cannot admit an unbound prepared request.

Typed preparation failures, including unsupported Dolby Vision encoding, can
therefore confirm exact End after releasing the fresh claim. Regression
definitions cover stable refusal/End retries and rejection of an unbound
normalized preparation. This closes the preparation-refusal gap; cluster
forwarding and distributed ingress cleanup remain separate work recorded on
the status page.

### Cluster forwarding and physical ingress custody (2026-10-06)

**Status:** Source runtime checkpoint `d32be9ca8` and receiver runtime checkpoint
`2e4fd9e21` compiled and linted. Combined integration `0ba500735`, including the final
lost-reply follow-ups and bounded closure fanout, passed its pinned normal hook
and Core contract-feature compilation. Platform receipts are recorded on the
[status page](SHARED-LIBRARIES-STATUS.md). No new runtime or
topology acceptance receipt is claimed. Paul deferred test execution until main
promotion after its adversarial review.

The concrete missing proof was the outer accepted connection: forwarding an
HTTP response through the playback owner did not make that owner's internal
writer own the ingress node's queued bytes. Source and receiver now use the
same private accepted-driver registry, bounded per-principal durable ledger,
sealed registration and authenticated actual-closure acknowledgments. End
retains its original deadline and cannot confirm from SQL ownership, stream
EOF, missing actors or elapsed time. A same-driver End first returns closing
so it does not wait for the connection carrying its own response.

Source placement uses signed read-only observations of the exact authorized
file, without requiring a warm fragment index or a legacy MPEG-TS offer.
The g0 claim records the chosen process and original credential hash before
fallible preparation. Fresh producer admission requires an opaque permission
backed by a registered ingress; subsequent renewal revalidates each retained
session owner's permission without requiring a continuously open HTTP
connection between requests. Receiver forwarding uses fresh read authority
and the actual retained owner. Its orphan cleanup closes original ingress
obligations before Source End; no database-only actor adoption is introduced.

The [custody decision](SHARED-LIBRARIES-INGRESS-CUSTODY.md) describes the schema,
retry and compatibility boundaries. Real non-owner HTTP regression definitions
cover signed placement/forwarding and same-driver End, with test-owned finite
servers and transport tasks. Their execution is deferred. Same-host fixtures
cannot establish remote-only mounts, Tailscale, independent NATs, physical
players, revocation bounds or active Shared restore qualification.

The final Source follow-up `8b885f707` narrows the earlier commit-unknown
limitation: only the actual joined invocation retains its private fresh intent
and planned incarnation. A same-write guard may clear that exact undispatched
g0, bound to the original worker boot and initial credential hash. Absence, a
foreign intent, g1 dispatch or historical metadata still cannot supply cleanup
proof. Pending forwarding retains the original process identity and releases
its bounded routing pin only after an authenticated exact settled End.
