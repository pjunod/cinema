//! Small independent ordered-map reference for catalog equivalence fixtures.
//! It is never selected by the dispatched product producer.
use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use super::{
    canonical, number, string, CatalogError, CatalogResult, ReadLimits, Record, RecordBatch,
    RecordKey, Storage, WriteContext,
};

#[derive(Clone, Default)]
struct State {
    sequence: u64,
    records: BTreeMap<(String, RecordKey), Record>,
    counters: BTreeMap<String, u64>,
    controls: BTreeMap<String, Value>,
    frontier: BTreeMap<u64, (u64, bool)>,
    initial: BTreeMap<String, u64>,
    next_ticket: u64,
}
pub struct MemoryStorage {
    state: State,
    prior: Option<State>,
    context: Option<WriteContext>,
    reading: bool,
    max_records: usize,
    max_bytes: u64,
}
impl MemoryStorage {
    pub fn new(max_records: usize, max_bytes: u64) -> CatalogResult<Self> {
        if max_records == 0 || max_records > 4096 || max_bytes == 0 || max_bytes > 16 * 1024 * 1024
        {
            return Err(CatalogError::ResourceIncomplete(
                "reference map bounds exceed small-fixture limits".into(),
            ));
        }
        Ok(Self {
            state: State::default(),
            prior: None,
            context: None,
            reading: false,
            max_records,
            max_bytes,
        })
    }
    fn writable(&self) -> CatalogResult<()> {
        if self.prior.is_none() || self.reading {
            return Err(CatalogError::IncompleteExpansion(
                "reference map has no owned writer".into(),
            ));
        }
        Ok(())
    }
    fn within_bounds(&self) -> CatalogResult<()> {
        let bytes = self
            .state
            .records
            .values()
            .try_fold(0u64, |total, record| {
                total
                    .checked_add(record.bytes().len() as u64)
                    .ok_or_else(|| {
                        CatalogError::ResourceIncomplete("reference byte overflow".into())
                    })
            })?;
        if self.state.records.len() > self.max_records || bytes > self.max_bytes {
            return Err(CatalogError::ResourceIncomplete(
                "small reference catalog exhausted".into(),
            ));
        }
        Ok(())
    }
}
impl Storage for MemoryStorage {
    fn begin(&mut self, context: WriteContext) -> CatalogResult<u64> {
        if self.prior.is_some() || self.reading {
            return Err(CatalogError::SnapshotMismatch(
                "reference transaction active".into(),
            ));
        }
        let next = self
            .state
            .sequence
            .checked_add(1)
            .filter(|n| *n <= i64::MAX as u64)
            .ok_or_else(|| CatalogError::NumericDomain("reference sequence exhausted".into()))?;
        self.prior = Some(self.state.clone());
        self.context = Some(context);
        Ok(next)
    }
    fn set_expansion(&mut self, expansion: u64) -> CatalogResult<()> {
        self.writable()?;
        self.context.as_mut().expect("owned writer").expansion = Some(expansion);
        Ok(())
    }
    fn commit(&mut self) -> CatalogResult<u64> {
        self.writable()?;
        self.within_bounds()?;
        self.state.sequence += 1;
        self.prior.take();
        self.context.take();
        Ok(self.state.sequence)
    }
    fn rollback(&mut self) -> CatalogResult<()> {
        self.state = self
            .prior
            .take()
            .ok_or_else(|| CatalogError::IncompleteExpansion("no reference transaction".into()))?;
        self.context.take();
        Ok(())
    }
    fn sequence(&self) -> CatalogResult<u64> {
        Ok(self.state.sequence)
    }
    fn allocate(&mut self, kind: &str) -> CatalogResult<u64> {
        self.writable()?;
        let current = self.state.counters.entry(kind.into()).or_default();
        *current = current
            .checked_add(1)
            .filter(|n| *n <= i64::MAX as u64)
            .ok_or_else(|| CatalogError::NumericDomain("reference ID exhausted".into()))?;
        Ok(*current)
    }
    fn get(&self, kind: &str, key: &RecordKey) -> CatalogResult<Option<Record>> {
        Ok(self.state.records.get(&(kind.into(), key.clone())).cloned())
    }
    fn put(&mut self, record: &Record, replace_private: bool) -> CatalogResult<()> {
        self.writable()?;
        record.validate()?;
        let key = (record.kind.clone(), record.key()?);
        if let Some(old) = self.state.records.get(&key) {
            if old.bytes() != record.bytes() && !replace_private {
                return Err(CatalogError::ConflictingOutcome(
                    "reference immutable key".into(),
                ));
            }
        }
        self.state.records.insert(key, record.clone());
        self.within_bounds()
    }
    fn find_canonical(&self, kind: &str, bytes: &str) -> CatalogResult<Option<Record>> {
        Ok(self
            .state
            .records
            .iter()
            .filter(|((k, _), _)| k == kind)
            .find_map(|(_, record)| {
                let value = if kind == "Product" {
                    canonical(&serde_json::json!([
                        record.record["context"],
                        record.record["canonical_utf8"]
                    ]))
                } else {
                    record.record["canonical_utf8"].as_str()?.into()
                };
                (value == bytes).then(|| record.clone())
            }))
    }
    fn scan(
        &self,
        kind: &str,
        after: Option<&RecordKey>,
        limits: &ReadLimits,
    ) -> CatalogResult<RecordBatch> {
        if limits.max_records == 0 || limits.max_encoded_bytes == 0 {
            return Err(CatalogError::ResourceIncomplete(
                "zero reference read bound".into(),
            ));
        }
        let mut records = Vec::new();
        let mut bytes = 0u64;
        let mut more = false;
        for ((k, key), record) in &self.state.records {
            if k != kind || after.is_some_and(|after| key <= after) {
                continue;
            }
            let size = record.bytes().len() as u64;
            if records.len() >= limits.max_records
                || bytes
                    .checked_add(size)
                    .is_none_or(|n| n > limits.max_encoded_bytes)
            {
                if records.is_empty() {
                    return Err(CatalogError::ResourceIncomplete(
                        "indivisible record exceeds read allowance".into(),
                    ));
                }
                more = true;
                break;
            }
            bytes += size;
            records.push(record.clone());
        }
        let next_after = if more {
            records.last().map(Record::key).transpose()?
        } else {
            None
        };
        Ok(RecordBatch {
            records,
            encoded_bytes: bytes,
            next_after,
        })
    }
    fn count(&self, kind: &str) -> CatalogResult<u64> {
        Ok(self.state.records.keys().filter(|(k, _)| k == kind).count() as u64)
    }
    fn oracle_edge_for_input(&self, from: u64, input: &str) -> CatalogResult<Option<u64>> {
        for record in self
            .state
            .records
            .values()
            .filter(|record| record.kind == "OracleEvidence")
        {
            if record.record["applied"]["from_store"] == from
                && record.record["applied"]["action_utf8"] == input
            {
                return Ok(record.record["id"].as_u64());
            }
        }
        Ok(None)
    }
    fn get_control(&self, key: &str) -> CatalogResult<Option<Value>> {
        Ok(self.state.controls.get(key).cloned())
    }
    fn set_control(&mut self, key: &str, value: &Value) -> CatalogResult<()> {
        self.writable()?;
        self.state.controls.insert(key.into(), value.clone());
        Ok(())
    }
    fn enqueue(&mut self, product: u64) -> CatalogResult<u64> {
        self.writable()?;
        if self.state.frontier.values().any(|(id, _)| *id == product) {
            return Err(CatalogError::ConflictingOutcome(
                "duplicate frontier product".into(),
            ));
        }
        self.state.next_ticket += 1;
        self.state
            .frontier
            .insert(self.state.next_ticket, (product, false));
        Ok(self.state.next_ticket)
    }
    fn oldest_pending(&self) -> CatalogResult<Option<(u64, u64)>> {
        Ok(self
            .state
            .frontier
            .iter()
            .find_map(|(ticket, (product, expanded))| (!expanded).then_some((*ticket, *product))))
    }
    fn mark_expanded(&mut self, product: u64) -> CatalogResult<()> {
        self.writable()?;
        if !self
            .state
            .records
            .values()
            .any(|r| r.kind == "Expansion" && r.record["predecessor"] == product)
        {
            return Err(CatalogError::IncompleteExpansion(
                "reference predecessor lacks seal".into(),
            ));
        }
        let (_, expanded) = self
            .state
            .frontier
            .values_mut()
            .find(|(id, _)| *id == product)
            .ok_or_else(|| CatalogError::MissingDependency("reference frontier".into()))?;
        if *expanded {
            return Err(CatalogError::ConflictingOutcome(
                "reference predecessor expanded twice".into(),
            ));
        }
        *expanded = true;
        Ok(())
    }
    fn pending(&self, context: Option<&str>) -> CatalogResult<u64> {
        let mut total = 0;
        for (id, expanded) in self.state.frontier.values() {
            if !expanded {
                let record = self
                    .get("Product", &RecordKey::Numeric(*id))?
                    .ok_or_else(|| CatalogError::MissingDependency("frontier Product".into()))?;
                if context.is_none_or(|context| record.record["context"] == context) {
                    total += 1;
                }
            }
        }
        Ok(total)
    }
    fn set_initial(&mut self, context: &str, product: u64) -> CatalogResult<()> {
        self.writable()?;
        let record = self
            .get("Product", &RecordKey::Numeric(product))?
            .ok_or_else(|| CatalogError::MissingDependency("initial Product".into()))?;
        if string(&record.record, "context")? != context
            || !record.record["predecessor_edge"].is_null()
        {
            return Err(CatalogError::CorruptRecord("reference Init".into()));
        }
        if self.state.initial.insert(context.into(), product).is_some() {
            return Err(CatalogError::ConflictingOutcome(
                "reference Init replaced".into(),
            ));
        }
        Ok(())
    }
    fn initial(&self, context: &str) -> CatalogResult<Option<u64>> {
        Ok(self.state.initial.get(context).copied())
    }
    fn begin_read(&mut self, expected: u64) -> CatalogResult<()> {
        if self.prior.is_some() || self.reading || self.state.sequence != expected {
            return Err(CatalogError::SnapshotUnavailable(
                "reference exact high-water unavailable".into(),
            ));
        }
        self.reading = true;
        Ok(())
    }
    fn end_read(&mut self) -> CatalogResult<()> {
        if !self.reading {
            return Err(CatalogError::SnapshotUnavailable(
                "reference reader closed".into(),
            ));
        }
        self.reading = false;
        Ok(())
    }
    fn save_checkpoint(&mut self, checkpoint: &Record, work_unit: &Record) -> CatalogResult<()> {
        if self.prior.is_some() || self.reading {
            return Err(CatalogError::SnapshotMismatch(
                "reference checkpoint writer active".into(),
            ));
        }
        self.state.controls.insert(
            format!("checkpoint:{}", checkpoint.sha256()),
            serde_json::to_value(checkpoint)?,
        );
        self.state.controls.insert(
            "latest_checkpoint".into(),
            serde_json::to_value(checkpoint)?,
        );
        self.state.controls.insert(
            format!("work_unit:{}", number(&work_unit.record, "unit_ordinal")?),
            serde_json::to_value(work_unit)?,
        );
        Ok(())
    }
    fn check_private(&self, checkpoint: &Record) -> CatalogResult<()> {
        if number(&checkpoint.record, "committed_transaction")? != self.state.sequence {
            return Err(CatalogError::SnapshotMismatch(
                "reference checkpoint sequence".into(),
            ));
        }
        for (kind, last) in &self.state.counters {
            let max = self
                .state
                .records
                .keys()
                .filter_map(|(k, key)| {
                    if k == kind {
                        if let RecordKey::Numeric(id) = key {
                            Some(*id)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                })
                .max()
                .unwrap_or(0);
            if *last != max {
                return Err(CatalogError::CorruptRecord(
                    "reference allocator differs from committed IDs".into(),
                ));
            }
        }
        Ok(())
    }
    fn backup(&self, _path: &Path, _limits: &super::BackupLimits) -> CatalogResult<()> {
        Err(CatalogError::ResourceIncomplete(
            "bounded map reference is not a dispatchable SQLite catalog image".into(),
        ))
    }
}
