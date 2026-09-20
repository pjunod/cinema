//! Deterministic catalogue-only planning for duplicate show identity repair.
//!
//! The database backends produce one bounded [`IdentityRepairSnapshot`]. This
//! module never reads storage or mutates rows: it validates the snapshot and
//! turns it into the complete set of moves, watch copies, retirements and
//! blockers that an administrator approves.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::StoreError;

pub const IDENTITY_REPAIR_SHOWS_MIN: usize = 2;
pub const IDENTITY_REPAIR_SHOWS_MAX: usize = 32;
pub const IDENTITY_REPAIR_SEASONS_MAX: usize = 512;
pub const IDENTITY_REPAIR_EPISODES_MAX: usize = 4_096;
pub const IDENTITY_REPAIR_FILES_MAX: usize = 8_192;
pub const IDENTITY_REPAIR_WATCHES_MAX: usize = 16_384;
pub const IDENTITY_REPAIR_PLAN_BYTES_MAX: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct IdentityRepairItem {
    pub id: i64,
    pub library_id: i64,
    pub kind: String,
    pub parent_id: Option<i64>,
    pub tmdb_id: Option<i64>,
    pub season_number: Option<i64>,
    pub episode_number: Option<i64>,
    pub added_at: i64,
    /// Canonical JSON array of every stored item column. It is part of the
    /// preimage even when the planner does not interpret the field.
    pub row: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct IdentityRepairFile {
    pub id: i64,
    pub item_id: i64,
    pub path: String,
    pub size: i64,
    pub mtime: i64,
    /// Canonical JSON array of every stored file column.
    pub row: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct IdentityRepairWatch {
    pub user_id: i64,
    pub item_id: i64,
    pub position_ms: i64,
    pub duration_ms: Option<i64>,
    pub watched: bool,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct IdentityRepairBlocker {
    pub code: String,
    pub detail: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct IdentityRepairSnapshot {
    pub library_id: i64,
    pub library_kind: String,
    pub library_paths: String,
    pub input_show_ids: Vec<i64>,
    pub items: Vec<IdentityRepairItem>,
    pub files: Vec<IdentityRepairFile>,
    pub watches: Vec<IdentityRepairWatch>,
    /// Owners discovered from exact parser-derived directory evidence.
    pub directory_owner_ids: Vec<i64>,
    /// Canonical rows from every dependency class checked by the backend.
    /// Empty derived classification rows are intentionally omitted.
    pub dependency_rows: Vec<String>,
    pub blockers: Vec<IdentityRepairBlocker>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct IdentityRepairItemMove {
    pub item_id: i64,
    pub expected_parent_id: i64,
    pub new_parent_id: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct IdentityRepairFileMove {
    pub file_id: i64,
    pub expected_item_id: i64,
    pub new_item_id: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct IdentityRepairWatchCopy {
    pub user_id: i64,
    pub source_item_id: i64,
    pub destination_item_id: i64,
    pub state: IdentityRepairWatch,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct IdentityRepairWatchConflict {
    pub user_id: i64,
    pub source_item_id: i64,
    pub destination_item_id: i64,
    pub source: IdentityRepairWatch,
    pub destination: IdentityRepairWatch,
    pub resolution: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct IdentityRepairCounts {
    pub shows: usize,
    pub seasons: usize,
    pub episodes: usize,
    pub files: usize,
    pub watches: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct IdentityRepairPlan {
    pub version: u32,
    pub library_id: i64,
    pub source_directory: Option<String>,
    pub survivor_show_id: Option<i64>,
    pub input_show_ids: Vec<i64>,
    pub item_moves: Vec<IdentityRepairItemMove>,
    pub file_moves: Vec<IdentityRepairFileMove>,
    pub retired_item_ids: Vec<i64>,
    pub watch_copies: Vec<IdentityRepairWatchCopy>,
    pub watch_conflicts: Vec<IdentityRepairWatchConflict>,
    pub reference_summary: BTreeMap<String, usize>,
    pub blockers: Vec<IdentityRepairBlocker>,
    pub before_counts: IdentityRepairCounts,
    pub expected_after_counts: IdentityRepairCounts,
    pub file_availability: String,
    pub fingerprint: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IdentityRepairOutcome {
    Applied,
    AlreadyApplied,
    Stale,
}

impl IdentityRepairPlan {
    pub fn ready(&self) -> bool {
        self.blockers.is_empty()
    }
}

fn blocker(code: &str, detail: impl Into<String>) -> IdentityRepairBlocker {
    IdentityRepairBlocker {
        code: code.to_owned(),
        detail: detail.into(),
    }
}

fn watch_state_eq(left: &IdentityRepairWatch, right: &IdentityRepairWatch) -> bool {
    left.position_ms == right.position_ms
        && left.duration_ms == right.duration_ms
        && left.watched == right.watched
        && left.updated_at == right.updated_at
}

fn parent<'a>(items: &'a BTreeMap<i64, &'a IdentityRepairItem>, id: i64) -> Option<i64> {
    items.get(&id).and_then(|item| item.parent_id)
}

/// Validate and deterministically plan one bounded duplicate-show repair.
pub fn plan_identity_repair(
    mut snapshot: IdentityRepairSnapshot,
) -> Result<IdentityRepairPlan, StoreError> {
    snapshot.input_show_ids.sort_unstable();
    snapshot.items.sort_by_key(|item| item.id);
    snapshot.files.sort_by_key(|file| file.id);
    snapshot
        .watches
        .sort_by_key(|watch| (watch.item_id, watch.user_id));
    snapshot.directory_owner_ids.sort_unstable();
    snapshot.directory_owner_ids.dedup();
    snapshot.dependency_rows.sort();
    snapshot
        .blockers
        .sort_by(|left, right| (&left.code, &left.detail).cmp(&(&right.code, &right.detail)));

    let mut blockers = snapshot.blockers.clone();
    if snapshot.library_kind != "shows" {
        blockers.push(blocker(
            "library_not_shows",
            "identity repair is limited to Shows libraries",
        ));
    }
    if !(IDENTITY_REPAIR_SHOWS_MIN..=IDENTITY_REPAIR_SHOWS_MAX)
        .contains(&snapshot.input_show_ids.len())
    {
        blockers.push(blocker(
            "invalid_show_count",
            format!("expected {IDENTITY_REPAIR_SHOWS_MIN}..={IDENTITY_REPAIR_SHOWS_MAX} show IDs"),
        ));
    }
    if snapshot
        .input_show_ids
        .windows(2)
        .any(|pair| pair[0] == pair[1])
    {
        blockers.push(blocker("duplicate_show_id", "show IDs must be unique"));
    }

    let items = snapshot
        .items
        .iter()
        .map(|item| (item.id, item))
        .collect::<BTreeMap<_, _>>();
    let shows = snapshot
        .input_show_ids
        .iter()
        .filter_map(|id| items.get(id).copied())
        .collect::<Vec<_>>();
    if shows.len() != snapshot.input_show_ids.len()
        || shows.iter().any(|show| {
            show.kind != "show"
                || show.library_id != snapshot.library_id
                || show.parent_id.is_some()
        })
    {
        blockers.push(blocker(
            "invalid_show_set",
            "every requested ID must be a top-level show in the requested library",
        ));
    }
    let known_tmdb = shows
        .iter()
        .filter_map(|show| show.tmdb_id)
        .collect::<BTreeSet<_>>();
    if known_tmdb.len() > 1 {
        blockers.push(blocker(
            "conflicting_show_tmdb_ids",
            "requested shows have different non-null TMDB IDs",
        ));
    }

    let seasons = snapshot
        .items
        .iter()
        .filter(|item| item.kind == "season")
        .collect::<Vec<_>>();
    let episodes = snapshot
        .items
        .iter()
        .filter(|item| item.kind == "episode")
        .collect::<Vec<_>>();
    if seasons.len() > IDENTITY_REPAIR_SEASONS_MAX
        || episodes.len() > IDENTITY_REPAIR_EPISODES_MAX
        || snapshot.files.len() > IDENTITY_REPAIR_FILES_MAX
        || snapshot.watches.len() > IDENTITY_REPAIR_WATCHES_MAX
    {
        return Err(StoreError::Task("repair_too_large".to_owned()));
    }

    let requested = snapshot
        .input_show_ids
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let omitted = snapshot
        .directory_owner_ids
        .iter()
        .copied()
        .filter(|id| !requested.contains(id))
        .collect::<Vec<_>>();
    if !omitted.is_empty() {
        blockers.push(blocker(
            "incomplete_directory_owner_set",
            format!("include directory owners {omitted:?} in a new preview"),
        ));
    }

    let mut source_directories = BTreeSet::new();
    for file in &snapshot.files {
        let parsed = crate::scan::parse::parse_episode(std::path::Path::new(&file.path))
            .or_else(|_| crate::scan::parse::parse_anime_episode(std::path::Path::new(&file.path)))
            .ok();
        match parsed.and_then(|value| value.source_directory) {
            Some(directory) => {
                source_directories.insert(directory.to_string_lossy().into_owned());
            }
            None => blockers.push(blocker(
                "missing_source_directory",
                format!("file {} has no recognized show directory", file.id),
            )),
        }
    }
    if source_directories.len() != 1 {
        blockers.push(blocker(
            "mixed_source_directories",
            "all selected shows and files must have one recognized source directory",
        ));
    }
    let source_directory = (source_directories.len() == 1)
        .then(|| source_directories.into_iter().next())
        .flatten();

    let before_counts = IdentityRepairCounts {
        shows: shows.len(),
        seasons: seasons.len(),
        episodes: episodes.len(),
        files: snapshot.files.len(),
        watches: snapshot.watches.len(),
    };

    let mut item_moves = Vec::new();
    let mut file_moves = Vec::new();
    let mut retired = BTreeSet::new();
    let mut episode_mapping = BTreeMap::<i64, i64>::new();
    let survivor = shows
        .iter()
        .min_by_key(|show| (show.added_at, show.id))
        .copied();

    if let Some(survivor) = survivor {
        let mut seasons_by_number = BTreeMap::<i64, Vec<&IdentityRepairItem>>::new();
        for season in &seasons {
            let Some(number) = season.season_number else {
                blockers.push(blocker(
                    "missing_season_number",
                    format!("season {} has no season number", season.id),
                ));
                continue;
            };
            let Some(show_id) = season.parent_id else {
                blockers.push(blocker(
                    "broken_ancestry",
                    format!("season {} has no show parent", season.id),
                ));
                continue;
            };
            if !requested.contains(&show_id) || season.library_id != snapshot.library_id {
                blockers.push(blocker(
                    "broken_ancestry",
                    format!("season {} is outside the selected shows", season.id),
                ));
                continue;
            }
            seasons_by_number.entry(number).or_default().push(season);
        }

        for (number, mut group) in seasons_by_number {
            group.sort_by_key(|season| (season.added_at, season.id));
            if group
                .windows(2)
                .any(|pair| pair[0].parent_id == pair[1].parent_id)
            {
                blockers.push(blocker(
                    "duplicate_season_number",
                    format!("season {number} is duplicated under one show"),
                ));
                continue;
            }
            let retained_season = group
                .iter()
                .find(|season| season.parent_id == Some(survivor.id))
                .copied()
                .unwrap_or(group[0]);
            if retained_season.parent_id != Some(survivor.id) {
                item_moves.push(IdentityRepairItemMove {
                    item_id: retained_season.id,
                    expected_parent_id: retained_season.parent_id.unwrap_or_default(),
                    new_parent_id: survivor.id,
                });
            }

            let mut episodes_by_number = BTreeMap::<i64, Vec<&IdentityRepairItem>>::new();
            for season in &group {
                for episode in episodes
                    .iter()
                    .filter(|episode| episode.parent_id == Some(season.id))
                {
                    let Some(episode_number) = episode.episode_number else {
                        blockers.push(blocker(
                            "missing_episode_number",
                            format!("episode {} has no episode number", episode.id),
                        ));
                        continue;
                    };
                    episodes_by_number
                        .entry(episode_number)
                        .or_default()
                        .push(episode);
                }
            }
            for (episode_number, mut episode_group) in episodes_by_number {
                episode_group.sort_by_key(|episode| (episode.added_at, episode.id));
                if episode_group
                    .windows(2)
                    .any(|pair| pair[0].parent_id == pair[1].parent_id)
                {
                    blockers.push(blocker(
                        "duplicate_episode_number",
                        format!("episode {episode_number} is duplicated under one season {number}"),
                    ));
                    continue;
                }
                let retained_episode = episode_group
                    .iter()
                    .find(|episode| episode.parent_id == Some(retained_season.id))
                    .copied()
                    .unwrap_or(episode_group[0]);
                if retained_episode.parent_id != Some(retained_season.id) {
                    item_moves.push(IdentityRepairItemMove {
                        item_id: retained_episode.id,
                        expected_parent_id: retained_episode.parent_id.unwrap_or_default(),
                        new_parent_id: retained_season.id,
                    });
                }
                for episode in episode_group {
                    episode_mapping.insert(episode.id, retained_episode.id);
                    if episode.id != retained_episode.id {
                        for file in snapshot
                            .files
                            .iter()
                            .filter(|file| file.item_id == episode.id)
                        {
                            file_moves.push(IdentityRepairFileMove {
                                file_id: file.id,
                                expected_item_id: episode.id,
                                new_item_id: retained_episode.id,
                            });
                        }
                        retired.insert(episode.id);
                    }
                }
            }
            for season in group {
                if season.id != retained_season.id {
                    retired.insert(season.id);
                }
            }
        }
        for show in shows.iter().copied().filter(|show| show.id != survivor.id) {
            retired.insert(show.id);
        }
    }

    let mut watch_copies = Vec::new();
    let mut watch_conflicts = Vec::new();
    let mut mapping_groups = BTreeMap::<i64, Vec<i64>>::new();
    for (source, destination) in &episode_mapping {
        mapping_groups
            .entry(*destination)
            .or_default()
            .push(*source);
    }
    for (destination, mut sources) in mapping_groups {
        sources.sort_by_key(|source| {
            items
                .get(source)
                .map(|item| (item.added_at, item.id))
                .unwrap_or((i64::MAX, *source))
        });
        let users = snapshot
            .watches
            .iter()
            .filter(|watch| sources.contains(&watch.item_id))
            .map(|watch| watch.user_id)
            .collect::<BTreeSet<_>>();
        for user_id in users {
            let rows = sources
                .iter()
                .filter_map(|source| {
                    snapshot
                        .watches
                        .iter()
                        .find(|watch| watch.user_id == user_id && watch.item_id == *source)
                })
                .collect::<Vec<_>>();
            let Some(selected) = rows
                .iter()
                .find(|watch| watch.item_id == destination)
                .copied()
                .or_else(|| rows.first().copied())
            else {
                continue;
            };
            if selected.item_id != destination {
                watch_copies.push(IdentityRepairWatchCopy {
                    user_id,
                    source_item_id: selected.item_id,
                    destination_item_id: destination,
                    state: selected.clone(),
                });
            }
            for source in rows {
                if source.item_id != selected.item_id && !watch_state_eq(source, selected) {
                    watch_conflicts.push(IdentityRepairWatchConflict {
                        user_id,
                        source_item_id: source.item_id,
                        destination_item_id: destination,
                        source: source.clone(),
                        destination: selected.clone(),
                        resolution: "survivor_wins".to_owned(),
                    });
                }
            }
        }
    }

    for episode in &episodes {
        if !seasons
            .iter()
            .any(|season| season.id == parent(&items, episode.id).unwrap_or_default())
        {
            blockers.push(blocker(
                "broken_ancestry",
                format!("episode {} has no selected season parent", episode.id),
            ));
        }
    }
    for show in &shows {
        if snapshot
            .watches
            .iter()
            .any(|watch| watch.item_id == show.id)
        {
            blockers.push(blocker(
                "show_or_season_watch_state",
                format!("show {} has unsupported watch state", show.id),
            ));
        }
    }
    for season in &seasons {
        if snapshot
            .watches
            .iter()
            .any(|watch| watch.item_id == season.id)
        {
            blockers.push(blocker(
                "show_or_season_watch_state",
                format!("season {} has unsupported watch state", season.id),
            ));
        }
    }

    item_moves.sort_by_key(|movement| movement.item_id);
    file_moves.sort_by_key(|movement| movement.file_id);
    watch_copies.sort_by_key(|copy| (copy.destination_item_id, copy.user_id));
    watch_conflicts.sort_by_key(|conflict| {
        (
            conflict.destination_item_id,
            conflict.user_id,
            conflict.source_item_id,
        )
    });
    blockers.sort_by(|left, right| (&left.code, &left.detail).cmp(&(&right.code, &right.detail)));
    blockers.dedup();
    let retired_item_ids = retired.into_iter().collect::<Vec<_>>();

    let mut reference_summary = BTreeMap::new();
    reference_summary.insert("dependency_rows".to_owned(), snapshot.dependency_rows.len());
    reference_summary.insert("watch_copies".to_owned(), watch_copies.len());
    reference_summary.insert("watch_conflicts".to_owned(), watch_conflicts.len());

    let retired_watch_rows = snapshot
        .watches
        .iter()
        .filter(|watch| retired_item_ids.contains(&watch.item_id))
        .count();
    let plan_copies = watch_copies.len();
    let expected_after_counts = IdentityRepairCounts {
        shows: usize::from(survivor.is_some()),
        seasons: seasons.len().saturating_sub(
            retired_item_ids
                .iter()
                .filter(|id| items.get(id).is_some_and(|item| item.kind == "season"))
                .count(),
        ),
        episodes: episodes.len().saturating_sub(
            retired_item_ids
                .iter()
                .filter(|id| items.get(id).is_some_and(|item| item.kind == "episode"))
                .count(),
        ),
        files: snapshot.files.len(),
        watches: snapshot
            .watches
            .len()
            .saturating_sub(retired_watch_rows)
            .saturating_add(plan_copies),
    };

    let mut plan = IdentityRepairPlan {
        version: 1,
        library_id: snapshot.library_id,
        source_directory,
        survivor_show_id: survivor.map(|show| show.id),
        input_show_ids: snapshot.input_show_ids.clone(),
        item_moves,
        file_moves,
        retired_item_ids,
        watch_copies,
        watch_conflicts,
        reference_summary,
        blockers,
        before_counts,
        expected_after_counts,
        file_availability: "not_checked".to_owned(),
        fingerprint: String::new(),
    };
    let canonical = serde_json::to_vec(&(&snapshot, &plan))
        .map_err(|error| StoreError::Task(format!("serialize identity repair plan: {error}")))?;
    if canonical.len() > IDENTITY_REPAIR_PLAN_BYTES_MAX {
        return Err(StoreError::Task("repair_too_large".to_owned()));
    }
    plan.fingerprint = format!("{:x}", Sha256::digest(&canonical));
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(
        id: i64,
        kind: &str,
        parent_id: Option<i64>,
        number: Option<i64>,
    ) -> IdentityRepairItem {
        IdentityRepairItem {
            id,
            library_id: 1,
            kind: kind.to_owned(),
            parent_id,
            tmdb_id: None,
            season_number: (kind == "season").then_some(number).flatten(),
            episode_number: (kind == "episode").then_some(number).flatten(),
            added_at: id,
            row: format!("[{id}]"),
        }
    }

    #[test]
    fn scan_identity_repair_planner_moves_versions_and_preserves_intact_ids() {
        let snapshot = IdentityRepairSnapshot {
            library_id: 1,
            library_kind: "shows".to_owned(),
            library_paths: r#"["/media"]"#.to_owned(),
            input_show_ids: vec![10, 20],
            items: vec![
                item(10, "show", None, None),
                item(20, "show", None, None),
                item(11, "season", Some(10), Some(1)),
                item(21, "season", Some(20), Some(1)),
                item(12, "episode", Some(11), Some(1)),
                item(22, "episode", Some(21), Some(1)),
                item(23, "season", Some(20), Some(2)),
                item(24, "episode", Some(23), Some(1)),
            ],
            files: vec![
                IdentityRepairFile {
                    id: 100,
                    item_id: 12,
                    path: "/media/Synthetic Show/Season 1/S01E01.mkv".to_owned(),
                    size: 1,
                    mtime: 1,
                    row: "[100]".to_owned(),
                },
                IdentityRepairFile {
                    id: 200,
                    item_id: 22,
                    path: "/media/Synthetic Show/Season 1/S01E01.alt.mkv".to_owned(),
                    size: 2,
                    mtime: 2,
                    row: "[200]".to_owned(),
                },
                IdentityRepairFile {
                    id: 201,
                    item_id: 24,
                    path: "/media/Synthetic Show/Season 2/S02E01.mkv".to_owned(),
                    size: 3,
                    mtime: 3,
                    row: "[201]".to_owned(),
                },
            ],
            watches: Vec::new(),
            directory_owner_ids: vec![10, 20],
            dependency_rows: Vec::new(),
            blockers: Vec::new(),
        };
        let plan = plan_identity_repair(snapshot).expect("plan");
        assert!(plan.ready(), "{:#?}", plan.blockers);
        assert_eq!(plan.survivor_show_id, Some(10));
        assert!(plan
            .item_moves
            .iter()
            .any(|movement| movement.item_id == 23 && movement.new_parent_id == 10));
        assert!(plan
            .file_moves
            .iter()
            .any(|movement| movement.file_id == 200 && movement.new_item_id == 12));
        assert!(!plan.retired_item_ids.contains(&24));
        assert!(!plan.fingerprint.is_empty());
    }
}
