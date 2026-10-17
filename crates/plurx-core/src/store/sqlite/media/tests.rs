use std::path::PathBuf;

use crate::domain::{
    ArtworkAttempt, AudioStream, ItemKind, ItemSort, LibraryKind, MetadataPatch, NewItem,
    NewLibrary, ProbeResult,
};
use crate::store::{LibraryStore, MediaStore, SqliteStore};

#[tokio::test]
async fn content_encoding_publication_is_source_fenced_and_reprobe_invalidates_it() {
    let store = SqliteStore::open_in_memory().expect("fixture succeeds");
    let library = store
        .create_library(&NewLibrary {
            name: "Movies".into(),
            kind: LibraryKind::Movies,
            paths: vec![PathBuf::from("/media")],
            anime: false,
        })
        .await
        .expect("fixture succeeds");
    let item = store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "Sample".into(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("fixture succeeds");
    let probe = ProbeResult {
        raw_json: Some("{\"streams\":[]}".into()),
        ..Default::default()
    };
    let file = store
        .upsert_file(item, "/media/sample.mkv", 100, 1, &probe)
        .await
        .expect("fixture succeeds");
    assert!(store
        .merge_file_probe_content_encoding(file, 100, 1, "{\"outcome\":\"measured\"}")
        .await
        .expect("fixture succeeds"));
    assert!(!store
        .merge_file_probe_content_encoding(file, 101, 1, "{}")
        .await
        .expect("fixture succeeds"));
    assert!(!store
        .merge_file_probe_content_encoding(file, 100, 2, "{}")
        .await
        .expect("fixture succeeds"));
    let document: serde_json::Value = serde_json::from_str(
        &store
            .get_file_probe_json(file)
            .await
            .expect("fixture succeeds")
            .expect("fixture succeeds"),
    )
    .expect("fixture succeeds");
    assert_eq!(document["plurx_content_encoding"]["outcome"], "measured");
    store
        .upsert_file(item, "/media/sample.mkv", 101, 2, &probe)
        .await
        .expect("fixture succeeds");
    let document: serde_json::Value = serde_json::from_str(
        &store
            .get_file_probe_json(file)
            .await
            .expect("fixture succeeds")
            .expect("fixture succeeds"),
    )
    .expect("fixture succeeds");
    assert!(document.get("plurx_content_encoding").is_none());
}

/// The whole-catalogue statement `recently_added` ran before K-05, kept
/// as the oracle the windowed read must agree with.
fn legacy_recently_added(
    conn: &rusqlite::Connection,
    library_id: Option<i64>,
    limit: i64,
) -> Vec<(i64, Option<String>, Option<String>)> {
    let sql = format!(
            "WITH ranked AS (
                 SELECT {i},
                        show.title AS rail_show_title,
                        season.poster_path AS rail_season_poster,
                        ROW_NUMBER() OVER (
                            PARTITION BY
                                CASE WHEN i.kind = 'episode' AND show.id IS NOT NULL
                                     THEN 'show:' || show.id
                                     ELSE 'item:' || i.id END
                            ORDER BY i.added_at DESC,
                                     COALESCE(season.season_number, -1) DESC,
                                     COALESCE(i.episode_number, -1) DESC,
                                     i.id DESC
                        ) AS rail_rank
                 FROM items i
                 LEFT JOIN items season
                        ON season.id = i.parent_id AND i.kind = 'episode'
                 LEFT JOIN items show ON show.id = season.parent_id
                 WHERE i.kind IN ('movie','episode','video','folder','book','audiobook')
                   AND (?1 IS NULL OR i.library_id = ?1)
                   AND (?1 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = i.library_id AND l.kind = 'recordings'))
             )
             SELECT r.id, r.rail_show_title, r.rail_season_poster
             FROM ranked r
             WHERE r.rail_rank = 1
             ORDER BY r.added_at DESC, r.id DESC
             LIMIT ?2",
            i = super::item_cols("i"),
        );
    let mut stmt = conn.prepare(&sql).expect("legacy statement");
    stmt.query_map(rusqlite::params![library_id, limit], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
    })
    .expect("legacy query")
    .collect::<rusqlite::Result<Vec<_>>>()
    .expect("legacy rows")
}

async fn windowed_recently_added(
    store: &SqliteStore,
    library_id: Option<i64>,
    limit: i64,
) -> Vec<(i64, Option<String>, Option<String>)> {
    store
        .recently_added(library_id, limit)
        .await
        .expect("recently added")
        .into_iter()
        .map(|card| (card.item.id, card.show_title, card.season_poster))
        .collect()
}

/// (id, library, kind, parent, added_at, season, episode)
type RecentSeedRow = (
    i64,
    i64,
    &'static str,
    Option<i64>,
    i64,
    Option<i64>,
    Option<i64>,
);

/// Seed libraries 1 Movies, 2 Shows, 3 Home and 4 Recordings, then run
/// `insert` (id, library, kind, parent, added_at, season, episode) rows.
async fn seed_recent(store: &SqliteStore, rows: Vec<RecentSeedRow>) {
    store
        .with_conn(move |conn| {
            conn.execute_batch(
                "INSERT INTO libraries(id,name,kind,paths) VALUES
                         (1,'Movies','movies','[]'),(2,'Shows','shows','[]'),
                         (3,'Home','home','[]'),(4,'Recordings','recordings','[]');",
            )?;
            let mut insert = conn.prepare(
                "INSERT INTO items(id,library_id,kind,parent_id,title,sort_title,
                                       added_at,season_number,episode_number,poster_path,
                                       tags,genres)
                     VALUES(?1,?2,?3,?4,'t'||?1,'t'||?1,?5,?6,?7,'poster-'||?1,'[]','[]')",
            )?;
            for (id, library, kind, parent, added_at, season, episode) in rows {
                insert.execute(rusqlite::params![
                    id, library, kind, parent, added_at, season, episode
                ])?;
            }
            Ok(())
        })
        .await
        .expect("seed");
}

/// K-05 M4: one show whose episodes fill the whole first window (nine
/// times the limit, newer than everything else) leaves the first pass
/// with a single card, so the read must widen until it has the limit.
#[tokio::test]
async fn recently_added_widens_window() {
    const LIMIT: i64 = 4;
    let store = SqliteStore::open_in_memory().expect("open");
    let mut rows = vec![
        (1, 2, "show", None, 0, None, None),
        (2, 2, "season", Some(1), 0, Some(1), None),
    ];
    for episode in 0..(9 * LIMIT) {
        rows.push((
            100 + episode,
            2,
            "episode",
            Some(2),
            10_000 + episode,
            Some(1),
            Some(episode + 1),
        ));
    }
    for movie in 0..LIMIT {
        rows.push((1_000 + movie, 1, "movie", None, 5_000 + movie, None, None));
    }
    seed_recent(&store, rows).await;
    let cards = windowed_recently_added(&store, None, LIMIT).await;
    assert_eq!(
        cards.iter().map(|card| card.0).collect::<Vec<_>>(),
        [100 + 9 * LIMIT - 1, 1_003, 1_002, 1_001],
        "the show's newest episode, then the newest movies"
    );
    let legacy = store
        .with_conn(move |conn| Ok(legacy_recently_added(conn, None, LIMIT)))
        .await
        .expect("legacy");
    assert_eq!(cards, legacy);
}

/// K-05 M4: the windowed read returns exactly what the whole-catalogue
/// ranking returned, over 200 seeded catalogues dense in `added_at` ties
/// (including ties across the window boundary), shows whose episodes span
/// several seasons, home folders and photos, and Recordings, for every
/// library filter and several limits.
#[tokio::test]
async fn recently_added_matches_the_whole_catalogue_ranking() {
    for seed in 0..200_u64 {
        let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut next = move |bound: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            i64::try_from(state % bound).expect("small")
        };
        let store = SqliteStore::open_in_memory().expect("open");
        let mut rows = Vec::new();
        let mut id = 1;
        let stamp = |next: &mut dyn FnMut(u64) -> i64| 1_000 + next(12);
        for _ in 0..(5 + next(20)) {
            rows.push((id, 1, "movie", None, stamp(&mut next), None, None));
            id += 1;
        }
        for _ in 0..(1 + next(4)) {
            let show = id;
            rows.push((show, 2, "show", None, stamp(&mut next), None, None));
            id += 1;
            for season in 1..=(1 + next(3)) {
                let season_id = id;
                rows.push((
                    season_id,
                    2,
                    "season",
                    Some(show),
                    stamp(&mut next),
                    Some(season),
                    None,
                ));
                id += 1;
                for episode in 1..=(1 + next(6)) {
                    rows.push((
                        id,
                        2,
                        "episode",
                        Some(season_id),
                        stamp(&mut next),
                        Some(season),
                        Some(episode),
                    ));
                    id += 1;
                }
            }
        }
        for _ in 0..next(4) {
            let folder = id;
            rows.push((folder, 3, "folder", None, stamp(&mut next), None, None));
            id += 1;
            rows.push((id, 3, "video", Some(folder), stamp(&mut next), None, None));
            id += 1;
            rows.push((id, 3, "photo", Some(folder), stamp(&mut next), None, None));
            id += 1;
        }
        for _ in 0..next(8) {
            rows.push((id, 4, "video", None, stamp(&mut next), None, None));
            id += 1;
        }
        seed_recent(&store, rows).await;
        for library_id in [None, Some(1), Some(2), Some(3), Some(4)] {
            for limit in [1, 2, 3, 7, 50] {
                let cards = windowed_recently_added(&store, library_id, limit).await;
                let legacy = store
                    .with_conn(move |conn| Ok(legacy_recently_added(conn, library_id, limit)))
                    .await
                    .expect("legacy");
                assert_eq!(
                    cards, legacy,
                    "seed {seed}, library {library_id:?}, limit {limit}"
                );
            }
        }
    }
}

#[tokio::test]
async fn scan_identity_directory_lookup_uses_the_path_range_index() {
    let store = SqliteStore::open_in_memory().expect("open");
    store
        .with_conn(|conn| {
            let mut statement = conn.prepare(
                "EXPLAIN QUERY PLAN
                     SELECT show.id
                     FROM files f INDEXED BY sqlite_autoindex_files_1
                     JOIN items episode ON episode.id = f.item_id AND episode.kind = 'episode'
                     JOIN items season ON season.id = episode.parent_id AND season.kind = 'season'
                     JOIN items show ON show.id = season.parent_id AND show.kind = 'show'
                     WHERE f.path >= ?1 AND f.path < ?2
                       AND episode.library_id = ?3
                       AND season.library_id = ?3
                       AND show.library_id = ?3",
            )?;
            let details = statement
                .query_map(
                    rusqlite::params!["/media/Show/", "/media/Show0", 1],
                    |row| row.get::<_, String>(3),
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            assert!(
                details.iter().any(|detail| {
                    detail.contains("sqlite_autoindex_files_1")
                        && detail.contains("path>?")
                        && detail.contains("path<?")
                }),
                "query plan must use the bounded path index: {details:?}"
            );
            Ok(())
        })
        .await
        .expect("query plan");
}

/// One item, two versions: the 2160p Dolby Vision remux and the 720p copy
/// someone kept for a phone. The block must read as the library actually
/// is — a 720p answer here is the exact failure the aggregation rule (see
/// `MediaFacts`) exists to prevent — while `files`/`bytes` still count
/// both, because both are on the volume.
#[tokio::test]
async fn media_facts_describe_the_best_version_and_count_them_all() {
    let store = SqliteStore::open_in_memory().expect("open");
    let lib = store
        .create_library(&NewLibrary {
            name: "Movies".into(),
            kind: LibraryKind::Movies,
            paths: vec![PathBuf::from("/media")],
            anime: false,
        })
        .await
        .expect("lib")
        .id;
    let item = store
        .insert_item(&NewItem {
            library_id: lib,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "Neon District 2049".into(),
            year: Some(2017),
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    // Deliberately inserted worst-first: a "first row wins" bug would pass
    // if the good version happened to be first.
    store
        .upsert_file(
            item,
            "/media/BR2049 720p.mp4",
            6_800_000_000,
            1,
            &ProbeResult {
                container: Some("mp4".into()),
                video_codec: Some("h264".into()),
                width: Some(1280),
                height: Some(720),
                bitrate: Some(5_000_000),
                audio_streams: vec![AudioStream {
                    index: 0,
                    codec: "aac".into(),
                    channels: Some(2),
                    sample_rate: None,
                    channel_layout: None,
                    language: Some("eng".into()),
                    title: None,
                    default: true,
                }],
                ..Default::default()
            },
        )
        .await
        .expect("720p");
    store
        .upsert_file(
            item,
            "/media/BR2049 2160p.mkv",
            69_000_000_000,
            1,
            &ProbeResult {
                container: Some("mkv".into()),
                video_codec: Some("hevc".into()),
                width: Some(3840),
                height: Some(2160),
                bit_depth: Some(10),
                hdr: Some("dolby_vision".into()),
                hdr_format: Some("Dolby Vision \u{b7} Profile 7 (HDR10-compatible)".into()),
                bitrate: Some(56_000_000),
                audio_streams: vec![
                    AudioStream {
                        index: 0,
                        codec: "truehd".into(),
                        channels: Some(8),
                        sample_rate: None,
                        channel_layout: None,
                        language: Some("eng".into()),
                        title: None,
                        default: true,
                    },
                    AudioStream {
                        index: 1,
                        codec: "eac3".into(),
                        channels: Some(6),
                        sample_rate: None,
                        channel_layout: None,
                        language: Some("eng".into()),
                        title: Some("Commentary".into()),
                        default: false,
                    },
                ],
                ..Default::default()
            },
        )
        .await
        .expect("2160p");

    let facts = store.item_media_facts(&[item]).await.expect("facts");
    let f = facts.get(&item).expect("item is in the map");
    assert_eq!(f.files, 2);
    assert_eq!(f.bytes, 75_800_000_000, "bytes is the sum, not the max");
    assert_eq!(f.height, Some(2160));
    assert_eq!(f.video.as_deref(), Some("HEVC"));
    assert_eq!(f.hdr.as_deref(), Some("dolby_vision"));
    assert_eq!(f.audio.as_deref(), Some("TrueHD 7.1"));
    assert_eq!(f.container.as_deref(), Some("MKV"));
    // The same number the resolution badge shows, from the same rule.
    let heights = store.item_max_heights(&[item]).await.expect("heights");
    assert_eq!(f.height, heights.get(&item).copied());
}

/// Equal heights: the remux (higher bitrate) is the item's face, and an
/// item with no files at all is absent from the map rather than present
/// with zeroes — "no files" and "a file of nothing" are different answers.
#[tokio::test]
async fn media_facts_break_ties_on_bitrate_and_skip_fileless_items() {
    let store = SqliteStore::open_in_memory().expect("open");
    let lib = store
        .create_library(&NewLibrary {
            name: "Movies".into(),
            kind: LibraryKind::Movies,
            paths: vec![PathBuf::from("/media")],
            anime: false,
        })
        .await
        .expect("lib")
        .id;
    let new = |title: &str| NewItem {
        library_id: lib,
        kind: ItemKind::Movie,
        parent_id: None,
        title: title.to_owned(),
        year: Some(2024),
        season_number: None,
        episode_number: None,
    };
    let item = store.insert_item(&new("Dune")).await.expect("item");
    let empty = store.insert_item(&new("Ghost")).await.expect("empty");
    for (path, codec, bitrate, size) in [
        (
            "/media/Dune x264.mkv",
            "h264",
            12_000_000,
            20_000_000_000i64,
        ),
        ("/media/Dune remux.mkv", "hevc", 68_000_000, 60_000_000_000),
    ] {
        store
            .upsert_file(
                item,
                path,
                size,
                1,
                &ProbeResult {
                    container: Some("mkv".into()),
                    video_codec: Some(codec.into()),
                    width: Some(3840),
                    height: Some(2160),
                    bitrate: Some(bitrate),
                    ..Default::default()
                },
            )
            .await
            .expect("file");
    }

    let facts = store.item_media_facts(&[item, empty]).await.expect("facts");
    assert_eq!(
        facts.get(&item).expect("dune").video.as_deref(),
        Some("HEVC")
    );
    assert!(
        !facts.contains_key(&empty),
        "an item with no files must not fabricate a block"
    );
    // No ids at all: no query, no map, no panic.
    assert!(store.item_media_facts(&[]).await.expect("none").is_empty());
}

/// The N+1 check, measured rather than asserted: sqlite's own statement
/// trace counts what the page really executed. A 200-item page is the
/// biggest one `MAX_LIMIT` allows, and it must cost exactly what a 2-item
/// page costs — one statement — or the library grid degrades into 200
/// round trips the moment someone scrolls.
#[tokio::test]
async fn media_facts_cost_one_statement_whatever_the_page_size() {
    use rusqlite::trace::{TraceEvent, TraceEventCodes};
    use std::sync::{LazyLock, Mutex};

    static TRACED: LazyLock<Mutex<Vec<String>>> = LazyLock::new(|| Mutex::new(Vec::new()));
    fn record(event: TraceEvent<'_>) {
        if let TraceEvent::Stmt(_, sql) = event {
            if let Ok(mut log) = TRACED.lock() {
                log.push(sql.to_owned());
            }
        }
    }

    let store = SqliteStore::open_in_memory().expect("open");
    let lib = store
        .create_library(&NewLibrary {
            name: "Movies".into(),
            kind: LibraryKind::Movies,
            paths: vec![PathBuf::from("/media")],
            anime: false,
        })
        .await
        .expect("lib")
        .id;
    let mut ids = Vec::new();
    for n in 0..200 {
        let item = store
            .insert_item(&NewItem {
                library_id: lib,
                kind: ItemKind::Movie,
                parent_id: None,
                title: format!("Film {n}"),
                year: Some(2024),
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        // Two files each: the fan-out this measurement is guarding against
        // would be per file, not just per item.
        for (suffix, height) in [("2160p.mkv", 2160), ("720p.mp4", 720)] {
            store
                .upsert_file(
                    item,
                    &format!("/media/Film {n} {suffix}"),
                    1_000_000_000 + i64::from(height),
                    1,
                    &ProbeResult {
                        container: Some("mkv".into()),
                        video_codec: Some("hevc".into()),
                        height: Some(i64::from(height)),
                        bitrate: Some(20_000_000),
                        audio_streams: vec![AudioStream {
                            index: 0,
                            codec: "truehd".into(),
                            channels: Some(8),
                            sample_rate: None,
                            channel_layout: None,
                            language: Some("eng".into()),
                            title: None,
                            default: true,
                        }],
                        ..Default::default()
                    },
                )
                .await
                .expect("file");
        }
        ids.push(item);
    }

    let measure = |ids: Vec<i64>| {
        let store = &store;
        async move {
            TRACED.lock().expect("log").clear();
            {
                let conn = store.conn.lock().expect("conn");
                conn.trace_v2(TraceEventCodes::SQLITE_TRACE_STMT, Some(record));
            }
            let facts = store.item_media_facts(&ids).await.expect("facts");
            {
                let conn = store.conn.lock().expect("conn");
                conn.trace_v2(TraceEventCodes::SQLITE_TRACE_STMT, None);
            }
            let statements = TRACED.lock().expect("log").clone();
            (facts.len(), statements)
        }
    };

    let (two, small) = measure(ids[..2].to_vec()).await;
    assert_eq!(two, 2);
    let (all, big) = measure(ids.clone()).await;
    assert_eq!(all, 200, "every item on the page got its block");
    assert_eq!(
        big.len(),
        small.len(),
        "statement count grew with the page: {big:#?}"
    );
    assert_eq!(
        big.len(),
        1,
        "the page cost more than one statement: {big:#?}"
    );
}

/// The census PERF-PLAN §5 is waiting on, against rows whose answer is
/// known by construction. The arithmetic is trivial; the SQL is not, and
/// every mistake it can make (counting unprobed rows as SDR, missing the
/// 4K cut, treating empty string as a flavour) produces a plausible number
/// rather than an error.
#[tokio::test]
async fn media_shape_counts_what_the_transcoder_cares_about() {
    let store = SqliteStore::open_in_memory().expect("open");
    let lib = store
        .create_library(&NewLibrary {
            name: "Movies".into(),
            kind: LibraryKind::Movies,
            paths: vec![PathBuf::from("/media")],
            anime: false,
        })
        .await
        .expect("lib")
        .id;

    let add = |title: &str, hdr: Option<&str>, height: i64, bitrate: i64| {
        let (title, hdr) = (title.to_owned(), hdr.map(str::to_owned));
        let store = &store;
        async move {
            let item = store
                .insert_item(&NewItem {
                    library_id: lib,
                    kind: ItemKind::Movie,
                    parent_id: None,
                    title: title.clone(),
                    year: Some(2024),
                    season_number: None,
                    episode_number: None,
                })
                .await
                .expect("item");
            store
                .upsert_file(
                    item,
                    &format!("/media/{title}.mkv"),
                    1,
                    1,
                    &ProbeResult {
                        container: Some("mkv".into()),
                        video_codec: Some("hevc".into()),
                        height: Some(height),
                        hdr: hdr.clone(),
                        bitrate: Some(bitrate),
                        ..Default::default()
                    },
                )
                .await
                .expect("file");
        }
    };
    add("dv4k", Some("dolby_vision"), 2160, 69_000_000).await;
    add("dv4k2", Some("dolby_vision"), 2160, 45_000_000).await;
    add("hdr4k", Some("hdr10"), 2160, 50_000_000).await;
    add("hlg4k", Some("hlg"), 2160, 30_000_000).await;
    add("sdr4k", None, 2160, 20_000_000).await;
    // 1080p HDR10: real, but not the set M2 is about.
    add("hdr1080", Some("hdr10"), 1080, 12_000_000).await;

    // A file that was never probed. It must not land in any flavour.
    let ghost = store
        .insert_item(&NewItem {
            library_id: lib,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "Ghost".into(),
            year: Some(2024),
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    store
        .upsert_file(ghost, "/media/Ghost.mkv", 1, 1, &ProbeResult::default())
        .await
        .expect("file");

    let shape = store.media_shape().await.expect("shape");
    assert_eq!(shape.probed, 6);
    assert_eq!(shape.unprobed, 1, "an unprobed file is its own category");

    let all: std::collections::HashMap<_, _> = shape.hdr.iter().cloned().collect();
    assert_eq!(all.get("dolby_vision"), Some(&2));
    assert_eq!(all.get("hdr10"), Some(&2), "includes the 1080p one");
    assert_eq!(all.get("hlg"), Some(&1));
    assert_eq!(
        all.get("sdr"),
        Some(&1),
        "null hdr reads as sdr, not as a gap"
    );
    assert_eq!(all.values().sum::<i64>(), shape.probed);

    let four_k: std::collections::HashMap<_, _> = shape.hdr_4k.iter().cloned().collect();
    assert_eq!(
        four_k.get("hdr10"),
        Some(&1),
        "the 1080p HDR10 file is excluded"
    );
    assert_eq!(four_k.get("dolby_vision"), Some(&2));
    assert_eq!(four_k.values().sum::<i64>(), 5);

    // The §5 answer itself: 1 of 4 pieces of 4K HDR can use the GPU graph.
    let fast = four_k.get("hdr10").copied().unwrap_or(0) + four_k.get("hlg").copied().unwrap_or(0);
    assert_eq!(fast, 2);
    assert_eq!(four_k.get("dolby_vision").copied().unwrap_or(0), 2);

    // Biggest first, so the shape of a library is its first line.
    assert!(
        shape.hdr.windows(2).all(|w| w[0].1 >= w[1].1),
        "{:?}",
        shape.hdr
    );

    assert_eq!(shape.over_segmented_floor, 3, "69, 50 and 45 Mb/s");
    assert_eq!(shape.max_bitrate, Some(69_000_000));
}

async fn seed_movie(store: &SqliteStore, lib: i64, title: &str, year: i32) -> i64 {
    let id = store
        .insert_item(&NewItem {
            library_id: lib,
            kind: ItemKind::Movie,
            parent_id: None,
            title: title.into(),
            year: Some(year),
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("insert");
    store
        .upsert_file(
            id,
            &format!("/media/{title}.mkv"),
            1000,
            1,
            &ProbeResult {
                container: Some("mkv".into()),
                video_codec: Some("h264".into()),
                audio_streams: vec![AudioStream {
                    index: 0,
                    codec: "aac".into(),
                    channels: Some(2),
                    default: true,
                    ..Default::default()
                }],
                ..Default::default()
            },
        )
        .await
        .expect("file");
    id
}

async fn seed_movie_h(store: &SqliteStore, lib: i64, title: &str, height: i64) -> i64 {
    let id = store
        .insert_item(&NewItem {
            library_id: lib,
            kind: ItemKind::Movie,
            parent_id: None,
            title: title.into(),
            year: Some(2000),
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("insert");
    store
        .upsert_file(
            id,
            &format!("/media/{title}.mkv"),
            1000,
            1,
            &ProbeResult {
                container: Some("mkv".into()),
                video_codec: Some("h264".into()),
                width: Some(height * 16 / 9),
                height: Some(height),
                ..Default::default()
            },
        )
        .await
        .expect("file");
    id
}

#[tokio::test]
async fn resolution_sort_orders_by_height_desc() {
    let store = SqliteStore::open_in_memory().expect("open");
    let lib = store
        .create_library(&NewLibrary {
            name: "Movies".into(),
            kind: LibraryKind::Movies,
            paths: vec![PathBuf::from("/media")],
            anime: false,
        })
        .await
        .expect("lib");
    seed_movie_h(&store, lib.id, "SD", 480).await;
    seed_movie_h(&store, lib.id, "UHD", 2160).await;
    seed_movie_h(&store, lib.id, "HD", 1080).await;
    // A movie whose file has no probed height sorts last (COALESCE -1).
    seed_movie(&store, lib.id, "Unknown", 2001).await;

    let page = store
        .list_top_items(lib.id, ItemSort::Resolution, 0, 10)
        .await
        .expect("list");
    let order: Vec<&str> = page.items.iter().map(|i| i.title.as_str()).collect();
    assert_eq!(order, vec!["UHD", "HD", "SD", "Unknown"]);
}

#[tokio::test]
async fn movie_placement_files_and_browse() {
    let store = SqliteStore::open_in_memory().expect("open");
    let lib = store
        .create_library(&NewLibrary {
            name: "Movies".into(),
            kind: LibraryKind::Movies,
            paths: vec![PathBuf::from("/media")],
            anime: false,
        })
        .await
        .expect("lib");

    assert!(store
        .find_movie(lib.id, "Heat", Some(1995))
        .await
        .expect("find")
        .is_none());
    let id = seed_movie(&store, lib.id, "Heat", 1995).await;
    let found = store
        .find_movie(lib.id, "heat", Some(1995))
        .await
        .expect("find")
        .expect("present");
    assert_eq!(found.id, id);
    // Different year → different item.
    assert!(store
        .find_movie(lib.id, "Heat", Some(2024))
        .await
        .expect("find")
        .is_none());

    seed_movie(&store, lib.id, "The Matrix", 1999).await;
    let page = store
        .list_top_items(lib.id, ItemSort::Title, 0, 10)
        .await
        .expect("list");
    assert_eq!(page.total, 2);
    // "The Matrix" sorts as "matrix" — after "heat".
    assert_eq!(page.items[0].title, "Heat");
    assert_eq!(page.items[1].title, "The Matrix");

    let files = store.files_for_item(id).await.expect("files");
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].audio_streams[0].codec, "aac");

    // Unchanged file is recognized by path.
    let existing = store
        .get_file_by_path("/media/Heat.mkv")
        .await
        .expect("by path")
        .expect("present");
    assert_eq!(existing.size, 1000);
    assert_eq!(existing.mtime, 1);
}

#[tokio::test]
async fn show_hierarchy_and_prune() {
    let store = SqliteStore::open_in_memory().expect("open");
    let lib = store
        .create_library(&NewLibrary {
            name: "TV".into(),
            kind: LibraryKind::Shows,
            paths: vec![PathBuf::from("/tv")],
            anime: false,
        })
        .await
        .expect("lib");

    let show = store
        .insert_item(&NewItem {
            library_id: lib.id,
            kind: ItemKind::Show,
            parent_id: None,
            title: "Severance".into(),
            year: Some(2022),
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("show");
    let season = store
        .insert_item(&NewItem {
            library_id: lib.id,
            kind: ItemKind::Season,
            parent_id: Some(show),
            title: "Season 1".into(),
            year: None,
            season_number: Some(1),
            episode_number: None,
        })
        .await
        .expect("season");
    let ep = store
        .insert_item(&NewItem {
            library_id: lib.id,
            kind: ItemKind::Episode,
            parent_id: Some(season),
            title: "Good News About Hell".into(),
            year: None,
            season_number: Some(1),
            episode_number: Some(1),
        })
        .await
        .expect("episode");
    store
        .upsert_file(
            ep,
            "/tv/severance-s01e01.mkv",
            5,
            5,
            &ProbeResult::default(),
        )
        .await
        .expect("file");

    let eps = store.episodes_for_show(show).await.expect("eps");
    assert_eq!(eps.len(), 1);
    assert_eq!(eps[0].id, ep);

    let found = store
        .find_episode(season, 1)
        .await
        .expect("find")
        .expect("present");
    assert_eq!(found.id, ep);

    // Nothing to prune yet.
    assert_eq!(store.prune_empty_items(lib.id).await.expect("prune"), 0);

    // Remove the file → episode, season, and show all prune away.
    let paths = store.library_file_paths(lib.id).await.expect("paths");
    assert_eq!(paths.len(), 1);
    store
        .delete_files(&[paths[0].0])
        .await
        .expect("delete files");
    assert_eq!(store.prune_empty_items(lib.id).await.expect("prune"), 3);
    assert!(store.get_item(show).await.expect("get").is_none());
}

#[tokio::test]
async fn global_recent_additions_exclude_recordings_before_limiting() {
    let store = SqliteStore::open_in_memory().expect("open");
    let mut movie_id = 0;
    let mut recordings_id = 0;
    for (kind, count) in [(LibraryKind::Movies, 1), (LibraryKind::Recordings, 21)] {
        let lib = store
            .create_library(&NewLibrary {
                name: format!("{kind:?}"),
                kind,
                paths: vec![PathBuf::from("/media")],
                anime: false,
            })
            .await
            .expect("library");
        if kind == LibraryKind::Recordings {
            recordings_id = lib.id;
        }
        for index in 0..count {
            let id = store
                .insert_item(&NewItem {
                    library_id: lib.id,
                    kind: if kind == LibraryKind::Movies {
                        ItemKind::Movie
                    } else {
                        ItemKind::Video
                    },
                    parent_id: None,
                    title: format!("Title {index}"),
                    year: None,
                    season_number: None,
                    episode_number: None,
                })
                .await
                .expect("item");
            if kind == LibraryKind::Movies {
                movie_id = id;
            }
        }
    }
    let home = store.recently_added(None, 20).await.expect("home");
    assert_eq!(
        home.iter().map(|row| row.item.id).collect::<Vec<_>>(),
        vec![movie_id]
    );
    let scoped = store
        .recently_added(Some(recordings_id), 20)
        .await
        .expect("scoped");
    assert_eq!(scoped.len(), 20);
    assert!(scoped
        .iter()
        .all(|row| row.item.library_id == recordings_id));
}

#[tokio::test]
async fn recently_added_uses_the_latest_seasons_poster_when_timestamps_tie() {
    let store = SqliteStore::open_in_memory().expect("open");
    let lib = store
        .create_library(&NewLibrary {
            name: "TV".into(),
            kind: LibraryKind::Shows,
            paths: vec![PathBuf::from("/tv")],
            anime: false,
        })
        .await
        .expect("lib");
    let show = store
        .insert_item(&NewItem {
            library_id: lib.id,
            kind: ItemKind::Show,
            parent_id: None,
            title: "Severance".into(),
            year: Some(2022),
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("show");

    let mut episode_ids = Vec::new();
    for season_number in 1..=2 {
        let season = store
            .insert_item(&NewItem {
                library_id: lib.id,
                kind: ItemKind::Season,
                parent_id: Some(show),
                title: format!("Season {season_number}"),
                year: None,
                season_number: Some(season_number),
                episode_number: None,
            })
            .await
            .expect("season");
        store
            .apply_metadata(
                season,
                &MetadataPatch {
                    poster_path: Some(format!("season-{season_number}.jpg")),
                    ..Default::default()
                },
            )
            .await
            .expect("season poster");
        let episode = store
            .insert_item(&NewItem {
                library_id: lib.id,
                kind: ItemKind::Episode,
                parent_id: Some(season),
                title: format!("Episode {season_number}"),
                year: None,
                season_number: Some(season_number),
                episode_number: Some(1),
            })
            .await
            .expect("episode");
        episode_ids.push(episode);
    }

    // A scan inserts a batch in one second, so added_at cannot decide
    // which season represents the show. The episode/season ordering must
    // break the tie instead of letting SQLite choose an arbitrary row.
    let first = episode_ids[0];
    let second = episode_ids[1];
    store
        .with_conn(move |conn| {
            conn.execute(
                "UPDATE items SET added_at = 100 WHERE id IN (?1, ?2)",
                rusqlite::params![first, second],
            )?;
            Ok(())
        })
        .await
        .expect("tie timestamps");

    let recent = store
        .recently_added(Some(lib.id), 20)
        .await
        .expect("recent");
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].item.id, second);
    assert_eq!(recent[0].season_poster.as_deref(), Some("season-2.jpg"));
}

#[tokio::test]
async fn home_library_items_round_trip() {
    use crate::domain::ItemEdit;

    let store = SqliteStore::open_in_memory().expect("open");
    let lib = store
        .create_library(&NewLibrary {
            name: "Home".into(),
            kind: LibraryKind::Home,
            paths: vec![PathBuf::from("/media/home")],
            anime: false,
        })
        .await
        .expect("lib");
    assert_eq!(lib.kind, LibraryKind::Home);

    let new_item = |kind, parent, title: &str| NewItem {
        library_id: lib.id,
        kind,
        parent_id: parent,
        title: title.to_owned(),
        year: None,
        season_number: None,
        episode_number: None,
    };
    let folder = store
        .insert_item(&new_item(ItemKind::Folder, None, "The Lake House 2021"))
        .await
        .expect("folder");
    let video = store
        .insert_item(&new_item(ItemKind::Video, Some(folder), "Christmas 2019"))
        .await
        .expect("video");
    let photo = store
        .insert_item(&new_item(ItemKind::Photo, Some(folder), "IMG_4021"))
        .await
        .expect("photo");

    // Folder titles keep their leading article — a folder is a place, not
    // a work, so "The Lake House 2021" must not file under L.
    let stored = store.get_item(folder).await.expect("get").expect("present");
    assert_eq!(stored.sort_title, "the lake house 2021");
    assert_eq!(stored.tags, Vec::<String>::new());
    assert_eq!(stored.nfo_seeded_at, None);

    // Folder identity is (library, parent, kind, title).
    let found = store
        .find_child_item(lib.id, None, ItemKind::Folder, "the lake house 2021")
        .await
        .expect("find")
        .expect("present");
    assert_eq!(found.id, folder);
    assert!(store
        .find_child_item(
            lib.id,
            Some(folder),
            ItemKind::Folder,
            "The Lake House 2021"
        )
        .await
        .expect("find")
        .is_none());

    // Seeding from an NFO goes through the ordinary metadata path.
    store
        .apply_metadata(
            video,
            &MetadataPatch {
                title: Some("Beach day".into()),
                recorded_at: Some("2019-06-14".into()),
                tags: Some(vec!["beach".into(), "kids".into()]),
                ..Default::default()
            },
        )
        .await
        .expect("seed");
    store.set_nfo_seeded(video).await.expect("mark seeded");
    let seeded = store.get_item(video).await.expect("get").expect("present");
    assert_eq!(seeded.recorded_at.as_deref(), Some("2019-06-14"));
    assert_eq!(seeded.tags, vec!["beach".to_owned(), "kids".to_owned()]);
    assert!(seeded.nfo_seeded_at.is_some());
    // Tags are searchable (they ride the rebuilt FTS index).
    let hits = store.search_items("kids", 10).await.expect("search");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].item.id, video);

    // An edit can clear a field; a patch cannot.
    let edited = store
        .update_item_fields(
            video,
            &ItemEdit {
                recorded_at: Some(None),
                overview: Some(Some("Windy.".into())),
                tags: Some(Vec::new()),
                ..Default::default()
            },
        )
        .await
        .expect("edit")
        .expect("present");
    assert_eq!(edited.recorded_at, None);
    assert_eq!(edited.overview.as_deref(), Some("Windy."));
    assert!(edited.tags.is_empty());
    assert_eq!(edited.title, "Beach day", "an absent field is untouched");

    // Top level = what sits under a root; children hang off the folder.
    let page = store
        .list_top_items(lib.id, ItemSort::Recorded, 0, 10)
        .await
        .expect("list");
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].id, folder);
    let kids = store.get_item_children(folder).await.expect("children");
    assert_eq!(kids.len(), 2);

    // Recorded sort: dated items first (newest first), undated last.
    store
        .apply_metadata(
            photo,
            &MetadataPatch {
                recorded_at: Some("2019-06-15".into()),
                ..Default::default()
            },
        )
        .await
        .expect("date photo");
    let dated = store
        .list_top_items(lib.id, ItemSort::Recorded, 0, 10)
        .await
        .expect("list");
    assert_eq!(dated.items[0].id, folder);
    assert_eq!(dated.items[0].recorded_at, None);
}

#[tokio::test]
async fn empty_folder_chains_prune_all_the_way_up() {
    let store = SqliteStore::open_in_memory().expect("open");
    let lib = store
        .create_library(&NewLibrary {
            name: "Home".into(),
            kind: LibraryKind::Home,
            paths: vec![PathBuf::from("/media/home")],
            anime: false,
        })
        .await
        .expect("lib");
    let mut parent = None;
    for name in ["2019", "Summer", "Beach Trip"] {
        parent = Some(
            store
                .insert_item(&NewItem {
                    library_id: lib.id,
                    kind: ItemKind::Folder,
                    parent_id: parent,
                    title: name.into(),
                    year: None,
                    season_number: None,
                    episode_number: None,
                })
                .await
                .expect("folder"),
        );
    }
    let video = store
        .insert_item(&NewItem {
            library_id: lib.id,
            kind: ItemKind::Video,
            parent_id: parent,
            title: "clip".into(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("video");
    store
        .upsert_file(video, "/media/home/clip.mp4", 1, 1, &ProbeResult::default())
        .await
        .expect("file");

    assert_eq!(store.prune_empty_items(lib.id).await.expect("prune"), 0);

    // Delete the only file: the video and all three folders above it go.
    let paths = store.library_file_paths(lib.id).await.expect("paths");
    store.delete_files(&[paths[0].0]).await.expect("delete");
    assert_eq!(
        store.prune_empty_items(lib.id).await.expect("prune"),
        4,
        "a nested empty chain must prune in full, not one layer per scan"
    );
}

#[tokio::test]
async fn metadata_patch_and_search() {
    let store = SqliteStore::open_in_memory().expect("open");
    let lib = store
        .create_library(&NewLibrary {
            name: "Movies".into(),
            kind: LibraryKind::Movies,
            paths: vec![],
            anime: false,
        })
        .await
        .expect("lib");
    let id = seed_movie(&store, lib.id, "Blade Runer", 1982).await; // scanner typo

    assert_eq!(
        store
            .items_needing_metadata(None, false, None)
            .await
            .expect("needing")
            .len(),
        1
    );
    // An id on its own does NOT mean "enriched". This is the shape of
    // patch another application's scan request produces: it names what
    // the item is, which is a reason to go and fetch its metadata, not a
    // reason to consider it fetched.
    store
        .apply_metadata(
            id,
            &MetadataPatch {
                tmdb_id: Some(78),
                ..Default::default()
            },
        )
        .await
        .expect("ids only");
    assert_eq!(
        store
            .items_needing_metadata(None, false, None)
            .await
            .expect("needing")
            .len(),
        1,
        "an item that arrived with an id still needs everything else"
    );

    store
        .apply_metadata(
            id,
            &MetadataPatch {
                title: Some("Blade Runner".into()),
                overview: Some("A blade runner must pursue replicants.".into()),
                tmdb_id: Some(78),
                enriched: true,
                ..Default::default()
            },
        )
        .await
        .expect("patch");
    assert!(store
        .items_needing_metadata(None, false, None)
        .await
        .expect("needing")
        .is_empty());

    // And the mark is sticky: a later id-only patch (a Trakt backfill,
    // a re-announced import) must not push the item back into the queue.
    store
        .apply_metadata(
            id,
            &MetadataPatch {
                imdb_id: Some("tt0083658".into()),
                ..Default::default()
            },
        )
        .await
        .expect("later patch");
    assert!(store
        .items_needing_metadata(None, false, None)
        .await
        .expect("needing")
        .is_empty());

    // A forced refresh returns the already-matched item anyway (backfills
    // season posters onto shows enriched before that existed).
    assert_eq!(
        store
            .items_needing_metadata(None, true, None)
            .await
            .expect("forced")
            .len(),
        1
    );

    // FTS picks up the corrected title (trigger-synced), prefix search works.
    let hits = store.search_items("blade run", 10).await.expect("search");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].item.title, "Blade Runner");
    // Overview matches too.
    let hits = store.search_items("replicants", 10).await.expect("search");
    assert_eq!(hits.len(), 1);
    // Garbage-only query is a no-op.
    assert!(store
        .search_items("  !!  ", 10)
        .await
        .expect("s")
        .is_empty());
}

/// The narrowing a targeted scan runs on. `None` must keep the query
/// exactly as it was — the whole-library path is not allowed to change
/// because a second caller wanted a filter.
#[tokio::test]
async fn items_needing_metadata_narrows_to_the_ids_asked_for() {
    let store = SqliteStore::open_in_memory().expect("open");
    let lib = store
        .create_library(&NewLibrary {
            name: "Movies".into(),
            kind: LibraryKind::Movies,
            paths: vec![],
            anime: false,
        })
        .await
        .expect("lib");
    let a = seed_movie(&store, lib.id, "Alien", 1979).await;
    let b = seed_movie(&store, lib.id, "Aliens", 1986).await;

    let all = store
        .items_needing_metadata(None, false, None)
        .await
        .expect("all");
    assert_eq!(all.len(), 2, "no filter is every candidate, as before");

    let one = store
        .items_needing_metadata(None, false, Some(&[a]))
        .await
        .expect("one");
    assert_eq!(one.iter().map(|i| i.id).collect::<Vec<_>>(), [a]);

    let both = store
        .items_needing_metadata(None, false, Some(&[a, b]))
        .await
        .expect("both");
    assert_eq!(both.len(), 2);

    // "These zero items" is not "every item" — the difference between a
    // scan that placed nothing and a scan that placed the library.
    assert!(store
        .items_needing_metadata(None, false, Some(&[]))
        .await
        .expect("none")
        .is_empty());
}

/// The retry sweep's input. The case that matters is the third one: an
/// item attempted moments ago must be left alone, or a permanently
/// art-less film is re-fetched on every cycle forever.
#[tokio::test]
async fn items_missing_artwork_finds_enriched_items_with_no_poster() {
    let store = SqliteStore::open_in_memory().expect("open");
    let lib = store
        .create_library(&NewLibrary {
            name: "Movies".into(),
            kind: LibraryKind::Movies,
            paths: vec![],
            anime: false,
        })
        .await
        .expect("lib");
    let blank = seed_movie(&store, lib.id, "Solaris", 1972).await;
    let posted = seed_movie(&store, lib.id, "Stalker", 1979).await;
    let unenriched = seed_movie(&store, lib.id, "Mirror", 1975).await;

    // Enriched, poster download failed: exactly the state one TMDB blip
    // used to leave behind, silently and permanently.
    store
        .apply_metadata(
            blank,
            &MetadataPatch {
                title: Some("Solaris".into()),
                enriched: true,
                artwork: Some(ArtworkAttempt::Failed("download: status 429".into())),
                ..Default::default()
            },
        )
        .await
        .expect("blank");
    store
        .apply_metadata(
            posted,
            &MetadataPatch {
                title: Some("Stalker".into()),
                poster_path: Some("2-poster.jpg".into()),
                backdrop_path: Some("2-backdrop.jpg".into()),
                enriched: true,
                artwork: Some(ArtworkAttempt::Stored),
                ..Default::default()
            },
        )
        .await
        .expect("posted");

    // No backoff: the one item that was matched and has no picture.
    let due = store
        .items_missing_artwork(None, 0, 100)
        .await
        .expect("missing artwork");
    assert_eq!(
        due.iter().map(|i| i.id).collect::<Vec<_>>(),
        [blank],
        "an item with a poster is done, and an item nobody has enriched \
             yet belongs to the scan, not to the sweep"
    );
    assert_eq!(
        due[0].artwork_error.as_deref(),
        Some("download: status 429"),
        "the reason survives, so an operator can tell a rate limit from a 404"
    );
    assert!(due[0].artwork_attempted_at.is_some());
    assert!(store.get_item(unenriched).await.expect("get").is_some());
    assert!(store
        .items_missing_artwork(None, 0, 0)
        .await
        .expect("zero limit")
        .is_empty());
    assert!(store
        .items_missing_artwork(None, 0, -1)
        .await
        .expect("negative limit")
        .is_empty());

    // With a backoff, the same item is skipped — it was attempted a
    // moment ago, and the point of recording that was to be able to wait.
    assert!(store
        .items_missing_artwork(None, 3600, 100)
        .await
        .expect("backoff")
        .is_empty());
}

/// TV children inherit their provider eligibility from the show. Seasons
/// and episodes never receive `metadata_at` themselves, so requiring that
/// field on the candidate row strands exactly the blank cards the sweep is
/// meant to repair.
#[tokio::test]
async fn items_missing_artwork_includes_children_of_an_enriched_show() {
    let store = SqliteStore::open_in_memory().expect("open");
    let lib = store
        .create_library(&NewLibrary {
            name: "TV".into(),
            kind: LibraryKind::Shows,
            paths: vec![],
            anime: false,
        })
        .await
        .expect("lib");
    let show = store
        .insert_item(&NewItem {
            library_id: lib.id,
            kind: ItemKind::Show,
            parent_id: None,
            title: "Severance".into(),
            year: Some(2022),
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("show");
    let season = store
        .insert_item(&NewItem {
            library_id: lib.id,
            kind: ItemKind::Season,
            parent_id: Some(show),
            title: "Season 1".into(),
            year: None,
            season_number: Some(1),
            episode_number: None,
        })
        .await
        .expect("season");
    let episode = store
        .insert_item(&NewItem {
            library_id: lib.id,
            kind: ItemKind::Episode,
            parent_id: Some(season),
            title: "Good News About Hell".into(),
            year: None,
            season_number: Some(1),
            episode_number: Some(1),
        })
        .await
        .expect("episode");
    store
        .apply_metadata(
            show,
            &MetadataPatch {
                tmdb_id: Some(42),
                poster_path: Some("show.jpg".into()),
                backdrop_path: Some("show-backdrop.jpg".into()),
                enriched: true,
                artwork: Some(ArtworkAttempt::Stored),
                ..Default::default()
            },
        )
        .await
        .expect("enrich show");

    let first = store
        .items_missing_artwork(Some(lib.id), 0, 1)
        .await
        .expect("bounded artwork");
    assert_eq!(
        first.iter().map(|item| item.id).collect::<Vec<_>>(),
        [season],
        "one pass honors its row budget and keeps deterministic ordering"
    );

    let due = store
        .items_missing_artwork(Some(lib.id), 0, 100)
        .await
        .expect("missing artwork");
    assert_eq!(
        due.iter().map(|item| item.id).collect::<Vec<_>>(),
        [season, episode],
        "the healthy show is done, but both blank child cards need retry"
    );
}

/// Child eligibility is deliberately narrower than "posterless TV row":
/// only numbered children of an enriched, non-anime provider show can be
/// repaired by the TMDB route.
#[tokio::test]
async fn items_missing_artwork_excludes_children_the_provider_cannot_repair() {
    let store = SqliteStore::open_in_memory().expect("open");

    async fn create_tree(
        store: &SqliteStore,
        name: &str,
        kind: LibraryKind,
        anime: bool,
        enriched: bool,
    ) -> (i64, i64, i64, i64) {
        let lib = store
            .create_library(&NewLibrary {
                name: name.into(),
                kind,
                paths: vec![],
                anime,
            })
            .await
            .expect("lib");
        let show = store
            .insert_item(&NewItem {
                library_id: lib.id,
                kind: ItemKind::Show,
                parent_id: None,
                title: format!("{name} Show"),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("show");
        let season = store
            .insert_item(&NewItem {
                library_id: lib.id,
                kind: ItemKind::Season,
                parent_id: Some(show),
                title: "Season 1".into(),
                year: None,
                season_number: Some(1),
                episode_number: None,
            })
            .await
            .expect("season");
        let episode = store
            .insert_item(&NewItem {
                library_id: lib.id,
                kind: ItemKind::Episode,
                parent_id: Some(season),
                title: "Episode 1".into(),
                year: None,
                season_number: Some(1),
                episode_number: Some(1),
            })
            .await
            .expect("episode");
        if enriched {
            store
                .apply_metadata(
                    show,
                    &MetadataPatch {
                        tmdb_id: Some(42),
                        poster_path: Some(format!("{show}-poster.jpg")),
                        backdrop_path: Some(format!("{show}-backdrop.jpg")),
                        enriched: true,
                        artwork: Some(ArtworkAttempt::Stored),
                        ..Default::default()
                    },
                )
                .await
                .expect("enrich show");
        }
        (lib.id, show, season, episode)
    }

    let (_, _, _, anime_episode) =
        create_tree(&store, "Anime", LibraryKind::Shows, true, true).await;
    let (_, _, _, unenriched_episode) =
        create_tree(&store, "Unenriched", LibraryKind::Shows, false, false).await;
    let (_, _, _, home_episode) = create_tree(&store, "Home", LibraryKind::Home, false, true).await;

    let normal = store
        .create_library(&NewLibrary {
            name: "Malformed TV".into(),
            kind: LibraryKind::Shows,
            paths: vec![],
            anime: false,
        })
        .await
        .expect("lib");
    let show = store
        .insert_item(&NewItem {
            library_id: normal.id,
            kind: ItemKind::Show,
            parent_id: None,
            title: "Malformed".into(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("show");
    let unnumbered_season = store
        .insert_item(&NewItem {
            library_id: normal.id,
            kind: ItemKind::Season,
            parent_id: Some(show),
            title: "Unknown Season".into(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("season");
    let unnumbered_episode = store
        .insert_item(&NewItem {
            library_id: normal.id,
            kind: ItemKind::Episode,
            parent_id: Some(unnumbered_season),
            title: "Unknown Episode".into(),
            year: None,
            season_number: None,
            episode_number: Some(1),
        })
        .await
        .expect("episode");
    store
        .apply_metadata(
            show,
            &MetadataPatch {
                tmdb_id: Some(43),
                poster_path: Some("malformed-show.jpg".into()),
                backdrop_path: Some("malformed-backdrop.jpg".into()),
                enriched: true,
                artwork: Some(ArtworkAttempt::Stored),
                ..Default::default()
            },
        )
        .await
        .expect("enrich show");

    let due = store
        .items_missing_artwork(None, 0, 100)
        .await
        .expect("missing artwork");
    let ids: Vec<i64> = due.into_iter().map(|item| item.id).collect();
    assert!(!ids.contains(&anime_episode));
    assert!(!ids.contains(&unenriched_episode));
    assert!(!ids.contains(&home_episode));
    assert!(!ids.contains(&unnumbered_season));
    assert!(!ids.contains(&unnumbered_episode));
}

/// Movies and shows own both a card poster and a hero backdrop. A healthy
/// card must not hide a failed hero download from the retry queue.
#[tokio::test]
async fn items_missing_artwork_includes_a_show_missing_only_its_backdrop() {
    let store = SqliteStore::open_in_memory().expect("open");
    let lib = store
        .create_library(&NewLibrary {
            name: "TV".into(),
            kind: LibraryKind::Shows,
            paths: vec![],
            anime: false,
        })
        .await
        .expect("lib");
    let show = store
        .insert_item(&NewItem {
            library_id: lib.id,
            kind: ItemKind::Show,
            parent_id: None,
            title: "Severance".into(),
            year: Some(2022),
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("show");
    store
        .apply_metadata(
            show,
            &MetadataPatch {
                tmdb_id: Some(42),
                poster_path: Some("show-poster.jpg".into()),
                enriched: true,
                artwork: Some(ArtworkAttempt::Stored),
                ..Default::default()
            },
        )
        .await
        .expect("enrich show");

    let due = store
        .items_missing_artwork(None, 0, 100)
        .await
        .expect("missing backdrop");
    assert_eq!(due.iter().map(|item| item.id).collect::<Vec<_>>(), [show]);
}

/// A patch that is not about artwork must not disturb the attempt record.
/// A Trakt backfill landing between sweeps would otherwise reset the
/// backoff and turn the daily retry into a per-write retry.
#[tokio::test]
async fn an_unrelated_patch_leaves_the_artwork_attempt_alone() {
    let store = SqliteStore::open_in_memory().expect("open");
    let lib = store
        .create_library(&NewLibrary {
            name: "Movies".into(),
            kind: LibraryKind::Movies,
            paths: vec![],
            anime: false,
        })
        .await
        .expect("lib");
    let id = seed_movie(&store, lib.id, "Stalker", 1979).await;
    store
        .apply_metadata(
            id,
            &MetadataPatch {
                enriched: true,
                artwork: Some(ArtworkAttempt::Failed("download: status 500".into())),
                ..Default::default()
            },
        )
        .await
        .expect("attempt");
    let before = store.get_item(id).await.expect("get").expect("item");

    store
        .apply_metadata(
            id,
            &MetadataPatch {
                imdb_id: Some("tt0079944".into()),
                ..Default::default()
            },
        )
        .await
        .expect("unrelated");
    let after = store.get_item(id).await.expect("get").expect("item");
    assert_eq!(after.artwork_attempted_at, before.artwork_attempted_at);
    assert_eq!(after.artwork_error.as_deref(), Some("download: status 500"));
}

/// `EXPLAIN QUERY PLAN` detail lines for `sql` bound to `params`.
async fn plan_of(
    store: &SqliteStore,
    sql: String,
    params: Vec<rusqlite::types::Value>,
) -> Vec<String> {
    store
        .with_conn(move |conn| {
            let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
            let details = stmt
                .query_map(rusqlite::params_from_iter(params), |row| {
                    row.get::<_, String>(3)
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(details)
        })
        .await
        .expect("plan")
}

/// K-05 M5: the external-id lookup is one partial-index search per arm
/// and reads its rows by id. Before M5 it scanned every item once per
/// coming-soon entry (`SCAN items USING INDEX idx_items_library_kind`,
/// 3.3 ms on the 75,600-item fixture).
#[tokio::test]
async fn item_by_external_id_searches_one_partial_index_per_arm() {
    use rusqlite::types::Value;
    let store = SqliteStore::open_in_memory().expect("open");
    let plan = plan_of(
        &store,
        crate::store::sql_source::item_by_external_id(super::ITEM_COLS).sqlite(),
        vec![
            Value::Text("movie".into()),
            Value::Integer(42),
            Value::Text("tt0000042".into()),
        ],
    )
    .await;
    assert!(
        plan.iter()
            .any(|line| line == "SEARCH items USING COVERING INDEX idx_items_tmdb (tmdb_id=?)"),
        "{plan:?}"
    );
    assert!(
        plan.iter()
            .any(|line| line == "SEARCH items USING COVERING INDEX idx_items_imdb (imdb_id=?)"),
        "{plan:?}"
    );
    assert!(
        plan.iter()
            .any(|line| line == "SEARCH items USING INTEGER PRIMARY KEY (rowid=?)"),
        "{plan:?}"
    );
    assert!(
        !plan.iter().any(|line| line.starts_with("SCAN items")),
        "{plan:?}"
    );
}

/// K-05 M5: the union spelling returns exactly the row the `OR`
/// statement it replaced returns, for every kind and id combination,
/// including a null TMDB id on an IMDb match (which the `OR` statement's
/// `DESC` key ranks after a non-matching TMDB id), case-folded IMDb ids,
/// a row that matches both arms, and ids shared across kinds.
#[tokio::test]
async fn item_by_external_id_matches_the_or_statement() {
    use rusqlite::OptionalExtension;
    const LEGACY: &str = "WHERE kind = ?1
                   AND ((?2 IS NOT NULL AND tmdb_id = ?2)
                     OR (?3 IS NOT NULL AND imdb_id = ?3 COLLATE NOCASE))
                 ORDER BY (?2 IS NOT NULL AND tmdb_id = ?2) DESC, id
                 LIMIT 1";
    const IMDB: [&str; 4] = ["tt01", "TT01", "tt02", "tt03"];
    let store = SqliteStore::open_in_memory().expect("open");
    store
        .with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO libraries(id,name,kind,paths) VALUES (1,'M','movies','[]');",
            )?;
            Ok(())
        })
        .await
        .expect("library");
    for seed in 0..200_u64 {
        let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut next = move |bound: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            i64::try_from(state % bound).expect("small")
        };
        let mut rows = Vec::new();
        for id in 1..=(4 + next(20)) {
            let kind = ["movie", "show", "episode"][usize::try_from(next(3)).expect("index")];
            let tmdb = (next(3) > 0).then(|| next(4));
            let imdb = (next(3) > 0).then(|| IMDB[usize::try_from(next(4)).expect("index")]);
            rows.push((id, kind, tmdb, imdb));
        }
        let lookups = store
            .with_conn(move |conn| {
                conn.execute_batch("DELETE FROM items;")?;
                let mut insert = conn.prepare(
                    "INSERT INTO items(id,library_id,kind,title,sort_title,tmdb_id,imdb_id,
                                           added_at,tags,genres)
                         VALUES(?1,1,?2,'t'||?1,'t'||?1,?3,?4,0,'[]','[]')",
                )?;
                for (id, kind, tmdb, imdb) in &rows {
                    insert.execute(rusqlite::params![id, kind, tmdb, imdb])?;
                }
                let current = crate::store::sql_source::item_by_external_id("id").sqlite();
                let legacy = format!("SELECT id FROM items {LEGACY}");
                let mut compared = 0;
                for kind in ["movie", "show", "episode"] {
                    for tmdb in [None, Some(0), Some(1), Some(2), Some(3), Some(9)] {
                        for imdb in [None, Some("tt01"), Some("Tt01"), Some("tt02"), Some("tt09")] {
                            let run = |sql: &str| -> rusqlite::Result<Option<i64>> {
                                conn.query_row(sql, rusqlite::params![kind, tmdb, imdb], |row| {
                                    row.get(0)
                                })
                                .optional()
                            };
                            assert_eq!(
                                run(&current)?,
                                run(&legacy)?,
                                "seed {seed}, kind {kind}, tmdb {tmdb:?}, imdb {imdb:?}"
                            );
                            compared += 1;
                        }
                    }
                }
                Ok(compared)
            })
            .await
            .expect("compare");
        assert_eq!(lookups, 90);
    }
}

/// K-05 M5: every library page's count, and the Title page itself, read
/// the library's top-level items from the partial
/// `idx_items_top_level_title`; the Title page reads them in order
/// instead of sorting the library in a temp B-tree. Before M5 each
/// searched `idx_items_library_kind` and visited every season and
/// episode of a show library to count its shows.
#[tokio::test]
async fn library_pages_read_top_level_items_from_the_title_index() {
    use rusqlite::types::Value;
    let store = SqliteStore::open_in_memory().expect("open");
    for sort in [
        ItemSort::Title,
        ItemSort::Added,
        ItemSort::Year,
        ItemSort::Resolution,
        ItemSort::Recorded,
    ] {
        let (count, page) = super::library_page_statements(crate::store::item_sort_order_by(sort));
        for genre in [Value::Null, Value::Text("Drama".into())] {
            let count_plan = plan_of(
                &store,
                count.clone(),
                vec![Value::Integer(1), genre.clone()],
            )
            .await;
            assert_eq!(
                count_plan.first().map(String::as_str),
                Some("SEARCH items USING INDEX idx_items_top_level_title (library_id=?)"),
                "{sort:?} count: {count_plan:?}"
            );
            let page_plan = plan_of(
                &store,
                page.clone(),
                vec![
                    Value::Integer(1),
                    Value::Integer(0),
                    Value::Integer(50),
                    genre.clone(),
                ],
            )
            .await;
            assert_eq!(
                page_plan.first().map(String::as_str),
                Some("SEARCH items USING INDEX idx_items_top_level_title (library_id=?)"),
                "{sort:?} page: {page_plan:?}"
            );
            if sort == ItemSort::Title {
                assert!(
                    !page_plan.iter().any(|line| line.contains("TEMP B-TREE")),
                    "the Title page reads the index in order: {page_plan:?}"
                );
            }
        }
    }
}
