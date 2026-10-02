//! Read-only, file-scoped processing facts. Opening Info never starts work.
use axum::extract::{Path, State};
use axum::Json;
use plurx_core::domain::{MediaFile, SubtitleStream};
use plurx_core::store::SubtitleSourcePublication;
use plurx_core::tracks::lang_matches;
use serde_json::{json, Value};

use super::{error::ApiError, extract::AuthUser};
use crate::state::AppState;
use crate::subtitle_source::TrackKind;

fn fact(state: &str, detail: impl Into<String>) -> Value {
    json!({"state":state,"detail":detail.into()})
}

/// The configured language still matters when automatic playback chooses Off.
/// Prefer the policy-selected matching track, then a full matching default,
/// then a full matching track, finally a forced track in that language.
fn preferred_subtitle(
    tracks: &[SubtitleStream],
    language: &str,
    selected: Option<i64>,
) -> Option<i64> {
    let matching = |s: &&SubtitleStream| lang_matches(&s.language, language);
    tracks
        .iter()
        .filter(matching)
        .find(|s| Some(s.index) == selected)
        .or_else(|| {
            tracks
                .iter()
                .filter(matching)
                .find(|s| s.default && !s.forced)
        })
        .or_else(|| tracks.iter().filter(matching).find(|s| !s.forced))
        .or_else(|| tracks.iter().find(matching))
        .map(|s| s.index)
}

fn track_state(
    file: &MediaFile,
    track: &SubtitleStream,
    publications: &[SubtitleSourcePublication],
    attestation: Option<&str>,
) -> &'static str {
    if file.downloaded_subtitle(track.index).is_some() {
        return "ready";
    }
    let kind = match track.codec.as_str() {
        "hdmv_pgs_subtitle" | "pgs" => TrackKind::Pgs,
        "ass" | "ssa" => TrackKind::TextStyled,
        "subrip" | "srt" | "webvtt" | "vtt" | "mov_text" | "text" => TrackKind::Text,
        _ => return "unsupported",
    };
    let Some(attestation) = attestation else {
        return "unknown";
    };
    let states: Vec<_> =
        kind.representations()
            .iter()
            .map(|format| {
                let rows: Vec<_> = publications
                    .iter()
                    .filter(|p| {
                        p.file_id == file.id
                            && p.source_size == file.size
                            && p.source_mtime == file.mtime
                            && p.source_attestation == attestation
                            && p.ordinal == track.index
                            && p.format == format.publication_name()
                            && p.origin == "extracted"
                    })
                    .collect();
                if rows
                    .iter()
                    .any(|p| p.verdict == "kept" && p.bytes > 0 && p.sha256.len() == 64)
                {
                    "ready"
                } else if rows.iter().any(|p| p.verdict == "empty") {
                    "empty"
                } else if rows.iter().any(|p| {
                    p.verdict == "malformed" || (p.verdict == "transient" && p.attempts >= 3)
                }) {
                    "failed"
                } else {
                    "pending"
                }
            })
            .collect();
    if states.contains(&"failed") {
        "failed"
    } else if states.contains(&"pending") {
        "pending"
    } else if states.iter().all(|s| *s == "empty") {
        "empty"
    } else {
        "ready"
    }
}

fn subtitle_indicator(states: &[(i64, &str)], preferred: Option<i64>) -> &'static str {
    let relevant: Vec<_> = states
        .iter()
        .filter(|(id, _)| preferred.is_none_or(|p| p == *id))
        .collect();
    if relevant.is_empty() {
        return if states.is_empty() { "none" } else { "unknown" };
    }
    if relevant
        .iter()
        .any(|(_, s)| matches!(*s, "failed" | "unsupported"))
    {
        "attention"
    } else if relevant.iter().any(|(_, s)| *s == "unknown") {
        "unknown"
    } else if relevant
        .iter()
        .all(|(_, s)| matches!(*s, "ready" | "empty"))
    {
        "done"
    } else {
        "pending"
    }
}

fn analysis_fact(history: &Value, component: &str, fallback: Value) -> Value {
    let row = history["analysis"]
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["component"] == component));
    let Some(row) = row else {
        return fallback;
    };
    let state = match row["state"].as_str().unwrap_or("") {
        "running" => "running",
        "queued" | "submitted" => "queued",
        "failed" => "failed",
        "cancelled" => "cancelled",
        _ => return fallback,
    };
    let detail = match state {
        "running" => "Processing this file",
        "queued" => "Waiting for a worker or retry window",
        "failed" => "Processing failed; open processing details to retry",
        _ => "The previous request was cancelled",
    };
    json!({"state":state,"detail":detail,"error":row["error"],"updated_at_ms":row["updated_at_ms"]})
}

pub async fn status(
    AuthUser(_user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    let file = state
        .store
        .get_file(id)
        .await?
        .ok_or(ApiError::NotFound("file"))?;
    let item = state
        .store
        .get_item(file.item_id)
        .await?
        .ok_or(ApiError::NotFound("item"))?;
    let history = state.store.media_preparation_history(id).await?;
    let object_version = crate::fragment_index_cluster::inspect_source(&file)
        .await
        .ok();
    let current = object_version.is_some();
    let observation = if let Some(version) = &object_version {
        state
            .store
            .fragment_index_source(&state.node_id, id, version)
            .await?
    } else {
        None
    };
    let attestation = observation
        .as_ref()
        .filter(|o| o.source_size == file.size && o.source_mtime == file.mtime)
        .map(|o| o.source_sha256.as_str());
    let prefs = state.transcode.lang_prefs().await;
    let defaults =
        super::dto::playback_defaults(&file.audio_streams, &file.subtitle_streams, &prefs);
    let preferred = preferred_subtitle(
        &file.subtitle_streams,
        &prefs.sub_lang,
        defaults.subtitle.selected_index,
    );
    let publications = state
        .store
        .list_subtitle_source_publications(id, file.size, file.mtime)
        .await?;
    let states: Vec<_> = file
        .subtitle_streams
        .iter()
        .map(|track| {
            (
                track.index,
                if current {
                    track_state(&file, track, &publications, attestation)
                } else {
                    "unknown"
                },
            )
        })
        .collect();
    let ready = states.iter().filter(|(_, s)| *s == "ready").count();
    let empty = states.iter().filter(|(_, s)| *s == "empty").count();
    let failed = states
        .iter()
        .filter(|(_, s)| matches!(*s, "failed" | "unsupported"))
        .count();
    let queue = analysis_fact(
        &history,
        "subtitle_source",
        fact("idle", "No active extraction request"),
    );
    let tracks: Vec<_> = file
        .subtitle_streams
        .iter()
        .zip(&states)
        .map(|(track, (_, status))| {
            json!({
                "index":track.index,"state":status,"configured_default":Some(track.index)==preferred
            })
        })
        .collect();
    let indicator = if current {
        subtitle_indicator(&states, preferred)
    } else {
        "unknown"
    };
    let subtitle = json!({"state":indicator,"preferred_index":preferred,"preferred_language":prefs.sub_lang,
        "ready":ready,"empty":empty,"failed":failed,"total":states.len(),"tracks":tracks,"work":queue});

    let probe = state.store.get_file_probe_json(id).await?;
    let chapters = super::dto::chapters_from_probe_json(probe.as_deref());
    let mut playback = if !crate::copyseg::supports(file.video_codec.as_deref()) {
        fact(
            "unsupported",
            "This codec uses another playback route; a VOD copy index is not applicable",
        )
    } else {
        let identities = crate::fragindex::video_identities(
            &file,
            probe.as_deref(),
            crate::ffmpeg::has_dovi_rpu().await,
            state.transcode.dv_convert_enabled().await,
        );
        let mut ready = 0;
        let mut refused = Vec::new();
        for video in &identities {
            let identity = crate::fragindex::identity_for(&file, *video);
            if state.store.fragment_index(id, &identity).await?.is_some()
                || state.transcode.cluster_index_available(&file, *video).await
            {
                ready += 1;
            } else if let Some(outcome) = state.store.fragment_index_outcome(id, &identity).await? {
                refused.push(outcome);
            }
        }
        if !identities.is_empty() && ready == identities.len() {
            fact(
                "ready",
                "VOD HLS indexes are ready for every required delivery route",
            )
        } else if let Some((terminal, reason)) = super::browse::index_refusal_summary(&refused) {
            fact(if terminal { "attention" } else { "pending" }, reason)
        } else {
            analysis_fact(
                &history,
                "fragment_index",
                fact(
                    "pending",
                    format!("{ready} of {} delivery routes prepared", identities.len()),
                ),
            )
        }
    };
    let annotation = state
        .store
        .timeline_annotation_set(id, &super::stream::annotation_source_identity(&file))
        .await?;
    let mut markers = if let Some(set) = annotation {
        fact(
            "done",
            if set.annotations.is_empty() {
                "Detection finished; no skip markers found".into()
            } else {
                format!("{} verified skip markers available", set.annotations.len())
            },
        )
    } else {
        analysis_fact(
            &history,
            "skip_markers",
            fact(
                "pending",
                "Intro, recap and credits detection has not completed",
            ),
        )
    };
    if !current {
        playback = fact("unknown", "File is missing or changed since its last scan");
        markers = playback.clone();
    }

    let metadata_fact = if item.artwork_error.is_some() {
        fact("attention", "The last artwork download failed")
    } else if matches!(
        history["library_kind"].as_str(),
        Some("home" | "recordings")
    ) {
        fact(
            "not_applicable",
            "Local media metadata; provider matching is not required",
        )
    } else if history["metadata_at"].as_i64().is_some_and(|v| v > 0) {
        fact(
            "done",
            format!(
                "Metadata matched · poster {} · backdrop {}",
                if item.poster_path.is_some() {
                    "available"
                } else {
                    "not supplied"
                },
                if item.backdrop_path.is_some() {
                    "available"
                } else {
                    "not supplied"
                }
            ),
        )
    } else {
        fact("pending", "Provider metadata has not been completed")
    };
    let conversion = state.store.dv_conversion(id).await?;
    let (modes, _, _) = state.jobs.dv_disk_settings_snapshot().await?;
    let mode = modes
        .get(&item.library_id)
        .map(|m| m.as_str())
        .unwrap_or("off");
    let dv = match conversion {
        Some(c) => {
            json!({"state":c.state,"detail":"On-disk Dolby Vision conversion","updated_at_ms":c.finished_at_ms.unwrap_or(c.queued_at_ms)})
        }
        None if file.dolby_vision.profile != Some(7) => fact(
            "not_applicable",
            "This file does not require Profile 7 conversion",
        ),
        None => fact(
            if mode == "off" { "off" } else { "on_demand" },
            "Optional on-disk Profile 7 → 8.1 conversion",
        ),
    };
    let automatic = state
        .store
        .get_setting("subtitles.automatic")
        .await?
        .as_deref()
        == Some("true");
    let thumbnails_enabled = plurx_core::store::stored_switch(
        state
            .store
            .get_setting(plurx_core::store::keys::CHAPTER_THUMBNAILS)
            .await?
            .as_deref(),
        true,
    );
    let search = crate::library_search::semantic::item_preparation(&state, item.id).await?;
    let versions = history["versions"].as_array().cloned().unwrap_or_default();
    let version_state = if versions.iter().any(|v| v["state"] == "running") {
        "running"
    } else if versions.iter().any(|v| v["state"] == "queued") {
        "queued"
    } else if versions.iter().any(|v| v["state"] == "cancelling") {
        "cancelling"
    } else if versions.first().is_some_and(|v| v["state"] == "failed") {
        "attention"
    } else if history["copies"].as_u64().unwrap_or(0) > 0 {
        "ready"
    } else {
        "on_demand"
    };
    let active = history["analysis"].as_array().is_some_and(|rows| {
        rows.iter().any(|r| {
            matches!(
                r["state"].as_str(),
                Some("queued" | "running" | "submitted")
            )
        })
    }) || matches!(version_state, "running" | "queued" | "cancelling")
        || matches!(
            dv["state"].as_str(),
            Some("queued" | "running" | "verified")
        );
    Ok(Json(
        json!({"file_id":id.to_string(),"checked_at_ms":crate::state::clock_ms(),"source_current":current,"active":active,
            "playback":playback,"subtitles":subtitle,"markers":markers,
            "probe":fact(if current&&file.probed{"done"}else{"attention"},if !current{"File is missing or changed since its scan"}else if file.probed{"Video, audio, subtitle tracks and chapters read"}else{"Media probing has not succeeded"}),
            "metadata":metadata_fact,"versions":fact(version_state,format!("{} published versions with recorded holders · quality and audio settings vary; copies are verified when used",history["copies"].as_u64().unwrap_or(0))),
            "thumbnails":fact(if !thumbnails_enabled{"off"}else if chapters.is_empty(){"not_applicable"}else{"on_demand"},format!("{} chapters · previews are generated when opened",chapters.len())),
            "conversion":dv,"search":search,
            "downloads":fact(if automatic{"enabled"}else{"off"},format!("{} downloaded tracks · online acquisition is separate from extraction",file.downloaded_subtitles.len()))
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn subtitle_indicator_follows_configured_default_before_all_tracks() {
        assert_eq!(
            subtitle_indicator(&[(0, "ready"), (1, "pending")], Some(0)),
            "done"
        );
        assert_eq!(
            subtitle_indicator(&[(0, "ready"), (1, "pending")], Some(1)),
            "pending"
        );
        assert_eq!(
            subtitle_indicator(&[(0, "ready"), (1, "pending")], None),
            "pending"
        );
        assert_eq!(
            subtitle_indicator(&[(0, "ready"), (1, "ready")], None),
            "done"
        );
        assert_eq!(
            subtitle_indicator(&[(0, "ready"), (1, "failed")], None),
            "attention"
        );
        assert_eq!(
            subtitle_indicator(&[(0, "ready"), (1, "failed")], Some(0)),
            "done"
        );
        assert_eq!(subtitle_indicator(&[(0, "unknown")], Some(0)), "unknown");
        assert_eq!(subtitle_indicator(&[], None), "none");
    }
    #[test]
    fn configured_language_matches_aliases_even_when_playback_defaults_to_off() {
        let track = |index, language: &str| SubtitleStream {
            index,
            codec: "subrip".into(),
            language: Some(language.into()),
            title: None,
            default: false,
            forced: false,
            hearing_impaired: false,
        };
        let tracks = vec![track(0, "fr"), track(1, "en")];
        assert_eq!(preferred_subtitle(&tracks, "eng", None), Some(1));
        assert_eq!(preferred_subtitle(&tracks, "jpn", Some(0)), None);
    }
    fn media_file(path: std::path::PathBuf) -> MediaFile {
        MediaFile {
            downloaded_subtitles: Vec::new(),
            id: 77,
            item_id: 1,
            path,
            size: 12_345,
            mtime: 678,
            duration_ms: Some(60_000),
            container: Some("mkv".into()),
            video_codec: Some("h264".into()),
            video_codec_tag: None,
            field_order: None,
            video_profile: None,
            width: Some(1920),
            height: Some(1080),
            bit_depth: Some(8),
            hdr: None,
            hdr_format: None,
            max_cll: None,
            max_fall: None,
            mastering_max_luminance: None,
            luminance_source: None,
            bitrate: Some(8_000_000),
            audio_streams: vec![],
            subtitle_streams: vec![],
            scanned_at: 0,
            audio_offset_ms: 0,
            probed: true,
            dolby_vision: Default::default(),
        }
    }

    #[test]
    fn styled_subtitles_require_both_attested_outputs() {
        let file = media_file("source.mkv".into());
        let track = SubtitleStream {
            index: 0,
            codec: "ass".into(),
            language: Some("eng".into()),
            title: None,
            default: true,
            forced: false,
            hearing_impaired: false,
        };
        let row = |format: &str, verdict: &str| SubtitleSourcePublication {
            file_id: file.id,
            source_size: file.size,
            source_mtime: file.mtime,
            source_attestation: "a".repeat(64),
            node_id: "node-a".into(),
            ordinal: 0,
            kind: "text_styled".into(),
            format: format.into(),
            verdict: verdict.into(),
            attempts: 1,
            origin: "extracted".into(),
            sha256: "b".repeat(64),
            bytes: 10,
            published_at_ms: 1,
        };
        let digest = "a".repeat(64);
        let mut rows = vec![row("matroska", "kept")];
        assert_eq!(track_state(&file, &track, &rows, Some(&digest)), "pending");
        rows.push(row("webvtt", "malformed"));
        assert_eq!(track_state(&file, &track, &rows, Some(&digest)), "failed");
        rows[1] = row("webvtt", "kept");
        assert_eq!(track_state(&file, &track, &rows, Some(&digest)), "ready");
        assert_eq!(track_state(&file, &track, &rows, None), "unknown");
        assert_eq!(
            track_state(&file, &track, &rows, Some(&"c".repeat(64))),
            "pending"
        );
        rows[0].source_mtime += 1;
        assert_eq!(track_state(&file, &track, &rows, Some(&digest)), "pending");
    }
    #[test]
    fn completed_or_absent_analysis_is_not_active_work() {
        for history in [
            json!({"analysis":[]}),
            json!({"analysis":[{"component":"subtitle_source","state":"ready"}]}),
        ] {
            assert_eq!(
                analysis_fact(
                    &history,
                    "subtitle_source",
                    fact("idle", "No active request")
                )["state"],
                "idle"
            );
        }
        assert_eq!(
            analysis_fact(
                &json!({"analysis":[{"component":"subtitle_source","state":"running"}]}),
                "subtitle_source",
                fact("idle", "No active request")
            )["state"],
            "running"
        );
    }
}
