//! A title's F+ fit, from the refit helper's per-scene statistics
//! (`docs/streaming/FEL-RPU-REFIT-BUILD.md` §3.1, §3.3–3.4).
//!
//! The helper (`segment_decode_render --refit-stats SOURCE VIDEO_INDEX`)
//! prints one [`RefitScene`] line per pooled scene in presentation order,
//! then one [`RefitSummary`] line. [`parse`] reads exactly that through the
//! typed lines and [`check_refit_run`], and refuses anything truncated or
//! inconsistent. An interrupted run resumes at a scene boundary:
//! [`complete_scenes`] keeps the whole scene lines it wrote, [`resume_point`]
//! names the start of the last of them (which the resumed run writes again),
//! and [`parse_resumed`] joins the resumed run to the scenes kept before it,
//! summarising the whole title with [`summarise`] as an uninterrupted run
//! would. Pooling short scenes is the helper's business; this consumes its
//! scenes as they are.
//!
//! [`fit`] solves and guards each scene with [`super::dv_refit`] and decides
//! the title: identity for a degenerate title, for a title none of whose
//! scenes passed, and for one whose pooled held-out gain is under
//! [`MIN_TITLE_GAIN`]. Served curves are carried as the quantised integers
//! [`super::dv_refit::apply_to_rpu`] writes, and the curve digest is taken
//! over exactly those integers, so it names the RPU bits a session serves.
use super::dv_refit::{
    check_refit_run, held_out_verdict, servable, summarise, QuantisedCurves, RefitCurves,
    RefitScene, RefitSummary, ServableCurves, FIT_VERSION,
};
use crate::store::dv_refit_jobs::{
    valid_time_base, RefitSceneRow, RefitTitle, RefitVerdict, MAX_REASON_BYTES, MAX_SCENES,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

/// A title whose served curves remove less than this share of the identity
/// curves' held-out squared error keeps identity everywhere (§3.4).
pub const MIN_TITLE_GAIN: f64 = 0.05;

/// A complete, checked helper run.
#[derive(Debug, Clone, PartialEq)]
pub struct HelperOutput {
    pub scenes: Vec<RefitScene>,
    pub summary: RefitSummary,
}

/// Scene lines then the summary line, each typed and the run checked.
fn lines(stdout: &[u8]) -> Result<HelperOutput, String> {
    let text = std::str::from_utf8(stdout).map_err(|_| "helper output is not UTF-8")?;
    let text = text.strip_suffix('\n').unwrap_or(text);
    let lines: Vec<&str> = text.split('\n').collect();
    if lines.len() > MAX_SCENES + 1 {
        return Err(format!("the helper reported more than {MAX_SCENES} scenes"));
    }
    let (last, scene_lines) = lines.split_last().ok_or("helper output is empty")?;
    let summary = RefitSummary::from_line(last).map_err(|error| {
        if RefitScene::from_line(last).is_ok() {
            "helper output has no summary: it is truncated".to_owned()
        } else {
            format!("helper summary (line {}): {error}", lines.len())
        }
    })?;
    let scenes = scene_lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            RefitScene::from_line(line)
                .map_err(|error| format!("helper line {}: {error}", index + 1))
        })
        .collect::<Result<Vec<_>, _>>()?;
    check_refit_run(&scenes, &summary).map_err(|error| error.to_string())?;
    Ok(HelperOutput { scenes, summary })
}

fn storable(output: HelperOutput) -> Result<HelperOutput, String> {
    if output.scenes[0].first_frame != 0 {
        return Err("the helper run does not start at the title's first frame".into());
    }
    if output.scenes.len() > MAX_SCENES {
        return Err(format!("the helper reported more than {MAX_SCENES} scenes"));
    }
    if !valid_time_base(&output.summary.time_base) {
        return Err(format!(
            "time base {:?} is not storable",
            output.summary.time_base
        ));
    }
    if output.summary.frames > i64::MAX as u64 {
        return Err("frame count out of range".into());
    }
    Ok(output)
}

/// Parses the helper's complete stdout: scene lines, then the summary as the
/// last line (a trailing newline is optional), the whole run checked by
/// [`check_refit_run`] and starting at the title's first frame.
pub fn parse(stdout: &[u8]) -> Result<HelperOutput, String> {
    storable(lines(stdout)?)
}

/// The whole scene lines an interrupted run wrote, in order, as long as they
/// are contiguous from the title's first frame: a torn last line, a summary
/// or anything after the first line that does not continue the title ends
/// the prefix. Each scene comes with its line, byte for byte.
pub fn complete_scenes(stdout: &[u8]) -> Vec<(String, RefitScene)> {
    continue_scenes(None, stdout)
}

/// [`complete_scenes`] for a resumed run's stdout: its first whole line must
/// continue `after` (the last scene kept before it) instead of starting the
/// title.
pub fn continue_scenes(after: Option<&RefitScene>, stdout: &[u8]) -> Vec<(String, RefitScene)> {
    let mut kept: Vec<(String, RefitScene)> = Vec::new();
    let whole = match stdout.iter().rposition(|&b| b == b'\n') {
        Some(end) => &stdout[..end],
        None => return kept,
    };
    let Ok(text) = std::str::from_utf8(whole) else {
        return kept;
    };
    for line in text.split('\n') {
        let Ok(scene) = RefitScene::from_line(line) else {
            break;
        };
        let continues = match kept.last().map(|(_, prev)| prev).or(after) {
            None => scene.first_frame == 0,
            Some(prev) => {
                scene.first_frame == prev.first_frame + prev.frames
                    && scene.first_pts > prev.first_pts
                    && scene.time_base == prev.time_base
            }
        };
        if !continues || kept.len() >= MAX_SCENES {
            break;
        }
        kept.push((line.to_owned(), scene));
    }
    kept
}

/// Where a resumed run starts: a scene's first frame and presentation time,
/// as `segment_decode_render --refit-stats … --resume FIRST FIRST_PTS` takes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResumePoint {
    pub first_frame: u64,
    pub first_pts: i64,
}

/// The start of the last kept scene, and how many scenes before it stand.
/// The last kept scene is written again by the resumed run, because it is
/// the only boundary known to be a pooled scene's start (the helper resumes
/// only there, which is what makes the resumed scenes equal an uninterrupted
/// run's). `None` when nothing before it would be kept: start over.
pub fn resume_point(scenes: &[RefitScene]) -> Option<(usize, ResumePoint)> {
    let last = scenes.last()?;
    (scenes.len() >= 2 && last.first_frame > 0).then_some((
        scenes.len() - 1,
        ResumePoint {
            first_frame: last.first_frame,
            first_pts: last.first_pts,
        },
    ))
}

/// A resumed run's complete stdout, checked on its own (its summary covers
/// only the frames it decoded), started exactly at `at`, and joined to the
/// `prefix` scenes kept before it. The joined title is summarised as an
/// uninterrupted run would have summarised it.
pub fn parse_resumed(
    prefix: Vec<RefitScene>,
    at: ResumePoint,
    stdout: &[u8],
) -> Result<HelperOutput, String> {
    let resumed = lines(stdout)?;
    let first = &resumed.scenes[0];
    if first.first_frame != at.first_frame || first.first_pts != at.first_pts {
        return Err("the resumed run does not start at its resume point".into());
    }
    let mut scenes = prefix;
    scenes.extend(resumed.scenes);
    let summary = summarise(&scenes).map_err(|error| format!("resumed title: {error}"))?;
    storable(HelperOutput { scenes, summary })
}

/// The helper's stdout an uninterrupted run would have written for
/// `output`: its scene lines as given, then the summary.
pub fn spool_text(scene_lines: &[String], summary: &RefitSummary) -> Result<String, String> {
    let mut text = scene_lines.join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(&serde_json::to_string(summary).map_err(|e| e.to_string())?);
    text.push('\n');
    Ok(text)
}

/// One scene's outcome.
#[derive(Debug, Clone, PartialEq)]
pub struct SceneFit {
    pub scene: RefitScene,
    /// Curves that passed every guard, fit domain.
    pub curves: Option<RefitCurves>,
    /// The served coefficients: present only when the scene's curves passed
    /// and the title serves fitted curves.
    pub served: Option<QuantisedCurves>,
    /// Why the scene keeps identity curves on its own account.
    pub reason: Option<String>,
    /// Held-out squared error of the identity curves, in 12-bit codes².
    pub identity_error: f64,
    /// Held-out squared error of the scene's own curves, when they passed.
    pub fitted_error: Option<f64>,
    /// 1 − fitted/identity on the held-out samples, when the curves passed.
    pub gain: Option<f64>,
}

impl SceneFit {
    /// What the scene serves on its own: its curves' error, or identity's.
    fn served_error(&self) -> f64 {
        self.fitted_error.unwrap_or(self.identity_error)
    }
}

/// A title's outcome.
#[derive(Debug, Clone, PartialEq)]
pub struct TitleFit {
    pub scenes: Vec<SceneFit>,
    pub verdict: RefitVerdict,
    pub reason: Option<String>,
    pub gain: Option<f64>,
    pub curve_digest: String,
    pub frames: i64,
}

/// The guards the stored curves are checked against when they are read
/// ([`ServableCurves::from_quantised`]) are applied here too, so a scene is
/// served only if its stored integers will load.
fn quantised(curves: &RefitCurves) -> Result<QuantisedCurves, String> {
    let q = servable(curves).map_err(|e| e.to_string())?.quantised();
    ServableCurves::from_quantised(&q).map_err(|e| format!("as stored: {e}"))?;
    Ok(q)
}

fn solve_scene(scene: RefitScene) -> (SceneFit, Option<QuantisedCurves>) {
    let identity_error = scene
        .held_out
        .squared_error(&RefitCurves::identity())
        .map_or(0.0, |error| error.total());
    let outcome = scene
        .fit
        .solve()
        .map_err(|error| format!("no fit: {error}"))
        .and_then(|curves| {
            let q = quantised(&curves)?;
            let verdict =
                held_out_verdict(&curves, &scene.held_out).map_err(|error| error.to_string())?;
            Ok((curves, q, verdict))
        });
    match outcome {
        Ok((curves, q, verdict)) => (
            SceneFit {
                fitted_error: Some(verdict.fitted.total()),
                gain: Some(verdict.gain),
                curves: Some(curves),
                served: None,
                reason: None,
                identity_error,
                scene,
            },
            Some(q),
        ),
        Err(reason) => (
            SceneFit {
                fitted_error: None,
                gain: None,
                curves: None,
                served: None,
                reason: Some(reason),
                identity_error,
                scene,
            },
            None,
        ),
    }
}

/// Solves and judges every scene, then the title.
pub fn fit(output: HelperOutput) -> TitleFit {
    let frames = output.summary.frames as i64;
    let degenerate = output.summary.degenerate;
    let (mut scenes, passed): (Vec<SceneFit>, Vec<Option<QuantisedCurves>>) =
        output.scenes.into_iter().map(solve_scene).unzip();
    let identity: f64 = scenes.iter().map(|s| s.identity_error).sum();
    let served: f64 = scenes.iter().map(SceneFit::served_error).sum();
    let gain = (identity > 0.0).then(|| 1.0 - served / identity);
    let fitted = passed.iter().filter(|q| q.is_some()).count();
    let (verdict, reason) = if degenerate {
        (
            RefitVerdict::Identity,
            Some(format!(
                "the RPU's scenes are a median {:.0} frames: its scene_refresh_flag does not mark cuts",
                output.summary.median_scene_frames
            )),
        )
    } else if fitted == 0 {
        (
            RefitVerdict::Identity,
            Some(format!(
                "none of the title's {} scenes passed the guards",
                scenes.len()
            )),
        )
    } else if gain.is_none_or(|gain| gain < MIN_TITLE_GAIN) {
        (
            RefitVerdict::Identity,
            Some(format!(
                "the fitted curves remove {:.1}% of the identity curves' held-out error, under the {:.0}% floor",
                gain.unwrap_or(0.0) * 100.0,
                MIN_TITLE_GAIN * 100.0
            )),
        )
    } else {
        (RefitVerdict::Fitted, None)
    };
    if verdict == RefitVerdict::Fitted {
        for (scene, q) in scenes.iter_mut().zip(passed) {
            scene.served = q;
        }
    }
    let curve_digest = curve_digest(scenes.iter().map(|s| DigestScene {
        first_frame: s.scene.first_frame,
        frames: s.scene.frames,
        first_pts: s.scene.first_pts,
        end_pts: s.scene.end_pts,
        time_base: &s.scene.time_base,
        coefficients: s.served.as_ref(),
    }));
    TitleFit {
        scenes,
        verdict,
        reason,
        gain,
        curve_digest,
        frames,
    }
}

/// One scene as the curve digest covers it: where it presents and the
/// integers it serves (`None`: identity).
#[derive(Debug, Clone, Serialize)]
pub struct DigestScene<'a> {
    pub first_frame: u64,
    pub frames: u64,
    pub first_pts: i64,
    pub end_pts: i64,
    pub time_base: &'a str,
    pub coefficients: Option<&'a QuantisedCurves>,
}

/// sha256 over each scene's span and served quantised coefficients, in
/// presentation order: the name of the curve set a session pins. Integers
/// only, so the same curves give the same digest on every node and after
/// every round trip through a store. The fit method is not part of it: the
/// same integers from a later fit version are the same served curves.
pub fn curve_digest<'a>(scenes: impl IntoIterator<Item = DigestScene<'a>>) -> String {
    let scenes: Vec<DigestScene<'a>> = scenes.into_iter().collect();
    hex::encode(Sha256::digest(
        serde_json::to_vec(&("dv-refit-curves-v2", scenes)).expect("curve digest"),
    ))
}

/// The digest of a title with no scenes (a refusal).
pub fn empty_digest() -> String {
    curve_digest(std::iter::empty())
}

/// What a fit was made from, as a title row records it (§3.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefitSource {
    pub file_id: i64,
    pub source_size: i64,
    pub source_mtime: i64,
    pub video_index: i64,
    pub probe_revision: i64,
    pub helper_identity: String,
}

impl TitleFit {
    /// The rows publication stores: the title and its scenes, in order.
    pub fn rows(&self, source: &RefitSource) -> Result<(RefitTitle, Vec<RefitSceneRow>), String> {
        let count = |value: u64| i64::try_from(value).map_err(|_| "count out of range".to_owned());
        let scenes = self
            .scenes
            .iter()
            .map(|s| -> Result<RefitSceneRow, String> {
                Ok(RefitSceneRow {
                    first_frame: count(s.scene.first_frame)?,
                    frames: count(s.scene.frames)?,
                    first_pts: s.scene.first_pts,
                    end_pts: s.scene.end_pts,
                    time_base: s.scene.time_base.clone(),
                    coefficients: s.served.clone(),
                    reason: s.reason.as_deref().map(bounded_reason),
                    identity_error: s.identity_error,
                    fitted_error: s.fitted_error,
                    gain: s.gain,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let title = RefitTitle {
            file_id: source.file_id,
            source_size: source.source_size,
            source_mtime: source.source_mtime,
            video_index: source.video_index,
            probe_revision: source.probe_revision,
            helper_identity: source.helper_identity.clone(),
            fit_version: FIT_VERSION.into(),
            curve_digest: self.curve_digest.clone(),
            verdict: self.verdict,
            reason: self.reason.as_deref().map(bounded_reason),
            gain: self.gain,
            scene_count: scenes.len() as i64,
            frames: self.frames,
        };
        Ok((title, scenes))
    }
}

/// One line without control characters, at most [`MAX_REASON_BYTES`].
pub fn bounded_reason(reason: &str) -> String {
    let mut reason: String = reason
        .trim()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if reason.is_empty() {
        reason = "the helper refused the title without a reason".into();
    }
    while reason.len() > MAX_REASON_BYTES {
        reason.pop();
    }
    reason
}

/// The title row for a helper refusal (for example a full-resolution EL, or
/// a stream that is not FEL): no scenes, identity everywhere, its one stderr
/// line as the reason.
pub fn refusal(source: &RefitSource, reason: &str) -> RefitTitle {
    let reason = reason.trim();
    let reason = reason.strip_prefix("Segment refused: ").unwrap_or(reason);
    RefitTitle {
        file_id: source.file_id,
        source_size: source.source_size,
        source_mtime: source.source_mtime,
        video_index: source.video_index,
        probe_revision: source.probe_revision,
        helper_identity: source.helper_identity.clone(),
        fit_version: FIT_VERSION.into(),
        curve_digest: empty_digest(),
        verdict: RefitVerdict::Refused,
        reason: Some(bounded_reason(reason)),
        gain: None,
        scene_count: 0,
        frames: 0,
    }
}

#[cfg(test)]
#[path = "dv_refit_title_tests.rs"]
mod tests;
