//! Text models reuse the managed, hash-verified artifact installer.
use crate::subtitle_models::artifacts::{self, CatalogModel, ManagedModel};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::LazyLock,
    time::Instant,
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

/// Only immutable compiled catalog entries may use the shared artifact downloader.
pub(crate) fn registered_file(spec: &CatalogModel) -> bool {
    SPEC.files.iter().any(|part| part.spec == *spec)
}

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
/// Metadata reads never reopen descriptor paths or change a shared file cursor.
pub(crate) fn read_metadata(
    model: &ManagedModel,
    limit: usize,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<Vec<u8>, String> {
    let active = || {
        if cancel.is_cancelled() {
            Err("worker_interrupted".to_owned())
        } else if Instant::now() >= deadline {
            Err("text_inference_timeout".to_owned())
        } else {
            Ok(())
        }
    };
    active()?;
    let size = usize::try_from(model.identity.size)
        .ok()
        .filter(|size| *size > 0 && *size <= limit)
        .ok_or("text_model_invalid")?;
    let identity = || {
        plurx_core::fs_secure::std_file_identity(&model.handle)
            .map_err(|_| "model_changed".to_owned())
    };
    if identity()? != model.identity {
        return Err("model_changed".into());
    }
    active()?;
    let mut bytes = vec![0_u8; size];
    for (index, chunk) in bytes.chunks_mut(128 * 1024).enumerate() {
        active()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            model
                .handle
                .read_exact_at(chunk, (index * 128 * 1024) as u64)
                .map_err(|error| {
                    tracing::warn!(%error, "Text model metadata read failed");
                    "text_model_invalid".to_owned()
                })?;
        }
        #[cfg(not(unix))]
        {
            let _ = (index, chunk);
            return Err("engine_unavailable".into());
        }
    }
    if identity()? != model.identity {
        return Err("model_changed".into());
    }
    active()?;
    Ok(bytes)
}

#[cfg(all(test, unix))]
mod metadata_tests {
    use super::*;
    use std::io::{Seek, SeekFrom};
    fn fixture() -> (tempfile::TempDir, ManagedModel) {
        let dir = tempfile::tempdir().expect("metadata fixture");
        let path = dir.path().join("vocab.json");
        std::fs::write(&path, b"verified vocabulary").expect("fixture bytes");
        let handle = std::fs::File::open(&path).expect("held metadata");
        let identity = plurx_core::fs_secure::std_file_identity(&handle).expect("held identity");
        (
            dir,
            ManagedModel {
                handle,
                path,
                identity,
                sha256: "a".repeat(64),
            },
        )
    }
    #[test]
    fn repeated_metadata_reads_ignore_and_preserve_the_held_cursor() {
        let (_dir, mut model) = fixture();
        let end = model
            .handle
            .seek(SeekFrom::End(0))
            .expect("held cursor at EOF");
        let cancel = CancellationToken::new();
        let deadline = Instant::now() + std::time::Duration::from_secs(2);
        for _ in 0..2 {
            assert_eq!(
                read_metadata(&model, 128, &cancel, deadline).expect("positional bytes"),
                b"verified vocabulary"
            );
            assert_eq!(model.handle.stream_position().expect("held cursor"), end);
        }
    }
    #[test]
    fn metadata_limits_cancellation_deadline_and_identity_fail_closed() {
        let (_dir, model) = fixture();
        let cancel = CancellationToken::new();
        let deadline = Instant::now() + std::time::Duration::from_secs(2);
        assert_eq!(
            read_metadata(&model, 1, &cancel, deadline).expect_err("size bound"),
            "text_model_invalid"
        );
        assert_eq!(
            read_metadata(&model, 128, &cancel, Instant::now()).expect_err("deadline"),
            "text_inference_timeout"
        );
        cancel.cancel();
        assert_eq!(
            read_metadata(&model, 128, &cancel, deadline).expect_err("cancellation"),
            "worker_interrupted"
        );
        std::fs::write(&model.path, b"replaced").expect("changed held object");
        assert_eq!(
            read_metadata(&model, 128, &CancellationToken::new(), deadline).expect_err("identity"),
            "model_changed"
        );
    }
}
