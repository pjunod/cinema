//! Bounded CPU text inference; held resources outlive a cancelled async waiter.
use super::{
    artifacts::{self, VerifiedBundle},
    text::TextCue,
    tokenizer, weights,
};
use candle_core::{Device, Tensor};
use candle_transformers::models::marian;
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;
const THREADS: usize = 2;
const MAX_BATCH: usize = 8;
const INPUT_TOKENS: usize = 256;
const OUTPUT_TOKENS: usize = 512;
fn checked<T, E: std::fmt::Display>(result: Result<T, E>, code: &'static str) -> Result<T, String> {
    result.map_err(|error| {
        tracing::warn!(reason=code, error=%error, "Text translation engine failed");
        code.to_owned()
    })
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
pub(crate) async fn translate_with_progress(
    bundle: Arc<VerifiedBundle>,
    cues: Vec<TextCue>,
    cancel: CancellationToken,
    keepalive: Arc<dyn Send + Sync>,
    progress: Arc<AtomicUsize>,
) -> Result<Vec<TextCue>, String> {
    if cues.is_empty() || cues.len() > MAX_BATCH {
        return Err("text_batch_invalid".into());
    }
    tokio::task::spawn_blocking(move || {
        // A cancelled JoinHandle never drops the actual inference reservation early.
        let _keepalive = keepalive;
        let pool = checked(
            rayon::ThreadPoolBuilder::new()
                .num_threads(THREADS)
                .thread_name(|n| format!("subtitle-text-{n}"))
                .build(),
            "text_engine_unavailable",
        )?;
        pool.install(|| run(&bundle, cues, &cancel, &progress))
    })
    .await
    .map_err(|_| "text_engine_failed".to_owned())?
}
fn run(
    bundle: &VerifiedBundle,
    cues: Vec<TextCue>,
    cancel: &CancellationToken,
    progress: &AtomicUsize,
) -> Result<Vec<TextCue>, String> {
    let deadline = Instant::now() + Duration::from_secs(120);
    active(cancel, deadline)?;
    let config: marian::Config = checked(
        serde_json::from_slice(&artifacts::read_metadata(
            bundle.file("config")?,
            16 * 1024,
            cancel,
            deadline,
        )?),
        "text_model_invalid",
    )?;
    if config.max_position_embeddings < OUTPUT_TOKENS
        || config.vocab_size != 64_172
        || config.d_model != 512
    {
        return Err("text_model_invalid".into());
    }
    let source_spm = artifacts::read_metadata(
        bundle.file("source_spm")?,
        2 * 1024 * 1024,
        cancel,
        deadline,
    )?;
    let target_spm = artifacts::read_metadata(
        bundle.file("target_spm")?,
        2 * 1024 * 1024,
        cancel,
        deadline,
    )?;
    let vocab = artifacts::read_metadata(bundle.file("vocab")?, 8 * 1024 * 1024, cancel, deadline)?;
    let source = checked(
        tokenizer::tokenizer(&source_spm, &vocab),
        "text_tokenizer_invalid",
    )?;
    let target = checked(
        tokenizer::tokenizer(&target_spm, &vocab),
        "text_tokenizer_invalid",
    )?;
    let vb = weights::load(bundle.file("weights")?, cancel, deadline)?;
    let mut model = checked(marian::MTModel::new(&config, vb), "text_model_invalid")?;
    let mut output = Vec::with_capacity(cues.len());
    for mut cue in cues {
        active(cancel, deadline)?;
        if cue.text.trim().is_empty() || cue.text.len() > 16 * 1024 {
            return Err("text_input_invalid".into());
        }
        let encoded = checked(
            source.encode(cue.text.as_str(), false),
            "text_tokenizer_invalid",
        )?;
        if encoded.is_empty() {
            return Err("text_input_invalid".into());
        }
        let mut pieces = Vec::new();
        for chunk in encoded.get_ids().chunks(INPUT_TOKENS) {
            active(cancel, deadline)?;
            model.reset_kv_cache();
            let mut ids = chunk.to_vec();
            ids.push(config.eos_token_id);
            let input = checked(
                Tensor::new(ids.as_slice(), &Device::Cpu).and_then(|v| v.unsqueeze(0)),
                "text_inference_failed",
            )?;
            let encoder = checked(model.encoder().forward(&input, 0), "text_inference_failed")?;
            let mut last = config.decoder_start_token_id;
            let mut translated = Vec::new();
            let mut ended = false;
            for position in 0..OUTPUT_TOKENS {
                active(cancel, deadline)?;
                let token = checked(
                    Tensor::new(&[last], &Device::Cpu).and_then(|v| v.unsqueeze(0)),
                    "text_inference_failed",
                )?;
                let logits = checked(
                    model
                        .decode(&token, &encoder, position)
                        .and_then(|v| v.squeeze(0))
                        .and_then(|v| v.squeeze(0))
                        .and_then(|v| v.to_vec1::<f32>()),
                    "text_inference_failed",
                )?;
                let next = logits
                    .iter()
                    .enumerate()
                    .filter(|(id, score)| *id != config.pad_token_id as usize && score.is_finite())
                    .max_by(|a, b| a.1.total_cmp(b.1))
                    .map(|(id, _)| id as u32)
                    .ok_or("text_output_invalid")?;
                if next == config.eos_token_id || next == config.forced_eos_token_id {
                    ended = true;
                    break;
                }
                translated.push(next);
                last = next;
            }
            if !ended {
                return Err("text_output_limit".into());
            }
            let text = checked(target.decode(&translated, true), "text_output_invalid")?;
            if text.trim().is_empty() {
                return Err("text_output_invalid".into());
            }
            pieces.push(text.trim().to_owned());
        }
        cue.text = pieces.join(" ");
        if cue.text.len() > 16 * 1024 {
            return Err("text_output_limit".into());
        }
        output.push(cue);
        progress.fetch_add(1, Ordering::Release);
    }
    active(cancel, deadline)?;
    Ok(output)
}

/// Verify the production path against three short, independently recognizable phrases.
pub(crate) async fn self_test(
    bundle: Arc<VerifiedBundle>,
    cancel: CancellationToken,
    keepalive: Arc<dyn Send + Sync>,
) -> Result<(), String> {
    let cues = vec![
        TextCue {
            id: "fr".into(),
            start_ms: 0,
            end_ms: 1000,
            text: "Bonjour le monde.".into(),
        },
        TextCue {
            id: "de".into(),
            start_ms: 1000,
            end_ms: 2000,
            text: "Guten Morgen.".into(),
        },
        TextCue {
            id: "es".into(),
            start_ms: 2000,
            end_ms: 3000,
            text: "Hola mundo.".into(),
        },
    ];
    let output = translate_with_progress(
        bundle,
        cues,
        cancel,
        keepalive,
        Arc::new(AtomicUsize::new(0)),
    )
    .await?;
    if output.len() != 3
        || !output[0].text.to_ascii_lowercase().contains("world")
        || !output[1].text.to_ascii_lowercase().contains("morning")
        || !output[2].text.to_ascii_lowercase().contains("world")
    {
        return Err("model_self_test_failed".into());
    }
    Ok(())
}

#[cfg(test)]
mod qualification {
    use super::*;
    /// Explicit runtime qualification: the caller owns a persistent model cache.
    /// Ordinary unit runs must never download hundreds of megabytes implicitly.
    #[tokio::test]
    #[ignore = "downloads the pinned publisher bundle; requires PLURX_TEXT_QUALIFICATION_ROOT"]
    async fn pinned_publisher_bundle_passes_production_multilingual_self_test() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .with_test_writer()
            .try_init();
        let root = std::env::var_os("PLURX_TEXT_QUALIFICATION_ROOT")
            .map(std::path::PathBuf::from)
            .expect("supply an owned persistent model cache outside temporary directories");
        assert!(root.is_absolute(), "qualification cache must be absolute");
        let cancel = CancellationToken::new();
        let bundle = artifacts::install(&root, &cancel)
            .await
            .expect("pinned model download and verification");
        assert_eq!(bundle.bundle_digest, artifacts::expected_bundle_digest());
        let identity = bundle.identity();
        self_test(Arc::new(bundle), cancel.clone(), Arc::new(()))
            .await
            .expect("production multilingual inference");
        let reopened = artifacts::inspect(&root, &cancel)
            .await
            .expect("restart-safe verified cache reopen");
        assert_eq!(reopened.identity(), identity);
    }
}
