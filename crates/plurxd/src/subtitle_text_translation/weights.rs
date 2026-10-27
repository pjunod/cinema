//! One pinned legacy PyTorch checkpoint, without a production pickle parser.
//!
//! The compiled layout comes from the developer-only authenticated metadata
//! extractor. Its checksum, model SHA/size and entire inventory are fixed here.
//! Only verified held descriptors supply bytes. Distinct storages are loaded
//! once; the publisher's tied embeddings retain the same Candle allocation.
use crate::subtitle_models::artifacts::ManagedModel;
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use plurx_core::fs_secure::std_file_identity;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashMap},
    fs::File,
    sync::LazyLock,
    time::Instant,
};
use tokio_util::sync::CancellationToken;

const MODEL_SHA256: &str = "33ff438ec37160a105f0700819a5b78a07918e1913fc2f249184b1f46a248e4e";
const MODEL_BYTES: u64 = 310_385_901;
const LAYOUT_SHA256: &str = "22db3d52648e6a4d76e713d348bec7249526a561717f25f0368239565786f4aa";
const LAYOUT_BYTES: &[u8] = include_bytes!("weights-layout.json");
const MAX_STORAGE_ELEMENTS: u64 = 32_856_064;
const CHUNK: usize = 128 * 1024;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Layout {
    version: u8,
    model_sha256: String,
    model_bytes: u64,
    format: String,
    metadata_end: u64,
    storages: Vec<Storage>,
    tensors: Vec<NamedTensor>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Storage {
    offset: u64,
    elements: u64,
    dtype: String,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct NamedTensor {
    name: String,
    storage: usize,
    offset: u64,
    shape: Vec<usize>,
    strides: Vec<usize>,
}
static LAYOUT: LazyLock<Result<Layout, String>> = LazyLock::new(|| pinned_layout(LAYOUT_BYTES));
fn invalid() -> String {
    "text_model_invalid".into()
}
fn active(cancel: &CancellationToken, deadline: Instant) -> Result<(), String> {
    if cancel.is_cancelled() {
        Err("worker_interrupted".into())
    } else if Instant::now() >= deadline {
        Err("text_inference_timeout".into())
    } else {
        Ok(())
    }
}
fn pinned_layout(bytes: &[u8]) -> Result<Layout, String> {
    if bytes.len() != 74_326 || hex::encode(Sha256::digest(bytes)) != LAYOUT_SHA256 {
        return Err(invalid());
    }
    let layout: Layout = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    if layout.version != 1
        || layout.format != "pytorch-legacy-1001-f32le"
        || layout.model_sha256 != MODEL_SHA256
        || layout.model_bytes != MODEL_BYTES
        || layout.storages.len() != 256
        || layout.tensors.len() != 258
    {
        return Err(invalid());
    }
    validate_ranges(&layout)?;
    Ok(layout)
}
/// Validate the whole descriptor inventory before reserving any tensor bytes.
fn validate_ranges(layout: &Layout) -> Result<(), String> {
    if layout.model_bytes > MODEL_BYTES
        || layout.metadata_end == 0
        || layout.metadata_end > 1024 * 1024
        || layout.metadata_end >= layout.model_bytes
        || layout.storages.is_empty()
        || layout.storages.len() > 512
        || layout.tensors.is_empty()
        || layout.tensors.len() > 1024
    {
        return Err(invalid());
    }
    let mut end = layout.metadata_end;
    let mut allocated = 0_u64;
    for storage in &layout.storages {
        let bytes = storage.elements.checked_mul(4).ok_or_else(invalid)?;
        allocated = allocated.checked_add(bytes).ok_or_else(invalid)?;
        if storage.dtype != "f32le"
            || storage.elements == 0
            || storage.elements > MAX_STORAGE_ELEMENTS
            || allocated > MODEL_BYTES
            || storage.offset != end.checked_add(8).ok_or_else(invalid)?
        {
            return Err(invalid());
        }
        end = storage.offset.checked_add(bytes).ok_or_else(invalid)?;
        if end > layout.model_bytes {
            return Err(invalid());
        }
        usize::try_from(storage.elements).map_err(|_| invalid())?;
    }
    if end != layout.model_bytes {
        return Err(invalid());
    }
    let mut names = BTreeSet::new();
    let mut aliases = HashMap::new();
    for tensor in &layout.tensors {
        if tensor.name.is_empty()
            || tensor.name.len() > 160
            || !tensor
                .name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.')
            || !names.insert(&tensor.name)
            || tensor.shape.is_empty()
            || tensor.shape.len() > 4
            || tensor.shape.len() != tensor.strides.len()
        {
            return Err(invalid());
        }
        let storage = layout.storages.get(tensor.storage).ok_or_else(invalid)?;
        let mut elements = 1_usize;
        for (&size, &stride) in tensor.shape.iter().zip(&tensor.strides).rev() {
            if size == 0 || stride != elements {
                return Err(invalid());
            }
            elements = elements.checked_mul(size).ok_or_else(invalid)?;
        }
        let elements = u64::try_from(elements).map_err(|_| invalid())?;
        // This authenticated publisher file contains only complete contiguous
        // storage views. Partial/overlapping or differently reshaped aliases
        // require a separately reviewed adapter rather than general unpickling.
        if tensor.offset != 0
            || elements != storage.elements
            || tensor.offset.checked_add(elements).ok_or_else(invalid)? > storage.elements
        {
            return Err(invalid());
        }
        if aliases
            .insert(tensor.storage, &tensor.shape)
            .is_some_and(|prior| prior != &tensor.shape)
        {
            return Err(invalid());
        }
    }
    if aliases.len() != layout.storages.len() {
        return Err(invalid());
    }
    Ok(())
}
fn held(model: &ManagedModel, sha256: &str, bytes: u64) -> Result<(), String> {
    if model.sha256 != sha256
        || model.identity.size != bytes
        || std_file_identity(&model.handle).ok() != Some(model.identity)
    {
        return Err(invalid());
    }
    Ok(())
}
#[cfg(unix)]
fn read_at(file: &File, bytes: &mut [u8], offset: u64) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.read_exact_at(bytes, offset)
}
#[cfg(windows)]
fn read_at(file: &File, mut bytes: &mut [u8], mut offset: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !bytes.is_empty() {
        let count = file.seek_read(bytes, offset)?;
        if count == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        offset = offset
            .checked_add(count as u64)
            .ok_or(std::io::ErrorKind::InvalidData)?;
        bytes = &mut bytes[count..];
    }
    Ok(())
}
fn tensors(
    file: &File,
    layout: &Layout,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<HashMap<String, Tensor>, String> {
    validate_ranges(layout)?;
    active(cancel, deadline)?;
    let mut storages = Vec::with_capacity(layout.storages.len());
    for storage in &layout.storages {
        active(cancel, deadline)?;
        let mut count = [0_u8; 8];
        read_at(
            file,
            &mut count,
            storage.offset.checked_sub(8).ok_or_else(invalid)?,
        )
        .map_err(|_| invalid())?;
        if u64::from_le_bytes(count) != storage.elements {
            return Err(invalid());
        }
        let elements = usize::try_from(storage.elements).map_err(|_| invalid())?;
        let mut values = Vec::new();
        values.try_reserve_exact(elements).map_err(|_| invalid())?;
        let mut buffer = [0_u8; CHUNK];
        let total = storage.elements.checked_mul(4).ok_or_else(invalid)?;
        let mut position = 0;
        while position < total {
            active(cancel, deadline)?;
            let length =
                usize::try_from((total - position).min(CHUNK as u64)).map_err(|_| invalid())?;
            read_at(
                file,
                &mut buffer[..length],
                storage.offset.checked_add(position).ok_or_else(invalid)?,
            )
            .map_err(|_| invalid())?;
            for bytes in buffer[..length].chunks_exact(4) {
                let value = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                if !value.is_finite() {
                    return Err(invalid());
                }
                values.push(value);
            }
            position = position.checked_add(length as u64).ok_or_else(invalid)?;
        }
        active(cancel, deadline)?;
        storages.push(Tensor::from_vec(values, elements, &Device::Cpu).map_err(|_| invalid())?);
    }
    let mut aliases: HashMap<(usize, Vec<usize>), Tensor> = HashMap::new();
    let mut named = HashMap::with_capacity(layout.tensors.len());
    for tensor in &layout.tensors {
        active(cancel, deadline)?;
        let key = (tensor.storage, tensor.shape.clone());
        let value = match aliases.get(&key) {
            Some(prior) => prior.clone(),
            None => {
                let value = storages[tensor.storage]
                    .reshape(tensor.shape.as_slice())
                    .map_err(|_| invalid())?;
                aliases.insert(key, value.clone());
                value
            }
        };
        named.insert(tensor.name.clone(), value);
    }
    active(cancel, deadline)?;
    Ok(named)
}
/// Called only with the managed downloader's full-hash-verified held model.
pub(crate) fn load(
    model: &ManagedModel,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<VarBuilder<'static>, String> {
    active(cancel, deadline)?;
    let layout = LAYOUT.as_ref().map_err(Clone::clone)?;
    held(model, MODEL_SHA256, MODEL_BYTES)?;
    let named = tensors(&model.handle, layout, cancel, deadline)?;
    active(cancel, deadline)?;
    held(model, MODEL_SHA256, MODEL_BYTES)?;
    Ok(VarBuilder::from_tensors(named, DType::F32, &Device::Cpu))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Seek, SeekFrom, Write},
        time::Duration,
    };
    fn synthetic() -> (File, Layout) {
        let mut file = tempfile::tempfile().expect("synthetic raw storage");
        file.write_all(b"RAW!").expect("non-ZIP prefix");
        file.write_all(&3_u64.to_le_bytes()).expect("storage size");
        for value in [0.5_f32, -1.25, 3.0] {
            file.write_all(&value.to_le_bytes()).expect("F32 bytes");
        }
        let layout = Layout {
            version: 1,
            model_sha256: "synthetic".into(),
            model_bytes: 24,
            format: "synthetic".into(),
            metadata_end: 4,
            storages: vec![Storage {
                offset: 12,
                elements: 3,
                dtype: "f32le".into(),
            }],
            tensors: ["first", "tied"]
                .into_iter()
                .map(|name| NamedTensor {
                    name: name.into(),
                    storage: 0,
                    offset: 0,
                    shape: vec![3],
                    strides: vec![1],
                })
                .collect(),
        };
        (file, layout)
    }
    #[test]
    fn nonzip_raw_f32_values_and_exact_aliases_share_one_candle_tensor() {
        let (file, layout) = synthetic();
        let loaded = tensors(
            &file,
            &layout,
            &CancellationToken::new(),
            Instant::now() + Duration::from_secs(5),
        )
        .expect("raw load");
        assert_eq!(
            loaded["first"].to_vec1::<f32>().expect("values"),
            [0.5, -1.25, 3.0]
        );
        assert_eq!(
            loaded["first"].id(),
            loaded["tied"].id(),
            "aliases retain one allocation"
        );
    }
    #[test]
    fn malformed_overflow_duplicate_overlap_dtype_and_stride_layouts_refuse_before_loading() {
        let (_, good) = synthetic();
        let mut bad = good.clone();
        bad.storages[0].elements = u64::MAX;
        assert!(validate_ranges(&bad).is_err());
        let mut bad = good.clone();
        bad.tensors[0].shape = vec![usize::MAX, 2];
        bad.tensors[0].strides = vec![2, 1];
        assert!(validate_ranges(&bad).is_err());
        let mut bad = good.clone();
        bad.tensors[1].name = "first".into();
        assert!(validate_ranges(&bad).is_err());
        let mut bad = good.clone();
        bad.storages.push(good.storages[0].clone());
        assert!(validate_ranges(&bad).is_err());
        let mut bad = good.clone();
        bad.storages[0].dtype = "f64le".into();
        assert!(validate_ranges(&bad).is_err());
        let mut bad = good.clone();
        bad.tensors[0].strides[0] = 2;
        assert!(validate_ranges(&bad).is_err());
        let mut bad = good.clone();
        bad.tensors[0].offset = 1;
        assert!(validate_ranges(&bad).is_err());
        let mut bad = good;
        bad.tensors[0].shape[0] = 2;
        assert!(validate_ranges(&bad).is_err());
    }
    #[test]
    fn compiled_layout_authentication_binds_names_shapes_and_complete_publisher_inventory() {
        let layout = pinned_layout(LAYOUT_BYTES).expect("compiled authenticated descriptor");
        assert_eq!((layout.storages.len(), layout.tensors.len()), (256, 258));
        let mut changed = LAYOUT_BYTES.to_vec();
        changed[50] ^= 1;
        assert!(pinned_layout(&changed).is_err());
        assert!(pinned_layout(&LAYOUT_BYTES[..LAYOUT_BYTES.len() - 1]).is_err());
    }
    #[test]
    fn cancellation_deadline_and_changed_held_identity_are_closed_failures() {
        let (mut file, layout) = synthetic();
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert_eq!(
            tensors(
                &file,
                &layout,
                &cancel,
                Instant::now() + Duration::from_secs(5)
            )
            .expect_err("cancelled"),
            "worker_interrupted"
        );
        assert_eq!(
            tensors(
                &file,
                &layout,
                &CancellationToken::new(),
                Instant::now() - Duration::from_secs(1)
            )
            .expect_err("deadline"),
            "text_inference_timeout"
        );
        let model = ManagedModel {
            identity: std_file_identity(&file).expect("identity"),
            handle: file.try_clone().expect("held"),
            path: Default::default(),
            sha256: "synthetic".into(),
        };
        held(&model, "synthetic", 24).expect("unchanged synthetic source");
        file.seek(SeekFrom::End(0)).expect("append");
        file.write_all(&[1]).expect("mutate");
        assert!(held(&model, "synthetic", 24).is_err());
        assert_eq!(
            load(&model, &cancel, Instant::now() + Duration::from_secs(5))
                .err()
                .expect("load cancellation"),
            "worker_interrupted"
        );
    }
}
