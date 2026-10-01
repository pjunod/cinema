//! Browsing: library grids, item detail, home hubs, and search. Every item is
//! annotated with the requesting user's watch state.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::Path as FsPath;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

use axum::extract::{Path, Query, State};
use axum::Json;
use plurx_core::domain::{Item, ItemKind, ItemSort, WatchState};
use plurx_core::mediafacts::MediaFacts;
use serde::{Deserialize, Serialize};

use super::dto::{
    chapters_from_probe_json, in_progress_dto, recent_dto, FileDto, ItemDto, LibraryDto, ReadingDto,
};
use super::error::ApiError;
use super::extract::{AuthUser, ReadAfter};
use crate::state::AppState;

const DEFAULT_LIMIT: i64 = 60;
const MAX_LIMIT: i64 = 200;

static INDEX_PRESENCES: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];
const PAIR_BUCKETS: [u64; 8] = [1, 2, 4, 8, 32, 128, 512, u64::MAX];
const PROBE_BYTE_BUCKETS: [u64; 8] = [
    1024,
    16_384,
    65_536,
    262_144,
    1_048_576,
    4_194_304,
    16_777_216,
    u64::MAX,
];
static PAIRS: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
static PROBE_BYTES: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
static PAIR_SUM: AtomicU64 = AtomicU64::new(0);
static PROBE_BYTE_SUM: AtomicU64 = AtomicU64::new(0);

fn record_detail_projection(
    statuses: &[plurx_core::store::FragmentIndexStatus],
    probe_bytes: usize,
) {
    use plurx_core::store::IndexPresence;
    for status in statuses {
        let index = match status.presence {
            IndexPresence::Ready => 0,
            IndexPresence::Unverified => 1,
            IndexPresence::Absent => 2,
        };
        INDEX_PRESENCES[index].fetch_add(1, AtomicOrdering::Relaxed);
    }
    for (value, buckets, counts, sum) in [
        (
            statuses.len() as u64,
            PAIR_BUCKETS.as_slice(),
            PAIRS.as_slice(),
            &PAIR_SUM,
        ),
        (
            probe_bytes as u64,
            PROBE_BYTE_BUCKETS.as_slice(),
            PROBE_BYTES.as_slice(),
            &PROBE_BYTE_SUM,
        ),
    ] {
        sum.fetch_add(value, AtomicOrdering::Relaxed);
        for (bucket, count) in buckets.iter().zip(counts) {
            if value <= *bucket {
                count.fetch_add(1, AtomicOrdering::Relaxed);
            }
        }
    }
}

/// Atomics only, with no paths, identities, payloads or Store reads on scrape.
pub(super) fn detail_projection_prometheus() -> String {
    let mut out = String::from("# HELP plurx_index_status_projections_total Metadata-only index badge answers.\n# TYPE plurx_index_status_projections_total counter\n");
    for (label, count) in ["ready", "unverified", "absent"]
        .iter()
        .zip(&INDEX_PRESENCES)
    {
        out.push_str(&format!(
            "plurx_index_status_projections_total{{presence=\"{label}\"}} {}\n",
            count.load(AtomicOrdering::Relaxed)
        ));
    }
    for (name, help, buckets, counts, sum) in [
        (
            "plurx_index_status_pairs",
            "Pairs per detail render.",
            PAIR_BUCKETS.as_slice(),
            PAIRS.as_slice(),
            &PAIR_SUM,
        ),
        (
            "plurx_detail_probe_json_bytes",
            "Catalogue probe JSON bytes per detail render.",
            PROBE_BYTE_BUCKETS.as_slice(),
            PROBE_BYTES.as_slice(),
            &PROBE_BYTE_SUM,
        ),
    ] {
        out.push_str(&format!("# HELP {name} {help}\n# TYPE {name} histogram\n"));
        for (bucket, count) in buckets.iter().zip(counts) {
            let label = if *bucket == u64::MAX {
                "+Inf".to_owned()
            } else {
                bucket.to_string()
            };
            out.push_str(&format!(
                "{name}_bucket{{le=\"{label}\"}} {}\n",
                count.load(AtomicOrdering::Relaxed)
            ));
        }
        out.push_str(&format!(
            "{name}_sum {}\n{name}_count {}\n",
            sum.load(AtomicOrdering::Relaxed),
            counts[counts.len() - 1].load(AtomicOrdering::Relaxed)
        ));
    }
    out
}

/// One sentence for why a file has no fragment index, and whether waiting will
/// change it.
///
/// The newest recorded refusal wins, and a terminal one outranks a retryable
/// one at the same instant: a file with one pipeline refused for good and
/// another merely truncated is a file an operator has to act on, and the badge
/// that says "pending" for it is the one that never resolves. `None` when the
/// indexer has not tried yet, which is the honest reading of `pending`.
fn index_refusal_summary(
    outcomes: &[plurx_core::segplan::FragmentIndexOutcome],
) -> Option<(bool, String)> {
    use plurx_core::segplan::IndexRefusal;
    let worst = outcomes
        .iter()
        .max_by_key(|outcome| (u8::from(outcome.is_terminal()), outcome.updated_at_ms))?;
    // From the typed decision when there is one, for the same reason the badge
    // is: the legacy refusal carries `Truncated` for every terminal code that
    // is not literally `Unsupported`, so a file that reached its attempt limit
    // was described as "incomplete after N fragments on attempt 5" — the
    // sentence for work still in progress, under a badge that now correctly
    // says it is not.
    let detail = match (worst.typed_code, worst.is_terminal()) {
        (Some(code), true) => match worst.terminal_reason.as_deref() {
            Some(terminal) => format!("{}: {} ({terminal})", code.as_str(), worst.reason),
            None => format!("{}: {}", code.as_str(), worst.reason),
        },
        (Some(code), false) => format!(
            "{}: {} — attempt {} so far",
            code.as_str(),
            worst.reason,
            worst.attempts
        ),
        (None, _) => match worst.refusal {
            IndexRefusal::Unsupported => format!("cannot be indexed: {}", worst.reason),
            IndexRefusal::Truncated { rows } => format!(
                "incomplete after {rows} fragment{} on attempt {}: {}",
                if rows == 1 { "" } else { "s" },
                worst.attempts,
                worst.reason
            ),
        },
    };
    Some((worst.is_terminal(), detail))
}

/// Compare paths the way a listener reads numbered parts: Part 2 precedes
/// Part 10 even though lexical ordering puts `10` first. Non-numeric runs are
/// compared case-insensitively, with the original spelling as a stable tie.
fn natural_path_cmp(left: &FsPath, right: &FsPath) -> Ordering {
    fn chunks(value: &str) -> Vec<(&str, bool)> {
        let mut out = Vec::new();
        let mut start = 0;
        let mut digit = value.as_bytes().first().is_some_and(u8::is_ascii_digit);
        for (index, byte) in value.bytes().enumerate().skip(1) {
            let next = byte.is_ascii_digit();
            if next != digit {
                out.push((&value[start..index], digit));
                start = index;
                digit = next;
            }
        }
        if start < value.len() {
            out.push((&value[start..], digit));
        }
        out
    }

    let left_text = left.to_string_lossy();
    let right_text = right.to_string_lossy();
    let left_chunks = chunks(&left_text);
    let right_chunks = chunks(&right_text);
    for ((left, left_digit), (right, right_digit)) in left_chunks.iter().zip(&right_chunks) {
        let order = if *left_digit && *right_digit {
            let left_trimmed = left.trim_start_matches('0');
            let right_trimmed = right.trim_start_matches('0');
            left_trimmed
                .len()
                .cmp(&right_trimmed.len())
                .then_with(|| left_trimmed.cmp(right_trimmed))
                .then_with(|| left.len().cmp(&right.len()))
        } else {
            left.to_ascii_lowercase().cmp(&right.to_ascii_lowercase())
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    left_chunks
        .len()
        .cmp(&right_chunks.len())
        .then_with(|| left_text.cmp(&right_text))
}

fn clamp_limit(limit: Option<i64>) -> i64 {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

/// Fetch this user's watch state for a set of items as a lookup map. Watch
/// state is read-your-write state: the reader serves it locally only behind
/// the user's write fence (K-04 M2), and from Authority otherwise.
async fn watch_lookup(
    state: &AppState,
    user_id: i64,
    items: &[Item],
    read_after: ReadAfter,
) -> Result<HashMap<i64, WatchState>, ApiError> {
    let ids: Vec<i64> = items.iter().map(|i| i.id).collect();
    let map = state
        .catalogue
        .watch_map(user_id, &ids, read_after.0)
        .await?;
    Ok(map.into_iter().collect())
}

/// Shows, seasons and folders carry no watch row of their own; their state
/// is a rollup over the playable leaves beneath them.
fn is_rollup_container(kind: ItemKind) -> bool {
    matches!(kind, ItemKind::Show | ItemKind::Season | ItemKind::Folder)
}

/// Map items to DTOs with per-user watch state, and (for folders) how many
/// children each holds.
fn annotate_with_counts(
    items: Vec<Item>,
    watch: &HashMap<i64, WatchState>,
    counts: &HashMap<i64, i64>,
    media: &mut HashMap<i64, MediaFacts>,
) -> Vec<ItemDto> {
    items
        .into_iter()
        .map(|item| {
            let w = watch.get(&item.id).copied();
            let count = counts.get(&item.id).copied();
            let facts = media.remove(&item.id);
            let resolution = facts.as_ref().and_then(|f| f.height);
            ItemDto::from(item)
                .with_watch(w)
                .with_resolution(resolution)
                .with_media(facts)
                .with_child_count(count)
        })
        .collect()
}

#[derive(Deserialize)]
pub struct ListQuery {
    pub sort: Option<String>,
    pub offset: Option<i64>,
    pub limit: Option<i64>,
    /// `1` adds the aggregated `media` block to each playable item. Opt-in
    /// because it costs one more (still page-wide) query and a few hundred
    /// bytes a row: the clients that want spec columns ask for them, and
    /// everyone else keeps the response they already parse, byte for byte.
    pub facts: Option<u8>,
    /// Narrow the grid to one genre. Absent (the default) is the whole
    /// library, byte for byte what this endpoint returned before the
    /// parameter existed. Matched case-insensitively against the item's
    /// stored genres; an unknown genre is an empty page and a `total` of 0,
    /// not an error — the client asked a well-formed question and the answer
    /// is "nothing".
    pub genre: Option<String>,
}

#[derive(Serialize)]
pub struct ItemListResponse {
    pub items: Vec<ItemDto>,
    pub total: i64,
    pub offset: i64,
    pub limit: i64,
}

/// GET /api/v1/libraries/:id/items
pub async fn list_items(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    read_after: ReadAfter,
    Path(library_id): Path<i64>,
    Query(q): Query<ListQuery>,
) -> Result<Json<ItemListResponse>, ApiError> {
    if state.catalogue.get_library(library_id).await?.is_none() {
        return Err(ApiError::NotFound("library"));
    }
    let sort = q
        .sort
        .as_deref()
        .and_then(ItemSort::parse)
        .unwrap_or_default();
    let offset = q.offset.unwrap_or(0).max(0);
    let limit = clamp_limit(q.limit);

    // Trimmed and emptied-to-None so `?genre=` and `?genre=%20` mean the same
    // thing as omitting it, rather than "the genre whose name is one space".
    let genre = q.genre.as_deref().map(str::trim).filter(|g| !g.is_empty());
    let page = state
        .catalogue
        .list_top_items_in_genre(library_id, sort, offset, limit, genre)
        .await?;
    // Containers carry no watch row of their own, so a grid filtering by
    // "Watched"/"In progress" has nothing to filter a show on — the state
    // lives on its episodes, which aren't in this response. One batched
    // rollup answers it for every container at once; the per-card version
    // would be an N+1 over a recursive walk. Leaves keep `watch` only, which
    // already tells the whole truth about them. Both halves come from one
    // read of the same watch state (K-04 M3).
    let item_ids: Vec<i64> = page.items.iter().map(|i| i.id).collect();
    let container_ids: Vec<i64> = page
        .items
        .iter()
        .filter(|i| is_rollup_container(i.kind))
        .map(|i| i.id)
        .collect();
    let summary = state
        .catalogue
        .watch_summary(user.id, &item_ids, &container_ids, read_after.0)
        .await?;
    let watch: HashMap<i64, WatchState> = summary.watch.into_iter().collect();
    let rollups = summary.rollups;
    // Per-item resolution so the grid can badge/section it. Home videos carry
    // a badge for the same reason movies do — phone footage ranges from 480p
    // to 4K in the same folder.
    let badged: Vec<i64> = page
        .items
        .iter()
        .filter(|i| i.kind.carries_resolution())
        .map(|i| i.id)
        .collect();
    let heights = state.catalogue.item_max_heights(&badged).await?;
    // Codec/HDR/audio/size for the same set of items, and only when asked.
    // One query for the page, never one per item: `badged` is already the
    // page's playable ids, so this is a second constant-cost lookup, not a
    // fan-out. Photos and folders are not in `badged` — a photo has no codec
    // to name and a folder has no files of its own.
    let file_backed: Vec<i64> = page
        .items
        .iter()
        .filter(|i| {
            matches!(
                i.kind,
                ItemKind::Movie | ItemKind::Video | ItemKind::Book | ItemKind::Audiobook
            )
        })
        .map(|i| i.id)
        .collect();
    let mut facts = if q.facts == Some(1) {
        state.catalogue.item_media_facts(&file_backed).await?
    } else {
        HashMap::new()
    };
    // Folder cards say how much is inside them.
    let folder_ids: Vec<i64> = page
        .items
        .iter()
        .filter(|i| i.kind == ItemKind::Folder)
        .map(|i| i.id)
        .collect();
    let counts = state.catalogue.child_counts(&folder_ids).await?;
    let items = page
        .items
        .into_iter()
        .map(|item| {
            let rollup = rollups.get(&item.id).copied();
            let w = watch.get(&item.id).copied();
            let res = heights.get(&item.id).copied();
            let count = counts.get(&item.id).copied();
            let media = facts.remove(&item.id);
            ItemDto::from(item)
                .with_watch(w)
                .with_resolution(res)
                .with_media(media)
                .with_child_count(count)
                .with_rollup(rollup)
        })
        .collect();
    Ok(Json(ItemListResponse {
        items,
        total: page.total,
        offset,
        limit,
    }))
}

#[derive(Serialize)]
pub struct ItemDetail {
    pub item: ItemDto,
    /// Parent chain, outermost first (show, then season) — the breadcrumb.
    pub ancestors: Vec<ItemDto>,
    pub children: Vec<ItemDto>,
    pub files: Vec<FileDto>,
    /// Other text/audio editions sharing a proven work id. Empty when no
    /// explicit relation exists; title + author never populate this list.
    pub editions: Vec<ItemDto>,
    /// Current revision-bound locator for text books. `None` means unread or
    /// that the saved locator belongs to a replaced file revision.
    pub reading: Option<ReadingDto>,
}

/// GET /api/v1/items/:id — item plus its ancestors (for breadcrumbs),
/// children (seasons/episodes), and files.
pub async fn item_detail(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    read_after: ReadAfter,
    Path(id): Path<i64>,
) -> Result<Json<ItemDetail>, ApiError> {
    let item = state
        .catalogue
        .get_item(id)
        .await?
        .ok_or(ApiError::NotFound("item"))?;

    // Walk up the parent chain (episode → season → show; home libraries mirror
    // whatever folder depth is on disk, which is legitimately deep). The guard
    // is only an anti-cycle backstop.
    let mut ancestors = Vec::new();
    let mut cursor = item.parent_id;
    while let Some(parent_id) = cursor {
        match state.catalogue.get_item(parent_id).await? {
            Some(parent) => {
                cursor = parent.parent_id;
                ancestors.push(parent);
                if ancestors.len() >= 16 {
                    break;
                }
            }
            None => break,
        }
    }
    ancestors.reverse();

    let children = state.catalogue.get_item_children(id).await?;
    let child_counts = state
        .catalogue
        .child_counts(
            &children
                .iter()
                .filter(|c| c.kind == ItemKind::Folder)
                .map(|c| c.id)
                .collect::<Vec<_>>(),
        )
        .await?;
    // A season page is a media listing just as surely as a library grid is.
    // Fetch one best-file summary for all episode children in a single query;
    // asking for every episode detail independently would turn a 24-episode
    // season into 24 extra requests and queries. Keep other item-detail child
    // responses unchanged: home folders, for example, did not previously
    // attach list-style media badges.
    let mut child_media = if item.kind == ItemKind::Season {
        let episode_ids: Vec<i64> = children
            .iter()
            .filter(|child| child.kind == ItemKind::Episode)
            .map(|child| child.id)
            .collect();
        state.catalogue.item_media_facts(&episode_ids).await?
    } else {
        HashMap::new()
    };
    let mut files = match item.kind {
        // Home videos and photos have files exactly like movies do; folders
        // (and shows/seasons) have children instead.
        ItemKind::Movie
        | ItemKind::Episode
        | ItemKind::Book
        | ItemKind::Audiobook
        | ItemKind::Video
        | ItemKind::Photo => state.catalogue.files_for_item(id).await?,
        _ => Vec::new(),
    };
    if item.kind == ItemKind::Audiobook {
        // Generic media versions sort best-quality first. Audiobook files are
        // sequential parts, not alternate versions, so their path order is
        // the playback order.
        files.sort_by(|left, right| natural_path_cmp(&left.path, &right.path));
    }

    // Start every file's node-local observation together and give the whole
    // detail page one deadline, including a many-part audiobook. This is an
    // advisory answer; playback's source open remains authoritative.
    //
    // Track defaults are independent of that availability check: they use the
    // stored stream rows plus one settings snapshot, never a playback decision
    // or a media probe. Missing/unmounted files can therefore still explain
    // which tracks they contain and which policy choice would apply.
    let playback_prefs = state.transcode.lang_prefs().await;
    let availability = state
        .detail_availability
        .observe_many(
            &files
                .iter()
                .map(|file| file.path.clone())
                .collect::<Vec<_>>(),
        )
        .await;
    let mut file_dtos: Vec<FileDto> = Vec::with_capacity(files.len());
    let mut part_offset_ms = 0_i64;
    let has_video = files
        .iter()
        .any(|file| crate::copyseg::supports(file.video_codec.as_deref()));
    let have_dovi = has_video && crate::ffmpeg::has_dovi_rpu().await;
    let convert = has_video && state.transcode.dv_convert_enabled().await;
    let mut wanted = Vec::new();
    let mut prepared = Vec::with_capacity(files.len());
    let mut probe_bytes = 0_usize;
    for file in &files {
        let raw_probe = state.catalogue.get_file_probe_json(file.id).await?;
        probe_bytes = probe_bytes.saturating_add(raw_probe.as_ref().map_or(0, String::len));
        let start = wanted.len();
        if crate::copyseg::supports(file.video_codec.as_deref()) {
            for video in
                crate::fragindex::video_identities(file, raw_probe.as_deref(), have_dovi, convert)
            {
                wanted.push((file.id, crate::fragindex::identity_for(file, video)));
            }
        }
        prepared.push((raw_probe, start..wanted.len()));
    }
    let mut statuses = Vec::with_capacity(wanted.len());
    for chunk in wanted.chunks(plurx_core::store::FRAGMENT_INDEX_STATUS_CHUNK) {
        let batch = state.store.fragment_index_status(chunk).await?;
        if batch.len() != chunk.len()
            || batch.iter().zip(chunk).any(|(status, (id, identity))| {
                status.file_id != *id || status.argv_fingerprint != identity.argv_fingerprint
            })
        {
            return Err(plurx_core::error::StoreError::Task(
                "fragment status batch lost input identity/order".into(),
            )
            .into());
        }
        statuses.extend(batch);
    }
    record_detail_projection(&statuses, probe_bytes);
    for ((f, observation), (raw_probe, range)) in files.into_iter().zip(availability).zip(prepared)
    {
        let path = f.path.clone();
        let duration_ms = f.duration_ms.unwrap_or(0).max(0);
        let mut vod_index_refusal = None;
        let vod_index_status = if f.video_codec.is_none() {
            None
        } else if !crate::copyseg::supports(f.video_codec.as_deref()) {
            Some("unsupported")
        } else {
            // A file is "indexed" only when EVERY pipeline a client can ask
            // for has an index. The badge used to ask about the
            // Dolby-Vision-stripped pipeline alone and call that indexed,
            // which read green for a title a DV-capable client could not serve
            // from (PLAYBACK-CAPS-V2-PLAN §4.7). `partial` is the state that
            // was previously invisible.
            let mut present = 0_usize;
            // Why the rest are missing, when the indexer has already found
            // out. `pending` used to cover three states an operator acts on
            // differently: never tried, tried and truncated at N rows, and
            // tried and refused for good. The first is a wait; the last never
            // ends, and nothing on this page said so.
            //
            // The batched projection keeps every identity and uses the same
            // match rule as the index itself — so a
            // file the operator has since replaced stops answering with its
            // predecessor's refusal. A per-file read would have kept showing
            // "cannot be indexed" for a title that now plays, which is the
            // false-permanent-state failure this whole milestone exists to
            // remove, inverted.
            let mut refusals = Vec::new();
            for status in &statuses[range.clone()] {
                if status.presence == plurx_core::store::IndexPresence::Ready {
                    present += 1;
                } else if let Some(outcome) = &status.outcome {
                    refusals.push(outcome.clone());
                }
            }
            vod_index_refusal = index_refusal_summary(&refusals);
            Some(if present == range.len() {
                "indexed"
            } else if present > 0 {
                "partial"
            } else if vod_index_refusal
                .as_ref()
                .is_some_and(|(terminal, _)| *terminal)
            {
                "refused"
            } else {
                "pending"
            })
        };
        let mut dto = FileDto::from_media_file(f, &playback_prefs);
        dto.available = observation.available();
        dto.availability = observation.state.as_str();
        dto.availability_observed_at_ms = observation.observed_at_ms;
        dto.vod_index_status = vod_index_status;
        dto.vod_index_refusal = vod_index_refusal.map(|(_, detail)| detail);
        dto.part_offset_ms = part_offset_ms;
        dto.chapters = chapters_from_probe_json(raw_probe.as_deref());
        if item.kind == ItemKind::Audiobook {
            part_offset_ms = part_offset_ms.saturating_add(duration_ms);
        }
        dto.missing_path = observation.missing_path(user.is_admin, &path);
        file_dtos.push(dto);
    }

    // Annotate the item and its children with watch state. Containers have
    // no watch row of their own, so the client would have no way to know a
    // series is finished — its seasons carry nothing, and their episodes
    // aren't in this response at all — so a container also gets its rollup,
    // from the same read (K-04 M3).
    let mut all_ids: Vec<i64> = children.iter().map(|child| child.id).collect();
    all_ids.push(item.id);
    let rollup_ids: Vec<i64> = if is_rollup_container(item.kind) {
        vec![id]
    } else {
        Vec::new()
    };
    let summary = state
        .catalogue
        .watch_summary(user.id, &all_ids, &rollup_ids, read_after.0)
        .await?;
    let watch: HashMap<i64, WatchState> = summary.watch.into_iter().collect();
    let rollup = is_rollup_container(item.kind)
        .then(|| summary.rollups.get(&id).copied().unwrap_or_default());

    let reading = if item.kind == ItemKind::Book {
        state
            .store
            .current_reading_state(user.id, id)
            .await?
            .map(ReadingDto::try_from)
            .transpose()?
    } else {
        None
    };
    let editions = if matches!(item.kind, ItemKind::Book | ItemKind::Audiobook) {
        match item.book_work_id.as_deref() {
            Some(work_id) => state.store.related_book_editions(item.id, work_id).await?,
            None => Vec::new(),
        }
    } else {
        Vec::new()
    };
    let item_dto = ItemDto::from(item)
        .with_watch(watch.get(&id).copied())
        .with_rollup(rollup);
    Ok(Json(ItemDetail {
        item: item_dto,
        ancestors: ancestors.into_iter().map(Into::into).collect(),
        children: annotate_with_counts(children, &watch, &child_counts, &mut child_media),
        files: file_dtos,
        editions: editions.into_iter().map(Into::into).collect(),
        reading,
    }))
}

#[derive(Deserialize)]
pub struct HubsQuery {
    pub library_id: Option<i64>,
}

#[derive(Serialize)]
pub struct Hubs {
    pub continue_watching: Vec<ItemDto>,
    pub next_up: Vec<ItemDto>,
    pub recently_added: Vec<ItemDto>,
}

#[derive(Serialize)]
pub struct HomePreviews {
    pub libraries: Vec<HomeLibraryPreview>,
}

#[derive(Serialize)]
pub struct HomeLibraryPreview {
    pub library: LibraryDto,
    pub items: Vec<ItemDto>,
    pub total: i64,
}

/// GET /api/v1/home/previews — every library's recent preview in one bounded
/// catalog read, followed by page-wide annotations whose call count does not
/// grow with the number of libraries.
pub async fn home_previews(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    read_after: ReadAfter,
) -> Result<Json<HomePreviews>, ApiError> {
    // Home has one card budget. Keeping it server-owned prevents a caller
    // from widening the replicated read while preserving a parameter surface
    // the browser does not need.
    const HOME_PREVIEW_LIMIT: i64 = 24;
    let (libraries, pages) = tokio::try_join!(
        state.catalogue.list_libraries(),
        state.catalogue.home_preview_pages(HOME_PREVIEW_LIMIT),
    )?;

    let all_items: Vec<&Item> = pages.iter().flat_map(|page| page.items.iter()).collect();
    let item_ids: Vec<i64> = all_items.iter().map(|item| item.id).collect();
    let badged: Vec<i64> = all_items
        .iter()
        .filter(|item| item.kind.carries_resolution())
        .map(|item| item.id)
        .collect();
    let folder_ids: Vec<i64> = all_items
        .iter()
        .filter(|item| item.kind == ItemKind::Folder)
        .map(|item| item.id)
        .collect();
    let container_ids: Vec<i64> = all_items
        .iter()
        .filter(|item| is_rollup_container(item.kind))
        .map(|item| item.id)
        .collect();
    // Watch state and rollups for every preview card are one read of the
    // same state (K-04 M3): one consistent round trip, not two.
    let (summary, heights, counts) = tokio::try_join!(
        state
            .catalogue
            .watch_summary(user.id, &item_ids, &container_ids, read_after.0),
        state.catalogue.item_max_heights(&badged),
        state.catalogue.child_counts(&folder_ids),
    )?;
    let watch: HashMap<i64, WatchState> = summary.watch.into_iter().collect();
    let rollups = summary.rollups;
    let mut pages: HashMap<_, _> = pages
        .into_iter()
        .map(|page| (page.library_id, page))
        .collect();

    let libraries = libraries
        .into_iter()
        .map(|library| {
            let page = pages.remove(&library.id);
            let total = page.as_ref().map_or(0, |page| page.total);
            let items = page
                .map(|page| {
                    page.items
                        .into_iter()
                        .map(|item| {
                            let id = item.id;
                            ItemDto::from(item)
                                .with_watch(watch.get(&id).copied())
                                .with_resolution(heights.get(&id).copied())
                                .with_child_count(counts.get(&id).copied())
                                .with_rollup(rollups.get(&id).copied())
                        })
                        .collect()
                })
                .unwrap_or_default();
            HomeLibraryPreview {
                library: library.into(),
                items,
                total,
            }
        })
        .collect();

    Ok(Json(HomePreviews { libraries }))
}

/// GET /api/v1/hubs — the home screen rows.
pub async fn hubs(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    read_after: ReadAfter,
    Query(q): Query<HubsQuery>,
) -> Result<Json<Hubs>, ApiError> {
    // Recently-added is independent of playback progress, so it can overlap
    // the two progress-derived rails. Continue-watching and next-up both
    // interpret the same progress state, so they are one read of it (K-04
    // M3), each rail in its own established order.
    let progress_rows = async {
        let rails = state
            .catalogue
            .progress_rails(user.id, 20, read_after.0)
            .await?;
        Ok::<_, ApiError>((rails.continue_watching, rails.next_up))
    };
    let recent_rows = async {
        state
            .catalogue
            .recently_added(q.library_id, 20)
            .await
            .map_err(ApiError::from)
    };
    let ((in_progress, next), recent) = tokio::try_join!(progress_rows, recent_rows)?;
    let mut continue_watching: Vec<ItemDto> =
        in_progress.into_iter().map(in_progress_dto).collect();

    // Next-up episodes (unwatched tracks per show); no per-item watch state.
    let mut next_up: Vec<ItemDto> = next.into_iter().map(|r| recent_dto(r, None)).collect();

    let recent_items: Vec<Item> = recent.iter().map(|r| r.item.clone()).collect();
    // Folder cards say "12 items" here too, not just on the library grid.
    let folder_ids: Vec<i64> = recent_items
        .iter()
        .filter(|i| i.kind == ItemKind::Folder)
        .map(|i| i.id)
        .collect();
    let (watch, counts) = tokio::try_join!(
        watch_lookup(&state, user.id, &recent_items, read_after),
        async {
            state
                .catalogue
                .child_counts(&folder_ids)
                .await
                .map_err(ApiError::from)
        },
    )?;
    let mut recently_added: Vec<ItemDto> = recent
        .into_iter()
        .map(|r| {
            let w = watch.get(&r.item.id).copied();
            let count = counts.get(&r.item.id).copied();
            recent_dto(r, w).with_child_count(count)
        })
        .collect();

    // Resolution badges, same as the library grid gets. Home is where most
    // people actually browse, so a movie card that carries a 4K chip in the
    // library should carry it here too — one lookup covers all three rows.
    let badged: Vec<i64> = continue_watching
        .iter()
        .chain(next_up.iter())
        .chain(recently_added.iter())
        .filter(|d| d.kind.carries_resolution())
        .map(|d| d.id)
        .collect();
    if !badged.is_empty() {
        let heights = state.catalogue.item_max_heights(&badged).await?;
        for d in continue_watching
            .iter_mut()
            .chain(next_up.iter_mut())
            .chain(recently_added.iter_mut())
        {
            if d.kind.carries_resolution() {
                d.resolution = heights.get(&d.id).copied();
            }
        }
    }

    Ok(Json(Hubs {
        continue_watching,
        next_up,
        recently_added,
    }))
}

#[derive(Deserialize)]
pub struct SearchQuery {
    pub q: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Serialize)]
pub struct SearchResponse {
    pub results: Vec<ItemDto>,
}

/// GET /api/v1/search?q=
pub async fn search(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    read_after: ReadAfter,
    Query(q): Query<SearchQuery>,
) -> Result<Json<SearchResponse>, ApiError> {
    let query = q.q.unwrap_or_default();
    let limit = clamp_limit(q.limit);
    let hits = state.catalogue.search_items(&query, limit).await?;
    let items: Vec<Item> = hits.iter().map(|r| r.item.clone()).collect();
    let watch = watch_lookup(&state, user.id, &items, read_after).await?;
    let results = hits
        .into_iter()
        .map(|r| {
            let w = watch.get(&r.item.id).copied();
            recent_dto(r, w)
        })
        .collect();
    Ok(Json(SearchResponse { results }))
}

#[cfg(test)]
mod tests {
    use axum::extract::{Path, Query, State};
    use axum::Json;
    use plurx_core::domain::{ItemKind, LibraryKind, NewItem, NewLibrary, User};
    use plurx_core::store::{scope_http_store_operations, HttpStoreOperationCounts};

    use crate::http::extract::{AuthUser, ReadAfter};
    use crate::state::AppState;

    struct IndexPage {
        state: AppState,
        user: User,
        item: i64,
        files: Vec<plurx_core::domain::MediaFile>,
        root: tempfile::TempDir,
    }

    async fn index_page(parts: usize, dv: bool) -> IndexPage {
        use plurx_core::domain::{DolbyVisionFacts, ProbeResult};
        let root = tempfile::tempdir().expect("owned detail fixture");
        let store = std::sync::Arc::new(
            plurx_core::store::SqliteStore::open(&root.path().join("catalogue.db")).expect("store"),
        );
        let state = AppState::new(
            "test".into(),
            store,
            crate::state::Dirs {
                artwork: root.path().join("artwork"),
                transcode: root.path().join("transcode"),
                cache: root.path().join("cache"),
                subs: root.path().join("subs"),
                runtime_cache: root.path().join("runtime"),
                renditions: root.path().join("renditions"),
            },
            "detail-node".into(),
            Default::default(),
            Default::default(),
            std::sync::Arc::new(crate::logbuf::LogBuffer::new(64)),
        );
        let user = state
            .store
            .create_user("detail", "hash", false)
            .await
            .expect("user");
        let library = state
            .store
            .create_library(&NewLibrary {
                name: "detail".into(),
                kind: LibraryKind::Movies,
                paths: vec![root.path().to_owned()],
                anime: false,
            })
            .await
            .expect("library");
        let item = state
            .store
            .insert_item(&NewItem {
                library_id: library.id,
                kind: if parts > 1 {
                    ItemKind::Audiobook
                } else {
                    ItemKind::Movie
                },
                parent_id: None,
                title: "detail fixture".into(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        let probe = ProbeResult {
            video_codec: Some(if dv { "hevc" } else { "h264" }.into()),
            duration_ms: Some(1000),
            hdr: dv.then(|| "dolby_vision".into()),
            dolby_vision: if dv {
                DolbyVisionFacts {
                    profile: Some(7),
                    level: Some(6),
                    bl_compat_id: Some(6),
                    el_present: Some(true),
                    rpu_present: Some(true),
                }
            } else {
                Default::default()
            },
            ..Default::default()
        };
        let mut files = Vec::new();
        for part in 0..parts {
            let id = state
                .store
                .upsert_file(
                    item,
                    &root
                        .path()
                        .join(format!("Part {part}.mkv"))
                        .to_string_lossy(),
                    4096,
                    100,
                    &probe,
                )
                .await
                .expect("file");
            files.push(
                state
                    .store
                    .get_file(id)
                    .await
                    .expect("lookup")
                    .expect("file"),
            );
        }
        IndexPage {
            state,
            user,
            item,
            files,
            root,
        }
    }

    async fn publish(page: &IndexPage, file: usize, identities: usize, rows: usize) {
        use plurx_core::segplan::{FragmentIndex, IndexRow};
        let file = &page.files[file];
        let raw = page
            .state
            .store
            .get_file_probe_json(file.id)
            .await
            .expect("probe");
        let videos = crate::fragindex::video_identities(
            file,
            raw.as_deref(),
            crate::ffmpeg::has_dovi_rpu().await,
            true,
        );
        for video in videos.into_iter().take(identities) {
            let index = FragmentIndex::new(
                1000,
                (0..rows)
                    .map(|n| IndexRow {
                        dts: n as u64 * 1000,
                        duration: 1000,
                        bytes: 100,
                        video_bytes: 90,
                        class: plurx_core::fmp4::CutClass::CleanIdr,
                    })
                    .collect(),
                "fixture-init",
                crate::fragindex::identity_for(file, video),
            );
            page.state
                .store
                .put_fragment_index(file.id, &index)
                .await
                .expect("publish");
        }
    }

    async fn detail(page: &IndexPage) -> serde_json::Value {
        let Json(body) = super::item_detail(
            AuthUser(page.user.clone()),
            State(page.state.clone()),
            ReadAfter(None),
            Path(page.item),
        )
        .await
        .expect("detail");
        serde_json::to_value(body).expect("body")
    }

    #[tokio::test]
    async fn a_detail_render_unpacks_no_index() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use http_body_util::BodyExt;
        use tower::ServiceExt;
        let page = index_page(1, false).await;
        publish(&page, 0, 1, 4100).await;
        let token = "synthetic-detail-token";
        page.state
            .store
            .create_token(&plurx_core::auth::hash_token(token), page.user.id, None)
            .await
            .expect("test token");
        // Positive control: this is a live counter on the actual decoder,
        // not a zero-valued test helper disconnected from production get.
        let raw = page
            .state
            .store
            .get_file_probe_json(page.files[0].id)
            .await
            .expect("probe");
        let video = crate::fragindex::video_identities(
            &page.files[0],
            raw.as_deref(),
            crate::ffmpeg::has_dovi_rpu().await,
            true,
        )[0];
        let counter = plurx_core::store::fragment_index_unpack_counter(
            &page.root.path().join("catalogue.db"),
        );
        let control = counter.load(std::sync::atomic::Ordering::Relaxed);
        assert!(page
            .state
            .store
            .fragment_index(
                page.files[0].id,
                &crate::fragindex::identity_for(&page.files[0], video)
            )
            .await
            .expect("full control read")
            .is_some());
        assert_eq!(
            counter.load(std::sync::atomic::Ordering::Relaxed),
            control + 1
        );
        let before = counter.load(std::sync::atomic::Ordering::Relaxed);
        let response = crate::http::router(page.state.clone())
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/items/{}", page.item))
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value = serde_json::from_slice(
            &response
                .into_body()
                .collect()
                .await
                .expect("body")
                .to_bytes(),
        )
        .expect("JSON");
        assert_eq!(body["files"][0]["vod_index_status"], "indexed");
        assert_eq!(counter.load(std::sync::atomic::Ordering::Relaxed), before);
    }

    #[tokio::test]
    async fn an_unverified_legacy_row_renders_pending_not_indexed() {
        let page = index_page(1, false).await;
        publish(&page, 0, 1, 4100).await;
        let conn = rusqlite::Connection::open(page.root.path().join("catalogue.db"))
            .expect("fixture connection");
        conn.execute("UPDATE fragment_indexes SET validated_revision = 0, promotion = 'malformed legacy JSON'", []).expect("legacy");
        let body = detail(&page).await;
        assert_eq!(body["files"][0]["vod_index_status"], "pending");
        assert!(body["files"][0]["vod_index_refusal"].is_null());
        assert_eq!(
            conn.query_row("SELECT count(*) FROM fragment_indexes", [], |r| r
                .get::<_, i64>(0))
                .expect("retained"),
            1
        );
    }

    #[tokio::test]
    async fn a_partially_indexed_dv_file_still_renders_partial() {
        let page = index_page(1, true).await;
        let file = &page.files[0];
        let raw = page
            .state
            .store
            .get_file_probe_json(file.id)
            .await
            .expect("probe");
        assert_eq!(
            crate::fragindex::video_identities(
                file,
                raw.as_deref(),
                crate::ffmpeg::has_dovi_rpu().await,
                true
            )
            .len(),
            3
        );
        publish(&page, 0, 2, 10).await;
        assert_eq!(
            detail(&page).await["files"][0]["vod_index_status"],
            "partial"
        );
    }

    #[tokio::test]
    async fn a_refused_identity_still_renders_its_summary() {
        let page = index_page(1, false).await;
        let raw = page
            .state
            .store
            .get_file_probe_json(page.files[0].id)
            .await
            .expect("probe");
        let source = crate::fragindex::identity_for(
            &page.files[0],
            plurx_core::transcode::CopyVideoOptions::from_probe(
                &page.files[0],
                raw.as_deref(),
                crate::ffmpeg::has_dovi_rpu().await,
                false,
            ),
        );
        let outcome = page
            .state
            .store
            .record_fragment_index_outcome(
                page.files[0].id,
                &source,
                plurx_core::segplan::IndexRefusal::Unsupported,
                "synthetic unsupported codec",
            )
            .await
            .expect("refusal");
        let body = detail(&page).await;
        assert_eq!(body["files"][0]["vod_index_status"], "refused");
        assert_eq!(
            body["files"][0]["vod_index_refusal"],
            super::index_refusal_summary(&[outcome]).expect("summary").1
        );
    }

    #[tokio::test]
    async fn a_three_hundred_part_audiobook_makes_two_status_calls() {
        let page = index_page(300, false).await;
        let counts = HttpStoreOperationCounts::default();
        let body = scope_http_store_operations(counts.clone(), detail(&page)).await;
        assert_eq!(counts.index_status_calls(), 2);
        assert_eq!(body["files"].as_array().expect("parts").len(), 300);
        assert_eq!(body["files"][299]["part_offset_ms"], 299_000);
    }

    #[test]
    fn detail_projection_metrics_are_fixed_and_store_free() {
        let text = super::detail_projection_prometheus();
        assert_eq!(
            text.lines()
                .filter(|l| l.starts_with("plurx_index_status_projections_total{"))
                .count(),
            3
        );
        assert!(text.contains("plurx_index_status_pairs_bucket{le=\"+Inf\"}"));
        assert!(text.contains("plurx_detail_probe_json_bytes_count"));
    }

    /// A movie library and a show library, one user with progress on a
    /// movie and one watched episode, so every browse page below has cards,
    /// containers and watch rows to annotate.
    struct WatchPages {
        state: AppState,
        user: User,
        shows: i64,
        show: i64,
    }

    async fn seed_watch_pages() -> WatchPages {
        let (_, state) = crate::http::tests::test_app_with_state();
        let store = &state.store;
        let user = store
            .create_user("watch-pages", "hash", false)
            .await
            .expect("user");
        let item = |library_id, kind, parent_id, title: &str, season, episode| NewItem {
            library_id,
            kind,
            parent_id,
            title: title.to_owned(),
            year: None,
            season_number: season,
            episode_number: episode,
        };
        let library = |name: &str, kind| NewLibrary {
            name: name.to_owned(),
            kind,
            paths: vec![std::path::PathBuf::from(format!("/{name}"))],
            anime: false,
        };
        let movies = store
            .create_library(&library("movies", LibraryKind::Movies))
            .await
            .expect("movie library")
            .id;
        let shows = store
            .create_library(&library("shows", LibraryKind::Shows))
            .await
            .expect("show library")
            .id;
        let movie = store
            .insert_item(&item(movies, ItemKind::Movie, None, "Movie", None, None))
            .await
            .expect("movie");
        let show = store
            .insert_item(&item(shows, ItemKind::Show, None, "Show", None, None))
            .await
            .expect("show");
        let season = store
            .insert_item(&item(
                shows,
                ItemKind::Season,
                Some(show),
                "S1",
                Some(1),
                None,
            ))
            .await
            .expect("season");
        let mut episodes = Vec::new();
        for number in 1..=2 {
            episodes.push(
                store
                    .insert_item(&item(
                        shows,
                        ItemKind::Episode,
                        Some(season),
                        &format!("E{number}"),
                        Some(1),
                        Some(number),
                    ))
                    .await
                    .expect("episode"),
            );
        }
        store
            .put_progress(user.id, movie, 10_000, Some(100_000))
            .await
            .expect("movie progress");
        store
            .set_watched(user.id, episodes[0], true)
            .await
            .expect("first episode watched");
        WatchPages {
            state,
            user,
            shows,
            show,
        }
    }

    /// Run one handler inside a request scope, as the route middleware does,
    /// and return its body with the number of watch-state reads the Store
    /// performed for it.
    async fn watch_reads_of<T: serde::Serialize>(
        handler: impl std::future::Future<Output = Result<Json<T>, super::ApiError>>,
    ) -> (serde_json::Value, u64) {
        let counts = HttpStoreOperationCounts::default();
        let Ok(Json(body)) = scope_http_store_operations(counts.clone(), handler).await else {
            panic!("handler failed");
        };
        (
            serde_json::to_value(body).expect("serialize body"),
            counts.watch_reads(),
        )
    }

    /// K-04 M3's acceptance (§5.4), as a counter rather than a reading of
    /// the source (review of #504, finding 3): Home's library previews read
    /// watch state exactly once — one `watch_summary` for every card's watch
    /// row and every container's rollup. The counter is the Store's own, so
    /// a watch read added through any method, `state.store` included, is
    /// counted; the store contract pins that the one read is one statement
    /// on the replicated store.
    #[tokio::test]
    async fn home_previews_reads_watch_state_exactly_once() {
        let pages = seed_watch_pages().await;
        let (body, reads) = watch_reads_of(super::home_previews(
            AuthUser(pages.user.clone()),
            State(pages.state.clone()),
            ReadAfter(None),
        ))
        .await;
        assert_eq!(reads, 1, "home previews: {body}");
        let cards: Vec<&serde_json::Value> = body["libraries"]
            .as_array()
            .expect("libraries")
            .iter()
            .flat_map(|page| page["items"].as_array().expect("items"))
            .collect();
        assert!(
            cards.iter().any(|card| card.get("watch").is_some()),
            "the one read answered the watch rows: {body}"
        );
        assert!(
            cards
                .iter()
                .any(|card| card["id"] == pages.show && card.get("rollup").is_some()),
            "the one read answered the rollups: {body}"
        );
    }

    /// The same counter for the other browse pages that used to issue two
    /// watch reads: the grid once for its cards and its containers, the item
    /// page once for its children and its rollup, and Home's rails once for
    /// both progress-derived rails, plus the recently-added rail's own lookup
    /// when it has cards (it annotates catalogue rows it has to read first).
    #[tokio::test]
    async fn home_hubs_grid_and_item_pages_read_watch_state_once_per_dependency() {
        let pages = seed_watch_pages().await;
        let (body, reads) = watch_reads_of(super::list_items(
            AuthUser(pages.user.clone()),
            State(pages.state.clone()),
            ReadAfter(None),
            Path(pages.shows),
            Query(serde_json::from_value(serde_json::json!({})).expect("default query")),
        ))
        .await;
        assert_eq!(reads, 1, "library grid: {body}");

        let (body, reads) = watch_reads_of(super::item_detail(
            AuthUser(pages.user.clone()),
            State(pages.state.clone()),
            ReadAfter(None),
            Path(pages.show),
        ))
        .await;
        assert_eq!(reads, 1, "item page: {body}");

        let (body, reads) = watch_reads_of(super::hubs(
            AuthUser(pages.user.clone()),
            State(pages.state.clone()),
            ReadAfter(None),
            Query(serde_json::from_value(serde_json::json!({})).expect("default query")),
        ))
        .await;
        assert!(
            !body["continue_watching"]
                .as_array()
                .expect("continue watching")
                .is_empty(),
            "the rails were read: {body}"
        );
        let recent = !body["recently_added"]
            .as_array()
            .expect("recently added")
            .is_empty();
        assert_eq!(reads, 1 + u64::from(recent), "hubs: {body}");
    }

    use super::index_refusal_summary;
    use plurx_core::segplan::{FragmentIndexOutcome, IndexRefusal, SourceIdentity};

    fn outcome(refusal: IndexRefusal, reason: &str, at_ms: i64) -> FragmentIndexOutcome {
        FragmentIndexOutcome {
            source: SourceIdentity::new(4_096, 1_700_000_000_000, "pipeline"),
            refusal,
            reason: reason.to_owned(),
            attempts: 2,
            next_attempt_at_ms: at_ms + 1_800_000,
            updated_at_ms: at_ms,
            typed_code: None,
            typed_retryable: None,
            retry_deadline_ms: 0,
            policy_revision: 0,
            terminal_reason: None,
            diagnostic: None,
        }
    }

    /// The badge has to separate three states an operator acts on differently,
    /// and `pending` used to be all three at once: never tried, tried and
    /// truncated, tried and refused for good. Only the last needs anyone to do
    /// something, and it was the one that looked like a wait.
    #[test]
    fn the_index_badge_says_whether_waiting_will_help() {
        assert_eq!(index_refusal_summary(&[]), None, "not tried is not refused");

        let (terminal, detail) = index_refusal_summary(&[outcome(
            IndexRefusal::Truncated { rows: 412 },
            "budget expired",
            10,
        )])
        .expect("a truncated attempt is worth reporting");
        assert!(!terminal);
        assert_eq!(
            detail,
            "incomplete after 412 fragments on attempt 2: budget expired"
        );

        // A terminal refusal outranks a retryable one even when it is older:
        // a file with one pipeline that can never be indexed is a file
        // somebody has to look at, and "pending" for it never resolves.
        let (terminal, detail) = index_refusal_summary(&[
            outcome(IndexRefusal::Truncated { rows: 1 }, "budget expired", 99),
            outcome(
                IndexRefusal::Unsupported,
                "the moov lost its video track",
                10,
            ),
        ])
        .expect("a refusal is worth reporting");
        assert!(terminal);
        assert_eq!(detail, "cannot be indexed: the moov lost its video track");

        // Singular reads as English, because this string is shown to a person.
        let (_, one) = index_refusal_summary(&[outcome(
            IndexRefusal::Truncated { rows: 1 },
            "budget expired",
            10,
        )])
        .expect("one row is still a finding");
        assert!(one.contains("after 1 fragment on"), "{one}");
    }
}
