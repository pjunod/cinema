use super::*;

const ORDINARY_QUALIFIED: &str =
    "original finite native start and close qualified the trial envelope";
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

fn native_leaf_count(root: &Path) -> usize {
    std::fs::read_dir(root)
        .expect("read the isolated delegated cgroup")
        .map(|entry| entry.expect("inspect cgroup entry"))
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("plurx-"))
        .count()
}

struct Viewer<'a> {
    client: &'a reqwest::Client,
    base: &'a str,
    token: &'a str,
}

impl Viewer<'_> {
    async fn start(&self, file_id: i64, playback_id: &str, node: &Daemon) -> Value {
        let response = self
            .client
            .post(format!("{}/api/v1/files/{file_id}/hls/sessions", self.base))
            .bearer_auth(self.token)
            .json(&json!({"playback_id":playback_id,"height":480}))
            .send()
            .await
            .expect("send public HLS Create");
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

    // Long enough that the ordinary run can be stopped after its first
    // verified segment and the next run can deliver a sustained window.
    write_fixture(
        ffmpeg.to_str().expect("FFmpeg path"),
        &media.join("Singleton VOD Trial (2024).mkv"),
        "testsrc2=size=1920x1080:rate=60:duration=90",
        "sine=frequency=440:sample_rate=48000:duration=90",
        "120",
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
                    "info,plurxd::vodserve=debug,plurx_core::adaptive_resources=debug",
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
