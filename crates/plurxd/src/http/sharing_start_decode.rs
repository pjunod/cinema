//! Closed private Source HLS wire decoding. This handle carries no admission,
//! worker, producer or receiver delivery authority.
use super::hls::{SourcePlaybackTarget, StartResponse};
use plurx_core::sharing_resources::{
    SharingHlsResource, SharingHlsResourceKind, SharingResourceUnsupported,
};
use serde::{Deserialize, Serialize};
use uuid::{Uuid, Variant};

const MAX_BYTES: usize = 4 * 1024 * 1024;
const MAX_SAFE: i64 = 9_007_199_254_740_991;
type Result<T> = std::result::Result<T, SharingResourceUnsupported>;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    reference: SourcePlaybackTarget,
    incarnation_id: Uuid,
    response: StartResponse,
}

// StartResponse intentionally stores this engine status as Value. Close its
// actual emitted shape while reusing the engine's complete cause enum.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogStatus {
    complete: bool,
    causes: Vec<crate::media_pool::CatalogCause>,
}

/// Nonserializable decoded facts only; even a valid Source response cannot
/// mint a Source observation, physical producer proof or B session binding.
pub(crate) struct DecodedSourceHlsStart(Envelope);
impl DecodedSourceHlsStart {
    pub(super) fn parse(bytes: &[u8], expected: &SourcePlaybackTarget) -> Result<Self> {
        if bytes.is_empty() || bytes.len() > MAX_BYTES {
            return Err(SharingResourceUnsupported);
        }
        let mut json = serde_json::Deserializer::from_slice(bytes);
        let value = super::sharing_decision_decode::bounded_decision_value(&mut json)
            .map_err(|_| SharingResourceUnsupported)?;
        json.end().map_err(|_| SharingResourceUnsupported)?;
        let envelope: Envelope =
            serde_json::from_value(value.clone()).map_err(|_| SharingResourceUnsupported)?;
        if &envelope.reference != expected
            || expected.server_id.is_nil()
            || expected.catalogue_epoch.is_nil()
            || !v4(envelope.incarnation_id)
            || serde_json::to_value(&envelope).map_err(|_| SharingResourceUnsupported)? != value
        {
            return Err(SharingResourceUnsupported);
        }
        validate_response(&envelope.response, envelope.incarnation_id)?;
        Ok(Self(envelope))
    }

    pub(super) fn reference(&self) -> &SourcePlaybackTarget {
        &self.0.reference
    }
    pub(super) fn incarnation_id(&self) -> Uuid {
        self.0.incarnation_id
    }
    pub(super) fn response(&self) -> &StartResponse {
        &self.0.response
    }
    pub(crate) fn into_parts(self) -> (SourcePlaybackTarget, Uuid, StartResponse) {
        (self.0.reference, self.0.incarnation_id, self.0.response)
    }
}

/// Validate the transport wrapper without granting Source admission. The
/// Source handler remains responsible for the complete ordinary session DTO.
pub(crate) fn validate_source_start_request(
    bytes: &[u8],
    expected: &SourcePlaybackTarget,
) -> Result<()> {
    if bytes.is_empty() || bytes.len() > 128 * 1024 {
        return Err(SharingResourceUnsupported);
    }
    let mut json = serde_json::Deserializer::from_slice(bytes);
    let value = super::sharing_decision_decode::bounded_decision_value(&mut json)
        .map_err(|_| SharingResourceUnsupported)?;
    json.end().map_err(|_| SharingResourceUnsupported)?;
    let wrapper = value.as_object().ok_or(SharingResourceUnsupported)?;
    if wrapper.len() != 2
        || wrapper.get("reference")
            != Some(&serde_json::to_value(expected).map_err(|_| SharingResourceUnsupported)?)
        || expected.server_id.is_nil()
        || expected.catalogue_epoch.is_nil()
        || serde_json::to_vec(&value).map_err(|_| SharingResourceUnsupported)? != bytes
    {
        return Err(SharingResourceUnsupported);
    }
    let session = wrapper
        .get("session")
        .and_then(serde_json::Value::as_object)
        .ok_or(SharingResourceUnsupported)?;
    if serde_json::to_vec(session)
        .map_err(|_| SharingResourceUnsupported)?
        .len()
        > 24 * 1024
    {
        return Err(SharingResourceUnsupported);
    }
    let text = session
        .get("request_id")
        .and_then(serde_json::Value::as_str)
        .ok_or(SharingResourceUnsupported)?;
    let id = Uuid::parse_str(text).map_err(|_| SharingResourceUnsupported)?;
    if !v4(id) || id.to_string() != text {
        return Err(SharingResourceUnsupported);
    }
    Ok(())
}

pub(crate) fn decode_source_start_response(
    bytes: &[u8],
    expected: &SourcePlaybackTarget,
) -> Result<DecodedSourceHlsStart> {
    DecodedSourceHlsStart::parse(bytes, expected)
}

fn v4(id: Uuid) -> bool {
    id.get_version_num() == 4 && id.get_variant() == Variant::RFC4122 && !id.is_nil()
}
fn validate_response(response: &StartResponse, incarnation: Uuid) -> Result<()> {
    let session = Uuid::parse_str(&response.session_id).map_err(|_| SharingResourceUnsupported)?;
    if !v4(session)
        || session.to_string() != response.session_id
        || !response.start_seconds.is_finite()
        || !(0.0..=9_007_199_254_740.0).contains(&response.start_seconds)
        || response
            .duration_ms
            .is_some_and(|n| !(0..=MAX_SAFE).contains(&n))
        || response
            .media_origin_ms
            .is_some_and(|n| !(-MAX_SAFE..=MAX_SAFE).contains(&n))
        || response.ladder.len() > 64
        || response.plan_notes.len() > 64
        || response
            .quality_candidates
            .as_ref()
            .is_some_and(|v| v.len() > 64)
    {
        return Err(SharingResourceUnsupported);
    }
    let prefix = format!("/api/v1/hls/{session}/");
    let suffix = response
        .playlist_url
        .strip_prefix(&prefix)
        .ok_or(SharingResourceUnsupported)?;
    if !matches!(
        SharingHlsResource::parse(suffix)?.kind(),
        SharingHlsResourceKind::Master
            | SharingHlsResourceKind::Index
            | SharingHlsResourceKind::Video
    ) {
        return Err(SharingResourceUnsupported);
    }
    let control = response
        .control
        .as_ref()
        .ok_or(SharingResourceUnsupported)?;
    if control.url != format!("{prefix}control")
        || control.generation != incarnation.to_string()
        || control.protocol != crate::playback_control::PROTOCOL_V1
        || !(1..=MAX_SAFE as u64).contains(&control.control_epoch)
        || control.next_exchange_ms != crate::playback_control::NEXT_EXCHANGE_MS
        || !matches!(
            control.lease_timeout_ms,
            crate::playback_control::ROLLING_LEASE_TIMEOUT_MS
                | crate::playback_control::VOD_LEASE_TIMEOUT_MS
        )
    {
        return Err(SharingResourceUnsupported);
    }
    if let Some(status) = &response.quality_catalog_status {
        let decoded: CatalogStatus =
            serde_json::from_value(status.clone()).map_err(|_| SharingResourceUnsupported)?;
        if decoded.causes.len() > 64
            || serde_json::to_value(decoded).map_err(|_| SharingResourceUnsupported)? != *status
        {
            return Err(SharingResourceUnsupported);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::{sharing::SourceId, sharing_catalogue_details::FileRevision};
    use serde_json::{json, Value};

    fn fixture() -> (SourcePlaybackTarget, Value) {
        let reference = SourcePlaybackTarget {
            server_id: Uuid::new_v4(),
            catalogue_epoch: Uuid::new_v4(),
            library_id: SourceId::parse("0").expect("zero library"),
            item_id: SourceId::parse("9007199254740993").expect("exact item"),
            file_id: SourceId::parse("9223372036854775807").expect("exact file"),
            revision: FileRevision::parse(&"a".repeat(64)).expect("revision"),
        };
        let session = Uuid::new_v4();
        let incarnation = Uuid::new_v4();
        let mut response: StartResponse = serde_json::from_value(json!({
            "quality_catalog_status":{"complete":true,"causes":["PeerBusy"]},
            "display_aware_auto_protocol":"route-v1","quality_candidate_id":"04040404040404040404040404040404","quality_candidates":[],
            "session_id":session,"playlist_url":format!("/api/v1/hls/{session}/index.m3u8?native=1&subtitle=2"),
            "duration_ms":100000,"start_seconds":30.0,"media_origin_ms":29800,"height":2160,"encoder":"fixture-encoder","vod":true,
            "ladder":[{"height":2160,"total_kbps":22000,"peak_kbps":32000}],"prior_kbps":50000,
            "delivered_dynamic_range":"dolby_vision","delivered_dolby_vision_profile":8,"plan_notes":["fixture override"]
        })).expect("whole actual HTTP DTO");
        response.control = crate::playback_control::ControlBootstrap::new(
            &session.to_string(),
            &incarnation.to_string(),
            7,
            crate::playback_control::VOD_LEASE_TIMEOUT_MS,
        );
        (
            reference.clone(),
            json!({"reference":reference,"incarnation_id":incarnation,"response":response}),
        )
    }
    fn decode(value: &Value, expected: &SourcePlaybackTarget) -> Result<DecodedSourceHlsStart> {
        DecodedSourceHlsStart::parse(&serde_json::to_vec(value).expect("fixture JSON"), expected)
    }
    #[test]
    fn sharing_start_decoder_preserves_complete_actual_engine_response_and_reference() {
        let (expected, value) = fixture();
        let decoded = decode(&value, &expected).expect("closed start");
        assert!(decoded.reference() == &expected);
        assert_eq!(
            decoded.incarnation_id().to_string(),
            value["incarnation_id"].as_str().expect("incarnation")
        );
        assert_eq!(
            serde_json::to_value(decoded.response()).expect("whole response"),
            value["response"]
        );
        let (reference, incarnation, response) = decoded.into_parts();
        assert!(reference == expected);
        assert_eq!(
            response.control.expect("control").generation,
            incarnation.to_string()
        );
    }
    #[test]
    fn sharing_start_decoder_refuses_whole_dto_field_drift_and_wrong_shapes() {
        let (expected, good) = fixture();
        for path in [
            "session_id",
            "playlist_url",
            "duration_ms",
            "start_seconds",
            "height",
            "encoder",
            "vod",
            "ladder",
        ] {
            let mut v = good.clone();
            v["response"]
                .as_object_mut()
                .expect("response")
                .remove(path);
            assert!(decode(&v, &expected).is_err(), "missing {path}");
        }
        for path in [
            "",
            "response",
            "reference",
            "response/control",
            "response/ladder/0",
            "response/quality_catalog_status",
        ] {
            let mut v = good.clone();
            let target = if path.is_empty() {
                &mut v
            } else {
                v.pointer_mut(&format!("/{path}")).expect("path")
            };
            target["unexpected"] = json!(true);
            assert!(decode(&v, &expected).is_err(), "unknown {path}");
        }
        for (path, bad) in [
            ("/response/start_seconds", json!("30")),
            ("/response/ladder", json!({})),
            ("/response/control/control_epoch", json!("7")),
            ("/response/quality_candidates", json!({})),
        ] {
            let mut v = good.clone();
            *v.pointer_mut(path).expect("field") = bad;
            assert!(decode(&v, &expected).is_err());
        }
    }
    #[test]
    fn sharing_start_decoder_binds_every_source_field_and_canonical_v4_incarnation() {
        let (expected, good) = fixture();
        for field in [
            "server_id",
            "catalogue_epoch",
            "library_id",
            "item_id",
            "file_id",
            "revision",
        ] {
            let mut v = good.clone();
            v["reference"][field] = match field {
                "server_id" | "catalogue_epoch" => json!(Uuid::new_v4()),
                "revision" => json!("b".repeat(64)),
                _ => json!("1"),
            };
            assert!(decode(&v, &expected).is_err(), "reference {field}");
        }
        for field in ["incarnation_id", "response/session_id"] {
            for bad in [
                Uuid::nil().to_string(),
                "aaaaaaaa-aaaa-1aaa-8aaa-aaaaaaaaaaaa".into(),
                "aaaaaaaa-aaaa-4aaa-1aaa-aaaaaaaaaaaa".into(),
                "AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA".into(),
            ] {
                let mut v = good.clone();
                *v.pointer_mut(&format!("/{field}")).expect("UUID field") = json!(bad);
                assert!(decode(&v, &expected).is_err());
            }
        }
        let mut v = good;
        v["response"]["control"]["generation"] = json!(Uuid::new_v4());
        assert!(decode(&v, &expected).is_err());
    }
    #[test]
    fn sharing_start_decoder_refuses_source_url_escape_and_missing_control() {
        let (expected, good) = fixture();
        let session = good["response"]["session_id"].as_str().expect("session");
        for suffix in [
            "../index.m3u8",
            "index.m3u8?url=https://source",
            "index.m3u8?native=1&native=0",
            "init.mp4",
            "index.m3u8#x",
            "%69ndex.m3u8",
        ] {
            let mut v = good.clone();
            v["response"]["playlist_url"] = json!(format!("/api/v1/hls/{session}/{suffix}"));
            assert!(decode(&v, &expected).is_err());
        }
        for field in ["playlist_url", "control/url"] {
            for url in [
                "https://source/index.m3u8",
                "//source/index.m3u8",
                "/api/v1/hls/aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa/index.m3u8",
            ] {
                let mut v = good.clone();
                *v.pointer_mut(&format!("/response/{field}")).expect("URL") = json!(url);
                assert!(decode(&v, &expected).is_err());
            }
        }
        let mut v = good;
        v["response"]
            .as_object_mut()
            .expect("response")
            .remove("control");
        assert!(decode(&v, &expected).is_err());
    }
    #[test]
    fn sharing_start_decoder_bounds_whole_json_body_duplicates_shape_and_finite_numbers() {
        let (expected, good) = fixture();
        let raw = serde_json::to_string(&good).expect("JSON");
        let mut exact = raw.clone();
        exact.push_str(&" ".repeat(MAX_BYTES - raw.len()));
        assert!(DecodedSourceHlsStart::parse(exact.as_bytes(), &expected).is_ok());
        exact.push(' ');
        assert!(DecodedSourceHlsStart::parse(exact.as_bytes(), &expected).is_err());
        for bad in [
            format!("{{\"reference\":null,{}", &raw[1..]),
            raw.replace("\"height\":2160", "\"height\":2160,\"height\":1"),
            raw.replace("\"start_seconds\":30.0", "\"start_seconds\":1e999"),
            format!("{raw} true"),
        ] {
            assert!(DecodedSourceHlsStart::parse(bad.as_bytes(), &expected).is_err());
        }
        let mut deep = json!(true);
        for _ in 0..34 {
            deep = json!([deep]);
        }
        let mut v = good.clone();
        v["response"]["quality_catalog_status"] = deep;
        assert!(decode(&v, &expected).is_err());
        let mut v = good;
        v["response"]["plan_notes"] = json!(vec!["x"; 16385]);
        assert!(decode(&v, &expected).is_err());
    }
    #[test]
    fn sharing_start_transport_request_closes_wrapper_reference_and_recipe_budget() {
        let (expected, _) = fixture();
        let good =
            json!({"reference":expected,"session":{"request_id":Uuid::new_v4(),"caps":{"v":2}}});
        let bytes = serde_json::to_vec(&good).expect("canonical wrapper");
        assert!(validate_source_start_request(&bytes, &expected).is_ok());
        for field in [
            "server_id",
            "catalogue_epoch",
            "library_id",
            "item_id",
            "file_id",
            "revision",
        ] {
            let mut bad = good.clone();
            bad["reference"][field] = json!("1");
            assert!(validate_source_start_request(
                &serde_json::to_vec(&bad).expect("JSON"),
                &expected
            )
            .is_err());
        }
        for bad in [
            json!(null),
            json!({}),
            json!({"request_id":Uuid::nil()}),
            json!({"request_id":Uuid::new_v4().to_string().to_uppercase()}),
            json!({"request_id":Uuid::new_v4(),"padding":"x".repeat(24*1024)}),
        ] {
            let mut v = good.clone();
            v["session"] = bad;
            assert!(validate_source_start_request(
                &serde_json::to_vec(&v).expect("JSON"),
                &expected
            )
            .is_err());
        }
        let mut noncanonical = bytes.clone();
        noncanonical.push(b' ');
        assert!(validate_source_start_request(&noncanonical, &expected).is_err());
        let duplicate = format!(
            "{{\"reference\":{},\"session\":{},\"session\":{}}}",
            good["reference"], good["session"], good["session"]
        );
        assert!(validate_source_start_request(duplicate.as_bytes(), &expected).is_err());
        assert!(validate_source_start_request(&vec![b' '; 128 * 1024 + 1], &expected).is_err());
    }

    #[tokio::test]
    #[ignore = "requires disposable CGNAT network and PLURX_SHARING_FIXTURE_IP"]
    async fn sharing_start_transport_pinned_source_h1_and_receiver_h1_h2_preserve_raw_envelope() {
        use axum::{
            body::{Body, Bytes},
            http::{Request, StatusCode},
            routing::{get, post},
            Router,
        };
        use http_body_util::BodyExt;
        use plurx_core::{
            config::{SharingEgressConfig, SharingNetworkConfig},
            secrets::CredentialKey,
            sharing::{Endpoint, SharingIdentity},
            sharing_tls::{LiveNodeTls, SharingTlsListener},
        };
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        let address: std::net::IpAddr = std::env::var("PLURX_SHARING_FIXTURE_IP")
            .expect("explicit CGNAT fixture")
            .parse()
            .expect("address");
        assert!(plurx_core::sharing::is_tailnet_address(address));
        for h2 in [false, true] {
            let (expected, envelope) = fixture();
            let credential = Arc::new(plurx_core::sharing::new_secret().expect("credential"));
            let canonical = serde_json::to_string(&json!({"reference":expected,"session":{"request_id":Uuid::new_v4(),"caps":{"v":2}}})).expect("wrapper");
            let case = Arc::new(AtomicUsize::new(0));
            let requests = Arc::new(AtomicUsize::new(0));
            let source_dir = tempfile::tempdir().expect("TLS directory");
            let tls = Arc::new(
                LiveNodeTls::open(
                    &source_dir.path().join("tls"),
                    i64::try_from(
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .expect("current clock")
                            .as_secs(),
                    )
                    .expect("clock seconds"),
                )
                .expect("Source TLS"),
            );
            let (pin, _) = tls.status().expect("pin");
            let source = tokio::net::TcpListener::bind((address, 0))
                .await
                .expect("Source listener");
            let endpoint = Endpoint {
                ipv4: match address {
                    std::net::IpAddr::V4(ip) => ip,
                    _ => panic!("IPv4 fixture"),
                },
                ipv6: None,
                ts_fqdn: "source.fixture.ts.net".into(),
                port: source.local_addr().expect("address").port(),
                spki_sha256: pin,
            };
            let identity = SharingIdentity {
                server_id: expected.server_id,
                catalogue_epoch: expected.catalogue_epoch,
                created_at_ms: 1,
            };
            let identity_body = json!({"server_id":identity.server_id,"catalogue_epoch":identity.catalogue_epoch,"name":"explicit transport fixture","protocol_min":1,"protocol_max":1}).to_string();
            let source_router = Router::new().route("/sharing/v1/identity", get(move || {let body=identity_body.clone();async move {body}})).route("/sharing/v1/items/{item}/files/{file}/sessions", post({
                let case=case.clone(); let requests=requests.clone();let envelope=envelope.clone();let credential=credential.clone();let canonical=canonical.clone();
                move |request:Request<Body>| {let case=case.clone();let requests=requests.clone();let mut envelope=envelope.clone();let credential=credential.clone();let canonical=canonical.clone();async move {
                    requests.fetch_add(1,Ordering::SeqCst);
                    assert_eq!(request.uri().path(),"/sharing/v1/items/9007199254740993/files/9223372036854775807/sessions");
                    assert_eq!(request.headers()["authorization"], format!("CinemaShare {}",credential.expose()));
                    assert_eq!(request.headers()["cinemashare-viewer"], "a".repeat(64));
                    assert_eq!(axum::body::to_bytes(request.into_body(),128*1024).await.expect("request"),canonical.as_bytes());
                    let index=case.load(Ordering::SeqCst);
                    if index==8 {tokio::time::sleep(std::time::Duration::from_millis(100)).await;}
                    let status=match index {4=>StatusCode::TEMPORARY_REDIRECT,5=>StatusCode::UNAUTHORIZED,_=>StatusCode::OK};
                    if index==1 {envelope["reference"]["revision"]=json!("b".repeat(64));}
                    if index==2 {envelope["response"]["unexpected"]=json!(true);}
                    let body=if index==3 {format!("{{\"reference\":{},\"incarnation_id\":{},\"response\":{},\"response\":{}}}",envelope["reference"],envelope["incarnation_id"],envelope["response"],envelope["response"])} else if index==6 {"x".repeat(4*1024*1024+1)} else if index==7 {"x".repeat(128*1024+1)} else {envelope.to_string()};
                    let status=if index==7 {StatusCode::FORBIDDEN}else{status};
                    axum::http::Response::builder().status(status).header("location","https://outside.invalid/credential-sink").body(Body::from(body)).expect("Source response")
                }}
            }));
            let (source_stop, source_stopped) = tokio::sync::oneshot::channel();
            let source_task = tokio::spawn(crate::serve_http(
                SharingTlsListener::new(source, tls),
                source_router,
                async move {
                    let _ = source_stopped.await;
                },
                crate::HTTP_TIMEOUTS,
            ));
            let manager_dir = tempfile::tempdir().expect("manager directory");
            let manager = Arc::new(crate::sharing::SharingManager::new(
                Arc::new(CredentialKey::generate()),
                manager_dir.path().join("unused"),
                SharingNetworkConfig {
                    bind: "127.0.0.1:32444".parse().expect("bind"),
                    egress: SharingEgressConfig::LocalAddress { address },
                },
            ));
            let receiver = tokio::net::TcpListener::bind((address, 0))
                .await
                .expect("B listener");
            let receiver_address = receiver.local_addr().expect("B address");
            let direct_manager = manager.clone();
            let direct_endpoint = endpoint.clone();
            let direct_identity = identity.clone();
            let direct_canonical = canonical.clone();
            let receiver_router = Router::new().route(
                "/fixture-start",
                post({
                    let expected = expected.clone();
                    let envelope = envelope.clone();
                    let credential = credential.clone();
                    move || {
                        let expected = expected.clone();
                        let envelope = envelope.clone();
                        let credential = credential.clone();
                        let manager = manager.clone();
                        let endpoint = endpoint.clone();
                        let identity = identity.clone();
                        let canonical = canonical.clone();
                        async move {
                            let (mut peer, _) = crate::sharing_client::PeerConnection::verified(
                                &manager,
                                &[endpoint],
                                &identity,
                            )
                            .await
                            .expect("actual pinned identity");
                            match peer
                                .file_start(&credential, &expected, &"a".repeat(64), &canonical)
                                .await
                            {
                                Ok(crate::http::sharing_direct_wire::DecodedSourceStart::Hls(
                                    decoded,
                                )) => {
                                    assert!(decoded.reference() == &expected);
                                    assert_eq!(
                                        serde_json::to_value(decoded.response())
                                            .expect("complete response"),
                                        envelope["response"]
                                    );
                                    StatusCode::OK
                                }
                                Ok(_) | Err(_) => StatusCode::BAD_GATEWAY,
                            }
                        }
                    }
                }),
            );
            let (receiver_stop, receiver_stopped) = tokio::sync::oneshot::channel();
            let receiver_task = tokio::spawn(crate::serve_http(
                receiver,
                receiver_router,
                async move {
                    let _ = receiver_stopped.await;
                },
                crate::HTTP_TIMEOUTS,
            ));
            for index in 0..8 {
                case.store(index, Ordering::SeqCst);
                let socket = tokio::net::TcpStream::connect(receiver_address)
                    .await
                    .expect("B socket");
                let request = Request::builder()
                    .method("POST")
                    .uri("/fixture-start")
                    .header("host", "receiver.fixture")
                    .body(Body::empty())
                    .expect("B request");
                let (status, body): (StatusCode, Bytes) = if h2 {
                    let (mut sender, driver) = hyper::client::conn::http2::Builder::new(
                        hyper_util::rt::TokioExecutor::new(),
                    )
                    .handshake::<_, Body>(hyper_util::rt::TokioIo::new(socket))
                    .await
                    .expect("B H2");
                    let driver = tokio::spawn(driver);
                    let response = sender.send_request(request).await.expect("response");
                    let status = response.status();
                    let bytes = response
                        .into_body()
                        .collect()
                        .await
                        .expect("body")
                        .to_bytes();
                    drop(sender);
                    driver.abort();
                    let _ = driver.await;
                    (status, bytes)
                } else {
                    let (mut sender, driver) = hyper::client::conn::http1::handshake::<_, Body>(
                        hyper_util::rt::TokioIo::new(socket),
                    )
                    .await
                    .expect("B H1");
                    let driver = tokio::spawn(driver);
                    let response = sender.send_request(request).await.expect("response");
                    let status = response.status();
                    let bytes = response
                        .into_body()
                        .collect()
                        .await
                        .expect("body")
                        .to_bytes();
                    drop(sender);
                    driver.abort();
                    let _ = driver.await;
                    (status, bytes)
                };
                assert_eq!(
                    status,
                    if index == 0 {
                        StatusCode::OK
                    } else {
                        StatusCode::BAD_GATEWAY
                    },
                    "H2={h2} case={index}"
                );
                assert!(body.is_empty());
                assert_eq!(
                    requests.load(Ordering::SeqCst),
                    index + 1,
                    "single Source request; no redirects/retry"
                );
            }
            let (mut peer, _) = crate::sharing_client::PeerConnection::verified(
                &direct_manager,
                &[direct_endpoint],
                &direct_identity,
            )
            .await
            .expect("direct pinned client");
            for viewer in [String::new(), "A".repeat(64), "g".repeat(64)] {
                assert!(peer
                    .file_start(&credential, &expected, &viewer, &direct_canonical)
                    .await
                    .is_err());
            }
            assert!(peer
                .file_start(&credential, &expected, &"a".repeat(64), "{}")
                .await
                .is_err());
            assert_eq!(
                requests.load(Ordering::SeqCst),
                8,
                "invalid local requests send nothing"
            );
            case.store(8, Ordering::SeqCst);
            assert!(
                tokio::time::timeout(
                    std::time::Duration::from_millis(50),
                    peer.file_start(&credential, &expected, &"a".repeat(64), &direct_canonical)
                )
                .await
                .is_err(),
                "caller can cancel a blocked Source reply"
            );
            drop(peer);
            assert_eq!(
                requests.load(Ordering::SeqCst),
                9,
                "cancellation never retries; no Source rollback claim"
            );
            source_stop.send(()).expect("Source stop");
            receiver_stop.send(()).expect("B stop");
            source_task
                .await
                .expect("Source task")
                .expect("Source server");
            receiver_task.await.expect("B task").expect("B server");
        }
    }
}
