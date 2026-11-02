//! Transcode pipeline: pick an encoder, build the ffmpeg argument graph.
//!
//! The verdict of whether to transcode comes from [`crate::playback`]; this
//! module turns "transcode this file for that profile" into a concrete ffmpeg
//! invocation that produces HLS. Hardware *encode* is the big CPU win and is
//! selected per detected capability (NVENC/QSV/VAAPI/VideoToolbox), with a
//! software x264 fallback; HDR→SDR tone-mapping and subtitle burn-in run as
//! filters (ARCHITECTURE §3).
//!
//! The video path between decode and encode is a [`Pipeline`] — either the CPU
//! chain that downloads every frame to tone-map it in float, or one of the
//! graphs that keeps frames on the GPU. Which a node uses is decided by probe,
//! not by version (PERF-PLAN §5).

mod avc_qualification;
mod decode;
pub mod decoder_inventory;
mod dolby;
pub mod dv_fel_qualification;
pub mod dv_processing;
pub mod dvconvert;
mod encoder;
pub mod health;
pub mod hevc_census;
mod hevc_scan;
mod macos;
pub mod manifest;
mod pipeline;
pub mod progress;
mod recipe;
mod vod;

pub use avc_qualification::QualifiedSdrAvc;
pub use decode::{
    continuous_avc_envelope_accepts, plan_can_name_decoder, resolve_transcode,
    ArtifactQualification, AttemptRestrictions, AutoQualityRateProfile, CapabilityStatus,
    CompletedReconstructedOutputPlan, DecodeBackend, DecodeCacheIdentity, DecodeCapabilities,
    DecodeCapability, DecodeCapabilitySnapshotIdentity, DecodeCatalogMetadata, DecodeEvidence,
    DecodeFacts, DecodePlanPolicy, DecodePolicySnapshot, DecodeReason, DecodeSourceIdentity,
    DecodeSurfaceContract, Deinterlace, DynamicRangeClass, FrameDomain, FrameRate,
    FrameRateProvenance, InterlaceVerdict, NormalizedGeometry, OutputBandwidth,
    OutputMetadataPolicy, OutputWidthRule, PlanError, PlanSourceBinding, PresentationContract,
    Rational, ResolvedDecode, ResolvedTranscode, SoftwareDecoder, StreamSelectionProvenance,
    SubtitleRendering, ToneMapPeakSource, TranscodeMediaOptions, TranscodeRequest,
    VideoSampleEnvelope, HEALTH_QUALIFIED_ARTIFACT_NAMESPACE, RESOLVED_TRANSCODE_PLAN_VERSION,
    UNQUALIFIED_ARTIFACT_NAMESPACE,
};
pub use dolby::{
    LinuxDolbyContext, LinuxDolbyExecutionBinding, LinuxDolbyIdentity, LinuxDolbyObservation,
    LinuxExecutionObject, LinuxExecutionObjectKind, LinuxSourceClockAssociation,
    StrictDolbyImplementation, StrictDolbyPolicy,
};
pub use encoder::{
    benchmark_encoder, detect_encoders, detect_video_decoders, hevc_level_for,
    reconstructed_p81_video_kbps, reconstructed_p81_window_encode_args, vaapi_device,
    validate_quality_rate_control, validate_quality_rate_control_yielding, vbv_maxrate_bps,
    vbv_maxrate_kbps, CompletedReconstructedOutputContract, EffectiveRateControl, Encoder,
    EncoderCaps, EncoderDeviceBinding, GraphicsDevice, OutputCodecContract, OutputGrade,
    QualityRateControlValidation, QualityRc, RateMode, VaapiHdrPolicy, VideoCodec,
    RECONSTRUCTED_P81_MAX_VIDEO_KBPS, RECONSTRUCTED_P81_RECIPE,
};
pub use macos::{
    MacosProcessingAvailability, MacosProcessingContext, MacosProcessingGraph,
    MacosProcessingIdentity, MacosProcessingSelection, MACOS_PROCESSING_GRAPH_REVISION,
};
pub use pipeline::{Pipeline, ReconstructedPqInput, CANDIDATES as PIPELINE_CANDIDATES};
pub use recipe::{PipelineDigest, Recipe, CACHE_RECIPE_VERSION};
pub use vod::{
    vod_audio_anchor, vod_completed_reconstructed_pipe_args, vod_pipe_args,
    vod_pipe_args_with_reorder, vod_pipe_args_with_source_clock, vod_reconstructed_pipe_args,
    vod_reconstructed_video_origin, vod_shared_audio_args, vod_shared_audio_plan, VodFrameGrid,
    VodPresentationFamily, VodRenditionBandwidth, VodSharedAudioRecipe, VodSharedAudioRendition,
    VodSourceClock, VodVideoFamily, VodVideoRung, VOD_AAC_FRAME_SAMPLES, VOD_AUDIO_RATE,
    VOD_HEVC_SAMPLE_ENTRY, VOD_SHARED_AUDIO_CPU_THREADS,
};

use crate::domain::MediaFile;
use std::path::PathBuf;

/// Segment length for on-the-fly HLS, in seconds. Keyframes are forced to
/// align to this so segments are independently decodable.
///
/// This is the floor under time-to-first-frame, and it is a hard one: a player
/// cannot start on half a segment, so nothing plays until one whole segment
/// exists. At 4 s a session that encodes at 1× spent four seconds producing
/// the thing the viewer is waiting for, before any of the rest of the start
/// path even begins. Two halves that (PERF-PLAN §4.4).
///
/// The costs are real and small: twice the playlist churn and segment
/// requests (nothing at these sizes), a denser forced-keyframe grid worth a
/// few percent of compression at a given bitrate, and double the per-segment
/// mux overhead. The copy path cannot force keyframes at all — there
/// `hls_time` is only a floor and real segmentation follows the source's GOP —
/// so 2 s helps copy when the source GOP is short and never hurts it.
///
/// It is also the unit of every boundary the cluster failover contract talks
/// about (`docs/cluster/PHASE3-SPIKE.md`): a session restarted on another node resumes
/// at `N * SEGMENT_SECONDS`. Nothing may hardcode the number — the spike
/// measured 4, this is now 2, and the property holds for any fixed length. A
/// second copy of the value is a failover bug waiting for the day it changes
/// again.
pub const SEGMENT_SECONDS: u32 = 2;

/// Segment floor for the COPY path, and deliberately not [`SEGMENT_SECONDS`].
///
/// On a copy, `hls_time` is a floor under cuts that can only happen at source
/// keyframes — and every cut costs a frame. The one-stream experiment
/// (docs/streaming/STUTTER-4K.md §5.3ter) proved it: the same open-GOP bitstream plays
/// 1781 frames with ZERO drops as one continuous stream, and drops exactly
/// one leading picture per segment start when fragmented, in Chrome's MSE
/// and Safari's native player alike. Only the segment-START keyframe gets
/// the random-access treatment; keyframes inside a segment decode
/// continuously and keep their leading pictures. So boundary count is the
/// drop count, and a 6 s floor cuts it ~3.4× against the ~1.75 s GOP grid.
///
/// Why 6 and not more: hls.js fetches whole segments into MediaSource, and
/// at the ~100 Mb/s this path carries a segment must stay comfortably inside
/// the byte-budgeted forward buffer (`bufferTargets`) — 6 s at 69 Mb/s is
/// ~52 MB against a 96 MB forward budget; 10 s would make one segment the
/// entire buffer. Why the transcode path stays at 2: its encoder produces at
/// ~1×, so a viewer waits for the whole first segment (TTFF), and its closed
/// GOP has no leading pictures to lose. The copy path bursts the first 90 s
/// at disk speed, so a 6 s first segment still exists in well under a
/// second.
///
/// Safe to diverge from [`SEGMENT_SECONDS`]: copy sessions are live-only —
/// never cached (no recipe hash to collide with) and outside the Phase 3
/// failover contract, which is about transcode sessions.
pub const COPY_SEGMENT_SECONDS: u32 = 6;

/// Hard byte ceiling on one copy-path segment.
///
/// The segmenter (`crate::fmp4`) holds fragments back until it finds a
/// keyframe a player can start on without discarding a leading picture. On a
/// stretch of film that offers no such point, "hold back" has to stop
/// somewhere, and bytes are the binding constraint here long before seconds
/// are: at the ~60 Mb/s a 4K remux carries, a segment is 7.5 MB a second.
///
/// **This number is the search window, and it was measured twice.** It first
/// shipped at 48 MB, chosen as half of `bufferTargets`' 96 MB forward budget
/// so MSE could hold two whole segments. That reasoning was arithmetic, and
/// the arithmetic missed something: at 58 Mb/s a 48 MB ceiling arrives 6.6
/// seconds in, so past the six-second floor the segmenter had **0.6 seconds**
/// — less than one GOP — to find a clean keyframe in. On *reference film G* (2024),
/// whose clean points come every 2.3 s, that produced 5.9 dropped frames a
/// minute against ffmpeg's own muxer at 8.0: a segmenter barely beating the
/// thing it replaced, on a source full of the cut points it was looking for.
/// `scripts/gop-census --sweep` is the instrument that showed it.
///
/// 64 MB opens the window to 2.8 seconds — about three and a half GOPs — and
/// takes the same file to 1.9 drops a minute, 4.2x better than the muxer. The
/// ceiling on going further is the SourceBuffer quota, and it is a
/// measurement rather than a budget: a real hls.js in headless Chromium, fed
/// a 59.6 Mb/s stream cut at each size with the shipped `bufferTargets()`
/// numbers (13 s forward / 4 s back), reports zero quota events at 48 and 64
/// MB, and `bufferFullError` at 80 MB and above — repeatably, in every run.
/// Peak buffered bytes were identical (155 MB) in all four arms, so what
/// fails at 80 MB is not the total: it is the size of a single append, which
/// is a limit no byte budget on the client side can be traded against.
///
/// So 64 MB is the last rung that measured clean, and the "two whole
/// segments" framing it replaces was never the thing that mattered.
pub const COPY_SEGMENT_MAX_BYTES: usize = 64 * 1024 * 1024;

/// Duration floor for the FIRST copy segment, and only the first.
///
/// A player starting a live EVENT playlist has exactly one segment of runway,
/// because that is all the playlist holds when it loads it; its next chance to
/// learn any other segment exists is one `EXT-X-TARGETDURATION` away. So the
/// first segment being long is not a small cost — it is the whole startup
/// buffer. Two seconds means the first reload lands with time to spare, and
/// by then the session's burst has filled the playlist and the question never
/// arises again.
///
/// It also puts the first frame on screen sooner: nothing plays until one
/// whole segment exists.
///
/// Costs at most one extra boundary per session, and usually not even that —
/// the cut still has to land on a keyframe with nothing to discard, which on a
/// source with clean points every couple of seconds it will.
pub const COPY_FIRST_SEGMENT_SECONDS: u32 = 2;

/// Secondary ceiling, in seconds, for sources whose bytes never pile up.
///
/// A 3 Mb/s home video would reach [`COPY_SEGMENT_MAX_BYTES`] after two
/// minutes, and `EXT-X-TARGETDURATION` has to be declared up front and may
/// never decrease — so without a duration ceiling the playlist would have to
/// promise something absurd on session one to stay honest on session two.
/// Fifteen seconds keeps the tag believable and still leaves the floor plenty
/// of room to find a clean point on any normal GOP grid.
///
/// The two ceilings meet at about 34 Mb/s (64 MB ÷ 15 s), and they meet
/// rather than cross: below that the duration ceiling binds and a segment is
/// under 64 MB, above it the byte ceiling binds and a segment is under 15 s.
/// So one published segment is at most [`COPY_SEGMENT_MAX_BYTES`] plus the
/// fragment that crossed it, at any bitrate — which is the property the
/// SourceBuffer measurement was taken against.
///
/// It spent a day at 6 s, chasing a once-per-film freeze, and came back. The
/// freeze was the client starting at the live edge and catching the producer
/// — shrinking segments only scaled it down (8.8 s frozen at 9.2 s in became
/// 5.5 s at 6.0 s), because the freeze length *is* one segment's production
/// time. The publish gate ([`COPY_PUBLISH_GATE_SEGMENTS`]) removes it
/// outright, at which point a 6 s ceiling was pure cost: boundaries on every
/// low-bitrate source, each one a dropped frame on an open-GOP disc, buying
/// nothing.
pub const COPY_SEGMENT_MAX_SECS: u32 = 15;

/// Immutable `EXT-X-TARGETDURATION` for an unfinished rolling presentation.
///
/// [`COPY_SEGMENT_MAX_SECS`] remains the preferred place to cut. This is a
/// separate hard presentation contract: every advertised `EXTINF` must fit it
/// exactly, including a fragment that crosses the preferred cut and an
/// audio-only tail at end of stream. Keeping the values separate prevents an
/// unknown GOP from being treated as if it were the one-second difference.
pub const ROLLING_PRESENTATION_TARGET_SECS: u32 = 16;

/// Seconds of media that must exist before the first `index.m3u8` is
/// published.
///
/// **A playlist that starts at the live edge starts one publication gap from
/// a freeze.** Delivery to the browser runs at hundreds of Mb/s against a
/// producer that reads a 4K remux off the NAS at not much over 1×, so a
/// client that starts on segment zero drains everything published within
/// seconds and is then advancing at exactly the producer's publication rate.
/// The first time it has to wait a whole publication out, the picture stops
/// for one segment's worth of *production* time — once per film, always at
/// the same second, because where the buffer first runs dry is a property of
/// the film's early bitrate, not of timing. Measured on *reference film G* in Chrome:
/// an 8.8 s freeze at 9.2 s in under a 15 s ceiling, a 5.5 s freeze at 6.0 s
/// under a 6 s one — the freeze moved with the ceiling and scaled with it,
/// which is the experiment that showed shrinking segments approaches this
/// asymptotically and can never remove it. After that first stall the
/// producer's accumulated lead covers every later gap, which is why it
/// happens exactly once.
///
/// So the fix is a head start rather than a segment size: hold the playlist
/// until this many seconds of media exist, and the client begins that far
/// behind the edge. The cushion lives on the server — the client's
/// quota-bounded buffer stays as small as `bufferTargets` says, it just
/// always has published-but-unfetched segments in front of it, and every
/// segment after the gate only adds lead.
///
/// **The unit is seconds, not segments, and it was segments for a day.** A
/// count gate multiplies by whatever the opening happens to cut, and the cut
/// is a property of the title: a stretch quiet enough for the 15 s duration
/// ceiling to bind turns "three segments" into 2 + 15 + 15 = 32 s of
/// cushion, 2.4× what the freeze needs, every extra second of it produced at
/// the paced rate before a viewer sees frame one. A duration gate promises
/// the cushion in the unit the freeze is measured in; its realized size is
/// this value up to this value plus one segment, whatever the title cuts.
/// (*reference film F*, the file that exposed the slow start at 21.0 s to first frame,
/// turned out to cut clean 7.5–8.5 s segments — the overlay's own
/// largest-appended figure said so — so its cushion was ~17 s under either
/// unit, and its 21 s decomposes as ~8 s of cushion at the flat 2× pacing
/// below plus a cold NFS open and probe of a Dolby Vision MKV plus fetch
/// and decode; a warm second play started visibly faster with nothing
/// changed. The count was still the wrong unit — it just wasn't *reference film F's*
/// biggest cost.)
///
/// Why 12 s clears the worst gap: the gap is one segment's production time,
/// and duration and production speed are inversely coupled through bitrate.
/// A quiet stretch cuts long segments (up to 15 s) but reads cheap —
/// `HLS_READRATE` holds production at 2×, so the gap is ≤ 7.5 s. A heavy
/// stretch can be NAS-bound near 1×, but its segments are byte-capped: at
/// any bitrate the NAS can sustain playback of at all, a 64 MB segment
/// produces in ≤ ~8.5 s (8.8 s is the worst ever measured, on a 15 s
/// segment under the old regime). 12 covers both shapes; raising it buys
/// margin at one-for-one cost in time-to-first-frame on every paced open.
///
/// The remaining startup cost is honest and has one more owner: an ffmpeg
/// without `-readrate_initial_burst` (pre-6.1 — jellyfin-ffmpeg7 has it)
/// fills the gate at the flat paced rate instead of at I/O speed, which
/// `pacing_caps` now warns about at startup. With the burst working, the
/// cushion is produced as fast as the NAS will go and the gate costs a few
/// seconds; without it, the gate costs cushion ÷ readrate. `Manager::playlist`
/// holds the HTTP response while the gate fills rather than 404ing (verified
/// against the vendored hls.js: the manifest loader waits indefinitely for
/// the first byte — `maxTimeToFirstByteMs` is `Infinity` — inside a 20 s
/// per-attempt window with two retries).
///
/// At end of stream the gate yields: a film shorter than the cushion
/// publishes whole, `ENDLIST` and all, the moment it finishes.
pub const COPY_PUBLISH_GATE_SECS: u32 = 12;

/// The bitstream filter every copied HEVC stream gets before a client sees it.
///
/// Two kinds of NAL unit ride inside a `-c:v copy` that have no business on
/// this wire. In-band parameter sets (VPS/SPS/PPS, types 32–34): a
/// Blu-ray-lineage stream repeats all three at every IRAP and copy preserves
/// them, but the `hvc1` sample entry tagged just above *promises they are not
/// there* (ISO 14496-15 — parameter sets live in the sample entry only, which
/// ffmpeg's hvcC already carries). So every fragment used to open with a spec
/// violation, at exactly the boundary cadence of the 4K stutter this exists
/// to fix (docs/streaming/STUTTER-4K.md §5.0). And Dolby Vision's EL/RPU units (62/63):
/// tagging `hvc1` rather than `dvh1`/`dvhe` already forecloses any client
/// engaging DV on this path, so those units are dead weight in every access
/// unit — the browser's parser steps over them, and the base layer is
/// HDR10-compatible on its own.
///
/// Removing them is not a transcode. Verified 2026-07-29 on a repeat-headers
/// open-GOP fixture: `framemd5` of the decoded output is bit-identical with
/// and without, HDR10 static metadata (SEI, type 39) survives, and hvcC in
/// the init segment is untouched. A measured side effect worth having:
/// routing copy through a bitstream filter makes ffmpeg re-derive packet
/// timing through the parser, so sample durations land exactly on the frame
/// grid (1001/24000 at 23.976) instead of carrying the MKV's millisecond
/// rounding — which also zeroes the sub-frame presentation overlap the plain
/// copy left at every segment join.
///
/// One more thing a Dolby Vision source has to shed, learned from Safari on
/// an M3 Max (docs/streaming/STUTTER-4K.md): the *container signaling*. Stripping the
/// RPU/EL NAL units leaves the stream's DOVI configuration side data intact,
/// and the muxer writes it out as a `dvcC` box — a stream that DECLARES
/// Dolby Vision Profile 7 (the dual-layer Blu-ray profile no browser and no
/// Apple device supports) while carrying none of its data. Chrome ignores
/// the box; VideoToolbox honours it, and Safari answered with a software
/// decode of 4K10 HEVC on hardware that has a dedicated HEVC block. The
/// `dovi_rpu=strip=1` filter (ffmpeg ≥ 7.1) removes the RPUs *and* the DOVI
/// side data, so nothing is left to write a `dvcC` from and the stream is
/// signalled as what it now is: plain HDR10.
pub fn hevc_copy_bsf(hdr: Option<&str>, have_dovi_bsf: bool) -> String {
    hevc_copy_bsf_for_client(hdr, have_dovi_bsf, false)
}

/// Client-aware form of [`hevc_copy_bsf`]. A player that explicitly supports
/// this Dolby Vision profile receives its RPU/EL units unchanged; other
/// clients receive only the compatible HDR base.
pub fn hevc_copy_bsf_for_client(
    hdr: Option<&str>,
    have_dovi_bsf: bool,
    preserve_dolby_vision: bool,
) -> String {
    hevc_copy_bsf_for_copy(hdr, have_dovi_bsf, preserve_dolby_vision, false)
}

/// [`hevc_copy_bsf_for_client`] for a source whose in-band parameter sets
/// must reach the decoder (see [`hevc_copy_bsf_for_copy_retaining`]).
pub fn hevc_copy_bsf_for_client_retaining(
    hdr: Option<&str>,
    have_dovi_bsf: bool,
    preserve_dolby_vision: bool,
    retain_parameter_sets: bool,
) -> String {
    hevc_copy_bsf_for_copy_retaining(
        hdr,
        have_dovi_bsf,
        preserve_dolby_vision,
        false,
        retain_parameter_sets,
    )
}

/// The same choice, told whether this copy also converts Profile 7 to 8.1.
///
/// A converting copy drops NAL type 63 — the enhancement layer, which Profile
/// 8.1 does not have and which no consumer decoder was ever going to use —
/// while keeping type 62, because the RPUs are what plurx rewrites after the
/// muxer. Every other Dolby Vision branch here keeps both or removes both,
/// since every other path is preserving the stream or stripping it; the
/// conversion is the only caller that wants exactly one.
///
/// Leaving the enhancement layer in would ship orphan type-63 units behind
/// RPUs that no longer reference them: a stream declaring single-layer
/// Profile 8.1 while carrying a second layer's data, which is bytes on the
/// wire that nothing will ever read.
pub fn hevc_copy_bsf_for_copy(
    hdr: Option<&str>,
    have_dovi_bsf: bool,
    preserve_dolby_vision: bool,
    convert_dolby_vision: bool,
) -> String {
    hevc_copy_bsf_for_copy_retaining(
        hdr,
        have_dovi_bsf,
        preserve_dolby_vision,
        convert_dolby_vision,
        false,
    )
}

/// The same choice, told whether the source's in-band parameter sets have to
/// survive the copy.
///
/// Every branch of the historical filter deletes VPS/SPS/PPS (types 32-34),
/// which is only lossless when the in-band sets repeat the sample entry's
/// record. A chunk-encoded source that redefines them between shots decodes
/// against the record's stale definitions once they are gone — pink and green
/// blotches over a picture whose samples were copied unchanged. For such a
/// source ([`hevc_census`]) this keeps types 32-34 and changes nothing else:
/// the Dolby Vision layers are kept, dropped or partly dropped exactly as
/// before. A filter that would otherwise have nothing left to do is `null`
/// rather than absent, so the packets take the same bitstream-filter path
/// they always have.
pub fn hevc_copy_bsf_for_copy_retaining(
    hdr: Option<&str>,
    have_dovi_bsf: bool,
    preserve_dolby_vision: bool,
    convert_dolby_vision: bool,
    retain_parameter_sets: bool,
) -> String {
    if retain_parameter_sets {
        return if hdr == Some("dolby_vision") && convert_dolby_vision {
            "filter_units=remove_types=63"
        } else if hdr == Some("dolby_vision") && preserve_dolby_vision {
            "null"
        } else if hdr == Some("dolby_vision") && have_dovi_bsf {
            "dovi_rpu=strip=1,filter_units=remove_types=62-63"
        } else if hdr == Some("dolby_vision") {
            "filter_units=remove_types=62-63"
        } else {
            "null"
        }
        .to_owned();
    }
    if hdr == Some("dolby_vision") && convert_dolby_vision {
        "filter_units=remove_types=32-34|63".to_owned()
    } else if hdr == Some("dolby_vision") && preserve_dolby_vision {
        "filter_units=remove_types=32-34".to_owned()
    } else if hdr == Some("dolby_vision") {
        if have_dovi_bsf {
            "dovi_rpu=strip=1,filter_units=remove_types=32-34|62-63".to_owned()
        } else {
            // Removes the layers but not the claim. See
            // `copy_leaves_a_stale_dolby_vision_record`: the caller has to
            // delete the record from the muxer's init afterwards, or this
            // filter reintroduces the exact stutter it exists to prevent.
            "filter_units=remove_types=32-34|62-63".to_owned()
        }
    } else {
        "filter_units=remove_types=32-34".to_owned()
    }
}

/// Whether the filter [`hevc_copy_bsf_for_copy`] just chose leaves the muxer
/// writing a Dolby Vision configuration record over a stream that no longer
/// has one.
///
/// True for exactly one branch: a Dolby Vision source being stripped on an
/// ffmpeg without `dovi_rpu`. `filter_units` removes the RPU and
/// enhancement-layer NAL units *by type*, and knows nothing about the DOVI
/// side data ffmpeg copied out of the source container — so movenc writes that
/// side data out as a `dvcC` declaring Profile 7 with `el_present_flag = 1`,
/// in front of a stream carrying neither layer.
///
/// That is not a cosmetic mismatch. Chrome ignores the box; VideoToolbox
/// honours it, and Safari answers a 4K10 HEVC stream so labelled with a
/// *software* decode on hardware that has a dedicated block for it — the
/// stutter `docs/streaming/STUTTER-4K.md` exists to fix, reintroduced by the filter
/// meant to prevent it. The half-strip is worse than either whole answer, so
/// the init must have the record removed
/// (`crate::fmp4::remove_dolby_vision_record`) before a client sees it.
///
/// It is reachable. The automatic ladder never picks this branch —
/// `dv_handling` gates a strip on `node.dv_strippable`, which is this same
/// `dovi_rpu` probe, and re-encodes instead. But forced Original means "no
/// video re-encode" whatever the ladder said, so it serves the copy, and the
/// comment claiming the client's error path rescues it assumed a client that
/// refuses the stream. Safari does not refuse it. It plays it, badly.
///
/// Derived from the same four inputs as the filter and deliberately in the
/// same function's module: the two answers are one decision, and a caller
/// reading them from different places could compensate for a filter it did not
/// get.
pub fn copy_leaves_a_stale_dolby_vision_record(
    hdr: Option<&str>,
    have_dovi_bsf: bool,
    preserve_dolby_vision: bool,
    convert_dolby_vision: bool,
) -> bool {
    hdr == Some("dolby_vision") && !convert_dolby_vision && !preserve_dolby_vision && !have_dovi_bsf
}

/// Whether the stored probe describes HEVC with the smallest legal hvcC
/// record: the 23-byte fixed header followed by zero parameter-set arrays.
///
/// These Matroska files carry VPS/SPS/PPS only in-band. Removing NAL types
/// 32–34 before the fragmented-MP4 muxer sees them makes ffmpeg emit an empty
/// hvcC record, which neither our fragment reader nor a player can initialize
/// from. The copy path converts those packets through Annex B so ffmpeg's
/// `extract_extradata` filter can rebuild a complete hvcC before muxing.
pub fn hevc_parameter_set_promotion_required(source: &MediaFile, probe_json: Option<&str>) -> bool {
    if !matches!(source.video_codec.as_deref(), Some("hevc" | "h265")) {
        return false;
    }
    let Ok(probe) = serde_json::from_str::<serde_json::Value>(probe_json.unwrap_or_default())
    else {
        return false;
    };
    probe
        .get("streams")
        .and_then(serde_json::Value::as_array)
        .and_then(|streams| {
            streams.iter().find(|stream| {
                stream.get("codec_type").and_then(serde_json::Value::as_str) == Some("video")
                    && stream
                        .get("disposition")
                        .and_then(|value| value.get("attached_pic"))
                        .and_then(serde_json::Value::as_i64)
                        != Some(1)
            })
        })
        .and_then(|stream| stream.get("extradata_size"))
        .and_then(|size| {
            size.as_u64()
                .or_else(|| size.as_str().and_then(|value| value.parse().ok()))
        })
        == Some(23)
}

/// Facts that determine the copied video byte stream.
///
/// Keeping this as one value prevents the index, production segmenter, and
/// cache identity from accidentally choosing different HEVC normalization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyVideoOptions {
    have_dovi_bsf: bool,
    preserve_dolby_vision: bool,
    promote_hevc_parameter_sets: bool,
    dv_convert: bool,
    retain_hevc_parameter_sets: bool,
}

impl CopyVideoOptions {
    pub const fn new(have_dovi_bsf: bool, preserve_dolby_vision: bool) -> Self {
        Self {
            have_dovi_bsf,
            preserve_dolby_vision,
            promote_hevc_parameter_sets: false,
            dv_convert: false,
            retain_hevc_parameter_sets: false,
        }
    }

    /// Keep the source's in-band VPS/SPS/PPS in the copied stream.
    ///
    /// Set by [`CopyVideoOptions::from_probe`] from the source's stored
    /// [`hevc_census`] when its in-band parameter sets redefine what the
    /// sample entry says. It reaches the bitstream filter and therefore the
    /// argv fingerprint, so a retaining copy has its own fragment-index
    /// identity; a source whose in-band sets only repeat the record keeps the
    /// historical argv, bytes and identity unchanged. The segmenter reads it
    /// too ([`crate::fmp4::Segmenter::retaining_hevc_parameter_sets`]), or the
    /// merge would delete the very units the filter kept.
    pub const fn with_parameter_set_retention(mut self, retain: bool) -> Self {
        self.retain_hevc_parameter_sets = retain;
        self
    }

    /// Whether this copy keeps its in-band HEVC parameter sets.
    pub const fn retains_hevc_parameter_sets(self) -> bool {
        self.retain_hevc_parameter_sets
    }

    pub const fn with_parameter_set_promotion(mut self, required: bool) -> Self {
        self.promote_hevc_parameter_sets = required;
        self
    }

    /// Convert this source's Dolby Vision Profile 7 RPUs to Profile 8.1 on the
    /// way through (PLAYBACK-CAPS-V2-PLAN §4.8).
    ///
    /// Set by the decider when the source is Profile 7 with a compatible base
    /// and the client takes 8 but not 7 — which is every consumer Dolby Vision
    /// decoder, since dual-layer has never shipped outside Blu-ray hardware.
    ///
    /// It is a field of `CopyVideoOptions` for one reason beyond tidiness: the
    /// options render into `copy_video_args`, and the argv is what the
    /// fragment index is keyed by. A converted stream is a different stream
    /// and must get its own index identity, or a client would be served
    /// segments cut for the unconverted one.
    ///
    /// **Converting implies preserving**, and this method enforces it rather
    /// than trusting the caller. The decider reaches the conversion by way of
    /// a client that refuses Profile 7 — which is exactly the client `decide`
    /// answers `preserve_dolby_vision = false` for. That answer renders
    /// `dovi_rpu=strip=1,filter_units=remove_types=32-34|62-63`, which deletes
    /// the type-62 units this conversion exists to rewrite. The stage would
    /// then report `rpus: 0` and pass plain HDR10 through while the badge, the
    /// playlist and the configuration record all claimed Profile 8.1. There is
    /// no argv shape where a converting copy wants a stripping bitstream
    /// filter, so the invariant lives here and not in each caller (plan §4.8).
    pub const fn with_dolby_vision_conversion(mut self, convert: bool) -> Self {
        self.dv_convert = convert;
        if convert {
            self.preserve_dolby_vision = true;
        }
        self
    }

    /// Whether this copy converts Profile 7 to Profile 8.1.
    pub const fn converts_dolby_vision(self) -> bool {
        self.dv_convert
    }

    pub fn from_probe(
        source: &MediaFile,
        probe_json: Option<&str>,
        have_dovi_bsf: bool,
        preserve_dolby_vision: bool,
    ) -> Self {
        Self::new(have_dovi_bsf, preserve_dolby_vision)
            .with_parameter_set_promotion(hevc_parameter_set_promotion_required(source, probe_json))
            .with_parameter_set_retention(hevc_census::in_band_parameter_sets_vary(
                source, probe_json,
            ))
    }

    pub const fn promotes_parameter_sets(self) -> bool {
        self.promote_hevc_parameter_sets
    }

    pub const fn preserves_dolby_vision(self) -> bool {
        self.preserve_dolby_vision
    }

    /// Whether this copy's bitstream filter removes the Dolby Vision layers
    /// while leaving the muxer a record that still declares them.
    ///
    /// See [`copy_leaves_a_stale_dolby_vision_record`], which this is the
    /// options-shaped spelling of. It is a method rather than four arguments
    /// at each call site because the three flags it reads are private, and
    /// keeping them private is what stops a caller compensating for a filter
    /// it did not get.
    pub fn leaves_a_stale_dolby_vision_record(self, source: &MediaFile) -> bool {
        copy_leaves_a_stale_dolby_vision_record(
            source.hdr.as_deref(),
            self.have_dovi_bsf,
            self.preserve_dolby_vision,
            self.dv_convert,
        )
    }
}

/// Historical name retained for callers that only configure Dolby Vision.
pub type DolbyVisionCopyOptions = CopyVideoOptions;

/// HEVC sample-entry tag for the copy output.
///
/// Dolby Vision Profiles 8.1 and 8.4 are backward-compatible enhancements of
/// HDR10 and HLG. Apple's HLS/ISOBMFF contract requires those streams to keep
/// the compatible `hvc1` base sample entry; the Dolby Vision profile is
/// advertised separately by `SUPPLEMENTAL-CODECS`. Non-compatible Dolby
/// Vision (such as Profile 5) remains `dvh1` when it is preserved.
pub fn hevc_copy_tag(hdr: Option<&str>, preserve_dolby_vision: bool) -> &'static str {
    hevc_copy_tag_for_format(hdr, None, preserve_dolby_vision)
}

/// Format-aware sample-entry choice for segmented copies, whose source model
/// includes the richer HDR compatibility label.
///
/// Prefer [`hevc_copy_tag_for_source`] where a whole `MediaFile` is in hand:
/// the label is the fallback answer, not the authoritative one.
pub fn hevc_copy_tag_for_format(
    hdr: Option<&str>,
    hdr_format: Option<&str>,
    preserve_dolby_vision: bool,
) -> &'static str {
    tag_for(
        hdr,
        compatible_base_from_label(hdr_format),
        preserve_dolby_vision,
    )
}

/// The sample-entry choice made from everything the source row knows.
pub fn hevc_copy_tag_for_source(source: &MediaFile, preserve_dolby_vision: bool) -> &'static str {
    tag_for(
        source.hdr.as_deref(),
        dolby_vision_has_compatible_base(source),
        preserve_dolby_vision,
    )
}

fn tag_for(hdr: Option<&str>, compatible_base: bool, preserve_dolby_vision: bool) -> &'static str {
    if hdr == Some("dolby_vision") && preserve_dolby_vision && !compatible_base {
        "dvh1"
    } else {
        "hvc1"
    }
}

/// Does this Dolby Vision source have a base layer a non-DV client can watch?
///
/// The stored compatibility id answers it directly — 1 and 6 are HDR10, 4 is
/// HLG, 2 is SDR, 0 is none — and the label parse is the fallback for rows
/// scanned before M2 populated the column. This is deliberately the same
/// precedence [`crate::playback`] uses, and it has to be: the two decide the
/// same question about the same row, and a row where they disagree is one
/// where the decider routes a copy that the argv builder then renders for a
/// different stream. A column-only row (the columns populated, no label
/// written) took the Profile 5 branch here while the decider called its base
/// HDR10-compatible — `-tag:v dvh1`, and no `-bsf:v` at all, so NAL types
/// 32-34 went unfiltered and the `hvc1` boundary-stutter fix was silently off.
fn dolby_vision_has_compatible_base(source: &MediaFile) -> bool {
    if let Some(compat) = source.dolby_vision.bl_compat_id {
        return matches!(compat, 1 | 4 | 6);
    }
    compatible_base_from_label(source.hdr_format.as_deref())
}

fn compatible_base_from_label(hdr_format: Option<&str>) -> bool {
    hdr_format.is_some_and(|format| {
        format.contains("HDR10-compatible") || format.contains("HLG-compatible")
    })
}

/// Whether this ffmpeg has the `dovi_rpu` bitstream filter (landed in 7.1).
///
/// Parsed from the version line the daemon already collects at boot
/// ("ffmpeg version 7.1.4-Jellyfin …"). Unknown parses answer `false`: the
/// fallback path merely leaves the `dvcC` box behind, while passing an
/// unknown filter to an older ffmpeg is a hard exit for the whole session.
pub fn ffmpeg_has_dovi_bsf(version_line: &str) -> bool {
    // The first token carrying a digit is the version, wherever the build put
    // it: "7.1.4-Jellyfin", "n7.1.2", "6.1.1-3ubuntu5" all qualify.
    let v = version_line
        .split_whitespace()
        .find(|w| w.chars().any(|c| c.is_ascii_digit()))
        .unwrap_or("");
    let mut parts = v.split(|c: char| !c.is_ascii_digit()).filter_map(|p| {
        if p.is_empty() {
            None
        } else {
            p.parse::<u32>().ok()
        }
    });
    match (parts.next(), parts.next()) {
        (Some(major), _) if major > 7 => true,
        (Some(7), Some(minor)) => minor >= 1,
        (Some(7), None) => false,
        _ => false,
    }
}

/// How fast ffmpeg may read a session's input, as data.
///
/// The daemon resolves this — it is the half that knows what the ffmpeg build
/// supports (`-readrate` landed in 5.1, `-readrate_initial_burst` in 6.1, and
/// passing either to an older build is a hard exit rather than a warning) and
/// what the admin configured. The builders here stay pure functions of it.
///
/// Why pacing exists at all: an unpaced `-c copy` is a disk-to-socket pipe
/// that will write a whole 4K film into the session directory as fast as the
/// NAS will serve it. Why it is not simply `1x`: a session paced at exactly
/// realtime can never build a buffer, so the player's runway is whatever it
/// managed to fetch before playback started and every jitter after that is a
/// visible stall. The shape that satisfies both is burst-then-hold — fill a
/// comfortable buffer immediately, then settle to a small multiple of realtime
/// and let the ahead-window suspend (`TranscodeManager`) bound the rest.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Pacing {
    /// Multiple of realtime to read the input at. `None` leaves it unpaced.
    pub readrate: Option<f64>,
    /// Seconds delivered flat-out before the rate engages. `None` omits the
    /// clause (an ffmpeg between 5.1 and 6.1 has the rate but not the burst).
    pub initial_burst: Option<f64>,
    /// Use bare `-re` instead: exactly 1x, no burst, and the only pacing an
    /// ffmpeg older than 5.1 understands. The degradation path, not a choice.
    pub legacy_re: bool,
}

impl Pacing {
    /// No pacing at all — the input is read as fast as it can be.
    pub fn unpaced() -> Pacing {
        Pacing::default()
    }

    /// The flags for **one** input. Must be placed before that input's `-i`,
    /// like `-ss`: these are input options, and a second input left unpaced
    /// drags the whole pipeline back to flat-out because the muxer interleaves
    /// them.
    ///
    /// Public because the progressive remux (`/stream.mp4`) builds a
    /// `Command` rather than an argument vector and needs the same flags —
    /// one place decides their shape, so the two delivery paths cannot drift.
    pub fn args(&self) -> Vec<String> {
        let mut args = Vec::new();
        self.push(&mut args);
        args
    }

    fn push(&self, args: &mut Vec<String>) {
        if self.legacy_re {
            args.push("-re".into());
            return;
        }
        let Some(rate) = self.readrate.filter(|r| *r > 0.0) else {
            return;
        };
        if let Some(burst) = self.initial_burst.filter(|b| *b > 0.0) {
            args.push("-readrate_initial_burst".into());
            args.push(format!("{burst:.1}"));
        }
        args.push("-readrate".into());
        args.push(format!("{rate:.2}"));
    }
}

/// How HDR→SDR tone-mapping is performed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToneMap {
    /// CPU zscale+tonemap — always available, no GPU needed (the default).
    Zscale,
    /// libplacebo (Vulkan) — higher quality on a capable GPU; opt-in.
    Libplacebo,
    /// Jellyfin FFmpeg's SIMD software mapper, used for Dolby Vision RPU
    /// reshaping when a non-compatible Dolby Vision profile requires it.
    Tonemapx,
    /// No tone-mapping: pass HDR through to 8-bit (looks washed on an SDR
    /// screen, but plays). An escape hatch + a diagnostic — if a file that
    /// grayed out now plays, the tone-map was the culprit.
    None,
}

/// What to burn into the video (image subs must be burned; text subs can be).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubtitleBurn {
    /// 0-based index among the file's subtitle streams.
    pub subtitle_index: i64,
    /// Whether the sub is a bitmap format (PGS/VobSub) → overlay, vs text.
    pub bitmap: bool,
}

/// Everything needed to build a transcode command.
#[derive(Debug, Clone, PartialEq)]
pub struct TranscodeOptions {
    /// Negotiated output codec; absent preserves the incumbent grade-based recipe.
    pub output_codec: Option<VideoCodec>,
    /// Frozen strict Profile5 provenance, retained through all media retries.
    pub strict_dolby: Option<StrictDolbyPolicy>,
    /// Explicit immutable video sample recipe; ordinary encodes preserve defaults.
    pub video_sample_envelope: VideoSampleEnvelope,
    /// Conditional candidate semantics; absent preserves the legacy recipe.
    pub auto_quality_rate_profile: Option<AutoQualityRateProfile>,
    pub normalized_geometry: bool,
    pub target_height: i64,
    pub video_bitrate_kbps: u32,
    /// Validated rate control. Requested settings never reach this struct: a
    /// refused quality mode is represented as VBR before args or identity are
    /// built.
    pub effective_rate_control: EffectiveRateControl,
    /// Audio: output channel count (2 = stereo downmix) and bitrate.
    pub audio_channels: u32,
    pub audio_bitrate_kbps: u32,
    /// Server-resolved route delivery. Absent retains the legacy scalar
    /// builder during the add-before-remove migration of those fields.
    pub audio: Option<crate::playback::audio::AudioDelivery>,
    /// 0-based index among the file's audio streams (default track otherwise).
    pub audio_index: Option<i64>,
    /// Start offset in seconds (resume / session start).
    pub start_seconds: f64,
    /// First immutable HLS object number. Ordinary sessions start at zero;
    /// a fenced takeover resumes above the predecessor's published prefix.
    pub start_number: i64,
    pub tone_map: ToneMap,
    /// The video path this session should use. Chosen per node by probe (see
    /// [`Pipeline`]); [`Pipeline::Cpu`] is the always-available default and
    /// what a GPU graph falls back to when it fails at runtime.
    pub pipeline: Pipeline,
    pub subtitle_burn: Option<SubtitleBurn>,
    /// Cached simple-text subtitle sidecar used by libass. `None` means the
    /// subtitle is bitmap, absent, or a styled embedded source retained for
    /// compatibility.
    /// Not part of the recipe: the selected stream identifies the pixels;
    /// this only avoids reopening and scanning the full media file.
    pub subtitle_file: Option<PathBuf>,
    /// Pass the encoder's forced-IDR option, so `-force_key_frames` produces
    /// key frames the HLS muxer can actually cut at (see
    /// [`Encoder::forced_idr_flag`]). Set from the startup probe, which is
    /// what establishes that this build accepts it.
    ///
    /// Deliberately NOT part of the cache recipe: it changes where segments
    /// begin, not what any frame looks like, and an entry produced before this
    /// existed decodes to the same picture.
    pub force_idr: bool,
    /// Explicit thread budget for the software encoder, from the admission
    /// pool's permit (plurxd's `Workload::software_threads`). `None` lets
    /// x264 pick — which is cores x 1.5, a fine answer for exactly one
    /// session and an oversubscription for two. Ignored by hardware
    /// encoders: their parallelism lives in silicon, not in threads.
    ///
    /// Also deliberately NOT part of the cache recipe, for force_idr's
    /// reason: it changes how fast frames are made, not what they look like
    /// to a viewer — and on any one box the same workload computes the same
    /// budget anyway.
    pub software_threads: Option<u32>,
}

/// Attempt-local values consumed by the HLS command builder after semantic
/// planning is complete. Construction copies only execution fields from the
/// legacy aggregate; decoder, renderer, encoder, tracks, and output grade come
/// exclusively from [`ResolvedTranscode`].
#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
pub enum TranscodeExecutionError {
    #[error("invalid transcode execution: {0}")]
    Invalid(&'static str),
}

#[derive(Debug, Clone)]
pub struct TranscodeExecution {
    pub source_path: PathBuf,
    pub start_seconds: f64,
    pub start_number: i64,
    pub subtitle_file: Option<PathBuf>,
    pub force_idr: bool,
    pub software_threads: Option<u32>,
    pub pacing: Pacing,
    pub out_dir: String,
    /// How much this attempt asks the child to say about itself.
    ///
    /// Execution context rather than plan: the same semantic plan run on a
    /// node whose diagnostics are qualified and on one whose are not is the
    /// same work and must resolve to the same artifact identity. Only the
    /// flags differ.
    pub diagnostics: DiagnosticLogging,
}

/// The `-loglevel` contract between the daemon and one child.
///
/// §7.1 requires `repeat+level+error` for a build whose grammar is qualified:
/// `level` supplies the severity labels the contract matches on, and `repeat`
/// stops FFmpeg compressing repeated messages into a summary whose timestamps
/// the sliding window cannot honestly evaluate.
///
/// It is not the default, and that is deliberate rather than cautious. A node
/// asks for the qualified flags only when a retained contract covers the exact
/// binary it is about to run — so a fleet with no qualified build emits the
/// arguments it has always emitted, and the frozen argv baselines keep
/// describing what ships.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiagnosticLogging {
    #[default]
    Legacy,
    Qualified,
}

impl DiagnosticLogging {
    pub fn flags(self) -> &'static str {
        match self {
            Self::Legacy => "error",
            Self::Qualified => "repeat+level+error",
        }
    }
}

impl TranscodeExecution {
    pub fn from_options(
        source: &MediaFile,
        options: &TranscodeOptions,
        pacing: Pacing,
        out_dir: &str,
    ) -> Result<Self, TranscodeExecutionError> {
        if source.path.as_os_str().is_empty() {
            return Err(TranscodeExecutionError::Invalid("source path is empty"));
        }
        if out_dir.is_empty() {
            return Err(TranscodeExecutionError::Invalid(
                "output directory is empty",
            ));
        }
        if !options.start_seconds.is_finite() || options.start_seconds < 0.0 {
            return Err(TranscodeExecutionError::Invalid(
                "start offset must be finite and nonnegative",
            ));
        }
        if options.start_number < 0 {
            return Err(TranscodeExecutionError::Invalid(
                "HLS start number must be nonnegative",
            ));
        }
        if options.software_threads == Some(0) {
            return Err(TranscodeExecutionError::Invalid(
                "software thread budget must be positive",
            ));
        }
        if pacing
            .readrate
            .is_some_and(|rate| !rate.is_finite() || rate <= 0.0)
            || pacing
                .initial_burst
                .is_some_and(|burst| !burst.is_finite() || burst <= 0.0)
        {
            return Err(TranscodeExecutionError::Invalid(
                "pacing values must be finite and positive",
            ));
        }
        Ok(Self {
            source_path: source.path.clone(),
            start_seconds: options.start_seconds,
            start_number: options.start_number,
            subtitle_file: options.subtitle_file.clone(),
            force_idr: options.force_idr,
            software_threads: options.software_threads,
            pacing,
            out_dir: out_dir.to_owned(),
            diagnostics: DiagnosticLogging::Legacy,
        })
    }

    /// Ask this attempt's child for the diagnostics a qualified grammar reads.
    ///
    /// Only a caller holding a contract that covers the exact binary it is
    /// about to run may say yes. Asking for `repeat+level+error` from a build
    /// nobody has qualified buys nothing — there is no grammar to read the
    /// severities it would add — and changes the arguments that ship.
    pub fn observing_qualified_grammar(mut self, qualified: bool) -> Self {
        self.diagnostics = if qualified {
            DiagnosticLogging::Qualified
        } else {
            DiagnosticLogging::Legacy
        };
        self
    }
}

/// The AAC bitrate every HLS transcode uses unless asked otherwise — public
/// because the quality ladder's advertised totals must include the audio a
/// session will actually carry, from the same constant rather than a copy.
pub const AUDIO_BITRATE_KBPS_DEFAULT: u32 = 160;

impl Default for TranscodeOptions {
    fn default() -> Self {
        TranscodeOptions {
            video_sample_envelope: VideoSampleEnvelope::EncoderDefault,
            auto_quality_rate_profile: None,
            normalized_geometry: false,
            output_codec: None,
            strict_dolby: None,
            target_height: 1080,
            video_bitrate_kbps: 8000,
            effective_rate_control: EffectiveRateControl::Vbr,
            audio_channels: 2,
            audio_bitrate_kbps: AUDIO_BITRATE_KBPS_DEFAULT,
            audio: None,
            audio_index: None,
            start_seconds: 0.0,
            start_number: 0,
            tone_map: ToneMap::Zscale,
            pipeline: Pipeline::Cpu,
            subtitle_burn: None,
            subtitle_file: None,
            force_idr: false,
            software_threads: None,
        }
    }
}

impl TranscodeOptions {
    pub fn audio_budget_kbps(&self) -> u32 {
        self.audio
            .as_ref()
            .map_or(self.audio_bitrate_kbps, |audio| audio.budget_kbps())
    }

    /// Keep the transitional scalar readers in step with the one typed
    /// decision; no second caller chooses channels or bitrate independently.
    pub fn set_audio_delivery(&mut self, audio: crate::playback::audio::AudioDelivery) {
        match audio.action {
            crate::playback::audio::AudioAction::Encode {
                channels,
                bitrate_kbps,
                ..
            } => {
                self.audio_channels = u32::from(channels);
                self.audio_bitrate_kbps = bitrate_kbps;
            }
            crate::playback::audio::AudioAction::Copy { channels, .. } => {
                self.audio_channels = u32::from(channels);
                self.audio_bitrate_kbps = audio.budget_kbps();
            }
            crate::playback::audio::AudioAction::None => {}
        }
        self.audio = Some(audio);
    }
}

/// Label the burned video carries out of `-filter_complex`.
const BURNED_VIDEO_LABEL: &str = "[vout]";

/// The exact frame size a session will produce: the target height, never above
/// the source's, with an even width at the source's aspect.
///
/// Worth computing rather than leaving to `scale=-2:...` because a bitmap
/// subtitle has to be scaled to *match* it, and "whatever the scaler decided"
/// is not something a second filter can be told. `None` when the source was
/// never probed — an unprobed file has no aspect to preserve, and guessing one
/// would misshape the picture rather than the subtitle.
pub fn output_size(source: &MediaFile, target_height: i64) -> Option<(i64, i64)> {
    let (sw, sh) = (source.width?, source.height?);
    if sw <= 0 || sh <= 0 {
        return None;
    }
    let h = target_height.min(sh).max(2);
    // Even dimensions: yuv420p has half-resolution chroma, so an odd side is
    // not representable and every encoder rejects it.
    let w = ((sw * h + sh / 2) / sh).max(2);
    Some((w & !1, h & !1))
}

/// The `-filter_complex` fragment that renders a bitmap subtitle to the size
/// of the output frame, ready to composite. `None` when nothing is burned, the
/// burn is text (which the chain handles inline), or the source was never
/// probed.
///
/// The scaling is the whole subtlety, and getting it wrong is not subtle at
/// all. A PGS stream carries its own canvas size and positions its bitmaps
/// against that canvas — and on a UHD Blu-ray the canvas is very often 1920×1080
/// while the video is 3840×2160. Composited as-is, every subtitle lands at
/// quarter size in the upper-left quadrant. Scaling the subtitle plane to the
/// output frame first puts it back where it was authored, whichever way the
/// two sizes differ.
fn bitmap_overlay(source: &MediaFile, opts: &TranscodeOptions) -> Option<String> {
    bitmap_overlay_for_size(output_size(source, opts.target_height), opts)
}

fn bitmap_overlay_for_size(
    output_size: Option<(i64, i64)>,
    opts: &TranscodeOptions,
) -> Option<String> {
    let burn = opts.subtitle_burn.as_ref().filter(|b| b.bitmap)?;
    let (w, h) = output_size?;
    Some(format!(
        "[0:s:{idx}]scale={w}:{h}[sburn]",
        idx = burn.subtitle_index
    ))
}

/// Build the video filter chain: deinterlace (when required) → scale
/// (never upscale) → tone-map (if the source is HDR) → subtitle burn-in.
/// Returns `None` when no filtering is needed (rare for transcode, but keeps
/// the caller simple).
fn video_filters(source: &MediaFile, opts: &TranscodeOptions, source_path: &str) -> String {
    let (peak_nits, peak_source) = source.max_cll.map_or_else(
        || {
            source
                .mastering_max_luminance
                .map_or((1000, ToneMapPeakSource::Default), |value| {
                    (
                        u32::try_from(value).unwrap_or(1000),
                        ToneMapPeakSource::Mdcv,
                    )
                })
        },
        |value| (u32::try_from(value).unwrap_or(1000), ToneMapPeakSource::Cll),
    );
    video_filters_for_contract(
        (output_size(source, opts.target_height), None),
        (
            source.hdr.as_deref(),
            source.hdr.is_some(),
            routing_hdr(source),
        ),
        Deinterlace::for_scan_type(crate::domain::ScanType::from_field_order(
            source.field_order.as_deref(),
        )),
        (peak_nits, peak_source),
        opts,
        source_path,
    )
}

// The three dynamic-range facts travel as one tuple for the same reason the
// tone-map peak does: they are read together and never apart, and clippy caps
// this signature at seven. What the source declares, whether that counts as
// HDR, and what the route will carry are one question asked three ways.
fn video_filters_for_contract(
    geometry: (Option<(i64, i64)>, Option<NormalizedGeometry>),
    dynamic_range: (Option<&str>, bool, Option<&str>),
    deinterlace: Deinterlace,
    tone_map_peak: (u32, ToneMapPeakSource),
    opts: &TranscodeOptions,
    source_path: &str,
) -> String {
    let (input_dynamic_range, input_is_hdr, routing_dynamic_range) = dynamic_range;
    let (output_size, normalization) = geometry;
    let mut chain: Vec<String> = Vec::new();
    if let Some(rate) = normalization.and_then(|geometry| geometry.frame_rate) {
        chain.push(format!("fps={}/{}", rate.numerator(), rate.denominator()));
    }

    // A GPU pipeline owns scale and tone-map together: they are one pass on
    // the video-processing block, and splitting them would put the download
    // this exists to remove back in the middle. It is only ever selected for
    // sessions it can handle — `Pipeline::for_session` has already routed HLG
    // and unsupported Dolby Vision to the CPU chain. Subtitle burns ride the
    // GPU graph too: scale + tone-map happen first, then vendor surfaces come
    // down to system memory exactly once for libass/overlay. What stays off
    // the CPU is the float tone-map — the cost that put a 4K burn under
    // realtime while the GPU graph measured 4.9× (PERF-PLAN §5).
    let bitmap_burn = opts.subtitle_burn.as_ref().is_some_and(|b| b.bitmap);
    let text_burn = opts.subtitle_burn.as_ref().is_some_and(|b| !b.bitmap);
    // Always give GPU scalers the same explicit even, no-upscale dimensions
    // the CPU path promises. `w=-1` can resolve to an odd NV12 width.
    let gpu_size = output_size;
    if let Some(mut gpu) = opts.pipeline.filters_at_peak(
        gpu_size.map(|(w, _)| w),
        gpu_size.map_or(opts.target_height, |(_, h)| h),
        input_dynamic_range,
        Some(tone_map_peak.0),
    ) {
        // The exact Mac interlace tuple retains file send-frame cadence and
        // progressive bypass. Deinterlace the full raster before scaling;
        // native BWDIF is independently observed, never inferred from scaling.
        if deinterlace == Deinterlace::BwdifSendFrame
            && matches!(
                opts.pipeline,
                Pipeline::VtScaleSdr | Pipeline::VtToneMapMetal
            )
        {
            gpu.insert_str(
                0,
                "bwdif_videotoolbox=mode=send_frame:parity=auto:deint=interlaced,",
            );
        }
        // The Dolby Vision and HDR10 software renderers own their entire
        // colour graph, so they do not fall through to the generic CPU chain
        // below. Insert bwdif immediately before their scale step: Dolby
        // Vision still reshapes before resizing, while every special graph
        // actually applies the deinterlace decision carried by the plan.
        if deinterlace == Deinterlace::BwdifSendFrame
            && matches!(
                opts.pipeline,
                Pipeline::DoviTonemapx | Pipeline::DoviPassthrough | Pipeline::Hdr10Passthrough
            )
        {
            let scale = gpu
                .find("scale=")
                .expect("special software renderers always contain a scale step");
            gpu.insert_str(scale, "bwdif=mode=send_frame:parity=auto:deint=interlaced,");
        }
        if let Some(normalization) = normalization {
            if let Some(rotation) = normalization.rotation_filter() {
                // Dolby RPU processing precedes spatial transformation. These
                // software graphs have one scale step after reshape/tonemap.
                let scale = gpu
                    .find("scale=")
                    .expect("normalized software renderer has scale");
                gpu.insert_str(scale, &format!("{rotation},"));
            }
            gpu.push_str(",setsar=1");
        }
        if (bitmap_burn || text_burn) && opts.pipeline.keeps_frames_off_the_cpu() {
            // These graphs end in vendor surfaces (their encoders read them
            // directly); the composite cannot. Libplacebo and OpenCL already
            // finish with their own download, so they need nothing here.
            chain.push(format!("{gpu},hwdownload,format=nv12"));
        } else {
            chain.push(gpu);
        }
        return with_subtitles(chain, opts, source_path);
    }

    if deinterlace == Deinterlace::BwdifSendFrame {
        chain.push("bwdif=mode=send_frame:parity=auto:deint=interlaced".to_owned());
    }
    if let Some(normalization) = normalization {
        if let Some(rotation) = normalization.rotation_filter() {
            chain.push(rotation.to_owned());
        }
        let (width, height) = output_size.expect("normalized plan has exact raster");
        chain.push(format!("scale={width}:{height},setsar=1"));
    } else {
        // Preserve the shipping argument/identity contract for legacy routes.
        chain.push(format!("scale=-2:'min({h},ih)'", h = opts.target_height));
    }

    // HDR → SDR tone-map when the source carries HDR.
    if input_is_hdr && opts.tone_map != ToneMap::None {
        match opts.tone_map {
            ToneMap::None => {} // guarded out above; format normalize happens below
            ToneMap::Libplacebo => chain.push(
                "libplacebo=tonemapping=bt.2390:colorspace=bt709:color_primaries=bt709:\
                 color_trc=bt709:format=yuv420p"
                    .to_owned(),
            ),
            ToneMap::Tonemapx => chain.push(dovi_tonemapx_sdr_filter()),
            ToneMap::Zscale => {
                // Declare the input transfer/primaries/matrix explicitly instead
                // of letting zscale read them off the frame. Hardware decode
                // (QSV/VAAPI) frequently drops color metadata across the
                // hwdownload, so an inferred `t=linear` mis-maps the PQ signal to
                // a flat gray picture — the exact 4K-HDR/DV symptom. HDR is
                // BT.2020; PQ (HDR10/HDR10+/DV) vs HLG differ only in transfer.
                let tin = if routing_dynamic_range == Some("hlg") {
                    "arib-std-b67"
                } else {
                    "smpte2084"
                };
                chain.push(zscale_tone_map_filter(tin, tone_map_peak.0));
            }
        }
    } else {
        // Normalize to a browser-safe pixel format.
        chain.push("format=yuv420p".to_owned());
    }

    if output_metadata_policy_for(opts.pipeline, opts.tone_map, input_is_hdr).is_some() {
        // zscale/tonemap consume these HDR facts but preserve AVFrame/link
        // side data. Remove only the consumed static HDR types at the actual
        // SDR boundary; caption and unrelated presentation data remain intact.
        chain.push("sidedata=mode=delete:type=MASTERING_DISPLAY_METADATA".to_owned());
        chain.push("sidedata=mode=delete:type=CONTENT_LIGHT_LEVEL".to_owned());
    }
    with_subtitles(chain, opts, source_path)
}

fn output_metadata_policy_for(
    pipeline: Pipeline,
    tone_map: ToneMap,
    input_is_hdr: bool,
) -> Option<OutputMetadataPolicy> {
    (pipeline == Pipeline::Cpu && tone_map == ToneMap::Zscale && input_is_hdr)
        .then_some(OutputMetadataPolicy::ConsumedHdrStaticV1)
}

/// Subtitle burn-in, last, so subs render at output resolution in the output
/// color space. Text/ASS is rendered with libass (covers the styled
/// anime-subtitle case, REQ-SUB-2). Bitmap subs are not here at all: they are
/// a second stream composited over this chain's output by [`bitmap_overlay`],
/// which `-vf` cannot express.
///
/// Shared by both branches above because it is genuinely the same step. On a
/// vendor GPU path the frame is downloaded immediately before this filter and
/// uploaded immediately after it; scale + tone-map remain on the GPU.
fn with_subtitles(mut chain: Vec<String>, opts: &TranscodeOptions, source_path: &str) -> String {
    // macOS libass defaults to CoreText. The application freezes Fontconfig
    // rules and faces, so its renderer must explicitly use that authority.
    // Other platforms keep their existing automatic provider selection.
    let provider = if cfg!(target_os = "macos") {
        ":font_provider=fontconfig"
    } else {
        ""
    };
    if let Some(burn) = &opts.subtitle_burn {
        // A bitmap burn is not part of this chain: it is a second stream
        // composited over the chain's output, built by `bitmap_overlay`.
        if !burn.bitmap {
            if let Some(path) = &opts.subtitle_file {
                let escaped = escape_filter_path(&path.to_string_lossy());
                chain.push(format!("subtitles='{escaped}'{provider}"));
            } else {
                let escaped = escape_filter_path(source_path);
                chain.push(format!(
                    "subtitles='{escaped}':si={idx}{provider}",
                    idx = burn.subtitle_index
                ));
            }
        }
    }
    chain.join(",")
}

/// The 8-bit pixel formats this crate's filter chains ever name. Listed
/// rather than inferred so a new spelling has to be added deliberately.
const EIGHT_BIT_FORMATS: &[&str] = &["yuv420p", "yuvj420p", "nv12", "yuv422p", "yuv444p"];

/// The CPU HDR→SDR tone map: linearise, convert BT.2020 → BT.709 primaries,
/// apply the Hable curve against an explicit peak, return to 8-bit BT.709
/// limited range with error-diffusion dither.
///
/// `transfer_in` is the source transfer (`smpte2084` or `arib-std-b67`) and
/// `peak_nits` the stated or policy peak; `tonemap` takes it in hundreds of
/// nits. This is the one spelling of the chain: the boot probe measures
/// every GPU graph against exactly this string, so it must not be copied.
///
/// The gamut conversion rides on the linearising `zscale` (`p=bt709`) rather
/// than on a separate pass ahead of `tonemap`. zimg converts primaries in
/// linear light either way, still before the curve, so the output is
/// bit-identical to a standalone `zscale=p=bt709` — and one float32 pass
/// cheaper. Naming the output primaries there matters on its own too: with
/// `pin=` but no `p=`, zimg takes the *output* primaries from the frame, so a
/// decoded frame that arrives without a primaries tag fails the graph with
/// "no path between colorspaces" and produces nothing.
pub fn zscale_tone_map_filter(transfer_in: &str, peak_nits: u32) -> String {
    format!(
        "zscale=tin={transfer_in}:min=bt2020nc:pin=bt2020:t=linear:p=bt709:npl=100,\
         format=gbrpf32le,\
         tonemap=tonemap=hable:desat=0:peak={peak},\
         zscale=t=bt709:m=bt709:r=tv:dither=error_diffusion,format=yuv420p",
        peak = f64::from(peak_nits) / 100.0,
    )
}

/// The Dolby Vision reshape + SDR tone map: `tonemapx` applies the RPU
/// (`apply_dovi=1`) and maps to 8-bit BT.709 limited range with bt2390.
///
/// The one spelling of it. The renderer graph
/// ([`Pipeline::DoviTonemapx`]), the CPU-pipeline `ToneMap::Tonemapx` arm and
/// the daemon's boot probe of the renderer all build their filter from this,
/// so the graph a node proves at boot is the graph its sessions run. Every
/// colour term comes from [`OutputGrade::Sdr`], the same value the encoder's
/// tags come from.
pub fn dovi_tonemapx_sdr_filter() -> String {
    let grade = OutputGrade::Sdr;
    format!(
        "tonemapx=tonemap=bt2390:transfer={transfer}:matrix={matrix}:\
         primaries={primaries}:range=tv:format={format}:apply_dovi=1",
        transfer = grade.transfer(),
        matrix = grade.matrix(),
        primaries = grade.primaries(),
        format = grade.pixel_format(),
    )
}

/// Refuse to hand ffmpeg a filter that asks for a PQ **output** transfer at an
/// 8-bit output depth.
///
/// This is not a style check. `tonemapx` with `transfer=smpte2084` and an
/// 8-bit output format does not return an error and does not print a
/// diagnostic: it hits
/// `Assertion 0 failed at libavfilter/vf_tonemapx.c:1475` and calls
/// `abort()` — SIGABRT, exit 134, measured on jellyfin-ffmpeg7 7.1.4-3. A
/// process kill is not something the session-level fallback machinery can
/// tell apart from an OOM kill or an operator's `kill -9`, so the
/// combination has to be impossible to *emit*, not merely recoverable from.
///
/// [`OutputGrade`] makes it impossible to construct in the first place —
/// transfer and pixel format come out of one value. This is the second line:
/// filter chains are also assembled as text (subtitle burns, the bitmap
/// overlay's `-filter_complex`, `hwdownload` prefixes, and anything a future
/// change concatenates), and text does not typecheck.
///
/// Judged **per filter**, and on the `transfer=` key specifically. The
/// distinction is load-bearing in both directions:
///
/// - `zscale=tin=smpte2084:…:t=bt709,format=yuv420p` is the SDR tone-map, and
///   it is correct. `tin=` declares the *input* transfer; a chain-wide search
///   for the string `smpte2084` condemns it.
/// - `tonemapx=transfer=smpte2084:format=yuv420p10le,scale=…,format=yuv420p`
///   would be a legitimate 10-bit map followed by an 8-bit downconvert done
///   by swscale, which does not abort. Only the filter that declares the PQ
///   output has to be 10-bit.
///
/// Panics rather than returning an error because there is no recovery and no
/// caller who could choose one: every reachable path builds the chain from an
/// `OutputGrade`, so a violation is a bug in this file that a test must catch
/// before a viewer's ffmpeg does.
pub fn assert_no_pq_at_8_bit(chain: &str) {
    // `,` separates filters, `;` separates the branches of a filter_complex.
    for filter in chain.split([',', ';']) {
        let options: Vec<&str> = filter.split(':').collect();
        let declares_pq = options.iter().any(|option| {
            option
                .rsplit('=')
                .next()
                .is_some_and(|value| value == "smpte2084")
                && option.contains("transfer=")
        });
        if !declares_pq {
            continue;
        }
        let offender = options.iter().find_map(|option| {
            let value = option.strip_prefix("format=")?;
            EIGHT_BIT_FORMATS.contains(&value).then_some(value)
        });
        assert!(
            offender.is_none(),
            "filter `{filter}` pairs a PQ output transfer with the 8-bit format \
             `{}`, which aborts ffmpeg (SIGABRT, vf_tonemapx.c:1475) rather than \
             failing: {chain}",
            offender.unwrap_or_default(),
        );
    }
}

/// Escape a path for use inside an ffmpeg filter argument (colons, quotes,
/// backslashes, commas are special).
fn escape_filter_path(path: &str) -> String {
    path.replace('\\', "\\\\")
        .replace(':', "\\:")
        .replace('\'', "\\'")
}

/// Decode-side flags, plus an optional filter-chain prefix.
///
/// The heavy case — HEVC that is 4K and/or HDR/Dolby Vision — is hardware-decoded
/// on Intel (QSV/VAAPI) as well, not just NVIDIA. Software-decoding a 4K 10-bit
/// HEVC/DV stream pins the CPU near 100% per core (the "~1000% CPU" symptom) and
/// is far too slow to finish even the first HLS segment, so the player sits on a
/// gray screen until the idle reaper kills the session. GPU-decoded frames arrive
/// as hardware surfaces, so a matching `hwdownload` (to the source's pixel format)
/// leads the CPU scale/tonemap chain before the encoder re-uploads them.
///
/// Lighter sources keep software decode: it's the most compatible path and is
/// already fast enough, so we don't risk the GPU decode/filter handoff where it
/// isn't needed.
/// The HDR format a session should be ROUTED by, as opposed to what the file
/// is.
///
/// A Dolby Vision source whose base layer is HDR10-compatible (Profiles 7/8
/// with the cross-compatible id — the probe's rich label says so) decodes to
/// ordinary PQ surfaces carrying HDR10 static metadata: exactly what an hdr10
/// file decodes to, because the BL *is* a compliant HDR10 stream by
/// construction. Neither tone-map chain reads the RPU dynamic metadata — the
/// CPU chain declares `tin=smpte2084` and maps statically, and the vendor
/// graphs do the same from surface metadata — so for graph selection this IS
/// an hdr10 source, and declining the 4.9× GPU tone-map for it (PERF-PLAN §5)
/// bought no correctness. Profiles without that base (5: IPTPQc2) keep
/// routing as `dolby_vision`, which the vendor graphs correctly decline.
///
/// Routing only: everything that *builds* filters keeps the file's real
/// `hdr` ("dolby_vision" still picks `tin=smpte2084`, still strips RPUs on
/// the copy path), and the cache digest keys on the pipeline that actually
/// ran, so entries made under either routing stay distinct and honest.
pub fn routing_hdr(file: &MediaFile) -> Option<&str> {
    match file.hdr.as_deref() {
        Some("dolby_vision")
            if file
                .hdr_format
                .as_deref()
                .is_some_and(|f| f.contains("HDR10-compatible")) =>
        {
            Some("hdr10")
        }
        Some("dolby_vision")
            if file
                .hdr_format
                .as_deref()
                .is_some_and(|f| f.contains("HLG-compatible")) =>
        {
            Some("hlg")
        }
        other => other,
    }
}

/// Is this the kind of source that justifies hardware decode — and, with it,
/// a GPU filter graph?
///
/// The distinction is the whole reason light sources still take the software
/// path: the GPU decode/filter handoff is a real risk (driver bugs, surface
/// format mismatches, Dolby Vision profiles that decode to garbage), and it is
/// worth taking only where the CPU cannot cope. A 1080p H.264 scale is not a
/// problem anyone has. Software-decoding 4K 10-bit HEVC pins a core at 100%
/// and cannot finish the first segment, which is a gray screen.
///
/// Shared with [`Pipeline::for_session`] so the decode choice and the filter
/// choice are made against one definition — a GPU graph selected for a source
/// that decodes in software would be a graph with nothing on the GPU to work
/// on.
pub fn heavy_source(source: &MediaFile) -> bool {
    matches!(
        source.video_codec.as_deref(),
        Some("hevc" | "h265" | "hevc10")
    ) && (source.hdr.is_some() || source.height.unwrap_or(0) >= 2160)
}

#[cfg(test)]
fn compatibility_value_forces_software_decode(value: Option<&str>) -> bool {
    matches!(value, Some("off" | "0" | "false" | "no"))
}

fn decode_setup_with_compatibility(
    encoder: Encoder,
    source: &MediaFile,
    force_software_decode: bool,
) -> (Vec<String>, Option<String>) {
    let arg = |x: &str| x.to_owned();
    // Escape hatch: force software decode (still hardware-encodes). Set when a
    // GPU decodes a stream to garbage — some Dolby Vision profiles — so you can
    // fall back without giving up the hardware encoder.
    if force_software_decode {
        return (Vec::new(), None);
    }
    let heavy = heavy_source(source);
    // 10-bit (HDR/DV) surfaces download as p010le; 8-bit as nv12.
    //
    // An HDR source counts as 10-bit whatever the column says. `bit_depth` is
    // nullable and is `None` for any row whose probe carried no `pix_fmt`, and
    // guessing 8 there means downloading a PQ signal through `nv12` — an
    // 8-bit quantisation, re-padded to 10 bits by the chain that follows and
    // encoded Main10 with a PQ tag. Every gradient bands, at exit 0. There is
    // no HDR flavour that is genuinely 8-bit, so the flag is the safer input.
    let ten_bit = source.bit_depth.unwrap_or(0) >= 10 || source.hdr.is_some();
    let dl = if ten_bit { "p010le" } else { "nv12" };
    match encoder {
        // These families decode into system memory implicitly — no hwdownload.
        Encoder::Nvenc => (vec![arg("-hwaccel"), arg("cuda")], None),
        Encoder::VideoToolbox => (vec![arg("-hwaccel"), arg("videotoolbox")], None),
        // Intel: keep frames on the GPU through decode, then hwdownload for the
        // CPU filters. Only for the heavy case that actually needs it.
        Encoder::Qsv if heavy => (
            vec![
                arg("-hwaccel"),
                arg("qsv"),
                arg("-hwaccel_output_format"),
                arg("qsv"),
            ],
            Some(format!("hwdownload,format={dl}")),
        ),
        Encoder::Vaapi if heavy => (
            vec![
                arg("-hwaccel"),
                arg("vaapi"),
                arg("-hwaccel_output_format"),
                arg("vaapi"),
            ],
            Some(format!("hwdownload,format={dl}")),
        ),
        // Software, and the light QSV/VAAPI cases: software decode.
        _ => (Vec::new(), None),
    }
}

/// The current playback's A/V-sync correction as an audio filter, for a session whose
/// audio is being ENCODED anyway. Same sign convention as the `-itsoffset`
/// input it replaces: positive = delay audio.
///
/// The two-input technique — the same file opened again, shifted, used only
/// for its audio — exists because a COPIED audio stream cannot take a
/// filter: copy moves packets and filters need frames. But every encoded
/// path was paying its price too: a second demuxer reading the whole source
/// again, which on NAS media doubles the read traffic and competes with the
/// video path for the same spindle (review §3.4). Where the audio is
/// encoded, the correction is a one-line filter on the single input:
/// `adelay` to push audio later, `atrim` + `asetpts` to pull it earlier
/// (the trimmed head is exactly what fell before the video's start under
/// `-itsoffset`, capped by the ±15s the API allows). Copy semantics keep
/// the second input, and nothing else does.
pub fn audio_offset_filter(offset_ms: i64) -> Option<String> {
    match offset_ms.cmp(&0) {
        std::cmp::Ordering::Equal => None,
        std::cmp::Ordering::Greater => Some(format!("adelay={offset_ms}:all=1")),
        std::cmp::Ordering::Less => Some(format!(
            "atrim=start={:.3},asetpts=PTS-STARTPTS",
            -offset_ms as f64 / 1000.0
        )),
    }
}

/// The one audio `-af` an encode carries: the delivery's measured downmix,
/// then the per-file A/V correction. ffmpeg keeps only the last `-af` it is
/// given, so the two must travel as one chain or the second silently erases
/// the first.
pub fn audio_filter_chain(
    audio: Option<&crate::playback::audio::AudioDelivery>,
    offset: Option<String>,
) -> Option<String> {
    let chain: Vec<String> = audio
        .and_then(|audio| audio.downmix_filter())
        .into_iter()
        .chain(offset)
        .collect();
    (!chain.is_empty()).then(|| chain.join(","))
}

/// Build the full ffmpeg argument vector to transcode `source` into HLS in
/// `out_dir` (which must exist). Produces `index.m3u8` + `seg%05d.ts`.
#[cfg(test)]
fn hls_args(
    source: &MediaFile,
    encoder: Encoder,
    opts: &TranscodeOptions,
    pacing: Pacing,
    out_dir: &str,
) -> Vec<String> {
    hls_args_with_compatibility(source, encoder, opts, pacing, out_dir, false)
}

/// Retained HLS keyframe policy, shared by producers and their sample scorer.
pub fn hls_keyframe_args() -> Vec<String> {
    vec![
        "-force_key_frames".into(),
        format!("expr:gte(t,n_forced*{SEGMENT_SECONDS})"),
    ]
}

/// Build a movie HLS command exclusively from one validated semantic plan and
/// its attempt-local execution context.
#[cfg(not(test))]
pub fn hls_args(plan: &ResolvedTranscode, execution: &TranscodeExecution) -> Vec<String> {
    hls_args_for_plan(plan, execution)
}

/// Explicit name retained so core unit tests can exercise the plan builder
/// while their baseline-only helper keeps freezing the pre-M2 token vectors.
pub fn hls_args_for_plan(plan: &ResolvedTranscode, execution: &TranscodeExecution) -> Vec<String> {
    let media = plan.options();
    let options = TranscodeOptions {
        output_codec: media.output_codec,
        strict_dolby: media.strict_dolby.clone(),
        video_sample_envelope: media.video_sample_envelope,
        auto_quality_rate_profile: None,
        normalized_geometry: false,
        target_height: media.target_height,
        video_bitrate_kbps: media.video_bitrate_kbps,
        effective_rate_control: media.effective_rate_control,
        audio_channels: media.audio_channels,
        audio_bitrate_kbps: media.audio_bitrate_kbps,
        audio: media.audio.clone(),
        audio_index: media.audio_index,
        start_seconds: execution.start_seconds,
        start_number: execution.start_number,
        tone_map: media.tone_map,
        pipeline: media.pipeline,
        subtitle_burn: media.subtitle_burn.clone(),
        subtitle_file: execution.subtitle_file.clone(),
        force_idr: execution.force_idr,
        software_threads: execution.software_threads,
    };
    hls_args_inner(
        None,
        &execution.source_path.to_string_lossy(),
        plan.encoder(),
        &options,
        execution.pacing,
        &execution.out_dir,
        None,
        Some(plan),
        execution.diagnostics,
    )
}

#[cfg(test)]
fn hls_args_with_compatibility(
    source: &MediaFile,
    encoder: Encoder,
    opts: &TranscodeOptions,
    pacing: Pacing,
    out_dir: &str,
    force_software_decode: bool,
) -> Vec<String> {
    hls_args_inner(
        Some(source),
        &source.path.to_string_lossy(),
        encoder,
        opts,
        pacing,
        out_dir,
        Some(force_software_decode),
        None,
        DiagnosticLogging::Legacy,
    )
}

#[allow(clippy::too_many_arguments)]
fn hls_args_inner(
    legacy_source: Option<&MediaFile>,
    source_path: &str,
    encoder: Encoder,
    opts: &TranscodeOptions,
    pacing: Pacing,
    out_dir: &str,
    legacy_force_software_decode: Option<bool>,
    plan: Option<&ResolvedTranscode>,
    diagnostics: DiagnosticLogging,
) -> Vec<String> {
    let source_path = source_path.to_owned();
    let mut args: Vec<String> = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        diagnostics.flags().into(),
    ];

    // Hardware device init (VAAPI/QSV) must precede the input, and so must a
    // filter device the pipeline brings of its own (Vulkan for libplacebo,
    // OpenCL for tonemap_opencl).
    if plan.is_some_and(|plan| plan.options().strict_dolby.is_some()) {
        args.push("-xerror".into());
    }
    args.extend(
        plan.and_then(|plan| plan.options().strict_dolby.as_ref())
            .and_then(|policy| policy.linux_encoder_init_args(encoder))
            .unwrap_or_else(|| opts.pipeline.device_args(encoder)),
    );

    // Fast input seek for resume/session start.
    if opts.start_seconds > 0.0 {
        args.push("-ss".into());
        args.push(format!("{:.3}", opts.start_seconds));
    }

    // Hardware-accelerated decode (GPU decode for the heavy HEVC/4K/HDR case on
    // Intel too; see decode_setup). `hwdownload` leads the filter chain when the
    // decoder hands back hardware surfaces.
    // A vendor GPU pipeline claims the decoder itself — it needs hardware
    // surfaces of its own family to work on, and cannot start from frames that
    // were never on the GPU. Everything else keeps `decode_setup`'s answer,
    // which knows things the pipeline doesn't (source codec, bit depth, and
    // the `PLURX_HWDECODE=off` escape hatch for Dolby Vision profiles that
    // hardware-decode to garbage).
    let pipeline_decode = opts.pipeline.decode_args();
    let (decode_args, hwdownload) =
        if let Some(plan) = plan.filter(|plan| plan.options().strict_dolby.is_some()) {
            (
                plan.options()
                    .pipeline
                    .strict_dolby_input_args_for_backend(plan.decode().backend())
                    .expect("resolved strict renderer has decoder enforcement"),
                (plan.decode().backend() == DecodeBackend::Vaapi)
                    .then(|| "hwdownload,format=p010le,setparams=colorspace=unknown".to_owned()),
            )
        } else if let Some(plan) = plan {
            let decode = plan.decode();
            let mut args = decode.backend().input_args(
                matches!(
                    decode.surface().decode_domain(),
                    FrameDomain::Cuda | FrameDomain::VideoToolbox
                ) || matches!(
                    decode.backend(),
                    DecodeBackend::Qsv | DecodeBackend::Vaapi | DecodeBackend::V4l2Request
                ),
            );
            if decode.backend() == DecodeBackend::Software {
                if let Some(implementation) = decode.software_decoder() {
                    args.extend(["-c:v".to_owned(), implementation.to_owned()]);
                }
            }
            let download = decode
                .surface()
                .decoder_download_format()
                .map(|format| format!("hwdownload,format={format}"));
            (args, download)
        } else if opts.pipeline.requires_software_decode() {
            (Vec::new(), None)
        } else if pipeline_decode.is_empty() {
            decode_setup_with_compatibility(
                encoder,
                legacy_source.expect("legacy argument construction has source metadata"),
                legacy_force_software_decode.unwrap_or(false),
            )
        } else {
            (pipeline_decode, None)
        };
    args.extend(decode_args);
    if let Some(identity) = plan
        .filter(|plan| plan.decode().backend() == DecodeBackend::Vaapi)
        .and_then(|plan| plan.options().strict_dolby.as_ref())
        .and_then(StrictDolbyPolicy::linux_identity)
    {
        args.extend([
            "-hwaccel_device".to_owned(),
            identity.device_path().to_owned(),
        ]);
    }
    if plan.is_some_and(|plan| plan.output_contract().normalized_geometry().is_some()) {
        // Clearing the INPUT matrix prevents it surviving manual pixel rotation
        // into MP4 (output rotate tags alone do not remove this side data).
        // Exact worker qualification must prove this option/graph on its build.
        args.extend([
            "-noautorotate".to_owned(),
            "-display_rotation".to_owned(),
            "0".to_owned(),
        ]);
    }

    // Pace this input (see [`Pacing`]). An encode that runs faster than
    // realtime otherwise writes the whole film ahead of a playhead that will
    // never reach most of it.
    pacing.push(&mut args);
    args.push("-i".into());
    args.push(source_path.clone());

    // A per-file A/V sync correction. The audio on this path is ALWAYS
    // re-encoded (AAC below), so the correction is an audio filter on the one
    // input — the second `-itsoffset` input this used to open read the whole
    // source again for nothing but its audio (review §3.4). Guarded on the
    // file actually having audio: a simple `-af` with no audio stream to
    // attach to is a hard ffmpeg error, where the old optional map was inert.
    let audio_offset = plan.map_or_else(
        || {
            let source = legacy_source.expect("legacy argument construction has source metadata");
            (!source.audio_streams.is_empty())
                .then(|| audio_offset_filter(source.audio_offset_ms))
                .flatten()
        },
        |plan| {
            plan.options()
                .input_has_audio
                .then(|| audio_offset_filter(plan.options().audio_offset_ms))
                .flatten()
        },
    );

    // Video filter chain: [hwdownload for GPU-decoded frames →] scale / tonemap /
    // subs [→ GPU upload suffix for VAAPI/QSV] → encoder.
    //
    // A vendor GPU pipeline skips both ends. Its output is already a surface
    // of its encoder's own family, so the upload suffix would be uploading
    // something that never came down — which is not a wasted copy but a broken
    // graph. The neutral pipelines download explicitly inside their own chain
    // and then take the suffix like the CPU path does. Any subtitle burn on a
    // vendor pipeline is the exception both ways: `video_filters` appended a
    // download for libass/overlay, so the encoder's upload IS owed again.
    let subtitle_burn = opts.subtitle_burn.is_some();
    // Only an observed, identity-bound Mac burn graph owns GPU composition.
    // Legacy and other vendor paths keep their incumbent CPU subtitle step.
    let macos_gpu_burn = subtitle_burn
        && plan.is_some_and(|plan| plan.macos_processing_identity().is_some())
        && matches!(
            opts.pipeline,
            Pipeline::VtScaleSdr | Pipeline::VtToneMapMetal
        )
        && opts.pipeline.output_grade() == OutputGrade::Sdr;
    let vendor_gpu = opts.pipeline.keeps_frames_off_the_cpu() && (!subtitle_burn || macos_gpu_burn);
    let mut main_options;
    let filter_options = if macos_gpu_burn {
        main_options = opts.clone();
        main_options.subtitle_burn = None;
        &main_options
    } else {
        opts
    };
    let suffix = opts
        .pipeline
        .encoder_upload(encoder)
        .filter(|_| !vendor_gpu);
    let mut vf = String::new();
    if let Some(prefix) = &hwdownload {
        vf.push_str(prefix);
        vf.push(',');
    }
    let planned_output_size = plan.and_then(|plan| {
        Some((
            i64::from(plan.output_contract().effective_width()?),
            i64::from(plan.output_contract().effective_height()?),
        ))
    });
    let filters = plan.map_or_else(
        || {
            video_filters(
                legacy_source.expect("legacy argument construction has source metadata"),
                filter_options,
                &source_path,
            )
        },
        |plan| {
            video_filters_for_contract(
                (
                    planned_output_size,
                    plan.output_contract().normalized_geometry(),
                ),
                (
                    plan.input_hdr_format(),
                    plan.input_is_hdr(),
                    plan.routing_dynamic_range(),
                ),
                plan.deinterlace(),
                (
                    plan.options().tone_map_peak_nits,
                    plan.options().tone_map_peak_source,
                ),
                filter_options,
                &source_path,
            )
        },
    );
    vf.push_str(&filters);

    // A bitmap subtitle is a picture, so burning one is a *composite* — two
    // streams into one filter — and that needs `-filter_complex` and a mapped
    // output label rather than the single-stream `-vf`. Everything above stays
    // the chain it was; this only wraps it.
    let overlay = plan.map_or_else(
        || {
            bitmap_overlay(
                legacy_source.expect("legacy argument construction has source metadata"),
                opts,
            )
        },
        |_| bitmap_overlay_for_size(planned_output_size, opts),
    );
    // Map the chosen (or default) audio, and video from wherever it now comes.
    args.push("-map".into());
    let selected_video = plan.map_or_else(
        || "0:v:0".to_owned(),
        |plan| format!("0:{}", plan.decode().input_video_stream()),
    );
    args.push(if macos_gpu_burn || overlay.is_some() {
        BURNED_VIDEO_LABEL.to_owned()
    } else {
        selected_video.clone()
    });
    let encode_audio = plan.is_none_or(|plan| plan.options().input_has_audio);
    if encode_audio {
        args.push("-map".into());
        match opts.audio_index {
            Some(i) => args.push(format!("0:a:{i}?")),
            None => args.push("0:a:0?".to_owned()),
        }
    } else {
        args.push("-an".into());
    }

    if macos_gpu_burn {
        let filter_input = plan.map_or_else(
            || "0:v".to_owned(),
            |plan| format!("0:{}", plan.decode().input_video_stream()),
        );
        let complex = if let Some(sub) = overlay {
            let prepared = sub.replace("[sburn]", ",format=yuva420p[sburn]");
            format!("[{filter_input}]{vf}[vburn];{prepared};[vburn][sburn]overlay_videotoolbox=bitmap=1:eof_action=pass{BURNED_VIDEO_LABEL}")
        } else {
            let prepared = with_subtitles(Vec::new(), opts, &source_path).replacen(
                "subtitles=",
                "subtitles_vt_images=",
                1,
            );
            format!("[{filter_input}]{vf},split[vburn][sclock];[sclock]{prepared}[sburn];[vburn][sburn]overlay_videotoolbox=ass=1:eof_action=pass{BURNED_VIDEO_LABEL}")
        };
        assert_no_pq_at_8_bit(&complex);
        args.push("-filter_complex".into());
        args.push(complex);
    } else {
        match overlay {
            Some(sub) => {
                // The upload suffix comes AFTER the composite. `overlay` draws in
                // system memory; a chain that uploads first hands it a hardware
                // surface it cannot read. It used to be appended to the [vburn]
                // half of this very graph — upload, then overlay — which is that
                // broken order exactly, hidden by the tests only ever burning
                // with the suffix-less software encoder.
                let up = suffix.map(|s| format!(",{s}")).unwrap_or_default();
                // `overlay`'s output format defaults to 8-bit `yuv420`, and ffmpeg
                // satisfies that by auto-inserting a downconvert on the main
                // input — silently, at exit 0. On a 10-bit chain that produces
                // HEVC **Main**, 8-bit, under a playlist advertising Main10 PQ:
                // banding across every shadow gradient, and a hard decode refusal
                // on players that check the advertised profile. Naming the format
                // is the only thing that stops it.
                let overlay_format = match opts.pipeline.output_grade() {
                    OutputGrade::Hdr10 => ":format=yuv420p10",
                    OutputGrade::Sdr => "",
                };
                let filter_input = plan.map_or_else(
                    || "0:v".to_owned(),
                    |plan| format!("0:{}", plan.decode().input_video_stream()),
                );
                let complex = format!(
                    "[{filter_input}]{vf}[vburn];{sub};\
                 [vburn][sburn]overlay=eof_action=pass{overlay_format}{up}{BURNED_VIDEO_LABEL}"
                );
                assert_no_pq_at_8_bit(&complex);
                args.push("-filter_complex".into());
                args.push(complex);
            }
            None => {
                if let Some(s) = suffix {
                    vf.push(',');
                    vf.push_str(s);
                }
                assert_no_pq_at_8_bit(&vf);
                args.push("-vf".into());
                args.push(vf);
            }
        }
    }
    // The grade is read off the pipeline rather than carried beside it, so a
    // session cannot encode HEVC Main10 through a chain that ended in BT.709,
    // or the reverse.
    args.extend(encoder.encode_args_for_codec(
        plan.map_or_else(
            || {
                opts.output_codec
                    .unwrap_or(if opts.pipeline.output_grade() == OutputGrade::Hdr10 {
                        VideoCodec::Hevc
                    } else {
                        VideoCodec::H264
                    })
            },
            |plan| plan.codec_contract().codec,
        ),
        opts.pipeline.output_grade(),
        opts.video_bitrate_kbps,
        opts.effective_rate_control,
        opts.force_idr,
        opts.software_threads,
    ));
    if plan.is_some_and(|plan| plan.macos_processing_identity().is_some()) {
        // The captured Mac output contract fixes codec and grade signaling.
        // Rolling and immutable VOD consume this same recipe.
        args.extend([
            "-allow_sw".into(),
            "0".into(),
            "-color_primaries".into(),
            opts.pipeline.output_grade().primaries().into(),
            "-color_trc".into(),
            opts.pipeline.output_grade().transfer().into(),
            "-colorspace".into(),
            if opts.pipeline.output_grade() == OutputGrade::Hdr10 {
                "bt2020nc"
            } else {
                "bt709"
            }
            .into(),
            "-color_range".into(),
            "tv".into(),
        ]);
    }
    if let Some(policy) = plan.and_then(|plan| plan.output_contract().vaapi_hdr_policy()) {
        args.extend(policy.flags().map(str::to_owned));
    }
    if let Some(proof) = plan.and_then(|plan| plan.output_contract().sdr_avc()) {
        args.extend(proof.flags());
    }

    if plan
        .and_then(|plan| plan.output_contract().normalized_geometry())
        .and_then(|geometry| geometry.rate_profile)
        .is_some()
        || plan.is_some_and(|plan| {
            plan.options().video_sample_envelope == VideoSampleEnvelope::ContinuousAvcHigh50
        })
    {
        // Hardware encoders consume level_idc (50). In particular VAAPI
        // treats the unrecognized decimal alias "5.0" as integer 5, producing
        // an invalid SPS. x264 owns a string-valued level option instead.
        // Clear only the newer constraint-set bits: constrained High remains
        // valid High, and every family member must publish avc1.640032.
        args.extend([
            "-profile:v".to_owned(),
            "high".to_owned(),
            "-level:v".to_owned(),
            if encoder == Encoder::Software {
                "5.0"
            } else {
                "50"
            }
            .to_owned(),
            "-bsf:v".to_owned(),
            "h264_metadata=zero_new_constraint_set_flags=1".to_owned(),
        ]);
    }

    // Segment-aligned keyframes so each segment is independently decodable.
    args.extend(hls_keyframe_args());

    // Audio: downmix + AAC (browser-universal), with the A/V correction as
    // a filter on the same input rather than a second read of the source.
    if encode_audio {
        if let Some(af) = audio_filter_chain(opts.audio.as_ref(), audio_offset) {
            args.push("-af".into());
            args.push(af);
        }
        if let Some(audio) = &opts.audio {
            push_audio_delivery_args(&mut args, audio, false);
        } else {
            args.push("-c:a".into());
            args.push("aac".into());
            args.push("-ac".into());
            args.push(opts.audio_channels.to_string());
            args.push("-b:a".into());
            args.push(format!("{}k", opts.audio_bitrate_kbps));
            args.push("-ar".into());
            args.push(crate::playback::audio::AUDIO_SAMPLE_RATE.to_string());
        }
    }

    // Start the MPEG-TS timeline at zero.
    //
    // ffmpeg's mpegts muxer defaults to `muxpreload 0.5` + `muxdelay 0.7`, so
    // the first video PTS in segment 0 lands at ~1.4 s (measured, this exact
    // argument list, ffmpeg 6.1: 1.480 PTS / 1.400 DTS before, 0.080 / 0.000
    // after). Nothing in the video path cared — the player seeks by playlist
    // time — but the native subtitle slicer anchors each WebVTT segment with
    // `X-TIMESTAMP-MAP=MPEGTS:{segment_start × 90000}`, i.e. it asserts that
    // segment 0 begins at PTS 0. That assertion was false by 1.4 s, and every
    // native cue on a transcode session rendered 1.4 s early. Fixing the map
    // instead would encode a muxer default into the subtitle path; making the
    // assertion true is the smaller, truer change.
    args.push("-muxdelay".into());
    args.push("0".into());
    args.push("-muxpreload".into());
    args.push("0".into());

    // HLS muxer.
    args.extend(
        [
            "-f",
            "hls",
            "-hls_time",
            &SEGMENT_SECONDS.to_string(),
            "-hls_playlist_type",
            "event",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    args.extend(hls_write_args(out_dir, &["independent_segments"]));
    args.extend(
        [
            "-hls_segment_type",
            "mpegts",
            "-hls_segment_filename",
            &format!("{out_dir}/seg%05d.ts"),
            "-start_number",
            &opts.start_number.max(0).to_string(),
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    args.push(format!("{out_dir}/index.m3u8"));
    args
}

/// Whether an HLS output base is an upload endpoint rather than a directory.
///
/// A rolling session hands FFmpeg `http://127.0.0.1:<port>/<token>/<lane>`
/// so every object its muxer writes passes the daemon's scratch grant before
/// it reaches the disk (`plurxd::scratch_put`).
pub fn hls_output_is_upload(out_dir: &str) -> bool {
    out_dir.starts_with("http://")
}

/// The muxer's write-mode options for one output base.
///
/// A directory keeps `temp_file`, so a reader sees an object either absent
/// or complete. An upload endpoint gets `-method PUT` instead, and no
/// `temp_file`: FFmpeg cannot rename over HTTP and warns that it will not,
/// and the receiver renames each object into place itself. The other flags
/// are the same either way, so the objects FFmpeg produces are too.
fn hls_write_args(out_dir: &str, flags: &[&str]) -> Vec<String> {
    let mut args = Vec::new();
    if hls_output_is_upload(out_dir) {
        if !flags.is_empty() {
            args.push("-hls_flags".to_owned());
            args.push(flags.join("+"));
        }
        args.push("-method".to_owned());
        args.push("PUT".to_owned());
    } else {
        let mut flags = flags.to_vec();
        flags.push("temp_file");
        args.push("-hls_flags".to_owned());
        args.push(flags.join("+"));
    }
    args
}

/// Shared resolved source selection, decode, filters, and encoder recipe used
/// by immutable VOD before it appends its own timestamp, keyframe, and muxer
/// contracts. Semantic choices come only from `plan`; the execution carries
/// attempt-local paths, offsets, thread caps, and diagnostics.
fn encode_input_args_for_plan(
    plan: &ResolvedTranscode,
    execution: &TranscodeExecution,
) -> Vec<String> {
    let mut args = hls_args_for_plan(plan, execution);
    let presentation = args
        .iter()
        .position(|argument| argument == "-muxdelay")
        .expect("the shared encode recipe precedes the HLS presentation");
    args.truncate(presentation);
    args
}

/// Everything a copy session's ffmpeg does *before* the muxer: input seek,
/// pacing, stream mapping, the video copy with its bitstream hygiene, and the
/// audio decision.
///
/// Shared by [`hls_copy_args`] (ffmpeg's HLS muxer writes the segments) and
/// [`copy_pipe_args`] (plurx cuts the segments itself, `crate::fmp4`). The two
/// paths deliver the same bitstream by different routes, and every argument up
/// to the muxer has to be identical for that to be true — a second copy of
/// this list is a divergence waiting for the day one of them is edited.
///
/// A copied video can only begin at the preceding keyframe. When audio is
/// encoded, ffmpeg's default accurate input seek discards audio up to the
/// requested timestamp while retaining that video preroll, leaving the two
/// tracks out of sync. Copy sessions therefore use `-noaccurate_seek` so both
/// tracks retain the same preroll and `-avoid_negative_ts make_zero` can shift
/// them onto one shared output timeline.
/// ffprobe arguments that answer the one question a copy session cannot
/// answer from its own request: **where does its timeline actually begin in
/// the source?**
///
/// A copy session seeks with `-noaccurate_seek` (see below), so the media it
/// emits starts at the keyframe *preceding* the requested offset, and
/// `-avoid_negative_ts make_zero` then relabels that keyframe as t=0. The
/// requested start and the real origin therefore differ by anything from zero
/// to a full GOP — 1–6 s on a 4K film — and anything computed against the
/// requested start (subtitle cue shifts, most of all) is wrong by exactly that
/// much.
///
/// `-read_intervals "{start}%+#4"` makes ffprobe perform the same backward
/// seek the demuxer will, then read four packets: no decoding, no scan, one
/// seek and a handful of packets even on a 19 GB remux over a NAS.
pub fn keyframe_probe_args(source_path: &str, start_seconds: f64) -> Vec<String> {
    vec![
        "-hide_banner".into(),
        "-v".into(),
        "error".into(),
        "-select_streams".into(),
        "v:0".into(),
        "-show_entries".into(),
        "packet=pts_time,dts_time,flags".into(),
        "-read_intervals".into(),
        format!("{start_seconds:.3}%+#4"),
        "-of".into(),
        "csv=p=0".into(),
        source_path.to_owned(),
    ]
}

/// The arguments that decide the **video** bytes of a copy pipe.
///
/// Extracted so the index pipe and the production pipe cannot drift: an index
/// is a list of output byte counts, and the landing matcher compares them, so
/// an index built by a subtly different video branch is not merely stale — it
/// is confidently wrong. Sharing the code is stronger than a test asserting
/// the two agree.
///
/// This is also what [`crate::segplan::argv_fingerprint`] is fed, which is why
/// it deliberately excludes the input path: a moved file is caught by its
/// identity, and re-indexing every file after a library move would be a cost
/// with no finding behind it.
pub fn copy_video_args(source: &MediaFile, options: CopyVideoOptions) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    args.push("-c:v".into());
    args.push("copy".into());
    // Keep the sample-entry contract shared by HLS admission and its CODECS
    // declaration. Retaining source updates does not authorize hev1/dvhe on
    // a client that only admits hvc1/dvh1; strict init validation still
    // refuses a retaining stream that cannot satisfy this contract.
    if matches!(source.video_codec.as_deref(), Some("hevc" | "h265")) {
        args.push("-tag:v".into());
        args.push(hevc_copy_tag_for_source(source, options.preserve_dolby_vision).into());
        // FFmpeg's MOV muxer guards dvcC/dvvC behind `unofficial`. Without
        // this, it keeps the Dolby Vision RPUs and writes a `dvh1` sample
        // entry but silently omits the decoder configuration box. A media
        // playlist can then appear to play through the HDR10 base layer,
        // while an HLS master that correctly advertises `dvh1.08.06` fails
        // AVPlayer with CoreMedia -12927. The strictness option is scoped to
        // preserved DV; ordinary HEVC and stripped HDR10 copies do not need
        // an experimental muxer feature.
        if source.hdr.as_deref() == Some("dolby_vision") && options.preserve_dolby_vision {
            args.push("-strict".into());
            args.push("unofficial".into());
        }
        // A non-backward-compatible Dolby Vision stream is tagged `dvh1`, so
        // its VPS/SPS/PPS must be present in hvcC before AVPlayer opens the
        // first fragment. Some WEB-DL Matroska files carry a minimal, empty
        // hvcC and repeat those parameter sets only in-band. Merely retaining
        // the sets is insufficient: FFmpeg's MOV muxer consumes them while
        // still emitting the source's 23-byte hvcC. Convert the packets to
        // Annex B and extract their parameter sets first, which gives the
        // muxer complete codec extradata and keeps the served `hvc1`/`dvh1`
        // promise honest.
        let promote_profile5_parameter_sets = source.hdr.as_deref() == Some("dolby_vision")
            && options.preserve_dolby_vision
            && !dolby_vision_has_compatible_base(source);
        if options.promote_hevc_parameter_sets {
            let mut filters = Vec::new();
            if source.hdr.as_deref() == Some("dolby_vision")
                && !options.preserve_dolby_vision
                && options.have_dovi_bsf
            {
                filters.push("dovi_rpu=strip=1");
            }
            filters.push("hevc_mp4toannexb");
            filters.push("extract_extradata");
            if source.hdr.as_deref() == Some("dolby_vision") && !options.preserve_dolby_vision {
                filters.push("filter_units=remove_types=62-63");
            } else if source.hdr.as_deref() == Some("dolby_vision") && options.dv_convert {
                // The enhancement layer goes here too. This branch rebuilds
                // the hvcC for a source whose parameter sets live only in
                // band, and it is reached by a Profile 7 title with a 23-byte
                // hvcC like any other — leaving type 63 in would ship orphan
                // enhancement-layer units behind RPUs that no longer
                // reference them, while the configuration record plurx writes
                // says `el_present = false`. Type 62 stays: those are what the
                // rewrite after the muxer converts.
                filters.push("filter_units=remove_types=63");
            }
            args.push("-bsf:v".into());
            args.push(filters.join(","));
        } else if !promote_profile5_parameter_sets {
            args.push("-bsf:v".into());
            args.push(hevc_copy_bsf_for_copy_retaining(
                source.hdr.as_deref(),
                options.have_dovi_bsf,
                options.preserve_dolby_vision,
                options.dv_convert,
                options.retain_hevc_parameter_sets,
            ));
        }
    }
    // The conversion is not an ffmpeg argument — it happens after this child,
    // in `transcode::dvconvert`, on the fragments the muxer wrote — but it has
    // to appear here, and this is the honest place for it.
    //
    // `copy_video_args` is what the fragment index fingerprints. A converted
    // stream has different bytes and therefore different segment boundaries,
    // so it needs its own index identity; without a token in the argv it would
    // silently share the unconverted stream's, and a client would be handed a
    // playlist whose cut points describe different media. The token is
    // stripped before exec by [`strip_plurx_markers`].
    //
    // Outside the HEVC branch, and gated on the source actually being Dolby
    // Vision, for the two reasons those are not the same reason: a non-HEVC
    // source with the flag set would otherwise fingerprint identically to one
    // without it, so a converted index could alias an unconverted one; and a
    // plain HDR10 HEVC source with the flag set would otherwise earn a third
    // index identity for a pipeline byte-identical to the second. Both are
    // unreachable through today's decider, which is exactly why the argv
    // builder should not depend on that.
    if options.dv_convert && source.hdr.as_deref() == Some("dolby_vision") {
        args.push(DV_CONVERT_MARKER.into());
    }
    // Retention changes the bytes on every branch, including the two that
    // emit no 32-34 filter at all (Profile 5 preservation and empty-hvcC
    // promotion): there it is the segmenter's merge that stops stripping.
    // The filter string cannot carry that, so the recipe does.
    if options.retain_hevc_parameter_sets
        && matches!(source.video_codec.as_deref(), Some("hevc" | "h265"))
    {
        args.push(HEVC_RETAIN_PARAMETER_SETS_MARKER.into());
    }
    args
}

/// The marker `copy_video_args` carries for a copy that keeps its in-band HEVC
/// parameter sets ([`CopyVideoOptions::retains_hevc_parameter_sets`]).
///
/// Like [`DV_CONVERT_MARKER`], fingerprinted and then removed before exec.
pub const HEVC_RETAIN_PARAMETER_SETS_MARKER: &str = "--plurx-hevc-parameter-sets=retain";

/// The marker `copy_video_args` carries for a Profile 7 → 8.1 conversion.
///
/// Deliberately not a valid ffmpeg option: it exists to be fingerprinted and
/// then removed, and anything that reached an exec would fail loudly rather
/// than being interpreted.
pub const DV_CONVERT_MARKER: &str = "--plurx-dv-convert=p7-to-p81";

/// Remove plurx's own markers from an argv before it is executed.
///
/// The argv is two things at once: the recipe the fragment index is keyed by,
/// and the command line ffmpeg receives. Where those disagree — a stage plurx
/// runs itself, on the far side of the muxer — the recipe carries a token and
/// the command line does not.
pub fn strip_plurx_markers(args: &[String]) -> Vec<String> {
    args.iter()
        .filter(|arg| !arg.starts_with("--plurx-"))
        .cloned()
        .collect()
}

/// The source-timeline origin implied by [`keyframe_probe_args`] output.
///
/// Prefers the first keyframe packet's decode timestamp, because that is the
/// value `-avoid_negative_ts make_zero` subtracts to build the output
/// timeline; the presentation timestamp is the fallback for a demuxer that
/// reports no DTS. `None` means the probe said nothing usable, and the caller
/// should fall back to the requested start — which is today's behaviour, so a
/// failed probe is no worse than not probing.
pub fn parse_keyframe_origin(stdout: &str) -> Option<f64> {
    let rows = stdout.lines().filter_map(|line| {
        let mut fields = line.trim().split(',');
        let pts = fields.next()?.trim();
        let dts = fields.next()?.trim();
        let flags = fields.next().unwrap_or("").trim();
        let timestamp = dts
            .parse::<f64>()
            .ok()
            .or_else(|| pts.parse::<f64>().ok())?;
        Some((timestamp, flags.contains('K')))
    });
    let mut first = None;
    for (timestamp, keyframe) in rows {
        if keyframe {
            return Some(timestamp.max(0.0));
        }
        first.get_or_insert(timestamp.max(0.0));
    }
    first
}

pub fn copy_input_seek_args(start_seconds: f64) -> Vec<String> {
    if start_seconds <= 0.0 {
        return Vec::new();
    }
    vec![
        "-noaccurate_seek".into(),
        "-ss".into(),
        format!("{start_seconds:.3}"),
    ]
}

fn copy_audio_channels(source: &MediaFile, audio_index: Option<i64>) -> Option<i64> {
    match audio_index {
        Some(index) => source
            .audio_streams
            .iter()
            .find(|stream| stream.index == index),
        None => source.audio_streams.first(),
    }
    .and_then(|stream| stream.channels)
}

/// One typed delivery builds the same audio tokens on encoded and copy-video
/// paths. The historical copy-video AAC conversion omits `-ac` and places
/// its explicit 5.1 layout after the bitrate; retain that exact shape.
pub fn push_audio_delivery_args(
    args: &mut Vec<String>,
    audio: &crate::playback::audio::AudioDelivery,
    legacy_copy_conversion: bool,
) {
    use crate::playback::audio::AudioAction;
    match &audio.action {
        AudioAction::None => args.push("-an".into()),
        AudioAction::Copy { .. } => args.extend(["-c:a".into(), "copy".into()]),
        AudioAction::Encode {
            codec,
            channels,
            layout,
            bitrate_kbps,
            sample_rate,
        } => {
            args.extend(["-c:a".into(), codec.clone()]);
            if !legacy_copy_conversion {
                args.extend(["-ac".into(), channels.to_string()]);
                if let Some(layout) = layout {
                    args.extend(["-channel_layout:a".into(), layout.clone()]);
                }
            }
            args.extend(["-b:a".into(), format!("{bitrate_kbps}k")]);
            if legacy_copy_conversion {
                if let Some(layout) = layout {
                    args.extend(["-channel_layout:a".into(), layout.clone()]);
                }
            }
            args.extend(["-ar".into(), sample_rate.to_string()]);
            // A measured downmix is a filter, emitted by the caller's single
            // `-af` (see `audio_filter_chain`); `-ac` above is then a no-op
            // confirmation of the stereo the chain already produced. The
            // incumbent `RequiresLayoutMeasurement` fold has no filter and
            // is performed by `-ac` alone, exactly as before.
        }
    }
}

fn copy_input_args(
    source: &MediaFile,
    start_seconds: f64,
    audio_index: Option<i64>,
    transcode_audio: bool,
    pacing: Pacing,
    video: CopyVideoOptions,
    audio: Option<&crate::playback::audio::AudioDelivery>,
) -> Vec<String> {
    let transcode_audio = audio.map_or(transcode_audio, |audio| audio.transcodes());
    let source_path = source.path.to_string_lossy().into_owned();
    let mut args: Vec<String> = vec!["-hide_banner".into(), "-loglevel".into(), "error".into()];

    // Fast input seek for resume/session start. The non-accurate mode keeps
    // encoded audio aligned with the keyframe preroll of the copied video.
    args.extend(copy_input_seek_args(start_seconds));
    // Copy runs as fast as the disk allows, so it has to be paced — but *not*
    // at 1x, which is what a bare `-re` here used to do. Realtime pacing meant
    // the segments existed exactly as fast as they were consumed, so a 4K
    // session's runway was whatever the player fetched before it started and
    // never grew: the "starts fine, buffers ten seconds in" report, and the
    // reason an Apple TV (which wants ~3 segments before it plays) took a
    // dozen seconds to start. The disk is bounded by the ahead-window suspend
    // and the behind-playhead GC now, not by starving the viewer.
    pacing.push(&mut args);
    args.push("-i".into());
    args.push(source_path.clone());

    // A per-file A/V sync correction (positive = audio later). COPIED audio
    // cannot take a filter — copy moves packets, filters need frames — so
    // that case keeps the second `-itsoffset`'d input of the same file, used
    // only for its audio. Audio that is being transcoded anyway takes the
    // correction as a filter on the one input instead: the second demuxer
    // read the whole source again — on NAS media, double the read traffic in
    // exactly the sessions that need the disk most (review §3.4).
    let has_offset = source.audio_offset_ms != 0 && !source.audio_streams.is_empty();
    let audio_input = if has_offset && !transcode_audio {
        args.extend(copy_input_seek_args(start_seconds));
        pacing.push(&mut args);
        args.push("-itsoffset".into());
        args.push(format!("{:.3}", source.audio_offset_ms as f64 / 1000.0));
        args.push("-i".into());
        args.push(source_path.clone());
        1
    } else {
        0
    };

    // Copy video when present; map the chosen (or first) audio; drop subs.
    // The optional video map is what lets the same established remux path
    // deliver an audio-only audiobook whose codec needs conversion to AAC.
    //
    // And drop CHAPTERS, which `-map` does not govern. ffmpeg's mp4 muxer
    // writes a source's chapters as a QuickTime `text` track plus a `chpl`
    // box, with `tref` links from the real tracks — a third track, in the
    // init and in every fragment, that nothing asked for. ffmpeg's own HLS
    // muxer never carried it, so the copy path never had it; the segmenter
    // reads a plain `-f mp4` pipe, which does. Safari refused the resulting
    // stream outright (`MEDIA_ERR_DECODE`, 708 ms in) and the player's error
    // fallback re-encoded a 4K remux down to 1080p. Chrome ignored the extra
    // track entirely, which is why only real hardware found it — and every
    // disc remux has chapters while no synthetic fixture does.
    //
    // Nothing is lost: plurx serves chapter markers from ffprobe at playback
    // start (ARCHITECTURE §7 decision 6), never from the media stream.
    args.push("-map_chapters".into());
    args.push("-1".into());
    args.push("-map".into());
    args.push("0:v:0?".into());
    args.push("-map".into());
    match audio_index {
        Some(i) => args.push(format!("{audio_input}:a:{i}?")),
        None => args.push(format!("{audio_input}:a:0?")),
    }
    args.push("-sn".into());

    args.extend(strip_plurx_markers(&copy_video_args(source, video)));

    if let Some(audio) = audio {
        let offset = if has_offset && transcode_audio {
            audio_offset_filter(source.audio_offset_ms)
        } else {
            None
        };
        if let Some(af) = audio_filter_chain(Some(audio), offset) {
            args.extend(["-af".into(), af]);
        }
        // Preserve the incumbent copy conversion's exact spelling when its
        // byte semantics match. Explanatory reason text must never select
        // argv, because it is deliberately excluded from recipe identity.
        let legacy_conversion = match &audio.action {
            crate::playback::audio::AudioAction::Encode {
                codec,
                channels,
                layout,
                bitrate_kbps,
                sample_rate,
            } => {
                codec == "aac"
                    && copy_audio_channels(source, audio_index) == Some(i64::from(*channels))
                    && *bitrate_kbps == if *channels == 6 { 320 } else { 256 }
                    && layout.as_deref() == if *channels == 6 { Some("5.1") } else { None }
                    && *sample_rate == 48_000
                    && audio.downmix.is_none()
            }
            _ => false,
        };
        push_audio_delivery_args(&mut args, audio, legacy_conversion);
    } else if transcode_audio {
        // The correction rides the encode as a filter — same input, no
        // second read.
        if has_offset {
            if let Some(af) = audio_offset_filter(source.audio_offset_ms) {
                args.push("-af".into());
                args.push(af);
            }
        }
        // e.g. DTS/TrueHD → AAC. Six-channel disc audio is commonly tagged
        // `5.1(side)`. ffmpeg's AAC encoder represents that layout with an
        // in-band Program Config Element (channel_configuration=0), while its
        // fragmented-MP4 sample entry still declares two channels. AVPlayer
        // rejects that contradictory initialization record with CoreMedia
        // -12848 before it opens the video. Standardize six-channel output on
        // AAC's channel_configuration=6 layout; the rematrix only moves the
        // surround pair from side to back labels and preserves 5.1 playback.
        args.push("-c:a".into());
        args.push("aac".into());
        args.push("-b:a".into());
        if copy_audio_channels(source, audio_index) == Some(6) {
            // Apple's HLS authoring recommendation for 5.1 AAC is 320 kbit/s.
            args.push("320k".into());
            args.push("-channel_layout:a".into());
            args.push("5.1".into());
        } else {
            args.push("256k".into());
        }
        args.push("-ar".into());
        args.push(crate::playback::audio::AUDIO_SAMPLE_RATE.to_string());
    } else {
        args.push("-c:a".into());
        args.push("copy".into());
    }
    args
}

/// Build the **index** pipe: the production copy pipe, video only, from the
/// start of the file, unpaced.
///
/// This is what builds a file's fragment index ([`crate::segplan`]). Three
/// deliberate differences from the production pipe, each with its reason:
///
/// - **video only** (`-an`, no audio map). The fragment *sequence* is
///   identical for every audio selection — count, verdicts, durations and
///   output byte counts matched on 9 of 9 M0 fixtures — so one index serves
///   them all. The *timeline* is not: the production pipe's DTS grid sits a
///   constant, audio-branch-dependent offset ahead of this one, which is why
///   nothing downstream addresses fragments by timestamp.
/// - **from zero**, never `-ss`. An index describes the whole file.
/// - **unpaced.** Pacing rate-limits the input to protect a live session's
///   supply; an index build is background work with no viewer behind it, and
///   throttling it would only make it take longer.
pub fn copy_index_pipe_args(source: &MediaFile, video: CopyVideoOptions) -> Vec<String> {
    copy_index_pipe_args_with_input(source, &source.path.to_string_lossy(), video)
}

/// The index pipe with an explicitly named input. Cluster index workers use a
/// fixed inherited descriptor here, keeping the file they attested attached
/// to ffmpeg even if the pathname is replaced before the child opens it.
///
/// This argv is an identity, not just a command: `pipeline_digest_for_transform`
/// hashes it into every cluster cache key. It is the one-identity case of
/// [`copy_index_shared_args_with_input`], and a test holds the two equal.
pub fn copy_index_pipe_args_with_input(
    source: &MediaFile,
    input: &str,
    video: CopyVideoOptions,
) -> Vec<String> {
    copy_index_shared_args_with_input(source, input, &[video])
}

/// One index pass that writes several copy identities from one read of the
/// input: the global options and `-i` once, then one output block per
/// identity. `videos[0]` writes to `pipe:1` and carries the HEVC header trace;
/// `videos[i]` writes to `pipe:{3 + i}` — fds 4, 5, … — and carries none.
///
/// Every option after `-i` is an output option, so each block is byte for
/// byte what the one-identity argv emits after `-i`, with only its URL
/// changed and, on every block but the first, no `trace_headers`. The trace
/// reads the packets before any other filter in the chain, so it describes
/// the source's parameter sets, which every identity of the file shares; one
/// trace on stderr is one proof. A trace per block would interleave N traces
/// into one parser.
pub fn copy_index_shared_args_with_input(
    source: &MediaFile,
    input: &str,
    videos: &[CopyVideoOptions],
) -> Vec<String> {
    let hevc = matches!(source.video_codec.as_deref(), Some("hevc" | "h265"));
    let mut args: Vec<String> = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        if hevc { "info" } else { "error" }.into(),
    ];
    if hevc {
        args.push("-xerror".into());
    }
    args.push("-i".into());
    args.push(input.to_owned());
    for (at, video) in videos.iter().enumerate() {
        let url = if at == 0 {
            "pipe:1".to_owned()
        } else {
            format!("pipe:{}", 3 + at)
        };
        args.extend(copy_index_output_args(source, *video, &url, at == 0));
    }
    args
}

/// One identity's output block of the index pipe, writing to `url`. `trace`
/// prepends `trace_headers` to an HEVC block's bitstream filters; see
/// [`copy_index_shared_args_with_input`] for why only one block carries it.
pub fn copy_index_output_args(
    source: &MediaFile,
    video: CopyVideoOptions,
    url: &str,
    trace: bool,
) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-map_chapters".into(),
        "-1".into(),
        "-map".into(),
        "0:v:0?".into(),
        "-an".into(),
        "-sn".into(),
    ];
    args.extend(strip_plurx_markers(&copy_video_args(source, video)));
    args.extend(
        [
            "-avoid_negative_ts",
            "make_zero",
            "-movflags",
            "frag_keyframe+empty_moov+default_base_moof+delay_moov",
            "-use_editlist",
            "0",
            "-f",
            "mp4",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    args.push(url.to_owned());
    if trace && matches!(source.video_codec.as_deref(), Some("hevc" | "h265")) {
        if let Some(index) = args.iter().position(|arg| arg == "-bsf:v") {
            args[index + 1] = format!("trace_headers,{}", args[index + 1]);
        } else {
            // Insert before the output URL so this remains an output option.
            let at = args.len() - 1;
            args.splice(at..at, ["-bsf:v".to_owned(), "trace_headers".to_owned()]);
        }
    }
    args
}

/// Build ffmpeg args for a copy session that plurx segments itself.
///
/// One continuous fragmented MP4 down stdout — `frag_keyframe` puts one
/// fragment per GOP on the wire, `delay_moov` lets the `moov` describe the
/// real stream instead of a guess, and `default_base_moof` keeps every sample
/// offset relative to its own `moof` so a reader that has never seen the file
/// start can resolve them. `crates/plurx-core/src/fmp4.rs` reads that stream
/// and decides where the segment boundaries go; the point of moving the cut
/// out of ffmpeg is that a boundary can then be placed only in front of a
/// keyframe no player will discard a leading picture at.
///
/// `-avoid_negative_ts make_zero` comes from the progressive remux, for its
/// reasons: an input seek leaves the first packet's timestamp wherever the
/// keyframe was, and a fragmented MP4 whose timeline does not start at zero is
/// a stream players disagree about.
///
/// Apple HLS deliberately does not use edit lists for fragmented MP4
/// compatibility. Generic MP4 muxing otherwise writes `edts`/`elst` boxes to
/// express the seek preroll; AVPlayer accepts that file directly but rejects
/// the same rendition during multivariant HLS validation. Keep the normalized
/// fragment decode times and suppress those movie-level edits.
pub fn copy_pipe_args(
    source: &MediaFile,
    start_seconds: f64,
    audio_index: Option<i64>,
    transcode_audio: bool,
    pacing: Pacing,
    have_dovi_bsf: bool,
) -> Vec<String> {
    copy_pipe_args_with_dolby_vision(
        source,
        start_seconds,
        audio_index,
        transcode_audio,
        pacing,
        CopyVideoOptions::new(have_dovi_bsf, false),
    )
}

pub fn copy_pipe_args_with_dolby_vision(
    source: &MediaFile,
    start_seconds: f64,
    audio_index: Option<i64>,
    transcode_audio: bool,
    pacing: Pacing,
    video: CopyVideoOptions,
) -> Vec<String> {
    copy_pipe_args_with_audio_delivery(
        source,
        start_seconds,
        audio_index,
        transcode_audio,
        pacing,
        video,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn copy_pipe_args_with_audio_delivery(
    source: &MediaFile,
    start_seconds: f64,
    audio_index: Option<i64>,
    transcode_audio: bool,
    pacing: Pacing,
    video: CopyVideoOptions,
    audio: Option<&crate::playback::audio::AudioDelivery>,
) -> Vec<String> {
    let mut args = copy_input_args(
        source,
        start_seconds,
        audio_index,
        transcode_audio,
        pacing,
        video,
        audio,
    );
    args.extend(
        [
            "-avoid_negative_ts",
            "make_zero",
            "-movflags",
            "frag_keyframe+empty_moov+default_base_moof+delay_moov",
            "-use_editlist",
            "0",
            "-f",
            "mp4",
            "pipe:1",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    args
}

/// Build ffmpeg args to *copy* the source video into HLS (fMP4 segments) with
/// ffmpeg's own HLS muxer doing the cutting, transcoding only the audio when
/// the client can't take the source codec.
///
/// This is "remux, packaged as HLS". Safari's `<video>` will not play a
/// progressive fragmented MP4 (the `/stream.mp4` remux) — it only accepts
/// fragmented content via HLS — but it decodes HEVC/HDR natively through HLS.
/// So for those clients we keep the original 4K video stream untouched and
/// repackage it as HLS, instead of letting the player's error-fallback
/// re-encode the whole thing down to 720p. fMP4 segments (not MPEG-TS) are
/// required: Apple does not support HEVC in a TS container.
///
/// Since the GOP-aware segmenter this is the FALLBACK path — taken for a
/// source whose keyframes `crate::fmp4` cannot read, and for a stream it
/// turned out not to be able to follow. It cuts wherever it finds a keyframe
/// past the floor, which on an open-GOP source costs one frame per boundary
/// (docs/streaming/STUTTER-4K.md §5.6). That is the behaviour the segmenter exists to
/// improve on, and the behaviour anything unexpected degrades back to.
pub fn hls_copy_args(
    source: &MediaFile,
    start_seconds: f64,
    audio_index: Option<i64>,
    transcode_audio: bool,
    pacing: Pacing,
    have_dovi_bsf: bool,
    out_dir: &str,
) -> Vec<String> {
    hls_copy_args_with_dolby_vision(
        source,
        start_seconds,
        audio_index,
        transcode_audio,
        pacing,
        DolbyVisionCopyOptions::new(have_dovi_bsf, false),
        out_dir,
    )
}

pub fn hls_copy_args_with_dolby_vision(
    source: &MediaFile,
    start_seconds: f64,
    audio_index: Option<i64>,
    transcode_audio: bool,
    pacing: Pacing,
    dolby_vision: DolbyVisionCopyOptions,
    out_dir: &str,
) -> Vec<String> {
    hls_copy_args_with_sequence(
        source,
        start_seconds,
        audio_index,
        transcode_audio,
        pacing,
        dolby_vision,
        0,
        "init.mp4",
        out_dir,
    )
}

/// Build copy-HLS arguments for a fenced successor generation. The init name
/// and segment number are generation-specific so a stable capability URL can
/// never make a previously cached URI name different bytes after takeover.
#[allow(clippy::too_many_arguments)]
pub fn hls_copy_args_with_sequence(
    source: &MediaFile,
    start_seconds: f64,
    audio_index: Option<i64>,
    transcode_audio: bool,
    pacing: Pacing,
    dolby_vision: DolbyVisionCopyOptions,
    start_number: i64,
    init_filename: &str,
    out_dir: &str,
) -> Vec<String> {
    hls_copy_args_with_audio_delivery(
        source,
        start_seconds,
        audio_index,
        transcode_audio,
        pacing,
        dolby_vision,
        start_number,
        init_filename,
        out_dir,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn hls_copy_args_with_audio_delivery(
    source: &MediaFile,
    start_seconds: f64,
    audio_index: Option<i64>,
    transcode_audio: bool,
    pacing: Pacing,
    dolby_vision: DolbyVisionCopyOptions,
    start_number: i64,
    init_filename: &str,
    out_dir: &str,
    audio: Option<&crate::playback::audio::AudioDelivery>,
) -> Vec<String> {
    let mut args = copy_input_args(
        source,
        start_seconds,
        audio_index,
        transcode_audio,
        pacing,
        dolby_vision,
        audio,
    );

    // fMP4 HLS. Segments split at existing keyframes (copy can't force them), so
    // a normally-GOP'd source segments cleanly; the init segment is `init.mp4`.
    //
    // NO `independent_segments` here, and the omission is the point. That tag
    // declares every segment independently decodable, which is TRUE for the
    // transcode path (closed GOP, forced IDR) and FALSE for a copy of an
    // open-GOP source: each segment opens with a CRA whose RASL leading
    // picture needs the previous segment's frames. A player that believes the
    // tag may treat every segment start as a random-access point, and the
    // HEVC spec's instruction at random access is to DISCARD the leading
    // pictures — one dropped frame per segment boundary, which is exactly
    // the measured residual stutter (22 drops in 45s, 15/23 within 150ms of
    // a boundary, docs/streaming/STUTTER-4K.md §5.5). The claim was a lie on this path;
    // whether any given player acts on it, a lie in a spec tag is not a
    // thing to keep shipping.
    args.extend(
        [
            "-f",
            "hls",
            "-hls_time",
            &COPY_SEGMENT_SECONDS.to_string(),
            "-hls_playlist_type",
            "event",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    args.extend(hls_write_args(out_dir, &[]));
    args.extend(
        [
            "-hls_segment_type",
            "fmp4",
            "-hls_fmp4_init_filename",
            init_filename,
            "-hls_segment_filename",
            &format!("{out_dir}/seg%05d.m4s"),
            "-start_number",
            &start_number.max(0).to_string(),
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    args.push(format!("{out_dir}/index.m3u8"));
    args
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod index_pipe_tests;

#[cfg(test)]
mod upload_output_tests;
