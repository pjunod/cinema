//! Optional service-cost observations for the fixed software writer. They are
//! not admission, a whole-graph CPU claim, or a physical memory ceiling.
use super::*;
use crate::adaptive_resources::runtime::finite_vod::SoftwareVodRequest;
use crate::process::observation::{Metric, ProcessCounters};
use crate::resource_history::{CostPhase, CostSample, HistoryKey, ResourceHistory};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::sync::{Arc, Mutex};

const MIN_WINDOW: Duration = Duration::from_secs(1);
const MAX_WINDOW: Duration = Duration::from_secs(10);
const MAX_PUBLICATION_WAIT: Duration = Duration::from_millis(250);

struct HashWriter<'a>(&'a mut Sha256);
impl Write for HashWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn text(hasher: &mut Sha256, value: &str) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}
fn number(hasher: &mut Sha256, value: u64) {
    hasher.update(value.to_le_bytes());
}

/// Selected recipe portion captured before source planning moves any fields.
/// Only the ledger can later supply the actual concurrent-owner mix.
pub(in crate::adaptive_resources::runtime) struct HistoryBasis {
    runtime_epoch: [u8; 32],
    workload: [u8; 32],
    recipe: [u8; 32],
}
impl HistoryBasis {
    pub(in crate::adaptive_resources::runtime) fn with_mix(
        self,
        concurrent_mix: [u8; 32],
    ) -> HistoryKey {
        HistoryKey {
            runtime_epoch: self.runtime_epoch,
            workload: self.workload,
            recipe: self.recipe,
            concurrent_mix,
            phase: CostPhase::Startup,
        }
    }
}

/// Hashing is streaming and bounded by the already-validated command, source
/// list and segment plan. This is only a prior namespace, never capacity.
pub(in crate::adaptive_resources::runtime) fn history_basis(
    request: &SoftwareVodRequest,
) -> Option<HistoryBasis> {
    let mut epoch = Sha256::new();
    epoch.update(b"plurx-finite-software-runtime-v1");
    epoch.update(request.command.executable().sha256());
    text(&mut epoch, &request.producer.encoded_process);
    number(&mut epoch, request.demand.limit_epoch);
    let demand = match request.demand.resources {
        Metric::Known(value) => value,
        Metric::Unknown(_) => return None,
    };
    for value in [
        demand.cpu_ns_per_second,
        demand.peak_memory_bytes,
        demand.handles,
        demand.processes,
        demand.hardware_slots,
        demand.tuner_slots,
        demand.scratch_bytes,
        request.output.response_memory_bytes,
        request.output.resident_memory_bytes,
        request.output.reserved_storage_bytes,
    ] {
        number(&mut epoch, value);
    }

    let mut workload = Sha256::new();
    workload.update(b"plurx-finite-software-workload-v1");
    serde_json::to_writer(HashWriter(&mut workload), &request.producer.plan).ok()?;
    number(&mut workload, u64::from(request.producer.start_entry));
    number(&mut workload, u64::from(request.producer.frame_ticks));
    number(
        &mut workload,
        request.producer.audio_anchor.unwrap_or(u64::MAX),
    );
    number(
        &mut workload,
        request.producer.video_origin.unwrap_or(u64::MAX),
    );
    for argument in request.command.args() {
        text(&mut workload, argument);
    }

    let mut recipe = Sha256::new();
    recipe.update(b"plurx-finite-software-recipe-v1");
    text(&mut recipe, &request.rendition_id);
    text(&mut recipe, &request.recipe_source_binding);
    for source in &request.sources {
        text(&mut recipe, &source.media_file_id);
        text(&mut recipe, &source.object_version);
        number(&mut recipe, source.scanner_size as u64);
        number(&mut recipe, source.scanner_mtime as u64);
    }
    Some(HistoryBasis {
        runtime_epoch: epoch.finalize().into(),
        workload: workload.finalize().into(),
        recipe: recipe.finalize().into(),
    })
}

#[derive(Clone, Copy)]
struct Baseline {
    at: Instant,
    counters: ProcessCounters,
    control_epoch: u64,
}

pub(super) struct FiniteTelemetry {
    key: Option<HistoryKey>,
    control: RegisteredChildControl,
    history: Arc<Mutex<ResourceHistory>>,
    baseline: Option<Baseline>,
    units: u64,
    recorded_windows: u32,
    recorded: bool,
}
impl FiniteTelemetry {
    pub(super) fn new(
        key: Option<HistoryKey>,
        control: RegisteredChildControl,
        history: Arc<Mutex<ResourceHistory>>,
    ) -> Self {
        let now = Instant::now();
        let baseline =
            control
                .snapshot_resources(now)
                .and_then(
                    |(observation, epoch, sampled_at)| match observation.counters {
                        Metric::Known(counters) => Some(Baseline {
                            at: sampled_at,
                            counters,
                            control_epoch: epoch,
                        }),
                        _ => None,
                    },
                );
        Self {
            key,
            control,
            history,
            baseline,
            units: 0,
            recorded_windows: 0,
            recorded: false,
        }
    }

    pub(super) fn reset(&mut self) {
        self.baseline = None;
        self.units = 0;
        self.recorded_windows = 0;
    }

    pub(super) fn delivered(&mut self, send_started: Instant, now: Instant) {
        if now.saturating_duration_since(send_started) > MAX_PUBLICATION_WAIT {
            self.reset();
        }
    }

    /// Called only after the same native file has been published and synced.
    /// Slow publication, changed process control, missing counters, cached
    /// samples and stale windows lose this observation rather than becoming
    /// zero-cost samples or an inferred InputPaced classification.
    pub(super) fn durable_unit(&mut self, entered_publish: Instant, now: Instant) {
        if self.key.is_none()
            || now.saturating_duration_since(entered_publish) > MAX_PUBLICATION_WAIT
        {
            self.reset();
            return;
        }
        let Some((observation, epoch, sampled_at)) = self.control.snapshot_resources(now) else {
            self.reset();
            return;
        };
        let Metric::Known(counters) = observation.counters else {
            self.reset();
            return;
        };
        let current = Baseline {
            at: sampled_at,
            counters,
            control_epoch: epoch,
        };
        if !matches!(counters.resident_bytes, Metric::Known(_)) {
            self.reset();
            return;
        }
        let Some(before) = self.baseline else {
            self.baseline = Some(current);
            self.units = 0;
            return;
        };
        if before.control_epoch != epoch || before.counters.identity != counters.identity {
            self.baseline = Some(current);
            self.units = 0;
            return;
        }
        let elapsed = sampled_at.saturating_duration_since(before.at);
        if elapsed > MAX_WINDOW || counters.cpu_ns < before.counters.cpu_ns {
            self.baseline = Some(current);
            self.units = 0;
            return;
        }
        self.units = self.units.saturating_add(1);
        if elapsed < MIN_WINDOW {
            return;
        }
        let (Metric::Known(resident), Some(elapsed_ns), Some(unix_ms)) = (
            counters.resident_bytes,
            u64::try_from(elapsed.as_nanos()).ok(),
            unix_ms(),
        ) else {
            self.reset();
            return;
        };
        let key = HistoryKey {
            phase: if self.recorded_windows == 0 {
                CostPhase::Startup
            } else {
                CostPhase::Steady
            },
            ..self.key.expect("checked above")
        };
        let sample = CostSample {
            at: sampled_at,
            unix_ms,
            cpu_ns: counters.cpu_ns - before.counters.cpu_ns,
            completed_units: self.units,
            elapsed_ns,
            observed_resident_bytes: resident,
        };
        if self
            .history
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .observe(key, sample)
        {
            self.recorded = true;
            self.recorded_windows = self.recorded_windows.saturating_add(1);
        }
        self.baseline = Some(current);
        self.units = 0;
    }

    pub(super) fn recorded_history(&self) -> Option<Arc<Mutex<ResourceHistory>>> {
        self.recorded.then(|| Arc::clone(&self.history))
    }
}

pub(super) async fn persist_history(history: Arc<Mutex<ResourceHistory>>) {
    let _ = tokio::task::spawn_blocking(move || {
        if let Some(unix_ms) = unix_ms() {
            let _ = history
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .save(unix_ms);
        }
    })
    .await;
}

fn unix_ms() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
}
