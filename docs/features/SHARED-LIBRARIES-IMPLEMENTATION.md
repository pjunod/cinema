# Shared libraries — the Tailscale build contract for Opus and Sol

**Status:** ready to build after Opus S0 corrections; S2 topology
qualification pending; S1 implemented; task gate pending · **Revised:** 2026-10-02 ·
**Source rechecked:** `15e36f7f4` ·
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

Proposed node-local configuration:

```toml
[sharing]
transport = "tailscale-serve"
bind = "127.0.0.1:32444" # bare host; container override is 0.0.0.0:32444
peer_port = 32443
key_directory = "/var/lib/plurx/sharing-tls"
```

For containers publish only `127.0.0.1:32444:32444`; the process binds the
container interface so Docker can reach it. The container interface is not
a public-host wildcard. Treat other containers on that bridge as network
peers with no application authority. Do not use host `100.x` publication:
Docker must start successfully before Tailscale obtains an address.

Proposed initialization command and existing Tailscale commands:

```bash
plurxd sharing init-tls --key-directory /var/lib/plurx/sharing-tls
tailscale serve --bg --tcp=32443 tcp://127.0.0.1:32444
tailscale serve status --json
```

`init-tls` is to be implemented; it creates a node key and self-signed
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
per-import `assignment_generation` on whole-set assignment replacement;
revalidate affected viewers and cancel only those who lost access.

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
empty; `endpoint_generation` owns local endpoint-edit CAS. Observed source
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
| GET/HEAD `/items/{item}/files/{file}/...` | Typed subtitle/overlay/chapter suffixes before start; active grant and item/file/library membership, bounded extraction; no raw file lookup |
| GET/HEAD `/playback/{session}/files/{file}/...` | Direct/progressive and other admitted file bytes under §7.3; exact principal/session/recipe |
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
| S1 | merged into effort | [PR #746](http://192.168.4.7:3000/noirr/plurx/pulls/746), `aaafc1a0f5` | SQLite v92 / Hiqlite v70; purpose-bound secrets; pairing/rotation/assignment transactions. Five store contracts, 11 sharing unit tests, schema migration parity and 31 import-inventory tests passed; workspace Clippy and catalog lint passed. Run 3890 and the final Effort development gate passed on the exact candidate; landed in the effort as `971265536a` with all seven regression fields. |
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
| S1 | merged into effort | [PR #746](http://192.168.4.7:3000/noirr/plurx/pulls/746), `aaafc1a0f5` | SQLite v92 / Hiqlite v70; purpose-bound secrets; pairing/rotation/assignment transactions. Five store contracts, 11 sharing unit tests, schema migration parity and 31 import-inventory tests passed; workspace Clippy and catalog lint passed. Run 3890 and the final Effort development gate passed on the exact candidate; landed in the effort as `971265536a` with all seven regression fields. |
| S2 | implementation in progress; topology qualification open | [draft PR #759](http://192.168.4.7:3000/noirr/plurx/pulls/759) | Dedicated loopback TLS transport, pinned direct dialing and fixed Tailscale DNS, isolated peer/admin routes, durable claim/rotation recovery, authenticated endpoint refresh and advisory Developer switch implemented. Two-NAT, shared-machine Serve and Docker isolation/egress receipts remain open; S2 is not complete. |
| S3 | implementation started; ownership migration open | `codex/sharing-s3-principals` (unpublished) | Canonical Local writers, complete principal reads and seven-table candidate rebuild implemented; ownership and actual-voter lifecycle regressions passed. Caller refusal and retained-read census are qualified; Shared grant/scope admission, migration installation and coordinated upgrade qualification remain open; §16.4 records the boundaries. |
| S4 | source catalogue and private history candidates implemented; qualification pending | `codex/sharing-s4-catalogue` | Consistent live keysets and batch metadata, candidate order maintenance and durable item identities, peer metadata routes and receiver-only ordered history. Viewer/cache/artwork/details, activation floor and qualification remain open. |
| S5 | not started | — | — |
| S6 | not started | — | — |
| S7 | not started | — | — |
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

**Task:** [S1 PR #746](http://192.168.4.7:3000/noirr/plurx/pulls/746) targets
`effort/shared-libraries`. Manual effort run 3884 refused its first catalog
mapping: the two new store adapters must also select `cluster.auth` in the
CI scope resolver. That mapping was corrected, and all 253 validation tests
passed locally (one platform-specific skip). The runtime source is unchanged.

Run 3887 passed policy, web, Apple and Android checks, but its Rust job
rejected the isolated Hiqlite spike lockfile before compilation. Commit
`aaafc1a0f5af031c3adbfe6760ad4edcff7cb082` synchronizes that lockfile;
`make spike-lock-check` and the normal hook passed locally. Exact-candidate
[run 3890](http://192.168.4.7:3000/noirr/plurx/actions/runs/3890) has passed
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

A sharing handler registers its authority monitor on the accepted transport.
Each monitor checks current authority every second with a one-second deadline
and cancels that connection on refusal or unavailable authority. The
connection task selects that cancellation while Hyper is writing and owns the
monitor tasks. This closes blocked transports within the three-second grant
bound, including after Hyper has consumed the entire application body.
Hyper 1.10.1 / h2 0.4.16 may still own queued DATA after Body Drop; a flush from
another stream does not establish that the protected stream has drained.
Therefore no Body Drop, global flush counter or timer retires these monitors.

The registries are finite: 64 content monitors process-wide and 32 per accepted
connection, with immediate refusal rather than a waiting queue. Completed
responses on an idle connection retain those slots until connection completion.
A request exceeding the connection bound cancels that connection; global
exhaustion returns a closed 429. On HTTP/2, authority cancellation closes all
multiplexed streams on that connection, including unrelated ordinary responses.
Ordinary handlers do not register monitors or cancel the sharing token.

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
These are single-file reads. The planned all-file detail query must also
bound 64 files and aggregate private projection bytes at 8 MiB before
materialization; that query and closed wire media facts remain open.

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
