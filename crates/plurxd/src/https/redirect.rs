//! Redirect only the public plaintext app; TLS and compatibility listeners
//! retain the original router and therefore cannot redirect back to themselves.
use axum::{
    body::Body,
    extract::{OriginalUri, Request, State},
    http::{header, uri::Authority, HeaderMap, HeaderValue, StatusCode, Uri},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    Router,
};
use std::{env::VarError, net::Ipv6Addr};

/// Resolve once at boot, independently of later saves of desired settings.
/// Docker can publish a different host port from the actual TLS bind port.
pub(crate) fn port(
    active: Option<u16>,
    published: Result<String, VarError>,
) -> anyhow::Result<Option<u16>> {
    let Some(active) = active else {
        return Ok(None);
    };
    let port = match published {
        Err(VarError::NotPresent) => active,
        Ok(value) => value
            .parse::<u16>()
            .ok()
            .filter(|port| *port != 0)
            .ok_or_else(|| {
                anyhow::anyhow!("PLURX_HTTPS_REDIRECT_PORT must be a port between 1 and 65535")
            })?,
        Err(VarError::NotUnicode(_)) => anyhow::bail!("PLURX_HTTPS_REDIRECT_PORT must be UTF-8"),
    };
    Ok(Some(port))
}

pub(crate) fn router(app: Router, port: Option<u16>) -> Router {
    match port {
        Some(port) => app.layer(middleware::from_fn_with_state(port, redirect)),
        None => app,
    }
}

fn machine_endpoint(path: &str) -> bool {
    matches!(
        path,
        "/healthz"
            | "/readyz"
            | "/metrics"
            | "/search"
            | "/identity"
            | "/api/v1/server"
            | "/api/v1/server/node"
            | "/api/v1/cluster/join/redeem"
            | "/api/v1/cluster/join/finalize"
            | "/api/v1/cluster/learner/join/redeem"
            | "/api/v1/cluster/learner/join/finalize"
    ) || [
        "/internal",
        "/_internal",
        "/api/v1/internal",
        "/api/v1/cluster/artwork",
        "/jellyfin",
        "/library",
        "/:",
        "/hubs",
        "/photo",
    ]
    .iter()
    .any(|prefix| {
        path == *prefix
            || path
                .strip_prefix(*prefix)
                .is_some_and(|tail| tail.starts_with('/'))
    })
}

fn destination(uri: &Uri, headers: &HeaderMap, port: u16) -> Option<HeaderValue> {
    let mut hosts = headers.get_all(header::HOST).iter();
    let host = hosts.next();
    if hosts.next().is_some() {
        return None;
    }
    let authority = match uri.authority() {
        Some(authority) => authority.clone(),
        None => host?.to_str().ok()?.parse::<Authority>().ok()?,
    };
    if authority.as_str().contains('@')
        || (authority.as_str() != authority.host() && authority.port_u16().is_none())
    {
        return None;
    }
    let host = authority.host();
    let unbracketed = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    let host = if unbracketed.parse::<Ipv6Addr>().is_ok() {
        format!("[{unbracketed}]")
    } else if host.is_empty() || !host.is_ascii() || host.contains([':', '\\', '%']) {
        return None;
    } else {
        host.to_owned()
    };
    let authority = if port == 443 {
        host
    } else {
        format!("{host}:{port}")
    };
    let path = uri.path_and_query().map_or("/", |value| value.as_str());
    if !path.starts_with('/') {
        return None;
    }
    HeaderValue::from_str(&format!("https://{authority}{path}")).ok()
}

async fn redirect(State(port): State<u16>, request: Request, next: Next) -> Response {
    let uri = request
        .extensions()
        .get::<OriginalUri>()
        .map_or(request.uri(), |original| &original.0);
    if machine_endpoint(uri.path())
        || (uri.path() == "/" && crate::http::looks_like_plex(request.headers()))
    {
        return next.run(request).await;
    }
    let Some(location) = destination(uri, request.headers(), port) else {
        return (
            StatusCode::BAD_REQUEST,
            "A valid request authority is required",
        )
            .into_response();
    };
    // A permanent redirect could strand a browser after HTTPS is disabled.
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::TEMPORARY_REDIRECT;
    response.headers_mut().insert(header::LOCATION, location);
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;

    async fn response(port: Option<u16>, uri: &str, host: Option<&str>) -> Response {
        let app = router(Router::new().fallback(|| async { "served" }), port);
        let mut request = Request::builder().uri(uri);
        if let Some(host) = host {
            request = request.header(header::HOST, host);
        }
        app.oneshot(request.body(Body::empty()).expect("request"))
            .await
            .expect("response")
    }

    #[tokio::test]
    async fn active_https_redirects_http_preserving_host_path_query_and_ipv6() {
        for (uri, host, port, expected) in [
            (
                "/",
                "media.example:32400",
                32443,
                "https://media.example:32443/",
            ),
            (
                "/watch/42?next=%2Fsettings&x=a%20b",
                "media.example:32400",
                33443,
                "https://media.example:33443/watch/42?next=%2Fsettings&x=a%20b",
            ),
            (
                "/settings",
                "[2001:db8::1]:32400",
                32443,
                "https://[2001:db8::1]:32443/settings",
            ),
            (
                "/settings",
                "media.example:32400",
                443,
                "https://media.example/settings",
            ),
        ] {
            let response = response(Some(port), uri, Some(host)).await;
            assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
            assert_eq!(response.headers()[header::LOCATION], expected);
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        }
        let response = response(Some(32443), "http://media.example:32400/settings?x=1", None).await;
        assert_eq!(
            response.headers()[header::LOCATION],
            "https://media.example:32443/settings?x=1"
        );
    }

    #[tokio::test]
    async fn disabled_https_and_machine_protocols_keep_their_http_handlers() {
        assert_eq!(
            response(None, "/settings?x=1", None).await.status(),
            StatusCode::OK
        );
        for path in [
            "/healthz",
            "/readyz",
            "/metrics",
            "/identity",
            "/api/v1/server",
            "/api/v1/server/node",
            "/internal/media",
            "/_internal/clock",
            "/api/v1/internal/operations",
            "/jellyfin",
            "/jellyfin/Users",
            "/library/sections",
            "/:/timeline",
            "/hubs/search",
            "/photo/:/transcode",
            "/search",
        ] {
            let response = response(Some(32443), path, None).await;
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            assert!(!response.headers().contains_key(header::LOCATION));
        }
        assert_eq!(
            response(Some(32443), "/internal-lookalike", Some("media.example"))
                .await
                .status(),
            StatusCode::TEMPORARY_REDIRECT
        );
        assert_eq!(
            response(Some(32443), "/api/v1/me", Some("media.example"))
                .await
                .status(),
            StatusCode::TEMPORARY_REDIRECT
        );
    }

    #[tokio::test]
    async fn plex_capabilities_keep_their_http_root_while_browser_root_redirects() {
        let app = router(Router::new().fallback(|| async { "served" }), Some(32443));
        let request = Request::builder()
            .uri("/")
            .header("x-plex-product", "Plexamp")
            .body(Body::empty())
            .expect("Plex request");
        assert_eq!(
            app.oneshot(request).await.expect("response").status(),
            StatusCode::OK
        );
        assert_eq!(
            response(Some(32443), "/", Some("media.example"))
                .await
                .status(),
            StatusCode::TEMPORARY_REDIRECT
        );
    }

    #[tokio::test]
    async fn nested_routes_keep_the_original_public_path_and_internal_exception() {
        let nested = Router::new()
            .route("/me", axum::routing::get(|| async { "account" }))
            .route(
                "/internal/operations",
                axum::routing::get(|| async { "RPC" }),
            );
        let app = router(Router::new().nest("/api/v1", nested), Some(32443));
        let request = Request::builder()
            .uri("/api/v1/me?x=1")
            .header(header::HOST, "media.example:32400")
            .body(Body::empty())
            .expect("request");
        let response = app.clone().oneshot(request).await.expect("response");
        assert_eq!(
            response.headers()[header::LOCATION],
            "https://media.example:32443/api/v1/me?x=1"
        );
        let request = Request::builder()
            .uri("/api/v1/internal/operations")
            .body(Body::empty())
            .expect("RPC");
        assert_eq!(
            app.oneshot(request).await.expect("RPC response").status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn cluster_bootstrap_and_peer_artwork_keep_http_without_exempting_admin_routes() {
        // Join and peer-artwork clients deliberately refuse redirects. Their
        // public-looking routes still belong to the cluster machine protocol.
        for path in [
            "/api/v1/cluster/join/redeem",
            "/api/v1/cluster/join/finalize",
            "/api/v1/cluster/learner/join/redeem",
            "/api/v1/cluster/learner/join/finalize",
            "/api/v1/cluster/artwork/poster.jpg",
        ] {
            let response = response(Some(32443), path, None).await;
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            assert!(!response.headers().contains_key(header::LOCATION));
        }
        for path in [
            "/api/v1/cluster",
            "/api/v1/cluster/join-tokens",
            "/api/v1/cluster/join/redeem-lookalike",
            "/api/v1/cluster/artwork-lookalike/poster.jpg",
        ] {
            assert_eq!(
                response(Some(32443), path, Some("media.example"))
                    .await
                    .status(),
                StatusCode::TEMPORARY_REDIRECT,
                "{path}"
            );
        }
    }

    #[tokio::test]
    async fn malformed_authorities_cannot_create_a_redirect() {
        for host in [
            None,
            Some(""),
            Some("user@media.example"),
            Some("media.example:bad"),
            Some("media.example:65536"),
            Some("media.example/path"),
        ] {
            assert_eq!(
                response(Some(32443), "/", host).await.status(),
                StatusCode::BAD_REQUEST,
                "{host:?}"
            );
        }
        let app = router(Router::new().fallback(|| async { "served" }), Some(32443));
        let request = Request::builder()
            .uri("/")
            .header(header::HOST, "media.example")
            .header(header::HOST, "other.example")
            .body(Body::empty())
            .expect("request");
        assert_eq!(
            app.oneshot(request).await.expect("response").status(),
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn redirects_use_only_the_boot_active_https_and_published_port() {
        assert_eq!(port(None, Ok("443".into())).expect("disabled"), None);
        assert_eq!(
            port(Some(32443), Err(VarError::NotPresent)).expect("bind port"),
            Some(32443)
        );
        assert_eq!(
            port(Some(32443), Ok("443".into())).expect("published port"),
            Some(443)
        );
        for value in ["", "0", "65536", "https://media.example"] {
            assert!(port(Some(32443), Ok(value.into())).is_err());
        }
    }
}
