//! Actual F+ whole and repositioned VOD materialization in an owned workspace.

use super::{
    super::m7_comparison::{HeldTitle, PublicationFrame, PublicationReadback, TitleContext},
    EvaluationError,
};
use crate::{
    producer_spawn::{ChildStdio, ChildStdios, Descriptors},
    renditiondir::{InitIdentity, RenditionDir, INIT_NAME},
    source_input::SourceInput,
    titlestore::Manifest,
    vodgen::{self, Generation, Outcome, Sink},
};
use plurx_core::{
    fmp4::{segment_name, CutPolicy, Fragment, FragmentReader, Init, Unit},
    process::{bounded, source_lifetime, ChildWork},
    segplan::{self, FragmentIndex, SegmentPlan, TrackDurations},
    transcode::{
        self,
        dv_m7_scoring::capture::FrameEvidence,
        dv_refit_serve::{ServedCurveTable, SourceClock},
        CopyVideoOptions,
    },
};
use sha2::{Digest as _, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    io,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::{io::AsyncReadExt, sync::Mutex};

const MAX_INIT_BYTES: usize = 4 * 1024 * 1024;

/// Bytes one fitted copy may read from its held source, and separately write
/// to its producer file. A fitted window from film zero reaches minutes into
/// a UHD title, gigabytes of source, so no fixed size can bound it. A stream
/// copy reads its source at most once; Matroska seeking also re-reads its
/// small SeekHead and Cues elements. A picture-only MP4 copy writes a subset
/// of what it read plus a few bytes of `trun` per picture. Twice the source
/// length is therefore a bound only a looping producer reaches.
fn fitted_copy_byte_ceiling(source_len: u64) -> Result<u64, String> {
    source_len
        .checked_mul(2)
        .filter(|bytes| *bytes > 0)
        .ok_or_else(|| "M7 fitted copy has no source byte ceiling".to_owned())
}

pub(super) struct FittedPublication {
    index: FragmentIndex,
    plan: SegmentPlan,
    policy: CutPolicy,
    table: Arc<ServedCurveTable>,
    video: CopyVideoOptions,
}

impl FittedPublication {
    pub(super) async fn load(context: &TitleContext<'_>) -> Result<Self, EvaluationError> {
        let current = context
            .state
            .store
            .dv_refit_curves(context.file.id)
            .await
            .map_err(|error| error.to_string())?;
        let current_method = current.as_ref().filter(|curves| {
            curves.title.fit_version == plurx_core::transcode::dv_refit::FIT_VERSION
        });
        let Some(curves) = current_method else {
            return if super::super::refit_work::enabled(context.state)
                .await
                .map_err(|error| error.to_string())?
            {
                Err(EvaluationError::Deferred(
                    "waiting for current fitted F+ table".into(),
                ))
            } else {
                Err(EvaluationError::Unsupported(
                    "F+ fit is disabled for this title".into(),
                ))
            };
        };
        let table = crate::dv_refit_serving::served_table(curves)?.ok_or_else(|| {
            EvaluationError::Unsupported("current F+ outcome has no fitted scenes".into())
        })?;
        let digest = parse_digest(table.digest())?;
        let video = CopyVideoOptions::from_probe(
            context.file,
            Some(&context.demand.probe_json),
            true,
            false,
        )
        .with_dolby_vision_conversion(true)
        .with_dolby_vision_refit(Some(digest));
        let identity = crate::fragindex::identity_for(context.file, video);
        let index = context
            .state
            .store
            .fragment_index(context.file.id, &identity)
            .await
            .map_err(|error| error.to_string())?
            .ok_or("canonical fitted F+ index is unavailable")?;
        if index.dv_refit_origin.is_none()
            || index
                .dv_refit_proofs
                .as_ref()
                .is_none_or(|p| p.len() != index.rows.len())
        {
            return Err("fitted index lacks its canonical source proof".into());
        }
        let policy = crate::vodserve::m7_copy_policy(index.timescale);
        let plan = segplan::plan_copy(
            &index,
            &policy,
            &TrackDurations {
                video_ms: context.file.duration_ms.unwrap_or_default(),
                audio_ms: 0,
                audio_bits_per_second: 0,
            },
        );
        Ok(Self {
            index,
            plan,
            policy,
            table: Arc::new(table),
            video,
        })
    }

    pub(super) fn index(&self) -> &FragmentIndex {
        &self.index
    }

    pub(super) fn plan(&self) -> &SegmentPlan {
        &self.plan
    }

    pub(super) fn table(&self) -> &Arc<ServedCurveTable> {
        &self.table
    }

    pub(super) fn clock(&self) -> Result<SourceClock, String> {
        SourceClock::for_index_landing(&self.index, 0, 0).map_err(|error| error.to_string())
    }

    /// One generation begins at film zero. The other has its own held input,
    /// nonzero producer seek and landing proof. The complete persisted
    /// segments are read back through their published init and track clock.
    pub(super) async fn readbacks(
        &self,
        context: &TitleContext<'_>,
        title: &HeldTitle,
        windows: &[Vec<FrameEvidence>],
        scratch: &Path,
    ) -> Result<Vec<(PublicationReadback, PublicationReadback)>, String> {
        let clock = self.clock()?;
        let mut frame_clock = BTreeMap::new();
        for frame in windows.iter().flatten().copied() {
            if (frame.time_base.numerator(), frame.time_base.denominator())
                != clock.source_time_base
                || frame_clock.insert(frame.pts, frame).is_some()
            {
                return Err("M7 source census overlaps or changes clock".into());
            }
        }
        let (first_pts, last_pts) = frame_clock
            .first_key_value()
            .zip(frame_clock.last_key_value())
            .map(|((&first, _), (&last, _))| (first, last))
            .ok_or("M7 publication needs at least one source frame")?;
        let mut first_entry = None;
        let mut last_entry = None;
        for entry in &self.plan.entries {
            let pts = clock
                .source_pts(i128::from(entry.start_ticks))
                .map_err(|error| error.to_string())?;
            if pts <= first_pts {
                first_entry = Some(entry.index);
            }
            if pts <= last_pts {
                last_entry = Some(entry.index);
            }
        }
        let first_entry = first_entry.ok_or("M7 census is outside the fitted publication")?;
        let seek_start = first_entry
            .checked_sub(1)
            .filter(|entry| *entry > 0)
            .ok_or("M7 windows need a nonzero independent seek")?;
        let final_entry = self.plan.entries.last().ok_or("empty fitted plan")?.index;
        let last_entry = last_entry
            .ok_or("M7 census is outside the fitted publication")?
            .saturating_add(1)
            .min(final_entry);

        let whole = self
            .publish(
                context,
                title,
                scratch.join("whole"),
                0,
                first_entry - 1,
                last_entry,
            )
            .await?;
        let seek = self
            .publish(
                context,
                title,
                scratch.join("seek"),
                seek_start,
                seek_start,
                last_entry,
            )
            .await?;
        if whole.init_hash != seek.init_hash {
            return Err("M7 whole and seek fitted initialization differ".into());
        }

        let reads = tokio::task::spawn_blocking(move || {
            let whole = read_publication(whole, clock, &frame_clock)?;
            let seek = read_publication(seek, clock, &frame_clock)?;
            whole.check_decode_durations(&seek)?;
            Ok::<_, String>((whole, seek))
        })
        .await
        .map_err(|error| error.to_string())??;
        let table = parse_digest(self.table.digest())?;
        windows
            .iter()
            .map(|window| {
                Ok((
                    reads.0.window(window, table)?,
                    reads.1.window(window, table)?,
                ))
            })
            .collect()
    }

    async fn publish(
        &self,
        context: &TitleContext<'_>,
        title: &HeldTitle,
        root: PathBuf,
        start: u32,
        retain: u32,
        last: u32,
    ) -> Result<PublishedRange, String> {
        let dir = RenditionDir::new(&root);
        dir.create().await.map_err(|e| e.to_string())?;
        let held = SourceFenceForInput::new(title)?;
        let input = SourceInput::open_background_held(
            held.0,
            "application/octet-stream",
            context.fence.source_budget(),
        )
        .await?;
        let outcome = self
            .produce(context, &input, dir, start, retain, last)
            .await;
        let closed = input.close_and_wait().await;
        closed.map_err(|error| error.to_string())?;
        outcome
    }

    async fn produce(
        &self,
        context: &TitleContext<'_>,
        input: &SourceInput,
        dir: RenditionDir,
        start: u32,
        retain: u32,
        last: u32,
    ) -> Result<PublishedRange, String> {
        let ceiling = fitted_copy_byte_ceiling(input.source_len())?;
        input.seal_read_ceiling(ceiling)?;
        let output_ceiling =
            usize::try_from(ceiling).map_err(|_| "M7 fitted copy ceiling exceeds memory")?;
        let mut file = context.file.clone();
        file.path = input.url().into();
        let entry = self
            .plan
            .entry(start)
            .ok_or("M7 fitted start entry is absent")?;
        let start_seconds = entry.start_ticks as f64 / f64::from(self.plan.timescale);
        let mut args = fitted_copy_args(&file, start_seconds, self.video);
        let executable =
            crate::ffmpeg::EncodedExecutable::capture_program(&context.state.system.ffmpeg).await?;
        let program = executable.path.clone();
        let runtime_cache = &context.state.runtime_cache_dir;
        let landing = if start == 0 {
            None
        } else {
            Some(
                crate::vodserve::m7_copy_landing_clock(
                    &args,
                    program.clone(),
                    runtime_cache.clone(),
                    self.index.timescale,
                    context.cancel.clone(),
                )
                .ok_or("M7 independent generation has no exact seek probe")?,
            )
        };
        self.plan
            .entry(last)
            .ok_or("M7 retained end entry is absent")?;
        if let Some(end_ticks) = bounded_copy_end_ticks(&self.plan, last, self.policy.max_ticks) {
            let seconds =
                end_ticks.saturating_sub(entry.start_ticks) as f64 / f64::from(self.plan.timescale);
            let insertion = args
                .len()
                .checked_sub(1)
                .ok_or("M7 copy argv has no output")?;
            args.splice(insertion..insertion, ["-t".into(), format!("{seconds:.6}")]);
        }

        let pipe = dir.path().join("producer.mp4");
        let stdout = File::create_new(&pipe).map_err(|e| e.to_string())?;
        let _pipe =
            source_lifetime::register_daemon_file(&stdout, &pipe).map_err(|e| e.to_string())?;
        let mut command = materialization_command(&program, args, dir.path(), runtime_cache);
        let descriptors = Descriptors::default()
            .prepare(
                &mut command,
                ChildStdios {
                    stdin: ChildStdio::Null,
                    stdout: ChildStdio::File(&stdout),
                    stderr: ChildStdio::Piped,
                },
            )
            .map_err(|e| e.to_string())?;
        let output = source_lifetime::mediated(bounded::output_command_bundle_env_to_file(
            &mut command,
            super::super::FEL_WINDOW_STAGE_DEADLINE,
            4096,
            output_ceiling,
            ChildWork::background("M7 fitted VOD materialization"),
            &[],
            stdout,
            Some(descriptors),
        ))
        .await
        .map_err(|e| e.to_string())?;
        if context.cancel.is_cancelled() {
            return Err("M7 fitted copy pipe was cancelled".into());
        }
        if !executable.is_current().await {
            return Err("M7 fitted copy FFmpeg identity changed".into());
        }
        if !output.status.success() {
            tracing::debug!(argv = ?command.as_std().get_args().collect::<Vec<_>>(), "M7 fitted copy failed");
            return Err(copy_failure(output.status, &output.stderr));
        }
        // FFmpeg ends a stream copy whose input stops early with exit 0 and a
        // truncated final fragment. Report the read, not that fragment's proof.
        if input.source_read_failed() {
            return Err(format!(
                "M7 fitted copy's held source read failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let mut source = tokio::fs::File::open(&pipe)
            .await
            .map_err(|e| e.to_string())?;
        let (consumed, init) = crate::vodserve::m7_read_muxer_init(&mut source, MAX_INIT_BYTES)
            .await
            .map_err(|e| e.to_string())?;
        let identity = InitIdentity::establish(&init, self.index.promotion.clone())
            .map_err(|e| e.to_string())?;
        let served = identity.served_init_for(&init).map_err(|e| e.to_string())?;
        dir.write_init(&served.bytes)
            .await
            .map_err(|e| e.to_string())?;
        let sink = PublicationSink {
            dir,
            manifest: Mutex::new(Manifest::new(self.plan.clone())),
            retain,
            last,
            cancel: context.cancel.clone(),
        };
        let gen = Generation {
            plan: self.plan.clone(),
            index: Some(self.index.clone()),
            landing_clock: landing,
            encoded_audio_anchor: None,
            encoded_frame_ticks: None,
            encoded_video_origin: None,
            source_audio: None,
            identity,
            convert_dolby_vision: true,
            dv_refit: Some(Arc::clone(&self.table)),
            retain_hevc_parameter_sets: self.video.retains_hevc_parameter_sets(),
            start_entry: start,
            policy: self.policy,
        };
        let output = vodgen::run(
            std::io::Cursor::new(consumed).chain(source),
            gen,
            &sink,
            "m7-fitted",
        )
        .await;
        if !matches!(output, Outcome::Ran { produced_through: Some(through) } if through == last)
            || context.cancel.is_cancelled()
        {
            return Err(format!(
                "M7 fitted publication did not materialize its complete window: {output:?}"
            ));
        }
        tokio::fs::remove_file(pipe)
            .await
            .map_err(|e| e.to_string())?;
        Ok(PublishedRange {
            root: sink.dir.path().to_owned(),
            retain,
            last,
            init_hash: Sha256::digest(&served.bytes).into(),
        })
    }
}

struct SourceFenceForInput(crate::fragment_index_cluster::SourceFence);

impl SourceFenceForInput {
    fn new(title: &HeldTitle) -> Result<Self, String> {
        crate::fragment_index_cluster::SourceFence::clone_held(
            &title.source.handle,
            title.source.object_version(),
        )
        .map(Self)
    }
}

struct PublicationSink {
    dir: RenditionDir,
    manifest: Mutex<Manifest>,
    retain: u32,
    last: u32,
    cancel: tokio_util::sync::CancellationToken,
}

impl Sink for PublicationSink {
    async fn materialize(&self, entry: u32, bytes: Vec<u8>) -> io::Result<()> {
        if self.cancel.is_cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "M7 publication cancelled",
            ));
        }
        if entry > self.last {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "bounded M7 publication completed",
            ));
        }
        if entry >= self.retain {
            let mut manifest = self.manifest.lock().await;
            self.dir
                .materialize(&mut manifest, entry, &bytes, super::super::now_ms())
                .await?;
        }
        Ok(())
    }
}

struct PublishedRange {
    root: PathBuf,
    retain: u32,
    last: u32,
    init_hash: [u8; 32],
}

struct PublishedPicture {
    frame: PublicationFrame,
    decode_duration: u32,
}

struct PublicationEvidence {
    publication_sha256: [u8; 32],
    timescale: u32,
    pictures: BTreeMap<i64, PublishedPicture>,
}

impl PublicationEvidence {
    fn check_decode_durations(&self, other: &Self) -> Result<(), String> {
        // Track sample duration describes decode spacing. The demuxer's
        // source packet duration is independent of that spacing.
        if self.timescale != other.timescale
            || self.pictures.len() != other.pictures.len()
            || self.pictures.iter().any(|(pts, picture)| {
                other
                    .pictures
                    .get(pts)
                    .is_none_or(|other| other.decode_duration != picture.decode_duration)
            })
        {
            return Err("M7 whole and seek fitted decode durations differ".into());
        }
        Ok(())
    }

    fn window(
        &self,
        frames: &[FrameEvidence],
        table: [u8; 32],
    ) -> Result<PublicationReadback, String> {
        let mut digest = Sha256::new();
        digest.update(b"plurx/m7/fplus-published-readback/v2\0");
        digest.update(self.timescale.to_be_bytes());
        let mut output = Vec::with_capacity(frames.len());
        for frame in frames {
            let observed = self
                .pictures
                .get(&frame.pts)
                .ok_or("F+ readback missed an exact source PTS")?;
            let frame = &observed.frame;
            digest.update(frame.source.pts.to_be_bytes());
            digest.update(frame.source.duration.to_be_bytes());
            digest.update(observed.decode_duration.to_be_bytes());
            digest.update(frame.bl_vcl_sha256);
            digest.update(frame.rpu_sha256);
            output.push(PublicationFrame {
                source: frame.source,
                bl_vcl_sha256: frame.bl_vcl_sha256,
                rpu_sha256: frame.rpu_sha256,
                rpu_nal: frame.rpu_nal.clone(),
            });
        }
        Ok(PublicationReadback {
            publication_sha256: self.publication_sha256,
            readback_sha256: digest.finalize().into(),
            fit_table_sha256: table,
            frames: output,
        })
    }
}

fn read_publication(
    published: PublishedRange,
    clock: SourceClock,
    window: &BTreeMap<i64, FrameEvidence>,
) -> Result<PublicationEvidence, String> {
    let init = std::fs::read(published.root.join(INIT_NAME)).map_err(|e| e.to_string())?;
    if <[u8; 32]>::from(Sha256::digest(&init)) != published.init_hash {
        return Err("published F+ init changed before M7 readback".into());
    }
    let mut pub_hash = Sha256::new();
    pub_hash.update(b"plurx/m7/fplus-publication/v1\0");
    pub_hash.update(published.init_hash);
    let mut pictures = BTreeMap::new();
    for entry in published.retain..=published.last {
        let bytes = std::fs::read(published.root.join(segment_name(u64::from(entry))))
            .map_err(|e| e.to_string())?;
        pub_hash.update(entry.to_be_bytes());
        pub_hash.update(Sha256::digest(&bytes));
        let mut reader = FragmentReader::new();
        reader.push(&init);
        reader.push(&bytes);
        let Some(Unit::Init(metadata)) = reader.next_unit().map_err(|e| e.to_string())? else {
            return Err("published F+ init is unreadable".into());
        };
        while let Some(unit) = reader.next_unit().map_err(|e| e.to_string())? {
            if let Unit::Fragment(fragment) = unit {
                extract_pictures(&fragment, &metadata, clock, window, &mut pictures)?;
            }
        }
    }
    if pictures.len() != window.len() {
        return Err("whole or seek F+ publication misses the exact M7 frame census".into());
    }
    Ok(PublicationEvidence {
        publication_sha256: pub_hash.finalize().into(),
        timescale: clock.track_timescale,
        pictures,
    })
}

fn extract_pictures(
    fragment: &Fragment,
    init: &Init,
    clock: SourceClock,
    expected: &BTreeMap<i64, FrameEvidence>,
    pictures: &mut BTreeMap<i64, PublishedPicture>,
) -> Result<(), String> {
    let video = init.video().ok_or("F+ readback has no video init")?;
    if video.timescale != clock.track_timescale || video.nal_length_size != 4 {
        return Err("F+ readback video clock or HEVC packet framing changed".into());
    }
    let track = fragment
        .track(video.id)
        .ok_or("F+ publication lost its video track")?;
    let mut decode = i128::from(track.base_decode_time);
    for run in &track.runs {
        let mut offset = run.data_offset;
        for sample in &run.samples {
            let end = offset
                .checked_add(sample.size as usize)
                .ok_or("F+ sample size overflow")?;
            let presentation = decode + i128::from(sample.cto);
            let pts = clock.source_pts(presentation).map_err(|e| e.to_string())?;
            if let Some(source) = expected.get(&pts) {
                let payload = fragment
                    .bytes
                    .get(offset..end)
                    .ok_or("F+ sample exceeds its fragment")?;
                let (bl_vcl_sha256, rpu_nal) = read_nals(payload)?;
                if sample.duration == 0
                    || source.bl_vcl_sha256 != bl_vcl_sha256
                    || pictures.contains_key(&pts)
                {
                    return Err("F+ readback diverges from the exact source frame census".into());
                }
                pictures.insert(
                    pts,
                    PublishedPicture {
                        frame: PublicationFrame {
                            source: *source,
                            bl_vcl_sha256,
                            rpu_sha256: Sha256::digest(&rpu_nal).into(),
                            rpu_nal,
                        },
                        decode_duration: sample.duration,
                    },
                );
            }
            offset = end;
            decode += i128::from(sample.duration);
        }
    }
    Ok(())
}

fn read_nals(mut sample: &[u8]) -> Result<([u8; 32], Vec<u8>), String> {
    let mut vcl = Sha256::new();
    let mut base_pictures = 0;
    let mut rpu = None;
    while !sample.is_empty() {
        let prefix = sample.get(..4).ok_or("invalid F+ NAL length prefix")?;
        let len = u32::from_be_bytes(prefix.try_into().expect("four bytes")) as usize;
        let extent = len.checked_add(4).ok_or("F+ NAL length overflow")?;
        let nal = sample
            .get(4..extent)
            .filter(|nal| nal.len() >= 2)
            .ok_or("invalid F+ NAL length")?;
        if ((nal[0] & 1) << 5 | nal[1] >> 3) != 0 || nal[1] & 7 == 0 {
            return Err("F+ readback has an unsupported coded layer or temporal ID".into());
        }
        match (nal[0] >> 1) & 63 {
            0..=31 => {
                base_pictures += usize::from(nal.get(2).ok_or("short F+ VCL header")? & 128 != 0);
                vcl.update(&sample[..extent]);
            }
            62 if (3..=4097).contains(&len) && rpu.is_none() => rpu = Some(nal.to_vec()),
            62 | 63 => {
                return Err("F+ readback has duplicate RPU or retained enhancement data".into())
            }
            _ => {}
        }
        sample = &sample[extent..];
    }
    if base_pictures != 1 {
        return Err("F+ readback does not contain exactly one base picture".into());
    }
    Ok((
        vcl.finalize().into(),
        rpu.ok_or("F+ readback omitted its RPU")?,
    ))
}

fn parse_digest(text: &str) -> Result<[u8; 32], String> {
    let mut digest = [0; 32];
    hex::decode_to_slice(text, &mut digest)
        .map_err(|_| "invalid fitted table digest".to_owned())?;
    Ok(digest)
}

/// Longest FFmpeg diagnosis carried into the title job's error.
const MAX_COPY_FAILURE_BYTES: usize = 512;

/// A failed fitted copy names how FFmpeg ended and its last few diagnostic
/// lines. FFmpeg states the cause near the end of stderr; without it every
/// failure reads the same and each retry rescans the whole title blind.
fn copy_failure(status: std::process::ExitStatus, stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let tail = lines[lines.len().saturating_sub(3)..].join(" | ");
    let mut message = format!("M7 fitted copy pipe failed ({status})");
    if !tail.is_empty() {
        let mut start = tail.len().saturating_sub(MAX_COPY_FAILURE_BYTES);
        while !tail.is_char_boundary(start) {
            start += 1;
        }
        message.push_str(": ");
        message.push_str(&tail[start..]);
    }
    message
}

/// Where a bounded fitted copy may stop. The run ends when the sink refuses
/// entry `last + 1`, which the segmenter emits only once the first fragment
/// of entry `last + 2` is pushed, and every pushed fitted fragment must first
/// match its canonical proof. FFmpeg's final fragment under `-t` is cut short
/// and its last duration is guessed at EOF, so it can never match. Ending at
/// `end(last)` plus the policy's maximum segment length lands inside that
/// closing fragment whenever entry `last + 1` is a maximum-length segment, so
/// the copy covers all of entry `last + 2` plus that margin. Without entry
/// `last + 2` the copy runs to the title's end, whose final fragment the index
/// measured the same way.
fn bounded_copy_end_ticks(plan: &SegmentPlan, last: u32, margin_ticks: u64) -> Option<u64> {
    let closing = plan.entry(last.checked_add(2)?)?;
    Some(
        closing
            .start_ticks
            .saturating_add(closing.duration_ticks)
            .saturating_add(margin_ticks),
    )
}

/// The fitted rendition is picture-only: its plan carries no audio track and
/// its comparison reads only video. Source audio is never mapped, so a
/// soundtrack MP4 cannot carry (TrueHD) cannot fail the copy.
fn fitted_copy_args(
    file: &plurx_core::domain::MediaFile,
    start_seconds: f64,
    video: CopyVideoOptions,
) -> Vec<String> {
    transcode::copy_video_only_pipe_args(
        file,
        start_seconds,
        plurx_core::transcode::Pacing::unpaced(),
        video,
    )
}

/// The fitted copy's FFmpeg runs in its own daemon-owned scratch directory,
/// like every other declared M7 and F+ child. Without one it inherits the
/// daemon's working directory, which the source-access verifier refuses
/// (`cwd_outside_daemon_roots`), and debug builds panic on that refusal.
fn materialization_command<I, S>(
    program: &Path,
    args: I,
    scratch: &Path,
    runtime_cache: &Path,
) -> tokio::process::Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut command = tokio::process::Command::new(program);
    command.current_dir(scratch).args(args);
    crate::producer_spawn::configure_ffmpeg_runtime(&mut command, runtime_cache);
    command
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "synthetic fMP4 fixtures and expected readback"
)]
mod tests {
    use super::*;
    use plurx_core::{
        fmp4::{Run, Sample, Track, TrackFragment, TrackKind, VideoCodec},
        transcode::Rational,
    };

    /// Eclipse's fitted copy failed in half a second with nothing recorded
    /// but "failed or was cancelled".
    #[cfg(unix)]
    #[test]
    fn a_failed_fitted_copy_names_ffmpegs_last_diagnosis() {
        use std::os::unix::process::ExitStatusExt;
        let status = std::process::ExitStatus::from_raw(1 << 8);
        let failure = copy_failure(
            status,
            b"banner\n[in] opened\n\n[mp4] Could not find tag\nConversion failed!\n",
        );
        assert!(failure.contains("exit status: 1"), "{failure}");
        assert!(
            failure.ends_with(": [in] opened | [mp4] Could not find tag | Conversion failed!"),
            "{failure}"
        );
        assert!(!copy_failure(status, b"").contains("): "));
        let long = copy_failure(status, "é".repeat(2 * MAX_COPY_FAILURE_BYTES).as_bytes());
        assert!(long.len() <= 64 + MAX_COPY_FAILURE_BYTES, "{}", long.len());
        assert!(long.ends_with('é'));
    }

    #[test]
    fn a_fitted_copy_may_read_and_write_its_whole_source() {
        // Eclipse's fitted window from film zero needs about 2 GB of a 83 GB
        // source; the former fixed 512 MiB ceiling ended it after 53 fragments.
        let eclipse = 83_773_946_095_u64;
        assert_eq!(fitted_copy_byte_ceiling(eclipse), Ok(2 * eclipse));
        assert!(fitted_copy_byte_ceiling(eclipse).expect("ceiling") > 2_000_000_000);
        assert!(fitted_copy_byte_ceiling(0).is_err());
        assert!(fitted_copy_byte_ceiling(u64::MAX).is_err());
    }

    /// A copy cut inside the fragment that closes entry `last + 1` leaves
    /// that fragment short, so its proof fails and the window never completes.
    #[test]
    fn a_bounded_fitted_copy_ends_past_the_fragment_that_closes_its_window() {
        use plurx_core::segplan::{PlanCut, PlanEntry, PlanEntryKind};
        let entries = [(0, 96), (96, 48), (144, 120), (264, 72)]
            .into_iter()
            .enumerate()
            .map(|(index, (start_ticks, duration_ticks))| PlanEntry {
                index: index as u32,
                kind: PlanEntryKind::Video,
                start_ticks,
                duration_ticks,
                est_bytes: 1,
                cut: PlanCut::Clean,
                fragments: 1,
            })
            .collect();
        let plan = SegmentPlan {
            version: 1,
            timescale: 24,
            entries,
            target_duration: 5,
        };
        // Entry 2 (144..264) holds the fragment whose push emits entry 1.
        assert_eq!(bounded_copy_end_ticks(&plan, 0, 30), Some(294));
        assert_eq!(bounded_copy_end_ticks(&plan, 1, 30), Some(366));
        // No entry after the closing one's predecessor: copy to the end.
        assert_eq!(bounded_copy_end_ticks(&plan, 2, 30), None);
        assert_eq!(bounded_copy_end_ticks(&plan, 3, 30), None);
        assert_eq!(bounded_copy_end_ticks(&plan, u32::MAX, 30), None);
    }

    /// Eclipse's fitted copy mapped its TrueHD soundtrack into the
    /// picture-only MP4 and FFmpeg refused the header (exit 88).
    #[test]
    fn the_fitted_copy_maps_only_the_selected_picture() {
        let mut file = context_free_file();
        file.audio_streams = vec![plurx_core::domain::AudioStream {
            index: 0,
            codec: "truehd".into(),
            channels: Some(8),
            default: true,
            ..Default::default()
        }];
        let args = fitted_copy_args(&file, 12.5, CopyVideoOptions::new(false, true));
        let maps: Vec<_> = args
            .windows(2)
            .filter(|pair| pair[0] == "-map")
            .map(|pair| pair[1].as_str())
            .collect();
        assert_eq!(maps, ["0:v:0"], "{args:?}");
        assert!(args.iter().any(|arg| arg == "-an"), "{args:?}");
        assert!(!args.iter().any(|arg| arg == "-c:a"), "{args:?}");
        assert_eq!(args.last().map(String::as_str), Some("pipe:1"));
        assert!(crate::vodserve::m7_copy_landing_clock(
            &args,
            "/bundle/ffmpeg".into(),
            "/owned/runtime".into(),
            24_000,
            tokio_util::sync::CancellationToken::new(),
        )
        .is_some());
    }

    fn context_free_file() -> plurx_core::domain::MediaFile {
        plurx_core::domain::MediaFile {
            downloaded_subtitles: Vec::new(),
            id: 1,
            item_id: 1,
            path: "http://127.0.0.1:1/source".into(),
            size: 1,
            mtime: 1,
            duration_ms: Some(60_000),
            container: Some("matroska".into()),
            video_codec: Some("hevc".into()),
            video_codec_tag: None,
            field_order: None,
            video_profile: Some("Main 10".into()),
            width: Some(3840),
            height: Some(2160),
            bit_depth: Some(10),
            hdr: None,
            hdr_format: None,
            max_cll: None,
            max_fall: None,
            mastering_max_luminance: None,
            luminance_source: None,
            bitrate: Some(1_000_000),
            audio_streams: vec![],
            subtitle_streams: vec![],
            scanned_at: 1,
            audio_offset_ms: 0,
            probed: true,
            dolby_vision: Default::default(),
        }
    }

    /// Eclipse's first fitted materialization panicked the M7 task with
    /// `cwd_outside_daemon_roots`, leaving its title job running forever.
    #[test]
    fn the_fitted_copy_runs_in_its_owned_scratch_directory() {
        let scratch = Path::new("/owned/m7-scratch");
        let command = materialization_command(
            Path::new("/bundle/ffmpeg"),
            ["-i", "in"],
            scratch,
            Path::new("/owned/runtime"),
        );
        assert_eq!(command.as_std().get_current_dir(), Some(scratch));
        assert_eq!(command.as_std().get_args().count(), 2);
    }

    #[test]
    fn copy_readback_keeps_source_packets_and_mux_decode_durations_in_their_clocks() {
        let time_base = Rational::new(1, 1_000).unwrap();
        let clock = SourceClock {
            track_timescale: 16_000,
            track_ticks: 0,
            source_pts: 0,
            source_time_base: (1, 1_000),
        };
        let init = Init {
            bytes: Vec::new(),
            tracks: vec![Track {
                id: 1,
                kind: TrackKind::Video,
                timescale: clock.track_timescale,
                has_edit_list: false,
                codec: Some(VideoCodec::Hevc),
                dolby_vision_config: true,
                nal_length_size: 4,
                default_sample_duration: 0,
                default_sample_size: 0,
                default_sample_flags: 0,
            }],
        };
        let vcl = [0, 0, 0, 3, 0x02, 0x01, 0x80];
        let rpu = [0, 0, 0, 3, 0x7c, 0x01, 0x80];
        let bytes = [vcl, rpu].concat();
        let source = FrameEvidence {
            ordinal: 760,
            pts: 31_906,
            duration: 41,
            time_base,
            access_unit_sha256: [1; 32],
            rpu_sha256: [2; 32],
            bl_vcl_sha256: Sha256::digest(vcl).into(),
        };
        let expected = BTreeMap::from([(source.pts, source)]);
        let read = |duration| {
            let fragment = Fragment {
                bytes: bytes.clone(),
                mdat_payload: 0..bytes.len(),
                tracks: vec![TrackFragment {
                    track_id: 1,
                    base_decode_time: 507_168,
                    runs: vec![Run {
                        version: 0,
                        composition_offsets_present: true,
                        data_offset: 0,
                        data_offset_at: None,
                        samples: vec![Sample {
                            duration,
                            size: bytes.len() as u32,
                            size_at: None,
                            flags: 0,
                            cto: 3_328,
                        }],
                    }],
                }],
            };
            let mut pictures = BTreeMap::new();
            extract_pictures(&fragment, &init, clock, &expected, &mut pictures).unwrap();
            PublicationEvidence {
                publication_sha256: [3; 32],
                timescale: clock.track_timescale,
                pictures,
            }
        };
        let whole = read(672);
        let seek = read(672);
        whole.check_decode_durations(&seek).unwrap();
        let window = whole.window(&[source], [4; 32]).unwrap();
        assert_eq!(window.frames[0].source, source);

        let changed_seek = read(656);
        assert!(whole.check_decode_durations(&changed_seek).is_err());
        assert_ne!(
            window.readback_sha256,
            changed_seek
                .window(&[source], [4; 32])
                .unwrap()
                .readback_sha256,
        );
    }
}
