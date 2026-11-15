//! Custom tags, saved rows and the Home layout over HTTP
//! (docs/features/CUSTOM-TAGS-PLAN.md §3–§4).
//!
//! The store enforces *visibility* (household plus the caller's own); this
//! module decides *who may change what*: a private tag or row is its
//! owner's, a household one needs the `manage_tags` grant, and admins hold
//! every grant. Nothing here is copied into the Jellyfin, Plex or
//! shared-library DTOs — those build their own and never read curation.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use futures_util::future::try_join_all;
use plurx_core::curation::{
    clean_color, clean_row_title, clean_suggest_terms, clean_tag_name, parse_row_key, tag_key,
    Grant, HomeEntry, HomeLayout, NewTag, RowSort, RowSpec, RuleQuery, SavedRow, Scope, Tag,
    TagEdit, TagSource, Term, MAX_BULK_ITEMS, TAG_COLORS,
};
use plurx_core::domain::{Item, ItemKind, User, WatchState};
use plurx_core::store::keys;
use serde::{Deserialize, Serialize};

use super::dto::ItemDto;
use super::error::ApiError;
use super::extract::{AdminUser, AuthUser, ReadAfter};
use crate::state::AppState;

const PAGE_MAX: i64 = 200;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn today() -> i64 {
    now_ms() / 86_400_000
}

fn page(offset: Option<i64>, limit: Option<i64>, default: i64) -> (i64, i64) {
    (
        offset.unwrap_or(0).max(0),
        limit.unwrap_or(default).clamp(1, PAGE_MAX),
    )
}

/// Admins hold every grant; everyone else holds what was granted.
pub(crate) async fn can_curate(state: &AppState, user: &User) -> Result<bool, ApiError> {
    if user.is_admin {
        return Ok(true);
    }
    Ok(state
        .store
        .user_grants(user.id)
        .await?
        .contains(&Grant::ManageTags))
}

fn may_edit(owner: Option<i64>, user: &User, curate: bool) -> bool {
    match owner {
        Some(owner) => owner == user.id,
        None => curate,
    }
}

async fn visible_tag(state: &AppState, user: &User, id: i64) -> Result<Tag, ApiError> {
    state
        .store
        .curation_tag(id)
        .await?
        .filter(|tag| tag.visible_to(user.id))
        .ok_or(ApiError::NotFound("tag"))
}

async fn editable_tag(state: &AppState, user: &User, id: i64) -> Result<Tag, ApiError> {
    let tag = visible_tag(state, user, id).await?;
    if !may_edit(tag.owner_user_id, user, can_curate(state, user).await?) {
        return Err(ApiError::Forbidden);
    }
    Ok(tag)
}

fn bad(message: String) -> ApiError {
    ApiError::BadRequest(message)
}

// ---------------------------------------------------------------------------
// Items: hydrate ids into the same cards a library grid returns.
// ---------------------------------------------------------------------------

/// Item ids → grid cards, in the given order, with the caller's watch state,
/// container rollups and resolution badges. Ids that vanished are dropped.
pub(crate) async fn item_cards(
    state: &AppState,
    user_id: i64,
    ids: &[i64],
    read_after: ReadAfter,
) -> Result<Vec<ItemDto>, ApiError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let items: Vec<Item> = try_join_all(ids.iter().map(|id| state.catalogue.get_item(*id)))
        .await?
        .into_iter()
        .flatten()
        .collect();
    let item_ids: Vec<i64> = items.iter().map(|i| i.id).collect();
    let container_ids: Vec<i64> = items
        .iter()
        .filter(|i| super::browse::is_rollup_container(i.kind))
        .map(|i| i.id)
        .collect();
    let badged: Vec<i64> = items
        .iter()
        .filter(|i| i.kind.carries_resolution())
        .map(|i| i.id)
        .collect();
    let (summary, heights) = tokio::try_join!(
        async {
            state
                .catalogue
                .watch_summary(user_id, &item_ids, &container_ids, read_after.0)
                .await
                .map_err(ApiError::from)
        },
        async {
            state
                .catalogue
                .item_max_heights(&badged)
                .await
                .map_err(ApiError::from)
        },
    )?;
    let watch: HashMap<i64, WatchState> = summary.watch.into_iter().collect();
    let rollups = summary.rollups;
    Ok(items
        .into_iter()
        .map(|item| {
            let id = item.id;
            ItemDto::from(item)
                .with_watch(watch.get(&id).copied())
                .with_rollup(rollups.get(&id).copied())
                .with_resolution(heights.get(&id).copied())
        })
        .collect())
}

#[derive(Serialize)]
pub struct ItemPage {
    pub items: Vec<ItemDto>,
    pub total: i64,
    pub offset: i64,
    pub limit: i64,
}

async fn rule_page(
    state: &AppState,
    user: &User,
    spec: &RowSpec,
    only_items: Option<&[i64]>,
    offset: i64,
    limit: i64,
    read_after: ReadAfter,
) -> Result<ItemPage, ApiError> {
    let query = RuleQuery {
        viewer: user.id,
        spec,
        only_items,
        day: today(),
    };
    let ids = state.store.rule_items(&query, offset, limit).await?;
    Ok(ItemPage {
        items: item_cards(state, user.id, &ids.item_ids, read_after).await?,
        total: ids.total,
        offset,
        limit,
    })
}

// ---------------------------------------------------------------------------
// Tags
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct TagDto {
    pub id: i64,
    pub name: String,
    pub scope: Scope,
    pub color: String,
    pub suggest_terms: Vec<String>,
    pub archived: bool,
    pub item_count: i64,
    /// Whether the caller may rename it, apply it, or review its suggestions.
    pub editable: bool,
}

fn tag_dto(tag: Tag, user: &User, curate: bool) -> TagDto {
    TagDto {
        editable: may_edit(tag.owner_user_id, user, curate),
        scope: tag.scope(),
        archived: tag.archived_at_ms.is_some(),
        id: tag.id,
        name: tag.name,
        color: tag.color,
        suggest_terms: tag.suggest_terms,
        item_count: tag.item_count,
    }
}

#[derive(Serialize)]
pub struct TagsResponse {
    pub tags: Vec<TagDto>,
    pub can_manage_household: bool,
    pub colors: &'static [&'static str],
}

/// GET /api/v1/tags — household tags and the caller's own, by name.
pub async fn list_tags(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
) -> Result<Json<TagsResponse>, ApiError> {
    let curate = can_curate(&state, &user).await?;
    let tags = state.store.visible_tags(user.id).await?;
    Ok(Json(TagsResponse {
        tags: tags
            .into_iter()
            .map(|t| tag_dto(t, &user, curate))
            .collect(),
        can_manage_household: curate,
        colors: &TAG_COLORS,
    }))
}

#[derive(Deserialize)]
pub struct NewTagBody {
    pub name: String,
    pub color: Option<String>,
    pub scope: Option<Scope>,
}

/// POST /api/v1/tags — create. A household tag needs `manage_tags`.
pub async fn create_tag(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Json(body): Json<NewTagBody>,
) -> Result<(StatusCode, Json<TagDto>), ApiError> {
    let curate = can_curate(&state, &user).await?;
    let scope = body.scope.unwrap_or(Scope::Private);
    if scope == Scope::Household && !curate {
        return Err(ApiError::Forbidden);
    }
    let name = clean_tag_name(&body.name).map_err(bad)?;
    let color = clean_color(body.color.as_deref()).map_err(bad)?;
    let owner = (scope == Scope::Private).then_some(user.id);
    let created = state
        .store
        .create_tag(
            &NewTag {
                name,
                owner_user_id: owner,
                color,
                created_by: user.id,
            },
            now_ms(),
        )
        .await?
        .ok_or_else(|| ApiError::Conflict("A tag with that name already exists.".into()))?;
    Ok((StatusCode::CREATED, Json(tag_dto(created, &user, curate))))
}

#[derive(Deserialize)]
pub struct TagPatch {
    pub name: Option<String>,
    pub color: Option<String>,
    pub archived: Option<bool>,
    pub suggest_terms: Option<Vec<String>>,
}

/// PATCH /api/v1/tags/{id} — rename, recolour, archive, set suggestion terms.
pub async fn update_tag(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(patch): Json<TagPatch>,
) -> Result<Json<TagDto>, ApiError> {
    let tag = editable_tag(&state, &user, id).await?;
    let mut edit = TagEdit::default();
    if let Some(name) = patch.name {
        let name = clean_tag_name(&name).map_err(bad)?;
        let key = tag_key(&name);
        let clash = state
            .store
            .visible_tags(user.id)
            .await?
            .into_iter()
            .any(|t| t.id != id && t.owner_user_id == tag.owner_user_id && tag_key(&t.name) == key);
        if clash {
            return Err(ApiError::Conflict(
                "A tag with that name already exists.".into(),
            ));
        }
        edit.name = Some(name);
    }
    if let Some(color) = patch.color {
        edit.color = Some(clean_color(Some(&color)).map_err(bad)?);
    }
    if let Some(terms) = patch.suggest_terms {
        edit.suggest_terms = Some(clean_suggest_terms(&terms).map_err(bad)?);
    }
    if let Some(archived) = patch.archived {
        edit.archived_at_ms = Some(archived.then(now_ms));
    }
    let updated = state
        .store
        .update_tag(id, &edit)
        .await?
        .ok_or(ApiError::NotFound("tag"))?;
    Ok(Json(tag_dto(
        updated,
        &user,
        can_curate(&state, &user).await?,
    )))
}

#[derive(Deserialize)]
pub struct ForceQuery {
    pub force: Option<u8>,
}

/// Drop a tag from a rule, or swap it for another. `None` when what is left
/// has no condition at all, i.e. the row should go.
fn rewrite_rule(spec: &RowSpec, from: i64, into: Option<i64>) -> Option<RowSpec> {
    let map = |terms: &[Term]| -> Vec<Term> {
        let mut out = Vec::new();
        for term in terms {
            let next = match term {
                Term::Tag(id) if *id == from => into.map(Term::Tag),
                other => Some(other.clone()),
            };
            if let Some(next) = next {
                if !out.contains(&next) {
                    out.push(next);
                }
            }
        }
        out
    };
    let mut spec = RowSpec {
        all_of: map(&spec.all_of),
        any_of: map(&spec.any_of),
        none_of: map(&spec.none_of),
        ..spec.clone()
    };
    spec.validate().ok().map(|()| spec)
}

async fn rewrite_rows(state: &AppState, from: i64, into: Option<i64>) -> Result<(), ApiError> {
    for row in state.store.rows_naming_tag(from).await? {
        match rewrite_rule(&row.spec, from, into) {
            Some(spec) => {
                state
                    .store
                    .update_row(row.id, &row.title, &spec, now_ms())
                    .await?;
            }
            None => {
                state.store.delete_row(row.id).await?;
            }
        }
    }
    Ok(())
}

/// DELETE /api/v1/tags/{id} — refused with the rows that use it unless
/// `?force=1`; forcing drops the tag from those rules (a rule left with no
/// condition is deleted). Archiving is the gentler answer and the UI offers it.
pub async fn delete_tag(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<ForceQuery>,
) -> Result<StatusCode, ApiError> {
    editable_tag(&state, &user, id).await?;
    let rows = state.store.rows_naming_tag(id).await?;
    if !rows.is_empty() && q.force != Some(1) {
        let titles: Vec<&str> = rows.iter().map(|r| r.title.as_str()).collect();
        return Err(ApiError::Typed {
            status: StatusCode::CONFLICT,
            code: "tag_in_use",
            message: format!(
                "Used by {} Home row{}: {}. Archive it instead, or delete anyway.",
                rows.len(),
                if rows.len() == 1 { "" } else { "s" },
                titles.join(", ")
            ),
        });
    }
    rewrite_rows(&state, id, None).await?;
    state.store.delete_tag(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct MergeBody {
    pub into: i64,
}

/// POST /api/v1/tags/{id}/merge — move everything onto `into` and delete
/// this tag. Both must be editable and in the same scope, so a merge can
/// never publish a private tag's titles to the household.
pub async fn merge_tag(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<MergeBody>,
) -> Result<Json<TagDto>, ApiError> {
    if body.into == id {
        return Err(bad("A tag cannot be merged into itself.".into()));
    }
    let from = editable_tag(&state, &user, id).await?;
    let into = editable_tag(&state, &user, body.into).await?;
    if from.owner_user_id != into.owner_user_id {
        return Err(bad("Only tags in the same scope can be merged.".into()));
    }
    rewrite_rows(&state, id, Some(body.into)).await?;
    state.store.merge_tags(id, body.into).await?;
    let merged = visible_tag(&state, &user, body.into).await?;
    Ok(Json(tag_dto(
        merged,
        &user,
        can_curate(&state, &user).await?,
    )))
}

#[derive(Serialize)]
pub struct ItemTagsResponse {
    pub tags: Vec<TagDto>,
}

async fn item_tags_for(
    state: &AppState,
    user: &User,
    item_id: i64,
) -> Result<Vec<TagDto>, ApiError> {
    let curate = can_curate(state, user).await?;
    let on: BTreeSet<i64> = state
        .store
        .item_tag_ids(user.id, item_id)
        .await?
        .into_iter()
        .collect();
    Ok(state
        .store
        .visible_tags(user.id)
        .await?
        .into_iter()
        .filter(|t| on.contains(&t.id))
        .map(|t| tag_dto(t, user, curate))
        .collect())
}

/// GET /api/v1/items/{id}/tags — the caller-visible tags on one title.
pub async fn item_tags(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<ItemTagsResponse>, ApiError> {
    Ok(Json(ItemTagsResponse {
        tags: item_tags_for(&state, &user, id).await?,
    }))
}

fn taggable(kind: ItemKind) -> bool {
    !matches!(kind, ItemKind::Season | ItemKind::Episode)
}

#[derive(Deserialize)]
pub struct ItemTagsBody {
    pub tag_ids: Vec<i64>,
}

/// PUT /api/v1/items/{id}/tags — make the caller-*editable* tags on the
/// title exactly `tag_ids`. Tags the caller can see but not edit are left
/// as they are, so a viewer's checklist can never strip a household tag.
pub async fn put_item_tags(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<ItemTagsBody>,
) -> Result<Json<ItemTagsResponse>, ApiError> {
    let item = state
        .catalogue
        .get_item(id)
        .await?
        .ok_or(ApiError::NotFound("item"))?;
    if !taggable(item.kind) {
        return Err(bad(
            "Tag the show; its seasons and episodes follow it.".into()
        ));
    }
    let curate = can_curate(&state, &user).await?;
    let wanted: BTreeSet<i64> = body.tag_ids.into_iter().collect();
    let on: BTreeSet<i64> = state
        .store
        .item_tag_ids(user.id, id)
        .await?
        .into_iter()
        .collect();
    let tags = state.store.visible_tags(user.id).await?;
    let known: BTreeSet<i64> = tags.iter().map(|t| t.id).collect();
    if let Some(unknown) = wanted.iter().find(|t| !known.contains(t)) {
        return Err(bad(format!("Unknown tag {unknown}.")));
    }
    // Adding a tag the caller may see but not edit is refused before
    // anything is written.
    if wanted.iter().any(|t| {
        !on.contains(t)
            && tags
                .iter()
                .any(|tag| tag.id == *t && !may_edit(tag.owner_user_id, &user, curate))
    }) {
        return Err(ApiError::Forbidden);
    }
    let now = now_ms();
    for tag in tags
        .iter()
        .filter(|t| may_edit(t.owner_user_id, &user, curate))
    {
        let add = wanted.contains(&tag.id) && !on.contains(&tag.id);
        let remove = !wanted.contains(&tag.id) && on.contains(&tag.id);
        if add || remove {
            let (a, r): (&[i64], &[i64]) = if add { (&[id], &[]) } else { (&[], &[id]) };
            state
                .store
                .set_tag_members(tag.id, a, r, user.id, TagSource::Manual, now)
                .await?;
        }
    }
    Ok(Json(ItemTagsResponse {
        tags: item_tags_for(&state, &user, id).await?,
    }))
}

#[derive(Deserialize)]
pub struct MembersBody {
    #[serde(default)]
    pub add: Vec<i64>,
    #[serde(default)]
    pub remove: Vec<i64>,
}

/// POST /api/v1/tags/{id}/items — bulk add/remove (a grid selection, or
/// "select all N matching").
pub async fn tag_members(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<MembersBody>,
) -> Result<Json<TagDto>, ApiError> {
    editable_tag(&state, &user, id).await?;
    if body.add.len() + body.remove.len() > MAX_BULK_ITEMS {
        return Err(bad(format!("At most {MAX_BULK_ITEMS} titles per request.")));
    }
    state
        .store
        .set_tag_members(
            id,
            &body.add,
            &body.remove,
            user.id,
            TagSource::Manual,
            now_ms(),
        )
        .await?;
    let tag = visible_tag(&state, &user, id).await?;
    Ok(Json(tag_dto(tag, &user, can_curate(&state, &user).await?)))
}

#[derive(Deserialize)]
pub struct PageQuery {
    pub offset: Option<i64>,
    pub limit: Option<i64>,
    pub sort: Option<String>,
}

fn row_sort(sort: Option<&str>) -> RowSort {
    match sort.unwrap_or("") {
        "added" => RowSort::Added,
        "year" | "released" => RowSort::Released,
        "title" => RowSort::Title,
        "shuffle_daily" => RowSort::ShuffleDaily,
        _ => RowSort::RecentlyTagged,
    }
}

/// GET /api/v1/tags/{id}/items — the titles carrying one tag (the tag page).
pub async fn tag_items(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    read_after: ReadAfter,
    Path(id): Path<i64>,
    Query(q): Query<PageQuery>,
) -> Result<Json<ItemPage>, ApiError> {
    visible_tag(&state, &user, id).await?;
    let (offset, limit) = page(q.offset, q.limit, 60);
    let spec = RowSpec {
        all_of: vec![Term::Tag(id)],
        sort: row_sort(q.sort.as_deref()),
        ..Default::default()
    };
    Ok(Json(
        rule_page(&state, &user, &spec, None, offset, limit, read_after).await?,
    ))
}

#[derive(Serialize)]
pub struct ItemIdsResponse {
    pub item_ids: Vec<i64>,
}

/// GET /api/v1/tags/{id}/item-ids — every title carrying one tag, ids only:
/// what a client that already holds the library needs to filter it.
pub async fn tag_item_ids(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<ItemIdsResponse>, ApiError> {
    visible_tag(&state, &user, id).await?;
    let spec = RowSpec {
        all_of: vec![Term::Tag(id)],
        sort: RowSort::Title,
        ..Default::default()
    };
    let query = RuleQuery {
        viewer: user.id,
        spec: &spec,
        only_items: None,
        day: 0,
    };
    let limit = i64::try_from(MAX_BULK_ITEMS * 4).unwrap_or(i64::MAX);
    let ids = state.store.rule_items(&query, 0, limit).await?;
    Ok(Json(ItemIdsResponse {
        item_ids: ids.item_ids,
    }))
}

#[derive(Serialize)]
pub struct SuggestionDto {
    pub item: ItemDto,
    /// The classifier terms that matched, so a broad keyword can be judged.
    pub matched: Vec<String>,
}

#[derive(Serialize)]
pub struct SuggestionsResponse {
    pub suggestions: Vec<SuggestionDto>,
    pub total: i64,
    pub offset: i64,
    pub limit: i64,
}

/// GET /api/v1/tags/{id}/suggestions — the review queue. Nothing in it is
/// tagged until someone accepts it.
pub async fn tag_suggestions(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    read_after: ReadAfter,
    Path(id): Path<i64>,
    Query(q): Query<PageQuery>,
) -> Result<Json<SuggestionsResponse>, ApiError> {
    editable_tag(&state, &user, id).await?;
    let (offset, limit) = page(q.offset, q.limit, 60);
    let found = state.store.tag_suggestions(id, offset, limit).await?;
    let ids: Vec<i64> = found.suggestions.iter().map(|s| s.item_id).collect();
    let mut cards: HashMap<i64, ItemDto> = item_cards(&state, user.id, &ids, read_after)
        .await?
        .into_iter()
        .map(|c| (c.id, c))
        .collect();
    Ok(Json(SuggestionsResponse {
        suggestions: found
            .suggestions
            .into_iter()
            .filter_map(|s| {
                cards.remove(&s.item_id).map(|item| SuggestionDto {
                    item,
                    matched: s.matched,
                })
            })
            .collect(),
        total: found.total,
        offset,
        limit,
    }))
}

#[derive(Deserialize)]
pub struct ReviewBody {
    #[serde(default)]
    pub accept: Vec<i64>,
    #[serde(default)]
    pub reject: Vec<i64>,
    /// Earlier rejections to put back in the queue.
    #[serde(default)]
    pub restore: Vec<i64>,
    /// Accept every open suggestion (up to the bulk limit).
    #[serde(default)]
    pub accept_all: bool,
}

/// POST /api/v1/tags/{id}/suggestions — accept, reject or restore.
pub async fn review_suggestions(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<ReviewBody>,
) -> Result<Json<TagDto>, ApiError> {
    editable_tag(&state, &user, id).await?;
    let mut accept = body.accept;
    if body.accept_all {
        let limit = i64::try_from(MAX_BULK_ITEMS).unwrap_or(i64::MAX);
        let all = state.store.tag_suggestions(id, 0, limit).await?;
        accept.extend(all.suggestions.into_iter().map(|s| s.item_id));
    }
    if accept.len() + body.reject.len() + body.restore.len() > MAX_BULK_ITEMS * 2 {
        return Err(bad("Too many titles in one review.".into()));
    }
    let now = now_ms();
    if !body.reject.is_empty() || !body.restore.is_empty() {
        state
            .store
            .set_suggestion_rejections(id, &body.reject, &body.restore, user.id, now)
            .await?;
    }
    if !accept.is_empty() {
        state
            .store
            .set_tag_members(id, &accept, &[], user.id, TagSource::Suggestion, now)
            .await?;
    }
    let tag = visible_tag(&state, &user, id).await?;
    Ok(Json(tag_dto(tag, &user, can_curate(&state, &user).await?)))
}

#[derive(Serialize)]
pub struct ItemsResponse {
    pub items: Vec<ItemDto>,
}

/// GET /api/v1/tags/{id}/rejected — rejected suggestions, newest first.
pub async fn rejected_items(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    read_after: ReadAfter,
    Path(id): Path<i64>,
    Query(q): Query<PageQuery>,
) -> Result<Json<ItemsResponse>, ApiError> {
    editable_tag(&state, &user, id).await?;
    let (offset, limit) = page(q.offset, q.limit, 60);
    let ids = state.store.rejected_items(id, offset, limit).await?;
    Ok(Json(ItemsResponse {
        items: item_cards(&state, user.id, &ids, read_after).await?,
    }))
}

// ---------------------------------------------------------------------------
// Grid filters and `tag:` search
// ---------------------------------------------------------------------------

fn id_list(raw: Option<&str>) -> Result<Vec<i64>, ApiError> {
    raw.unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.parse::<i64>()
                .map_err(|_| bad(format!("`{s}` is not a tag id.")))
        })
        .collect()
}

/// A library grid narrowed by tags (`tags=1,2&tag_mode=any|all&not_tags=3`),
/// optionally by genre. `None` when no tag parameter was given, so the
/// grid's own path answers exactly as before.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn tag_filtered_grid(
    state: &AppState,
    user: &User,
    library_id: i64,
    tags: Option<&str>,
    tag_mode: Option<&str>,
    not_tags: Option<&str>,
    genre: Option<&str>,
    sort: Option<&str>,
    offset: i64,
    limit: i64,
    read_after: ReadAfter,
) -> Result<Option<ItemPage>, ApiError> {
    let tags = id_list(tags)?;
    let not_tags = id_list(not_tags)?;
    if tags.is_empty() && not_tags.is_empty() && sort != Some("recently_tagged") {
        return Ok(None);
    }
    let terms: Vec<Term> = tags.into_iter().map(Term::Tag).collect();
    let mut spec = RowSpec {
        libraries: vec![library_id],
        none_of: not_tags.into_iter().map(Term::Tag).collect(),
        sort: row_sort(sort.or(Some("title"))),
        ..Default::default()
    };
    if tag_mode == Some("all") {
        spec.all_of = terms;
    } else {
        spec.any_of = terms;
    }
    if let Some(genre) = genre {
        spec.all_of.push(Term::Genre(genre.to_owned()));
    }
    if sort == Some("recently_tagged") {
        spec.sort = RowSort::RecentlyTagged;
    }
    Ok(Some(
        rule_page(state, user, &spec, None, offset, limit, read_after).await?,
    ))
}

/// Split `tag:` / `-tag:` tokens (`tag:"two words"` quotes a name) out of a
/// search; what is left is the text query.
pub(crate) fn split_tag_query(query: &str) -> (String, Vec<(bool, String)>) {
    let mut text = Vec::new();
    let mut tags = Vec::new();
    let mut rest = query.trim();
    while !rest.is_empty() {
        let (negate, after) = if let Some(r) = rest.strip_prefix("-tag:") {
            (true, Some(r))
        } else if let Some(r) = rest.strip_prefix("tag:") {
            (false, Some(r))
        } else {
            (false, None)
        };
        match after {
            Some(r) => {
                let (name, remaining) = if let Some(q) = r.strip_prefix('"') {
                    match q.find('"') {
                        Some(end) => (&q[..end], &q[end + 1..]),
                        None => (q, ""),
                    }
                } else {
                    let end = r.find(char::is_whitespace).unwrap_or(r.len());
                    (&r[..end], &r[end..])
                };
                if !name.trim().is_empty() {
                    tags.push((negate, name.trim().to_owned()));
                }
                rest = remaining.trim_start();
            }
            None => {
                let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
                text.push(&rest[..end]);
                rest = rest[end..].trim_start();
            }
        }
    }
    (text.join(" "), tags)
}

/// Answer a search that names tags. `None` when it names none, so ordinary
/// search is untouched. A tag name nobody can see matches nothing.
pub(crate) async fn tag_search(
    state: &AppState,
    user: &User,
    query: &str,
    limit: i64,
    read_after: ReadAfter,
) -> Result<Option<Vec<ItemDto>>, ApiError> {
    let (text, named) = split_tag_query(query);
    if named.is_empty() {
        return Ok(None);
    }
    let visible = state.store.visible_tags(user.id).await?;
    let mut spec = RowSpec {
        sort: RowSort::Title,
        ..Default::default()
    };
    for (negate, name) in named {
        let key = tag_key(&name);
        let ids: Vec<i64> = visible
            .iter()
            .filter(|t| tag_key(&t.name) == key)
            .map(|t| t.id)
            .collect();
        if ids.is_empty() {
            if negate {
                continue;
            }
            return Ok(Some(Vec::new()));
        }
        // A name can be both a household and a private tag: either counts.
        let terms: Vec<Term> = ids.into_iter().map(Term::Tag).collect();
        if negate {
            spec.none_of.extend(terms);
        } else if terms.len() == 1 {
            spec.all_of.extend(terms);
        } else {
            spec.any_of.extend(terms);
        }
    }
    if spec.all_of.is_empty() && spec.any_of.is_empty() {
        spec.kinds = vec!["movie".into(), "show".into()];
    }
    if text.trim().is_empty() {
        return Ok(Some(
            rule_page(state, user, &spec, None, 0, limit, read_after)
                .await?
                .items,
        ));
    }
    let hits = state.catalogue.search_items(&text, PAGE_MAX).await?;
    let hit_ids: Vec<i64> = hits.iter().map(|h| h.item.id).collect();
    let query = RuleQuery {
        viewer: user.id,
        spec: &spec,
        only_items: Some(&hit_ids),
        day: today(),
    };
    let kept: BTreeSet<i64> = state
        .store
        .rule_items(&query, 0, PAGE_MAX)
        .await?
        .item_ids
        .into_iter()
        .collect();
    let ordered: Vec<i64> = hit_ids
        .into_iter()
        .filter(|id| kept.contains(id))
        .take(usize::try_from(limit).unwrap_or(usize::MAX))
        .collect();
    Ok(Some(
        item_cards(state, user.id, &ordered, read_after).await?,
    ))
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct RowDto {
    pub id: i64,
    pub title: String,
    pub scope: Scope,
    pub spec: RowSpec,
    pub editable: bool,
}

fn row_dto(row: SavedRow, user: &User, curate: bool) -> RowDto {
    RowDto {
        editable: may_edit(row.owner_user_id, user, curate),
        scope: if row.owner_user_id.is_some() {
            Scope::Private
        } else {
            Scope::Household
        },
        id: row.id,
        title: row.title,
        spec: row.spec,
    }
}

#[derive(Serialize)]
pub struct RowsResponse {
    pub rows: Vec<RowDto>,
    pub can_manage_household: bool,
}

/// GET /api/v1/rows — household rows and the caller's own.
pub async fn list_rows(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
) -> Result<Json<RowsResponse>, ApiError> {
    let curate = can_curate(&state, &user).await?;
    let rows = state.store.visible_rows(user.id).await?;
    Ok(Json(RowsResponse {
        rows: rows
            .into_iter()
            .map(|r| row_dto(r, &user, curate))
            .collect(),
        can_manage_household: curate,
    }))
}

/// A rule may only name tags its viewers could see: a household row naming
/// someone's private tag would show its titles to nobody but leak the id.
async fn check_rule(
    state: &AppState,
    user: &User,
    spec: &mut RowSpec,
    scope: Scope,
) -> Result<(), ApiError> {
    spec.validate().map_err(bad)?;
    let visible = state.store.visible_tags(user.id).await?;
    for id in spec.tag_ids() {
        let tag = visible
            .iter()
            .find(|t| t.id == id)
            .ok_or_else(|| bad(format!("Unknown tag {id}.")))?;
        if scope == Scope::Household && tag.owner_user_id.is_some() {
            return Err(bad(format!(
                "Household rows can only use household tags; “{}” is private.",
                tag.name
            )));
        }
    }
    let libraries = state.catalogue.list_libraries().await?;
    if let Some(missing) = spec
        .libraries
        .iter()
        .find(|id| !libraries.iter().any(|l| l.id == **id))
    {
        return Err(bad(format!("Unknown library {missing}.")));
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct NewRowBody {
    pub title: String,
    pub spec: RowSpec,
    pub scope: Option<Scope>,
}

/// POST /api/v1/rows — save a row. A household row needs `manage_tags`.
pub async fn create_row(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Json(mut body): Json<NewRowBody>,
) -> Result<(StatusCode, Json<RowDto>), ApiError> {
    let curate = can_curate(&state, &user).await?;
    let scope = body.scope.unwrap_or(Scope::Private);
    if scope == Scope::Household && !curate {
        return Err(ApiError::Forbidden);
    }
    let title = clean_row_title(&body.title).map_err(bad)?;
    check_rule(&state, &user, &mut body.spec, scope).await?;
    let owner = (scope == Scope::Private).then_some(user.id);
    let row = state
        .store
        .create_row(owner, &title, &body.spec, user.id, now_ms())
        .await?;
    Ok((StatusCode::CREATED, Json(row_dto(row, &user, curate))))
}

async fn editable_row(state: &AppState, user: &User, id: i64) -> Result<SavedRow, ApiError> {
    let row = state
        .store
        .curation_row(id)
        .await?
        .filter(|r| r.visible_to(user.id))
        .ok_or(ApiError::NotFound("row"))?;
    if !may_edit(row.owner_user_id, user, can_curate(state, user).await?) {
        return Err(ApiError::Forbidden);
    }
    Ok(row)
}

#[derive(Deserialize)]
pub struct RowPatch {
    pub title: Option<String>,
    pub spec: Option<RowSpec>,
}

/// PATCH /api/v1/rows/{id}
pub async fn update_row(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(patch): Json<RowPatch>,
) -> Result<Json<RowDto>, ApiError> {
    let row = editable_row(&state, &user, id).await?;
    let scope = if row.owner_user_id.is_some() {
        Scope::Private
    } else {
        Scope::Household
    };
    let title = match patch.title {
        Some(t) => clean_row_title(&t).map_err(bad)?,
        None => row.title.clone(),
    };
    let mut spec = patch.spec.unwrap_or_else(|| row.spec.clone());
    check_rule(&state, &user, &mut spec, scope).await?;
    state.store.update_row(id, &title, &spec, now_ms()).await?;
    let row = state
        .store
        .curation_row(id)
        .await?
        .ok_or(ApiError::NotFound("row"))?;
    Ok(Json(row_dto(row, &user, can_curate(&state, &user).await?)))
}

/// DELETE /api/v1/rows/{id}
pub async fn delete_row(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    editable_row(&state, &user, id).await?;
    state.store.delete_row(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// GET /api/v1/rows/{id}/items — a row's whole result ("See all").
pub async fn row_items(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    read_after: ReadAfter,
    Path(id): Path<i64>,
    Query(q): Query<PageQuery>,
) -> Result<Json<ItemPage>, ApiError> {
    let row = state
        .store
        .curation_row(id)
        .await?
        .filter(|r| r.visible_to(user.id))
        .ok_or(ApiError::NotFound("row"))?;
    let (offset, limit) = page(q.offset, q.limit, 60);
    Ok(Json(
        rule_page(&state, &user, &row.spec, None, offset, limit, read_after).await?,
    ))
}

#[derive(Deserialize)]
pub struct PreviewBody {
    pub spec: RowSpec,
    pub limit: Option<i64>,
}

/// POST /api/v1/rows/preview — what a rule being edited would show, and how
/// many titles match, before it is saved.
pub async fn preview_row(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    read_after: ReadAfter,
    Json(mut body): Json<PreviewBody>,
) -> Result<Json<ItemPage>, ApiError> {
    check_rule(&state, &user, &mut body.spec, Scope::Private).await?;
    let limit = body.limit.unwrap_or(body.spec.limit()).clamp(1, PAGE_MAX);
    Ok(Json(
        rule_page(&state, &user, &body.spec, None, 0, limit, read_after).await?,
    ))
}

// ---------------------------------------------------------------------------
// Home layout
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LayoutSource {
    /// The caller arranged it.
    User,
    /// An admin's household default.
    Household,
    /// Nobody arranged anything: today's Home.
    Builtin,
}

async fn household_default(state: &AppState) -> Result<Option<HomeLayout>, ApiError> {
    Ok(state
        .store
        .get_setting(keys::HOME_DEFAULT_LAYOUT)
        .await?
        .and_then(|raw| serde_json::from_str(&raw).ok()))
}

async fn resolved_layout(
    state: &AppState,
    user: &User,
    rows: &[SavedRow],
) -> Result<(HomeLayout, LayoutSource), ApiError> {
    let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    let (stored, source) = match state.store.home_layout(user.id).await? {
        Some(layout) => (layout, LayoutSource::User),
        None => match household_default(state).await? {
            Some(layout) => (layout, LayoutSource::Household),
            None => (HomeLayout::default(), LayoutSource::Builtin),
        },
    };
    Ok((stored.resolve(&ids), source))
}

#[derive(Serialize)]
pub struct LayoutResponse {
    pub entries: Vec<HomeEntry>,
    pub source: LayoutSource,
    pub rows: Vec<RowDto>,
    pub can_manage_household: bool,
    pub is_admin: bool,
}

/// GET /api/v1/home/layout — the caller's resolved order, hidden entries
/// included, with the rows it can name (for the Customize Home panel).
pub async fn home_layout(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
) -> Result<Json<LayoutResponse>, ApiError> {
    let curate = can_curate(&state, &user).await?;
    let rows = state.store.visible_rows(user.id).await?;
    let (layout, source) = resolved_layout(&state, &user, &rows).await?;
    Ok(Json(LayoutResponse {
        entries: layout.entries,
        source,
        rows: rows
            .into_iter()
            .map(|r| row_dto(r, &user, curate))
            .collect(),
        can_manage_household: curate,
        is_admin: user.is_admin,
    }))
}

#[derive(Deserialize)]
pub struct LayoutBody {
    pub entries: Vec<HomeEntry>,
}

async fn validated(
    state: &AppState,
    user: &User,
    body: LayoutBody,
    household: bool,
) -> Result<HomeLayout, ApiError> {
    let layout = HomeLayout {
        entries: body.entries,
    };
    layout.validate().map_err(bad)?;
    let rows = state.store.visible_rows(user.id).await?;
    for entry in &layout.entries {
        if let Some(id) = parse_row_key(&entry.key) {
            let row = rows
                .iter()
                .find(|r| r.id == id)
                .ok_or_else(|| bad(format!("Unknown row {id}.")))?;
            if household && row.owner_user_id.is_some() {
                return Err(bad(
                    "The household default can only place household rows.".into()
                ));
            }
        }
    }
    Ok(layout)
}

/// PUT /api/v1/home/layout — save the caller's order and hidden rows.
pub async fn put_home_layout(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Json(body): Json<LayoutBody>,
) -> Result<StatusCode, ApiError> {
    let layout = validated(&state, &user, body, false).await?;
    state
        .store
        .put_home_layout(user.id, Some(&layout), now_ms())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// DELETE /api/v1/home/layout — back to the household default.
pub async fn reset_home_layout(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
) -> Result<StatusCode, ApiError> {
    state.store.put_home_layout(user.id, None, now_ms()).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// PUT /api/v1/home/default-layout — admin: the household default for
/// everyone who has not arranged their own Home.
pub async fn put_default_layout(
    AdminUser(user): AdminUser,
    State(state): State<AppState>,
    Json(body): Json<LayoutBody>,
) -> Result<StatusCode, ApiError> {
    let layout = validated(&state, &user, body, true).await?;
    let raw = serde_json::to_string(&layout).map_err(|e| ApiError::BadRequest(e.to_string()))?;
    state
        .store
        .put_setting(keys::HOME_DEFAULT_LAYOUT, &raw)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// DELETE /api/v1/home/default-layout — admin: back to the built-in Home.
pub async fn reset_default_layout(
    AdminUser(_): AdminUser,
    State(state): State<AppState>,
) -> Result<StatusCode, ApiError> {
    state
        .store
        .put_setting(keys::HOME_DEFAULT_LAYOUT, "")
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
pub struct HomeRowEntry {
    pub key: String,
    /// `builtin` rows are drawn by the client from the endpoints it already
    /// calls; `row` entries carry their titles in `rows`.
    pub kind: &'static str,
    pub row_id: Option<i64>,
}

#[derive(Serialize)]
pub struct HomeRowBody {
    pub title: String,
    pub items: Vec<ItemDto>,
    pub total: i64,
}

#[derive(Serialize)]
pub struct HomeRowsResponse {
    /// Visible entries only, in order.
    pub layout: Vec<HomeRowEntry>,
    pub rows: BTreeMap<String, HomeRowBody>,
}

/// GET /api/v1/home/rows — what Home draws: the ordered visible entries and
/// each custom row's first page. Built-in rows keep coming from `/hubs`,
/// `/home/previews` and `/coming-soon`, so their read budgets are unchanged;
/// an empty custom row is left out rather than drawn blank.
pub async fn home_rows(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    read_after: ReadAfter,
) -> Result<Json<HomeRowsResponse>, ApiError> {
    let rows = state.store.visible_rows(user.id).await?;
    let (layout, _) = resolved_layout(&state, &user, &rows).await?;
    let wanted: Vec<&SavedRow> = layout
        .entries
        .iter()
        .filter(|e| !e.hidden)
        .filter_map(|e| parse_row_key(&e.key))
        .filter_map(|id| rows.iter().find(|r| r.id == id))
        .collect();
    let pages = try_join_all(wanted.iter().map(|row| {
        let state = &state;
        let user = &user;
        async move {
            let limit = row.spec.limit();
            let page = rule_page(state, user, &row.spec, None, 0, limit, read_after).await?;
            Ok::<_, ApiError>((row.id, row.title.clone(), page))
        }
    }))
    .await?;
    let mut bodies = BTreeMap::new();
    for (id, title, page) in pages {
        if page.total > 0 {
            bodies.insert(
                id.to_string(),
                HomeRowBody {
                    title,
                    items: page.items,
                    total: page.total,
                },
            );
        }
    }
    let entries = layout
        .entries
        .into_iter()
        .filter(|e| !e.hidden)
        .filter_map(|e| match parse_row_key(&e.key) {
            Some(id) if bodies.contains_key(&id.to_string()) => Some(HomeRowEntry {
                key: e.key,
                kind: "row",
                row_id: Some(id),
            }),
            Some(_) => None,
            None => Some(HomeRowEntry {
                key: e.key,
                kind: "builtin",
                row_id: None,
            }),
        })
        .collect();
    Ok(Json(HomeRowsResponse {
        layout: entries,
        rows: bodies,
    }))
}

// ---------------------------------------------------------------------------
// Grants
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
pub struct GrantsBody {
    pub grants: Vec<Grant>,
}

/// GET /api/v1/users/{id}/grants — admin.
pub async fn user_grants(
    AdminUser(_): AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<GrantsBody>, ApiError> {
    Ok(Json(GrantsBody {
        grants: state.store.user_grants(id).await?,
    }))
}

/// PUT /api/v1/users/{id}/grants — admin: exactly these grants.
pub async fn put_user_grants(
    AdminUser(admin): AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<GrantsBody>,
) -> Result<Json<GrantsBody>, ApiError> {
    if state.store.get_user(id).await?.is_none() {
        return Err(ApiError::NotFound("user"));
    }
    let now = now_ms();
    for grant in Grant::ALL {
        state
            .store
            .set_user_grant(id, grant, body.grants.contains(&grant), admin.id, now)
            .await?;
    }
    Ok(Json(GrantsBody {
        grants: state.store.user_grants(id).await?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_tokens_split_from_text_with_quotes_and_negation() {
        let (text, tags) = split_tag_query(r#"harbor tag:gay-themed -tag:"date night" lights"#);
        assert_eq!(text, "harbor lights");
        assert_eq!(
            tags,
            vec![
                (false, "gay-themed".to_owned()),
                (true, "date night".to_owned())
            ]
        );
        let (text, tags) = split_tag_query("plain words");
        assert_eq!((text.as_str(), tags.len()), ("plain words", 0));
        let (text, tags) = split_tag_query("tag:");
        assert_eq!((text.as_str(), tags.len()), ("", 0));
    }

    #[test]
    fn dropping_a_tag_from_a_rule_deletes_a_row_left_with_nothing() {
        let spec = RowSpec {
            all_of: vec![Term::Tag(1)],
            ..Default::default()
        };
        assert!(rewrite_rule(&spec, 1, None).is_none());
        let spec = RowSpec {
            all_of: vec![Term::Tag(1)],
            any_of: vec![Term::Tag(2)],
            ..Default::default()
        };
        assert_eq!(
            rewrite_rule(&spec, 1, None).expect("kept").any_of,
            vec![Term::Tag(2)]
        );
        let merged = rewrite_rule(&spec, 1, Some(2)).expect("kept");
        assert_eq!(
            (merged.all_of, merged.any_of),
            (vec![Term::Tag(2)], vec![Term::Tag(2)])
        );
    }

    #[test]
    fn private_and_household_edit_rights() {
        let user = User {
            id: 5,
            username: "u".into(),
            password_hash: String::new(),
            is_admin: false,
            created_at: 0,
        };
        assert!(may_edit(Some(5), &user, false), "own private tag");
        assert!(
            !may_edit(Some(6), &user, true),
            "someone else's private tag, even with the grant"
        );
        assert!(
            !may_edit(None, &user, false),
            "household tag without the grant"
        );
        assert!(may_edit(None, &user, true), "household tag with the grant");
    }

    #[test]
    fn row_sort_names() {
        assert_eq!(row_sort(Some("title")), RowSort::Title);
        assert_eq!(row_sort(None), RowSort::RecentlyTagged);
    }
}
