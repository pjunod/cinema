//! Test-only persistent exact proof catalog; distinct from the production Store.
//! The data path never decides production transitions. It stores checked values
//! and complete atomic predecessor expansions, with full-byte identity checks.
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
    fn save_checkpoint(&mut self, checkpoint: &Record, work_unit: &Record) -> CatalogResult<()>;
    fn check_private(&self, checkpoint: &Record) -> CatalogResult<()>;
    fn backup(&self, path: &Path, limits: &BackupLimits) -> CatalogResult<()>;
}

#[derive(Clone, Debug)]
pub struct CatalogConfig {
    pub path: PathBuf,
    pub source: SourceIdentity,
    pub session_id: uuid::Uuid,
    pub resource_plan: Record,
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
}
#[derive(Clone, Debug)]
pub struct RecordAllowance {
    pub records: u64,
    pub max_encoded_bytes: u64,
}
#[derive(Clone, Debug)]
pub struct ResourceLimits {
    pub record_allowances: BTreeMap<String, RecordAllowance>,
    pub index_bytes: u64,
    pub wal_bytes: u64,
    pub scratch_bytes: u64,
    pub retained_bytes: u64,
    pub duplicate_backup_bytes: u64,
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
                "overhead",
                "accounting_method_sha256",
                "measurement_sha256",
            ],
        )?;
        if wrapper["schema_version"] != 2
            || wrapper["accounting_rule"] != "operational_encoded_caps_v1"
            || !wrapper["source_receipt"].is_object()
        {
            return Err(CatalogError::SourceMismatch(
                "unsupported operational accounting rule".into(),
            ));
        }
        for field in ["accounting_method_sha256", "measurement_sha256"] {
            validate_definition("Sha256", &wrapper[field])?;
        }
        let total_disk_bytes = number(body, "total_session_disk_bytes")?;
        let entries = wrapper["record_allowances"]
            .as_object()
            .ok_or_else(|| CatalogError::CorruptRecord("allowance inventory".into()))?;
        let mut allowances = BTreeMap::new();
        let mut reserved = 0u64;
        for (kind, value) in entries {
            closed_fields(value, &["records", "max_encoded_bytes"])?;
            let records = number(value, "records")?;
            let bytes = number(value, "max_encoded_bytes")?;
            if bytes == 0 {
                return Err(CatalogError::ResourceIncomplete(
                    "zero encoded record allowance".into(),
                ));
            }
            // Exact comparison against the admitted finite disk quota avoids
            // overflowing even when caller-supplied operational products are huge.
            if records > total_disk_bytes / bytes {
                return Err(CatalogError::ResourceIncomplete(
                    "operational record allocation exceeds total disk".into(),
                ));
            }
            reserved = reserved
                .checked_add(records * bytes)
                .filter(|n| *n <= total_disk_bytes)
                .ok_or_else(|| {
                    CatalogError::ResourceIncomplete(
                        "operational allocation sum exceeds total disk".into(),
                    )
                })?;
            allowances.insert(
                kind.clone(),
                RecordAllowance {
                    records,
                    max_encoded_bytes: bytes,
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
        let overhead = &wrapper["overhead"];
        closed_fields(
            overhead,
            &[
                "index_bytes",
                "wal_bytes",
                "scratch_bytes",
                "retained_bytes",
                "duplicate_backup_bytes",
            ],
        )?;
        for field in [
            "index_bytes",
            "wal_bytes",
            "scratch_bytes",
            "retained_bytes",
            "duplicate_backup_bytes",
        ] {
            reserved = reserved
                .checked_add(number(overhead, field)?)
                .filter(|n| *n <= total_disk_bytes)
                .ok_or_else(|| {
                    CatalogError::ResourceIncomplete(
                        "operational overhead exceeds total disk".into(),
                    )
                })?;
        }
        Ok(Self {
            record_allowances: allowances,
            index_bytes: number(overhead, "index_bytes")?,
            wal_bytes: number(overhead, "wal_bytes")?,
            scratch_bytes: number(overhead, "scratch_bytes")?,
            retained_bytes: number(overhead, "retained_bytes")?,
            duplicate_backup_bytes: number(overhead, "duplicate_backup_bytes")?,
            resident_bytes: number(body, "resident_bytes")?,
            rss_bytes: number(body, "resident_rss_bytes")?,
            total_disk_bytes,
            unit_records: number(body, "unit_record_allowance")?,
            unit_bytes: number(body, "unit_byte_allowance")?,
            unit_predecessors: number(body, "unit_predecessor_allowance")?,
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
        let backend = sqlite::SqliteStorage::open(
            &config.path,
            number(&config.resource_plan.record, "resident_bytes")?,
        )?;
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
        let limits = ResourceLimits::from_plan(&config.resource_plan)?;
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
        let bytes = record.encoded_len(allowance.max_encoded_bytes)?;
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
        self.inner
            .storage
            .borrow_mut()
            .save_checkpoint(checkpoint, work_unit)
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
        let storage = sqlite::SqliteStorage::open_readonly(
            &config.path,
            number(&config.resource_plan.record, "resident_bytes")?,
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
            .scan(&range.kind, range.after.as_ref(), &limits)
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

pub(super) fn fixture_configuration(path: PathBuf) -> CatalogConfig {
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
    p["source_bounds_utf8"] = json!(canonical(
        &json!({"schema_version":2,"accounting_rule":"operational_encoded_caps_v1","source_receipt":{},"record_allowances":allowances,"overhead":{"index_bytes":4*1024*1024*1024u64,"wal_bytes":64*1024*1024,"scratch_bytes":128*1024*1024,"retained_bytes":128*1024*1024,"duplicate_backup_bytes":256*1024*1024},"accounting_method_sha256":"e".repeat(64),"measurement_sha256":"f".repeat(64)})
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn expand_selfloop(catalog: &Catalog) -> CatalogResult<()> {
        let (ticket, product) = catalog.oldest_pending()?.expect("fixture frontier");
        let tx = catalog.begin_expansion(ticket, product, 1)?;
        let candidate = catalog.allocate("Candidate")?;
        catalog.append_record("Candidate",json!({"id":candidate,"context":"base","predecessor":product.get(),"ordinal":0,"family":"fixture_selfloop","worker":null,"choice_utf8":"{}","descriptor_utf8":"{}","classification":"eligible","reason":null}))?;
        let edge=catalog.append_product_edge(json!({"context":"base","from_product":product.get(),"to_product":product.get(),"candidate":candidate,"branch_utf8":"{}","primary_edge":null,"resolution_edge":null,"read_edge":null,"cleanup_edge":null}))?;
        let pair = catalog.union_fair_pair("base", product, product, &["Admission".into()])?;
        let enabled = catalog.append_enabledness(
            json!({"context":"base","product_id":product.get(),"full_graph_labels":[]}),
        )?;
        let expansion = tx.expansion_id()?.get();
        tx.seal(json!({"id":expansion,"context":"base","predecessor":product.get(),"fifo_ticket":ticket.get(),"candidates":[candidate],"demands":[],"oracle_evidence":[],"product_edges":[edge.get()],"fair_pairs":[pair],"enabledness":enabled,"discovered_products":[],"capacity_only_stores":[],"unit_ordinal":1,"complete":true}))?;
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
            expand_selfloop(catalog).expect("complete predecessor");
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
        a.close().expect("close reference");
        b.close().expect("close disk");
        // Bookkeeping is deliberately after the pinned semantic snapshot and
        // before the actual SQLite backup. No current artifact hashes enter it.
        let snapshot = disk
            .pin_read_snapshot(disk.current_transaction().expect("checkpoint sequence"))
            .expect("pin checkpoint");
        let checkpoint=Record::new("Checkpoint",json!({"session_id":disk.session_id(),"sequence":0,"source":disk.source(),"initial_image_id":"a".repeat(64),"resource_plan_sha256":disk.resource_plan().sha256(),"unit_ordinal":1,"previous_checkpoint":null,"committed_transaction":snapshot.transaction().get(),"oldest_pending_ticket":ticket.get(),"counts":snapshot.counts().expect("checkpoint counts"),"contexts":snapshot.context_snapshots().expect("checkpoint contexts"),"page_root":"d".repeat(64),"catalog_logical_digest":snapshot.logical_digest().expect("checkpoint digest")})).expect("closed checkpoint");
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
}
