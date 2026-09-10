//! The programme guide: a read-only feed the tuner contract never depends on.
//!
//! Everything here is owner-side and in memory. The guide is fetched by the
//! node that owns the tuner, cached in process, and served to every client
//! through one authenticated endpoint; a non-owner ingress relays the owner's
//! answer rather than fetching its own. Nothing is written to the store — the
//! `settings` table is replicated to every voter on every write and copied
//! whole into every snapshot, which is the wrong place for a few hundred
//! kilobytes that change every twenty minutes.
//!
//! Its failure modes are rendered, never fatal. `GET /live-tv/channels` and
//! session start never wait on a guide fetch and never consult guide state, so
//! a guide that is off, stale, or erroring costs a viewer some text under a
//! channel name and nothing else.

use std::collections::{BTreeMap, HashMap};
use std::io::Read as _;
use std::net::Ipv4Addr;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{
    fetch_bounded, pinned_url, sanitize_error, unix_seconds, DiscoverDocument, GuideSource,
    LiveTvChannel, LiveTvError,
};

/// Silicondust's own clients poll on this order. The free tier only ever
/// answers a few hours ahead, so anything slower leaves the end of the grid
/// empty even when the cache is nominally fresh.
pub(crate) const GUIDE_REFRESH_INTERVAL: Duration = Duration::from_secs(20 * 60);
/// After this the cache is dropped and `unavailable` is the honest answer.
/// A day-old grid is worse than no grid: it is confidently wrong.
pub(crate) const GUIDE_STALE_TTL: Duration = Duration::from_secs(6 * 60 * 60);
/// A refresh needs a lineup it did not fetch, and on a freshly started owner
/// the lineup cache is cold until a client reads channels. Waiting a full
/// refresh interval for that would leave the first visitor a guideless grid
/// for twenty minutes, so a refresh that found no lineup comes back soon.
pub(crate) const GUIDE_COLD_LINEUP_RETRY: Duration = Duration::from_secs(60);
pub(crate) const GUIDE_FETCH_TIMEOUT: Duration = Duration::from_secs(15);
/// One refresh owns this total wall-clock budget, including DNS, credentials,
/// every extension request, decompression and parsing. The manual endpoint's
/// 30-second client budget therefore still has time to return a useful error.
pub(crate) const GUIDE_REFRESH_TIMEOUT: Duration = Duration::from_secs(25);
/// XMLTV for a large lineup is a few MiB. Anything larger is refused rather
/// than streamed: this is a cache fill, not a download service.
pub(crate) const GUIDE_MAX_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const GUIDE_MAX_PROGRAMMES_PER_CHANNEL: usize = 200;
/// One bulk call, then at most one extension per channel.
pub(crate) const GUIDE_MAX_EXTENSION_REQUESTS: usize = 64;
pub(crate) const MAX_GUIDE_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_TITLE_BYTES: usize = 256;
const MAX_EPISODE_TITLE_BYTES: usize = 256;
const MAX_SYNOPSIS_BYTES: usize = 1024;
/// A ceiling on the character data accumulated for one XMLTV element before
/// its own field bound is applied. Without it, a hostile document could make
/// a single `<desc>` out of unlimited entity fragments and grow the buffer
/// without bound while every individual fragment stayed small.
const MAX_XMLTV_FIELD_BYTES: usize = 16 * 1024;
const MAX_AFFILIATE_BYTES: usize = 32;
const MAX_IMAGE_URL_BYTES: usize = 512;
const MAX_FILTERS: usize = 8;
const MAX_FILTER_BYTES: usize = 32;
const MAX_EPISODE_BYTES: usize = 16;
/// Guide rows come from a host plurx does not control. A programme longer than
/// this is a parse accident, not a broadcast.
const MAX_PROGRAMME_SECONDS: i64 = 24 * 60 * 60;

/// The guide host is pinned by allowlist exactly as artwork hosts are. Both
/// entries are Silicondust's; the path differs between them and which one
/// answers is verified against real hardware in M1's acceptance rather than
/// asserted here.
const GUIDE_HOSTS: &[&str] = &["api.hdhomerun.com", "my.hdhomerun.com"];
const GUIDE_BULK_URL: &str = "https://api.hdhomerun.com/api/guide";

pub(crate) fn approved_guide_url(url: &reqwest::Url) -> bool {
    url.scheme() == "https"
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
        && url
            .host_str()
            .is_some_and(|host| GUIDE_HOSTS.contains(&host))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GuideFreshness {
    Fresh,
    Stale,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GuideWindow {
    pub(crate) start: i64,
    pub(crate) end: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LiveTvProgramme {
    pub(crate) start: i64,
    pub(crate) end: i64,
    pub(crate) title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) episode_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) episode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) synopsis: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) image_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) original_air_date: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) filters: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LiveTvGuideChannel {
    pub(crate) id: String,
    pub(crate) guide_number: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) affiliate: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) image_url: Option<String>,
    pub(crate) programmes: Vec<LiveTvProgramme>,
}

/// The public guide document, and also exactly what one node relays to
/// another. There is no separate internal shape: relaying the owner's public
/// answer verbatim is what keeps the credential on the owner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LiveTvGuide {
    pub(crate) source: String,
    pub(crate) freshness: GuideFreshness,
    pub(crate) age_seconds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) fetched_at: Option<i64>,
    pub(crate) window: GuideWindow,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) refresh_error: Option<String>,
    /// How many lineup channels the source could be matched to, and how many
    /// were offered. The operator's whole diagnostic for a grabber whose
    /// display names do not line up.
    #[serde(default)]
    pub(crate) matched_channels: usize,
    #[serde(default)]
    pub(crate) lineup_channels: usize,
    pub(crate) channels: Vec<LiveTvGuideChannel>,
}

impl LiveTvGuide {
    pub(crate) fn unavailable(
        source: GuideSource,
        window: GuideWindow,
        error: Option<String>,
    ) -> Self {
        Self {
            source: source.as_str().to_owned(),
            freshness: GuideFreshness::Unavailable,
            age_seconds: 0,
            fetched_at: None,
            window,
            refresh_error: error,
            matched_channels: 0,
            lineup_channels: 0,
            channels: Vec::new(),
        }
    }

    pub(crate) fn total_programmes(&self) -> usize {
        self.channels.iter().map(|c| c.programmes.len()).sum()
    }

    /// Clip to the requested window and enforce the response cap by dropping
    /// the furthest-out programmes — never by dropping channels. A lineup that
    /// loses rows because the guide is long is a lineup an operator cannot
    /// trust; a grid that stops early is one they can read.
    pub(crate) fn clipped(&self, window: &GuideWindow) -> Self {
        let mut out = self.clone();
        out.window = window.clone();
        for channel in &mut out.channels {
            channel
                .programmes
                .retain(|p| p.end > window.start && p.start < window.end);
        }
        // Drop by measured bytes, not one row per re-serialisation. A large
        // lineup carries tens of thousands of rows, and re-encoding the whole
        // document after each pop is quadratic — on a real oversized guide
        // that is minutes of CPU on a request path, which the 512-channel
        // bounds test found by taking longer than the test harness allowed.
        for _ in 0..8 {
            let encoded = serde_json::to_vec(&out).map(|body| body.len()).unwrap_or(0);
            if encoded <= MAX_GUIDE_RESPONSE_BYTES {
                break;
            }
            // Headroom is a fixed slack for the enclosing punctuation, NOT a
            // fraction of the excess: scaling it with the overshoot meant a
            // document nine times the cap asked to drop nine times the cap's
            // worth of rows and emptied the guide entirely — the one outcome
            // this whole function exists to avoid.
            let mut excess = encoded - MAX_GUIDE_RESPONSE_BYTES + 1024;
            while excess > 0 {
                let Some((_, index)) = out
                    .channels
                    .iter()
                    .enumerate()
                    .filter_map(|(index, channel)| {
                        channel.programmes.last().map(|row| (row.start, index))
                    })
                    .max()
                else {
                    break;
                };
                let Some(row) = out.channels[index].programmes.pop() else {
                    break;
                };
                let cost = serde_json::to_vec(&row).map(|body| body.len()).unwrap_or(0) + 1;
                excess = excess.saturating_sub(cost);
            }
            if out.total_programmes() == 0 {
                break;
            }
        }
        out
    }
}

/// Normalisation applied to every source, because both of them are untrusted
/// input: bound every string, drop a programme without a title or with an
/// impossible span, sort by start, and remove overlaps by trusting the earlier
/// row's end. A guide that overlaps itself makes "what is on now" ambiguous.
pub(crate) fn normalise_programmes(mut rows: Vec<LiveTvProgramme>) -> Vec<LiveTvProgramme> {
    rows.retain(|p| {
        !p.title.is_empty() && p.end > p.start && p.end - p.start <= MAX_PROGRAMME_SECONDS
    });
    rows.sort_by_key(|p| (p.start, p.end));
    let mut out: Vec<LiveTvProgramme> = Vec::with_capacity(rows.len());
    for mut row in rows {
        if let Some(previous) = out.last() {
            if row.start < previous.end {
                row.start = previous.end;
            }
            if row.end <= row.start {
                continue;
            }
        }
        out.push(row);
        if out.len() >= GUIDE_MAX_PROGRAMMES_PER_CHANNEL {
            break;
        }
    }
    out
}

pub(crate) fn bounded_text(value: Option<String>, limit: usize) -> Option<String> {
    let value = value?;
    // The contract's limits are BYTES, and the guide host is untrusted: taking
    // `limit` *characters* let a title of 256 CJK or emoji characters through
    // at up to four times the bound it was supposed to enforce, which is a
    // response-size bound as much as a display one. Take whole characters
    // while they fit, so the result is bounded and never split mid-character.
    let mut cleaned = String::new();
    for c in value.chars().filter(|c| !c.is_control()) {
        if cleaned.len() + c.len_utf8() > limit {
            break;
        }
        cleaned.push(c);
    }
    let trimmed = cleaned.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// Programme artwork is passed through to clients, never fetched here. That
/// means the only thing to check is that the URL is one a client can safely
/// put in an `<img src>`: https, no credentials, bounded.
pub(crate) fn bounded_image_url(value: Option<String>) -> Option<String> {
    let value = bounded_text(value, MAX_IMAGE_URL_BYTES)?;
    let url = reqwest::Url::parse(&value).ok()?;
    (url.scheme() == "https" && url.username().is_empty() && url.password().is_none())
        .then_some(value)
}

/// `S03E14` and `3.13.` both mean the same episode. Normalise to `S3E14`, and
/// drop anything that is neither.
pub(crate) fn normalise_episode(value: Option<String>) -> Option<String> {
    let value = bounded_text(value, MAX_EPISODE_BYTES)?;
    let upper = value.to_ascii_uppercase();
    let bytes = upper.as_bytes();
    if bytes.first() == Some(&b'S') {
        let rest = &upper[1..];
        let split = rest.find('E')?;
        let raw_season = &rest[..split];
        let raw_episode = &rest[split + 1..];
        if raw_season.is_empty()
            || raw_episode.is_empty()
            || !raw_season.chars().all(|c| c.is_ascii_digit())
            || !raw_episode.chars().all(|c| c.is_ascii_digit())
        {
            return None;
        }
        // `S00E05` is how every guide source spells a special, and trimming
        // zeros first turned its season into the empty string and dropped the
        // episode number with it. Trim for display, but decide validity on the
        // digits that were actually sent.
        let season = raw_season.trim_start_matches('0');
        let episode = raw_episode.trim_start_matches('0');
        let season = if season.is_empty() { "0" } else { season };
        let episode = if episode.is_empty() { "0" } else { episode };
        return Some(format!("S{season}E{episode}"));
    }
    if bytes.first() == Some(&b'E') {
        let raw = &upper[1..];
        if raw.is_empty() || !raw.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let episode = raw.trim_start_matches('0');
        let episode = if episode.is_empty() { "0" } else { episode };
        return Some(format!("E{episode}"));
    }
    None
}

pub(crate) fn bounded_filters(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .filter_map(|value| bounded_text(Some(value), MAX_FILTER_BYTES))
        .take(MAX_FILTERS)
        .collect()
}

// ---- HDHomeRun source -----------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct HdhrGuideChannel {
    guide_number: Option<String>,
    #[allow(dead_code)]
    guide_name: Option<String>,
    affiliate: Option<String>,
    #[serde(rename = "ImageURL")]
    image_url: Option<String>,
    guide: Option<Vec<HdhrGuideEntry>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct HdhrGuideEntry {
    start_time: Option<i64>,
    end_time: Option<i64>,
    title: Option<String>,
    episode_title: Option<String>,
    episode_number: Option<String>,
    synopsis: Option<String>,
    #[serde(rename = "OriginalAirdate")]
    original_airdate: Option<i64>,
    #[serde(rename = "ImageURL")]
    image_url: Option<String>,
    filter: Option<Vec<String>>,
}

impl HdhrGuideEntry {
    fn into_programme(self) -> Option<LiveTvProgramme> {
        Some(LiveTvProgramme {
            start: self.start_time?,
            end: self.end_time?,
            title: bounded_text(self.title, MAX_TITLE_BYTES)?,
            episode_title: bounded_text(self.episode_title, MAX_EPISODE_TITLE_BYTES),
            episode: normalise_episode(self.episode_number),
            synopsis: bounded_text(self.synopsis, MAX_SYNOPSIS_BYTES),
            image_url: bounded_image_url(self.image_url),
            original_air_date: self.original_airdate.and_then(unix_to_air_date),
            filters: bounded_filters(self.filter.unwrap_or_default()),
        })
    }
}

/// Silicondust sends the original air date as a unix instant; the clients want
/// a plain calendar day. Civil-date arithmetic from days-since-epoch, because
/// pulling in a date crate for one field is not worth the dependency.
pub(crate) fn unix_to_air_date(value: i64) -> Option<String> {
    if !(0..=253_402_300_799).contains(&value) {
        return None;
    }
    let days = value.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    // The civil conversion is only correct for instants inside the range
    // checked above; anything that lands outside a real calendar day is a
    // guide row we drop rather than print. An air date is a fact shown to a
    // person, and "2026-02-31" is worse than no date at all.
    if !(1..=12).contains(&m) || !(1..=i64::from(days_in_month(y, m))).contains(&d) {
        return None;
    }
    Some(format!("{y:04}-{m:02}-{d:02}"))
}

fn days_in_month(year: i64, month: i64) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year.rem_euclid(4) == 0
            && (year.rem_euclid(100) != 0 || year.rem_euclid(400) == 0) =>
        {
            29
        }
        2 => 28,
        _ => 0,
    }
}

/// Read `DeviceAuth` fresh from the device at the moment of each refresh, use
/// it in flight, and forget it. It is never returned from this function, never
/// stored, never logged, and never crosses to another node — the guide
/// *result* is what gets relayed. Reading it every refresh is also why the
/// guide survives Silicondust rotating it.
pub(crate) async fn read_device_auth(
    client: &reqwest::Client,
    address: Ipv4Addr,
) -> Result<String, LiveTvError> {
    let url = pinned_url(address, 80, "/discover.json")?;
    let document =
        fetch_bounded(client, url, super::MAX_DOCUMENT_BYTES, GUIDE_FETCH_TIMEOUT).await?;
    let parsed: DiscoverDocument = serde_json::from_slice(&document).map_err(|_| {
        LiveTvError::InvalidResponse("HDHomeRun discovery document is not valid JSON".to_owned())
    })?;
    parsed
        .device_auth
        .filter(|value| !value.is_empty() && value.len() <= 256)
        .ok_or_else(|| {
            LiveTvError::DeviceUnavailable(
                "this HDHomeRun does not publish a guide credential; use an XMLTV source instead"
                    .to_owned(),
            )
        })
}

pub(crate) fn guide_request_url(
    device_auth: &str,
    channel: Option<&str>,
    start: Option<i64>,
) -> Result<reqwest::Url, LiveTvError> {
    let mut url = reqwest::Url::parse(GUIDE_BULK_URL)
        .map_err(|_| LiveTvError::InvalidConfig("guide URL is not valid".to_owned()))?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("DeviceAuth", device_auth);
        query.append_pair("SynopsisLength", "1024");
        if let Some(channel) = channel {
            query.append_pair("Channel", channel);
        }
        if let Some(start) = start {
            query.append_pair("Start", &start.to_string());
        }
    }
    if !approved_guide_url(&url) {
        return Err(LiveTvError::InvalidConfig(
            "guide host is not on the allowlist".to_owned(),
        ));
    }
    Ok(url)
}

pub(crate) fn parse_hdhomerun_guide(
    body: &[u8],
    lineup: &BTreeMap<String, &LiveTvChannel>,
) -> Result<Vec<LiveTvGuideChannel>, LiveTvError> {
    let rows: Vec<HdhrGuideChannel> = serde_json::from_slice(body).map_err(|_| {
        LiveTvError::InvalidResponse("the guide service returned an unexpected document".to_owned())
    })?;
    let mut out = Vec::new();
    for row in rows {
        let Some(number) = bounded_text(row.guide_number, super::MAX_GUIDE_NUMBER_BYTES) else {
            continue;
        };
        // A guide row for a channel this lineup does not carry is not an
        // error and not a channel: the lineup is the authority on what exists.
        let Some(channel) = lineup.get(number.as_str()) else {
            continue;
        };
        let programmes = normalise_programmes(
            row.guide
                .unwrap_or_default()
                .into_iter()
                .filter_map(HdhrGuideEntry::into_programme)
                .collect(),
        );
        out.push(LiveTvGuideChannel {
            id: channel.id.clone(),
            guide_number: number,
            affiliate: bounded_text(row.affiliate, MAX_AFFILIATE_BYTES),
            image_url: bounded_image_url(row.image_url),
            programmes,
        });
    }
    Ok(out)
}

/// Merge an extension page into a channel that is already present. The
/// extension asks for the same channel further ahead, so the union is
/// re-normalised rather than appended: overlapping repeats are the norm.
pub(crate) fn merge_channel(into: &mut LiveTvGuideChannel, extra: LiveTvGuideChannel) {
    let mut rows = std::mem::take(&mut into.programmes);
    rows.extend(extra.programmes);
    into.programmes = normalise_programmes(rows);
    if into.affiliate.is_none() {
        into.affiliate = extra.affiliate;
    }
    if into.image_url.is_none() {
        into.image_url = extra.image_url;
    }
}

// ---- XMLTV source ---------------------------------------------------------

/// Grabbers write gzipped files and servers gzip on the wire, and the two are
/// not distinguishable from the URL. Sniff the magic bytes rather than
/// trusting an extension or a header.
pub(crate) fn decompress_if_gzip(body: Vec<u8>) -> Result<Vec<u8>, LiveTvError> {
    if body.len() < 2 || body[0] != 0x1f || body[1] != 0x8b {
        return Ok(body);
    }
    let mut decoder = flate2::read::GzDecoder::new(body.as_slice());
    let mut out = Vec::new();
    // Bounded by the same cap as the wire document: a compressed bomb must not
    // become an allocation.
    let mut limited = std::io::Read::take(&mut decoder, GUIDE_MAX_DOCUMENT_BYTES as u64 + 1);
    limited.read_to_end(&mut out).map_err(|_| {
        LiveTvError::InvalidResponse("the XMLTV document is not valid gzip".to_owned())
    })?;
    if out.len() > GUIDE_MAX_DOCUMENT_BYTES {
        return Err(LiveTvError::InvalidResponse(
            "the XMLTV document is larger than the guide cache accepts".to_owned(),
        ));
    }
    Ok(out)
}

struct XmltvChannel {
    display_names: Vec<String>,
    lcn: Option<String>,
}

/// XMLTV time: `YYYYMMDDhhmmss +hhmm`, with the offset optional in the wild.
/// A missing offset is an error for that programme rather than a guess —
/// guessing puts a programme an hour off and nothing in the UI can say so.
pub(crate) fn parse_xmltv_time(value: &str) -> Option<i64> {
    let value = value.trim();
    let (stamp, offset) = match value.split_once(' ') {
        Some((stamp, offset)) => (stamp, Some(offset.trim())),
        None => (value, None),
    };
    if stamp.len() < 12 || !stamp.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let num = |from: usize, to: usize| stamp.get(from..to)?.parse::<i64>().ok();
    let year = num(0, 4)?;
    let month = num(4, 6)?;
    let day = num(6, 8)?;
    let hour = num(8, 10)?;
    let minute = num(10, 12)?;
    let second = if stamp.len() >= 14 { num(12, 14)? } else { 0 };
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    // A missing offset is an error for this row rather than a guess: guessing
    // puts a programme an hour off and nothing in the UI can say so.
    let offset_seconds = parse_utc_offset(offset?)?;
    Some(civil_to_unix(year, month, day, hour, minute, second) - offset_seconds)
}

fn parse_utc_offset(value: &str) -> Option<i64> {
    if value.eq_ignore_ascii_case("Z") || value.eq_ignore_ascii_case("UTC") || value == "GMT" {
        return Some(0);
    }
    let bytes = value.as_bytes();
    if bytes.len() != 5 || !(bytes[0] == b'+' || bytes[0] == b'-') {
        return None;
    }
    if !value[1..].chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let hours = value[1..3].parse::<i64>().ok()?;
    let minutes = value[3..5].parse::<i64>().ok()?;
    if hours > 14 || minutes > 59 {
        return None;
    }
    let magnitude = hours * 3600 + minutes * 60;
    Some(if bytes[0] == b'-' {
        -magnitude
    } else {
        magnitude
    })
}

fn civil_to_unix(year: i64, month: i64, day: i64, hour: i64, minute: i64, second: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    days * 86_400 + hour * 3600 + minute * 60 + second
}

/// Match XMLTV channels to lineup channels by display name, then by `lcn`,
/// then by callsign — first hit wins, in that order. Unmatched XMLTV channels
/// are dropped and unmatched lineup channels simply have no programmes; the
/// operator gets `matched N of M` on the Developer card rather than a mapping
/// UI they did not ask for.
pub(crate) fn parse_xmltv(
    body: &[u8],
    lineup: &[LiveTvChannel],
) -> Result<Vec<LiveTvGuideChannel>, LiveTvError> {
    use quick_xml::events::Event;

    let mut reader = quick_xml::Reader::from_reader(body);
    // Text must NOT be trimmed per event. quick-xml splits an element's
    // character data at every entity and CDATA boundary, so "Tom &amp; Jerry"
    // arrives as three events; trimming each one would glue the fragments
    // together as "Tom&Jerry". The assembled value is trimmed once, below.
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    // Character data for the element currently open, accumulated across every
    // Text / CData / GeneralRef fragment and committed on its End event.
    let mut pending = String::new();

    let mut channels: HashMap<String, XmltvChannel> = HashMap::new();
    let mut programmes: HashMap<String, Vec<LiveTvProgramme>> = HashMap::new();

    let mut channel_id: Option<String> = None;
    let mut programme: Option<(String, LiveTvProgramme)> = None;
    let mut field: Option<String> = None;
    let mut episode_system: Option<String> = None;

    loop {
        match reader.read_event_into(&mut buffer) {
            Err(_) => {
                return Err(LiveTvError::InvalidResponse(
                    "the XMLTV document is not well-formed".to_owned(),
                ))
            }
            Ok(Event::Eof) => break,
            Ok(Event::Start(event)) => {
                let name = String::from_utf8_lossy(event.name().as_ref()).into_owned();
                match name.as_str() {
                    "channel" => {
                        if let Some(id) = attribute(&event, "id") {
                            channels.entry(id.clone()).or_insert(XmltvChannel {
                                display_names: Vec::new(),
                                lcn: None,
                            });
                            channel_id = Some(id);
                        }
                    }
                    "programme" => {
                        let (Some(channel), Some(start), Some(stop)) = (
                            attribute(&event, "channel"),
                            attribute(&event, "start")
                                .as_deref()
                                .and_then(parse_xmltv_time),
                            attribute(&event, "stop")
                                .as_deref()
                                .and_then(parse_xmltv_time),
                        ) else {
                            programme = None;
                            continue;
                        };
                        programme = Some((
                            channel,
                            LiveTvProgramme {
                                start,
                                end: stop,
                                title: String::new(),
                                episode_title: None,
                                episode: None,
                                synopsis: None,
                                image_url: None,
                                original_air_date: None,
                                filters: Vec::new(),
                            },
                        ));
                    }
                    "episode-num" => {
                        episode_system = attribute(&event, "system");
                        field = Some(name);
                    }
                    other => field = Some(other.to_owned()),
                }
                // Whitespace between the parent's tags is not this element's
                // text.
                pending.clear();
            }
            Ok(Event::Empty(event)) => {
                let name = String::from_utf8_lossy(event.name().as_ref()).into_owned();
                if name == "icon" {
                    if let Some((_, row)) = programme.as_mut() {
                        row.image_url = bounded_image_url(attribute(&event, "src"));
                    }
                }
            }
            Ok(Event::Text(text)) => {
                if let Ok(decoded) = text.decode() {
                    let unescaped = quick_xml::escape::unescape(&decoded)
                        .map(|value| value.into_owned())
                        .unwrap_or_else(|_| decoded.into_owned());
                    push_fragment(&mut pending, &unescaped);
                }
            }
            // CDATA is character data too. Reading only Text events meant a
            // title wrapped in CDATA — which grabbers use precisely for titles
            // full of punctuation — produced an empty title and the programme
            // was dropped.
            Ok(Event::CData(data)) => {
                if let Ok(decoded) = data.decode() {
                    push_fragment(&mut pending, &decoded);
                }
            }
            // An entity is its own event, not part of the surrounding text.
            // Dropping these is what turned "Tom &amp; Jerry" into the title
            // "Tom" and, for the fields that take the last fragment, the
            // synopsis " Jerry". Entities are ubiquitous in real grabber
            // output, so this was the common case rather than an edge one.
            Ok(Event::GeneralRef(entity)) => {
                let resolved = match entity.resolve_char_ref() {
                    Ok(Some(c)) => Some(c.to_string()),
                    Ok(None) => entity
                        .decode()
                        .ok()
                        .and_then(|name| named_entity(name.as_ref()).map(str::to_owned)),
                    Err(_) => None,
                };
                if let Some(resolved) = resolved {
                    push_fragment(&mut pending, &resolved);
                }
            }
            Ok(Event::End(event)) => {
                let name = String::from_utf8_lossy(event.name().as_ref()).into_owned();
                // The whole element's character data is now in hand, entities
                // and CDATA included, so this is the only place it is read.
                let value = std::mem::take(&mut pending).trim().to_owned();
                if !value.is_empty() {
                    commit_xmltv_field(
                        field.as_deref(),
                        value,
                        &mut channels,
                        channel_id.as_deref(),
                        programme.as_mut(),
                        episode_system.as_deref(),
                    );
                }
                match name.as_str() {
                    "channel" => channel_id = None,
                    "programme" => {
                        if let Some((channel, row)) = programme.take() {
                            programmes.entry(channel).or_default().push(row);
                        }
                    }
                    _ => {}
                }
                field = None;
                episode_system = None;
            }
            _ => {}
        }
        buffer.clear();
    }

    let mut out = Vec::new();
    for channel in lineup {
        let Some(matched) = match_xmltv_channel(&channels, channel) else {
            continue;
        };
        let Some(rows) = programmes.remove(&matched) else {
            continue;
        };
        out.push(LiveTvGuideChannel {
            id: channel.id.clone(),
            guide_number: channel.guide_number.clone(),
            affiliate: None,
            image_url: None,
            programmes: normalise_programmes(rows),
        });
    }
    Ok(out)
}

fn push_fragment(pending: &mut String, fragment: &str) {
    if pending.len() + fragment.len() <= MAX_XMLTV_FIELD_BYTES {
        pending.push_str(fragment);
    }
}

/// The five entities XML predefines. A document that uses any other named
/// entity has to declare it in a DTD, and plurx does not process DTDs from an
/// untrusted document — an undeclared name resolves to nothing rather than to
/// a guess.
fn named_entity(name: &str) -> Option<&'static str> {
    match name {
        "amp" => Some("&"),
        "lt" => Some("<"),
        "gt" => Some(">"),
        "quot" => Some("\""),
        "apos" => Some("'"),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn commit_xmltv_field(
    field: Option<&str>,
    value: String,
    channels: &mut HashMap<String, XmltvChannel>,
    channel_id: Option<&str>,
    programme: Option<&mut (String, LiveTvProgramme)>,
    episode_system: Option<&str>,
) {
    match (field, programme, channel_id) {
        (Some("display-name"), None, Some(id)) => {
            if let Some(entry) = channels.get_mut(id) {
                entry.display_names.push(value);
            }
        }
        (Some("lcn"), None, Some(id)) => {
            if let Some(entry) = channels.get_mut(id) {
                entry.lcn = Some(value);
            }
        }
        (Some("title"), Some((_, row)), _) if row.title.is_empty() => {
            row.title = bounded_text(Some(value), MAX_TITLE_BYTES).unwrap_or_default();
        }
        (Some("sub-title"), Some((_, row)), _) => {
            row.episode_title = bounded_text(Some(value), MAX_EPISODE_TITLE_BYTES);
        }
        (Some("desc"), Some((_, row)), _) => {
            row.synopsis = bounded_text(Some(value), MAX_SYNOPSIS_BYTES);
        }
        (Some("date"), Some((_, row)), _) => {
            row.original_air_date = xmltv_date(&value);
        }
        (Some("category"), Some((_, row)), _) => {
            if row.filters.len() < MAX_FILTERS {
                if let Some(filter) = bounded_text(Some(value), MAX_FILTER_BYTES) {
                    row.filters.push(filter);
                }
            }
        }
        (Some("episode-num"), Some((_, row)), _) => {
            let candidate = if episode_system == Some("xmltv_ns") {
                xmltv_ns_episode(&value)
            } else {
                normalise_episode(Some(value))
            };
            if row.episode.is_none() {
                row.episode = candidate;
            }
        }
        _ => {}
    }
}

fn attribute(event: &quick_xml::events::BytesStart<'_>, name: &str) -> Option<String> {
    event.attributes().flatten().find_map(|attribute| {
        (attribute.key.as_ref() == name.as_bytes())
            .then(|| String::from_utf8_lossy(&attribute.value).trim().to_owned())
    })
}

fn match_xmltv_channel(
    channels: &HashMap<String, XmltvChannel>,
    channel: &LiveTvChannel,
) -> Option<String> {
    let number = channel.guide_number.as_str();
    let name = channel.guide_name.to_ascii_lowercase();
    let by = |predicate: &dyn Fn(&XmltvChannel) -> bool| -> Option<String> {
        let mut hits = channels
            .iter()
            .filter(|(_, entry)| predicate(entry))
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        hits.sort();
        hits.into_iter().next()
    };
    by(&|entry| entry.display_names.iter().any(|value| value == number))
        .or_else(|| by(&|entry| entry.lcn.as_deref() == Some(number)))
        .or_else(|| {
            by(&|entry| {
                entry
                    .display_names
                    .iter()
                    .any(|value| value.to_ascii_lowercase() == name)
            })
        })
}

/// `2.13.` in the `xmltv_ns` system is zero-based season 2, episode 13 —
/// which every viewer calls S3E14.
pub(crate) fn xmltv_ns_episode(value: &str) -> Option<String> {
    let mut parts = value.split('.');
    let season = parts.next()?.split('/').next()?.trim();
    let episode = parts.next()?.split('/').next()?.trim();
    if episode.is_empty() || !episode.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let episode = episode.parse::<u32>().ok()? + 1;
    if season.is_empty() {
        return Some(format!("E{episode}"));
    }
    if !season.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let season = season.parse::<u32>().ok()? + 1;
    Some(format!("S{season}E{episode}"))
}

fn xmltv_date(value: &str) -> Option<String> {
    let digits = value.trim();
    if digits.len() < 8 || !digits[..8].chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(format!(
        "{}-{}-{}",
        &digits[0..4],
        &digits[4..6],
        &digits[6..8]
    ))
}

// ---- window helpers -------------------------------------------------------

/// The window a refresh tries to fill, and the default window a client that
/// asks for nothing in particular gets. It starts an hour back so the
/// programme that began before the page opened is present — a grid whose first
/// cell is always cut off reads as broken.
pub(crate) fn refresh_window(hours: u8) -> GuideWindow {
    let now = unix_seconds();
    GuideWindow {
        start: now - 3600,
        end: now + i64::from(hours) * 3600,
    }
}

pub(crate) fn sanitize_refresh_error(error: &LiveTvError) -> String {
    sanitize_error(&error.to_string())
}
