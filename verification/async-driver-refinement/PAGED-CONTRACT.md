# Paged exact proof — schema 6 boundary

**Status: M0 normative interfaces under design conformance review; implementation
and acceptance pending. Updated 2026-10-28.** This contract changes storage and
qualification scheduling for the exact selected product in [CONTRACT.md](CONTRACT.md).
It preserves D, all raw identities and times, all seven contexts, original
Next/fairness, actual production replay and backend obligations. It grants no
proof credit to a page, checkpoint, prefix, or unfinished session.

The [implementation ledger](../../docs/cluster/PAGED-ASYNC-PROOF-IMPLEMENTATION.md)
owns delivery and file ownership. [paged-proof.schema.json](paged-proof.schema.json)
defines the closed schema 6 envelopes; [paged-catalog.sql](paged-catalog.sql)
defines the separate harness catalog. Neither is a production Store migration.
The [finite inventory](finite-inventory.json) is a source-reviewed construction
argument awaiting independent conformance, not an executed closure certificate.

## Exact records and concrete Rust interfaces

The implementation lives in `crates/plurx-core/src/store/async_driver_catalog.rs`
with `records.rs`, `sqlite.rs` and `memory.rs` in its same-named subdirectory.
These paths and signatures are normative interfaces to implement after M0;
there are no linked placeholder modules or runtime stubs in M0. SQLite is the
persistent implementation; the bounded memory implementation supports small
backend-equivalence fixtures. Both implement the same record/transaction API.

```rust
pub struct PhysicalRowsId(pub u64);
pub struct EpisodeId(pub u64);
pub struct StoreId(pub u64);
pub struct ProductId(pub u64);
pub struct OracleEdgeId(pub u64);
pub struct ExpansionId(pub u64);
pub struct FifoTicket(pub u64);
pub struct Sha256(pub [u8; 32]);
pub struct ExactUtf8(Vec<u8>); // private; validated UTF-8, exact existing bytes
pub struct PhysicalRows { pub id: PhysicalRowsId, pub canonical: ExactUtf8 }
pub struct RawEpisode { pub id: EpisodeId, pub canonical: ExactUtf8 }
pub struct StoreRecord {
    pub id: StoreId, pub physical: PhysicalRowsId, pub episode: EpisodeId,
    pub canonical: ExactUtf8, pub certification: StoreCertification,
    pub replay_parent: Option<OracleEdgeId>, pub boundary: BoundaryId,
}
pub struct ProductKey { pub context: Context, pub canonical: ExactUtf8 }
pub struct ProductRecord { pub id: ProductId, pub key: ProductKey,
    pub store: StoreId, pub predecessor: Option<ProductEdgeId> }
pub struct AppliedEvidence {
    pub from: StoreId, pub to: StoreId, pub action: ExactUtf8,
    pub result: String, pub resolved_token: Option<usize>,
    pub claim_snapshot: Option<ExactUtf8>, pub error: Option<String>,
    pub claim_request: Option<ExactUtf8>, pub job_read: Option<ExactUtf8>,
}
pub enum Classification {
    Eligible, ExcludedDomain { reason: String }, ExcludedSample { reason: String },
}
pub enum DemandRole { Apply, Capacity, DependentResolve, DependentRead, DependentCancel }
pub enum StoreCertification { ActualInitChecked, NovelActualStateChecked }
pub enum CatalogError {
    CorruptRecord, SourceMismatch, InitialImageMismatch, SchemaMismatch,
    MissingDependency, IncompleteExpansion, ConflictingOutcome, PoisonedOracle,
    NumericDomain, ResourceIncomplete, Io,
}
pub type CatalogResult<T> = Result<T, CatalogError>;
```

```rust
pub enum RecordKey { Numeric(u64), Text(String) }
pub struct RecordRange {
    pub kind: RecordKind, pub after: Option<RecordKey>,
    pub through_transaction: TransactionId,
}
pub struct ReadLimits {
    pub max_records: std::num::NonZeroUsize,
    pub max_encoded_bytes: std::num::NonZeroU64,
}
pub struct RecordBatch {
    pub records: Vec<RecordEnvelope>, pub encoded_bytes: u64,
    pub next_after: Option<RecordKey>,
}
pub struct CommittedRange {
    pub kind: RecordKind, pub first: RecordKey, pub last: RecordKey,
    pub transaction_high_water: TransactionId,
}
pub struct PageLimits {
    pub max_records: std::num::NonZeroUsize,
    pub max_member_bytes: std::num::NonZeroU64,
    pub max_manifest_node_bytes: std::num::NonZeroU64,
}
pub struct CommittedExpansion {
    pub id: ExpansionId, pub transaction: TransactionId,
    pub discovered: Vec<ProductId>,
}
```

`RecordKind` and `RecordEnvelope` are the schema's closed kind enum and matching
payload sum type. `PageDescriptor`/`InitialImageRecord` use the complete Page/
InitialImage field sets. Numeric keys sort numerically, text keys by exact UTF-8
bytes; a record kind has the one key form fixed below. `RecordRange.after` is
exclusive. `next_after` is the last returned key when more committed records
remain, otherwise null. `CommittedRange.first/last` are inclusive actual keys,
not dense integer assumptions; composite keys stay exact canonical strings.
SQL page key BLOBs encode the same canonical JSON key values. Byte ceilings
include envelope/dependency overhead and may never exceed the fixed member
limit. Vectors above are bounded by their read or one-predecessor limits; they
are not a retained global frontier or graph.

`BoundaryId`, `ProductEdgeId`, `CandidateId`, `DemandId`, `MonitorId`,
`ComponentId` and `TransactionId` are distinct positive global-ID newtypes;
`Context` is exactly the seven schema values. Checked conversion to signed
SQLite IDs must refuse exhaustion, never wrap or recycle. All records use the
schema's full payload, including fields omitted from the short signatures.
`CandidateRecord`, `DemandRecord`, `OracleEvidenceRecord`, `ProductEdgeRecord`,
`FairPairRecord`, `EnablednessRecord`, `ExpansionSeal`, `CheckpointRecord`,
`SourceIdentity`, `ResourcePlan`, `WorkUnit`, `MonitorNode`, `MonitorEdge`,
`SccCertificate`, `Counterexample` and `Outcome` have exactly their schema 6
field sets; additional private cache fields cannot affect equality or wire data.

```rust
pub trait Catalog {
    type Expansion<'a>: ExpansionTransaction where Self: 'a;
    fn initialize(&mut self, value: CheckedInitialState) -> CatalogResult<InitialImageRecord>;
    fn get_store(&self, id: StoreId) -> CatalogResult<StoreRecord>;
    fn get_product(&self, id: ProductId) -> CatalogResult<ProductRecord>;
    fn get_oracle_edge(&self, id: OracleEdgeId) -> CatalogResult<OracleEvidenceRecord>;
    fn oldest_pending(&self) -> CatalogResult<Option<(FifoTicket, ProductId)>>;
    fn begin_expansion(&mut self, ticket: FifoTicket, product: ProductId)
        -> CatalogResult<Self::Expansion<'_>>;
    fn checkpoint(&mut self, unit: &WorkUnit) -> CatalogResult<CheckpointRecord>;
    fn resume(&mut self, checkpoint: &CheckpointRecord, source: &SourceIdentity,
        plan: &ResourcePlan) -> CatalogResult<()>;
    fn read_records(&self, range: RecordRange, limits: ReadLimits)
        -> CatalogResult<RecordBatch>;
    fn export_page(&self, range: CommittedRange, limits: PageLimits)
        -> CatalogResult<PageDescriptor>;
}
pub trait ExpansionTransaction {
    fn intern_physical(&mut self, value: ExactUtf8) -> CatalogResult<PhysicalRowsId>;
    fn intern_episode(&mut self, value: ExactUtf8) -> CatalogResult<EpisodeId>;
    fn intern_checked_store(&mut self, record: CheckedStore) -> CatalogResult<StoreId>;
    fn intern_product(&mut self, key: ProductKey, store: StoreId) -> CatalogResult<ProductId>;
    fn append_candidate(&mut self, value: NewCandidate) -> CatalogResult<CandidateId>;
    fn append_demand(&mut self, value: NewDemand) -> CatalogResult<DemandId>;
    fn append_oracle_evidence(&mut self, value: NewOracleEvidence) -> CatalogResult<OracleEdgeId>;
    fn append_product_edge(&mut self, value: NewProductEdge) -> CatalogResult<ProductEdgeId>;
    fn union_fair_pair(&mut self, value: FairPairRecord) -> CatalogResult<()>;
    fn seal(self, value: ExpansionSeal) -> CatalogResult<CommittedExpansion>;
    fn abort(self) -> CatalogResult<()>;
}
```

`NewCandidate`, `NewDemand`, `NewOracleEvidence` and `NewProductEdge` have
exactly their corresponding schema record fields except generated `id`; the
transaction alone allocates that field. `CheckedStore` likewise carries the
checked Store payload before generated ID assignment. No caller selects or
reuses a committed global ID.

`initialize` atomically persists the actual checked Init image, rows/Episode and
seven ordered initial context/product/frontier records, once only. Later
interning occurs solely inside an expansion transaction; returned provisional
IDs stay private until commit and become invalid on rollback.
`CheckedStore` is constructible only by the actual oracle after initial/novel
state checks; loading an interned record cannot manufacture it. `ReadLimits`
and `PageLimits` carry nonzero byte/record ceilings. A batch returns records,
exact consumed bytes and an exclusive resume-after cursor, never a global Vec.
`CommittedRange` ends at a committed transaction high-water mark. Physical rows,
Episodes, boundaries and oracle evidence can be read through the same typed
record-batch operation; callers check envelope kind and ID. `ExactUtf8` preserves
the current Rust canonical serialization, rather than sorting/reencoding its
inner keys. Digests index candidates; full bytes determine equality.

## Eight frozen boundaries

| Boundary | Required invariant and failure behavior |
|---|---|
| Immutable identity store | Losslessly intern the exact nine-table PhysicalRows and full raw Episode. Reconstruct exact existing `{rows,episode}` canonical bytes. Every pair remains a logical Store, including ClockAdvance. Product keys retain complete raw Valuation/context. No in-place canonical mutation. |
| Oracle evidence store | Preserve all Applied fields above, input, actual production witness, source/target, predecessor replay and before/after/rollback boundary observations. Capacity-only targets count in the global Store union. No fabricated transition or typed-row replacement. |
| Expansion transaction | One predecessor's independently enumerated candidates, classifications, apply/capacity/dependent resolve/read/cancel demands, outcomes, successors, pair-label unions, enabledness and discoveries commit atomically. Any failure rolls everything back and leaves the predecessor pending. |
| Frontier/checkpoint | One writer, deterministic FIFO tickets; seven initial contexts in source order, successors in source enumeration order. Checkpoint only between complete committed expansions. Restore exact source, raw Init, schema, plan and checkpoint; persisted records are never trusted live SQLite endpoints. |
| Page export/read | Stream immutable complete committed ranges with exact hashes/counts and complete dependencies. Page membership never changes D, equality, Next or enabledness. Missing, duplicate, conflicting or unsafe dependencies invalidate the entire session. |
| Semantic verifier | Independently reconstruct global Init reachability, D/classifications, every candidate/demand/dependent outcome/successor, pair unions, enabledness and numeric obligations. Common codecs are allowed; calling Rust successors to decide completeness is not. |
| Temporal layer | Verify source-bound history monitors, full-graph nonstuttering enabledness/occurrence, complete SCC partition and original-Init prefix/cycle witnesses. It cannot redefine original actions or use component-local enabledness. |
| Session controller | Admit one immutable total resource plan; authenticate ordered registered work units/checkpoints. Only complete semantic/temporal/backend coverage plus exact-head Main aggregation yields acceptance. Resource exhaustion remains incomplete. |

The existing actual materializer is retained: production replay and first/novel
logical-state independent queries still run even when PhysicalRows is already
interned. Every actual operation retains all-Applied comparisons, refused-edge
full-preimage equality, logical-resolution read-only equality, omitted extension
rows/markers/sequences, real post-rollback reads and poison-on-failure behavior.
There is no general physical-ID materialization shortcut. The initial migrated
SQLite image, raw generated UUIDs/settings, source/schema and observations are
saved once, hashed and rechecked after restore; another fresh seed is invalid.

An expansion can exceed a work unit's remaining budget. Abort it without
publication, checkpoint the preceding committed boundary, and continue only
within the immutable plan. If a complete indivisible predecessor cannot fit
its admitted per-unit limits, fail incomplete; do not partially seal it. A
capacity-only successor is interned/certified even if no product edge targets it.
A globally empty frontier across all seven contexts is necessary for closure.

## Pages, dependencies and canonical hashes

Every envelope has `schema_version:6`, one closed `kind`, and its closed
`record`. A data page embeds complete homogeneous `DataEnvelope` records, not
merely pointers; its dependencies identify external referenced records. Indexes
map full `(kind,id)` keys to authenticated page/member offsets. Reference keys
are exact and kind-dependent:

| Record kind | `Ref.id` |
|---|---|
| PhysicalRows, Episode, Store, Product, OracleEvidence, Candidate, Demand, ProductEdge, Expansion, MonitorNode, MonitorEdge, Scc, BoundaryObservation | Positive record `id` |
| FairPair | Canonical compact JSON `[context,from_product,to_product]` string |
| Enabledness | Canonical compact JSON `[context,product_id]` string |
| Applied | Transport envelope SHA; full bytes checked (normally nested in OracleEvidence) |
| Counts | Literal `global` |
| ContextRoot | Exact context string |
| InitialImage, Checkpoint, Page, ManifestNode, ResourcePlan | Image/checkpoint/page/manifest/plan content SHA respectively |
| Session, Completion | Exact session UUID |
| WorkUnit | Unit ordinal, including zero |
| Counterexample, Outcome | Exact family string (one complete family outcome/witness) |
| ArtifactReceipt | Actual positive artifact ID |
| Blob/source/tool/runtime/backend/causal/numeric/eligibility/Main receipt | Exact logical member name bound by the authenticated manifest |

Readers enforce this key table and exact referenced record content; the union
field does not authorize ambiguous aliases. Multiple page copies of one key
must contain identical full bytes. The record-key map is external to payloads
without an `id`; transport hashes index full bytes and never authorize equality
by digest alone.

Exact inner canonical UTF-8 strings are opaque preserved bytes; decoded semantics
and byte counts are independently checked. Outer canonical JSON uses UTF-8,
sorted object keys, compact separators, no NaN and no string normalization.
PhysicalRows/Episode/Store/Product `sha256` fields hash their exact inner
canonical bytes. A `Ref.sha256` hashes the complete canonical target envelope;
only content-addressed self fields are removed: `Page.page_id`,
`ManifestNode.sha256`, `ResourcePlan.plan_sha256` respectively. Other identity
hashes stay included. A checkpoint's digest hashes its complete envelope (no
self-digest field). `InitialImage.image_sha256` hashes actual SQLite file bytes.
Page/ManifestNode `utf8_bytes` counts their canonical envelope excluding its own
`utf8_bytes` and content-addressed self field; readers also enforce the limit
against the full actual encoded member, including envelope overhead. PhysicalRows
and Episode `utf8_bytes` count their exact inner UTF-8 bytes. No self-referential
hash or size calculation is required.

A page references only already committed immutable records and its **previous**
checkpoint. Its descriptor hashes the page bytes and ordered dependency list.
The new checkpoint references the newly exported manifest root; pages never
reference that same new checkpoint, preventing a hash cycle. Manifest trees
have at most 256 children per node, bounded node bytes and exact transitive
counts. Fanout and page bounds limit transport, never global graph size.

All readers reject absolute paths, `.`/`..` components, traversal, duplicate
members, symlinks, conflicting IDs, unreferenced hidden payloads and references
outside the authenticated session/source closure. ZIP and extracted size/member
limits apply before loading; every dependency is authenticated and transitively
available, not merely named. `ArtifactReceipt` binds actual API IDs and upload
logs to exact members/checkpoint roots. Counts are recomputed from unique global
IDs, never summed across overlapping page copies. Full canonical collision
comparison is mandatory in catalog insertion and independent verification.

## Finite source domain and admitted resource plan

The source-backed [inventory](finite-inventory.json) accounts for every row
writer and repeating/transient path. Proposed source envelopes are `Bw=61`,
`Bm=984`, visited wall `1000..44641000`, monotonic `0..29550985`; they are
consequences to validate, **never guards in Next**. Raw initial/fixed-offset
values outside a visited-clock interval remain unchanged. Refused renewal,
repeated AdapterSample, bind/release cycles, availability toggles, claim-loss
keys, delayed recovery and all mutation branches must satisfy the ledger.
Any counterpath invalidates the finite certificate and session before dispatch.

The new target retires the old **global** 20,000 Store cap. It permits at most
20,000 resident cached Store entries plus separately admitted resident-byte
and RSS ceilings; every global Store remains persisted and counted. Current
schema 5 runtime still has its old global cap until M1/M2 replace it.

Fixed limits are 256 MiB per page/SourceTables member, 512 MiB per retained or
extracted artifact, 300 members per artifact, 1,800 seconds per work unit,
4 GiB TLC heap and one TLC worker. The unadmitted
[plan example](paged-resource-plan.example.json) deliberately supplies no runner
quota. Root must measure owned runner disk and bind immutable total session
disk, total retained artifact bytes, maximum units, resident bytes/RSS and
per-unit predecessor/record/byte allowances before dispatch.

Admission computes source-derived per-record encoded-width bounds `B_k` from
field/row vocabularies and measures index/WAL/scratch/manifest overhead and live
artifact retention. For operational record allowances `n_k`, require
`sum(n_k*B_k)+index+WAL+scratch+retained_artifacts <= admitted_disk` and
`retained_artifacts <= admitted_artifact_total`. These allowances bound resource
use, never the accepted graph: exceeding one leaves the session incomplete.
Separately report the finite worst-case cardinality expression `sum(N_k*B_k)`
for all seven contexts, capacity-only Stores, products, demands, edges and
monitors; do not claim that astronomical bound fits the operational quota.
All sums/products use checked arbitrary-precision metadata arithmetic. Unit
estimates use admitted record/byte/predecessor allowances plus indivisible
expansion/component sizes; measured throughput may refine an estimate but
cannot prove completeness or justify an invented quota. Missing per-record
bounds/measurements block admission; inability to fit an indivisible record,
expansion or component remains an explicit incomplete result.

`ResourcePlan(admitted)` has no nullable operational fields, includes source,
finite-inventory/schema/toolchain/workflow identities, disk measurement receipt
and plan digest. A checkpoint cannot change that plan. Exhausting disk, RSS,
units, bytes, time, ID space or numeric support fails incomplete. More units
require a newly reviewed admitted session, never a silent extension or pruning.

## Temporal records and eleven outcome families

[paged-outcomes.json](paged-outcomes.json) pins the eleven semantic families to
existing configurations and original formulas. Several component TLC processes
may establish one family; eleven process invocations are not the new contract.
All seven complete contexts and all safety/coverage/fairness obligations remain.
One-step tables wrap original actions; original Next receives no page guard.

History monitors initialize from actual Init: `seen_goal=goal(Init)`,
`pending=P(Init) /\\ ~Q(Init)`, `capacity_ok=WithinEpisodeCapacity(Init)`.
Transitions use `seen_goal'=seen_goal \\/ goal(next)`,
`pending'=(pending \\/ P(next)) /\\ ~Q(next)` and
`capacity_ok'=capacity_ok /\\ WithinEpisodeCapacity(next)`. Completion searches
reachable suffixes with `seen_goal=false`; cleanup searches suffixes permanently
`pending=true`, with `capacity_ok=true` for conditional cleanup. Recurring
pending alone is insufficient; completion goal stability is not assumed.

Each SCC certificate covers every monitor node exactly once, has actual-edge
forward and reverse rooted arborescences, and a strictly increasing rank on
cross-component edges. The independent checker verifies all original edges,
including outgoing dependencies, and exact partition maximality. SCCs are
indivisible; an oversized SCC fails incomplete. Fairness is evaluated from
**full original graph** enabledness and original `<A>_vars` occurrences:
`WF = GF(~En) \\/ GF(Occ)`; `SF = FG(~En) \\/ GF(Occ)`. An original state-selfloop
is non-occurring even if labeled. Implicit original TLA stutter remains available,
including at a reachable deadlock; no past-action flag counts during stutter.

Counterexample.prefix and cycle contain ordered MonitorEdge IDs projected to
actual original edges. A nonempty cycle has stutter_at=null; an empty cycle
requires stutter_at equal to its reachable terminal monitor node and the original
implicit-stutter fairness checks. Finite invariant/nonvacuity and sparse boundary
control traces use source-bound causal/TLC receipt members instead of fabricating
a temporal component. Fair counterexamples carry the actual original-Init prefix, monitor transitions,
and an actual fair recurrent walk/cycle (or stutter witness), with action
occurrences and disabledness witnesses rechecked independently. A representative
cycle without the required fair evidence cannot stand for a whole SCC. Required
small comparison fixtures cover outgoing-only enabling, deadlock stutter,
non-occurring selfloop labels, goal already seen in prefix, pending across a
component boundary and capacity false in prefix. Their definitions and execution
come after M0 and final independent source review.

## Session and completion protocol

`Session` phases and `WorkUnit` statuses are closed schema enums. The sequence is
admitted → running → checkpointed_incomplete (repeat) → closed_unqualified →
qualified → complete; resource_blocked, semantic_invalid and completion_rejected
are explicit nonacceptance states. A failed work unit never advances a checkpoint.
Each unit has stage, a closed ProgressCursor (context, record kind/next ID, FIFO
ticket, component ID, family and case index; irrelevant slots null), registered
cases, ordinal, expected prior
checkpoint, source/Init/plan identity, authenticated run/job/task and attempt 1.
Stages are expansion, semantic verification, monitor/SCC verification, temporal
components, backend qualification and final completion as named in the schema.

The only new entry is the separately registered `async-proof-session.yml`
`workflow_dispatch` protocol with modes `unit` and `complete`. The trusted
controller validates repository, PR/open/ready state as applicable, raw event
base, exact head/tree/source closure, workflow digest, runner/toolchain,
session/plan, ordered predecessor checkpoint, stage/cases and fresh actual
run/job/task attempt 1. Source and Oracle never receive API credentials.
Existing ordinary positive/current gates keep their strict attempt-1 authority;
no compatibility adapter or exception accepts a partial session.

The registered successor alone may resume a checkpoint, after checking catalog
logical digest, full dependency closure, actual Init image and exact sources.
Completion mode reauthenticates the current ready PR/head/base, all ordered
unit artifacts/receipts, all eleven families, all seven contexts, global closure,
existing Rust/backend/source registries and current Main requirements. It emits
one complete root consumed by exact-head Main aggregation. An incomplete old
PR job is never relabeled success; no dummy source commit continues a session.
