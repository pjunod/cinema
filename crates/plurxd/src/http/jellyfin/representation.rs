//! Init-prefix representation over already admitted native bodies and native byte ranges.
use super::*;
use axum::body::Body;
use axum::http::header;
use futures_util::StreamExt;

fn length(response: &Response) -> Result<u64, ApiError> {
    if response.status() != StatusCode::OK {
        return Err(ApiError::ServiceUnavailable(
            "native media response unavailable".into(),
        ));
    }
    response
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .ok_or(ApiError::ServiceUnavailable(
            "native media length unavailable".into(),
        ))
}
type ByteRange = Option<(u64, u64)>;
struct Layout {
    init_len: u64,
    fragment_len: u64,
    start: u64,
    end: u64,
    partial: bool,
    etag: String,
}
enum Selection {
    Read(Layout),
    Complete(Response),
}
fn select(
    init: &Response,
    fragment: &Response,
    headers: &HeaderMap,
    session: &str,
) -> Result<Selection, ApiError> {
    let init_len = length(init)?;
    let fragment_len = length(fragment)?;
    let len = init_len
        .checked_add(fragment_len)
        .filter(|v| *v > 0)
        .ok_or(ApiError::ServiceUnavailable(
            "native media length overflow".into(),
        ))?;
    let tag = |r: &Response| {
        r.headers()
            .get(header::ETAG)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned()
    };
    let etag = format!(
        "\"{}\"",
        plurx_core::auth::hash_token(&format!(
            "plurx/jellyfin/inline-init/v1:{session}:{init_len}:{}:{fragment_len}:{}",
            tag(init),
            tag(fragment)
        ))
    );
    let response = |status, range: Option<String>| {
        let mut response = Response::builder()
            .status(status)
            .header(header::ETAG, &etag)
            .header(header::CACHE_CONTROL, "private, no-cache");
        if let Some(range) = range {
            response = response.header(header::CONTENT_RANGE, range);
        }
        response
            .body(Body::empty())
            .map_err(|_| ApiError::Internal("media response construction failed".into()))
    };
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(',')
                .map(str::trim)
                .any(|v| v == "*" || v.strip_prefix("W/").unwrap_or(v) == etag)
        })
    {
        return Ok(Selection::Complete(response(
            StatusCode::NOT_MODIFIED,
            None,
        )?));
    }
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .filter(|_| {
            headers
                .get(header::IF_RANGE)
                .is_none_or(|v| v.to_str().ok() == Some(etag.as_str()))
        });
    let range = match super::super::hls::requested_byte_range(range, len) {
        Ok(range) => range,
        Err(()) => {
            return Ok(Selection::Complete(response(
                StatusCode::RANGE_NOT_SATISFIABLE,
                Some(format!("bytes */{len}")),
            )?))
        }
    };
    let (start, end) = range.unwrap_or((0, len - 1));
    Ok(Selection::Read(Layout {
        init_len,
        fragment_len,
        start,
        end,
        partial: range.is_some(),
        etag,
    }))
}
impl Layout {
    fn native_ranges(&self) -> (ByteRange, ByteRange) {
        let init = (self.init_len > 0 && self.start < self.init_len)
            .then(|| (self.start, self.end.min(self.init_len - 1)));
        let fragment = (self.fragment_len > 0 && self.end >= self.init_len).then(|| {
            (
                self.start.saturating_sub(self.init_len),
                self.end - self.init_len,
            )
        });
        (init, fragment)
    }
    fn response(self, init: Body, fragment: Body) -> Result<Response, ApiError> {
        let mut response = Response::builder()
            .status(if self.partial {
                StatusCode::PARTIAL_CONTENT
            } else {
                StatusCode::OK
            })
            .header(header::CONTENT_TYPE, "video/mp2t")
            .header(header::CONTENT_LENGTH, self.end - self.start + 1)
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::ETAG, self.etag)
            .header(header::CACHE_CONTROL, "private, no-cache");
        if self.partial {
            response = response.header(
                header::CONTENT_RANGE,
                format!(
                    "bytes {}-{}/{}",
                    self.start,
                    self.end,
                    self.init_len + self.fragment_len
                ),
            );
        }
        response
            .body(Body::from_stream(
                init.into_data_stream().chain(fragment.into_data_stream()),
            ))
            .map_err(|_| ApiError::Internal("media response construction failed".into()))
    }
}

// Construct the native handler future outside the adapter's polling frame. The native
// handler also contains the rolling-session path; embedding that temporary in each
// inline-range branch needlessly multiplies its stack use in unoptimized builds.
#[inline(never)]
fn native_segment(
    state: &AppState,
    session: &str,
    name: &str,
    headers: HeaderMap,
) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<Response, ApiError>> + Send + use<>>>
{
    Box::pin(super::super::hls::segment(
        State(state.clone()),
        Path((session.to_owned(), name.to_owned())),
        headers,
    ))
}

/// Metadata bodies are dropped unpolled for a partial request. Byte ranges are then requested
/// from each native object, so native completion/frontier accounting observes exactly those
/// bytes instead of a full fragment which the adapter later slices. No object buffer, task,
/// timer or producer is added; native bodies retain all their original lifecycle owners.
pub(super) async fn inline_native_fragment(
    state: &AppState,
    session: &str,
    segment: &str,
    headers: &HeaderMap,
) -> Result<Response, ApiError> {
    let mut native_headers = headers.clone();
    for name in [header::RANGE, header::IF_RANGE, header::IF_NONE_MATCH] {
        native_headers.remove(name);
    }
    let init = native_segment(state, session, "init.mp4", native_headers.clone()).await?;
    let fragment = native_segment(state, session, segment, native_headers.clone()).await?;
    let layout = match select(&init, &fragment, headers, session)? {
        Selection::Complete(response) => return Ok(response),
        Selection::Read(layout) => layout,
    };
    if !layout.partial {
        return layout.response(init.into_body(), fragment.into_body());
    }
    let init_etag = init.headers().get(header::ETAG).cloned();
    let fragment_etag = fragment.headers().get(header::ETAG).cloned();
    drop(init);
    drop(fragment);
    let (init_range, fragment_range) = layout.native_ranges();
    async fn part(
        state: &AppState,
        session: &str,
        name: &str,
        headers: &HeaderMap,
        range: Option<(u64, u64)>,
        expected_etag: Option<axum::http::HeaderValue>,
    ) -> Result<Body, ApiError> {
        let Some((start, end)) = range else {
            return Ok(Body::empty());
        };
        let mut headers = headers.clone();
        headers.insert(
            header::RANGE,
            format!("bytes={start}-{end}")
                .parse()
                .map_err(|_| ApiError::BadRequest("invalid media range".into()))?,
        );
        if let Some(etag) = &expected_etag {
            headers.insert(header::IF_RANGE, etag.clone());
        }
        let response = native_segment(state, session, name, headers).await?;
        let len = response
            .headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        if response.status() != StatusCode::PARTIAL_CONTENT
            || len != Some(end - start + 1)
            || response.headers().get(header::ETAG) != expected_etag.as_ref()
        {
            return Err(ApiError::Conflict(
                "native range representation changed".into(),
            ));
        }
        Ok(response.into_body())
    }
    let init = Box::pin(part(
        state,
        session,
        "init.mp4",
        &native_headers,
        init_range,
        init_etag,
    ))
    .await?;
    let fragment = Box::pin(part(
        state,
        session,
        segment,
        &native_headers,
        fragment_range,
        fragment_etag,
    ))
    .await?;
    layout.response(init, fragment)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn native(bytes: &'static str) -> Response {
        Response::builder()
            .header(header::CONTENT_LENGTH, bytes.len())
            .body(Body::from(bytes))
            .expect("native fixture")
    }
    #[tokio::test]
    async fn init_prefix_ranges_cross_the_boundary_and_conditionals_name_the_combined_bytes() {
        for (range, expected) in [
            ("bytes=0-", "INITfragment"),
            ("bytes=2-6", "ITfra"),
            ("bytes=4-", "fragment"),
            ("bytes=-3", "ent"),
            ("bytes=0-2", "INI"),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(header::RANGE, range.parse().expect("range"));
            let Selection::Read(layout) = select(
                &native("INIT"),
                &native("fragment"),
                &headers,
                "private-session",
            )
            .expect("layout") else {
                panic!("read layout");
            };
            let (init, fragment) = layout.native_ranges();
            let body = |bytes: &'static str, range: Option<(u64, u64)>| {
                range.map_or(Body::empty(), |(a, b)| {
                    Body::from(bytes[a as usize..=b as usize].to_owned())
                })
            };
            let response = layout
                .response(body("INIT", init), body("fragment", fragment))
                .expect("representation");
            assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
            assert_eq!(
                axum::body::to_bytes(response.into_body(), 128)
                    .await
                    .expect("stream"),
                expected
            );
        }
        let Selection::Read(layout) = select(
            &native("INIT"),
            &native("fragment"),
            &HeaderMap::new(),
            "private-session",
        )
        .expect("full") else {
            panic!("read layout");
        };
        assert!(!layout.etag.contains("private-session"));
        let mut headers = HeaderMap::new();
        headers.insert(
            header::IF_NONE_MATCH,
            format!("W/{}", layout.etag).parse().expect("etag"),
        );
        let Selection::Complete(response) = select(
            &native("INIT"),
            &native("fragment"),
            &headers,
            "private-session",
        )
        .expect("conditional") else {
            panic!("conditional response");
        };
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        headers.clear();
        headers.insert(header::RANGE, "bytes=999-".parse().expect("range"));
        let Selection::Complete(response) = select(
            &native("INIT"),
            &native("fragment"),
            &headers,
            "private-session",
        )
        .expect("refused") else {
            panic!("range refusal");
        };
        assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        headers.insert(
            header::IF_RANGE,
            "\"old-object\"".parse().expect("old etag"),
        );
        let Selection::Read(layout) = select(
            &native("INIT"),
            &native("fragment"),
            &headers,
            "private-session",
        )
        .expect("ignore stale range") else {
            panic!("full layout");
        };
        assert!(!layout.partial);
    }
}
