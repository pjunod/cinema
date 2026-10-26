//! Text models reuse the managed, hash-verified artifact installer.
use crate::subtitle_models::artifacts::{self, CatalogModel, ManagedModel};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::LazyLock,
};
use tokio_util::sync::CancellationToken;

pub(crate) const MODEL_ID: &str = "opus-mt-mul-en";
pub(crate) const ADAPTER_CONTRACT: &str = "candle-marian-0.11-v1";
const CATALOG: &str = include_str!("catalog.json");
#[derive(Deserialize)]
struct Part {
    role: String,
    #[serde(flatten)]
    spec: CatalogModel,
}
#[derive(Deserialize)]
struct Catalog {
    source_languages: Vec<String>,
    files: Vec<Part>,
}
static SPEC: LazyLock<Catalog> =
    LazyLock::new(|| serde_json::from_str(CATALOG).expect("pinned text translation catalog"));

pub(crate) fn expected_bundle_digest() -> String {
    hex::encode(Sha256::digest(CATALOG.as_bytes()))
}
pub(crate) fn input_languages() -> BTreeSet<String> {
    SPEC.source_languages
        .iter()
        .map(|s| {
            crate::subtitle_transcription::normalized_language(
                s.split(['_', '-']).next().unwrap_or(s),
            )
        })
        .filter(|s| (2..=3).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_lowercase()))
        .collect()
}
pub(crate) fn supports_language(language: &str) -> bool {
    input_languages().contains(&crate::subtitle_transcription::normalized_language(
        language,
    ))
}
pub(crate) fn catalog_public() -> Value {
    json!({"version":1,"model_id":MODEL_ID,"label":"Multilingual subtitles to English","adapter_contract":ADAPTER_CONTRACT,
      "bundle_digest":expected_bundle_digest(),"model_sha256":SPEC.files.iter().find(|p|p.role=="weights").map(|p|&p.spec.sha256),
      "input_languages":input_languages(),"target_language":"en","license":"Apache-2.0",
      "download_bytes":SPEC.files.iter().map(|p|p.spec.download_bytes).sum::<u64>(),
      "source":"https://huggingface.co/Helsinki-NLP/opus-mt-mul-en/tree/848eae0c1676cfce9bb791c200e8228e5a6396ff"})
}
pub(crate) struct VerifiedBundle {
    pub(crate) bundle_digest: String,
    pub(crate) files: BTreeMap<String, ManagedModel>,
}
impl VerifiedBundle {
    pub(crate) fn file(&self, role: &str) -> Result<&ManagedModel, String> {
        self.files
            .get(role)
            .ok_or_else(|| "text_model_bundle_invalid".into())
    }
    pub(crate) fn identity(&self) -> String {
        let mut hash = Sha256::new();
        for (role, model) in &self.files {
            hash.update(role.as_bytes());
            hash.update([0]);
            hash.update(artifacts::identity_key(model.identity).as_bytes());
            hash.update([0]);
        }
        hex::encode(hash.finalize())
    }
}
pub(crate) async fn inspect(
    root: &Path,
    cancel: &CancellationToken,
) -> Result<VerifiedBundle, String> {
    let mut files = BTreeMap::new();
    for part in &SPEC.files {
        files.insert(
            part.role.clone(),
            artifacts::open_model_cancellable(root, &part.spec, cancel).await?,
        );
    }
    Ok(VerifiedBundle {
        bundle_digest: expected_bundle_digest(),
        files,
    })
}
pub(crate) async fn install(
    root: &Path,
    cancel: &CancellationToken,
) -> Result<VerifiedBundle, String> {
    let mut files = BTreeMap::new();
    for part in &SPEC.files {
        files.insert(
            part.role.clone(),
            artifacts::install_model(root, &part.spec, cancel).await?,
        );
    }
    Ok(VerifiedBundle {
        bundle_digest: expected_bundle_digest(),
        files,
    })
}
/// Read the already verified held object, never a replacement at its cache pathname.
pub(crate) fn held_path(model: &ManagedModel) -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        PathBuf::from(format!("/proc/self/fd/{}", model.handle.as_raw_fd()))
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        use std::os::fd::AsRawFd;
        PathBuf::from(format!("/dev/fd/{}", model.handle.as_raw_fd()))
    }
    #[cfg(not(unix))]
    {
        model.path.clone()
    }
}
