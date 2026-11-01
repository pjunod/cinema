# Paged exact proof — schema 6 boundary

**Normative specification: schema 6 and its closed transport, continuation,
resource and control adjuncts.** This contract defines storage and qualification
scheduling for the exact selected product in [CONTRACT.md](CONTRACT.md).
It preserves D, all raw identities and times, all seven contexts, original
Next/fairness, actual production replay and backend obligations. It grants no
proof credit to a page, checkpoint, prefix, or unfinished session.

The [implementation ledger](../../docs/cluster/PAGED-ASYNC-PROOF-IMPLEMENTATION.md)
owns implementation status, source conformance, qualification evidence and file
ownership. [paged-proof.schema.json](paged-proof.schema.json)
defines the closed schema 6 envelopes; [paged-catalog.sql](paged-catalog.sql)
defines the separate harness catalog. Neither is a production Store migration.
The [finite inventory](finite-inventory.json) is a source-reviewed construction
argument. Source-conformance dispositions and execution evidence belong to the
implementation ledger. The inventory is not an executed closure certificate.

## Exact records and concrete Rust interfaces

The implementation lives in `crates/plurx-core/src/store/async_driver_catalog.rs`
with `records.rs`, `sqlite.rs` and `memory.rs` in its same-named subdirectory.
These paths and signatures define the normative harness API; source delivery
and qualification evidence belong to the implementation ledger. SQLite is the
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
per-unit predecessor/record/byte allowances before dispatch. The example's
nested wrapper shows only the closed version3 `PhysicalPolicy3` field set with
illustrative round numbers; its `source_receipt`, `calibration`,
`accounting_method_sha256` and `measurement_sha256` are null and its
`record_allowances` are empty because they exist only after registration and
calibration. It is unadmitted, validates no fit and cannot pass admission.

Admission computes source-derived per-record encoded-width bounds `B_k` from
field/row vocabularies as source-global evidence only. The operational plan is
the closed `OperationalBounds3` wrapper carried inside
`ResourcePlan.source_bounds_utf8` (see "Typed physical-cap resource amendment"):
per-kind logical allowances `{records,max_encoded_bytes}`, which bound record
counts and full canonical encodings and never estimate SQLite pages, plus a
separate `PhysicalPolicy3` of immutable byte reservations for eight database
families and six pools. No record-count-times-table product, per-table
index/WAL/scratch multiplier, `duplicate_backup` term or
whole-backups-in-fixed-upload-slots inequality remains: a logical allowance is
never multiplied against a physical family. Admission instead requires the
calibrated simultaneous peak
`peak_bytes = sum(family main+journal+temp) + sum(pool bytes) +
existing_unpooled_bytes + 6*total_retained_artifact_bytes +
32 MiB*(maximum_units+3)` to satisfy `peak_bytes <= total_session_disk_bytes`
and `peak_bytes <=` the measured free disk, and
`total_retained_artifact_bytes <= total_session_disk_bytes`. These allowances
and reservations bound resource use, never the accepted graph: exceeding one
leaves the session incomplete.
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

Counterexample.original_initial is an original ProductId; prefix and cycle
contain ordered MonitorEdge IDs projected to actual original edges. A nonempty
cycle has stutter_at=null. For an empty cycle, stutter_at is the original
ProductId projected from the unique MonitorNode reached by the valid ordered
prefix from the actual MonitorInit; an empty prefix uses that MonitorInit.
Replay retains that exact monitor endpoint, history, fixed obligation, component,
capacity and full original enabledness. Product and MonitorNode numeric IDs are
never assumed equal, and histories projecting to the same Product are never
collapsed. Original implicit-stutter fairness checks remain required. That
endpoint may have outgoing
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

Executed attempts have monotonically increasing unused ordinals. Every prior
actual dispatch, source event, producer job/task and attempt1 terminal state is
authenticated, including failed or unpublished slots; maximum_units counts them
all. Successful checkpoint/control sequences may therefore have ordinal gaps
explained only by those authenticated failures. Session.next_unit is the lower
bound from the last successful checkpoint, not a dense-success counter.
Previous control and actual Init/private cursors remain those of the latest
fully authenticated successful control. Failed artifacts, local transactions
and checked flags grant no continuation credit. A failed initial attempt with
no authenticated Init requires a new session; it cannot reseed the same UUID.

The controller automatically dispatches only after useful authenticated progress.
After terminal failure, a source-bound trusted controller infrastructure resume
decision must identify the exact failed task/log, concrete diagnosis and repair,
unchanged predecessor/source/allocation and explicit nonacceptance scope. Unknown
failure is not evidence of retryability. Semantic-invalid and permanent
resource-blocked states never continue, and no retry extends quotas or slots.
Producer nonoverlap is established by exact terminal job/task observations before
dispatch and before new unit storage/work, plus the exclusive owned execution
lease and pinned workflow concurrency. Aggregate run stop times are not producer
end timestamps. The bounded current predecessor observation is retained as an
uploaded constituent, not a new semantic record or proof credit.

The registered successor alone may resume a checkpoint, after checking catalog
logical digest, full dependency closure, actual Init image and exact sources.
Completion mode reauthenticates the current ready PR/head/base, all ordered
unit artifacts/receipts, all eleven families, all seven contexts, global closure,
existing Rust/backend/source registries and current Main requirements. It emits
one complete root consumed by exact-head Main aggregation. An incomplete old
PR job is never relabeled success; no dummy source commit continues a session.


## Snapshot transport and publication adjunct

The closed [snapshot transport schema](paged-catalog-transport.schema.json)
defines the operational auxiliary kinds `catalog_backup` and `verifier_backup`
(snapshot roots, schema_version 2), `snapshot_manifest_node` (schema_version 2),
`evidence_manifest_node`, `transfer_manifest_node`, `publication_intent`,
`pending_publication`, `transfer_batch_receipt` and `publication_activation`
(schema_version 1), and `native_transport_conformance`. The control carrier
schema is version 2. These kinds change neither schema6 semantic records nor
the eight interfaces, are never source-global record kinds, and their complete
encodings, retained occurrences and physical bytes are charged as operational
allowances and `PhysicalPolicy3` reservations. Their bytes are included in the
explicit proof_contract_sha256 preimage above. The retired version1 roots
(`catalog_chunk_index`, 256 MiB chunks addressed by part ordinal) carry no
authority in a new session and are never silently converted.

M1 takes a SQLite backup API snapshot in bounded page steps, with all source,
temporal and bookkeeping writers paused, after the produced Checkpoint is
recorded and before upload receipts exist (`snapshot_boundary`
`post_checkpoint_pre_upload`; the verifier uses
`post_verifier_commit_pre_upload`). The standalone backup (`format` `sqlite3`,
`backup_method` `sqlite_backup_api_v1`) includes committed WAL contents,
private IDs, FIFO, allocator state and indexes; copying live database/WAL files
or reconstructing private catalog state from semantic pages is not permitted.
The logical Checkpoint identity is independent of physical roots and never
references its backup. A snapshot root binds source, session_id, unit_ordinal,
resource_plan_sha256, initial_image_sha256, the original `execution_origin`,
checkpoint_sha256, previous_checkpoint_sha256, `predecessor_root`,
catalog_commit, catalog_logical_digest, semantic_manifest_root,
`chunk_bytes_limit`, `chunk_count` and a `manifest_root` ContentRef
`{kind,sha256,bytes}` that carries no native artifact locator. A catalog root
adds catalog_bytes and catalog_sha256; a verifier root adds work_db_bytes,
work_db_sha256, verifier_schema_sha256, verifier_algorithm_version,
verifier_commit, verifier_logical_digest, previous_verifier_progress_sha256 and
progress_sha256. The fixed member paths `catalog-backup/root.json` and
`verifier-backup/root.json` hold these roots; neither binds a future upload
receipt.

The backup is streamed into immutable content-addressed chunks of exactly
`chunk_bytes_limit` = 1 MiB (1,048,576 bytes) except the final positive
remainder; this is transport granularity below the unchanged 256 MiB member
limit, not a semantic page or record partition. Each chunk is identified by
the SHA-256 of its original bytes. A snapshot manifest tree
(`snapshot_manifest_node`: scope_sha256, level, first_chunk, chunk_count,
offset, bytes and 1 to 256 entries; a leaf entry is
`{ordinal,offset,bytes,content}` with a `snapshot_chunk` ContentRef, a branch
entry is `{first_chunk,chunk_count,offset,bytes,child}`) describes every
interval of the file, not only changed ones, with exact contiguous ranges,
exact summaries, the exact end of file and the whole-file SHA-256. Holes,
overlaps, duplicate intervals, inconsistent summaries, cycles, extra tails and
scope/kind mixing are rejected. The scope hash binds source, session, plan and
initial image, so a chunk hash alone never authorizes content for another
snapshot. An unchanged chunk may be referenced through the authenticated
`predecessor_root` lineage, meaning only batches of publications whose
activation was accepted (the replay-derived set of accepted batch receipts), or
an already authenticated batch of the same pending snapshot. Locators of a
pending, discarded or rehearsal publication seed nothing; local cache presence
grants no provenance, and no repository-wide same-hash search is authority.

Producing a snapshot does not publish it. After the ordinary quantum and its
complete result/backup descriptors succeed, the controller freezes a
`publication_intent` (source, session, unit, plan, initial image,
`execution_origin`, previous and candidate checkpoint, catalog_root,
verifier_root or null, `execution_result`, `evidence_root`). The immutable
planned result hashed by the intent is separate from the accepted result,
which is reconstructed only after closure. A `pending_publication` state
(intent, previous_batch, coverage_sha256, coverage_count, next_batch_ordinal)
lives outside the semantic phase and cursor. Each `transfer_batch_receipt` has
`purpose` `transfer` or `activate`, a batch_ordinal, a transfer_manifest
(`transfer_manifest_node` tree whose leaves carry the physical
`{content,part_ordinal,member_path}` locator), cumulative coverage hashes and
counts, between 1 and 32 ArtifactReceipt envelopes and the actual
retained_artifact_bytes, and fixes `acceptance` and
`proof_checkpoint_advanced` to false. It separates the original `execution_origin`
from the actual `publication_origin`; a later upload job is never relabelled as
the original execution, and no ordinary same-origin rule is relaxed for it.
Coverage is a streamed SHA-256 over sorted `{kind,sha256,bytes}` identities and
is recomputed from disk-backed indexes, never carried as a control array.
Already authenticated content is referenced, not retransmitted.

The registered workflow still contains a finite pinned sequence of
runner-native `uses: upload-artifact` slots in the same producer task. Packing
is exact per dispatch: every batch independently respects 32 slots, 300
members and 512 MiB extracted/retained ZIP limits including ZIP overhead,
256 MiB members and 8 MiB metadata members; the batch first reserves the
source-bounded space of its transfer-manifest nodes, then packs only content
not already authenticated, and ends at the first content that no longer fits.
A batch that can pack no required content is `resource_blocked`. Progress is
closure-relative on the receiver as well: a `transfer` batch is credited only
for identities in the pending publication's required closure (the intent, then
the children of covered, authenticated parents, recomputed from the batch's own
bytes) and must newly cover at least one; authenticated bytes of unrelated
content are refused, credit nothing and gain no locator, and an `activate`
batch may carry only validation receipts. The sender therefore offers content
only once an owner is covered or packed earlier in the same batch. A snapshot
therefore need not fit one job. Remaining content is carried by further
closed `publish` dispatches (`Origin.operation` `execute` or `publish`) on the
same registered route, runtime and storage lease. These execute no proof
operation. Each dispatch's `dispatch_ordinal` is the length of the authenticated
attempt inventory read from the source repository's own event history and
re-derived by every replay (a local counter could diverge from that inventory,
so none is kept), and failed or uncertain dispatches, publish dispatches and the final
activation dispatch all consume the immutable `maximum_units`; there is no
hidden transfer loop, dynamic matrix, runtime slot increase or operator-created
batch receipt. No direct action-bundle execution or separate uploader task is
authorized. The actual registered workflow/ref/native-slot capability must be
established before admission; the declaration proves none. A failed first execution cannot gain semantic
or TLC authority from a later upload: its uploaded evidence stays charged, the
pending authority is discarded and the session reruns from the last accepted
checkpoint.

Completion of publication requires full snapshot and evidence closure, then
streaming reconstruction of both entire files into fresh owned staging, with
per-chunk and whole-file size and SHA-256 checked against the roots, and a
final `publish` dispatch with `purpose` `activate`. M1 checks SQLite
integrity/foreign keys, exact installed catalog schema, source/session/raw
Init/plan/checkpoint, commit sequence, ID and allocator relationships, FIFO/
expansion correspondence and rooted witnesses. M3 independently recomputes the
streamed logical digest, counts, all seven context snapshots and semantic
manifest correspondence, reconciles the restored verifier result chain and
reauthenticates the original TLC logs and every historical member. The
`publication_activation` record (`activated` true, `acceptance` false) binds
the `validations` `{catalog_private,catalog_semantic,verifier,final_input}`
ContentRefs, the `stage` and `stage_complete`. Activation concerns the entire
current retained snapshot, not premature global closure: a partial expansion may
activate with `stage_complete` false and its existing allowed frontier and
open-context prefix, while later stages keep their existing closure
requirements. Only after both validations and atomic installation with no
active writer may `advance` run exactly once on the original ordinary result. A
publication-only success never grants checkpoint, partial Main pass or
Completion; the final complete operation remains mandatory. Failure preserves
the previous valid catalog and checkpoint.

On interruption the last fully published checkpoint remains authority. The
exact pending publication resumes after its batch chain is authenticated and
the immutable spool checked; the local chunk cache is rebuilt from authenticated
native artifacts. If required unuploaded spool bytes are lost, the pending
authority is discarded and the session reruns from the last accepted
checkpoint; spool directories no authenticated pending chain can still use are
removed at the next job start and their bytes are never reused: a rebuilt
snapshot is transferred and authenticated again. A new controller process also
settles `physical-transfers/` and `physical-grants/` records a crashed parent
left (the single moving file is located exactly and charged to the side that
holds it, a lost or ambiguous one refuses the job, a grant of a dead parent is
removed, a live foreign parent refuses). Production Init
restoration and actual API replay remain mandatory; a snapshot is neither a live
Store endpoint certificate nor semantic acceptance, and transport never grants
proof acceptance.

The `native_transport_conformance` result (case
`native_snapshot_three_batch_v1`, carrier role `transport_conformance`) is a
mandatory genuine native probe after the first authenticated pair: the first two
distinct real snapshot chunks are each the sole new payload of batch zero and
batch one, the remaining genuine closure is carried by batch two onward, and a
fresh receiver reconstructs both files. It requires at least three nonempty
transfer batches, uses no padding or fake graph, advances no checkpoint and
grants no proof credit; all of its real dispatches and bytes are charged.

The immutable admitted resource plan accounts for these physical peaks through
the `PhysicalPolicy3` families and pools below: live catalog and verifier
databases, the snapshot spool, restore staging, transfer staging, the verified
chunk cache, packed/downloaded artifacts and retained prior artifacts. A unit/
slot/time/disk/RSS overrun remains incomplete and preserves prior progress.
No automatic quota increase, source change, pruning or partial upload can
satisfy continuation or Main.


## Authenticated verifier continuation adjunct

The closed [verifier continuation schema](paged-verifier-continuation.schema.json)
has exactly four schema_version 2 envelope kinds: verifier_backup (the
snapshot root defined above), verifier_unit_progress, verifier_check_result and
verifier_evidence_index. The retired version1 verifier_chunk_index no longer
exists. Its bytes and the exact M3-owned
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
algorithm work. load_input and check_input are bounded private preparation
operations: their authenticated algorithm_checkpoint results preserve path-free
member loading and root-scoped dependency checks without granting semantic
coverage. Unrelated historical cached members do not become current roots.
Preparation-only incomplete prefixes may be sealed, uploaded and restored;
semantic use and the catalog writer remain blocked until the exact current
snapshot correspondence checks succeed. Durable member indexes, dependency
queues and tallies enter the private logical digest and backup accounting;
host paths remain disposable bindings to authenticated members. No committed progress is resource_blocked unless an already
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
They bind exact current or historical `MemberRef2`
`{unit_ordinal,member_path,bytes,sha256}` identities (the physical part_ordinal
no longer belongs to a member identity; native location is authenticated
separately by the transfer receipts), typed record
keys, contiguous result sequences and ordered inventories. Empty roots are null
with zero count; cycles, gaps, duplicate ownership, hidden inputs or forged
member references fail. Restore reconciles every credit/progress row with the
authenticated result chain and exact input preimages. Paths are rebound only
from verified member references; derivative path indexes carry no checked credit.

After committed result reconciliation, M3 computes its source-pinned table/PK
ordered domain-separated logical digest, emits progress, then takes a bounded
consistent SQLite backup with all writers paused. The `verifier_backup` root
at verifier-backup/root.json binds progress_sha256 and the exact workDB
work_db_bytes and work_db_sha256, never a future upload receipt.
The private SQL defines all durable digest tables, including sqlite_sequence,
and the only allowed relocatable/external-bookkeeping exclusions. The verifier
snapshot is published by the same publication chain as the catalog snapshot
through 1 MiB content-addressed chunks and a snapshot manifest tree. Each
native batch fits the 32 fixed same-task upload slots; a snapshot spans as many
counted dispatches as its missing content needs within the immutable
`maximum_units` and total resources, with no extra task or automatic
slot/quota increase.

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

ResourcePlan.source_bounds_utf8 uses only the version3 canonical
`OperationalBounds3` wrapper {schema_version:3,
accounting_rule:typed_physical_caps_v1,source_receipt,record_allowances,
physical_policy,calibration,accounting_method_sha256,measurement_sha256}. Each
record_allowances entry is closed {records,max_encoded_bytes}. The physical
policy and its calibration appear only inside this string, never as a competing
top-level plan field. The earlier version2 wrapper
{operational_encoded_caps_v1, overhead:{index_bytes,wal_bytes,scratch_bytes,
retained_bytes,duplicate_backup_bytes}} is retired: the receipt schema no longer
defines it, and the verifier Budget, the session admission wrapper reader and
the Rust catalog configuration each refuse it with no conversion or compatibility
reader. Source receipt/specification report_role is source_global_symbolic.
Global B_k/N_k formulas are independently checked and reported symbolically; no
B_k<=b_k condition or n_k*B_k operational reservation remains. Operational
counts and caps are immutable allocations under a source-pinned method, with
explicit lifetimes and nonsemantic mappings, and physical reservations are
operator capacities rather than measured consumption (see "Typed physical-cap
resource amendment"). Checked arbitrary-precision arithmetic
retains symbolic finite-cardinality DAGs where exact expansion is impractical;
a proven greater-than comparison never prints a truncated value as exact.
All catalog/verifier snapshot, chunk, artifact, restore staging and prior
retention peaks remain within the one immutable admitted plan. No practical
closure/indivisible-component fit is inferred from the certificate.

The specification binds non-self source/schema operands and rule references,
never its own final proof-contract digest. The generated receipt carries final
SourceIdentity but is an admission input outside its own source preimage.
Registration pins the specification, derivation algorithm and receipt schema.


## Typed physical-cap resource amendment

This rule explicitly supersedes the earlier n_k*source-global-B_k admission
formula and the version2 `operational_encoded_caps_v1` wrapper, including its
per-table index/WAL/scratch/retained/duplicate-backup overhead inventory and the
inequality that required both complete standalone backups to fit the current
job's fixed upload slots. That inequality charged every logical record
allowance against every catalog and verifier table and index, and could not be
satisfied by more RAM, disk or slots; replacing it with a smaller multiplier
would be unjustified. No session was admitted under the earlier rules and no
compatibility waiver accepts both meanings. Complete global symbolic width/
cardinality reports remain required source-coverage evidence under reviewed
lemmas, not a tight-width theorem or practical memory reservation. In particular
Scc arrays and Outcome component receipts may have enormous sound global bounds
without forcing that allocation for every actual record. Nothing here changes
the seven contexts, eleven outcomes, full-D and raw Store identity, production
Init, the global SCC/outgoing-edge/fairness/stutter checks or the claim scope:
a finite selected two-worker operation under explicit premises, not a complete
Rust/Tokio/Raft simulation or cluster-wide deadlock freedom.

Logical and physical limits are separate. Per-kind `record_allowances`
(`{records,max_encoded_bytes}`, with `max_encoded_bytes` at most the 256 MiB
member limit and at most 8 MiB for operational auxiliary kinds) bound record
counts and complete canonical encodings. They are never multiplied against
tables and never estimate SQLite pages. Physical safety rests on independent
enforced file and pool caps declared by `PhysicalPolicy3`, closed and carried
only inside `ResourcePlan.source_bounds_utf8`:
`{schema_version:3,kind:physical_policy,table_map_sha256,families,pools}`.
`families` has exactly eight members, each a closed `FamilyPolicy`
`{owner,database_bytes,main_bytes,journal_bytes,temp_bytes}` of positive
integers with `main_bytes >= database_bytes`; `database_bytes` caps each
database file, `main_bytes` reserves the family's simultaneous main files
(calibration requires it to cover every database the map declares live together,
below),
`journal_bytes` covers WAL, shared-memory and rollback companions and
`temp_bytes` covers SQLite FILE temporaries:

| Family | Exact owner | Holds |
|---|---|---|
| `catalog_main` | `catalog` | live Rust catalogs |
| `verifier_main` | `verifier` | live M3 verifier databases |
| `catalog_export_scratch` | `catalog_export` | the manifest-closure export database |
| `verifier_continuation_scratch` | `verifier_continuation` | continuation and work scratch |
| `catalog_or_verifier_restore_scratch` | `snapshot_restore` | ancillary journal and temp work of the read-only restore validators; it declares no database role, because a restored file is attached from `restore_staging_bytes` and never given its own scratch database |
| `publication_pack_scratch` | `snapshot_transport` | PartPlan and manifest queues |
| `authentication_index` | `provenance` | dependency, member and locator indexes |
| `publication_pending_scratch` | `session_controller` | pending, coverage and dispatch indexes |

`pools` has exactly six positive byte members with these owners:
`snapshot_spool_bytes` (`snapshot_transport`), `restore_staging_bytes`
(`snapshot_restore`), `transfer_staging_bytes` (`snapshot_transport`),
`cas_bytes` (`provenance`), `source_runtime_scratch_bytes` and `control_bytes`
(both `session_controller`). Every directory or file family has exactly one
owner and one reservation; a copy belongs to its pool, not to a second
independent grant; there is no unnamed shared pool or unaccounted fallback.
Downloaded artifact ZIPs and their extractions are charged to
`restore_staging_bytes` with their extraction subtree tracked, and the
dependency indexes and receiver ledgers are opened through their declared
families. The unit quantum's own output directory (pages, manifests, verifier
evidence, TLC logs, the initial image) is the one tree not yet delegated as a
named subtree sized from `unit_byte_allowance`; it is bounded only by the
runtime aggregate session-disk check until the grant ABI can claim it.
`database_bytes` values are operator capacity ceilings rounded down to the
measured SQLite page size, neither observed consumption nor a promise of
sufficiency, and no B-tree fill factor, average row width or one-page-per-row
assumption is made.

`table_map_sha256` is the digest of the canonical reviewed table-ownership map
`validation/async_physical_table_map.json`. The map lists every explicit
installed table, index and scratch DDL of each family's database roles with
pinned constructor sources whose hashes are verified when the map loads; implicit
objects (`sqlite_sequence` and `sqlite_autoindex_*`) are derived from the actual
engine and the declared PRIMARY KEY/UNIQUE constraints. Every database role also
declares its `role_lifetime` `{simultaneous,holders}`: the largest number of
databases of that role live at once in one job or ledger (active input plus
pending output generation for the catalog and the verifier; the pack job's part
plan and its content and evidence inventories; the working, conformance and
controller dependency indexes; the store, replay and conformance pending
ledgers) and who holds them. Loading the map also requires each object's
constructor to resolve to a real definition in its pinned source (a Python
class method or function, a Rust impl function, or a CREATE statement of the
source SQL), so a removed constructor cannot stay calibrated. Calibration rejects an
unknown or missing table or index, a drifted constructor source and an installed
DDL that differs from the map, and requires each family's reservations to cover
its declared live databases: with `n` the sum of the family's role
`simultaneous` counts (at least one), `main_bytes >= n*database_bytes`,
`journal_bytes >= n*(2*database_bytes + 1 MiB)` and
`temp_bytes >= n*2*database_bytes`. The map supports relational planning and source
review; it is not a packing theorem. The nonsemantic accounting classes of the
registered inventory each name exactly one physical charge:
`backup_and_chunks` charges `snapshot_spool_bytes`, `downloads_and_restores`
`restore_staging_bytes`, `logs` the retained-artifact term of the peak,
`source_archive` `source_runtime_scratch_bytes`, `sql_indexes` the families'
`main_bytes` and `sql_wal` their `journal_bytes`.

The wrapper's `calibration` is `PhysicalCalibration1`
`{schema_version:1,kind:physical_calibration,policy_sha256,table_map_sha256,
geometry,baseline,peak_bytes}`. `geometry` lists, for every family, a
`{database_role,sqlite_source_id,schema_sha256,page_size,page_count,tables,
indexes}` observation of each source-declared database role, taken from the
actual engine; every declared role is measured exactly once, each empty database
must fit its family's `database_bytes` rounded down to the measured page size,
and the empty databases of a family together must fit its `main_bytes`.
`baseline` is `{source_workspace_bytes,existing_unpooled_bytes}`, disjoint from
the pools; the measured source archive and extraction must fit
`source_runtime_scratch_bytes` and are not added twice. The conservative
simultaneous reservation is
`peak_bytes = sum(family main+journal+temp) + sum(pool bytes) +
existing_unpooled_bytes + 6*total_retained_artifact_bytes +
32 MiB*(maximum_units+3)`, where the factor six keeps the conservative
retained/download allowance and `32 MiB*(maximum_units+3)` the per-dispatch log
reserve and the three pre-admission capability probes. Admission requires
`peak_bytes` within `total_session_disk_bytes` and the measured free disk, a
non-null calibration whose `policy_sha256` is the digest of the policy and whose
`table_map_sha256` matches it, the same calibration as the measurement's
`resource_calibration`, and a non-null `measurement_sha256`. A draft wrapper has
null calibration and measurement. The measured closed `resource_method` is
`{id:typed_physical_caps_v1,source_sha256,policy_sha256,table_map_sha256,
accounting_inventory,record_allowances,source_bound_specification_sha256,
plan_inputs,prefix_policy:late_bound_per_batch_v1}` where `plan_inputs` keeps the
seven scalar plan inputs and adds `physical_policy_sha256`,
`record_allowances_sha256`, `source_receipt_sha256` and
`unit_predecessor_allowance`, each hash covering the complete canonical value.
`prefix_policy` states that no fixed-prefix part count is an admission premise;
exact packing is enforced per batch.

Enforcement is prospective. Each reservation is made before the file is
created, growth is refused before allocation, and a reservation is released only
after the owned files are actually removed and the removal is made durable; a
surviving file keeps its charge, including after a failure. For a writable
database with ceiling `c` the grant reserves `c` main, `2c + 1 MiB` journal
(WAL, shared memory and rollback overlap plus framing) and `2c` temp, sets
`max_page_count` to `floor(c / page_size)`, requires the requested pragma
result to equal it, and refuses a restored or existing database already beyond
it. The verifier `Budget` and the Rust catalog (`catalog_main.database_bytes`)
apply the same ceiling, re-verify it around SQL work and commits, and require
their family's `journal_bytes` and `temp_bytes` to cover
`2*database_bytes + 1 MiB` and `2*database_bytes`.
Exhaustion rolls back the active transaction and is an incomplete disposition
(`SQLITE_FULL` or a refused reservation), never a semantic result, never proof
credit and never a reason to change the cursor or accepted checkpoint.

An allocation-capable child owns only what its parent delegated. The closed
private `LocalPhysicalGrant1` descriptor (`schema_version`, `kind`, `grant_id`,
`source`, `session_id`, `resource_plan_sha256`, `physical_policy_sha256`,
`owned_root`, `lease`, `parent`, `allocations`, `attachments`) is local and
transient, never an uploaded receipt, a ResourcePlan field or a request field. The
parent deducts every delegated allocation before the child starts and keeps the
charges until the child is reaped and its surviving files are reconciled; the
child, located only through the environment names
`PLURX_ASYNC_PHYSICAL_GRANT_FD`, `PLURX_ASYNC_PHYSICAL_GRANT_SHA256` and
`PLURX_ASYNC_PHYSICAL_LEASE_FD`, narrows every family and pool limit to that
grant and never instantiates the full policy again; without a grant it refuses
to run as allocation-capable. An already-restored read-only database attaches
its held reservation instead of consuming another main allocation, and its
journal and temp work need a separate allocation. Ownership transfer reserves
the destination while the source stays charged and commits one owner only after
the rename and directory fsync; an alias never creates fresh quota. The attached
restored database is checked against its effective page ceiling (its destination
family's `database_bytes` rounded down to the file's own page size) from its
header, before it is first opened, by both private restore validators; a
database beyond it is an incomplete disposition. The descriptor's `parent` is
`{pid,start_ticks}` where `start_ticks` is the platform's exact process start
identity (field 22 of `/proc/<pid>/stat` on Linux; the kernel start time of
`sysctl kern.proc.pid` in microseconds on macOS), compared for equality with the
same pid's current reading; the owned-root and lease device/inode checks are
exact file-system comparisons on both platforms.

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

Reducing operational b_k/n_k or any physical reservation may only produce
incomplete/resource_blocked, never a new accepted execution. No zero-progress
automatic retry, silent resource increase, source mutation or domain/fairness
change is authorized. Per-batch native packing remains exact against the same
32 pinned native slots and existing 256 MiB member, 8 MiB metadata and 512 MiB
artifact limits. These guards are conditional operational enforcement, not a
syscall-level allocation theorem or a guarantee that any admitted logical record
set, indivisible component or complete-Outcome memory fits. The unadmitted
example intentionally leaves source receipt/measurement/method values null and
cannot pass admission; no plan has been admitted, calibrated or measured, and
actual service capability and fresh owned resources remain prerequisites after
final review.


### Post-upload session control

The closed [control carrier schema](paged-control-carrier.schema.json) carries
only final WorkUnit, ArtifactReceipt and optional Completion envelopes after
all native uploads in their actual producer task. Current ZIP members never
contain their own final control envelopes. The canonical control has no self
hash; its digest binds the next executed unit. A running or unsuccessful task
never grants continuation or completion authority. The version2 carrier has
`role` `unit_checkpoint`, `completion`, `publication_batch` or
`transport_conformance`. A `unit_checkpoint` carries an `activation`
(ActivationResult) and no pending publication; a `publication_batch` carries a
`pending_publication` and `transfer_batch` and no completion, activation or Main
postcheck authority; `completion` carries none of these.

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
