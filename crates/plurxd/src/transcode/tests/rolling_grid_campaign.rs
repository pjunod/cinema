//! Explicit owned-lab acquisition entry point, never an ordinary unit run.
//! The private bridge uses the shipped HTTP body pump; it cannot attest a
//! public create route, physical screen, native client or HDR presentation.
use super::*;
use axum::{
    extract::State,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use plurx_core::store::SqliteStore;
use serde::Deserialize;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Cell {
    nonce: String,
    root: PathBuf,
    source: PathBuf,
    source_sha256: String,
    probe: PathBuf,
    height: i64,
    page: PathBuf,
}

#[derive(Clone)]
struct Bridge {
    state: crate::state::AppState,
    session: String,
    generation: String,
    nonce: String,
    gate: Arc<tokio::sync::Semaphore>,
    frames: Arc<Mutex<(u64, f64, u64, Instant)>>,
    deadline: Instant,
    observations: Arc<AtomicU64>,
    root: PathBuf,
}

fn bounded(path: &std::path::Path, cap: u64) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > cap {
        return Err("non-file, symlink or oversized input".into());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(cap + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > cap {
        return Err("growing input exceeded cap".into());
    }
    Ok(bytes)
}

async fn media(
    State(b): State<Bridge>,
    axum::extract::Path((nonce, name)): axum::extract::Path<(String, String)>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    use futures_util::StreamExt;
    if nonce != b.nonce || Instant::now() >= b.deadline {
        return axum::http::StatusCode::GONE.into_response();
    }
    let permit = match Arc::clone(&b.gate).try_acquire_owned() {
        Ok(p) => p,
        Err(_) => return axum::http::StatusCode::TOO_MANY_REQUESTS.into_response(),
    };
    let response = if name == "index.m3u8" {
        crate::http::hls::playlist(
            axum::extract::State(b.state),
            axum::extract::Path(b.session),
            axum::extract::Query(Default::default()),
            headers,
        )
        .await
    } else if is_safe_segment(&name) && name.ends_with(".ts") {
        crate::http::hls::segment(
            axum::extract::State(b.state),
            axum::extract::Path((b.session, name)),
            headers,
        )
        .await
    } else {
        return axum::http::StatusCode::NOT_FOUND.into_response();
    };
    let response = match response {
        Ok(r) => r,
        Err(e) => e.into_response(),
    };
    // Hold admission through actual downstream body EOF/drop, not just headers.
    // The existing HLS body pump alone settles delivery and authorization.
    let (parts, body) = response.into_parts();
    let stream = futures_util::stream::unfold(
        (body.into_data_stream(), permit),
        |(mut stream, permit)| async move { stream.next().await.map(|chunk| (chunk, (stream, permit))) },
    );
    axum::response::Response::from_parts(parts, axum::body::Body::from_stream(stream))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Frame {
    nonce: String,
    generation: String,
    session: String,
    producer_attempt: u64,
    presented_frames: u64,
    media_time: f64,
    current_time: f64,
    buffered_from: f64,
    buffered_through: f64,
}

async fn frame(State(b): State<Bridge>, Json(f): Json<Frame>) -> axum::response::Response {
    use crate::playback_control::*;
    if Instant::now() >= b.deadline
        || f.nonce != b.nonce
        || f.generation != b.generation
        || f.session != b.session
        || ![
            f.media_time,
            f.current_time,
            f.buffered_from,
            f.buffered_through,
        ]
        .iter()
        .all(|x| x.is_finite() && *x >= 0.0)
        || (f.current_time - f.media_time).abs() > 1.0
        || f.buffered_from > f.current_time
        || f.buffered_through < f.current_time
    {
        return axum::http::StatusCode::BAD_REQUEST.into_response();
    }
    let Some(session) = b
        .state
        .transcode
        .sessions
        .lock()
        .await
        .get(&b.session)
        .cloned()
    else {
        return axum::http::StatusCode::GONE.into_response();
    };
    if session.control.current_producer_attempt() != f.producer_attempt {
        return axum::http::StatusCode::CONFLICT.into_response();
    }
    let mut last = b.frames.lock().await;
    if f.presented_frames <= last.0
        || f.media_time <= last.1
        || last.3.elapsed() < Duration::from_millis(450)
        || last.2 >= 4096
    {
        return axum::http::StatusCode::CONFLICT.into_response();
    }
    let sequence = last.2 + 1;
    let origin = (session.media_origin_seconds * 1000.0).round() as i64;
    let request: ControlRequestV1 = match serde_json::from_value(serde_json::json!({
        "protocol":PROTOCOL_V1,"generation":b.generation,"control_epoch":1,
        "client_instance_id":b.generation,"sequence":sequence,"demand":"active",
        "position_ms":origin + (f.media_time*1000.0).round() as i64,
        "buffered_from_ms":origin + (f.buffered_from*1000.0).round() as i64,
        "buffered_through_ms":origin + (f.buffered_through*1000.0).round() as i64,
        "playback_rate":1.0,"render_state":"rendering","seek_target_ms":null,
        "selection":{"quality":{"mode":"manual","height":session.target_height},"audio_track":0,"subtitle":{"mode":"off","track":null},"audio_offset_ms":0,"codec":"h264","dynamic_range":"sdr"},
        "capabilities":{"platform":"web","max_height":1080,"codecs":["h264"],"dynamic_ranges":["sdr"],"dual_player_preparation":false},"supported_actions":[]
    })) {
        Ok(r) => r,
        Err(_) => return axum::http::StatusCode::BAD_REQUEST.into_response(),
    };
    if request.validate(None, 2000).is_err() {
        return axum::http::StatusCode::BAD_REQUEST.into_response();
    }
    let result = b
        .state
        .transcode
        .hls_session_control(LocalControlRequest {
            session_id: &b.session,
            generation: &b.generation,
            owner_node_id: "rolling-lab",
            owner_epoch: 1,
            client_instance_id: &b.generation,
            sequence,
            snapshot: PlaybackDemandSnapshot::from(&request),
            prepared_successor: PreparedSuccessorObservation::Inactive,
        })
        .await;
    match result {
        Some(Ok(result)) if result.disposition == ControlDisposition::Accepted => {
            *last = (f.presented_frames, f.media_time, sequence, Instant::now());
            b.observations.fetch_add(1, Relaxed);
            let record = serde_json::json!({"sequence":sequence,"frames":f.presented_frames,"media_time":f.media_time,"position_ms":request.position_ms,"status":result.status});
            let path = b.root.join(format!("observation-{sequence:04}.json"));
            if std::fs::write(path, record.to_string()).is_err() {
                return axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
            Json(record).into_response()
        }
        _ => axum::http::StatusCode::CONFLICT.into_response(),
    }
}

#[tokio::test]
#[ignore = "explicit owned rolling campaign only; requires validated manifest and real browser"]
async fn owned_real_rolling_cell() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("plurxd::transcode=info")
        .try_init();
    let path = std::env::var_os("PLURX_ROLLING_CELL").expect("explicit cell manifest required");
    let cell: Cell = serde_json::from_slice(
        &bounded(std::path::Path::new(&path), 65536).expect("bounded manifest"),
    )
    .expect("manifest schema");
    assert_eq!(cell.nonce.len(), 64);
    assert!(cell.nonce.bytes().all(|b| b.is_ascii_hexdigit()));
    assert!([360, 480, 720, 1080].contains(&cell.height));
    assert!(
        cell.root.is_absolute()
            && (cell.root.parent() == Some(std::path::Path::new("/private/tmp"))
                || cell.root.parent() == Some(std::path::Path::new("/var/tmp")))
    );
    assert_eq!(
        std::fs::canonicalize(&cell.root).expect("owned root"),
        cell.root
    );
    let root_meta = std::fs::symlink_metadata(&cell.root).expect("root metadata");
    assert_eq!(
        root_meta.permissions().mode() & 0o077,
        0,
        "mode700 owned root required"
    );
    assert_eq!(root_meta.uid(), unsafe { libc::geteuid() }, "foreign root");
    let owner = bounded(&cell.root.join("owner.json"), 65536).expect("owner");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&owner).expect("owner json")["nonce"],
        cell.nonce
    );
    assert_eq!(
        std::fs::canonicalize(&cell.source).expect("source"),
        cell.source
    );
    let source_before = std::fs::symlink_metadata(&cell.source).expect("source metadata");
    assert!(
        source_before.is_file()
            && !source_before.file_type().is_symlink()
            && source_before.len() <= 2 * 1024 * 1024 * 1024
    );
    assert_eq!(source_before.uid(), root_meta.uid(), "foreign source");
    use sha2::Digest;
    let mut hash = sha2::Sha256::new();
    let mut source = std::fs::File::open(&cell.source).expect("source open");
    let mut block = [0u8; 65536];
    use std::io::Read;
    let mut read = 0u64;
    loop {
        let n = source.read(&mut block).expect("source hash read");
        if n == 0 {
            break;
        }
        read += n as u64;
        assert!(read <= 2 * 1024 * 1024 * 1024, "source grew beyond cap");
        hash.update(&block[..n]);
    }
    assert_eq!(
        format!("{:x}", hash.finalize()),
        cell.source_sha256,
        "source hash mismatch"
    );
    let raw: serde_json::Value =
        serde_json::from_slice(&bounded(&cell.probe, 1048576).expect("bounded probe"))
            .expect("supplied probe");
    let expected = plurx_core::scan::probe::parse_probe_json(&raw);
    let probe = plurx_core::scan::probe::probe_single_threaded(&cell.source)
        .await
        .expect("actual pinned-tool source probe");
    assert_eq!(
        (probe.width, probe.height, probe.video_codec.as_deref()),
        (
            expected.width,
            expected.height,
            expected.video_codec.as_deref()
        ),
        "probe mismatch"
    );
    assert!(probe.duration_ms.is_some_and(|ms| ms >= 64000));
    assert!(probe.height.is_some_and(|h| h >= cell.height));
    assert_eq!(cell.source_sha256.len(), 64);
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("owned store"));
    let file =
        super::seed_file_with_probe_at(&store, &cell.source.to_string_lossy(), probe.clone()).await;
    let item = store
        .get_file(file)
        .await
        .expect("file lookup")
        .expect("file")
        .item_id;
    assert_eq!(
        store
            .upsert_file(
                item,
                &cell.source.to_string_lossy(),
                source_before.len() as i64,
                source_before.mtime(),
                &probe
            )
            .await
            .expect("real source metadata"),
        file
    );
    let caps = plurx_core::transcode::detect_encoders(&crate::ffmpeg::ffmpeg_bin()).await;
    std::fs::write(
        cell.root.join("measured-encoder-caps.json"),
        serde_json::to_vec(&caps).expect("caps json"),
    )
    .expect("caps receipt");
    let state = crate::state::AppState::new(
        "rolling-lab".into(),
        Arc::clone(&store),
        crate::create_dirs(&cell.root).expect("dirs"),
        "rolling-lab".into(),
        caps,
        Default::default(),
        Arc::new(crate::logbuf::LogBuffer::new(512)),
    );
    let req:SessionRequest=serde_json::from_value(serde_json::json!({"file_id":file,"playback_id":cell.nonce,"request_id":null,"control_sequence":null,"automatic":false,"previous_session_id":null,"reopen_reason":null,"kind":{"kind":"transcode","height":cell.height},"start_seconds":0.0,"audio_index":null,"subtitle_burn":null,"audio_offset_ms":0,"hdr10":false,"presentation":"live","transport":"hlsjs"})).expect("internal request");
    let info = state
        .transcode
        .create_session(&req, "rolling-lab")
        .await
        .expect("actual owner start");
    let generation = uuid::Uuid::new_v4().to_string();
    use futures_util::FutureExt;
    let run=std::panic::AssertUnwindSafe(async {
    assert!(!info.vod && info.encoder != "copy" && info.encoder != "cached");
    assert_eq!(info.target_height,cell.height,"actual producer rung differs");
    let now = crate::media_sessions::unix_ms();
    let fingerprint = req.intent_fingerprint("rolling-lab");
    store
        .claim_media_session_request(
            7,
            &generation,
            &fingerprint,
            &cell.nonce,
            &generation,
            now,
            now + 600000,
        )
        .await
        .expect("claim");
    assert!(store
        .assign_media_session_request_owner(7, &generation, &generation, "rolling-lab", now)
        .await
        .expect("assign"));
    let origin_ms = (info.media_origin_seconds * 1000.0).round() as i64;
    let response_json=serde_json::json!({"session_id":info.session_id,"playlist_url":info.playlist_url,"duration_ms":info.duration_ms,"media_origin_seconds":info.media_origin_seconds,"target_height":info.target_height,"encoder":info.encoder,"grade":info.grade,"vod":info.vod}).to_string();
    let activation = plurx_core::domain::MediaSessionActivation {
        recovery_epoch: String::new(),
        expected_desired_revision: None,
        incarnation_id: generation.clone(),
        session_id: info.session_id.clone(),
        user_id: 7,
        playback_id: cell.nonce.clone(),
        expected_predecessor_incarnation_id: None,
        fence_predecessor: false,
        request_id: Some(generation.clone()),
        request_fingerprint: fingerprint,
        owner_node_id: "rolling-lab".into(),
        recipe_json: serde_json::to_string(&req).expect("recipe"),
        response_json,
        publication_ready_at_ms: plurx_core::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,
        media_origin_ms: origin_ms,
        now_ms: now,
        lease_expires_at_ms: now + 600000,
    };
    store
        .activate_media_session(&activation)
        .await
        .expect("activate")
        .expect("accepted");
    store
        .settle_media_session_activation(
            &activation,
            plurx_core::domain::MediaSessionActivationSettlement::Confirm {
                publication_ready_at_ms: 0,
            },
            now,
        )
        .await
        .expect("settle")
        .expect("confirmed");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback");
    let port = listener.local_addr().expect("address").port();
    let bridge = Bridge {
        state: state.clone(),
        session: info.session_id.clone(),
        generation: generation.clone(),
        nonce: cell.nonce.clone(),
        gate: Arc::new(tokio::sync::Semaphore::new(4)),
        frames: Arc::new(Mutex::new((
            0,
            -1.0,
            0,
            Instant::now() - Duration::from_secs(1),
        ))),
        deadline: Instant::now() + Duration::from_secs(600),
        observations: Arc::new(AtomicU64::new(0)),
        root: cell.root.clone(),
    };
    let ready = serde_json::json!({"port":port,"nonce":cell.nonce,"session":info.session_id,"generation":generation,"producer_attempt":1,"media_origin_ms":origin_ms,"source_sha256":cell.source_sha256});
    let page = String::from_utf8(bounded(&cell.page, 65536).expect("page"))
        .expect("page UTF8")
        .replace("READY", &ready.to_string());
    let hls = include_bytes!("../../web/hls.min.js");
    let page_nonce = cell.nonce.clone();
    let app = Router::new()
        .route(
            "/page/{nonce}",
            get(
                move |axum::extract::Path(nonce): axum::extract::Path<String>| {
                    let page = page.clone();
                    let expected = page_nonce.clone();
                    async move {
                        if nonce == expected {
                            axum::response::Html(page).into_response()
                        } else {
                            axum::http::StatusCode::NOT_FOUND.into_response()
                        }
                    }
                },
            ),
        )
        .route(
            "/hls.js",
            get(|| async { ([("content-type", "text/javascript")], hls.as_slice()) }),
        )
        .route("/media/{nonce}/{name}", get(media))
        .route("/frame", post(frame))
        .with_state(bridge.clone());
    std::fs::write(cell.root.join("ready.json"), ready.to_string()).expect("ready");
    let stop = cell.root.join("stop");
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let until = tokio::time::Instant::now() + Duration::from_secs(600);
            while !stop.exists() && tokio::time::Instant::now() < until {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        })
        .await
        .expect("bridge");
    bridge.observations.load(Relaxed)
    }).catch_unwind().await;
    let stopped = state
        .transcode
        .stop_session(&info.session_id, "owned-lab-finished")
        .await;
    std::fs::write(cell.root.join("bridge-final.json"),serde_json::json!({"session":info.session_id,"generation":generation,"stopped":stopped,"accepted_observations":run.as_ref().ok(),"source_sha256":cell.source_sha256,"inner_completed":run.is_ok()}).to_string()).expect("final ownership receipt");
    let observations = match run {
        Ok(n) => n,
        Err(error) => std::panic::resume_unwind(error),
    };
    let after = std::fs::metadata(&cell.source).expect("source after");
    assert_eq!(
        (
            source_before.dev(),
            source_before.ino(),
            source_before.len(),
            source_before.mtime(),
            source_before.mtime_nsec()
        ),
        (
            after.dev(),
            after.ino(),
            after.len(),
            after.mtime(),
            after.mtime_nsec()
        ),
        "source changed"
    );
    assert!(
        observations >= 2,
        "two real accepted frame observations required"
    );
}
