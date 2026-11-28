//! HLS requests and response bytes by surface, resource and rendition role.
//!
//! `plurx_http_requests_total` counts every HLS object as one `playback`
//! route group, which cannot say how many playlist, init and segment GETs a
//! presentation costs, how much of it a client revalidated (304) or resumed
//! by range (206), or how the bytes divide between a muxed copy and the
//! video and audio renditions of a split one. Those are the request-count and
//! bandwidth baselines a split default has to be measured against
//! (`docs/streaming/SPLIT-HLS-DEFAULT-INVENTORY.md` §5), so this family answers
//! them from the same matched route template the request metrics use.
//!
//! Every label comes from a fixed table: the template picks the surface and
//! resource, the role is the `{role}` path value only when it is one of the
//! two roles the child routes accept, and init versus segment is decided by
//! the object name against the server's own init naming. No session id,
//! rendition key or object name can mint a series.

use std::sync::atomic::{AtomicU64, Ordering};

use axum::http::StatusCode;

/// One counted HLS object class: `(surface, resource, role)`.
///
/// Only the combinations a registered route can produce are listed, so the
/// exposition is a fixed, small set rather than a cross product.
pub(super) const HLS_OBJECTS: [(&str, &str, &str); 26] = [
    ("session", "master", "none"),
    ("session", "media_playlist", "muxed"),
    ("session", "init", "muxed"),
    ("session", "segment", "muxed"),
    ("session", "media_playlist", "video"),
    ("session", "init", "video"),
    ("session", "segment", "video"),
    ("session", "media_playlist", "audio"),
    ("session", "init", "audio"),
    ("session", "segment", "audio"),
    ("session", "media_playlist", "subtitle"),
    ("session", "segment", "subtitle"),
    ("live_tv", "master", "none"),
    ("live_tv", "media_playlist", "muxed"),
    ("live_tv", "init", "muxed"),
    ("live_tv", "segment", "muxed"),
    ("live_tv", "media_playlist", "video"),
    ("live_tv", "init", "video"),
    ("live_tv", "segment", "video"),
    ("live_tv", "media_playlist", "audio"),
    ("live_tv", "init", "audio"),
    ("live_tv", "segment", "audio"),
    ("offline", "master", "none"),
    ("offline", "media_playlist", "muxed"),
    ("offline", "segment", "muxed"),
    ("offline", "segment", "subtitle"),
];

/// Response statuses that matter to object reuse: a full body, a ranged
/// body, a revalidation without one, and the two failure classes. Anything
/// else (1xx, 3xx other than 304) is folded into `other`.
pub(super) const HLS_STATUSES: [&str; 6] = ["200", "206", "304", "4xx", "5xx", "other"];

fn object_index(surface: &str, resource: &str, role: &str) -> Option<usize> {
    HLS_OBJECTS
        .iter()
        .position(|object| *object == (surface, resource, role))
}

fn status_index(status: StatusCode) -> usize {
    match status.as_u16() {
        200 => 0,
        206 => 1,
        304 => 2,
        400..=499 => 3,
        500..=599 => 4,
        _ => 5,
    }
}

/// The last path component of the request, which for every classified
/// template is the object name the handler serves.
fn last_component(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or_default()
}

fn init_or_segment(object: &str) -> &'static str {
    if crate::transcode::is_init_object(object) {
        "init"
    } else {
        "segment"
    }
}

/// The child routes' `{role}` value, read from the request path. The template
/// `/…/{session}/{role}/{rendition}/…` puts it two components after the
/// session; it is a label only when the child route would accept it.
fn child_role(path: &str, components_after_role: usize) -> Option<&'static str> {
    let role = path.rsplit('/').nth(components_after_role + 1)?;
    match role {
        "video" => Some("video"),
        "audio" => Some("audio"),
        _ => None,
    }
}

/// Classify one request by its method, matched route template and request
/// path. `None` for every route that is not an HLS object, and for anything
/// but GET: axum answers HEAD on the same routes, and a probe that moves no
/// object is not a fetch.
pub(super) fn classify(method: &axum::http::Method, template: &str, path: &str) -> Option<usize> {
    if method != axum::http::Method::GET {
        return None;
    }
    let template = template.strip_prefix("/api/v1").unwrap_or(template);
    let (surface, resource, role) = match template {
        "/hls/{session}/master.m3u8" => ("session", "master", "none"),
        "/hls/{session}/index.m3u8" | "/hls/{session}/video.m3u8" => {
            ("session", "media_playlist", "muxed")
        }
        "/hls/{session}/subs/{index}/index.m3u8" => ("session", "media_playlist", "subtitle"),
        "/hls/{session}/subs/{index}/{segment}" => ("session", "segment", "subtitle"),
        "/hls/{session}/{segment}" => ("session", init_or_segment(last_component(path)), "muxed"),
        "/hls/{session}/{role}/{rendition}/index.m3u8" => {
            ("session", "media_playlist", child_role(path, 1)?)
        }
        "/hls/{session}/{role}/{rendition}/{kind}/{object}" => {
            let kind = path.rsplit('/').nth(1)?;
            let resource = match kind {
                "init" => "init",
                "segment" => "segment",
                _ => return None,
            };
            ("session", resource, child_role(path, 2)?)
        }
        "/live-tv/sessions/{capability}/master.m3u8" => ("live_tv", "master", "none"),
        "/live-tv/sessions/{capability}/{role}/{rendition}/index.m3u8" => {
            ("live_tv", "media_playlist", child_role(path, 1)?)
        }
        "/live-tv/sessions/{capability}/{role}/{rendition}/{kind}/{object}" => {
            let kind = if path.rsplit('/').nth(1) == Some("init") {
                "init"
            } else {
                "segment"
            };
            ("live_tv", kind, child_role(path, 2)?)
        }
        "/live-tv/sessions/{capability}/index.m3u8" => ("live_tv", "media_playlist", "muxed"),
        "/live-tv/sessions/{capability}/{segment}" => {
            ("live_tv", init_or_segment(last_component(path)), "muxed")
        }
        "/offline/media/{token}/master.m3u8" => ("offline", "master", "none"),
        "/offline/media/{token}/index.m3u8" => ("offline", "media_playlist", "muxed"),
        "/offline/media/{token}/subs/{index}/{segment}" => ("offline", "segment", "subtitle"),
        "/offline/media/{token}/{segment}" => ("offline", "segment", "muxed"),
        _ => return None,
    };
    object_index(surface, resource, role)
}

pub(super) struct HlsRequestMetrics {
    requests: [AtomicU64; HLS_OBJECTS.len() * HLS_STATUSES.len()],
    bytes: [AtomicU64; HLS_OBJECTS.len()],
}

impl HlsRequestMetrics {
    pub(super) const fn new() -> Self {
        Self {
            requests: [const { AtomicU64::new(0) }; HLS_OBJECTS.len() * HLS_STATUSES.len()],
            bytes: [const { AtomicU64::new(0) }; HLS_OBJECTS.len()],
        }
    }

    pub(super) fn record_request(&self, object: usize, status: StatusCode) {
        if let Some(cell) = self
            .requests
            .get(object * HLS_STATUSES.len() + status_index(status))
        {
            super::add_saturating(cell, 1);
        }
    }

    /// Body bytes handed to the connection for one response, counted when
    /// the body ends or is dropped, so an aborted transfer counts what it
    /// actually sent.
    pub(super) fn record_bytes(&self, object: usize, bytes: u64) {
        if let Some(cell) = self.bytes.get(object) {
            super::add_saturating(cell, bytes);
        }
    }

    pub(super) fn render(&self) -> String {
        use std::fmt::Write;

        let mut out = String::from(
            "# HELP plurx_hls_requests_total HLS object requests by surface, resource, rendition role and response status.\n\
             # TYPE plurx_hls_requests_total counter\n",
        );
        for (object_index, (surface, resource, role)) in HLS_OBJECTS.iter().enumerate() {
            for (status_index, status) in HLS_STATUSES.iter().enumerate() {
                let count = self.requests[object_index * HLS_STATUSES.len() + status_index]
                    .load(Ordering::Relaxed);
                let _ = writeln!(
                    out,
                    "plurx_hls_requests_total{{surface=\"{surface}\",resource=\"{resource}\",role=\"{role}\",status=\"{status}\"}} {count}"
                );
            }
        }
        out.push_str(
            "# HELP plurx_hls_response_bytes_total HLS response body bytes handed to connections, by surface, resource and rendition role.\n\
             # TYPE plurx_hls_response_bytes_total counter\n",
        );
        for (object_index, (surface, resource, role)) in HLS_OBJECTS.iter().enumerate() {
            let bytes = self.bytes[object_index].load(Ordering::Relaxed);
            let _ = writeln!(
                out,
                "plurx_hls_response_bytes_total{{surface=\"{surface}\",resource=\"{resource}\",role=\"{role}\"}} {bytes}"
            );
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(template: &str, path: &str) -> Option<(&'static str, &'static str, &'static str)> {
        classify(&axum::http::Method::GET, template, path).map(|index| HLS_OBJECTS[index])
    }

    #[test]
    fn hls_request_classification_names_role_and_object_without_path_values() {
        assert_eq!(
            label(
                "/api/v1/hls/{session}/master.m3u8",
                "/api/v1/hls/s1/master.m3u8"
            ),
            Some(("session", "master", "none"))
        );
        assert_eq!(
            label("/api/v1/hls/{session}/{segment}", "/api/v1/hls/s1/init.mp4"),
            Some(("session", "init", "muxed"))
        );
        assert_eq!(
            label(
                "/api/v1/hls/{session}/{segment}",
                "/api/v1/hls/s1/init-e3.mp4"
            ),
            Some(("session", "init", "muxed"))
        );
        assert_eq!(
            label(
                "/api/v1/hls/{session}/{segment}",
                "/api/v1/hls/s1/seg00007.m4s"
            ),
            Some(("session", "segment", "muxed"))
        );
        assert_eq!(
            label(
                "/api/v1/hls/{session}/{role}/{rendition}/index.m3u8",
                "/api/v1/hls/s1/audio/abc/index.m3u8"
            ),
            Some(("session", "media_playlist", "audio"))
        );
        assert_eq!(
            label(
                "/api/v1/hls/{session}/{role}/{rendition}/{kind}/{object}",
                "/api/v1/hls/s1/video/abc/init/0123.mp4"
            ),
            Some(("session", "init", "video"))
        );
        assert_eq!(
            label(
                "/api/v1/hls/{session}/{role}/{rendition}/{kind}/{object}",
                "/api/v1/hls/s1/audio/abc/segment/00004.m4s"
            ),
            Some(("session", "segment", "audio"))
        );
        assert_eq!(
            label(
                "/api/v1/live-tv/sessions/{capability}/{segment}",
                "/api/v1/live-tv/sessions/c/seg00001.ts"
            ),
            Some(("live_tv", "segment", "muxed"))
        );
        assert_eq!(
            label(
                "/api/v1/offline/media/{token}/{segment}",
                "/api/v1/offline/media/t/seg1.ts"
            ),
            Some(("offline", "segment", "muxed"))
        );
    }

    #[test]
    fn hls_request_classification_refuses_unknown_roles_kinds_and_routes() {
        // A role or kind the child routes would reject mints no series.
        assert_eq!(
            label(
                "/api/v1/hls/{session}/{role}/{rendition}/{kind}/{object}",
                "/api/v1/hls/s1/subtitle/abc/segment/00004.m4s"
            ),
            None
        );
        assert_eq!(
            label(
                "/api/v1/hls/{session}/{role}/{rendition}/{kind}/{object}",
                "/api/v1/hls/s1/video/abc/sidx/00004.m4s"
            ),
            None
        );
        assert_eq!(
            label("/api/v1/hls/{session}/status", "/api/v1/hls/s1/status"),
            None
        );
        assert_eq!(label("/api/v1/items/{id}", "/api/v1/items/3"), None);
    }

    #[test]
    fn hls_request_metrics_count_statuses_and_bytes_per_object() {
        let metrics = HlsRequestMetrics::new();
        let audio_segment =
            object_index("session", "segment", "audio").expect("audio segments are listed");
        metrics.record_request(audio_segment, StatusCode::OK);
        metrics.record_request(audio_segment, StatusCode::PARTIAL_CONTENT);
        metrics.record_request(audio_segment, StatusCode::NOT_MODIFIED);
        metrics.record_request(audio_segment, StatusCode::NOT_MODIFIED);
        metrics.record_bytes(audio_segment, 1500);
        let text = metrics.render();
        assert!(text.contains(
            "plurx_hls_requests_total{surface=\"session\",resource=\"segment\",role=\"audio\",status=\"304\"} 2\n"
        ));
        assert!(text.contains(
            "plurx_hls_requests_total{surface=\"session\",resource=\"segment\",role=\"audio\",status=\"206\"} 1\n"
        ));
        assert!(text.contains(
            "plurx_hls_response_bytes_total{surface=\"session\",resource=\"segment\",role=\"audio\"} 1500\n"
        ));
        assert!(text.contains(
            "plurx_hls_requests_total{surface=\"session\",resource=\"segment\",role=\"muxed\",status=\"200\"} 0\n"
        ));
    }

    #[test]
    fn hls_request_classification_counts_get_only() {
        let template = "/api/v1/hls/{session}/{segment}";
        let path = "/api/v1/hls/s1/seg00007.m4s";
        assert!(classify(&axum::http::Method::GET, template, path).is_some());
        for method in [axum::http::Method::HEAD, axum::http::Method::OPTIONS] {
            assert_eq!(classify(&method, template, path), None, "{method}");
        }
    }

    #[test]
    fn hls_object_table_has_no_duplicate_labels() {
        for (index, object) in HLS_OBJECTS.iter().enumerate() {
            assert_eq!(
                HLS_OBJECTS.iter().position(|other| other == object),
                Some(index)
            );
        }
    }
}
