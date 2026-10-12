use crate::{
    handoff, operator,
    store::{Database, Fault, GrantRequest, CHUNK},
};
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Path, State},
    http::{header, HeaderMap, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::{net::SocketAddr, sync::Arc};
use webauthn_rs::prelude::*;

#[derive(Clone)]
pub struct Service {
    pub db: Database,
    pub support_origin: String,
    pub operator_origin: String,
    pub webauthn: Arc<Webauthn>,
    pub challenges: operator::Challenges,
    pub requests: Arc<tokio::sync::Semaphore>,
    pub rates: handoff::Rates,
    pub inspections: Arc<tokio::sync::Semaphore>,
    pub trust_loopback_ingress: bool,
}

pub fn checked_origin(origin: &str, development: bool) -> std::result::Result<Url, Fault> {
    let url = Url::parse(origin).map_err(|_| Fault::Invalid)?;
    if url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
        || url.host_str().is_none()
    {
        return Err(Fault::Invalid);
    }
    if url.scheme() != "https"
        && !(development
            && url.scheme() == "http"
            && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")))
    {
        return Err(Fault::Invalid);
    }
    Ok(url)
}
pub async fn serve(
    db: Database,
    support_origin: String,
    operator_origin: String,
    ingress: SocketAddr,
    operator_addr: SocketAddr,
    development: bool,
    trust_loopback_ingress: bool,
) -> anyhow::Result<()> {
    let support = checked_origin(&support_origin, development).map_err(crate::error)?;
    let origin = checked_origin(&operator_origin, development).map_err(crate::error)?;
    if support.origin() == origin.origin() {
        anyhow::bail!("support and operator origins must be distinct");
    }
    let webauthn = WebauthnBuilder::new(
        origin
            .host_str()
            .ok_or_else(|| crate::error(Fault::Invalid))?,
        &origin,
    )?
    .rp_name("Cinema support operators")
    .build()?;
    let concurrent_requests = db.limits.concurrent_requests as usize;
    let concurrent_inspections = db.limits.concurrent_inspections as usize;
    let service = Service {
        db,
        trust_loopback_ingress,
        support_origin: support.origin().ascii_serialization(),
        operator_origin: origin.origin().ascii_serialization(),
        webauthn: Arc::new(webauthn),
        challenges: operator::Challenges::default(),
        requests: Arc::new(tokio::sync::Semaphore::new(concurrent_requests)),
        rates: handoff::Rates::default(),
        inspections: Arc::new(tokio::sync::Semaphore::new(concurrent_inspections)),
    };
    let public = public_router(&service);
    let operator = operator_router(&service);
    let public_listener = tokio::net::TcpListener::bind(ingress).await?;
    let operator_listener = tokio::net::TcpListener::bind(operator_addr).await?;
    let stop = tokio_util_token();
    let gc = service.clone();
    let token = stop.clone();
    let maintenance = tokio::spawn(async move {
        let mut ticks = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            tokio::select! {_=token.cancelled()=>break,_=ticks.tick()=>{let db=gc.db.clone();let _=tokio::task::spawn_blocking(move||db.reconcile()).await;}}
        }
    });
    let signal = stop.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        signal.cancel();
    });
    let one = stop.clone();
    let two = stop.clone();
    let result = tokio::try_join!(
        axum::serve(
            public_listener,
            public.into_make_service_with_connect_info::<SocketAddr>()
        )
        .with_graceful_shutdown(async move { one.cancelled().await }),
        axum::serve(
            operator_listener,
            operator.into_make_service_with_connect_info::<SocketAddr>()
        )
        .with_graceful_shutdown(async move { two.cancelled().await })
    );
    stop.cancel();
    let _ = maintenance.await;
    result?;
    Ok(())
}
pub fn public_router(service: &Service) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route(
            "/support/v1/{*path}",
            post(api).put(api).get(api).delete(api).options(preflight),
        )
        .route("/", get(handoff::page))
        .route("/phone.js", get(handoff::script))
        .route("/support.css", get(styles))
        .layer(DefaultBodyLimit::max(CHUNK))
        .layer(middleware::from_fn_with_state(service.clone(), bounded))
        .with_state(service.clone())
}
pub fn operator_router(service: &Service) -> Router {
    operator::router()
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::from_fn_with_state(service.clone(), bounded))
        .with_state(service.clone())
}
async fn health(State(service): State<Service>) -> Response {
    response(Ok(
        json!({"state":"ready","version":"cinema.diagnostics.v1","limits":&*service.db.limits,"chunk_bytes":CHUNK,"archive_bytes":crate::store::ARCHIVE,"draft_lifetime_ms":86400000,"enrollment_revocation_delay_ms":86400000,"trusted_loopback_ingress":service.trust_loopback_ingress}),
    ))
}
fn tokio_util_token() -> tokio_util::sync::CancellationToken {
    tokio_util::sync::CancellationToken::new()
}
pub fn secret(headers: &HeaderMap) -> Result<String, Fault> {
    let s = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(Fault::Unauthorized)?;
    if s.len() > 64 {
        return Err(Fault::Unauthorized);
    }
    Ok(s.to_owned())
}
pub fn decode(body: &[u8]) -> Result<Value, Fault> {
    plurx_core::diagnostics::strict_json(body, 64 * 1024).map_err(|_| Fault::Invalid)
}
pub fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str, Fault> {
    v.get(key).and_then(Value::as_str).ok_or(Fault::Invalid)
}
pub fn fields(v: &Value, required: &[&str], optional: &[&str]) -> Result<(), Fault> {
    let o = v.as_object().ok_or(Fault::Invalid)?;
    if required.iter().all(|k| o.contains_key(*k))
        && o.keys()
            .all(|k| required.contains(&k.as_str()) || optional.contains(&k.as_str()))
    {
        Ok(())
    } else {
        Err(Fault::Invalid)
    }
}
pub fn response(result: Result<Value, Fault>) -> Response {
    let r = match result {
        Ok(v) => Json(v).into_response(),
        Err(e) => {
            let (status, code) = match e {
                Fault::Invalid => (StatusCode::BAD_REQUEST, "invalid_report_or_request"),
                Fault::Unauthorized => (StatusCode::UNAUTHORIZED, "authorization_required"),
                Fault::NotFound => (StatusCode::NOT_FOUND, "not_found"),
                Fault::Conflict => (StatusCode::CONFLICT, "state_or_content_conflict"),
                Fault::Expired => (StatusCode::GONE, "expired"),
                Fault::Quota => (StatusCode::TOO_MANY_REQUESTS, "quota_exceeded"),
                Fault::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable"),
            };
            (
                status,
                Json(json!({"error":code,"retryable":matches!(e,Fault::Quota|Fault::Unavailable)})),
            )
                .into_response()
        }
    };
    private(r)
}
pub async fn styles() -> Response {
    let mut r = include_str!("../web/support.css").into_response();
    r.headers_mut()
        .insert(header::CONTENT_TYPE, "text/css".parse().expect("static"));
    private(r)
}
pub fn private(mut r: Response) -> Response {
    r.headers_mut().insert(header::CONTENT_SECURITY_POLICY,"default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'".parse().expect("static"));
    for (k, v) in [
        (header::CACHE_CONTROL, "no-store"),
        (header::REFERRER_POLICY, "no-referrer"),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
    ] {
        r.headers_mut().insert(k, v.parse().expect("static header"));
    }
    r
}
async fn bounded(
    State(service): State<Service>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let Ok(_permit) = service.requests.clone().try_acquire_owned() else {
        return response(Err(Fault::Quota));
    };
    let origin = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let cors = if let Some(origin) = &origin {
        if origin == &service.support_origin || origin == &service.operator_origin {
            true
        } else {
            service
                .db
                .with(|db| {
                    let rows = db
                        .prepare("SELECT origins FROM publishers WHERE active=1")?
                        .query_map([], |r| r.get::<_, String>(0))?
                        .collect::<std::result::Result<Vec<_>, _>>()?;
                    Ok(rows.iter().any(|v| {
                        serde_json::from_str::<Vec<String>>(v).is_ok_and(|o| o.contains(origin))
                    }))
                })
                .unwrap_or(false)
        }
    } else {
        true
    };
    if !cors {
        return response(Err(Fault::Unauthorized));
    }
    if request.method() == Method::POST
        && (request.uri().path().ends_with("/claim")
            || request.uri().path().ends_with("/redeem")
            || request.uri().path().starts_with("/operator/v1/auth/"))
    {
        let tcp = request
            .extensions()
            .get::<axum::extract::ConnectInfo<SocketAddr>>()
            .map(|v| v.0.ip());
        let peer = if service.trust_loopback_ingress && tcp.is_some_and(|ip| ip.is_loopback()) {
            request
                .headers()
                .get("x-cinema-client-ip")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<std::net::IpAddr>().ok())
                .map(|v| v.to_string())
                .unwrap_or_else(|| "invalid-ingress-peer".into())
        } else {
            tcp.map(|v| v.to_string())
                .unwrap_or_else(|| "unknown-peer".into())
        };
        if service
            .rates
            .admit(
                &format!(
                    "{}:{peer}",
                    if request.uri().path().starts_with("/operator/v1/auth/") {
                        "operator-auth"
                    } else {
                        "handoff-code"
                    }
                ),
                service.db.limits.code_attempts_per_minute,
            )
            .is_err()
        {
            return response(Err(Fault::Quota));
        }
    }
    let mut r = next.run(request).await;
    if let Some(origin) = origin {
        if let Ok(value) = origin.parse() {
            r.headers_mut()
                .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, value);
            r.headers_mut()
                .insert(header::VARY, "Origin".parse().expect("static"));
        }
    }
    private(r)
}
async fn preflight() -> Response {
    let mut r = StatusCode::NO_CONTENT.into_response();
    r.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        "GET, POST, PUT, DELETE, OPTIONS".parse().expect("static"),
    );
    r.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        "Authorization, Content-Type, X-Chunk-Sha256, X-Publisher-Id"
            .parse()
            .expect("static"),
    );
    private(r)
}
async fn api(
    State(service): State<Service>,
    Path(path): Path<String>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let _inspection =
        if path.ends_with("/complete") || path.ends_with("/preview") || path.ends_with("/redact") {
            match service.inspections.clone().try_acquire_owned() {
                Ok(p) => Some(p),
                Err(_) => return response(Err(Fault::Quota)),
            }
        } else {
            None
        };
    let db = service.db.clone();
    let service_clone = service.clone();
    let result = tokio::task::spawn_blocking(move || {
        execute(&service_clone, &db, &path, &method, &headers, &body)
    })
    .await
    .unwrap_or(Err(Fault::Unavailable));
    response(result)
}
fn execute(
    service: &Service,
    db: &Database,
    path: &str,
    method: &Method,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<Value, Fault> {
    let parts = path.split('/').collect::<Vec<_>>();
    let auth = secret(headers).ok();
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        if origin != service.support_origin {
            let publisher =
                if let Some(id) = headers.get("x-publisher-id").and_then(|v| v.to_str().ok()) {
                    db.publisher(id, auth.as_deref().ok_or(Fault::Unauthorized)?)?;
                    id.to_owned()
                } else {
                    db.authority(auth.as_deref().ok_or(Fault::Unauthorized)?)?
                        .publisher
                };
            let allowed = db.with(|c| {
                let raw: String = c.query_row(
                    "SELECT origins FROM publishers WHERE id=?1 AND active=1",
                    [publisher],
                    |r| r.get(0),
                )?;
                Ok(serde_json::from_str::<Vec<String>>(&raw)
                    .is_ok_and(|v| v.iter().any(|v| v == origin)))
            })?;
            if !allowed {
                return Err(Fault::Unauthorized);
            }
        }
    }
    match (method.as_str(), parts.as_slice()) {
        ("POST", ["enrollments"]) => {
            let v = decode(body)?;
            fields(&v, &["principal"], &[])?;
            let publisher = headers
                .get("x-publisher-id")
                .and_then(|v| v.to_str().ok())
                .ok_or(Fault::Unauthorized)?;
            db.enrollment(
                publisher,
                auth.as_deref().ok_or(Fault::Unauthorized)?,
                text(&v, "principal")?,
            )
        }
        ("DELETE", ["enrollments"]) => {
            let v = decode(body)?;
            fields(&v, &["principal"], &[])?;
            let publisher = headers
                .get("x-publisher-id")
                .and_then(|v| v.to_str().ok())
                .ok_or(Fault::Unauthorized)?;
            db.revoke_enrollment(
                publisher,
                auth.as_deref().ok_or(Fault::Unauthorized)?,
                text(&v, "principal")?,
            )?;
            Ok(json!({"state":"revoked"}))
        }
        ("POST", ["grants"]) => {
            let request: GrantRequest =
                serde_json::from_value(decode(body)?).map_err(|_| Fault::Invalid)?;
            let publisher = headers.get("x-publisher-id").and_then(|v| v.to_str().ok());
            db.grant(publisher.zip(auth.as_deref()), auth.as_deref(), &request)
        }
        ("POST", ["uploads"]) => {
            let request: GrantRequest =
                serde_json::from_value(decode(body)?).map_err(|_| Fault::Invalid)?;
            db.reserve(auth.as_deref().ok_or(Fault::Unauthorized)?, &request)
        }
        ("PUT", ["uploads", id, "chunks", index]) => {
            let index = index.parse::<u32>().map_err(|_| Fault::Invalid)?;
            let hash = headers
                .get("x-chunk-sha256")
                .and_then(|v| v.to_str().ok())
                .ok_or(Fault::Invalid)?;
            db.chunk(
                auth.as_deref().ok_or(Fault::Unauthorized)?,
                id,
                index,
                hash,
                body,
            )
        }
        ("GET", ["uploads", id, "status"]) | ("GET", ["reports", id, "receipt"]) => {
            db.status(auth.as_deref().ok_or(Fault::Unauthorized)?, id)
        }
        ("POST", ["uploads", id, "refresh"]) => {
            db.refresh(auth.as_deref().ok_or(Fault::Unauthorized)?, id)
        }
        ("POST", ["uploads", id, "complete"]) => {
            db.complete(auth.as_deref().ok_or(Fault::Unauthorized)?, id)
        }
        ("POST", ["reports", id, "submit"]) => {
            let v = decode(body)?;
            fields(&v, &["description"], &[])?;
            db.submit(
                auth.as_deref().ok_or(Fault::Unauthorized)?,
                id,
                text(&v, "description")?,
            )
        }
        ("DELETE", ["reports", id]) => {
            db.erase(auth.as_deref().ok_or(Fault::Unauthorized)?, id)?;
            Ok(json!({"state":"deleted"}))
        }
        _ => handoff::execute(service, path, method, headers, body),
    }
}
