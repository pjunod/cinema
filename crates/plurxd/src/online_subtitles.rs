//! OpenSubtitles.com REST adapter. Credentials never reach download hosts.

use reqwest::{Client, StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;

const API: &str = "https://api.opensubtitles.com/api/v1";
const RESPONSE_LIMIT: usize = 1024 * 1024;

pub struct Provider {
    api: String,
    client: Client,
    key: String,
    username: String,
    password: String,
}

#[derive(Debug)]
pub struct Error {
    pub code: &'static str,
    pub message: &'static str,
    pub retry_after: u64,
}

fn unavailable() -> Error {
    Error {
        code: "subtitle_provider_unavailable",
        message: "OpenSubtitles is temporarily unavailable. Try again later.",
        retry_after: 60,
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Candidate {
    pub file_id: i64,
    pub language: String,
    pub release: String,
    pub hearing_impaired: bool,
    pub forced: bool,
    pub hash_match: bool,
    pub machine_translated: bool,
    pub ai_translated: bool,
    pub downloads: u64,
}

#[derive(Default)]
pub struct Search {
    pub language: String,
    pub imdb_id: Option<String>,
    pub tmdb_id: Option<i64>,
    pub query: Option<String>,
    pub moviehash: Option<String>,
}

impl Provider {
    pub fn new(key: String, username: String, password: String) -> Result<Self, Error> {
        if key.trim().is_empty() {
            return Err(Error { code: "subtitle_provider_disabled", message: "An administrator must configure OpenSubtitles in Settings before subtitles can be downloaded.", retry_after: 0 });
        }
        reqwest::header::HeaderValue::from_str(&key).map_err(|_| Error {
            code: "subtitle_provider_credentials",
            message: "The OpenSubtitles API key is invalid.",
            retry_after: 0,
        })?;
        let client = Client::builder()
            .user_agent(concat!("Plurx v", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| unavailable())?;
        Ok(Self {
            api: API.into(),
            client,
            key,
            username,
            password,
        })
    }

    async fn json(&self, request: reqwest::RequestBuilder) -> Result<Value, Error> {
        let response = request
            .header("Api-Key", &self.key)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|_| unavailable())?;
        check_status(&response)?;
        let bytes = bounded(response, RESPONSE_LIMIT).await?;
        serde_json::from_slice(&bytes).map_err(|_| unavailable())
    }

    pub async fn search(&self, search: &Search) -> Result<Vec<Candidate>, Error> {
        let mut query = std::collections::BTreeMap::new();
        query.insert("languages", search.language.clone());
        if let Some(id) = &search.imdb_id {
            query.insert("imdb_id", id.trim_start_matches("tt").to_owned());
        } else if let Some(id) = search.tmdb_id {
            query.insert("tmdb_id", id.to_string());
        } else if let Some(value) = &search.query {
            query.insert("query", value.clone());
        }
        if let Some(hash) = &search.moviehash {
            query.insert("moviehash", hash.clone());
        }
        query.insert("order_by", "download_count".into());
        let data = self
            .json(
                self.client
                    .get(format!("{}/subtitles", self.api))
                    .query(&query),
            )
            .await?;
        parse_candidates(&data)
    }

    pub async fn download(&self, file_id: i64) -> Result<String, Error> {
        let mut request = self
            .client
            .post(format!("{}/download", self.api))
            .json(&json!({"file_id":file_id,"sub_format":"webvtt"}));
        if !self.username.is_empty() && !self.password.is_empty() {
            let login = self
                .json(
                    self.client
                        .post(format!("{}/login", self.api))
                        .json(&json!({"username":self.username,"password":self.password})),
                )
                .await?;
            let token = login
                .get("token")
                .and_then(Value::as_str)
                .ok_or_else(unavailable)?;
            let base = match login.get("base_url").and_then(Value::as_str) {
                Some("vip-api.opensubtitles.com") => "https://vip-api.opensubtitles.com/api/v1",
                Some("api.opensubtitles.com") => API,
                None => &self.api,
                _ => return Err(unavailable()),
            };
            request = self
                .client
                .post(format!("{base}/download"))
                .json(&json!({"file_id":file_id,"sub_format":"webvtt"}))
                .bearer_auth(token);
        }
        let download = self.json(request).await?;
        let link = download
            .get("link")
            .and_then(Value::as_str)
            .ok_or_else(unavailable)?;
        let mut url = Url::parse(link).map_err(|_| unavailable())?;
        // Each hop is checked and uses a fresh request without the API key,
        // login body or bearer token. A provider response cannot target a LAN URL.
        for _ in 0..4 {
            if !allowed_download_url(&url) {
                return Err(unavailable());
            }
            let response = self
                .client
                .get(url.clone())
                .send()
                .await
                .map_err(|_| unavailable())?;
            if response.status().is_redirection() {
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|h| h.to_str().ok())
                    .ok_or_else(unavailable)?;
                url = url.join(location).map_err(|_| unavailable())?;
                continue;
            }
            check_status(&response)?;
            let bytes = bounded(response, plurx_core::store::MAX_DOWNLOADED_SUBTITLE_BYTES).await?;
            return normalize_vtt(bytes);
        }
        Err(unavailable())
    }
}

fn check_status(response: &reqwest::Response) -> Result<(), Error> {
    match response.status() {
        status if status.is_success() => Ok(()),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(Error {
            code: "subtitle_provider_credentials", message: "OpenSubtitles rejected the configured API key or account.", retry_after: 0,
        }),
        StatusCode::TOO_MANY_REQUESTS | StatusCode::NOT_ACCEPTABLE => Err(Error {
            code: "subtitle_provider_quota", message: "The OpenSubtitles request or download allowance has been reached. Try again after it resets.",
            retry_after: response.headers().get(reqwest::header::RETRY_AFTER)
                .and_then(|h| h.to_str().ok()).and_then(|h| h.parse().ok()).unwrap_or(3600).clamp(1,86400),
        }),
        _ => Err(unavailable()),
    }
}

async fn bounded(mut response: reqwest::Response, max: usize) -> Result<Vec<u8>, Error> {
    if response.content_length().is_some_and(|n| n > max as u64) {
        return Err(unavailable());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
        if chunk.len() > max.saturating_sub(bytes.len()) {
            return Err(unavailable());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn allowed_download_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && matches!(
            url.host_str(),
            Some("www.opensubtitles.com" | "dl.opensubtitles.com" | "api.opensubtitles.com")
        )
}

fn parse_candidates(data: &Value) -> Result<Vec<Candidate>, Error> {
    let rows = data
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(unavailable)?;
    let mut candidates = Vec::new();
    for row in rows.iter().take(60) {
        let a = &row["attributes"];
        let Some(language) = a["language"].as_str().filter(|s| valid_language(s)) else {
            continue;
        };
        let Some(files) = a["files"].as_array() else {
            continue;
        };
        // Multi-CD subtitles require joining parts; a single partial file
        // must not be offered as captions for the whole movie.
        if files.len() != 1 {
            continue;
        }
        let Some(file_id) = files[0]["file_id"].as_i64().filter(|id| *id > 0) else {
            continue;
        };
        let release = a["release"]
            .as_str()
            .or_else(|| files[0]["file_name"].as_str())
            .unwrap_or("OpenSubtitles");
        candidates.push(Candidate {
            file_id,
            language: language.into(),
            release: {
                let mut text = release.to_owned();
                while text.len() > 200 {
                    text.pop();
                }
                text
            },
            hearing_impaired: a["hearing_impaired"].as_bool().unwrap_or(false),
            forced: a["foreign_parts_only"].as_bool().unwrap_or(false),
            hash_match: a["moviehash_match"].as_bool().unwrap_or(false),
            ai_translated: a["ai_translated"].as_bool().unwrap_or(false),
            machine_translated: a["machine_translated"].as_bool().unwrap_or(false),
            downloads: a["download_count"].as_u64().unwrap_or(0),
        });
    }
    candidates.sort_by_key(|c| {
        (
            std::cmp::Reverse(c.hash_match),
            std::cmp::Reverse(c.downloads),
        )
    });
    Ok(candidates)
}

pub fn valid_language(value: &str) -> bool {
    (2..=12).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_alphabetic() || b == b'-')
}

fn normalize_vtt(bytes: Vec<u8>) -> Result<String, Error> {
    let raw = String::from_utf8(bytes).map_err(|_| unavailable())?;
    let vtt = raw
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    if !plurx_core::store::valid_downloaded_vtt(&vtt) {
        return Err(Error {
            code: "subtitle_invalid",
            message: "The downloaded file is not a usable WebVTT subtitle.",
            retry_after: 0,
        });
    }
    Ok(vtt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn provider_http_fixture_checks_search_auth_and_quota_response() {
        use axum::{
            routing::{get, post},
            Json, Router,
        };
        let app=Router::new().route("/subtitles",get(|headers:axum::http::HeaderMap|async move{
            assert_eq!(headers.get("Api-Key").expect("provider fixture"),"fixture-key");
            assert_eq!(headers.get("Accept").expect("provider fixture"),"application/json");
            Json(json!({"data":[{"attributes":{"language":"en","files":[{"file_id":42}],"moviehash_match":true}}]}))
        })).route("/download",post(||async{(StatusCode::NOT_ACCEPTABLE,[("Retry-After","7200")],"quota")}));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("provider fixture");
        let address = listener.local_addr().expect("provider fixture");
        let server =
            tokio::spawn(
                async move { axum::serve(listener, app).await.expect("provider fixture") },
            );
        let mut provider = Provider::new("fixture-key".into(), String::new(), String::new())
            .expect("provider fixture");
        provider.api = format!("http://{address}");
        let found = provider
            .search(&Search {
                language: "en".into(),
                imdb_id: Some("tt1234".into()),
                ..Default::default()
            })
            .await
            .expect("provider fixture");
        assert_eq!(found[0].file_id, 42);
        let error = provider
            .download(42)
            .await
            .expect_err("provider must reject request");
        assert_eq!(error.code, "subtitle_provider_quota");
        assert_eq!(error.retry_after, 7200);
        server.abort();
    }

    #[tokio::test]
    async fn provider_download_refuses_redirect_targets_outside_its_hosts() {
        let app = axum::Router::new().route(
            "/download",
            axum::routing::post(|| async {
                axum::Json(json!({"link":"http://127.0.0.1/private"}))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("provider fixture");
        let address = listener.local_addr().expect("provider fixture");
        let server =
            tokio::spawn(
                async move { axum::serve(listener, app).await.expect("provider fixture") },
            );
        let mut provider = Provider::new("fixture-key".into(), String::new(), String::new())
            .expect("provider fixture");
        provider.api = format!("http://{address}");
        assert!(provider.download(42).await.is_err());
        server.abort();
    }

    #[test]
    fn download_destinations_are_restricted_at_every_hop() {
        for url in [
            "http://dl.opensubtitles.com/x",
            "https://localhost/x",
            "https://dl.opensubtitles.com.evil.test/x",
            "https://user@dl.opensubtitles.com/x",
            "https://dl.opensubtitles.com:444/x",
        ] {
            assert!(
                !allowed_download_url(&Url::parse(url).expect("provider fixture")),
                "{url}"
            );
        }
        assert!(allowed_download_url(
            &Url::parse("https://dl.opensubtitles.com/subtitle.webvtt").expect("provider fixture")
        ));
    }

    #[test]
    fn download_uses_file_id_and_refuses_partial_cd_results() {
        let value = json!({"data":[{"id":"111","attributes":{"language":"en","release":"Movie","moviehash_match":true,"files":[{"file_id":222}] }}, {"attributes":{"language":"en","files":[{"file_id":333},{"file_id":444}]}}]});
        let candidates = parse_candidates(&value).expect("provider fixture");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].file_id, 222);
        assert!(candidates[0].hash_match);
    }

    #[test]
    fn normalize_accepts_bom_and_crlf_but_rejects_html() {
        assert!(normalize_vtt(b"<html>error</html>".to_vec()).is_err());
        assert!(normalize_vtt(b"WEBVTT\n\ninvalid --> nonsense\nBad\n".to_vec()).is_err());
        assert!(
            normalize_vtt(b"WEBVTT\n\n00:00:04.000 --> 00:00:01.000\nBackwards\n".to_vec())
                .is_err()
        );
        assert_eq!(
            normalize_vtt(
                "\u{feff}WEBVTT\r\n\r\n00:00:01.000 --> 00:00:02.000\r\nHello\r\n"
                    .as_bytes()
                    .to_vec()
            )
            .expect("provider fixture"),
            "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nHello\n"
        );
    }
}
