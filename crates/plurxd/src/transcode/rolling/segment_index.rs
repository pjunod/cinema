use super::*;

/// One completed, published segment — the unit of playlist, retention, and
/// delivery-frontier accounting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SegmentVisibility {
    Advertised,
    Grace {
        removed_at: Instant,
        serve_until: Instant,
    },
    Deleted,
}

impl SegmentVisibility {
    pub(super) fn is_advertised(self) -> bool {
        matches!(self, Self::Advertised)
    }

    pub(super) fn is_servable(self, now: Instant) -> bool {
        matches!(self, Self::Advertised)
            || matches!(self, Self::Grace { serve_until, .. } if now < serve_until)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct SegmentMeta {
    pub(super) index: i64,
    /// The URI exactly as the playlist wrote it, so the file can be found
    /// without guessing an extension (`.ts` for transcode, `.m4s` for copy).
    pub(super) name: String,
    /// Session-relative bounds, accumulated from `EXTINF`. Never
    /// `index × SEGMENT_SECONDS`: the copy path cannot force keyframes, so its
    /// segments run to the source's GOP and a 2-second target routinely
    /// produces 5- and 10-second segments. Multiplying an index by the target
    /// is a lie on exactly the sessions this accounting exists to bound.
    pub(super) start_ms: i64,
    pub(super) end_ms: i64,
    /// Size on disk, or 0 until it has been measured (or after it is deleted).
    pub(super) bytes: i64,
    /// Playlist removal and physical deletion are separate promises. Grace
    /// objects remain charged and readable through their exact URI even
    /// though new playlist clients can no longer discover them.
    pub(super) visibility: SegmentVisibility,
}

/// What a session has actually published, in media time and bytes.
///
/// This is the second of the three frontiers a session has, and the one that
/// pacing uses. The others are ffmpeg's `out_time` (encoder progress —
/// includes the in-progress `.tmp` segment nobody can fetch) and the client's
/// download frontier. Conflating any two of them produces a plausible number
/// that is wrong in a different way each time.
#[derive(Clone, Debug, Default)]
pub(super) struct SegmentIndex {
    pub(super) segs: Vec<SegmentMeta>,
    /// Monotonic in-memory catalog revision. Attempt fencing separates
    /// producer generations; this separates concurrent observations of the
    /// same attempt so an older rewritten playlist cannot land last.
    pub(super) revision: u64,
}

impl SegmentIndex {
    /// End of the newest completed segment: the pacing clock.
    pub(super) fn produced_playable_end_ms(&self) -> Option<i64> {
        self.segs.last().map(|s| s.end_ms)
    }

    pub(super) fn next_media_sequence(&self) -> i64 {
        self.segs
            .last()
            .map_or(0, |segment| segment.index.saturating_add(1).max(0))
    }

    /// Where a given segment ends, for turning "the client fetched segment N"
    /// into a position on the media timeline.
    pub(super) fn end_ms_of(&self, index: i64) -> Option<i64> {
        self.segs
            .iter()
            .find(|s| s.index == index)
            .map(|s| s.end_ms)
    }

    /// The complete session-relative window for one segment. Pruned entries
    /// stay in the index precisely so internal timelines (notably native
    /// subtitles) do not forget how much media preceded the served window.
    pub(super) fn window_ms_of(&self, index: i64) -> Option<(i64, i64)> {
        self.segs
            .iter()
            .find(|s| s.index == index)
            .map(|s| (s.start_ms, s.end_ms))
    }

    /// First segment whose bytes are still available to a playlist client.
    pub(super) fn first_retained_index(&self) -> Option<i64> {
        self.segs
            .iter()
            .find(|s| s.visibility.is_advertised())
            .map(|s| s.index)
    }

    /// Complete retained media contiguous with an absolute film-time anchor.
    ///
    /// `published_end_ms` and every segment bound in this index are relative
    /// to the generation's achieved origin. Applying the origin here, once,
    /// prevents status clients from guessing which timeline a frontier uses.
    pub(super) fn server_ready(&self, media_origin_ms: i64, anchor_ms: i64) -> ReadyCoverage {
        let relative_anchor = anchor_ms.saturating_sub(media_origin_ms);
        let containing = self.segs.iter().position(|segment| {
            segment.start_ms <= relative_anchor && relative_anchor < segment.end_ms
        });
        let retained_interval_after = |floor_ms: i64, inclusive: bool| {
            self.segs
                .iter()
                .position(|segment| {
                    segment.visibility.is_advertised()
                        && if inclusive {
                            segment.start_ms >= floor_ms
                        } else {
                            segment.start_ms > floor_ms
                        }
                })
                .map(|start| {
                    let first = &self.segs[start];
                    let mut end_ms = first.end_ms;
                    for segment in self.segs.iter().skip(start + 1) {
                        if !segment.visibility.is_advertised() || segment.start_ms > end_ms {
                            break;
                        }
                        end_ms = end_ms.max(segment.end_ms);
                    }
                    (
                        media_origin_ms.saturating_add(first.start_ms),
                        media_origin_ms.saturating_add(end_ms),
                    )
                })
        };
        let Some(start) = containing else {
            let known_missing = relative_anchor >= 0
                && self
                    .produced_playable_end_ms()
                    .is_none_or(|end| relative_anchor >= end);
            return ReadyCoverage::without_anchor(
                anchor_ms,
                if known_missing {
                    "missing"
                } else {
                    "unavailable"
                },
                retained_interval_after(relative_anchor, false),
            );
        };
        if !self.segs[start].visibility.is_advertised() {
            return ReadyCoverage::without_anchor(
                anchor_ms,
                "unavailable",
                retained_interval_after(relative_anchor, false),
            );
        }
        let mut end_ms = self.segs[start].end_ms;
        for segment in self.segs.iter().skip(start + 1) {
            if !segment.visibility.is_advertised() || segment.start_ms > end_ms {
                break;
            }
            end_ms = end_ms.max(segment.end_ms);
        }
        let absolute_end_ms = media_origin_ms.saturating_add(end_ms);
        let next_interval = retained_interval_after(end_ms, true);
        ReadyCoverage {
            state: "ready",
            anchor_ms: Some(anchor_ms),
            end_ms: Some(absolute_end_ms),
            seconds: Some(absolute_end_ms.saturating_sub(anchor_ms) as f64 / 1_000.0),
            next_start_ms: next_interval.map(|(start, _)| start),
            next_end_ms: next_interval.map(|(_, end)| end),
        }
    }

    /// Bytes of published segments lying entirely after `ms`.
    pub(super) fn bytes_after_ms(&self, ms: i64) -> i64 {
        self.segs
            .iter()
            .filter(|s| s.start_ms >= ms)
            .map(|s| s.bytes)
            .sum()
    }

    /// Every byte still on disk, wherever the frontier is. Pruned segments
    /// carry zero, so this is what the session actually occupies.
    pub(super) fn total_bytes(&self) -> i64 {
        self.segs.iter().map(|s| s.bytes).sum()
    }

    pub(super) fn advertised_bytes(&self) -> i64 {
        self.segs
            .iter()
            .filter(|segment| segment.visibility.is_advertised())
            .map(|segment| segment.bytes)
            .sum()
    }

    pub(super) fn grace_bytes(&self) -> i64 {
        self.segs
            .iter()
            .filter(|segment| matches!(segment.visibility, SegmentVisibility::Grace { .. }))
            .map(|segment| segment.bytes)
            .sum()
    }

    /// Segments old enough to delete: those that END before the retention
    /// window opens. A segment straddling the boundary is kept — half a
    /// segment is no use to anyone and the arithmetic is cheap.
    pub(super) fn prunable(&self, keep_from_ms: i64) -> impl Iterator<Item = &SegmentMeta> {
        self.segs.iter().filter(move |s| {
            s.visibility.is_advertised() && s.bytes > 0 && s.end_ms <= keep_from_ms
        })
    }

    /// Bring the index up to date with the playlist text by *appending* what
    /// is newly published, and only that. Returns true when it had to rebuild
    /// instead.
    ///
    /// This replaced re-parsing the complete growing EVENT playlist on every
    /// refresh — reconstructing every prior entry, round-tripping every known
    /// size through a map — which approached quadratic work over a long
    /// session on exactly the hot path that runs on every segment publish
    /// (review §2.6). An EVENT playlist may not mutate published entries, so
    /// known ordinals are counted and skipped without so much as a float
    /// parse; sizes and prune flags stay where they are.
    ///
    /// Two things do force a rebuild, both real: the playlist shrank
    /// (truncation, recovery), or its content disagrees with what is held —
    /// the fallback respawn clears the directory and rewrites the timeline
    /// from the same seek point, reusing the same names, so the sentinel is
    /// the last *known* entry's duration and index rather than the count.
    /// A rebuild drops carried sizes on purpose: they described files a
    /// replaced timeline no longer contains.
    #[cfg(test)]
    pub(super) fn extend_from_playlist(&mut self, text: &str) -> bool {
        let known = self.segs.len();
        let mut seen = 0usize;
        let mut pending: Option<&str> = None;
        let mut cursor_ms = self.segs.last().map(|s| s.end_ms).unwrap_or(0);
        let mut disagreed = false;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(rest) = line.strip_prefix("#EXTINF:") {
                pending = Some(rest);
                continue;
            }
            if line.starts_with('#') {
                continue;
            }
            let (Some(ext), Some(index)) = (pending.take(), segment_index(line)) else {
                continue;
            };
            seen += 1;
            if seen <= known {
                let held = &self.segs[seen - 1];
                // The cheap sentinel on every known ordinal is the index; the
                // one duration parse per refresh is spent on the last known
                // entry, where a rewritten timeline's different cut point
                // shows first.
                if held.index != index
                    || (seen == known
                        && extinf_ms(ext).is_some_and(|d| d != held.end_ms - held.start_ms))
                {
                    disagreed = true;
                    break;
                }
                continue;
            }
            let Some(duration_ms) = extinf_ms(ext) else {
                continue;
            };
            self.segs.push(SegmentMeta {
                index,
                name: line.to_owned(),
                start_ms: cursor_ms,
                end_ms: cursor_ms + duration_ms,
                bytes: 0,
                visibility: SegmentVisibility::Advertised,
            });
            cursor_ms += duration_ms;
        }
        if disagreed || seen < known {
            self.segs = parse_playlist(text);
            return true;
        }
        false
    }

    /// Apply an already parsed and measured playlist without doing storage
    /// work while the producer-transition gate is held. An exact shorter
    /// prefix is an older concurrent read, not evidence that a newer index
    /// should move backward. A disagreement is a real same-attempt rewrite.
    pub(super) fn merge_prepared(
        &mut self,
        expected_revision: u64,
        mut observed: SegmentIndex,
    ) -> Option<bool> {
        if self.revision != expected_revision {
            return None;
        }
        let overlap = self.segs.len().min(observed.segs.len());
        let disagreed =
            self.segs
                .iter()
                .zip(&observed.segs)
                .take(overlap)
                .any(|(current, observed)| {
                    current.index != observed.index
                        || current.name != observed.name
                        || current.start_ms != observed.start_ms
                        || current.end_ms != observed.end_ms
                });
        if disagreed {
            self.segs = observed.segs;
            self.revision = self.revision.wrapping_add(1);
            return Some(true);
        }
        for (current, observed) in self.segs.iter_mut().zip(observed.segs.iter()).take(overlap) {
            if current.visibility.is_advertised() && current.bytes == 0 && observed.bytes > 0 {
                current.bytes = observed.bytes;
            }
        }
        let known = self.segs.len();
        if observed.segs.len() > known {
            self.segs.extend(observed.segs.drain(known..));
        }
        self.revision = self.revision.wrapping_add(1);
        Some(false)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ReadyCoverage {
    pub(super) state: &'static str,
    pub(super) anchor_ms: Option<i64>,
    pub(super) end_ms: Option<i64>,
    pub(super) seconds: Option<f64>,
    pub(super) next_start_ms: Option<i64>,
    pub(super) next_end_ms: Option<i64>,
}

impl ReadyCoverage {
    pub(super) fn unavailable() -> Self {
        Self {
            state: "unavailable",
            anchor_ms: None,
            end_ms: None,
            seconds: None,
            next_start_ms: None,
            next_end_ms: None,
        }
    }

    pub(super) fn without_anchor(
        anchor_ms: i64,
        state: &'static str,
        next_interval: Option<(i64, i64)>,
    ) -> Self {
        Self {
            state,
            anchor_ms: Some(anchor_ms),
            end_ms: (state == "missing").then_some(anchor_ms),
            seconds: (state == "missing").then_some(0.0),
            next_start_ms: next_interval.map(|(start, _)| start),
            next_end_ms: next_interval.map(|(_, end)| end),
        }
    }
}

/// Parse `EXTINF` durations and segment URIs out of an HLS media playlist.
///
/// Pure, and the reason the copy path's variable segment lengths stop being a
/// guess: the playlist is the only place the true duration of a copied segment
/// is written down.
pub(super) fn extinf_ms(rest: &str) -> Option<i64> {
    rest.split(',')
        .next()
        .and_then(|d| d.trim().parse::<f64>().ok())
        .filter(|d| *d >= 0.0)
        .map(|secs| (secs * 1000.0).round() as i64)
}

pub(super) fn parse_playlist(text: &str) -> Vec<SegmentMeta> {
    let mut out: Vec<SegmentMeta> = Vec::new();
    let mut pending_ms: Option<i64> = None;
    let mut cursor_ms = 0i64;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            pending_ms = extinf_ms(rest);
            continue;
        }
        // Every other tag, including `#EXT-X-MAP:URI="init.mp4"`, which names
        // a file that is not a segment and carries no duration.
        if line.starts_with('#') {
            continue;
        }
        // A URI line counts only when an EXTINF introduced it.
        let (Some(duration_ms), Some(index)) = (pending_ms.take(), segment_index(line)) else {
            continue;
        };
        out.push(SegmentMeta {
            index,
            name: line.to_owned(),
            start_ms: cursor_ms,
            end_ms: cursor_ms + duration_ms,
            bytes: 0,
            visibility: SegmentVisibility::Advertised,
        });
        cursor_ms += duration_ms;
    }
    out
}

/// Whether the first HTTP response for a live transcode can safely expose this
/// EVENT playlist. Later reloads never pass through this verdict: the session
/// remembers that publication opened.
pub(super) fn transcode_first_playlist_ready(raw: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(raw) else {
        // Preserve the old fail-fast behavior for malformed ffmpeg output. A
        // media player can report the parse error; hiding it behind a 30-second
        // wait would turn a useful failure into a gray screen.
        return true;
    };
    if text.lines().any(|line| line.trim() == "#EXT-X-ENDLIST") {
        return true;
    }
    let segments = parse_playlist(text);
    segments.len() >= 2
        && segments
            .last()
            .is_some_and(|segment| segment.end_ms >= TRANSCODE_START_CUSHION_MS)
}

/// Prove that a raw rolling-writer revision fits the presentation target that
/// the serving adapter freezes into every response.
///
/// FFmpeg is allowed to rewrite its private target as it learns about later
/// segments. Clients are not: once a target has crossed the response boundary
/// it is part of the presentation contract. Validate the actual durations,
/// then replace the private tag in [`served_live_playlist`]. Cached immutable
/// VOD never passes through either function.
pub(super) fn validate_rolling_target(raw: &[u8]) -> Result<(), String> {
    let text = std::str::from_utf8(raw)
        .map_err(|_| "rolling HLS playlist was not valid UTF-8".to_owned())?;
    let mut target_tags = 0usize;
    let mut durations = 0usize;
    for line in text.lines().map(str::trim) {
        if let Some(value) = line.strip_prefix("#EXT-X-TARGETDURATION:") {
            let _ = value
                .parse::<u32>()
                .ok()
                .filter(|value| *value > 0)
                .ok_or_else(|| "rolling HLS playlist had an invalid target duration".to_owned())?;
            target_tags += 1;
        } else if let Some(value) = line.strip_prefix("#EXTINF:") {
            let seconds = value
                .split(',')
                .next()
                .and_then(|value| value.trim().parse::<f64>().ok())
                .filter(|value| value.is_finite() && *value > 0.0)
                .ok_or_else(|| "rolling HLS playlist had an invalid EXTINF".to_owned())?;
            if seconds > f64::from(plurx_core::transcode::ROLLING_PRESENTATION_TARGET_SECS) {
                return Err(format!(
                    "rolling HLS segment duration {seconds:.6}s exceeded the fixed {}s presentation target",
                    plurx_core::transcode::ROLLING_PRESENTATION_TARGET_SECS,
                ));
            }
            durations += 1;
        }
    }
    if target_tags != 1 {
        return Err(format!(
            "rolling HLS playlist had {target_tags} target-duration tags instead of one"
        ));
    }
    if durations == 0 {
        return Err("rolling HLS playlist had no complete segment durations".to_owned());
    }
    Ok(())
}

/// Turn the append-only writer playlist into the sliding view clients see.
///
/// FFmpeg and the copy segmenter keep an EVENT playlist on disk because the
/// segment index needs the complete duration history. Retention eventually
/// unlinks a prefix of those segment files. Serving that raw EVENT playlist
/// after the unlink advertises media that no longer exists; AVPlayer may ask
/// for one of those stale URIs during a playlist reload or decoder reset and
/// stall even though the current encoder is healthy.
///
/// Every rolling session serves a typeless sliding shape from its first
/// response, with an explicit zero start offset. Retention is part of this
/// presentation's lifetime contract, so EVENT is never a valid served shape.
/// The raw file and in-memory index retain the complete writer history.
pub(super) fn served_live_playlist(
    raw: Vec<u8>,
    first_retained: Option<i64>,
    last_retained: Option<i64>,
    takeover: Option<&SessionTakeoverStart>,
) -> Option<Vec<u8>> {
    // A successor's numbering starts at its epoch floor, not at zero, so the
    // "nothing has been pruned yet" baseline is that floor.
    let baseline = takeover.map_or(0, |takeover| takeover.media_sequence);
    let first_retained = first_retained
        .filter(|index| *index > baseline)
        .unwrap_or(baseline);
    let text = std::str::from_utf8(&raw).ok()?;
    let lines: Vec<&str> = text.lines().collect();
    let header_end = lines
        .iter()
        .position(|line| line.trim_start().starts_with("#EXTINF:"))?;

    let body_start = if first_retained == 0 {
        header_end
    } else {
        // Start immediately after the prior segment URI. This retains any tags
        // attached to the first surviving segment rather than assuming EXTINF
        // is always the first line in its block.
        let mut next_block = header_end;
        let mut body_start = None;
        for (position, line) in lines.iter().enumerate().skip(header_end) {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if segment_index(line) == Some(first_retained) {
                body_start = Some(next_block);
                break;
            }
            if segment_index(line).is_some() {
                next_block = position + 1;
            }
        }
        // A concurrent prune or producer replacement can disagree with this
        // writer snapshot. Retry within the existing request budget; never
        // publish the raw EVENT history or guess a different media sequence.
        body_start?
    };
    let body_end = last_retained.map_or(lines.len(), |last_retained| {
        lines
            .iter()
            .enumerate()
            .skip(body_start)
            .find_map(|(position, line)| {
                (segment_index(line.trim()) == Some(last_retained)).then_some(position + 1)
            })
            .unwrap_or(body_start)
    });
    if body_end <= body_start {
        return None;
    }

    let mut out = String::with_capacity(text.len());
    let mut wrote_media_sequence = false;
    let mut wrote_discontinuity_sequence = false;
    let mut wrote_start = false;
    for line in &lines[..header_end] {
        let trimmed = line.trim();
        if trimmed.starts_with("#EXT-X-PLAYLIST-TYPE:") {
            continue;
        }
        if trimmed.starts_with("#EXT-X-START:") {
            wrote_start = true;
        }
        if trimmed.starts_with("#EXT-X-TARGETDURATION:") {
            out.push_str(&format!(
                "#EXT-X-TARGETDURATION:{}\n",
                plurx_core::transcode::ROLLING_PRESENTATION_TARGET_SECS
            ));
        } else if trimmed.starts_with("#EXT-X-MEDIA-SEQUENCE:") {
            out.push_str(&format!("#EXT-X-MEDIA-SEQUENCE:{first_retained}\n"));
            wrote_media_sequence = true;
        } else if trimmed.starts_with("#EXT-X-DISCONTINUITY-SEQUENCE:") {
            if let Some(takeover) = takeover {
                let includes_boundary = first_retained <= takeover.media_sequence;
                let sequence = takeover
                    .discontinuity_sequence
                    .saturating_sub(i64::from(includes_boundary));
                out.push_str(&format!("#EXT-X-DISCONTINUITY-SEQUENCE:{sequence}\n"));
                wrote_discontinuity_sequence = true;
            } else {
                out.push_str(line);
                out.push('\n');
            }
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    if !wrote_media_sequence {
        out.push_str(&format!("#EXT-X-MEDIA-SEQUENCE:{first_retained}\n"));
    }
    if let Some(takeover) = takeover {
        let includes_boundary = first_retained <= takeover.media_sequence;
        if !wrote_discontinuity_sequence {
            let sequence = takeover
                .discontinuity_sequence
                .saturating_sub(i64::from(includes_boundary));
            out.push_str(&format!("#EXT-X-DISCONTINUITY-SEQUENCE:{sequence}\n"));
        }
        if includes_boundary {
            out.push_str("#EXT-X-DISCONTINUITY\n");
        }
    }
    if !wrote_start {
        out.push_str("#EXT-X-START:TIME-OFFSET=0\n");
    }
    for line in &lines[body_start..body_end] {
        out.push_str(line);
        out.push('\n');
    }
    Some(out.into_bytes())
}
