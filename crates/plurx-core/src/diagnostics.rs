//! Fixed-purpose diagnostic data. This observer never grants playback authority.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    io::{Cursor, Read, Write},
};
use thiserror::Error;

pub const VERSION: &str = "cinema.diagnostics.v1";
pub const ARCHIVE_BYTES: usize = 16 * 1024 * 1024;
pub const EXPANDED_BYTES: usize = 64 * 1024 * 1024;
pub const ENTRY_BYTES: usize = 8 * 1024 * 1024;
pub const MANIFEST_BYTES: usize = 64 * 1024;
pub const MAX_FILES: usize = 32;

#[derive(Debug, Error)]
#[error("invalid diagnostics: {0}")]
pub struct Invalid(pub &'static str);
type Result<T> = std::result::Result<T, Invalid>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: String,
    pub report_id: String,
    pub captured_at: String,
    pub category: String,
    pub scope: String,
    pub client: Client,
    pub capture: Capture,
    pub components: Vec<Component>,
    pub privacy: Privacy,
    pub files: Vec<FileDigest>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Client {
    pub platform: String,
    pub app_version: String,
    pub build: String,
    pub os_version: String,
    pub device_model: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capture {
    pub mode: String,
    pub window_ms: u32,
    pub dropped_events: String,
    pub persistence: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Component {
    pub name: String,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Privacy {
    pub titles_included: bool,
    pub attachments_included: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileDigest {
    pub name: String,
    pub size_bytes: u32,
    pub sha256: String,
}

fn valid(ok: bool, why: &'static str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(Invalid(why))
    }
}
pub fn canonical_uuid(s: &str) -> bool {
    uuid::Uuid::parse_str(s).is_ok_and(|id| id.to_string() == s)
}
pub fn decimal(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 20
        && (s == "0" || !s.starts_with('0'))
        && s.bytes().all(|b| b.is_ascii_digit())
        && s.parse::<u64>().is_ok()
}
fn digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn build_label(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
}
fn utc(s: &str) -> bool {
    s.len() == 20
        && s.ends_with('Z')
        && s.as_bytes().get(17..19) != Some(b"60")
        && chrono::DateTime::parse_from_rfc3339(s)
            .is_ok_and(|d| d.format("%Y-%m-%dT%H:%M:%SZ").to_string() == s)
}
pub fn entry_name(s: &str) -> bool {
    matches!(
        s,
        "summary.txt"
            | "description.txt"
            | "client-events.jsonl"
            | "playback.json"
            | "connection.json"
            | "server-events.jsonl"
            | "server-snapshot.json"
            | "diagnostics/platform.json"
    )
}
pub const REASONS: &[&str] = &[
    "connection_failed",
    "timeout",
    "unsupported",
    "context_unavailable",
    "telemetry_disabled",
    "storage_denied",
    "storage_full",
    "overflow",
    "peer_unavailable",
    "cancelled",
    "not_requested",
    "interrupted",
    "clock_unknown",
    "revoked",
    "history_disabled",
    "not_delivered",
    "node_limit",
];
impl Manifest {
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let value = strict_json(bytes, MANIFEST_BYTES)?;
        let manifest: Self =
            serde_json::from_value(value).map_err(|_| Invalid("manifest schema"))?;
        manifest.validate()?;
        Ok(manifest)
    }
    pub fn validate(&self) -> Result<()> {
        valid(
            self.version == VERSION,
            "unsupported version; update the inspector",
        )?;
        valid(canonical_uuid(&self.report_id), "report UUID")?;
        valid(utc(&self.captured_at), "UTC timestamp")?;
        valid(
            ["playback", "connection", "interface", "other"].contains(&self.category.as_str()),
            "category",
        )?;
        valid(
            ["anonymous", "viewer", "administrator"].contains(&self.scope.as_str()),
            "scope",
        )?;
        valid(
            [
                "web",
                "browser_tv",
                "ios",
                "ipados",
                "tvos",
                "android",
                "android_tv",
            ]
            .contains(&self.client.platform.as_str()),
            "platform",
        )?;
        for label in [
            &self.client.app_version,
            &self.client.build,
            &self.client.os_version,
            &self.client.device_model,
        ] {
            valid(build_label(label), "build label")?;
        }
        valid(
            ["ordinary", "reproduction"].contains(&self.capture.mode.as_str()),
            "capture mode",
        )?;
        valid(self.capture.window_ms <= 900_000, "capture window")?;
        valid(decimal(&self.capture.dropped_events), "canonical counter")?;
        valid(
            ["available", "memory_only", "unavailable"]
                .contains(&self.capture.persistence.as_str()),
            "persistence",
        )?;
        valid(
            !self.components.is_empty() && self.components.len() <= 16,
            "component count",
        )?;
        let mut names = BTreeSet::new();
        for component in &self.components {
            valid(
                [
                    "client",
                    "server",
                    "workers",
                    "connection",
                    "platform",
                    "clock",
                ]
                .contains(&component.name.as_str()),
                "component name",
            )?;
            valid(names.insert(&component.name), "duplicate component")?;
            valid(
                ["complete", "partial", "unavailable"].contains(&component.state.as_str()),
                "component state",
            )?;
            valid(
                if component.state == "complete" {
                    component.reason.is_none()
                } else {
                    component
                        .reason
                        .as_ref()
                        .is_some_and(|r| REASONS.contains(&r.as_str()))
                },
                "coverage reason",
            )?;
        }
        valid(
            names.contains(&"client".to_owned()),
            "client coverage absent",
        )?;
        valid(
            self.files.len() >= 3 && self.files.len() < MAX_FILES,
            "file count",
        )?;
        let mut files = BTreeSet::new();
        let mut total = 0usize;
        for file in &self.files {
            valid(
                entry_name(&file.name) && files.insert(file.name.as_str()),
                "entry name",
            )?;
            valid(
                file.size_bytes as usize <= ENTRY_BYTES && digest(&file.sha256),
                "entry bounds/hash",
            )?;
            total = total
                .checked_add(file.size_bytes as usize)
                .ok_or(Invalid("expanded overflow"))?;
        }
        valid(total <= EXPANDED_BYTES, "expanded size")?;
        valid(
            ["summary.txt", "description.txt", "client-events.jsonl"]
                .iter()
                .all(|name| files.contains(name)),
            "required files",
        )?;
        valid(
            self.privacy.attachments_included == files.contains("diagnostics/platform.json"),
            "attachment selection",
        )?;
        Ok(())
    }
    pub fn summary(&self) -> String {
        let coverage = self
            .components
            .iter()
            .map(|c| {
                format!(
                    "{}: {}{}",
                    c.name,
                    c.state,
                    c.reason
                        .as_ref()
                        .map(|r| format!(" ({r})"))
                        .unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!("Cinema diagnostic report {}\nCaptured: {}\nCategory: {}\nClient: {} {} build {}\nCapture: {} ({} ms); dropped {}; persistence {}\n{}\nComplete describes collection scope, not retained history.\n", self.report_id, self.captured_at, self.category, self.client.platform, self.client.app_version, self.client.build, self.capture.mode, self.capture.window_ms, self.capture.dropped_events, self.capture.persistence, coverage)
    }
}

/// Recursive visitor rejects duplicates before serde_json can discard them.
/// Depth and element budgets are checked before allocating nested values.
pub fn strict_json(bytes: &[u8], limit: usize) -> Result<Value> {
    use serde::de::{DeserializeSeed, Error, MapAccess, SeqAccess, Visitor};
    use std::fmt;
    struct Seed<'a> {
        depth: usize,
        budget: &'a mut usize,
    }
    impl<'de> DeserializeSeed<'de> for Seed<'_> {
        type Value = Value;
        fn deserialize<D: serde::Deserializer<'de>>(
            self,
            d: D,
        ) -> std::result::Result<Value, D::Error> {
            if self.depth > 16 || *self.budget == 0 {
                return Err(D::Error::custom("JSON budget"));
            }
            *self.budget -= 1;
            d.deserialize_any(self)
        }
    }
    impl<'de> Visitor<'de> for Seed<'_> {
        type Value = Value;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("bounded integer JSON")
        }
        fn visit_bool<E: Error>(self, v: bool) -> std::result::Result<Value, E> {
            Ok(Value::Bool(v))
        }
        fn visit_u64<E: Error>(self, v: u64) -> std::result::Result<Value, E> {
            Ok(v.into())
        }
        fn visit_i64<E: Error>(self, v: i64) -> std::result::Result<Value, E> {
            if v < 0 {
                Err(E::custom("negative integer"))
            } else {
                Ok(v.into())
            }
        }
        fn visit_f64<E: Error>(self, _: f64) -> std::result::Result<Value, E> {
            Err(E::custom("integer lexeme required"))
        }
        fn visit_str<E: Error>(self, v: &str) -> std::result::Result<Value, E> {
            if v.len() > 16_384 {
                Err(E::custom("string bound"))
            } else {
                Ok(Value::String(v.into()))
            }
        }
        fn visit_string<E: Error>(self, v: String) -> std::result::Result<Value, E> {
            self.visit_str(&v)
        }
        fn visit_unit<E: Error>(self) -> std::result::Result<Value, E> {
            Err(E::custom("null unsupported"))
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> std::result::Result<Value, A::Error> {
            let mut result = Vec::new();
            while let Some(v) = a.next_element_seed(Seed {
                depth: self.depth + 1,
                budget: self.budget,
            })? {
                result.push(v);
            }
            Ok(Value::Array(result))
        }
        fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> std::result::Result<Value, A::Error> {
            let mut result = serde_json::Map::new();
            while let Some(k) = a.next_key::<String>()? {
                if k.len() > 128 || result.contains_key(&k) {
                    return Err(A::Error::custom("duplicate/oversize key"));
                }
                let v = a.next_value_seed(Seed {
                    depth: self.depth + 1,
                    budget: self.budget,
                })?;
                result.insert(k, v);
            }
            Ok(Value::Object(result))
        }
    }
    valid(bytes.len() <= limit, "JSON size")?;
    let mut d = serde_json::Deserializer::from_slice(bytes);
    let v = Seed {
        depth: 0,
        budget: &mut 60_000,
    }
    .deserialize(&mut d)
    .map_err(|_| Invalid("strict JSON"))?;
    d.end().map_err(|_| Invalid("trailing JSON"))?;
    Ok(v)
}

/// Events have a closed fact vocabulary. Free text belongs only to description.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub run_id: String,
    pub sequence: String,
    pub utc: String,
    pub elapsed_ms: u32,
    pub domain: String,
    pub kind: String,
    pub facts: std::collections::BTreeMap<String, Value>,
}
impl Event {
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let e: Self = serde_json::from_value(strict_json(bytes, 2048)?)
            .map_err(|_| Invalid("event schema"))?;
        e.validate()?;
        Ok(e)
    }
    pub fn validate(&self) -> Result<()> {
        valid(
            canonical_uuid(&self.run_id) && decimal(&self.sequence) && utc(&self.utc),
            "event identity/time",
        )?;
        valid(self.elapsed_ms <= 604_800_000, "elapsed bound")?;
        valid(
            [
                "ui",
                "network",
                "playback",
                "producer",
                "lifecycle",
                "coverage",
            ]
            .contains(&self.domain.as_str()),
            "event domain",
        )?;
        valid(
            [
                "action",
                "state",
                "sample",
                "error",
                "terminal",
                "request",
                "availability",
            ]
            .contains(&self.kind.as_str()),
            "event kind",
        )?;
        valid(self.facts.len() <= 16, "fact count")?;
        for (k, v) in &self.facts {
            let allowed = match k.as_str() {
                "duration_ms"
                | "playhead_ms"
                | "headroom_ms"
                | "width"
                | "height"
                | "retry_count"
                | "clock_ahead_ms"
                | "clock_behind_ms"
                | "clock_uncertainty_ms"
                | "active_producers"
                | "hw_slots"
                | "hw_max" => v.as_u64().is_some_and(|n| n <= 604_800_000),
                "status" => v
                    .as_u64()
                    .is_some_and(|n| n == 0 || (100..=599).contains(&n)),
                "frame_count" | "dropped_frames" | "dropped_events" => {
                    v.as_str().is_some_and(decimal)
                }
                "app_version" | "build" => v.as_str().is_some_and(build_label),
                "correlation" => v
                    .as_str()
                    .is_some_and(|s| s.strip_prefix("s-").is_some_and(digest)),
                "node" => v.as_str().is_some_and(|s| {
                    s.strip_prefix("node-")
                        .is_some_and(|s| s.parse::<u8>().is_ok_and(|n| (1..=8).contains(&n)))
                }),
                "source_codec" | "delivered_codec" | "render_codec" | "audio_codec" => {
                    v.as_str().is_some_and(|s| {
                        [
                            "unknown", "h264", "hevc", "av1", "vp9", "aac", "ac3", "eac3", "dts",
                            "truehd", "opus", "flac", "pcm", "other",
                        ]
                        .contains(&s)
                    })
                }
                "source_range" | "delivered_range" | "render_range" => v
                    .as_str()
                    .is_some_and(|s| ["unknown", "sdr", "hdr10", "hlg", "dv"].contains(&s)),
                "mode" => v
                    .as_str()
                    .is_some_and(|s| ["unknown", "direct", "remux", "transcode"].contains(&s)),
                "failure" | "reason" => v.as_str().is_some_and(|s| {
                    REASONS.contains(&s)
                        || [
                            "network",
                            "tls",
                            "dns",
                            "connect",
                            "authentication",
                            "http",
                            "decode",
                            "delivery",
                            "process_exit",
                            "browser_network",
                        ]
                        .contains(&s)
                }),
                "action" => v.as_str().is_some_and(|s| {
                    [
                        "play",
                        "pause",
                        "stop",
                        "seek",
                        "audio",
                        "subtitle",
                        "quality",
                        "navigate",
                        "report",
                        "reproduce",
                        "cancel",
                        "check",
                    ]
                    .contains(&s)
                }),
                "state" => v.as_str().is_some_and(|s| {
                    [
                        "unknown",
                        "starting",
                        "playing",
                        "paused",
                        "waiting",
                        "stalled",
                        "recovered",
                        "failed",
                        "stopped",
                        "foreground",
                        "background",
                        "interrupted",
                        "clean_shutdown",
                    ]
                    .contains(&s)
                }),
                "route" => v.as_str().is_some_and(|s| {
                    [
                        "auth",
                        "browse",
                        "playback",
                        "settings",
                        "diagnostics",
                        "support",
                        "other",
                    ]
                    .contains(&s)
                }),
                "method" => v
                    .as_str()
                    .is_some_and(|s| ["GET", "POST", "PUT", "DELETE", "HEAD"].contains(&s)),
                "phase" => v.as_str().is_some_and(|s| {
                    [
                        "dns",
                        "connect",
                        "tls",
                        "auth",
                        "readiness",
                        "delivery",
                        "unavailable",
                    ]
                    .contains(&s)
                }),
                "observed" => v.is_boolean(),
                _ => false,
            };
            valid(allowed, "forbidden/invalid fact")?;
        }
        valid(
            serde_json::to_vec(self)
                .map_err(|_| Invalid("event encoding"))?
                .len()
                <= 2048,
            "event bytes",
        )
    }
}

pub struct Inspected {
    pub manifest: Manifest,
    pub entries: std::collections::BTreeMap<String, Vec<u8>>,
}
fn verify_contents(
    m: &Manifest,
    entries: &std::collections::BTreeMap<String, Vec<u8>>,
) -> Result<()> {
    valid(entries.len() == m.files.len(), "archive inventory")?;
    let mut event_count = 0;
    for file in &m.files {
        let bytes = entries.get(&file.name).ok_or(Invalid("missing entry"))?;
        valid(
            bytes.len() == file.size_bytes as usize
                && hex::encode(Sha256::digest(bytes)) == file.sha256,
            "entry checksum",
        )?;
        match file.name.as_str() {
            "summary.txt" => valid(
                bytes.as_slice() == m.summary().as_bytes(),
                "summary differs from manifest",
            )?,
            "description.txt" => {
                let s = std::str::from_utf8(bytes).map_err(|_| Invalid("description UTF8"))?;
                valid(
                    s.len() <= 16_384
                        && s.chars().count() <= 4000
                        && !s
                            .chars()
                            .any(|c| c.is_control() && !['\n', '\r', '\t'].contains(&c)),
                    "description bound",
                )?;
            }
            "client-events.jsonl" | "server-events.jsonl" => {
                let text = std::str::from_utf8(bytes).map_err(|_| Invalid("events UTF8"))?;
                let mut last = std::collections::BTreeMap::<String, u64>::new();
                for line in text.lines() {
                    event_count += 1;
                    valid(event_count <= 20_000, "event count")?;
                    let e = Event::decode(line.as_bytes())?;
                    let seq = e.sequence.parse::<u64>().map_err(|_| Invalid("sequence"))?;
                    valid(
                        last.get(&e.run_id).is_none_or(|prior| seq > *prior),
                        "run ordering",
                    )?;
                    last.insert(e.run_id, seq);
                }
            }
            _ => {
                // Optional projections are an array of the same typed events;
                // platform adapters may add only this normalized vocabulary.
                let value = strict_json(bytes, ENTRY_BYTES)?;
                let values = if file.name == "playback.json" && value.is_object() {
                    let object = value.as_object().ok_or(Invalid("playback projection"))?;
                    valid(
                        object.keys().all(|k| k == "events" || k == "title"),
                        "playback fields",
                    )?;
                    if let Some(title) = object.get("title") {
                        let title = title.as_str().ok_or(Invalid("title encoding"))?;
                        valid(
                            m.privacy.titles_included
                                && title.chars().count() <= 256
                                && !title.chars().any(char::is_control),
                            "title consent/bound",
                        )?;
                    }
                    object
                        .get("events")
                        .and_then(Value::as_array)
                        .ok_or(Invalid("playback events"))?
                } else {
                    value.as_array().ok_or(Invalid("projection array"))?
                };
                valid(values.len() <= 10_000, "projection count")?;
                for value in values {
                    let e: Event = serde_json::from_value(value.clone())
                        .map_err(|_| Invalid("projection event"))?;
                    e.validate()?;
                }
            }
        }
    }
    Ok(())
}

/// Never extracts to disk, executes content, or returns unchecked entry bytes.
// Reject oversize/duplicate central-directory claims before ZipArchive allocates
// metadata. V1 deliberately accepts single-disk, non-ZIP64 archives only.
fn central_directory(bytes: &[u8]) -> Result<()> {
    fn u16_at(b: &[u8], i: usize) -> Result<usize> {
        Ok(u16::from_le_bytes(
            b.get(i..i + 2)
                .ok_or(Invalid("ZIP header"))?
                .try_into()
                .map_err(|_| Invalid("ZIP header"))?,
        ) as usize)
    }
    fn u32_at(b: &[u8], i: usize) -> Result<usize> {
        Ok(u32::from_le_bytes(
            b.get(i..i + 4)
                .ok_or(Invalid("ZIP header"))?
                .try_into()
                .map_err(|_| Invalid("ZIP header"))?,
        ) as usize)
    }
    valid(bytes.len() >= 22, "ZIP end")?;
    let end = bytes.len() - 22;
    valid(
        &bytes[end..end + 4] == b"PK\x05\x06" && u16_at(bytes, end + 20)? == 0,
        "ZIP comment/end",
    )?;
    valid(
        u16_at(bytes, end + 4)? == 0 && u16_at(bytes, end + 6)? == 0,
        "ZIP multi disk",
    )?;
    let count = u16_at(bytes, end + 10)?;
    valid(
        count > 0 && count <= MAX_FILES && u16_at(bytes, end + 8)? == count,
        "ZIP count",
    )?;
    let size = u32_at(bytes, end + 12)?;
    let start = u32_at(bytes, end + 16)?;
    valid(start.checked_add(size) == Some(end), "ZIP central extent")?;
    let mut at = start;
    let mut names = BTreeSet::new();
    for _ in 0..count {
        valid(
            bytes.get(at..at + 4) == Some(b"PK\x01\x02"),
            "ZIP central record",
        )?;
        let name_len = u16_at(bytes, at + 28)?;
        let extra = u16_at(bytes, at + 30)?;
        let comment = u16_at(bytes, at + 32)?;
        valid(
            name_len > 0
                && name_len <= 64
                && extra <= 1024
                && comment == 0
                && u16_at(bytes, at + 34)? == 0,
            "ZIP metadata bounds",
        )?;
        valid(u16_at(bytes, at + 8)? & 1 == 0, "encrypted ZIP")?;
        let name = std::str::from_utf8(
            bytes
                .get(at + 46..at + 46 + name_len)
                .ok_or(Invalid("ZIP name"))?,
        )
        .map_err(|_| Invalid("ZIP name UTF8"))?;
        valid(
            (name == "manifest.json" || entry_name(name)) && names.insert(name),
            "ZIP duplicate/name",
        )?;
        let local = u32_at(bytes, at + 42)?;
        valid(
            local < start && bytes.get(local..local + 4) == Some(b"PK\x03\x04"),
            "ZIP local header",
        )?;
        let local_name = u16_at(bytes, local + 26)?;
        let local_extra = u16_at(bytes, local + 28)?;
        valid(
            bytes.get(local + 30..local + 30 + local_name) == Some(name.as_bytes()),
            "ZIP name disagreement",
        )?;
        let compressed = u32_at(bytes, at + 20)?;
        let body = local
            .checked_add(30 + local_name + local_extra)
            .ok_or(Invalid("ZIP offset"))?;
        valid(
            body.checked_add(compressed)
                .is_some_and(|last| last <= start),
            "ZIP compressed extent",
        )?;
        at = at
            .checked_add(46 + name_len + extra + comment)
            .ok_or(Invalid("ZIP central overflow"))?;
        valid(at <= end, "ZIP central bound")?;
    }
    valid(at == end, "ZIP directory count")
}

pub fn inspect(bytes: &[u8]) -> Result<Inspected> {
    valid(bytes.len() <= ARCHIVE_BYTES, "compressed size")?;
    central_directory(bytes)?;
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| Invalid("ZIP structure"))?;
    valid(archive.len() <= MAX_FILES, "ZIP entry count")?;
    let mut entries = std::collections::BTreeMap::new();
    let mut expanded = 0usize;
    for index in 0..archive.len() {
        let mut file = archive.by_index(index).map_err(|_| Invalid("ZIP entry"))?;
        let name = file.name().to_owned();
        valid(name == "manifest.json" || entry_name(&name), "ZIP name")?;
        valid(
            !file.encrypted()
                && !file.is_dir()
                && !file.is_symlink()
                && file
                    .unix_mode()
                    .is_none_or(|m| m & 0o170000 == 0 || m & 0o170000 == 0o100000),
            "ZIP entry type",
        )?;
        valid(
            matches!(
                file.compression(),
                zip::CompressionMethod::Stored | zip::CompressionMethod::Deflated
            ),
            "ZIP compression",
        )?;
        let limit = if name == "manifest.json" {
            MANIFEST_BYTES
        } else {
            ENTRY_BYTES
        };
        valid(file.size() <= limit as u64, "declared entry size")?;
        let mut data = Vec::new();
        (&mut file)
            .take((limit + 1) as u64)
            .read_to_end(&mut data)
            .map_err(|_| Invalid("ZIP content/CRC"))?;
        valid(
            data.len() <= limit && data.len() as u64 == file.size(),
            "actual entry size",
        )?;
        expanded = expanded
            .checked_add(data.len())
            .ok_or(Invalid("expanded overflow"))?;
        valid(
            expanded <= EXPANDED_BYTES && !entries.contains_key(&name),
            "expanded/duplicate ZIP",
        )?;
        entries.insert(name, data);
    }
    let manifest_bytes = entries
        .remove("manifest.json")
        .ok_or(Invalid("manifest missing"))?;
    let manifest = Manifest::decode(&manifest_bytes)?;
    verify_contents(&manifest, &entries)?;
    Ok(Inspected { manifest, entries })
}

/// Hashes final contents and adds a manifest excluded from its own inventory.
pub fn export(
    mut m: Manifest,
    mut entries: std::collections::BTreeMap<String, Vec<u8>>,
) -> Result<Vec<u8>> {
    entries.insert("summary.txt".into(), m.summary().into_bytes());
    m.files = entries
        .iter()
        .map(|(name, data)| FileDigest {
            name: name.clone(),
            size_bytes: data.len().try_into().unwrap_or(u32::MAX),
            sha256: hex::encode(Sha256::digest(data)),
        })
        .collect();
    m.validate()?;
    verify_contents(&m, &entries)?;
    entries.insert(
        "manifest.json".into(),
        serde_json::to_vec(&m).map_err(|_| Invalid("manifest encoding"))?,
    );
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, data) in entries {
        writer
            .start_file(
                name,
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated),
            )
            .map_err(|_| Invalid("ZIP write"))?;
        writer.write_all(&data).map_err(|_| Invalid("ZIP write"))?;
    }
    let bytes = writer
        .finish()
        .map_err(|_| Invalid("ZIP finish"))?
        .into_inner();
    valid(bytes.len() <= ARCHIVE_BYTES, "compressed size")?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_event_fixtures_preserve_codec_layers_and_refuse_private_fields() {
        let fixture: Value =
            serde_json::from_str(include_str!("../tests/fixtures/diagnostics-events-v1.json"))
                .expect("fixture");
        for case in fixture["cases"].as_array().expect("cases") {
            assert_eq!(
                Event::decode(case["json"].as_str().expect("JSON").as_bytes()).is_ok(),
                case["valid"].as_bool().expect("outcome"),
                "{}",
                case["name"]
            );
        }
    }
    #[test]
    fn title_projection_requires_explicit_consent_and_frame_counters_are_strings() {
        let mut manifest = Manifest::decode(
            fixture()["cases"][0]["json"]
                .as_str()
                .expect("fixture")
                .as_bytes(),
        )
        .expect("manifest");
        let mut entries = std::collections::BTreeMap::from([
            ("description.txt".into(), Vec::new()),
            ("client-events.jsonl".into(), Vec::new()),
            (
                "playback.json".into(),
                br#"{"events":[],"title":"Explicitly selected title"}"#.to_vec(),
            ),
        ]);
        assert!(export(manifest.clone(), entries.clone()).is_err());
        manifest.privacy.titles_included = true;
        let archive = export(manifest, entries.clone()).expect("consented archive");
        assert!(inspect(&archive).is_ok());
        entries.insert(
            "playback.json".into(),
            br#"{"events":[],"title":"Selected","url":"CANARY_SECRET"}"#.to_vec(),
        );
        assert!(export(inspect(&archive).expect("inspect").manifest, entries).is_err());
        let base = serde_json::json!({"run_id":"00000000-0000-4000-8000-000000000001","sequence":"0","utc":"2026-10-12T16:00:00Z","elapsed_ms":0,"domain":"playback","kind":"sample","facts":{"frame_count":"18446744073709551615","dropped_frames":"9007199254740993"}});
        assert!(Event::decode(serde_json::to_string(&base).expect("JSON").as_bytes()).is_ok());
        let mut invalid = base;
        invalid["facts"]["dropped_frames"] = 1.into();
        assert!(Event::decode(serde_json::to_string(&invalid).expect("JSON").as_bytes()).is_err());
    }
    fn fixture() -> Value {
        serde_json::from_str(include_str!("../tests/fixtures/diagnostics-v1.json"))
            .expect("fixture")
    }
    #[test]
    fn shared_manifest_fixtures_reject_privacy_and_encoding_violations() {
        for case in fixture()["cases"].as_array().expect("cases") {
            assert_eq!(
                Manifest::decode(case["json"].as_str().expect("raw").as_bytes()).is_ok(),
                case["valid"].as_bool().expect("outcome"),
                "{}",
                case["name"]
            );
        }
    }
    fn manifest() -> Manifest {
        Manifest::decode(
            fixture()["cases"][0]["json"]
                .as_str()
                .expect("raw")
                .as_bytes(),
        )
        .expect("valid")
    }
    #[test]
    fn archive_round_trip_checks_actual_hash_inventory_and_readable_coverage() {
        let entries = [
            ("description.txt".into(), b"Picture stopped".to_vec()),
            ("client-events.jsonl".into(), Vec::new()),
        ]
        .into();
        let bytes = export(manifest(), entries).expect("export");
        let result = inspect(&bytes).expect("inspect");
        assert!(result
            .manifest
            .summary()
            .contains("server: unavailable (connection_failed)"));
        assert!(!result.manifest.summary().contains("CANARY_SECRET"));
        let mut entries = result.entries;
        entries.insert("description.txt".into(), b"tampered".to_vec());
        assert!(verify_contents(&result.manifest, &entries).is_err());
    }
    #[test]
    fn archive_rejects_traversal_symlinks_duplicates_bombs_and_unknown_entries() {
        for name in [
            "../manifest.json",
            "/manifest.json",
            "nested.zip",
            "secret.txt",
        ] {
            let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
            w.start_file(name, zip::write::SimpleFileOptions::default())
                .expect("entry");
            w.write_all(b"{}").expect("write");
            assert!(inspect(&w.finish().expect("ZIP").into_inner()).is_err());
        }
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        w.start_file(
            "description.txt",
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated),
        )
        .expect("entry");
        w.write_all(&vec![b'x'; ENTRY_BYTES + 1]).expect("bomb");
        assert!(inspect(&w.finish().expect("ZIP").into_inner()).is_err());
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        w.add_symlink(
            "manifest.json",
            "elsewhere",
            zip::write::SimpleFileOptions::default(),
        )
        .expect("symlink");
        assert!(inspect(&w.finish().expect("ZIP").into_inner()).is_err());
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        w.start_file("manifest.json", zip::write::SimpleFileOptions::default())
            .expect("entry");
        w.write_all(b"{}").expect("write");
        let mut bytes = w.finish().expect("ZIP").into_inner();
        // Duplicate central directory record without asking ZipWriter to accept a duplicate.
        let at = bytes
            .windows(4)
            .position(|b| b == b"PK\x01\x02")
            .expect("central");
        let end = bytes
            .windows(4)
            .position(|b| b == b"PK\x05\x06")
            .expect("end");
        let central = bytes[at..end].to_vec();
        bytes.splice(end..end, central.clone());
        let end = end + central.len();
        bytes[end + 8..end + 10].copy_from_slice(&2u16.to_le_bytes());
        bytes[end + 10..end + 12].copy_from_slice(&2u16.to_le_bytes());
        bytes[end + 12..end + 16].copy_from_slice(&((central.len() * 2) as u32).to_le_bytes());
        assert!(inspect(&bytes).is_err());
    }
    #[test]
    fn event_schema_excludes_canary_secrets_and_arbitrary_text() {
        let mut v: Value = serde_json::from_str(r#"{"run_id":"00000000-0000-4000-8000-000000000001","sequence":"1","utc":"2026-10-12T16:00:00Z","elapsed_ms":1,"domain":"network","kind":"request","facts":{"route":"auth","status":401}}"#).expect("event");
        assert!(Event::decode(&serde_json::to_vec(&v).expect("encode")).is_ok());
        for key in ["url", "headers", "error", "path", "email", "token"] {
            v["facts"][key] = "CANARY_SECRET_user_host_/home/user?key=secret#proof".into();
            assert!(Event::decode(&serde_json::to_vec(&v).expect("encode")).is_err());
            v["facts"].as_object_mut().expect("facts").remove(key);
        }
    }
}
