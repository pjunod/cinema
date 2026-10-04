//! Bounded engine-relative HLS resources. Validation never fetches or rewrites
//! a URL and never supplies grant, session or response-publication authority.
use std::collections::BTreeMap;

pub const MAX_SHARING_PLAYLIST_BYTES: usize = 1024 * 1024;
const MAX_PLAYLIST_LINES: usize = 32_768;
const MAX_PLAYLIST_LINE_BYTES: usize = 8192;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("sharing_resource_unsupported")]
pub struct SharingResourceUnsupported;
type Result<T> = std::result::Result<T, SharingResourceUnsupported>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharingHlsResourceKind {
    Master,
    Index,
    Video,
    Init,
    MediaSegment,
    SubtitlePlaylist { index: u16 },
    SubtitleSegment { index: u16 },
}

/// The original bounded relative URI is retained, including segment padding.
/// Only the parser constructs a resource; callers cannot replace its path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharingHlsResource {
    uri: String,
    kind: SharingHlsResourceKind,
    native: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharingFileResourceKind {
    Decision,
    Start,
    Direct,
    Progressive,
    Subtitle { index: u16 },
    SubtitleManifest { index: u16 },
    SubtitleObject { index: u16 },
    ChapterThumbnail { index: u16 },
}

/// A file-relative suffix only. Authentication, exact file/revision membership
/// and the optional B session binding are separate mandatory handler inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharingFileResource {
    suffix: String,
    kind: SharingFileResourceKind,
}
impl SharingFileResource {
    pub fn parse(suffix: &str) -> Result<Self> {
        if suffix.len() > 256 || !suffix.is_ascii() {
            return Err(SharingResourceUnsupported);
        }
        let fields = suffix.split('/').collect::<Vec<_>>();
        let index = |value: &str| -> Result<u16> {
            let parsed = digits(value, 4095)?;
            if parsed.to_string() != value {
                return Err(SharingResourceUnsupported);
            }
            Ok(parsed as u16)
        };
        let digest = |value: &str| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        };
        let kind = match fields.as_slice() {
            ["decision"] => SharingFileResourceKind::Decision,
            ["hls", "sessions"] => SharingFileResourceKind::Start,
            ["direct"] => SharingFileResourceKind::Direct,
            ["stream.mp4"] => SharingFileResourceKind::Progressive,
            ["subs", track] => SharingFileResourceKind::Subtitle {
                index: index(track.strip_suffix(".vtt").unwrap_or(track))?,
            },
            ["subs", track, "overlay.json"] => SharingFileResourceKind::SubtitleManifest {
                index: index(track)?,
            },
            ["subs", track, "overlay", generation, "objects", object]
                if digest(generation) && object.strip_suffix(".png").is_some_and(digest) =>
            {
                SharingFileResourceKind::SubtitleObject {
                    index: index(track)?,
                }
            }
            ["chapters", chapter, "thumb"] => SharingFileResourceKind::ChapterThumbnail {
                index: index(chapter)?,
            },
            _ => return Err(SharingResourceUnsupported),
        };
        Ok(Self {
            suffix: suffix.to_owned(),
            kind,
        })
    }
    pub fn as_str(&self) -> &str {
        &self.suffix
    }
    pub fn kind(&self) -> SharingFileResourceKind {
        self.kind
    }
}

fn digits(value: &str, maximum: u64) -> Result<u64> {
    if value.is_empty() || value.len() > 19 || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(SharingResourceUnsupported);
    }
    let parsed = value
        .parse::<u64>()
        .map_err(|_| SharingResourceUnsupported)?;
    if parsed > maximum {
        return Err(SharingResourceUnsupported);
    }
    Ok(parsed)
}

fn segment(value: &str, suffix: &str) -> Result<()> {
    let number = value
        .strip_prefix("seg")
        .and_then(|value| value.strip_suffix(suffix))
        .ok_or(SharingResourceUnsupported)?;
    digits(number, i64::MAX as u64)?;
    Ok(())
}

impl SharingHlsResource {
    pub fn parse(uri: &str) -> Result<Self> {
        if uri.is_empty()
            || uri.len() > 384
            || !uri.is_ascii()
            || uri.bytes().any(|b| b <= b' ' || b == 127)
            || uri.contains(['%', '\\', '#', ':'])
            || uri.starts_with('/')
        {
            return Err(SharingResourceUnsupported);
        }
        let (path, query) = match uri.split_once('?') {
            Some((path, query)) if !query.is_empty() && !query.contains('?') => (path, Some(query)),
            Some(_) => return Err(SharingResourceUnsupported),
            None => (uri, None),
        };
        let kind = match path {
            "master.m3u8" => SharingHlsResourceKind::Master,
            "index.m3u8" => SharingHlsResourceKind::Index,
            "video.m3u8" => SharingHlsResourceKind::Video,
            "init.mp4" => SharingHlsResourceKind::Init,
            _ if path.starts_with("init-e") && path.ends_with(".mp4") => {
                let epoch = digits(&path[6..path.len() - 4], i64::MAX as u64)?;
                if epoch == 0 {
                    return Err(SharingResourceUnsupported);
                }
                SharingHlsResourceKind::Init
            }
            _ if path.starts_with("seg") && (path.ends_with(".ts") || path.ends_with(".m4s")) => {
                segment(path, if path.ends_with(".ts") { ".ts" } else { ".m4s" })?;
                SharingHlsResourceKind::MediaSegment
            }
            _ => {
                let fields: Vec<_> = path.split('/').collect();
                let ["subs", index, name] = fields.as_slice() else {
                    return Err(SharingResourceUnsupported);
                };
                let index = digits(index, 4095)? as u16;
                if *name == "index.m3u8" {
                    SharingHlsResourceKind::SubtitlePlaylist { index }
                } else {
                    segment(name, ".vtt")?;
                    SharingHlsResourceKind::SubtitleSegment { index }
                }
            }
        };
        let mut native = None;
        if let Some(query) = query {
            if query.len() > 256
                || !matches!(
                    kind,
                    SharingHlsResourceKind::Master
                        | SharingHlsResourceKind::Index
                        | SharingHlsResourceKind::Video
                )
            {
                return Err(SharingResourceUnsupported);
            }
            let mut seen = std::collections::BTreeSet::new();
            for pair in query.split('&') {
                let (key, value) = pair.split_once('=').ok_or(SharingResourceUnsupported)?;
                if !seen.insert(key) {
                    return Err(SharingResourceUnsupported);
                }
                match key {
                    "native" if matches!(value, "0" | "1") => {
                        native = Some(if value == "1" { 1 } else { 0 });
                    }
                    "subtitle" if value == "-1" => {}
                    "subtitle" => {
                        digits(value, 4095)?;
                    }
                    "diagnostic"
                        if matches!(
                            value,
                            "video-only"
                                | "video-only-codecs"
                                | "video-only-range"
                                | "video-only-hdr"
                        ) => {}
                    _ => return Err(SharingResourceUnsupported),
                }
            }
        }
        Ok(Self {
            uri: uri.into(),
            kind,
            native,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.uri
    }
    pub fn kind(&self) -> SharingHlsResourceKind {
        self.kind
    }
}

fn attributes(raw: &str) -> Result<BTreeMap<&str, &str>> {
    let mut result = BTreeMap::new();
    let mut rest = raw;
    while !rest.is_empty() {
        if result.len() >= 16 {
            return Err(SharingResourceUnsupported);
        }
        let (key, tail) = rest.split_once('=').ok_or(SharingResourceUnsupported)?;
        if key.is_empty()
            || !key
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(SharingResourceUnsupported);
        }
        let (value, tail) = if let Some(quoted) = tail.strip_prefix('"') {
            let end = quoted.find('"').ok_or(SharingResourceUnsupported)?;
            let remaining = &quoted[end + 1..];
            let remaining = if remaining.is_empty() {
                ""
            } else {
                remaining
                    .strip_prefix(',')
                    .ok_or(SharingResourceUnsupported)?
            };
            (&quoted[..end], remaining)
        } else {
            match tail.split_once(',') {
                Some((value, tail)) => (value, tail),
                None => (tail, ""),
            }
        };
        if value.is_empty()
            || value.len() > 2048
            || value.contains('"')
            || result.insert(key, value).is_some()
        {
            return Err(SharingResourceUnsupported);
        }
        if tail.is_empty() && rest.ends_with(',') {
            return Err(SharingResourceUnsupported);
        }
        rest = tail;
    }
    Ok(result)
}

fn closed_attributes<'a>(raw: &'a str, allowed: &[&str]) -> Result<BTreeMap<&'a str, &'a str>> {
    let attrs = attributes(raw)?;
    if attrs.keys().any(|key| !allowed.contains(key)) {
        return Err(SharingResourceUnsupported);
    }
    Ok(attrs)
}

/// Validate the actual engine's master, TS/fMP4 media and WebVTT playlists.
/// All bytes remain unchanged. Unknown URI-bearing tags, encryption and
/// resource forms fail closed rather than becoming a general proxy grammar.
pub fn validate_sharing_playlist(resource: &SharingHlsResource, bytes: &[u8]) -> Result<()> {
    if bytes.is_empty() || bytes.len() > MAX_SHARING_PLAYLIST_BYTES {
        return Err(SharingResourceUnsupported);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| SharingResourceUnsupported)?;
    if text
        .bytes()
        .any(|b| (b < 32 && b != b'\r' && b != b'\n') || b == 127)
    {
        return Err(SharingResourceUnsupported);
    }
    let master = matches!(resource.kind, SharingHlsResourceKind::Master)
        || matches!(resource.kind, SharingHlsResourceKind::Index) && resource.native == Some(1);
    let subtitle = match resource.kind {
        SharingHlsResourceKind::SubtitlePlaylist { index } => Some(index),
        SharingHlsResourceKind::Master
        | SharingHlsResourceKind::Index
        | SharingHlsResourceKind::Video => None,
        _ => return Err(SharingResourceUnsupported),
    };
    let mut lines = text.lines();
    if lines.next() != Some("#EXTM3U") {
        return Err(SharingResourceUnsupported);
    }
    let mut pending = false;
    for (number, line) in lines.enumerate() {
        if number >= MAX_PLAYLIST_LINES
            || line.len() > MAX_PLAYLIST_LINE_BYTES
            || line.contains('\r')
        {
            return Err(SharingResourceUnsupported);
        }
        if line.is_empty() {
            continue;
        }
        if !line.starts_with('#') {
            if !pending {
                return Err(SharingResourceUnsupported);
            }
            let child = if let Some(index) = subtitle {
                SharingHlsResource::parse(&format!("subs/{index}/{line}"))?
            } else {
                SharingHlsResource::parse(line)?
            };
            let valid = if master {
                matches!(
                    child.kind,
                    SharingHlsResourceKind::Index | SharingHlsResourceKind::Video
                ) && child.native != Some(1)
            } else if subtitle.is_some() {
                matches!(child.kind, SharingHlsResourceKind::SubtitleSegment { .. })
            } else {
                child.kind == SharingHlsResourceKind::MediaSegment
            };
            if !valid {
                return Err(SharingResourceUnsupported);
            }
            pending = false;
            continue;
        }
        if let Some(raw) = line.strip_prefix("#EXT-X-MEDIA:") {
            if !master || pending {
                return Err(SharingResourceUnsupported);
            }
            let attrs = closed_attributes(
                raw,
                &[
                    "TYPE",
                    "GROUP-ID",
                    "NAME",
                    "LANGUAGE",
                    "DEFAULT",
                    "AUTOSELECT",
                    "FORCED",
                    "CHARACTERISTICS",
                    "URI",
                ],
            )?;
            if attrs.get("TYPE") != Some(&"SUBTITLES") {
                return Err(SharingResourceUnsupported);
            }
            let child =
                SharingHlsResource::parse(attrs.get("URI").ok_or(SharingResourceUnsupported)?)?;
            if !matches!(child.kind, SharingHlsResourceKind::SubtitlePlaylist { .. }) {
                return Err(SharingResourceUnsupported);
            }
        } else if let Some(raw) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            if !master || pending {
                return Err(SharingResourceUnsupported);
            }
            let attrs = closed_attributes(
                raw,
                &[
                    "BANDWIDTH",
                    "AVERAGE-BANDWIDTH",
                    "RESOLUTION",
                    "FRAME-RATE",
                    "VIDEO-RANGE",
                    "CODECS",
                    "SUPPLEMENTAL-CODECS",
                    "CLOSED-CAPTIONS",
                    "SUBTITLES",
                ],
            )?;
            digits(
                attrs.get("BANDWIDTH").ok_or(SharingResourceUnsupported)?,
                i64::MAX as u64,
            )?;
            pending = true;
        } else if let Some(raw) = line.strip_prefix("#EXT-X-MAP:") {
            if master || subtitle.is_some() || pending {
                return Err(SharingResourceUnsupported);
            }
            let attrs = closed_attributes(raw, &["URI"])?;
            if SharingHlsResource::parse(attrs.get("URI").ok_or(SharingResourceUnsupported)?)?.kind
                != SharingHlsResourceKind::Init
            {
                return Err(SharingResourceUnsupported);
            }
        } else if let Some(raw) = line.strip_prefix("#EXTINF:") {
            if master || pending {
                return Err(SharingResourceUnsupported);
            }
            let (duration, _) = raw.split_once(',').ok_or(SharingResourceUnsupported)?;
            let duration = duration
                .parse::<f64>()
                .map_err(|_| SharingResourceUnsupported)?;
            if !duration.is_finite() || duration <= 0.0 || duration > 86400.0 {
                return Err(SharingResourceUnsupported);
            }
            pending = true;
        } else if let Some(raw) = line.strip_prefix("#EXT-X-START:") {
            let attrs = closed_attributes(raw, &["TIME-OFFSET"])?;
            let offset = attrs
                .get("TIME-OFFSET")
                .ok_or(SharingResourceUnsupported)?
                .parse::<f64>()
                .map_err(|_| SharingResourceUnsupported)?;
            if !offset.is_finite() || offset.abs() > 86400.0 {
                return Err(SharingResourceUnsupported);
            }
        } else if let Some(raw) = line.strip_prefix("#EXT-X-VERSION:") {
            if digits(raw, 10)? == 0 {
                return Err(SharingResourceUnsupported);
            }
        } else if let Some(raw) = line.strip_prefix("#EXT-X-TARGETDURATION:") {
            if digits(raw, 86400)? == 0 {
                return Err(SharingResourceUnsupported);
            }
        } else if let Some(raw) = line
            .strip_prefix("#EXT-X-MEDIA-SEQUENCE:")
            .or_else(|| line.strip_prefix("#EXT-X-DISCONTINUITY-SEQUENCE:"))
        {
            digits(raw, i64::MAX as u64)?;
        } else if matches!(
            line,
            "#EXT-X-PLAYLIST-TYPE:EVENT"
                | "#EXT-X-PLAYLIST-TYPE:VOD"
                | "#EXT-X-ENDLIST"
                | "#EXT-X-INDEPENDENT-SEGMENTS"
                | "#EXT-X-DISCONTINUITY"
                | "#EXT-X-ALLOW-CACHE:YES"
                | "#EXT-X-ALLOW-CACHE:NO"
        ) {
            if master || pending {
                return Err(SharingResourceUnsupported);
            }
        } else if line.starts_with("#EXT") {
            return Err(SharingResourceUnsupported);
        }
    }
    if pending {
        return Err(SharingResourceUnsupported);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sharing_file_resources_bind_closed_current_router_suffixes() {
        for suffix in [
            "decision",
            "hls/sessions",
            "direct",
            "stream.mp4",
            "subs/0",
            "subs/4095.vtt",
            "subs/1/overlay.json",
            "chapters/4095/thumb",
        ] {
            assert_eq!(
                SharingFileResource::parse(suffix)
                    .expect("current router suffix")
                    .as_str(),
                suffix
            );
        }
        let object = format!(
            "subs/2/overlay/{}/objects/{}.png",
            "a".repeat(64),
            "b".repeat(64)
        );
        assert_eq!(
            SharingFileResource::parse(&object)
                .expect("published PGS naming")
                .kind(),
            SharingFileResourceKind::SubtitleObject { index: 2 }
        );
        for suffix in [
            "",
            "https://source/direct",
            "/direct",
            "//source/direct",
            "../direct",
            "direct?session=anything",
            "direct#other",
            "subs/01",
            "subs/4096",
            "subs/-1",
            "subs/0%2fvtt",
            "subs/0\\vtt",
            "subs/0/../direct",
            "chapters/1/thumb/extra",
            "hls/sessions/../control",
        ] {
            assert!(SharingFileResource::parse(suffix).is_err(), "{suffix}");
        }
        assert!(SharingFileResource::parse(&object.replace(".png", ".jpg")).is_err());
        assert!(
            SharingFileResource::parse(&object.replace(&"a".repeat(64), &"A".repeat(64))).is_err()
        );
    }
    #[test]
    fn sharing_hls_resources_refuse_proxy_and_query_escapes() {
        for uri in [
            "index.m3u8",
            "index.m3u8?native=1&subtitle=-1",
            "master.m3u8?diagnostic=video-only-hdr",
            "init-e2.mp4",
            "seg00001.ts",
            "seg00001.m4s",
            "subs/2/index.m3u8",
            "subs/2/seg00001.vtt",
        ] {
            assert!(SharingHlsResource::parse(uri).is_ok(), "{uri}");
        }
        for uri in [
            "/init.mp4",
            "//source/init.mp4",
            "https://source/init.mp4",
            "../init.mp4",
            "subs/2/../init.mp4",
            "seg%2f00001.ts",
            "seg00001.ts#fragment",
            "seg00001.ts?url=source",
            "index.m3u8?native=1&native=0",
            "index.m3u8?native=2",
            "index.m3u8?subtitle=4096",
            "index.m3u8?token=secret",
            "index.m3u8?diagnostic=unknown",
            "index.m3u8?",
            "subs/0/seg9999999999999999999.vtt",
            "init-e0.mp4",
        ] {
            assert!(SharingHlsResource::parse(uri).is_err(), "{uri}");
        }
    }
    #[test]
    fn sharing_hls_playlist_preserves_current_engine_resource_shapes() {
        for (uri, playlist) in [
            ("master.m3u8", "#EXTM3U\n#EXT-X-VERSION:10\n#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",NAME=\"Français, SDH\",LANGUAGE=\"fr\",DEFAULT=NO,AUTOSELECT=YES,FORCED=NO,URI=\"subs/2/index.m3u8\"\n#EXT-X-STREAM-INF:BANDWIDTH=1000000,AVERAGE-BANDWIDTH=800000,CODECS=\"hvc1.2.4.L153.B0,mp4a.40.2\",SUPPLEMENTAL-CODECS=\"dvh1.08.06/db4h\",CLOSED-CAPTIONS=NONE,SUBTITLES=\"subs\"\nindex.m3u8\n"),
            ("index.m3u8", "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:6\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-MAP:URI=\"init-e2.mp4\"\n#EXT-X-START:TIME-OFFSET=0\n#EXTINF:6.0,\nseg00000.m4s\n#EXT-X-ENDLIST\n"),
            ("video.m3u8", "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-INDEPENDENT-SEGMENTS\n#EXT-X-PLAYLIST-TYPE:EVENT\n#EXTINF:6.0,\nseg00000.ts\n"),
            ("subs/2/index.m3u8", "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:6\n#EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:6.0,\nseg00000.vtt\n#EXT-X-ENDLIST\n"),
        ] {
            let before = playlist.as_bytes().to_vec();
            validate_sharing_playlist(&SharingHlsResource::parse(uri).expect("engine resource"), &before).expect("engine playlist");
            assert_eq!(before, playlist.as_bytes());
        }
    }

    #[test]
    fn sharing_hls_playlist_accepts_the_actual_fmp4_generator_output() {
        let mut playlist = crate::fmp4::playlist_header(6);
        for index in [0, 1, 100000] {
            playlist.push_str(&crate::fmp4::playlist_entry(
                6.0,
                &crate::fmp4::segment_name(index),
            ));
        }
        playlist.push_str("#EXT-X-ENDLIST\n");
        validate_sharing_playlist(
            &SharingHlsResource::parse("index.m3u8").expect("media resource"),
            playlist.as_bytes(),
        )
        .expect("unmodified actual generator output");
    }
    #[test]
    fn sharing_hls_playlist_refuses_hidden_uris_encryption_and_unbounded_bodies() {
        let resource = SharingHlsResource::parse("index.m3u8").expect("media playlist");
        for tag in [
            "#EXT-X-KEY:METHOD=NONE",
            "#EXT-X-KEY:METHOD=AES-128,URI=\"key\"",
            "#EXT-X-SESSION-KEY:METHOD=AES-128,URI=\"key\"",
            "#EXT-X-PART:DURATION=1,URI=\"seg00000.m4s\"",
            "#EXT-X-MAP:URI=\"https://source/init.mp4\"",
            "#EXT-X-MAP:URI=\"init.mp4\",URI=\"init-e2.mp4\"",
            "#EXT-X-MAP:URI=\"init.mp4\",BYTERANGE=\"20@0\"",
            "#EXT-X-START:TIME-OFFSET=0,URI=\"source\"",
        ] {
            assert!(
                validate_sharing_playlist(
                    &resource,
                    format!("#EXTM3U\n{tag}\n#EXTINF:6.0,\nseg00000.m4s\n").as_bytes()
                )
                .is_err(),
                "{tag}"
            );
        }
        for body in [
            "#EXTM3U\n#EXTINF:NaN,\nseg00000.ts\n",
            "#EXTM3U\n#EXTINF:6,\n",
            "#EXTM3U\nseg00000.ts\n",
            "#EXTM3U\n#EXTINF:6,\nsubs/0/seg00000.vtt\n",
            "#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4\",\n",
        ] {
            assert!(
                validate_sharing_playlist(&resource, body.as_bytes()).is_err(),
                "{body}"
            );
        }
        assert!(
            validate_sharing_playlist(&resource, &vec![b'x'; MAX_SHARING_PLAYLIST_BYTES + 1])
                .is_err()
        );
        let master = SharingHlsResource::parse("master.m3u8").expect("master");
        assert!(validate_sharing_playlist(
            &master,
            b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1,UNKNOWN-URI=\"source\"\nindex.m3u8\n"
        )
        .is_err());
    }
    #[test]
    fn sharing_protocol_fixture_file_suffixes() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../../tests/sharing/protocol-cases.json"))
                .expect("synthetic sharing fixture");
        let rows = fixture["file_suffixes"]
            .as_array()
            .expect("file suffix rows");
        assert!(rows.len() >= 20, "file suffix rows");
        for row in rows {
            let suffix = row["suffix"].as_str().expect("suffix");
            let parsed = SharingFileResource::parse(suffix);
            assert_eq!(parsed.is_ok(), row["expected"] == "accepted", "{suffix:?}");
            let Ok(resource) = parsed else { continue };
            let route = match resource.kind() {
                SharingFileResourceKind::Decision => "decision",
                SharingFileResourceKind::Start => "start",
                SharingFileResourceKind::Direct => "direct",
                SharingFileResourceKind::Progressive => "closed",
                SharingFileResourceKind::Subtitle { .. }
                | SharingFileResourceKind::SubtitleManifest { .. }
                | SharingFileResourceKind::SubtitleObject { .. }
                | SharingFileResourceKind::ChapterThumbnail { .. } => "asset",
            };
            assert_eq!(row["b"], route, "{suffix:?}");
        }
    }
}
