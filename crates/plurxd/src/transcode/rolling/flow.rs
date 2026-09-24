use super::*;

/// How far a session has run ahead of the client, both ways it can matter.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(super) struct Ahead {
    pub(super) seconds: i64,
    pub(super) bytes: i64,
}

/// The bounds a session's ahead-window is held to.
#[derive(Debug, Clone, Copy)]
pub(super) struct AheadLimits {
    pub(super) max_secs: i64,
    pub(super) max_bytes: i64,
    pub(super) global_max_bytes: i64,
}

/// Space that a live rolling producer may materialize before the next
/// accounting pass can suspend it.  The configured per-session ceiling is
/// reserved in full, plus one bounded in-flight filesystem envelope for an
/// init object, temporary segment and playlist rewrite.
pub(super) const ROLLING_SCRATCH_IN_FLIGHT_BYTES: i64 = 64 * 1024 * 1024;

/// How long retirement waits for registered scratch writers to prove they can
/// no longer write. A writer that outlives this keeps the conservative
/// producer charge and says so; the timeout is not settlement.
pub(super) const ROLLING_SCRATCH_WRITER_SETTLE: Duration = Duration::from_secs(30);

/// How often the conversion is retried while a measurement keeps failing or a
/// writer keeps running. Bounded work on a detached owner, never on the
/// admission path.
pub(super) const ROLLING_SCRATCH_CONVERSION_RETRY: Duration = Duration::from_secs(5);

/// How many times the conversion is retried before it is left to cleanup.
pub(super) const ROLLING_SCRATCH_CONVERSION_ATTEMPTS: u32 = 24;

/// What a directory measurement is worth to the ledger.
///
/// The scanner preserves the previous charge on a read or stat failure, which
/// is right and also indistinguishable from "measured that much" at the call
/// site. Retirement has to tell the two apart before it collapses a
/// reservation, so the scanner says which one happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ScratchMeasurement {
    /// Every entry was enumerated and stat'd. This is a final inventory.
    Complete(i64),
    /// The directory is gone. Nothing is left to charge for.
    Absent,
    /// Enumeration or stat failed part-way. The previous charge stands.
    Incomplete,
    /// A cache-backed session owns no scratch directory.
    NotScratch,
}

/// Never admit less than this, whatever the sizing arithmetic says: an init
/// object, one temporary segment and a playlist rewrite have to fit before a
/// grant boundary can do anything useful.
pub(super) const ROLLING_SCRATCH_MIN_GRANT_BYTES: i64 = 64 * 1024 * 1024;

/// The startup allowance for a source whose output rate is not known.
///
/// A bootstrap, not a safety proof and not a floor on what the session will
/// need: an unknown-rate 80 Mb/s source has to obtain further grants before
/// it consumes them, and the write boundary is what makes that true.
pub(super) const ROLLING_SCRATCH_UNKNOWN_RATE_BYTES: i64 = 256 * 1024 * 1024;

/// How far ahead of the measured bytes a producing session is authorized.
///
/// This is the `envelope` in `charge = actual + envelope`, re-granted on each
/// flow evaluation. It has to cover everything the producer can materialize
/// between one evaluation and the next taking effect: the paced input, the
/// remaining uncontrolled initial burst, and the writes already issued when a
/// suspension lands.
///
/// For the native copy writer this is a convenience, not the bound — every
/// object passes an exact grant at `copyseg::publish_file`, so an
/// under-estimated envelope makes the writer wait rather than overrun. For
/// direct FFmpeg output there is no such boundary, and the envelope is a
/// measurement-derived allowance rather than an enforced one. That asymmetry
/// is why only the copy path is admitted with a reduced startup reservation.
const ROLLING_SCRATCH_ENVELOPE_SAFETY: f64 = 5.0;

/// How far above a title's *average* bitrate its opening is sized.
///
/// `MediaFile::bitrate` is ffprobe's `format.bit_rate` — the whole container
/// averaged over the whole title, all tracks included. A UHD remux's first
/// reel commonly runs at two to three times that. This is the multiplier on
/// the startup allowance only; steady-state growth is governed by the write
/// boundary, which does not need to guess.
const ROLLING_SCRATCH_STARTUP_PEAK_FACTOR: f64 = 3.0;

/// The evaluation cadence the envelope is sized against.
///
/// Honest about what it is: a *sizing* input, not an enforced deadline. The
/// publication clock refreshes the measurement every
/// [`ROLLING_PUBLICATION_POLL`], and the flow worker re-grants on each client
/// segment fetch and on the [`FLOW_CONTROL_REPAIR_INTERVAL`] repair pass —
/// but neither is a proven maximum gap. The envelope is therefore an
/// allowance derived from measurement, and the thing that actually bounds the
/// copy writer is the exact grant at `copyseg::publish_file`, which no
/// timing assumption can undercut.
const ROLLING_SCRATCH_EVALUATION_INTERVAL: Duration = Duration::from_secs(1);

/// How much future capacity a start admits before it is allowed to make
/// bytes.
///
/// The historical answer was always the whole per-session ceiling, which is
/// why three rolling producers exhausted an 8 GiB budget no matter how little
/// they actually wrote. A writer whose every write passes a grant boundary
/// can start small and grow instead; one whose writes cannot be intercepted
/// still has to reserve the ceiling, because nothing else bounds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RollingScratchSizing {
    /// The configured per-session ceiling plus one in-flight envelope.
    SessionCeiling,
    /// A startup allowance covering every enabled publish gate plus one
    /// complete segment, plus the enforcement envelope. Grows under an
    /// enforced write boundary; never exceeds the ceiling.
    Startup(i64),
}

/// The output bytes a rolling producer can publish before any client can
/// drain it, plus the enforcement envelope.
///
/// The gate is not one number. The copy writer withholds its playlist for
/// `COPY_PUBLISH_GATE_SECS`, and the rolling publication clock refuses the
/// first served playlist until `ROLLING_INITIAL_RUNWAY_MS` — 48 s at 1x, and
/// scaled with the playback rate. Sizing only the writer's own gate misses
/// the larger one, and a session sized below its effective gate can never
/// publish anything for a client to drain.
pub(super) fn rolling_startup_bytes(
    bitrate_bits_per_second: Option<f64>,
    playback_rate: f64,
) -> i64 {
    let gate_ms = rolling_initial_runway_ms(playback_rate)
        .max(i64::from(plurx_core::transcode::COPY_PUBLISH_GATE_SECS).saturating_mul(1_000));
    // One complete segment beyond the gate: the gate is measured in published
    // media, and the segment that crosses it is written in full first.
    let startup_ms = gate_ms.saturating_add(
        i64::from(plurx_core::transcode::ROLLING_PRESENTATION_TARGET_SECS).saturating_mul(1_000),
    );
    let Some(bitrate) = bitrate_bits_per_second.filter(|rate| rate.is_finite() && *rate > 0.0)
    else {
        return ROLLING_SCRATCH_UNKNOWN_RATE_BYTES;
    };
    // The stored figure is the container's *average* over the whole title,
    // every track included. An opening reel routinely runs at two or three
    // times it, and under-sizing the startup allowance is the one case that
    // cannot be recovered by growing — a session that cannot publish a first
    // playlist has no client to drain it and nothing to wait for. Pay for
    // the peak here; the exact write boundary is what stops the cost running
    // away afterwards.
    let media_bytes =
        (bitrate / 8.0) * (startup_ms as f64 / 1_000.0) * ROLLING_SCRATCH_STARTUP_PEAK_FACTOR;
    let envelope = rolling_scratch_envelope(Some(bitrate), playback_rate) as f64;
    let total = media_bytes + envelope;
    if total.is_finite() && total > 0.0 {
        total.min(i64::MAX as f64) as i64
    } else {
        ROLLING_SCRATCH_UNKNOWN_RATE_BYTES
    }
}

/// How far a producer may run past its measured bytes before the next
/// evaluation can hold it.
pub(super) fn rolling_scratch_envelope(
    bitrate_bits_per_second: Option<f64>,
    playback_rate: f64,
) -> i64 {
    let Some(bitrate) = bitrate_bits_per_second.filter(|rate| rate.is_finite() && *rate > 0.0)
    else {
        return ROLLING_SCRATCH_UNKNOWN_RATE_BYTES;
    };
    let seconds = ROLLING_SCRATCH_EVALUATION_INTERVAL.as_secs_f64()
        * playback_rate.clamp(0.25, 4.0)
        * ROLLING_SCRATCH_ENVELOPE_SAFETY;
    let bytes = (bitrate / 8.0) * seconds;
    let bounded = bytes.max(ROLLING_SCRATCH_MIN_GRANT_BYTES as f64);
    if bounded.is_finite() {
        bounded.min(ROLLING_SCRATCH_UNKNOWN_RATE_BYTES as f64) as i64
    } else {
        ROLLING_SCRATCH_UNKNOWN_RATE_BYTES
    }
}

impl RollingScratchSizing {
    fn ceiling(limits: AheadLimits) -> i64 {
        let session_ceiling = if limits.max_bytes > 0 {
            limits.max_bytes
        } else {
            limits.global_max_bytes
        };
        session_ceiling.saturating_add(ROLLING_SCRATCH_IN_FLIGHT_BYTES)
    }

    pub(super) fn grant_bytes(self, limits: AheadLimits) -> i64 {
        let full = Self::ceiling(limits);
        match self {
            Self::SessionCeiling => full,
            Self::Startup(bytes) => bytes.clamp(ROLLING_SCRATCH_MIN_GRANT_BYTES.min(full), full),
        }
    }
}

/// The active bound keeping an ahead-window session held.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AheadHoldReason {
    Demand,
    Time,
    Bytes,
    Global,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AheadHold {
    pub(super) reason: AheadHoldReason,
    pub(super) release_value: i64,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct FlowEvaluation {
    pub(super) hold: Option<AheadHold>,
    pub(super) policy: &'static str,
    pub(super) production_ahead_seconds: Option<i64>,
    pub(super) production_target_seconds: Option<i64>,
}

#[derive(Clone, Copy)]
pub(super) struct FlowInputs<'a> {
    pub(super) physical_ahead: Option<Ahead>,
    pub(super) published_end_ms: Option<i64>,
    /// Completed media not yet present in the actor-authorized served
    /// snapshot. `None` means the first snapshot has not opened yet.
    pub(super) staged_publication_seconds: Option<i64>,
    /// Actor-owned presentation admission. Server publication, downloads, and
    /// positive buffer reports do not spend this protection.
    pub(super) startup_protected: bool,
    pub(super) media_origin_ms: i64,
    pub(super) lease_mode: crate::playback_control::RollingLeaseMode,
    pub(super) demand: Option<&'a crate::playback_control::PlaybackDemandSnapshot>,
    pub(super) global_live_bytes: i64,
    pub(super) global_ahead_bytes: i64,
    pub(super) limits: AheadLimits,
    pub(super) currently_suspended: bool,
    /// `Some(grant)` when this session has materialized everything its ledger
    /// entry authorizes and the budget refused to raise it. The producer stays
    /// held until a re-grant succeeds.
    pub(super) scratch_grant_exhausted: Option<i64>,
}

pub(super) fn rolling_playback_rate(
    demand: Option<&crate::playback_control::PlaybackDemandSnapshot>,
) -> f64 {
    demand
        .filter(|demand| demand.demand == crate::playback_control::PlaybackDemand::Active)
        .map_or(1.0, |demand| demand.playback_rate)
        .clamp(0.25, 4.0)
}

pub(super) fn rolling_publication_batch_ms(rate: f64) -> i64 {
    let base = i64::from(plurx_core::transcode::ROLLING_PRESENTATION_TARGET_SECS) * 1_000;
    ((base as f64) * rate).ceil() as i64
}

pub(super) fn rolling_initial_runway_ms(rate: f64) -> i64 {
    (((ROLLING_INITIAL_RUNWAY_MS as f64) * rate).ceil() as i64)
        .clamp(ROLLING_INITIAL_RUNWAY_MS, ROLLING_RESERVE_MAX_MS)
}

pub(super) fn rolling_insufficient_capacity(rate: f64, recent_speed: Option<f64>) -> bool {
    rate > 0.0 && recent_speed.is_some_and(|speed| speed + 0.05 < rate)
}

#[derive(Debug, Clone, Copy)]
pub(super) struct SuspendedAt {
    pub(super) since: Instant,
    pub(super) hold: AheadHold,
}

pub(super) fn flow_event_extra(
    evaluation: FlowEvaluation,
    lease: &crate::playback_control::RollingLeaseSnapshot,
    ahead: Option<Ahead>,
    global_live_bytes: i64,
    global_ahead_bytes: i64,
    previous_hold_reason: Option<AheadHoldReason>,
) -> String {
    let lease_mode = match lease.mode {
        crate::playback_control::RollingLeaseMode::Legacy => "legacy",
        crate::playback_control::RollingLeaseMode::Explicit => "explicit",
    };
    let lease_state = lease.terminal.map_or("active", |cause| cause.status());
    let demand = lease.demand.as_ref();
    serde_json::json!({
        "lease_mode": lease_mode,
        "lease_state": lease_state,
        "lease_timeout_ms": lease.timeout_ms(),
        "control_demand": demand.map(|demand| demand.demand),
        "startup_state": lease.startup.status(),
        "startup_remaining_ms": lease.startup.remaining.map(|remaining| {
            i64::try_from(remaining.as_millis()).unwrap_or(i64::MAX)
        }),
        "presentation_progress_seen": lease.startup.presentation_progress_seen,
        "producer_control": &lease.producer_control,
        "reported_position_ms": demand.map(|demand| demand.position_ms),
        "budget_anchor_sequence": lease.accepted_demand_sequence,
        "demand_observation_age_ms": lease.demand_observation_age.map(|age| {
            i64::try_from(age.as_millis()).unwrap_or(i64::MAX)
        }),
        "client_runway_ms": demand.map(|demand| demand.runway_ms()),
        "production_policy": evaluation.policy,
        "production_ahead_seconds": evaluation.production_ahead_seconds,
        "production_target_seconds": evaluation.production_target_seconds,
        "physical_ahead_seconds": ahead.map(|ahead| ahead.seconds),
        "physical_ahead_bytes": ahead.map(|ahead| ahead.bytes),
        "global_live_bytes": global_live_bytes,
        "global_ahead_bytes": global_ahead_bytes,
        "previous_hold_reason": previous_hold_reason,
    })
    .to_string()
}

pub(super) fn evaluate_flow(inputs: FlowInputs<'_>) -> FlowEvaluation {
    let FlowInputs {
        physical_ahead,
        published_end_ms,
        staged_publication_seconds,
        media_origin_ms,
        lease_mode,
        demand,
        global_live_bytes,
        global_ahead_bytes,
        limits,
        currently_suspended,
        startup_protected,
        scratch_grant_exhausted,
    } = inputs;
    let publication_target_seconds =
        (rolling_publication_batch_ms(rolling_playback_rate(demand)) + 999) / 1_000;
    if lease_mode == crate::playback_control::RollingLeaseMode::Legacy {
        let hard_hold = scratch_grant_hold(scratch_grant_exhausted).or_else(|| {
            physical_ahead.and_then(|ahead| {
                ahead_hold(
                    ahead,
                    global_live_bytes,
                    global_ahead_bytes,
                    AheadLimits {
                        max_secs: 0,
                        ..limits
                    },
                    currently_suspended,
                )
            })
        });
        return FlowEvaluation {
            hold: hard_hold.or_else(|| {
                staged_publication_seconds
                    .filter(|staged| *staged >= publication_target_seconds)
                    .map(|_| AheadHold {
                        reason: AheadHoldReason::Time,
                        release_value: 0,
                    })
            }),
            policy: "publication_clock_legacy",
            production_ahead_seconds: staged_publication_seconds,
            production_target_seconds: Some(publication_target_seconds),
        };
    }

    let Some(demand) = demand else {
        // Explicit mode is entered by the same actor command that stores the
        // demand snapshot. If that invariant is ever broken, stop production
        // instead of silently returning to inference.
        return FlowEvaluation {
            hold: Some(AheadHold {
                reason: AheadHoldReason::Demand,
                release_value: 0,
            }),
            policy: "explicit_missing_demand",
            production_ahead_seconds: None,
            production_target_seconds: None,
        };
    };

    // Startup is presentation evidence, not an encoder-output threshold. The
    // actor spends this once after two accepted Rendering observations advance
    // the same settled timeline, or retires it at the finite startup deadline.
    let starting = startup_protected;

    // `End` suspends from any state: that client is gone rather than waiting,
    // so there is nothing further production could unblock. `Hold` is the one
    // that cannot be honoured while starting.
    let demand_suspends = match demand.demand {
        crate::playback_control::PlaybackDemand::End => true,
        crate::playback_control::PlaybackDemand::Hold => {
            !starting
                && staged_publication_seconds
                    .is_some_and(|staged| staged >= publication_target_seconds)
        }
        crate::playback_control::PlaybackDemand::Active => false,
    };
    if demand_suspends {
        return FlowEvaluation {
            hold: Some(AheadHold {
                reason: AheadHoldReason::Demand,
                release_value: 0,
            }),
            policy: "explicit_demand",
            production_ahead_seconds: published_end_ms.map(|published_end_ms| {
                media_origin_ms
                    .saturating_add(published_end_ms)
                    .saturating_sub(demand.buffer_anchor_ms())
                    / 1_000
            }),
            production_target_seconds: Some(0),
        };
    }

    let production_ahead_seconds = published_end_ms.map(|published_end_ms| {
        media_origin_ms
            .saturating_add(published_end_ms)
            .saturating_sub(demand.buffer_anchor_ms())
            / 1_000
    });
    // Time pacing belongs exclusively to the publication clock. The physical
    // index still supplies byte accounting and an active-demand diagnostic,
    // but the old ahead-time hysteresis must not compete with a scheduled
    // batch or leave a held producer below the next publication floor.
    // A session inside the copy publish gate has an empty index: segments are
    // on disk, but `index.m3u8` is withheld until the gate opens, and the
    // segment index, the ahead-window suspend and the GC all read the playlist
    // (see `copyseg`). `physical_ahead` is `None` there, so this session's own
    // seconds and bytes are unknowable — and, for the same reason, it
    // contributes nothing to `global_live_bytes` either, so its own gate
    // writes cannot be what trips the fleet cap. What asking anyway buys is
    // the other direction: a box already over its scratch ceiling refuses to
    // start new sessions rather than exempting each one for its first twelve
    // seconds. Ask with this session's contribution zeroed rather than
    // skipping the question.
    let ahead_for_limits = physical_ahead
        .map(|physical_ahead| Ahead {
            seconds: production_ahead_seconds.unwrap_or(physical_ahead.seconds),
            bytes: physical_ahead.bytes,
        })
        .or(starting.then_some(Ahead {
            seconds: 0,
            bytes: 0,
        }));
    let hard_hold = scratch_grant_hold(scratch_grant_exhausted).or_else(|| {
        ahead_for_limits.and_then(|ahead| {
            ahead_hold(
                ahead,
                global_live_bytes,
                global_ahead_bytes,
                AheadLimits {
                    max_secs: 0,
                    ..limits
                },
                currently_suspended,
            )
        })
    });
    let hold = hard_hold.or_else(|| {
        (!starting)
            .then_some(staged_publication_seconds)
            .flatten()
            .filter(|staged| *staged >= publication_target_seconds)
            .map(|_| AheadHold {
                reason: AheadHoldReason::Time,
                release_value: 0,
            })
    });
    FlowEvaluation {
        hold,
        policy: "publication_clock_explicit",
        production_ahead_seconds: staged_publication_seconds.or(production_ahead_seconds),
        production_target_seconds: (!starting).then_some(publication_target_seconds),
    }
}

/// Whether a session should be held, given how far ahead it is, how much
/// scratch every session is using between them, and whether it is already
/// held.
///
/// Byte budgets release at half because they are hard disk bounds. Media-time
/// pacing is intentionally absent: [`evaluate_flow`] derives it from staged
/// inventory and the immutable publication clock.
/// A producer that has materialized everything its ledger entry authorizes
/// must stop until the budget re-grants.
///
/// Deliberately **not** folded into [`ahead_hold`]: that one is reached
/// through `physical_ahead.and_then(...)`, and `physical_ahead` is `None`
/// for exactly the session this is about — a copy producer inside its
/// publish gate has an empty segment index. A capacity hold that evaporates
/// whenever there is nothing published yet is no hold at all.
fn scratch_grant_hold(scratch_grant: Option<i64>) -> Option<AheadHold> {
    scratch_grant.map(|release_value| AheadHold {
        reason: AheadHoldReason::Global,
        release_value,
    })
}

pub(super) fn ahead_hold(
    ahead: Ahead,
    global_live_bytes: i64,
    global_ahead_bytes: i64,
    limits: AheadLimits,
    currently_suspended: bool,
) -> Option<AheadHold> {
    let half = |limit: i64| limit / 2;
    let byte_limit = |limit: i64| {
        if currently_suspended {
            half(limit)
        } else {
            limit
        }
    };
    let over = |value: i64, limit: i64| limit > 0 && value > limit;

    // Capacity holds take precedence in telemetry because they cannot be
    // cleared merely by crossing the media-time release point.
    let global_limit = byte_limit(limits.global_max_bytes);
    // Enter on total scratch so the configured cap remains a real disk
    // bound. Release on the drainable reserve: retained bytes behind every
    // client's frontier cannot be pruned inside RETENTION_SECS, so using
    // total scratch for the half-cap release line can make that line
    // structurally unreachable even after every client has caught up.
    let global_value = if currently_suspended {
        global_ahead_bytes
    } else {
        global_live_bytes
    };
    if over(global_value, global_limit) {
        return Some(AheadHold {
            reason: AheadHoldReason::Global,
            release_value: global_limit,
        });
    }
    let session_byte_limit = byte_limit(limits.max_bytes);
    if over(ahead.bytes, session_byte_limit) {
        return Some(AheadHold {
            reason: AheadHoldReason::Bytes,
            release_value: session_byte_limit,
        });
    }
    None
}

#[cfg(test)]
pub(super) fn should_suspend(
    ahead: Ahead,
    global_live_bytes: i64,
    global_ahead_bytes: i64,
    limits: AheadLimits,
    currently_suspended: bool,
) -> bool {
    ahead_hold(
        ahead,
        global_live_bytes,
        global_ahead_bytes,
        limits,
        currently_suspended,
    )
    .is_some()
}
