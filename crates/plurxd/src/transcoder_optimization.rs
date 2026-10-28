//! Node-local capability diagnostics and admitted real-footage comparisons.
mod comparison;
pub use comparison::Comparison;
use std::sync::Arc;

use plurx_core::store::Store;
use plurx_core::transcode::{Encoder, EncoderCaps};
use serde::{Deserialize, Serialize};

use crate::state::{AppState, SystemInfo};

pub const ENCODERS: [Encoder; 5] = [
    Encoder::Software,
    Encoder::Nvenc,
    Encoder::Qsv,
    Encoder::Vaapi,
    Encoder::VideoToolbox,
];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Measurement {
    pub backend: String,
    pub fps: f64,
    pub relative_to_cpu: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Report {
    pub measured_at: i64,
    pub results: Vec<Measurement>,
    #[serde(default)]
    pub comparison: Option<Comparison>,
}

impl Report {
    pub fn fastest(&self, caps: &EncoderCaps) -> Option<Encoder> {
        // Historical testsrc throughput only describes encoder initialization.
        // It never promotes a CPU over validated, working hardware.
        self.comparison
            .as_ref()
            .and_then(|report| {
                (report.status == "complete")
                    .then(|| report.preferred_hardware(caps).unwrap_or(Encoder::Software))
            })
            .or_else(|| Some(caps.choose("")))
    }
}

pub fn effective_override(system: &SystemInfo) -> Option<String> {
    if system.hwaccel_pref.is_empty() || system.hwaccel_pref == "auto" {
        system
            .transcoder_optimization
            .as_ref()
            .filter(|report| {
                report.comparison.as_ref().is_none_or(|comparison| {
                    comparison.valid_for(
                        &system.ffmpeg,
                        &system.encoders,
                        &comparison.policy_snapshot,
                    )
                })
            })
            .and_then(|report| report.fastest(&system.encoders))
            .map(|encoder| encoder.family_name().to_owned())
            .or_else(|| Some(system.encoders.choose("").family_name().to_owned()))
    } else {
        system.hwaccel_override.clone()
    }
}

fn key(node_id: &str) -> String {
    format!("node.{node_id}.transcode.benchmark")
}

pub async fn save(store: &Arc<dyn Store>, node_id: &str, report: &Report) -> anyhow::Result<()> {
    store
        .put_setting(&key(node_id), &serde_json::to_string(report)?)
        .await?;
    Ok(())
}

/// Reuse only a completed comparison of this executable, fixture set and
/// hardware/driver snapshot. Old synthetic rankings remain diagnostic only.
pub async fn load(
    store: &Arc<dyn Store>,
    node_id: &str,
    ffmpeg: &str,
    caps: &EncoderCaps,
) -> anyhow::Result<Option<Report>> {
    let policy = comparison::PolicySnapshot::saved(store).await?;
    Ok(store
        .get_setting(&key(node_id))
        .await?
        .and_then(|raw| serde_json::from_str::<Report>(&raw).ok())
        .filter(|report| {
            report.comparison.as_ref().is_some_and(|comparison| {
                comparison.validate_executable(ffmpeg)
                    && comparison.valid_for(ffmpeg, caps, &policy)
            })
        }))
}

fn benchmark_thread_budgets() -> [usize; 3] {
    use crate::admission::Workload;
    [
        Workload {
            source_height: 1080,
            codec: "h264",
            hdr: None,
            target_height: 720,
        },
        Workload {
            source_height: 2160,
            codec: "hevc",
            hdr: None,
            target_height: 1080,
        },
        Workload {
            source_height: 2160,
            codec: "hevc",
            hdr: Some("hdr10"),
            target_height: 1080,
        },
    ]
    .map(|workload| workload.software_threads())
}

/// Reserve the largest actual production workload's software budget while
/// each case still uses its own matching production thread count.
pub fn benchmark_threads() -> usize {
    benchmark_thread_budgets().into_iter().max().unwrap_or(1)
}

#[derive(Clone, Default, Serialize)]
pub struct Status {
    pub running: bool,
    pub restart_required: bool,
    pub error: Option<String>,
    pub report: Option<Report>,
    /// Current/last attempt, including canceled partial comparisons.
    pub comparison: Option<Comparison>,
}

// One daemon serves one node. The mutex serializes admission, while the report
// is replaced only after a complete successful run. HTTP disconnection does
// not cancel a run and two browser tabs cannot run competing benchmarks.
static STATUS: tokio::sync::Mutex<Status> = tokio::sync::Mutex::const_new(Status {
    running: false,
    restart_required: false,
    error: None,
    report: None,
    comparison: None,
});

pub async fn status(system: &SystemInfo, manager: &crate::transcode::TranscodeManager) -> Status {
    let policy = comparison::PolicySnapshot::current(manager);
    let mut status = STATUS.lock().await.clone();
    if status.report.is_none() {
        status.report = system.transcoder_optimization.clone();
    }
    if status.comparison.is_none() {
        status.comparison = status
            .report
            .as_ref()
            .and_then(|report| report.comparison.clone());
    }
    if let Some(comparison) = status
        .comparison
        .as_mut()
        .filter(|comparison| comparison.status == "complete")
    {
        if !comparison.valid_for(&system.ffmpeg, &system.encoders, &policy) {
            comparison.status = "stale".into();
            comparison.error = Some(
                "FFmpeg, fixtures, hardware/driver identity, or processing policy changed; run comparison again"
                    .into(),
            );
        }
    }
    status.restart_required = (system.hwaccel_pref == "auto" || system.hwaccel_pref.is_empty())
        && status
            .report
            .as_ref()
            .filter(|report| {
                report.comparison.as_ref().is_some_and(|comparison| {
                    comparison.valid_for(&system.ffmpeg, &system.encoders, &policy)
                })
            })
            .and_then(|report| report.fastest(&system.encoders))
            .is_some_and(|encoder| encoder.label() != system.encoder_selected);
    status
}

pub async fn start(state: AppState) -> Result<(), String> {
    let mut status = STATUS.lock().await;
    if status.running {
        return Err("Optimization is already running on this node".into());
    }
    if state.transcode.active_sessions().await > 0 {
        return Err("Stop playback before benchmarking this node".into());
    }
    status.running = true;
    status.error = None;
    status.comparison = None;
    tokio::spawn(async move {
        let run = async {
            let _admission = state
                .transcode
                .admit_transcoder_benchmark()
                .await
                .ok_or_else(|| anyhow::anyhow!("The node is busy; run optimization when idle"))?;
            let caps = plurx_core::transcode::detect_encoders(&state.system.ffmpeg).await;
            // Qualify the same production graph selector for every encoder,
            // then apply production's per-source session routing to it.
            let scratch = tempfile::tempdir_in(&state.runtime_cache_dir)?;
            let mut proven = Vec::new();
            for encoder in ENCODERS
                .into_iter()
                .filter(|encoder| caps.available(*encoder))
            {
                let pipe = if encoder == Encoder::Software {
                    plurx_core::transcode::Pipeline::Cpu
                } else {
                    crate::pipeprobe::probe(scratch.path(), &state.runtime_cache_dir, encoder)
                        .await
                        .selected()
                };
                proven.push((encoder, pipe));
            }
            let comparison = comparison::measure(
                &state.system.ffmpeg,
                &caps,
                &state.runtime_cache_dir,
                &proven,
                &state.transcode,
            )
            .await?;
            if comparison.status != "complete" {
                return Err(anyhow::anyhow!(comparison
                    .error
                    .clone()
                    .unwrap_or_else(|| "Representative comparison incomplete".into())));
            }
            let report = Report {
                measured_at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64,
                results: Vec::new(),
                comparison: Some(comparison),
            };
            save(&state.store, &state.node_id, &report).await?;
            Ok::<_, anyhow::Error>(report)
        };
        let playback = async {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                if state.transcode.active_sessions().await > 0
                    || state.transcode.transcoder_benchmark_must_yield()
                {
                    break;
                }
            }
        };
        let result = tokio::select! {
            result = tokio::time::timeout(std::time::Duration::from_secs(600), run) =>
                result.unwrap_or_else(|_| Err(anyhow::anyhow!("Optimization timed out; previous results retained"))),
            () = playback => Err(anyhow::anyhow!("Optimization stopped because playback started; run again when idle")),
        };
        let mut status = STATUS.lock().await;
        finish_attempt(&mut status, result);
    });
    Ok(())
}

fn finish_attempt(status: &mut Status, result: anyhow::Result<Report>) {
    status.running = false;
    match result {
        Ok(report) => status.report = Some(report),
        Err(error) => {
            status.error = Some(error.to_string());
            if let Some(comparison) = status
                .comparison
                .as_mut()
                .filter(|comparison| comparison.status != "failed")
            {
                comparison.status = "aborted".into();
                comparison.error = Some(error.to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> Report {
        Report {
            measured_at: 1,
            comparison: None,
            results: vec![
                Measurement {
                    backend: "nvenc".into(),
                    fps: 600.0,
                    relative_to_cpu: 2.0,
                },
                Measurement {
                    backend: "software".into(),
                    fps: 300.0,
                    relative_to_cpu: 1.0,
                },
                Measurement {
                    backend: "qsv".into(),
                    fps: 150.0,
                    relative_to_cpu: 0.5,
                },
            ],
        }
    }

    #[test]
    fn auto_prefers_validated_hardware_over_synthetic_cpu_speed_and_respects_explicit_choice() {
        let mut system = SystemInfo {
            hwaccel_pref: "auto".into(),
            encoders: EncoderCaps {
                qsv: true,
                ..Default::default()
            },
            transcoder_optimization: Some(report()),
            ..Default::default()
        };
        assert_eq!(effective_override(&system).as_deref(), Some("qsv"));
        system.encoders.nvenc = true;
        assert_eq!(effective_override(&system).as_deref(), Some("nvenc"));
        system.hwaccel_pref = "qsv".into();
        system.hwaccel_override = Some("qsv".into());
        assert_eq!(effective_override(&system).as_deref(), Some("qsv"));
        system.transcoder_optimization = None;
        system.hwaccel_pref = "auto".into();
        system.hwaccel_override = Some("auto".into());
        assert_eq!(effective_override(&system).as_deref(), Some("nvenc"));
    }

    #[test]
    fn invalid_or_unavailable_synthetic_rows_do_not_change_hardware_preference() {
        let mut report = report();
        report.results[0].fps = f64::NAN;
        report.results[1].fps = 0.0;
        let caps = EncoderCaps {
            qsv: true,
            nvenc: true,
            ..Default::default()
        };
        assert_eq!(report.fastest(&caps), Some(Encoder::Nvenc));
        assert_eq!(
            report.fastest(&EncoderCaps::default()),
            Some(Encoder::Software)
        );
    }
    #[test]
    fn aborted_partial_comparison_keeps_previous_report_and_no_auto_authority() {
        let comparison = Comparison {
            version: 1,
            fixture_source: comparison::fixture_source(),
            policy_snapshot: comparison::PolicySnapshot::default(),
            status: "running".into(),
            error: None,
            executable_sha256: String::new(),
            executable_path: std::path::PathBuf::new(),
            executable_size: 0,
            executable_modified_ns: 0,
            hardware_identity: String::new(),
            cases: Vec::new(),
            results: Vec::new(),
        };
        let mut status = Status {
            running: true,
            report: Some(report()),
            comparison: Some(comparison),
            ..Default::default()
        };
        finish_attempt(&mut status, Err(anyhow::anyhow!("Playback started")));
        assert!(!status.running);
        assert_eq!(
            status
                .report
                .as_ref()
                .expect("retain published report")
                .measured_at,
            1
        );
        let attempt = status.comparison.expect("retain aborted attempt");
        assert_eq!(attempt.status, "aborted");
        assert_eq!(attempt.error.as_deref(), Some("Playback started"));
        assert_eq!(
            attempt.preferred_hardware(&EncoderCaps {
                qsv: true,
                ..Default::default()
            }),
            None
        );
    }
}
