//! Node-local startup calibration and admin-requested reruns.
use std::sync::Arc;

use plurx_core::store::Store;
use plurx_core::transcode::{benchmark_encoder, Encoder, EncoderCaps};
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
}

impl Report {
    pub fn fastest(&self, caps: &EncoderCaps) -> Option<Encoder> {
        self.results
            .iter()
            .filter(|row| row.fps.is_finite() && row.fps > 0.0)
            .filter_map(|row| {
                ENCODERS
                    .into_iter()
                    .find(|encoder| {
                        encoder.family_name() == row.backend && caps.available(*encoder)
                    })
                    .map(|encoder| (encoder, row.fps))
            })
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(encoder, _)| encoder)
    }
}

pub fn effective_override(system: &SystemInfo) -> Option<String> {
    if system.hwaccel_pref.is_empty() || system.hwaccel_pref == "auto" {
        system
            .transcoder_optimization
            .as_ref()
            .and_then(|report| report.fastest(&system.encoders))
            .map(|encoder| encoder.family_name().to_owned())
            .or_else(|| system.hwaccel_override.clone())
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

/// Run after fresh capability qualification, never resurrect a previous boot's
/// result: drivers, devices and the FFmpeg executable may have changed.
pub fn benchmark_threads() -> usize {
    crate::admission::Workload {
        source_height: 720,
        codec: "h264",
        hdr: None,
        target_height: 720,
    }
    .software_threads()
}

pub async fn measure(ffmpeg: &str, caps: &EncoderCaps) -> anyhow::Result<Report> {
    let mut results = Vec::new();
    for encoder in ENCODERS {
        if !caps.available(encoder) {
            continue;
        }
        if let Some(fps) = benchmark_encoder(
            ffmpeg,
            encoder,
            caps.forced_idr.wanted_by(encoder),
            benchmark_threads() as u32,
        )
        .await
        {
            results.push(Measurement {
                backend: encoder.family_name().into(),
                fps,
                relative_to_cpu: 1.0,
            });
        }
    }
    let cpu = results
        .iter()
        .find(|row| row.backend == "software")
        .ok_or_else(|| {
            anyhow::anyhow!("CPU benchmark did not complete; no optimization was applied")
        })?
        .fps;
    for row in &mut results {
        row.relative_to_cpu = row.fps / cpu;
    }
    results.sort_by(|a, b| b.fps.total_cmp(&a.fps));
    Ok(Report {
        measured_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64,
        results,
    })
}

#[derive(Clone, Default, Serialize)]
pub struct Status {
    pub running: bool,
    pub restart_required: bool,
    pub error: Option<String>,
    pub report: Option<Report>,
}

// One daemon serves one node. The mutex serializes admission, while the report
// is replaced only after a complete successful run. HTTP disconnection does
// not cancel a run and two browser tabs cannot run competing benchmarks.
static STATUS: tokio::sync::Mutex<Status> = tokio::sync::Mutex::const_new(Status {
    running: false,
    restart_required: false,
    error: None,
    report: None,
});

pub async fn status(system: &SystemInfo) -> Status {
    let mut status = STATUS.lock().await.clone();
    if status.report.is_none() {
        status.report = system.transcoder_optimization.clone();
    }
    if status.report.is_none() && status.error.is_none() && !status.running {
        status.error = Some("Startup benchmark unavailable; Auto uses the validated fallback order. Optimize to retry.".into());
    }
    status.restart_required = (system.hwaccel_pref == "auto" || system.hwaccel_pref.is_empty())
        && status
            .report
            .as_ref()
            .and_then(|report| report.results.first())
            .and_then(|row| {
                ENCODERS
                    .into_iter()
                    .find(|encoder| encoder.family_name() == row.backend)
            })
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
    tokio::spawn(async move {
        let run = async {
            let _admission = state
                .transcode
                .admit_transcoder_benchmark()
                .await
                .ok_or_else(|| anyhow::anyhow!("The node is busy; run optimization when idle"))?;
            let caps = plurx_core::transcode::detect_encoders(&state.system.ffmpeg).await;
            let report = measure(&state.system.ffmpeg, &caps).await?;
            // Recheck each candidate's HDR graph in isolated scratch. Production
            // keeps its boot-tested graph until the next startup.
            let scratch = tempfile::tempdir_in(&state.runtime_cache_dir)?;
            for encoder in ENCODERS
                .into_iter()
                .filter(|encoder| caps.available(*encoder))
            {
                crate::pipeprobe::probe(scratch.path(), &state.runtime_cache_dir, encoder).await;
            }
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
        status.running = false;
        match result {
            Ok(report) => status.report = Some(report),
            Err(error) => status.error = Some(error.to_string()),
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> Report {
        Report {
            measured_at: 1,
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
    fn auto_uses_measured_winner_including_cpu_and_respects_explicit_choice() {
        let mut system = SystemInfo {
            hwaccel_pref: "auto".into(),
            encoders: EncoderCaps {
                qsv: true,
                ..Default::default()
            },
            transcoder_optimization: Some(report()),
            ..Default::default()
        };
        assert_eq!(effective_override(&system).as_deref(), Some("software"));
        system.encoders.nvenc = true;
        assert_eq!(effective_override(&system).as_deref(), Some("nvenc"));
        system.hwaccel_pref = "qsv".into();
        system.hwaccel_override = Some("qsv".into());
        assert_eq!(effective_override(&system).as_deref(), Some("qsv"));
        system.transcoder_optimization = None;
        system.hwaccel_pref = "auto".into();
        system.hwaccel_override = Some("auto".into());
        assert_eq!(effective_override(&system).as_deref(), Some("auto"));
    }

    #[test]
    fn invalid_or_unavailable_benchmark_rows_never_win() {
        let mut report = report();
        report.results[0].fps = f64::NAN;
        report.results[1].fps = 0.0;
        let caps = EncoderCaps {
            qsv: true,
            nvenc: true,
            ..Default::default()
        };
        assert_eq!(report.fastest(&caps), Some(Encoder::Qsv));
        assert_eq!(report.fastest(&EncoderCaps::default()), None);
    }

    #[tokio::test]
    async fn failed_cpu_benchmark_cannot_publish_a_ranking() {
        assert!(measure(
            "/nonexistent/plurx-benchmark-ffmpeg",
            &EncoderCaps::default()
        )
        .await
        .is_err());
    }
}
