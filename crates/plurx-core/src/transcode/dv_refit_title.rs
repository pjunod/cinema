//! A title's F+ fit, from the refit helper's per-scene statistics
//! (`docs/streaming/FEL-RPU-REFIT-BUILD.md` §3.1, §3.3–3.4).
//!
//! The helper (`segment_decode_render --refit-stats SOURCE VIDEO_INDEX`)
//! prints one [`RefitScene`] line per pooled scene in presentation order,
//! then one [`RefitSummary`] line. [`parse`] reads exactly that through the
//! typed lines and [`check_refit_run`], and refuses anything truncated or
//! inconsistent. Pooling short scenes is the helper's business; this
//! consumes its scenes as they are. [`fit`] solves and guards each scene
//! with [`super::dv_refit`] and decides the title: identity for a degenerate
//! title, for a title none of whose scenes passed, and for one whose pooled
//! held-out gain is under [`MIN_TITLE_GAIN`]. Its curve digest names the
//! served curve set.
use super::dv_refit::{
    check_refit_run, held_out_verdict, servable, ComposerCurves, HeldOutVerdict, RefitCurves,
    RefitScene, RefitSummary, FIT_VERSION,
};
use crate::store::dv_refit_jobs::{
    valid_time_base, RefitTitle, RefitVerdict, StagedScene, MAX_SCENES,
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

/// Parses the helper's complete stdout: scene lines, then the summary as the
/// last line (a trailing newline is optional), the whole run checked by
/// [`check_refit_run`] and starting at the title's first frame.
pub fn parse(stdout: &[u8]) -> Result<HelperOutput, String> {
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
    if scenes[0].first_frame != 0 {
        return Err("the helper run does not start at the title's first frame".into());
    }
    if !valid_time_base(&summary.time_base) {
        return Err(format!("time base {:?} is not storable", summary.time_base));
    }
    if summary.frames > i64::MAX as u64 {
        return Err("frame count out of range".into());
    }
    Ok(HelperOutput { scenes, summary })
}

/// One scene's outcome.
#[derive(Debug, Clone, PartialEq)]
pub struct SceneFit {
    pub scene: RefitScene,
    /// Curves that passed every guard, fit domain.
    pub curves: Option<RefitCurves>,
    /// Why the scene keeps identity curves.
    pub reason: Option<String>,
    pub verdict: Option<HeldOutVerdict>,
    /// Held-out squared error of the identity curves, and of what the scene
    /// serves on its own (its curves, or identity), in 12-bit codes².
    pub identity_error: f64,
    pub served_error: f64,
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

fn solve_scene(scene: RefitScene) -> SceneFit {
    let identity_error = scene
        .held_out
        .squared_error(&RefitCurves::identity())
        .map_or(0.0, |error| error.total());
    let outcome = scene
        .fit
        .solve()
        .map_err(|error| format!("no fit: {error}"))
        .and_then(|curves| {
            servable(&curves).map_err(|error| error.to_string())?;
            let verdict =
                held_out_verdict(&curves, &scene.held_out).map_err(|error| error.to_string())?;
            Ok((curves, verdict))
        });
    match outcome {
        Ok((curves, verdict)) => SceneFit {
            served_error: verdict.fitted.total(),
            curves: Some(curves),
            reason: None,
            verdict: Some(verdict),
            identity_error,
            scene,
        },
        Err(reason) => SceneFit {
            served_error: identity_error,
            curves: None,
            reason: Some(reason),
            verdict: None,
            identity_error,
            scene,
        },
    }
}

/// Solves and judges every scene, then the title.
pub fn fit(output: HelperOutput) -> TitleFit {
    let frames = output.summary.frames as i64;
    let degenerate = output.summary.degenerate;
    let scenes: Vec<SceneFit> = output.scenes.into_iter().map(solve_scene).collect();
    let identity: f64 = scenes.iter().map(|s| s.identity_error).sum();
    let served: f64 = scenes.iter().map(|s| s.served_error).sum();
    let gain = (identity > 0.0).then(|| 1.0 - served / identity);
    let fitted = scenes.iter().filter(|s| s.curves.is_some()).count();
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
    let served: Vec<Option<ComposerCurves>> = scenes
        .iter()
        .map(|s| match (verdict, &s.curves) {
            (RefitVerdict::Fitted, Some(curves)) => {
                Some(servable(curves).expect("judged servable").curves().clone())
            }
            _ => None,
        })
        .collect();
    let curve_digest = curve_digest(scenes.iter().zip(&served).map(|(s, curves)| {
        (
            s.scene.first_frame as i64,
            s.scene.first_pts,
            s.scene.time_base.as_str(),
            curves.as_ref(),
        )
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

#[derive(Serialize)]
#[serde(untagged)]
enum Served<'a> {
    Identity(&'static str),
    Curves(&'a ComposerCurves),
}

/// sha256 over [`FIT_VERSION`] and each scene's (first frame, first pts, time
/// base, served composer coefficients or `"identity"`), in presentation
/// order: the name of the curve set a session pins.
pub fn curve_digest<'a>(
    scenes: impl IntoIterator<Item = (i64, i64, &'a str, Option<&'a ComposerCurves>)>,
) -> String {
    let scenes: Vec<_> = scenes
        .into_iter()
        .map(|(frame, pts, time_base, curves)| {
            (
                frame,
                pts,
                time_base,
                curves.map_or(Served::Identity("identity"), Served::Curves),
            )
        })
        .collect();
    hex::encode(Sha256::digest(
        serde_json::to_vec(&("dv-refit-curves-v1", FIT_VERSION, scenes)).expect("curve digest"),
    ))
}

/// The digest of a title with no scenes (a refusal).
pub fn empty_digest() -> String {
    curve_digest(std::iter::empty())
}

fn json<T: Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_string(value).map_err(|e| e.to_string())
}

impl TitleFit {
    /// The rows publication stores: the title and its scenes, in order.
    pub fn rows(
        &self,
        file_id: i64,
        source_size: i64,
        source_mtime: i64,
        probe_revision: i64,
        helper_digest: &str,
    ) -> Result<(RefitTitle, Vec<StagedScene>), String> {
        let count = |value: u64| i64::try_from(value).map_err(|_| "count out of range".to_owned());
        let scenes = self
            .scenes
            .iter()
            .map(|s| -> Result<StagedScene, String> {
                Ok(StagedScene {
                    first_frame: count(s.scene.first_frame)?,
                    frames: count(s.scene.frames)?,
                    first_pts: s.scene.first_pts,
                    end_pts: s.scene.end_pts,
                    time_base: s.scene.time_base.clone(),
                    fit_json: json(&s.scene.fit)?,
                    held_out_json: json(&s.scene.held_out)?,
                    curves_json: s.curves.as_ref().map(json).transpose()?,
                    reason: s.reason.clone(),
                    verdict_json: s.verdict.as_ref().map(json).transpose()?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let title = RefitTitle {
            file_id,
            source_size,
            source_mtime,
            probe_revision,
            fit_version: FIT_VERSION.into(),
            helper_digest: helper_digest.into(),
            curve_digest: self.curve_digest.clone(),
            verdict: self.verdict,
            reason: self.reason.clone(),
            gain: self.gain,
            scene_count: scenes.len() as i64,
            frames: self.frames,
        };
        Ok((title, scenes))
    }
}

/// The title row for a helper refusal (for example a full-resolution EL, or
/// a stream that is not FEL): no scenes, identity everywhere, its one stderr
/// line as the reason.
pub fn refusal(
    file_id: i64,
    source_size: i64,
    source_mtime: i64,
    probe_revision: i64,
    helper_digest: &str,
    reason: &str,
) -> RefitTitle {
    let mut reason: String = reason.trim().chars().filter(|c| !c.is_control()).collect();
    if reason.is_empty() {
        reason = "the helper refused the title without a reason".into();
    }
    while reason.len() > crate::store::dv_refit_jobs::MAX_REASON_BYTES {
        reason.pop();
    }
    RefitTitle {
        file_id,
        source_size,
        source_mtime,
        probe_revision,
        fit_version: FIT_VERSION.into(),
        helper_digest: helper_digest.into(),
        curve_digest: empty_digest(),
        verdict: RefitVerdict::Refused,
        reason: Some(reason),
        gain: None,
        scene_count: 0,
        frames: 0,
    }
}

#[cfg(test)]
#[path = "dv_refit_title_tests.rs"]
mod tests;
