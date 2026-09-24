// K-05 section 3.4: the calls whose statements the query-plan protocol
// measures, shared by the `query_plans` example (standalone SQLite) and the
// ignored Hiqlite capture test in `tests/store_contract.rs`, so both
// backends' plans are taken for the same requests. Every argument names a row
// the `catalogue_fixture` example creates (movies are ids 1..=20000, show `k`
// is id 20001 + 105k, user 1 owns token `fixture-token-1`).
//
// Nothing here knows SQL: the statements come from the store's own TRACE
// events (`trace_statement`), so the plans are taken on what executed.

use std::sync::{Arc, Mutex};

use plurx_core::domain::{ItemKind, ItemSort};
use plurx_core::store::{keys, MediaStore, Store, UserStore, WatchStore};
use serde_json::{json, Value};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata, Subscriber};

/// One statement to plan and time: which call produced it, the statement
/// name the store traced, the SQL as executed and the values it was bound to.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Planned {
    pub label: String,
    pub statement: String,
    pub sql: String,
    pub params: Vec<Value>,
}

/// A global subscriber that keeps only the store's statement events.
#[derive(Clone, Default)]
pub struct StatementCapture {
    events: Arc<Mutex<Vec<(String, String)>>>,
}

impl StatementCapture {
    pub fn drain(&self) -> Vec<(String, String)> {
        std::mem::take(&mut *self.events.lock().expect("capture lock"))
    }
}

#[derive(Default)]
struct StatementVisitor {
    statement: Option<String>,
    sql: Option<String>,
}

impl Visit for StatementVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "statement" => self.statement = Some(value.to_owned()),
            "sql" => self.sql = Some(value.to_owned()),
            _ => {}
        }
    }

    fn record_debug(&mut self, _field: &Field, _value: &dyn std::fmt::Debug) {}
}

impl Subscriber for StatementCapture {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        *metadata.level() == Level::TRACE
            && matches!(
                metadata.target(),
                "plurx_core::store::sqlite" | "plurx_core::store::hiqlite"
            )
    }

    fn new_span(&self, _span: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut visitor = StatementVisitor::default();
        event.record(&mut visitor);
        if let (Some(statement), Some(sql)) = (visitor.statement, visitor.sql) {
            self.events
                .lock()
                .expect("capture lock")
                .push((statement, sql));
        }
    }

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

const USER: i64 = 1;
const TOKEN: &str = "fixture-token-1";
const PAGE_OFFSET: i64 = 10_000;
const PAGE: i64 = 50;
const RAIL: i64 = 24;
/// `HOME_PREVIEW_LIMIT` in `plurxd::http::browse`.
const HOME_PREVIEW: i64 = 24;
const SEARCH: &str = "Movie 0123";
const MOVIE_TMDB: i64 = 1_012_345;
const MOVIE_ID: i64 = 12_345;

fn page_ids() -> Vec<i64> {
    (1..=PAGE).collect()
}

fn show_ids() -> Vec<i64> {
    (0..RAIL).map(|show| 20_001 + show * 105).collect()
}

/// The values each statement is bound to, in its backend's binding order.
/// The SQLite statements number `?N` explicitly; the replicated statements
/// introduce `$N` in first-appearance order, so the two lists differ where
/// the SQL does.
///
/// `pass` counts earlier executions of the same statement in the same call:
/// `recently_added` widens its window and runs once per pass.
fn bind(backend: &str, label: &str, statement: &str, pass: u32) -> Vec<Value> {
    let hiqlite = backend == "hiqlite";
    let library = |label: &str| -> (i64, i64, Value) {
        match label {
            "library_page_title_genre" => (1, PAGE_OFFSET, json!("Drama")),
            "shows_page_title" => (2, 200, Value::Null),
            "home_page_recorded" => (3, 0, Value::Null),
            _ => (1, PAGE_OFFSET, Value::Null),
        }
    };
    let ids_json = || json!(serde_json::to_string(&page_ids()).expect("ids"));
    match statement {
        "list_top_items_in_genre.count" => {
            let (library_id, _, genre) = library(label);
            vec![json!(library_id), genre]
        }
        "list_top_items_in_genre.page" => {
            let (library_id, offset, genre) = library(label);
            if hiqlite {
                vec![json!(library_id), genre, json!(PAGE), json!(offset)]
            } else {
                vec![json!(library_id), json!(offset), json!(PAGE), genre]
            }
        }
        "home_preview_pages" => vec![json!(HOME_PREVIEW)],
        "recently_added" => {
            let library = if label == "recently_added_shows" {
                json!(2)
            } else {
                Value::Null
            };
            // The widening window: eight rows a card on the first pass,
            // doubled on each later one, bound as the zero-based offset of
            // the window's last row (K-05 M4).
            vec![library, json!(RAIL * 8 * (1_i64 << pass) - 1), json!(RAIL)]
        }
        "search_items" => vec![
            json!(plurx_core::metadata::classification::fts_query(SEARCH).expect("tokens")),
            json!(PAGE),
        ],
        "item_by_external_id" => vec![json!("movie"), json!(MOVIE_TMDB), Value::Null],
        "files_for_item" => vec![json!(MOVIE_ID)],
        "item_media_facts" if hiqlite => vec![ids_json()],
        "item_media_facts" => vec![],
        "watch_map" if hiqlite => vec![ids_json(), json!(USER)],
        "watch_map" => vec![json!(USER), ids_json()],
        "watch_rollups" if hiqlite => vec![
            json!(serde_json::to_string(&show_ids()).expect("ids")),
            json!(USER),
        ],
        "watch_rollups" => vec![json!(USER)],
        "continue_watching" | "next_up" => vec![json!(USER), json!(RAIL)],
        "authenticate_token" => {
            let keys = [
                json!(keys::AUTH_TOKEN_EXPIRY_ENABLED),
                json!(keys::AUTH_TOKEN_IDLE_DAYS),
                json!(keys::AUTH_TOKEN_EXPIRY_SINCE),
            ];
            if hiqlite {
                keys.into_iter().chain([json!(TOKEN)]).collect()
            } else {
                [json!(TOKEN)].into_iter().chain(keys).collect()
            }
        }
        other => panic!("no binding for statement {other}"),
    }
}

/// Run every measured call once and return the statements it executed.
pub async fn drive(store: &dyn Store, backend: &str, capture: &StatementCapture) -> Vec<Planned> {
    let mut planned = Vec::new();
    let mut take = |label: &str| {
        let mut passes = std::collections::HashMap::<String, u32>::new();
        for (statement, sql) in capture.drain() {
            let pass = passes.entry(statement.clone()).or_default();
            let params = bind(backend, label, &statement, *pass);
            *pass += 1;
            planned.push(Planned {
                label: label.to_owned(),
                statement,
                sql,
                params,
            });
        }
    };
    capture.drain();
    for (label, library_id, sort, offset, genre) in [
        ("library_page_title", 1, ItemSort::Title, PAGE_OFFSET, None),
        (
            "library_page_title_genre",
            1,
            ItemSort::Title,
            PAGE_OFFSET,
            Some("Drama"),
        ),
        ("library_page_added", 1, ItemSort::Added, PAGE_OFFSET, None),
        ("library_page_year", 1, ItemSort::Year, PAGE_OFFSET, None),
        (
            "library_page_resolution",
            1,
            ItemSort::Resolution,
            PAGE_OFFSET,
            None,
        ),
        ("shows_page_title", 2, ItemSort::Title, 200, None),
        ("home_page_recorded", 3, ItemSort::Recorded, 0, None),
    ] {
        MediaStore::list_top_items_in_genre(store, library_id, sort, offset, PAGE, genre)
            .await
            .expect(label);
        take(label);
    }
    MediaStore::home_preview_pages(store, HOME_PREVIEW)
        .await
        .expect("home previews");
    take("home_preview_pages");
    MediaStore::recently_added(store, None, RAIL)
        .await
        .expect("recently added");
    take("recently_added_all");
    MediaStore::recently_added(store, Some(2), RAIL)
        .await
        .expect("recently added shows");
    take("recently_added_shows");
    MediaStore::search_items(store, SEARCH, PAGE)
        .await
        .expect("search");
    take("search");
    MediaStore::item_by_external_id(store, ItemKind::Movie, Some(MOVIE_TMDB), None)
        .await
        .expect("external id");
    take("item_by_external_id");
    MediaStore::files_for_item(store, MOVIE_ID)
        .await
        .expect("files");
    take("files_for_item");
    MediaStore::item_media_facts(store, &page_ids())
        .await
        .expect("facts");
    take("item_media_facts");
    WatchStore::watch_map(store, USER, &page_ids())
        .await
        .expect("watch map");
    take("watch_map");
    WatchStore::watch_rollups(store, USER, &show_ids())
        .await
        .expect("rollups");
    take("watch_rollups");
    WatchStore::continue_watching(store, USER, RAIL)
        .await
        .expect("continue watching");
    take("continue_watching");
    WatchStore::next_up(store, USER, RAIL)
        .await
        .expect("next up");
    take("next_up");
    // The token may be unknown to a store that does not hold the fixture;
    // only the statement matters here.
    let _ = UserStore::authenticate_token(store, TOKEN).await;
    take("authenticate_token");
    planned
}
