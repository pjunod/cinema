//! Test-only persistent exact proof catalog; distinct from the production Store.
//! The data path never decides production transitions. It stores checked values
//! and complete atomic predecessor expansions, with full-byte identity checks.
pub mod grant;
pub mod memory;
pub mod records;
pub mod sqlite;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde_json::{json, Value};

pub use records::*;

pub const PAGE_BYTES: u64 = 256 * 1024 * 1024;
pub const ARTIFACT_BYTES: u64 = 512 * 1024 * 1024;
pub const CONTEXTS: [&str; 7] = [
    "base",
    "stable",
    "weak_admission",
    "no_claim_win",
    "withheld_join",
    "old_poll_policy",
    "drop_before_join",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitPhase {
    Initial,
    Source,
    Temporal,
}
impl CommitPhase {
    pub fn name(self) -> &'static str {
        match self {
            Self::Initial => "initial",
            Self::Source => "source",
            Self::Temporal => "temporal",
        }
    }
}
#[derive(Clone, Debug)]
pub struct WriteContext {
    pub phase: CommitPhase,
    pub unit_ordinal: u64,
    pub expansion: Option<u64>,
}
#[derive(Clone, Debug)]
pub struct ReadLimits {
    pub max_records: usize,
    pub max_encoded_bytes: u64,
}
#[derive(Clone, Debug)]
pub struct RecordRange {
    pub kind: String,
    pub after: Option<RecordKey>,
    pub through_transaction: TransactionId,
}
#[derive(Clone, Debug)]
pub struct RecordBatch {
    pub records: Vec<Record>,
    pub encoded_bytes: u64,
    pub next_after: Option<RecordKey>,
}
#[derive(Clone, Debug)]
pub struct CommittedRange {
    pub kind: String,
    pub first: RecordKey,
    pub last: RecordKey,
    pub transaction_high_water: TransactionId,
}
#[derive(Clone, Debug)]
pub struct BackupLimits {
    pub max_bytes: u64,
    pub deadline: std::time::Instant,
}
#[derive(Clone, Debug)]
pub struct PageLimits {
    pub max_records: usize,
    pub max_member_bytes: u64,
    pub max_manifest_node_bytes: u64,
}

/// Storage primitives shared by the disk catalog and the deliberately small
/// independent map reference. No backend can substitute a transition outcome.
pub trait Storage {
    fn begin(&mut self, context: WriteContext) -> CatalogResult<u64>;
    fn set_expansion(&mut self, expansion: u64) -> CatalogResult<()>;
    fn commit(&mut self) -> CatalogResult<u64>;
    fn rollback(&mut self) -> CatalogResult<()>;
    fn sequence(&self) -> CatalogResult<u64>;
    fn allocate(&mut self, kind: &str) -> CatalogResult<u64>;
    fn get(&self, kind: &str, key: &RecordKey) -> CatalogResult<Option<Record>>;
    fn put(&mut self, record: &Record, replace_private: bool) -> CatalogResult<()>;
    fn find_canonical(&self, kind: &str, bytes: &str) -> CatalogResult<Option<Record>>;
    fn scan(
        &self,
        kind: &str,
        after: Option<&RecordKey>,
        after_transaction: u64,
        limits: &ReadLimits,
    ) -> CatalogResult<RecordBatch>;
    fn count(&self, kind: &str) -> CatalogResult<u64>;
    fn oracle_edge_for_input(&self, from: u64, input: &str) -> CatalogResult<Option<u64>>;
    fn get_control(&self, key: &str) -> CatalogResult<Option<Value>>;
    fn set_control(&mut self, key: &str, value: &Value) -> CatalogResult<()>;
    fn enqueue(&mut self, product: u64) -> CatalogResult<u64>;
    fn oldest_pending(&self) -> CatalogResult<Option<(u64, u64)>>;
    fn mark_expanded(&mut self, product: u64) -> CatalogResult<()>;
    fn pending(&self, context: Option<&str>) -> CatalogResult<u64>;
    fn set_initial(&mut self, context: &str, product: u64) -> CatalogResult<()>;
    fn initial(&self, context: &str) -> CatalogResult<Option<u64>>;
    fn begin_read(&mut self, expected: u64) -> CatalogResult<()>;
    fn end_read(&mut self) -> CatalogResult<()>;
    fn save_checkpoint(
        &mut self,
        checkpoint: &Record,
        work_unit: &Record,
        session: &Record,
    ) -> CatalogResult<()>;
    fn save_export(&mut self, record: &Record) -> CatalogResult<()>;
    fn check_private(&self, checkpoint: &Record) -> CatalogResult<()>;
    fn backup(&self, path: &Path, limits: &BackupLimits) -> CatalogResult<()>;
}

#[derive(Clone, Debug)]
pub struct CatalogConfig {
    pub path: PathBuf,
    pub source: SourceIdentity,
    pub session_id: uuid::Uuid,
    pub resource_plan: Record,
    /// The inherited child grant. An allocation-capable child spends only this
    /// view; `None` is the parent/fixture view of the whole admitted policy.
    pub grant: Option<std::sync::Arc<grant::ChildGrant>>,
}
impl CatalogConfig {
    pub fn validate(&self) -> CatalogResult<()> {
        self.source.validate()?;
        self.resource_plan.validate()?;
        let plan = &self.resource_plan.record;
        if self.resource_plan.kind != "ResourcePlan"
            || plan["admission"] != "admitted"
            || plan["source"] != serde_json::to_value(&self.source)?
            || plan["session_id"] != self.session_id.to_string()
            || plan["plan_sha256"] != self.resource_plan.sha256()
        {
            return Err(CatalogError::SourceMismatch(
                "unadmitted or mismatched resource plan".into(),
            ));
        }
        Ok(())
    }
    fn binding(&self) -> Value {
        json!({"source":self.source,"session_id":self.session_id,"resource_plan":self.resource_plan})
    }
    /// The plan's limits, narrowed to the child grant when one was inherited.
    pub fn limits(&self) -> CatalogResult<ResourceLimits> {
        let limits = ResourceLimits::from_plan(&self.resource_plan)?;
        match &self.grant {
            Some(grant) => limits.granted(grant),
            None => Ok(limits),
        }
    }
}
#[derive(Clone, Debug)]
pub struct RecordAllowance {
    pub records: u64,
    pub max_encoded_bytes: u64,
}
#[derive(Clone, Debug)]
pub struct FamilyLimits {
    pub database_bytes: u64,
    pub main_bytes: u64,
    pub journal_bytes: u64,
    pub temp_bytes: u64,
}
#[derive(Clone, Debug)]
pub struct ResourceLimits {
    pub record_allowances: BTreeMap<String, RecordAllowance>,
    /// The plan's closed PhysicalPolicy3, retained so a child can bind its grant to it.
    pub physical_policy: Value,
    pub families: BTreeMap<String, FamilyLimits>,
    pub pools: BTreeMap<String, u64>,
    pub resident_bytes: u64,
    pub rss_bytes: u64,
    pub total_disk_bytes: u64,
    pub unit_records: u64,
    pub unit_bytes: u64,
    pub unit_predecessors: u64,
}
impl ResourceLimits {
    pub fn from_plan(plan: &Record) -> CatalogResult<Self> {
        plan.validate()?;
        let body = &plan.record;
        let text = string(body, "source_bounds_utf8")?;
        let wrapper = strict_json(text.as_bytes())?;
        if canonical(&wrapper) != text {
            return Err(CatalogError::CorruptRecord(
                "noncanonical operational wrapper".into(),
            ));
        }
        closed_fields(
            &wrapper,
            &[
                "schema_version",
                "accounting_rule",
                "source_receipt",
                "record_allowances",
                "physical_policy",
                "calibration",
                "accounting_method_sha256",
                "measurement_sha256",
            ],
        )?;
        if wrapper["schema_version"] != 3
            || wrapper["accounting_rule"] != "typed_physical_caps_v1"
            || !wrapper["source_receipt"].is_object()
        {
            return Err(CatalogError::SourceMismatch(
                "unsupported operational accounting rule".into(),
            ));
        }
        for field in ["accounting_method_sha256", "measurement_sha256"] {
            validate_definition("Sha256", &wrapper[field])?;
        }
        let policy = &wrapper["physical_policy"];
        closed_fields(
            policy,
            &[
                "schema_version",
                "kind",
                "table_map_sha256",
                "families",
                "pools",
            ],
        )?;
        let source_map = strict_json(include_bytes!(
            "../../../../validation/async_physical_table_map.json"
        ))?;
        if policy["schema_version"] != 3
            || policy["kind"] != "physical_policy"
            || policy["table_map_sha256"] != digest(canonical(&source_map).as_bytes())
        {
            return Err(CatalogError::SourceMismatch(
                "physical policy/source-map identity".into(),
            ));
        }
        let family_owners = [
            ("catalog_main", "catalog"),
            ("verifier_main", "verifier"),
            ("catalog_export_scratch", "catalog_export"),
            ("verifier_continuation_scratch", "verifier_continuation"),
            ("catalog_or_verifier_restore_scratch", "snapshot_restore"),
            ("publication_pack_scratch", "snapshot_transport"),
            ("authentication_index", "provenance"),
            ("publication_pending_scratch", "session_controller"),
        ];
        closed_fields(
            &policy["families"],
            &family_owners.map(|(family, _)| family),
        )?;
        let mut families = BTreeMap::new();
        let mut peak = 0u64;
        let add = |total: u64, value: u64| {
            total.checked_add(value).ok_or_else(|| {
                CatalogError::ResourceIncomplete("physical reservation overflow".into())
            })
        };
        for (family, owner) in family_owners {
            let entry = &policy["families"][family];
            closed_fields(
                entry,
                &[
                    "owner",
                    "database_bytes",
                    "main_bytes",
                    "journal_bytes",
                    "temp_bytes",
                ],
            )?;
            let limits = FamilyLimits {
                database_bytes: number(entry, "database_bytes")?,
                main_bytes: number(entry, "main_bytes")?,
                journal_bytes: number(entry, "journal_bytes")?,
                temp_bytes: number(entry, "temp_bytes")?,
            };
            if entry["owner"] != owner
                || limits.database_bytes == 0
                || limits.main_bytes < limits.database_bytes
                || limits.journal_bytes == 0
                || limits.temp_bytes == 0
            {
                return Err(CatalogError::ResourceIncomplete(
                    "invalid physical family policy".into(),
                ));
            }
            for bytes in [limits.main_bytes, limits.journal_bytes, limits.temp_bytes] {
                peak = add(peak, bytes)?;
            }
            families.insert(family.to_owned(), limits);
        }
        let pool_names = [
            "snapshot_spool_bytes",
            "restore_staging_bytes",
            "transfer_staging_bytes",
            "cas_bytes",
            "source_runtime_scratch_bytes",
            "control_bytes",
        ];
        closed_fields(&policy["pools"], &pool_names)?;
        let mut pools = BTreeMap::new();
        for name in pool_names {
            let bytes = number(&policy["pools"], name)?;
            if bytes == 0 {
                return Err(CatalogError::ResourceIncomplete(
                    "zero physical pool".into(),
                ));
            }
            peak = add(peak, bytes)?;
            pools.insert(name.to_owned(), bytes);
        }
        let calibration = &wrapper["calibration"];
        closed_fields(
            calibration,
            &[
                "schema_version",
                "kind",
                "policy_sha256",
                "table_map_sha256",
                "geometry",
                "baseline",
                "peak_bytes",
            ],
        )?;
        if calibration["schema_version"] != 1
            || calibration["kind"] != "physical_calibration"
            || calibration["policy_sha256"] != digest(canonical(policy).as_bytes())
            || calibration["table_map_sha256"] != policy["table_map_sha256"]
        {
            return Err(CatalogError::SourceMismatch(
                "physical calibration policy binding".into(),
            ));
        }
        closed_fields(
            &calibration["baseline"],
            &["source_workspace_bytes", "existing_unpooled_bytes"],
        )?;
        if number(&calibration["baseline"], "source_workspace_bytes")?
            > pools["source_runtime_scratch_bytes"]
        {
            return Err(CatalogError::ResourceIncomplete(
                "source/runtime baseline exceeds pool".into(),
            ));
        }
        peak = add(
            peak,
            number(&calibration["baseline"], "existing_unpooled_bytes")?,
        )?;
        peak = add(
            peak,
            number(body, "total_retained_artifact_bytes")?
                .checked_mul(6)
                .ok_or_else(|| {
                    CatalogError::ResourceIncomplete("retention reservation overflow".into())
                })?,
        )?;
        peak = add(
            peak,
            number(body, "maximum_units")?
                .checked_add(3)
                .and_then(|n| n.checked_mul(32 * 1024 * 1024))
                .ok_or_else(|| {
                    CatalogError::ResourceIncomplete("dispatch reservation overflow".into())
                })?,
        )?;
        let total_disk_bytes = number(body, "total_session_disk_bytes")?;
        if peak != number(calibration, "peak_bytes")? || peak > total_disk_bytes {
            return Err(CatalogError::ResourceIncomplete(
                "physical peak differs/exceeds total disk".into(),
            ));
        }
        let entries = wrapper["record_allowances"]
            .as_object()
            .ok_or_else(|| CatalogError::CorruptRecord("allowance inventory".into()))?;
        let mut allowances = BTreeMap::new();
        for (kind, value) in entries {
            closed_fields(value, &["records", "max_encoded_bytes"])?;
            let records = number(value, "records")?;
            let max_encoded_bytes = number(value, "max_encoded_bytes")?;
            if max_encoded_bytes == 0 || max_encoded_bytes > PAGE_BYTES {
                return Err(CatalogError::ResourceIncomplete(
                    "invalid full encoded record allowance".into(),
                ));
            }
            allowances.insert(
                kind.clone(),
                RecordAllowance {
                    records,
                    max_encoded_bytes,
                },
            );
        }
        for kind in record_kinds() {
            if !allowances.contains_key(*kind) {
                return Err(CatalogError::ResourceIncomplete(format!(
                    "missing operational kind {kind}"
                )));
            }
        }
        Ok(Self {
            record_allowances: allowances,
            physical_policy: policy.clone(),
            families,
            pools,
            resident_bytes: number(body, "resident_bytes")?,
            rss_bytes: number(body, "resident_rss_bytes")?,
            total_disk_bytes,
            unit_records: number(body, "unit_record_allowance")?,
            unit_bytes: number(body, "unit_byte_allowance")?,
            unit_predecessors: number(body, "unit_predecessor_allowance")?,
        })
    }
    /// This child's view: only what the parent deducted and handed down. A
    /// family or pool the grant never named is absent, not defaulted to policy.
    pub fn granted(&self, grant: &grant::ChildGrant) -> CatalogResult<Self> {
        let mut view = self.clone();
        view.families.clear();
        view.pools.clear();
        for (name, policy) in &self.families {
            if let Some(main) = grant.limit(name, "main") {
                view.families.insert(
                    name.clone(),
                    FamilyLimits {
                        database_bytes: policy.database_bytes.min(main),
                        main_bytes: main.min(policy.main_bytes),
                        journal_bytes: grant
                            .limit(name, "journal")
                            .unwrap_or(0)
                            .min(policy.journal_bytes),
                        temp_bytes: grant
                            .limit(name, "temp")
                            .unwrap_or(0)
                            .min(policy.temp_bytes),
                    },
                );
            }
        }
        for name in self.pools.keys() {
            if let Some(bytes) = grant.limit(name, "bytes") {
                view.pools.insert(name.clone(), bytes);
            }
        }
        Ok(view)
    }
    pub fn pool(&self, name: &str) -> CatalogResult<u64> {
        self.pools.get(name).copied().ok_or_else(|| {
            CatalogError::ResourceIncomplete(format!("unregistered physical pool {name}"))
        })
    }
    pub fn family(&self, name: &str) -> CatalogResult<&FamilyLimits> {
        self.families.get(name).ok_or_else(|| {
            CatalogError::ResourceIncomplete(format!("unregistered physical family {name}"))
        })
    }
    pub fn allowance(&self, kind: &str) -> CatalogResult<&RecordAllowance> {
        self.record_allowances.get(kind).ok_or_else(|| {
            CatalogError::ResourceIncomplete(format!("unregistered accounting kind {kind}"))
        })
    }
}
pub fn closed_fields(value: &Value, fields: &[&str]) -> CatalogResult<()> {
    let object = value
        .as_object()
        .ok_or_else(|| CatalogError::CorruptRecord("closed object required".into()))?;
    if object.len() != fields.len() || fields.iter().any(|field| !object.contains_key(*field)) {
        return Err(CatalogError::CorruptRecord("closed object fields".into()));
    }
    Ok(())
}

struct Pending {
    context: WriteContext,
    sequence: u64,
    predecessor: Option<(String, u64, u64)>,
    new_stores: BTreeSet<u64>,
    new_products: Vec<u64>,
    written: BTreeMap<String, Vec<RecordKey>>,
    record_count: u64,
    record_bytes: u64,
}
#[derive(Default)]
struct UnitUsage {
    unit: Option<u64>,
    records: u64,
    bytes: u64,
    predecessors: u64,
}
struct Inner {
    storage: RefCell<Box<dyn Storage>>,
    config: CatalogConfig,
    limits: ResourceLimits,
    pending: RefCell<Option<Pending>>,
    reading: Cell<bool>,
    usage: RefCell<UnitUsage>,
}
#[derive(Clone)]
pub struct Catalog {
    inner: Rc<Inner>,
}

impl Catalog {
    pub fn disk(config: CatalogConfig) -> CatalogResult<Self> {
        config.validate()?;
        let limits = config.limits()?;
        if let Some(grant) = &config.grant {
            // The writable catalog file must lie inside its granted main allocation.
            if !grant.allocation_covers("catalog_main", "main", &config.path) {
                return Err(CatalogError::ResourceIncomplete(
                    "catalog path is outside its granted main allocation".into(),
                ));
            }
        }
        let mut backend = sqlite::SqliteStorage::open_reserved(
            &config.path,
            number(&config.resource_plan.record, "resident_bytes")?,
            &limits,
        )?;
        backend.reserve_operational_storage(&limits)?;
        Self::with_storage(config, Box::new(backend))
    }
    pub fn memory(
        config: CatalogConfig,
        max_records: usize,
        max_bytes: u64,
    ) -> CatalogResult<Self> {
        config.validate()?;
        Self::with_storage(
            config,
            Box::new(memory::MemoryStorage::new(max_records, max_bytes)?),
        )
    }
    fn with_storage(config: CatalogConfig, storage: Box<dyn Storage>) -> CatalogResult<Self> {
        if let Some(binding) = storage.get_control("binding")? {
            if binding != config.binding() {
                return Err(CatalogError::SourceMismatch("catalog binding".into()));
            }
        } else if storage.sequence()? != 0 {
            return Err(CatalogError::SourceMismatch(
                "committed catalog lacks binding".into(),
            ));
        }
        let limits = config.limits()?;
        Ok(Self {
            inner: Rc::new(Inner {
                storage: RefCell::new(storage),
                config,
                limits,
                pending: RefCell::new(None),
                reading: Cell::new(false),
                usage: RefCell::new(UnitUsage::default()),
            }),
        })
    }
    pub fn source(&self) -> &SourceIdentity {
        &self.inner.config.source
    }
    pub fn session_id(&self) -> uuid::Uuid {
        self.inner.config.session_id
    }
    pub fn resource_plan(&self) -> &Record {
        &self.inner.config.resource_plan
    }
    pub fn active_unit(&self) -> CatalogResult<u64> {
        self.inner
            .pending
            .borrow()
            .as_ref()
            .map(|p| p.context.unit_ordinal)
            .ok_or_else(|| CatalogError::IncompleteExpansion("no active unit".into()))
    }
    pub fn current_transaction(&self) -> CatalogResult<TransactionId> {
        TransactionId::new(self.inner.storage.borrow().sequence()?)
    }
    pub fn is_initialized(&self) -> CatalogResult<bool> {
        Ok(self.inner.storage.borrow().sequence()? != 0)
    }
    pub fn get(&self, kind: &str, key: &RecordKey) -> CatalogResult<Record> {
        self.inner
            .storage
            .borrow()
            .get(kind, key)?
            .ok_or_else(|| CatalogError::MissingDependency(format!("{kind}/{}", key.canonical())))
    }
    pub fn get_id(&self, kind: &str, id: u64) -> CatalogResult<Record> {
        self.get(kind, &RecordKey::Numeric(id))
    }
    pub fn find_canonical(&self, kind: &str, bytes: &str) -> CatalogResult<Option<Record>> {
        self.inner.storage.borrow().find_canonical(kind, bytes)
    }
    pub fn control(&self, key: &str) -> CatalogResult<Option<Value>> {
        self.inner.storage.borrow().get_control(key)
    }
    pub fn set_control(&self, key: &str, value: &Value) -> CatalogResult<()> {
        self.require_write()?;
        self.inner.storage.borrow_mut().set_control(key, value)
    }
    pub fn oracle_edge_for_input(&self, from: u64, input: &str) -> CatalogResult<Option<u64>> {
        self.inner
            .storage
            .borrow()
            .oracle_edge_for_input(from, input)
    }
    pub fn count(&self, kind: &str) -> CatalogResult<u64> {
        self.inner.storage.borrow().count(kind)
    }
    pub fn oldest_pending(&self) -> CatalogResult<Option<(FifoTicket, ProductId)>> {
        self.inner
            .storage
            .borrow()
            .oldest_pending()?
            .map(|(ticket, id)| Ok((FifoTicket::new(ticket)?, ProductId::new(id)?)))
            .transpose()
    }
    pub fn begin_initial(&self, unit: u64) -> CatalogResult<WriteTransaction> {
        if self.is_initialized()? {
            return Err(CatalogError::ConflictingOutcome(
                "Init already persisted".into(),
            ));
        }
        self.begin(
            WriteContext {
                phase: CommitPhase::Initial,
                unit_ordinal: unit,
                expansion: None,
            },
            None,
        )
    }
    pub fn begin_expansion(
        &self,
        ticket: FifoTicket,
        product: ProductId,
        unit: u64,
    ) -> CatalogResult<WriteTransaction> {
        if self.oldest_pending()? != Some((ticket, product)) {
            return Err(CatalogError::IncompleteExpansion(
                "not oldest global FIFO predecessor".into(),
            ));
        }
        let state = self.get_id("Product", product.get())?;
        let context = string(&state.record, "context")?.to_owned();
        let transaction = self.begin(
            WriteContext {
                phase: CommitPhase::Source,
                unit_ordinal: unit,
                expansion: None,
            },
            Some((context, product.get(), ticket.get())),
        )?;
        let expansion = self.allocate("Expansion")?;
        self.inner.storage.borrow_mut().set_expansion(expansion)?;
        self.inner
            .pending
            .borrow_mut()
            .as_mut()
            .expect("active transaction")
            .context
            .expansion = Some(expansion);
        Ok(transaction)
    }
    pub fn begin_temporal(&self, unit: u64) -> CatalogResult<WriteTransaction> {
        if self.oldest_pending()?.is_some() {
            return Err(CatalogError::IncompleteExpansion(
                "temporal publication before complete source closure".into(),
            ));
        }
        self.begin(
            WriteContext {
                phase: CommitPhase::Temporal,
                unit_ordinal: unit,
                expansion: None,
            },
            None,
        )
    }
    fn begin(
        &self,
        context: WriteContext,
        predecessor: Option<(String, u64, u64)>,
    ) -> CatalogResult<WriteTransaction> {
        if self.inner.reading.get() || self.inner.pending.borrow().is_some() {
            return Err(CatalogError::SnapshotMismatch(
                "writer/read snapshot already active".into(),
            ));
        }
        {
            let mut usage = self.inner.usage.borrow_mut();
            if usage.unit.is_some_and(|old| context.unit_ordinal < old) {
                return Err(CatalogError::SnapshotMismatch(
                    "unit ordinal moved backwards".into(),
                ));
            }
            if usage.unit != Some(context.unit_ordinal) {
                *usage = UnitUsage {
                    unit: Some(context.unit_ordinal),
                    ..UnitUsage::default()
                };
            }
            if context.phase == CommitPhase::Source
                && usage.predecessors >= self.inner.limits.unit_predecessors
            {
                return Err(CatalogError::ResourceIncomplete(
                    "unit predecessor allowance exhausted".into(),
                ));
            }
        }
        let sequence = self.inner.storage.borrow_mut().begin(context.clone())?;
        *self.inner.pending.borrow_mut() = Some(Pending {
            context,
            sequence,
            predecessor,
            new_stores: BTreeSet::new(),
            new_products: Vec::new(),
            written: BTreeMap::new(),
            record_count: 0,
            record_bytes: 0,
        });
        let binding = (|| {
            if self.control("binding")?.is_none() {
                self.set_control("binding", &self.inner.config.binding())?;
            }
            Ok(())
        })();
        if let Err(error) = binding {
            let _ = self.inner.storage.borrow_mut().rollback();
            self.inner.pending.borrow_mut().take();
            return Err(error);
        }
        Ok(WriteTransaction {
            catalog: self.clone(),
            finished: false,
        })
    }
    fn require_write(&self) -> CatalogResult<()> {
        if self.inner.reading.get() || self.inner.pending.borrow().is_none() {
            return Err(CatalogError::IncompleteExpansion(
                "no owned atomic writer".into(),
            ));
        }
        Ok(())
    }
    pub fn allocate(&self, kind: &str) -> CatalogResult<u64> {
        self.require_write()?;
        self.inner.storage.borrow_mut().allocate(kind)
    }
    fn append(&self, record: Record) -> CatalogResult<RecordRef> {
        self.require_write()?;
        record.validate()?;
        let phase = self
            .inner
            .pending
            .borrow()
            .as_ref()
            .ok_or_else(|| CatalogError::IncompleteExpansion("append without transaction".into()))?
            .context
            .phase;
        let allowed = match phase {
            CommitPhase::Initial => matches!(
                record.kind.as_str(),
                "PhysicalRows"
                    | "Episode"
                    | "BoundaryObservation"
                    | "Store"
                    | "Product"
                    | "InitialImage"
            ),
            CommitPhase::Source => matches!(
                record.kind.as_str(),
                "PhysicalRows"
                    | "Episode"
                    | "BoundaryObservation"
                    | "Store"
                    | "Product"
                    | "OracleEvidence"
                    | "Applied"
                    | "Candidate"
                    | "Demand"
                    | "ProductEdge"
                    | "FairPair"
                    | "Enabledness"
                    | "Expansion"
            ),
            CommitPhase::Temporal => matches!(
                record.kind.as_str(),
                "MonitorNode"
                    | "MonitorEdge"
                    | "Scc"
                    | "Counterexample"
                    | "Outcome"
                    | "Counts"
                    | "ContextRoot"
            ),
        };
        if record.kind == "Outcome"
            && matches!(
                record.record["actual"].as_str(),
                Some("pending" | "incomplete")
            )
        {
            return Err(CatalogError::IncompleteExpansion(
                "pending/incomplete Outcome belongs only to private progress".into(),
            ));
        }
        if !allowed {
            return Err(CatalogError::IncompleteExpansion(
                "record kind does not belong to active phase".into(),
            ));
        }
        let reference = record.reference()?;
        if let Some(existing) = self
            .inner
            .storage
            .borrow()
            .get(&record.kind, &reference.id)?
        {
            if existing.bytes() != record.bytes() {
                return Err(CatalogError::ConflictingOutcome(
                    "immutable record key".into(),
                ));
            }
            return Ok(reference);
        }
        self.charge_record(&record, true)?;
        self.inner.storage.borrow_mut().put(&record, false)?;
        self.inner
            .pending
            .borrow_mut()
            .as_mut()
            .expect("active writer")
            .written
            .entry(record.kind)
            .or_default()
            .push(reference.id.clone());
        Ok(reference)
    }
    pub fn limits(&self) -> &ResourceLimits {
        &self.inner.limits
    }
    fn charge_record(&self, record: &Record, is_new: bool) -> CatalogResult<()> {
        let allowance = self.inner.limits.allowance(&record.kind)?;
        let bytes = record.encoded_len(
            allowance
                .max_encoded_bytes
                .min(self.inner.limits.resident_bytes / 4),
        )?;
        decoded_json_reservation(&record.bytes(), self.inner.limits.resident_bytes)?;
        if is_new && self.count(&record.kind)? >= allowance.records {
            return Err(CatalogError::ResourceIncomplete(format!(
                "{} lifetime count exhausted",
                record.kind
            )));
        }
        let mut pending = self.inner.pending.borrow_mut();
        let pending = pending.as_mut().ok_or_else(|| {
            CatalogError::IncompleteExpansion("no resource reservation transaction".into())
        })?;
        let count = pending
            .record_count
            .checked_add(u64::from(is_new))
            .ok_or_else(|| {
                CatalogError::ResourceIncomplete("unit record counter overflow".into())
            })?;
        let total = pending.record_bytes.checked_add(bytes).ok_or_else(|| {
            CatalogError::ResourceIncomplete("unit encoded bytes overflow".into())
        })?;
        let usage = self.inner.usage.borrow();
        if usage
            .records
            .checked_add(count)
            .is_none_or(|n| n > self.inner.limits.unit_records)
            || usage
                .bytes
                .checked_add(total)
                .is_none_or(|n| n > self.inner.limits.unit_bytes)
        {
            return Err(CatalogError::ResourceIncomplete(
                "whole atomic quantum exceeds registered unit record/byte allowance".into(),
            ));
        }
        pending.record_count = count;
        pending.record_bytes = total;
        Ok(())
    }
    pub fn unit_usage(&self) -> (u64, u64, u64) {
        let usage = self.inner.usage.borrow();
        (usage.records, usage.bytes, usage.predecessors)
    }
    pub fn active_reservation(&self) -> CatalogResult<(u64, u64)> {
        self.inner
            .pending
            .borrow()
            .as_ref()
            .map(|p| (p.record_count, p.record_bytes))
            .ok_or_else(|| CatalogError::IncompleteExpansion("no active reservation".into()))
    }
    pub fn append_record(&self, kind: &str, value: Value) -> CatalogResult<RecordRef> {
        if matches!(
            kind,
            "Store"
                | "Product"
                | "OracleEvidence"
                | "BoundaryObservation"
                | "Expansion"
                | "FairPair"
        ) {
            return Err(CatalogError::IncompleteExpansion(
                "record requires checked typed append path".into(),
            ));
        }
        self.append(Record::new(kind, value)?)
    }
    pub fn validate_ref(&self, reference: &RecordRef) -> CatalogResult<Record> {
        let record = self.get(&reference.kind, &reference.id)?;
        if record.sha256() != reference.sha256 {
            return Err(CatalogError::CorruptRecord("reference content hash".into()));
        }
        Ok(record)
    }
    fn intern_canonical(&self, kind: &str, value: &str) -> CatalogResult<u64> {
        self.require_write()?;
        serde_json::from_str::<Value>(value)?;
        if let Some(record) = self.find_canonical(kind, value)? {
            return number(&record.record, "id");
        }
        let id = self.allocate(kind)?;
        self.append(Record::new(kind,json!({"id":id,"canonical_utf8":value,"utf8_bytes":value.len(),"sha256":digest(value.as_bytes())}))?)?;
        Ok(id)
    }
    pub fn intern_physical(&self, value: &str) -> CatalogResult<PhysicalRowsId> {
        PhysicalRowsId::new(self.intern_canonical("PhysicalRows", value)?)
    }
    pub fn intern_episode(&self, value: &str) -> CatalogResult<EpisodeId> {
        EpisodeId::new(self.intern_canonical("Episode", value)?)
    }
    pub fn append_checked_boundary(&self, value: CheckedBoundary) -> CatalogResult<BoundaryId> {
        self.require_write()?;
        let mut record = value.0;
        let id = self.allocate("BoundaryObservation")?;
        record["id"] = json!(id);
        let rows = self.get_id("PhysicalRows", number(&record, "physical_rows_id")?)?;
        let episode = self.get_id("Episode", number(&record, "episode_id")?)?;
        let rebuilt = serde_json::to_string(
            &json!({"rows":serde_json::from_str::<Value>(string(&rows.record,"canonical_utf8")?)?,"episode":serde_json::from_str::<Value>(string(&episode.record,"canonical_utf8")?)?}),
        )?;
        if string(&record, "canonical_utf8")? != rebuilt
            || record["source"] != serde_json::to_value(self.source())?
        {
            return Err(CatalogError::CorruptRecord(
                "actual boundary reconstruction/source mismatch".into(),
            ));
        }
        self.append(Record::new("BoundaryObservation", record)?)?;
        BoundaryId::new(id)
    }
    pub fn intern_checked_store(&self, value: CheckedStore) -> CatalogResult<StoreId> {
        self.require_write()?;
        let mut record = value.0;
        if record.get("id").is_some() || record.get("replay_parent_edge").is_some() {
            return Err(CatalogError::CorruptRecord(
                "caller assigned Store identity/link".into(),
            ));
        }
        let boundary = self.get_id(
            "BoundaryObservation",
            number(&record, "omitted_observation_id")?,
        )?;
        if record["physical_rows_id"] != boundary.record["physical_rows_id"]
            || record["episode_id"] != boundary.record["episode_id"]
            || record["canonical_utf8"] != boundary.record["canonical_utf8"]
        {
            return Err(CatalogError::CorruptRecord("checked Store boundary".into()));
        }
        let text = string(&record, "canonical_utf8")?;
        if let Some(old) = self.find_canonical("Store", text)? {
            return StoreId::new(number(&old.record, "id")?);
        }
        let id = self.allocate("Store")?;
        record["id"] = json!(id);
        record["replay_parent_edge"] = Value::Null;
        self.append(Record::new("Store", record)?)?;
        self.inner
            .pending
            .borrow_mut()
            .as_mut()
            .expect("active writer")
            .new_stores
            .insert(id);
        StoreId::new(id)
    }
    pub fn intern_product(
        &self,
        context: &str,
        bytes: &str,
        store: StoreId,
    ) -> CatalogResult<ProductId> {
        self.require_write()?;
        if !CONTEXTS.contains(&context) {
            return Err(CatalogError::CorruptRecord("unknown context".into()));
        }
        let key = canonical(&json!([context, bytes]));
        if let Some(record) = self
            .inner
            .storage
            .borrow()
            .find_canonical("Product", &key)?
        {
            return ProductId::new(number(&record.record, "id")?);
        }
        let state: Value = serde_json::from_str(bytes)?;
        if state["context"] != context
            || state["sql"].as_u64().and_then(|h| h.checked_add(1)) != Some(store.get())
        {
            return Err(CatalogError::CorruptRecord(
                "complete ProductKey/Store mismatch".into(),
            ));
        }
        self.get_id("Store", store.get())?;
        let id = self.allocate("Product")?;
        self.append(Record::new("Product",json!({"id":id,"context":context,"store_id":store.get(),"canonical_utf8":bytes,"sha256":digest(bytes.as_bytes()),"predecessor_edge":null}))?)?;
        self.inner.storage.borrow_mut().enqueue(id)?;
        self.inner
            .pending
            .borrow_mut()
            .as_mut()
            .expect("active writer")
            .new_products
            .push(id);
        ProductId::new(id)
    }
    fn replace_private_link(
        &self,
        kind: &str,
        id: u64,
        field: &str,
        edge: u64,
    ) -> CatalogResult<()> {
        let allowed = {
            let pending = self.inner.pending.borrow();
            let p = pending
                .as_ref()
                .ok_or_else(|| CatalogError::IncompleteExpansion("no writer".into()))?;
            if kind == "Store" {
                p.new_stores.contains(&id)
            } else {
                p.new_products.contains(&id)
            }
        };
        if !allowed {
            return Ok(());
        }
        let mut record = self.get_id(kind, id)?;
        if record.record[field].is_null() {
            record.record[field] = json!(edge);
            record.validate()?;
            self.charge_record(&record, false)?;
            self.inner.storage.borrow_mut().put(&record, true)?;
        }
        Ok(())
    }
    pub fn append_oracle_evidence(&self, mut value: Value) -> CatalogResult<OracleEdgeId> {
        self.require_write()?;
        if value.get("id").is_some() {
            return Err(CatalogError::CorruptRecord(
                "caller assigned Oracle edge".into(),
            ));
        }
        let from = number(&value["applied"], "from_store")?;
        let to = number(&value["applied"], "to_store")?;
        self.get_id("Store", from)?;
        self.get_id("Store", to)?;
        if value["source"] != serde_json::to_value(self.source())? {
            return Err(CatalogError::SourceMismatch(
                "Oracle evidence source".into(),
            ));
        }
        for name in ["before_boundary", "after_boundary", "rollback_boundary"] {
            let reference: RecordRef = serde_json::from_value(value[name].clone())?;
            if reference.kind != "BoundaryObservation" {
                return Err(CatalogError::CorruptRecord("Oracle boundary kind".into()));
            }
            self.validate_ref(&reference)?;
        }
        let id = self.allocate("OracleEvidence")?;
        value["id"] = json!(id);
        self.append(Record::new("OracleEvidence", value)?)?;
        self.replace_private_link("Store", to, "replay_parent_edge", id)?;
        OracleEdgeId::new(id)
    }
    pub fn append_product_edge(&self, mut value: Value) -> CatalogResult<ProductEdgeId> {
        self.require_write()?;
        let from = number(&value, "from_product")?;
        let to = number(&value, "to_product")?;
        let parent = self.get_id("Product", from)?;
        let target = self.get_id("Product", to)?;
        if parent.record["context"] != value["context"]
            || target.record["context"] != value["context"]
        {
            return Err(CatalogError::CorruptRecord("product edge context".into()));
        }
        self.get_id("Candidate", number(&value, "candidate")?)?;
        for field in [
            "primary_edge",
            "resolution_edge",
            "read_edge",
            "cleanup_edge",
        ] {
            if let Some(id) = value[field].as_u64() {
                self.get_id("OracleEvidence", id)?;
            }
        }
        let id = self.allocate("ProductEdge")?;
        value["id"] = json!(id);
        self.append(Record::new("ProductEdge", value)?)?;
        self.replace_private_link("Product", to, "predecessor_edge", id)?;
        ProductEdgeId::new(id)
    }
    pub fn union_fair_pair(
        &self,
        context: &str,
        from: ProductId,
        to: ProductId,
        labels: &[String],
    ) -> CatalogResult<RecordRef> {
        self.require_write()?;
        let key = RecordKey::Text(canonical(&json!([context, from.get(), to.get()])));
        let mut labels = labels.iter().cloned().collect::<BTreeSet<_>>();
        let old = self.inner.storage.borrow().get("FairPair", &key)?;
        if let Some(old) = &old {
            let p = self.inner.pending.borrow();
            let pending = p.as_ref().expect("active writer");
            if pending.predecessor.as_ref().map(|(_, id, _)| *id) != Some(from.get()) {
                return Err(CatalogError::ConflictingOutcome(
                    "committed fair pair mutation".into(),
                ));
            }
            for label in old.record["labels"]
                .as_array()
                .ok_or_else(|| CatalogError::CorruptRecord("fair labels".into()))?
            {
                labels.insert(
                    label
                        .as_str()
                        .ok_or_else(|| CatalogError::CorruptRecord("fair label".into()))?
                        .into(),
                );
            }
        }
        let record = Record::new(
            "FairPair",
            json!({"context":context,"from_product":from.get(),"to_product":to.get(),"labels":labels,"nonstuttering":from!=to}),
        )?;
        let reference = record.reference()?;
        if old.is_some() {
            self.charge_record(&record, false)?;
            self.inner.storage.borrow_mut().put(&record, true)?;
        } else {
            self.append(record)?;
        }
        Ok(reference)
    }
    pub fn append_enabledness(&self, value: Value) -> CatalogResult<RecordRef> {
        self.append(Record::new("Enabledness", value)?)
    }
    /// Mutable pre-upload controller bookkeeping is private, never a data page.
    pub fn record_checkpoint(&self, checkpoint: &Record, work_unit: &Record) -> CatalogResult<()> {
        let phase = if work_unit.record["status"] == "closed_unqualified" {
            SessionCheckpointPhase::ClosedUnqualified
        } else {
            SessionCheckpointPhase::CheckpointedIncomplete
        };
        self.record_checkpoint_phase(checkpoint, work_unit, phase)
    }
    pub fn record_checkpoint_phase(
        &self,
        checkpoint: &Record,
        work_unit: &Record,
        phase: SessionCheckpointPhase,
    ) -> CatalogResult<()> {
        if self.inner.reading.get() || self.inner.pending.borrow().is_some() {
            return Err(CatalogError::SnapshotMismatch(
                "checkpoint bookkeeping while writer/snapshot active".into(),
            ));
        }
        checkpoint.validate()?;
        work_unit.validate()?;
        if checkpoint.kind != "Checkpoint"
            || work_unit.kind != "WorkUnit"
            || checkpoint.record["source"] != json!(self.source())
            || checkpoint.record["session_id"] != json!(self.session_id())
            || checkpoint.record["unit_ordinal"] != work_unit.record["unit_ordinal"]
            || work_unit.record["source"] != json!(self.source())
            || work_unit.record["session_id"] != json!(self.session_id())
            || work_unit.record["resource_plan_sha256"] != self.resource_plan().sha256()
            || checkpoint.record["previous_checkpoint"]
                != work_unit.record["expected_prior_checkpoint"]
            || !matches!(
                work_unit.record["status"].as_str(),
                Some("checkpointed_incomplete" | "closed_unqualified")
            )
            || !work_unit.record["artifact_roots"]
                .as_array()
                .is_some_and(Vec::is_empty)
        {
            return Err(CatalogError::SourceMismatch(
                "private checkpoint/unit binding".into(),
            ));
        }
        let previous = self.latest_checkpoint()?;
        let (expected_sequence, expected_previous) = match previous {
            Some(previous) => (
                number(&previous.record, "sequence")?
                    .checked_add(1)
                    .ok_or_else(|| {
                        CatalogError::NumericDomain("checkpoint sequence exhausted".into())
                    })?,
                json!(previous.sha256()),
            ),
            None => (0, Value::Null),
        };
        if checkpoint.record["sequence"] != expected_sequence
            || checkpoint.record["previous_checkpoint"] != expected_previous
        {
            return Err(CatalogError::SnapshotMismatch(
                "checkpoint chain does not extend latest checkpoint".into(),
            ));
        }
        let snapshot = self.pin_read_snapshot(self.current_transaction()?)?;
        snapshot.check_checkpoint(checkpoint)?;
        snapshot.close()?;
        self.inner.storage.borrow_mut().save_checkpoint(
            checkpoint,
            work_unit,
            &checkpoint_session(checkpoint, work_unit, phase)?,
        )
    }
    pub fn exported_record(&self, kind: &str, key: &RecordKey) -> CatalogResult<Record> {
        if kind == "Checkpoint" {
            return self.checkpoint_record(match key {
                RecordKey::Text(text) => text,
                _ => return Err(CatalogError::CorruptRecord("checkpoint key".into())),
            });
        }
        if kind == "ResourcePlan" && *key == self.resource_plan().key()? {
            return Ok(self.resource_plan().clone());
        }
        if matches!(kind, "Page" | "ManifestNode") {
            let value = self
                .control(&format!("export:{kind}:{}", key.canonical()))?
                .ok_or_else(|| CatalogError::MissingDependency(format!("exported {kind}")))?;
            let record: Record = serde_json::from_value(value)?;
            record.validate()?;
            if record.kind != kind || record.key() != Ok(key.clone()) {
                return Err(CatalogError::CorruptRecord("exported container key".into()));
            }
            return Ok(record);
        }
        self.get(kind, key)
    }
    pub fn register_export(&self, record: &Record) -> CatalogResult<()> {
        if self.inner.reading.get() || self.inner.pending.borrow().is_some() {
            return Err(CatalogError::SnapshotMismatch(
                "export bookkeeping while snapshot/writer active".into(),
            ));
        }
        if !matches!(record.kind.as_str(), "Page" | "ManifestNode")
            || record.record["source"] != json!(self.source())
            || record.record["session_id"] != json!(self.session_id())
        {
            return Err(CatalogError::SourceMismatch(
                "export bookkeeping binding".into(),
            ));
        }
        record.validate()?;
        let allowance = self.inner.limits.allowance(&record.kind)?;
        record.encoded_len(
            allowance
                .max_encoded_bytes
                .min(self.inner.limits.resident_bytes / 4),
        )?;
        let key = format!("export:{}:{}", record.kind, record.key()?.canonical());
        if let Some(old) = self.control(&key)? {
            if old != serde_json::to_value(record)? {
                return Err(CatalogError::ConflictingOutcome(
                    "immutable export changed".into(),
                ));
            }
            return Ok(());
        }
        let count = self
            .control(&format!("export_count:{}", record.kind))?
            .map(|value| {
                value
                    .as_u64()
                    .ok_or_else(|| CatalogError::CorruptRecord("export count".into()))
            })
            .transpose()?
            .unwrap_or(0);
        if count >= allowance.records {
            return Err(CatalogError::ResourceIncomplete(format!(
                "{} lifetime export allowance",
                record.kind
            )));
        }
        self.inner.storage.borrow_mut().save_export(record)
    }
    pub fn latest_checkpoint(&self) -> CatalogResult<Option<Record>> {
        self.control("latest_checkpoint")?
            .map(|value| {
                let record: Record = serde_json::from_value(value)?;
                record.validate()?;
                Ok(record)
            })
            .transpose()
    }
    pub fn checkpoint_record(&self, digest: &str) -> CatalogResult<Record> {
        let value = self
            .control(&format!("checkpoint:{digest}"))?
            .ok_or_else(|| CatalogError::MissingDependency("prior checkpoint".into()))?;
        let record: Record = serde_json::from_value(value)?;
        record.validate()?;
        if record.sha256() != digest {
            return Err(CatalogError::CorruptRecord("checkpoint digest".into()));
        }
        Ok(record)
    }
    pub fn backup_checkpoint_catalog(
        &self,
        checkpoint: &Record,
        destination: &Path,
        limits: &BackupLimits,
    ) -> CatalogResult<Value> {
        checkpoint.validate()?;
        let stored = self.checkpoint_record(&checkpoint.sha256())?;
        if stored.bytes() != checkpoint.bytes() {
            return Err(CatalogError::SnapshotMismatch(
                "checkpoint bookkeeping not fixed".into(),
            ));
        }
        let snapshot = self.pin_read_snapshot(TransactionId::new(number(
            &checkpoint.record,
            "committed_transaction",
        )?)?)?;
        snapshot.check_checkpoint(checkpoint)?;
        self.inner.storage.borrow().check_private(checkpoint)?;
        snapshot.write_consistent_backup(destination, limits)?;
        let (bytes, hash) = hash_file(destination, limits.max_bytes, limits.deadline)?;
        snapshot.check_checkpoint(checkpoint)?;
        let descriptor = json!({"source":self.source(),"session_id":self.session_id(),"unit_ordinal":checkpoint.record["unit_ordinal"],"checkpoint_sha256":checkpoint.sha256(),"previous_checkpoint_sha256":checkpoint.record["previous_checkpoint"],"resource_plan_sha256":checkpoint.record["resource_plan_sha256"],"initial_image_sha256":checkpoint.record["initial_image_id"],"catalog_commit":checkpoint.record["committed_transaction"],"catalog_logical_digest":checkpoint.record["catalog_logical_digest"],"semantic_manifest_root":checkpoint.record["page_root"],"catalog_bytes":bytes,"catalog_sha256":hash,"snapshot_boundary":"post_checkpoint_pre_upload","format":"sqlite3","backup_method":"sqlite_backup_api_v1"});
        snapshot.close()?;
        Ok(descriptor)
    }
    pub fn validation(config: CatalogConfig) -> CatalogResult<Self> {
        config.validate()?;
        let mut limits = config.limits()?;
        let attachment = match &config.grant {
            // A restored image attaches the reservation the parent already holds;
            // it never receives a second main allocation of its own. Its ceiling
            // is the policy's main database cap bounded by that held reservation.
            Some(grant) => {
                let held = grant
                    .attachment_for(&config.path)
                    .ok_or_else(|| {
                        CatalogError::ResourceIncomplete(
                            "restored catalog lacks a read-only attachment grant".into(),
                        )
                    })?
                    .bytes;
                let policy = ResourceLimits::from_plan(&config.resource_plan)?;
                let family = policy.family("catalog_main")?;
                limits.families.insert(
                    "catalog_main".to_owned(),
                    FamilyLimits {
                        database_bytes: family.database_bytes.min(held),
                        main_bytes: held,
                        journal_bytes: grant.limit("catalog_main", "journal").unwrap_or(0),
                        temp_bytes: grant.limit("catalog_main", "temp").unwrap_or(0),
                    },
                );
                Some(held)
            }
            None => None,
        };
        let storage = sqlite::SqliteStorage::open_readonly(
            &config.path,
            number(&config.resource_plan.record, "resident_bytes")?,
            limits,
            attachment,
        )?;
        Self::with_storage(config, Box::new(storage))
    }
    pub fn validate_restore(
        &self,
        checkpoint: &Record,
        expected_bytes: u64,
        expected_hash: &str,
        limits: &BackupLimits,
    ) -> CatalogResult<Value> {
        let (bytes, hash) = hash_file(&self.inner.config.path, limits.max_bytes, limits.deadline)?;
        if bytes != expected_bytes || hash != expected_hash {
            return Err(CatalogError::CorruptRecord(
                "staged catalog full-file identity".into(),
            ));
        }
        self.inner.storage.borrow().check_private(checkpoint)?;
        let stored = self.checkpoint_record(&checkpoint.sha256())?;
        if stored.bytes() != checkpoint.bytes() {
            return Err(CatalogError::SnapshotMismatch(
                "restored checkpoint metadata".into(),
            ));
        }
        let snapshot = self.pin_read_snapshot(TransactionId::new(number(
            &checkpoint.record,
            "committed_transaction",
        )?)?)?;
        snapshot.check_checkpoint(checkpoint)?;
        let counts = snapshot.counts()?;
        let contexts = snapshot.context_snapshots()?;
        snapshot.close()?;
        if hash_file(&self.inner.config.path, limits.max_bytes, limits.deadline)?
            != (bytes, hash.clone())
        {
            return Err(CatalogError::CorruptRecord(
                "staged catalog changed during validation".into(),
            ));
        }
        Ok(
            json!({"schema_version":6,"operation":"validate_restore","source":self.source(),"session_id":self.session_id(),"checkpoint_sha256":checkpoint.sha256(),"catalog_sha256":hash,"catalog_bytes":bytes,"catalog_commit":checkpoint.record["committed_transaction"],"catalog_logical_digest":checkpoint.record["catalog_logical_digest"],"semantic_manifest_root":checkpoint.record["page_root"],"counts":counts,"contexts":contexts,"initial_image_sha256":checkpoint.record["initial_image_id"],"resource_plan_sha256":checkpoint.record["resource_plan_sha256"]}),
        )
    }
    pub fn pin_read_snapshot(&self, expected: TransactionId) -> CatalogResult<ReadSnapshot> {
        if self.inner.pending.borrow().is_some() || self.inner.reading.replace(true) {
            return Err(CatalogError::SnapshotMismatch(
                "writer/reader active".into(),
            ));
        }
        if let Err(error) = self.inner.storage.borrow_mut().begin_read(expected.get()) {
            self.inner.reading.set(false);
            return Err(error);
        }
        Ok(ReadSnapshot {
            catalog: self.clone(),
            sequence: expected,
            closed: false,
        })
    }
}

/// Bounded streaming digest for actual images/artifacts, never a whole-file Vec.
pub fn hash_file(
    path: &Path,
    max_bytes: u64,
    deadline: std::time::Instant,
) -> CatalogResult<(u64, String)> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > max_bytes {
        return Err(CatalogError::ResourceIncomplete(
            "invalid/over-budget file".into(),
        ));
    }
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut bytes = 0u64;
    loop {
        if std::time::Instant::now() >= deadline {
            return Err(CatalogError::ResourceIncomplete(
                "file digest deadline".into(),
            ));
        }
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes = bytes
            .checked_add(read as u64)
            .ok_or_else(|| CatalogError::NumericDomain("file length overflow".into()))?;
        if bytes > max_bytes {
            return Err(CatalogError::ResourceIncomplete(
                "file grew beyond bound".into(),
            ));
        }
        hash.update(&buffer[..read]);
    }
    if bytes != metadata.len() {
        return Err(CatalogError::CorruptRecord(
            "file changed while hashing".into(),
        ));
    }
    Ok((bytes, format!("{:x}", hash.finalize())))
}

/// Private checked inputs are constructed by the actual Oracle only. They do
/// not carry catalog-assigned identities or invented transition predicates.
pub struct CheckedBoundary(Value);
pub struct CheckedStore(Value);
pub(super) fn checked_boundary(value: Value) -> CheckedBoundary {
    CheckedBoundary(value)
}
pub(super) fn checked_store(value: Value) -> CheckedStore {
    CheckedStore(value)
}

pub struct WriteTransaction {
    catalog: Catalog,
    finished: bool,
}
impl WriteTransaction {
    pub fn expansion_id(&self) -> CatalogResult<ExpansionId> {
        ExpansionId::new(
            self.catalog
                .inner
                .pending
                .borrow()
                .as_ref()
                .and_then(|p| p.context.expansion)
                .ok_or_else(|| CatalogError::IncompleteExpansion("not source expansion".into()))?,
        )
    }
    pub fn discovered_products(&self) -> Vec<u64> {
        self.catalog
            .inner
            .pending
            .borrow()
            .as_ref()
            .expect("active writer")
            .new_products
            .clone()
    }
    pub fn written_ids(&self, kind: &str) -> Vec<RecordKey> {
        self.catalog
            .inner
            .pending
            .borrow()
            .as_ref()
            .expect("active writer")
            .written
            .get(kind)
            .cloned()
            .unwrap_or_default()
    }
    pub fn seal_initial(
        mut self,
        initial_image: Record,
        contexts: &[(String, u64)],
    ) -> CatalogResult<TransactionId> {
        if contexts
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>()
            != CONTEXTS
        {
            return Err(CatalogError::IncompleteExpansion(
                "initial contexts/order".into(),
            ));
        }
        self.catalog.append(initial_image.clone())?;
        self.catalog
            .set_control("initial_image", &serde_json::to_value(initial_image)?)?;
        for (context, id) in contexts {
            self.catalog
                .inner
                .storage
                .borrow_mut()
                .set_initial(context, *id)?;
        }
        self.finish(true)
    }
    pub fn seal(mut self, seal: Value) -> CatalogResult<TransactionId> {
        let record = Record::new("Expansion", seal)?;
        let (context, pred, ticket, expansion, unit) = {
            let p = self.catalog.inner.pending.borrow();
            let p = p.as_ref().expect("active writer");
            let (context, pred, ticket) = p.predecessor.clone().ok_or_else(|| {
                CatalogError::IncompleteExpansion("not predecessor transaction".into())
            })?;
            (
                context,
                pred,
                ticket,
                p.context.expansion,
                p.context.unit_ordinal,
            )
        };
        if record.record["context"] != context
            || record.record["predecessor"] != pred
            || record.record["fifo_ticket"] != ticket
            || record.record["id"].as_u64() != expansion
            || record.record["unit_ordinal"] != unit
        {
            return Err(CatalogError::IncompleteExpansion(
                "expansion identity".into(),
            ));
        }
        for (field, kind) in [
            ("candidates", "Candidate"),
            ("demands", "Demand"),
            ("product_edges", "ProductEdge"),
        ] {
            let expected = self
                .written_ids(kind)
                .iter()
                .map(|key| match key {
                    RecordKey::Numeric(id) => Ok(*id),
                    _ => Err(CatalogError::CorruptRecord("numeric expansion ID".into())),
                })
                .collect::<CatalogResult<Vec<_>>>()?;
            if record.record[field] != json!(expected) {
                return Err(CatalogError::IncompleteExpansion(format!(
                    "incomplete {kind} list"
                )));
            }
        }
        let mut evidence = Vec::new();
        let mut evidence_seen = BTreeSet::new();
        let mut capacity_targets = BTreeSet::new();
        for key in self.written_ids("Demand") {
            let demand = self.catalog.get("Demand", &key)?;
            let candidate = self
                .catalog
                .get_id("Candidate", number(&demand.record, "candidate")?)?;
            if candidate.record["predecessor"] != pred
                || candidate.record["context"] != context
                || demand.record["ordinal"] != 0
            {
                return Err(CatalogError::IncompleteExpansion(
                    "demand/candidate predecessor or ordinal".into(),
                ));
            }
            if demand.record["classification"] == "eligible" {
                let id = number(&demand.record, "outcome_id")?;
                let edge = self.catalog.get_id("OracleEvidence", id)?;
                if edge.record["applied"]["from_store"] != demand.record["from_store"] {
                    return Err(CatalogError::IncompleteExpansion(
                        "demand source outcome".into(),
                    ));
                }
                if evidence_seen.insert(id) {
                    evidence.push(id);
                }
                if demand.record["role"] == "capacity" {
                    capacity_targets.insert(number(&edge.record["applied"], "to_store")?);
                }
            }
        }
        if record.record["oracle_evidence"] != json!(evidence) {
            return Err(CatalogError::IncompleteExpansion(
                "complete first-use Oracle outcome union".into(),
            ));
        }
        for key in self.written_ids("OracleEvidence") {
            if !matches!(key,RecordKey::Numeric(id) if evidence_seen.contains(&id)) {
                return Err(CatalogError::IncompleteExpansion(
                    "unreferenced new Oracle outcome".into(),
                ));
            }
        }
        let mut targets = BTreeSet::new();
        let mut target_stores = BTreeSet::new();
        for key in self.written_ids("ProductEdge") {
            let edge = self.catalog.get("ProductEdge", &key)?;
            if edge.record["from_product"] != pred || edge.record["context"] != context {
                return Err(CatalogError::IncompleteExpansion(
                    "product edge predecessor".into(),
                ));
            }
            let target = number(&edge.record, "to_product")?;
            targets.insert(target);
            target_stores.insert(number(
                &self.catalog.get_id("Product", target)?.record,
                "store_id",
            )?);
        }
        let mut pair_refs = Vec::new();
        let mut labels = BTreeSet::new();
        for target in targets {
            let pair = self.catalog.get(
                "FairPair",
                &RecordKey::Text(canonical(&json!([context, pred, target]))),
            )?;
            if pair.record["nonstuttering"] != json!(pred != target) {
                return Err(CatalogError::IncompleteExpansion(
                    "nonstuttering pair identity".into(),
                ));
            }
            if pred != target {
                for label in pair.record["labels"]
                    .as_array()
                    .ok_or_else(|| CatalogError::CorruptRecord("pair labels".into()))?
                {
                    labels.insert(
                        label
                            .as_str()
                            .ok_or_else(|| CatalogError::CorruptRecord("pair label".into()))?
                            .to_owned(),
                    );
                }
            }
            pair_refs.push(pair.reference()?);
        }
        if record.record["fair_pairs"] != json!(pair_refs) {
            return Err(CatalogError::IncompleteExpansion(
                "complete target-ordered fair-pair union".into(),
            ));
        }
        let capacity_only = capacity_targets
            .difference(&target_stores)
            .copied()
            .collect::<Vec<_>>();
        if record.record["capacity_only_stores"] != json!(capacity_only) {
            return Err(CatalogError::IncompleteExpansion(
                "capacity-only Store identity union".into(),
            ));
        }
        if record.record["discovered_products"] != json!(self.discovered_products()) {
            return Err(CatalogError::IncompleteExpansion(
                "discovery FIFO list".into(),
            ));
        }
        let en: RecordRef = serde_json::from_value(record.record["enabledness"].clone())?;
        let enabled = self.catalog.validate_ref(&en)?;
        if enabled.record["full_graph_labels"] != json!(labels) {
            return Err(CatalogError::IncompleteExpansion(
                "full-graph nonstuttering enabledness union".into(),
            ));
        }
        if enabled.kind != "Enabledness" || enabled.record["product_id"] != pred {
            return Err(CatalogError::IncompleteExpansion(
                "enabledness predecessor".into(),
            ));
        }
        self.catalog.append(record)?;
        self.catalog
            .inner
            .storage
            .borrow_mut()
            .mark_expanded(pred)?;
        self.finish(false)
    }
    pub fn seal_temporal(mut self) -> CatalogResult<TransactionId> {
        if self
            .catalog
            .inner
            .pending
            .borrow()
            .as_ref()
            .expect("active writer")
            .context
            .phase
            != CommitPhase::Temporal
        {
            return Err(CatalogError::IncompleteExpansion(
                "not temporal commit".into(),
            ));
        }
        self.finish(false)
    }
    fn finish(&mut self, initial: bool) -> CatalogResult<TransactionId> {
        self.check_links(initial)?;
        let expected = self
            .catalog
            .inner
            .pending
            .borrow()
            .as_ref()
            .expect("active writer")
            .sequence;
        let sequence = self.catalog.inner.storage.borrow_mut().commit()?;
        if sequence != expected {
            return Err(CatalogError::SnapshotMismatch(
                "commit sequence changed".into(),
            ));
        }
        {
            let pending = self.catalog.inner.pending.borrow();
            let pending = pending.as_ref().expect("committed owned writer");
            let mut usage = self.catalog.inner.usage.borrow_mut();
            usage.records = usage
                .records
                .checked_add(pending.record_count)
                .ok_or_else(|| CatalogError::NumericDomain("unit record count".into()))?;
            usage.bytes = usage
                .bytes
                .checked_add(pending.record_bytes)
                .ok_or_else(|| CatalogError::NumericDomain("unit bytes".into()))?;
            usage.predecessors += u64::from(pending.context.phase == CommitPhase::Source);
        }
        self.catalog.inner.pending.borrow_mut().take();
        self.finished = true;
        TransactionId::new(sequence)
    }
    fn check_links(&self, initial: bool) -> CatalogResult<()> {
        let (stores, products) = {
            let p = self.catalog.inner.pending.borrow();
            let p = p.as_ref().expect("active writer");
            (p.new_stores.clone(), p.new_products.clone())
        };
        for id in stores {
            let mut current = id;
            let mut seen = BTreeSet::new();
            loop {
                if !seen.insert(current) {
                    return Err(CatalogError::IncompleteExpansion(
                        "unrooted Store creating-link cycle".into(),
                    ));
                }
                let state = self.catalog.get_id("Store", current)?;
                if let Some(edge) = state.record["replay_parent_edge"].as_u64() {
                    let edge = self.catalog.get_id("OracleEvidence", edge)?;
                    if edge.record["applied"]["to_store"] != current {
                        return Err(CatalogError::IncompleteExpansion(
                            "creating edge target".into(),
                        ));
                    }
                    current = number(&edge.record["applied"], "from_store")?;
                } else if current == 1
                    && (initial || self.catalog.control("initial_image")?.is_some())
                {
                    break;
                } else {
                    return Err(CatalogError::IncompleteExpansion(
                        "new Store lacks rooted creating edge".into(),
                    ));
                }
            }
        }
        for id in products {
            let state = self.catalog.get_id("Product", id)?;
            if let Some(edge) = state.record["predecessor_edge"].as_u64() {
                let edge = self.catalog.get_id("ProductEdge", edge)?;
                if edge.record["to_product"] != id
                    || edge.record["context"] != state.record["context"]
                {
                    return Err(CatalogError::IncompleteExpansion(
                        "discovery edge target/context".into(),
                    ));
                }
            } else if !initial {
                return Err(CatalogError::IncompleteExpansion(
                    "new Product lacks discovery edge".into(),
                ));
            }
        }
        Ok(())
    }
    pub fn abort(mut self) -> CatalogResult<()> {
        self.catalog.inner.storage.borrow_mut().rollback()?;
        self.catalog.inner.pending.borrow_mut().take();
        self.finished = true;
        Ok(())
    }
}
impl Drop for WriteTransaction {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.catalog.inner.storage.borrow_mut().rollback();
            self.catalog.inner.pending.borrow_mut().take();
        }
    }
}

pub struct ReadSnapshot {
    catalog: Catalog,
    sequence: TransactionId,
    closed: bool,
}
impl ReadSnapshot {
    pub fn transaction(&self) -> TransactionId {
        self.sequence
    }
    pub fn read_records(
        &self,
        range: RecordRange,
        limits: ReadLimits,
    ) -> CatalogResult<RecordBatch> {
        if self.closed || range.through_transaction != self.sequence {
            return Err(CatalogError::SnapshotMismatch(
                "read range high-water".into(),
            ));
        }
        if limits.max_records == 0
            || limits.max_encoded_bytes == 0
            || limits.max_encoded_bytes > PAGE_BYTES
        {
            return Err(CatalogError::ResourceIncomplete(
                "unbounded read request".into(),
            ));
        }
        self.catalog
            .inner
            .storage
            .borrow()
            .scan(&range.kind, range.after.as_ref(), 0, &limits)
    }
    pub fn counts(&self) -> CatalogResult<Value> {
        let storage = self.catalog.inner.storage.borrow();
        let mut counts = serde_json::Map::new();
        for (field, kind) in [
            ("physical_rows", "PhysicalRows"),
            ("episodes", "Episode"),
            ("stores", "Store"),
            ("oracle_edges", "OracleEvidence"),
            ("products", "Product"),
            ("candidates", "Candidate"),
            ("demands", "Demand"),
            ("product_edges", "ProductEdge"),
            ("fair_pairs", "FairPair"),
            ("enabledness", "Enabledness"),
            ("expansions", "Expansion"),
            ("monitor_nodes", "MonitorNode"),
            ("monitor_edges", "MonitorEdge"),
            ("components", "Scc"),
            ("boundaries", "BoundaryObservation"),
        ] {
            counts.insert(field.into(), json!(storage.count(kind)?));
        }
        counts.insert("frontier".into(), json!(storage.pending(None)?));
        Ok(Value::Object(counts))
    }
    pub fn context_snapshots(&self) -> CatalogResult<Vec<Value>> {
        let storage = self.catalog.inner.storage.borrow();
        let mut out = Vec::new();
        for context in CONTEXTS {
            let initial = storage
                .initial(context)?
                .ok_or_else(|| CatalogError::CorruptRecord("missing context Init".into()))?;
            let mut after = None;
            let mut reachable = 0u64;
            let mut expanded = 0u64;
            loop {
                let batch = storage.scan(
                    "Product",
                    after.as_ref(),
                    0,
                    &ReadLimits {
                        max_records: 64,
                        max_encoded_bytes: PAGE_BYTES,
                    },
                )?;
                for product in &batch.records {
                    if product.record["context"] == context {
                        reachable += 1;
                    }
                }
                after = batch.next_after;
                if after.is_none() {
                    break;
                }
            }
            let pending = storage.pending(Some(context))?;
            expanded = expanded
                .checked_add(reachable.checked_sub(pending).ok_or_else(|| {
                    CatalogError::CorruptRecord("frontier exceeds context".into())
                })?)
                .ok_or_else(|| CatalogError::NumericDomain("context counts".into()))?;
            out.push(json!({"context":context,"initial_product":initial,"reachable_products":reachable,"expanded_products":expanded,"frontier":pending}));
        }
        Ok(out)
    }
    pub fn logical_digest(&self) -> CatalogResult<String> {
        use sha2::{Digest, Sha256};
        let mut hash = Sha256::new();
        hash.update(b"plurx-paged-catalog-v6\0");
        hash.update(canonical(&json!({"source":self.catalog.source(),"session_id":self.catalog.session_id(),"resource_plan_sha256":self.catalog.resource_plan().sha256(),"transaction":self.sequence.get()})).as_bytes());
        for kind in record_kinds().iter().filter(|kind| {
            !matches!(
                **kind,
                "Page"
                    | "ManifestNode"
                    | "Checkpoint"
                    | "Session"
                    | "WorkUnit"
                    | "ArtifactReceipt"
                    | "Completion"
            )
        }) {
            let mut after = None;
            loop {
                let batch = self.catalog.inner.storage.borrow().scan(
                    kind,
                    after.as_ref(),
                    0,
                    &ReadLimits {
                        max_records: 64,
                        max_encoded_bytes: PAGE_BYTES,
                    },
                )?;
                for record in batch.records {
                    let bytes = record.bytes();
                    hash.update((bytes.len() as u64).to_be_bytes());
                    hash.update(bytes);
                }
                after = batch.next_after;
                if after.is_none() {
                    break;
                }
            }
        }
        hash.update(canonical(&self.counts()?).as_bytes());
        hash.update(canonical(&self.context_snapshots()?).as_bytes());
        Ok(format!("{:x}", hash.finalize()))
    }
    pub fn check_checkpoint(&self, checkpoint: &Record) -> CatalogResult<()> {
        if checkpoint.record["source"] != json!(self.catalog.source())
            || checkpoint.record["session_id"] != json!(self.catalog.session_id())
            || checkpoint.record["resource_plan_sha256"] != self.catalog.resource_plan().sha256()
            || checkpoint.record["committed_transaction"] != self.sequence.get()
            || checkpoint.record["catalog_logical_digest"] != self.logical_digest()?
            || checkpoint.record["counts"] != self.counts()?
            || checkpoint.record["contexts"] != json!(self.context_snapshots()?)
            || self
                .catalog
                .control("initial_image")?
                .as_ref()
                .map(|image| &image["record"]["image_sha256"])
                != Some(&checkpoint.record["initial_image_id"])
            || checkpoint.record["oldest_pending_ticket"]
                != json!(self
                    .catalog
                    .oldest_pending()?
                    .map(|(ticket, _)| ticket.get()))
        {
            return Err(CatalogError::SnapshotMismatch(
                "checkpoint exact source/logical/count/context snapshot".into(),
            ));
        }
        Ok(())
    }
    pub fn write_consistent_backup(&self, path: &Path, limits: &BackupLimits) -> CatalogResult<()> {
        if self.closed {
            return Err(CatalogError::SnapshotUnavailable("closed snapshot".into()));
        }
        self.catalog.inner.storage.borrow().backup(path, limits)
    }
    pub fn close(mut self) -> CatalogResult<()> {
        self.catalog.inner.storage.borrow_mut().end_read()?;
        self.catalog.inner.reading.set(false);
        self.closed = true;
        Ok(())
    }
}
impl Drop for ReadSnapshot {
    fn drop(&mut self) {
        if !self.closed {
            let _ = self.catalog.inner.storage.borrow_mut().end_read();
            self.catalog.inner.reading.set(false);
        }
    }
}

#[derive(Clone, Debug)]
pub struct ExportedSnapshot {
    pub root: RecordRef,
    pub members: Vec<String>,
}
struct ExportBuilder<'a> {
    snapshot: &'a ReadSnapshot,
    output: &'a Path,
    work: rusqlite::Connection,
    limits: &'a PageLimits,
    deadline: std::time::Instant,
    members: Vec<String>,
    bytes: u64,
}
impl ExportBuilder<'_> {
    fn check(&self) -> CatalogResult<()> {
        if std::time::Instant::now() >= self.deadline {
            return Err(CatalogError::ResourceIncomplete(
                "export unit deadline".into(),
            ));
        }
        let pages: u64 = self
            .work
            .query_row("PRAGMA page_count", [], |row| sqlite::row_uint(row, 0))?;
        let size: u64 = self
            .work
            .query_row("PRAGMA page_size", [], |row| sqlite::row_uint(row, 0))?;
        let ceiling = self
            .snapshot
            .catalog
            .inner
            .limits
            .family("catalog_export_scratch")?
            .database_bytes;
        if pages.checked_mul(size).is_none_or(|bytes| bytes > ceiling) {
            return Err(CatalogError::ResourceIncomplete(
                "export closure scratch allowance".into(),
            ));
        }
        Ok(())
    }
    fn resolve(&self, kind: &str, key: &RecordKey) -> CatalogResult<Record> {
        use rusqlite::OptionalExtension;
        let path: Option<String> = self
            .work
            .query_row(
                "SELECT path FROM exports WHERE kind=?1 AND key=?2",
                rusqlite::params![kind, key.canonical()],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(path) = path {
            let path = self.output.join(path);
            let size = path.metadata()?.len();
            if size > self.snapshot.catalog.inner.limits.resident_bytes / 4 {
                return Err(CatalogError::ResourceIncomplete(
                    "export dependency decoded reservation".into(),
                ));
            }
            let record = Record::parse(&std::fs::read(path)?)?;
            if record.kind != kind || record.key() != Ok(key.clone()) {
                return Err(CatalogError::CorruptRecord(
                    "new export dependency identity".into(),
                ));
            }
            return Ok(record);
        }
        self.snapshot.catalog.exported_record(kind, key)
    }
    fn reference(&self, link: RecordLink) -> CatalogResult<RecordRef> {
        let sha = if record_kinds().contains(&link.kind.as_str()) {
            let actual = self.resolve(&link.kind, &link.key)?;
            let sha = actual.sha256();
            if link
                .sha256
                .as_ref()
                .is_some_and(|expected| *expected != sha)
            {
                return Err(CatalogError::CorruptRecord(
                    "export dependency exact hash mismatch".into(),
                ));
            }
            sha
        } else {
            link.sha256
                .ok_or_else(|| CatalogError::MissingDependency("unhashed external member".into()))?
        };
        Ok(RecordRef {
            kind: link.kind,
            id: link.key,
            sha256: sha,
        })
    }
    fn dependencies(&self, records: &[Record]) -> CatalogResult<Vec<RecordRef>> {
        let local = records
            .iter()
            .map(|record| Ok((record.kind.clone(), record.key()?)))
            .collect::<CatalogResult<BTreeSet<_>>>()?;
        let mut dependencies = BTreeMap::new();
        for record in records {
            for link in record_links(record)? {
                if local.contains(&(link.kind.clone(), link.key.clone())) {
                    continue;
                }
                let reference = self.reference(link)?;
                let key = (reference.kind.clone(), reference.id.clone());
                if let Some(old) = dependencies.insert(key, reference.clone()) {
                    if old != reference {
                        return Err(CatalogError::CorruptRecord(
                            "dependency key has unequal full hashes".into(),
                        ));
                    }
                }
            }
        }
        Ok(dependencies.into_values().collect())
    }
    fn emit(&mut self, record: &Record) -> CatalogResult<RecordRef> {
        use std::io::Write;
        self.check()?;
        match self.resolve(&record.kind, &record.key()?) {
            Ok(existing) => {
                if existing.bytes() != record.bytes() {
                    return Err(CatalogError::ConflictingOutcome(
                        "addressed export key has unequal full bytes".into(),
                    ));
                }
                return existing.reference();
            }
            Err(CatalogError::MissingDependency(_)) => {}
            Err(error) => return Err(error),
        }
        let limits = &self.snapshot.catalog.inner.limits;
        let cap = if record.kind == "ManifestNode" {
            self.limits.max_manifest_node_bytes
        } else {
            self.limits.max_member_bytes
        };
        let size = record.encoded_len(
            cap.min(limits.allowance(&record.kind)?.max_encoded_bytes)
                .min(limits.resident_bytes / 4),
        )?;
        let next = self
            .bytes
            .checked_add(size)
            .filter(|bytes| *bytes <= limits.unit_bytes)
            .ok_or_else(|| CatalogError::ResourceIncomplete("export unit byte allowance".into()))?;
        let reference = record.reference()?;
        let name = format!(
            "{}-{}.json",
            record.kind.to_ascii_lowercase(),
            record.sha256()
        );
        let metadata_bytes = self
            .members
            .iter()
            .try_fold(name.len() as u64, |total, path| {
                total.checked_add(path.len() as u64).ok_or_else(|| {
                    CatalogError::ResourceIncomplete("export member inventory".into())
                })
            })?;
        if metadata_bytes > limits.resident_bytes / 64
            || self.members.len() as u64 >= limits.unit_records
        {
            return Err(CatalogError::ResourceIncomplete(
                "bounded export member inventory".into(),
            ));
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.output.join(&name))?;
        file.write_all(&record.bytes())?;
        file.sync_all()?;
        self.work.execute(
            "INSERT INTO exports(kind,key,path,level) VALUES(?1,?2,?3,?4)",
            rusqlite::params![
                record.kind,
                record.key()?.canonical(),
                name,
                record.record["level"].as_u64().map(|n| n as i64)
            ],
        )?;
        self.members.push(name);
        self.bytes = next;
        Ok(reference)
    }
    fn enqueue(&self, kind: &str, key: &RecordKey, expected: Option<&str>) -> CatalogResult<()> {
        use rusqlite::OptionalExtension;
        self.check()?;
        let old: Option<Option<String>> = self
            .work
            .query_row(
                "SELECT expected FROM closure WHERE kind=?1 AND key=?2",
                rusqlite::params![kind, key.canonical()],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(old) = old {
            if old.as_deref().zip(expected).is_some_and(|(a, b)| a != b) {
                return Err(CatalogError::CorruptRecord(
                    "transitive full-key hash collision".into(),
                ));
            }
            if old.is_none() && expected.is_some() {
                self.work.execute(
                    "UPDATE closure SET expected=?3 WHERE kind=?1 AND key=?2",
                    rusqlite::params![kind, key.canonical(), expected],
                )?;
            }
        } else {
            self.work.execute(
                "INSERT INTO closure(kind,key,expected,done) VALUES(?1,?2,?3,0)",
                rusqlite::params![kind, key.canonical(), expected],
            )?;
        }
        Ok(())
    }
    fn closure_counts(&self, roots: &[RecordRef]) -> CatalogResult<Value> {
        use rusqlite::OptionalExtension;
        self.work.execute("DELETE FROM closure", [])?;
        for root in roots {
            self.enqueue(&root.kind, &root.id, Some(&root.sha256))?;
        }
        loop {
            self.check()?;
            let next: Option<(String, String, Option<String>)> = self
                .work
                .query_row(
                    "SELECT kind,key,expected FROM closure WHERE done=0 ORDER BY kind,key LIMIT 1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            let Some((kind, key, expected)) = next else {
                break;
            };
            let id: RecordKey = serde_json::from_str(&key)?;
            if record_kinds().contains(&kind.as_str()) {
                let record = self.resolve(&kind, &id)?;
                let actual = record.sha256();
                if expected.as_ref().is_some_and(|hash| *hash != actual) {
                    return Err(CatalogError::CorruptRecord(
                        "transitive dependency bytes changed".into(),
                    ));
                }
                if kind == "Page" {
                    for inner in record.record["records"]
                        .as_array()
                        .expect("validated page records")
                    {
                        let inner: Record = serde_json::from_value(inner.clone())?;
                        let reference = inner.reference()?;
                        self.enqueue(&reference.kind, &reference.id, Some(&reference.sha256))?;
                    }
                }
                for link in record_links(&record)? {
                    self.enqueue(&link.kind, &link.key, link.sha256.as_deref())?;
                }
                self.work.execute(
                    "UPDATE closure SET done=1,expected=?3 WHERE kind=?1 AND key=?2",
                    rusqlite::params![kind, key, actual],
                )?;
            } else {
                if expected.is_none() {
                    return Err(CatalogError::MissingDependency(
                        "unhashed external closure leaf".into(),
                    ));
                }
                self.work.execute(
                    "UPDATE closure SET done=1 WHERE kind=?1 AND key=?2",
                    rusqlite::params![kind, key],
                )?;
            }
        }
        let mut counts = serde_json::Map::new();
        for (field, kind) in count_kinds() {
            let count: u64 = self.work.query_row(
                "SELECT count(*) FROM closure WHERE kind=?1",
                [kind],
                |row| sqlite::row_uint(row, 0),
            )?;
            counts.insert(field.into(), json!(count));
        }
        let products = number(&Value::Object(counts.clone()), "products")?;
        let expansions = number(&Value::Object(counts.clone()), "expansions")?;
        counts.insert(
            "frontier".into(),
            json!(products
                .checked_sub(expansions)
                .ok_or_else(|| CatalogError::CorruptRecord(
                    "dependency closure has more expansions than predecessors".into()
                ))?),
        );
        Ok(Value::Object(counts))
    }
    fn manifest(&mut self, level: u64, children: &[RecordRef]) -> CatalogResult<RecordRef> {
        if children.is_empty() || children.len() > 256 {
            return Err(CatalogError::CorruptRecord(
                "bounded nonempty manifest fanout".into(),
            ));
        }
        let counts = self.closure_counts(children)?;
        let record = Record::addressed(
            "ManifestNode",
            json!({"session_id":self.snapshot.catalog.session_id(),"source":self.snapshot.catalog.source(),"sha256":"0".repeat(64),"level":level,"children":children,"counts":counts,"utf8_bytes":0}),
            self.limits.max_manifest_node_bytes,
        )?;
        self.emit(&record)
    }
    fn combine_pages(&mut self, pages: &[RecordRef]) -> CatalogResult<Option<(RecordRef, u64)>> {
        if pages.is_empty() {
            return Ok(None);
        }
        let mut nodes = Vec::new();
        for group in pages.chunks(256) {
            nodes.push(self.manifest(0, group)?);
        }
        let mut level = 0u64;
        while nodes.len() > 1 {
            let mut parents = Vec::new();
            for group in nodes.chunks(256) {
                parents.push(self.manifest(level + 1, group)?);
            }
            nodes = parents;
            level += 1;
        }
        Ok(Some((nodes.pop().expect("nonempty manifest tree"), level)))
    }
}
fn count_kinds() -> [(&'static str, &'static str); 15] {
    [
        ("physical_rows", "PhysicalRows"),
        ("episodes", "Episode"),
        ("stores", "Store"),
        ("oracle_edges", "OracleEvidence"),
        ("products", "Product"),
        ("candidates", "Candidate"),
        ("demands", "Demand"),
        ("product_edges", "ProductEdge"),
        ("fair_pairs", "FairPair"),
        ("enabledness", "Enabledness"),
        ("expansions", "Expansion"),
        ("monitor_nodes", "MonitorNode"),
        ("monitor_edges", "MonitorEdge"),
        ("components", "Scc"),
        ("boundaries", "BoundaryObservation"),
    ]
}
impl ReadSnapshot {
    pub fn export(
        &self,
        output: &Path,
        prior: Option<&Record>,
        limits: &PageLimits,
        deadline: std::time::Instant,
    ) -> CatalogResult<ExportedSnapshot> {
        if self.closed {
            return Err(CatalogError::SnapshotUnavailable(
                "export on closed snapshot".into(),
            ));
        }
        if limits.max_records == 0
            || limits.max_member_bytes == 0
            || limits.max_member_bytes > PAGE_BYTES
            || limits.max_manifest_node_bytes == 0
        {
            return Err(CatalogError::ResourceIncomplete(
                "invalid export page allowances".into(),
            ));
        }
        let resource = &self.catalog.inner.limits;
        let since = if let Some(prior) = prior {
            let stored = self.catalog.checkpoint_record(&prior.sha256())?;
            if stored.bytes() != prior.bytes()
                || prior.record["source"] != json!(self.catalog.source())
                || prior.record["session_id"] != json!(self.catalog.session_id())
            {
                return Err(CatalogError::SnapshotMismatch(
                    "export prior checkpoint binding".into(),
                ));
            }
            number(&prior.record, "committed_transaction")?
        } else {
            0
        };
        if since > self.sequence.get() {
            return Err(CatalogError::SnapshotMismatch(
                "export prior high-water is in future".into(),
            ));
        }
        std::fs::create_dir_all(output)?;
        let scratch = output.join(".manifest-closure.sqlite");
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&scratch)?;
        let work = rusqlite::Connection::open(&scratch)?;
        work.execute_batch(
            "PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; PRAGMA temp_store=FILE;",
        )?;
        work.pragma_update(
            None,
            "cache_size",
            -((resource.resident_bytes / 64 / 1024).clamp(1, i32::MAX as u64) as i64),
        )?;
        let page_size: u64 =
            work.query_row("PRAGMA page_size", [], |row| sqlite::row_uint(row, 0))?;
        let family = resource.family("catalog_export_scratch")?;
        let pages = family.database_bytes / page_size;
        if pages == 0
            || family
                .database_bytes
                .checked_mul(2)
                .and_then(|n| n.checked_add(1024 * 1024))
                .is_none_or(|n| n > family.journal_bytes)
            || family
                .database_bytes
                .checked_mul(2)
                .is_none_or(|n| n > family.temp_bytes)
        {
            return Err(CatalogError::ResourceIncomplete(
                "export prospective database/journal/temp reservation".into(),
            ));
        }
        let effective = work.query_row(&format!("PRAGMA max_page_count={pages}"), [], |row| {
            sqlite::row_uint(row, 0)
        })?;
        if effective != pages {
            return Err(CatalogError::ResourceIncomplete(
                "ineffective export max_page_count".into(),
            ));
        }
        work.execute_batch("CREATE TABLE exports(kind TEXT NOT NULL,key TEXT NOT NULL,path TEXT NOT NULL,level INTEGER,PRIMARY KEY(kind,key)); CREATE TABLE closure(kind TEXT NOT NULL,key TEXT NOT NULL,expected TEXT,done INTEGER NOT NULL,PRIMARY KEY(kind,key)); CREATE INDEX closure_pending ON closure(done,kind,key);")?;
        let mut builder = ExportBuilder {
            snapshot: self,
            output,
            work,
            limits,
            deadline,
            members: Vec::new(),
            bytes: 0,
        };
        let result = (|| {
            let mut pages = Vec::new();
            for kind in record_kinds().iter().filter(|kind| {
                !matches!(
                    **kind,
                    "Page"
                        | "ManifestNode"
                        | "Checkpoint"
                        | "ResourcePlan"
                        | "Session"
                        | "WorkUnit"
                        | "ArtifactReceipt"
                        | "Completion"
                )
            }) {
                let mut after = None;
                let mut previous = self.catalog.control(&format!("last_page:{kind}"))?;
                loop {
                    builder.check()?;
                    let batch = self.catalog.inner.storage.borrow().scan(
                        kind,
                        after.as_ref(),
                        since,
                        &ReadLimits {
                            max_records: limits.max_records,
                            max_encoded_bytes: limits
                                .max_member_bytes
                                .min(resource.resident_bytes / 8),
                        },
                    )?;
                    if batch.records.is_empty() {
                        break;
                    }
                    let original_count = batch.records.len();
                    let mut records = batch.records;
                    let page = loop {
                        let dependencies = builder.dependencies(&records)?;
                        let first = records.first().expect("nonempty page").key()?;
                        let last = records.last().expect("nonempty page").key()?;
                        let page = Record::addressed(
                            "Page",
                            json!({"session_id":self.catalog.session_id(),"source":self.catalog.source(),"page_id":"0".repeat(64),"kind":kind,"first_key":first,"last_key":last,"record_count":records.len(),"utf8_bytes":0,"records":records,"dependencies":dependencies,"previous_page":previous,"previous_checkpoint":prior.map(Record::sha256),"transaction_high_water":self.sequence.get()}),
                            limits
                                .max_member_bytes
                                .min(resource.allowance("Page")?.max_encoded_bytes)
                                .min(resource.resident_bytes / 4),
                        );
                        match page {
                            Ok(page) => break page,
                            Err(CatalogError::ResourceIncomplete(_)) if records.len() > 1 => {
                                records.truncate(records.len() / 2)
                            }
                            Err(error) => return Err(error),
                        }
                    };
                    after = Some(records.last().expect("nonempty page").key()?);
                    previous = Some(page.record["page_id"].clone());
                    pages.push(builder.emit(&page)?);
                    if batch.next_after.is_none() && records.len() == original_count {
                        break;
                    }
                }
            }
            let current = builder.combine_pages(&pages)?;
            let root = match (prior, current) {
                (None, Some((root, _))) => root,
                (Some(previous), None) => builder
                    .resolve(
                        "ManifestNode",
                        &RecordKey::Text(string(&previous.record, "page_root")?.into()),
                    )?
                    .reference()?,
                (Some(previous), Some((mut new, mut new_level))) => {
                    let old_record = builder.resolve(
                        "ManifestNode",
                        &RecordKey::Text(string(&previous.record, "page_root")?.into()),
                    )?;
                    let mut old = old_record.reference()?;
                    let mut old_level = number(&old_record.record, "level")?;
                    while old_level < new_level {
                        old = builder.manifest(old_level + 1, &[old])?;
                        old_level += 1;
                    }
                    while new_level < old_level {
                        new = builder.manifest(new_level + 1, &[new])?;
                        new_level += 1;
                    }
                    builder.manifest(old_level + 1, &[old, new])?
                }
                (None, None) => {
                    return Err(CatalogError::IncompleteExpansion(
                        "no actual Init records to export".into(),
                    ))
                }
            };
            if builder.closure_counts(std::slice::from_ref(&root))? != self.counts()? {
                return Err(CatalogError::SnapshotMismatch(
                    "root transitive counts differ from exact source snapshot".into(),
                ));
            }
            Ok(ExportedSnapshot {
                root,
                members: builder.members.clone(),
            })
        })();
        drop(builder);
        std::fs::remove_file(&scratch)?;
        result
    }
}

/// Private controller request. This object is not an immutable proof envelope
/// and grants no completion authority; M4 authenticates its external origin.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuantumRequest {
    pub schema_version: u64,
    pub operation: String,
    pub deadline_monotonic: Value,
    pub work_unit: Value,
    pub execution_origin: Value,
    pub resource_plan: Record,
    pub initial_image: Option<Record>,
    pub prior_checkpoint: Option<Record>,
    pub checkpoint: Option<Record>,
    pub catalog_path: PathBuf,
    pub output_dir: PathBuf,
    pub dependency_index: PathBuf,
    pub source_files: BTreeMap<String, String>,
    pub runtime: Value,
    pub driver_graph: PathBuf,
    pub tlc: Value,
    pub verifier_work_db: PathBuf,
    pub prepare_path: PathBuf,
    pub reservation_path: PathBuf,
    pub previous_verifier_progress_sha256: Option<String>,
    pub previous_verifier_backup_sha256: Option<String>,
    pub temporal_publication: Option<PathBuf>,
    pub temporal_import: Option<PathBuf>,
}
impl QuantumRequest {
    pub fn read(path: &Path) -> CatalogResult<Self> {
        const CONTROL_BYTES: u64 = 8 * 1024 * 1024;
        if !path.is_absolute()
            || path.symlink_metadata()?.file_type().is_symlink()
            || !path.is_file()
        {
            return Err(CatalogError::Io("unsafe private request path".into()));
        }
        if path.metadata()?.len() > CONTROL_BYTES {
            return Err(CatalogError::ResourceIncomplete(
                "private control decoded allowance".into(),
            ));
        }
        let bytes = std::fs::read(path)?;
        decoded_json_reservation(&bytes, 128 * 1024 * 1024)?;
        let value = strict_json(&bytes)?;
        let base = [
            "schema_version",
            "operation",
            "deadline_monotonic",
            "work_unit",
            "execution_origin",
            "resource_plan",
            "initial_image",
            "prior_checkpoint",
            "checkpoint",
            "catalog_path",
            "output_dir",
            "dependency_index",
            "source_files",
            "runtime",
            "driver_graph",
            "tlc",
            "verifier_work_db",
            "prepare_path",
            "reservation_path",
            "previous_verifier_progress_sha256",
            "previous_verifier_backup_sha256",
        ];
        let mut fields = base.to_vec();
        if value["operation"] == "temporal_import" {
            fields.extend(["temporal_publication", "temporal_import"]);
        }
        closed_fields(&value, &fields)?;
        let request: Self = serde_json::from_value(value)?;
        if request.schema_version != 6
            || !matches!(
                request.operation.as_str(),
                "expand" | "temporal_import" | "validate_restore"
            )
            || !request
                .deadline_monotonic
                .as_f64()
                .is_some_and(|value| value.is_finite() && value > 0.0)
        {
            return Err(CatalogError::SourceMismatch(
                "unsupported private quantum request".into(),
            ));
        }
        for path in [
            &request.catalog_path,
            &request.output_dir,
            &request.dependency_index,
            &request.driver_graph,
            &request.verifier_work_db,
            &request.prepare_path,
            &request.reservation_path,
        ] {
            if !path.is_absolute()
                || path
                    .symlink_metadata()
                    .is_ok_and(|metadata| metadata.file_type().is_symlink())
            {
                return Err(CatalogError::Io("unsafe private quantum path".into()));
            }
        }
        for path in [&request.temporal_publication, &request.temporal_import]
            .into_iter()
            .flatten()
        {
            if !path.is_absolute() || path.symlink_metadata()?.file_type().is_symlink() {
                return Err(CatalogError::Io("unsafe temporal import path".into()));
            }
        }
        let unit = request.unit()?;
        closed_fields(
            &request.execution_origin,
            &[
                "repository",
                "pr",
                "event_base",
                "workflow_id",
                "event_name",
                "mode",
                "operation",
                "run",
                "job",
                "task",
                "attempt",
                "dispatch_ordinal",
            ],
        )?;
        for field in [
            "repository",
            "pr",
            "event_base",
            "workflow_id",
            "event_name",
            "mode",
            "run",
            "job",
            "task",
            "attempt",
        ] {
            if request.execution_origin[field] != unit.record[field] {
                return Err(CatalogError::SourceMismatch(
                    "private execution origin/WorkUnit mismatch".into(),
                ));
            }
        }
        if request.execution_origin["operation"] != "execute" {
            return Err(CatalogError::SourceMismatch(
                "proof operation lacks execute origin".into(),
            ));
        }
        if number(&request.execution_origin, "dispatch_ordinal")?
            >= number(&request.resource_plan.record, "maximum_units")?
        {
            return Err(CatalogError::ResourceIncomplete(
                "total attempted-unit allowance exhausted".into(),
            ));
        }
        let config = request.config()?;
        config.validate()?;
        if unit.record["session_id"] != json!(config.session_id)
            || unit.record["resource_plan_sha256"] != config.resource_plan.sha256()
            || unit.record["mode"] != "unit"
            || !unit.record["artifact_roots"]
                .as_array()
                .is_some_and(Vec::is_empty)
        {
            return Err(CatalogError::SourceMismatch(
                "private running unit/plan binding".into(),
            ));
        }
        if request.operation == "expand" && unit.record["stage"] != "expand" {
            return Err(CatalogError::SourceMismatch("expand request stage".into()));
        }
        if request.operation == "temporal_import"
            && (unit.record["stage"] == "expand" || unit.record["stage"] == "complete")
        {
            return Err(CatalogError::SourceMismatch(
                "publication request stage".into(),
            ));
        }
        Ok(request)
    }
    pub fn unit(&self) -> CatalogResult<Record> {
        Record::new("WorkUnit", self.work_unit.clone())
    }
    pub fn config(&self) -> CatalogResult<CatalogConfig> {
        let source: SourceIdentity = serde_json::from_value(self.work_unit["source"].clone())?;
        let session_id: uuid::Uuid = serde_json::from_value(self.work_unit["session_id"].clone())?;
        let policy = ResourceLimits::from_plan(&self.resource_plan)?.physical_policy;
        // Every allocation-capable child is spawned with its explicit current grant.
        let grant = grant::ChildGrant::from_environment(&grant::GrantExpectation {
            source: &self.work_unit["source"],
            session_id: &session_id.to_string(),
            resource_plan_sha256: &self.resource_plan.sha256(),
            policy: &policy,
        })?;
        Ok(CatalogConfig {
            path: self.catalog_path.clone(),
            source,
            session_id,
            resource_plan: self.resource_plan.clone(),
            grant: Some(std::sync::Arc::new(grant)),
        })
    }
    pub fn deadline(&self) -> CatalogResult<std::time::Instant> {
        // M4 separately enforces the controller's absolute monotonic deadline.
        // Rust's private Instant has no portable external epoch; this local
        // watchdog can only shorten work relative to the fixed unit duration.
        Ok(std::time::Instant::now()
            + std::time::Duration::from_secs(number(&self.resource_plan.record, "unit_seconds")?))
    }
    pub fn verify_source(
        &self,
        compiled: &BTreeMap<String, String>,
        deadline: std::time::Instant,
    ) -> CatalogResult<()> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()?;
        let mut files = Vec::new();
        for (name, advertised) in &self.source_files {
            let relative = Path::new(name);
            if relative
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
            {
                return Err(CatalogError::SourceMismatch(
                    "unsafe source closure relative path".into(),
                ));
            }
            validate_definition("Sha256", &json!(advertised))?;
            let path = root.join(relative);
            if path.symlink_metadata()?.file_type().is_symlink()
                || !path.canonicalize()?.starts_with(&root)
            {
                return Err(CatalogError::SourceMismatch(
                    "source closure leaves archive".into(),
                ));
            }
            let (_, actual) = hash_file(&path, PAGE_BYTES, deadline)?;
            if actual != *advertised {
                return Err(CatalogError::SourceMismatch(format!(
                    "source closure bytes differ: {name}"
                )));
            }
            files.push(json!([name, actual]));
        }
        let source = &self.config()?.source;
        if files.is_empty() || digest(canonical(&files).as_bytes()) != source.source_closure_sha256
        {
            return Err(CatalogError::SourceMismatch(
                "complete source closure digest".into(),
            ));
        }
        for (name, hash) in compiled {
            if self.source_files.get(name) != Some(hash) {
                return Err(CatalogError::SourceMismatch(format!(
                    "compiled source differs from registered archive: {name}"
                )));
            }
        }
        let names = [
            "PAGED-CONTRACT.md",
            "paged-proof.schema.json",
            "paged-catalog.sql",
            "finite-inventory.json",
            "paged-outcomes.json",
            "paged-catalog-transport.schema.json",
            "paged-verifier-continuation.schema.json",
            "paged-verifier-state.sql",
            "source-bound-formulas.json",
            "source-bound-receipt.schema.json",
            "paged-control-carrier.schema.json",
        ];
        let mut proof = Vec::new();
        for name in names {
            let path = format!("verification/async-driver-refinement/{name}");
            let hash = self.source_files.get(&path).ok_or_else(|| {
                CatalogError::MissingDependency(format!("proof contract source {path}"))
            })?;
            proof.push(json!([path, hash]));
        }
        proof.sort_by(|a, b| a[0].as_str().cmp(&b[0].as_str()));
        if digest(canonical(&proof).as_bytes()) != source.proof_contract_sha256
            || self
                .source_files
                .get("verification/async-driver-refinement/finite-inventory.json")
                != Some(&source.finite_inventory_sha256)
            || self
                .source_files
                .get(".github/workflows/async-proof-session.yml")
                != Some(&source.workflow_sha256)
            || !self.runtime.is_object()
            || digest(canonical(&self.runtime).as_bytes()) != source.toolchain_sha256
        {
            return Err(CatalogError::SourceMismatch(
                "proof/finite/workflow/runtime binding".into(),
            ));
        }
        let (_, driver_hash) = hash_file(&self.driver_graph, PAGE_BYTES, deadline)?;
        if driver_hash != source.driver_sha256 {
            return Err(CatalogError::SourceMismatch(
                "actual driver graph bytes".into(),
            ));
        }
        Ok(())
    }
    pub fn initial_path(&self, deadline: std::time::Instant) -> CatalogResult<PathBuf> {
        let image = self.initial_image.as_ref().ok_or_else(|| {
            CatalogError::InitialImageMismatch("resumed request lacks actual Init".into())
        })?;
        image.validate()?;
        if image.kind != "InitialImage" || image.record["source"] != self.work_unit["source"] {
            return Err(CatalogError::InitialImageMismatch(
                "resumed Init source".into(),
            ));
        }
        let connection = rusqlite::Connection::open_with_flags(
            &self.dependency_index,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let mut statement=connection.prepare("SELECT local_path FROM authenticated_members WHERE member_path=?1 AND bytes=?2 AND sha256=?3 ORDER BY unit_ordinal,part_ordinal")?;
        let mut rows = statement.query(rusqlite::params![
            string(&image.record, "member_path")?,
            number(&image.record, "image_bytes")? as i64,
            string(&image.record, "image_sha256")?
        ])?;
        let mut selected: Option<PathBuf> = None;
        while let Some(row) = rows.next()? {
            let path = PathBuf::from(row.get::<_, String>(0)?);
            if !path.is_absolute() || path.symlink_metadata()?.file_type().is_symlink() {
                return Err(CatalogError::InitialImageMismatch(
                    "unsafe authenticated Init path".into(),
                ));
            }
            let (bytes, hash) = hash_file(&path, number(&image.record, "image_bytes")?, deadline)?;
            if bytes != number(&image.record, "image_bytes")?
                || hash != string(&image.record, "image_sha256")?
            {
                return Err(CatalogError::InitialImageMismatch(
                    "actual authenticated Init bytes".into(),
                ));
            }
            if let Some(prior) = selected.as_ref() {
                same_file_bytes(prior, &path, deadline)?;
            } else {
                selected = Some(path);
            }
        }
        selected.ok_or_else(|| {
            CatalogError::InitialImageMismatch("authenticated Init member unavailable".into())
        })
    }
    pub fn dependency_member(
        &self,
        reference: &RecordRef,
        deadline: std::time::Instant,
    ) -> CatalogResult<PathBuf> {
        use rusqlite::OptionalExtension;
        let connection = rusqlite::Connection::open_with_flags(
            &self.dependency_index,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let row: Option<(String, String)> = connection
            .query_row(
                "SELECT digest,path FROM objects WHERE kind=?1 AND key=?2",
                rusqlite::params![reference.kind, reference.id.canonical().as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (advertised, path) = row.ok_or_else(|| {
            CatalogError::MissingDependency("authenticated external member".into())
        })?;
        let path = PathBuf::from(path);
        if advertised != reference.sha256
            || !path.is_absolute()
            || path.symlink_metadata()?.file_type().is_symlink()
        {
            return Err(CatalogError::CorruptRecord(
                "external member reference/path".into(),
            ));
        }
        let (_, actual) = hash_file(&path, PAGE_BYTES, deadline)?;
        if actual != reference.sha256 {
            return Err(CatalogError::CorruptRecord(
                "actual external member bytes".into(),
            ));
        }
        Ok(path)
    }
}
impl QuantumRequest {
    pub fn expected_backup(
        &self,
        checkpoint: &Record,
        deadline: std::time::Instant,
    ) -> CatalogResult<(u64, String)> {
        let connection = rusqlite::Connection::open_with_flags(
            &self.dependency_index,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let mut statement=connection.prepare("SELECT bytes,sha256,local_path FROM authenticated_snapshot_roots WHERE unit_ordinal=?1 AND kind='catalog_backup'")?;
        let mut rows = statement
            .query(rusqlite::params![
                number(&checkpoint.record, "unit_ordinal")? as i64
            ])?;
        let row = rows.next()?.ok_or_else(|| {
            CatalogError::MissingDependency("authenticated catalog backup root".into())
        })?;
        let bytes = sqlite::row_uint(row, 0)?;
        let advertised: String = row.get(1)?;
        let path = PathBuf::from(row.get::<_, String>(2)?);
        if rows.next()?.is_some()
            || !path.is_absolute()
            || path.symlink_metadata()?.file_type().is_symlink()
            || bytes > 8 * 1024 * 1024
        {
            return Err(CatalogError::CorruptRecord(
                "catalog backup root identity/size".into(),
            ));
        }
        if hash_file(&path, 8 * 1024 * 1024, deadline)? != (bytes, advertised) {
            return Err(CatalogError::CorruptRecord(
                "catalog backup root actual bytes".into(),
            ));
        }
        let root = strict_json(&std::fs::read(&path)?)?;
        closed_fields(
            &root,
            &[
                "schema_version",
                "kind",
                "source",
                "session_id",
                "unit_ordinal",
                "checkpoint_sha256",
                "previous_checkpoint_sha256",
                "resource_plan_sha256",
                "initial_image_sha256",
                "catalog_commit",
                "catalog_logical_digest",
                "semantic_manifest_root",
                "snapshot_boundary",
                "format",
                "backup_method",
                "catalog_bytes",
                "catalog_sha256",
                "chunk_bytes_limit",
                "chunk_count",
                "execution_origin",
                "predecessor_root",
                "manifest_root",
            ],
        )?;
        let c = &checkpoint.record;
        if root["schema_version"] != 2
            || root["kind"] != "catalog_backup"
            || root["source"] != c["source"]
            || root["session_id"] != c["session_id"]
            || root["unit_ordinal"] != c["unit_ordinal"]
            || root["checkpoint_sha256"] != checkpoint.sha256()
            || root["previous_checkpoint_sha256"] != c["previous_checkpoint"]
            || root["resource_plan_sha256"] != c["resource_plan_sha256"]
            || root["initial_image_sha256"] != c["initial_image_id"]
            || root["catalog_commit"] != c["committed_transaction"]
            || root["catalog_logical_digest"] != c["catalog_logical_digest"]
            || root["semantic_manifest_root"] != c["page_root"]
            || root["snapshot_boundary"] != "post_checkpoint_pre_upload"
            || root["format"] != "sqlite3"
            || root["backup_method"] != "sqlite_backup_api_v1"
            || root["chunk_bytes_limit"] != 1024 * 1024
            || number(&root, "catalog_bytes")? == 0
            || number(&root, "chunk_count")? == 0
            || number(&root, "chunk_count")?
                != number(&root, "catalog_bytes")?.div_ceil(1024 * 1024)
        {
            return Err(CatalogError::SourceMismatch(
                "authenticated backup root/checkpoint binding".into(),
            ));
        }
        closed_fields(
            &root["execution_origin"],
            &[
                "repository",
                "pr",
                "event_base",
                "workflow_id",
                "event_name",
                "mode",
                "operation",
                "run",
                "job",
                "task",
                "attempt",
                "dispatch_ordinal",
            ],
        )?;
        if root["execution_origin"]["operation"] != "execute"
            || root["execution_origin"]["attempt"] != 1
        {
            return Err(CatalogError::SourceMismatch(
                "snapshot execution origin differs".into(),
            ));
        }
        number(&root["execution_origin"], "dispatch_ordinal")?;
        for field in ["manifest_root", "predecessor_root"] {
            let reference = &root[field];
            if field == "predecessor_root" && reference.is_null() {
                continue;
            }
            closed_fields(reference, &["kind", "sha256", "bytes"])?;
            validate_definition("Sha256", &reference["sha256"])?;
            if number(reference, "bytes")? == 0
                || field == "manifest_root" && reference["kind"] != "snapshot_manifest_node"
                || field == "predecessor_root" && reference["kind"] != "catalog_backup"
            {
                return Err(CatalogError::SourceMismatch(
                    "snapshot manifest/predecessor content identity".into(),
                ));
            }
        }
        validate_definition("Sha256", &root["catalog_sha256"])?;
        Ok((
            number(&root, "catalog_bytes")?,
            string(&root, "catalog_sha256")?.into(),
        ))
    }
    pub fn check_continuation(&self, catalog: &Catalog) -> CatalogResult<Record> {
        let checkpoint = self.checkpoint.as_ref().ok_or_else(|| {
            CatalogError::SnapshotMismatch("missing continuation checkpoint".into())
        })?;
        checkpoint.validate()?;
        if self.prior_checkpoint.as_ref() != Some(checkpoint)
            || checkpoint.kind != "Checkpoint"
            || checkpoint.record["source"] != self.work_unit["source"]
            || checkpoint.record["session_id"] != self.work_unit["session_id"]
            || checkpoint.record["resource_plan_sha256"] != self.work_unit["resource_plan_sha256"]
            || self.work_unit["expected_prior_checkpoint"] != checkpoint.sha256()
            || number(&self.work_unit, "unit_ordinal")?
                < number(&checkpoint.record, "unit_ordinal")?
                    .checked_add(1)
                    .ok_or_else(|| CatalogError::NumericDomain("unit ordinal exhausted".into()))?
            || catalog.latest_checkpoint()?.as_ref() != Some(checkpoint)
        {
            return Err(CatalogError::SnapshotMismatch(
                "exact continuation checkpoint/unit identity".into(),
            ));
        }
        let image = self.initial_image.as_ref().ok_or_else(|| {
            CatalogError::InitialImageMismatch("continuation lacks actual Init descriptor".into())
        })?;
        if catalog.control("initial_image")? != Some(serde_json::to_value(image)?)
            || image.record["image_sha256"] != checkpoint.record["initial_image_id"]
        {
            return Err(CatalogError::InitialImageMismatch(
                "continuation actual Init descriptor".into(),
            ));
        }
        Ok(checkpoint.clone())
    }
}
pub fn compiled_catalog_sources() -> BTreeMap<String, String> {
    [
        (
            "crates/plurx-core/src/store/async_driver_catalog.rs",
            include_str!("async_driver_catalog.rs"),
        ),
        (
            "crates/plurx-core/src/store/async_driver_catalog/records.rs",
            include_str!("async_driver_catalog/records.rs"),
        ),
        (
            "crates/plurx-core/src/store/async_driver_catalog/sqlite.rs",
            include_str!("async_driver_catalog/sqlite.rs"),
        ),
        (
            "crates/plurx-core/src/store/async_driver_catalog/memory.rs",
            include_str!("async_driver_catalog/memory.rs"),
        ),
        (
            "verification/async-driver-refinement/paged-proof.schema.json",
            include_str!(
                "../../../../verification/async-driver-refinement/paged-proof.schema.json"
            ),
        ),
        (
            "verification/async-driver-refinement/paged-catalog.sql",
            include_str!("../../../../verification/async-driver-refinement/paged-catalog.sql"),
        ),
    ]
    .into_iter()
    .map(|(path, text)| (path.into(), digest(text.as_bytes())))
    .collect()
}
#[derive(Clone, Copy, Debug)]
pub enum SessionCheckpointPhase {
    CheckpointedIncomplete,
    ClosedUnqualified,
    ResourceBlocked,
}
impl SessionCheckpointPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CheckpointedIncomplete => "checkpointed_incomplete",
            Self::ClosedUnqualified => "closed_unqualified",
            Self::ResourceBlocked => "resource_blocked",
        }
    }
}
pub fn checkpoint_session(
    checkpoint: &Record,
    unit: &Record,
    phase: SessionCheckpointPhase,
) -> CatalogResult<Record> {
    let c = &checkpoint.record;
    let u = &unit.record;
    let next = number(c, "unit_ordinal")?
        .checked_add(1)
        .filter(|n| *n <= i64::MAX as u64)
        .ok_or_else(|| CatalogError::NumericDomain("session next unit exhausted".into()))?;
    Record::new(
        "Session",
        json!({"session_id":c["session_id"],"source":c["source"],"pr":u["pr"],"repository":u["repository"],"event_base":u["event_base"],"initial_image_id":c["initial_image_id"],"resource_plan_sha256":c["resource_plan_sha256"],"phase":phase.as_str(),"checkpoint":checkpoint.sha256(),"next_unit":next,"counts":c["counts"]}),
    )
}
pub fn same_file_bytes(
    first: &Path,
    second: &Path,
    deadline: std::time::Instant,
) -> CatalogResult<()> {
    use std::io::Read;
    let mut a = std::fs::File::open(first)?;
    let mut b = std::fs::File::open(second)?;
    if a.metadata()?.len() != b.metadata()?.len() {
        return Err(CatalogError::CorruptRecord(
            "full file identity length collision".into(),
        ));
    }
    let mut x = [0u8; 8192];
    let mut y = [0u8; 8192];
    loop {
        if std::time::Instant::now() >= deadline {
            return Err(CatalogError::ResourceIncomplete(
                "full byte identity deadline".into(),
            ));
        }
        let n = a.read(&mut x)?;
        b.read_exact(&mut y[..n])?;
        if x[..n] != y[..n] {
            return Err(CatalogError::CorruptRecord(
                "equal file digest has unequal full bytes".into(),
            ));
        }
        if n == 0 {
            break;
        }
    }
    Ok(())
}

pub(super) fn fixture_configuration(path: PathBuf) -> CatalogConfig {
    fixture_configuration_shaped(
        path,
        FamilyLimits {
            database_bytes: 64 * 1024 * 1024,
            main_bytes: 128 * 1024 * 1024,
            journal_bytes: 129 * 1024 * 1024,
            temp_bytes: 128 * 1024 * 1024,
        },
    )
}
/// The admitted fixture plan with an explicit `catalog_main` physical policy.
pub(super) fn fixture_configuration_shaped(
    path: PathBuf,
    catalog_main: FamilyLimits,
) -> CatalogConfig {
    let source = SourceIdentity {
        commit: "0".repeat(40),
        tree: "1".repeat(40),
        source_closure_sha256: "2".repeat(64),
        driver_sha256: "3".repeat(64),
        finite_inventory_sha256: "4".repeat(64),
        proof_contract_sha256: "5".repeat(64),
        toolchain_sha256: "6".repeat(64),
        workflow_sha256: "7".repeat(64),
    };
    let session_id = uuid::Uuid::from_u128(1);
    let mut plan: Value = serde_json::from_str(include_str!(
        "../../../../verification/async-driver-refinement/paged-resource-plan.example.json"
    ))
    .expect("example");
    let p = &mut plan["record"];
    p["admission"] = json!("admitted");
    p["session_id"] = json!(session_id);
    p["source"] = json!(source);
    for field in [
        "resident_bytes",
        "resident_rss_bytes",
        "total_session_disk_bytes",
        "total_retained_artifact_bytes",
        "maximum_units",
        "unit_predecessor_allowance",
        "unit_record_allowance",
        "unit_byte_allowance",
    ] {
        p[field] = json!(1_000_000);
    }
    p["available_disk_receipt"] =
        json!({"kind":"runtime_receipt","id":"synthetic-fixture","sha256":"8".repeat(64)});
    p["resident_bytes"] = json!(64 * 1024 * 1024u64);
    p["resident_rss_bytes"] = json!(512 * 1024 * 1024u64);
    p["total_session_disk_bytes"] = json!(8 * 1024 * 1024 * 1024u64);
    p["unit_record_allowance"] = json!(4096);
    p["unit_byte_allowance"] = json!(16 * 1024 * 1024u64);
    let allowances = record_kinds()
        .iter()
        .map(|kind| {
            (
                (*kind).to_owned(),
                json!({"records":256,"max_encoded_bytes":256*1024}),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    p["maximum_units"] = json!(16);
    let mut families = serde_json::Map::new();
    let mut family_total = 0u64;
    for (name, owner) in [
        ("catalog_main", "catalog"),
        ("verifier_main", "verifier"),
        ("catalog_export_scratch", "catalog_export"),
        ("verifier_continuation_scratch", "verifier_continuation"),
        ("catalog_or_verifier_restore_scratch", "snapshot_restore"),
        ("publication_pack_scratch", "snapshot_transport"),
        ("authentication_index", "provenance"),
        ("publication_pending_scratch", "session_controller"),
    ] {
        let shape = if name == "catalog_main" {
            catalog_main.clone()
        } else {
            FamilyLimits {
                database_bytes: 64 * 1024 * 1024,
                main_bytes: 128 * 1024 * 1024,
                journal_bytes: 129 * 1024 * 1024,
                temp_bytes: 128 * 1024 * 1024,
            }
        };
        family_total += shape.main_bytes + shape.journal_bytes + shape.temp_bytes;
        families.insert(name.into(),json!({"owner":owner,"database_bytes":shape.database_bytes,"main_bytes":shape.main_bytes,"journal_bytes":shape.journal_bytes,"temp_bytes":shape.temp_bytes}));
    }
    let source_map = strict_json(include_bytes!(
        "../../../../validation/async_physical_table_map.json"
    ))
    .expect("fixture source map");
    let table_map_sha = digest(canonical(&source_map).as_bytes());
    let policy = json!({"schema_version":3,"kind":"physical_policy","table_map_sha256":table_map_sha,"families":families,
        "pools":{"snapshot_spool_bytes":64*1024*1024u64,"restore_staging_bytes":64*1024*1024u64,"transfer_staging_bytes":64*1024*1024u64,
        "cas_bytes":64*1024*1024u64,"source_runtime_scratch_bytes":64*1024*1024u64,"control_bytes":64*1024*1024u64}});
    let peak = family_total
        + 6 * 64 * 1024 * 1024u64
        + 6 * number(p, "total_retained_artifact_bytes").expect("fixture retention")
        + 32 * 1024 * 1024u64 * 19;
    let calibration = json!({"schema_version":1,"kind":"physical_calibration","policy_sha256":digest(canonical(&policy).as_bytes()),"table_map_sha256":table_map_sha,
        "geometry":{},"baseline":{"source_workspace_bytes":0,"existing_unpooled_bytes":0},"peak_bytes":peak});
    p["source_bounds_utf8"] = json!(canonical(
        &json!({"schema_version":3,"accounting_rule":"typed_physical_caps_v1","source_receipt":{},"record_allowances":allowances,
        "physical_policy":policy,"calibration":calibration,"accounting_method_sha256":"e".repeat(64),"measurement_sha256":"f".repeat(64)})
    ));
    p["plan_sha256"] = json!("0".repeat(64));
    let provisional: Record = serde_json::from_value(plan).expect("plan shape");
    let hash = provisional.sha256();
    let mut value = provisional.record;
    value["plan_sha256"] = json!(hash);
    CatalogConfig {
        path,
        source,
        session_id,
        resource_plan: Record::new("ResourcePlan", value).expect("fixture plan"),
        grant: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MANUAL OPERATIONAL MEASUREMENT ENTRY. No behavioral regression or proof
    /// credit: M4 registers the exact ignored invocation and captures its origin.
    #[test]
    #[ignore = "manual operational calibration requires an owned output path"]
    fn catalog_sqlite_empty_schema_reports_actual_engine_geometry() {
        use std::io::Write;
        let output = PathBuf::from(
            std::env::var_os("PLURX_ASYNC_CATALOG_CALIBRATION")
                .expect("manual calibration output required"),
        );
        assert!(
            output.is_absolute() && !output.exists(),
            "fresh absolute owned measurement output"
        );
        let parent = output.parent().expect("owned measurement directory");
        let directory = tempfile::tempdir_in(parent).expect("owned persistent measurement scratch");
        let storage = sqlite::SqliteStorage::open(
            &directory.path().join("empty-harness.sqlite"),
            4 * 1024 * 1024,
        )
        .expect("actual empty harness DDL");
        let report = storage
            .empty_geometry()
            .expect("actual linked engine geometry");
        assert_eq!(report["acceptance"], false);
        let bytes = canonical(&report);
        assert!(bytes.len() <= 64 * 1024, "bounded operational marker");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(output)
            .expect("owned report file");
        file.write_all(bytes.as_bytes())
            .expect("write actual operational report");
        file.sync_all().expect("durable operational report");
        eprintln!("async_paged_catalog_calibration={bytes}");
    }

    // Synthetic storage values, deliberately not a Store-D proof certificate.
    // The test exercises the two actual catalog backends and their transaction
    // boundaries; production transition correctness remains the Oracle's job.
    fn seed(catalog: &Catalog) -> CatalogResult<()> {
        let transaction = catalog.begin_initial(0)?;
        catalog.set_control(
            "context_descriptors",
            &json!(CONTEXTS
                .iter()
                .map(|id| json!({"id":id}))
                .collect::<Vec<_>>()),
        )?;
        let rows = json!({"background_jobs":[],"background_job_waiters":[],"background_job_attempts":[],"background_job_reservations":[],"background_job_domain_leases":[],"background_library_requests":[],"background_job_commands":[],"job_leases":[],"settings":[]});
        let episode = json!({"now":1000,"claims":[0,0],"claim_snapshots":[],"tokens":[[],[]],"renewed_claims":[],"leases":[],"domain_acquired":[false,false,false],"removed":[false,false]});
        let bytes = serde_json::to_string(&json!({"rows":rows,"episode":episode}))?;
        let physical = catalog.intern_physical(&serde_json::to_string(&rows)?)?;
        let episode = catalog.intern_episode(&serde_json::to_string(&episode)?)?;
        let boundary=catalog.append_checked_boundary(checked_boundary(json!({"physical_rows_id":physical.get(),"episode_id":episode.get(),"omitted_utf8":"{}","canonical_utf8":bytes,"phase":"initial","source":catalog.source(),"writer_epoch":1,"initial_image_id":"a".repeat(64)})))?;
        let store=catalog.intern_checked_store(checked_store(json!({"physical_rows_id":physical.get(),"episode_id":episode.get(),"canonical_utf8":bytes,"sha256":digest(bytes.as_bytes()),"initial_image_id":"a".repeat(64),"sqlite_schema_sha256":"b".repeat(64),"omitted_observation_id":boundary.get(),"certification":"actual_init_checked"})))?;
        let mut contexts = Vec::new();
        for context in CONTEXTS {
            let payload = super::super::async_driver_product::catalog_fixture_payload(context);
            let id = catalog.intern_product(context, &canonical(&payload), store)?;
            contexts.push((context.into(), id.get()));
        }
        let image = Record::new(
            "InitialImage",
            json!({"image_sha256":"a".repeat(64),"member_path":"initial-image.sqlite","image_bytes":1,"schema_inventory":{"kind":"schema_inventory","id":"schema-inventory.json","sha256":"b".repeat(64)},"initial_store":store.get(),"raw_fixture_utf8":"{}","omitted_observation":{"kind":"BoundaryObservation","id":boundary.get(),"sha256":catalog.get_id("BoundaryObservation",boundary.get())?.sha256()},"source":catalog.source()}),
        )?;
        transaction.seal_initial(image, &contexts)?;
        Ok(())
    }
    fn expand_selfloop(catalog: &Catalog, unit: u64) -> CatalogResult<()> {
        let (ticket, product) = catalog.oldest_pending()?.expect("fixture frontier");
        let tx = catalog.begin_expansion(ticket, product, unit)?;
        let state = catalog.get_id("Product", product.get())?;
        let context = string(&state.record, "context")?;
        let candidate = catalog.allocate("Candidate")?;
        catalog.append_record("Candidate",json!({"id":candidate,"context":context,"predecessor":product.get(),"ordinal":0,"family":"fixture_selfloop","worker":null,"choice_utf8":"{}","descriptor_utf8":"{}","classification":"eligible","reason":null}))?;
        let edge=catalog.append_product_edge(json!({"context":context,"from_product":product.get(),"to_product":product.get(),"candidate":candidate,"branch_utf8":"{}","primary_edge":null,"resolution_edge":null,"read_edge":null,"cleanup_edge":null}))?;
        let pair = catalog.union_fair_pair(context, product, product, &["Admission".into()])?;
        let enabled = catalog.append_enabledness(
            json!({"context":context,"product_id":product.get(),"full_graph_labels":[]}),
        )?;
        let expansion = tx.expansion_id()?.get();
        tx.seal(json!({"id":expansion,"context":context,"predecessor":product.get(),"fifo_ticket":ticket.get(),"candidates":[candidate],"demands":[],"oracle_evidence":[],"product_edges":[edge.get()],"fair_pairs":[pair],"enabledness":enabled,"discovered_products":[],"capacity_only_stores":[],"unit_ordinal":unit,"complete":true}))?;
        Ok(())
    }
    #[test]
    fn catalog_reference_and_sqlite_atomic_fifo_snapshots_agree() {
        let directory = tempfile::tempdir().expect("owned test scratch");
        let config = fixture_configuration(directory.path().join("catalog.sqlite"));
        let memory =
            Catalog::memory(config.clone(), 256, 4 * 1024 * 1024).expect("bounded reference");
        let disk = Catalog::disk(config).expect("persistent catalog");
        for catalog in [&memory, &disk] {
            seed(catalog).expect("seed catalog");
            expand_selfloop(catalog, 1).expect("complete predecessor");
            let (ticket, product) = catalog
                .oldest_pending()
                .expect("frontier")
                .expect("pending");
            assert_eq!(
                product.get(),
                2,
                "all seven initial seeds precede discovery"
            );
            {
                let _aborted = catalog
                    .begin_expansion(ticket, product, 2)
                    .expect("private transaction");
                let id = catalog.allocate("Candidate").expect("private id");
                catalog.append_record("Candidate",json!({"id":id,"context":"stable","predecessor":product.get(),"ordinal":0,"family":"aborted","worker":null,"choice_utf8":"{}","descriptor_utf8":"{}","classification":"eligible","reason":null})).expect("private record");
            }
            assert_eq!(
                catalog.count("Candidate").expect("count"),
                1,
                "drop publishes nothing"
            );
            assert!(catalog
                .pin_read_snapshot(TransactionId::new(1).expect("old sequence"))
                .is_err());
        }
        let a = memory
            .pin_read_snapshot(memory.current_transaction().expect("memory commit"))
            .expect("pin memory");
        let b = disk
            .pin_read_snapshot(disk.current_transaction().expect("disk commit"))
            .expect("pin disk");
        assert_eq!(
            a.logical_digest().expect("reference digest"),
            b.logical_digest().expect("disk digest")
        );
        assert_eq!(
            a.counts().expect("reference counts"),
            b.counts().expect("disk counts")
        );
        let (ticket, product) = disk.oldest_pending().expect("frontier").expect("pending");
        assert!(
            disk.begin_expansion(ticket, product, 3).is_err(),
            "reader pauses writer"
        );
        let row = b
            .read_records(
                RecordRange {
                    kind: "FairPair".into(),
                    after: None,
                    through_transaction: b.transaction(),
                },
                ReadLimits {
                    max_records: 1,
                    max_encoded_bytes: 4096,
                },
            )
            .expect("composite-key read");
        assert_eq!(row.records.len(), 1);
        assert_eq!(row.records[0].record["nonstuttering"], false);
        let page_limits = PageLimits {
            max_records: 2,
            max_member_bytes: 256 * 1024,
            max_manifest_node_bytes: 256 * 1024,
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let memory_output = directory.path().join("reference-pages");
        let disk_output = directory.path().join("sqlite-pages");
        let memory_export = a
            .export(&memory_output, None, &page_limits, deadline)
            .expect("reference dependency closure export");
        let disk_export = b
            .export(&disk_output, None, &page_limits, deadline)
            .expect("SQLite dependency closure export");
        assert_eq!(
            memory_export.root, disk_export.root,
            "same exact records imply same addressed root"
        );
        assert_eq!(memory_export.members, disk_export.members);
        for member in &memory_export.members {
            assert_eq!(
                std::fs::read(memory_output.join(member)).expect("reference member"),
                std::fs::read(disk_output.join(member)).expect("SQLite member")
            );
        }
        a.close().expect("close reference");
        b.close().expect("close disk");
        for (catalog, output, export) in [
            (&memory, &memory_output, &memory_export),
            (&disk, &disk_output, &disk_export),
        ] {
            for member in &export.members {
                let record = Record::parse(
                    &std::fs::read(output.join(member)).expect("closed exported bytes"),
                )
                .expect("closed export record");
                catalog
                    .register_export(&record)
                    .expect("post-snapshot immutable export bookkeeping");
            }
        }
        // Bookkeeping is deliberately after the pinned semantic snapshot and
        // before the actual SQLite backup. No current artifact hashes enter it.
        let snapshot = disk
            .pin_read_snapshot(disk.current_transaction().expect("checkpoint sequence"))
            .expect("pin checkpoint");
        let checkpoint=Record::new("Checkpoint",json!({"session_id":disk.session_id(),"sequence":0,"source":disk.source(),"initial_image_id":"a".repeat(64),"resource_plan_sha256":disk.resource_plan().sha256(),"unit_ordinal":1,"previous_checkpoint":null,"committed_transaction":snapshot.transaction().get(),"oldest_pending_ticket":ticket.get(),"counts":snapshot.counts().expect("checkpoint counts"),"contexts":snapshot.context_snapshots().expect("checkpoint contexts"),"page_root":disk_export.root.id,"catalog_logical_digest":snapshot.logical_digest().expect("checkpoint digest")})).expect("closed checkpoint");
        let unit=Record::new("WorkUnit",json!({"session_id":disk.session_id(),"unit_ordinal":1,"expected_prior_checkpoint":null,"source":disk.source(),"pr":1,"repository":1,"event_base":"0".repeat(40),"workflow_id":"async-proof-session.yml","event_name":"workflow_dispatch","mode":"unit","run":1,"job":1,"task":1,"attempt":1,"resource_plan_sha256":disk.resource_plan().sha256(),"runner_identity_utf8":"{}","status":"checkpointed_incomplete","artifact_roots":[],"stage":"expand","registered_cases":[],"progress":{"context":null,"record_kind":null,"after_key":null,"fifo_ticket":ticket.get(),"component_id":null,"family":null,"case_index":0}})).expect("private unit");
        assert!(
            disk.record_checkpoint(&checkpoint, &unit).is_err(),
            "checkpoint cannot race the read snapshot"
        );
        snapshot.close().expect("close checkpoint snapshot");
        disk.record_checkpoint(&checkpoint, &unit)
            .expect("private preupload bookkeeping");
        let backup = directory.path().join("catalog-backup.sqlite");
        let limits = BackupLimits {
            max_bytes: 16 * 1024 * 1024,
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(30),
        };
        let descriptor = disk
            .backup_checkpoint_catalog(&checkpoint, &backup, &limits)
            .expect("actual backup API");
        let mut restored_config = disk.inner.config.clone();
        restored_config.path = backup.clone();
        let restored =
            Catalog::validation(restored_config.clone()).expect("readonly staged catalog");
        let receipt = restored
            .validate_restore(
                &checkpoint,
                number(&descriptor, "catalog_bytes").expect("backup bytes"),
                string(&descriptor, "catalog_sha256").expect("backup hash"),
                &limits,
            )
            .expect("private restore checks");
        assert_eq!(
            receipt["catalog_logical_digest"],
            checkpoint.record["catalog_logical_digest"]
        );
        drop(restored);
        let old_root = disk
            .exported_record("ManifestNode", &disk_export.root.id)
            .expect("immutable prior root");
        expand_selfloop(&disk, 2).expect("next global FIFO predecessor");
        let next_snapshot = disk
            .pin_read_snapshot(disk.current_transaction().expect("new high-water"))
            .expect("pin new source snapshot");
        let next_output = directory.path().join("next-unit-pages");
        let next = next_snapshot
            .export(&next_output, Some(&checkpoint), &page_limits, deadline)
            .expect("incremental export retains exact prior dependency closure");
        assert_ne!(next.root, disk_export.root);
        let mut next_checkpoint = checkpoint.record.clone();
        next_checkpoint["sequence"] = json!(1);
        next_checkpoint["unit_ordinal"] = json!(2);
        next_checkpoint["previous_checkpoint"] = json!(checkpoint.sha256());
        next_checkpoint["committed_transaction"] = json!(next_snapshot.transaction());
        next_checkpoint["oldest_pending_ticket"] = json!(
            disk.oldest_pending()
                .expect("new FIFO head")
                .expect("pending")
                .0
        );
        next_checkpoint["counts"] = next_snapshot.counts().expect("new counts");
        next_checkpoint["contexts"] =
            json!(next_snapshot.context_snapshots().expect("new contexts"));
        next_checkpoint["page_root"] = json!(next.root.id);
        next_checkpoint["catalog_logical_digest"] =
            json!(next_snapshot.logical_digest().expect("new logical digest"));
        next_snapshot.close().expect("close next snapshot");
        for member in &next.members {
            let record =
                Record::parse(&std::fs::read(next_output.join(member)).expect("new member bytes"))
                    .expect("new closed member");
            disk.register_export(&record)
                .expect("new immutable export registration");
        }
        assert_eq!(
            disk.exported_record("ManifestNode", &disk_export.root.id)
                .expect("prior root unchanged")
                .bytes(),
            old_root.bytes()
        );
        let next_checkpoint = Record::new("Checkpoint", next_checkpoint).expect("next checkpoint");
        let mut next_unit = unit.record.clone();
        next_unit["unit_ordinal"] = json!(2);
        next_unit["expected_prior_checkpoint"] = json!(checkpoint.sha256());
        disk.record_checkpoint(
            &next_checkpoint,
            &Record::new("WorkUnit", next_unit).expect("next private unit"),
        )
        .expect("monotone checkpoint chain");
        assert!(
            disk.record_checkpoint(&checkpoint, &unit).is_err(),
            "cannot replay an old checkpoint over new progress"
        );
        let tamper = rusqlite::Connection::open(&backup).expect("fixture tamper writer");
        tamper
            .execute(
                "UPDATE catalog_allocators SET last_id=last_id+1 WHERE kind='Product'",
                [],
            )
            .expect("private allocator mutation");
        drop(tamper);
        let (bytes, hash) =
            hash_file(&backup, limits.max_bytes, limits.deadline).expect("mutated file identity");
        let restored = Catalog::validation(restored_config).expect("readonly mutated catalog");
        assert!(
            matches!(restored.validate_restore(&checkpoint,bytes,&hash,&limits),Err(CatalogError::CorruptRecord(reason)) if reason.contains("allocator")),
            "a newly hashed file cannot launder a corrupted private allocator"
        );
    }
    #[test]
    fn catalog_closed_records_reject_duplicate_keys_and_source_aliases() {
        assert!(Record::parse(
            br#"{"schema_version":6,"kind":"Counts","kind":"Store","record":{}}"#
        )
        .is_err());
        let mut source = fixture_configuration(PathBuf::from("unused-reference-path")).source;
        source.proof_contract_sha256 = "not-a-digest".into();
        assert!(source.validate().is_err());
        let left = Record::new(
            "PhysicalRows",
            json!({"id":1,"canonical_utf8":"[]","utf8_bytes":2,"sha256":digest(b"[]")}),
        )
        .expect("left");
        let right = Record::new(
            "PhysicalRows",
            json!({"id":2,"canonical_utf8":"{}","utf8_bytes":2,"sha256":digest(b"{}")}),
        )
        .expect("right");
        assert_ne!(left.bytes(), right.bytes());
        assert!(matches!(
            Record::parse_bounded(&left.bytes(), 1),
            Err(CatalogError::ResourceIncomplete(_))
        ));
        assert_eq!(
            Record::parse_bounded(&left.bytes(), 64 * 1024 * 1024).expect("bounded actual decoder"),
            left
        );
        let nested = format!("{}0{}", "[".repeat(129), "]".repeat(129));
        assert!(decoded_json_reservation(nested.as_bytes(), 64 * 1024 * 1024).is_err());
        let lexical = decoded_json_reservation(
            br#"{"quoted":"[\\\"}]","list":[false,0,null]}"#,
            64 * 1024 * 1024,
        )
        .expect("escaped lexical reservation");
        assert!(decoded_json_reservation(
            br#"{"quoted":"[\\\"}]","list":[false,0,null]}"#,
            lexical - 1
        )
        .is_err());
        // Full outer envelope accounting includes every JSON escape and header.
        let inner = canonical(&json!(["\"", "\\", "\n", "λ"]));
        let escaped=Record::new("PhysicalRows",json!({"id":3,"canonical_utf8":inner,"utf8_bytes":inner.len(),"sha256":digest(inner.as_bytes())})).expect("escaped exact inner bytes");
        assert_eq!(
            escaped.encoded_len(u64::MAX).expect("escaped full size"),
            escaped.bytes().len() as u64
        );
        assert!(escaped
            .encoded_len(escaped.bytes().len() as u64 - 1)
            .is_err());
        let full = left.bytes().len() as u64;
        assert_eq!(left.encoded_len(full).expect("exact outer limit"), full);
        assert!(left.encoded_len(full - 1).is_err());
        assert!(
            full > string(&left.record, "canonical_utf8")
                .expect("inner bytes")
                .len() as u64
        );
        assert_ne!(
            left.reference().expect("left ref"),
            right.reference().expect("right ref")
        );
    }

    #[test]
    fn granted_limits_are_only_what_the_parent_deducted() {
        let directory = tempfile::tempdir().expect("owned scratch");
        let root = directory.path().canonicalize().expect("root");
        let config = fixture_configuration(root.join("catalog.sqlite"));
        let full = ResourceLimits::from_plan(&config.resource_plan).expect("policy limits");
        let grant = grant::ChildGrant::for_test(
            &root,
            vec![
                grant::GrantEntry::for_test(
                    "catalog_main",
                    "main",
                    2 * 1024 * 1024,
                    "catalog.sqlite",
                ),
                grant::GrantEntry::for_test(
                    "catalog_main",
                    "journal",
                    5 * 1024 * 1024,
                    "catalog.sqlite",
                ),
                grant::GrantEntry::for_test(
                    "catalog_main",
                    "temp",
                    4 * 1024 * 1024,
                    "catalog.sqlite",
                ),
                grant::GrantEntry::for_test(
                    "snapshot_spool_bytes",
                    "bytes",
                    512 * 1024,
                    "backup.sqlite",
                ),
            ],
            Vec::new(),
        );
        let view = full.granted(&grant).expect("narrowed view");
        let family = view.family("catalog_main").expect("granted family");
        assert_eq!(
            (
                family.database_bytes,
                family.main_bytes,
                family.journal_bytes,
                family.temp_bytes
            ),
            (
                2 * 1024 * 1024,
                2 * 1024 * 1024,
                5 * 1024 * 1024,
                4 * 1024 * 1024
            ),
            "the view is the grant, not the 64 MiB policy ceiling"
        );
        assert_eq!(
            view.pool("snapshot_spool_bytes").expect("granted pool"),
            512 * 1024
        );
        assert!(
            view.family("verifier_main").is_err(),
            "an ungranted family is absent, not policy-sized"
        );
        assert!(
            view.pool("cas_bytes").is_err(),
            "an ungranted pool is absent"
        );
        // The journal/temp reservations of the narrowed view must still cover its ceiling.
        // The exact reservation (journal = 2*ceiling + 1 MiB, temp = 2*ceiling) opens.
        sqlite::SqliteStorage::open_reserved(&root.join("probe.sqlite"), 4 * 1024 * 1024, &view)
            .expect("the granted ceiling and its reservations are exactly sufficient");
    }

    #[test]
    fn disk_catalog_must_lie_inside_its_granted_main_allocation() {
        let directory = tempfile::tempdir().expect("owned scratch");
        let root = directory.path().canonicalize().expect("root");
        let mut config = fixture_configuration(root.join("catalog.sqlite"));
        let grant = grant::ChildGrant::for_test(
            &root,
            vec![
                grant::GrantEntry::for_test(
                    "catalog_main",
                    "main",
                    4 * 1024 * 1024,
                    "elsewhere.sqlite",
                ),
                grant::GrantEntry::for_test(
                    "catalog_main",
                    "journal",
                    9 * 1024 * 1024,
                    "elsewhere.sqlite",
                ),
                grant::GrantEntry::for_test(
                    "catalog_main",
                    "temp",
                    8 * 1024 * 1024,
                    "elsewhere.sqlite",
                ),
            ],
            Vec::new(),
        );
        config.grant = Some(std::sync::Arc::new(grant));
        assert!(
            matches!(Catalog::disk(config.clone()), Err(CatalogError::ResourceIncomplete(reason)) if reason.contains("granted main")),
            "a writable catalog outside its allocation is refused before it is created"
        );
        assert!(!root.join("catalog.sqlite").exists());
        let covering = grant::ChildGrant::for_test(
            &root,
            vec![
                grant::GrantEntry::for_test(
                    "catalog_main",
                    "main",
                    4 * 1024 * 1024,
                    "catalog.sqlite",
                ),
                grant::GrantEntry::for_test(
                    "catalog_main",
                    "journal",
                    9 * 1024 * 1024,
                    "catalog.sqlite",
                ),
                grant::GrantEntry::for_test(
                    "catalog_main",
                    "temp",
                    8 * 1024 * 1024,
                    "catalog.sqlite",
                ),
            ],
            Vec::new(),
        );
        config.grant = Some(std::sync::Arc::new(covering));
        Catalog::disk(config)
            .expect("catalog inside its allocation opens against the granted ceiling");
    }

    #[test]
    fn restored_catalog_requires_a_readonly_attachment_not_a_second_allocation() {
        let directory = tempfile::tempdir().expect("owned scratch");
        let root = directory.path().canonicalize().expect("root");
        let source = fixture_configuration(root.join("live.sqlite"));
        let live = Catalog::disk(source.clone()).expect("live catalog");
        drop(live);
        let restored = root.join("restored.sqlite");
        std::fs::copy(root.join("live.sqlite"), &restored).expect("restored image");
        let mut config = fixture_configuration(restored.clone());
        let writable_only = grant::ChildGrant::for_test(
            &root,
            vec![grant::GrantEntry::for_test(
                "catalog_main",
                "main",
                4 * 1024 * 1024,
                "restored.sqlite",
            )],
            Vec::new(),
        );
        config.grant = Some(std::sync::Arc::new(writable_only));
        assert!(
            matches!(Catalog::validation(config.clone()), Err(CatalogError::ResourceIncomplete(reason)) if reason.contains("attachment")),
            "a main allocation is not an attachment of the held restore reservation"
        );
        let mut attachment = grant::GrantEntry::for_test(
            "restore_staging_bytes",
            "bytes",
            64 * 1024 * 1024,
            "restored.sqlite",
        );
        attachment.reservation_id = "d".repeat(64);
        let attached = grant::ChildGrant::for_test(
            &root,
            vec![
                grant::GrantEntry::for_test(
                    "catalog_main",
                    "journal",
                    9 * 1024 * 1024,
                    "restored.sqlite",
                ),
                grant::GrantEntry::for_test(
                    "catalog_main",
                    "temp",
                    8 * 1024 * 1024,
                    "restored.sqlite",
                ),
            ],
            vec![attachment],
        );
        config.grant = Some(std::sync::Arc::new(attached));
        Catalog::validation(config)
            .expect("an attached restore spends the held reservation and opens read-only");
    }
}
