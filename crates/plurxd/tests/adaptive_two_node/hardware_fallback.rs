use super::*;

const RELAY_PATH: &str = "/internal/cluster/media/sessions/relay";
// Playback exposes per-kind track ordinals.
const AUDIO_INDEX: i64 = 0;

fn caps() -> Value {
    json!({
        "v": 2,
        "video": [{"codec":"h264", "profiles":["baseline","main","high"],
                   "present":["sdr"], "max_height":1080, "max_width":1920}],
        "audio": ["aac"],
        "containers": ["mkv", "mp4"],
        "display": {"hdr":false, "dolby_vision":false}
    })
}

async fn hardware_slots(cluster: &Cluster, base: &str) -> (u64, u64) {
    let system = cluster.get_json(base, "/api/v1/system").await;
    (
        system["hw_slots_in_use"]
            .as_u64()
            .expect("live hardware slots"),
        system["hw_slots_max"].as_u64().expect("hardware limit"),
    )
}

async fn start_hardware(cluster: &Cluster, file: i64, height: i64, playback: &str) -> Value {
    let response = cluster
        .client
        .post(format!("{}/api/v1/files/{file}/decision", cluster.a_base))
        .query(&[("audio", AUDIO_INDEX), ("subtitle", -1)])
        .bearer_auth(&cluster.token)
        .json(&json!({"caps": caps()}))
        .send()
        .await
        .expect("decision request");
    let status = response.status();
    let body = response.text().await.expect("decision body");
    assert_eq!(status, StatusCode::OK, "{body}{}", cluster.logs());
    let decision: Value = serde_json::from_str(&body).expect("decision JSON");
    let candidate = decision["quality_candidates"]
        .as_array()
        .expect("quality catalog")
        .iter()
        .find(|row| {
            row["route"] == "encode"
                && row["planned_codec"] == "h264"
                && row["decoder_compatible"] == true
                && row["processed_dv"] != true
                && row["target_height"] == height
        })
        .unwrap_or_else(|| panic!("no compatible AVC candidate at {height}: {decision}"));
    let candidate = candidate["id"].as_str().expect("exact candidate identity");
    let response = cluster
        .client
        .post(format!(
            "{}/api/v1/files/{file}/hls/sessions",
            cluster.a_base
        ))
        .bearer_auth(&cluster.token)
        .json(&json!({
            "playback_id": playback,
            "request_id": uuid::Uuid::new_v4(),
            "start": 0,
            "height": height,
            "copy": false,
            "quality_auto": true,
            "audio": AUDIO_INDEX,
            "subtitle": -1,
            "caps": caps(),
            "intent": {
                "lifetime_id": uuid::Uuid::new_v4(),
                "recipe_revision": 1,
                "destination_revision": 1,
                "transport_revision": 1,
                "selection": {
                    "quality": {"mode":"auto", "height":height, "candidate_id":candidate},
                    "codec":"h264", "dynamic_range":"sdr",
                    "audio_track":AUDIO_INDEX, "audio_offset_ms":0,
                    "subtitles":{"mode":"off"}
                }
            }
        }))
        .send()
        .await
        .expect("public negotiated hardware Start");
    let status = response.status();
    let body = response.text().await.expect("Start response");
    assert_eq!(status, StatusCode::OK, "{body}{}", cluster.logs());
    let started: Value = serde_json::from_str(&body).expect("Start JSON");
    assert_eq!(started["height"], height);
    assert_eq!(started["quality_candidate_id"], candidate);
    assert_eq!(started["display_aware_auto_protocol"], "route-v1");
    assert_eq!(started["vod"], true);
    assert_eq!(started["encoder"], "VA-API");
    started
}

async fn stop(cluster: &Cluster, started: &Value) {
    let session = started["session_id"].as_str().expect("started session");
    let response = cluster
        .client
        .delete(format!("{}/api/v1/hls/{session}", cluster.a_base))
        .bearer_auth(&cluster.token)
        .send()
        .await
        .expect("public stop");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a real supported VA-API encoder and an isolated two-daemon lab"]
async fn real_hardware_busy_uses_one_signed_peer_start_and_releases_its_slot() {
    let ffmpeg = std::env::var("PLURX_FFMPEG").expect("pinned real FFmpeg");
    let root = canonical_tempdir();
    let media = root.path().join("media");
    std::fs::create_dir(&media).expect("media directory");
    write_fixture(
        &ffmpeg,
        &media.join("Hardware Fallback (2026).mkv"),
        "testsrc=size=1280x720:rate=24:duration=120",
        "sine=frequency=440:sample_rate=48000:duration=120",
        "48",
    );
    let mut cluster = Cluster::start_with_encoder(root, "vaapi", false).await;
    let (a, b) = (cluster.a_base.clone(), cluster.b_base.clone());
    let settings = cluster
        .client
        .put(format!("{a}/api/v1/settings"))
        .bearer_auth(&cluster.token)
        .json(&json!({
            "cluster_media_pool_enabled": true,
            "transcode_max_hw_sessions": 1,
            "transcode_software_pool_threads": 8,
            "hls_readrate": "1"
        }))
        .send()
        .await
        .expect("slot policy");
    assert_eq!(settings.status(), StatusCode::OK);
    let file = scan_fixture(
        &cluster.client,
        &mut cluster.node_a,
        &a,
        &cluster.token,
        &media,
        Some((&cluster.node_b, &b)),
    )
    .await;
    cluster
        .wait_grant_policy(&a, "met", Duration::from_secs(60))
        .await;
    cluster
        .wait_grant_policy(&b, "met", Duration::from_secs(60))
        .await;
    assert_eq!(hardware_slots(&cluster, &a).await, (0, 1));
    assert_eq!(hardware_slots(&cluster, &b).await, (0, 1));

    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let offers = cluster
            .client
            .post(format!("{a}/api/v1/cluster/media/offers"))
            .bearer_auth(&cluster.token)
            .json(&json!({
                "file_id":file, "target_height":360, "start_millis":0,
                "audio_index":AUDIO_INDEX, "subtitle_index":null, "hdr10":false
            }))
            .send()
            .await
            .expect("signed peer offer round")
            .error_for_status()
            .expect("offers status")
            .json::<Value>()
            .await
            .expect("offers JSON");
        if offers["offers"].as_array().is_some_and(|rows| {
            rows.iter().any(|offer| {
                offer["node_id"] == cluster.b_id
                    && offer["eligible"] == true
                    && offer["initial_hardware_vod"] == true
                    && offer["cache_hit"] == false
                    && offer["encoder"] == "vaapi"
                    && offer["free_hardware_slots"] == 1
            })
        }) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "B must offer a real unused VA-API slot for the cold recipe: {offers}{}",
            cluster.logs(),
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    let start_calls = cluster.proxy_b.hits(START_PATH);
    let relay_calls = cluster.proxy_b.hits(RELAY_PATH);
    let incumbent = start_hardware(&cluster, file, 480, "hardware-slot-incumbent").await;
    cluster.play_created(&incumbent, "A incumbent", true).await;
    assert_eq!(cluster.proxy_b.hits(START_PATH), start_calls);
    assert_eq!(cluster.proxy_b.hits(RELAY_PATH), relay_calls);
    assert_eq!(hardware_slots(&cluster, &a).await, (1, 1));
    assert_eq!(hardware_slots(&cluster, &b).await, (0, 1));

    let peer = start_hardware(&cluster, file, 360, "hardware-peer-after-busy").await;
    assert_eq!(cluster.proxy_b.hits(START_PATH), start_calls + 1);
    let accepted = cluster
        .proxy_b
        .state
        .hardware_start
        .lock()
        .expect("Start receipt")
        .clone()
        .expect("successful signed hardware Start");
    assert!(accepted.cutoff_unix_ms > 0);
    assert_eq!(peer["session_id"], accepted.session_id);
    cluster.play_created(&peer, "B fallback", true).await;
    assert!(cluster.proxy_b.hits(RELAY_PATH) > relay_calls);
    assert_eq!(hardware_slots(&cluster, &a).await, (1, 1));
    assert_eq!(hardware_slots(&cluster, &b).await, (1, 1));

    stop(&cluster, &peer).await;
    let deadline = Instant::now() + Duration::from_secs(10);
    while hardware_slots(&cluster, &b).await != (0, 1) {
        assert!(
            Instant::now() < deadline,
            "B still owns the original slot{}",
            cluster.logs()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(hardware_slots(&cluster, &a).await, (1, 1));
    stop(&cluster, &incumbent).await;
}
