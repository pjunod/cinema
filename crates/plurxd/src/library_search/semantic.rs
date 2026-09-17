//! Optional CPU sentence embeddings. Model files and vectors stay on this node.
use crate::state::AppState;
use anyhow::{bail, Result};
use candle_core::{Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config, DTYPE};
use plurx_core::store::classification::Entry;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::Duration,
};
use tokenizers::Tokenizer;
use tokio_util::sync::CancellationToken;
const REVISION: &str = "1110a243fdf4706b3f48f1d95db1a4f5529b4d41";
const DIM: usize = 384;
const MAX_ITEMS: usize = 100_000;
const FILES: [(&str, &str, u64); 3] = [
    (
        "config.json",
        "953f9c0d463486b10a6871cc2fd59f223b2c70184f49815e7efbcab5d8908b41",
        612,
    ),
    (
        "tokenizer.json",
        "be50c3628f2bf5bb5e3a7f17b1f74611b2561a3a27eeab05e5aa30f411572037",
        466247,
    ),
    (
        "model.safetensors",
        "53aa51172d142c89d9012cce15ae4d6cc0ca6895895114379cacb4fab128d9db",
        90868376,
    ),
];
struct Encoder {
    model: BertModel,
    tokenizer: Tokenizer,
}
impl Encoder {
    fn load(dir: &Path) -> Result<Self> {
        let config: Config = serde_json::from_slice(&std::fs::read(dir.join("config.json"))?)?;
        let mut tokenizer =
            Tokenizer::from_file(dir.join("tokenizer.json")).map_err(|e| anyhow::anyhow!("{e}"))?;
        tokenizer
            .with_padding(None)
            .with_truncation(Some(tokenizers::TruncationParams {
                max_length: 256,
                ..Default::default()
            }))
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let weights = std::fs::read(dir.join("model.safetensors"))?;
        let vb = VarBuilder::from_buffered_safetensors(weights, DTYPE, &Device::Cpu)?;
        Ok(Self {
            model: BertModel::load(vb, &config)?,
            tokenizer,
        })
    }
    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let tokens = self
            .tokenizer
            .encode(text, true)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let ids = Tensor::new(tokens.get_ids(), &Device::Cpu)?.unsqueeze(0)?;
        let output = self
            .model
            .forward(&ids, &ids.zeros_like()?, None)?
            .mean(1)?
            .squeeze(0)?
            .to_vec1::<f32>()?;
        normalize(output)
    }
}
fn normalize(mut values: Vec<f32>) -> Result<Vec<f32>> {
    let norm = values.iter().map(|v| v * v).sum::<f32>().sqrt();
    if values.len() != DIM || !norm.is_finite() || norm <= 0.0 {
        bail!("invalid sentence vector");
    }
    values.iter_mut().for_each(|v| *v /= norm);
    Ok(values)
}
#[derive(Clone, Serialize, Deserialize)]
struct Vector {
    source: String,
    embedding: Vec<f32>,
}
#[derive(Default, Serialize, Deserialize)]
struct Index {
    revision: String,
    rows: BTreeMap<i64, Vector>,
}
#[derive(Default)]
struct Runtime {
    encoder: Option<Encoder>,
    index: Index,
    phase: &'static str,
}
static RUNTIME: OnceLock<Arc<Mutex<Runtime>>> = OnceLock::new();
static ENABLED: AtomicBool = AtomicBool::new(false);
fn runtime() -> Arc<Mutex<Runtime>> {
    RUNTIME.get_or_init(Default::default).clone()
}
pub fn status() -> serde_json::Value {
    let r = runtime();
    let Ok(r) = r.try_lock() else {
        return serde_json::json!({"state":"working"});
    };
    serde_json::json!({"state":if ENABLED.load(Ordering::Acquire){r.phase}else{"disabled"},"indexed":r.index.rows.len(),"model":"all-MiniLM-L6-v2","revision":REVISION})
}
pub fn disable() {
    ENABLED.store(false, Ordering::Release);
    if let Ok(mut r) = runtime().try_lock() {
        *r = Runtime::default();
    }
}
fn phase(value: &'static str) {
    if let Ok(mut r) = runtime().try_lock() {
        r.phase = value;
    }
}
fn directory(state: &AppState) -> PathBuf {
    PathBuf::from(&state.system.data_dir)
        .join("semantic-search")
        .join(REVISION)
}
async fn download(dir: &Path) -> Result<()> {
    tokio::fs::create_dir_all(dir).await?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .build()?;
    for (name, hash, size) in FILES {
        if !ENABLED.load(Ordering::Acquire) {
            bail!("disabled");
        }
        let path = dir.join(name);
        if tokio::fs::metadata(&path)
            .await
            .is_ok_and(|m| m.len() == size)
        {
            let data = tokio::fs::read(&path).await?;
            if hex::encode(Sha256::digest(&data)) == hash {
                continue;
            }
        }
        let url=format!("https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/{REVISION}/{name}");
        let mut response = client.get(url).send().await?.error_for_status()?;
        let mut data = Vec::with_capacity(size as usize);
        while let Some(chunk) = response.chunk().await? {
            if !ENABLED.load(Ordering::Acquire) {
                bail!("disabled");
            }
            if data.len() + chunk.len() > size as usize {
                bail!("model file exceeds manifest size");
            }
            data.extend_from_slice(&chunk);
        }
        if data.len() != size as usize || hex::encode(Sha256::digest(&data)) != hash {
            bail!("model file checksum mismatch");
        }
        let pending = dir.join(format!("{name}.part"));
        tokio::fs::write(&pending, &data).await?;
        tokio::fs::rename(pending, path).await?;
    }
    Ok(())
}
fn embedding_text(entry: &Entry) -> Result<String> {
    let input = entry.input()?;
    let labels = entry
        .record
        .as_ref()
        .filter(|r| r.source_json == entry.source_json)
        .map(|r| r.classification.terms(&r.overrides).join(", "))
        .unwrap_or_default();
    Ok(format!(
        "{}. {}. {}. {}. {}",
        input.title,
        input.genres.join(", "),
        input.tags.join(", "),
        labels,
        input.overview
    )
    .chars()
    .take(8000)
    .collect())
}
fn source_key(entry: &Entry) -> String {
    // Corrections and newly imported provider keywords also invalidate embeddings.
    hex::encode(Sha256::digest(
        format!(
            "{}:{}",
            entry.source_json,
            entry.record.as_ref().map(|r| r.revision).unwrap_or(0)
        )
        .as_bytes(),
    ))
}
pub async fn worker(state: AppState, shutdown: CancellationToken) {
    let mut cursor = 0;
    let mut seen = BTreeSet::new();
    loop {
        let enabled = state
            .store
            .get_setting(super::SEMANTIC_KEY)
            .await
            .ok()
            .flatten()
            .as_deref()
            == Some("true");
        if !enabled {
            disable();
            cursor = 0;
            seen.clear();
        } else {
            ENABLED.store(true, Ordering::Release);
            let result = tokio::select! {_=shutdown.cancelled()=>{disable();return;},result=step(&state,&mut cursor,&mut seen)=>result};
            if let Err(error) = result {
                tracing::warn!(%error,"Embedded semantic search paused; local text search remains available");
                phase("unavailable");
            }
        }
        tokio::select! {_=shutdown.cancelled()=>{disable();return;},_=tokio::time::sleep(Duration::from_secs(if enabled&&cursor>0{1}else{30}))=>{}}
    }
}
async fn step(state: &AppState, cursor: &mut i64, seen: &mut BTreeSet<i64>) -> Result<()> {
    if !ENABLED.load(Ordering::Acquire) {
        return Ok(());
    }
    let dir = directory(state);
    let rt = runtime();
    let loaded = match rt.try_lock() {
        Ok(r) => r.encoder.is_some(),
        Err(_) => return Ok(()),
    };
    if !loaded {
        phase("downloading");
        download(&dir).await?;
        phase("loading");
        let r = runtime();
        let model_dir = dir.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let encoder = Encoder::load(&model_dir)?;
            let mut index = Index::default();
            let path = model_dir.join("vectors.json");
            if std::fs::metadata(&path).is_ok_and(|m| m.len() < 300_000_000) {
                if let Ok(cached) = serde_json::from_slice::<Index>(&std::fs::read(path)?) {
                    if cached.revision == REVISION
                        && cached.rows.len() <= MAX_ITEMS
                        && cached.rows.values().all(|v| {
                            v.embedding.len() == DIM && v.embedding.iter().all(|x| x.is_finite())
                        })
                    {
                        index = cached;
                    }
                }
            }
            if ENABLED.load(Ordering::Acquire) {
                let mut r = r.lock().map_err(|_| anyhow::anyhow!("model lock"))?;
                r.encoder = Some(encoder);
                r.index = index;
                r.phase = "indexing";
            }
            Ok(())
        })
        .await??;
    }
    let entries = state.store.classification_page(*cursor, 16).await?;
    if entries.is_empty() || seen.len() >= MAX_ITEMS {
        let r = runtime();
        let keep = std::mem::take(seen);
        let capped = keep.len() >= MAX_ITEMS;
        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut r = r.lock().map_err(|_| anyhow::anyhow!("model lock"))?;
            r.index.rows.retain(|id, _| keep.contains(id));
            r.index.revision = REVISION.into();
            r.phase = if capped {
                "ready_capacity_limit"
            } else {
                "ready"
            };
            if ENABLED.load(Ordering::Acquire) {
                let temp = dir.join("vectors.json.part");
                std::fs::write(&temp, serde_json::to_vec(&r.index)?)?;
                std::fs::rename(temp, dir.join("vectors.json"))?;
            }
            Ok(())
        })
        .await??;
        *cursor = 0;
        return Ok(());
    }
    for entry in entries {
        let id = entry.input()?.id;
        *cursor = id;
        seen.insert(id);
        let source = source_key(&entry);
        let text = embedding_text(&entry)?;
        let r = runtime();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut r = r.lock().map_err(|_| anyhow::anyhow!("model lock"))?;
            if !ENABLED.load(Ordering::Acquire) {
                *r = Runtime::default();
                return Ok(());
            }
            if r.index.rows.get(&id).is_some_and(|v| v.source == source) {
                return Ok(());
            }
            if let Some(encoder) = &r.encoder {
                let embedding = encoder.embed(&text)?;
                r.index.rows.insert(id, Vector { source, embedding });
            }
            Ok(())
        })
        .await??;
    }
    Ok(())
}
/// Best-effort related IDs. Never delays or substitutes for the text endpoint.
pub async fn related(state: &AppState, query: &str) -> Vec<i64> {
    if query.trim().is_empty()
        || query.chars().count() > 1000
        || state
            .store
            .get_setting(super::SEMANTIC_KEY)
            .await
            .ok()
            .flatten()
            .as_deref()
            != Some("true")
        || !ENABLED.load(Ordering::Acquire)
    {
        return Vec::new();
    }
    let r = runtime();
    let text = query.to_owned();
    let operation = tokio::task::spawn_blocking(move || -> Result<Vec<(i64, String)>> {
        let Ok(r) = r.try_lock() else {
            return Ok(Vec::new());
        };
        let Some(encoder) = &r.encoder else {
            return Ok(Vec::new());
        };
        let embedding = encoder.embed(&text)?;
        let mut scored = r
            .index
            .rows
            .iter()
            .map(|(id, v)| {
                (
                    *id,
                    v,
                    embedding
                        .iter()
                        .zip(&v.embedding)
                        .map(|(a, b)| a * b)
                        .sum::<f32>(),
                )
            })
            .filter(|(_, _, s)| *s >= 0.35)
            .collect::<Vec<_>>();
        scored.sort_by(|a, b| b.2.total_cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
        Ok(scored
            .into_iter()
            .take(20)
            .map(|(id, v, _)| (id, v.source.clone()))
            .collect())
    });
    let Ok(Ok(Ok(hits))) = tokio::time::timeout(Duration::from_secs(2), operation).await else {
        return Vec::new();
    };
    let mut results = Vec::new();
    for (id, source) in hits {
        if let Ok(entries) = state
            .store
            .classification_page(id.saturating_sub(1), 1)
            .await
        {
            if entries
                .first()
                .is_some_and(|e| e.input().is_ok_and(|i| i.id == id) && source_key(e) == source)
            {
                results.push(id);
            }
        }
    }
    if ENABLED.load(Ordering::Acquire) {
        results
    } else {
        Vec::new()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn vector_validation_rejects_invalid_model_output() {
        assert!(normalize(vec![0.; DIM]).is_err());
        assert!(normalize(vec![f32::NAN; DIM]).is_err());
        let v = normalize(vec![1.; DIM]).expect("vector");
        assert!((v.iter().map(|x| x * x).sum::<f32>() - 1.).abs() < 0.00001);
    }
    #[test]
    #[ignore = "downloads are opt-in; set PLURX_TEST_MINILM_DIR to verified model files"]
    fn embedded_model_distinguishes_meaning() {
        let dir = std::env::var("PLURX_TEST_MINILM_DIR").expect("model directory");
        let e = Encoder::load(Path::new(&dir)).expect("load");
        let q = e
            .embed("a documentary about astronauts exploring the universe")
            .expect("query");
        let space = e
            .embed("Space exploration: a factual film about NASA missions to the moon")
            .expect("space");
        let cooking = e
            .embed("A chef bakes bread and prepares a delicious dinner")
            .expect("cooking");
        let score = |v: &[f32]| q.iter().zip(v).map(|(a, b)| a * b).sum::<f32>();
        assert!(score(&space) > score(&cooking) + 0.15);
    }
}
