// One explicitly admitted real public HTTP control; never regenerate its input.
// The external runtime owner supplies process/memory/disk limits and a watchdog.
use super::*;

const S10_SOURCE_SHA: &str = "4a038dfd79aefcee9efde81e528c6a896cb55ac99a871f9d5e08f7054ff4716a";
const S10_RESPONSE_CAP: usize = 8 * 1024 * 1024;
const S10_MEDIA_CAP: u64 = 16 * 1024 * 1024;

struct S10FailureGuards {
    state: Option<AppState>,
    directory: Option<tempfile::TempDir>,
    clean: bool,
}

impl Drop for S10FailureGuards {
    fn drop(&mut self) {
        if !self.clean {
            // Also covers unexpected panic: never remove files under an uncertain writer.
            // The external owner ends/reaps this exact session before reclaiming its root.
            if let Some(state) = self.state.take() {
                std::mem::forget(state);
            }
            if let Some(directory) = self.directory.take() {
                std::mem::forget(directory);
            }
        }
    }
}

fn s10_require(ok: bool, message: &str) -> Result<(), String> {
    if ok {
        Ok(())
    } else {
        Err(message.to_owned())
    }
}

fn s10_hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

fn s10_read_bounded(path: &std::path::Path, cap: u64) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|_| "bounded file open")?;
    let mut bytes = Vec::new();
    file.take(cap + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "bounded file read")?;
    s10_require(bytes.len() as u64 <= cap, "file byte cap")?;
    Ok(bytes)
}

async fn s10_response(mut response: reqwest::Response) -> Result<(u16, Vec<u8>), String> {
    let status = response.status().as_u16();
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "HTTP body failed")? {
        s10_require(
            bytes.len().saturating_add(chunk.len()) <= S10_RESPONSE_CAP,
            "HTTP body cap",
        )?;
        bytes.extend_from_slice(&chunk);
    }
    Ok((status, bytes))
}

async fn s10_get(client: &reqwest::Client, root: &str, path: &str) -> Result<Vec<u8>, String> {
    let response = client
        .get(format!("{root}{path}"))
        .send()
        .await
        .map_err(|_| "HTTP GET failed")?;
    let (status, bytes) = s10_response(response).await?;
    s10_require(status == 200, "HTTP GET non-200")?;
    Ok(bytes)
}

// Exact fixed-decimal playlist duration, independent of the production reducer.
fn s10_duration(text: &str) -> Result<u64, String> {
    let (seconds, fraction) = text.split_once('.').ok_or("duration lacks decimal")?;
    s10_require(
        fraction.len() == 6 && fraction.bytes().all(|b| b.is_ascii_digit()),
        "duration precision",
    )?;
    let seconds = seconds.parse::<u64>().map_err(|_| "duration seconds")?;
    let fraction = fraction.parse::<u64>().map_err(|_| "duration fraction")?;
    seconds
        .checked_mul(1_000_000)
        .and_then(|n| n.checked_add(fraction))
        .filter(|n| *n > 0)
        .ok_or_else(|| "duration overflow/zero".to_owned())
}

fn s10_playlist(text: &str) -> Result<(u64, Vec<(String, u64)>), String> {
    s10_require(
        text.contains("#EXT-X-ENDLIST") && text.contains("#EXT-X-PLAYLIST-TYPE:VOD"),
        "not immutable VOD",
    )?;
    let mut target = None;
    let mut duration = None;
    let mut members = Vec::new();
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("#EXT-X-TARGETDURATION:") {
            target = value
                .parse::<u64>()
                .ok()
                .and_then(|n| n.checked_mul(1_000_000));
        } else if let Some(value) = line.strip_prefix("#EXTINF:") {
            s10_require(duration.is_none(), "duplicate pending EXTINF")?;
            duration = Some(s10_duration(value.trim_end_matches(','))?);
        } else if !line.is_empty() && !line.starts_with('#') {
            // Fetch only capability-local canonical members, never an arbitrary URI.
            s10_require(
                line.starts_with("seg")
                    && line.ends_with(".m4s")
                    && !line.contains('/')
                    && !line.contains('?'),
                "noncanonical media URI",
            )?;
            members.push((
                line.to_owned(),
                duration.take().ok_or("media lacks EXTINF")?,
            ));
            s10_require(members.len() <= 128, "member cap")?;
        }
    }
    s10_require(
        duration.is_none() && !members.is_empty(),
        "incomplete playlist parse",
    )?;
    Ok((
        target.filter(|n| *n > 0).ok_or("missing target duration")?,
        members,
    ))
}

fn s10_rates(target: u64, members: &[(u64, u64)]) -> Result<(u64, u64), String> {
    fn rate(bytes: u128, duration: u128) -> Result<u64, String> {
        let numerator = bytes.checked_mul(8_000_000).ok_or("rate overflow")?;
        let value = numerator
            .checked_add(duration.checked_sub(1).ok_or("zero duration")?)
            .ok_or("rounding overflow")?
            / duration;
        u64::try_from(value).map_err(|_| "rate out of range".to_owned())
    }
    let bytes: u128 = members.iter().map(|(bytes, _)| u128::from(*bytes)).sum();
    let duration: u128 = members
        .iter()
        .map(|(_, duration)| u128::from(*duration))
        .sum();
    let average = rate(bytes, duration)?;
    let mut peak = None;
    for start in 0..members.len() {
        let (mut bytes, mut duration) = (0_u128, 0_u128);
        for &(next_bytes, next_duration) in &members[start..] {
            bytes += u128::from(next_bytes);
            duration += u128::from(next_duration);
            if duration * 2 > u128::from(target) * 3 {
                break;
            }
            if duration * 2 >= u128::from(target) {
                let value = rate(bytes, duration)?;
                peak = Some(peak.map_or(value, |previous: u64| previous.max(value)));
            }
        }
    }
    Ok((average, peak.ok_or("no eligible RFC peak window")?))
}

fn s10_master_rates(text: &str) -> Result<(u64, u64), String> {
    let mut variants = text
        .lines()
        .filter_map(|line| line.strip_prefix("#EXT-X-STREAM-INF:"));
    let variant = variants.next().ok_or("no master variant")?;
    s10_require(variants.next().is_none(), "unexpected multiple variants")?;
    let attribute = |name: &str| -> Result<u64, String> {
        variant
            .split(',')
            .find_map(|part| part.strip_prefix(name))
            .ok_or_else(|| format!("missing {name}"))?
            .parse()
            .map_err(|_| "master rate parse".to_owned())
    };
    Ok((attribute("AVERAGE-BANDWIDTH=")?, attribute("BANDWIDTH=")?))
}

async fn s10_context(
    state: &AppState,
    session: &str,
) -> Result<crate::transcode::HlsContext, String> {
    match state
        .transcode
        .hls_presentation_before(session, Instant::now() + Duration::from_secs(5))
        .await
    {
        crate::transcode::HlsPresentationResolution::Ready(context, _, _) => Ok(context),
        _ => Err("public session presentation unavailable".to_owned()),
    }
}

async fn s10_fetch_mux(
    client: &reqwest::Client,
    root: &str,
    session: &str,
    members: &[(String, u64)],
    directory: &std::path::Path,
) -> Result<(Vec<(u64, u64)>, Vec<Value>, std::path::PathBuf), String> {
    use sha2::{Digest, Sha256};
    std::fs::create_dir(directory).map_err(|_| "owned fetch directory")?;
    let init = s10_get(client, root, &format!("/api/v1/hls/{session}/init.mp4")).await?;
    let mux_path = directory.join("client-received.mp4");
    let mut mux = std::fs::File::create(&mux_path).map_err(|_| "owned mux file")?;
    mux.write_all(&init).map_err(|_| "write received init")?;
    std::fs::write(directory.join("init.mp4"), &init).map_err(|_| "record init")?;
    let mut total = init.len() as u64;
    let mut rates = Vec::new();
    let mut receipt =
        vec![json!({"name":"init.mp4", "bytes":init.len(), "sha256":s10_hash(&init)})];
    for (name, duration) in members {
        let response = client
            .get(format!("{root}/api/v1/hls/{session}/{name}"))
            .send()
            .await
            .map_err(|_| "media GET failed")?;
        s10_require(response.status().as_u16() == 200, "media GET non-200")?;
        let mut response = response;
        let mut member =
            std::fs::File::create(directory.join(name)).map_err(|_| "owned member file")?;
        let mut bytes = 0_u64;
        let mut digest = Sha256::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| "media body failed")? {
            bytes = bytes
                .checked_add(chunk.len() as u64)
                .ok_or("media length overflow")?;
            total = total
                .checked_add(chunk.len() as u64)
                .ok_or("total length overflow")?;
            s10_require(
                bytes <= S10_RESPONSE_CAP as u64 && total <= S10_MEDIA_CAP,
                "media byte cap",
            )?;
            digest.update(&chunk);
            member
                .write_all(&chunk)
                .map_err(|_| "record received media")?;
            mux.write_all(&chunk).map_err(|_| "record received mux")?;
        }
        s10_require(bytes > 0, "empty media body")?;
        rates.push((bytes, *duration));
        receipt.push(json!({"name":name,"bytes":bytes,"duration_micros":duration,"sha256":hex::encode(digest.finalize())}));
    }
    Ok((rates, receipt, mux_path))
}

async fn s10_control(
    state: &AppState,
    client: &reqwest::Client,
    root: &str,
    base: &std::path::Path,
    sessions: &mut Vec<String>,
    admin: &mut Option<String>,
) -> Result<Value, String> {
    use plurx_core::domain::{ItemKind, LibraryKind, NewItem, NewLibrary};
    let frozen = std::env::var_os("PLURX_S10_PUBLIC_COPY_SOURCE")
        .ok_or("frozen source admission missing")?;
    let frozen = std::path::PathBuf::from(frozen);
    let metadata = std::fs::metadata(&frozen).map_err(|_| "frozen source metadata")?;
    s10_require(
        metadata.is_file() && metadata.len() == 2_592_240,
        "frozen source size/type",
    )?;
    let bytes = s10_read_bounded(&frozen, 2_592_240)?;
    s10_require(s10_hash(&bytes) == S10_SOURCE_SHA, "frozen source hash")?;
    let path = base.join("source.mkv");
    std::fs::write(&path, bytes).map_err(|_| "copy frozen source into owned directory")?;
    let held = std::fs::File::open(&path).map_err(|_| "hold exact source")?;
    let raw = crate::ffmpeg::held_source_probe_json(
        &held,
        crate::process_control::ChildWork::background("S10 public Copy source"),
    )
    .await
    .map_err(|_| "actual held-source probe failed")?;
    let raw: Value = serde_json::from_str(&raw).map_err(|_| "source probe JSON")?;
    let probe = plurx_core::scan::probe::parse_probe_json(&raw);
    s10_require(
        probe.video_codec.as_deref() == Some("h264") && probe.audio_streams.len() == 1,
        "actual source track inventory",
    )?;
    let audio = &probe.audio_streams[0];
    s10_require(
        audio.codec == "aac" && audio.channels == Some(2) && audio.sample_rate == Some(48_000),
        "actual source audio",
    )?;
    let library = state
        .store
        .create_library(&NewLibrary {
            name: "S10 public wire".into(),
            kind: LibraryKind::Movies,
            paths: vec![base.to_owned()],
            anime: false,
        })
        .await
        .map_err(|_| "create owned library")?;
    let item = state
        .store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "S10 public wire".into(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .map_err(|_| "create owned item")?;
    let metadata = held.metadata().map_err(|_| "held metadata")?;
    let mtime = metadata
        .modified()
        .map_err(|_| "held mtime")?
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "mtime before epoch")?
        .as_secs() as i64;
    let file_id = state
        .store
        .upsert_file(
            item,
            &path.to_string_lossy(),
            metadata.len() as i64,
            mtime,
            &probe,
        )
        .await
        .map_err(|_| "store actual probe")?;
    let file = state
        .store
        .get_file(file_id)
        .await
        .map_err(|_| "read actual file")?
        .ok_or("actual file absent")?;
    let source_fence = crate::fragment_index_cluster::open_source_fence(&file, None)
        .await
        .map_err(|_| "exact source fence")?;
    let video = plurx_core::transcode::CopyVideoOptions::from_probe(
        &file,
        probe.raw_json.as_deref(),
        false,
        false,
    );
    let crate::fragindex::IndexOutcome::Built(index) = crate::fragindex::build(
        &file,
        video,
        state.transcode.runtime_cache_dir(),
        Duration::from_secs(20),
    )
    .await
    else {
        return Err("actual complete fragment index failed".into());
    };
    state
        .store
        .put_fragment_index(file_id, &index)
        .await
        .map_err(|_| "store actual index")?;
    state
        .store
        .put_setting(plurx_core::store::keys::VOD_LIVE_RECOVERY, "0")
        .await
        .map_err(|_| "pin VOD")?;
    state
        .store
        .put_setting(plurx_core::store::keys::CACHE_MAX_GB, "1")
        .await
        .map_err(|_| "nonzero retention budget")?;
    let create_path = format!("{root}/api/v1/files/{file_id}/hls/sessions");
    let response = client
        .post(&create_path)
        .json(&json!({"copy":true,"playback_id":"unauthorized-s10"}))
        .send()
        .await
        .map_err(|_| "refusal request failed")?;
    let (status, _) = s10_response(response).await?;
    s10_require(
        status == 401 && state.transcode.active_sessions().await == 0,
        "unauthorized Create admitted work",
    )?;
    let jobs = state
        .store
        .list_jobs(plurx_core::store::background_jobs::JobQuery {
            node_id: None,
            state: None,
            kind: None,
            after_id: None,
            limit: 1,
        })
        .await
        .map_err(|_| "refusal job observation")?;
    s10_require(jobs.jobs.is_empty(), "unauthorized Create queued work")?;
    let response = client
        .post(format!("{root}/api/v1/setup"))
        .json(&json!({"username":"s10-owned","password":"owned-control-only-password"}))
        .send()
        .await
        .map_err(|_| "public setup failed")?;
    let (status, bytes) = s10_response(response).await?;
    s10_require(status == 200, "public setup non-200")?;
    let setup: Value = serde_json::from_slice(&bytes).map_err(|_| "setup JSON")?;
    let token = setup["token"]
        .as_str()
        .ok_or("setup token missing")?
        .to_owned();
    *admin = Some(token.clone());
    let mut masters = Vec::new();
    let mut mux_receipts = Vec::new();
    let mut measured = None;
    let mut expected_artifact = None;
    for number in 0..2 {
        let response = client.post(&create_path).bearer_auth(&token).json(&json!({
            "copy":true,"aac":false,"audio":audio.index,"audio_offset_ms":0,"start":0,
            "playback_id":format!("s10-public-{number}"),"request_id":format!("s10-request-{number}")
        })).send().await.map_err(|_| "public Create failed")?;
        let (status, bytes) = s10_response(response).await?;
        s10_require(status == 200, "public Create non-200")?;
        let created: Value = serde_json::from_slice(&bytes).map_err(|_| "Create JSON")?;
        s10_require(created["vod"] == true, "public Create not VOD")?;
        let session = created["session_id"]
            .as_str()
            .ok_or("Create session missing")?
            .to_owned();
        s10_require(
            !sessions.contains(&session),
            "NEW attachment reused incumbent",
        )?;
        sessions.push(session.clone());
        let context = s10_context(state, &session).await?;
        s10_require(
            context.file_id == file_id && context.media_origin_seconds == 0.0,
            "presentation source/origin",
        )?;
        let facts =
            serde_json::to_value(&context.codec_facts).map_err(|_| "frozen facts serialization")?;
        if number == 0 {
            s10_require(
                context.bandwidth.is_none() && facts["retained_output"].is_null(),
                "cold capture not unknown",
            )?;
        } else {
            measured = context.bandwidth;
            s10_require(
                measured.is_some() && !facts["retained_output"].is_null(),
                "NEW attachment missing actual retained proof",
            )?;
            s10_require(
                facts["retained_output"]["artifact_id"].as_str() == expected_artifact.as_deref(),
                "NEW attachment did not capture the exact completed artifact",
            )?;
        }
        let master = s10_get(client, root, &format!("/api/v1/hls/{session}/master.m3u8")).await?;
        let playlist = s10_get(client, root, &format!("/api/v1/hls/{session}/index.m3u8")).await?;
        let (target, members) =
            s10_playlist(std::str::from_utf8(&playlist).map_err(|_| "playlist UTF8")?)?;
        s10_require(
            std::str::from_utf8(&playlist)
                .map_err(|_| "playlist UTF8")?
                .contains("URI=\"init.mp4\""),
            "actual init URI",
        )?;
        let (sizes, receipt, mux_path) = tokio::time::timeout(
            Duration::from_secs(40),
            s10_fetch_mux(
                client,
                root,
                &session,
                &members,
                &base.join(format!("wire-{number}")),
            ),
        )
        .await
        .map_err(|_| "aggregate fetch deadline")??;
        let rates = s10_rates(target, &sizes)?;
        if number == 1 {
            let budget = measured.as_ref().ok_or("measured budget absent")?;
            s10_require(
                (budget.average_bps, budget.peak_bps) == rates,
                "independent rates differ from captured facts",
            )?;
            s10_require(
                s10_master_rates(std::str::from_utf8(&master).map_err(|_| "master UTF8")?)?
                    == rates,
                "wire master rates differ",
            )?;
        }
        let output = std::fs::File::open(mux_path).map_err(|_| "hold received mux")?;
        let raw_output = crate::ffmpeg::held_source_probe_json(
            &output,
            crate::process_control::ChildWork::background("S10 client-received mux"),
        )
        .await
        .map_err(|_| "actual fetched mux probe failed")?;
        let parsed: Value = serde_json::from_str(&raw_output).map_err(|_| "fetched mux JSON")?;
        let output_probe = plurx_core::scan::probe::parse_probe_json(&parsed);
        s10_require(
            output_probe.video_codec.as_deref() == Some("h264")
                && output_probe.audio_streams.len() == 1,
            "wire output track inventory",
        )?;
        let output_audio = &output_probe.audio_streams[0];
        s10_require(
            output_audio.codec == audio.codec
                && output_audio.channels == audio.channels
                && output_audio.sample_rate == audio.sample_rate,
            "wire output/source audio mapping",
        )?;
        masters.push(master);
        mux_receipts.push(receipt);
        if number == 0 {
            // A real completion creates this manifest; reading it never grants authority.
            let retained = base.join("renditions/.retained");
            let manifest = tokio::time::timeout(Duration::from_secs(20), async {
                loop {
                    if let Ok(entries) = std::fs::read_dir(&retained) {
                        for entry in entries.take(65).flatten() {
                            let path = entry.path().join("complete-v1.json");
                            if path.is_file() {
                                let metadata =
                                    std::fs::metadata(&path).map_err(|_| "manifest metadata")?;
                                s10_require(metadata.len() <= 4 * 1024 * 1024, "manifest cap")?;
                                let bytes = s10_read_bounded(&path, 4 * 1024 * 1024)?;
                                let manifest: Value =
                                    serde_json::from_slice(&bytes).map_err(|_| "manifest JSON")?;
                                return Ok::<Value, String>(manifest);
                            }
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await
            .map_err(|_| "actual retained completion deadline")??;
            s10_require(
                manifest["logical"]["file_id"] == file_id
                    && manifest["logical"]["audio_index"] == audio.index
                    && manifest["logical"]["audio_offset_ms"] == 0
                    && manifest["logical"]["kind"]["kind"] == "copy"
                    && manifest["logical"]["kind"]["aac"] == false
                    && manifest["origin"]["source_version"] == source_fence.object_version(),
                "retained exact source/audio identity",
            )?;
            expected_artifact = Some(
                manifest["id"]
                    .as_str()
                    .ok_or("actual artifact ID missing")?
                    .to_owned(),
            );
            let observed = manifest["members"]
                .as_array()
                .ok_or("retained members missing")?;
            s10_require(
                observed.len() == sizes.len(),
                "retained/fetched member count",
            )?;
            for (index, member) in observed.iter().enumerate() {
                let digest: Vec<u8> = serde_json::from_value(member["digest"].clone())
                    .map_err(|_| "retained digest")?;
                s10_require(
                    member["bytes"] == sizes[index].0
                        && hex::encode(digest)
                            == mux_receipts[0][index + 1]["sha256"]
                                .as_str()
                                .ok_or("wire digest missing")?,
                    "retained/fetched member bytes",
                )?;
            }
            s10_require(
                manifest["origin"]["served_init"] == mux_receipts[0][0]["sha256"],
                "retained/fetched init digest",
            )?;
        }
    }
    s10_require(
        mux_receipts[0] == mux_receipts[1],
        "retained NEW wire bytes differ from original complete mux",
    )?;
    for (index, session) in sessions.iter().enumerate() {
        s10_require(
            s10_get(client, root, &format!("/api/v1/hls/{session}/master.m3u8")).await?
                == masters[index],
            "published master mutated",
        )?;
    }
    s10_require(
        s10_context(state, &sessions[0]).await?.bandwidth.is_none(),
        "cold owner upgraded after completion",
    )?;
    s10_require(
        source_fence.unchanged()
            && s10_hash(&s10_read_bounded(&path, 2_592_240)?) == S10_SOURCE_SHA,
        "source drift",
    )?;
    Ok(
        json!({"source_sha256":S10_SOURCE_SHA,"audio_index":audio.index,
        "audio_codec":audio.codec,"channels":audio.channels,"sample_rate":audio.sample_rate,
        "cold_master_sha256":s10_hash(&masters[0]),"retained_master_sha256":s10_hash(&masters[1]),
        "average_bps":measured.as_ref().map(|r|r.average_bps),"rfc_peak_bps":measured.as_ref().map(|r|r.peak_bps),
        "members":mux_receipts[1],"cold_capture_unknown":true,"masters_immutable":true}),
    )
}

#[tokio::test]
#[ignore = "requires explicit frozen-source and externally bounded runtime admission"]
async fn public_copy_new_retained_attachment_freezes_measured_master_and_exact_mux_wire() {
    let base = crate::test_tempdir().expect("owned S10 root");
    let store = Arc::new(SqliteStore::open_in_memory().expect("owned store"));
    let state = AppState::new(
        "test".into(),
        store,
        test_dirs(base.path()),
        "test-node".into(),
        Default::default(),
        Default::default(),
        Arc::new(crate::logbuf::LogBuffer::new(64)),
    );
    let mut guards = S10FailureGuards {
        state: Some(state.clone()),
        directory: Some(base),
        clean: false,
    };
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .expect("bounded HTTP client");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("owned listener");
    let address = listener.local_addr().expect("owned address");
    let stop = tokio_util::sync::CancellationToken::new();
    let stopped = stop.clone();
    let app = router(state.clone());
    let mut server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(stopped.cancelled_owned())
            .await
    });
    let root = format!("http://{address}");
    let mut sessions = Vec::new();
    let mut admin = None;
    let result = tokio::time::timeout(
        Duration::from_secs(120),
        s10_control(
            &state,
            &client,
            &root,
            guards
                .directory
                .as_ref()
                .expect("owned directory guard")
                .path(),
            &mut sessions,
            &mut admin,
        ),
    )
    .await
    .map_err(|_| "S10 control wall deadline".to_owned())
    .and_then(|value| value);
    // Cleanup never depends on a body future completing or an assertion not panicking.
    let cleanup = tokio::time::timeout(Duration::from_secs(10), async {
        for session in &sessions {
            let token = admin.as_ref().ok_or("cleanup token unavailable")?;
            let response = client
                .delete(format!("{root}/api/v1/hls/{session}"))
                .bearer_auth(token)
                .send()
                .await
                .map_err(|_| "public DELETE failed")?;
            let (status, _) = s10_response(response).await?;
            s10_require(status == 204, "public DELETE non-204")?;
        }
        s10_require(
            state.transcode.active_sessions().await == 0,
            "active session survived cleanup",
        )
    })
    .await;
    state.shutdown.cancel();
    stop.cancel();
    let joined = tokio::time::timeout(Duration::from_secs(5), &mut server).await;
    let listener_reusable = if matches!(joined, Ok(Ok(Ok(())))) {
        tokio::net::TcpListener::bind(address).await.is_ok()
    } else {
        false
    };
    let clean = matches!(cleanup, Ok(Ok(()))) && listener_reusable && result.is_ok();
    guards.clean = clean;
    let receipt = result.expect("S10 public wire control failed; guards retained on failure");
    assert!(
        clean,
        "S10 cleanup unresolved; guards retained for external owner"
    );
    println!("S10_PUBLIC_COPY_WIRE {}", receipt);
}
