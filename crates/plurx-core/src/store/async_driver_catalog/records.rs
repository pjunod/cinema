//! Closed schema6 records. Inner canonical strings remain exact source bytes.
use std::collections::BTreeMap;
use std::fmt;
use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub type CatalogResult<T> = Result<T, CatalogError>;
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogError {
    CorruptRecord(String),
    SourceMismatch(String),
    InitialImageMismatch(String),
    SchemaMismatch(String),
    MissingDependency(String),
    IncompleteExpansion(String),
    ConflictingOutcome(String),
    PoisonedOracle(String),
    NumericDomain(String),
    ResourceIncomplete(String),
    SnapshotUnavailable(String),
    SnapshotMismatch(String),
    Io(String),
}
impl fmt::Display for CatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for CatalogError {}
impl From<rusqlite::Error> for CatalogError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Io(error.to_string())
    }
}
impl From<std::io::Error> for CatalogError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}
impl From<serde_json::Error> for CatalogError {
    fn from(error: serde_json::Error) -> Self {
        Self::CorruptRecord(error.to_string())
    }
}

macro_rules! identifier {
    ($($name:ident),+ $(,)?) => {$ (
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "u64", into = "u64")]
        pub struct $name(u64);
        impl $name {
            pub fn get(self) -> u64 { self.0 }
            pub fn new(value: u64) -> CatalogResult<Self> { Self::try_from(value) }
        }
        impl TryFrom<u64> for $name {
            type Error = CatalogError;
            fn try_from(value: u64) -> CatalogResult<Self> {
                if value == 0 || value > i64::MAX as u64 {
                    return Err(CatalogError::NumericDomain(stringify!($name).into()));
                }
                Ok(Self(value))
            }
        }
        impl From<$name> for u64 { fn from(value: $name) -> Self { value.0 } }
    )+};
}
identifier!(
    PhysicalRowsId,
    EpisodeId,
    StoreId,
    ProductId,
    OracleEdgeId,
    ExpansionId,
    FifoTicket,
    BoundaryId,
    ProductEdgeId,
    CandidateId,
    DemandId,
    MonitorId,
    ComponentId,
    TransactionId
);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RecordKey {
    Numeric(u64),
    Text(String),
}
impl RecordKey {
    pub fn canonical(&self) -> String {
        canonical(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceIdentity {
    pub commit: String,
    pub tree: String,
    pub source_closure_sha256: String,
    pub driver_sha256: String,
    pub finite_inventory_sha256: String,
    pub proof_contract_sha256: String,
    pub toolchain_sha256: String,
    pub workflow_sha256: String,
}
impl SourceIdentity {
    pub fn validate(&self) -> CatalogResult<()> {
        validate_definition("SourceIdentity", &serde_json::to_value(self)?)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordRef {
    pub kind: String,
    pub id: RecordKey,
    pub sha256: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Obligation {
    pub node: u8,
    pub job: u8,
    pub claim_id: uuid::Uuid,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub schema_version: u8,
    pub kind: String,
    pub record: Value,
}
impl Record {
    pub fn new(kind: &str, record: Value) -> CatalogResult<Self> {
        let value = Self {
            schema_version: 6,
            kind: kind.into(),
            record,
        };
        value.validate()?;
        Ok(value)
    }
    pub fn parse(bytes: &[u8]) -> CatalogResult<Self> {
        let record: Self = serde_json::from_value(strict_json(bytes)?)?;
        record.validate()?;
        Ok(record)
    }
    pub fn validate(&self) -> CatalogResult<()> {
        if self.schema_version != 6 || !record_kinds().contains(&self.kind.as_str()) {
            return Err(CatalogError::CorruptRecord(
                "unknown envelope kind/version".into(),
            ));
        }
        validate_definition(&self.kind, &self.record)?;
        for field in [
            "canonical_utf8",
            "action_utf8",
            "descriptor_utf8",
            "choice_utf8",
            "input_utf8",
            "omitted_utf8",
            "raw_fixture_utf8",
            "production_witness_utf8",
            "independent_checks_utf8",
            "source_bounds_utf8",
            "runner_identity_utf8",
        ] {
            if let Some(Value::String(text)) = self.record.get(field) {
                strict_json(text.as_bytes())?;
            }
        }
        if matches!(
            self.kind.as_str(),
            "PhysicalRows" | "Episode" | "Store" | "Product"
        ) {
            let text = string(&self.record, "canonical_utf8")?;
            if string(&self.record, "sha256")? != digest(text.as_bytes()) {
                return Err(CatalogError::CorruptRecord(
                    "canonical payload hash mismatch".into(),
                ));
            }
            if let Some(bytes) = self.record.get("utf8_bytes") {
                if bytes.as_u64() != Some(text.len() as u64) {
                    return Err(CatalogError::CorruptRecord(
                        "canonical payload byte count".into(),
                    ));
                }
            }
        }
        Ok(())
    }
    pub fn key(&self) -> CatalogResult<RecordKey> {
        let value = &self.record;
        if let Some(id) = value.get("id").and_then(Value::as_u64) {
            return Ok(RecordKey::Numeric(id));
        }
        let text = match self.kind.as_str() {
            "FairPair" => canonical(&json!([
                value["context"],
                value["from_product"],
                value["to_product"]
            ])),
            "Enabledness" => canonical(&json!([value["context"], value["product_id"]])),
            "Counts" => "global".into(),
            "ContextRoot" => string(value, "context")?.into(),
            "InitialImage" => string(value, "image_sha256")?.into(),
            "Checkpoint" | "Applied" => self.sha256(),
            "Page" => string(value, "page_id")?.into(),
            "ManifestNode" => string(value, "sha256")?.into(),
            "ResourcePlan" => string(value, "plan_sha256")?.into(),
            "Session" | "Completion" => string(value, "session_id")?.into(),
            "Counterexample" | "Outcome" => string(value, "family")?.into(),
            "WorkUnit" => return Ok(RecordKey::Numeric(number(value, "unit_ordinal")?)),
            "ArtifactReceipt" => return Ok(RecordKey::Numeric(number(value, "artifact_id")?)),
            _ => {
                return Err(CatalogError::CorruptRecord(
                    "record has no exact key".into(),
                ))
            }
        };
        Ok(RecordKey::Text(text))
    }
    pub fn sha256(&self) -> String {
        let mut value = serde_json::to_value(self).expect("record serialization");
        let own = match self.kind.as_str() {
            "Page" => Some("page_id"),
            "ManifestNode" => Some("sha256"),
            "ResourcePlan" => Some("plan_sha256"),
            _ => None,
        };
        if let Some(field) = own {
            value["record"]
                .as_object_mut()
                .expect("record object")
                .remove(field);
        }
        digest(canonical(&value).as_bytes())
    }
    pub fn reference(&self) -> CatalogResult<RecordRef> {
        Ok(RecordRef {
            kind: self.kind.clone(),
            id: self.key()?,
            sha256: self.sha256(),
        })
    }
    pub fn bytes(&self) -> Vec<u8> {
        canonical(self).into_bytes()
    }
}

pub fn record_kinds() -> &'static [&'static str] {
    &[
        "PhysicalRows",
        "Episode",
        "Store",
        "Product",
        "Applied",
        "OracleEvidence",
        "Candidate",
        "Demand",
        "ProductEdge",
        "FairPair",
        "Enabledness",
        "Expansion",
        "Counts",
        "ContextRoot",
        "InitialImage",
        "Checkpoint",
        "Page",
        "ManifestNode",
        "ResourcePlan",
        "Session",
        "WorkUnit",
        "MonitorNode",
        "MonitorEdge",
        "Scc",
        "Counterexample",
        "Outcome",
        "Completion",
        "BoundaryObservation",
        "ArtifactReceipt",
    ]
}
pub fn string<'a>(value: &'a Value, field: &str) -> CatalogResult<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| CatalogError::CorruptRecord(format!("missing string {field}")))
}
pub fn number(value: &Value, field: &str) -> CatalogResult<u64> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .filter(|n| *n <= i64::MAX as u64)
        .ok_or_else(|| CatalogError::CorruptRecord(format!("missing integer {field}")))
}
pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn canonical<T: Serialize>(value: &T) -> String {
    fn sort(value: &mut Value) {
        match value {
            Value::Object(map) => {
                for value in map.values_mut() {
                    sort(value);
                }
                map.sort_keys();
            }
            Value::Array(values) => {
                for value in values {
                    sort(value);
                }
            }
            _ => {}
        }
    }
    let mut value = serde_json::to_value(value).expect("JSON value serialization");
    sort(&mut value);
    serde_json::to_string(&value).expect("canonical JSON serialization")
}
pub fn schema() -> &'static Value {
    static SCHEMA: OnceLock<Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        serde_json::from_str(include_str!(
            "../../../../../verification/async-driver-refinement/paged-proof.schema.json"
        ))
        .expect("compiled closed schema6")
    })
}
pub fn validate_definition(name: &str, value: &Value) -> CatalogResult<()> {
    let definition = schema()["$defs"]
        .get(name)
        .ok_or_else(|| CatalogError::SchemaMismatch(format!("unknown definition {name}")))?;
    validate(definition, value, 0)
        .map_err(|reason| CatalogError::CorruptRecord(format!("{name}: {reason}")))
}
fn validate(rule: &Value, value: &Value, depth: usize) -> Result<(), String> {
    if depth > 128 {
        return Err("schema/value nesting limit".into());
    }
    if let Some(reference) = rule.get("$ref").and_then(Value::as_str) {
        let name = reference
            .strip_prefix("#/$defs/")
            .ok_or("nonlocal schema ref")?;
        return validate(&schema()["$defs"][name], value, depth + 1);
    }
    if let Some(constant) = rule.get("const") {
        if value != constant {
            return Err("constant mismatch".into());
        }
    }
    if let Some(options) = rule.get("enum").and_then(Value::as_array) {
        if !options.contains(value) {
            return Err("enum mismatch".into());
        }
    }
    for (name, exactly_one) in [("anyOf", false), ("oneOf", true)] {
        if let Some(options) = rule.get(name).and_then(Value::as_array) {
            let count = options
                .iter()
                .filter(|option| validate(option, value, depth + 1).is_ok())
                .count();
            if count == 0 || (exactly_one && count != 1) {
                return Err(format!("{name} mismatch"));
            }
        }
    }
    if let Some(options) = rule.get("allOf").and_then(Value::as_array) {
        for option in options {
            validate(option, value, depth + 1)?;
        }
    }
    if let Some(condition) = rule.get("if") {
        let branch = if validate(condition, value, depth + 1).is_ok() {
            "then"
        } else {
            "else"
        };
        if let Some(option) = rule.get(branch) {
            validate(option, value, depth + 1)?;
        }
    }
    if let Some(negative) = rule.get("not") {
        if validate(negative, value, depth + 1).is_ok() {
            return Err("forbidden shape".into());
        }
    }
    if let Some(kind) = rule.get("type").and_then(Value::as_str) {
        let matches = match kind {
            "null" => value.is_null(),
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "boolean" => value.is_boolean(),
            "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
            _ => false,
        };
        if !matches {
            return Err(format!("type {kind} mismatch"));
        }
    }
    if let Some(object) = value.as_object() {
        if let Some(required) = rule.get("required").and_then(Value::as_array) {
            for field in required {
                if !object.contains_key(field.as_str().ok_or("required key")?) {
                    return Err(format!("missing {field}"));
                }
            }
        }
        if let Some(properties) = rule.get("properties").and_then(Value::as_object) {
            for (name, value) in object {
                if let Some(field) = properties.get(name) {
                    validate(field, value, depth + 1)?;
                } else if rule.get("additionalProperties") == Some(&Value::Bool(false)) {
                    return Err(format!("unknown field {name}"));
                }
            }
        }
    }
    if let Some(array) = value.as_array() {
        if let Some(max) = rule.get("maxItems").and_then(Value::as_u64) {
            if array.len() as u64 > max {
                return Err("array bound".into());
            }
        }
        if rule.get("uniqueItems") == Some(&Value::Bool(true)) {
            let mut seen = std::collections::BTreeSet::new();
            for item in array {
                if !seen.insert(canonical(item)) {
                    return Err("duplicate array entry".into());
                }
            }
        }
        if let Some(items) = rule.get("items") {
            for item in array {
                validate(items, item, depth + 1)?;
            }
        }
    }
    if let Some(text) = value.as_str() {
        if let Some(min) = rule.get("minLength").and_then(Value::as_u64) {
            if (text.chars().count() as u64) < min {
                return Err("short string".into());
            }
        }
        if let Some(pattern) = rule.get("pattern").and_then(Value::as_str) {
            static PATTERNS: OnceLock<BTreeMap<String, Regex>> = OnceLock::new();
            let patterns = PATTERNS.get_or_init(|| {
                schema()["$defs"]
                    .as_object()
                    .expect("schema definitions")
                    .values()
                    .filter_map(|d| d.get("pattern").and_then(Value::as_str))
                    .map(|p| (p.into(), Regex::new(p).expect("compiled pattern")))
                    .collect()
            });
            if !patterns
                .get(pattern)
                .ok_or("unknown schema pattern")?
                .is_match(text)
            {
                return Err("string pattern".into());
            }
        }
    }
    if value.is_number() {
        let n = value
            .as_i64()
            .map(i128::from)
            .or_else(|| value.as_u64().map(i128::from))
            .ok_or("integer expected")?;
        if let Some(min) = rule.get("minimum").and_then(Value::as_i64) {
            if n < i128::from(min) {
                return Err("integer minimum".into());
            }
        }
        if let Some(max) = rule.get("maximum").and_then(Value::as_i64) {
            if n > i128::from(max) {
                return Err("integer maximum".into());
            }
        }
    }
    Ok(())
}

/// Reject duplicate keys at every nesting level before interpreting a closed
/// envelope or a control file. serde_json::Value alone would keep the last key.
pub fn strict_json(bytes: &[u8]) -> CatalogResult<Value> {
    struct Strict(Value);
    impl<'de> Deserialize<'de> for Strict {
        fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            struct Visitor;
            impl<'de> serde::de::Visitor<'de> for Visitor {
                type Value = Strict;
                fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    f.write_str("JSON without duplicate keys")
                }
                fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Strict, E> {
                    Ok(Strict(Value::Bool(v)))
                }
                fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Strict, E> {
                    Ok(Strict(json!(v)))
                }
                fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Strict, E> {
                    Ok(Strict(json!(v)))
                }
                fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Strict, E> {
                    serde_json::Number::from_f64(v)
                        .map(|n| Strict(Value::Number(n)))
                        .ok_or_else(|| E::custom("nonfinite JSON number"))
                }
                fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Strict, E> {
                    Ok(Strict(Value::String(v.into())))
                }
                fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Strict, E> {
                    Ok(Strict(Value::String(v)))
                }
                fn visit_none<E: serde::de::Error>(self) -> Result<Strict, E> {
                    Ok(Strict(Value::Null))
                }
                fn visit_unit<E: serde::de::Error>(self) -> Result<Strict, E> {
                    Ok(Strict(Value::Null))
                }
                fn visit_seq<A: serde::de::SeqAccess<'de>>(
                    self,
                    mut seq: A,
                ) -> Result<Strict, A::Error> {
                    let mut values = Vec::new();
                    while let Some(value) = seq.next_element::<Strict>()? {
                        values.push(value.0);
                    }
                    Ok(Strict(Value::Array(values)))
                }
                fn visit_map<A: serde::de::MapAccess<'de>>(
                    self,
                    mut map: A,
                ) -> Result<Strict, A::Error> {
                    let mut values = serde_json::Map::new();
                    while let Some((key, value)) = map.next_entry::<String, Strict>()? {
                        if values.insert(key, value.0).is_some() {
                            return Err(serde::de::Error::custom("duplicate JSON key"));
                        }
                    }
                    Ok(Strict(Value::Object(values)))
                }
            }
            deserializer.deserialize_any(Visitor)
        }
    }
    let mut parser = serde_json::Deserializer::from_slice(bytes);
    let value = Strict::deserialize(&mut parser)?.0;
    parser.end()?;
    Ok(value)
}
