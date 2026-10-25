//! OMDb: the one source plurx has for IMDb, Rotten Tomatoes and Metacritic
//! scores (TMDB carries none of them). Keyed by IMDb id only — a title search
//! is the thing that matches the wrong film.

use serde_json::Value;

use super::{PROVIDER_CONNECT_TIMEOUT, PROVIDER_READ_TIMEOUT};
use crate::annotations::{Rating, RatingSource};
use crate::error::MetadataError;

pub const API_BASE: &str = "https://www.omdbapi.com/";

/// A small JSON answer; anything larger is not an OMDb title record.
const BODY_MAX_BYTES: usize = 256 * 1024;

pub struct OmdbClient {
    api_key: String,
    http: reqwest::Client,
    base: String,
}

impl OmdbClient {
    #[must_use]
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            http: reqwest::Client::builder()
                .user_agent(concat!("plurx/", env!("CARGO_PKG_VERSION")))
                .connect_timeout(PROVIDER_CONNECT_TIMEOUT)
                .read_timeout(PROVIDER_READ_TIMEOUT)
                .timeout(PROVIDER_READ_TIMEOUT)
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            base: API_BASE.to_owned(),
        }
    }

    /// Point at another host (a mock server in tests).
    #[must_use]
    pub fn with_base(mut self, base: impl Into<String>) -> Self {
        self.base = base.into();
        self
    }

    /// The scores OMDb has for one IMDb id. A title OMDb does not know is an
    /// empty answer; a refused key or an exhausted daily allowance is an error,
    /// so the caller does not record OMDb as having been asked.
    pub async fn scores(&self, imdb_id: &str) -> Result<Vec<Rating>, MetadataError> {
        let response = self
            .http
            .get(&self.base)
            .query(&[("i", imdb_id), ("apikey", self.api_key.as_str())])
            .send()
            .await
            // The request URL carries the API key: strip it from every error
            // before it can be formatted into a log line.
            .map_err(|error| {
                let timeout = error.is_timeout();
                let error = error.without_url();
                if timeout {
                    MetadataError::Timeout(format!("OMDb: {error}"))
                } else {
                    MetadataError::Http(format!("OMDb: {error}"))
                }
            })?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|error| MetadataError::Http(format!("OMDb body: {}", error.without_url())))?;
        if bytes.len() > BODY_MAX_BYTES {
            return Err(MetadataError::BodyBound(BODY_MAX_BYTES as u64));
        }
        let body: Value = serde_json::from_slice(&bytes)
            .map_err(|error| MetadataError::Parse(format!("OMDb: {error}")))?;
        // OMDb answers a bad key with 401 and a JSON error. Any non-success
        // status is a failure whatever the body says: an empty proxy answer
        // must never replace stored scores with nothing.
        if !status.is_success() {
            return Err(MetadataError::Status(status.as_u16()));
        }
        parse_scores(&body)
    }
}

/// Scores from one OMDb title record. `"N/A"` means the source has none.
pub(crate) fn parse_scores(body: &Value) -> Result<Vec<Rating>, MetadataError> {
    match body.get("Response").and_then(Value::as_str) {
        Some("True" | "False") => {}
        _ => {
            return Err(MetadataError::Parse(
                "OMDb answered without its Response envelope".into(),
            ))
        }
    }
    if body.get("Response").and_then(Value::as_str) == Some("False") {
        let error = body
            .get("Error")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        // "Incorrect IMDb ID." / "Movie not found!" / "Series not found!":
        // a real answer that there is nothing to show.
        let lowered = error.to_ascii_lowercase();
        if lowered.contains("not found") || lowered.contains("incorrect imdb id") {
            return Ok(Vec::new());
        }
        return Err(MetadataError::Parse(format!("OMDb refused: {error}")));
    }
    let text = |key: &str| {
        body.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty() && *value != "N/A")
    };
    let mut ratings = Vec::new();
    if let Some(value) = text("imdbRating").and_then(|v| v.parse::<f64>().ok()) {
        ratings.push(Rating {
            source: RatingSource::Imdb,
            value,
            scale: 10,
            votes: text("imdbVotes").and_then(|v| v.replace(',', "").parse().ok()),
        });
    }
    let rotten = body
        .get("Ratings")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|rating| rating.get("Source").and_then(Value::as_str) == Some("Rotten Tomatoes"))
        .and_then(|rating| rating.get("Value").and_then(Value::as_str))
        .and_then(|value| value.trim().trim_end_matches('%').parse::<f64>().ok());
    if let Some(value) = rotten {
        ratings.push(Rating {
            source: RatingSource::RottenTomatoes,
            value,
            scale: 100,
            votes: None,
        });
    }
    if let Some(value) = text("Metascore").and_then(|v| v.parse::<f64>().ok()) {
        ratings.push(Rating {
            source: RatingSource::Metacritic,
            value,
            scale: 100,
            votes: None,
        });
    }
    ratings.retain(|rating| rating.value.is_finite() && rating.value >= 0.0);
    Ok(ratings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_title_record_gives_imdb_rotten_tomatoes_and_metacritic() {
        let body = json!({
            "Response": "True",
            "imdbRating": "8.1",
            "imdbVotes": "214,000",
            "Metascore": "81",
            "Ratings": [
                {"Source": "Internet Movie Database", "Value": "8.1/10"},
                {"Source": "Rotten Tomatoes", "Value": "94%"},
                {"Source": "Metacritic", "Value": "81/100"}
            ]
        });
        let ratings = parse_scores(&body).expect("scores");
        assert_eq!(
            ratings,
            vec![
                Rating {
                    source: RatingSource::Imdb,
                    value: 8.1,
                    scale: 10,
                    votes: Some(214_000)
                },
                Rating {
                    source: RatingSource::RottenTomatoes,
                    value: 94.0,
                    scale: 100,
                    votes: None
                },
                Rating {
                    source: RatingSource::Metacritic,
                    value: 81.0,
                    scale: 100,
                    votes: None
                },
            ]
        );
    }

    #[test]
    fn a_body_without_the_omdb_envelope_is_a_failure_not_an_empty_answer() {
        assert!(parse_scores(&json!({})).is_err());
        assert!(parse_scores(&json!({"imdbRating": "8.1"})).is_err());
    }

    #[tokio::test]
    async fn a_failed_request_never_carries_the_key_and_a_bad_status_is_an_error() {
        let client = OmdbClient::new("secret-key-123").with_base("http://127.0.0.1:9/");
        let error = client
            .scores("tt1")
            .await
            .expect_err("nothing listens on port 9")
            .to_string();
        assert!(!error.contains("secret-key-123"), "{error}");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let app = axum::Router::new().route(
                "/",
                axum::routing::get(|| async {
                    (
                        axum::http::StatusCode::SERVICE_UNAVAILABLE,
                        axum::Json(json!({})),
                    )
                }),
            );
            let _ = axum::serve(listener, app).await;
        });
        let client = OmdbClient::new("secret-key-123").with_base(format!("http://{addr}/"));
        assert!(matches!(
            client.scores("tt1").await,
            Err(MetadataError::Status(503))
        ));
    }

    #[test]
    fn not_applicable_scores_are_left_out() {
        let body = json!({"Response": "True", "imdbRating": "N/A", "imdbVotes": "N/A", "Metascore": "N/A", "Ratings": []});
        assert!(parse_scores(&body).expect("scores").is_empty());
    }

    #[test]
    fn an_unknown_title_is_an_empty_answer_but_a_refusal_is_an_error() {
        assert!(
            parse_scores(&json!({"Response": "False", "Error": "Movie not found!"}))
                .expect("not found is an answer")
                .is_empty()
        );
        assert!(
            parse_scores(&json!({"Response": "False", "Error": "Request limit reached!"})).is_err()
        );
        assert!(parse_scores(&json!({"Response": "False", "Error": "Invalid API key!"})).is_err());
    }
}
