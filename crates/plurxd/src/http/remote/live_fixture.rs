//! Explicit local acceptance seam; no shipped route or credential is added.
use super::*;
use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};

fn asset_path(root: &Path, relative: &str) -> Option<PathBuf> {
    if relative.is_empty()
        || Path::new(relative)
            .components()
            .any(|p| !matches!(p, Component::Normal(_)))
    {
        return None;
    }
    let file = root.join(relative).canonicalize().ok()?;
    (file.starts_with(root) && file.is_file()).then_some(file)
}
fn web_snapshot(root: &Path) -> String {
    fn visit(root: &Path, dir: &Path, hash: &mut Sha256) {
        let mut files = std::fs::read_dir(dir)
            .expect("fixture web tree")
            .map(|e| e.expect("web entry").path())
            .collect::<Vec<_>>();
        files.sort();
        for file in files {
            if file.is_symlink() {
                continue;
            }
            if file.is_dir() {
                visit(root, &file, hash);
            } else {
                hash.update(
                    file.strip_prefix(root)
                        .expect("relative asset")
                        .to_string_lossy()
                        .as_bytes(),
                );
                hash.update([0]);
                hash.update(std::fs::read(&file).expect("asset snapshot"));
            }
        }
    }
    let mut hash = Sha256::new();
    visit(root, root, &mut hash);
    hex::encode(hash.finalize())
}
async fn overlay(
    request: axum::extract::Request,
    next: axum::middleware::Next,
    root: Option<PathBuf>,
) -> Response {
    if request.method() == Method::GET {
        let relative = match request.uri().path() {
            "/" => Some("index.html"),
            path => path.strip_prefix("/assets/"),
        };
        if let Some(file) = root
            .as_deref()
            .and_then(|root| relative.and_then(|path| asset_path(root, path)))
        {
            if std::fs::metadata(&file).is_ok_and(|m| m.len() <= 4 * 1024 * 1024) {
                if let Ok(bytes) = tokio::fs::read(&file).await {
                    let kind = match file.extension().and_then(|e| e.to_str()) {
                        Some("html") => "text/html; charset=utf-8",
                        Some("js") => "application/javascript",
                        Some("css") => "text/css",
                        Some("svg") => "image/svg+xml",
                        _ => "application/octet-stream",
                    };
                    // Run the real Router first, retaining its security and
                    // request middleware. Unknown new overlay assets borrow
                    // the same production static-header handler as known ones.
                    let is_shell = relative == Some("index.html");
                    let mut response = next.run(request).await;
                    let security = if is_shell {
                        super::super::web::index().await
                    } else {
                        super::super::web::asset(
                            axum::extract::Path("core/errors.js".to_owned()),
                            HeaderMap::new(),
                        )
                        .await
                    };
                    for name in [
                        "x-content-type-options",
                        "referrer-policy",
                        "cross-origin-resource-policy",
                        "content-security-policy",
                    ] {
                        if let Some(value) = security.headers().get(name) {
                            response.headers_mut().insert(name, value.clone());
                        }
                    }
                    *response.status_mut() = StatusCode::OK;
                    for name in [
                        "content-length",
                        "content-encoding",
                        "etag",
                        "last-modified",
                    ] {
                        response.headers_mut().remove(name);
                    }
                    response
                        .headers_mut()
                        .insert("content-type", axum::http::HeaderValue::from_static(kind));
                    response.headers_mut().insert(
                        "cache-control",
                        axum::http::HeaderValue::from_static("no-store"),
                    );
                    *response.body_mut() = Body::from(bytes);
                    return response;
                }
            }
        }
    }
    next.run(request).await
}

#[test]
fn remote_live_overlay_refuses_traversal_and_external_symlinks() {
    let dir = tempfile::tempdir().expect("fixture root");
    std::fs::write(dir.path().join("index.html"), "synthetic").expect("index");
    let root = dir.path().canonicalize().expect("root");
    assert!(asset_path(&root, "index.html").is_some());
    for path in ["", "../index.html", "/etc/passwd", "a/../../index.html"] {
        assert!(asset_path(&root, path).is_none());
    }
    #[cfg(unix)]
    {
        let external = tempfile::NamedTempFile::new().expect("external file");
        std::os::unix::fs::symlink(external.path(), root.join("outside.js")).expect("symlink");
        assert!(asset_path(&root, "outside.js").is_none());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "explicit loopback-only synthetic acceptance server; stops by file, Ctrl-C or one hour"]
async fn remote_live_server_fixture() {
    let state = super::super::tests::test_state_without_router();
    let username = "cinema-remote-fixture";
    let password = "cinema-remote-fixture-only";
    let user = state
        .store
        .create_user(
            username,
            &auth::hash_password(password).expect("fixture password"),
            false,
        )
        .await
        .expect("synthetic viewer");
    state
        .store
        .put_setting(FEATURE_KEY, "1")
        .await
        .expect("fixture feature");
    let make_token = async || {
        let token = auth::generate_token().expect("synthetic token");
        state
            .store
            .create_token(&auth::hash_token(&token), user.id, None)
            .await
            .expect("synthetic Native login");
        token
    };
    let tv_token = make_token().await;
    let phone_token = make_token().await;
    let admin_username = "cinema-remote-admin-fixture";
    let admin_password = "cinema-remote-admin-fixture-only";
    let admin = state
        .store
        .create_user(
            admin_username,
            &auth::hash_password(admin_password).expect("synthetic admin password"),
            true,
        )
        .await
        .expect("synthetic admin");
    let admin_token = auth::generate_token().expect("synthetic admin token");
    state
        .store
        .create_token(&auth::hash_token(&admin_token), admin.id, None)
        .await
        .expect("synthetic admin Native login");
    let source = plurx_core::testfixtures::source("h264");
    let library = state
        .store
        .create_library(&plurx_core::domain::NewLibrary {
            name: "Remote fixture".into(),
            kind: plurx_core::domain::LibraryKind::Movies,
            paths: vec![source.parent().expect("fixture parent").to_path_buf()],
            anime: false,
        })
        .await
        .expect("fixture library");
    let item = state
        .store
        .insert_item(&plurx_core::domain::NewItem {
            library_id: library.id,
            kind: plurx_core::domain::ItemKind::Movie,
            parent_id: None,
            title: "B05\nFixture\tTitle".into(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("fixture item");
    let probe = plurx_core::scan::probe::probe(&source)
        .await
        .expect("actual synthetic media probe");
    let file = state
        .store
        .upsert_file(
            item,
            &source.to_string_lossy(),
            i64::try_from(std::fs::metadata(&source).expect("synthetic media").len())
                .expect("bounded file"),
            1,
            &probe,
        )
        .await
        .expect("fixture file");
    let root = std::env::var_os("PLURX_REMOTE_FIXTURE_WEB_ROOT").map(|p| {
        PathBuf::from(p)
            .canonicalize()
            .expect("explicit web overlay")
    });
    let api_hash = hex::encode(Sha256::digest(
        concat!(
            include_str!("../remote.rs"),
            include_str!("owner.rs"),
            include_str!("wire.rs")
        )
        .as_bytes(),
    ));
    let app = super::super::router(state.clone()).layer(axum::middleware::from_fn({
        let root = root.clone();
        move |request, next| overlay(request, next, root.clone())
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback random port");
    let address = listener.local_addr().expect("loopback address");
    let output = std::env::var_os("PLURX_REMOTE_FIXTURE_OUTPUT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    tokio::fs::create_dir_all(&output)
        .await
        .expect("fixture output directory");
    let fixture = output.join("cinema-b04-live-seam.json");
    let stop = output.join("cinema-b04-live-seam.stop");
    if stop.exists() {
        std::fs::remove_file(&stop).expect("old fixture shutdown marker");
    }
    let details = json!({"synthetic_only":true,"base_url":format!("http://{address}"),"api_prefix":"/api/remote/v1","instance_id":state.store.instance_id().await.expect("instance"),"user_id":user.id,"username":username,"password":password,"tv_token":tv_token,"phone_token":phone_token,"admin_username":admin_username,"admin_password":admin_password,"admin_user_id":admin.id,"admin_token":admin_token,"library_id":library.id,"item_id":item,"file_id":file,"api_source_revision":std::env::var("PLURX_REMOTE_FIXTURE_API_REVISION").ok(),"api_source_sha256":api_hash,"web_root":root,"web_source_revision":std::env::var("PLURX_REMOTE_FIXTURE_WEB_REVISION").ok(),"web_source_sha256":root.as_deref().map(web_snapshot),"shutdown_file":stop,"expires_at":now_seconds().expect("clock")+3600,"lifetime_seconds":3600});
    tokio::fs::write(
        &fixture,
        serde_json::to_vec_pretty(&details).expect("fixture JSON"),
    )
    .await
    .expect("synthetic fixture details");
    println!(
        "Synthetic Cinema Remote acceptance server: http://{address}; details: {}",
        fixture.display()
    );
    let shutdown_marker = stop.clone();
    let shutdown_hub = state.remote.clone();
    let shutdown = async move {
        tokio::select! {
            _=tokio::signal::ctrl_c()=>{},
            _=tokio::time::sleep(Duration::from_secs(3600))=>{},
            _=async {while !tokio::fs::try_exists(&shutdown_marker).await.unwrap_or(false) {tokio::time::sleep(Duration::from_millis(250)).await;}}=>{},
        }
        shutdown_hub.disable();
    };
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
        .expect("fixture server");
    state.remote.disable();
    let _ = tokio::fs::remove_file(&fixture).await;
    let _ = tokio::fs::remove_file(&stop).await;
}

#[tokio::test]
async fn remote_live_overlay_retains_production_security_headers() {
    use tower::ServiceExt;
    let dir = tempfile::tempdir().expect("overlay root");
    std::fs::write(dir.path().join("index.html"), "synthetic shell").expect("shell");
    std::fs::write(dir.path().join("new-fixture.js"), "window.synthetic=1;").expect("asset");
    let root = dir.path().canonicalize().expect("root");
    let state = super::super::tests::test_state_without_router();
    let app = super::super::router(state).layer(axum::middleware::from_fn(move |request, next| {
        overlay(request, next, Some(root.clone()))
    }));
    for (path, body, is_shell) in [
        ("/", "synthetic shell", true),
        ("/assets/new-fixture.js", "window.synthetic=1;", false),
    ] {
        let response = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri(path)
                    .header("accept-encoding", "gzip")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        assert_eq!(response.headers()["referrer-policy"], "same-origin");
        assert_eq!(
            response.headers()["cross-origin-resource-policy"],
            "same-origin"
        );
        if is_shell {
            assert_eq!(
                response.headers()["content-security-policy"],
                "frame-ancestors 'none'; base-uri 'none'; object-src 'none'"
            );
        }
        assert!(!response.headers().contains_key("content-encoding"));
        assert!(!response.headers().contains_key("etag"));
        assert_eq!(
            axum::body::to_bytes(response.into_body(), 4096)
                .await
                .expect("body")
                .as_ref(),
            body.as_bytes()
        );
    }
}
