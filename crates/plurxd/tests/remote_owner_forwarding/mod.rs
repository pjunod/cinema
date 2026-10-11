//! Focused signed Cinema owner routing through two real daemon HTTP nodes.
use super::*;
use axum::body::to_bytes;
use axum::http::Request;
use axum::response::{IntoResponse, Response};
use std::sync::atomic::AtomicU8;
use std::sync::Mutex;
use tokio_util::sync::CancellationToken;

const DISPATCH: &str = "/internal/remote/v1/dispatch";
#[derive(Clone)]
struct ProxyState {
    target: Arc<Mutex<Option<String>>>,
    mode: Arc<AtomicU8>,
    hits: Arc<AtomicU64>,
    client: reqwest::Client,
}
pub(super) struct RemoteProxy {
    pub(super) port: u16,
    state: ProxyState,
    stop: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}
impl RemoteProxy {
    async fn start() -> Self {
        let listener = TokioTcpListener::bind("127.0.0.1:0")
            .await
            .expect("proxy loopback");
        let port = listener.local_addr().expect("proxy address").port();
        let state = ProxyState {
            target: Arc::new(Mutex::new(None)),
            mode: Arc::new(AtomicU8::new(0)),
            hits: Arc::new(AtomicU64::new(0)),
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(25))
                .build()
                .expect("proxy client"),
        };
        let stop = CancellationToken::new();
        let shutdown = stop.clone();
        let app = Router::new()
            .fallback(proxy_request)
            .with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown.cancelled_owned())
                .await
                .expect("test proxy");
        });
        Self {
            port,
            state,
            stop,
            task,
        }
    }
    pub(super) fn set_target(&self, target: String) {
        *self.state.target.lock().expect("proxy target") = Some(target);
    }
    fn mode(&self, mode: u8) {
        self.state.mode.store(mode, Ordering::SeqCst);
    }
}
impl Drop for RemoteProxy {
    fn drop(&mut self) {
        self.stop.cancel();
        self.task.abort();
    }
}
async fn proxy_request(State(state): State<ProxyState>, request: Request<Body>) -> Response {
    let (mut parts, body) = request.into_parts();
    let is_remote = parts.uri.path() == DISPATCH;
    let mode = state.mode.load(Ordering::SeqCst);
    if is_remote {
        state.hits.fetch_add(1, Ordering::SeqCst);
        if mode == 3 {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        if mode == 1 {
            parts.headers.insert(
                "x-plurx-cluster-signature",
                "00".repeat(64).parse().expect("bad signature"),
            );
        }
    }
    let Some(target) = state.target.lock().expect("proxy target").clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Ok(body) = to_bytes(body, 1024 * 1024).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    parts.headers.remove("host");
    parts.headers.remove("content-length");
    let Ok(upstream) = state
        .client
        .request(parts.method, format!("{target}{}", parts.uri))
        .headers(parts.headers)
        .body(body)
        .send()
        .await
    else {
        return StatusCode::BAD_GATEWAY.into_response();
    };
    let status = upstream.status();
    let mut headers = upstream.headers().clone();
    headers.remove("content-length");
    if is_remote && mode == 2 {
        headers.insert(
            "x-plurx-response-signature",
            "00".repeat(64).parse().expect("bad response signature"),
        );
    }
    let Ok(body) = upstream.bytes().await else {
        return StatusCode::BAD_GATEWAY.into_response();
    };
    (status, headers, body).into_response()
}
async fn remote(
    cluster: &Cluster,
    base: &str,
    route: &str,
    token: &str,
    proof: Option<(&str, &str)>,
    body: Value,
) -> (StatusCode, Value) {
    let mut request = cluster
        .client
        .post(format!("{base}/api/remote/v1/{route}"))
        .bearer_auth(token)
        .json(&body);
    if let Some((key, value)) = proof {
        request = request.header(key, value);
    }
    let response = request.send().await.expect("remote request");
    let status = response.status();
    let bytes = response.bytes().await.expect("response body");
    assert!(bytes.len() <= 65536);
    (
        status,
        serde_json::from_slice(&bytes).expect("versioned remote JSON"),
    )
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn signed_owner_forwarding_refuses_tampering_and_unavailable_owner() {
    let proxy = RemoteProxy::start().await;
    let cluster = Cluster::start_with_remote_proxy(Some(&proxy)).await;
    let settings = cluster
        .client
        .put(format!("{}/api/v1/settings", cluster.a_base))
        .bearer_auth(&cluster.token)
        .json(&json!({"cinema_remote_control":true}))
        .send()
        .await
        .expect("enable remote");
    assert_eq!(settings.status(), StatusCode::OK);
    let version = "cinema.remote.v1";
    let (status, receiver) = remote(
        &cluster,
        &cluster.b_base,
        "receivers",
        &cluster.token,
        None,
        json!({"version":version,"name":"Signed TV","platform":"web"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let receiver_secret = receiver["receiver_secret"]
        .as_str()
        .expect("receiver proof");
    let receiver_proof = Some(("x-cinema-receiver-secret", receiver_secret));
    let (status, session) = remote(&cluster,&cluster.b_base,"sessions",&cluster.token,receiver_proof,json!({"version":version,"receiver_id":receiver["receiver_id"],"foreground_id":uuid::Uuid::new_v4()})).await;
    assert_eq!(status, StatusCode::OK);
    let target = session["target"].clone();
    assert_eq!(target["owner_node_id"], cluster.node_b_id);
    let (status, challenge) = remote(
        &cluster,
        &cluster.a_base,
        "pairing/start",
        &cluster.token,
        receiver_proof,
        json!({"version":version,"target":target}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status,pending) = remote(&cluster,&cluster.a_base,"pairing/claim",&cluster.token,None,json!({"version":version,"target":target,"challenge_id":null,"code":challenge["code"],"controller_name":"Signed phone"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(remote(&cluster,&cluster.a_base,"pairing/approve",&cluster.token,receiver_proof,json!({"version":version,"target":target,"pending_id":pending["pending_id"],"approve":true})).await.0,StatusCode::OK);
    let grant = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (status, result) = remote(
                &cluster,
                &cluster.a_base,
                "pairing/result",
                &cluster.token,
                Some((
                    "x-cinema-pairing-secret",
                    pending["poll_secret"].as_str().expect("poll proof"),
                )),
                json!({"version":version,"target":target,"pending_id":pending["pending_id"]}),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            if result["status"] == "approved" {
                break result;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("pair approval");
    let proof = Some((
        "x-cinema-grant-secret",
        grant["grant_secret"].as_str().expect("grant proof"),
    ));
    let (status,control) = remote(&cluster,&cluster.a_base,"control",&cluster.token,proof,json!({"version":version,"target":target,"grant_id":grant["grant_id"],"action":"acquire","control_epoch":null})).await;
    assert_eq!(status, StatusCode::OK);
    let command = |sequence| json!({"version":version,"target":target,"grant_id":grant["grant_id"],"control_epoch":control["control"]["control_epoch"],"sequence":sequence,"credit":uuid::Uuid::new_v4(),"context_revision":1,"focus_revision":1,"action":{"type":"select"}});
    assert_eq!(
        remote(
            &cluster,
            &cluster.a_base,
            "commands",
            &cluster.token,
            proof,
            command(1)
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    let poll = || json!({"version":version,"target":target,"after_delivery_id":0,"after_response_revision":0,"wait_ms":0});
    let (status, batch) = remote(
        &cluster,
        &cluster.a_base,
        "poll",
        &cluster.token,
        receiver_proof,
        poll(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(batch["commands"][0]["sequence"], 1);
    assert_eq!(batch["target"], target);
    assert!(
        proxy.state.hits.load(Ordering::SeqCst) >= 6,
        "actual owner HTTP dispatch"
    );
    proxy.mode(2);
    assert_eq!(
        remote(
            &cluster,
            &cluster.a_base,
            "poll",
            &cluster.token,
            receiver_proof,
            poll()
        )
        .await
        .0,
        StatusCode::SERVICE_UNAVAILABLE,
        "signed response must verify"
    );
    proxy.mode(1);
    assert_eq!(
        remote(
            &cluster,
            &cluster.a_base,
            "commands",
            &cluster.token,
            proof,
            command(2)
        )
        .await
        .0,
        StatusCode::SERVICE_UNAVAILABLE,
        "tampered signed request cannot enqueue"
    );
    proxy.mode(3);
    assert_eq!(
        remote(
            &cluster,
            &cluster.a_base,
            "commands",
            &cluster.token,
            proof,
            command(3)
        )
        .await
        .0,
        StatusCode::SERVICE_UNAVAILABLE,
        "unavailable owner cannot retarget locally"
    );
    proxy.mode(0);
    let (status, retry) = remote(
        &cluster,
        &cluster.a_base,
        "poll",
        &cluster.token,
        receiver_proof,
        poll(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(retry["delivery_id"], batch["delivery_id"]);
    assert_eq!(
        retry["commands"], batch["commands"],
        "failed ingress never reached owner queue"
    );
    let before = proxy.state.hits.load(Ordering::SeqCst);
    let mut unknown = poll();
    unknown["target"]["owner_node_id"] = json!(uuid::Uuid::new_v4());
    assert_eq!(
        remote(
            &cluster,
            &cluster.a_base,
            "poll",
            &cluster.token,
            receiver_proof,
            unknown
        )
        .await
        .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(proxy.state.hits.load(Ordering::SeqCst), before);
    let nonmember = cluster
        .client
        .post(format!("{}{}", cluster.b_base, DISPATCH))
        .header("x-plurx-cluster-node", uuid::Uuid::new_v4().to_string())
        .header("x-plurx-cluster-target", &cluster.node_b_id)
        .header("x-plurx-cluster-time-ms", "1")
        .header("x-plurx-cluster-nonce", "00".repeat(32))
        .header("x-plurx-cluster-signature", "00".repeat(64))
        .body("{}")
        .send()
        .await
        .expect("unknown peer");
    assert_eq!(nonmember.status(), StatusCode::UNAUTHORIZED);
}
