//! Bounded sharing for immutable generation manifests.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use plurx_core::transcode::manifest::GenerationManifest;

const MAX_ENTRIES: usize = 16;
const MAX_DECODED_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenerationKey {
    pub cache_root: PathBuf,
    pub node_id: String,
    pub recipe_hash: String,
    pub storage_class: String,
    pub relative_dir: String,
    pub manifest_digest: String,
}

struct Entry {
    key: GenerationKey,
    manifest: Arc<GenerationManifest>,
    decoded_bytes: usize,
}

#[derive(Default)]
struct Cache {
    entries: VecDeque<Entry>,
    decoded_bytes: usize,
}

fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Cache::default()))
}

fn decoded_weight(manifest: &GenerationManifest) -> usize {
    manifest.objects.iter().fold(
        std::mem::size_of::<GenerationManifest>()
            .saturating_add(manifest.generation_id.len())
            .saturating_add(manifest.manifest_digest.len()),
        |total, object| {
            total
                .saturating_add(std::mem::size_of_val(object))
                .saturating_add(object.name.len())
                .saturating_add(object.sha256.len())
        },
    )
}

pub async fn load(key: GenerationKey, directory: &Path) -> Result<Arc<GenerationManifest>, String> {
    {
        let mut cache = cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(position) = cache.entries.iter().position(|entry| entry.key == key) {
            let entry = cache.entries.remove(position).expect("cache position");
            let manifest = Arc::clone(&entry.manifest);
            cache.entries.push_back(entry);
            return Ok(manifest);
        }
    }

    let manifest = plurx_core::transcode::manifest::load(directory).await?;
    if manifest.manifest_digest != key.manifest_digest {
        return Err("generation manifest disagrees with its fenced digest".to_owned());
    }
    let manifest = Arc::new(manifest);
    let decoded_bytes = decoded_weight(&manifest);
    if decoded_bytes > MAX_DECODED_BYTES {
        return Ok(manifest);
    }

    let mut cache = cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(existing) = cache.entries.iter().find(|entry| entry.key == key) {
        return Ok(Arc::clone(&existing.manifest));
    }
    cache.decoded_bytes = cache.decoded_bytes.saturating_add(decoded_bytes);
    cache.entries.push_back(Entry {
        key,
        manifest: Arc::clone(&manifest),
        decoded_bytes,
    });
    while cache.entries.len() > MAX_ENTRIES || cache.decoded_bytes > MAX_DECODED_BYTES {
        let Some(evicted) = cache.entries.pop_front() else {
            break;
        };
        cache.decoded_bytes = cache.decoded_bytes.saturating_sub(evicted.decoded_bytes);
    }
    Ok(manifest)
}
