//! F+ curves on the copy path as the daemon serves them
//! (`docs/streaming/FEL-RPU-REFIT-BUILD.md` §3.5): which curve digest a
//! session pins, which tables an index pass needs, and the one adapter from
//! the store's curve table to the serving table.
//!
//! - **Enables, not gates.** One setting per [`ClientPlatform`], absent is
//!   off. A session on a disabled platform, a title with no fitted table, and
//!   every copy that does not convert serve identity curves under the v1
//!   identity, exactly as before F+.
//! - **Pinned per session.** A session's digest is chosen once, when it is
//!   created, and rides in its request ([`crate::transcode::SessionRequest`])
//!   and its copy options; every producer loads the table that digest names
//!   ([`pinned_table`]). A refit published later reaches only later sessions.
//! - **Platform.** At create the only platform the server has is the
//!   capability document's `client.kind`, mapped onto [`ClientPlatform`] by
//!   [`platform_of_caps`]. A session with no document has no platform, so no
//!   enable applies to it.
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use plurx_core::domain::MediaFile;
use plurx_core::store::dv_refit_jobs::{RefitCurveTable, RefitVerdict};
use plurx_core::store::Store;
use plurx_core::transcode::dv_refit_serve::{ServedCurveTable, SourceKeyframe};
use plurx_core::transcode::CopyVideoOptions;

use crate::playback_control::ClientPlatform;

/// Per-platform enables (registered by milestone 3). Absent is off.
pub(crate) const REFIT_WEB: &str = "playback.dolby_vision_refit_web";
pub(crate) const REFIT_APPLE: &str = "playback.dolby_vision_refit_apple";
pub(crate) const REFIT_ANDROID: &str = "playback.dolby_vision_refit_android";
pub(crate) const REFIT_LINUX: &str = "playback.dolby_vision_refit_linux";
pub(crate) const REFIT_KEYS: [&str; 4] = [REFIT_WEB, REFIT_APPLE, REFIT_ANDROID, REFIT_LINUX];

/// The enable that governs sessions on `platform`.
pub(crate) fn setting_key(platform: ClientPlatform) -> &'static str {
    match platform {
        ClientPlatform::Web => REFIT_WEB,
        ClientPlatform::Apple => REFIT_APPLE,
        ClientPlatform::Android => REFIT_ANDROID,
        ClientPlatform::Linux => REFIT_LINUX,
    }
}

/// The platform a session is created for, from its capability document's
/// `client.kind`: the web player says `web`, the Apple app `ios` or `tvos`,
/// Android `android`, plurx Player `linux-player`. Anything else, or no
/// document, is no platform.
pub(crate) fn platform_of_caps(
    caps: Option<&plurx_core::playback::DeviceCaps>,
) -> Option<ClientPlatform> {
    match caps?.client.as_ref()?.kind.as_str() {
        "web" => Some(ClientPlatform::Web),
        "ios" | "tvos" => Some(ClientPlatform::Apple),
        "android" => Some(ClientPlatform::Android),
        "linux-player" => Some(ClientPlatform::Linux),
        _ => None,
    }
}

/// Whether `platform`'s enable is on in `settings`.
pub(crate) fn enabled_in(settings: &BTreeMap<String, String>, platform: ClientPlatform) -> bool {
    plurx_core::store::stored_switch(
        settings.get(setting_key(platform)).map(String::as_str),
        false,
    )
}

/// Whether any platform's enable is on: the indexer builds a fitted title's
/// v2 index only then.
pub(crate) async fn any_enabled(store: &dyn Store) -> Result<bool, String> {
    let settings = store
        .get_settings(&REFIT_KEYS)
        .await
        .map_err(|error| error.to_string())?;
    Ok(REFIT_KEYS
        .iter()
        .any(|key| plurx_core::store::stored_switch(settings.get(*key).map(String::as_str), false)))
}

/// A curve digest as the store spells it (64 lowercase hex), as bytes.
pub(crate) fn digest_bytes(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (slot, pair) in out.iter_mut().zip(text.as_bytes().chunks(2)) {
        let pair = std::str::from_utf8(pair).ok()?;
        if pair.bytes().any(|b| b.is_ascii_uppercase()) {
            return None;
        }
        *slot = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(out)
}

pub(crate) fn digest_hex(digest: [u8; 32]) -> String {
    hex::encode(digest)
}

/// The one adapter from the store's curve table: `None` unless the title
/// serves fitted curves in at least one scene.
pub(crate) fn served_table(table: &RefitCurveTable) -> Result<Option<ServedCurveTable>, String> {
    if table.title.verdict != RefitVerdict::Fitted {
        return Ok(None);
    }
    let served = ServedCurveTable::new(
        table.title.curve_digest.clone(),
        table.scenes.iter().map(|scene| {
            (
                scene.first_pts,
                scene.end_pts,
                scene.time_base.as_str(),
                scene.curves.as_ref(),
            )
        }),
    )
    .map_err(|error| format!("the F+ curve table cannot be served: {error}"))?;
    Ok(served.fitted().then_some(served))
}

/// The file's current fitted table, when it has one.
pub(crate) async fn current_table(
    store: &dyn Store,
    file: &MediaFile,
) -> Result<Option<Arc<ServedCurveTable>>, String> {
    if !plurx_core::playback::file_can_convert_to_p81(file) {
        return Ok(None);
    }
    let Some(table) = store
        .dv_refit_curves(file.id)
        .await
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    Ok(served_table(&table)?.map(Arc::new))
}

/// The table a pinned digest names, or `None` when the store no longer
/// keeps it (milestone 3 keeps superseded tables for seven days).
///
/// Until milestone 3's `dv_refit_curves_by_digest` lands this answers from
/// the current table only, which is the same answer for every session
/// pinned to the current fit.
pub(crate) async fn pinned_table(
    store: &dyn Store,
    file: &MediaFile,
    digest: [u8; 32],
) -> Result<Option<Arc<ServedCurveTable>>, String> {
    Ok(current_table(store, file)
        .await?
        .filter(|table| digest_bytes(table.digest()) == Some(digest)))
}

/// The digest a new session pins: the current fitted table's, when the
/// session converts and its platform's enable is on.
pub(crate) async fn session_digest(
    store: &dyn Store,
    file: &MediaFile,
    converts: bool,
    enabled: bool,
) -> Result<Option<[u8; 32]>, String> {
    if !(converts && enabled) {
        return Ok(None);
    }
    Ok(current_table(store, file)
        .await?
        .and_then(|table| digest_bytes(table.digest())))
}

/// The copy options and pinned table a VOD rendition serves for a session
/// that pinned `pinned` (its request's digest).
///
/// The pinned table is loaded by its digest. When the store no longer keeps
/// it (superseded more than seven days ago, which takes a fit-version or
/// helper change), the rendition re-pins to the current fit: this rendition
/// is a new generation boundary for the session, and its identity names what
/// it serves. A read failure serves identity curves under the v1 identity,
/// which is always consistent.
pub(crate) async fn rendition_refit(
    store: &dyn Store,
    file: &MediaFile,
    video: CopyVideoOptions,
    pinned: Option<&str>,
) -> (CopyVideoOptions, Option<Arc<ServedCurveTable>>) {
    let Some(digest) = pinned
        .and_then(digest_bytes)
        .filter(|_| video.converts_dolby_vision())
    else {
        return (video.with_dolby_vision_refit(None), None);
    };
    let table = match pinned_table(store, file, digest).await {
        Ok(Some(table)) => Some(table),
        Ok(None) => match current_table(store, file).await {
            Ok(current) => {
                tracing::info!(
                    file_id = file.id,
                    pinned = %digest_hex(digest),
                    current = current.as_ref().map(|table| table.digest()),
                    "the session's F+ curves are no longer kept; re-pinning at this rendition"
                );
                current
            }
            Err(error) => {
                tracing::warn!(file_id = file.id, %error, "reading the current F+ curve table");
                None
            }
        },
        Err(error) => {
            tracing::warn!(file_id = file.id, %error, "reading the pinned F+ curve table");
            None
        }
    };
    match table
        .as_ref()
        .and_then(|table| digest_bytes(table.digest()))
    {
        Some(digest) => (video.with_dolby_vision_refit(Some(digest)), table),
        None => (video.with_dolby_vision_refit(None), None),
    }
}

/// The tables an index pass's identities name, by digest.
#[derive(Debug, Clone, Default)]
pub(crate) struct RefitTables(Vec<Arc<ServedCurveTable>>);

impl RefitTables {
    pub(crate) fn none() -> Self {
        Self::default()
    }

    pub(crate) fn get(&self, digest: [u8; 32]) -> Option<Arc<ServedCurveTable>> {
        self.0
            .iter()
            .find(|table| digest_bytes(table.digest()) == Some(digest))
            .cloned()
    }

    /// Loads the table of every identity in `videos` that writes curves. An
    /// identity whose table is no longer kept is left out, and its pass
    /// refuses it.
    pub(crate) async fn for_videos(
        store: &dyn Store,
        file: &MediaFile,
        videos: &[CopyVideoOptions],
    ) -> Result<Self, String> {
        let mut tables = Vec::new();
        for digest in videos.iter().filter_map(|video| video.dolby_vision_refit()) {
            if tables
                .iter()
                .any(|table: &Arc<ServedCurveTable>| digest_bytes(table.digest()) == Some(digest))
            {
                continue;
            }
            if let Some(table) = pinned_table(store, file, digest).await? {
                tables.push(table);
            }
        }
        Ok(Self(tables))
    }
}

/// The digest the indexer builds a v2 identity for: the current fitted
/// table's, when the node converts and any platform's enable is on.
pub(crate) async fn index_digest(
    store: &dyn Store,
    file: &MediaFile,
    convert: bool,
) -> Result<Option<[u8; 32]>, String> {
    if !convert
        || !plurx_core::playback::file_can_convert_to_p81(file)
        || !any_enabled(store).await?
    {
        return Ok(None);
    }
    Ok(current_table(store, file)
        .await?
        .and_then(|table| digest_bytes(table.digest())))
}

/// The F+ curve digest a session on `platform` pins, chosen once at create
/// from the plan's conversion and the platform's enable. A read failure
/// pins nothing, so the session serves identity curves under v1.
pub(crate) async fn pin_for_session(
    store: &dyn Store,
    settings: &BTreeMap<String, String>,
    file: &MediaFile,
    request: &mut crate::transcode::SessionRequest,
    platform: Option<ClientPlatform>,
) {
    let converts = matches!(
        request.kind,
        crate::transcode::SessionKind::Copy {
            convert_dolby_vision: true,
            ..
        }
    );
    let enabled = platform.is_some_and(|platform| enabled_in(settings, platform));
    request.dv_refit = match session_digest(store, file, converts, enabled).await {
        Ok(digest) => digest.map(digest_hex),
        Err(error) => {
            tracing::warn!(file_id = file.id, %error, "reading the F+ curve table for a new session");
            None
        }
    };
}

/// How many packets the keyframe probe reads: enough to pass any leading
/// non-keyframes a remux opens with.
const KEYFRAME_PROBE_PACKETS: u32 = 64;
const KEYFRAME_PROBE_MAX_BYTES: u64 = 256 * 1024;

/// The held source's first video keyframe in `stream_index`, from ffprobe:
/// the anchor every converting copy of this source places its timeline by.
pub(crate) async fn source_keyframe(
    source: &std::fs::File,
    stream_index: u32,
    timeout: Duration,
) -> Result<SourceKeyframe, String> {
    use std::io::{Seek, SeekFrom};
    let mut view = source;
    view.seek(SeekFrom::Start(0))
        .map_err(|error| format!("positioning the held source for the keyframe probe: {error}"))?;
    let result = crate::ffmpeg::held_source_packet_probe_json(
        source,
        crate::ffmpeg::HeldPacketProbeRequest {
            stream_index,
            start_seconds: None,
            packet_limit: Some(KEYFRAME_PROBE_PACKETS),
        },
        timeout,
        KEYFRAME_PROBE_MAX_BYTES,
    )
    .await;
    view.seek(SeekFrom::Start(0))
        .map_err(|error| format!("resetting the held source after the keyframe probe: {error}"))?;
    let raw = result.map_err(|error| format!("keyframe probe: {}", error.reason()))?;
    let document: serde_json::Value =
        serde_json::from_slice(&raw).map_err(|error| format!("keyframe probe JSON: {error}"))?;
    SourceKeyframe::from_ffprobe(&document).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_platform_comes_from_the_documents_client_kind() {
        let caps = |kind: &str| plurx_core::playback::DeviceCaps {
            client: Some(plurx_core::playback::caps::ClientInfo {
                kind: kind.into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            platform_of_caps(Some(&caps("web"))),
            Some(ClientPlatform::Web)
        );
        assert_eq!(
            platform_of_caps(Some(&caps("ios"))),
            Some(ClientPlatform::Apple)
        );
        assert_eq!(
            platform_of_caps(Some(&caps("tvos"))),
            Some(ClientPlatform::Apple)
        );
        assert_eq!(
            platform_of_caps(Some(&caps("android"))),
            Some(ClientPlatform::Android)
        );
        assert_eq!(
            platform_of_caps(Some(&caps("linux-player"))),
            Some(ClientPlatform::Linux)
        );
        assert_eq!(platform_of_caps(Some(&caps("macos"))), None);
        assert_eq!(platform_of_caps(None), None);
        assert_eq!(
            platform_of_caps(Some(&plurx_core::playback::DeviceCaps::default())),
            None
        );
    }

    #[test]
    fn each_platform_reads_its_own_enable_and_absent_is_off() {
        let mut settings = BTreeMap::new();
        for platform in [
            ClientPlatform::Web,
            ClientPlatform::Apple,
            ClientPlatform::Android,
            ClientPlatform::Linux,
        ] {
            assert!(!enabled_in(&settings, platform));
        }
        settings.insert(REFIT_APPLE.to_owned(), "true".to_owned());
        assert!(enabled_in(&settings, ClientPlatform::Apple));
        assert!(!enabled_in(&settings, ClientPlatform::Web));
        assert_eq!(setting_key(ClientPlatform::Linux), REFIT_LINUX);
    }

    #[test]
    fn digests_round_trip_and_refuse_other_spellings() {
        let text = "0123456789abcdef".repeat(4);
        let bytes = digest_bytes(&text).expect("a digest");
        assert_eq!(digest_hex(bytes), text);
        assert_eq!(digest_bytes(&text.to_uppercase()), None);
        assert_eq!(digest_bytes(&text[1..]), None);
        assert_eq!(digest_bytes(&"g".repeat(64)), None);
    }
}
