use std::sync::Once;
use std::time::Duration;

/// Name the rustls crypto provider this crate's HTTP clients run on.
///
/// `reqwest` is declared here with `rustls-no-provider` (see `Cargo.toml`), so
/// `ClientBuilder::build` takes `rustls::crypto::CryptoProvider::get_default()`
/// and panics outright when no provider was installed — every client, not only
/// the ones that go on to speak TLS, because the TLS config is assembled during
/// `build`.
///
/// Until patch 16 gated `cryptr/s3` behind this crate's own `s3` feature, a
/// provider arrived here by accident and nobody had asked for it: an
/// ungated `cryptr/s3` pulled `s3-simple` and with it a second `reqwest` whose
/// provider feature Cargo unified onto this dependency. Plurx builds this crate
/// without `backup`/`s3`, so that edge is gone from the workspace graph — and
/// with it the provider, which turned every client this fork builds (the
/// peer/management transport, the init probes, the split-brain check) into a
/// panic. Naming the provider here is what stops the remaining cuts in
/// `docs/cluster/HIQLITE-FORK-AND-DEPENDENCY-CLEANUP.md` from taking it away
/// again: §3.6's option A retires `aws-lc-sys` entirely, and this call site is
/// unaffected by that because it does not depend on `aws-lc-rs`.
///
/// `ring` is the provider the rest of this fork already asks for
/// (`[dependencies.rustls] features = [..., "ring"]`, `axum-server`'s
/// `tls-rustls-no-provider`) and the one `plurx-core`'s
/// `install_default_crypto_provider` installs, so naming it keeps a single
/// provider in the process rather than introducing `aws-lc-rs` as a second one.
///
/// It lives at the point of client construction rather than in a `main`: a test
/// binary, an integration harness or a library consumer has no `main` of ours to
/// run, and that is exactly how the missing provider stayed hidden until the
/// workspace test lane hit it. `plurx-core`'s `tests/hiqlite_tls_provider.rs`
/// pins this.
pub fn ensure_rustls_crypto_provider() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        // An `Err` means the embedding process already chose a provider. That is
        // a legitimate choice and `reqwest` will use it, so there is nothing to
        // report here.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

// not really dead code
// It will be used in any (real) scenario. This is only to get rid of a warning during some
// `clippy` checks.
#[allow(dead_code)]
pub fn build_http_client(tls_no_verify: bool) -> reqwest::Client {
    ensure_rustls_crypto_provider();
    #[allow(unused_mut)]
    let mut builder = reqwest::Client::builder()
        .http2_prior_knowledge()
        // API requests carry a custom shared-secret header. Reqwest does not
        // classify that header as sensitive across origins, so management
        // clients must never follow a configured peer's redirect.
        .redirect(reqwest::redirect::Policy::none())
        .tls_danger_accept_invalid_certs(tls_no_verify)
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(30));

    #[cfg(feature = "webpki-roots")]
    {
        builder = builder.tls_certs_merge(
            webpki_root_certs::TLS_SERVER_ROOT_CERTS
                .iter()
                .map(|c| reqwest::Certificate::from_der(c).unwrap()),
        );
    }

    #[cfg(test)]
    {
        // Unit tests use only loopback HTTP or explicit no-verification TLS.
        // Keep them deterministic on headless macOS runners where the native
        // keychain can be unavailable even though no test requests HTTPS.
        builder = builder.tls_certs_only(std::iter::empty::<reqwest::Certificate>());
    }

    builder.build().unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::HEADER_NAME_SECRET;
    use axum::Router;
    use axum::http::header::LOCATION;
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::get;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn management_client_does_not_forward_api_secret_across_redirects() {
        let redirect_seen = Arc::new(AtomicBool::new(false));
        let secret_seen = Arc::new(AtomicBool::new(false));
        let sink_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let sink_addr = sink_listener.local_addr().unwrap();
        let sink_redirect_seen = Arc::clone(&redirect_seen);
        let sink_secret_seen = Arc::clone(&secret_seen);
        let sink = Router::new().route(
            "/",
            get(move |headers: HeaderMap| {
                let redirect_seen = Arc::clone(&sink_redirect_seen);
                let secret_seen = Arc::clone(&sink_secret_seen);
                async move {
                    redirect_seen.store(true, Ordering::SeqCst);
                    secret_seen.store(headers.contains_key(HEADER_NAME_SECRET), Ordering::SeqCst);
                    StatusCode::OK
                }
            }),
        );
        let sink_task = tokio::spawn(async move {
            axum::serve(sink_listener, sink).await.unwrap();
        });

        let redirect_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let redirect_addr = redirect_listener.local_addr().unwrap();
        let target = format!("http://{sink_addr}/");
        let redirect = Router::new().route(
            "/",
            get(move || {
                let target = target.clone();
                async move { (StatusCode::TEMPORARY_REDIRECT, [(LOCATION, target)]) }
            }),
        );
        let redirect_task = tokio::spawn(async move {
            axum::serve(redirect_listener, redirect).await.unwrap();
        });

        let response = build_http_client(false)
            .get(format!("http://{redirect_addr}/"))
            .header(HEADER_NAME_SECRET, "redirect-secret-sentinel")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        tokio::task::yield_now().await;
        assert!(!redirect_seen.load(Ordering::SeqCst));
        assert!(!secret_seen.load(Ordering::SeqCst));

        redirect_task.abort();
        sink_task.abort();
    }
}
