//! Facade request counters with finite labels (contract §8.3): the registered
//! route template and a closed outcome set. Never a user, device, item, play
//! or token; the query is never read.
use super::*;
use std::collections::BTreeMap;
use std::sync::{LazyLock, Mutex};

/// Outcome classes chosen so a generic client "cannot play" is diagnosable:
/// an unsupported route or profile, an authentication failure, a source or
/// state refusal, a capacity wait and an owner or server failure differ.
pub(super) const OUTCOMES: [&str; 11] = [
    "ok",
    "not_supported",
    "unauthorized",
    "forbidden",
    "not_found",
    "conflict",
    "gone",
    "refused",
    "throttled",
    "unavailable",
    "error",
];
/// The route label of a request that matched no facade route.
pub(super) const UNMATCHED: &str = "unmatched";

/// Set on a successful response whose body refuses the request (a
/// `PlaybackInfo` answering `ErrorCode: NotSupported`).
#[derive(Clone, Copy)]
pub(super) struct NotSupported;

/// Keys are axum's matched templates or `UNMATCHED`, so the map holds at most
/// one entry per registered route plus one.
static COUNTS: LazyLock<Mutex<BTreeMap<String, [u64; OUTCOMES.len()]>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

pub(super) fn outcome(response: &Response) -> &'static str {
    let status = response.status();
    if status.is_success() || status.is_redirection() {
        return if response.extensions().get::<NotSupported>().is_some() {
            "not_supported"
        } else {
            "ok"
        };
    }
    match status {
        StatusCode::UNAUTHORIZED => "unauthorized",
        StatusCode::FORBIDDEN => "forbidden",
        StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED => "not_found",
        StatusCode::CONFLICT => "conflict",
        StatusCode::GONE => "gone",
        StatusCode::TOO_MANY_REQUESTS => "throttled",
        StatusCode::SERVICE_UNAVAILABLE => "unavailable",
        status if status.is_client_error() => "refused",
        _ => "error",
    }
}

pub(super) fn record(route: &str, outcome: &str) {
    let Some(index) = OUTCOMES.iter().position(|o| *o == outcome) else {
        return;
    };
    let mut counts = COUNTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(cells) = counts.get_mut(route) {
        cells[index] += 1;
    } else {
        let mut cells = [0; OUTCOMES.len()];
        cells[index] = 1;
        counts.insert(route.to_owned(), cells);
    }
}

/// Counts every matched facade request by its template.
pub(super) async fn count_matched(
    request: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> Response {
    let route = request
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map_or_else(|| UNMATCHED.to_owned(), |m| m.as_str().to_owned());
    let response = next.run(request).await;
    record(&route, outcome(&response));
    response
}

pub(crate) fn prometheus() -> String {
    let counts = COUNTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut out = String::from(
        "# HELP plurx_jellyfin_requests_total Jellyfin compatibility requests by route template and outcome.\n\
         # TYPE plurx_jellyfin_requests_total counter\n",
    );
    for (route, cells) in counts.iter() {
        for (outcome, count) in OUTCOMES.iter().zip(cells) {
            if *count > 0 {
                out.push_str(&format!(
                    "plurx_jellyfin_requests_total{{route=\"{route}\",outcome=\"{outcome}\"}} {count}\n"
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn outcomes_are_a_closed_set_and_not_supported_is_distinct_from_ok() {
        let with = |status: StatusCode| {
            let mut response = Response::new(axum::body::Body::empty());
            *response.status_mut() = status;
            response
        };
        let mut refused = with(StatusCode::OK);
        refused.extensions_mut().insert(NotSupported);
        assert_eq!(outcome(&refused), "not_supported");
        for (status, expected) in [
            (StatusCode::OK, "ok"),
            (StatusCode::PARTIAL_CONTENT, "ok"),
            (StatusCode::NO_CONTENT, "ok"),
            (StatusCode::BAD_REQUEST, "refused"),
            (StatusCode::UNPROCESSABLE_ENTITY, "refused"),
            (StatusCode::UNAUTHORIZED, "unauthorized"),
            (StatusCode::FORBIDDEN, "forbidden"),
            (StatusCode::NOT_FOUND, "not_found"),
            (StatusCode::CONFLICT, "conflict"),
            (StatusCode::GONE, "gone"),
            (StatusCode::TOO_MANY_REQUESTS, "throttled"),
            (StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
            (StatusCode::INTERNAL_SERVER_ERROR, "error"),
        ] {
            let got = outcome(&with(status));
            assert_eq!(got, expected, "{status}");
            assert!(OUTCOMES.contains(&got));
        }
        record("/jellyfin/__metrics_test", "conflict");
        record("/jellyfin/__metrics_test", "conflict");
        record("/jellyfin/__metrics_test", "not-an-outcome");
        let text = prometheus();
        assert!(text.contains(
            "plurx_jellyfin_requests_total{route=\"/jellyfin/__metrics_test\",outcome=\"conflict\"} 2\n"
        ));
        assert!(!text.contains("not-an-outcome"));
    }
}
