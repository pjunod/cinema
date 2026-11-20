//! A real test MPEG-TS source behind the HDHomeRun-shaped fixture endpoints.

use std::net::UdpSocket;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::Router;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio_util::io::ReaderStream;

pub(crate) const GUIDE_NUMBER: &str = "7.1";
const DEVICE_PORT: u16 = 80;
const TUNER_PORT: u16 = 5004;

/// The address the daemon will accept as a device: private, and not loopback.
pub(crate) fn private_ipv4() -> String {
    let probe = UdpSocket::bind("0.0.0.0:0").expect("probe socket");
    probe.connect("192.0.2.1:80").expect("probe route"); // TEST-NET-1; nothing is sent
    let address = probe.local_addr().expect("probe address").ip().to_string();
    let octets: Vec<u8> = address
        .split('.')
        .map(|part| part.parse().expect("dotted quad"))
        .collect();
    let private = octets[0] == 10
        || (octets[0] == 172 && (16..=31).contains(&octets[1]))
        || (octets[0] == 192 && octets[1] == 168)
        || (octets[0] == 169 && octets[1] == 254);
    assert!(
        private,
        "this host's address {address} is not one the daemon accepts as a tuner device"
    );
    address
}

#[derive(Clone)]
struct TunerState {
    address: String,
    opens: Arc<AtomicU64>,
    encoder: PathBuf,
    on_open: Option<Arc<dyn Fn() + Send + Sync>>,
}

async fn discover(State(state): State<TunerState>) -> axum::Json<Value> {
    let base = format!("http://{}:{DEVICE_PORT}", state.address);
    axum::Json(json!({
        "FriendlyName": "Fixture HDHomeRun",
        "ModelNumber": "HDFX-4K",
        "FirmwareName": "hdhomerun_fixture",
        "FirmwareVersion": "20260326",
        "DeviceID": "FIXTURE2",
        "TunerCount": 2,
        "BaseURL": base,
        "LineupURL": format!("{base}/lineup.json"),
    }))
}

async fn lineup(State(state): State<TunerState>) -> axum::Json<Value> {
    axum::Json(json!([{
        "GuideNumber": GUIDE_NUMBER,
        "GuideName": "Two Node Fixture",
        "URL": format!("http://{}:{TUNER_PORT}/auto/v{GUIDE_NUMBER}", state.address),
    }]))
}

/// The stream. Counting entries here is the whole point: the physical tuner is
/// opened by whoever owns it, once, and an ingress must never open a second.
async fn stream(State(state): State<TunerState>) -> Result<Body, StatusCode> {
    if let Some(on_open) = &state.on_open {
        on_open();
    }
    state.opens.fetch_add(1, Ordering::AcqRel);
    let mut child = tokio::process::Command::new(&state.encoder)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-re",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x180:rate=25",
            "-re",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-tune",
            "zerolatency",
            "-pix_fmt",
            "yuv420p",
            "-g",
            "50",
            "-keyint_min",
            "50",
            "-sc_threshold",
            "0",
            "-c:a",
            "aac",
            "-b:a",
            "96k",
            "-f",
            "mpegts",
            "pipe:1",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let stdout = child
        .stdout
        .take()
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
    Ok(Body::from_stream(ReaderStream::new(stdout)))
}

pub(crate) async fn serve_fixture_device(
    address: String,
    encoder: PathBuf,
    opens: Arc<AtomicU64>,
    on_open: Option<Arc<dyn Fn() + Send + Sync>>,
) {
    let state = TunerState {
        address,
        opens,
        encoder,
        on_open,
    };
    let router = Router::new()
        .route("/discover.json", get(discover))
        .route("/lineup.json", get(lineup))
        .route(&format!("/auto/v{GUIDE_NUMBER}"), get(stream))
        .with_state(state);
    for port in [DEVICE_PORT, TUNER_PORT] {
        let listener = TcpListener::bind(("0.0.0.0", port))
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "the fixture device could not bind port {port}: {error}. \
                        Use an isolated network with permission to bind the fixture ports."
                )
            });
        let app = router.clone();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
    }
}
