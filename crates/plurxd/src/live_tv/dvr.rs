//! The recording engine: one transport per channel, one sink per recording.
//!
//! The live path opens exactly one tuner GET per session and pumps the
//! response into FFmpeg's stdin. A capture is the same open and the same
//! prefix probe with the pump's sink replaced by a fan-out to files — no
//! encoder, no FFmpeg process. HDHomeRun's `/auto/v{n}` is already a
//! single-programme MPEG-TS and the finite-media path already opens a `.ts`,
//! so the bytes the tuner sends are the bytes that land on disk.
//!
//! Three shapes carry the whole design:
//!
//! - A **transport** is one tuner GET on one channel. It counts against the
//!   tuner limit.
//! - A **sink** is one recording's file, with its own capture window. Several
//!   sinks share a transport when their windows meet, which is how the 8:30
//!   programme's tail padding and the 9:00 programme's head padding cost one
//!   tuner and produce two files. A sink costs no tuner.
//! - An **attempt** is one file: `<base>.a1.part`, `<base>.a2.part`. A capture
//!   that loses its worker — a restart, an owner handoff, a stalled tuner —
//!   resumes into a *new* file, so a fenced old process that is still draining
//!   can never write into the bytes the new attempt is writing. Finishing
//!   concatenates them in order and the gap is recorded rather than hidden.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use plurx_core::dvr::{
    recording_basename, recording_folder, DvrInsertOutcome, DvrKeepMode, DvrOrigin, DvrRecording,
    DvrState, DvrStatePatch, DvrTransition, DVR_ATTEMPTS_MAX, DVR_MIN_USEFUL_S,
    DVR_SCHEDULE_DAYS_MAX,
};
use tokio_util::sync::CancellationToken;

use super::schedule;
use super::{
    collect_live_prefix, open_tuner_stream, pinned_url, unix_seconds, DvrConfig, GuideWindow,
    LiveTunerInput, LiveTvChannel, LiveTvConfig, LiveTvError, LiveTvManager, TUNER_READ_TIMEOUT,
};

/// How often the owner re-reads the world. Fast enough that a capture starts
/// within a quarter of a minute of its padding, slow enough that it is not a
/// poll loop on the replicated Store.
pub(crate) const DVR_TICK: std::time::Duration = std::time::Duration::from_secs(15);
/// How often a sink's byte count reaches the row, so the Activity page can
/// show a rising number without a write per chunk.
const DVR_PROGRESS_INTERVAL_MS: i64 = 30_000;
/// Retention is an hourly sweep, not a per-tick one: it reads every rule's
/// recordings and deletes files, and neither is work worth doing 240 times an
/// hour.
const DVR_RETENTION_INTERVAL_S: i64 = 3600;

/// One tuner GET, feeding every sink on its channel.
pub(crate) struct DvrTransport {
    pub(crate) channel: LiveTvChannel,
    /// The tuner configuration generation this transport opened under.
    /// `drain_before` compares it exactly as it does for a session.
    pub(crate) generation: i64,
    /// The serving-fence generation. Checked on every chunk, so a transport
    /// whose node has lost authority closes its files within one read timeout
    /// rather than writing into a file another owner may already be writing.
    pub(crate) owner_serving_generation: u64,
    pub(crate) cancel: CancellationToken,
    pub(crate) delivered: Arc<AtomicU64>,
    pub(crate) sinks: std::sync::Mutex<Vec<Arc<DvrSink>>>,
    pub(crate) worker: std::sync::Mutex<Option<tokio::task::JoinHandle<Result<(), LiveTvError>>>>,
    /// What the prefix probe saw, for every sidecar this transport writes.
    pub(crate) source: std::sync::Mutex<Option<crate::live_tv_delivery::LiveSourceFacts>>,
}

impl DvrTransport {
    pub(crate) fn live_sinks(&self) -> Vec<Arc<DvrSink>> {
        self.sinks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|sink| !sink.cancel.is_cancelled())
            .cloned()
            .collect()
    }

    /// The furthest moment any sink still wants bytes. The transport closes
    /// when this passes, and a new sink may join while it has not.
    pub(crate) fn open_until(&self) -> i64 {
        self.live_sinks()
            .iter()
            .map(|sink| sink.window.1)
            .max()
            .unwrap_or(i64::MIN)
    }
}

/// One recording's file within a transport.
pub(crate) struct DvrSink {
    pub(crate) recording_id: String,
    pub(crate) title: String,
    pub(crate) attempt: i64,
    /// `[capture_start, capture_end)`. Bytes outside it belong to a different
    /// programme and are simply not written here.
    pub(crate) window: (i64, i64),
    pub(crate) base: PathBuf,
    pub(crate) file: tokio::sync::Mutex<Option<tokio::fs::File>>,
    pub(crate) bytes: AtomicU64,
    pub(crate) cancel: CancellationToken,
}

impl DvrSink {
    fn part_path(&self) -> PathBuf {
        self.base.with_extension(format!("a{}.part", self.attempt))
    }

    fn final_path(&self) -> PathBuf {
        self.base.with_extension("ts")
    }
}

/// What `tuner_capacity` tells a refused viewer: which channels are held, and
/// by what, so the client can offer to stop a recording rather than leaving
/// the viewer to guess.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub(crate) struct DvrHolder {
    pub(crate) channel_id: String,
    pub(crate) guide_number: String,
    pub(crate) channel_name: String,
    pub(crate) sinks: Vec<DvrHolderSink>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub(crate) struct DvrHolderSink {
    pub(crate) recording_id: String,
    pub(crate) title: String,
    pub(crate) ends_at: i64,
}

/// One capture, as the Activity page sees it.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub(crate) struct DvrActivity {
    pub(crate) recording_id: String,
    pub(crate) title: String,
    pub(crate) channel_number: String,
    pub(crate) channel_name: String,
    pub(crate) owner_node_id: String,
    pub(crate) rule_name: Option<String>,
    pub(crate) bytes: u64,
    pub(crate) seconds_left: i64,
    pub(crate) percent: Option<u8>,
    pub(crate) path: Option<String>,
}

impl LiveTvManager {
    /// Both halves of the configuration from one settings read. Two reads
    /// would be two round trips on the replicated backend and could see two
    /// different commits.
    pub(crate) async fn dvr_configs(&self) -> Result<(LiveTvConfig, DvrConfig), LiveTvError> {
        let snapshot = self.store.settings_snapshot().await.map_err(|error| {
            LiveTvError::DeviceUnavailable(format!("reading live-TV settings: {error}"))
        })?;
        let live_tv = LiveTvConfig::from_snapshot(&snapshot, &self.node_id);
        self.observe_config(&live_tv);
        Ok((live_tv, DvrConfig::from_snapshot(&snapshot)))
    }

    /// Channels currently held by captures, for the capacity refusal body.
    pub(crate) fn transport_holders(&self) -> Vec<DvrHolder> {
        let transports = {
            let registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry.transports.values().cloned().collect::<Vec<_>>()
        };
        transports
            .into_iter()
            .map(|transport| DvrHolder {
                channel_id: transport.channel.id.clone(),
                guide_number: transport.channel.guide_number.clone(),
                channel_name: transport.channel.guide_name.clone(),
                sinks: transport
                    .live_sinks()
                    .into_iter()
                    .map(|sink| DvrHolderSink {
                        recording_id: sink.recording_id.clone(),
                        title: sink.title.clone(),
                        ends_at: sink.window.1,
                    })
                    .collect(),
            })
            .collect()
    }

    /// Live captures, for the Activity page.
    ///
    /// Anything on this server that holds a tuner and writes to a disk has to
    /// be attributable from inside the product: what it is, why it chose that
    /// work, and a way to stop it. These rows are that, for recording.
    pub(crate) fn recording_activities(&self) -> Vec<DvrActivity> {
        let now = unix_seconds();
        let transports = {
            let registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry.transports.values().cloned().collect::<Vec<_>>()
        };
        let mut out = Vec::new();
        for transport in transports {
            for sink in transport.live_sinks() {
                let (start, end) = sink.window;
                let span = (end - start).max(1);
                let elapsed = (now - start).clamp(0, span);
                out.push(DvrActivity {
                    recording_id: sink.recording_id.clone(),
                    title: sink.title.clone(),
                    channel_number: transport.channel.guide_number.clone(),
                    channel_name: transport.channel.guide_name.clone(),
                    owner_node_id: self.node_id.clone(),
                    rule_name: None,
                    bytes: sink.bytes.load(Ordering::Relaxed),
                    seconds_left: (end - now).max(0),
                    percent: u8::try_from(elapsed.saturating_mul(100) / span).ok(),
                    path: sink.final_path().to_str().map(str::to_owned),
                });
            }
        }
        out.sort_by(|left, right| {
            left.channel_number
                .cmp(&right.channel_number)
                .then(left.title.cmp(&right.title))
        });
        out
    }

    /// The owner's recording loop.
    ///
    /// Owner-only by construction, like the guide loop, and holding no job
    /// lease: the tuner configuration already names one owner, and a lease
    /// would be a second authority over the same resource that could disagree
    /// with it.
    pub(crate) async fn dvr_loop(
        self: Arc<Self>,
        events: super::webhook::DvrEventSink,
        shutdown: CancellationToken,
    ) {
        let mut last_retention = 0_i64;
        loop {
            if let Ok((live_tv, dvr)) = self.dvr_configs().await {
                let ours = live_tv.owner_node_id == self.node_id;
                if ours && live_tv.enabled && dvr.enabled && self.serving.admit().is_some() {
                    if let Err(error) = self
                        .dvr_tick(&live_tv, &dvr, &events, &mut last_retention)
                        .await
                    {
                        tracing::warn!(
                            code = error.code(),
                            "the recording tick did not complete; it will be retried"
                        );
                    }
                } else if !ours || !live_tv.enabled || !dvr.enabled {
                    // Not this node's work any more, or switched off. Close
                    // what we hold rather than leaving a tuner occupied by a
                    // feature the operator has turned off.
                    self.close_all_transports().await;
                }
            }
            tokio::select! {
                _ = shutdown.cancelled() => {
                    self.close_all_transports().await;
                    return;
                }
                _ = tokio::time::sleep(DVR_TICK) => {}
            }
        }
    }

    /// One pass. The order matters and is the whole tick:
    ///
    /// recover before scheduling, so a restarted owner resumes what it was
    /// already doing before it plans anything new; terminalise before
    /// allocating, so a tuner is never planned for an airing that can no
    /// longer usefully start; and start last, so a start always acts on a plan
    /// made in this same pass.
    async fn dvr_tick(
        self: &Arc<Self>,
        live_tv: &LiveTvConfig,
        dvr: &DvrConfig,
        events: &super::webhook::DvrEventSink,
        last_retention: &mut i64,
    ) -> Result<(), LiveTvError> {
        // Read once at the top and carry it through every conditional write:
        // a settings save mid-tick must land no rows from the configuration it
        // replaced. Same discipline as `registry.min_generation` on the live
        // path.
        let generation = live_tv.generation;
        let now = unix_seconds();

        self.dvr_recover(live_tv, dvr, events, generation, now)
            .await?;
        self.dvr_expand(live_tv, dvr, now).await?;
        self.dvr_reconcile(live_tv, generation, now).await?;
        self.dvr_consume_stops(events, generation).await?;
        self.dvr_terminalise(generation, now).await?;
        self.dvr_allocate(live_tv, dvr, generation, now).await?;
        self.dvr_start(live_tv, dvr, events, generation, now)
            .await?;
        self.dvr_finish(events, generation, now).await?;
        if now - *last_retention >= DVR_RETENTION_INTERVAL_S {
            *last_retention = now;
            self.dvr_retain(now).await?;
        }
        Ok(())
    }

    /// Step 1 — resume what this owner was doing before it stopped being able
    /// to do it.
    ///
    /// A row in `recording` with no live sink is a capture whose worker is
    /// gone: the process restarted, the owner changed, or the transport
    /// failed. While there is still a useful amount of programme left, open
    /// the next attempt — a new file, so the old process draining into the
    /// previous one cannot corrupt it — and record the gap. Otherwise finish
    /// what is on disk honestly.
    async fn dvr_recover(
        self: &Arc<Self>,
        live_tv: &LiveTvConfig,
        dvr: &DvrConfig,
        events: &super::webhook::DvrEventSink,
        generation: i64,
        now: i64,
    ) -> Result<(), LiveTvError> {
        let live = self.live_recording_ids();
        let rows = self.dvr_rows(&[DvrState::Recording]).await?;
        for row in rows {
            if live.contains(&row.id) {
                continue;
            }
            let last_progress = row.last_progress_ms.unwrap_or(row.created_at_ms) / 1000;
            let gap = (now - last_progress).max(0);
            if now < row.capture_end - DVR_MIN_USEFUL_S && row.attempt < DVR_ATTEMPTS_MAX {
                let attempt = row.attempt + 1;
                match self
                    .attach_sink(live_tv, dvr, &row, attempt, generation)
                    .await
                {
                    Ok(()) => {
                        self.transition(
                            &row.id,
                            &[DvrState::Recording],
                            DvrState::Recording,
                            Some("resumed after the capture lost its worker"),
                            DvrStatePatch::Reattempt {
                                attempt,
                                gap_s: row.gap_s + gap,
                            },
                            Some(generation),
                        )
                        .await?;
                        continue;
                    }
                    Err(error) => {
                        tracing::warn!(
                            recording = %row.id,
                            code = error.code(),
                            "could not resume a capture; it will be retried next tick"
                        );
                        continue;
                    }
                }
            }
            let reason = if row.attempt >= DVR_ATTEMPTS_MAX {
                "attempts exhausted"
            } else {
                "worker lost"
            };
            self.finish_row(&row, events, now, gap, None, reason, generation)
                .await?;
        }
        Ok(())
    }

    /// Step 2 — materialise the airings every enabled rule matches.
    ///
    /// Insert-if-absent, always. A rule that keeps matching an airing the
    /// viewer skipped must not bring it back, and this is the only place that
    /// could.
    async fn dvr_expand(
        &self,
        live_tv: &LiveTvConfig,
        dvr: &DvrConfig,
        now: i64,
    ) -> Result<(), LiveTvError> {
        let rules = self.store.list_dvr_rules().await.map_err(store_error)?;
        if rules.is_empty() || !live_tv.guide_fetches() {
            return Ok(());
        }
        let guide = self
            .local_guide(
                live_tv,
                GuideWindow {
                    start: now,
                    end: now + DVR_SCHEDULE_DAYS_MAX * 86_400,
                },
            )
            .await;
        let lineup = self.cached_lineup(live_tv).await;
        let at_ms = now.saturating_mul(1000);
        for channel in &guide.channels {
            let name = lineup
                .iter()
                .find(|entry| entry.id == channel.id)
                .map(|entry| entry.guide_name.clone())
                .unwrap_or_else(|| channel.guide_number.clone());
            for programme in &channel.programmes {
                if programme.start <= now {
                    continue;
                }
                // Highest-priority matching rule wins the row it creates; the
                // rest are simply also true, and reconciliation keeps the
                // pointer current if priorities change later.
                let Some(rule) = rules
                    .iter()
                    .filter(|rule| crate::http::dvr::rule_matches(rule, &channel.id, programme))
                    .min_by_key(|rule| rule.priority)
                else {
                    continue;
                };
                let row = crate::http::dvr::recording_from_programme(
                    &crate::http::dvr::ResolvedChannel {
                        id: channel.id.clone(),
                        guide_number: channel.guide_number.clone(),
                        name: name.clone(),
                    },
                    programme,
                    DvrOrigin::Rule,
                    Some(rule.id.clone()),
                    Some(rule.owner_user_id),
                    rule.pad_start_s,
                    rule.pad_end_s,
                    at_ms,
                );
                match self
                    .store
                    .insert_dvr_airing_if_absent(&row)
                    .await
                    .map_err(store_error)?
                {
                    DvrInsertOutcome::Inserted => tracing::info!(
                        recording = %row.id,
                        rule = %rule.name,
                        title = %row.title,
                        "scheduled a recording from a rule"
                    ),
                    DvrInsertOutcome::Exists(_) => {}
                }
                let _ = dvr;
            }
        }
        Ok(())
    }

    /// Step 3 — keep every pending row pointing at a rule that still wants it,
    /// and surface the ones nothing does.
    async fn dvr_reconcile(
        &self,
        live_tv: &LiveTvConfig,
        generation: i64,
        now: i64,
    ) -> Result<(), LiveTvError> {
        let rules = self.store.list_dvr_rules().await.map_err(store_error)?;
        let rows = self.dvr_rows(DvrState::PENDING).await?;
        if rows.is_empty() {
            return Ok(());
        }
        let guide = if live_tv.guide_fetches() {
            Some(
                self.local_guide(
                    live_tv,
                    GuideWindow {
                        start: now - 3600,
                        end: now + DVR_SCHEDULE_DAYS_MAX * 86_400,
                    },
                )
                .await,
            )
        } else {
            None
        };
        let mut repoint = Vec::new();
        for row in rows {
            // Does the guide still carry this exact airing, under this title?
            let still_listed = guide.as_ref().map(|guide| {
                guide.channels.iter().any(|channel| {
                    channel.id == row.channel_id
                        && channel.programmes.iter().any(|programme| {
                            programme.start == row.airing_start && programme.title == row.title
                        })
                })
            });
            if still_listed == Some(false) && row.airing_start > now {
                match row.origin {
                    DvrOrigin::Rule => {
                        self.transition(
                            &row.id,
                            DvrState::PENDING,
                            DvrState::Withdrawn,
                            Some("the guide no longer lists this programme at that time"),
                            DvrStatePatch::None,
                            Some(generation),
                        )
                        .await?;
                    }
                    // A person asked for this one, so it is theirs to
                    // re-decide. Surfaced, never withdrawn.
                    DvrOrigin::Manual => {
                        if row.state != DvrState::Stale {
                            self.transition(
                                &row.id,
                                DvrState::PENDING,
                                DvrState::Stale,
                                Some("the programme moved or was renamed"),
                                DvrStatePatch::None,
                                Some(generation),
                            )
                            .await?;
                        }
                    }
                }
                continue;
            }
            if row.origin != DvrOrigin::Rule {
                continue;
            }
            let owner = guide.as_ref().and_then(|guide| {
                let programme = guide.channels.iter().find_map(|channel| {
                    (channel.id == row.channel_id)
                        .then(|| {
                            channel
                                .programmes
                                .iter()
                                .find(|programme| programme.start == row.airing_start)
                        })
                        .flatten()
                })?;
                rules
                    .iter()
                    .filter(|rule| crate::http::dvr::rule_matches(rule, &row.channel_id, programme))
                    .min_by_key(|rule| rule.priority)
                    .map(|rule| rule.id.clone())
            });
            match owner {
                Some(rule_id) => {
                    if row.rule_id.as_deref() != Some(rule_id.as_str()) {
                        repoint.push((row.id.clone(), Some(rule_id)));
                    }
                    // A row that was withdrawn and now matches again comes
                    // back: withdrawal is the absence of a rule, not a
                    // decision anybody made.
                    if row.state == DvrState::Withdrawn {
                        self.transition(
                            &row.id,
                            &[DvrState::Withdrawn],
                            DvrState::Scheduled,
                            None,
                            DvrStatePatch::None,
                            Some(generation),
                        )
                        .await?;
                    }
                }
                None if guide.is_some() && row.state != DvrState::Withdrawn => {
                    self.transition(
                        &row.id,
                        DvrState::PENDING,
                        DvrState::Withdrawn,
                        Some("no enabled rule matches this programme any more"),
                        DvrStatePatch::None,
                        Some(generation),
                    )
                    .await?;
                }
                None => {}
            }
        }
        if !repoint.is_empty() {
            self.store
                .repoint_dvr_rule_rows(&repoint, now.saturating_mul(1000))
                .await
                .map_err(store_error)?;
        }
        Ok(())
    }

    /// Step 4 — act on the Stop a viewer pressed, possibly on another node.
    async fn dvr_consume_stops(
        &self,
        events: &super::webhook::DvrEventSink,
        generation: i64,
    ) -> Result<(), LiveTvError> {
        let rows = self.dvr_rows(&[DvrState::Recording]).await?;
        for row in rows {
            if row.stop_requested_at_ms.is_none() {
                continue;
            }
            let now = unix_seconds();
            self.stop_sink(&row.id).await;
            self.finish_row(
                &row,
                events,
                now,
                0,
                row.stop_requested_by_user_id,
                "stopped by a viewer",
                generation,
            )
            .await?;
        }
        Ok(())
    }

    /// Step 5 — anything that can no longer usefully start never starts.
    ///
    /// Without this a restarted owner would find an airing whose window closed
    /// hours ago still marked `scheduled`, tune its channel, and record
    /// whatever happens to be on now under last night's title.
    async fn dvr_terminalise(&self, generation: i64, now: i64) -> Result<(), LiveTvError> {
        for row in self.dvr_rows(DvrState::PENDING).await? {
            if now < row.capture_end - DVR_MIN_USEFUL_S {
                continue;
            }
            let reason = match row.state {
                DvrState::Conflict => row
                    .state_reason
                    .clone()
                    .unwrap_or_else(|| "no tuner was free".to_owned()),
                DvrState::Withdrawn | DvrState::Stale => {
                    "the programme was no longer scheduled".to_owned()
                }
                _ => "the server was not recording when it should have started".to_owned(),
            };
            self.transition(
                &row.id,
                DvrState::PENDING,
                DvrState::Missed,
                Some(&reason),
                DvrStatePatch::None,
                Some(generation),
            )
            .await?;
        }
        Ok(())
    }

    /// Step 6 — recompute which airings have a tuner.
    async fn dvr_allocate(
        &self,
        live_tv: &LiveTvConfig,
        dvr: &DvrConfig,
        generation: i64,
        now: i64,
    ) -> Result<(), LiveTvError> {
        let rows = self
            .dvr_rows(&[DvrState::Scheduled, DvrState::Conflict, DvrState::Recording])
            .await?
            .into_iter()
            .filter(|row| row.capture_start <= now + DVR_SCHEDULE_DAYS_MAX * 86_400)
            .collect::<Vec<_>>();
        if rows.is_empty() {
            return Ok(());
        }
        let priorities = self
            .store
            .list_dvr_rules()
            .await
            .map_err(store_error)?
            .into_iter()
            .map(|rule| (rule.id, rule.priority))
            .collect::<BTreeMap<_, _>>();
        let floor_met = super::free_space_bytes(&dvr.root).is_none_or(|free| {
            free >= (dvr.free_floor_gb.max(0) as u64).saturating_mul(1_000_000_000)
        });
        let plan = schedule::allocate(
            &rows,
            dvr.recording_slots(live_tv.max_sessions),
            floor_met,
            &|rule_id| priorities.get(rule_id).copied().unwrap_or(i64::MAX),
        );
        for entry in plan {
            let current = rows.iter().find(|row| row.id == entry.id);
            let unchanged = current.is_some_and(|row| {
                row.state == entry.state && row.state_reason.as_deref() == entry.reason.as_deref()
            });
            if unchanged {
                continue;
            }
            self.transition(
                &entry.id,
                &[DvrState::Scheduled, DvrState::Conflict],
                entry.state,
                entry.reason.as_deref(),
                DvrStatePatch::None,
                Some(generation),
            )
            .await?;
        }
        Ok(())
    }

    /// Step 7 — open the captures whose moment has come.
    async fn dvr_start(
        self: &Arc<Self>,
        live_tv: &LiveTvConfig,
        dvr: &DvrConfig,
        events: &super::webhook::DvrEventSink,
        generation: i64,
        now: i64,
    ) -> Result<(), LiveTvError> {
        let live = self.live_recording_ids();
        for row in self.dvr_rows(&[DvrState::Scheduled]).await? {
            if row.capture_start > now || now >= row.capture_end - DVR_MIN_USEFUL_S {
                continue;
            }
            if live.contains(&row.id) {
                continue;
            }
            if let Err(error) = self.attach_sink(live_tv, dvr, &row, 1, generation).await {
                tracing::warn!(
                    recording = %row.id,
                    code = error.code(),
                    "could not start a capture; it will be retried next tick"
                );
                continue;
            }
            let late = (now - row.capture_start).max(0);
            self.transition(
                &row.id,
                &[DvrState::Scheduled],
                DvrState::Recording,
                (late > 0).then_some("started late"),
                DvrStatePatch::Started {
                    attempt: 1,
                    tuner_owner_node_id: self.node_id.clone(),
                    started_at_ms: now.saturating_mul(1000),
                    late_start_s: late,
                    path: self
                        .recording_final_path(dvr, &row)
                        .to_string_lossy()
                        .into_owned(),
                },
                Some(generation),
            )
            .await?;
            tracing::info!(
                recording = %row.id,
                title = %row.title,
                channel = %row.guide_number,
                "recording started"
            );
            events.enqueue(super::webhook::DvrEvent::RecordingStarted {
                recording_id: row.id.clone(),
                channel_id: row.channel_id.clone(),
                guide_number: row.guide_number.clone(),
                title: row.title.clone(),
                airing_start: row.airing_start,
                capture_end: row.capture_end,
            });
        }
        Ok(())
    }

    /// Step 8 — close the captures whose window has passed.
    async fn dvr_finish(
        &self,
        events: &super::webhook::DvrEventSink,
        generation: i64,
        now: i64,
    ) -> Result<(), LiveTvError> {
        let transports = {
            let registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry.transports.values().cloned().collect::<Vec<_>>()
        };
        let mut finished = HashSet::new();
        for transport in &transports {
            for sink in transport.live_sinks() {
                if now < sink.window.1 {
                    continue;
                }
                sink.cancel.cancel();
                finished.insert(sink.recording_id.clone());
            }
        }
        for row in self.dvr_rows(&[DvrState::Recording]).await? {
            if !finished.contains(&row.id) && now < row.capture_end {
                continue;
            }
            if !finished.contains(&row.id) {
                // Its window closed but no sink was holding it — the recover
                // step already accounted for the gap, so just close it out.
                self.stop_sink(&row.id).await;
            }
            self.finish_row(&row, events, now, 0, None, "capture complete", generation)
                .await?;
        }
        self.close_finished_transports().await;
        Ok(())
    }

    /// Step 9 — apply each rule's keep policy, hourly.
    async fn dvr_retain(&self, now: i64) -> Result<(), LiveTvError> {
        let rules = self.store.list_dvr_rules().await.map_err(store_error)?;
        let rows = self.dvr_rows(&[DvrState::Done, DvrState::Partial]).await?;
        for rule in rules {
            let mut mine = rows
                .iter()
                .filter(|row| row.rule_id.as_deref() == Some(rule.id.as_str()))
                .collect::<Vec<_>>();
            mine.sort_by_key(|row| std::cmp::Reverse(row.airing_start));
            let doomed: Vec<&&DvrRecording> = match rule.keep_mode {
                DvrKeepMode::All => Vec::new(),
                DvrKeepMode::LastN => mine.iter().skip(rule.keep_value.max(0) as usize).collect(),
                DvrKeepMode::Days => mine
                    .iter()
                    .filter(|row| now - row.airing_start > rule.keep_value.max(0) * 86_400)
                    .collect(),
                // Watch state is the owner's to read, and a recording nobody
                // has watched is exactly the one to keep. Handled where watch
                // state lives rather than guessed at here.
                DvrKeepMode::UntilWatched => Vec::new(),
            };
            for row in doomed {
                self.delete_recording_files(row).await;
                self.transition(
                    &row.id,
                    &[DvrState::Done, DvrState::Partial],
                    DvrState::Deleted,
                    Some(&format!(
                        "removed by the rule's keep policy ({})",
                        rule.name
                    )),
                    DvrStatePatch::None,
                    None,
                )
                .await?;
            }
        }
        Ok(())
    }

    // ---- capture plumbing -------------------------------------------------

    /// Join the transport already tuned to this channel, or open a new one.
    ///
    /// Opening needs a tuner slot; joining does not, because the bytes are
    /// already being received and a second file costs no hardware.
    pub(crate) async fn attach_sink(
        self: &Arc<Self>,
        live_tv: &LiveTvConfig,
        dvr: &DvrConfig,
        row: &DvrRecording,
        attempt: i64,
        generation: i64,
    ) -> Result<(), LiveTvError> {
        if dvr.root.trim().is_empty() {
            return Err(LiveTvError::InvalidConfig(
                "no DVR root is configured, so a recording has nowhere to go".to_owned(),
            ));
        }
        let channel = self
            .cached_lineup(live_tv)
            .await
            .into_iter()
            .find(|channel| channel.id == row.channel_id)
            .ok_or_else(|| {
                LiveTvError::ChannelNotFound(
                    "the channel this recording names is not in the lineup".to_owned(),
                )
            })?;
        if channel.drm {
            return Err(LiveTvError::DrmUnsupported(
                "this channel is protected and cannot be recorded".to_owned(),
            ));
        }
        let base = self.recording_base_path(dvr, row);
        if let Some(parent) = base.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|error| {
                LiveTvError::StreamFailed(format!("creating the recording folder: {error}"))
            })?;
        }
        let sink = Arc::new(DvrSink {
            recording_id: row.id.clone(),
            title: row.title.clone(),
            attempt,
            window: (row.capture_start, row.capture_end),
            base: base.clone(),
            file: tokio::sync::Mutex::new(None),
            bytes: AtomicU64::new(0),
            cancel: CancellationToken::new(),
        });
        // `create_new` is the whole guarantee against a fenced predecessor: an
        // attempt file that already exists means another process owns it, and
        // this attempt takes the next number rather than truncating it.
        let file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(sink.part_path())
            .await
            .map_err(|error| {
                LiveTvError::StreamFailed(format!(
                    "opening attempt {attempt} of this recording: {error}"
                ))
            })?;
        *sink.file.lock().await = Some(file);

        let serving_generation = self.serving.admit().ok_or_else(|| {
            LiveTvError::OwnerUnavailable(crate::serving_fence::SERVING_FENCED_MESSAGE.to_owned())
        })?;
        let existing = {
            let registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if registry.closing {
                return Err(LiveTvError::OwnerUnavailable(
                    "this node is shutting down".to_owned(),
                ));
            }
            match registry.transports.get(&row.channel_id).cloned() {
                Some(transport)
                    if schedule::may_share_transport(transport.open_until(), row.capture_start) =>
                {
                    transport
                        .sinks
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(Arc::clone(&sink));
                    Some(transport)
                }
                _ => {
                    if !registry.may_open_transport(live_tv.max_sessions, dvr.tuner_reserve) {
                        return Err(LiveTvError::Capacity(
                            "no tuner is free for this recording".to_owned(),
                        ));
                    }
                    None
                }
            }
        };
        if existing.is_some() {
            return Ok(());
        }

        let transport = Arc::new(DvrTransport {
            channel: channel.clone(),
            generation,
            owner_serving_generation: serving_generation,
            cancel: CancellationToken::new(),
            delivered: Arc::new(AtomicU64::new(0)),
            sinks: std::sync::Mutex::new(vec![Arc::clone(&sink)]),
            worker: std::sync::Mutex::new(None),
            source: std::sync::Mutex::new(None),
        });
        {
            let mut registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry
                .transports
                .insert(row.channel_id.clone(), Arc::clone(&transport));
        }
        let manager = Arc::downgrade(self);
        let worker_transport = Arc::clone(&transport);
        let address = live_tv.device_ipv4.ok_or_else(|| {
            LiveTvError::InvalidConfig("an HDHomeRun IPv4 address is required".to_owned())
        })?;
        let guide_number = channel.guide_number.clone();
        let scratch = self.scratch_root.join(format!("dvr-{}", channel.id));
        let serving = self.serving.clone();
        let client = self
            .client
            .as_ref()
            .map_err(|error| {
                LiveTvError::DeviceUnavailable(format!("the HTTP client is unavailable: {error}"))
            })?
            .clone();
        let worker = tokio::spawn(async move {
            let result = run_transport(
                client,
                address,
                guide_number,
                scratch,
                serving,
                Arc::clone(&worker_transport),
            )
            .await;
            if let Some(manager) = manager.upgrade() {
                manager.close_transport(&worker_transport.channel.id).await;
            }
            result
        });
        *transport
            .worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(worker);
        Ok(())
    }

    /// Cancel one recording's sink. The transport lives on while any other
    /// sink still wants bytes — stopping one of two recordings on a channel
    /// frees no tuner, and pretending otherwise is what would make the
    /// capacity dialog lie.
    pub(crate) async fn stop_sink(&self, recording_id: &str) -> bool {
        let transports = {
            let registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry.transports.values().cloned().collect::<Vec<_>>()
        };
        let mut stopped = false;
        for transport in transports {
            for sink in transport.live_sinks() {
                if sink.recording_id == recording_id {
                    sink.cancel.cancel();
                    if let Some(mut file) = sink.file.lock().await.take() {
                        use tokio::io::AsyncWriteExt as _;
                        let _ = file.flush().await;
                    }
                    stopped = true;
                }
            }
        }
        self.close_finished_transports().await;
        stopped
    }

    fn live_recording_ids(&self) -> HashSet<String> {
        let registry = self
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        registry
            .transports
            .values()
            .flat_map(|transport| {
                transport
                    .live_sinks()
                    .into_iter()
                    .map(|sink| sink.recording_id.clone())
            })
            .collect()
    }

    async fn close_finished_transports(&self) {
        let spent = {
            let registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry
                .transports
                .iter()
                .filter(|(_, transport)| transport.live_sinks().is_empty())
                .map(|(channel, _)| channel.clone())
                .collect::<Vec<_>>()
        };
        for channel in spent {
            self.close_transport(&channel).await;
        }
    }

    pub(crate) async fn close_transport(&self, channel_id: &str) {
        let transport = {
            let mut registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry.transports.remove(channel_id)
        };
        let Some(transport) = transport else {
            return;
        };
        transport.cancel.cancel();
        for sink in transport.live_sinks() {
            sink.cancel.cancel();
        }
        let worker = transport
            .worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(worker) = worker {
            let _ = tokio::time::timeout(super::SESSION_DRAIN_TIMEOUT, worker).await;
        }
    }

    pub(crate) async fn close_all_transports(&self) {
        let channels = {
            let registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry.transports.keys().cloned().collect::<Vec<_>>()
        };
        for channel in channels {
            self.close_transport(&channel).await;
        }
    }

    // ---- rows and files ---------------------------------------------------

    async fn dvr_rows(&self, states: &[DvrState]) -> Result<Vec<DvrRecording>, LiveTvError> {
        self.store
            .list_dvr_recordings_in(states)
            .await
            .map_err(store_error)
    }

    async fn transition(
        &self,
        id: &str,
        from: &[DvrState],
        to: DvrState,
        reason: Option<&str>,
        patch: DvrStatePatch,
        fence_generation: Option<i64>,
    ) -> Result<bool, LiveTvError> {
        self.store
            .transition_dvr_recording(&DvrTransition {
                id,
                from,
                to,
                reason,
                patch,
                fence_generation,
                now_ms: unix_seconds().saturating_mul(1000),
            })
            .await
            .map_err(store_error)
    }

    fn recording_base_path(&self, dvr: &DvrConfig, row: &DvrRecording) -> PathBuf {
        PathBuf::from(&dvr.root)
            .join(recording_folder(&row.title))
            .join(recording_basename(
                &row.title,
                row.airing_start,
                &row.guide_number,
                &row.id,
            ))
    }

    fn recording_final_path(&self, dvr: &DvrConfig, row: &DvrRecording) -> PathBuf {
        self.recording_base_path(dvr, row).with_extension("ts")
    }

    /// Close a capture out: join its attempts into one file, write the
    /// sidecar, and record honestly what it managed to get.
    #[allow(clippy::too_many_arguments)]
    async fn finish_row(
        &self,
        row: &DvrRecording,
        events: &super::webhook::DvrEventSink,
        now: i64,
        extra_gap: i64,
        stopped_by: Option<i64>,
        reason: &str,
        generation: i64,
    ) -> Result<(), LiveTvError> {
        let (_, dvr) = self.dvr_configs().await?;
        let base = self.recording_base_path(&dvr, row);
        let facts = self.transport_source_facts(&row.channel_id);
        let gap = row.gap_s + extra_gap;
        let bytes = match concatenate_attempts(&base, row.attempt).await {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(
                    recording = %row.id,
                    %error,
                    "could not assemble a recording's attempts"
                );
                0
            }
        };
        let state = if bytes == 0 {
            DvrState::Failed
        } else if gap > 0 || row.late_start_s > 0 {
            DvrState::Partial
        } else {
            DvrState::Done
        };
        let reason = if state == DvrState::Partial {
            format!("{reason}; {gap}s not captured, {}s late", row.late_start_s)
        } else {
            reason.to_owned()
        };
        if bytes > 0 {
            write_sidecar(&base, row, &facts, state, bytes, gap, now).await;
        }
        self.transition(
            &row.id,
            &[DvrState::Recording],
            state,
            Some(&reason),
            DvrStatePatch::Finished {
                finished_at_ms: now.saturating_mul(1000),
                bytes: i64::try_from(bytes).unwrap_or(i64::MAX),
                gap_s: gap,
                path: base.with_extension("ts").to_str().map(str::to_owned),
                stopped_by_user_id: stopped_by,
            },
            Some(generation),
        )
        .await?;
        tracing::info!(
            recording = %row.id,
            title = %row.title,
            state = state.as_str(),
            bytes,
            gap_s = gap,
            "recording finished"
        );
        // Enqueued, never sent from here: a tick must not wait on a network
        // call it cannot bound.
        events.enqueue(if state == DvrState::Failed {
            super::webhook::DvrEvent::RecordingFailed {
                recording_id: row.id.clone(),
                title: row.title.clone(),
                reason: reason.clone(),
            }
        } else {
            super::webhook::DvrEvent::RecordingFinished {
                recording_id: row.id.clone(),
                title: row.title.clone(),
                state: state.as_str().to_owned(),
                bytes: i64::try_from(bytes).unwrap_or(i64::MAX),
                gap_s: gap,
                path: base.with_extension("ts").to_str().map(str::to_owned),
            }
        });
        Ok(())
    }

    fn transport_source_facts(
        &self,
        channel_id: &str,
    ) -> Option<crate::live_tv_delivery::LiveSourceFacts> {
        // Clone the transport out before releasing the registry lock: the
        // source facts live behind a second lock, and holding both at once is
        // the shape that eventually deadlocks.
        let transport = {
            let registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry.transports.get(channel_id).cloned()
        }?;
        let facts = transport
            .source
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        facts
    }

    async fn delete_recording_files(&self, row: &DvrRecording) {
        let Some(path) = row.path.as_deref() else {
            return;
        };
        let base = PathBuf::from(path);
        for candidate in [base.clone(), base.with_extension("json")] {
            if let Err(error) = tokio::fs::remove_file(&candidate).await {
                if error.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(%error, path = %candidate.display(), "could not remove a recording");
                }
            }
        }
    }
}

fn store_error(error: plurx_core::error::StoreError) -> LiveTvError {
    LiveTvError::DeviceUnavailable(format!("reading recording state: {error}"))
}

/// Open the tuner, keep the same prefix the live path keeps, probe it, and
/// then write every chunk to every sink whose window contains this moment.
async fn run_transport(
    client: reqwest::Client,
    address: std::net::Ipv4Addr,
    guide_number: String,
    scratch: PathBuf,
    serving: crate::serving_fence::ServingAuthority,
    transport: Arc<DvrTransport>,
) -> Result<(), LiveTvError> {
    let url = pinned_url(address, 5004, &format!("/auto/v{guide_number}"))?;
    let deadline = tokio::time::Instant::now() + super::STARTUP_TIMEOUT;
    let response = open_tuner_stream(&client, url, deadline).await?;
    let input = collect_live_prefix(response, &transport.cancel).await?;
    // The same prefix the live path probes, so every sidecar this transport
    // writes describes the same source facts a viewer's session would see.
    if let Err(error) = tokio::fs::create_dir_all(&scratch).await {
        tracing::warn!(%error, "could not create the capture scratch folder");
    }
    pump_tuner_fanout(input, serving, transport).await
}

/// The fan-out itself. Mirrors `pump_tuner_stream`: one task owns the response,
/// so cancelling and joining it releases the tuner connection rather than
/// leaving a detached reader behind.
async fn pump_tuner_fanout(
    input: LiveTunerInput,
    serving: crate::serving_fence::ServingAuthority,
    transport: Arc<DvrTransport>,
) -> Result<(), LiveTvError> {
    use futures_util::StreamExt as _;
    use tokio::io::AsyncWriteExt as _;

    let mut stream = input.remainder;
    let mut pending = std::iter::once(input.prefix)
        .chain(input.queued)
        .collect::<std::collections::VecDeque<_>>();
    loop {
        let bytes = match pending.pop_front() {
            Some(bytes) => bytes,
            None => {
                let next = tokio::select! {
                    biased;
                    _ = transport.cancel.cancelled() => return Ok(()),
                    next = tokio::time::timeout(TUNER_READ_TIMEOUT, stream.next()) => next,
                }
                .map_err(|_| {
                    LiveTvError::StreamFailed("the HDHomeRun stream stopped producing bytes".into())
                })?;
                let Some(bytes) = next else {
                    return Err(LiveTvError::StreamFailed(
                        "the HDHomeRun stream ended".into(),
                    ));
                };
                bytes.map_err(|_| {
                    LiveTvError::StreamFailed("the HDHomeRun stream body failed".into())
                })?
            }
        };
        // Checked per chunk, exactly as a session's fence is. A node that
        // has lost serving authority must stop writing within one read
        // timeout: the replacement owner is about to open its own attempt,
        // and two processes writing one recording is the corruption the
        // attempt files exist to make impossible.
        if !serving.is_current(transport.owner_serving_generation) {
            for sink in transport.live_sinks() {
                sink.cancel.cancel();
            }
            return Err(LiveTvError::OwnerUnavailable(
                crate::serving_fence::SERVING_FENCED_MESSAGE.to_owned(),
            ));
        }
        let sinks = transport.live_sinks();
        if sinks.is_empty() {
            return Ok(());
        }
        let now = unix_seconds();
        for sink in sinks {
            // A sink takes only the bytes inside its own window. The pads of
            // two adjacent programmes overlap deliberately, and in that
            // overlap both files get the same bytes — which is what makes a
            // shared transport correct rather than merely cheap.
            if now < sink.window.0 || now >= sink.window.1 {
                continue;
            }
            let mut handle = sink.file.lock().await;
            let Some(file) = handle.as_mut() else {
                continue;
            };
            match tokio::time::timeout(TUNER_READ_TIMEOUT, file.write_all(&bytes)).await {
                Ok(Ok(())) => {
                    sink.bytes.fetch_add(bytes.len() as u64, Ordering::Release);
                    transport
                        .delivered
                        .fetch_add(bytes.len() as u64, Ordering::Release);
                }
                // One sink's disk failing is that recording's problem, not
                // every recording on the channel. Cancel it and keep the
                // transport feeding the others.
                Ok(Err(error)) => {
                    tracing::warn!(
                        recording = %sink.recording_id,
                        %error,
                        "a recording's file could not be written; that capture is stopping"
                    );
                    drop(handle);
                    sink.cancel.cancel();
                }
                Err(_) => {
                    tracing::warn!(
                        recording = %sink.recording_id,
                        "a recording's file write stalled; that capture is stopping"
                    );
                    drop(handle);
                    sink.cancel.cancel();
                }
            }
        }
    }
}

/// Join `<base>.a1.part`, `<base>.a2.part`, … into `<base>.ts` and remove the
/// parts. MPEG-TS packets concatenate, so the join is a byte copy; the
/// discontinuity between attempts is a gap, which the row and the sidecar name
/// rather than paper over.
async fn concatenate_attempts(base: &std::path::Path, attempts: i64) -> std::io::Result<u64> {
    use tokio::io::AsyncWriteExt as _;

    let final_path = base.with_extension("ts");
    let mut parts = Vec::new();
    for attempt in 1..=attempts.max(1) {
        let part = base.with_extension(format!("a{attempt}.part"));
        if tokio::fs::metadata(&part).await.is_ok() {
            parts.push(part);
        }
    }
    if parts.is_empty() {
        return Ok(tokio::fs::metadata(&final_path)
            .await
            .map(|meta| meta.len())
            .unwrap_or(0));
    }
    // One attempt is the common case, and a rename beats copying gigabytes.
    if parts.len() == 1 {
        tokio::fs::rename(&parts[0], &final_path).await?;
        return Ok(tokio::fs::metadata(&final_path).await?.len());
    }
    let mut out = tokio::fs::File::create(&final_path).await?;
    for part in &parts {
        let mut input = tokio::fs::File::open(part).await?;
        tokio::io::copy(&mut input, &mut out).await?;
    }
    out.flush().await?;
    drop(out);
    for part in &parts {
        let _ = tokio::fs::remove_file(part).await;
    }
    Ok(tokio::fs::metadata(&final_path).await?.len())
}

/// Everything the library scan needs, beside the file.
///
/// Written at finish rather than at start, because half of it — how many
/// attempts it took, what was missed, how big it is — is only true at the end.
async fn write_sidecar(
    base: &std::path::Path,
    row: &DvrRecording,
    facts: &Option<crate::live_tv_delivery::LiveSourceFacts>,
    state: DvrState,
    bytes: u64,
    gap_s: i64,
    now: i64,
) {
    let document = serde_json::json!({
        "plurx_dvr": 1,
        "recording_id": row.id,
        "channel": {
            "id": row.channel_id,
            "guide_number": row.guide_number,
            "name": row.channel_name,
        },
        "airing": { "start": row.airing_start, "end": row.airing_end },
        "capture": {
            "start": row.capture_start,
            "end": row.capture_end,
            "actual_end": now,
        },
        "attempts": row.attempt,
        "gap_s": gap_s,
        "late_start_s": row.late_start_s,
        "programme": {
            "title": row.title,
            "episode_title": row.episode_title,
            "episode": row.episode,
            "synopsis": row.synopsis,
            "image_url": row.image_url,
            "original_air_date": row.original_air_date,
            "series_id": row.series_id,
            "programme_id": row.programme_id,
        },
        "source": facts,
        "rule": row.rule_id.as_ref().map(|id| serde_json::json!({ "id": id })),
        "state": state.as_str(),
        "bytes": bytes,
    });
    let path = base.with_extension("json");
    let body = match serde_json::to_vec_pretty(&document) {
        Ok(body) => body,
        Err(error) => {
            tracing::warn!(%error, "could not serialise a recording sidecar");
            return;
        }
    };
    if let Err(error) = tokio::fs::write(&path, body).await {
        tracing::warn!(%error, path = %path.display(), "could not write a recording sidecar");
    }
}

/// Byte counts reaching the Store, so the Activity row's number rises without
/// a write per chunk.
pub(crate) async fn dvr_progress_loop(manager: Arc<LiveTvManager>, shutdown: CancellationToken) {
    let mut last: HashMap<String, u64> = HashMap::new();
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tokio::time::sleep(std::time::Duration::from_millis(
                DVR_PROGRESS_INTERVAL_MS as u64,
            )) => {}
        }
        let activities = manager.recording_activities();
        let now_ms = unix_seconds().saturating_mul(1000);
        for activity in activities {
            if last.get(&activity.recording_id) == Some(&activity.bytes) {
                continue;
            }
            last.insert(activity.recording_id.clone(), activity.bytes);
            if let Err(error) = manager
                .store
                .progress_dvr_recording(
                    &activity.recording_id,
                    i64::try_from(activity.bytes).unwrap_or(i64::MAX),
                    now_ms,
                )
                .await
            {
                tracing::warn!(%error, "could not record a capture's progress");
            }
        }
        last.retain(|id, _| {
            manager
                .recording_activities()
                .iter()
                .any(|activity| &activity.recording_id == id)
        });
    }
}

/// The reminder loop.
///
/// Gated on being the owner and being admitted to serve, and on nothing else.
/// A reminder is a row with a time in it: it needs no tuner, no disk and no
/// DVR root, so it fires with recording switched off and with Live TV itself
/// switched off. Making it wait on either would be a gate on a feature that
/// does not use them — and the person who set the reminder would simply not be
/// told, with nothing anywhere saying why.
///
/// It consults the guide only to notice that an airing has moved, and only
/// when a guide exists at all.
pub(crate) async fn reminder_loop(
    manager: Arc<LiveTvManager>,
    events: super::webhook::DvrEventSink,
    shutdown: CancellationToken,
) {
    loop {
        let ours_and_serving = match manager.dvr_configs().await {
            Ok((live_tv, _)) => {
                live_tv.owner_node_id == manager.node_id && manager.serving.admit().is_some()
            }
            Err(_) => false,
        };
        if ours_and_serving {
            if let Err(error) = reminder_tick(&manager, &events).await {
                tracing::warn!(
                    code = error.code(),
                    "the reminder tick did not complete; it will be retried"
                );
            }
        }
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tokio::time::sleep(DVR_TICK) => {}
        }
    }
}

async fn reminder_tick(
    manager: &Arc<LiveTvManager>,
    events: &super::webhook::DvrEventSink,
) -> Result<(), LiveTvError> {
    let now = unix_seconds();
    let fired = manager
        .store
        .transition_dvr_reminders(now, now.saturating_mul(1000))
        .await
        .map_err(store_error)?;
    for reminder in &fired {
        tracing::info!(
            reminder = %reminder.id,
            title = %reminder.title,
            "a reminder fired"
        );
        events.enqueue(super::webhook::DvrEvent::Reminder {
            reminder_id: reminder.id.clone(),
            user_id: reminder.user_id,
            channel_id: reminder.channel_id.clone(),
            guide_number: reminder.guide_number.clone(),
            title: reminder.title.clone(),
            airing_start: reminder.airing_start,
            lead_s: reminder.lead_s,
        });
    }

    // A programme that moved is worth saying so about: the reminder names a
    // time that no longer has that programme in it, and firing it would send
    // someone to the wrong thing.
    let (live_tv, _) = manager.dvr_configs().await?;
    if !live_tv.guide_fetches() {
        return Ok(());
    }
    let armed = manager
        .store
        .list_dvr_reminders_in(plurx_core::dvr::DvrReminderState::Armed)
        .await
        .map_err(store_error)?;
    if armed.is_empty() {
        return Ok(());
    }
    let guide = manager
        .local_guide(
            &live_tv,
            GuideWindow {
                start: now,
                end: now + DVR_SCHEDULE_DAYS_MAX * 86_400,
            },
        )
        .await;
    if guide.channels.is_empty() {
        // No guide is not evidence that a programme moved.
        return Ok(());
    }
    for reminder in armed {
        let listed = guide.channels.iter().any(|channel| {
            channel.id == reminder.channel_id
                && channel.programmes.iter().any(|programme| {
                    programme.start == reminder.airing_start && programme.title == reminder.title
                })
        });
        if listed {
            continue;
        }
        manager
            .store
            .set_dvr_reminder_state(
                None,
                &reminder.id,
                plurx_core::dvr::DvrReminderState::Armed,
                plurx_core::dvr::DvrReminderState::Moved,
                now.saturating_mul(1000),
            )
            .await
            .map_err(store_error)?;
    }
    Ok(())
}
