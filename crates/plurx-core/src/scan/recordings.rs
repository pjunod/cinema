//! The `recordings` library arm: what the DVR wrote, read back as media.
//!
//! Disk is the truth here, exactly as it is for home video, and for the same
//! reason — there is no provider to ask. The difference is that plurx wrote
//! these files itself, so beside every recording is a sidecar carrying what
//! the guide said when the capture was scheduled: the programme, the channel,
//! the episode, what was missed. Reading that back is strictly better than
//! re-deriving it from a filename.
//!
//! **No provider lookup, ever.** A local news bulletin called *Eyewitness
//! News* matches a dozen things in TMDB and is none of them; a recording that
//! silently acquired a stranger's poster and overview would be worse than one
//! with no artwork at all.
//!
//! One rule shapes the code more than any other: **an item's title has to be
//! computable from its path alone, identically on every scan.** Item identity
//! is `(library, parent, kind, title)`, so a title derived from anything that
//! can change — or that a later patch rewrites — makes the next rescan fail to
//! recognise the file and create a second item beside the first.

use std::path::Path;

use serde::Deserialize;

use super::home;
use crate::domain::{ItemKind, Library, MetadataPatch, NewItem, ProbeResult};
use crate::error::StoreError;
use crate::store::PublicationStore;

/// What the engine writes beside each capture. Only the fields the library
/// needs are read; the rest of the document is for a person with a text
/// editor and for whatever the DVR grows later.
#[derive(Debug, Default, Deserialize)]
pub struct Sidecar {
    #[serde(default)]
    pub plurx_dvr: u32,
    #[serde(default)]
    pub recording_id: Option<String>,
    #[serde(default)]
    pub channel: Option<SidecarChannel>,
    #[serde(default)]
    pub programme: Option<SidecarProgramme>,
    #[serde(default)]
    pub airing: Option<SidecarSpan>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub gap_s: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
pub struct SidecarChannel {
    #[serde(default)]
    pub guide_number: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct SidecarProgramme {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub episode_title: Option<String>,
    /// `S3E14`, as the guide normalises it.
    #[serde(default)]
    pub episode: Option<String>,
    #[serde(default)]
    pub synopsis: Option<String>,
    #[serde(default)]
    pub original_air_date: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct SidecarSpan {
    #[serde(default)]
    pub start: Option<i64>,
    #[serde(default)]
    pub end: Option<i64>,
}

pub fn sidecar_for(media: &Path) -> std::path::PathBuf {
    media.with_extension("json")
}

pub fn read_sidecar(media: &Path) -> Option<Sidecar> {
    let raw = std::fs::read(sidecar_for(media)).ok()?;
    let parsed = serde_json::from_slice::<Sidecar>(&raw).ok()?;
    // A document that does not claim to be ours is not ours. `.json` beside a
    // video is a common enough convention that guessing would eventually read
    // somebody else's file as a recording.
    (parsed.plurx_dvr == 1).then_some(parsed)
}

/// The item title for one recording file, from its path alone.
///
/// The engine names files `<Title> - <YYYY-MM-DD HHMM> - <channel> - <id8>`.
/// The folder above already carries the programme title, so the item keeps the
/// part that distinguishes one airing from another and drops the channel and
/// the id, which are diagnostics rather than names.
///
/// Derived from the path and nothing else, because identity depends on it: a
/// title taken from the sidecar and then rewritten by a later patch would make
/// the next scan create a second item for the same file.
pub fn item_title(path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let parts = stem.split(" - ").collect::<Vec<_>>();
    if parts.len() >= 4 {
        // Drop the trailing channel and id, keep everything before them —
        // a programme title containing " - " survives, because only the last
        // two segments are removed.
        parts[..parts.len() - 2].join(" - ")
    } else {
        stem.to_owned()
    }
}

/// Mirror the DVR root's folders and find-or-create the item for this file.
///
/// The directory tree is one folder per programme, which is what the engine
/// writes, so the same folder mirroring home video uses gives exactly the
/// shape this library wants.
pub async fn place(
    store: &PublicationStore<'_>,
    library: &Library,
    path: &Path,
) -> Result<Option<home::Placed>, StoreError> {
    let Some(dirs) = home::relative_dirs(library, path) else {
        return Ok(None);
    };
    let mut parent: Option<i64> = None;
    for dir in dirs {
        parent = Some(home::find_or_create_folder(store, library, parent, &dir).await?);
    }
    let title = item_title(path);
    if let Some(existing) = store
        .find_child_item(library.id, parent, ItemKind::Video, &title)
        .await?
    {
        return Ok(Some(home::Placed {
            id: existing.id,
            created: false,
        }));
    }
    // Season and episode are set at insert, not patched: `MetadataPatch`
    // carries no episode numbering, and these are facts the guide already
    // stated rather than anything an agent would revise.
    let (season_number, episode_number) = read_sidecar(path)
        .and_then(|sidecar| sidecar.programme?.episode)
        .and_then(|episode| parse_season_episode(&episode))
        .map(|(season, episode)| (Some(season), Some(episode)))
        .unwrap_or((None, None));
    let id = store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Video,
            parent_id: parent,
            title,
            year: None,
            season_number,
            episode_number,
        })
        .await?;
    Ok(Some(home::Placed { id, created: true }))
}

/// `S3E14` → `(3, 14)`. The guide's own normalisation, read back.
pub fn parse_season_episode(value: &str) -> Option<(i32, i32)> {
    let upper = value.trim().to_ascii_uppercase();
    let rest = upper.strip_prefix('S')?;
    let (season, episode) = rest.split_once('E')?;
    Some((season.parse().ok()?, episode.parse().ok()?))
}

/// Everything after the file's row is written: apply what the sidecar said,
/// and tell the DVR which item and file its recording became.
///
/// Idempotent by construction — every value comes from an immutable sidecar,
/// so a rescan writes the same patch — but guarded anyway on the store's own
/// "don't rewrite an unchanged value" habit, because on the replicated backend
/// every write is a consensus round.
pub async fn after_record(
    store: &PublicationStore<'_>,
    placed: home::Placed,
    file_id: i64,
    path: &Path,
    probe: &ProbeResult,
    mtime: i64,
    now_ms: i64,
) -> Result<bool, StoreError> {
    let Some(sidecar) = read_sidecar(path) else {
        return Ok(false);
    };
    let programme = sidecar.programme.unwrap_or_default();
    let channel = sidecar.channel.unwrap_or_default();
    let recorded_at = sidecar
        .airing
        .and_then(|airing| airing.start)
        .map(home::date_from_unix)
        .or_else(|| (mtime > 0).then(|| home::date_from_unix(mtime)));

    let mut tags = vec!["dvr".to_owned()];
    if let Some(name) = channel.name.clone() {
        tags.push(name);
    } else if let Some(number) = channel.guide_number.clone() {
        tags.push(number);
    }
    // A partial recording is worth saying so on the item itself: someone
    // deciding whether to watch it should not have to open the sidecar to
    // find out that four minutes are missing.
    if sidecar.state.as_deref() == Some("partial") {
        tags.push("partial".to_owned());
    }

    let overview = match (programme.synopsis.clone(), programme.episode_title.clone()) {
        (Some(synopsis), _) => Some(synopsis),
        // An episode title is not an overview, but it is the only sentence
        // some broadcasts supply, and an empty description reads as an error.
        (None, Some(episode_title)) => Some(episode_title),
        (None, None) => None,
    };

    let patch = MetadataPatch {
        overview,
        air_date: programme.original_air_date.clone(),
        recorded_at,
        runtime_ms: probe.duration_ms,
        tags: Some(tags),
        ..Default::default()
    };
    if !patch.is_empty() {
        store.apply_metadata(placed.id, &patch).await?;
    }
    if let Some(recording_id) = sidecar.recording_id {
        // The shelf joins on this. Without it a finished capture is a file in
        // a library with no way back to the row that asked for it, so "play
        // this recording" would have to match on a path.
        //
        // The row must already name this exact file. A sidecar is a document
        // in a folder an operator can write to, and without the check anyone
        // who could drop a file into a recordings library could re-point any
        // recording's media at it.
        let owns_this_file = store
            .get_dvr_recording(&recording_id)
            .await?
            .and_then(|row| row.path)
            .is_some_and(|stored| std::path::Path::new(&stored) == path);
        if owns_this_file {
            store
                .link_dvr_recording_media(&recording_id, placed.id, file_id, now_ms)
                .await?;
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_items_title_comes_from_its_path_and_only_its_path() {
        let title = item_title(Path::new(
            "/20t/dvr/Kitchen Table/Kitchen Table - 2026-09-10 0040 - 7.1 - abcdef01.ts",
        ));
        assert_eq!(
            title, "Kitchen Table - 2026-09-10 0040",
            "the channel and the id are diagnostics, not part of the name"
        );
    }

    #[test]
    fn a_programme_title_containing_a_dash_survives_the_trim() {
        let title = item_title(Path::new(
            "/20t/dvr/Law - Order/Law - Order - 2026-09-10 0040 - 7.1 - abcdef01.ts",
        ));
        assert_eq!(title, "Law - Order - 2026-09-10 0040");
    }

    #[test]
    fn a_file_the_dvr_did_not_name_keeps_its_own_stem() {
        assert_eq!(item_title(Path::new("/20t/dvr/loose.ts")), "loose");
    }

    #[test]
    fn two_airings_of_one_programme_are_two_items() {
        let first = item_title(Path::new(
            "/x/Kitchen Table - 2026-09-10 0040 - 7.1 - aaaa.ts",
        ));
        let second = item_title(Path::new(
            "/x/Kitchen Table - 2026-09-17 0040 - 7.1 - bbbb.ts",
        ));
        assert_ne!(
            first, second,
            "identity is per airing; a weekly series must not collapse into one row"
        );
    }

    #[test]
    fn a_json_file_that_does_not_claim_to_be_ours_is_not_read_as_a_sidecar() {
        let directory = tempfile::tempdir().expect("temp");
        let media = directory.path().join("clip.ts");
        std::fs::write(&media, b"x").expect("media");
        std::fs::write(sidecar_for(&media), br#"{"title":"someone else's"}"#).expect("json");
        assert!(
            read_sidecar(&media).is_none(),
            "`.json` beside a video is a common convention; guessing would eventually \
             read a stranger's file as a recording"
        );
        std::fs::write(
            sidecar_for(&media),
            br#"{"plurx_dvr":1,"recording_id":"r1","programme":{"title":"Kitchen Table"}}"#,
        )
        .expect("json");
        let parsed = read_sidecar(&media).expect("ours");
        assert_eq!(parsed.recording_id.as_deref(), Some("r1"));
    }

    #[test]
    fn the_guides_episode_spelling_reads_back_as_numbers() {
        assert_eq!(parse_season_episode("S3E14"), Some((3, 14)));
        assert_eq!(parse_season_episode("s03e07"), Some((3, 7)));
        assert_eq!(parse_season_episode("E14"), None, "no season, no placement");
        assert_eq!(parse_season_episode("nonsense"), None);
    }
}
