use super::hdhomerun_fixture::{private_ipv4, serve_fixture_device, GUIDE_NUMBER};
use super::*;
use std::sync::atomic::AtomicU64;
use std::time::{SystemTime, UNIX_EPOCH};

const ORDINARY_QUALIFIED: &str =
    "original finite native start and close qualified the trial envelope";
const TRIAL_ENTERED: &str = "original finite trial entered its native producer";
const PROTECTED_WAIT: &str = "protected playback registered behind the original finite trial";
const TRIAL_VERIFIED: &str =
    "original finite trial verified a sustained output window before Winner";

fn required_path(name: &str) -> PathBuf {
    let path = std::env::var_os(name)
        .unwrap_or_else(|| panic!("an explicit fixture path in {name} is required"));
    std::fs::canonicalize(&path)
        .unwrap_or_else(|error| panic!("resolve fixture path {name}={path:?}: {error}"))
}

fn private_noswap_tmpfs() -> PathBuf {
    let root = required_path("PLURX_TEST_FINITE_NOSWAP_TMPFS");
    let mode = std::fs::metadata(&root)
        .expect("inspect tmpfs mountpoint")
        .permissions()
        .mode();
    assert_eq!(mode & 0o077, 0, "fixture tmpfs must be private: {root:?}");
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo").expect("Linux mount table");
    let mountpoint = root
        .to_str()
        .expect("UTF-8 fixture mountpoint")
        .replace('\\', "\\134")
        .replace(' ', "\\040")
        .replace('\t', "\\011")
        .replace('\n', "\\012");
    assert!(
        mountinfo.lines().any(|line| {
            let Some((mount, superblock)) = line.split_once(" - ") else {
                return false;
            };
            let options: Vec<_> = superblock.split_ascii_whitespace().collect();
            mount.split_ascii_whitespace().nth(4) == Some(mountpoint.as_str())
                && matches!(options.as_slice(), ["tmpfs", _, flags]
                    if flags.split(',').any(|flag| flag == "noswap"))
        }),
        "provide the mountpoint of a private noswap tmpfs in PLURX_TEST_FINITE_NOSWAP_TMPFS"
    );
    root
}

fn native_delegation() -> PathBuf {
    // These calls query capabilities only. The real daemon still has to
    // prove delegation and enter its own original contained child.
    let uid = unsafe { libc::geteuid() };
    assert_ne!(uid, 0, "run the native fixture as a non-root user");
    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<libc::c_void>(),
            0usize,
            1u32,
        )
    };
    assert!(abi >= 6, "the native fixture requires Landlock ABI 6");
    let clone3 =
        unsafe { libc::syscall(libc::SYS_clone3, std::ptr::null::<libc::c_void>(), 0usize) };
    assert!(
        clone3 == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINVAL),
        "the native fixture requires an available clone3 syscall"
    );
    let root = required_path("PLURX_TEST_FINITE_NATIVE_ROOT");
    let controllers = std::fs::read_to_string(root.join("cgroup.subtree_control"))
        .expect("supply an isolated, writable cgroup v2 root");
    for controller in ["memory", "pids"] {
        assert!(
            controllers
                .split_ascii_whitespace()
                .any(|value| value == controller),
            "enable {controller} in the fixture delegation"
        );
    }
    for file in ["memory.max", "pids.max"] {
        let value = std::fs::read_to_string(root.join(file))
            .unwrap_or_else(|error| panic!("read the delegated {file}: {error}"));
        assert!(
            value.trim().parse::<u64>().is_ok_and(|limit| limit > 0),
            "the fixture requires an explicit positive delegated {file}"
        );
    }
    root
}

fn native_leaves(root: &Path) -> impl Iterator<Item = PathBuf> {
    std::fs::read_dir(root)
        .expect("read the isolated delegated cgroup")
        .map(|entry| entry.expect("inspect cgroup entry"))
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("plurx-"))
        .map(|entry| entry.path())
}

fn native_leaf_count(root: &Path) -> usize {
    native_leaves(root).count()
}

fn populated(leaf: &Path) -> bool {
    std::fs::read_to_string(leaf.join("cgroup.events"))
        .is_ok_and(|events| events.lines().any(|line| line == "populated 1"))
}

struct Viewer<'a> {
    client: &'a reqwest::Client,
    base: &'a str,
    token: &'a str,
}

impl Viewer<'_> {
    async fn request_start(&self, file_id: i64, playback_id: &str) -> reqwest::Response {
        self.client
            .post(format!("{}/api/v1/files/{file_id}/hls/sessions", self.base))
            .bearer_auth(self.token)
            .json(&json!({"playback_id":playback_id,"height":480}))
            .send()
            .await
            .expect("send public HLS Create")
    }

    async fn start(&self, file_id: i64, playback_id: &str, node: &Daemon) -> Value {
        let response = self.request_start(file_id, playback_id).await;
        let status = response.status();
        let body = response.text().await.expect("read public Create response");
        assert_eq!(
            status,
            StatusCode::OK,
            "{playback_id} Create: {body}\n{}",
            node.diagnostics()
        );
        let start: Value = serde_json::from_str(&body).expect("public Create JSON");
        assert_eq!(
            start["height"], 480,
            "both runs must use the same AVC recipe"
        );
        start
    }

    async fn verified_progress(&self, started: &Value, node: &mut Daemon) {
        let id = started["session_id"].as_str().expect("session capability");
        let path = format!("/api/v1/hls/{id}/status");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            node.assert_running("waiting for positive original producer output");
            let status = get_json(self.client, self.base, self.token, &path).await;
            let service = &status["resource_service"];
            if service["phase"] == "producing"
                && service["unit"] == "media_milliseconds"
                && service["completed"]
                    .as_u64()
                    .is_some_and(|completed| completed > 0)
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "original producer never published a verified segment: {status}\n{}",
                node.diagnostics()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn play(&self, started: &Value) {
        let playlist = started["playlist_url"].as_str().expect("playlist URL");
        let root = reqwest::Url::parse(self.base).expect("local daemon URL");
        let playlist = root.join(playlist).expect("resolve playlist URL");
        let response = self
            .client
            .get(playlist.clone())
            .bearer_auth(self.token)
            .send()
            .await
            .expect("fetch the public playlist")
            .error_for_status()
            .expect("the playlist was published");
        let text = response.text().await.expect("playlist body");
        assert!(
            text.starts_with("#EXTM3U"),
            "expected a media playlist: {text}"
        );
        let segment = text
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with('#'))
            .expect("at least one published segment");
        let response = self
            .client
            .get(playlist.join(segment).expect("segment URL"))
            .bearer_auth(self.token)
            .send()
            .await
            .expect("fetch a listed segment")
            .error_for_status()
            .expect("the listed segment was served");
        assert!(
            response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .is_some_and(|value| {
                    value.as_bytes().starts_with(b"video/")
                        || value.as_bytes() == b"application/mp4"
                }),
            "the listed segment must be media"
        );
        assert!(!response.bytes().await.expect("segment body").is_empty());
    }

    async fn settings(&self, desired: Value) {
        let response = self
            .client
            .put(format!("{}/api/v1/settings", self.base))
            .bearer_auth(self.token)
            .json(&desired)
            .send()
            .await
            .expect("public Settings write");
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "fixture Settings write: {}",
            response.text().await.expect("Settings response"),
        );
    }

    async fn stop(&self, started: &Value, node: &Daemon) {
        let id = started["session_id"].as_str().expect("session capability");
        let response = self
            .client
            .delete(format!("{}/api/v1/activity/sessions/{id}", self.base))
            .bearer_auth(self.token)
            .send()
            .await
            .expect("stop the original HLS route through Activity");
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "release refused: {}\n{}",
            response.text().await.expect("release response"),
            node.diagnostics()
        );
    }
}

async fn wait_original_removal(
    node: &mut Daemon,
    delegated: &Path,
    output: &Path,
    qualified: bool,
) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        node.assert_running("waiting for original finite removal");
        if native_leaf_count(delegated) == 0
            && !output.exists()
            && (!qualified || node.diagnostics().contains(ORDINARY_QUALIFIED))
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "original close or required qualification remains unverified at {delegated:?} / {output:?}\n{}",
            node.diagnostics()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires private noswap tmpfs, delegated cgroup v2, non-root clone3 and Landlock ABI6, and native FFmpeg/FFprobe"]
async fn activated_hiqlite_singleton_qualifies_and_adopts_a_real_native_vod_trial() {
    run_singleton_trial(false).await;
}

/// Uses PLURX_TEST_FINITE_NOSWAP_TMPFS, PLURX_TEST_FINITE_NATIVE_ROOT,
/// PLURX_FFMPEG and PLURX_FFPROBE just like the original singleton trial.
/// The test user also needs exclusive fixture ports 80 and 5004 on an
/// isolated private IPv4 network.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the native trial prerequisites and an isolated private IPv4 with non-root access to ports 80 and 5004"]
async fn activated_hiqlite_singleton_closes_native_trial_before_dvr_capture_opens() {
    run_singleton_trial(true).await;
}

async fn run_singleton_trial(with_capture: bool) {
    static DELEGATION: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _owned_fixture_slot = DELEGATION.lock().await;
    let ffmpeg = required_path("PLURX_FFMPEG");
    let ffprobe = required_path("PLURX_FFPROBE");
    let delegated = native_delegation();
    assert_eq!(
        native_leaf_count(&delegated),
        0,
        "fixture delegation must be unused"
    );
    let ram = tempfile::tempdir_in(private_noswap_tmpfs()).expect("isolated tmpfs fixture");
    let root = canonical_tempdir();
    let data = root.path().join("data");
    let media = ram.path().join("media");
    let cache = ram.path().join("cache");
    std::fs::create_dir(&data).expect("singleton data directory");
    std::fs::create_dir(&media).expect("singleton media directory");
    drop(SqliteStore::open(&data.join("plurx.db")).expect("legacy activation source"));

    // Leave enough media to stop the original running producer after its
    // first verified segment. A 24 fps source keeps the test focused on
    // the supported startup envelope rather than 60 fps encode throughput.
    write_fixture(
        ffmpeg.to_str().expect("FFmpeg path"),
        &media.join("Singleton VOD Trial (2024).mkv"),
        "testsrc=size=1280x720:rate=24:duration=90",
        "sine=frequency=440:sample_rate=48000:duration=90",
        "48",
    );
    let mut ports = std::array::from_fn::<_, 3, _>(|_| ReservedPort::new());
    let [http, raft, api] = std::array::from_fn(|index| ports[index].port);
    let base = format!("http://127.0.0.1:{http}");
    let config = root.path().join("plurx.toml");
    let configuration = node_config(http, &data, raft, api, http, None).replacen(
        "[cluster]\n",
        &format!("cache_dir = \"{}\"\n[cluster]\n", toml_path(&cache)),
        1,
    );
    std::fs::write(&config, configuration).expect("real singleton daemon config");
    for port in &mut ports {
        port.release();
    }
    let mut node = Daemon::spawn_configured(
        &config,
        root.path().join("plurxd.log"),
        "native-singleton",
        |command| {
            command
                .env("PLURX_NATIVE_CGROUP_ROOT", &delegated)
                .env("PLURX_FFMPEG", &ffmpeg)
                .env("PLURX_FFPROBE", &ffprobe)
                .env(
                    "PLURX_LOG",
                    "info,plurxd::vodserve=debug,plurxd::admission=debug,plurx_core::adaptive_resources=debug",
                );
        },
    );
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(30))
        .build()
        .expect("fixture HTTP client");
    wait_ready(&client, &mut node, http).await;
    assert!(
        data.join("hiqlite/activation.json").is_file(),
        "Hiqlite must activate"
    );
    let setup = client
        .post(format!("{base}/api/v1/setup"))
        .json(&json!({"username":"owner","password":"adaptive-singleton-password"}))
        .send()
        .await
        .expect("public singleton setup")
        .error_for_status()
        .expect("setup succeeded");
    let setup: Value = setup.json().await.expect("setup token");
    let token = setup["token"].as_str().expect("owner token");
    let roster = get_json(&client, &base, token, "/api/v1/cluster/nodes").await;
    assert_eq!(roster["replication"]["backend"], "replicated");
    let members = roster["nodes"].as_array().expect("committed roster");
    assert_eq!(members.len(), 1, "exactly one member and no learners");
    assert_eq!(members[0]["node_id"], roster["local_node_id"]);
    assert_eq!(members[0]["is_voter"], true);
    let file_id = scan_fixture(&client, &mut node, &base, token, &media, None).await;
    let viewer = Viewer {
        client: &client,
        base: &base,
        token,
    };
    let capture = if with_capture {
        Some(CaptureFixture::start(&viewer, &ffmpeg, root.path()).await)
    } else {
        None
    };
    let renditions = cache.join("renditions");

    let ordinary = viewer.start(file_id, "ordinary-qualification", &node).await;
    viewer.verified_progress(&ordinary, &mut node).await;
    viewer.play(&ordinary).await;
    assert_eq!(
        native_leaf_count(&delegated),
        1,
        "ordinary qualification needs its original running native owner"
    );
    assert!(
        !node.diagnostics().contains(TRIAL_VERIFIED),
        "cold entry must use ordinary admission"
    );
    let output = std::fs::read_dir(&renditions)
        .expect("finite rendition root")
        .map(|entry| entry.expect("rendition entry"))
        .find(|entry| entry.file_type().expect("rendition entry type").is_dir())
        .expect("original ordinary rendition")
        .path();
    viewer.stop(&ordinary, &node).await;
    wait_original_removal(&mut node, &delegated, &output, true).await;

    if let Some(capture) = capture {
        capture
            .preempt_native_trial(&viewer, &mut node, file_id, &delegated, &output)
            .await;
        return;
    }

    // Same exact source and byte recipe, but a new viewer attempt. A
    // surviving retained output or a normal fallback cannot emit this
    // milestone, which is before the durable Winner may publish.
    let trial = viewer.start(file_id, "qualified-native-trial", &node).await;
    assert_ne!(trial["session_id"], ordinary["session_id"]);
    assert_eq!(
        node.diagnostics().matches(TRIAL_VERIFIED).count(),
        1,
        "second Create never admitted a qualified native trial\n{}",
        node.diagnostics()
    );
    viewer.play(&trial).await;
    viewer.stop(&trial, &node).await;
    wait_original_removal(&mut node, &delegated, &output, false).await;
}

struct TrialPaths {
    leaf: PathBuf,
    output: PathBuf,
}

struct CaptureFixture {
    channel_id: String,
    original: Arc<Mutex<Option<TrialPaths>>>,
    opens: Arc<AtomicU64>,
    early_open: Arc<AtomicBool>,
}

impl CaptureFixture {
    async fn start(viewer: &Viewer<'_>, ffmpeg: &Path, root: &Path) -> Self {
        let address = private_ipv4();
        let original = Arc::new(Mutex::new(None::<TrialPaths>));
        let opens = Arc::new(AtomicU64::new(0));
        let early_open = Arc::new(AtomicBool::new(false));
        let observe_open = {
            let original = Arc::clone(&original);
            let early_open = Arc::clone(&early_open);
            Arc::new(move || {
                let original = original.lock().expect("current original paths");
                // This is the actual tuner GET. Require positive absence of
                // both original paths, including after a failed filesystem read.
                if original.as_ref().is_none_or(|paths| {
                    !matches!(paths.leaf.try_exists(), Ok(false))
                        || !matches!(paths.output.try_exists(), Ok(false))
                }) {
                    early_open.store(true, Ordering::Release);
                }
            })
        };
        serve_fixture_device(
            address.clone(),
            ffmpeg.to_owned(),
            Arc::clone(&opens),
            Some(observe_open),
        )
        .await;
        let current = get_json(viewer.client, viewer.base, viewer.token, "/api/v1/settings").await;
        viewer
            .settings(json!({
                "live_tv_config_generation": current["live_tv_config_generation"],
                "live_tv_device_ipv4": address,
                "live_tv_enabled": true
            }))
            .await;
        let recordings = root.join("recordings");
        std::fs::create_dir(&recordings).expect("private DVR recordings directory");
        viewer
            .settings(json!({
                "dvr_enabled": false,
                "dvr_root": recordings,
                "dvr_free_floor_gb": 0,
                "dvr_pad_end_s": 0
            }))
            .await;
        // Keep DVR off until the original trial is observably active. Its
        // enabled scheduler holds the protected admission lane even when
        // there are no due rows. Discovery and lineup only are safe now.
        let lineup = get_json(
            viewer.client,
            viewer.base,
            viewer.token,
            "/api/v1/live-tv/channels",
        )
        .await;
        let channel_id = lineup["channels"]
            .as_array()
            .expect("channels")
            .iter()
            .find(|row| row["guide_number"] == GUIDE_NUMBER)
            .expect("fixture tuner channel")["id"]
            .as_str()
            .expect("fixture channel id")
            .to_owned();
        assert_eq!(
            opens.load(Ordering::Acquire),
            0,
            "lineup is not a tuner open"
        );
        Self {
            channel_id,
            original,
            opens,
            early_open,
        }
    }

    async fn preempt_native_trial(
        self,
        viewer: &Viewer<'_>,
        node: &mut Daemon,
        file_id: i64,
        delegated: &Path,
        output: &Path,
    ) {
        let request = viewer.request_start(file_id, "dvr-preempts-trial");
        tokio::pin!(request);
        let capture_intent = async {
            let deadline = Instant::now() + Duration::from_secs(8);
            let leaf = loop {
                let entered = node.diagnostics().contains(TRIAL_ENTERED);
                if let Some(leaf) = native_leaves(delegated).find(|leaf| entered && populated(leaf))
                {
                    break leaf;
                }
                assert!(
                    Instant::now() < deadline,
                    "no active original native trial\n{}",
                    node.diagnostics()
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            };
            assert!(
                output.is_dir(),
                "the new trial must own the same rendition key"
            );
            *self.original.lock().expect("record the original paths") = Some(TrialPaths {
                leaf: leaf.clone(),
                output: output.to_owned(),
            });
            assert_eq!(self.opens.load(Ordering::Acquire), 0);
            // A real replicated write completes while the exact original
            // cgroup is still populated. No capture intent exists yet.
            viewer.settings(json!({"dvr_pad_end_s": 3})).await;
            assert!(
                populated(&leaf),
                "the Settings write must finish while the trial is active"
            );
            viewer.settings(json!({"dvr_enabled": true})).await;
            let now = i64::try_from(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("wall clock")
                    .as_secs(),
            )
            .expect("unix seconds");
            let response = viewer
                .client
                .post(format!("{}/api/v1/dvr/recordings", viewer.base))
                .bearer_auth(viewer.token)
                .json(&json!({
                    "channel_id": self.channel_id,
                    "capture_start": now,
                    "capture_end": now + 180,
                    "title": "Native trial protected capture fixture"
                }))
                .send()
                .await
                .expect("schedule actual protected manual capture");
            let status = response.status();
            let recording: Value = response.json().await.expect("recording response");
            assert_eq!(
                status,
                StatusCode::CREATED,
                "{recording}\n{}",
                node.diagnostics()
            );
            (leaf, recording)
        };
        let (response, (leaf, recording)) = tokio::join!(&mut request, capture_intent);
        assert_eq!(
            response.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "the preempted Create did not refuse: {}\n{}",
            response.text().await.expect("Create refusal"),
            node.diagnostics()
        );
        let diagnostics = node.diagnostics();
        assert!(
            diagnostics.contains(PROTECTED_WAIT),
            "protected demand did not meet the original trial\n{diagnostics}"
        );
        assert!(
            !diagnostics.contains(TRIAL_VERIFIED),
            "the preempted trial reached Winner evidence\n{diagnostics}"
        );
        wait_original_removal(node, delegated, output, false).await;
        assert!(!leaf.try_exists().expect("observe original cgroup removal"));

        let id = recording["id"].as_str().expect("original recording id");
        let route = format!("/api/v1/dvr/recordings/{id}");
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            node.assert_running("waiting for the original DVR scheduler capture");
            let row = get_json(viewer.client, viewer.base, viewer.token, &route).await;
            if row["state"] == "recording" && row["bytes"].as_i64().is_some_and(|bytes| bytes > 0) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "no positive bytes from the recording's real sink: {row}\n{}",
                node.diagnostics()
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        assert_eq!(
            self.opens.load(Ordering::Acquire),
            1,
            "one physical tuner request"
        );
        assert!(
            !self.early_open.load(Ordering::Acquire),
            "tuner opened before exact original cgroup removal"
        );

        let response = viewer
            .client
            .delete(format!("{}{route}", viewer.base))
            .bearer_auth(viewer.token)
            .send()
            .await
            .expect("request original DVR stop");
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let row = get_json(viewer.client, viewer.base, viewer.token, &route).await;
            if matches!(row["state"].as_str(), Some("done" | "partial" | "failed")) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "capture did not settle: {row}\n{}",
                node.diagnostics()
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}
