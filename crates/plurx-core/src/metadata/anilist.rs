//! AniList metadata provider for anime (REQ-META-3).
//!
//! AniList's public GraphQL API needs no API key, and gives anime-correct
//! titles (romaji/english/native), synopsis, year, and artwork. Per-episode
//! detail is limited on AniList, so episodes keep their absolute numbering and
//! generic titles — the series-level match is what makes an anime library
//! look right.

use serde_json::Value;
use std::time::Instant;

use super::{
    bounded_json_response, provider_client, provider_outcome, record_provider_request, HostHealth,
    Provider, ProviderOutcome, PROVIDER_CALL_BUDGET, PROVIDER_CONNECT_TIMEOUT,
    PROVIDER_READ_TIMEOUT, PROVIDER_TOTAL_TIMEOUT,
};
use crate::error::MetadataError;

const API: &str = "https://graphql.anilist.co";

/// One transient transport failure must not disable a whole enrichment pass.
/// One retry is enough to distinguish that blip from an outage; subsequent
/// titles wait for the next pass instead of each spending another call budget.
const TRANSPORT_ATTEMPTS: u32 = 2;

const SEARCH_QUERY: &str = r#"
query ($search: String) {
  Media(search: $search, type: ANIME) {
    id
    title { romaji english native }
    description(asHtml: false)
    seasonYear
    genres
    coverImage { extraLarge large }
    bannerImage
  }
}"#;

/// A resolved AniList series match.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AniMatch {
    pub anilist_id: i64,
    /// Preferred display title (english, else romaji, else native).
    pub title: String,
    pub year: Option<i32>,
    pub overview: Option<String>,
    /// Absolute image URLs (AniList serves full URLs, unlike TMDB).
    pub cover_url: Option<String>,
    pub banner_url: Option<String>,
    /// AniList's own genre vocabulary ("Action", "Slice of Life"), which
    /// overlaps TMDB's without matching it. Not translated: an anime library
    /// is enriched entirely from AniList, so its genre facets are internally
    /// consistent, and a hand-written mapping table would be a second thing
    /// to keep current for no reader's benefit.
    ///
    /// Free — `genres` is one more field on the search query that already
    /// runs, so this costs no request at all, not even a cached one.
    pub genres: Vec<String>,
}

pub struct AniListClient {
    budget: super::budget::SharedProviderBudget,
    http: reqwest::Client,
    /// GraphQL endpoint (defaults to [`API`]); overridable for tests.
    base: String,
    /// The GraphQL host and the image CDN, as in `TmdbClient`. A transient
    /// send failure is retried once before it can trip either host.
    api_health: HostHealth,
    image_health: HostHealth,
}

impl AniListClient {
    pub fn with_budget(mut self, budget: super::budget::SharedProviderBudget) -> Self {
        self.budget = budget;
        self
    }

    pub fn new() -> Self {
        AniListClient {
            budget: None,
            http: provider_client(
                Provider::AniList,
                reqwest::Client::builder()
                    .user_agent(concat!("plurx/", env!("CARGO_PKG_VERSION")))
                    .connect_timeout(PROVIDER_CONNECT_TIMEOUT)
                    .read_timeout(PROVIDER_READ_TIMEOUT)
                    .timeout(PROVIDER_TOTAL_TIMEOUT)
                    .redirect(reqwest::redirect::Policy::limited(3)),
            ),
            base: API.to_owned(),
            api_health: HostHealth::default(),
            image_health: HostHealth::default(),
        }
    }

    /// Why this client stopped calling AniList in this pass, if it has.
    pub fn unreachable(&self) -> Option<&str> {
        self.api_health.cause()
    }

    /// The same for AniList's image CDN.
    pub fn images_unreachable(&self) -> Option<&str> {
        self.image_health.cause()
    }

    /// Point the GraphQL endpoint elsewhere (mock server in tests).
    pub fn with_base(mut self, base: impl Into<String>) -> Self {
        self.base = base.into();
        self
    }

    /// Retry only connect failures/timeouts before receiving a response.
    /// GraphQL here is a read-only search and artwork uses GET, so repeating
    /// either does not repeat a mutation. Every send charges the shared
    /// allowance and obeys cancellation; no periodic timer or retry task exists.
    /// A response (including 429/5xx), parse error or body failure retains its
    /// existing handling rather than being mistaken for a dead provider.
    async fn send_with_transport_retry(
        &self,
        health: &HostHealth,
        build: impl Fn() -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, MetadataError> {
        // Allowance acquisition has its own wait budget. Cap the *whole*
        // operation too, so a cooldown during the retry cannot restart this
        // call's clock. Dropping these futures owns no subprocess or task.
        tokio::time::timeout(
            PROVIDER_CALL_BUDGET,
            self.send_transport_attempts(health, build),
        )
        .await
        .map_err(|_| MetadataError::Timeout("provider call retry budget exhausted".to_owned()))?
    }

    async fn send_transport_attempts(
        &self,
        health: &HostHealth,
        build: impl Fn() -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, MetadataError> {
        let started = Instant::now();
        for attempt in 1..=TRANSPORT_ATTEMPTS {
            health.check()?;
            super::budget::acquire(&self.budget, Provider::AniList).await?;
            // Another call can have tripped the host while allowance waited.
            health.check()?;
            let remaining = PROVIDER_CALL_BUDGET.saturating_sub(started.elapsed());
            if remaining < PROVIDER_CONNECT_TIMEOUT {
                return Err(MetadataError::Timeout(
                    "provider call retry budget exhausted".to_owned(),
                ));
            }
            let response = super::budget::cancellable(
                build()
                    .timeout(remaining.min(PROVIDER_TOTAL_TIMEOUT))
                    .send(),
            )
            .await?;
            match response {
                Ok(response) => return Ok(response),
                Err(error)
                    if attempt < TRANSPORT_ATTEMPTS
                        && (error.is_connect() || error.is_timeout()) => {}
                Err(error) => {
                    let error = health.failed(error);
                    record_provider_request(Provider::AniList, provider_outcome(&error));
                    return Err(error);
                }
            }
        }
        unreachable!("the last transport attempt always returns")
    }

    /// Search anime by title and return the best (first) match.
    pub async fn find_anime(&self, title: &str) -> Result<Option<AniMatch>, MetadataError> {
        let body = serde_json::json!({
            "query": SEARCH_QUERY,
            "variables": { "search": title },
        });
        let resp = self
            .send_with_transport_retry(&self.api_health, || self.http.post(&self.base).json(&body))
            .await?;
        super::budget::observe(&self.budget, Provider::AniList, &resp).await?;
        // AniList returns 404 with a GraphQL error when nothing matches.
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            record_provider_request(Provider::AniList, ProviderOutcome::Status);
            return Ok(None);
        }
        if !resp.status().is_success() {
            record_provider_request(Provider::AniList, ProviderOutcome::Status);
            return Err(MetadataError::Status(resp.status().as_u16()));
        }
        let result = match bounded_json_response(resp).await {
            Ok(bytes) => serde_json::from_slice::<Value>(&bytes)
                .map_err(|error| MetadataError::Parse(error.to_string())),
            Err(error) => Err(error),
        };
        record_provider_request(
            Provider::AniList,
            result
                .as_ref()
                .map(|_| ProviderOutcome::Ok)
                .unwrap_or_else(provider_outcome),
        );
        let json = result?;
        Ok(parse_media(json.get("data").and_then(|d| d.get("Media"))))
    }

    /// Download an image from an absolute URL (AniList serves full URLs).
    pub async fn download_image(&self, url: &str) -> Result<Vec<u8>, MetadataError> {
        let resp = self
            .send_with_transport_retry(&self.image_health, || self.http.get(url))
            .await?;
        super::budget::observe(&self.budget, Provider::AniList, &resp).await?;
        if !resp.status().is_success() {
            record_provider_request(Provider::AniList, ProviderOutcome::Status);
            return Err(MetadataError::Status(resp.status().as_u16()));
        }
        let result = super::bounded_artwork_response(resp).await;
        record_provider_request(
            Provider::AniList,
            result
                .as_ref()
                .map(|_| ProviderOutcome::Ok)
                .unwrap_or_else(provider_outcome),
        );
        result
    }
}

impl Default for AniListClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse the `Media` object from an AniList response.
fn parse_media(media: Option<&Value>) -> Option<AniMatch> {
    let media = media?;
    if media.is_null() {
        return None;
    }
    let title = media.get("title");
    let display = title
        .and_then(|t| t.get("english"))
        .and_then(|v| v.as_str())
        .or_else(|| title.and_then(|t| t.get("romaji")).and_then(|v| v.as_str()))
        .or_else(|| title.and_then(|t| t.get("native")).and_then(|v| v.as_str()))?
        .to_owned();

    Some(AniMatch {
        anilist_id: media.get("id").and_then(|v| v.as_i64()).unwrap_or(0),
        title: display,
        year: media
            .get("seasonYear")
            .and_then(|v| v.as_i64())
            .map(|y| y as i32),
        overview: media
            .get("description")
            .and_then(|v| v.as_str())
            .map(strip_html)
            .filter(|s| !s.is_empty()),
        cover_url: media
            .get("coverImage")
            .and_then(|c| c.get("extraLarge").or_else(|| c.get("large")))
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        banner_url: media
            .get("bannerImage")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
        genres: media
            .get("genres")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|g| g.as_str())
                    .filter(|g| !g.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// AniList descriptions carry light HTML (`<br>`, `<i>`); strip tags to plain
/// text so the overview renders cleanly.
fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            // A closed tag becomes a space so `over<br>but` → `over but`.
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#039;", "'")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_media_prefers_english_title() {
        let media = json!({
            "id": 154587,
            "title": { "romaji": "Sousou no Frieren", "english": "Frieren: Beyond Journey's End", "native": "葬送のフリーレン" },
            "description": "The <i>adventure</i> is over<br>but life goes on.",
            "seasonYear": 2023,
            "coverImage": { "extraLarge": "https://img/cover.jpg", "large": "https://img/large.jpg" },
            "bannerImage": "https://img/banner.jpg"
        });
        let m = parse_media(Some(&media)).expect("match");
        assert_eq!(m.anilist_id, 154587);
        assert_eq!(m.title, "Frieren: Beyond Journey's End");
        assert_eq!(m.year, Some(2023));
        assert_eq!(
            m.overview.as_deref(),
            Some("The adventure is over but life goes on.")
        );
        assert_eq!(m.cover_url.as_deref(), Some("https://img/cover.jpg"));
        assert_eq!(m.banner_url.as_deref(), Some("https://img/banner.jpg"));
    }

    #[test]
    fn null_media_is_no_match() {
        assert!(parse_media(Some(&json!(null))).is_none());
        assert!(parse_media(None).is_none());
    }

    #[test]
    fn falls_back_to_romaji() {
        let media = json!({
            "id": 1, "title": { "romaji": "Bocchi the Rock!", "english": null, "native": "..." }
        });
        assert_eq!(
            parse_media(Some(&media)).expect("m").title,
            "Bocchi the Rock!"
        );
    }

    async fn serve(app: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn find_anime_hits_graphql_and_downloads() {
        use axum::routing::{get, post};
        use axum::Json;
        let app = axum::Router::new()
            .route(
                "/",
                post(|| async {
                    Json(json!({ "data": { "Media": {
                        "id": 21, "title": { "english": "One Piece", "romaji": "One Piece" },
                        "description": "Pirates.", "seasonYear": 1999,
                        "coverImage": { "extraLarge": "https://img/op.jpg" },
                        "bannerImage": "https://img/op-banner.jpg"
                    }}}))
                }),
            )
            .route("/img.jpg", get(|| async { vec![9u8, 8, 7] }));
        let base = serve(app).await;
        let c = AniListClient::new().with_base(&base);
        let m = c.find_anime("One Piece").await.expect("ok").expect("match");
        assert_eq!(m.anilist_id, 21);
        assert_eq!(m.title, "One Piece");
        assert_eq!(m.year, Some(1999));

        let bytes = c
            .download_image(&format!("{base}/img.jpg"))
            .await
            .expect("image");
        assert_eq!(bytes, vec![9, 8, 7]);
    }

    #[tokio::test]
    async fn not_found_is_none_and_error_status_propagates() {
        use axum::http::StatusCode;
        // AniList answers 404 when nothing matches.
        let nf = serve(axum::Router::new().fallback(|| async { StatusCode::NOT_FOUND })).await;
        let c = AniListClient::new().with_base(&nf);
        assert!(c.find_anime("nothing").await.expect("ok").is_none());

        // A 500 is a hard error.
        let boom =
            serve(axum::Router::new().fallback(|| async { StatusCode::INTERNAL_SERVER_ERROR }))
                .await;
        let c = AniListClient::new().with_base(&boom);
        assert!(c.find_anime("x").await.is_err());
    }

    fn short_read_client() -> reqwest::Client {
        reqwest::Client::builder()
            .read_timeout(std::time::Duration::from_millis(100))
            .build()
            .expect("client")
    }

    fn matched_anime() -> Value {
        json!({ "data": { "Media": {
            "id": 21, "title": { "english": "One Piece" }
        }}})
    }

    #[tokio::test]
    async fn one_anilist_timeout_is_retried_without_disabling_the_pass() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        let hits = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(tokio::sync::Notify::new());
        let base = serve(axum::Router::new().fallback({
            let hits = hits.clone();
            let release = release.clone();
            move || {
                let hits = hits.clone();
                let release = release.clone();
                async move {
                    if hits.fetch_add(1, Ordering::SeqCst) == 0 {
                        release.notified().await;
                    } else {
                        release.notify_waiters();
                    }
                    axum::Json(matched_anime())
                }
            }
        }))
        .await;
        let mut client = AniListClient::new().with_base(&base);
        client.http = short_read_client();
        assert!(client
            .find_anime("first")
            .await
            .expect("retry recovers")
            .is_some());
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        assert!(client.unreachable().is_none());
        assert!(client
            .find_anime("next")
            .await
            .expect("pass continues")
            .is_some());
        assert_eq!(hits.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn two_anilist_timeouts_defer_this_pass_but_the_next_pass_retries() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        let hits = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(tokio::sync::Notify::new());
        let base = serve(axum::Router::new().fallback({
            let hits = hits.clone();
            let release = release.clone();
            move || {
                let hits = hits.clone();
                let release = release.clone();
                async move {
                    if hits.fetch_add(1, Ordering::SeqCst) < 2 {
                        release.notified().await;
                    } else {
                        release.notify_waiters();
                    }
                    axum::Json(matched_anime())
                }
            }
        }))
        .await;
        let mut client = AniListClient::new().with_base(&base);
        client.http = short_read_client();
        assert!(matches!(
            client.find_anime("first").await,
            Err(MetadataError::Timeout(_))
        ));
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        assert!(client.unreachable().is_some());
        assert!(matches!(
            client.find_anime("deferred").await,
            Err(MetadataError::Unavailable)
        ));
        assert_eq!(hits.load(Ordering::SeqCst), 2, "no per-title outage replay");
        let mut next = AniListClient::new().with_base(&base);
        next.http = short_read_client();
        assert!(next
            .find_anime("next pass")
            .await
            .expect("fresh pass")
            .is_some());
        assert_eq!(hits.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn one_anilist_image_timeout_is_retried_without_disabling_matching() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        let hits = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(tokio::sync::Notify::new());
        let base = serve(
            axum::Router::new()
                .route(
                    "/",
                    axum::routing::post(|| async { axum::Json(matched_anime()) }),
                )
                .route(
                    "/image",
                    axum::routing::get({
                        let hits = hits.clone();
                        let release = release.clone();
                        move || {
                            let hits = hits.clone();
                            let release = release.clone();
                            async move {
                                if hits.fetch_add(1, Ordering::SeqCst) == 0 {
                                    release.notified().await;
                                } else {
                                    release.notify_waiters();
                                }
                                vec![9_u8, 8, 7]
                            }
                        }
                    }),
                ),
        )
        .await;
        let mut client = AniListClient::new().with_base(&base);
        client.http = short_read_client();
        assert_eq!(
            client
                .download_image(&format!("{base}/image"))
                .await
                .expect("image retry"),
            vec![9, 8, 7]
        );
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        assert!(client.images_unreachable().is_none());
        assert!(client
            .find_anime("matching still works")
            .await
            .expect("match")
            .is_some());
    }

    #[tokio::test]
    async fn anilist_http_and_parse_failures_never_retry_or_trip_host_health() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        for status in [404_u16, 429, 500, 200] {
            let hits = Arc::new(AtomicUsize::new(0));
            let base = serve(axum::Router::new().fallback({
                let hits = hits.clone();
                move || {
                    let hits = hits.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        (
                            axum::http::StatusCode::from_u16(status).expect("status"),
                            "not JSON",
                        )
                    }
                }
            }))
            .await;
            let client = AniListClient::new().with_base(&base);
            let result = client.find_anime("first").await;
            match status {
                404 => assert!(result.expect("not found").is_none()),
                200 => assert!(matches!(result, Err(MetadataError::Parse(_)))),
                _ => assert!(matches!(result, Err(MetadataError::Status(s)) if s == status)),
            }
            assert_eq!(
                hits.load(Ordering::SeqCst),
                1,
                "HTTP {status} is not retried"
            );
            assert!(client.unreachable().is_none());
        }
    }

    #[tokio::test]
    async fn an_anilist_retry_reacquires_authority_before_sending() {
        use crate::store::background_jobs_provider::{ProviderBudgetAction, ProviderBudgetOutcome};
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };

        struct LostOnRetry(AtomicUsize);
        #[async_trait::async_trait]
        impl super::super::budget::ProviderBudget for LostOnRetry {
            async fn update(
                &self,
                _: Provider,
                action: ProviderBudgetAction,
            ) -> Result<ProviderBudgetOutcome, crate::error::StoreError> {
                assert!(matches!(action, ProviderBudgetAction::Charge));
                Ok(if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                    ProviderBudgetOutcome::Charged
                } else {
                    ProviderBudgetOutcome::LostAuthority
                })
            }
        }
        let hits = Arc::new(AtomicUsize::new(0));
        let base = serve(axum::Router::new().fallback({
            let hits = hits.clone();
            move || {
                let hits = hits.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    std::future::pending::<axum::Json<Value>>().await
                }
            }
        }))
        .await;
        let budget = Arc::new(LostOnRetry(AtomicUsize::new(0)));
        let mut client = AniListClient::new()
            .with_base(&base)
            .with_budget(Some(budget.clone()));
        client.http = short_read_client();
        let error = client
            .find_anime("first")
            .await
            .expect_err("authority lost before retry");
        assert!(
            error.to_string().contains("provider ownership lost"),
            "{error}"
        );
        assert_eq!(
            budget.0.load(Ordering::SeqCst),
            2,
            "each attempt charges authority"
        );
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "refused retry never reaches the wire"
        );
        assert!(
            client.unreachable().is_none(),
            "authority loss is not a dead host"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_anilist_retry_cooldown_cannot_restart_the_call_budget() {
        use crate::store::background_jobs_provider::{ProviderBudgetAction, ProviderBudgetOutcome};
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };

        struct CooldownAfterFirst(AtomicUsize);
        #[async_trait::async_trait]
        impl super::super::budget::ProviderBudget for CooldownAfterFirst {
            async fn update(
                &self,
                _: Provider,
                action: ProviderBudgetAction,
            ) -> Result<ProviderBudgetOutcome, crate::error::StoreError> {
                assert!(matches!(action, ProviderBudgetAction::Charge));
                if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                    // Spend part of the call before its first transport error.
                    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                    Ok(ProviderBudgetOutcome::Charged)
                } else {
                    Ok(ProviderBudgetOutcome::Wait {
                        until_ms: crate::cluster::coordination::unix_ms().expect("clock") + 61_000,
                    })
                }
            }
        }
        let budget = Arc::new(CooldownAfterFirst(AtomicUsize::new(0)));
        let client = AniListClient::new().with_budget(Some(budget.clone()));
        let builds = AtomicUsize::new(0);
        let started = tokio::time::Instant::now();
        let error = client
            .send_with_transport_retry(&client.api_health, || {
                builds.fetch_add(1, Ordering::SeqCst);
                client.http.get("http://127.0.0.1:1")
            })
            .await
            .expect_err("retry allowance exceeds the original call deadline");
        assert_eq!(started.elapsed(), PROVIDER_CALL_BUDGET);
        assert!(
            error
                .to_string()
                .contains("provider call retry budget exhausted"),
            "{error}"
        );
        assert!(
            budget.0.load(Ordering::SeqCst) >= 2,
            "retry tried to reacquire"
        );
        assert_eq!(
            builds.load(Ordering::SeqCst),
            1,
            "no request after the retry cooldown"
        );
        assert!(
            client.unreachable().is_none(),
            "cooldown is not a dead host"
        );
    }
}
