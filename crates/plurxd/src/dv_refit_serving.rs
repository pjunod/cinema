//! F+ curves on the copy path as the daemon serves them
//! (`docs/streaming/FEL-RPU-REFIT-BUILD.md` §3.5): which curve digest a
//! session pins, which tables an index pass needs, and the one adapter from
//! the store's curve table to the serving table.
//!
//! - **Enables, not gates.** One setting per [`ClientPlatform`], absent is
//!   off. A session on a disabled platform, a title with no fitted table, and
//!   every copy that does not convert serve identity curves under the v1
//!   identity, exactly as before F+.
//! - **Pinned per session.** A chosen digest rides in the copy options; each
//!   producer loads the table by that digest ([`pinned_table`]). A newer fit
//!   reaches only a later generation that deliberately chooses it.
//! - **Platform.** The control session's typed [`ClientPlatform`] selects the
//!   enable. The diagnostic `client.kind` string grants no serving policy.
use std::collections::BTreeMap;
use std::sync::Arc;

use plurx_core::domain::MediaFile;
use plurx_core::store::dv_refit_jobs::{RefitCurveTable, RefitVerdict};
use plurx_core::store::Store;
use plurx_core::transcode::dv_refit_serve::ServedCurveTable;
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

/// Status-only fact read with the exact curve table selected for a rendition.
/// This never contributes to a recipe, index, or output byte identity.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PinnedRefitGain {
    pub digest: [u8; 32],
    pub gain: Option<f64>,
}

struct FittedSelection {
    table: Arc<ServedCurveTable>,
    fact: PinnedRefitGain,
}

fn fitted_selection(table: &RefitCurveTable) -> Result<Option<FittedSelection>, String> {
    let Some(served) = served_table(table)? else {
        return Ok(None);
    };
    let digest =
        digest_bytes(served.digest()).ok_or("the fitted F+ curve table has an invalid digest")?;
    Ok(Some(FittedSelection {
        table: Arc::new(served),
        fact: PinnedRefitGain {
            digest,
            gain: table.title.gain,
        },
    }))
}

/// The one adapter from the store's curve table: `None` unless the title
/// serves fitted curves in at least one scene.
pub(crate) fn served_table(table: &RefitCurveTable) -> Result<Option<ServedCurveTable>, String> {
    if table.title.verdict != RefitVerdict::Fitted {
        return Ok(None);
    }
    let served = ServedCurveTable::from_servable(
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
    Ok(current_selection(store, file)
        .await?
        .map(|selection| selection.table))
}

async fn current_selection(
    store: &dyn Store,
    file: &MediaFile,
) -> Result<Option<FittedSelection>, String> {
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
    fitted_selection(&table)
}

/// The table a pinned digest names, or `None` when the store no longer
/// keeps it (milestone 3 keeps superseded tables for seven days).
///
pub(crate) async fn pinned_table(
    store: &dyn Store,
    file: &MediaFile,
    digest: [u8; 32],
) -> Result<Option<Arc<ServedCurveTable>>, String> {
    Ok(pinned_selection(store, file, digest)
        .await?
        .map(|selection| selection.table))
}

async fn pinned_selection(
    store: &dyn Store,
    file: &MediaFile,
    digest: [u8; 32],
) -> Result<Option<FittedSelection>, String> {
    let table = store
        .dv_refit_curves_by_digest(file.id, &digest_hex(digest))
        .await
        .map_err(|error| error.to_string())?;
    match table {
        Some(table) => {
            let selection =
                fitted_selection(&table)?.ok_or("a pinned F+ table has no fitted scenes")?;
            if selection.fact.digest != digest {
                return Err("the pinned F+ table's digest disagrees with its identity".into());
            }
            Ok(Some(selection))
        }
        None => Ok(None),
    }
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
    match current_table(store, file).await? {
        Some(table) => digest_bytes(table.digest())
            .map(Some)
            .ok_or_else(|| "the fitted F+ table has an invalid digest".into()),
        None => Ok(None),
    }
}

/// Resolve the fitted table only from the predecessor's accepted typed
/// control platform. An initial create with no such control stays on v1;
/// the next session boundary can pin the current fit.
pub(crate) async fn pin_for_session(
    store: &dyn Store,
    file: &MediaFile,
    request: &mut crate::transcode::SessionRequest,
    platform: Option<ClientPlatform>,
) -> Result<(), String> {
    let converts = matches!(
        request.kind,
        crate::transcode::SessionKind::Copy {
            convert_dolby_vision: true,
            ..
        }
    );
    let Some(platform) = platform.filter(|_| converts) else {
        request.dv_refit_digest = None;
        return Ok(());
    };
    let key = setting_key(platform);
    let settings = store
        .get_settings(&[key])
        .await
        .map_err(|error| error.to_string())?;
    let enabled = enabled_in(&settings, platform);
    request.dv_refit_digest = session_digest(store, file, converts, enabled)
        .await?
        .map(digest_hex);
    Ok(())
}

/// The copy options and pinned table a VOD rendition serves for a session
/// that pinned `pinned` (its request's digest).
///
/// The pinned table is loaded by its digest. When the store no longer keeps
/// it (superseded more than seven days ago, which takes a fit-version or
/// helper change), the rendition re-pins to the current fit: this rendition
/// is a new generation boundary for the session, and its identity names what
/// it serves. A read failure refuses the rendition before publication.
pub(crate) async fn rendition_refit(
    store: &dyn Store,
    file: &MediaFile,
    video: CopyVideoOptions,
    pinned: Option<&str>,
) -> Result<(CopyVideoOptions, Option<PinnedRefitGain>), String> {
    let Some(digest) = pinned
        .and_then(digest_bytes)
        .filter(|_| video.converts_dolby_vision())
    else {
        return Ok((video.with_dolby_vision_refit(None), None));
    };
    let selection = match pinned_selection(store, file, digest).await {
        Ok(Some(table)) => Some(table),
        Ok(None) => {
            let current = current_selection(store, file).await?;
            tracing::info!(
                file_id = file.id,
                pinned = %digest_hex(digest),
                current = current.as_ref().map(|selection| selection.table.digest()),
                "the session's F+ curves are no longer kept; re-pinning at this rendition"
            );
            current
        }
        Err(error) => return Err(format!("reading the pinned F+ curve table: {error}")),
    };
    match selection {
        Some(selection) => Ok((
            video.with_dolby_vision_refit(Some(selection.fact.digest)),
            Some(selection.fact),
        )),
        None => Ok((video.with_dolby_vision_refit(None), None)),
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
    match current_table(store, file).await? {
        Some(table) => digest_bytes(table.digest())
            .map(Some)
            .ok_or_else(|| "the fitted F+ table has an invalid digest".into()),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
