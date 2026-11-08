# Cinema dependency boundaries — primitive entry and trusted authority ownership

**Status:** implementation draft — SDK-free packaging is built under the private
Core co-owner; behavior/release qualification is pending. Primitive P3a remains
a separate task; cross-crate authority extraction remains blocked.
**Written:** 2026-11-04 · **Owner:** Codex · **Reviewer:** Astra.
**Source:** runtime `5484d7986729ee942daf7fd83afa5555cf38edcd`.

This decision refines P3/P4 of the [modular-build plan](CINEMA-MODULAR-BUILD-PLAN.md).
A primitive-only first
task changes no sealed representation, trusted factory or backend selection.
SDK-free packaging may retain Core as the current private authority/SQL owner.
Cross-crate authority extraction has a separate strict entry gate and remains
blocked until its ownership/API and downstream negative fixtures are complete.

## 1. Acyclic ownership

The initial contracts crate contains only common values and checked validators.
It cannot depend on Core, full Store, playback planning or Hiqlite. Canonical
schema and identity owners are future cross-crate targets below authority;
Store API consumes opaque
authority and owns its own checked row witnesses. SQLite and optional cluster
adapters sit above Store API. The authority crate's optional replicated module
may depend on the actual Hiqlite SDK but never on high-level MembershipManager
or a Store implementation.

Secrets are not moved as simple DTOs. Their unchecked legacy decoder, purpose
master verifier, scoped signing functions and platform-secure file IO need one
trusted identity owner. Keep them private until that complete closure moves.

## 2. P3a: first reviewable primitive slice

One task owns the new plurx-contracts crate, workspace/Core manifests and lock,
and these compatibility facades. No other task edits those integration files.

| New owner | Existing source to move | Kept in Core |
|---|---|---|
| error | StoreError and its rusqlite Error conversion together | Store services; no foreign-trait implementation left behind |
| lease | Lease, LeaseClaim, existing bounded validators and publication_successor | StoreCoordinator and ClusterJobAuthority, which use Store/JobKind |
| catalog | Library/Item DTO group and existing metadata/edit/book patches through sort_title_for | MediaFile private helpers, TraktAuth, secrets and opaque playback proofs |
| principal | PlaybackPrincipal, SharingViewerKey, PrincipalError and canonical UUID parser | Full sharing/session engines |
| playback | CandidateRoute enum only | Candidate planning, selection and link proofs |

Current owners are [errors](../../crates/plurx-core/src/error.rs),
[coordination](../../crates/plurx-core/src/cluster/coordination.rs),
[catalogue values](../../crates/plurx-core/src/domain.rs),
[principals](../../crates/plurx-core/src/playback_principal.rs),
[canonical UUID parsing](../../crates/plurx-core/src/sharing.rs),
and [candidate routes](../../crates/plurx-core/src/playback/candidate.rs).
Retain the private representation and checked parser of `SharingViewerKey`.
Core's row-to-value conversions remain beside their local row types.

Initial external dependencies are serde, uuid, thiserror and rusqlite. Keeping
the SQLite conversion with StoreError avoids an orphan-rule violation. Moving
only the referenced principal/parser/route leaves avoids pulling full engines
down into contracts. Core retains existing public type paths as reexports, so
the actual type identity is shared rather than duplicated or converted.

Checked operations preserve their argument/return types. Their current Core
visibility is `pub(crate)`. The new contracts crate intentionally exposes these
checked value operations across crates; Core's compatibility imports remain
`pub(crate)`, rather than broadening the old API:

```rust
impl Lease {
    pub fn publication_successor(&self) -> Result<Self, StoreError>;
}
pub fn validate_lease_identity(resource: &str, owner: &str) -> Result<(), StoreError>;
pub fn validate_lease_input(resource: &str, owner: &str, now: i64, expiry: i64)
    -> Result<(), StoreError>;
pub fn validate_lease_renewal(lease: &Lease, now: i64, expiry: i64)
    -> Result<(), StoreError>;
```

Lease is already a public value; validation does not mint serving, Source,
ingress, purpose or removal authority. P3a acceptance covers old/new type
identity, unchanged Serde wire values and canonical UUID refusal, bounds and
overflow/renewal behavior, and a downstream consumer with contracts alone.
The proof-bearing Core modules remain unchanged in this first slice. There is
no immediate daemon speedup claim: its consumers still depend on Core.

The contracts lease module owns MIN/MAX TTL, byte bounds, the private component
validator and the checked Unix-millisecond helper needed by publication.
Explicitly expose only checked helpers needed across this boundary, retaining
Core's old visibility through individual reexports. A narrow checked owner
validator lets StoreCoordinator validate its owner without copying validation
or inventing a synthetic resource. Coordinator's TTL-to-expiry orchestration
and removed-owner marker/key helpers stay in Core. Preserve current error text,
clock overflow behavior and all existing public constants/paths.

## 2.1 P3b: SDK-free packaging under the current private co-owner

P3a primitive extraction and P2c live assets/metadata can advance independently;
neither is a prerequisite for the SDK-free delivery path. P2b measurements and
the final before/after comparison retain their own acceptance obligations.

After the checked runtime and P1 measurement/source baseline, a bounded
packaging phase may make the actual cluster implementation optional while retaining all
proof representations, credentials, private Backend, trusted factories, Store
traits and generic SQL together in Core. Daemon composition and selected package
recipes stop unconditionally selecting Hiqlite/WAL. No authority crate, public
Backend or cross-crate witness factory is introduced by this phase.

The draft implements this packaging cut with native SQLite as the daemon's
default and an explicit `cluster` build feature. Actual cluster services are
absent in native composition; local Source/Receiver ownership uses the genuine
native factory and accepted-driver registry. The selected daemon/controller/
tray graph omits Hiqlite/WAL/OpenRaft, while explicit cluster builds retain the
actual SDK. This is graph evidence, not current runtime/release qualification.

The separate `plurx-storage-migrate` executable calls Core's existing private
converter. Managed selection is authenticated by optional signed V2 release
metadata, the accepted installed ledger and immutable generation hashes;
source installs use the explicit verified self-tuple CLI. Neither path
introduces a public Backend, a new authority crate or a credential/proof mint.
The [plan's evidence ledger](CINEMA-MODULAR-BUILD-PLAN.md#8-evidence-ledger--distinguish-implementation-from-acceptance)
records focused helper/staging/close passes on distinct snapshots. Native
Clippy, SDK compatibility, real FFmpeg HTTP/child-process and distribution
qualification remain pending on the integrated candidate.

Native Source and Receiver sharing remain supported. Receiver's existing
privately constructed relay-read authority plus actual owner/local-registry
checks admit its native `members=None` path; the uninhabited no-feature member
type is not evidence that native Receiver lacks custody. Preserve these real
checks and physical closure obligations. Do not cfg away sharing or replace
membership/replication with a successful synthetic provider.

Acceptance requires actual standalone and explicit-cluster composition tests:
no native cluster listeners/quorum tasks; actual Source/Receiver ingress and
closure; native restart/backup/restore; and real cluster membership, applied/
clock authority, quorum-loss fencing/recovery and never-auto-downgrade behavior.
Package-specific graphs run independently of workspace feature unification.
Complete distribution omission and supported upgrades still need the unchanged
P4 helper/root and packaging gates before shipping standalone becomes default.

P3b is a build-selection boundary, not permission to move trusted minting or
resource-binding ownership. Future identity/schema/authority/Store extraction
remains blocked by sections 3–4. In particular the existing MediaSessionStore
bridge used by Source publication and Receiver retirement remains part of the
full cross-crate gate; do not publicize Backend or duplicate SQL to bypass it.

## 3. Authority extraction: concrete resources, private minting

These proposed APIs describe required ownership, not an admitted implementation:

```rust
pub fn bind_native(
    lease: DirectoryLease,
    connection: &mut rusqlite::Connection,
    key: Arc<CredentialKey>,
) -> Result<NativeAuthoritySession, StoreError>;

impl NativeAuthoritySession {
    pub fn prepare_source(&self, connection: &mut rusqlite::Connection)
        -> Result<NativeSourceBoot, StoreError>;
    pub fn ingress_authority(&self, connection: &rusqlite::Connection,
                             registry_boot: Uuid)
        -> Result<IngressCustodyAuthority, StoreError>;
}
impl NativeSourceBoot {
    pub fn admission(&self, registry_boot: Uuid)
        -> Result<SourceAdmissionMembers, StoreError>;
    pub fn revoke(&self);
}
```

DirectoryLease has a private representation and actual exclusive ownership.
Binding derives the database/root identity itself. Every mint revalidates its
actual connection/root binding, canonical schema, persisted generation, master
and sealed key material. Source preparation performs the real boot transaction;
the unique boot owner controls shared revocation. A Ready flag, guard tuple,
caller fingerprint or caller-supplied SQL result never substitutes for proof.

The replicated owner must install its own SDK admission hook, capture actual
clock/metrics/local identity and bind a real Client exactly once before any
qualified observation. Observation retains the existing consistent query,
before/after membership comparison, freshness/transition/roster/master checks
and final guarded-query validation. Startup remains explicitly unqualified
until those steps complete.

The current startup hook contains Weak<ReplicatedMembership> and reconstructs
the full manager for reduction verification. It cannot move unchanged. Its
actual direct queries, peer-directory verification, original clock tickets and
private durable-row AppliedRemovalFence factory must move together. High-level
removal intent/orchestration stays in cluster. No public boolean callback or
caller-constructed removal fence replaces that engine.

Keep row-bound session/write/route/publication witnesses, Source ingress
permissions and Receiver relay-read authorities with their checked SQL
factories in Store API. Generic SQL blanket implementations stay with their
trait owner initially, or become shared functions plus backend-owned forwarding
implementations. No duplicate SQL or publicly unchecked witness constructors.

## 4. Unresolved authority entry obligations

Before moving a proof or factory, resolve these against the actual source:

1. Own CredentialKey plus its private verified decoder/signers and platform
   secure-open closure without depending on Core. Publicizing private helpers
   is not an extraction strategy.
2. Define native startup sequencing around uninitialized/new stores and
   manifest publication. A bind method cannot demand a manifest before its
   real owner can create it, or trust a caller assertion that creation happened.
3. Define a Client construction/binding API that proves the actual SDK hook
   installed on that Client. Proposed bind_local(Client) alone is insufficient
   unless the installed-hook identity is verified; prefer an owning factory
   if the SDK supplies no such observation.
4. Specify the extracted Receiver API and prove parity with the actual native
   relay-read/local-registry custody path. Its `members=None` branch already
   has real guards; the uninhabited replicated-member type alone proves neither
   absence nor acceptance of native Receiver capability.
5. Finish every trusted mint path's ownership/signature and dependency closure.
   Source, ingress, purpose and reduction proofs remain distinct; purpose-only
   evidence cannot authorize Source sessions.

Add an actual downstream compile-fail suite that rejects sealed trait impls,
proof construction/mutation/deserialization, raw-roster test constructors,
purpose-to-session substitution, snapshot/guard-to-proof conversion and imports
of the disabled replicated module. Pair it with real native revocation/stale
boot and actual membership/clock-loss refusals. Compile-fail fixtures alone do
not prove runtime verification. The feature-disabled authority graph must omit
Hiqlite/WAL/OpenRaft.

This decision permits no authority extraction while those obligations remain.
Only the enumerated primitive P3a slice and P3b packaging under the unchanged
private Core co-owner may precede that cross-crate extraction gate.

## 5. Astra review disposition

Astra admitted the enumerated primitive-only P3a slice. The separate reviewed
P3b packaging amendment retains private co-ownership; P3c remains blocked.
P3a remains limited to the enumerated public value/error/DTO operations, with
checked visibility and clock/constants/helper ownership. P3b adds only section
2.1's SDK-free packaging while the private Core co-owner remains unchanged.
The complete authority gate stays mandatory before moving a proof, trusted
factory, credential owner, SDK hook or resource-binding/backend-selection
authority across a crate boundary, regardless of task name. The canonical plan
carries the same narrow P3a/P3b scope. Contracts-only dependency checks run in
isolation from workspace feature unification. The later packaging/helper draft does not change these admission limits.
No cross-crate authority extraction or final runtime/release acceptance is
established by this source review.
