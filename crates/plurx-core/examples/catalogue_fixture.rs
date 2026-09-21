//! Build the representative catalogue used by the SQLite query-plan protocol.
//!
//! The tracked `scripts/bench` path is already the playback benchmark program,
//! so the K-05 catalogue fixture is exposed as a Cargo example instead of
//! turning that file into a directory.

use std::env;
use std::path::PathBuf;

use plurx_core::store::SqliteStore;
use rusqlite::{params, Connection};

const MOVIES: i64 = 20_000;
const SHOWS: i64 = 500;
const SEASONS_PER_SHOW: i64 = 8;
const EPISODES_PER_SEASON: i64 = 12;
const HOME_FOLDERS: i64 = 100;
const HOME_VIDEOS: i64 = 1_000;
const HOME_PHOTOS: i64 = 1_000;
const RECORDINGS: i64 = 1_000;
const FILES: i64 = 100_000;
const USERS: i64 = 5;
const WATCH_PERCENT: i64 = 10;

fn fixture_count(conn: &Connection, library_kind: &str, item_kind: &str) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM items i
         JOIN libraries l ON l.id = i.library_id
         WHERE l.kind = ?1 AND i.kind = ?2",
        params![library_kind, item_kind],
        |row| row.get(0),
    )
}

fn require_count(
    label: &str,
    actual: i64,
    expected: i64,
) -> Result<(), Box<dyn std::error::Error>> {
    if actual != expected {
        return Err(format!("fixture census {label}: expected {expected}, got {actual}").into());
    }
    Ok(())
}

fn assert_fixture_census(conn: &Connection) -> Result<(), Box<dyn std::error::Error>> {
    let expected = [
        ("movies/movie", "movies", "movie", MOVIES),
        ("shows/show", "shows", "show", SHOWS),
        ("shows/season", "shows", "season", SHOWS * SEASONS_PER_SHOW),
        (
            "shows/episode",
            "shows",
            "episode",
            SHOWS * SEASONS_PER_SHOW * EPISODES_PER_SEASON,
        ),
        ("home/folder", "home", "folder", HOME_FOLDERS),
        ("home/video", "home", "video", HOME_VIDEOS),
        ("home/photo", "home", "photo", HOME_PHOTOS),
        ("recordings/video", "recordings", "video", RECORDINGS),
    ];
    for (label, library_kind, item_kind, count) in expected {
        require_count(label, fixture_count(conn, library_kind, item_kind)?, count)?;
    }

    let preview_rails = conn.query_row(
        "SELECT COUNT(*) FROM (
             SELECT library_id FROM items
             WHERE kind IN ('movie','show','book','audiobook')
                OR (kind IN ('folder','video','photo') AND parent_id IS NULL)
             GROUP BY library_id
         )",
        [],
        |row| row.get(0),
    )?;
    require_count("home preview rails", preview_rails, 4)?;

    let invalid_file_owners = conn.query_row(
        "SELECT COUNT(*) FROM files f
         JOIN items i ON i.id = f.item_id
         JOIN libraries l ON l.id = i.library_id
         WHERE NOT (
             (l.kind = 'movies' AND i.kind = 'movie') OR
             (l.kind = 'shows' AND i.kind = 'episode') OR
             (l.kind = 'home' AND i.kind IN ('video','photo')) OR
             (l.kind = 'recordings' AND i.kind = 'video')
         )",
        [],
        |row| row.get(0),
    )?;
    require_count("files on invalid item kinds", invalid_file_owners, 0)?;

    let invalid_watch_owners = conn.query_row(
        "SELECT COUNT(*) FROM watch_state w
         JOIN items i ON i.id = w.item_id
         WHERE i.kind NOT IN ('movie','episode','video','audiobook')",
        [],
        |row| row.get(0),
    )?;
    require_count("watch rows on invalid item kinds", invalid_watch_owners, 0)?;

    let recordings_recently_added_candidates = conn.query_row(
        "SELECT COUNT(*) FROM items i
         JOIN libraries l ON l.id = i.library_id
         WHERE l.kind = 'recordings'
           AND i.kind IN ('movie','episode','video','folder','book','audiobook')",
        [],
        |row| row.get(0),
    )?;
    require_count(
        "recordings excluded from catalog-wide recently-added",
        recordings_recently_added_candidates,
        RECORDINGS,
    )?;
    let catalog_recent_candidates = conn.query_row(
        "SELECT COUNT(*) FROM items i
         WHERE i.kind IN ('movie','episode','video','folder','book','audiobook')
           AND NOT EXISTS (
               SELECT 1 FROM libraries l
               WHERE l.id = i.library_id AND l.kind = 'recordings'
           )",
        [],
        |row| row.get(0),
    )?;
    let explicit_non_recording_candidates = conn.query_row(
        "SELECT COUNT(*) FROM items i
         JOIN libraries l ON l.id = i.library_id
         WHERE i.kind IN ('movie','episode','video','folder','book','audiobook')
           AND l.kind != 'recordings'",
        [],
        |row| row.get(0),
    )?;
    require_count(
        "catalog-wide recently-added candidate set",
        catalog_recent_candidates,
        explicit_non_recording_candidates,
    )?;
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("usage: catalogue_fixture <new-output.db>")?;
    if path.exists() {
        return Err(format!("refusing to replace existing {}", path.display()).into());
    }

    // Let production own the schema. The generator deliberately knows only
    // the columns it populates, so a migration with defaults remains usable
    // while a newly-required field fails here instead of producing a stale
    // benchmark database.
    drop(SqliteStore::open(&path)?);
    let mut conn = Connection::open(&path)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    let tx = conn.transaction()?;

    for id in 1..=4 {
        let (name, kind) = match id {
            1 => ("Movies", "movies"),
            2 => ("Shows", "shows"),
            3 => ("Home", "home"),
            _ => ("Recordings", "recordings"),
        };
        tx.execute(
            "INSERT INTO libraries(id,name,kind,paths) VALUES(?1,?2,?3,'[]')",
            params![id, name, kind],
        )?;
    }

    let mut item_id = 1_i64;
    let mut file_owners = Vec::with_capacity(70_000);
    let mut photo_owners = Vec::with_capacity(HOME_PHOTOS as usize);
    let mut watch_owners = Vec::with_capacity(70_000);
    {
        let mut insert = tx.prepare(
            "INSERT INTO items(
                 id,library_id,kind,parent_id,title,sort_title,year,overview,
                 tmdb_id,season_number,episode_number,runtime_ms,added_at,
                 updated_at,recorded_at,tags,genres
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?13,?14,'[]',?15)",
        )?;
        for movie in 0..MOVIES {
            let title = format!("Movie {movie:05}");
            insert.execute(params![
                item_id,
                1,
                "movie",
                Option::<i64>::None,
                &title,
                &title,
                1980 + movie % 47,
                "Representative movie overview for query planning",
                1_000_000 + movie,
                Option::<i64>::None,
                Option::<i64>::None,
                7_200_000,
                1_700_000_000 + movie % 10_000,
                Option::<i64>::None,
                if movie % 4 == 0 {
                    "[\"Drama\"]"
                } else {
                    "[\"Science Fiction\"]"
                },
            ])?;
            file_owners.push((item_id, 7_200_000));
            watch_owners.push((item_id, 7_200_000));
            item_id += 1;
        }

        for show in 0..SHOWS {
            let show_id = item_id;
            let title = format!("Show {show:03}");
            insert.execute(params![
                show_id,
                2,
                "show",
                Option::<i64>::None,
                &title,
                &title,
                2000 + show % 25,
                "Representative show overview for query planning",
                2_000_000 + show,
                Option::<i64>::None,
                Option::<i64>::None,
                Option::<i64>::None,
                1_700_010_000 + show,
                Option::<i64>::None,
                "[\"Drama\"]",
            ])?;
            item_id += 1;
            for season in 1..=SEASONS_PER_SHOW {
                let season_id = item_id;
                let season_title = format!("{title} Season {season}");
                insert.execute(params![
                    season_id,
                    2,
                    "season",
                    show_id,
                    &season_title,
                    &season_title,
                    Option::<i64>::None,
                    "",
                    Option::<i64>::None,
                    season,
                    Option::<i64>::None,
                    Option::<i64>::None,
                    1_700_020_000 + show * 100 + season,
                    Option::<i64>::None,
                    "[]",
                ])?;
                item_id += 1;
                for episode in 1..=EPISODES_PER_SEASON {
                    let episode_title = format!("{title} S{season:02}E{episode:02}");
                    insert.execute(params![
                        item_id,
                        2,
                        "episode",
                        season_id,
                        &episode_title,
                        &episode_title,
                        Option::<i64>::None,
                        "Representative episode overview for query planning",
                        Option::<i64>::None,
                        season,
                        episode,
                        2_700_000,
                        1_700_030_000 + show * 1_000 + season * 20 + episode,
                        Option::<i64>::None,
                        "[\"Drama\"]",
                    ])?;
                    file_owners.push((item_id, 2_700_000));
                    watch_owners.push((item_id, 2_700_000));
                    item_id += 1;
                }
            }
        }

        let first_home_folder = item_id;
        for folder in 0..HOME_FOLDERS {
            let title = format!("Home Folder {folder:03}");
            insert.execute(params![
                item_id,
                3,
                "folder",
                Option::<i64>::None,
                &title,
                &title,
                Option::<i64>::None,
                "Representative home-video folder",
                Option::<i64>::None,
                Option::<i64>::None,
                Option::<i64>::None,
                Option::<i64>::None,
                1_700_040_000 + folder,
                Option::<i64>::None,
                "[]",
            ])?;
            item_id += 1;
        }
        for video in 0..HOME_VIDEOS {
            let title = format!("Home Video {video:04}");
            let parent_id = (video % 2 == 1).then_some(first_home_folder + video % HOME_FOLDERS);
            insert.execute(params![
                item_id,
                3,
                "video",
                parent_id,
                &title,
                &title,
                Option::<i64>::None,
                "Representative home video",
                Option::<i64>::None,
                Option::<i64>::None,
                Option::<i64>::None,
                600_000,
                1_700_050_000 + video,
                Option::<i64>::None,
                "[]",
            ])?;
            file_owners.push((item_id, 600_000));
            watch_owners.push((item_id, 600_000));
            item_id += 1;
        }
        for photo in 0..HOME_PHOTOS {
            let title = format!("Home Photo {photo:04}");
            let parent_id = (photo % 2 == 1).then_some(first_home_folder + photo % HOME_FOLDERS);
            insert.execute(params![
                item_id,
                3,
                "photo",
                parent_id,
                &title,
                &title,
                Option::<i64>::None,
                "Representative home photo",
                Option::<i64>::None,
                Option::<i64>::None,
                Option::<i64>::None,
                Option::<i64>::None,
                1_700_060_000 + photo,
                Option::<i64>::None,
                "[]",
            ])?;
            photo_owners.push(item_id);
            item_id += 1;
        }
        for recording in 0..RECORDINGS {
            let title = format!("Recording {recording:04}");
            let recorded_at = 1_700_070_000 + recording;
            insert.execute(params![
                item_id,
                4,
                "video",
                Option::<i64>::None,
                &title,
                &title,
                Option::<i64>::None,
                "Representative DVR recording",
                Option::<i64>::None,
                Option::<i64>::None,
                Option::<i64>::None,
                3_600_000,
                recorded_at,
                recorded_at,
                "[]",
            ])?;
            file_owners.push((item_id, 3_600_000));
            watch_owners.push((item_id, 3_600_000));
            item_id += 1;
        }
    }
    let item_count = item_id - 1;

    for user in 1..=USERS {
        tx.execute(
            "INSERT INTO users(id,username,password_hash,is_admin) VALUES(?1,?2,'fixture',?3)",
            params![user, format!("fixture-user-{user}"), i64::from(user == 1)],
        )?;
        tx.execute(
            "INSERT INTO tokens(token_hash,user_id,device,last_seen_at) VALUES(?1,?2,'fixture',0)",
            params![format!("fixture-token-{user}"), user],
        )?;
    }

    // Deterministic sizes span the plan's 5–30 KiB range. Pre-building the 26
    // payload sizes keeps generation CPU bounded while still putting every
    // probe on overflow pages, which is the row-layout issue M0 must measure.
    let video_probe_json = (5..=30)
        .map(|kib| {
            let padding = "x".repeat(kib * 1024);
            format!(
                "{{\"format\":{{\"format_name\":\"matroska\"}},\"streams\":[{{\"codec_type\":\"video\",\"codec_name\":\"h264\",\"width\":1920,\"height\":1080}}],\"padding\":\"{padding}\"}}"
            )
        })
        .collect::<Vec<_>>();
    let photo_probe_json = (5..=30)
        .map(|kib| {
            let padding = "x".repeat(kib * 1024);
            format!(
                "{{\"format\":{{\"format_name\":\"image2\"}},\"streams\":[{{\"codec_type\":\"video\",\"codec_name\":\"mjpeg\",\"width\":4032,\"height\":3024}}],\"padding\":\"{padding}\"}}"
            )
        })
        .collect::<Vec<_>>();
    {
        let mut insert = tx.prepare(
            "INSERT INTO files(
                 id,item_id,path,size,mtime,duration_ms,container,video_codec,
                 width,height,bit_depth,bitrate,audio_streams,subtitle_streams,
                 probe_json,scanned_at
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,8,?11,'[]','[]',?12,?5)",
        )?;
        for file in 1..=FILES {
            let photo_index = file as usize - 1;
            let is_photo = photo_index < photo_owners.len();
            let (owner, duration_ms) = if is_photo {
                (photo_owners[photo_index], None)
            } else {
                let owner = file_owners[(photo_index - photo_owners.len()) % file_owners.len()];
                (owner.0, Some(owner.1))
            };
            let probe_index = (file as usize) % video_probe_json.len();
            insert.execute(params![
                file,
                owner,
                if is_photo {
                    format!("/fixture/home/{file:06}.jpg")
                } else {
                    format!("/fixture/media/{file:06}.mkv")
                },
                if is_photo {
                    5_000_000_i64 + file
                } else {
                    8_000_000_000_i64 + file
                },
                1_700_000_000 + file % 100_000,
                duration_ms,
                if is_photo { "image2" } else { "mkv" },
                if is_photo { "mjpeg" } else { "h264" },
                if is_photo { 4_032 } else { 1_920 },
                if is_photo { 3_024 } else { 1_080 },
                if is_photo {
                    Option::<i64>::None
                } else {
                    Some(8_000_000_i64)
                },
                if is_photo {
                    &photo_probe_json[probe_index]
                } else {
                    &video_probe_json[probe_index]
                },
            ])?;
        }
    }

    let mut watch_rows = 0_i64;
    {
        let mut insert = tx.prepare(
            "INSERT INTO watch_state(
                 user_id,item_id,position_ms,duration_ms,watched,updated_at
             ) VALUES(?1,?2,?3,?4,?5,?6)",
        )?;
        for user in 1..=USERS {
            for (ordinal, (item, duration_ms)) in watch_owners.iter().copied().enumerate() {
                if ordinal as i64 % (100 / WATCH_PERCENT) == user - 1 {
                    let watched = i64::from(item % 3 == 0);
                    insert.execute(params![
                        user,
                        item,
                        if watched == 1 {
                            duration_ms
                        } else {
                            duration_ms / 4
                        },
                        duration_ms,
                        watched,
                        1_700_000_000 + item,
                    ])?;
                    watch_rows += 1;
                }
            }
        }
    }

    // Exercise both search branches without making the generated index a
    // toy: one eighth of top-level media has current classifications.
    tx.execute_batch(
        "INSERT INTO media_classifications(item_id,source_json,payload,terms)
         SELECT i.id,
                json_object('id',i.id,'kind',i.kind,'title',i.title,
                            'overview',COALESCE(i.overview,''),'year',i.year,
                            'tmdb_id',i.tmdb_id,'genres',json(i.genres),
                            'tags',json(i.tags)),
                '{}',
                json_array('fixture','classified')
           FROM items i
          WHERE i.kind IN ('movie','show') AND i.id % 8 = 0;",
    )?;
    assert_fixture_census(&tx)?;
    tx.commit()?;

    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")?;
    println!(
        "built {}: {item_count} items, {FILES} files, {USERS} users, {watch_rows} watch rows",
        path.display(),
    );
    Ok(())
}
