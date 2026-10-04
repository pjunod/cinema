// Diagnostic replay for the October 2 catalog RCA. Append to candidates.rs
// in a source-only archive of the incident revision; do not ship as a fix.
#[cfg(test)]
mod incident_replay {
    use super::*;
    #[tokio::test]
    async fn incident_catalog_replay() {
        use plurx_core::{
            domain::{ItemKind, LibraryKind, NewItem, NewLibrary},
            store::SqliteStore,
        };
        let j: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(
                std::env::var("PLURX_RCA_FIXTURE").expect("sanitized fixture path"),
            )
            .unwrap(),
        )
        .unwrap();
        let mut p = plurx_core::scan::probe::parse_probe_json(&j["probe"]);
        p.max_cll = Some(2259);
        p.max_fall = Some(183);
        p.mastering_max_luminance = Some(1000);
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let l = store
            .create_library(&NewLibrary {
                name: "incident".into(),
                kind: LibraryKind::Movies,
                paths: vec![],
                anime: false,
            })
            .await
            .unwrap();
        let item = store
            .insert_item(&NewItem {
                library_id: l.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: "fixture".into(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .unwrap();
        let id = store
            .upsert_file(item, "/sanitized/movie.mkv", 18986891679, 1790349395, &p)
            .await
            .unwrap();
        let file = store.get_file(id).await.unwrap().unwrap();
        let base = crate::test_tempdir().unwrap();
        let manager = TranscodeManager::new(
            store,
            base.path().join("work"),
            EncoderCaps {
                vaapi: true,
                ..Default::default()
            },
            Pipeline::Cpu,
        )
        .with_cache(
            base.path().join("cache"),
            "incident-ffmpeg".into(),
            "incident".into(),
        )
        .with_decoders(vec!["hevc".into()]);
        let caps:plurx_core::playback::DeviceCaps=serde_json::from_value(serde_json::json!({"v":2,"video":[{"codec":"h264","decode":true,"present":["sdr"]}],"audio":["aac"],"transports":["hls"]})).unwrap();
        let rows = manager
            .quality_candidates_with_copy_contract(
                &file,
                &caps,
                Some(1),
                0,
                None,
                Presentation::Vod,
                None,
            )
            .await;
        let heights: Vec<_> = rows.iter().map(|row| row.target_height).collect();
        eprintln!("INCIDENT catalog_count={} heights={heights:?}", rows.len());
        assert_eq!(heights, vec![144, 240, 360, 480, 720, 1080, 1918]);
        assert!(rows.iter().all(|row| row.decoder_compatible));
    }
}
