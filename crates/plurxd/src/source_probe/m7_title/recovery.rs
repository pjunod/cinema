//! Seal a scored experiment before publishing its nonterminal title receipt.

use super::{load_json, string, sync_registry, EvaluationError, EvaluationResult};
use crate::source_probe::{
    m7_candidate::reclaim_candidate_media,
    m7_comparison::{self, HeldTitle, Measurement, TitleContext},
};
use plurx_core::{
    error::StoreError,
    transcode::{
        dv_m7_receipt::{M7Binding, M7Receipt, M7SourceProbe, MODEL_VERSION},
        dv_m7_scoring::{
            aggregation::{Disposition, AGGREGATION_REVISION},
            SCORING_REVISION,
        },
        dv_refit::FIT_VERSION,
    },
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    fs::{self, File},
    future::Future,
    io::{self, Write},
    path::{Path, PathBuf},
};
use tokio_util::sync::CancellationToken;

const COMPLETION: &str = "completion";
const TERMINAL: &str = "terminal.json";
const REPORT: &str = "comparison-result.json";
const BINDING: &str = "measurement-binding.json";
const PENDING: &str = "pending-receipt.json";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Terminal {
    source_sha256: String,
    protocol_sha256: String,
    binding_key: String,
    binding_sha256: String,
    report_sha256: String,
    disposition: Disposition,
    /// Older sealed experiments have their report and binding, without a
    /// compact receipt eligible for reconciliation.
    #[serde(default)]
    pending_receipt_sha256: Option<String>,
}

pub(super) struct NativeCompletion {
    experiment: PathBuf,
    terminal: Terminal,
    binding: M7Binding,
    full_report_sha256: [u8; 32],
    pending_receipt: Option<String>,
}

impl NativeCompletion {
    fn open(experiment: &Path, artifacts: &Path) -> Result<Self, String> {
        let terminal: Terminal =
            serde_json::from_slice(&load_json(&artifacts.join(TERMINAL), None)?).map_err(string)?;
        if experiment.file_name().and_then(|name| name.to_str())
            != Some(terminal.protocol_sha256.as_str())
            || experiment
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                != Some(terminal.source_sha256.as_str())
            || !super::digest_text(&terminal.protocol_sha256)
            || !super::digest_text(&terminal.source_sha256)
            || !super::digest_text(&terminal.report_sha256)
            || !super::digest_text(&terminal.binding_sha256)
            || !super::digest_text(&terminal.binding_key)
        {
            return Err("invalid native M7 terminal manifest".into());
        }
        load_json(
            &experiment.join("native-protocol.json"),
            Some(&terminal.protocol_sha256),
        )?;
        let report = load_json(&artifacts.join(REPORT), Some(&terminal.report_sha256))?;
        let full_report_sha256 = Sha256::digest(report).into();
        let binding: M7Binding = serde_json::from_slice(&load_json(
            &artifacts.join(BINDING),
            Some(&terminal.binding_sha256),
        )?)
        .map_err(string)?;
        if hex::encode(binding.source_sha256) != terminal.source_sha256
            || hex::encode(binding.protocol_sha256) != terminal.protocol_sha256
            || !binding.margin.is_finite()
        {
            return Err("native M7 terminal manifest differs from its measurement binding".into());
        }
        let pending_receipt = terminal
            .pending_receipt_sha256
            .as_ref()
            .map(|digest| {
                if !super::digest_text(digest) {
                    return Err("invalid native M7 pending receipt digest".into());
                }
                let bytes = load_json(&artifacts.join(PENDING), Some(digest))?;
                String::from_utf8(bytes).map_err(string)
            })
            .transpose()?;
        Ok(Self {
            experiment: experiment.to_owned(),
            terminal,
            binding,
            full_report_sha256,
            pending_receipt,
        })
    }

    fn pending(&self, identity: &LiveBinding<'_>) -> Result<Option<M7Receipt>, String> {
        let Some(encoded) = &self.pending_receipt else {
            return Ok(None);
        };
        if !identity.matches(&self.binding) {
            return Ok(None);
        }
        let receipt = M7Receipt::decode_recovery(encoded, &self.binding, self.full_report_sha256)
            .map_err(string)?;
        if receipt.binding_key() != self.terminal.binding_key
            || receipt.disposition() != self.terminal.disposition
        {
            return Err("native M7 pending receipt differs from its sealed report".into());
        }
        Ok(Some(receipt))
    }

    pub(super) async fn reconcile(
        self,
        context: &TitleContext<'_>,
        title: &HeldTitle,
    ) -> EvaluationResult {
        tracing::info!(file_id = context.file.id, disposition = ?self.terminal.disposition,
            protocol_sha256 = %self.terminal.protocol_sha256,
            report_sha256 = %self.terminal.report_sha256,
            binding_key = %self.terminal.binding_key,
            "retained native M7 experiment");
        self.reconcile_pending(context, title).await?;
        if self.pending_receipt.is_some() {
            reclaim_candidate_media(
                &self.experiment,
                self.binding.source_sha256,
                self.binding.protocol_sha256,
            )
            .await
            .map_err(string)?;
        }
        Ok(())
    }

    async fn reconcile_pending(
        &self,
        context: &TitleContext<'_>,
        title: &HeldTitle,
    ) -> EvaluationResult {
        let scanner = context.tools.title_scanner_digest();
        let renderer = context.tools.backend_digest();
        let identity = LiveBinding {
            file_id: context.file.id,
            source_size: context.file.size,
            source_mtime: context.file.mtime,
            probe_revision: context.demand.probe_revision,
            source_sha256: title.content_sha256,
            held_source_version: title.source.object_version(),
            probe_sha256: title.probe_sha256,
            scanner_digest: &scanner,
            renderer_digest: &renderer,
            encoder_digest: context.tools.muxer().map(|(_, digest)| digest),
        };
        let Some(receipt) = self.pending(&identity)? else {
            return Ok(());
        };

        m7_comparison::verify_authority(context, title, context.cancel)
            .await
            .map_err(string)?;
        if !context.tools.muxer_is_current().await {
            return Err(EvaluationError::Deferred(
                "M7 encoder changed during recovery".into(),
            ));
        }
        if m7_comparison::scorer_identity(context.cancel)
            .await
            .map_err(string)?
            != self.binding.scorer_sha256
        {
            return Ok(());
        }
        let table =
            crate::dv_refit_serving::current_table(context.state.store.as_ref(), context.file)
                .await?;
        if table
            .is_none_or(|current| current.digest() != hex::encode(self.binding.fit_table_sha256))
        {
            return Ok(());
        }
        let probe = M7SourceProbe::from_probe_json(&context.demand.probe_json).map_err(string)?;
        if let Some(stored) = context
            .state
            .store
            .dv_m7_receipt(&self.binding, &probe)
            .await
            .map_err(string)?
        {
            if stored.report_sha256() != receipt.report_sha256() {
                return Err(EvaluationError::Deferred(
                    "catalogue receipt disagrees with the sealed M7 report".into(),
                ));
            }
            return Ok(());
        }
        commit_pending(
            context.cancel,
            context.shutdown,
            context.fence.record_dv_m7_receipt(probe, receipt),
        )
        .await
    }
}

#[derive(Clone, Copy)]
struct LiveBinding<'a> {
    file_id: i64,
    source_size: i64,
    source_mtime: i64,
    probe_revision: i64,
    source_sha256: [u8; 32],
    held_source_version: &'a str,
    probe_sha256: [u8; 32],
    scanner_digest: &'a str,
    renderer_digest: &'a str,
    encoder_digest: Option<&'a str>,
}

impl LiveBinding<'_> {
    fn matches(&self, binding: &M7Binding) -> bool {
        let mut encoder = [0; 32];
        self.encoder_digest
            .is_some_and(|digest| hex::decode_to_slice(digest, &mut encoder).is_ok())
            && self.file_id == binding.file_id
            && self.source_size == binding.source_size
            && self.source_mtime == binding.source_mtime
            && self.probe_revision == binding.probe_revision
            && self.source_sha256 == binding.source_sha256
            && self.held_source_version == binding.held_source_version
            && self.probe_sha256 == binding.probe_sha256
            && self.scanner_digest == binding.scanner_digest
            && self.renderer_digest == binding.renderer_digest
            && binding.model_version == MODEL_VERSION
            && binding.scoring_revision == SCORING_REVISION
            && binding.aggregation_revision == AGGREGATION_REVISION
            && binding.fit_method == FIT_VERSION
            && binding
                .windows
                .iter()
                .all(|window| window.encoder_sha256 == encoder)
    }
}

pub(super) async fn find(
    root: &Path,
    source: [u8; 32],
) -> Result<Option<NativeCompletion>, String> {
    let source_dir = root.join("experiments").join(hex::encode(source));
    tokio::task::spawn_blocking(move || find_in(&source_dir))
        .await
        .map_err(string)?
}

fn find_in(source_dir: &Path) -> Result<Option<NativeCompletion>, String> {
    let entries = match fs::read_dir(source_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(string(error)),
    };
    for entry in entries {
        let entry = entry.map_err(string)?;
        if !entry.file_type().map_err(string)?.is_dir() {
            continue;
        }
        let experiment = entry.path();
        let completion = experiment.join(COMPLETION);
        let artifacts = if completion.is_dir() {
            &completion
        } else if experiment.join(TERMINAL).is_file() {
            &experiment
        } else {
            continue;
        };
        return NativeCompletion::open(&experiment, artifacts).map(Some);
    }
    Ok(None)
}

/// The report, its exact binding and compact pending receipt appear together
/// in an atomic directory rename. The report survives a lost title fence.
pub(super) async fn seal(
    experiment: PathBuf,
    measurement: Measurement,
) -> Result<NativeCompletion, String> {
    tokio::task::spawn_blocking(move || seal_owned(&experiment, measurement))
        .await
        .map_err(string)?
}

fn seal_owned(experiment: &Path, measurement: Measurement) -> Result<NativeCompletion, String> {
    let staging = tempfile::Builder::new()
        .prefix(".completion-")
        .tempdir_in(experiment)
        .map_err(string)?;
    let mut report = File::create_new(staging.path().join(REPORT)).map_err(string)?;
    let receipt = M7Receipt::from_report(
        measurement.custody,
        measurement.measured_at_ms,
        &measurement.report,
        &mut report,
    )
    .map_err(string)?;
    report.sync_all().map_err(string)?;
    let binding = serde_json::to_vec(receipt.binding()).map_err(string)?;
    let pending = receipt.encode_recovery().map_err(string)?;
    let terminal = Terminal {
        source_sha256: hex::encode(receipt.binding().source_sha256),
        protocol_sha256: hex::encode(receipt.binding().protocol_sha256),
        binding_key: receipt.binding_key().to_owned(),
        binding_sha256: hex::encode(Sha256::digest(&binding)),
        report_sha256: hex::encode(receipt.report_sha256()),
        disposition: receipt.disposition(),
        pending_receipt_sha256: Some(hex::encode(Sha256::digest(pending.as_bytes()))),
    };
    write_synced(&staging.path().join(BINDING), &binding)?;
    write_synced(&staging.path().join(PENDING), pending.as_bytes())?;
    write_synced(
        &staging.path().join(TERMINAL),
        &serde_json::to_vec(&terminal).map_err(string)?,
    )?;
    plurx_core::fs_secure::open_directory_nofollow_blocking(staging.path())
        .and_then(|dir| dir.sync_all())
        .map_err(string)?;
    let committed = experiment.join(COMPLETION);
    if let Err(error) = fs::rename(staging.path(), &committed) {
        if !committed.is_dir() {
            return Err(string(error));
        }
    }
    sync_registry(experiment)?;
    let result = NativeCompletion::open(experiment, &committed)?;
    if result.binding != *receipt.binding() || result.full_report_sha256 != receipt.report_sha256()
    {
        return Err("sealed native M7 experiment conflicts with the completed comparison".into());
    }
    Ok(result)
}

fn write_synced(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut output = File::create_new(path).map_err(string)?;
    output.write_all(bytes).map_err(string)?;
    output.sync_all().map_err(string)
}

/// The write future is only polled while the current attempt is active.
/// SQL independently checks the live claim at the point of mutation.
async fn commit_pending(
    cancel: &CancellationToken,
    shutdown: &CancellationToken,
    write: impl Future<Output = Result<bool, StoreError>>,
) -> EvaluationResult {
    if cancel.is_cancelled() || shutdown.is_cancelled() {
        return Err(EvaluationError::Deferred(
            "M7 receipt publication interrupted".into(),
        ));
    }
    if !write.await.map_err(string)? {
        return Err(EvaluationError::Deferred(
            "M7 receipt publication lost its title authority".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "sealed scratch fixtures and expected recovery results"
)]
mod tests {
    use super::*;
    use plurx_core::transcode::{
        dv_m7_receipt::{CalibrationWindow, WindowBinding},
        dv_m7_scoring::{
            aggregation::{Aggregator, FrozenMargin, WindowShape},
            ModelSampleScore,
        },
        Rational,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn fixture(root: &Path) -> (PathBuf, Measurement) {
        let source_sha256 = [1; 32];
        let protocol = br#"{"schema":"test-only-sealed-protocol"}"#;
        let protocol_sha256 = Sha256::digest(protocol).into();
        let experiment = root
            .join("experiments")
            .join(hex::encode(source_sha256))
            .join(hex::encode(protocol_sha256));
        fs::create_dir_all(&experiment).unwrap();
        write_synced(&experiment.join("native-protocol.json"), protocol).unwrap();
        let time_base = Rational::new(1, 1_000).unwrap();
        let binding = M7Binding {
            file_id: 1,
            source_size: 2,
            source_mtime: 3,
            probe_revision: 4,
            source_sha256,
            held_source_version: "held-generation".into(),
            probe_sha256: [2; 32],
            scanner_digest: hex::encode([3; 32]),
            renderer_digest: hex::encode([4; 32]),
            scorer_sha256: [5; 32],
            model_version: MODEL_VERSION.into(),
            scoring_revision: SCORING_REVISION,
            aggregation_revision: AGGREGATION_REVISION,
            protocol_sha256,
            recipe_sha256: [6; 32],
            fit_method: FIT_VERSION.into(),
            fit_table_sha256: [7; 32],
            calibration: vec![CalibrationWindow {
                first_ordinal: 0,
                last_ordinal: 0,
                first_pts: 0,
                end_pts: 1,
                time_base,
                receipt_sha256: [8; 32],
            }],
            margin: 0.05,
            windows: vec![WindowBinding {
                first_ordinal: 1,
                last_ordinal: 1,
                first_pts: 1,
                end_pts: 2,
                time_base,
                frames: 1,
                sites_per_frame: 1,
                source_census_sha256: [9; 32],
                candidate_census_sha256: [10; 32],
                authored_crop_sha256: [11; 32],
                encoder_sha256: [12; 32],
                encoded_sha256: [13; 32],
                authored_sha256: [14; 32],
                authoring_receipt_sha256: [15; 32],
                source_vcl_sha256: [16; 32],
                whole_publication_sha256: [17; 32],
                whole_readback_sha256: [18; 32],
                whole_rpu_sha256: [19; 32],
                seek_publication_sha256: [20; 32],
                seek_readback_sha256: [21; 32],
                seek_rpu_sha256: [22; 32],
            }],
        };
        let mut aggregator = Aggregator::new(
            &experiment,
            &[WindowShape {
                frames: 1,
                sites_per_frame: 1,
            }],
            FrozenMargin::new(0.05).unwrap(),
            1024,
        )
        .unwrap();
        let model = ModelSampleScore {
            reference_luminance_nits: 0.8,
            served_fplus_delta_e_itp: 2.0,
            candidate_delta_e_itp: 1.0,
        };
        aggregator.push_frame(&[[model; 4]], || false).unwrap();
        let measurement = Measurement {
            custody: binding,
            report: aggregator.finish(|| false).unwrap(),
            measured_at_ms: 5,
        };
        (experiment, measurement)
    }

    fn live<'a>(binding: &'a M7Binding, encoder: &'a str) -> LiveBinding<'a> {
        LiveBinding {
            file_id: binding.file_id,
            source_size: binding.source_size,
            source_mtime: binding.source_mtime,
            probe_revision: binding.probe_revision,
            source_sha256: binding.source_sha256,
            held_source_version: &binding.held_source_version,
            probe_sha256: binding.probe_sha256,
            scanner_digest: &binding.scanner_digest,
            renderer_digest: &binding.renderer_digest,
            encoder_digest: Some(encoder),
        }
    }

    #[tokio::test]
    async fn completed_m7_recovers_after_cancel_fence_loss_and_store_error() {
        let root = crate::test_tempdir().unwrap();
        let (experiment, measurement) = fixture(root.path());
        let source = measurement.custody.source_sha256;
        let encoder = hex::encode(measurement.custody.windows[0].encoder_sha256);
        let binding = measurement.custody.clone();
        let fresh = live(&binding, &encoder);
        assert!(find(root.path(), source).await.unwrap().is_none());

        // The comparison is finished once. From this point forwarding attempts
        // use only the recovered digest-bound receipt.
        let first = seal(experiment.clone(), measurement).await.unwrap();
        let report = first.full_report_sha256;
        let key = first.terminal.binding_key;
        let write_calls = AtomicUsize::new(0);

        for fault in 0..3 {
            let resumed = find(root.path(), source)
                .await
                .unwrap()
                .expect("sealed comparison");
            let pending = resumed.pending(&fresh).unwrap().expect("exact receipt");
            assert_eq!(pending.binding_key(), key);
            assert_eq!(pending.report_sha256(), report);
            let cancel = CancellationToken::new();
            if fault == 0 {
                cancel.cancel();
            }
            let deferred = commit_pending(&cancel, &CancellationToken::new(), async {
                write_calls.fetch_add(1, Ordering::Relaxed);
                if fault == 1 {
                    Ok(false)
                } else {
                    Err(StoreError::Task("injected replicated write failure".into()))
                }
            })
            .await;
            assert!(matches!(deferred, Err(EvaluationError::Deferred(_))));
        }
        assert_eq!(write_calls.load(Ordering::Relaxed), 2);

        let resumed = find(root.path(), source).await.unwrap().unwrap();
        let exact = resumed.pending(&fresh).unwrap().unwrap();
        commit_pending(
            &CancellationToken::new(),
            &CancellationToken::new(),
            async {
                assert_eq!(exact.binding_key(), key);
                assert_eq!(exact.report_sha256(), report);
                write_calls.fetch_add(1, Ordering::Relaxed);
                Ok(true)
            },
        )
        .await
        .unwrap();
        assert_eq!(write_calls.load(Ordering::Relaxed), 3);
        assert_eq!(
            hex::encode(Sha256::digest(
                fs::read(experiment.join(COMPLETION).join(REPORT)).unwrap()
            )),
            hex::encode(report),
        );
    }

    #[tokio::test]
    async fn sealed_m7_keeps_stale_bindings_historical() {
        let root = crate::test_tempdir().unwrap();
        let (experiment, measurement) = fixture(root.path());
        let source = measurement.custody.source_sha256;
        let encoder = hex::encode(measurement.custody.windows[0].encoder_sha256);
        let binding = measurement.custody.clone();
        let exact = live(&binding, &encoder);
        let completed = seal(experiment.clone(), measurement).await.unwrap();
        assert!(completed.pending(&exact).unwrap().is_some());
        let wrong = hex::encode([23; 32]);
        for stale in [
            LiveBinding {
                source_sha256: [24; 32],
                ..exact
            },
            LiveBinding {
                held_source_version: "another-generation",
                ..exact
            },
            LiveBinding {
                probe_sha256: [25; 32],
                ..exact
            },
            LiveBinding {
                scanner_digest: &wrong,
                ..exact
            },
            LiveBinding {
                renderer_digest: &wrong,
                ..exact
            },
            LiveBinding {
                encoder_digest: Some(&wrong),
                ..exact
            },
        ] {
            assert!(completed.pending(&stale).unwrap().is_none());
        }
        assert!(find(root.path(), source).await.unwrap().is_some());
        fs::write(experiment.join(COMPLETION).join(REPORT), b"corrupt report").unwrap();
        assert!(find(root.path(), source).await.is_err());
    }
}
