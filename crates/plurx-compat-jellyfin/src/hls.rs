//! Closed translation of native HLS resource names. The only thing added is the
//! caller's own URL credential, when the handler passes one (`public_query`).
use std::ops::Range;

const MAX_MANIFEST_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resource {
    Master,
    Media,
    Init,
    Segment(String),
    SubtitlePlaylist(u32),
    SubtitleSegment(u32, String),
}

fn decimal(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 10
        && value.bytes().all(|c| c.is_ascii_digit())
        && value.parse::<u32>().is_ok()
}

impl Resource {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "master.m3u8" => return Some(Self::Master),
            "index.m3u8" => return Some(Self::Media),
            "init.mp4" => return Some(Self::Init),
            _ => {}
        }
        if let Some(index) = value
            .strip_prefix("seg")
            .and_then(|s| s.strip_suffix(".m4s"))
        {
            return decimal(index).then(|| Self::Segment(index.to_owned()));
        }
        let rest = value.strip_prefix("subs/")?;
        let (track, filename) = rest.split_once('/')?;
        if !decimal(track) {
            return None;
        }
        let track = track.parse().ok()?;
        if filename == "index.m3u8" {
            return Some(Self::SubtitlePlaylist(track));
        }
        let index = filename.strip_prefix("seg")?.strip_suffix(".vtt")?;
        decimal(index).then(|| Self::SubtitleSegment(track, index.to_owned()))
    }

    pub fn native_path(&self) -> String {
        match self {
            Self::Master => "master.m3u8".into(),
            Self::Media => "index.m3u8".into(),
            Self::Init => "init.mp4".into(),
            Self::Segment(index) => format!("seg{index}.m4s"),
            Self::SubtitlePlaylist(track) => format!("subs/{track}/index.m3u8"),
            Self::SubtitleSegment(track, index) => format!("subs/{track}/seg{index}.vtt"),
        }
    }

    fn public_path(&self, inline_init: bool) -> String {
        match self {
            Self::Segment(index) if inline_init => format!("seg{index}.ts"),
            _ => self.native_path(),
        }
    }
}

/// Return the URI's quoted value range, while respecting commas inside other quoted attributes.
fn uri_attribute(attributes: &str) -> Result<Option<Range<usize>>, &'static str> {
    let mut start = 0;
    let mut quoted = false;
    let mut uri = None;
    for (end, c) in attributes
        .bytes()
        .enumerate()
        .chain(std::iter::once((attributes.len(), b',')))
    {
        if c == b'"' {
            quoted = !quoted;
        }
        if c != b',' || quoted {
            continue;
        }
        let field = &attributes[start..end];
        let (key, value) = field.split_once('=').ok_or("invalid HLS attribute")?;
        if key == "URI" {
            if uri.is_some() || value.len() < 2 || !value.starts_with('"') || !value.ends_with('"')
            {
                return Err("invalid HLS URI attribute");
            }
            uri = Some(start + key.len() + 2..end - 1);
        }
        start = end + 1;
    }
    if quoted {
        return Err("unterminated HLS attribute");
    }
    Ok(uri)
}

fn resolve(uri: &str, parent: &Resource, native_session: &str) -> Option<Resource> {
    let absolute = format!("/api/v1/hls/{native_session}/");
    if let Some(path) = uri.strip_prefix(&absolute) {
        return Resource::parse(path);
    }
    if uri.starts_with('/') {
        return None;
    }
    match parent {
        Resource::SubtitlePlaylist(track) => Resource::parse(&format!("subs/{track}/{uri}")),
        Resource::Master | Resource::Media => Resource::parse(uri),
        _ => None,
    }
}

/// Translate only native resources supported by the adapter. Original timelines and HLS metadata
/// survive unchanged. Inline-init delivery removes MAP and gives each native video fragment a TS
/// filename; the HTTP handler must prefix the native initialization bytes for that delivery mode.
///
/// `public_query`, when given, is appended to every rewritten URI. A player
/// resolves a playlist's children without the playlist's own query, so a
/// client that authenticates media only by URL (Android TV sends `ApiKey`
/// and no header) needs it on each child, as Jellyfin's playlists carry it.
pub fn rewrite_manifest(
    input: &str,
    parent: &Resource,
    native_session: &str,
    public_base: &str,
    public_query: Option<&str>,
    inline_init: bool,
) -> Result<String, &'static str> {
    if public_query.is_some_and(|query| {
        query.is_empty()
            || query.len() > 256
            || !query
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'=' | b'_' | b'-'))
    }) {
        return Err("invalid HLS manifest context");
    }
    let suffix = public_query
        .map(|query| format!("?{query}"))
        .unwrap_or_default();
    if input.len() > MAX_MANIFEST_BYTES
        || input.contains('\r')
        || input.contains('\0')
        || input.lines().next() != Some("#EXTM3U")
        || !matches!(
            parent,
            Resource::Master | Resource::Media | Resource::SubtitlePlaylist(_)
        )
        || native_session.is_empty()
        || native_session.len() > 128
        || !native_session
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        || !public_base.starts_with("/jellyfin/")
        || !public_base.ends_with('/')
        || public_base.contains(['?', '#', '%', '\\'])
        || public_base.contains("..")
        || !public_base
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'/' || c == b'-' || c == b'_')
    {
        return Err("invalid HLS manifest context");
    }
    let mut output = String::with_capacity(input.len());
    let mut map_seen = false;
    for line in input.lines() {
        if line.len() > 16 * 1024 {
            return Err("HLS line exceeds bound");
        }
        if !line.starts_with('#') && !line.is_empty() {
            let resource =
                resolve(line, parent, native_session).ok_or("unsupported HLS resource")?;
            let legal = match (parent, &resource) {
                (Resource::Master, Resource::Media) | (Resource::Media, Resource::Segment(_)) => {
                    true
                }
                // A subtitle playlist lists only its own track's cues, even
                // when the native URI is absolute.
                (Resource::SubtitlePlaylist(track), Resource::SubtitleSegment(owner, _)) => {
                    track == owner
                }
                _ => false,
            };
            if !legal {
                return Err("unexpected HLS child resource");
            }
            output.push_str(public_base);
            output.push_str(&resource.public_path(inline_init));
            output.push_str(&suffix);
        } else if let Some((tag, attributes)) = line.split_once(':') {
            if tag.starts_with("#EXT")
                && !matches!(
                    tag,
                    "#EXT-X-MEDIA"
                        | "#EXT-X-MAP"
                        | "#EXT-X-STREAM-INF"
                        | "#EXT-X-VERSION"
                        | "#EXT-X-TARGETDURATION"
                        | "#EXT-X-MEDIA-SEQUENCE"
                        | "#EXT-X-PLAYLIST-TYPE"
                        | "#EXT-X-DISCONTINUITY-SEQUENCE"
                        | "#EXT-X-START"
                        | "#EXT-X-PROGRAM-DATE-TIME"
                        | "#EXT-X-BYTERANGE"
                        | "#EXTINF"
                        | "#EXT-X-KEY"
                        | "#EXT-X-SESSION-KEY"
                )
            {
                return Err("unsupported HLS extension");
            }
            if tag == "#EXT-X-BYTERANGE" && inline_init {
                return Err("inline-init byte range unsupported");
            }
            if (tag == "#EXT-X-KEY" || tag == "#EXT-X-SESSION-KEY") && attributes != "METHOD=NONE" {
                return Err("encrypted HLS unsupported");
            }
            let attribute_tag = matches!(
                tag,
                "#EXT-X-MEDIA"
                    | "#EXT-X-MAP"
                    | "#EXT-X-I-FRAME-STREAM-INF"
                    | "#EXT-X-SESSION-DATA"
                    | "#EXT-X-KEY"
                    | "#EXT-X-SESSION-KEY"
                    | "#EXT-X-PART"
                    | "#EXT-X-PRELOAD-HINT"
                    | "#EXT-X-RENDITION-REPORT"
            );
            let uri = if attribute_tag {
                uri_attribute(attributes)?
            } else {
                None
            };
            if let Some(range) = uri {
                let resource = resolve(&attributes[range.clone()], parent, native_session)
                    .ok_or("unsupported HLS resource")?;
                let legal = matches!(
                    (tag, parent, &resource),
                    (
                        "#EXT-X-MEDIA",
                        Resource::Master,
                        Resource::SubtitlePlaylist(_)
                    ) | ("#EXT-X-MAP", Resource::Media, Resource::Init)
                );
                if !legal {
                    return Err("unsupported HLS URI tag");
                }
                if tag == "#EXT-X-MAP" {
                    if map_seen
                        || attributes != "URI=\"init.mp4\""
                            && attributes
                                != format!("URI=\"/api/v1/hls/{native_session}/init.mp4\"")
                    {
                        return Err("unsupported HLS map");
                    }
                    map_seen = true;
                    if inline_init {
                        continue;
                    }
                }
                output.push_str(tag);
                output.push(':');
                output.push_str(&attributes[..range.start]);
                output.push_str(public_base);
                output.push_str(&resource.public_path(inline_init));
                output.push_str(&suffix);
                output.push_str(&attributes[range.end..]);
            } else {
                output.push_str(line);
            }
        } else if line.starts_with("#EXT")
            && !matches!(
                line,
                "#EXTM3U"
                    | "#EXT-X-ENDLIST"
                    | "#EXT-X-DISCONTINUITY"
                    | "#EXT-X-INDEPENDENT-SEGMENTS"
            )
        {
            // The colon-less tags native playlists emit; any other tag is an
            // HLS feature this adapter has not qualified.
            return Err("unsupported HLS extension");
        } else {
            output.push_str(line);
        }
        output.push('\n');
    }
    if matches!(parent, Resource::Media) && !map_seen {
        return Err("native fMP4 initialization missing");
    }
    if output.contains(native_session) || output.contains("/api/v1/hls/") {
        return Err("private native HLS reference");
    }
    Ok(output)
}

/// Remove the subtitle rendition group from a native multivariant playlist.
///
/// A play negotiated without manifest subtitles still needs the multivariant
/// wrapper at its master URL (Infuse refuses a media playlist there), but must
/// not advertise text renditions its profile did not ask for: a player could
/// auto-select one over the user's choice or a burn-in. Each `TYPE=SUBTITLES`
/// rendition and every variant's `SUBTITLES` reference are dropped; all other
/// lines, including other attributes' order and quoting, are unchanged.
pub fn without_subtitle_renditions(master: &str) -> Result<String, &'static str> {
    let mut output = String::with_capacity(master.len());
    for line in master.lines() {
        if let Some(list) = line.strip_prefix("#EXT-X-MEDIA:") {
            if attribute_fields(list)?
                .iter()
                .any(|(key, value)| *key == "TYPE" && *value == "SUBTITLES")
            {
                continue;
            }
        } else if let Some(list) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            let kept = attribute_fields(list)?
                .into_iter()
                .filter(|(key, _)| *key != "SUBTITLES")
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>();
            output.push_str("#EXT-X-STREAM-INF:");
            output.push_str(&kept.join(","));
            output.push('\n');
            continue;
        }
        output.push_str(line);
        output.push('\n');
    }
    Ok(output)
}

/// Split an HLS attribute list into `(key, value)` pairs, respecting commas
/// inside quoted values.
fn attribute_fields(list: &str) -> Result<Vec<(&str, &str)>, &'static str> {
    let mut fields = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    for (end, c) in list
        .bytes()
        .enumerate()
        .chain(std::iter::once((list.len(), b',')))
    {
        if c == b'"' {
            quoted = !quoted;
        }
        if c != b',' || quoted {
            continue;
        }
        fields.push(
            list[start..end]
                .split_once('=')
                .ok_or("invalid HLS attribute")?,
        );
        start = end + 1;
    }
    if quoted {
        return Err("invalid HLS attribute");
    }
    Ok(fields)
}

#[cfg(test)]
mod tests {
    use super::*;
    const BASE: &str =
        "/jellyfin/Videos/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb/hls/";
    const SESSION: &str = "native-private-session";

    #[test]
    fn subtitle_free_master_drops_only_the_subtitle_group() {
        let master = "#EXTM3U\n#EXT-X-INDEPENDENT-SEGMENTS\n#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",NAME=\"English, SDH\",LANGUAGE=\"en\",AUTOSELECT=YES,URI=\"subs/2/index.m3u8\"\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"Main\",URI=\"audio.m3u8\"\n#EXT-X-STREAM-INF:BANDWIDTH=750000,CODECS=\"avc1.64001f,mp4a.40.2\",SUBTITLES=\"subs\",FRAME-RATE=24.000\nindex.m3u8\n";
        let stripped = without_subtitle_renditions(master).expect("native master");
        assert_eq!(
            stripped,
            "#EXTM3U\n#EXT-X-INDEPENDENT-SEGMENTS\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"Main\",URI=\"audio.m3u8\"\n#EXT-X-STREAM-INF:BANDWIDTH=750000,CODECS=\"avc1.64001f,mp4a.40.2\",FRAME-RATE=24.000\nindex.m3u8\n"
        );
        let wrapped = without_subtitle_renditions(&stripped).expect("idempotent");
        assert_eq!(wrapped, stripped);
        assert!(without_subtitle_renditions(
            "#EXTM3U\n#EXT-X-STREAM-INF:CODECS=\"avc1\nindex.m3u8\n"
        )
        .is_err());
        assert!(without_subtitle_renditions("#EXTM3U\n#EXT-X-MEDIA:TYPE\n").is_err());
    }

    #[test]
    fn manifests_rewrite_exact_resources_and_preserve_source_timeline() {
        let master = "#EXTM3U\n#EXT-X-MEDIA:TYPE=SUBTITLES,NAME=\"English, URI=pretend\",URI=\"subs/2/index.m3u8\"\n#EXT-X-STREAM-INF:BANDWIDTH=750000,CODECS=\"hvc1.1.6.L120,B0,mp4a.40.2\"\nindex.m3u8\n";
        let result = rewrite_manifest(master, &Resource::Master, SESSION, BASE, None, false)
            .expect("native manifest fixture");
        assert!(result.contains(&format!("URI=\"{BASE}subs/2/index.m3u8\"")));
        assert!(result.contains("NAME=\"English, URI=pretend\""));
        assert!(result.ends_with(&format!("{BASE}index.m3u8\n")));
        let media = "#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXT-X-MEDIA-SEQUENCE:99\n#EXTINF:6.003,\nseg00099.m4s\n#EXT-X-ENDLIST\n";
        let mp4 = rewrite_manifest(media, &Resource::Media, SESSION, BASE, None, false)
            .expect("native manifest fixture");
        assert!(mp4.contains(&format!("URI=\"{BASE}init.mp4\"")));
        assert!(mp4.contains("#EXT-X-MEDIA-SEQUENCE:99\n#EXTINF:6.003,"));
        let inline = rewrite_manifest(media, &Resource::Media, SESSION, BASE, None, true)
            .expect("native manifest fixture");
        assert!(!inline.contains("#EXT-X-MAP"));
        assert!(inline.contains(&format!("{BASE}seg00099.ts")));
        let subtitles = "#EXTM3U\n#EXTINF:6.003,\nseg99.vtt\n#EXT-X-ENDLIST\n";
        let result = rewrite_manifest(
            subtitles,
            &Resource::SubtitlePlaylist(2),
            SESSION,
            BASE,
            None,
            true,
        )
        .expect("native manifest fixture");
        assert!(result.contains(&format!("{BASE}subs/2/seg99.vtt")));
        let absolute = media
            .replace("init.mp4", &format!("/api/v1/hls/{SESSION}/init.mp4"))
            .replace(
                "seg00099.m4s",
                &format!("/api/v1/hls/{SESSION}/seg00099.m4s"),
            );
        assert_eq!(
            rewrite_manifest(&absolute, &Resource::Media, SESSION, BASE, None, false)
                .expect("native manifest fixture"),
            mp4
        );
    }

    #[test]
    fn every_rewritten_uri_carries_the_url_credential_for_header_less_clients() {
        let master = "#EXTM3U\n#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",NAME=\"English\",URI=\"subs/2/index.m3u8\"\n#EXT-X-STREAM-INF:BANDWIDTH=1,SUBTITLES=\"subs\"\nindex.m3u8\n";
        let result = rewrite_manifest(
            master,
            &Resource::Master,
            SESSION,
            BASE,
            Some("ApiKey=0123abcd"),
            false,
        )
        .expect("native manifest fixture");
        assert!(result.contains(&format!("URI=\"{BASE}subs/2/index.m3u8?ApiKey=0123abcd\"")));
        assert!(result.ends_with(&format!("{BASE}index.m3u8?ApiKey=0123abcd\n")));
        let media = "#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:6.003,\nseg00099.m4s\n";
        let mp4 = rewrite_manifest(
            media,
            &Resource::Media,
            SESSION,
            BASE,
            Some("ApiKey=0123abcd"),
            false,
        )
        .expect("native manifest fixture");
        assert!(mp4.contains(&format!("URI=\"{BASE}init.mp4?ApiKey=0123abcd\"")));
        assert!(mp4.contains(&format!("{BASE}seg00099.m4s?ApiKey=0123abcd\n")));
        for query in ["", "ApiKey=a&b=c", "ApiKey=\"x\"", "ApiKey=a b"] {
            assert!(
                rewrite_manifest(media, &Resource::Media, SESSION, BASE, Some(query), false)
                    .is_err(),
                "{query}"
            );
        }
    }

    #[test]
    fn malformed_or_foreign_resources_cannot_escape_authenticated_alias() {
        for uri in [
            "../index.m3u8",
            "https://peer/index.m3u8",
            "//peer/index.m3u8",
            "index.m3u8?token=private",
            "index%2em3u8",
            "/api/v1/hls/other/index.m3u8",
            "subs/0/seg1.vtt",
            "init.mp4",
            "index.m3u8\\x",
        ] {
            assert!(
                rewrite_manifest(
                    &format!("#EXTM3U\n{uri}\n"),
                    &Resource::Master,
                    SESSION,
                    BASE,
                    None,
                    false
                )
                .is_err(),
                "{uri}"
            );
        }
        for line in [
            "#EXT-X-MEDIA:URI=\"subs/0/index.m3u8\",URI=\"subs/1/index.m3u8\"",
            "#EXT-X-MEDIA:URI=\"subs/0/index.m3u8",
            "#EXT-X-MEDIA:URI=subs/0/index.m3u8",
            "#EXT-X-KEY:METHOD=AES-128,URI=\"secret\"",
            "#EXT-X-PART:URI=\"index.m3u8\"",
            "#EXT-X-MEDIA:URI=\"/api/v1/hls/other/subs/0/index.m3u8\"",
            "#comment native-private-session",
        ] {
            assert!(
                rewrite_manifest(
                    &format!("#EXTM3U\n{line}\n"),
                    &Resource::Master,
                    SESSION,
                    BASE,
                    None,
                    false
                )
                .is_err(),
                "{line}"
            );
        }
        assert!(rewrite_manifest(
            "#EXTM3U\nseg0.m4s\n",
            &Resource::Media,
            SESSION,
            BASE,
            None,
            true
        )
        .is_err());
        assert!(rewrite_manifest(
            "#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4\",BYTERANGE=\"10@0\"\n",
            &Resource::Media,
            SESSION,
            BASE,
            None,
            false
        )
        .is_err());
        for (line, parent) in [
            ("#EXT-X-GAP", Resource::Media),
            ("#EXT-X-I-FRAMES-ONLY", Resource::Media),
            ("#EXT-X-UNKNOWN-FLAG", Resource::Master),
            (
                "/api/v1/hls/native-private-session/subs/3/seg1.vtt",
                Resource::SubtitlePlaylist(2),
            ),
        ] {
            assert!(
                rewrite_manifest(
                    &if parent == Resource::Media {
                        format!("#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4\"\n{line}\n")
                    } else {
                        format!("#EXTM3U\n{line}\n")
                    },
                    &parent,
                    SESSION,
                    BASE,
                    None,
                    false
                )
                .is_err(),
                "{line}"
            );
        }
        let own = rewrite_manifest(
            "#EXTM3U\n#EXT-X-INDEPENDENT-SEGMENTS\n/api/v1/hls/native-private-session/subs/2/seg1.vtt\n#EXT-X-DISCONTINUITY\n#EXT-X-ENDLIST\n",
            &Resource::SubtitlePlaylist(2),
            SESSION,
            BASE,
            None,
            false,
        )
        .expect("own subtitle track");
        assert!(own.contains(&format!("{BASE}subs/2/seg1.vtt")));
        assert!(Resource::parse("subs/4294967296/index.m3u8").is_none());
        assert!(Resource::parse("seg4294967296.m4s").is_none());
    }
}
