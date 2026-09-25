//! What the Developer settings section needs to say out loud.
//!
//! The Developer tab already lists, for each switch that costs something, what
//! must be true before turning it on. Those lists were static prose: an
//! operator read "every node must advertise its private cluster API address"
//! and then had no way to find out whether their nodes do. This route answers
//! the second half — for every prerequisite, what this server can currently
//! observe about it.
//!
//! Three rules shape it, and each one is a decision worth keeping:
//!
//! 1. **It is advisory. It gates nothing.** No enable path reads this route,
//!    and `an_unmet_prerequisite_does_not_block_the_switch` exists to keep
//!    that true. A prerequisite the server cannot see is not a reason to
//!    refuse an operator who can see it — they have the deployment in front of
//!    them and this process does not.
//! 2. **`Unobservable` is a real answer, and it is the common one.** A fleet
//!    qualification receipt lives in a CI artifact the daemon has never held.
//!    A single-node install has no committed roster. A counter that can see
//!    the clients which *did* report cannot speak for the ones which did not.
//!    Every one of those is `Unobservable`, because a green tick meaning "not
//!    checked" is the failure mode this route exists to replace, and answering
//!    `Met` from a fail-open default would be that tick with extra steps.
//! 3. **Evidence is what was read, and it names what was not read.** A row
//!    saying "budgets are configured" teaches nothing; a row saying "chunk
//!    30 s, transfer 1,200 s, install 120 s, and nothing here knows your state
//!    size" lets the operator decide.
//!
//! Rule 2 is why few rows can ever be `Met`. That is the honest shape of these
//! questions, not a gap to be closed by relaxing a threshold.

use axum::{extract::State, Json};
use plurx_core::cluster::membership::MembershipError;
use plurx_core::domain::{PlaybackEvent, PlaybackEventQuery};
use serde::Serialize;

use super::error::ApiError;
use super::extract::AdminUser;
use crate::state::AppState;

/// What the server can see of this prerequisite right now.
///
/// Deliberately three values. Two would force every fact the daemon cannot
/// reach into either a false `Met` or a misleading `Unmet`, and both of those
/// are worse than saying so.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RequirementStatus {
    /// Read from this process, and what was read satisfies the requirement.
    Met,
    /// Read from this process, and what was read contradicts the requirement.
    Unmet,
    /// Not answerable from a running daemon, or answerable only in part.
    /// `evidence` says what was read, what was not, and where the rest lives.
    Unobservable,
}

#[derive(Debug, Serialize)]
pub(crate) struct DeveloperRequirement {
    /// Stable across builds so the web rows and an operator's notes can name
    /// one question. Never reused for a different one.
    pub id: &'static str,
    pub title: &'static str,
    pub status: RequirementStatus,
    /// What this process actually read. One sentence, always populated.
    pub evidence: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct DeveloperEnableItem {
    pub id: &'static str,
    pub title: &'static str,
    /// The switch's current position, when the item has one. `None` means the
    /// capability has no switch — it is compiled in and behaves by itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// The `PUT /api/v1/settings` field this row's switch is, when it has one.
    ///
    /// The settings *API field*, deliberately, not the store key. They differ,
    /// and only one of them is something a caller can act on: the update
    /// request is a struct of named optional fields and ignores anything it
    /// does not recognise, so a caller handed a store key writes nothing and
    /// is told 200. The first version of this field carried the store key, and
    /// the test that walks every switch to prove no prerequisite refuses one
    /// passed by writing nothing at all.
    ///
    /// Named at all — rather than implied by `id` — because the property this
    /// whole route has to keep can only be tested by a caller that can
    /// enumerate the switches. A test that hard-codes the list tests the
    /// switch somebody remembered, and a gate is cheapest to add on the one
    /// they did not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setting: Option<&'static str>,
    pub requirements: Vec<DeveloperRequirement>,
}

#[derive(Debug, Serialize)]
pub(crate) struct DeveloperReadiness {
    /// Nothing here is cached between requests. Most rows are read when the
    /// request arrives; three of them (`recovery_receipt`, `fleet_receipt`,
    /// `server_preparation_is_real`) are statements about an artifact or about
    /// this build rather than about the deployment, and say so in evidence.
    pub items: Vec<DeveloperEnableItem>,
}

/// `GET /api/v1/developer/readiness` — admin, read-only, advisory.
pub(crate) async fn readiness(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<DeveloperReadiness>, ApiError> {
    // Propagated, not defaulted. An empty map from a failed replicated read
    // would render as "the switch is off", which is a reading this route did
    // not take — the same dishonesty it exists to remove, one layer down.
    let settings = state.store.settings_snapshot().await?;
    let live_tv = crate::live_tv::LiveTvConfig::from_snapshot(&settings, &state.node_id);
    let dvr_config = crate::live_tv::DvrConfig::from_snapshot(&settings);
    let dvr_on = dvr_config.enabled;
    let library_channels_on = plurx_core::store::stored_switch(
        settings
            .get(plurx_core::store::keys::LIBRARY_CHANNELS_ENABLED)
            .map(String::as_str),
        false,
    );
    let control_advertised = plurx_core::store::stored_switch(
        settings
            .get(plurx_core::store::keys::PLAYBACK_CONTROL_PROTOCOL_V1)
            .map(String::as_str),
        true,
    );
    let prepared_handoff_on = plurx_core::store::stored_switch(
        settings
            .get(plurx_core::store::keys::PREPARED_QUALITY_HANDOFF)
            .map(String::as_str),
        true,
    );
    let content_analysis_on = plurx_core::store::stored_switch(
        settings
            .get(plurx_core::store::keys::VOD_INDEX_CLUSTER_CACHE)
            .map(String::as_str),
        true,
    );

    // Parsed exactly the way the engine parses it. That was the point when all
    // three sites compared the raw value — a row that trimmed on its own would
    // report the switch off while the engine kept falling back — and it is
    // still the point now that all of them, takeover included, share
    // `stored_switch`.
    let live_recovery_on = plurx_core::store::stored_switch(
        settings
            .get(plurx_core::store::keys::VOD_LIVE_RECOVERY)
            .map(String::as_str),
        true,
    );

    let overlay_on = plurx_core::store::stored_switch(
        settings
            .get(plurx_core::store::keys::PGS_OVERLAY)
            .map(String::as_str),
        false,
    );
    // Absent is on: keeping and reading PGS tracks is the point of the store.
    // One switch stops the producer and both consumers.
    let stored_sources_on = plurx_core::store::stored_switch(
        settings
            .get(plurx_core::store::keys::SUBTITLE_STORED_SOURCES)
            .map(String::as_str),
        true,
    );
    let cluster_sources_on = plurx_core::store::stored_switch(
        settings
            .get(plurx_core::store::keys::SUBTITLE_CLUSTER_SOURCES)
            .map(String::as_str),
        false,
    );
    let subtitle_backfill_on = plurx_core::store::stored_switch(
        settings
            .get(plurx_core::store::keys::SUBTITLE_BACKFILL)
            .map(String::as_str),
        false,
    );

    // Absent is on, unlike every other switch here, because a Profile 7 title
    // reaching a Dolby Vision client as HDR10 is what the conversion exists to
    // stop.
    let convert_on = plurx_core::store::stored_switch(
        settings
            .get(plurx_core::store::keys::DV_CONVERT)
            .map(String::as_str),
        true,
    );
    let display_mode_match_on = plurx_core::store::stored_switch(
        settings
            .get(plurx_core::store::keys::PLAYBACK_DISPLAY_MODE_MATCH)
            .map(String::as_str),
        false,
    );
    let display_mode_events = state
        .store
        .playback_events(&PlaybackEventQuery {
            since_ms: Some(
                crate::media_sessions::unix_ms().saturating_sub(7 * 24 * 60 * 60 * 1_000),
            ),
            event: Some("playback_display_mode".into()),
            limit: 2_000,
        })
        .await?;

    Ok(Json(DeveloperReadiness {
        items: vec![
            cluster_backup(
                &state,
                settings.get(plurx_core::store::keys::BACKUP_DESTINATION),
            ),
            windows_server(&state, convert_on),
            android_display_mode_match(display_mode_match_on, &display_mode_events),
            library_channels(&state, library_channels_on).await,
            channel_subjects(plurx_core::store::stored_switch(
                settings
                    .get(crate::channel_subjects::ENABLE_KEY)
                    .map(String::as_str),
                true,
            )),
            semantic_search(
                settings
                    .get(crate::library_search::SEMANTIC_KEY)
                    .is_some_and(|v| v == "true"),
            ),
            dvr(&state, dvr_on, &live_tv, &dvr_config).await,
            cluster_transport_recovery(&state).await,
            playback_control_protocol(control_advertised),
            prepared_quality_handoff(prepared_handoff_on),
            content_analysis_repair(&state, content_analysis_on).await,
            live_hls_recovery(live_recovery_on),
            pgs_overlay(overlay_on),
            subtitle_stored_sources(stored_sources_on, &state.runtime_cache_dir),
            subtitle_cluster_sources(
                &state,
                cluster_sources_on,
                state.jobs.analysis_queue_enabled().await,
            )
            .await,
            subtitle_backfill(&state, subtitle_backfill_on).await,
            subtitle_not_ready_503(plurx_core::store::stored_switch(
                settings
                    .get(plurx_core::store::keys::SUBTITLE_NOT_READY_503)
                    .map(String::as_str),
                false,
            )),
            chapter_thumbnails(&state).await,
            dolby_vision_convert(convert_on),
            source_probe_comparison().await,
        ],
    }))
}

/// Readiness is a report about this node and the peers it can currently see.
/// Saving either switch goes through Settings without consulting these rows.
async fn subtitle_cluster_sources(
    state: &AppState,
    enabled: bool,
    queue_enabled: bool,
) -> DeveloperEnableItem {
    use crate::subtitle_ride_along::{free_space, local_filesystem};

    let root = crate::subtitle_source::store_root(&state.runtime_cache_dir);
    let checked = if root.is_dir() {
        root
    } else {
        state.runtime_cache_dir.clone()
    };
    let peers = state.membership.media_peers().await;
    let (peer_status, peer_evidence) = match peers {
        Ok(peers) if peers.iter().any(|peer| peer.reachable) => (
            RequirementStatus::Met,
            format!(
                "{} reachable media peer(s) currently advertise capacity.",
                peers.iter().filter(|peer| peer.reachable).count()
            ),
        ),
        Ok(peers) => (
            RequirementStatus::Unmet,
            format!(
                "{} media peer(s) are known, but none currently advertises reachable capacity.",
                peers.len()
            ),
        ),
        Err(error) => (
            RequirementStatus::Unobservable,
            format!("The media peer directory could not be read: {error}"),
        ),
    };
    let (filesystem_status, filesystem_evidence) = match local_filesystem(&checked) {
        Ok(kind) => (
            RequirementStatus::Met,
            format!("{} is on a local filesystem ({kind}).", checked.display()),
        ),
        Err(error) => (
            RequirementStatus::Unmet,
            format!("{}: {error}", checked.display()),
        ),
    };
    let (space_status, space_evidence) = match free_space(&checked) {
        Ok(space) => (
            RequirementStatus::Met,
            format!("{}: {space}.", checked.display()),
        ),
        Err(error) => (
            RequirementStatus::Unmet,
            format!("{}: {error}", checked.display()),
        ),
    };
    DeveloperEnableItem {
        id: "subtitle_cluster_sources",
        title: "Share stored subtitle tracks across the cluster",
        enabled: Some(enabled),
        setting: Some("subtitle_cluster_sources"),
        requirements: vec![
            DeveloperRequirement {
                id: "analysis_queue",
                title: "Cluster analysis queue is enabled",
                status: if queue_enabled { RequirementStatus::Met } else { RequirementStatus::Unmet },
                evidence: format!("The cluster analysis scheduler currently reports {}.", if queue_enabled { "available" } else { "unavailable" }),
            },
            DeveloperRequirement {
                id: "schema_v47",
                title: "Every voter runs schema 47",
                status: RequirementStatus::Unobservable,
                evidence: "This binary supports schema 47. The daemon has no per-voter schema-version reading; inspect the committed voter fleet before enabling.".to_owned(),
            },
            DeveloperRequirement { id: "reachable_peer", title: "A reachable media peer", status: peer_status, evidence: peer_evidence },
            DeveloperRequirement { id: "local_cache", title: "A local subtitle store", status: filesystem_status, evidence: filesystem_evidence },
            DeveloperRequirement { id: "free_space", title: "Space for stored tracks", status: space_status, evidence: space_evidence },
        ],
    }
}

async fn subtitle_backfill(state: &AppState, enabled: bool) -> DeveloperEnableItem {
    let observed = state
        .subtitle_backfill_status()
        .await
        .map_err(|error| error.to_string());
    subtitle_backfill_item(enabled, observed)
}

fn subtitle_backfill_item(
    enabled: bool,
    observed: Result<crate::state::SubtitleBackfillStatus, String>,
) -> DeveloperEnableItem {
    let (lease, enqueued, remaining, bytes) = match observed {
        Ok(status) => {
            let holder = status
                .lease_holder
                .as_deref()
                .unwrap_or("none between passes");
            (
                DeveloperRequirement {
                    id: "backfill_lease",
                    title: "Exclusive backfill lease",
                    status: if status.lease_holder.is_some() {
                        RequirementStatus::Met
                    } else {
                        RequirementStatus::Unobservable
                    },
                    evidence: format!("Current media:subtitle-source:backfill lease holder: {holder}."),
                },
                DeveloperRequirement {
                    id: "backfill_enqueued",
                    title: "Files enqueued by this process",
                    status: RequirementStatus::Met,
                    evidence: format!("{} files enqueued since this process started.", status.enqueued_process),
                },
                DeveloperRequirement {
                    id: "backfill_remaining",
                    title: "Eligible files remaining",
                    status: RequirementStatus::Met,
                    evidence: format!("{} files still have an eligible subtitle ordinal without settling coverage.", status.remaining_files),
                },
                DeveloperRequirement {
                    id: "backfill_bytes",
                    title: "Estimated bytes remaining",
                    status: RequirementStatus::Met,
                    evidence: format!("{} source bytes across the eligible files; this is an estimate of source reading, not output size.", status.remaining_bytes),
                },
            )
        }
        Err(error) => {
            let unavailable = |id: &'static str, title: &'static str| DeveloperRequirement {
                id,
                title,
                status: RequirementStatus::Unobservable,
                evidence: format!("The backfill diagnostics read failed: {error}."),
            };
            (
                unavailable("backfill_lease", "Exclusive backfill lease"),
                unavailable("backfill_enqueued", "Files enqueued by this process"),
                unavailable("backfill_remaining", "Eligible files remaining"),
                unavailable("backfill_bytes", "Estimated bytes remaining"),
            )
        }
    };
    DeveloperEnableItem {
        id: "subtitle_backfill",
        title: "Backfill uncovered subtitle tracks while idle",
        enabled: Some(enabled),
        setting: Some("subtitle_backfill"),
        requirements: vec![lease, enqueued, remaining, bytes],
    }
}

fn cluster_backup(state: &AppState, destination: Option<&String>) -> DeveloperEnableItem {
    let destination = destination.map(String::as_str).unwrap_or("").trim();
    let configured = !destination.is_empty();
    let path = std::path::Path::new(destination);
    let exists = configured && path.is_absolute() && path.is_dir();
    let last = state.backup.metrics().last_success_seconds();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let recent = last > 0 && now.saturating_sub(last) < 26 * 60 * 60;
    DeveloperEnableItem {
        id: "cluster_backup",
        title: "Portable cluster backup",
        enabled: Some(configured),
        setting: None,
        requirements: vec![
            DeveloperRequirement {
                id: "destination",
                title: "Existing absolute destination",
                status: if exists {
                    RequirementStatus::Met
                } else {
                    RequirementStatus::Unmet
                },
                evidence: if configured {
                    format!("Configured destination: {destination}. This node observes it as {}.", if exists { "an existing directory" } else { "missing, relative, or not a directory" })
                } else {
                    "No destination is configured, so the scheduled job does not run.".to_owned()
                },
            },
            DeveloperRequirement {
                id: "off_node_and_space",
                title: "Off-node storage and two-image free space",
                status: RequirementStatus::Unobservable,
                evidence: "The settings page cannot prove the destination's failure domain or future free space. Put it on a separately protected mount and keep at least twice the current state-machine image size free.".to_owned(),
            },
            DeveloperRequirement {
                id: "recent_success",
                title: "Successful artefact in the last 26 hours",
                status: if recent {
                    RequirementStatus::Met
                } else if last > 0 {
                    RequirementStatus::Unmet
                } else {
                    RequirementStatus::Unobservable
                },
                evidence: if last > 0 {
                    format!("This process last completed a portable backup {} seconds ago.", now.saturating_sub(last))
                } else {
                    "This process has not completed a portable backup since it started; durable fleet history must be checked at the destination.".to_owned()
                },
            },
        ],
    }
}

fn android_display_mode_match(enabled: bool, events: &[PlaybackEvent]) -> DeveloperEnableItem {
    let matched = events.iter().any(|event| {
        event
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("outcome=matched"))
    });
    let latest = events.first().and_then(|event| event.detail.as_deref());
    DeveloperEnableItem {
        id: "android_display_mode_match",
        title: "Android TV display-mode matching",
        enabled: Some(enabled),
        setting: Some("playback_display_mode_match"),
        requirements: vec![DeveloperRequirement {
            id: "recent_matched_switch",
            title: "A television reported a matched switch on this node in the last 7 days",
            status: if matched {
                RequirementStatus::Met
            } else {
                RequirementStatus::Unobservable
            },
            evidence: latest.map_or_else(
                || "No display-mode telemetry is retained for this node in the last 7 days. This is advisory; the enable switch remains available.".into(),
                |detail| format!("Latest bounded playback_display_mode detail: {detail}. This observation is advisory and never blocks enablement."),
            ),
        }],
    }
}

async fn content_analysis_repair(state: &AppState, enabled: bool) -> DeveloperEnableItem {
    let tools_ready =
        state.system.ffmpeg_version.is_some() && state.system.ffprobe_build_digest.is_some();
    let queue = state.store.analysis_status_summary().await;
    let queue_status = if queue.is_ok() {
        RequirementStatus::Met
    } else {
        RequirementStatus::Unmet
    };
    let queue_evidence = match queue {
        Ok(summary) => format!(
            "The durable analysis store responded: {} running, {} queued, {} recent failures.",
            summary.running, summary.queued, summary.attention
        ),
        Err(error) => format!("The durable analysis store could not be read: {error}"),
    };
    DeveloperEnableItem {
        id: "content_analysis_repair",
        title: "Enable durable selected-video analysis",
        enabled: Some(enabled),
        setting: Some("vod_index_cluster_cache"),
        requirements: vec![
            DeveloperRequirement {
                id: "media_tools",
                title: "Measured FFmpeg and FFprobe",
                status: if tools_ready {
                    RequirementStatus::Met
                } else {
                    RequirementStatus::Unmet
                },
                evidence: if tools_ready {
                    "This node measured both media executables at startup; selected-stream probes and index passes can use the recorded binaries.".to_owned()
                } else {
                    "This node did not record both an FFmpeg version and an FFprobe build digest at startup.".to_owned()
                },
            },
            DeveloperRequirement {
                id: "durable_queue",
                title: "Durable analysis authority",
                status: queue_status,
                evidence: queue_evidence,
            },
            DeveloperRequirement {
                id: "source_fencing",
                title: "Held-source fencing and bounded diagnostics",
                status: RequirementStatus::Met,
                evidence: "This build binds the metadata probe and index pass to one held source, stores typed diagnostics with the fenced transition, and uses a fixed retry deadline.".to_owned(),
            },
            DeveloperRequirement {
                id: "compatibility_inventory",
                title: "Successful-file timing sample",
                status: RequirementStatus::Unobservable,
                evidence: "The bounded successful-index timing inventory is an operator rollout receipt; the daemon does not receive that artifact. Enabling remains available without it.".to_owned(),
            },
        ],
    }
}

/// Advisory, and deliberately switchless: there is nothing here to turn on.
/// It answers one question an operator otherwise has to read a metrics
/// endpoint for — is this node checking held sources against the whole stored
/// scan, or against the narrower set of media facts — and says what would
/// restore the stricter one.
async fn source_probe_comparison() -> DeveloperEnableItem {
    let reporter =
        plurx_core::scan::probe::known_reporter_identity_of(&crate::ffmpeg::ffprobe_bin()).await;
    let named = match reporter.flatten() {
        Some(build) => DeveloperRequirement {
            id: "probe_reporter_named",
            title: "This node can name the FFprobe build it probes with",
            status: RequirementStatus::Met,
            evidence: format!(
                "Held sources are probed with {build}, and every scan and held-source probe \
                 written here is stamped with that name so a later comparison can tell an \
                 unchanged source from a changed reporter."
            ),
        },
        None if reporter.is_some() => DeveloperRequirement {
            id: "probe_reporter_named",
            title: "This node can name the FFprobe build it probes with",
            status: RequirementStatus::Unmet,
            evidence: "`ffprobe -version` did not answer, so documents written here carry no \
                       reporter name. Every held source is then compared against the whole \
                       stored scan, which refuses whenever that scan came from a different \
                       FFprobe build."
                .to_owned(),
        },
        // Read, never probed for: an advisory readout does not spawn a
        // subprocess to report what this node has been doing.
        None => DeveloperRequirement {
            id: "probe_reporter_named",
            title: "This node can name the FFprobe build it probes with",
            status: RequirementStatus::Unobservable,
            evidence: "Nothing has been scanned or played on this node since it started, so \
                       the FFprobe build it would stamp its documents with has not been read \
                       yet. The first scan or playback start reads it."
                .to_owned(),
        },
    };
    let admissions = crate::ffmpeg::reporter_drift_admissions();
    let whole_document = if admissions == 0 {
        DeveloperRequirement {
            id: "sources_match_their_scan_whole",
            title: "Held sources match their stored scan whole",
            status: RequirementStatus::Met,
            evidence: "No source has been admitted on its media facts since this process \
                       started: every held source this node served matched its stored scan \
                       field for field."
                .to_owned(),
        }
    } else {
        DeveloperRequirement {
            id: "sources_match_their_scan_whole",
            title: "Held sources match their stored scan whole",
            status: RequirementStatus::Unmet,
            evidence: format!(
                "{admissions} held sources were admitted on their media facts rather than on \
                 the whole stored document, because their scan came from a different FFprobe \
                 build. Geometry, cadence, codec identity, colour, Dolby Vision side data, the \
                 audio shape and chapter timing were all compared; the reporter's own schema \
                 was not. Reanalyzing an item rewrites its scan with this build and restores \
                 the stricter comparison for it."
            ),
        }
    };
    DeveloperEnableItem {
        id: "source_probe_comparison",
        title: "Held-source probe comparison",
        enabled: None,
        setting: None,
        requirements: vec![named, whole_document],
    }
}

fn windows_server(state: &AppState, enabled: bool) -> DeveloperEnableItem {
    #[cfg(windows)]
    let platform = DeveloperRequirement {
        id: "native_runtime",
        title: "Native Windows runtime",
        status: RequirementStatus::Met,
        evidence: "This process is the native x86_64-pc-windows-msvc server. Startup accepted its managed NTFS/ReFS storage roots and installed Windows process controls.".to_owned(),
    };
    #[cfg(not(windows))]
    let platform = DeveloperRequirement {
        id: "native_runtime",
        title: "Native Windows runtime",
        status: RequirementStatus::Unobservable,
        evidence: format!(
            "This node runs {}; Windows filesystem, Job Object, service-control, and long-path behavior must be read from a Windows node.",
            std::env::consts::OS
        ),
    };

    let runtime_status = if cfg!(windows) {
        if state.system.ffmpeg_version.is_some() {
            RequirementStatus::Met
        } else {
            RequirementStatus::Unmet
        }
    } else {
        RequirementStatus::Unobservable
    };
    let hardware_status = |available| {
        if !cfg!(windows) {
            RequirementStatus::Unobservable
        } else if available {
            RequirementStatus::Met
        } else {
            RequirementStatus::Unmet
        }
    };
    DeveloperEnableItem {
        id: "windows_server",
        title: "Windows server",
        enabled: Some(enabled),
        setting: Some("dolby_vision_convert"),
        requirements: vec![
            platform,
            DeveloperRequirement {
                id: "ffmpeg_runtime",
                title: "Pinned FFmpeg runtime",
                status: runtime_status,
                evidence: if cfg!(windows) {
                    state.system.ffmpeg_version.clone().map_or_else(
                        || format!("{} did not answer the startup version probe; software transcode is unavailable.", state.system.ffmpeg),
                        |version| format!("{} answered the startup probe: {version}", state.system.ffmpeg),
                    )
                } else {
                    "Only a Windows node can prove the packaged jellyfin-ffmpeg runtime.".to_owned()
                },
            },
            DeveloperRequirement {
                id: "nvenc",
                title: "NVIDIA NVENC",
                status: hardware_status(state.system.encoders.nvenc),
                evidence: if state.system.encoders.nvenc {
                    "The startup encoder and forced-IDR probes admitted NVENC on this node."
                        .to_owned()
                } else {
                    "NVENC was not admitted by this node's startup probes; software remains available.".to_owned()
                },
            },
            DeveloperRequirement {
                id: "qsv",
                title: "Intel Quick Sync",
                status: hardware_status(state.system.encoders.qsv),
                evidence: if state.system.encoders.qsv {
                    "The startup encoder and forced-IDR probes admitted Quick Sync on this node."
                        .to_owned()
                } else {
                    "Quick Sync was not admitted by this node's startup probes; software remains available.".to_owned()
                },
            },
        ],
    }
}

/// Scheduled playback made entirely from the already-probed local catalogue.
/// These are diagnostic facts, never admission predicates: the settings write
/// does not call this function and therefore cannot accidentally grow a gate.
async fn library_channels(state: &AppState, enabled: bool) -> DeveloperEnableItem {
    let catalogue = state.store.library_channel_catalog_snapshot(1).await;
    let (store_status, store_evidence, media_status, media_evidence) = match catalogue {
        Ok(page) if page.is_empty() => (
            RequirementStatus::Met,
            "The authoritative Store accepted a bounded Library-channel catalogue read."
                .to_owned(),
            RequirementStatus::Unmet,
            "No eligible probed movie or episode with positive video duration is currently in the catalogue. Definitions and empty drafts still work."
                .to_owned(),
        ),
        Ok(_) => (
            RequirementStatus::Met,
            "The authoritative Store accepted a bounded Library-channel catalogue read."
                .to_owned(),
            RequirementStatus::Met,
            "At least one eligible probed movie or episode with a video stream and positive duration is available."
                .to_owned(),
        ),
        Err(error) => (
            RequirementStatus::Unmet,
            format!("The authoritative Library-channel catalogue read failed: {error}."),
            RequirementStatus::Unobservable,
            "Media eligibility cannot be observed until the authoritative Store read succeeds."
                .to_owned(),
        ),
    };
    DeveloperEnableItem {
        id: "library_channels",
        title: "Enable Library channels",
        enabled: Some(enabled),
        setting: Some("library_channels_enabled"),
        requirements: vec![
            DeveloperRequirement {
                id: "authoritative_store",
                title: "The authoritative channel Store is readable",
                status: store_status,
                evidence: store_evidence,
            },
            DeveloperRequirement {
                id: "eligible_video",
                title: "At least one schedulable video exists",
                status: media_status,
                evidence: media_evidence,
            },
            DeveloperRequirement {
                id: "compatible_clients",
                title: "Deployed clients understand following playback",
                status: RequirementStatus::Unobservable,
                evidence: "This server implements the dedicated Library-channel session contract, but it cannot prove that every installed web, Apple, Android, or TV client has been upgraded. Older clients retain ordinary VOD and Live TV."
                    .to_owned(),
            },
        ],
    }
}

/// Profile 7 titles converted to 8.1 rather than delivered as HDR10.
///
/// The odd one in this section: it is on by default and costs work only when a
/// Profile 7 title is actually played. It is here because it was
/// `PLURX_DV_CONVERT`, an environment variable that could turn off encode work
/// this node does on somebody's GPU, with nothing in the product to say it had
/// been turned off or why a Dolby Vision client had started seeing HDR10.
fn dolby_vision_convert(enabled: bool) -> DeveloperEnableItem {
    DeveloperEnableItem {
        id: "dolby_vision_convert",
        title: "Convert Dolby Vision Profile 7 to 8.1",
        enabled: Some(enabled),
        setting: Some("dolby_vision_convert"),
        // No prerequisites, and that is the honest answer rather than a gap.
        // The conversion is plurx's own code and runs wherever this binary
        // runs; there is nothing to measure and nothing that has to be true
        // first. Inventing a row to say so would have put this section's only
        // green tick on the one switch that never earned a reading, which is
        // exactly the "checked" tick this route exists to remove.
        requirements: Vec::new(),
    }
}

/// Image subtitles served as an overlay rather than hidden or burned in.
///
/// This was `PLURX_PGS_OVERLAY`, a boot-time environment read that decided
/// which subtitle tracks a client is offered. It is here because that decision
/// is a product question — a viewer with a PGS-only subtitle track either sees
/// it or does not — and an operator could neither see the answer nor change it
/// without redeploying.
fn pgs_overlay(enabled: bool) -> DeveloperEnableItem {
    let (served, refused) = crate::http::pgs_overlay::overlay_demand_snapshot();

    // Reachable both ways, and neither direction can be read upward into "the
    // fleet renders overlays". One manifest served proves one client asked and
    // was answered; it does not prove the client drew anything, which is what
    // the acceptance row below is for and which no counter can see.
    let clients = if served > 0 {
        DeveloperRequirement {
            id: "clients_render_overlays",
            title: "A client in the fleet renders pgs-v1",
            status: RequirementStatus::Unobservable,
            evidence: format!(
                "{served} overlay manifest(s) have been served since this process started \
                 (published ones only; a poll while one is still being prepared is not counted), \
                 so at least one client asked for one and got it. Whether it drew them is not \
                 something this server can see. These counters are process-local and a restart \
                 returns them to zero."
            ),
        }
    } else if refused > 0 {
        DeveloperRequirement {
            id: "clients_render_overlays",
            title: "A client in the fleet renders pgs-v1",
            status: RequirementStatus::Unobservable,
            evidence: format!(
                "{refused} request(s) for a real PGS track since this process started were refused \
                 because this switch is off, so a client here is asking for the capability. That \
                 is a reason to consider turning it on, not evidence that the client renders it."
            ),
        }
    } else {
        DeveloperRequirement {
            id: "clients_render_overlays",
            title: "A client in the fleet renders pgs-v1",
            status: RequirementStatus::Unobservable,
            evidence: "No client has asked this process for an overlay manifest since it \
                       started \u{2014} which is also what a fleet with no PGS subtitles looks \
                       like. These counters are process-local and a restart returns them to zero."
                .to_owned(),
        }
    };

    DeveloperEnableItem {
        id: "pgs_overlay",
        title: "Serve PGS subtitles as an overlay",
        enabled: Some(enabled),
        setting: Some("pgs_overlay"),
        requirements: vec![
            clients,
            DeveloperRequirement {
                id: "overlay_acceptance",
                title: "Physical-client acceptance is complete",
                status: RequirementStatus::Unobservable,
                evidence: "Whether image subtitles land in the right place, at the right size, at \
                           the right moment is judged on a screen. The daemon never receives that \
                           receipt, and a green unit suite is not it."
                    .to_owned(),
            },
        ],
    }
}

/// Stored PGS tracks: kept by the fragment-index pass, read in place of a
/// whole-source extraction.
///
/// `docs/clients/PGS-SUBTITLE-START-PATH-RCA-AND-PLAN.md` §6. One switch for
/// both halves: off stops the index pass keeping tracks and makes the overlay
/// and the burn path ignore what is stored, so a wrong artifact is out of
/// service without a redeploy. The producer also needs the startup self-test
/// to have passed and a local cache filesystem; the rows below say whether it
/// has them. Advisory: no row turns the switch on or off.
fn subtitle_stored_sources(enabled: bool, runtime_cache: &std::path::Path) -> DeveloperEnableItem {
    use crate::subtitle_ride_along::{self as ride_along, SelfTest};

    let (hits, empty, misses) = crate::subtitle_source::lookup_snapshot();
    let (attempted, [kept, no_cues, malformed, transient], written, published) =
        ride_along::snapshot();
    let (self_test_status, self_test_evidence) = match ride_along::self_test_state() {
        SelfTest::Passed {
            elapsed_ms,
            version,
        } => (
            RequirementStatus::Met,
            format!(
                "Passed in {elapsed_ms} ms: ffprobe and ffmpeg are the same build ({version}); \
                 the tee left the index output byte-identical, kept the intact track, recognised \
                 the corrupted track's stderr failure, and the framecrc byte arithmetic alone \
                 judged the corrupted track malformed."
            ),
        ),
        SelfTest::Failed { reason } => (
            RequirementStatus::Unmet,
            format!("Failed; inspect this ffmpeg build before relying on stored tracks: {reason}"),
        ),
        SelfTest::Running => (
            RequirementStatus::Unobservable,
            "Running now; its result is advisory and does not change the saved switch.".to_owned(),
        ),
        SelfTest::NotRun => (
            RequirementStatus::Unobservable,
            "Has not run in this process; verify the configured ffmpeg build before relying on stored tracks."
                .to_owned(),
        ),
    };
    let store_root = crate::subtitle_source::store_root(runtime_cache);
    let checked = if store_root.is_dir() {
        store_root
    } else {
        runtime_cache.to_owned()
    };
    let (filesystem_status, filesystem_evidence) = match ride_along::local_filesystem(&checked) {
        Ok(kind) => (
            RequirementStatus::Met,
            format!("{} is on a local filesystem ({kind}).", checked.display()),
        ),
        Err(reason) => (
            RequirementStatus::Unmet,
            format!("{}: {reason}. A blocked stage write may stall the shared index demuxer; the setting remains available.", checked.display()),
        ),
    };
    let (space_status, space_evidence) = match ride_along::free_space(&checked) {
        Ok(evidence) => (
            RequirementStatus::Met,
            format!("{}: {evidence}.", checked.display()),
        ),
        Err(reason) => (
            RequirementStatus::Unmet,
            format!("{}: {reason}. Stage writes share capacity with the index blob; the setting remains available.", checked.display()),
        ),
    };
    let failed = ride_along::failed_ride_count();
    DeveloperEnableItem {
        id: "subtitle_stored_sources",
        title: "Keep PGS tracks during indexing and read them instead of the source",
        enabled: Some(enabled),
        setting: Some("subtitle_stored_sources"),
        requirements: vec![
            DeveloperRequirement {
                id: "stored_source_producer",
                title: "Something fills the store",
                // A statement about this build, read from this build.
                status: RequirementStatus::Met,
                evidence: format!(
                    "The fragment-index pass keeps every PGS track it reads. Since this process \
                     started: {attempted} track(s) attempted — {kept} kept, {no_cues} with no \
                     cues, {malformed} malformed, {transient} transient — {written} byte(s) \
                     written to stages, {published} manifest(s) published; {failed} file(s) \
                     whose riding pass did not build its index are indexed without the \
                     ride-along until the next restart. Process-local."
                ),
            },
            DeveloperRequirement {
                id: "stored_source_self_test",
                title: "The startup self-test passed",
                status: self_test_status,
                evidence: self_test_evidence,
            },
            DeveloperRequirement {
                id: "stored_source_local_cache",
                title: "The cache is on a local filesystem",
                status: filesystem_status,
                evidence: filesystem_evidence,
            },
            DeveloperRequirement {
                id: "stored_source_free_space",
                title: "The cache has room for the stage",
                status: space_status,
                evidence: space_evidence,
            },
            DeveloperRequirement {
                id: "stored_source_lookups",
                title: "Lookups are answered from the store",
                status: RequirementStatus::Unobservable,
                evidence: format!(
                    "Since this process started: {hits} lookup(s) served a stored track, {empty} \
                     answered that the track has no cues, and {misses} fell through to \
                     extraction. Process-local; a restart returns them to zero."
                ),
            },
        ],
    }
}

/// The retained live-HLS engine: what it is, why it chose the work, and where
/// the switch is.
///
/// It spends real encode time on real hardware and, until this row existed,
/// said so only in a log line. It also used to be gated by a default Cargo
/// feature no build ever turned off, which meant the question "does this node
/// even have that engine" was answered by a compile flag rather than by
/// anything an operator could see.
fn live_hls_recovery(enabled: bool) -> DeveloperEnableItem {
    let counts = crate::transcode::live_recovery_snapshot();
    // The fallback's own total. Index 0 is the takeover path, which never
    // consults the switch, so folding it in here would tell an operator that
    // turning the switch off refuses sessions it cannot touch — and the row
    // directly below says the opposite about the same sessions.
    let fallback = counts[1..].iter().sum::<u64>();
    let by_reason = crate::transcode::LIVE_RECOVERY_LABELS
        .iter()
        .zip(counts.iter())
        .skip(1)
        .filter(|(_, count)| **count > 0)
        .map(|(label, count)| format!("{label} {count}"))
        .collect::<Vec<_>>()
        .join(", ");

    let coverage = if fallback == 0 {
        DeveloperRequirement {
            id: "vod_coverage_replaces_it",
            title: "Real VOD recipe coverage has replaced it",
            status: RequirementStatus::Unobservable,
            evidence: "No VOD prerequisite refusal has fallen back to the retained engine since \
                       this process started, which is consistent with complete coverage and also \
                       with nobody having asked. These counters are process-local and a restart \
                       returns them to zero, so a quiet hour is not a coverage proof."
                .to_owned(),
        }
    } else {
        DeveloperRequirement {
            id: "vod_coverage_replaces_it",
            title: "Real VOD recipe coverage has replaced it",
            status: RequirementStatus::Unmet,
            evidence: format!(
                "{fallback} session(s) since this process started reached the retained engine \
                 because immutable VOD refused them ({by_reason}). Turning the fallback off \
                 today would have refused those viewers instead. Settings \u{2192} Developer \
                 \u{2192} Sliding Live HLS holds the switch."
            ),
        }
    };

    // Never `Met`. The bypass is structural, not empirical: a request that
    // arrives already naming the live presentation takes that branch with no
    // setting consulted, so no number of switch-respecting sessions makes the
    // title true. A sample that happens to contain no takeover is exactly the
    // green tick meaning "not checked" this route exists to remove.
    let unswitchable = counts[0];
    let takeover = if unswitchable == 0 {
        DeveloperRequirement {
            id: "no_session_bypasses_the_switch",
            title: "Nothing reaches the live engine past the switch",
            status: RequirementStatus::Unobservable,
            evidence: format!(
                "No session since this process started arrived already naming the live \
                 presentation. That is not a guarantee: the path consults no setting, so this \
                 says only that nothing has taken it yet, and a restart returns the count to \
                 zero. ({fallback} fallback session(s) in the same window.)"
            ),
        }
    } else {
        DeveloperRequirement {
            id: "no_session_bypasses_the_switch",
            title: "Nothing reaches the live engine past the switch",
            status: RequirementStatus::Unmet,
            evidence: format!(
                "{unswitchable} session(s) since start arrived already naming the live \
                 presentation and were served without consulting the fallback setting. That is \
                 the peer takeover path, whose recipe validation requires it; turning the switch \
                 off does not stop those."
            ),
        }
    };

    let rolling_contract = DeveloperRequirement {
        id: "rolling_contract_built",
        title: "Bounded sliding publication and object grace are built",
        status: RequirementStatus::Met,
        evidence: "This build freezes the rolling target before first response, publishes from an actor-owned clock, retains removed objects through a bounded Grace lifetime, and charges advertised and Grace bytes to the live scratch total. This is build evidence, not device qualification."
            .to_owned(),
    };
    let clients = DeveloperRequirement {
        id: "rolling_clients_qualified",
        title: "Physical clients tolerate scheduled sliding updates",
        status: RequirementStatus::Unobservable,
        evidence: "Apple native HLS, Media3 and hls.js need physical-client qualification for startup, long playback, pause, resume and retirement. The daemon cannot observe a device test receipt; an absent error is not proof."
            .to_owned(),
    };

    DeveloperEnableItem {
        id: "live_hls_recovery",
        title: "Enable sliding Live HLS recovery",
        enabled: Some(enabled),
        setting: Some("vod_live_recovery"),
        requirements: vec![rolling_contract, coverage, takeover, clients],
    }
}

async fn cluster_transport_recovery(state: &AppState) -> DeveloperEnableItem {
    let budgets = &state.snapshot_recovery_budgets;
    let mut requirements = Vec::with_capacity(4);

    // "Fit the deployment" is a judgement about state size, transfer rate and
    // the container's health start period, and this process holds none of
    // those. It holds the three numbers, which are the useful half; reporting
    // them under `Met` would assert the half it cannot see.
    requirements.push(DeveloperRequirement {
        id: "recovery_budgets",
        title: "Recovery budgets fit the deployment",
        status: RequirementStatus::Unobservable,
        evidence: format!(
            "This node is configured for chunk {} s, whole transfer {} s, final install {} s, and \
             startup validation accepted them (it requires chunk \u{2264} transfer). Whether they \
             cover your measured state size, and whether the container's health start period \
             leaves room for a final install, are facts the daemon does not hold.",
            budgets.chunk_secs, budgets.transfer_secs, budgets.install_secs
        ),
    });

    requirements.push(cluster_api_requirement(state).await);
    requirements.push(cache_revocation_requirement(state).await);

    // The 20+20 campaign is a `make cluster-transport-recovery-check`
    // artifact validated by plurx-cluster-check. No running daemon has ever
    // held it, and inventing a proxy for it here would be exactly the
    // dishonesty this route replaces.
    requirements.push(DeveloperRequirement {
        id: "recovery_receipt",
        title: "The recovery receipt passes",
        status: RequirementStatus::Unobservable,
        evidence: "The real-TLS 3 MiB transport matrix and the twenty learner plus twenty voter \
                   recovery cycles are a qualification artifact from `make \
                   cluster-transport-recovery-check`. The daemon never receives it; read the \
                   run's receipt."
            .to_owned(),
    });

    DeveloperEnableItem {
        id: "cluster_transport_recovery",
        title: "Enable cluster transport recovery",
        enabled: None,
        setting: None,
        requirements,
    }
}

/// The roster half of "authenticated diagnostics are reachable".
///
/// One of the requirement's three clauses is readable and two are not. The
/// readable one is worth reading: `advertised_host` rewrites a loopback API
/// address to `localhost`, so a roster full of `localhost` is a cluster whose
/// nodes advertise addresses no peer can dial — a real and silent
/// misconfiguration. A non-loopback host proves only that a host was
/// registered: nothing here dials it, nothing here knows whether the cluster
/// API credential is shared, and nothing here knows whether the listener is
/// exposed to the public Internet. So the best case is `Unobservable` with the
/// roster reported, not `Met`.
async fn cluster_api_requirement(state: &AppState) -> DeveloperRequirement {
    const ID: &str = "cluster_api_advertised";
    const TITLE: &str = "Authenticated diagnostics are reachable";

    let status = match state.membership.status().await {
        Ok(status) => status,
        Err(MembershipError::Unavailable) => {
            return DeveloperRequirement {
                id: ID,
                title: TITLE,
                status: RequirementStatus::Unobservable,
                evidence: "This node is not replicated, so it holds no committed roster to read."
                    .to_owned(),
            };
        }
        Err(error) => {
            return DeveloperRequirement {
                id: ID,
                title: TITLE,
                status: RequirementStatus::Unobservable,
                evidence: format!(
                    "This node is replicated, but reading the committed roster failed, so the \
                     answer is unknown rather than negative ({error})."
                ),
            };
        }
    };

    // A loopback API address is only wrong when there is a peer that would
    // have to dial it. A single-voter cluster on 127.0.0.1 is configured
    // correctly and must not render red.
    let unreachable = if status.nodes.len() < 2 {
        Vec::new()
    } else {
        status
            .nodes
            .iter()
            .filter(|node| {
                let host = node.advertised_host.trim();
                host.is_empty() || host.eq_ignore_ascii_case("localhost")
            })
            .map(|node| node.node_id.clone())
            .collect::<Vec<_>>()
    };

    if unreachable.is_empty() {
        // Two different readings, and they must not share a sentence. Below
        // two nodes the host check was skipped on purpose, so claiming the
        // hosts are routable would be claiming a reading that did not happen
        // — and it would be false for the single voter on 127.0.0.1 that the
        // skip exists to accommodate.
        //
        // `status.nodes` counts `cluster_nodes` rows that are not tombstoned,
        // which is not the same as the committed Raft configuration; a
        // two-member config with one tombstoned row reports one node here.
        // Said out loud rather than rounded to "committed".
        let evidence = if status.nodes.len() < 2 {
            format!(
                "{} node(s) in the local roster, so no peer would have to dial an advertised host \
                 and the host check was skipped rather than passed \u{2014} a single voter on \
                 127.0.0.1 is configured correctly. Nothing else in this requirement is readable \
                 from here either: the daemon does not dial, does not know whether every node \
                 shares the cluster API credential, and cannot see whether \
                 /cluster/transport/sqlite is reachable beyond your trusted network.",
                status.nodes.len()
            )
        } else {
            format!(
                "None of the {} node(s) in the local roster advertises an empty or loopback \
                 cluster API host, which is the one clause of this requirement the daemon can \
                 read. It does not dial them, does not know whether every node shares the cluster \
                 API credential, and cannot see whether /cluster/transport/sqlite is reachable \
                 beyond your trusted network.",
                status.nodes.len()
            )
        };
        DeveloperRequirement {
            id: ID,
            title: TITLE,
            status: RequirementStatus::Unobservable,
            evidence,
        }
    } else {
        DeveloperRequirement {
            id: ID,
            title: TITLE,
            status: RequirementStatus::Unmet,
            evidence: format!(
                "{} of {} committed node(s) advertise a cluster API address no peer can dial \
                 (empty or loopback): {}.",
                unreachable.len(),
                status.nodes.len(),
                unreachable.join(", ")
            ),
        }
    }
}

/// The cache-revocation capability, and the fail-open value it must not report.
///
/// `cache_admin_revocation_ready()` answers `Ok(true)` for an unreplicated node
/// — correctly, because its caller asks "may cache-only recovery auth
/// proceed", and on a node with no cluster there is nothing to revoke. Read as
/// a prerequisite reading it would claim "every committed node proved the
/// capability in its current heartbeat" on an install that has no committed
/// nodes and takes no heartbeats. The replication check has to come first.
async fn cache_revocation_requirement(state: &AppState) -> DeveloperRequirement {
    const ID: &str = "cache_revocation_capability";
    const TITLE: &str = "Every committed node proves cache revocation";

    if !state.membership.is_replicated() {
        return DeveloperRequirement {
            id: ID,
            title: TITLE,
            status: RequirementStatus::Unobservable,
            evidence: "This node is not replicated: there is no committed configuration, so there \
                       is nothing that could have proven the capability."
                .to_owned(),
        };
    }

    match state.membership.cache_admin_revocation_ready().await {
        Ok(true) => DeveloperRequirement {
            id: ID,
            title: TITLE,
            status: RequirementStatus::Met,
            evidence: "In this node's local applied roster, every member of the committed \
                       configuration has the cache revocation capability in a current heartbeat. \
                       That read is deliberately local so it still answers during quorum loss, so \
                       a lagging follower can report this from rows a leader has already moved \
                       past."
                .to_owned(),
        },
        // The readiness answer is one boolean over four separate conditions,
        // and naming the wrong one sends an operator hunting a peer that is
        // fine. Only one of the four has its own accessor, so read that one
        // and name the rest as the disjunction they are.
        //
        // The `false` arms are: this node is outside committed membership; a
        // committed member has no current-heartbeat capability row; the
        // credential guard has never activated; a revocation is in flight. An
        // earlier version of this sentence asserted the second as fact, which
        // on this fleet — where the guard has not activated — was a reading
        // the function never took.
        Ok(false) => {
            let activated = state
                .membership
                .cache_admin_revocation_activated_locally()
                .await;
            let evidence = match activated {
                Ok(false) => "The credential guard has never activated on this cluster \
                              (`cluster_credential_guard_activation` is empty), which closes \
                              cache-only recovery authorization on its own. Nothing here says \
                              anything about any peer's heartbeat."
                    .to_owned(),
                Ok(true) => "The guard has activated, so the closure is one of: this node is \
                             itself outside committed membership; some committed member has no \
                             current-heartbeat capability row; or a cache-admin revocation is in \
                             flight. This route cannot tell those three apart \u{2014} check \
                             whether Cluster \u{2192} Nodes lists this node before looking for a \
                             faulty peer."
                    .to_owned(),
                Err(error) => format!(
                    "Cache-only recovery authorization is closed. Which of the four causes it is \
                     could not be narrowed, because the guard activation marker could not be read \
                     ({error})."
                ),
            };
            DeveloperRequirement {
                id: ID,
                title: TITLE,
                status: RequirementStatus::Unmet,
                evidence,
            }
        }
        Err(error) => DeveloperRequirement {
            id: ID,
            title: TITLE,
            status: RequirementStatus::Unobservable,
            evidence: format!("The local applied roster could not be read ({error})."),
        },
    }
}

fn playback_control_protocol(advertised: bool) -> DeveloperEnableItem {
    let vocabulary = crate::playback_control::control_vocabulary_snapshot();
    let complete = vocabulary.passive_complete.iter().sum::<u64>();
    let partial = vocabulary.passive_partial.iter().sum::<u64>();

    // The passive vocabulary is `hold`, `terminal` and `retry_resource` —
    // what a reporter applies to a stream already playing. Deliberately not
    // the four-action "fully managed" reading: `prepare_replacement` belongs
    // to the prepared handoff and has its own row below. A
    // passive-reporter row that demanded it answered `Unmet` on every fleet
    // that had a complete passive reporter on every client, which is a red
    // tick that means "measured something else".
    //
    // "Every client" still cannot be read upward from this counter. A client
    // with no reporter never performs a control exchange, so it appears in
    // neither column: any number of complete exchanges is consistent with a
    // fleet full of silent clients. Downward it reads fine — one partial
    // exchange proves a client in the fleet is not fully declared. So `Unmet`
    // is reachable and `Met` is not, which is the honest shape of this
    // question.
    let reporters = if partial > 0 {
        DeveloperRequirement {
            id: "clients_report",
            title: "Every client has a passive reporter",
            status: RequirementStatus::Unmet,
            evidence: format!(
                "{partial} exchange(s) since this process started came from a client that did not \
                 declare all of hold, terminal and retry_resource, so at least one client in the \
                 fleet has no complete passive reporter. By platform (web/apple/android): partial \
                 {:?}, complete {:?}.",
                vocabulary.passive_partial, vocabulary.passive_complete
            ),
        }
    } else if complete > 0 {
        DeveloperRequirement {
            id: "clients_report",
            title: "Every client has a passive reporter",
            status: RequirementStatus::Unobservable,
            evidence: format!(
                "{complete} exchange(s) since this process started declared all of hold, terminal \
                 and retry_resource, and none were partial \u{2014} but a client with no reporter \
                 never performs an exchange at all, so this cannot speak for clients it has not \
                 heard from. By platform (web/apple/android): complete {:?}.",
                vocabulary.passive_complete
            ),
        }
    } else {
        DeveloperRequirement {
            id: "clients_report",
            title: "Every client has a passive reporter",
            status: RequirementStatus::Unobservable,
            evidence: "No client has completed a control exchange with this process since it \
                       started, so it has nothing to report. These counters are process-local and \
                       a restart returns them to zero."
                .to_owned(),
        }
    };

    DeveloperEnableItem {
        id: "playback_control_protocol_v1",
        title: "Advertise playback control protocol v1",
        enabled: Some(advertised),
        setting: Some("playback_control_protocol_v1"),
        requirements: vec![reporters],
    }
}

fn prepared_quality_handoff(enabled: bool) -> DeveloperEnableItem {
    let staged = crate::playback_control::preparation_staged_snapshot();
    let prepare_capable = crate::playback_control::prepare_capable_snapshot();
    let declared = prepare_capable.iter().sum::<u64>();

    let mut requirements = Vec::with_capacity(3);

    requirements.push(DeveloperRequirement {
        id: "server_preparation_is_real",
        title: "Server preparation is real",
        status: RequirementStatus::Met,
        evidence: "This build durably reserves a successor, attaches its VOD worker before the \
                   actor may announce it, serves only that staged generation's media capability, \
                   and tears the worker down on abort or expiry."
            .to_owned(),
    });

    // Counted separately from the full-vocabulary counter on purpose. A client
    // may declare `prepare_replacement` without declaring `retry_resource`;
    // folding the two would let this row report that nobody asked for a
    // handoff while a client was asking for exactly that.
    requirements.push(if declared > 0 {
        DeveloperRequirement {
            id: "client_two_player_handoff",
            title: "A client owns a measured two-player handoff",
            status: RequirementStatus::Unmet,
            evidence: format!(
                "{declared} exchange(s) since start declared `prepare_replacement` (by platform \
                 web/apple/android: {prepare_capable:?}), but declaring the action is not the \
                 physical qualification. The web, Apple and Android builds contain two-player \
                 adapters; this process cannot prove their timeline alignment or qualifying first \
                 frame from a capability declaration. Preparations since start: {} staged, {} \
                 refused.",
                staged.staged, staged.refused
            ),
        }
    } else {
        DeveloperRequirement {
            id: "client_two_player_handoff",
            title: "A client owns a measured two-player handoff",
            status: RequirementStatus::Unmet,
            evidence: format!(
                "The web, Apple and Android builds contain two-player adapters, but no client has \
                 declared `prepare_replacement` to this process since it started. This node has \
                 therefore observed no physical first-frame qualification. Preparations since \
                 start: {} staged, {} refused.",
                staged.staged, staged.refused
            ),
        }
    });

    requirements.push(DeveloperRequirement {
        id: "fleet_receipt",
        title: "The fleet receipt is complete",
        status: RequirementStatus::Unobservable,
        evidence: "Twenty consecutive commits on the corrected realistic-runway instrument with \
                   stable memory is a fleet measurement. One node's counters cannot stand in for \
                   it."
        .to_owned(),
    });

    DeveloperEnableItem {
        id: "prepared_quality_handoff",
        title: "Enable prepared quality handoff",
        enabled: Some(enabled),
        setting: Some("prepared_quality_handoff"),
        requirements,
    }
}

// Chapter thumbnails.
//
// The watch view's chapter rail shows a frame per chapter, made on request by
// one ffmpeg seek and kept under the runtime cache. The switch is the enable
// path; the rows say what an extraction needs and what this process has done
// so far, and never turn the switch.
async fn chapter_thumbnails(state: &AppState) -> DeveloperEnableItem {
    use crate::http::chapter_thumbs;
    use crate::subtitle_ride_along as ride_along;

    let enabled = chapter_thumbs::enabled(state).await;
    let counts = chapter_thumbs::snapshot();
    let root = chapter_thumbs::cache_root(&state.runtime_cache_dir);
    let checked = if root.is_dir() {
        root.clone()
    } else {
        state.runtime_cache_dir.clone()
    };
    // Two directory walks and a statvfs, off the async runtime.
    let runtime_cache = state.runtime_cache_dir.clone();
    let checked_for_space = checked.clone();
    let ((files, bytes), space) = tokio::task::spawn_blocking(move || {
        (
            chapter_thumbs::cache_footprint(&runtime_cache),
            ride_along::free_space(&checked_for_space),
        )
    })
    .await
    .unwrap_or_else(|_| ((0, 0), Err("the readiness walk panicked".to_owned())));
    let (ffmpeg_status, ffmpeg_evidence) = match state.system.ffmpeg_version.as_deref() {
        Some(version) if !version.trim().is_empty() => (
            RequirementStatus::Met,
            format!(
                "{} answered `-version` at startup: {}.",
                state.system.ffmpeg,
                version.trim()
            ),
        ),
        _ => (
            RequirementStatus::Unmet,
            format!(
                "{} did not answer `-version` at startup, so every extraction will fail \
                 and the rail shows numbered tiles.",
                state.system.ffmpeg
            ),
        ),
    };
    let (space_status, space_evidence) = match space {
        Ok(evidence) => (
            RequirementStatus::Met,
            format!("{}: {evidence}.", checked.display()),
        ),
        Err(reason) => (
            RequirementStatus::Unmet,
            format!(
                "{}: {reason}. A thumbnail is tens of kilobytes, but a cache with no room \
                 fails every write.",
                checked.display()
            ),
        ),
    };
    DeveloperEnableItem {
        id: "chapter_thumbnails",
        title: "Make chapter thumbnails for the watch view",
        enabled: Some(enabled),
        setting: Some("chapter_thumbnails"),
        requirements: vec![
            DeveloperRequirement {
                id: "chapter_thumbs_ffmpeg",
                title: "ffmpeg can decode a frame",
                status: ffmpeg_status,
                evidence: ffmpeg_evidence,
            },
            DeveloperRequirement {
                id: "chapter_thumbs_cache_space",
                title: "The runtime cache has room",
                status: space_status,
                evidence: space_evidence,
            },
            DeveloperRequirement {
                id: "chapter_thumbs_work",
                title: "What this process has extracted",
                // Counters read from this process, so always answerable.
                status: RequirementStatus::Met,
                evidence: format!(
                    "Since this process started: {} thumbnail(s) made, {} served from the \
                     cache, {} extraction(s) failed, {} running now, {} request(s) refused \
                     while the switch was off. Each extraction is one ffmpeg seek and one \
                     decoded frame, CPU only, bounded to {} at a time and {} seconds each; \
                     nothing runs unless a watch page asks for that chapter, and a failed \
                     chapter is not retried for an hour. On disk: {files} thumbnail(s), {} \
                     under {}.",
                    counts.generated,
                    counts.served_cached,
                    counts.failed,
                    counts.in_flight,
                    counts.refused_off,
                    chapter_thumbs::EXTRACT_CONCURRENCY,
                    chapter_thumbs::EXTRACT_TIMEOUT.as_secs(),
                    human_bytes(bytes),
                    root.display()
                ),
            },
        ],
    }
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Refusing a subtitle segment whose sidecar has failed, instead of serving a
/// syntactically valid empty track.
///
/// Every row is an engine observation, and not one of them is consulted by
/// the settings write. The honest answer for all three is `Unobservable`
/// today, and saying so is the point: the measurement is *"does this engine
/// keep playing video through a subtitle 503"*, which is a fact about
/// AVPlayer, Media3 and hls.js running on real devices, not a fact a server
/// process can read off itself. An operator who has run the physical
/// verification may turn this on over three grey rows.
fn subtitle_not_ready_503(enabled: bool) -> DeveloperEnableItem {
    let engine = |id: &'static str, title: &'static str, detail: String| DeveloperRequirement {
        id,
        title,
        status: RequirementStatus::Unobservable,
        evidence: detail,
    };

    DeveloperEnableItem {
        id: "subtitle_not_ready_503",
        title: "Refuse a subtitle segment whose extraction failed",
        enabled: Some(enabled),
        setting: Some("subtitle_not_ready_503"),
        requirements: vec![
            engine(
                "avplayer_survives_subtitle_refusal",
                "AVPlayer keeps the picture through a subtitle 503",
                "AVPlayer allows a subtitle segment roughly two seconds and blocks the muxed \
                 video while it waits, so a refusal on that request is the one with a picture \
                 riding on it. Whether it drops the legible selection and plays on, or stalls \
                 the video, is a measurement on an Apple TV and an iPad — not something this \
                 process can read. Off until it is taken."
                    .to_owned(),
            ),
            engine(
                "media3_survives_subtitle_refusal",
                "Media3 keeps the picture through a subtitle 503",
                "ExoPlayer's HLS text renderer may surface a 4xx/5xx on a subtitle rendition as \
                 a fatal source error rather than a text-track error. The distinction decides \
                 whether an Android viewer loses their subtitles or their film, and it is a \
                 measurement on a device."
                    .to_owned(),
            ),
            engine(
                "hlsjs_survives_subtitle_refusal",
                "hls.js keeps the picture through a subtitle 503",
                "hls.js retries subtitle fragments on its own schedule and escalates to a fatal \
                 network error after its retry budget. Whether the bundled 1.6.16 build treats a \
                 503 with `Retry-After` on a subtitle rendition as recoverable is a browser \
                 measurement."
                    .to_owned(),
            ),
        ],
    }
}

/// Recording from the tuner to a disk. Every row here is a fact this process
/// read just now, and not one of them is consulted by the settings write:
/// `dvr_enabled` is a plain switch, and an operator who can see the deployment
/// may turn it on over any amount of red.
///
/// The honest shape of these questions is why two of the six can be
/// `Unobservable` on a healthy server. Whether every node mounts the DVR root
/// is a fact about a cluster, and a single-node install has no peers to ask;
/// whether the root is writable is a fact about the owner's filesystem, and a
/// node that is not the owner is not the one that will be writing to it.
async fn dvr(
    state: &AppState,
    enabled: bool,
    live_tv: &crate::live_tv::LiveTvConfig,
    dvr: &crate::live_tv::DvrConfig,
) -> DeveloperEnableItem {
    let owner = live_tv.owner_node_id == state.node_id;
    let mut requirements = vec![
        root_writable(state, dvr, owner).await,
        free_space(dvr, owner),
    ];
    requirements.push(guide_horizon(state, live_tv).await);
    requirements.push(tuner_reserve(live_tv, dvr));
    requirements.push(DeveloperRequirement {
        id: "every_node_mounts_root",
        title: "Every node can read the DVR root",
        status: RequirementStatus::Unobservable,
        evidence: if state.membership.is_replicated() {
            "The owner writes recordings and any node may serve them, so the root has to be a \
             mount every node has. This process can see its own filesystem and no peer's."
                .to_owned()
        } else {
            "This is a single-node install, so the node that records is the node that serves."
                .to_owned()
        },
    });
    if !dvr.webhook_url.is_empty() {
        requirements.push(webhook_url_approved(dvr));
    }
    DeveloperEnableItem {
        id: "dvr",
        title: "Enable recording",
        enabled: Some(enabled),
        setting: Some("dvr_enabled"),
        requirements,
    }
}

async fn root_writable(
    state: &AppState,
    dvr: &crate::live_tv::DvrConfig,
    owner: bool,
) -> DeveloperRequirement {
    let (status, evidence) = if dvr.root.is_empty() {
        (
            RequirementStatus::Unmet,
            "No DVR root is set. A recording has nowhere to go until one is.".to_owned(),
        )
    } else if !owner {
        (
            RequirementStatus::Unobservable,
            format!(
                "This node is not the tuner owner, so it is not the node that will write to \
                 `{}`. Read this row on the owner.",
                dvr.root
            ),
        )
    } else {
        // On a blocking thread: the DVR root is by design a shared mount, and
        // a hung NFS server would otherwise park a runtime worker for as long
        // as the mount stayed hung — on a *diagnostic* page.
        let root = dvr.root.clone();
        match tokio::task::spawn_blocking(move || probe_root(&root)).await {
            Ok(Ok(())) => (
                RequirementStatus::Met,
                format!("Created and removed a probe file under `{}`.", dvr.root),
            ),
            Ok(Err(error)) => (
                RequirementStatus::Unmet,
                format!("Could not write under `{}`: {error}.", dvr.root),
            ),
            Err(error) => (
                RequirementStatus::Unobservable,
                format!("The probe of `{}` did not finish: {error}.", dvr.root),
            ),
        }
    };
    let _ = state;
    DeveloperRequirement {
        id: "dvr_root_writable",
        title: "The tuner owner can write to the DVR root",
        status,
        evidence,
    }
}

/// Write and remove one small file rather than checking permission bits. A
/// read-only remount, a full filesystem and an ACL that says yes but means no
/// all look identical to `statx`.
///
/// It does **not** create the root. A readiness page is a question, and a
/// question that silently makes a directory on a shared mount is a side
/// effect nobody asked for — the engine creates it when a capture starts.
fn probe_root(root: &str) -> std::io::Result<()> {
    let directory = std::path::Path::new(root);
    if !directory.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "the DVR root does not exist yet; recording creates it when it first captures",
        ));
    }
    let probe = directory.join(".plurx-probe");
    std::fs::write(&probe, b"plurx")?;
    std::fs::remove_file(&probe)
}

fn free_space(dvr: &crate::live_tv::DvrConfig, owner: bool) -> DeveloperRequirement {
    let floor = dvr.free_floor_gb.max(0) as u64 * 1_000_000_000;
    let (status, evidence) = match crate::live_tv::free_space_bytes(&dvr.root) {
        Some(free) if free >= floor => (
            RequirementStatus::Met,
            format!(
                "{:.1} GB free, against a floor of {} GB.",
                free as f64 / 1e9,
                dvr.free_floor_gb
            ),
        ),
        Some(free) => (
            RequirementStatus::Unmet,
            format!(
                "{:.1} GB free, below the {} GB floor. The scheduler will not start a capture \
                 under the floor; it will show the conflict rather than fill the disk.",
                free as f64 / 1e9,
                dvr.free_floor_gb
            ),
        ),
        None if !owner => (
            RequirementStatus::Unobservable,
            "This node cannot see the DVR root. Read this row on the tuner owner.".to_owned(),
        ),
        None => (
            RequirementStatus::Unmet,
            "The DVR root is unset or unreadable, so its free space cannot be read.".to_owned(),
        ),
    };
    DeveloperRequirement {
        id: "dvr_free_space",
        title: "The DVR root has room above the floor",
        status,
        evidence,
    }
}

/// A four-hour guide can only record what is nearly on. Everything else the
/// DVR offers — a series rule, a schedule, a conflict worth resolving — needs
/// a horizon, and the free HDHomeRun tier does not have one.
async fn guide_horizon(
    state: &AppState,
    live_tv: &crate::live_tv::LiveTvConfig,
) -> DeveloperRequirement {
    let now = crate::live_tv::unix_seconds();
    let guide = crate::http::live_tv::owner_guide(
        state,
        live_tv,
        crate::live_tv::GuideWindow {
            start: now,
            end: now + i64::from(live_tv.guide_hours) * 3600,
        },
    )
    .await;
    let furthest = guide
        .channels
        .iter()
        .filter_map(|channel| channel.programmes.last().map(|row| row.end))
        .max();
    let (status, evidence) = match furthest {
        Some(end) if end > now + 24 * 3600 && live_tv.guide_hours > 24 => (
            RequirementStatus::Met,
            format!(
                "The cached guide reaches {:.0} hours ahead, with a {}-hour look-ahead \
                 configured.",
                (end - now) as f64 / 3600.0,
                live_tv.guide_hours
            ),
        ),
        Some(end) => (
            RequirementStatus::Unmet,
            format!(
                "The cached guide reaches only {:.0} hours ahead (look-ahead is {} hours) — \
                 record-what's-on only. A subscription tier or an XMLTV source is what gives a \
                 series rule something to schedule.",
                (end - now).max(0) as f64 / 3600.0,
                live_tv.guide_hours
            ),
        ),
        None => (
            RequirementStatus::Unmet,
            "The owner has no cached guide, so nothing can be scheduled from it. Recording a \
             fixed time on a channel still works."
                .to_owned(),
        ),
    };
    DeveloperRequirement {
        id: "guide_horizon",
        title: "The guide reaches far enough to schedule from",
        status,
        evidence,
    }
}

fn tuner_reserve(
    live_tv: &crate::live_tv::LiveTvConfig,
    dvr: &crate::live_tv::DvrConfig,
) -> DeveloperRequirement {
    let slots = dvr.recording_slots(live_tv.max_sessions);
    let (status, evidence) = if slots == 0 {
        (
            RequirementStatus::Unmet,
            format!(
                "{} tuner session(s) are configured and {} are reserved for viewing, so no \
                 recording could ever start.",
                live_tv.max_sessions, dvr.tuner_reserve
            ),
        )
    } else {
        (
            RequirementStatus::Met,
            format!(
                "Recordings may hold {slots} of {} tuner session(s); {} stays reserved so a \
                 viewer is never locked out by their own schedule.",
                live_tv.max_sessions, dvr.tuner_reserve
            ),
        )
    };
    DeveloperRequirement {
        id: "tuner_reserve",
        title: "A tuner stays free for watching",
        status,
        evidence,
    }
}

fn webhook_url_approved(dvr: &crate::live_tv::DvrConfig) -> DeveloperRequirement {
    let (status, evidence) = match crate::live_tv::approved_webhook_url(&dvr.webhook_url) {
        Ok(url) => (
            RequirementStatus::Met,
            format!(
                "`{}` passes the outbound policy: https anywhere, or plain http only to a \
                 private address.",
                url.host_str().unwrap_or("the configured host")
            ),
        ),
        Err(reason) => (RequirementStatus::Unmet, reason),
    };
    DeveloperRequirement {
        id: "webhook_url_approved",
        title: "The webhook URL is one this server will call",
        status,
        evidence,
    }
}

fn semantic_search(enabled: bool) -> DeveloperEnableItem {
    let observed = crate::library_search::semantic::status();
    let phase = observed["state"].as_str().unwrap_or("disabled");
    let model_loaded = matches!(phase, "indexing" | "ready" | "ready_capacity_limit");
    let indexed = observed["indexed"].as_u64().unwrap_or(0);
    DeveloperEnableItem{id:"embedded_semantic_search",title:"Embedded semantic search",enabled:Some(enabled),setting:None,requirements:vec![
        DeveloperRequirement{id:"runtime",title:"Embedded CPU runtime",status:RequirementStatus::Met,evidence:"Included in this server build. Enabling needs no external inference service.".into()},
        DeveloperRequirement{id:"model",title:"Verified model loaded on this node",status:if model_loaded{RequirementStatus::Met}else{RequirementStatus::Unmet},evidence:format!("Current state: {phase}. First enable downloads about 91 MB from Hugging Face; model files are pinned and checksum-verified.")},
        DeveloperRequirement{id:"index",title:"Local semantic index",status:if indexed>0{RequirementStatus::Met}else{RequirementStatus::Unmet},evidence:format!("{indexed} titles indexed on this node. Building the index uses additional CPU and memory; ordinary text search remains available.")},
    ]}
}

fn channel_subjects(enabled: bool) -> DeveloperEnableItem {
    let observed = crate::channel_subjects::observation();
    let status = if observed.error.is_some() {
        RequirementStatus::Unmet
    } else if observed.profile.is_some() {
        RequirementStatus::Met
    } else {
        RequirementStatus::Unobservable
    };
    DeveloperEnableItem {id:"library_channel_subject_matching",title:"Library channel subject matching",enabled:Some(enabled),setting:Some("library_channel_subject_matching_enabled"),requirements:vec![
        DeveloperRequirement{id:"provider",title:"Local catalogue matcher",status,evidence:observed.error.clone().unwrap_or_else(||observed.profile.clone().unwrap_or_else(||"Local metadata rules are available without an inference provider. The worker has not reported a batch yet.".into()))},
        DeveloperRequirement{id:"metadata",title:"Metadata coverage",status:if observed.metadata_total>0{RequirementStatus::Met}else{RequirementStatus::Unobservable},evidence:format!("Last observed scope: {} titles, {} missing item overviews, {} truncated inputs. Sparse metadata can remain uncertain.",observed.metadata_total,observed.missing_overviews,observed.truncated)},
        DeveloperRequirement{id:"batch",title:"Recent batch outcome and queued work",status:if observed.error.is_some(){RequirementStatus::Unmet}else{RequirementStatus::Unobservable},evidence:format!("{}; queued work observed: {}. Only new rule evaluations pause when disabled; saves, cached decisions and published playback remain available.",observed.error.unwrap_or_else(||"No recent error recorded".into()),observed.pending>0)},
    ]}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn developer_item_renders_each_requirement_from_real_probes() {
        let item = subtitle_backfill_item(
            true,
            Ok(crate::state::SubtitleBackfillStatus {
                lease_holder: Some("node-b".to_owned()),
                enqueued_process: 7,
                remaining_files: 12,
                remaining_bytes: 987_654,
            }),
        );
        assert_eq!(item.enabled, Some(true));
        assert_eq!(item.setting, Some("subtitle_backfill"));
        assert_eq!(
            item.requirements
                .iter()
                .map(|row| row.id)
                .collect::<Vec<_>>(),
            [
                "backfill_lease",
                "backfill_enqueued",
                "backfill_remaining",
                "backfill_bytes"
            ]
        );
        for (row, expected) in
            item.requirements
                .iter()
                .zip(["node-b", "7 files", "12 files", "987654 source bytes"])
        {
            assert!(
                row.evidence.contains(expected),
                "{}: {}",
                row.id,
                row.evidence
            );
            assert_eq!(row.status, RequirementStatus::Met);
        }

        let between_passes = subtitle_backfill_item(
            true,
            Ok(crate::state::SubtitleBackfillStatus {
                lease_holder: None,
                enqueued_process: 0,
                remaining_files: 0,
                remaining_bytes: 0,
            }),
        );
        assert_eq!(
            between_passes.enabled,
            Some(true),
            "a missing lease never changes the saved switch"
        );
        assert_eq!(
            between_passes.requirements[0].status,
            RequirementStatus::Unobservable
        );
        assert!(between_passes.requirements[0]
            .evidence
            .contains("none between passes"));

        let unavailable = subtitle_backfill_item(true, Err("store unavailable".to_owned()));
        assert_eq!(
            unavailable.enabled,
            Some(true),
            "an unavailable probe never changes the saved switch"
        );
        assert!(unavailable
            .requirements
            .iter()
            .all(|row| row.status == RequirementStatus::Unobservable
                && row.evidence.contains("store unavailable")));
    }

    #[test]
    fn android_display_mode_readiness_is_advisory_and_observation_based() {
        let absent = android_display_mode_match(true, &[]);
        assert_eq!(absent.setting, Some("playback_display_mode_match"));
        assert!(absent.enabled == Some(true));
        assert!(matches!(
            absent.requirements[0].status,
            RequirementStatus::Unobservable
        ));

        let matched = android_display_mode_match(
            false,
            &[PlaybackEvent {
                detail: Some("outcome=matched source_fps=23.976".into()),
                ..PlaybackEvent::default()
            }],
        );
        assert!(matched.enabled == Some(false));
        assert!(matches!(
            matched.requirements[0].status,
            RequirementStatus::Met
        ));
    }
}
