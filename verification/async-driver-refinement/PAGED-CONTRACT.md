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
argument with source-only finite-generation design conformance; the amended
schema/API still awaits design recheck. It is not an executed closure certificate.

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
    NumericDomain, ResourceIncomplete, SnapshotUnavailable, SnapshotMismatch, Io,
}
pub type CatalogResult<T> = Result<T, CatalogError>;
```

```rust
pub enum RecordKey { Numeric(u64), Text(String) }
pub struct Obligation { pub node: u8, pub job: u8, pub claim_id: uuid::Uuid }
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
    type Snapshot<'a>: CatalogReadSnapshot where Self: 'a;
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
    fn current_transaction(&self) -> CatalogResult<TransactionId>;
    fn pin_read_snapshot(&self, expected: TransactionId)
        -> CatalogResult<Self::Snapshot<'_>>;
}
pub trait CatalogReadSnapshot {
    fn transaction(&self) -> TransactionId;
    fn logical_digest(&self) -> CatalogResult<Sha256>;
    fn counts(&self) -> CatalogResult<Counts>;
    fn context_snapshots(&self) -> CatalogResult<Vec<ContextRoot>>;
    fn read_records(&self, range: RecordRange, limits: ReadLimits)
        -> CatalogResult<RecordBatch>;
    fn export_page(&self, range: CommittedRange, limits: PageLimits)
        -> CatalogResult<PageDescriptor>;
    fn close(self) -> CatalogResult<()>;
}
pub trait ExpansionTransaction {
    fn intern_physical(&mut self, value: ExactUtf8) -> CatalogResult<PhysicalRowsId>;
    fn intern_episode(&mut self, value: ExactUtf8) -> CatalogResult<EpisodeId>;
    fn intern_checked_store(&mut self, record: CheckedStore) -> CatalogResult<StoreId>;
    fn intern_product(&mut self, key: ProductKey, store: StoreId) -> CatalogResult<ProductId>;
    fn append_checked_boundary(&mut self, value: CheckedBoundary) -> CatalogResult<BoundaryId>;
    fn append_enabledness(&mut self, value: EnablednessRecord) -> CatalogResult<()>;
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
transaction alone allocates that field. `CheckedStore` carries the checked Store payload excluding catalog-assigned
`id` and `replay_parent_edge`; `CheckedBoundary` carries the complete
BoundaryObservation excluding its assigned `id`. Only the actual oracle can
construct either checked input. Enabledness appends its complete context/product
and full-graph label value, returning success only for one exact consistent
record. No caller selects or reuses a committed global ID.

`initialize` atomically persists the actual checked Init image, rows/Episode and
seven ordered initial context/product/frontier records, once only. Later
interning occurs solely inside an expansion transaction; returned provisional
IDs stay private until commit and become invalid on rollback.
New noninitial Store/Product identity slots may have unbound creating/discovery
links **only while uncommitted**. `append_oracle_evidence` assigns its newly
allocated edge as the first creating edge of a newly interned target Store;
`append_product_edge` assigns the first discovery edge of a newly interned
target Product. Later edges targeting the same provisional identity preserve
that first link. Existing committed links never change. Seal validates exact
source/target/context and actual witness agreement and that every new noninitial
Store/Product has a rooted creating/discovery chain from its initialized root;
missing, cyclic-unrooted or inconsistent links abort the whole transaction.
Only initialization creates linkless roots. Deferred SQL foreign keys permit
these private assignments before commit; they grant no mutation of committed
records. No provisional slot may be read by an external snapshot or exported.
All actual before/after/rollback checked observations are appended and linked
before the evidence can seal; their presence never substitutes for real reads.

`CheckedStore` is constructible only by the actual oracle after initial/novel
state checks; loading an interned record cannot manufacture it. `ReadLimits`
and `PageLimits` carry nonzero byte/record ceilings. A batch returns records,
exact consumed bytes and an exclusive resume-after cursor, never a global Vec.
`CommittedRange` ends at a committed transaction high-water mark. Physical rows,
Episodes, boundaries and oracle evidence can be read through the same typed
record-batch operation; callers check envelope kind and ID. `ExactUtf8` preserves
the current Rust canonical serialization, rather than sorting/reencoding its
inner keys. Digests index candidates; full bytes determine equality.

## Commit sequence and pinned read lifecycle

`TransactionId` is the monotone **catalog commit sequence**, not a per-table ID.
Initialization commits sequence1. Every atomic source expansion or temporal
catalog write appends `current+1` to `catalog_commits` and updates
`catalog_meta.current_commit` in that same transaction. Rollback advances neither.
One phase-exclusive writer owns this rule across Rust source and Python temporal
writers; no temporal phase invents a separate sequence. Controller/page bookkeeping
is outside semantic-record equality, logical digest and counts, and cannot mutate
committed semantic records. Actual production Store savepoint/writer state remains
a different boundary entirely.

`pin_read_snapshot(expected)` opens the catalog's read transaction and reads the
commit sequence as its first query. It succeeds only for that exact currently
available sequence; an older/newer/unavailable request returns SnapshotUnavailable
or SnapshotMismatch. The first version pauses semantic writers for the entire
read-snapshot lifetime. All batches/exports share that one snapshot and ranges
must name its exact high-water. Snapshot counts/context views and logical digest
refer to those same rows; no later committed record can leak into a batch. Direct
Catalog get operations serve the owning writer's current replay; verifier/export
consumers use only the pinned snapshot interface.

`close(self)` consumes the read handle, ends its read transaction and releases
the writer pause. Drop/error also closes and publishes no checkpoint/page.
A released handle cannot resume a cursor. A reader reopens a cursor only after
successfully pinning its original exact high-water, otherwise it must use an
explicitly restored exact checkpoint catalog and revalidate it. This is not
arbitrary historical-version reconstruction or permission to promote a stale
cursor to the current database. `ProgressCursor.after_key` uses RecordKey and
is exclusive exactly like RecordRange.after; null means the typed start of that
stage/range. Stage/record kind/context/family and case index determine whether
that cursor is at a stage boundary; an unrelated null cannot reset progress.

Checkpoint creation occurs with no expansion active: internally pin the current
sequence, export committed pages and compute inline counts/context snapshots and
logical digest at that same snapshot, close it, then record the authenticated
checkpoint/controller bookkeeping. No semantic writer interleaves during that
operation. Resume requires the exact persisted/restored catalog, sequence, digest,
source/Init/plan and page closure; it never infers a snapshot from table IDs.

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

Counts, ContextRoot and Outcome retain these singleton keys but are **final-only
immutable data-page records**. Counts/ContextRoot publish only after their relevant
complete final counts are frozen; each Outcome publishes only once its semantic
family result is sealed. `final_records` stores those exact immutable payloads.
A checkpoint's changing counts/context snapshots remain inline in Checkpoint,
not DataEnvelope publication. Pending/incomplete family tracking stays in controller
progress and cannot reuse an Outcome family key in a page. A verifier checks this
publication phase and rejects conflicting or premature singleton copies. Future
checkpoint/page fixtures must exercise this distinction; no fixture ran in M0.

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

`SourceIdentity.proof_contract_sha256` hashes the canonical sorted array of
`[repository_relative_path, sha256(exact_file_bytes)]` for PAGED-CONTRACT.md,
paged-proof.schema.json, paged-catalog.sql, finite-inventory.json,
paged-outcomes.json, paged-catalog-transport.schema.json,
paged-verifier-continuation.schema.json, paged-verifier-state.sql,
source-bound-formulas.json, source-bound-receipt.schema.json and
paged-control-carrier.schema.json under this directory. It binds the proof-record contract,
not the production database. `Store.sqlite_schema_sha256` instead hashes the
canonical sorted actual migrated SQLite schema inventory (installed schema objects
and migration identities); `InitialImage.schema_inventory` references that exact
inventory. Neither may substitute for the other. Initial image bytes have their
separate image digest. The coordinator must rebind finite/source inventory hashes
to actual final Rust/Python/TLA sources after implementation; the7431 finite-ledger
source review remains historical design advice, not a permanent acceptance pin.

A page references only already committed immutable records and its **previous**
checkpoint. Its descriptor hashes the page bytes and ordered dependency list.
The new checkpoint references the newly exported manifest root; pages never
reference that same new checkpoint, preventing a hash cycle. Manifest trees
have at most 256 children per node, bounded node bytes and exact transitive
counts. Fanout and page bounds limit transport, never global graph size.

All readers reject absolute paths, `.`/`..` components, traversal, duplicate
members, symlinks, conflicting IDs, unreferenced hidden payloads and references
outside the authenticated session/source closure. Every path component starts
with a safe alphanumeric/underscore/hyphen and may then contain dots; root
`driver-graph.json`, `initial-image.sqlite` and `catalog.sqlite` are valid shapes.
Dot/dotdot components and all aliases remain rejected semantically. ZIP and extracted size/member
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
guarded AdapterSample, bind/release cycles, availability toggles, claim-loss
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
artifact retention. For operational record count/byte allocations `n_k,b_k`, require
`sum(n_k*b_k)+index+WAL+scratch+retained+duplicate_backups <= admitted_disk` and
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

`Scc.obligation` is null for seen_goal_false; pending_true and
capacity_ok_pending_true require one closed exact `(node,job,claim_id)` Obligation.
Its full view key is canonical `[context,property,obligation]`, never just a hash.
For that fixed triple, the induced graph contains precisely all globally reachable
monitor nodes whose pending_attempts includes it (and capacity_ok where required),
with every original monitor edge whose endpoints lie in the set. The completion
view instead contains reachable seen_goal=false nodes. Each view has its own
complete maximal SCC partition: a node pending two obligations can belong to both
partitions without conflicting membership keys. The checker regenerates and checks
**every relevant obligation view**, not a supplied subset; identity cannot switch
around a cycle. Monitor pending triples are unique and sorted by node/job/claim ID.

Within each view, each SCC certificate covers its members exactly once, has
actual-edge forward and reverse rooted arborescences and strictly increasing rank
for every cross-component induced edge. SQL component_members is keyed by full
view_key plus monitor_id and binds the matching component/view. Counterexample's
component reference binds this same fixed obligation. All original outgoing edges
leaving a view/component still contribute to global enabledness/fairness; they
are not artificial boundary sinks. Partition maximality and dependencies are
independently verified over the selected induced graph. SCCs are
indivisible; an oversized SCC fails incomplete. Fairness is evaluated from
**full original graph** enabledness and original `<A>_vars` occurrences:
`WF = GF(~En) \\/ GF(Occ)`; `SF = FG(~En) \\/ GF(Occ)`. An original state-selfloop
is non-occurring even if labeled. Implicit original TLA stutter remains available,
including at a reachable deadlock; no past-action flag counts during stutter.

Counterexample.prefix and cycle contain ordered MonitorEdge IDs projected to
actual original edges. A nonempty cycle has stutter_at=null; an empty cycle
requires stutter_at equal to the endpoint of its valid original-Init prefix and
the original implicit-stutter fairness checks. That endpoint may have outgoing
nonfair edges; stutter is not restricted to graph deadlocks. Full original
fairness determines whether the infinite stutter is a valid witness. Finite invariant/nonvacuity and sparse boundary
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
Each unit has stage, a closed ProgressCursor (context, record kind/exclusive after_key, FIFO
ticket, component ID, family and case index; irrelevant slots null), registered
cases, ordinal, expected prior
checkpoint, source/Init/plan identity, authenticated run/job/task and attempt 1.
Stages are expansion, semantic verification, monitor/SCC verification, temporal
components, backend qualification and final completion as named in the schema.

The proposed separately registered entry is `async-proof-session.yml`
`workflow_dispatch` with modes `unit` and `complete`. **M4 admission prerequisite:**
establish on the actual Forgejo service that the exact candidate workflow/ref
can be dispatched, or bind this same strict new session protocol to one already
dispatchable workflow path as permitted by the sealed architecture. M0 performs
no network probe and asserts neither route works. An unqualified bootstrap merge
is forbidden. M1/M3 consume the fixed workflow/source identity fields without
assuming candidate-only dispatch capability.

The selected producer path, event, modes, exact workflow bytes, candidate source
and source-archive execution must all be pinned in one exact registration. If
M4 needs the existing-dispatchable-path route, it hands that exact registration
and WorkUnit.workflow_id constant to the sole schema owner **before protocol
code freezes**. No generic workflow allowlist, ordinary attempt waiver, dummy
commit or manual Main bypass is allowed. The schema's current constant names the
proposed target, not an authenticated installed-service capability.

The trusted controller validates repository, PR/open/ready state as applicable,
raw event base, exact head/tree/source closure, workflow digest, runner/toolchain,
session/plan, ordered predecessor checkpoint, stage/cases and fresh actual
run/job/task attempt1. Source and Oracle never receive API credentials. Existing
ordinary positive/current gates keep strict attempt1 authority; no compatibility
adapter or exception accepts a partial session.

The registered successor alone may resume a checkpoint, after checking catalog
logical digest, full dependency closure, actual Init image and exact sources.
Completion mode reauthenticates the current ready PR/head/base, all ordered
unit artifacts/receipts, all eleven families, all seven contexts, global closure,
existing Rust/backend/source registries and current Main requirements. It emits
one complete root consumed by exact-head Main aggregation. An incomplete old
PR job is never relabeled success; no dummy source commit continues a session.


## Catalog backup transport adjunct

The closed auxiliary [catalog transport schema](paged-catalog-transport.schema.json)
uses schema_version 1 and exactly `catalog_backup` and `catalog_chunk_index`
kinds. It changes neither schema6 semantic records nor the eight interfaces.
Its bytes are included in the explicit proof_contract_sha256 preimage above.

M1 takes a SQLite backup API snapshot in bounded page steps, with all source,
temporal and bookkeeping writers paused, after the produced Checkpoint is
recorded and before upload receipts exist (`post_checkpoint_pre_upload`).
The standalone backup includes committed WAL contents, private IDs, FIFO,
allocator state and indexes; copying live database/WAL files or reconstructing
private catalog state from semantic pages is not permitted. The checkpoint
never references its backup. The fixed `catalog-backup/root.json` in part0
references that already fixed checkpoint, source, raw Init, plan, semantic
manifest/high-water/logical digest and complete backup byte digest.

M4 streams the entire backup into chunks of exactly 256 MiB except the final
positive remainder. Closed index nodes contain at most 256 children and at most
8 MiB canonical metadata. Each node covers consecutive ordinal and byte
intervals exactly; higher nodes reference children exactly one level lower.
The root covers every chunk and byte, with no gap, overlap, duplicate, cycle,
alias or conflicting member. A disk index bounds descriptor memory. Chunk
paths and part ordinals are authenticated by all current-unit ArtifactReceipt
members; no new Ref kind, WorkUnit field or future ZIP self-reference is added.
Every expected part is required before the checkpoint is resumable.

The registered workflow contains a finite pinned sequence of runner-native
`uses: upload-artifact` slots in the same producer job/task. Each used slot
has one deterministic name `async-proof-<session>-<unit>-part-<ordinal>` and
validated member inventory. Actual part count must fit the registered slot
count before any upload. No direct action-bundle execution, separate uploader
task, dynamic matrix, partial-part acceptance or runtime slot increase is
authorized. Each member is at most 256 MiB (metadata at most 8 MiB); every
part independently respects 300 members and 512 MiB extracted/retained ZIP
limits, including ZIP overhead. The actual registered workflow/ref/native-slot
capability must be established before admission; the declaration proves none.

Restore authenticates all part receipts and the complete index, then streams
consecutive chunks into a fresh owned staging file, verifying per-chunk and
whole-file bytes/hash. M1 checks SQLite integrity/foreign keys, exact installed
catalog schema, source/session/raw Init/plan/checkpoint, commit sequence, ID and
allocator relationships, FIFO/expansion correspondence and rooted witnesses.
M3 independently recomputes streamed logical digest, counts, all seven context
snapshots and semantic manifest correspondence. Only after both checks and
closed validation handles may M4 atomically install the staged catalog with no
active writer. Failure preserves the previous valid catalog/checkpoint.
Production Init restoration and actual API replay remain mandatory; the backup
is neither a live Store endpoint certificate nor semantic acceptance.

The immutable admitted resource plan accounts for peak live catalog, standalone
backup, chunk staging, packed/downloaded artifacts, restore scratch, metadata
and retained prior artifacts. A full backup is transferred per checkpoint;
cross-unit delta reuse is not authorized. A unit/slot/time/disk/RSS overrun
remains incomplete and preserves prior progress. No automatic quota increase,
source change, pruning or partial upload can satisfy continuation or Main.


## Authenticated verifier continuation adjunct

The closed [verifier continuation schema](paged-verifier-continuation.schema.json)
has exactly five schema_version 1 envelope kinds: verifier_backup,
verifier_chunk_index, verifier_unit_progress, verifier_check_result and
verifier_evidence_index. Its bytes and the exact M3-owned
[paged-verifier-state.sql](paged-verifier-state.sql) join the explicit proof
contract preimage above. Existing schema6 records and reference kinds are unchanged.

The separate verifier workDB preserves its private operation state, atomic
execution sequence, DFS/reachability queues, results and credit links. It does
not enter M1's authoritative catalog or allocate semantic IDs/commit sequences.
Copied checked bits alone grant no credit. Actual authenticated executions of
the reviewed independent checker, exact ordered input inventories and committed
CheckResult records grant individually attributable continuation credit. An
active quantum commits only after its full obligations succeed; interruption
or a semantic error rolls it back. Source/temporal publication still uses M1's
single authoritative writer and final-only singleton rules.

Every verifier envelope binds exact source/session/unit/checkpoint. UnitProgress
at verifier-progress/unit.json binds the actual attempt1 producer origin,
input snapshot/highwater/counts/contexts, previous authenticated progress/backup,
private before/after sequence/logical digests/cursors and bounded result root.
The first before state is deterministic empty private schema initialization at
commit0. Later before state equals the last authenticated after state, even
across intervening M1-only units. Cursor operations follow M3's explicit legal
transition graph and typed scope/input ordering; scalar ordering cannot skip
algorithm work. No committed progress is resource_blocked unless an already
admitted different stage has actual work. WorkUnit.progress mirrors only the
after_cursor.position and is never coverage authority.

VerifierCheckResult coverage binds exact enumerated record inputs and actual
committed quanta; endpoints alone do not assert coverage. algorithm_checkpoint
preserves unfinished algorithm state without semantic/SCC/TLC completion. TLC
credit requires an exact invocation tuple over family/scope, all module/config/
generated-table/map/runtime identities and normalized fixed argv, plus retained
actual log/receipt bytes. Resume compares that tuple and reparses the real log
under the original strict expected-failure rules; tags, timeout or complete:true
metadata cannot substitute.

Bounded EvidenceIndex trees have at most256 children and8MiB per metadata member.
They bind exact current or historical unit/part/member identities, typed record
keys, contiguous result sequences and ordered inventories. Empty roots are null
with zero count; cycles, gaps, duplicate ownership, hidden inputs or forged
member references fail. Restore reconciles every credit/progress row with the
authenticated result chain and exact input preimages. Paths are rebound only
from verified member references; derivative path indexes carry no checked credit.

After committed result reconciliation, M3 computes its source-pinned table/PK
ordered domain-separated logical digest, emits progress, then takes a bounded
consistent SQLite backup with all writers paused. verifier-backup/root.json in
part0 binds progress and exact workDB bytes, never a future upload receipt.
The private SQL defines all durable digest tables, including sqlite_sequence,
and the only allowed relocatable/external-bookkeeping exclusions. The separate
verifier chunk/index ordinal space shares the complete current-unit part set
and exact part count with catalog backup. Both streams, evidence, source pages
and all retained TLC bytes fit the same32 fixed native same-task upload slots
and immutable total resources; no extra task or automatic slot/quota increase.

Before atomic installation, M3 validates the staged verifier schema/integrity/
foreign keys/private sequence/logical digest and reconciles the authenticated
result/coverage chain, append-only source snapshots and retained TLC invocations.
A stale/missing/swapped result or altered checked flag rejects the snapshot,
without selective clearing as acceptance. M1 private catalog checks and M3
independent catalog/page checks remain separate. Final aggregation consumes
authenticated coverage/result roots and all semantic/backend/Main conditions,
not the last workDB's checked flags.


## Source-bound admission certificate

The explicit proof-contract preimage also includes the reviewed construction
[source-bound-formulas.json](source-bound-formulas.json) and closed
[source-bound receipt schema](source-bound-receipt.schema.json). M3 derives the
complete record width/cardinality FormulaDAG from pinned source/schema/finite
vocabulary inputs before Init creation. M4 independently checks every pinned
operand/anchor, exact full kind inventory and closed topological arithmetic
graph. Caller quotas cannot provide or reduce width/cardinality facts.

The receipt explicitly states premise_class=reviewed_source_lemmas,
machine_sqlite_rust_proof=false and acceptance=false. It is deterministic
arithmetic under reviewed source lemmas, not a machine proof of SQLite/Rust or
an empirical maximum. Every encoded contribution, escaping layer, array/row
count, raw identifier and timestamp range needs its source derivation. Missing
per-field work or an incomplete specification refuses admission; no i64/page
limit or sampled fixture substitutes. Actual unit0 validates the reviewed
Init-class premises against its migrated DB/raw settings/IDs/rows before
expansion; mismatch invalidates the session without normalization or clipping.

ResourcePlan.source_bounds_utf8 uses only the version2 canonical wrapper
{schema_version:2,accounting_rule:operational_encoded_caps_v1,source_receipt,
record_allowances,overhead,accounting_method_sha256,measurement_sha256}. Each
record_allowances entry is closed {records,max_encoded_bytes}; overhead is
closed {index_bytes,wal_bytes,scratch_bytes,retained_bytes,duplicate_backup_bytes}.
Source receipt/specification report_role is source_global_symbolic. Global
B_k/N_k formulas are independently checked and reported symbolically; no
B_k<=b_k condition or n_k*B_k operational reservation remains. Operational
counts/caps/overheads are immutable allocations under a source-pinned method,
with explicit lifetimes/copy counts and nonsemantic mappings. Fresh measured
overheads never claim to prove all future SQLite growth. Checked arbitrary-precision arithmetic
retains symbolic finite-cardinality DAGs where exact expansion is impractical;
a proven greater-than comparison never prints a truncated value as exact.
All catalog/verifier backup, chunk, artifact, restore scratch and prior
retention peaks remain within the one immutable admitted plan. No practical
closure/indivisible-component fit is inferred from the certificate.

The specification binds non-self source/schema operands and rule references,
never its own final proof-contract digest. The generated receipt carries final
SourceIdentity but is an admission input outside its own source preimage.
Registration pins the specification, derivation algorithm and receipt schema.


## Operational encoded-cap resource amendment

This rule explicitly supersedes the earlier n_k*source-global-B_k admission
formula and unfinished wrapper; no session was admitted under them and no
compatibility waiver accepts both meanings. Complete global symbolic width/
cardinality reports remain required source-coverage evidence under reviewed
lemmas, not a tight-width theorem or practical memory reservation. In particular
Scc arrays and Outcome component receipts may have enormous sound global bounds
without forcing that allocation for every actual record.

Admission uses n_k*b_k plus every named overhead/backup/restore/retention peak.
The exact accounting method is pinned by registration and enumerates canonical
records, catalog rows, private index/algorithm state, SQL index/WAL, pages,
packing, downloads and restore copies; nonsemantic logs/scratch/backup bytes
carry explicit operational mappings rather than fabricated source cardinalities.
The unadmitted example intentionally leaves source receipt/measurement/method
values null and cannot pass admission.

Producers/readers enforce b_k on the full canonical outer record, including
headers and every escaping layer, plus separate decoded-working-set/RSS limits.
They reserve an entire atomic predecessor/private quantum before publication.
An overrun rolls back the active transaction, leaves required demands/frontier
unexpanded and grants no new coverage/TLC credit. Earlier safe committed
quanta may remain unqualified progress; unuploaded local writes never advance
the authenticated chain. Whole SCC/Outcome records and TLC components remain
indivisible: no truncation/slicing/omitted receipt or semantic refusal hides
resource exhaustion. n_k uses the registered unique/retained lifetime boundary;
required graph/history/results cannot be garbage-collected to reclaim quota.

Reducing operational b_k/n_k may only produce incomplete/resource_blocked, never
a new accepted execution. No zero-progress automatic retry, silent resource
increase, source mutation or domain/fairness change is authorized. Combined
source/verifier backup/result staging still fits the same32 pinned native slots
and existing256MiB member/8MiB metadata/512MiB artifact limits. Actual service
capability and fresh owned resources remain prerequisites after final review.


### Post-upload session control

The closed [control carrier schema](paged-control-carrier.schema.json) carries
only final WorkUnit, ArtifactReceipt and optional Completion envelopes after
all native uploads in their actual producer task. Current ZIP members never
contain their own final control envelopes. The canonical control has no self
hash; its digest binds the next executed unit. A running or unsuccessful task
never grants continuation or completion authority.

The registered synchronous last required finalizer emits the unique bounded
BEGIN/chunk/END frame between its bound FINALIZER-BEGIN and FINALIZER-DONE
markers. Canonical base64 chunks are 2048 characters except the last; decoded
canonical JSON is at most 8 MiB and also satisfies its admitted smaller cap.
Consumers require exact source/event/task/attempt identity and terminal SUCCESS
before and after retrieval, reconstruct every native artifact receipt, then
recheck checkpoint/proof/Main and current ready PR head/base identities.

For this protocol only, upload_log_sha256 hashes ASCII
`plurx-async-upload-transaction-v1\0` followed by the validated timestamp-normalized
native transaction lines joined by newline with one final newline. The ZIP
hash remains separate. The transaction excludes the later control frame;
ordinary historical receipt interpretation is unchanged. Only WorkUnit,
ArtifactReceipt and Completion may be materialized from this carrier as
explicit authenticated resolver roots; all other references remain uploaded
immutable constituent members. Control framing, raw logs, base64 expansion and
decoded working memory are operational resources, with no invented semantic
source cardinalities.
