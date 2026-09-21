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
const FILES: i64 = 100_000;
const USERS: i64 = 5;
const WATCH_PERCENT: i64 = 10;

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
    {
        let mut insert = tx.prepare(
            "INSERT INTO items(
                 id,library_id,kind,parent_id,title,sort_title,year,overview,
                 tmdb_id,season_number,episode_number,runtime_ms,added_at,
                 updated_at,tags,genres
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?13,'[]',?14)",
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
                if movie % 4 == 0 {
                    "[\"Drama\"]"
                } else {
                    "[\"Science Fiction\"]"
                },
            ])?;
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
                        "[\"Drama\"]",
                    ])?;
                    item_id += 1;
                }
            }
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
    let probe_json = (5..=30)
        .map(|kib| {
            let padding = "x".repeat(kib * 1024);
            format!(
                "{{\"format\":{{\"format_name\":\"matroska\"}},\"streams\":[{{\"codec_type\":\"video\",\"codec_name\":\"h264\",\"width\":1920,\"height\":1080}}],\"padding\":\"{padding}\"}}"
            )
        })
        .collect::<Vec<_>>();
    {
        let mut insert = tx.prepare(
            "INSERT INTO files(
                 id,item_id,path,size,mtime,duration_ms,container,video_codec,
                 width,height,bit_depth,bitrate,audio_streams,subtitle_streams,
                 probe_json,scanned_at
             ) VALUES(?1,?2,?3,?4,?5,?6,'mkv','h264',1920,1080,8,8000000,'[]','[]',?7,?5)",
        )?;
        for file in 1..=FILES {
            let owner = ((file - 1) % item_count) + 1;
            insert.execute(params![
                file,
                owner,
                format!("/fixture/library/{file:06}.mkv"),
                8_000_000_000_i64 + file,
                1_700_000_000 + file % 100_000,
                7_200_000,
                &probe_json[(file as usize) % probe_json.len()],
            ])?;
        }
    }

    {
        let mut insert = tx.prepare(
            "INSERT INTO watch_state(
                 user_id,item_id,position_ms,duration_ms,watched,updated_at
             ) VALUES(?1,?2,?3,7200000,?4,?5)",
        )?;
        for user in 1..=USERS {
            for item in 1..=item_count {
                if item % (100 / WATCH_PERCENT) == user - 1 {
                    let watched = i64::from(item % 3 == 0);
                    insert.execute(params![
                        user,
                        item,
                        if watched == 1 { 7_200_000 } else { 1_800_000 },
                        watched,
                        1_700_000_000 + item,
                    ])?;
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
    tx.commit()?;

    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")?;
    println!(
        "built {}: {item_count} items, {FILES} files, {USERS} users, {} watch rows",
        path.display(),
        USERS * (item_count / (100 / WATCH_PERCENT))
    );
    Ok(())
}
