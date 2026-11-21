use super::*;

const RELAY_PATH: &str = "/internal/cluster/media/sessions/relay";

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("missing controlled FEL prerequisite {name}"))
}

fn run_tool(command: &mut Command) -> std::process::Output {
    let output = command.output().expect("run controlled FEL tool");
    assert!(
        output.status.success(),
        "{command:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn controls() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/dv_processing/controls")
}

async fn readiness(cluster: &Cluster, base: &str, card: &str, requirement: &str) -> Value {
    let items = cluster.get_json(base, "/api/v1/developer/readiness").await;
    items["items"]
        .as_array()
        .expect("developer items")
        .iter()
        .find(|row| row["id"] == card)
        .expect("developer card")["requirements"]
        .as_array()
        .expect("developer requirements")
        .iter()
        .find(|row| row["id"] == requirement)
        .expect("developer requirement")
        .clone()
}

struct HeldHelper {
    descriptor: std::os::fd::OwnedFd,
    paused: bool,
}

impl HeldHelper {
    fn pause(daemon: &Daemon, executable: &Path) -> Option<Self> {
        use std::os::fd::FromRawFd;

        let pid = owned_render_child(daemon, executable)?;
        // Linux pins one process instance in this descriptor. Subsequent
        // signals cannot be delivered to a process that reuses its PID.
        let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if raw < 0 {
            return None;
        }
        // A successful pidfd_open returned an owned descriptor.
        let descriptor = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw as i32) };
        let mut helper = Self {
            descriptor,
            paused: false,
        };
        helper.paused =
            owned_render_child(daemon, executable) == Some(pid) && helper.signal(libc::SIGSTOP);
        helper.paused.then_some(helper)
    }

    fn resume(&mut self) {
        if self.paused && (self.signal(libc::SIGCONT) || self.exited()) {
            self.paused = false;
        }
    }

    fn exited(&self) -> bool {
        use std::os::fd::AsRawFd;

        let mut fd = libc::pollfd {
            fd: self.descriptor.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // The kernel makes this exact pidfd readable when it exits.
        unsafe { libc::poll(&mut fd, 1, 0) == 1 && fd.revents & libc::POLLIN != 0 }
    }

    fn signal(&self, signal: i32) -> bool {
        use std::os::fd::AsRawFd;

        // The verified descendant's descriptor remains owned until Drop.
        unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.descriptor.as_raw_fd(),
                signal,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            ) == 0
        }
    }
}

impl Drop for HeldHelper {
    fn drop(&mut self) {
        self.resume();
        if self.paused {
            self.signal(libc::SIGKILL);
        }
    }
}

fn children(pid: u32) -> impl Iterator<Item = u32> {
    std::fs::read_dir(format!("/proc/{pid}/task"))
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|task| std::fs::read_to_string(task.path().join("children")).ok())
        .flat_map(|text| {
            text.split_whitespace()
                .filter_map(|pid| pid.parse().ok())
                .collect::<Vec<_>>()
        })
}

fn owned_render_child(daemon: &Daemon, executable: &Path) -> Option<u32> {
    let mut queue = std::collections::VecDeque::from([daemon.child.id()]);
    let mut checked = std::collections::HashSet::new();
    while let Some(pid) = queue.pop_front() {
        if !checked.insert(pid) {
            continue;
        }
        if std::fs::read_link(format!("/proc/{pid}/exe")).is_ok_and(|current| current == executable)
        {
            return Some(pid);
        }
        queue.extend(children(pid));
    }
    None
}

fn caps() -> Value {
    json!({
        "v": 2,
        "video": [{"codec":"hevc","profiles":["main10"],"present":["sdr","pq"],"dv_profiles":[8],"max_height":2160}],
        "transports": ["hls"], "hls_hevc_sample_entries": ["hvc1"],
        "audio": ["aac"], "containers": ["mkv","mp4"], "dv_transport":"hls",
        "display": {"hdr":true,"dolby_vision":true}
    })
}

async fn start(cluster: &Cluster, file: i64, playback: &str) -> Value {
    let decision = cluster
        .client
        .post(format!("{}/api/v1/files/{file}/decision", cluster.a_base))
        .bearer_auth(&cluster.token)
        .json(&json!({"caps": caps()}))
        .send()
        .await
        .expect("negotiate the ordinary Dolby Vision starter");
    let status = decision.status();
    let body = decision.text().await.expect("decision body");
    assert_eq!(status, StatusCode::OK, "{body}{}", cluster.logs());
    let decision: Value = serde_json::from_str(&body).expect("decision JSON");
    let delivery = &decision["delivery"];
    assert_eq!(delivery["mode"], "remux", "{decision}");
    assert_eq!(delivery["preserve_dolby_vision"], true, "{decision}");
    let candidate = decision["quality_candidates"]
        .as_array()
        .expect("ordinary playback candidates")
        .iter()
        .find(|row| {
            row["route"] == "remux"
                && row["decoder_compatible"] == true
                && row["processed_dv"] != true
        })
        .unwrap_or_else(|| panic!("no ordinary compatible Dolby Vision candidate: {decision}"));
    let candidate_id = candidate["id"].as_str().expect("ordinary candidate ID");
    let height = candidate["target_height"]
        .as_i64()
        .expect("ordinary raster");
    let sessions = delivery["sessions_url"]
        .as_str()
        .expect("negotiated sessions URL");
    let response = cluster
        .client
        .post(wire_url(&cluster.a_base, sessions))
        .bearer_auth(&cluster.token)
        .json(&json!({
            "playback_id": playback,
            "copy": true,
            "aac": delivery["aac"],
            "preserve_dolby_vision": delivery["preserve_dolby_vision"],
            "quality_auto": true,
            "caps": caps(),
            "intent": {
                "lifetime_id": uuid::Uuid::new_v4(),
                "recipe_revision": 1,
                "destination_revision": 1,
                "transport_revision": 1,
                "selection": {
                    "quality": {"mode":"auto", "height":height, "candidate_id":candidate_id},
                    "codec":"hevc", "dynamic_range":"dolby_vision",
                    "audio_track":null, "audio_offset_ms":0,
                    "subtitles":{"mode":"off"}
                }
            }
        }))
        .send()
        .await
        .expect("ordinary FEL source start");
    let status = response.status();
    let body = response.text().await.expect("start body");
    assert_eq!(status, StatusCode::OK, "{body}{}", cluster.logs());
    let response: Value = serde_json::from_str(&body).expect("start response");
    assert_eq!(response["quality_candidate_id"], candidate_id, "{response}");
    assert_eq!(
        response["enhancement"]["active"],
        json!(["DYN"]),
        "{response}"
    );
    response
}

fn wire_url(base: &str, relative: &str) -> reqwest::Url {
    reqwest::Url::parse(base)
        .expect("harness base")
        .join(relative)
        .expect("media URL")
}

async fn window(cluster: &Cluster, playback: &Value) -> Vec<u8> {
    let mut url = wire_url(
        &cluster.a_base,
        playback["playlist_url"].as_str().expect("playlist URL"),
    );
    let mut preferred_seconds = 0.0;
    for _ in 0..2 {
        let response = cluster
            .client
            .get(url.clone())
            .bearer_auth(&cluster.token)
            .send()
            .await
            .expect("playlist");
        assert_eq!(response.status(), StatusCode::OK, "{}", cluster.logs());
        let text = response.text().await.expect("playlist text");
        if let Some(offset) = text
            .lines()
            .find_map(|line| line.strip_prefix("#EXT-X-START:TIME-OFFSET="))
            .and_then(|value| value.split(',').next())
        {
            preferred_seconds = offset.parse().expect("playlist acquisition offset");
        }
        let media = text
            .lines()
            .find(|line| !line.trim().is_empty() && !line.starts_with('#'))
            .expect("playlist has a media item");
        let item = url.join(media).expect("media or variant URL");
        if item.path().ends_with(".m3u8") {
            url = item;
            continue;
        }
        let mut segment_end = 0.0;
        let mut parts = text.lines();
        let media = loop {
            let duration = parts
                .find_map(|line| line.strip_prefix("#EXTINF:"))
                .and_then(|value| value.split(',').next())
                .expect("playlist covers the promised acquisition offset");
            segment_end += duration.parse::<f64>().expect("segment duration");
            let segment = parts
                .find(|line| !line.trim().is_empty() && !line.starts_with('#'))
                .expect("media item after its duration");
            if segment_end > preferred_seconds {
                break segment;
            }
        };
        let init = text
            .lines()
            .find_map(|line| line.strip_prefix("#EXT-X-MAP:URI=\""))
            .and_then(|value| value.split('"').next())
            .expect("fMP4 initializer");
        let mut bytes = Vec::new();
        for part in [init, media] {
            let reply = cluster
                .client
                .get(url.join(part).expect("fMP4 part URL"))
                .bearer_auth(&cluster.token)
                .send()
                .await
                .expect("fMP4 part");
            assert_eq!(reply.status(), StatusCode::OK, "{}", cluster.logs());
            bytes.extend_from_slice(&reply.bytes().await.expect("fMP4 bytes"));
        }
        assert!(!bytes.is_empty());
        return bytes;
    }
    panic!("a single HLS master and media playlist are expected");
}

async fn decoded_first_picture(
    cluster: &Cluster,
    playback: &Value,
    ffmpeg: &str,
    dump: &Path,
) -> u64 {
    let started = Instant::now();
    std::fs::write(dump, window(cluster, playback).await).expect("actual viewer bytes");
    let decoded = run_tool(
        Command::new(ffmpeg)
            .args(["-nostdin", "-v", "error", "-i"])
            .arg(dump)
            .args(["-frames:v", "1", "-f", "rawvideo", "pipe:1"]),
    );
    assert!(
        !decoded.stdout.is_empty(),
        "first picture must actually decode"
    );
    u64::try_from(started.elapsed().as_millis()).expect("bounded first picture timing")
}

async fn control(
    cluster: &Cluster,
    playback: &Value,
    client: &str,
    sequence: u64,
    rendered_position_ms: Option<u64>,
    ack: Value,
) -> Value {
    let control = &playback["control"];
    let position = rendered_position_ms.unwrap_or_default();
    let render_state = if rendered_position_ms.is_some() {
        "rendering"
    } else {
        "starting"
    };
    let response = cluster
        .client
        .post(wire_url(
            &cluster.a_base,
            control["url"].as_str().expect("control URL"),
        ))
        .bearer_auth(&cluster.token)
        .json(&json!({
            "protocol":control["protocol"], "generation":control["generation"],
            "control_epoch":control["control_epoch"], "client_instance_id":client,
            "sequence":sequence, "demand":"active", "position_ms":position,
            "buffered_from_ms":0, "buffered_through_ms":position + 2000, "playback_rate":1,
            "render_state":render_state,
            "selection":{"quality":{"mode":"auto"},"subtitle":{"mode":"off"},
                         "audio_offset_ms":0,"codec":"auto","dynamic_range":"auto"},
            "capabilities":{
                "platform":"web", "max_height":2160, "codecs":["hevc"],
                "dynamic_ranges":["sdr","hdr10","dolby_vision"],
                "dual_player_preparation":true
            },
            "acknowledgement":ack,
            "supported_actions":["hold","retry_resource","terminal","prepare_replacement"]
        }))
        .send()
        .await
        .expect("real viewer exchange");
    let status = response.status();
    let body = response.text().await.expect("control body");
    assert_eq!(status, StatusCode::OK, "{body}{}", cluster.logs());
    serde_json::from_str(&body).expect("control reply")
}

async fn first_frame(cluster: &Cluster, file: i64, started: &Value, elapsed_ms: u64) {
    let reply = cluster
        .client
        .post(format!("{}/api/v1/client-log", cluster.a_base))
        .bearer_auth(&cluster.token)
        .json(
            &json!({"level":"info","event":"ttff","message":"first controlled picture decoded",
                      "file_id":file,"session_id":started["session_id"],"ms":elapsed_ms}),
        )
        .send()
        .await
        .expect("origin viewer first-frame event");
    assert_eq!(reply.status(), StatusCode::NO_CONTENT);
}

async fn status(cluster: &Cluster, base: &str, started: &Value) -> StatusCode {
    cluster
        .client
        .get(format!(
            "{base}/api/v1/hls/{}/status",
            started["session_id"].as_str().expect("session capability")
        ))
        .bearer_auth(&cluster.token)
        .send()
        .await
        .expect("session status")
        .status()
}

/// Explicit Linux lab acceptance. Both daemons use the real deployed
/// renderer and a proved hardware HEVC Main10 path. The tested media is
/// authored from the repository controls, with no imported movie.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires qualified FEL hardware, the real renderer, and built redistributable controls"]
async fn cold_auto_fel_keeps_origin_until_verified_peer_commit() {
    let encoder = required("PLURX_FEL_CONTROL_ENCODER");
    assert!(matches!(encoder.as_str(), "vaapi" | "qsv" | "nvenc"));
    let ffmpeg = required("PLURX_FFMPEG");
    let ffprobe = required("PLURX_FFPROBE");
    let _dovi = required("PLURX_DOVI_TOOL");
    let mkvmerge = required("PLURX_MKVMERGE");
    let bundle = PathBuf::from(required("PLURX_DV_PROCESSING_DIR"));
    let renderer = bundle
        .join("bin/segment_decode_render")
        .canonicalize()
        .expect("real renderer");
    assert!(
        renderer.is_file(),
        "the renderer must be an actual executable"
    );
    let control_root = required("PLURX_FEL_CONTROL_ROOT");
    let root = canonical_tempdir();
    let authored = root.path().join("authored");
    run_tool(
        Command::new("python3")
            .arg(controls().join("run_window_boundary.py"))
            .arg("--author-valid-only")
            .arg(&authored)
            .args([
                "--controls",
                &control_root,
                "--ffmpeg",
                &ffmpeg,
                "--ffprobe",
                &ffprobe,
                "--mkvmerge",
                &mkvmerge,
            ]),
    );
    let media = root.path().join("media");
    std::fs::create_dir(&media).expect("media directory");
    let source = media.join("Cold FEL Control (2026).mkv");
    std::fs::rename(authored.join("source.mkv"), &source).expect("authored FEL source");
    let ordinary_dump = root.path().join("ordinary.mp4");
    let occupied_dump = root.path().join("occupied.mp4");
    let peer_window = root.path().join("peer-window");
    std::fs::create_dir(&peer_window).expect("peer sample directory");
    let peer_dump = peer_window.join("window-000.mp4");
    let verifier = root.path().join("profile81-verifier");
    let composition = root.path().join("composition-verifier");
    let mut cluster = Cluster::start_with_encoder(root, &encoder, false).await;
    let (a, b) = (cluster.a_base.clone(), cluster.b_base.clone());
    let settings = cluster
        .client
        .put(format!("{a}/api/v1/settings"))
        .bearer_auth(&cluster.token)
        .json(&json!({"cluster_media_pool_enabled":true,
                      "dolby_vision_convert":true, "dolby_vision_fel_reencode":true}))
        .send()
        .await
        .expect("controlled FEL settings");
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

    // This cluster contains only the controlled title. Each daemon must
    // report a real verdict for its own serving backend before any viewer
    // is opened. A refused/unsupported control is a fixture failure.
    for base in [&a, &b] {
        let deadline = Instant::now() + Duration::from_secs(180);
        loop {
            let quality = readiness(
                &cluster,
                base,
                "dolby_vision_fel_reencode",
                "dv_fel_bitrate_quality",
            )
            .await;
            if quality["status"] == "met" {
                break;
            }
            assert!(Instant::now() < deadline, "{quality}{}", cluster.logs());
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    let occupied = start(&cluster, file, "fel-original-slot").await;
    let ordinary = start(&cluster, file, "fel-cold-origin").await;
    let relays = cluster.proxy_b.hits(RELAY_PATH);
    let occupied_ms = decoded_first_picture(&cluster, &occupied, &ffmpeg, &occupied_dump).await;
    let ordinary_ms = decoded_first_picture(&cluster, &ordinary, &ffmpeg, &ordinary_dump).await;
    assert_eq!(
        cluster.proxy_b.hits(RELAY_PATH),
        relays,
        "ordinary originates on A"
    );
    assert_eq!(status(&cluster, &a, &ordinary).await, StatusCode::OK);
    let occupied_client = uuid::Uuid::new_v4().to_string();
    let cold_client = uuid::Uuid::new_v4().to_string();
    // A rendering exchange can itself arm the real upgrade. Attach both
    // reporters first, then signal the holder's actual decoded picture.
    control(&cluster, &occupied, &occupied_client, 1, None, Value::Null).await;
    control(&cluster, &ordinary, &cold_client, 1, None, Value::Null).await;
    first_frame(&cluster, file, &occupied, occupied_ms).await;
    let mut helper = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(helper) = HeldHelper::pause(&cluster.node_a, &renderer) {
                break helper;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("origin has an actual original renderer in flight");
    assert_eq!(
        readiness(
            &cluster,
            &a,
            "processed_dv_upgrade",
            "processed_upgrade_slot"
        )
        .await["status"],
        "unmet"
    );

    let prepares = cluster.proxy_b.hits(PREPARE_PATH);
    let adopts = cluster.proxy_b.hits(ADOPT_PATH);
    let relays = cluster.proxy_b.hits(RELAY_PATH);
    let presented = Instant::now();
    first_frame(&cluster, file, &ordinary, ordinary_ms).await;
    tokio::time::sleep(Duration::from_millis(260)).await;
    let mut sequence = 2;
    control(
        &cluster,
        &ordinary,
        &cold_client,
        sequence,
        Some(presented.elapsed().as_millis() as u64),
        Value::Null,
    )
    .await;
    let deadline = Instant::now() + Duration::from_secs(5);
    while cluster.proxy_b.hits(PREPARE_PATH) == prepares {
        assert!(
            Instant::now() < deadline,
            "origin never took typed FEL capacity refusal path{}",
            cluster.logs()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Deferred FEL Register is reachable only from the local typed
    // capacity refusal. Re-read the real slot before resuming its owner.
    assert_eq!(
        readiness(
            &cluster,
            &a,
            "processed_dv_upgrade",
            "processed_upgrade_slot"
        )
        .await["status"],
        "unmet"
    );
    helper.resume();
    let released = cluster
        .client
        .delete(format!(
            "{a}/api/v1/hls/{}",
            occupied["session_id"].as_str().expect("holding session")
        ))
        .bearer_auth(&cluster.token)
        .send()
        .await
        .expect("retire slot holder");
    assert!(released.status().is_success(), "{}", cluster.logs());

    let deadline = Instant::now() + Duration::from_secs(30);
    let action = loop {
        tokio::time::sleep(Duration::from_millis(260)).await;
        sequence += 1;
        let reply = control(
            &cluster,
            &ordinary,
            &cold_client,
            sequence,
            Some(presented.elapsed().as_millis() as u64),
            Value::Null,
        )
        .await;
        if reply["action"]["type"] == "prepare" {
            break reply["action"].clone();
        }
        assert!(Instant::now() < deadline, "{reply}{}", cluster.logs());
    };
    assert!(
        cluster.proxy_b.hits(PREPARE_PATH) >= prepares + 2,
        "real peer Register and Grant"
    );
    assert_eq!(
        cluster.proxy_b.hits(ADOPT_PATH),
        adopts,
        "viewer has not committed"
    );
    assert_eq!(
        status(&cluster, &a, &ordinary).await,
        StatusCode::OK,
        "incumbent stays usable"
    );
    assert_eq!(action["media_origin_ms"], 0);
    assert_eq!(action["delivered_dolby_vision_profile"], 8);
    assert!(action["enhancement"]["active"]
        .as_array()
        .expect("enhancements")
        .iter()
        .any(|item| item == "FEL"));
    decoded_first_picture(&cluster, &action, &ffmpeg, &peer_dump).await;
    assert!(
        cluster.proxy_b.hits(RELAY_PATH) > relays,
        "staged media came through B transport"
    );
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("wall time")
        .as_millis() as u64;
    tokio::time::sleep(Duration::from_millis(260)).await;
    let committed = control(
        &cluster,
        &ordinary,
        &cold_client,
        sequence + 1,
        Some(presented.elapsed().as_millis() as u64),
        json!({"action_id":action["action_id"],"state":"committed",
               "committed_media_origin_ms":action["media_origin_ms"],"first_frame_unix_ms":now_ms}),
    )
    .await;
    assert_eq!(committed["accepted_sequence"], sequence + 1);
    // Announce the decoded successor as presented if the incumbent has
    // a drain window. A route already retired needs no second exchange.
    if status(&cluster, &a, &ordinary).await == StatusCode::OK {
        tokio::time::sleep(Duration::from_millis(260)).await;
        control(
            &cluster, &ordinary, &cold_client, sequence + 2,
            Some(presented.elapsed().as_millis() as u64),
            json!({"action_id":action["action_id"],"state":"switched","first_frame_unix_ms":now_ms}),
        ).await;
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if status(&cluster, &a, &ordinary).await == StatusCode::GONE
            && status(&cluster, &b, &action).await == StatusCode::OK
            && helper.exited()
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "winner or original settlement remained unproved{}",
            cluster.logs()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        cluster.proxy_b.hits(ADOPT_PATH) > adopts,
        "actual viewer commit adopted B"
    );
    let probe = run_tool(
        Command::new(ffprobe)
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "packet=pts_time",
                "-of",
                "csv=p=0",
            ])
            .arg(&peer_dump),
    );
    let timestamps = String::from_utf8(probe.stdout).expect("served packet timestamps");
    let timestamps: Vec<f64> = timestamps
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            line.split(',')
                .next()
                .expect("packet timestamp")
                .parse()
                .expect("numeric packet timestamp")
        })
        .collect();
    let frames = u32::try_from(timestamps.len()).expect("bounded window frame count");
    let first = timestamps
        .into_iter()
        .reduce(f64::min)
        .expect("served picture");
    assert!(first.is_finite() && first >= 0.0);
    let first_tick = (first * 24.0).round() as u32;
    run_tool(
        Command::new("python3")
            .arg(controls().join("check_p81_window.py"))
            .arg(&verifier)
            .arg(&peer_dump)
            .arg(&source)
            .arg(frames.to_string())
            .arg(first_tick.to_string()),
    );

    // The controlled input is an exact 24fps grid. This metadata tells
    // the comparison which bytes were pulled through B's real route.
    std::fs::write(
        peer_window.join("window-000.json"),
        serde_json::to_vec(&json!({
            "numerator":24, "denominator":1, "start_ticks":first_tick, "duration_ticks":frames,
        }))
        .expect("peer window clock"),
    )
    .expect("write peer window clock");
    run_tool(
        Command::new("python3")
            .arg(controls().join("fel_composition_proof.py"))
            .arg(composition)
            .arg(source)
            .arg(peer_window),
    );
}
