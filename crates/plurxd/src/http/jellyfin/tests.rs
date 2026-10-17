use super::*;
use axum::{body::Body, http::Request};

/// A refused request is logged so the missing protocol piece is visible,
/// never with a credential in it.
#[test]
fn refused_requests_are_logged_without_credentials() {
    let line = refused_request_target(&Uri::from_static(
            "/jellyfin/Users/u/Items?SortBy=Bogus&api_key=secret1&ApiKey=secret2&X-Emby-Token=secret3&Recursive",
        ));
    assert_eq!(
            line,
            "/jellyfin/Users/u/Items?SortBy=Bogus&api_key=[REDACTED]&ApiKey=[REDACTED]&X-Emby-Token=[REDACTED]&Recursive"
        );
    // The direct-play `tag` is a bearer for one title; an unknown key may be.
    let direct = refused_request_target(&Uri::from_static(
            "/jellyfin/Videos/item/stream?static=true&tag=0123abcd&X-Emby-Authorization=MediaBrowser%20Token%3Dx",
        ));
    assert_eq!(
        direct,
        "/jellyfin/Videos/item/stream?static=true&tag=[REDACTED]&X-Emby-Authorization=[REDACTED]"
    );
    let hls = refused_request_target(&Uri::from_static(
        "/jellyfin/Videos/item/capability/hls/index.m3u8",
    ));
    assert!(!hls.contains("capability"), "{hls}");
    let long = refused_request_target(
        &Uri::try_from(format!("/jellyfin/Items?q={}", "a".repeat(2000))).expect("uri"),
    );
    assert!(long.len() <= 1024);
}
use http_body_util::BodyExt;
use tower::ServiceExt;
fn request(method: &str, path: &str, token: Option<&str>, body: Value) -> Request<Body> {
    let mut r = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(token) = token {
        r = r
            .header("x-emby-token", token)
            .header("authorization", format!("Bearer {token}"));
    }
    r.body(Body::from(body.to_string())).expect("request")
}
/// Facade URLs are client-base-relative: the client joins its server
/// address, which already carries any mount, with them (design 3.1; J0
/// Infuse requested `/jellyfin` + `TranscodingUrl`). The test app is
/// Plurx's own port, so the request goes under the mount.
fn mounted(url: &str) -> String {
    assert!(
        url.starts_with("/Videos/"),
        "a facade URL names no mount: {url}"
    );
    format!("{MOUNT}{url}")
}
async fn json_call(app: &Router, r: Request<Body>) -> (StatusCode, Value) {
    let path = r.uri().path().to_owned();
    let response = app.clone().oneshot(r).await.expect("response");
    let status = response.status();
    assert!(
        response
            .headers()
            .get("content-type")
            .expect("JSON content type")
            .to_str()
            .expect("content type")
            .starts_with("application/json"),
        "{path}: expected JSON response, got {status}"
    );
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    (status, serde_json::from_slice(&bytes).expect("JSON body"))
}
/// A returned media URL as the client requests it: clients prefix the
/// configured server address, which already ends in `/jellyfin`.
fn without_key(url: &str, token: &str) -> String {
    let key = format!("ApiKey={token}");
    assert!(
        url.contains(&key),
        "a returned URL carries the login: {url}"
    );
    url.replace(&format!("&{key}"), "")
        .replace(&format!("?{key}"), "")
}
async fn setup(app: &Router) -> String {
    let (status, body) = json_call(
        app,
        request(
            "POST",
            "/api/v1/setup",
            None,
            json!({"username":"catalog-admin","password":"supersecret"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    body["token"].as_str().expect("setup token").into()
}
async fn facade_login(app: &Router) -> (String, String) {
    let mut r = request(
        "POST",
        "/jellyfin/Users/AuthenticateByName",
        None,
        json!({"Username":"catalog-admin","Pw":"supersecret"}),
    );
    r.headers_mut().insert("x-emby-authorization", "MediaBrowser Client=\"Jellyfin+Android+TV\", Device=\"test TV\", DeviceId=\"catalog-contract\", Version=\"0.19.10\"".parse().expect("metadata"));
    let (status, body) = json_call(app, r).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["User"]["Policy"]["IsAdministrator"], false);
    assert_eq!(body["User"]["Policy"]["EnableContentDownloading"], false);
    (
        body["AccessToken"].as_str().expect("login token").into(),
        body["User"]["Id"].as_str().expect("user wire").into(),
    )
}
#[tokio::test]
async fn jellyfin_hls_requires_login_and_rejects_a_direct_binding_before_activation() {
    let f = playback_fixture().await;
    let (status, body) = json_call(&f.app, request("POST", &format!("/jellyfin/Items/{}/PlaybackInfo", f.item), Some(&f.token), json!({"DeviceProfile":{"DirectPlayProfiles":[{"Type":"Video","Container":"mp4","VideoCodec":"h264","AudioCodec":"aac"}]}}))).await;
    assert_eq!(status, StatusCode::OK);
    let play = body["PlaySessionId"].as_str().expect("direct binding");
    let root = format!(
        "/jellyfin/Videos/{}/master.m3u8?MediaSourceId={}&PlaySessionId={play}",
        f.item, f.source
    );
    let (status, _) = json_call(&f.app, request("GET", &root, None, Value::Null)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = json_call(&f.app, request("GET", &root, Some(&f.token), Value::Null)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let client_scope = f
        .state
        .store
        .jellyfin_login_scope(plurx_core::auth::hash_token(&f.token))
        .await
        .expect("scope")
        .expect("login");
    let unchanged = f
        .state
        .store
        .jellyfin_play(play, &client_scope)
        .await
        .expect("read")
        .expect("play");
    assert_eq!(unchanged.state, "pending");
    assert!(unchanged.native_incarnation_id.is_none());
    assert!(
        unchanged.direct_grant_id.is_some(),
        "the negotiated link grant exists, but HLS cannot activate a direct play"
    );
    let path = format!("/jellyfin/Videos/{}/{play}/hls/seg00001.ts", f.item);
    let (status, _) = json_call(&f.app, request("GET", &path, None, Value::Null)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = json_call(&f.app, request("GET", &path, Some(&f.token), Value::Null)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn jellyfin_hls_prerequisite_refusals_allocate_no_native_reader_or_producer() {
    let f = playback_fixture().await;
    f.state
        .store
        .put_setting(plurx_core::store::keys::VOD_PRESENTATION, "0")
        .await
        .expect("disable native VOD");
    let path = format!("/jellyfin/Items/{}/PlaybackInfo", f.item);
    let (status, body) = json_call(&f.app, request("POST", &path, Some(&f.token), json!({"EnableDirectPlay":false,"MaxStreamingBitrate":750000,"DeviceProfile":{"TranscodingProfiles":[{"Type":"Video","Container":"ts","VideoCodec":"h264","AudioCodec":"aac","Protocol":"hls","MaxAudioChannels":"2","ManifestSubtitles":"vtt"}]}}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ErrorCode"], "NotSupported");
    assert!(body["MediaSources"].as_array().expect("sources").is_empty());
    assert!(body.get("PlaySessionId").is_none());
    assert!(f.state.transcode.active_session_ids().await.is_empty());
    assert!(f
        .state
        .transcode
        .vod_live_or_preparing_session_ids()
        .await
        .is_empty());
}

#[tokio::test]
async fn jellyfin_native_hls_copy_preserves_original_time_auth_and_inline_fragment_ranges() {
    Box::pin(native_hls_copy_flow(false, false)).await;
}
#[tokio::test]
async fn jellyfin_native_hls_duplicate_entries_share_one_activation_and_preserve_ranges_and_stop() {
    Box::pin(native_hls_copy_flow(true, false)).await;
}
#[tokio::test]
async fn jellyfin_native_hls_encoded_without_copy_index_preserves_ranges_clock_and_stop() {
    Box::pin(native_hls_copy_flow(false, true)).await;
}
async fn native_hls_copy_flow(duplicate: bool, encoded: bool) {
    let f = playback_fixture().await;
    // Production renews the native route's 12-second lease from the
    // owner loop. Without it, a flow slower than one lease (a loaded CI
    // runner) sees its own route expire and every request answer 409.
    let lease_owner = tokio::spawn(crate::media_sessions::lease_loop(f.state.clone()));
    let path = f
        .root
        .path()
        .canonicalize()
        .expect("canonical media")
        .join("movie.mp4");
    std::fs::copy(plurx_core::testfixtures::source("h264"), &path).expect("real native source");
    let metadata = std::fs::metadata(&path).expect("source metadata");
    let mtime = metadata
        .modified()
        .expect("mtime")
        .duration_since(std::time::UNIX_EPOCH)
        .expect("epoch")
        .as_secs() as i64;
    let source_probe = if encoded {
        let mut probe = plurx_core::scan::probe::probe(&path)
            .await
            .expect("complete encoded source probe");
        // A text track the native master would advertise; this profile
        // asks for no manifest subtitles, so the facade must not.
        probe
            .subtitle_streams
            .push(plurx_core::domain::SubtitleStream {
                index: 2,
                codec: "subrip".into(),
                language: Some("eng".into()),
                title: None,
                default: false,
                forced: false,
                hearing_impaired: false,
            });
        probe
    } else {
        plurx_core::domain::ProbeResult {
                duration_ms: Some(12_000), container: Some("mkv".into()), video_codec: Some("h264".into()),
                video_profile: Some("Main".into()), width: Some(640), height: Some(360), bit_depth: Some(8), bitrate: Some(1_000_000),
                audio_streams: vec![plurx_core::domain::AudioStream { index: 0, codec: "aac".into(), channel_layout: None, channels: Some(2), sample_rate: Some(48000), default: true, ..Default::default() }],
                raw_json: Some(json!({"streams":[{"index":0,"codec_type":"video","codec_name":"h264","width":640,"height":360,"profile":"Main","avg_frame_rate":"24/1"},{"index":1,"codec_type":"audio","codec_name":"aac","channels":2,"sample_rate":"48000"}]}).to_string()), ..Default::default()
            }
    };
    let file_id = f
        .state
        .store
        .upsert_file(
            f.native_item,
            path.to_str().expect("path"),
            metadata.len() as i64,
            mtime,
            &source_probe,
        )
        .await
        .expect("native source probe");
    let file = f
        .state
        .store
        .get_file(file_id)
        .await
        .expect("source")
        .expect("file");
    if !encoded {
        let indexed = Box::pin(crate::fragindex::build(
            &file,
            plurx_core::transcode::CopyVideoOptions::new(
                crate::ffmpeg::has_dovi_rpu().await,
                false,
            ),
            &f.root.path().join("native-index"),
            std::time::Duration::from_secs(120),
        ))
        .await;
        let crate::fragindex::IndexOutcome::Built(index) = indexed else {
            panic!("native index: {indexed:?}");
        };
        f.state
            .store
            .put_fragment_index(file_id, &index)
            .await
            .expect("index");
    }
    if encoded {
        assert!(!f
            .state
            .store
            .holds_fragment_index_for_source(file_id, metadata.len() as i64, mtime)
            .await
            .expect("no copy index"));
    }
    // The encoded run omits manifest subtitles, as Infuse's profile does:
    // its master alias must still be a multivariant wrapper.
    let mut transcoding = json!({"Type":"Video","Container":"ts","VideoCodec":"h264","AudioCodec":"aac","Protocol":"hls","MaxAudioChannels":"2"});
    if !encoded {
        transcoding["ManifestSubtitles"] = json!("vtt");
    }
    let (status, info) = json_call(&f.app, request("POST", &format!("/jellyfin/Items/{}/PlaybackInfo", f.item), Some(&f.token), json!({"EnableDirectPlay":false,"StartTimeTicks":20_000_000,"MaxStreamingBitrate":if encoded {750_000} else {2_000_000},"AllowVideoStreamCopy":!encoded,"DeviceProfile":{"TranscodingProfiles":[transcoding]}}))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        info["ErrorCode"].is_null(),
        "native prerequisites refused: {info}"
    );
    let play_id = info["PlaySessionId"].as_str().expect("play");
    let root_url = info["MediaSources"][0]["TranscodingUrl"]
        .as_str()
        .expect("native HLS URL");
    let url = &mounted(root_url);
    let response = f
        .app
        .clone()
        .oneshot(request(
            "GET",
            &without_key(url, &f.token),
            None,
            Value::Null,
        ))
        .await
        .expect("unauthenticated root");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    // Infuse lower-cases the first letter of every query key it was given.
    let lowered = url
        .replace("MediaSourceId=", "mediaSourceId=")
        .replace("PlaySessionId=", "playSessionId=")
        .replace("ApiKey=", "apiKey=");
    let response = if duplicate {
        let (first, second) = tokio::join!(
            f.app
                .clone()
                .oneshot(request("GET", url, Some(&f.token), Value::Null)),
            f.app
                .clone()
                .oneshot(request("GET", url, Some(&f.token), Value::Null)),
        );
        let first = first.expect("first canonical root");
        let second = second.expect("duplicate canonical root");
        let duplicate_status = second.status();
        let duplicate_bytes = second
            .into_body()
            .collect()
            .await
            .expect("duplicate manifest")
            .to_bytes();
        assert_eq!(
            duplicate_status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&duplicate_bytes)
        );
        assert_eq!(
            f.state
                .transcode
                .vod_live_or_preparing_session_ids()
                .await
                .len(),
            1,
            "duplicate entries attach one native reader identity"
        );
        first
    } else {
        // Android TV sends no header on media: the URL's ApiKey is all.
        f.app
            .clone()
            .oneshot(request("GET", &lowered, None, Value::Null))
            .await
            .expect("native root")
    };
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("root body")
        .to_bytes();
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let master = String::from_utf8(bytes.to_vec()).expect("manifest");
    let scope = f
        .state
        .store
        .jellyfin_login_scope(plurx_core::auth::hash_token(&f.token))
        .await
        .expect("scope")
        .expect("login");
    let play = f
        .state
        .store
        .jellyfin_play(play_id, &scope)
        .await
        .expect("binding")
        .expect("play");
    assert_eq!(play.negotiation.source_origin_ms, 0);
    let frozen: Value = serde_json::from_str(&play.negotiation.selection_json).expect("selection");
    assert_eq!(
        frozen["vod"]["body"]["copy"], !encoded,
        "frozen native recipe must reflect actual video copying"
    );
    if encoded {
        assert_eq!(frozen["vod"]["bitrate"], 750_000);
    }

    let route = f
        .state
        .store
        .media_session_route_by_incarnation(
            play.native_incarnation_id
                .as_deref()
                .expect("native binding"),
        )
        .await
        .expect("route")
        .expect("native");
    assert!(!master.contains(&route.session_id) && !master.contains("/api/v1/hls/"));
    assert!(
        master.contains("#EXT-X-STREAM-INF"),
        "the master alias is a multivariant wrapper: {master}"
    );
    // A client at the root of the Jellyfin standard port is handed the
    // same children at that root, never under a mount it did not use.
    let at_root = super::standard_port_app(f.app.clone())
        .oneshot(request("GET", root_url, Some(&f.token), Value::Null))
        .await
        .expect("root-mounted master");
    assert_eq!(at_root.status(), StatusCode::OK);
    let root_master = String::from_utf8(
        at_root
            .into_body()
            .collect()
            .await
            .expect("root master body")
            .to_bytes()
            .to_vec(),
    )
    .expect("root manifest");
    let children = |manifest: &str| {
        manifest
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let root_children = children(&root_master);
    assert!(!root_children.is_empty(), "{root_master}");
    assert!(
        root_children
            .iter()
            .all(|child| child.starts_with("/Videos/")),
        "{root_master}"
    );
    assert_eq!(
        children(&master),
        root_children
            .iter()
            .map(|child| format!("{MOUNT}{child}"))
            .collect::<Vec<_>>()
    );
    let child = super::standard_port_app(f.app.clone())
        .oneshot(request(
            "GET",
            &root_children[0],
            Some(&f.token),
            Value::Null,
        ))
        .await
        .expect("root-mounted child");
    assert_eq!(child.status(), StatusCode::OK, "{}", root_children[0]);
    if encoded {
        assert!(
            !master.contains("#EXT-X-MEDIA") && !master.contains("SUBTITLES="),
            "a play negotiated without manifest subtitles advertises none: {master}"
        );
    }
    let media_url = master
        .lines()
        .find(|line| line.contains("index.m3u8") && !line.starts_with('#'))
        .expect("media child");
    assert!(
        media_url.ends_with(&format!("index.m3u8?ApiKey={}", f.token)),
        "every child carries the URL credential: {media_url}"
    );
    if encoded {
        let subtitles = format!(
            "{}subs/2/index.m3u8",
            media_url.split_once("index.m3u8").expect("HLS mount").0
        );
        let refused = f
            .app
            .clone()
            .oneshot(request("GET", &subtitles, Some(&f.token), Value::Null))
            .await
            .expect("subtitle playlist");
        assert_eq!(refused.status(), StatusCode::NOT_FOUND);
    }
    let response = f
        .app
        .clone()
        .oneshot(request("GET", media_url, Some(&f.token), Value::Null))
        .await
        .expect("media playlist");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("media body")
        .to_bytes();
    let media = String::from_utf8(bytes.to_vec()).expect("media manifest");
    assert!(!media.contains("#EXT-X-MAP"));
    assert!(media.contains("#EXT-X-ENDLIST"));
    let segment_url = media
        .lines()
        .find(|line| line.contains(".ts?ApiKey="))
        .expect("inline child");
    let init = Box::pin(super::super::hls::segment(
        State(f.state.clone()),
        Path((route.session_id.clone(), "init.mp4".into())),
        HeaderMap::new(),
    ))
    .await
    .expect("native init");
    let init = init
        .into_body()
        .collect()
        .await
        .expect("init bytes")
        .to_bytes();
    let mut range = request("GET", segment_url, Some(&f.token), Value::Null);
    range.headers_mut().insert(
        "range",
        format!("bytes={}-{}", init.len() - 3, init.len() + 4)
            .parse()
            .expect("range"),
    );
    let response = f.app.clone().oneshot(range).await.expect("crossing range");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("range bytes")
        .to_bytes();
    assert_eq!(
        status,
        StatusCode::PARTIAL_CONTENT,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    assert_eq!(bytes.len(), 8);
    assert_eq!(&bytes[..3], &init[init.len() - 3..]);
    let fresh = f
        .state
        .store
        .media_session_route_by_incarnation(&route.incarnation_id)
        .await
        .expect("frontier")
        .expect("route");
    assert_eq!(
        fresh.fetched_through_ms, 0,
        "a partial composite read cannot mark a full native fragment fetched"
    );
    // While a node has lost serving authority (a quorum leader restart),
    // the gate answers the facade as it answers native media: 503 with
    // Retry-After, before any store read. The binding and route are
    // untouched, so the same play answers once authority is back. That
    // the native session itself outlives a short loss is the serving
    // fence's own property (`serving_fence_flapping_losses_share_one_grace`).
    f.state.serving.validation_set_ready(false).await;
    let fenced = f
        .app
        .clone()
        .oneshot(request("GET", segment_url, None, Value::Null))
        .await
        .expect("fenced child");
    assert_eq!(fenced.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(fenced.headers().contains_key("retry-after"));
    f.state.serving.validation_set_ready(true).await;
    let recovered = f
        .app
        .clone()
        .oneshot(request("GET", segment_url, None, Value::Null))
        .await
        .expect("recovered child");
    assert_eq!(
        recovered.status(),
        StatusCode::OK,
        "the play survives a recovered authority loss"
    );
    if encoded {
        let response = f
            .app
            .clone()
            .oneshot(request("GET", segment_url, Some(&f.token), Value::Null))
            .await
            .expect("whole encoded fragment");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("whole encoded body")
            .to_bytes();
        let decoded_input = f.root.path().join("encoded-alias-get.mp4");
        tokio::fs::write(&decoded_input, &bytes)
            .await
            .expect("decoder fixture");
        let decoded = tokio::process::Command::new(crate::ffmpeg::ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-xerror", "-i"])
            .arg(&decoded_input)
            .args(["-map", "0:v:0", "-map", "0:a:0", "-f", "framehash", "-"])
            .kill_on_drop(true)
            .output()
            .await
            .expect("decode actual mapped video and audio");
        assert!(
            decoded.status.success(),
            "{}",
            String::from_utf8_lossy(&decoded.stderr)
        );
        let frames = String::from_utf8(decoded.stdout).expect("decoded frame hashes");
        assert!(
            frames.lines().filter(|line| line.starts_with("0,")).count() >= 2,
            "mapped video must decode: {frames}"
        );
        assert!(
            frames.lines().filter(|line| line.starts_with("1,")).count() >= 2,
            "mapped audio must decode: {frames}"
        );
    }

    let response = f
        .app
        .clone()
        .oneshot(request(
            "GET",
            &without_key(segment_url, &f.token),
            None,
            Value::Null,
        ))
        .await
        .expect("anonymous child");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    // A long pause sends progress, not media requests. That authenticated
    // presence must renew the exact passive grant, or the idle route ends
    // and the client's resume receives 410.
    f.state
        .transcode
        .shorten_passive_grant_for_test(&route.session_id, std::time::Duration::from_secs(1))
        .await;
    let paused = f.app.clone().oneshot(request("POST", "/jellyfin/Sessions/Playing/Progress", Some(&f.token), json!({"UserId":f.user,"ItemId":f.item,"MediaSourceId":f.source,"PlaySessionId":play_id,"PlayMethod":"Transcode","PositionTicks":5000*10_000,"IsPaused":true}))).await.expect("paused progress");
    assert_eq!(paused.status(), StatusCode::NO_CONTENT);
    let renewed = f
        .state
        .transcode
        .passive_grant_remaining_for_test(&route.session_id)
        .await
        .expect("live passive grant");
    assert!(
        renewed > std::time::Duration::from_secs(500),
        "paused presence must renew the passive grant, left {renewed:?}"
    );
    // Past the reader idle TTL the reader is reaped; the paused client's
    // resume is a later segment, which must resurrect the same route
    // through the facade (the long-pause path a real client takes).
    let vod = f.state.transcode.vod_for_test();
    vod.force_reader_idle_for_test(&route.session_id).await;
    vod.maintain().await;
    assert_eq!(vod.active_sessions().await, 0, "the reader was reaped");
    let resume_url = media
        .lines()
        .rfind(|line| !line.is_empty() && !line.starts_with("#"))
        .expect("last segment");
    let resumed = f
        .app
        .clone()
        .oneshot(request("GET", resume_url, Some(&f.token), Value::Null))
        .await
        .expect("resume after idle reap");
    let resumed_status = resumed.status();
    let resumed_bytes = resumed
        .into_body()
        .collect()
        .await
        .expect("resumed body")
        .to_bytes();
    assert_eq!(
        resumed_status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&resumed_bytes)
    );
    assert!(!resumed_bytes.is_empty());
    assert_eq!(
        vod.active_sessions().await,
        1,
        "the resume resurrected the reader"
    );
    // Progress and Ping keep the play alive; ActiveEncodings releases its
    // encoding but keeps the binding, so the final Stopped still commits.
    for (method, endpoint, position) in [
        (
            "POST",
            "/jellyfin/Sessions/Playing/Progress".to_owned(),
            Some(5000),
        ),
        (
            "POST",
            format!("/jellyfin/Sessions/Playing/Ping?PlaySessionId={play_id}"),
            None,
        ),
        (
            "DELETE",
            format!("/jellyfin/Videos/ActiveEncodings?PlaySessionId={play_id}"),
            None,
        ),
        (
            "POST",
            "/jellyfin/Sessions/Playing/Stopped".to_owned(),
            Some(7000),
        ),
    ] {
        let body = position.map_or(Value::Null, |position: i64| json!({"UserId":f.user,"ItemId":f.item,"MediaSourceId":f.source,"PlaySessionId":play_id,"PlayMethod":"Transcode","PositionTicks":position*10_000}));
        let response = f
            .app
            .clone()
            .oneshot(request(method, &endpoint, Some(&f.token), body))
            .await
            .expect("native watch event");
        assert_eq!(
            response.status(),
            StatusCode::NO_CONTENT,
            "{method} {endpoint}"
        );
        if method == "DELETE" {
            let released = f
                .state
                .store
                .media_session_route_by_incarnation(&route.incarnation_id)
                .await
                .expect("released")
                .expect("native row");
            assert_eq!(released.state, "ended", "the encoding is released");
        }
    }
    let watch = f
        .state
        .store
        .watch_state(1, f.native_item)
        .await
        .expect("watch")
        .expect("saved");
    assert_eq!(
        watch.position_ms, 7000,
        "progress remains on the original movie timeline"
    );
    let ended = f
        .state
        .store
        .media_session_route_by_incarnation(&route.incarnation_id)
        .await
        .expect("ended")
        .expect("native row");
    assert_eq!(ended.state, "ended");
    assert!(f.state.transcode.active_session_ids().await.is_empty());
    assert!(f
        .state
        .transcode
        .vod_live_or_preparing_session_ids()
        .await
        .is_empty());
    let response = f
        .app
        .clone()
        .oneshot(request("GET", segment_url, Some(&f.token), Value::Null))
        .await
        .expect("late child");
    assert_ne!(response.status(), StatusCode::OK);
    lease_owner.abort();
    assert!(lease_owner.await.is_err_and(|error| error.is_cancelled()));
}

struct PlaybackFixture {
    app: Router,
    state: AppState,
    root: tempfile::TempDir,
    token: String,
    user: String,
    item: String,
    source: String,
    native_item: i64,
}
async fn playback_fixture() -> PlaybackFixture {
    use plurx_core::domain::{
        AudioStream, ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult,
    };
    let root = tempfile::tempdir().expect("playback root");
    let store = std::sync::Arc::new(
        plurx_core::store::SqliteStore::open(&root.path().join("state.db")).expect("store"),
    );
    let canonical_root = root.path().canonicalize().expect("canonical fixture dirs");
    let dirs = crate::state::Dirs {
        artwork: canonical_root.join("artwork"),
        transcode: canonical_root.join("transcode"),
        cache: canonical_root.join("cache"),
        subs: canonical_root.join("subs"),
        runtime_cache: canonical_root.join("runtime"),
        renditions: canonical_root.join("renditions"),
    };
    for path in [
        &dirs.artwork,
        &dirs.transcode,
        &dirs.cache,
        &dirs.subs,
        &dirs.runtime_cache,
        &dirs.renditions,
    ] {
        std::fs::create_dir_all(path).expect("fixture cache root");
    }
    let state = AppState::new_unhooked(
        "playback-contract".into(),
        store,
        dirs,
        "test-node".into(),
        Default::default(),
        Default::default(),
        std::sync::Arc::new(crate::logbuf::LogBuffer::new(64)),
    );
    let app = super::super::router(state.clone());
    setup(&app).await;
    state
        .store
        .set_jellyfin_compatibility(true)
        .await
        .expect("enable");
    let library = state
        .store
        .create_library(&NewLibrary {
            name: "Direct films".into(),
            kind: LibraryKind::Movies,
            paths: vec![root.path().into()],
            anime: false,
        })
        .await
        .expect("library");
    let native_item = state
        .store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "Direct contract".into(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    let path = root
        .path()
        .canonicalize()
        .expect("canonical media root")
        .join("movie.mp4");
    std::fs::write(&path, b"0123456789abcdef").expect("bytes");
    state.store.upsert_file(native_item,path.to_str().expect("path"),16,1,&ProbeResult {duration_ms:Some(100_000),container:Some("mp4".into()),video_codec:Some("h264".into()),video_codec_tag:Some("avc1".into()),video_profile:Some("Main".into()),width:Some(1920),height:Some(1080),bit_depth:Some(8),bitrate:Some(100_000),audio_streams:vec![AudioStream {index:0,codec:"aac".into(), channel_layout: None,channels:Some(2),sample_rate:Some(48_000),language:None,title:None,default:true}],raw_json:Some(json!({"streams":[{"index":0,"codec_type":"audio","codec_name":"aac","channels":2,"sample_rate":"48000"},{"index":1,"codec_type":"video","codec_name":"h264","width":1920,"height":1080}]}).to_string()),..Default::default()}).await.expect("file");
    let (token, user) = facade_login(&app).await;
    let (status, page) = json_call(
        &app,
        request(
            "GET",
            "/jellyfin/Items?Recursive=true",
            Some(&token),
            json!({}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let item = page["Items"][0]["Id"].as_str().expect("item").to_owned();
    let source = page["Items"][0]["MediaSources"][0]["Id"]
        .as_str()
        .expect("source")
        .to_owned();
    PlaybackFixture {
        app,
        state,
        root,
        token,
        user,
        item,
        source,
        native_item,
    }
}
async fn negotiate(f: &PlaybackFixture) -> Value {
    let (status,body)=json_call(&f.app,request("POST",&format!("/jellyfin/Items/{}/PlaybackInfo",f.item),Some(&f.token),json!({"UserId":f.user,"MediaSourceId":f.source,"AudioStreamIndex":0,"SubtitleStreamIndex":-1,"EnableDirectPlay":true,"DeviceProfile":{"DirectPlayProfiles":[{"Type":"Video","Container":"mp4","VideoCodec":"h264","AudioCodec":"aac"}]}}))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["ErrorCode"].is_null(), "negotiation refused: {body}");
    assert_eq!(body["MediaSources"][0]["SupportsDirectPlay"], true);
    assert_eq!(body["MediaSources"][0]["DefaultAudioStreamIndex"], 0);
    assert!(!body
        .to_string()
        .contains(f.root.path().to_str().expect("path")));
    body
}
async fn play_event(
    f: &PlaybackFixture,
    route: &str,
    play: &str,
    position: Option<i64>,
) -> StatusCode {
    let mut body = json!({"UserId":f.user,"ItemId":f.item,"MediaSourceId":f.source,"PlaySessionId":play,"PlayMethod":"DirectPlay"});
    if let Some(position) = position {
        body["PositionTicks"] = json!(position * 10_000);
    }
    f.app
        .clone()
        .oneshot(request("POST", route, Some(&f.token), body))
        .await
        .expect("event")
        .status()
}
#[tokio::test]
async fn jellyfin_replacement_activation_fences_old_events_and_preserves_new_direct_presence() {
    let f = playback_fixture().await;
    let old = negotiate(&f).await;
    let old_id = old["PlaySessionId"].as_str().expect("old");
    assert_eq!(
        play_event(&f, "/jellyfin/Sessions/Playing", old_id, Some(1000)).await,
        StatusCode::NO_CONTENT
    );
    let next = negotiate(&f).await;
    let next_id = next["PlaySessionId"].as_str().expect("next");
    let scope = f
        .state
        .store
        .jellyfin_login_scope(plurx_core::auth::hash_token(&f.token))
        .await
        .expect("scope")
        .expect("login");
    let old_binding = f
        .state
        .store
        .jellyfin_play(old_id, &scope)
        .await
        .expect("old")
        .expect("binding");
    let next_binding = f
        .state
        .store
        .jellyfin_play(next_id, &scope)
        .await
        .expect("next")
        .expect("binding");
    assert_eq!(
        old_binding.state, "active",
        "negotiation must keep incumbent"
    );
    assert_eq!(
        old_binding.negotiation.playback_id,
        next_binding.negotiation.playback_id
    );
    assert_eq!(
        play_event(&f, "/jellyfin/Sessions/Playing", next_id, Some(2000)).await,
        StatusCode::NO_CONTENT
    );
    let old_grant = f
        .state
        .store
        .file_grant_by_id(old_binding.direct_grant_id.as_deref().expect("old grant"))
        .await
        .expect("old grant")
        .expect("old grant");
    assert!(
        old_grant.revoked_at.is_some(),
        "a replacement without Stopped releases its predecessor's grant"
    );
    let url = &mounted(
        next["MediaSources"][0]["DirectStreamUrl"]
            .as_str()
            .expect("URL"),
    );
    let response = f
        .app
        .clone()
        .oneshot(request("GET", url, Some(&f.token), json!({})))
        .await
        .expect("new direct");
    assert_eq!(response.status(), StatusCode::OK);
    response.into_body().collect().await.expect("direct body");
    // Native start notification is asynchronous; observe its committed
    // presence before exercising the late Stop, without adding an owner.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while !f
        .state
        .direct_plays
        .list()
        .iter()
        .any(|p| p.registry_id.contains(next_id))
    {
        assert!(
            std::time::Instant::now() < deadline,
            "native start notification did not register this play"
        );
        tokio::task::yield_now().await;
    }
    assert!(
        f.state
            .direct_plays
            .list()
            .iter()
            .any(|p| p.registry_id.contains(next_id)),
        "new play has its own direct presence"
    );
    assert_eq!(
        play_event(&f, "/jellyfin/Sessions/Playing/Stopped", old_id, Some(9000)).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        f.state.direct_plays.list().len(),
        1,
        "old Stop cannot remove new presence"
    );
    assert_eq!(
        play_event(
            &f,
            "/jellyfin/Sessions/Playing/Progress",
            old_id,
            Some(9999)
        )
        .await,
        StatusCode::CONFLICT
    );
    let before = f
        .state
        .store
        .watch_state(scope.user_id, f.native_item)
        .await
        .expect("watch");
    assert!(before.is_none_or(|w| w.position_ms != 9000 && w.position_ms != 9999));
    assert_eq!(
        play_event(
            &f,
            "/jellyfin/Sessions/Playing/Stopped",
            next_id,
            Some(2500)
        )
        .await,
        StatusCode::NO_CONTENT
    );
    assert!(f.state.direct_plays.list().is_empty());
    assert_eq!(
        f.state
            .store
            .watch_state(scope.user_id, f.native_item)
            .await
            .expect("watch")
            .expect("final")
            .position_ms,
        2500
    );
}

#[tokio::test]
async fn jellyfin_direct_range_head_and_stop_preserve_native_authority_and_final_position() {
    let f = playback_fixture().await;
    let negotiation = negotiate(&f).await;
    let play = negotiation["PlaySessionId"].as_str().expect("play");
    // Without the ApiKey it carries, the authenticated URL is just a path.
    let url = &without_key(
        &mounted(
            negotiation["MediaSources"][0]["DirectStreamUrl"]
                .as_str()
                .expect("direct URL"),
        ),
        &f.token,
    );
    let unauthorized = f
        .app
        .clone()
        .oneshot(request("GET", url, None, json!({})))
        .await
        .expect("unauthorized");
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    let mut ranged = request("GET", url, Some(&f.token), json!({}));
    ranged
        .headers_mut()
        .insert("range", "bytes=2-5".parse().expect("range"));
    let response = f.app.clone().oneshot(ranged).await.expect("range");
    if response.status() != StatusCode::PARTIAL_CONTENT {
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("error body")
            .to_bytes();
        panic!(
            "range refused {status}: {}",
            String::from_utf8_lossy(&bytes)
        );
    }
    assert_eq!(response.headers()["content-range"], "bytes 2-5/16");
    assert_eq!(
        &response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes()[..],
        b"2345"
    );
    let head = f
        .app
        .clone()
        .oneshot(request("HEAD", url, Some(&f.token), json!({})))
        .await
        .expect("head");
    assert_eq!(head.status(), StatusCode::OK);
    assert!(head
        .into_body()
        .collect()
        .await
        .expect("head body")
        .to_bytes()
        .is_empty());
    assert_eq!(
        play_event(&f, "/jellyfin/Sessions/Playing", play, Some(1000)).await,
        StatusCode::NO_CONTENT
    );
    // Infuse labels static Range delivery `DirectStream`; only a label
    // naming the other delivery is refused.
    for (method, expected) in [
        ("DirectStream", StatusCode::NO_CONTENT),
        ("Transcode", StatusCode::BAD_REQUEST),
    ] {
        let body = json!({"UserId":f.user,"ItemId":f.item,"MediaSourceId":f.source,"PlaySessionId":play,"PlayMethod":method,"PositionTicks":3000*10_000});
        let response = f
            .app
            .clone()
            .oneshot(request(
                "POST",
                "/jellyfin/Sessions/Playing/Progress",
                Some(&f.token),
                body,
            ))
            .await
            .expect("labelled progress");
        assert_eq!(response.status(), expected, "{method}");
    }
    assert_eq!(
        play_event(&f, "/jellyfin/Sessions/Playing/Progress", play, Some(5000)).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        play_event(&f, "/jellyfin/Sessions/Playing/Stopped", play, Some(7000)).await,
        StatusCode::NO_CONTENT
    );
    let watch = f
        .state
        .store
        .watch_state(1, f.native_item)
        .await
        .expect("watch")
        .expect("watch");
    assert_eq!(watch.position_ms, 7000);
    assert_eq!(
        play_event(&f, "/jellyfin/Sessions/Playing/Stopped", play, Some(9000)).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        f.state
            .store
            .watch_state(1, f.native_item)
            .await
            .expect("preserved"),
        Some(watch)
    );
    assert!(f.state.direct_plays.list().is_empty());
    let late = f
        .app
        .clone()
        .oneshot(request("GET", url, Some(&f.token), json!({})))
        .await
        .expect("late range");
    assert_eq!(late.status(), StatusCode::CONFLICT);
}
async fn status_of(f: &PlaybackFixture, r: Request<Body>) -> (StatusCode, Vec<u8>) {
    let response = f.app.clone().oneshot(r).await.expect("response");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes()
        .to_vec();
    (status, bytes)
}
fn android_direct_url(f: &PlaybackFixture, item: &str, tag: &str) -> String {
    // The exact query Jellyfin Android TV 0.19.10 builds for direct play:
    // no PlaySessionId and no credential, the source ETag as `tag`.
    format!(
            "/jellyfin/Videos/{item}/stream?container=mp4&static=true&tag={tag}&mediaSourceId={}&streamOptions=%7B%7D&enableAudioVbrEncoding=true",
            f.source
        )
}
#[tokio::test]
async fn jellyfin_scoped_media_link_serves_one_title_without_login_and_ends_with_stop_or_logout() {
    let f = playback_fixture().await;
    let negotiation = negotiate(&f).await;
    let play = negotiation["PlaySessionId"].as_str().expect("play");
    let tag = negotiation["MediaSources"][0]["ETag"]
        .as_str()
        .expect("scoped link")
        .to_owned();
    assert_eq!(tag.len(), 64);
    let direct_url = &mounted(
        negotiation["MediaSources"][0]["DirectStreamUrl"]
            .as_str()
            .expect("direct URL"),
    );
    assert!(
        !direct_url.contains(&tag),
        "the authenticated URL must not carry the link"
    );
    let binding = f
        .state
        .store
        .jellyfin_play(
            play,
            &f.state
                .store
                .jellyfin_login_scope(plurx_core::auth::hash_token(&f.token))
                .await
                .expect("scope")
                .expect("scope"),
        )
        .await
        .expect("binding")
        .expect("binding");
    assert_eq!(binding.state, "pending");
    let grant = f
        .state
        .store
        .file_grant_by_id(
            binding
                .direct_grant_id
                .as_deref()
                .expect("negotiated grant"),
        )
        .await
        .expect("grant")
        .expect("grant");
    assert!(
        grant.expires_at <= binding.negotiation.created_at_ms / 1000 + 86_400,
        "the link expires within 24 hours of the negotiation"
    );
    let url = android_direct_url(&f, &f.item, &tag);
    let mut ranged = request("GET", &url, None, Value::Null);
    ranged
        .headers_mut()
        .insert("range", "bytes=2-5".parse().expect("range"));
    let (status, body) = status_of(&f, ranged).await;
    assert_eq!(
        status,
        StatusCode::PARTIAL_CONTENT,
        "{}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(&body[..], b"2345");
    let (status, body) = status_of(&f, request("HEAD", &url, None, Value::Null)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_empty());
    // Device metadata without a token is still an anonymous caller.
    let mut metadata = request("GET", &url, None, Value::Null);
    metadata.headers_mut().insert(
        "x-emby-authorization",
        "MediaBrowser Client=\"Jellyfin Android TV\", DeviceId=\"catalog-contract\""
            .parse()
            .expect("metadata"),
    );
    assert_eq!(status_of(&f, metadata).await.0, StatusCode::OK);
    // The link names one title: another item path or a guessed tag refuse.
    let other = WireId::random().to_hex();
    assert_eq!(
        status_of(
            &f,
            request(
                "GET",
                &android_direct_url(&f, &other, &tag),
                None,
                Value::Null
            )
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let mut guessed = tag.clone().into_bytes();
    guessed[0] = if guessed[0] == b'0' { b'1' } else { b'0' };
    let guessed = String::from_utf8(guessed).expect("hex");
    assert_eq!(
        status_of(
            &f,
            request(
                "GET",
                &android_direct_url(&f, &f.item, &guessed),
                None,
                Value::Null
            )
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    // The native open-in grant route refuses a Jellyfin link outright.
    assert_eq!(
        status_of(
            &f,
            request(
                "GET",
                &format!("/api/v1/grants/{tag}/content"),
                None,
                Value::Null
            )
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    // A presented login must be the one the link was issued under.
    assert_eq!(
        status_of(&f, request("GET", &url, Some(&f.token), Value::Null))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        play_event(&f, "/jellyfin/Sessions/Playing/Stopped", play, Some(3000)).await,
        StatusCode::NO_CONTENT
    );
    let (status, body) = status_of(&f, request("GET", &url, None, Value::Null)).await;
    assert_eq!(status, StatusCode::GONE);
    assert!(String::from_utf8_lossy(&body).contains("media_link_gone"));

    // Revoking the login ends a link that was never used.
    let second = negotiate(&f).await;
    let second_tag = second["MediaSources"][0]["ETag"]
        .as_str()
        .expect("second link")
        .to_owned();
    assert_ne!(second_tag, tag);
    let second_url = android_direct_url(&f, &f.item, &second_tag);
    assert_eq!(
        f.app
            .clone()
            .oneshot(request(
                "POST",
                "/jellyfin/Sessions/Logout",
                Some(&f.token),
                Value::Null
            ))
            .await
            .expect("logout")
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        status_of(&f, request("GET", &second_url, None, Value::Null))
            .await
            .0,
        StatusCode::GONE
    );
    // The switch still owns the whole surface.
    f.state
        .store
        .set_jellyfin_compatibility(false)
        .await
        .expect("disable");
    assert_eq!(
        status_of(&f, request("GET", &second_url, None, Value::Null))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}
#[tokio::test]
async fn jellyfin_direct_representation_matrix_keeps_native_range_and_validator_truth() {
    let f = playback_fixture().await;
    let negotiation = negotiate(&f).await;
    let url = mounted(
        negotiation["MediaSources"][0]["DirectStreamUrl"]
            .as_str()
            .expect("URL"),
    );
    let call = |range: Option<&str>, if_range: Option<&str>, method: &str| {
        let mut r = request(method, &url, Some(&f.token), Value::Null);
        if let Some(range) = range {
            r.headers_mut()
                .insert("range", range.parse().expect("range"));
        }
        if let Some(value) = if_range {
            r.headers_mut()
                .insert("if-range", value.parse().expect("if-range"));
        }
        f.app.clone().oneshot(r)
    };
    let full = call(None, None, "GET").await.expect("full");
    assert_eq!(full.status(), StatusCode::OK);
    assert_eq!(full.headers()["content-length"], "16");
    assert_eq!(full.headers()["accept-ranges"], "bytes");
    // Raw native files publish no strong validator, so none is invented.
    assert!(full.headers().get("etag").is_none());
    let unsatisfiable = call(Some("bytes=100-200"), None, "GET").await.expect("416");
    assert_eq!(unsatisfiable.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(unsatisfiable.headers()["content-range"], "bytes */16");
    let suffix = call(Some("bytes=-4"), None, "GET").await.expect("suffix");
    assert_eq!(suffix.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(suffix.headers()["content-range"], "bytes 12-15/16");
    // Without a validator an If-Range precondition cannot authorize a part.
    let guarded = call(Some("bytes=2-5"), Some("\"anything\""), "GET")
        .await
        .expect("if-range");
    assert_eq!(guarded.status(), StatusCode::OK);
    assert_eq!(
        guarded
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes()
            .len(),
        16
    );
    let head = call(Some("bytes=2-5"), None, "HEAD").await.expect("head");
    assert_eq!(head.status(), StatusCode::OK);
    assert_eq!(head.headers()["content-length"], "16");
}
#[tokio::test]
async fn jellyfin_switch_off_ends_negotiated_plays_even_after_it_is_turned_back_on() {
    let f = playback_fixture().await;
    let negotiation = negotiate(&f).await;
    let play = negotiation["PlaySessionId"].as_str().expect("play");
    let tag = negotiation["MediaSources"][0]["ETag"]
        .as_str()
        .expect("link")
        .to_owned();
    let url = mounted(
        negotiation["MediaSources"][0]["DirectStreamUrl"]
            .as_str()
            .expect("URL"),
    );
    for enabled in [false, true] {
        f.state
            .store
            .set_jellyfin_compatibility(enabled)
            .await
            .expect("save switch");
    }
    assert_eq!(
        status_of(&f, request("GET", &url, Some(&f.token), Value::Null))
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        status_of(
            &f,
            request(
                "GET",
                &android_direct_url(&f, &f.item, &tag),
                None,
                Value::Null
            )
        )
        .await
        .0,
        StatusCode::GONE
    );
    assert_eq!(
        play_event(&f, "/jellyfin/Sessions/Playing", play, Some(1000)).await,
        StatusCode::CONFLICT
    );
    // Stop still cleans an old-generation play up: it ends and releases
    // it, but writes no progress through it.
    assert_eq!(
        play_event(&f, "/jellyfin/Sessions/Playing/Stopped", play, Some(4000)).await,
        StatusCode::NO_CONTENT
    );
    let scope = f
        .state
        .store
        .jellyfin_login_scope(plurx_core::auth::hash_token(&f.token))
        .await
        .expect("scope")
        .expect("login");
    let stopped = f
        .state
        .store
        .jellyfin_play(play, &scope)
        .await
        .expect("read")
        .expect("tombstone");
    assert_eq!(stopped.state, "ended");
    let grant = f
        .state
        .store
        .file_grant_by_id(stopped.direct_grant_id.as_deref().expect("grant"))
        .await
        .expect("grant")
        .expect("grant");
    assert!(grant.revoked_at.is_some());
    assert!(f
        .state
        .store
        .watch_state(scope.user_id, f.native_item)
        .await
        .expect("watch")
        .is_none_or(|watch| watch.position_ms != 4000));
    // A facade login is not a native bearer.
    let native = f
        .app
        .clone()
        .oneshot(request("GET", "/api/v1/me", Some(&f.token), Value::Null))
        .await
        .expect("native call");
    assert_eq!(native.status(), StatusCode::UNAUTHORIZED);
    // A fresh negotiation under the current generation plays normally.
    let fresh = negotiate(&f).await;
    let fresh_url = &mounted(
        fresh["MediaSources"][0]["DirectStreamUrl"]
            .as_str()
            .expect("fresh URL"),
    );
    assert_eq!(
        status_of(&f, request("GET", fresh_url, Some(&f.token), Value::Null))
            .await
            .0,
        StatusCode::OK
    );
}
#[tokio::test]
async fn jellyfin_stop_without_position_and_external_edit_never_write_zero_or_restore_progress() {
    let f = playback_fixture().await;
    let first = negotiate(&f).await;
    let play = first["PlaySessionId"].as_str().expect("play");
    assert_eq!(
        play_event(&f, "/jellyfin/Sessions/Playing", play, Some(1000)).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        play_event(&f, "/jellyfin/Sessions/Playing/Stopped", play, None).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        f.state
            .store
            .watch_state(1, f.native_item)
            .await
            .expect("watch")
            .expect("watch")
            .position_ms,
        1000
    );
    let second = negotiate(&f).await;
    let play = second["PlaySessionId"].as_str().expect("second play");
    assert_eq!(
        play_event(&f, "/jellyfin/Sessions/Playing", play, Some(2000)).await,
        StatusCode::NO_CONTENT
    );
    f.state
        .store
        .set_watched_tree(1, f.native_item, false)
        .await
        .expect("external unwatch");
    let edited = f
        .state
        .store
        .watch_state(1, f.native_item)
        .await
        .expect("edit");
    assert_eq!(
        play_event(&f, "/jellyfin/Sessions/Playing/Stopped", play, Some(8000)).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        f.state
            .store
            .watch_state(1, f.native_item)
            .await
            .expect("edit preserved"),
        edited
    );
}
#[tokio::test]
async fn jellyfin_failed_final_releases_exact_resources_and_retry_commits_before_success() {
    let f = playback_fixture().await;
    let negotiation = negotiate(&f).await;
    let play = negotiation["PlaySessionId"].as_str().expect("play");
    assert_eq!(
        play_event(&f, "/jellyfin/Sessions/Playing", play, Some(1000)).await,
        StatusCode::NO_CONTENT
    );
    let scope = f
        .state
        .store
        .jellyfin_login_scope(plurx_core::auth::hash_token(&f.token))
        .await
        .expect("scope")
        .expect("scope");
    let binding = f
        .state
        .store
        .jellyfin_play(play, &scope)
        .await
        .expect("binding")
        .expect("binding");
    let conn =
        rusqlite::Connection::open(f.root.path().join("state.db")).expect("fault connection");
    conn.execute_batch("CREATE TRIGGER injected_final_failure BEFORE INSERT ON watch_state BEGIN SELECT RAISE(ABORT,'injected final failure'); END;").expect("inject");
    assert_eq!(
        play_event(&f, "/jellyfin/Sessions/Playing/Stopped", play, Some(8000)).await,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(
        f.state
            .store
            .jellyfin_play(play, &scope)
            .await
            .expect("retry binding")
            .expect("retry binding")
            .state,
        "active"
    );
    let grant = f
        .state
        .store
        .file_grant_by_id(binding.direct_grant_id.as_deref().expect("grant"))
        .await
        .expect("grant")
        .expect("grant");
    assert!(grant.revoked_at.is_some());
    assert!(f.state.direct_plays.list().is_empty());
    assert_eq!(
        f.state
            .store
            .watch_state(1, f.native_item)
            .await
            .expect("old watch")
            .expect("watch")
            .position_ms,
        1000
    );
    conn.execute_batch("DROP TRIGGER injected_final_failure;")
        .expect("repair");
    assert_eq!(
        play_event(&f, "/jellyfin/Sessions/Playing/Stopped", play, Some(8000)).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        f.state
            .store
            .watch_state(1, f.native_item)
            .await
            .expect("durable final")
            .expect("watch")
            .position_ms,
        8000
    );
    assert_eq!(
        f.state
            .store
            .jellyfin_play(play, &scope)
            .await
            .expect("terminal")
            .expect("terminal")
            .state,
        "ended"
    );
}
#[tokio::test]
async fn jellyfin_negotiation_keeps_profiles_separate_and_rejects_unknown_constraints_before_allocation(
) {
    let f = playback_fixture().await;
    let path = format!("/jellyfin/Items/{}/PlaybackInfo", f.item);
    let direct = json!({"Type":"Video","Container":"mp4","VideoCodec":"h264","AudioCodec":"aac"});
    for profile in [
        json!({"DirectPlayProfiles":[{"Type":"Video","Container":"mp4","VideoCodec":"hevc","AudioCodec":"aac"},{"Type":"Video","Container":"mkv","VideoCodec":"h264","AudioCodec":"ac3"}]}),
        json!({"DirectPlayProfiles":[direct.clone()],"CodecProfiles":[{"Type":"Video","Conditions":[{"Property":"UnknownCapability","Condition":"Equals","Value":"true"}]}]}),
        json!({"DirectPlayProfiles":[direct.clone()],"CodecProfiles":"invalid constraints"}),
        json!({"DirectPlayProfiles":[direct.clone()],"MaxStaticBitrate":10_000}),
    ] {
        let (status,body)=json_call(&f.app,request("POST",&path,Some(&f.token),json!({"UserId":f.user,"MediaSourceId":f.source,"EnableDirectPlay":true,"MaxStreamingBitrate":1_000_000,"DeviceProfile":profile}))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["ErrorCode"], "NotSupported");
        assert!(body["PlaySessionId"].is_null());
    }
    let conn = rusqlite::Connection::open(f.root.path().join("state.db")).expect("inspect");
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM jellyfin_plays", [], |row| row
            .get::<_, i64>(0))
            .expect("count"),
        0
    );
    let (status,body)=json_call(&f.app,request("POST",&path,Some(&f.token),json!({"MediaSourceId":f.source,"DeviceProfile":{"DirectPlayProfiles":[{"Type":"Video","Container":"mp4","VideoCodec":"h264","AudioCodec":"ac3"},direct]}}))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["PlaySessionId"].is_string(),
        "an independently valid second profile wins"
    );
    let (status, _) = json_call(
        &f.app,
        request(
            "POST",
            &path,
            Some(&f.token),
            json!({"MediaSourceId":f.source,"UnknownPlaybackLimit":1}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
#[tokio::test]
async fn jellyfin_codec_conditions_use_exact_probed_source_and_selected_audio_before_allocation() {
    let f = playback_fixture().await;
    for (kind, codec, property, value, expected) in [
        ("Video", "h264", "Height", "1080", true),
        ("Video", "h264", "Height", "720", false),
        ("VideoAudio", "aac", "AudioChannels", "2", true),
        ("VideoAudio", "aac", "AudioChannels", "1", false),
        ("Video", "h264", "VideoLevel", "52", false),
        ("Video", "-hevc", "Height", "720", false),
        ("Video", "-h264", "Height", "720", true),
    ] {
        let body = json!({"UserId":f.user,"AudioStreamIndex":0,"SubtitleStreamIndex":-1,
                "DeviceProfile":{"DirectPlayProfiles":[{"Type":"Video","Container":"mp4","VideoCodec":"h264","AudioCodec":"aac"}],
                    "CodecProfiles":[{"Type":kind,"Codec":codec,"Container":"mp4",
                        "Conditions":[{"Property":property,"Condition":"LessThanEqual","Value":value,"IsRequired":true}]}]}});
        let (status, answer) = json_call(
            &f.app,
            request(
                "POST",
                &format!("/jellyfin/Items/{}/PlaybackInfo", f.item),
                Some(&f.token),
                body,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            answer["PlaySessionId"].is_string(),
            expected,
            "{property} {value}"
        );
    }
}
#[tokio::test]
async fn jellyfin_ancillary_routes_share_native_markers_and_refuse_unbound_requests() {
    let f = playback_fixture().await;
    let id = f
        .state
        .store
        .jellyfin_resolve_entity(JellyfinEntityKind::File, &f.source)
        .await
        .expect("mapping")
        .expect("file");
    f.state
        .store
        .merge_file_probe_chapters(
            id,
            &json!([
                {"start_time":"0.000","end_time":"10.000","tags":{"title":"Intro"}},
                {"start_time":"90.000","end_time":"100.000","tags":{"title":"Credits"}}
            ])
            .to_string(),
        )
        .await
        .expect("chapters");
    let path = format!("/jellyfin/MediaSegments/{}", f.item);
    let (status, body) = json_call(&f.app, request("GET", &path, Some(&f.token), json!({}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["TotalRecordCount"], 2);
    assert_eq!(body["Items"][0]["Type"], "Intro");
    assert_eq!(body["Items"][0]["StartTicks"], 0);
    assert_eq!(body["Items"][0]["EndTicks"], 100_000_000);
    let (_, filtered) = json_call(
        &f.app,
        request(
            "GET",
            &format!("{path}?includeSegmentTypes=Outro"),
            Some(&f.token),
            json!({}),
        ),
    )
    .await;
    assert_eq!(filtered["TotalRecordCount"], 1);
    assert_eq!(filtered["Items"][0], body["Items"][1]);
    let intro = format!("/jellyfin/Items/{}/Intros", f.item);
    let (status, empty) =
        json_call(&f.app, request("GET", &intro, Some(&f.token), json!({}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(empty["TotalRecordCount"], 0);
    for (path, token, expected) in [
        (path.clone(), None, StatusCode::UNAUTHORIZED),
        (
            format!("{path}?includeSegmentTypes=MadeUp"),
            Some(f.token.as_str()),
            StatusCode::BAD_REQUEST,
        ),
        (
            format!("{intro}?UserId={}", "1".repeat(32)),
            Some(f.token.as_str()),
            StatusCode::FORBIDDEN,
        ),
        (
            format!("/jellyfin/MediaSegments/{}", "1".repeat(32)),
            Some(f.token.as_str()),
            StatusCode::NOT_FOUND,
        ),
    ] {
        let (status, _) = json_call(&f.app, request("GET", &path, token, json!({}))).await;
        assert_eq!(status, expected, "{path}");
    }
    f.state
        .store
        .set_jellyfin_compatibility(false)
        .await
        .expect("disable");
    let (status, _) = json_call(&f.app, request("GET", &path, Some(&f.token), json!({}))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn jellyfin_subtitle_native_failures_and_bitmap_refusals_are_not_empty_success() {
    use plurx_core::domain::{ProbeResult, SubtitleStream};
    let f = playback_fixture().await;
    let path = f
        .root
        .path()
        .canonicalize()
        .expect("root")
        .join("movie.mp4");
    for (codec, expected) in [
        ("subrip", StatusCode::INTERNAL_SERVER_ERROR),
        ("hdmv_pgs_subtitle", StatusCode::BAD_REQUEST),
    ] {
        f.state
            .store
            .upsert_file(
                f.native_item,
                path.to_str().expect("path"),
                16,
                1,
                &ProbeResult {
                    duration_ms: Some(100_000),
                    container: Some("mp4".into()),
                    subtitle_streams: vec![SubtitleStream {
                        index: 2,
                        codec: codec.into(),
                        language: None,
                        title: None,
                        default: false,
                        forced: false,
                        hearing_impaired: false,
                    }],
                    raw_json: Some(
                        json!({"streams":[{"index":2,"codec_type":"subtitle","codec_name":codec}]})
                            .to_string(),
                    ),
                    ..Default::default()
                },
            )
            .await
            .expect("probe");
        for format in ["vtt", "srt"] {
            let route = format!(
                "/jellyfin/Videos/{}/{}/Subtitles/2/Stream.{format}",
                f.item, f.source
            );
            let (status, body) =
                json_call(&f.app, request("GET", &route, Some(&f.token), json!({}))).await;
            assert_eq!(status, expected, "{body}");
        }
    }
}

#[tokio::test]
async fn jellyfin_subtitle_aliases_reuse_native_extraction_with_global_indices_and_source_times() {
    use plurx_core::testfixtures;
    testfixtures::require_ffmpeg();
    let f = playback_fixture().await;
    let caption = f.root.path().join("source.srt");
    std::fs::write(
        &caption,
        "1\n00:00:00,000 --> 00:00:02,125\nFirst\n\n2\n00:00:03,456 --> 00:00:05,789\nSecond\n",
    )
    .expect("caption");
    let movie = f
        .root
        .path()
        .canonicalize()
        .expect("canonical fixture root")
        .join("movie.mp4");
    let staged = f.root.path().join("captions.mkv");
    testfixtures::run(
        std::process::Command::new(testfixtures::ffmpeg())
            .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-i"])
            .arg(testfixtures::source("clean-cra"))
            .arg("-i")
            .arg(&caption)
            .args([
                "-map", "0:v:0", "-map", "0:a:0?", "-map", "1:0", "-c", "copy", "-c:s", "srt",
            ])
            .arg(&staged),
    );
    std::fs::rename(&staged, &movie).expect("replace fixture source");
    let probe = plurx_core::scan::probe::probe(&movie)
        .await
        .expect("real probe");
    let raw: Value =
        serde_json::from_str(probe.raw_json.as_deref().expect("raw probe")).expect("probe JSON");
    let global_index = raw["streams"]
        .as_array()
        .expect("streams")
        .iter()
        .find(|s| s["codec_type"] == "subtitle")
        .expect("caption stream")["index"]
        .as_i64()
        .expect("global index");
    assert!(
        global_index > 0,
        "subtitle zero ordinal must not be confused with its global stream index"
    );
    let metadata = std::fs::metadata(&movie).expect("source identity");
    let mtime = metadata
        .modified()
        .expect("mtime")
        .duration_since(std::time::UNIX_EPOCH)
        .expect("epoch")
        .as_secs() as i64;
    f.state
        .store
        .upsert_file(
            f.native_item,
            movie.to_str().expect("path"),
            metadata.len() as i64,
            mtime,
            &probe,
        )
        .await
        .expect("updated probe");
    for (format, content_type, timing) in [
        ("vtt", "text/vtt", "00:00.000 --> 00:02.125"),
        (
            "srt",
            "application/x-subrip",
            "00:00:00,000 --> 00:00:02,125",
        ),
    ] {
        let path = format!(
            "/jellyfin/Videos/{}/{}/Subtitles/{global_index}/Stream.{format}",
            f.item, f.source
        );
        let response = f
            .app
            .clone()
            .oneshot(request("GET", &path, Some(&f.token), json!({})))
            .await
            .expect("subtitle");
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers()["content-type"]
            .to_str()
            .expect("type")
            .starts_with(content_type));
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        let text = std::str::from_utf8(&bytes).expect("subtitle UTF8");
        assert!(text.contains(timing), "{text}");
        assert!(text.contains("First") && text.contains("Second"));
        let response = f
            .app
            .clone()
            .oneshot(request("GET", &path, None, json!({})))
            .await
            .expect("auth refusal");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    // The five-segment form both pinned clients request (J0 traces):
    // Infuse with its login header and `Range: bytes=0-`, Android TV with
    // only the `ApiKey` from `DeliveryUrl`, in `subrip` or `vtt`.
    let subtitle = |path: String, token: Option<&str>| {
        let mut r = request("GET", &path, token, json!({}));
        r.headers_mut()
            .insert("range", "bytes=0-".parse().expect("range"));
        let app = f.app.clone();
        async move {
            let response = app.oneshot(r).await.expect("subtitle");
            let status = response.status();
            let kind = response
                .headers()
                .get("content-type")
                .map(|v| v.to_str().expect("type").to_owned())
                .unwrap_or_default();
            let bytes = response
                .into_body()
                .collect()
                .await
                .expect("body")
                .to_bytes();
            (
                status,
                kind,
                String::from_utf8(bytes.to_vec()).expect("UTF8"),
            )
        }
    };
    let base = format!(
        "/jellyfin/Videos/{}/{}/Subtitles/{global_index}",
        f.item, f.source
    );
    let (status, kind, text) = subtitle(format!("{base}/0/Stream.srt"), Some(&f.token)).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(kind.starts_with("application/x-subrip"));
    assert!(text.contains("00:00:00,000 --> 00:00:02,125"), "{text}");
    for (format, timing) in [
        ("subrip", "00:00:03,456 --> 00:00:05,789"),
        ("vtt", "00:03.456 --> 00:05.789"),
    ] {
        let (status, _, text) =
            subtitle(format!("{base}/0/Stream.{format}?ApiKey={}", f.token), None).await;
        assert_eq!(status, StatusCode::OK, "{format}: {text}");
        assert!(text.contains(timing), "{format}: {text}");
    }
    // A start position is Jellyfin's window: earlier cues dropped, the
    // rest moved by the start unless CopyTimestamps keeps source times.
    let (status, _, text) = subtitle(format!("{base}/30000000/Stream.vtt"), Some(&f.token)).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(!text.contains("First"), "{text}");
    assert!(
        text.contains("00:00:00.456 --> 00:00:02.789\nSecond"),
        "{text}"
    );
    let (status, _, text) = subtitle(
        format!("{base}/Stream.srt?StartPositionTicks=30000000&CopyTimestamps=true"),
        Some(&f.token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(text, "1\n00:00:03,456 --> 00:00:05,789\nSecond\n\n");
    assert_eq!(
        subtitle(format!("{base}/0/Stream.ass"), Some(&f.token))
            .await
            .0,
        StatusCode::NOT_FOUND,
        "only implemented formats are served"
    );
    assert_eq!(
        subtitle(format!("{base}/0/Stream.vtt"), None).await.0,
        StatusCode::UNAUTHORIZED
    );
    let invalid = format!(
        "/jellyfin/Videos/{}/{}/Subtitles/0/Stream.vtt",
        f.item, f.source
    );
    let response = f
        .app
        .clone()
        .oneshot(request("GET", &invalid, Some(&f.token), json!({})))
        .await
        .expect("wrong index");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let old = f
        .state
        .store
        .get_item(f.native_item)
        .await
        .expect("item")
        .expect("live");
    let other = f
        .state
        .store
        .insert_item(&plurx_core::domain::NewItem {
            library_id: old.library_id,
            kind: plurx_core::domain::ItemKind::Movie,
            parent_id: None,
            title: "Other source membership".into(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("other item");
    f.state
        .store
        .upsert_file(
            other,
            movie.to_str().expect("path"),
            metadata.len() as i64,
            mtime,
            &probe,
        )
        .await
        .expect("move source");
    let route = format!(
        "/jellyfin/Videos/{}/{}/Subtitles/{global_index}/Stream.vtt",
        f.item, f.source
    );
    let (status, _) = json_call(&f.app, request("GET", &route, Some(&f.token), json!({}))).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a moved source cannot serve through its old item"
    );
}

#[tokio::test]
async fn jellyfin_logout_releases_only_presented_login_and_preserves_other_device() {
    let f = playback_fixture().await;
    let info = negotiate(&f).await;
    let play = info["PlaySessionId"].as_str().expect("play");
    assert_eq!(
        play_event(&f, "/jellyfin/Sessions/Playing", play, Some(1000)).await,
        StatusCode::NO_CONTENT
    );
    let mut login = request(
        "POST",
        "/jellyfin/Users/AuthenticateByName",
        None,
        json!({"Username":"catalog-admin","Pw":"supersecret"}),
    );
    login.headers_mut().insert("x-emby-authorization", "MediaBrowser Client=\"Jellyfin+Android+TV\", DeviceId=\"logout-other-device\", Version=\"0.19.10\"".parse().expect("metadata"));
    let (status, other) = json_call(&f.app, login).await;
    assert_eq!(status, StatusCode::OK);
    let other_token = other["AccessToken"].as_str().expect("other login");
    let scope = f
        .state
        .store
        .jellyfin_login_scope(plurx_core::auth::hash_token(&f.token))
        .await
        .expect("scope")
        .expect("scope");
    let active = f
        .state
        .store
        .jellyfin_play(play, &scope)
        .await
        .expect("binding")
        .expect("binding");
    let other_scope = f
        .state
        .store
        .jellyfin_login_scope(plurx_core::auth::hash_token(other_token))
        .await
        .expect("other scope")
        .expect("other scope");
    f.state
        .store
        .create_file_grant(plurx_core::store::NewFileGrant {
            id: "other-device-grant".into(),
            token_hash: plurx_core::auth::hash_token("other-device-secret"),
            file_id: active.negotiation.file_id,
            user_id: 1,
            source_token_hash: other_scope.token_digest,
            created_at: 1,
            expires_at: i64::MAX,
        })
        .await
        .expect("other native grant");
    let response = f
        .app
        .clone()
        .oneshot(request(
            "POST",
            "/jellyfin/Sessions/Logout",
            Some(&f.token),
            json!({}),
        ))
        .await
        .expect("logout");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(f
        .state
        .store
        .jellyfin_login_scope(scope.token_digest.clone())
        .await
        .expect("revoked login")
        .is_none());
    let ended = f
        .state
        .store
        .jellyfin_play(play, &scope)
        .await
        .expect("tombstone")
        .expect("tombstone");
    assert_eq!(ended.state, "ended");
    assert!(f
        .state
        .store
        .file_grant_by_id(active.direct_grant_id.as_deref().expect("grant"))
        .await
        .expect("grant")
        .expect("grant")
        .revoked_at
        .is_some());
    assert!(f
        .state
        .store
        .file_grant_by_id("other-device-grant")
        .await
        .expect("other grant")
        .expect("other grant")
        .revoked_at
        .is_none());
    let (status, _) = json_call(
        &f.app,
        request("GET", "/jellyfin/Users/Me", Some(other_token), json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = json_call(
        &f.app,
        request("GET", "/jellyfin/Users/Me", Some(&f.token), json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
#[tokio::test]
async fn jellyfin_play_events_reject_other_login_even_when_it_claims_the_owner_device() {
    let f = playback_fixture().await;
    let negotiation = negotiate(&f).await;
    let play = negotiation["PlaySessionId"].as_str().expect("play");
    assert_eq!(
        play_event(&f, "/jellyfin/Sessions/Playing", play, Some(1000)).await,
        StatusCode::NO_CONTENT
    );
    let mut login = request(
        "POST",
        "/jellyfin/Users/AuthenticateByName",
        None,
        json!({"Username":"catalog-admin","Pw":"supersecret"}),
    );
    login.headers_mut().insert("x-emby-authorization","MediaBrowser Client=\"Jellyfin+Android+TV\", DeviceId=\"other-playback-device\", Version=\"0.19.10\"".parse().expect("metadata"));
    let (status, other) = json_call(&f.app, login).await;
    assert_eq!(status, StatusCode::OK);
    let token = other["AccessToken"].as_str().expect("token");
    for route in [
        "/jellyfin/Sessions/Playing/Progress",
        "/jellyfin/Sessions/Playing/Stopped",
    ] {
        let mut report = request(
            "POST",
            route,
            Some(token),
            json!({"ItemId":f.item,"MediaSourceId":f.source,"PlaySessionId":play,"PositionTicks":80_000_000,"DeviceId":"catalog-contract"}),
        );
        report.headers_mut().insert("x-emby-authorization","MediaBrowser Client=\"Jellyfin+Android+TV\", DeviceId=\"catalog-contract\", Version=\"0.19.10\"".parse().expect("claimed owner"));
        let (status, _) = json_call(&f.app, report).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
    assert_eq!(
        play_event(&f, "/jellyfin/Sessions/Playing/Stopped", play, Some(3000)).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        f.state
            .store
            .watch_state(1, f.native_item)
            .await
            .expect("watch")
            .expect("watch")
            .position_ms,
        3000
    );
}
#[tokio::test]
async fn jellyfin_disabled_bare_paths_and_mutations_are_json_404_and_native_shell_survives() {
    let (app, _) = super::super::tests::test_app_with_state();
    for (method, path) in [
        ("GET", "/jellyfin"),
        ("GET", "/jellyfin/"),
        ("GET", "/jellyfin/System/Info/Public"),
        ("POST", "/jellyfin/Users/AuthenticateByName"),
        ("POST", "/jellyfin/Items"),
        ("GET", "/jellyfin/unknown"),
    ] {
        let (status, body) = json_call(&app, request(method, path, None, json!({}))).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert!(body["error"].is_string());
    }
    let response = app
        .oneshot(
            Request::builder()
                .uri("/")
                .body(Body::empty())
                .expect("root request"),
        )
        .await
        .expect("root response");
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers()["content-type"]
        .to_str()
        .expect("root content type")
        .starts_with("text/html"));
}
/// A client given only a host reaches Jellyfin's standard port at the
/// root. That listener maps every request under the mount, so it serves
/// exactly the facade: the same answers as `/jellyfin` on Plurx's port, a
/// JSON 404 where the native API, web app or Plex routes would be, and
/// nothing while compatibility is off.
#[tokio::test]
async fn jellyfin_standard_port_serves_only_the_facade_at_the_server_root() {
    let (app, _state) = super::super::tests::test_app_with_state();
    let root = super::standard_port_app(app.clone());
    let admin = setup(&app).await;
    let (status, _) = json_call(
        &root,
        request("GET", "/System/Info/Public", None, Value::Null),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "off is off on every port");
    let (status, _) = json_call(
        &app,
        request(
            "PUT",
            "/api/v1/settings",
            Some(&admin),
            json!({"jellyfin_compatibility_enabled":true}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    for path in ["/System/Info/Public", "/jellyfin/System/Info/Public"] {
        let (status, info) = json_call(&root, request("GET", path, None, Value::Null)).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(info["Version"], "10.11.11", "{path}");
    }
    let mut login = request(
        "POST",
        "/Users/AuthenticateByName",
        None,
        json!({"Username":"catalog-admin","Pw":"supersecret"}),
    );
    login.headers_mut().insert(
        "x-emby-authorization",
        "MediaBrowser Client=\"Infuse-Library\", DeviceId=\"root-port\", Version=\"8.5.6\""
            .parse()
            .expect("metadata"),
    );
    let (status, session) = json_call(&root, login).await;
    assert_eq!(status, StatusCode::OK, "{session}");
    let token = session["AccessToken"].as_str().expect("token");
    let (status, me) =
        json_call(&root, request("GET", "/Users/Me", Some(token), Value::Null)).await;
    assert_eq!(status, StatusCode::OK, "{me}");
    // The native API, the web app and the Plex routes are not on this
    // port, whatever form the request target takes.
    for path in [
        "/",
        "/api/v1/server",
        "/api/v1/me",
        "/web/index.html",
        "/identity",
        "/library/sections",
        "*",
        "http://elsewhere/api/v1/me",
        "//api/v1/me",
        "/%6Aellyfin/../api/v1/me",
        "/Jellyfin/System/Info/Public",
        "/jellyfinfoo",
    ] {
        let (status, _) = json_call(&root, request("GET", path, Some(&admin), Value::Null)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
    }
}

/// The store reads at most 500 rows at a time; Jellyfin has no page
/// ceiling and answers a request without `Limit` with every row. Larger
/// pages are assembled from consecutive reads without a gap or repeat.
#[tokio::test]
async fn jellyfin_catalog_pages_past_the_store_read_bound() {
    use plurx_core::domain::{ItemKind, LibraryKind, NewItem, NewLibrary};
    let (app, state) = super::super::tests::test_app_with_state();
    let admin = setup(&app).await;
    let (status, _) = json_call(
        &app,
        request(
            "PUT",
            "/api/v1/settings",
            Some(&admin),
            json!({"jellyfin_compatibility_enabled":true}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let library = state
        .store
        .create_library(&NewLibrary {
            name: "Many films".into(),
            kind: LibraryKind::Movies,
            paths: vec!["/private/many".into()],
            anime: false,
        })
        .await
        .expect("library");
    for n in 0..520 {
        state
            .store
            .insert_item(&NewItem {
                library_id: library.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: format!("Film {n:04}"),
                year: Some(2024),
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
    }
    let (token, uid) = facade_login(&app).await;
    let base = format!("/jellyfin/Users/{uid}/Items?Recursive=true&IncludeItemTypes=Movie");
    for (query, start, count) in [
        ("", 0, 520),
        ("&Limit=510", 0, 510),
        ("&StartIndex=495&Limit=20", 495, 20),
        ("&StartIndex=515", 515, 5),
    ] {
        let (status, page) = json_call(
            &app,
            request("GET", &format!("{base}{query}"), Some(&token), Value::Null),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{query}");
        assert_eq!(page["TotalRecordCount"], 520, "{query}");
        let names = page["Items"]
            .as_array()
            .expect("items")
            .iter()
            .map(|item| item["Name"].as_str().expect("name").to_owned())
            .collect::<Vec<_>>();
        let expected = (start..start + count)
            .map(|n| format!("Film {n:04}"))
            .collect::<Vec<_>>();
        assert_eq!(names, expected, "{query}");
    }
    // Limit=0 still counts; Latest keeps Jellyfin's default of 20.
    let (status, page) = json_call(
        &app,
        request("GET", &format!("{base}&Limit=0"), Some(&token), Value::Null),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["TotalRecordCount"], 520);
    assert!(page["Items"].as_array().expect("items").is_empty());
    let (status, latest) = json_call(
        &app,
        request(
            "GET",
            &format!("/jellyfin/Users/{uid}/Items/Latest"),
            Some(&token),
            Value::Null,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(latest.as_array().expect("latest").len(), 20);
}

#[tokio::test]
async fn jellyfin_connection_catalog_preserves_zero_paging_and_refuses_user_and_credential_conflicts(
) {
    use plurx_core::domain::{ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult};
    let (app, state) = super::super::tests::test_app_with_state();
    let admin = setup(&app).await;
    let (status, saved) = json_call(
        &app,
        request(
            "PUT",
            "/api/v1/settings",
            Some(&admin),
            json!({"jellyfin_compatibility_enabled":true}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["jellyfin_compatibility_enabled"], true);
    // Clients gate on the protocol version before they ever sign in: a
    // Plurx build version here reads as an unsupported Jellyfin server.
    let (status, info) = json_call(
        &app,
        request("GET", "/jellyfin/System/Info/Public", None, Value::Null),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(info["Version"], "10.11.11");
    assert_eq!(info["ProductName"], "Jellyfin Server");
    assert_eq!(info["StartupWizardCompleted"], true);
    assert!(info["ServerName"]
        .as_str()
        .is_some_and(|name| !name.is_empty()));
    assert!(info["Id"].as_str().is_some_and(|id| !id.is_empty()));
    // Every Infuse share mode is the Infuse family; an unknown app is not.
    for (client, expected) in [
        ("Infuse-Library", StatusCode::OK),
        ("Infuse-Direct", StatusCode::OK),
        ("Infuse", StatusCode::OK),
        ("Some Other App", StatusCode::BAD_REQUEST),
    ] {
        let mut login = request(
            "POST",
            "/jellyfin/Users/AuthenticateByName",
            None,
            json!({"Username":"catalog-admin","Pw":"supersecret"}),
        );
        login.headers_mut().insert(
                "x-emby-authorization",
                format!("MediaBrowser Client=\"{client}\", DeviceId=\"family-{expected}\", Version=\"8.5.6\"")
                    .parse()
                    .expect("metadata"),
            );
        let (status, _) = json_call(&app, login).await;
        assert_eq!(status, expected, "{client}");
    }
    let library = state
        .store
        .create_library(&NewLibrary {
            name: "Contract films".into(),
            kind: LibraryKind::Movies,
            paths: vec!["/private/native-only".into()],
            anime: false,
        })
        .await
        .expect("library");
    let mut native_items = Vec::new();
    for name in ["First", "Second"] {
        native_items.push(
            state
                .store
                .insert_item(&NewItem {
                    library_id: library.id,
                    kind: ItemKind::Movie,
                    parent_id: None,
                    title: name.into(),
                    year: Some(2024),
                    season_number: None,
                    episode_number: None,
                })
                .await
                .expect("item"),
        );
    }
    state
        .store
        .upsert_file(
            native_items[0],
            "/private/native-only/movie.mkv",
            100,
            1,
            &ProbeResult {
                duration_ms: Some(10000),
                container: Some("mkv".into()),
                ..Default::default()
            },
        )
        .await
        .expect("source");
    let (token, uid) = facade_login(&app).await;
    let (status, views) = json_call(
        &app,
        request(
            "GET",
            &format!("/jellyfin/Users/{uid}/Views"),
            Some(&token),
            Value::Null,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(views["TotalRecordCount"], 1);
    let parent = views["Items"][0]["Id"].as_str().expect("library wire");
    let base = format!("/jellyfin/Users/{uid}/Items?ParentId={parent}&SortBy=SortName");
    let (status, page) = json_call(
        &app,
        request(
            "GET",
            &format!("{base}&StartIndex=1&Limit=1"),
            Some(&token),
            Value::Null,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["StartIndex"], 1);
    assert_eq!(page["TotalRecordCount"], 2);
    assert_eq!(page["Items"][0]["Name"], "Second");
    assert_eq!(page["Items"][0]["UserData"]["PlaybackPositionTicks"], 0);
    let (_, first) = json_call(
        &app,
        request("GET", &format!("{base}&Limit=1"), Some(&token), Value::Null),
    )
    .await;
    assert!(!first.to_string().contains("native-only"));
    assert_eq!(first["Items"][0]["MediaSourceCount"], 1);
    for start in ["2", "9223372036854775807"] {
        let (_, empty) = json_call(
            &app,
            request(
                "GET",
                &format!("{base}&StartIndex={start}&Limit=1"),
                Some(&token),
                Value::Null,
            ),
        )
        .await;
        assert_eq!(empty["TotalRecordCount"], 2);
        assert!(empty["Items"].as_array().expect("items").is_empty());
    }
    // Infuse's library mode and advanced browsing name kinds, orders and
    // filters the catalogue lacks. Jellyfin answers those requests, so
    // the facade narrows or degrades them; it never refuses them.
    let movie = first["Items"][0]["Id"].as_str().expect("item wire");
    let library_items = format!("/jellyfin/Users/{uid}/Items?ParentId={parent}");
    for (query, total) in [
            ("&IncludeItemTypes=Movie,BoxSet,Video&SortBy=SortName,Random,CommunityRating&SortOrder=Ascending&Recursive=true", 2),
            ("&IncludeItemTypes=BoxSet,Folder&Recursive=true", 0),
            ("&Limit=2000&Recursive=true", 2),
            ("&SortBy=DateCreated,SortName&Recursive=true&Fields=Genres,Overview&Filters=IsNotFolder&EnableUserData=true", 2),
        ] {
            let (status, page) = json_call(
                &app,
                request("GET", &format!("{library_items}{query}"), Some(&token), Value::Null),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{query}: {page}");
            assert_eq!(page["TotalRecordCount"], total, "{query}");
        }
    let (status, page) = json_call(
        &app,
        request(
            "GET",
            &format!("/jellyfin/Users/{uid}/Items?SeriesId={movie}&IncludeItemTypes=Episode"),
            Some(&token),
            Value::Null,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["TotalRecordCount"], 0);
    let (status, folders) = json_call(
        &app,
        request(
            "GET",
            &format!("/jellyfin/Users/{uid}/Items?IncludeItemTypes=CollectionFolder"),
            Some(&token),
            Value::Null,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{folders}");
    assert_eq!(folders["Items"][0]["Id"], parent);
    // Inside a library there are no library folders, and the views keep
    // the facade's user check.
    let (status, none) = json_call(
        &app,
        request(
            "GET",
            &format!("{library_items}&IncludeItemTypes=CollectionFolder&Recursive=true"),
            Some(&token),
            Value::Null,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{none}");
    assert_eq!(none["TotalRecordCount"], 0);
    let stranger = WireId::random().to_hex();
    let (status, _) = json_call(
        &app,
        request(
            "GET",
            &format!("/jellyfin/Items?IncludeItemTypes=CollectionFolder&UserId={stranger}"),
            Some(&token),
            Value::Null,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let image_path = format!(
        "/jellyfin/Items/{}/Images/Primary",
        first["Items"][0]["Id"].as_str().expect("item wire")
    );
    // Mapped artwork needs no login (approved 2026-10-03); this item has
    // none, so both callers get the same honest miss, never a 401.
    assert_eq!(
        json_call(&app, request("GET", &image_path, None, Value::Null))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        json_call(&app, request("GET", &image_path, Some(&token), Value::Null))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let other = WireId::random().to_hex();
    assert_eq!(
        json_call(
            &app,
            request(
                "GET",
                &format!("/jellyfin/Users/{other}/Views"),
                Some(&token),
                Value::Null
            )
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        json_call(
            &app,
            request(
                "GET",
                &format!("{base}&Limit=1&limit=2"),
                Some(&token),
                Value::Null
            )
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        json_call(
            &app,
            request(
                "GET",
                &format!("{base}&api_key=different"),
                Some(&token),
                Value::Null
            )
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        json_call(
            &app,
            request(
                "GET",
                "/jellyfin/Items",
                Some("plx_scoped_key"),
                Value::Null
            )
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        json_call(
            &app,
            request("POST", "/jellyfin/Items", Some(&token), json!({}))
        )
        .await
        .0,
        StatusCode::METHOD_NOT_ALLOWED
    );
    state
        .store
        .set_jellyfin_compatibility(false)
        .await
        .expect("disable");
    assert_eq!(
        json_call(&app, request("GET", &base, Some(&token), Value::Null))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}
#[test]
fn jellyfin_source_projection_keeps_global_zero_and_omits_relative_indices_and_native_paths() {
    let row = json!({"wire_id":WireId::random().to_hex(),"path":"/secret/file.mkv","probe_json":{"format":{"filename":"/secret/file.mkv"}},"duration_ms":null,"container":null,
            "streams":[{"index":0,"codec_type":"audio","codec_name":"aac","tags":{"language":"eng"},"disposition":{"default":1}},
                {"codec_type":"audio","codec_name":"aac"}]});
    let source = source_dto(&row).expect("source DTO");
    let encoded = serde_json::to_value(source).expect("DTO");
    assert_eq!(
        encoded["MediaStreams"].as_array().expect("streams").len(),
        1
    );
    assert_eq!(encoded["MediaStreams"][0]["Index"], 0);
    assert!(!encoded.to_string().contains("secret"));
    assert!(encoded.get("RunTimeTicks").is_none());
    assert_eq!(encoded["SupportsDirectPlay"], false);
}

fn subtitle_probe(container: &str) -> plurx_core::domain::ProbeResult {
    use plurx_core::domain::{AudioStream, ProbeResult, SubtitleStream};
    let track = |index: i64, codec: &str| SubtitleStream {
        index,
        codec: codec.into(),
        language: Some("eng".into()),
        title: None,
        default: false,
        forced: false,
        hearing_impaired: false,
    };
    ProbeResult {
            duration_ms: Some(100_000),
            container: Some(container.into()),
            video_codec: Some("h264".into()),
            video_codec_tag: Some("avc1".into()),
            video_profile: Some("Main".into()),
            width: Some(1920),
            height: Some(1080),
            bit_depth: Some(8),
            bitrate: Some(100_000),
            audio_streams: vec![AudioStream {
                index: 0,
                codec: "aac".into(),
                channel_layout: None,
                channels: Some(2),
                sample_rate: Some(48_000),
                language: None,
                title: None,
                default: true,
            }],
            subtitle_streams: vec![
                track(2, "subrip"),
                track(3, "hdmv_pgs_subtitle"),
                track(4, "ass"),
            ],
            raw_json: Some(
                json!({"streams":[
                    {"index":0,"codec_type":"audio","codec_name":"aac","channels":2,"sample_rate":"48000"},
                    {"index":1,"codec_type":"video","codec_name":"h264","width":1920,"height":1080},
                    {"index":2,"codec_type":"subtitle","codec_name":"subrip","tags":{"language":"eng"}},
                    {"index":3,"codec_type":"subtitle","codec_name":"hdmv_pgs_subtitle","tags":{"language":"eng"}},
                    {"index":4,"codec_type":"subtitle","codec_name":"ass","tags":{"language":"eng"}}]})
                .to_string(),
            ),
            ..Default::default()
        }
}

#[tokio::test]
async fn jellyfin_subtitle_delivery_follows_each_client_profile_with_a_requestable_url() {
    let f = playback_fixture().await;
    let path = f
        .root
        .path()
        .canonicalize()
        .expect("root")
        .join("movie.mp4");
    f.state
        .store
        .upsert_file(
            f.native_item,
            path.to_str().expect("path"),
            16,
            1,
            &subtitle_probe("mkv"),
        )
        .await
        .expect("probe");
    let ask = |subtitles: Value, selected: i64| {
        json!({"UserId":f.user,"MediaSourceId":f.source,"AudioStreamIndex":0,"SubtitleStreamIndex":selected,
                "DeviceProfile":{"DirectPlayProfiles":[{"Type":"Video","Container":"mkv","VideoCodec":"h264","AudioCodec":"aac"}],
                "SubtitleProfiles":subtitles}})
    };
    let negotiate_with = |body: Value| {
        let request = request(
            "POST",
            &format!("/jellyfin/Items/{}/PlaybackInfo", f.item),
            Some(&f.token),
            body,
        );
        let app = f.app.clone();
        async move { json_call(&app, request).await }
    };
    let streams = |body: &Value| {
        body["MediaSources"][0]["MediaStreams"]
            .as_array()
            .expect("streams")
            .iter()
            .filter(|s| s["Type"] == "Subtitle")
            .cloned()
            .collect::<Vec<_>>()
    };
    // Jellyfin Android TV 0.19.10: the direct file's own tracks are
    // embedded, the bitmap one under Jellyfin's codec name.
    let android = json!([
            {"Format":"vtt","Method":"Embed"},{"Format":"vtt","Method":"External"},{"Format":"vtt","Method":"Hls"},
            {"Format":"subrip","Method":"Embed"},{"Format":"subrip","Method":"External"},
            {"Format":"pgssub","Method":"Embed"},{"Format":"pgssub","Method":"Encode"}]);
    let (status, body) = negotiate_with(ask(android, -1)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["MediaSources"][0]["SupportsDirectPlay"], true);
    let tracks = streams(&body);
    assert_eq!(tracks.len(), 3);
    assert!(
        tracks[..2].iter().all(|s| s["DeliveryMethod"] == "Embed"),
        "{tracks:?}"
    );
    assert!(tracks[..2].iter().all(|s| s.get("DeliveryUrl").is_none()));
    // Android TV cannot render ASS itself: a converted VTT sidecar.
    assert_eq!(tracks[2]["DeliveryMethod"], "External", "{tracks:?}");
    assert_eq!(tracks[2]["IsTextSubtitleStream"], true);
    assert!(tracks[2]["DeliveryUrl"]
        .as_str()
        .is_some_and(|url| url.contains("/Subtitles/4/0/Stream.vtt?ApiKey=")));
    // Infuse 8.5.6: External VTT only. The text track becomes a sidecar at
    // the five-segment route both clients request; the bitmap one cannot.
    let infuse = json!([{"Format":"vtt","Method":"External","AllowChunkedResponse":true},
            {"Format":"ass","Method":"External"},{"Format":"ssa","Method":"External"}]);
    let (status, body) = negotiate_with(ask(infuse.clone(), -1)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let tracks = streams(&body);
    assert_eq!(tracks[0]["Index"], 2);
    assert_eq!(tracks[0]["DeliveryMethod"], "External");
    assert_eq!(tracks[0]["IsExternalUrl"], false);
    assert_eq!(
        tracks[0]["DeliveryUrl"],
        format!(
            "/Videos/{}/{}/Subtitles/2/0/Stream.vtt?ApiKey={}",
            f.item, f.source, f.token
        )
    );
    assert_eq!(tracks[1]["DeliveryMethod"], "Encode");
    assert!(tracks[1].get("DeliveryUrl").is_none());
    // ASS is text the native extractor converts: a sidecar, not a burn.
    assert_eq!(tracks[2]["DeliveryMethod"], "External");
    let (status, body) = negotiate_with(ask(infuse.clone(), 4)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["MediaSources"][0]["SupportsDirectPlay"], true,
        "a sidecar-capable selection keeps direct play: {body}"
    );
    // Selecting a track the direct file cannot deliver makes the play a
    // transcode; this profile offers none, so negotiation refuses.
    let (status, body) = negotiate_with(ask(infuse.clone(), 3)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ErrorCode"], "NotSupported", "{body}");
    // Infuse's own request (J0) declares no DirectPlayProfiles and asks
    // for HTTP direct play: it streams the file itself, tracks and all,
    // so even a bitmap selection stays a direct play.
    let mut own = infuse_request(&f);
    own["SubtitleStreamIndex"] = json!(3);
    own["DeviceProfile"]["SubtitleProfiles"] = infuse;
    let (status, body) = negotiate_with(own).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["MediaSources"][0]["SupportsDirectPlay"], true,
        "{body}"
    );
    // No profile list grants nothing.
    let (_, body) = negotiate_with(ask(Value::Null, -1)).await;
    assert!(streams(&body)
        .iter()
        .all(|s| s["DeliveryMethod"] == "Encode"));
}

/// Infuse 8.5.6's normal-play PlaybackInfo body (J0 `profiles[0]`).
fn infuse_request(f: &PlaybackFixture) -> Value {
    json!({"AutoOpenLiveStream":true,"IsPlayback":true,"UserId":f.user,"EnableDirectPlay":true,
            "MediaSourceId":f.source,"MaxStreamingBitrate":200_000_000,"DirectPlayProtocols":["Http"],
            "DeviceProfile":{"MaxStreamingBitrate":200_000_000,"MaxStaticBitrate":200_000_000,
                "MusicStreamingTranscodingBitrate":192_000,
                "TranscodingProfiles":[
                    {"MinSegments":1,"AudioCodec":"aac","Container":"aac","BreakOnNonKeyFrames":true,"MaxAudioChannels":"2","Type":"Audio","Context":"Streaming","Protocol":"hls"},
                    {"AudioCodec":"aac","MinSegments":1,"Container":"ts","ManifestSubtitles":"vtt","MaxAudioChannels":"2","BreakOnNonKeyFrames":true,"Type":"Video","Context":"Streaming","VideoCodec":"hevc,h264,av1","Protocol":"hls"}],
                "SubtitleProfiles":[{"AllowChunkedResponse":true,"Format":"vtt","Method":"External"},
                    {"Format":"ass","Method":"External"},{"Format":"ssa","Method":"External"}]}})
}
#[tokio::test]
async fn jellyfin_infuse_direct_request_without_a_play_id_uses_its_own_newest_direct_play() {
    let f = playback_fixture().await;
    // Infuse's PlaybackInfo, as J0 captured it: no DirectPlayProfiles,
    // `EnableDirectPlay` with `DirectPlayProtocols: ["Http"]`.
    let infuse_info = |body: Value| {
        let request = request(
            "POST",
            &format!("/jellyfin/Items/{}/PlaybackInfo", f.item),
            Some(&f.token),
            body,
        );
        let app = f.app.clone();
        async move { json_call(&app, request).await }
    };
    let (status, older) = infuse_info(infuse_request(&f)).await;
    assert_eq!(status, StatusCode::OK, "{older}");
    assert_eq!(
        older["MediaSources"][0]["SupportsDirectPlay"], true,
        "{older}"
    );
    assert!(older["MediaSources"][0].get("TranscodingUrl").is_none());
    let (_, newer) = infuse_info(infuse_request(&f)).await;
    let newest = newer["PlaySessionId"].as_str().expect("play");
    let mut odd = infuse_request(&f);
    odd["DirectPlayProtocols"] = json!(["Carrier-Pigeon"]);
    assert_eq!(infuse_info(odd).await.0, StatusCode::BAD_REQUEST);
    // The direct URL Infuse builds: login header, `MediaSourceId`, `Static`.
    let infuse = format!(
        "/jellyfin/Videos/{}/stream?MediaSourceId={}&Static=true",
        f.item, f.source
    );
    let mut ranged = request("GET", &infuse, Some(&f.token), Value::Null);
    ranged
        .headers_mut()
        .insert("range", "bytes=0-".parse().expect("range"));
    let (status, bytes) = status_of(&f, ranged).await;
    assert_eq!(
        status,
        StatusCode::PARTIAL_CONTENT,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    assert_eq!(bytes, b"0123456789abcdef");
    let scope = f
        .state
        .store
        .jellyfin_login_scope(plurx_core::auth::hash_token(&f.token))
        .await
        .expect("scope")
        .expect("login");
    let state_of = |play: String| {
        let store = std::sync::Arc::clone(&f.state.store);
        let scope = scope.clone();
        async move {
            store
                .jellyfin_play(&play, &scope)
                .await
                .expect("read")
                .expect("play")
                .state
        }
    };
    assert_eq!(state_of(newest.to_owned()).await, "active");
    assert_ne!(
        state_of(older["PlaySessionId"].as_str().expect("play").to_owned()).await,
        "active",
        "the request resolved the newest negotiation only"
    );
    // Without a login the same URL is not a capability.
    assert_eq!(
        status_of(&f, request("GET", &infuse, None, Value::Null))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    // Another source of the item resolves nothing.
    let other = format!(
        "/jellyfin/Videos/{}/stream?MediaSourceId={}&Static=true",
        f.item, f.item
    );
    assert_eq!(
        status_of(&f, request("GET", &other, Some(&f.token), Value::Null))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn jellyfin_contract_routes_answer_their_minimum_semantics_and_are_counted() {
    let f = playback_fixture().await;
    let call = |method: &str, path: &str, token: Option<&str>, body: Value| {
        let request = request(method, path, token, body);
        let app = f.app.clone();
        async move {
            let response = app.oneshot(request).await.expect("response");
            let status = response.status();
            let bytes = response
                .into_body()
                .collect()
                .await
                .expect("body")
                .to_bytes();
            (
                status,
                serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null),
            )
        }
    };
    let token = Some(f.token.as_str());
    // Public users: always empty, no login needed.
    assert_eq!(
        call("GET", "/jellyfin/Users/Public", None, Value::Null).await,
        (StatusCode::OK, json!([]))
    );
    // Authenticated system information: the baseline plus Plurx's build.
    assert_eq!(
        call("GET", "/jellyfin/System/Info", None, Value::Null)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (status, info) = call("GET", "/jellyfin/System/Info", token, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(info["Version"], "10.11.11");
    assert!(info["PackageName"]
        .as_str()
        .is_some_and(|p| p.starts_with("plurx ")));
    assert!(info.get("ProgramDataPath").is_none() && info.get("LogPath").is_none());
    // Capabilities: validated and accepted, not stored.
    assert_eq!(
        call(
            "POST",
            "/jellyfin/Sessions/Capabilities?PlayableMediaTypes=Video&SupportsMediaControl=false",
            token,
            Value::Null
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(
            "POST",
            "/jellyfin/Sessions/Capabilities?SupportsMediaControl=maybe",
            token,
            Value::Null
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(
            "POST",
            "/jellyfin/Sessions/Capabilities/Full",
            token,
            json!({"PlayableMediaTypes":["Video"]})
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(
            "POST",
            "/jellyfin/Sessions/Capabilities/Full",
            token,
            json!([])
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    // Search hints over the catalog search.
    let (status, hints) = call(
        "GET",
        "/jellyfin/Search/Hints?searchTerm=direct&Limit=10",
        token,
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{hints}");
    assert_eq!(hints["TotalRecordCount"], 1);
    assert_eq!(hints["SearchHints"][0]["Id"], f.item);
    assert_eq!(hints["SearchHints"][0]["Type"], "Movie");
    let (_, people) = call(
        "GET",
        "/jellyfin/Search/Hints?searchTerm=direct&IncludeItemTypes=Person",
        token,
        Value::Null,
    )
    .await;
    assert_eq!(people["TotalRecordCount"], 0);
    assert_eq!(
        call("GET", "/jellyfin/Search/Hints", token, Value::Null)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    // Downloads: an honest refusal for a mapped item.
    let (status, refused) = call(
        "GET",
        &format!("/jellyfin/Items/{}/Download", f.item),
        token,
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(refused["code"], "download_not_offered");
    // Watched state through the 10.9+ route, user optional.
    let played = format!("/jellyfin/UserPlayedItems/{}", f.item);
    let (status, data) = call("POST", &played, token, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(data["Played"], true);
    let (status, data) = call(
        "DELETE",
        &format!("{played}?userId={}", f.user),
        token,
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(data["Played"], false);
    assert_eq!(
        call(
            "POST",
            &format!("{played}?userId={}", f.item),
            token,
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    // Ping and ActiveEncodings act only on a named play of this login.
    let mut second = request(
        "POST",
        "/jellyfin/Users/AuthenticateByName",
        None,
        json!({"Username":"catalog-admin","Pw":"supersecret"}),
    );
    second.headers_mut().insert("x-emby-authorization", "MediaBrowser Client=\"Jellyfin+Android+TV\", Device=\"other TV\", DeviceId=\"second-device\", Version=\"0.19.10\"".parse().expect("metadata"));
    let (status, second) = json_call(&f.app, second).await;
    assert_eq!(status, StatusCode::OK);
    let other = second["AccessToken"]
        .as_str()
        .expect("second login")
        .to_owned();
    let play = negotiate(&f).await["PlaySessionId"]
        .as_str()
        .expect("play")
        .to_owned();
    let direct = format!(
        "/jellyfin/Videos/{}/stream?MediaSourceId={}&PlaySessionId={play}",
        f.item, f.source
    );
    assert_eq!(
        status_of(&f, request("GET", &direct, token, Value::Null))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            "POST",
            &format!("/jellyfin/Sessions/Playing/Ping?playSessionId={play}"),
            token,
            Value::Null
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(
            "POST",
            "/jellyfin/Sessions/Playing/Ping",
            token,
            Value::Null
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(
            "POST",
            &format!("/jellyfin/Sessions/Playing/Ping?playSessionId={play}&deviceId=someone-else"),
            token,
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            "DELETE",
            "/jellyfin/Videos/ActiveEncodings?deviceId=catalog-contract",
            token,
            Value::Null
        )
        .await
        .0,
        StatusCode::BAD_REQUEST,
        "never a kill by device alone"
    );
    for (method, path) in [
        (
            "POST",
            format!("/jellyfin/Sessions/Playing/Ping?playSessionId={play}"),
        ),
        (
            "DELETE",
            format!("/jellyfin/Videos/ActiveEncodings?playSessionId={play}"),
        ),
    ] {
        assert_eq!(
            call(method, &path, Some(&other), Value::Null).await.0,
            StatusCode::NOT_FOUND,
            "{method} {path}: another login's play is not this login's"
        );
    }
    assert_eq!(
        call(
            "DELETE",
            &format!(
                "/jellyfin/Videos/ActiveEncodings?deviceId=catalog-contract&playSessionId={play}"
            ),
            token,
            Value::Null
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    // Every request above is counted under its template; an unknown path
    // under `unmatched` (a delta: the counter is process-wide).
    let unmatched = || {
        super::metrics::prometheus()
            .lines()
            .find_map(|line| {
                line.strip_prefix(
                    "plurx_jellyfin_requests_total{route=\"unmatched\",outcome=\"not_found\"} ",
                )
                .map(|n| n.parse::<u64>().expect("count"))
            })
            .unwrap_or(0)
    };
    let before = unmatched();
    call("GET", "/jellyfin/No/Such/Route", token, Value::Null).await;
    assert!(unmatched() > before, "an unknown route is counted");
    let text = super::metrics::prometheus();
    for line in [
        "route=\"/jellyfin/Search/Hints\",outcome=\"ok\"",
        "route=\"/jellyfin/Items/{item_id}/Download\",outcome=\"forbidden\"",
        "route=\"/jellyfin/Sessions/Playing/Ping\",outcome=\"refused\"",
        "route=\"unmatched\",outcome=\"not_found\"",
    ] {
        assert!(text.contains(line), "{line} missing from:\n{text}");
    }
    assert!(!text.contains(&f.item) && !text.contains(&f.token) && !text.contains(&play));
}
