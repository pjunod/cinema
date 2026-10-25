//! How one session's video gets from an HDR source to SDR output.
//!
//! Some paths decode through an accelerator and transfer frames to system memory,
//! tone-map through a float chain on the CPU, and upload it back for the
//! encoder. PERF-PLAN §2.9 measured what that costs on identical work — same
//! resolution, same codec, same rung, same encoder, the only difference being
//! this chain:
//!
//! ```text
//!   4k-hevc-sdr   0.98x p10    4K decode, no tone-map
//!   4k-hdr10      0.71x p10    4K decode, CPU float tone-map
//! ```
//!
//! A quarter of the pipeline's throughput, and it is the quarter that takes a
//! session from above realtime to below it — which is the whole difference
//! between a stream that builds reserve and one that drains the viewer's.
//! A hardware-surface graph avoids that explicit transfer/filter sequence.
//! Frame domains do not establish physical copies or their cost, especially
//! on Apple unified memory; complete-graph experiments establish the benefit.
//!
//! Which one a node uses is decided by *probe*, never by version sniffing or
//! by what the hardware claims. A graph that parses is not a graph that works:
//! it can move frames and still produce clipped gray, or lose the HDR metadata
//! at the decoder, or mistag the output, or be slower than the CPU chain it
//! replaced. `plurxd`'s pipeline probe checks each of those against real HDR10
//! and only then lets a node use a graph (PERF-PLAN §5).

use super::{Encoder, OutputGrade};
use crate::domain::ScanType;

/// The independently reconstructed renderer's exact input, never source HDR.
/// This contract does not participate in ordinary pipeline negotiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconstructedPqInput {
    /// BT.2020 NCL limited-range 4:2:0 10-bit PQ, converted on the GPU.
    Yuv420p10Bt2020NclPq,
    /// Display-targeted BT.709, pure gamma 2.4, limited-range 10-bit.
    Yuv420p10Bt709Gamma24,
}
impl ReconstructedPqInput {
    pub fn pixel_format(self) -> &'static str {
        "yuv420p10le"
    }
    pub fn transfer(self) -> &'static str {
        match self {
            Self::Yuv420p10Bt2020NclPq => "smpte2084",
            Self::Yuv420p10Bt709Gamma24 => "bt709",
        }
    }
    pub fn primaries(self) -> &'static str {
        match self {
            Self::Yuv420p10Bt2020NclPq => "bt2020",
            Self::Yuv420p10Bt709Gamma24 => "bt709",
        }
    }
    pub fn matrix(self) -> &'static str {
        match self {
            Self::Yuv420p10Bt2020NclPq => "bt2020nc",
            Self::Yuv420p10Bt709Gamma24 => "bt709",
        }
    }
    pub(super) fn pairs_with(self, encoder: Encoder) -> bool {
        matches!(encoder, Encoder::Software | Encoder::Nvenc | Encoder::Vaapi)
    }
}

/// The video path for one session, from decoded frames to the encoder's input.
///
/// The boot probe walks [`CANDIDATES`] from most to least preferred, and every
/// GPU graph — the Apple Metal tone-map included — must match the CPU chain's
/// picture there before a node may claim it. Sessions reach the Apple graphs
/// only through their separate runtime compatibility context
/// ([`Pipeline::requires_macos_context`]). [`Pipeline::Cpu`] is last and
/// unconditional — it needs no hardware, it is what every other variant falls
/// back to, and it is the reference the others are checked against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Pipeline {
    /// VideoToolbox surfaces → native SDR scale → H.264 VideoToolbox encode.
    /// Selected only by the macOS compatibility context in the resolver.
    VtScaleSdr,
    /// HDR VideoToolbox surfaces → native P010 scale → Jellyfin Metal mapping
    /// → H.264 VideoToolbox encode. PQ input takes the CPU chain's operator
    /// and HLG keeps BT.2390/ITP ([`Pipeline::tone_map_terms`]). HDR10 and HLG
    /// require independent complete-graph observations, PQ additionally the
    /// boot probe's CPU parity; Dolby is excluded.
    VtToneMapMetal,
    /// Ordinary HDR10 P010 native scale, preserving PQ and static metadata.
    VtScaleHdr10,
    /// Strict Profile5 current-AU metadata, software HEVC → CPU renderer.
    DoviStrictTonemapx,
    /// Strict hardware HEVC → P010 download → CPU Dolby renderer.
    VtDoviTonemapx,
    /// Strict hardware HEVC → Dolby Metal rendering at source raster → scale.
    VtDoviMetal,
    /// Strict software HEVC → explicit upload → Dolby Metal rendering → scale.
    DoviMetal,
    /// NVDEC → CUDA tone-map/scale → NVENC, without a system-memory handoff.
    TonemapCuda,
    /// Intel, frames never leave the GPU: `vpp_qsv` scales and tone-maps in
    /// one pass on the video-processing block.
    VppQsv,
    /// The VA-API equivalent — Intel, and AMD where the driver implements it.
    TonemapVaapi,
    /// VA-API decode and encode around Vulkan tone mapping. Hardware frames
    /// are imported by libplacebo and mapped back to VA-API without a CPU
    /// download. Probed separately because not every driver supports mapping.
    LibplaceboVaapi,
    /// Vulkan, vendor-neutral. The likely answer for the AMD boxes, whose VCN
    /// has no tone-map block of its own. Already reachable as
    /// `PLURX_TONEMAP=libplacebo`; the probe is what turns a blind preference
    /// into a checked capability.
    Libplacebo,
    /// Vulkan shaders with system-memory AVFrames on both sides. The filter
    /// owns its uploads; no external Vulkan frame context is required.
    LibplaceboSoftware,
    /// OpenCL, jellyfin-ffmpeg's portable alternate. Needs an explicit
    /// download after the map — `tonemap_opencl` outputs OpenCL surfaces the
    /// H.264 encoders cannot take directly.
    TonemapOpencl,
    /// Dolby Vision RPU reshaping through Jellyfin FFmpeg's SIMD software
    /// filter. This is intentionally separate from the ordinary HDR10
    /// candidate list: it has a different decode contract and must never fall
    /// back to the non-Dolby-aware CPU zscale graph.
    DoviTonemapx,
    /// Dolby Vision RPU application with **no tone-map at all**: the same
    /// SIMD filter run in its HDR passthrough mode, emitting HEVC Main10 PQ
    /// instead of BT.709 8-bit. The rung between "preserve Dolby Vision" and
    /// "720p SDR H.264", for a client that cannot decode DV but can decode
    /// Main10 PQ.
    ///
    /// `transfer=smpte2084` is what engages passthrough — the `tonemap=`
    /// algorithm is ignored in this mode, which is why it is spelled `none`.
    /// Measured on jellyfin-ffmpeg7 7.1.4-3 against a real Profile 5 sample:
    /// the output ffprobes as `profile=Main 10`, `pix_fmt=yuv420p10le`,
    /// `color_range=tv`, `color_space=bt2020nc`,
    /// `color_transfer=smpte2084`, `color_primaries=bt2020`, with no residual
    /// DV RPU side data.
    ///
    /// **It is Dolby-Vision-input-only, and it does not say so at runtime.**
    /// Handed a plain HDR10 source it does not error: it emits a broken
    /// picture — crushed shadows, everything above roughly 70% clipped to
    /// white — with correct-looking tags and exit 0 (measured). The filter
    /// logs `HDR passthrough only works for Dolby Vision inputs at the
    /// moment` when its preconditions fail, and then carries on. So
    /// [`Pipeline::handles`] admits `dolby_vision` and nothing else, and the
    /// daemon gates on a *proved* RPU (`dovi_reshape_changes_pixels`) rather
    /// than on the library row alone.
    DoviPassthrough,
    /// PQ in, PQ out, with no tone-map and no RPU: a 10-bit scale straight
    /// into HEVC Main10. The rung for every ordinary HDR source — HDR10,
    /// HDR10+, and the HDR10 base layer of a Dolby Vision title whose
    /// enhancement layer has been stripped — on a client that presents Main10
    /// PQ.
    ///
    /// **Deliberately not [`Pipeline::DoviPassthrough`] with the input
    /// widened.** That graph runs `tonemapx` with `apply_dovi=1`, which is
    /// Dolby-Vision-input-only and, handed a plain HDR10 frame, emits crushed
    /// shadows and clipping above roughly 70% at exit 0 with correct-looking
    /// tags — the worst kind of failure, because it plays. This one touches no
    /// pixel values at all: it scales, and it names the output format. Every
    /// colour term is signalled by the encoder flags
    /// (`-color_primaries bt2020 -color_trc smpte2084 -colorspace bt2020nc`),
    /// which is what a PQ source's frames already carry.
    ///
    /// Software decode is NOT required here, unlike the two Dolby graphs:
    /// there is no RPU side data for a hardware decode to drop, and
    /// `decode_setup` already brings QSV/VA-API surfaces down as `p010le`,
    /// which this chain takes as-is.
    Hdr10Passthrough,
    /// hwdownload → CPU float tone-map → the encoder's own hwupload. Always
    /// available, always correct, and slow in exactly the case that matters.
    Cpu,
}

/// The CPU chain's operator in the Jellyfin GPU filters' terms: Hable on the
/// brightest channel with no desaturation. See [`Pipeline::tone_map_terms`].
const CPU_MATCHED_TONE_MAP_TERMS: &str = "tonemap=hable:tonemap_mode=max:desat=0";
// Jellyfin linearizes PQ at 203 nits; Hable must normalize both operands
// to the CPU chain's 100-nit white. Other GPU filters retain their terms.
const METAL_CPU_WHITE_TONE_MAP_TERMS: &str =
    "tonemap=hable:tonemap_mode=max:desat=0:reference_white=100";

/// Every pipeline the probe may consider, best first.
///
/// [`Pipeline::pairs_with`] narrows it per encoder, so each family sees its own
/// native graph first: Metal for VideoToolbox, then the vendor-neutral OpenCL
/// and Vulkan graphs, then the CPU chain.
pub const CANDIDATES: &[Pipeline] = &[
    Pipeline::VtToneMapMetal,
    Pipeline::TonemapCuda,
    Pipeline::VppQsv,
    Pipeline::TonemapVaapi,
    Pipeline::LibplaceboVaapi,
    Pipeline::Libplacebo,
    Pipeline::TonemapOpencl,
    Pipeline::LibplaceboSoftware,
    Pipeline::Cpu,
];

impl Pipeline {
    /// Stable identifier for settings, logs, and the stats overlay.
    pub fn name(self) -> &'static str {
        match self {
            Pipeline::VtScaleSdr => "vt_scale_sdr",
            Pipeline::VtToneMapMetal => "vt_tonemap_metal",
            Pipeline::VtScaleHdr10 => "vt_scale_hdr10",
            Pipeline::DoviStrictTonemapx => "dovi_strict_tonemapx",
            Pipeline::VtDoviTonemapx => "vt_dovi_tonemapx",
            Pipeline::VtDoviMetal => "vt_dovi_metal",
            Pipeline::DoviMetal => "dovi_metal",
            Pipeline::TonemapCuda => "tonemap_cuda",
            Pipeline::VppQsv => "vpp_qsv",
            Pipeline::TonemapVaapi => "tonemap_vaapi",
            Pipeline::Libplacebo => "libplacebo",
            Pipeline::LibplaceboSoftware => "libplacebo_software",
            Pipeline::LibplaceboVaapi => "libplacebo_vaapi",
            Pipeline::TonemapOpencl => "tonemap_opencl",
            Pipeline::DoviTonemapx => "dovi_tonemapx",
            Pipeline::DoviPassthrough => "dovi_passthrough",
            Pipeline::Hdr10Passthrough => "hdr10_passthrough",
            Pipeline::Cpu => "cpu",
        }
    }

    pub fn parse(s: &str) -> Option<Pipeline> {
        CANDIDATES
            .iter()
            .copied()
            .chain([
                Pipeline::VtScaleSdr,
                Pipeline::VtScaleHdr10,
                Pipeline::DoviStrictTonemapx,
                Pipeline::VtDoviTonemapx,
                Pipeline::VtDoviMetal,
                Pipeline::DoviMetal,
                Pipeline::DoviTonemapx,
                Pipeline::DoviPassthrough,
                Pipeline::Hdr10Passthrough,
            ])
            .find(|p| p.name() == s)
    }

    /// Human label for the overlay and the admin log.
    pub fn label(self) -> &'static str {
        match self {
            Pipeline::VtScaleSdr => "VideoToolbox SDR scaling",
            Pipeline::VtToneMapMetal => "GPU tone-map (Metal)",
            Pipeline::VtScaleHdr10 => "VideoToolbox HDR10 scaling",
            Pipeline::DoviStrictTonemapx => "Strict Dolby Vision CPU reshape",
            Pipeline::VtDoviTonemapx => "Strict VideoToolbox Dolby Vision CPU reshape",
            Pipeline::VtDoviMetal => "Strict VideoToolbox Dolby Vision Metal reshape",
            Pipeline::DoviMetal => "Strict software decode Dolby Vision Metal reshape",
            Pipeline::TonemapCuda => "GPU tone-map (CUDA)",
            Pipeline::VppQsv => "GPU tone-map (QSV)",
            Pipeline::TonemapVaapi => "GPU tone-map (VA-API)",
            Pipeline::Libplacebo => "GPU tone-map (Vulkan)",
            Pipeline::LibplaceboSoftware => "GPU tone-map (Vulkan / CPU frame transfer)",
            Pipeline::LibplaceboVaapi => "GPU tone-map (Vulkan / VA-API)",
            Pipeline::TonemapOpencl => "GPU tone-map (OpenCL)",
            Pipeline::DoviTonemapx => "Dolby Vision reshape (tonemapx)",
            Pipeline::DoviPassthrough => "Dolby Vision → HDR10 (tonemapx passthrough)",
            Pipeline::Hdr10Passthrough => "HDR10 passthrough (no tone-map)",
            Pipeline::Cpu => "CPU tone-map",
        }
    }

    /// Processing uses an accelerator; this does not measure physical copies.
    pub fn on_gpu(self) -> bool {
        !matches!(
            self,
            Pipeline::Cpu
                | Pipeline::DoviStrictTonemapx
                | Pipeline::VtDoviTonemapx
                | Pipeline::DoviTonemapx
                | Pipeline::DoviPassthrough
                | Pipeline::Hdr10Passthrough
        )
    }

    /// Can this pipeline pair with `encoder`?
    ///
    /// The vendor graphs are tied to their own device — `vpp_qsv` needs the
    /// QSV device the QSV encoder initialises, and hands back QSV surfaces
    /// nothing else can take. The vendor-neutral ones are not, but they still
    /// need a hardware encoder on the other side: doing the tone-map on the GPU
    /// and then the H.264 encode on the CPU means downloading every frame
    /// anyway, which is the copy this exists to remove.
    pub fn pairs_with(self, encoder: Encoder) -> bool {
        match self {
            Pipeline::DoviStrictTonemapx => {
                matches!(
                    encoder,
                    Encoder::VideoToolbox | Encoder::Software | Encoder::Qsv | Encoder::Vaapi
                )
            }
            Pipeline::VtDoviTonemapx | Pipeline::VtDoviMetal | Pipeline::DoviMetal => {
                encoder == Encoder::VideoToolbox
            }
            Pipeline::VtScaleSdr | Pipeline::VtToneMapMetal | Pipeline::VtScaleHdr10 => {
                encoder == Encoder::VideoToolbox
            }
            Pipeline::TonemapCuda => encoder == Encoder::Nvenc,
            Pipeline::VppQsv => encoder == Encoder::Qsv,
            Pipeline::TonemapVaapi | Pipeline::LibplaceboVaapi => encoder == Encoder::Vaapi,
            Pipeline::Libplacebo | Pipeline::TonemapOpencl => encoder != Encoder::Software,
            Pipeline::LibplaceboSoftware => encoder == Encoder::Software,
            // Current production policy pins software decode until each
            // hardware tuple proves DOVI metadata/PTS transport, strict mapper
            // consumption and seek behavior (`requires_software_decode`).
            // A bounded artificial FATE sample transported metadata through
            // VideoToolbox, but RPU pixel influence and real-content visual
            // qualification remain unmeasured; transport alone does not meet
            // the RPU-positive visual criterion. This is a qualification policy,
            // not a universal hardware decoder limitation. The encoder is
            // independent: filtered system-memory frames reach it through the
            // same
            // `filter_suffix()` upload every other CPU-filtered path uses, and
            // the pairing is probed at boot (`has_dovi_reshape_with`) before it
            // is ever attempted — unproved pairings fall back to software.
            Pipeline::DoviTonemapx => true,
            // The same software-decode constraint applies to HDR passthrough.
            // Its encode half uses the independently proved Main10 upload
            // routes; the daemon additionally requires the Dolby renderer and
            // per-source RPU pixel proof before choosing this pipeline.
            Pipeline::DoviPassthrough => {
                matches!(encoder, Encoder::Software | Encoder::Qsv | Encoder::Vaapi)
            }
            // Plain HDR has no RPU dependency; its VAAPI P010/Main10 graph
            // has an independent boot proof and a 1080p planner ceiling.
            Pipeline::Hdr10Passthrough => {
                matches!(
                    encoder,
                    Encoder::Software | Encoder::Qsv | Encoder::Vaapi | Encoder::VideoToolbox
                )
            }
            Pipeline::Cpu => true,
        }
    }

    /// Can this pipeline handle `hdr_format` (the source's `hdr` column)?
    ///
    /// HDR10 and HDR10+ are PQ and are what the vendor filters are built and
    /// tested for. HLG is a different transfer function and the vendor filters
    /// handle it inconsistently across drivers — a wrong answer there is a
    /// washed-out picture rather than a failure, which is the worst kind of
    /// bug because nobody files it. Dolby Vision is worse still: some profiles
    /// hardware-decode to garbage, which is why `PLURX_HWDECODE=off` exists.
    /// Both route to the CPU chain, and the graph-selection log line says so
    /// (PERF-PLAN §5 scope guards).
    pub fn handles(self, hdr_format: Option<&str>) -> bool {
        match (self, hdr_format) {
            (
                Pipeline::DoviStrictTonemapx
                | Pipeline::VtDoviTonemapx
                | Pipeline::VtDoviMetal
                | Pipeline::DoviMetal,
                source,
            ) => source == Some("dolby_vision"),
            (Pipeline::VtScaleSdr, None | Some("sdr")) => true,
            (Pipeline::VtToneMapMetal, Some("hdr10" | "hlg")) => true,
            (Pipeline::VtScaleHdr10, Some("hdr10")) => true,
            (Pipeline::VtScaleSdr | Pipeline::VtToneMapMetal | Pipeline::VtScaleHdr10, _) => false,
            (Pipeline::Cpu, _) => true,
            (Pipeline::DoviTonemapx, Some("dolby_vision")) => true,
            (Pipeline::DoviTonemapx, _) => false,
            // Guard, not a preference. Passthrough on a non-DV HDR10 source
            // is a broken picture at exit 0 — see the variant's doc comment.
            (Pipeline::DoviPassthrough, Some("dolby_vision")) => true,
            (Pipeline::DoviPassthrough, _) => false,
            // The mirror image of the guard above. PQ only: an HLG source
            // through this chain would be re-tagged as PQ by the encoder
            // flags, which is a grade change rather than a passthrough, and
            // Dolby Vision belongs to the graph that reads its RPU. A DV
            // source whose base layer is HDR10-compatible arrives here as
            // `hdr10` — see `transcode::routing_hdr` — which is correct: its
            // base decodes to ordinary PQ frames.
            (Pipeline::Hdr10Passthrough, Some("hdr10" | "hdr10plus")) => true,
            (Pipeline::Hdr10Passthrough, _) => false,
            // Nothing to map. The graph is still allowed — its scaler is the
            // reason — and the tone-map step simply drops out.
            (_, None) => true,
            (_, Some(f)) => matches!(f, "hdr10" | "hdr10plus"),
        }
    }

    /// Decode-side flags. The GPU pipelines all need the decoder to hand back
    /// hardware surfaces — a pipeline that keeps frames on the GPU cannot
    /// start from frames that were never there.
    pub fn decode_args(self) -> Vec<String> {
        let a = |s: &str| s.to_owned();
        match self {
            Pipeline::VtDoviTonemapx | Pipeline::VtDoviMetal => vec![
                a("-hwaccel"),
                a("videotoolbox"),
                a("-hwaccel_output_format"),
                a("videotoolbox_vld"),
            ],
            Pipeline::DoviStrictTonemapx | Pipeline::DoviMetal => Vec::new(),
            Pipeline::VtScaleSdr | Pipeline::VtToneMapMetal | Pipeline::VtScaleHdr10 => vec![
                a("-hwaccel"),
                a("videotoolbox"),
                a("-hwaccel_output_format"),
                a("videotoolbox_vld"),
            ],
            Pipeline::TonemapCuda => vec![
                a("-hwaccel"),
                a("cuda"),
                a("-hwaccel_output_format"),
                a("cuda"),
            ],
            Pipeline::VppQsv => vec![
                a("-hwaccel"),
                a("qsv"),
                a("-hwaccel_output_format"),
                a("qsv"),
            ],
            Pipeline::TonemapVaapi | Pipeline::LibplaceboVaapi => vec![
                a("-hwaccel"),
                a("vaapi"),
                a("-hwaccel_output_format"),
                a("vaapi"),
            ],
            // Vulkan and OpenCL map from system-memory or VA-API frames; the
            // caller's existing decode choice stands, and the filter chain
            // uploads. Deriving decode flags here would fight `decode_setup`,
            // which knows things this type does not (source codec, bit depth).
            Pipeline::Libplacebo
            | Pipeline::LibplaceboSoftware
            | Pipeline::TonemapOpencl
            | Pipeline::DoviTonemapx
            | Pipeline::DoviPassthrough
            | Pipeline::Hdr10Passthrough
            | Pipeline::Cpu => Vec::new(),
        }
    }

    /// Exact decoder enforcement shared by production recipe projection and
    /// complete-graph qualification. Only strict Profile5 variants expose it.
    pub fn strict_dolby_input_args(self) -> Option<Vec<String>> {
        self.strict_dolby_input_args_for_backend(if self.requires_software_decode() {
            super::DecodeBackend::Software
        } else {
            super::DecodeBackend::VideoToolbox
        })
    }

    /// A strict CPU renderer may consume an observed native VAAPI decoder.
    /// Selection remains in the semantic resolver; these arguments enforce the
    /// selected backend and current-AU decoder contract without a VT-only flag.
    pub fn strict_dolby_input_args_for_backend(
        self,
        backend: super::DecodeBackend,
    ) -> Option<Vec<String>> {
        if !self.requires_strict_dovi()
            || !matches!(
                backend,
                super::DecodeBackend::Software
                    | super::DecodeBackend::VideoToolbox
                    | super::DecodeBackend::Vaapi
            )
        {
            return None;
        }
        let mut args: Vec<String> = [
            "-c:v",
            "hevc",
            "-strict_dovi",
            "1",
            "-err_detect",
            "explode",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        args.extend(backend.input_args(true));
        if backend == super::DecodeBackend::VideoToolbox {
            args.extend(["-hwaccel_flags".to_owned(), "+require_hardware".to_owned()]);
        }
        Some(args)
    }

    /// Device-init flags this pipeline needs before the input, beyond whatever
    /// the encoder already initialises.
    pub fn init_args(self) -> Vec<String> {
        let a = |s: &str| s.to_owned();
        match self {
            Pipeline::DoviMetal => vec![
                a("-init_hw_device"),
                a("videotoolbox=plurx_p5_vt"),
                a("-filter_hw_device"),
                a("plurx_p5_vt"),
            ],
            Pipeline::TonemapCuda => vec![
                a("-init_hw_device"),
                a("cuda=cu:0"),
                a("-filter_hw_device"),
                a("cu"),
            ],
            // libplacebo wants a Vulkan device; ffmpeg derives one from the
            // existing hardware context where it can, but naming it is what
            // makes the graph work on a box whose encoder is VA-API.
            Pipeline::Libplacebo | Pipeline::LibplaceboVaapi => vec![
                a("-init_hw_device"),
                a("vulkan=vk"),
                a("-filter_hw_device"),
                a("vk"),
            ],
            Pipeline::TonemapOpencl => vec![
                a("-init_hw_device"),
                a("opencl=ocl"),
                a("-filter_hw_device"),
                a("ocl"),
            ],
            _ => Vec::new(),
        }
    }

    /// Initialize both device families together. A Vulkan upload uses the
    /// global filter device, so its VA-API encoder must be an ancestor that
    /// the final upload can recover with `derive_device=vaapi`.
    pub fn device_args(self, encoder: Encoder) -> Vec<String> {
        if matches!(self, Pipeline::Libplacebo | Pipeline::LibplaceboVaapi)
            && encoder == Encoder::Vaapi
        {
            return vec![
                "-init_hw_device".into(),
                format!("vaapi=hw:{}", super::encoder::vaapi_device()),
                "-init_hw_device".into(),
                "vulkan=vk@hw".into(),
                "-filter_hw_device".into(),
                "vk".into(),
            ];
        }
        let mut args = encoder.init_args();
        args.extend(self.init_args());
        args
    }

    /// Upload software output (including a subtitle composite) to the encoder.
    /// A bare upload would use Vulkan again and hand the VA-API encoder the
    /// wrong hardware surface; recover its device from the derived context.
    pub fn encoder_upload(self, encoder: Encoder) -> Option<&'static str> {
        if matches!(self, Pipeline::Libplacebo | Pipeline::LibplaceboVaapi)
            && encoder == Encoder::Vaapi
        {
            Some("format=nv12,hwupload=derive_device=vaapi")
        } else {
            encoder.filter_suffix_for(self.output_grade())
        }
    }

    /// The scale + tone-map segment of the filter chain, for a source with
    /// `hdr_format` targeting `height`.
    ///
    /// Returns `None` for [`Pipeline::Cpu`], whose chain is built by the
    /// existing CPU path — this type describes the graphs that replace it, and
    /// having it also own the one it replaces would put two spellings of the
    /// CPU chain in the tree.
    ///
    /// `hdr_format` is taken rather than assumed because a GPU pipeline may be
    /// selected for a session with no HDR at all: the scaler is worth having
    /// on the GPU either way, and the tone-map step simply drops out.
    ///
    /// `width` pins the frame to an exact size instead of letting the scaler
    /// keep aspect on its own. A bitmap burn needs that: the subtitle plane is
    /// scaled by a *different* filter against `output_size`'s answer, and two
    /// scalers rounding independently is a composite that can miss by a pixel.
    /// `None` preserves the keep-aspect spellings, which also never upscale;
    /// an exact size is the caller promising it already clamped to the source.
    pub fn filters(
        self,
        width: Option<i64>,
        height: i64,
        hdr_format: Option<&str>,
    ) -> Option<String> {
        self.filters_at_peak(width, height, hdr_format, None)
    }

    /// [`Pipeline::filters`] with the source's peak, in nits, threaded into
    /// the graphs that take the CPU chain's operator — the same peak the CPU
    /// chain is given (MaxCLL, else mastering maximum, else 1,000), in the
    /// filter's unit. Metal's explicit option is in tens of nits; CUDA and
    /// OpenCL use the CPU's hundreds of nits. Without it Jellyfin's filters read the peak from frame
    /// metadata and fall back to their own default for an untagged source: a
    /// different curve from the CPU chain's under the same operator. HLG and
    /// graphs without the controls ignore it.
    pub fn filters_at_peak(
        self,
        width: Option<i64>,
        height: i64,
        hdr_format: Option<&str>,
        peak_nits: Option<u32>,
    ) -> Option<String> {
        let hdr = hdr_format.is_some();
        let mapping = self.tone_map_terms(hdr_format).map(|terms| {
            match peak_nits.and_then(|nits| self.tone_map_peak_option(hdr_format, nits)) {
                Some(peak) => format!("{terms}:peak={peak}"),
                None => terms.to_owned(),
            }
        });
        let w = width.map_or_else(|| "-1".to_owned(), |w| w.to_string());
        Some(match self {
            Pipeline::VtScaleSdr => format!("scale_vt=w={w}:h={height}:format=nv12"),
            Pipeline::VtScaleHdr10 => format!("scale_vt=w={w}:h={height}:format=p010le"),
            Pipeline::VtToneMapMetal => format!(
                "scale_vt=w={w}:h={height}:format=p010le,\
                 tonemap_videotoolbox={terms}:transfer=bt709:matrix=bt709:\
                 primaries=bt709:range=tv:format=nv12:apply_dovi=0",
                terms = mapping.as_deref().expect("the Metal graph always maps")
            ),
            Pipeline::Cpu => return None,
            Pipeline::TonemapCuda => {
                let w = width.map_or_else(|| "-2".to_owned(), |w| w.to_string());
                let scale = format!("scale_cuda=w={w}:h={height}");
                if let Some(terms) = mapping.as_deref() {
                    format!("tonemap_cuda={terms}:transfer=bt709:matrix=bt709:primaries=bt709:range=tv:format=nv12,{scale}")
                } else {
                    format!("{scale}:format=nv12")
                }
            }
            // vpp_qsv does scale and tone-map in one pass. `w=-1` keeps the
            // aspect; the output is nv12 in GPU memory, which is what h264_qsv
            // wants, so no format conversion is needed on either side.
            Pipeline::VppQsv => {
                if hdr {
                    format!("vpp_qsv=w={w}:h={height}:tonemap=1:format=nv12")
                } else {
                    format!("vpp_qsv=w={w}:h={height}:format=nv12")
                }
            }
            // VA-API splits them: scale_vaapi resizes, tonemap_vaapi maps. Both
            // stay in VA-API surfaces, which h264_vaapi encodes directly.
            Pipeline::TonemapVaapi => {
                if hdr {
                    format!("scale_vaapi=w={w}:h={height}:format=p010,tonemap_vaapi=format=nv12:matrix=bt709:transfer=bt709:primaries=bt709")
                } else {
                    format!("scale_vaapi=w={w}:h={height}:format=nv12")
                }
            }
            // libplacebo scales and maps together and outputs to whatever the
            // next filter needs; `hwupload`/`hwdownload` around it are what let
            // it sit between decoders and encoders of different families.
            Pipeline::Libplacebo | Pipeline::LibplaceboVaapi => {
                let tm = if hdr {
                    ":tonemapping=bt.2390:colorspace=bt709:color_primaries=bt709:color_trc=bt709"
                } else {
                    ""
                };
                let renderer = format!("libplacebo=w={w}:h={height}{tm}:format=nv12");
                if self == Pipeline::LibplaceboVaapi {
                    format!("{renderer},hwmap=derive_device=vaapi,format=vaapi")
                } else {
                    format!("hwupload,{renderer},hwdownload,format=nv12")
                }
            }
            Pipeline::LibplaceboSoftware => {
                // RGB conversion changes the matrix, not the source transfer or
                // primaries. Preserve those tags so SDR is never stamped as PQ.
                // Convert rendered RGB samples to limited-range BT.709 YUV; a
                // pixel-format change alone can retain the RGB matrix tag.
                let tm = if hdr { ":tonemapping=bt.2390" } else { "" };
                format!("format=rgba64le,setparams=colorspace=gbr,libplacebo=w={w}:h={height}{tm}:colorspace=gbr:color_primaries=bt709:color_trc=bt709:format=rgba,scale=out_color_matrix=bt709:out_range=tv,format=yuv420p")
            }
            // tonemap_opencl maps only, so the scale stays on the CPU side of
            // it. Its output is an OpenCL surface no H.264 encoder takes, hence
            // the explicit download.
            Pipeline::TonemapOpencl => {
                let scale = match width {
                    Some(w) => format!("scale={w}:{height}"),
                    None => format!("scale=-2:'min({height},ih)'"),
                };
                if let Some(terms) = mapping.as_deref() {
                    format!(
                        "{scale},format=p010,hwupload,\
                         tonemap_opencl={terms}:transfer=bt709:matrix=bt709:primaries=bt709:range=tv:format=nv12,\
                         hwdownload,format=nv12"
                    )
                } else {
                    format!("{scale},format=nv12")
                }
            }
            Pipeline::DoviStrictTonemapx | Pipeline::VtDoviTonemapx => {
                let prefix = if self == Pipeline::VtDoviTonemapx {
                    "hwdownload,format=p010le,setparams=colorspace=unknown,format=yuv420p10le,"
                } else {
                    "format=yuv420p10le,"
                };
                format!(
                    "{prefix}{tonemap}:require_dovi=1,scale={w}:{},format=yuv420p",
                    height.max(2),
                    tonemap = super::dovi_tonemapx_sdr_filter(),
                )
            }
            Pipeline::VtDoviMetal | Pipeline::DoviMetal => {
                let prefix = if self == Pipeline::DoviMetal {
                    "format=yuv420p10le,setparams=colorspace=unknown,format=p010le,hwupload,"
                } else {
                    ""
                };
                let scale = Pipeline::VtScaleSdr
                    .filters(width, height, Some("sdr"))
                    .expect("native scaler");
                format!("{prefix}tonemap_videotoolbox=tonemap=bt2390:transfer=bt709:matrix=bt709:primaries=bt709:range=tv:format=nv12:apply_dovi=1:require_dovi=1,{scale}")
            }
            Pipeline::DoviTonemapx => {
                let h = height.max(2);
                let format = OutputGrade::Sdr.pixel_format();
                format!(
                    "{tonemap},scale={w}:{h},format={format}",
                    tonemap = super::dovi_tonemapx_sdr_filter(),
                )
            }
            // The same filter, the same `apply_dovi=1`, the same
            // reshape-before-scale order — and a different destination. Every
            // colour term comes from one `OutputGrade`, which is what makes
            // the PQ/8-bit pairing that `abort()`s the process unspellable
            // here. `tonemap=none` is honest rather than meaningful: selecting
            // `transfer=smpte2084` engages passthrough and the algorithm is
            // ignored.
            Pipeline::DoviPassthrough => {
                let h = height.max(2);
                let grade = OutputGrade::Hdr10;
                let transfer = grade.transfer();
                let matrix = grade.matrix();
                let primaries = grade.primaries();
                let format = grade.pixel_format();
                format!(
                    "tonemapx=tonemap=none:transfer={transfer}:matrix={matrix}:\
                     primaries={primaries}:range=tv:format={format}:apply_dovi=1,\
                     scale={w}:{h},format={format}"
                )
            }
            // No filter reads or rewrites a pixel value: the frames are
            // already PQ/BT.2020 and stay that way. Scale, then name the
            // 10-bit format the Main10 encoder needs. The `format=` comes from
            // the same `OutputGrade` as the encoder's transfer, which is what
            // makes the PQ-at-8-bit pairing that `abort()`s ffmpeg
            // unspellable here.
            Pipeline::Hdr10Passthrough => {
                let h = height.max(2);
                let format = OutputGrade::Hdr10.pixel_format();
                format!("scale={w}:{h},format={format}")
            }
        })
    }

    /// The tone-map operator terms a GPU graph passes, for the graphs whose
    /// filter exposes them. `None` where there is nothing to map or the graph
    /// has no such controls (a driver curve, or libplacebo's own defaults).
    ///
    /// One source for the filter string and for plan identity
    /// (`ResolvedTranscode::plan_digest` feeds it), so a cached rendition can
    /// never carry a different curve than its key names.
    ///
    /// For PQ input every graph with the controls takes the CPU reference's
    /// operator ([`super::zscale_tone_map_filter`]: Hable on the brightest
    /// channel, no desaturation), and [`Pipeline::filters_at_peak`] adds the
    /// CPU chain's peak. Naming the curve alone is not enough: the
    /// Jellyfin filters default `tonemap_mode` to `auto`, which picks ITP on
    /// capable GPUs, and leave desaturation on — a different picture behind
    /// the same operator name. That is what put an Apple Silicon node's
    /// OpenCL graph 59 levels from the CPU chain at the boot probe
    /// (Y/U/V 178/115/93 against 119/136/138); with these terms the same run
    /// lands within ten levels on OpenCL and Metal alike.
    ///
    /// HLG through Metal keeps BT.2390/ITP. Its runtime observation pins
    /// reference white under that treatment, and the boot probe's PQ fixture
    /// says nothing about an HLG curve.
    pub fn tone_map_terms(self, hdr_format: Option<&str>) -> Option<&'static str> {
        const CPU_MATCHED: &str = CPU_MATCHED_TONE_MAP_TERMS;
        match self {
            Pipeline::VtToneMapMetal if hdr_format == Some("hlg") => {
                Some("tonemap=bt2390:tonemap_mode=itp")
            }
            Pipeline::VtToneMapMetal => Some(METAL_CPU_WHITE_TONE_MAP_TERMS),
            Pipeline::TonemapCuda | Pipeline::TonemapOpencl if hdr_format.is_some() => {
                Some(CPU_MATCHED)
            }
            _ => None,
        }
    }

    /// Translate absolute nits into this filter's explicit peak option.
    /// Jellyfin 8.1.3-1 Metal converts that option as `peak / 10 * 100 / 203`,
    /// while its PQ linearizer uses nits / 203. Supplying the CPU's nits / 100
    /// therefore loses a factor of ten. Its <=100-nit fallback masks this at
    /// exactly 1,000 nits, but not at the 1,179-nit real-title control.
    pub fn tone_map_peak_option(self, hdr_format: Option<&str>, nits: u32) -> Option<f64> {
        if !matches!(
            self.tone_map_terms(hdr_format),
            Some(CPU_MATCHED_TONE_MAP_TERMS | METAL_CPU_WHITE_TONE_MAP_TERMS)
        ) {
            return None;
        }
        let divisor = if self == Pipeline::VtToneMapMetal {
            10.0
        } else {
            100.0
        };
        Some(f64::from(nits) / divisor)
    }

    /// Whether sessions may reach this graph only through the resolved macOS
    /// compatibility context. A node may still *prove* one of these at boot —
    /// the probe holds the Metal tone-map to the same CPU parity as every
    /// other GPU graph — but proving it never routes a session to it directly:
    /// opaque VideoToolbox frames skip the CPU path's rotation, aspect and
    /// cadence normalization, and only the context checks those.
    pub fn requires_macos_context(self) -> bool {
        matches!(
            self,
            Pipeline::VtScaleSdr | Pipeline::VtToneMapMetal | Pipeline::VtScaleHdr10
        )
    }

    /// Whether the unburned graph retains vendor surfaces from decode to encode.
    ///
    /// The vendor VPP graphs and the VA-API/Vulkan interop graph hand the
    /// encoder hardware surfaces directly, as do the Apple processing graphs.
    /// This describes FFmpeg frame domains, not physical copy counts or PCIe.
    /// The remaining graphs use system-memory stages on
    /// every frame, and the amounts are not small — the CPU float tone-map is
    /// the 0.71x measurement this module's header records, `libplacebo` ends
    /// with `hwdownload` into system memory, and `tonemap_opencl` leaves the
    /// scale on the CPU side by construction, at *source* resolution.
    ///
    /// Admission reads this, because "the encoder is hardware" says nothing
    /// about whether the pipeline feeding it is.
    pub fn keeps_frames_off_the_cpu(self) -> bool {
        matches!(
            self,
            Pipeline::VtDoviMetal
                | Pipeline::DoviMetal
                | Pipeline::VtScaleSdr
                | Pipeline::VtToneMapMetal
                | Pipeline::VtScaleHdr10
                | Pipeline::TonemapCuda
                | Pipeline::VppQsv
                | Pipeline::TonemapVaapi
                | Pipeline::LibplaceboVaapi
        )
    }

    /// Conservative default for incumbent renderers. The semantic resolver
    /// overrides strict CPU decode only for an exact observed Linux graph;
    /// current-AU metadata enforcement and the promised grade remain required.
    pub fn requires_software_decode(self) -> bool {
        matches!(
            self,
            Pipeline::DoviTonemapx
                | Pipeline::DoviPassthrough
                | Pipeline::DoviStrictTonemapx
                | Pipeline::DoviMetal
        )
    }

    /// Exact Profile5 routes require current-AU metadata and strict rendering.
    pub fn requires_strict_dovi(self) -> bool {
        matches!(
            self,
            Pipeline::DoviStrictTonemapx
                | Pipeline::VtDoviTonemapx
                | Pipeline::VtDoviMetal
                | Pipeline::DoviMetal
        )
    }

    /// The dynamic range of the bytes this pipeline's session puts on the
    /// wire.
    ///
    /// The single source of truth for the grade, read by the filter builder,
    /// the encoder choice, the cache recipe and the badge alike — so a
    /// session cannot encode one grade and report another. Everything except
    /// [`Pipeline::DoviPassthrough`] ends in BT.709 8-bit.
    pub fn output_grade(self) -> OutputGrade {
        match self {
            Pipeline::DoviPassthrough | Pipeline::Hdr10Passthrough | Pipeline::VtScaleHdr10 => {
                OutputGrade::Hdr10
            }
            Pipeline::DoviStrictTonemapx
            | Pipeline::VtDoviTonemapx
            | Pipeline::VtDoviMetal
            | Pipeline::DoviMetal
            | Pipeline::VtScaleSdr
            | Pipeline::VtToneMapMetal
            | Pipeline::TonemapCuda
            | Pipeline::VppQsv
            | Pipeline::TonemapVaapi
            | Pipeline::Libplacebo
            | Pipeline::LibplaceboSoftware
            | Pipeline::LibplaceboVaapi
            | Pipeline::TonemapOpencl
            | Pipeline::DoviTonemapx
            | Pipeline::Cpu => OutputGrade::Sdr,
        }
    }

    /// The pipeline this session actually gets, given the one the node proved
    /// and what this particular session is doing.
    ///
    /// A probe answers "can this box run this graph"; it cannot answer "should
    /// this session". Two correctness constraints send a session back to the
    /// CPU chain no matter what the node proved, and both are quiet failures
    /// than loud ones — which is why they are decided here, once, instead of
    /// being left to the graph to discover:
    ///
    /// - **The source it can't map.** HLG and Dolby Vision (see
    ///   [`Pipeline::handles`] — but note `transcode::routing_hdr`: a DV
    ///   source whose base layer is HDR10-compatible routes as `hdr10`,
    ///   because it decodes to ordinary PQ surfaces and neither chain reads
    ///   the RPUs anyway).
    /// - **The encoder it can't feed.** A graph and an encoder from different
    ///   families produce surfaces the other cannot read (see
    ///   [`Pipeline::pairs_with`]).
    ///
    /// Text and bitmap subtitle burns both keep the GPU scale + tone-map. The
    /// mapped frame comes down exactly once for libass/overlay and returns to
    /// the hardware encoder; it is the CPU float tone-map, not that one copy,
    /// that decides whether a 4K burn can produce its first segment in time.
    ///
    /// And one more that is about worth rather than correctness: `heavy` (see
    /// `transcode::heavy_source`). The vendor graphs require hardware decode,
    /// and `decode_setup` deliberately keeps light sources on software decode
    /// — the GPU decode/filter handoff is a risk, and a 1080p H.264 scale is
    /// not a problem anyone has. A GPU path is for the sessions that are
    /// actually slow.
    pub fn for_session(
        proven: Pipeline,
        encoder: Encoder,
        hdr_format: Option<&str>,
        heavy: bool,
        burns_text_subtitles: bool,
    ) -> Pipeline {
        match Pipeline::declined(proven, encoder, hdr_format, heavy, burns_text_subtitles) {
            Some(_) => Pipeline::Cpu,
            None => proven,
        }
    }

    /// Session routing with the source scan contract included. Hardware
    /// deinterlace is not admitted until a graph is qualified independently,
    /// so known interlaced input uses the CPU filter chain.
    pub fn for_session_with_scan(
        proven: Pipeline,
        encoder: Encoder,
        hdr_format: Option<&str>,
        heavy: bool,
        burns_text_subtitles: bool,
        scan_type: ScanType,
    ) -> Pipeline {
        match Pipeline::declined_with_scan(
            proven,
            encoder,
            hdr_format,
            heavy,
            burns_text_subtitles,
            scan_type,
        ) {
            Some(_) => Pipeline::Cpu,
            None => proven,
        }
    }

    /// Why this session is not getting the graph the node proved, in words.
    ///
    /// `None` when it is getting it — or when there was nothing to decline,
    /// because the CPU chain is all this node has.
    ///
    /// [`Pipeline::for_session`] is defined in terms of this, so the decision
    /// and its explanation cannot drift apart. That matters more than it
    /// sounds. The doc above has said "the log says why" since M2 shipped, and
    /// the log did not: it printed `pipeline=cpu` on a 4K HDR session, on a box
    /// that had just probed a GPU graph at 4.9× — and left the reader to guess
    /// which of three conditions had fired. A fallback nobody can explain is
    /// indistinguishable from a bug, and this one is usually *correct*.
    pub fn declined(
        proven: Pipeline,
        encoder: Encoder,
        hdr_format: Option<&str>,
        heavy: bool,
        _burns_text_subtitles: bool,
    ) -> Option<&'static str> {
        if proven == Pipeline::Cpu {
            return None;
        }
        if proven.requires_macos_context() {
            return Some("macOS processing requires its resolved compatibility context");
        }
        if !heavy {
            return Some("light source — a GPU graph is not worth the handoff");
        }
        if !proven.handles(hdr_format) {
            return Some(match hdr_format {
                Some("dolby_vision") => {
                    "Dolby Vision — the vendor tone-map cannot read its dynamic metadata, \
                     so the CPU chain is the correct answer rather than a fallback"
                }
                Some("hlg") => {
                    "HLG — the vendor tone-map handles it inconsistently across drivers, and \
                     a wrong answer there is a washed-out picture nobody reports"
                }
                _ => "the source's HDR format is not one the vendor tone-map is built for",
            });
        }
        if !proven.pairs_with(encoder) {
            return Some("the proven graph and this session's encoder are different families");
        }
        None
    }

    pub fn declined_with_scan(
        proven: Pipeline,
        encoder: Encoder,
        hdr_format: Option<&str>,
        heavy: bool,
        burns_text_subtitles: bool,
        scan_type: ScanType,
    ) -> Option<&'static str> {
        if matches!(scan_type, ScanType::Interlaced(_))
            && matches!(
                proven,
                Pipeline::TonemapCuda
                    | Pipeline::VppQsv
                    | Pipeline::TonemapVaapi
                    | Pipeline::Libplacebo
                    | Pipeline::LibplaceboSoftware
                    | Pipeline::LibplaceboVaapi
                    | Pipeline::TonemapOpencl
            )
        {
            return Some("interlaced source — hardware deinterlace has not passed qualification");
        }
        Self::declined(proven, encoder, hdr_format, heavy, burns_text_subtitles)
    }

    /// The next thing to try when this pipeline fails at runtime.
    ///
    /// One step, and it goes straight to the CPU chain rather than down the
    /// candidate list. A graph that passed its probe and then stopped
    /// progressing is evidence about *this driver right now* — GPU contention,
    /// a codec profile the probe's fixture didn't cover, a device that has
    /// wedged — and none of that makes the next GPU graph a better bet. The
    /// viewer is waiting; the second attempt should be the one that always
    /// works.
    pub fn fallback(self) -> Option<Pipeline> {
        match self {
            Pipeline::DoviStrictTonemapx => None,
            Pipeline::VtDoviTonemapx | Pipeline::VtDoviMetal | Pipeline::DoviMetal => {
                Some(Pipeline::DoviStrictTonemapx)
            }
            Pipeline::VtScaleHdr10 => Some(Pipeline::Hdr10Passthrough),
            // A Dolby renderer has nothing below it. Falling back to the
            // non-Dolby-aware CPU zscale graph would render Profile 5 as
            // garbage rather than failing, and for the passthrough rung it
            // would additionally swap the grade the client was promised.
            // A grade-preserving renderer has nothing below it. Falling back
            // to the CPU zscale graph would render Profile 5 as garbage and,
            // for either passthrough rung, swap the grade the client was
            // promised for the one it was not.
            Pipeline::Cpu
            | Pipeline::DoviTonemapx
            | Pipeline::DoviPassthrough
            | Pipeline::Hdr10Passthrough => None,
            _ => Some(Pipeline::Cpu),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::FieldOrder;

    #[test]
    fn metal_explicit_peak_preserves_absolute_nits_across_the_fallback_boundary() {
        // Exercise the pinned filter's conversion, including the discontinuity
        // that lets the old option=10 pass while option=11.79 clips highlights.
        for nits in [999, 1_000, 1_001, 1_179, 4_000] {
            for hdr in ["hdr10", "hdr10plus"] {
                let option = Pipeline::VtToneMapMetal
                    .tone_map_peak_option(Some(hdr), nits)
                    .expect("PQ has a peak");
                let filter_peak = option / 10.0 * (100.0 / 203.0);
                assert!((filter_peak - f64::from(nits) / 203.0).abs() < 1e-12);
                let graph = Pipeline::VtToneMapMetal
                    .filters_at_peak(Some(1920), 1080, Some(hdr), Some(nits))
                    .expect("graph");
                assert!(graph.contains(&format!(":peak={option}:")), "{graph}");
            }
        }
        assert_eq!(
            Pipeline::VtToneMapMetal.tone_map_peak_option(Some("hlg"), 1_179),
            None
        );
        for pipeline in [Pipeline::TonemapCuda, Pipeline::TonemapOpencl] {
            assert_eq!(
                pipeline.tone_map_peak_option(Some("hdr10"), 1_179),
                Some(11.79)
            );
        }
    }

    /// Each GPU graph that names its curve takes the terms from one place,
    /// and for PQ those terms are the CPU reference's operator. A graph that
    /// says `hable` and leaves the mode to Jellyfin's `auto` is a different
    /// picture under the same name — 59 levels away on an Apple Silicon node.
    #[test]
    fn gpu_tone_map_terms_are_one_source_and_match_the_cpu_operator_on_pq() {
        let cpu = crate::transcode::zscale_tone_map_filter("smpte2084", 1_000);
        assert!(cpu.contains("tonemap=tonemap=hable:desat=0:"), "{cpu}");
        for pipeline in [
            Pipeline::TonemapCuda,
            Pipeline::TonemapOpencl,
            Pipeline::VtToneMapMetal,
        ] {
            for hdr in ["hdr10", "hdr10plus"] {
                let terms = pipeline.tone_map_terms(Some(hdr)).expect("maps PQ");
                assert_eq!(
                    terms,
                    if pipeline == Pipeline::VtToneMapMetal {
                        "tonemap=hable:tonemap_mode=max:desat=0:reference_white=100"
                    } else {
                        "tonemap=hable:tonemap_mode=max:desat=0"
                    },
                    "{pipeline:?}"
                );
                let graph = pipeline
                    .filters(Some(1920), 1080, Some(hdr))
                    .expect("graph");
                assert!(graph.contains(&format!("={terms}:")), "{graph}");
                assert!(
                    graph.contains(":range=tv:"),
                    "limited range like the CPU chain: {graph}"
                );
            }
        }
        for pipeline in [
            Pipeline::TonemapCuda,
            Pipeline::TonemapOpencl,
            Pipeline::VtToneMapMetal,
        ] {
            let graph = pipeline
                .filters_at_peak(Some(1920), 1080, Some("hdr10"), Some(4_000))
                .expect("graph");
            assert!(
                graph.contains(if pipeline == Pipeline::VtToneMapMetal {
                    "=tonemap=hable:tonemap_mode=max:desat=0:reference_white=100:peak=400:"
                } else {
                    "=tonemap=hable:tonemap_mode=max:desat=0:peak=40:"
                }),
                "the same absolute peak in this filter's unit (cf. {cpu}): {graph}"
            );
        }
        let hlg_at_peak = Pipeline::VtToneMapMetal
            .filters_at_peak(Some(1920), 1080, Some("hlg"), Some(4_000))
            .expect("graph");
        assert!(
            !hlg_at_peak.contains("peak=") && !hlg_at_peak.contains("reference_white="),
            "HLG keeps its pinned treatment: {hlg_at_peak}"
        );
        let hlg = Pipeline::VtToneMapMetal.tone_map_terms(Some("hlg"));
        assert_eq!(hlg, Some("tonemap=bt2390:tonemap_mode=itp"));
        let graph = Pipeline::VtToneMapMetal
            .filters(Some(1920), 1080, Some("hlg"))
            .expect("graph");
        assert!(
            graph.contains("tonemap_videotoolbox=tonemap=bt2390:tonemap_mode=itp:"),
            "{graph}"
        );
        for pipeline in [Pipeline::TonemapCuda, Pipeline::TonemapOpencl] {
            assert_eq!(pipeline.tone_map_terms(None), None);
            let sdr = pipeline.filters(Some(1920), 1080, None).expect("scaler");
            assert!(
                !sdr.contains("tonemap"),
                "an SDR source is never mapped: {sdr}"
            );
        }
        for pipeline in [
            Pipeline::VppQsv,
            Pipeline::TonemapVaapi,
            Pipeline::Libplacebo,
            Pipeline::LibplaceboVaapi,
            Pipeline::Cpu,
        ] {
            assert_eq!(pipeline.tone_map_terms(Some("hdr10")), None, "{pipeline:?}");
        }
    }

    /// The boot probe proves the Metal graph like any other; sessions still
    /// reach it only through the macOS compatibility context.
    #[test]
    fn the_metal_graph_is_probed_first_on_videotoolbox_and_routed_only_through_the_mac_context() {
        assert_eq!(CANDIDATES.first(), Some(&Pipeline::VtToneMapMetal));
        assert_eq!(CANDIDATES.last(), Some(&Pipeline::Cpu));
        assert_eq!(
            Pipeline::parse("vt_tonemap_metal"),
            Some(Pipeline::VtToneMapMetal)
        );
        assert_eq!(Pipeline::VtToneMapMetal.label(), "GPU tone-map (Metal)");
        for encoder in [
            Encoder::Software,
            Encoder::Nvenc,
            Encoder::Qsv,
            Encoder::Vaapi,
            Encoder::VideoToolbox,
        ] {
            assert_eq!(
                Pipeline::VtToneMapMetal.pairs_with(encoder),
                encoder == Encoder::VideoToolbox
            );
        }
        for pipeline in [
            Pipeline::VtScaleSdr,
            Pipeline::VtToneMapMetal,
            Pipeline::VtScaleHdr10,
        ] {
            assert!(pipeline.requires_macos_context());
            assert_eq!(
                Pipeline::for_session(pipeline, Encoder::VideoToolbox, Some("hdr10"), true, false),
                Pipeline::Cpu
            );
        }
        assert!(!Pipeline::TonemapOpencl.requires_macos_context());
        assert_eq!(
            Pipeline::for_session(
                Pipeline::TonemapOpencl,
                Encoder::VideoToolbox,
                Some("hdr10"),
                true,
                false
            ),
            Pipeline::TonemapOpencl
        );
    }

    #[test]
    fn software_frame_vulkan_route_preserves_transfer_and_reports_cpu_handoff() {
        let pipeline = Pipeline::LibplaceboSoftware;
        assert!(pipeline.pairs_with(Encoder::Software));
        assert!(!pipeline.pairs_with(Encoder::Vaapi));
        assert!(pipeline.on_gpu());
        assert!(!pipeline.keeps_frames_off_the_cpu());
        assert!(pipeline.device_args(Encoder::Software).is_empty());
        assert!(pipeline.decode_args().is_empty());
        assert_eq!(Pipeline::parse("libplacebo_software"), Some(pipeline));
        for hdr in [None, Some("hdr10")] {
            let graph = pipeline
                .filters(Some(1280), 720, hdr)
                .expect("software-frame Vulkan has a filter graph");
            assert!(graph.starts_with("format=rgba64le,setparams=colorspace=gbr,"));
            assert!(!graph.contains("hwupload"));
            assert!(!graph.contains("hwdownload"));
            assert!(!graph.contains("smpte2084"));
            assert!(!graph.contains("color_primaries=bt2020"));
            assert!(graph
                .ends_with("format=rgba,scale=out_color_matrix=bt709:out_range=tv,format=yuv420p"));
            assert_eq!(graph.contains("tonemapping=bt.2390"), hdr.is_some());
        }
    }

    #[test]
    fn paired_devices_preserve_other_pipeline_encoder_contracts() {
        for &pipeline in CANDIDATES {
            for encoder in [
                Encoder::Software,
                Encoder::Vaapi,
                Encoder::Qsv,
                Encoder::Nvenc,
                Encoder::VideoToolbox,
            ] {
                if matches!(pipeline, Pipeline::Libplacebo | Pipeline::LibplaceboVaapi)
                    && encoder == Encoder::Vaapi
                {
                    continue;
                }
                let mut expected = encoder.init_args();
                expected.extend(pipeline.init_args());
                assert_eq!(pipeline.device_args(encoder), expected);
                assert_eq!(
                    pipeline.encoder_upload(encoder),
                    encoder.filter_suffix_for(pipeline.output_grade())
                );
            }
        }
    }

    #[test]
    fn names_round_trip_and_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for p in CANDIDATES {
            assert!(seen.insert(p.name()), "duplicate name {}", p.name());
            assert_eq!(Pipeline::parse(p.name()), Some(*p));
        }
        assert_eq!(Pipeline::parse("gpu-magic"), None);
        // A stored preference from a future version must not panic anything.
        assert_eq!(Pipeline::parse(""), None);
        assert_eq!(
            Pipeline::parse("dovi_tonemapx"),
            Some(Pipeline::DoviTonemapx)
        );
        assert!(
            !CANDIDATES.contains(&Pipeline::DoviTonemapx),
            "the Profile 5 renderer must be probed/admitted independently"
        );
        assert_eq!(
            Pipeline::parse("dovi_passthrough"),
            Some(Pipeline::DoviPassthrough)
        );
        assert!(
            !CANDIDATES.contains(&Pipeline::DoviPassthrough),
            "the HDR10 rung has its own boot probe and its own admission"
        );
        assert_eq!(
            Pipeline::parse("hdr10_passthrough"),
            Some(Pipeline::Hdr10Passthrough)
        );
        assert!(
            !CANDIDATES.contains(&Pipeline::Hdr10Passthrough),
            "the plain HDR10 rung is chosen by the grade negotiation, never by \
             the tone-map probe -- an SDR session must not be able to land on a \
             graph that tags its output PQ"
        );
        for pair in [
            (Pipeline::DoviPassthrough, Pipeline::DoviTonemapx),
            (Pipeline::DoviPassthrough, Pipeline::Hdr10Passthrough),
            (Pipeline::DoviTonemapx, Pipeline::Hdr10Passthrough),
        ] {
            assert_ne!(
                pair.0.name(),
                pair.1.name(),
                "every renderer needs a distinct cache identity"
            );
        }
    }

    /// The HDR10 rung, end to end as a graph: what it will take, what it will
    /// pair with, what it emits, and — the two that matter — that it never
    /// sees a non-Dolby source and never emits 8 bits.
    ///
    /// Both are quiet failures. A non-DV source through the passthrough
    /// filter is a broken picture at exit 0; PQ at 8 bits is `abort()`.
    #[test]
    fn the_hdr10_rung_is_dolby_vision_only_and_never_eight_bit() {
        let p = Pipeline::DoviPassthrough;
        assert_eq!(p.output_grade(), OutputGrade::Hdr10);
        assert!(p.requires_software_decode());
        assert!(!p.on_gpu());
        assert_eq!(p.fallback(), None, "there is nothing below a DV renderer");
        assert!(p.decode_args().is_empty(), "software decode keeps the RPU");
        assert!(p.init_args().is_empty());

        assert!(p.pairs_with(Encoder::Software));
        assert!(p.pairs_with(Encoder::Qsv));
        assert!(p.pairs_with(Encoder::Vaapi));
        for encoder in [Encoder::Nvenc, Encoder::VideoToolbox] {
            assert!(
                !p.pairs_with(encoder),
                "{encoder:?} has no measured HEVC Main10 encoder"
            );
        }

        assert!(p.handles(Some("dolby_vision")));
        for source in [None, Some("hdr10"), Some("hdr10plus"), Some("hlg")] {
            assert!(
                !p.handles(source),
                "{source:?} through the passthrough filter is a broken picture at exit 0"
            );
        }

        let graph = p
            .filters(Some(1920), 1080, Some("dolby_vision"))
            .expect("the HDR10 graph");
        let reshape = graph.find("tonemapx=").expect("tonemapx transform");
        assert_eq!(reshape, 0, "Dolby reshape must precede scale: {graph}");
        assert!(graph.contains("apply_dovi=1"), "{graph}");
        assert!(graph.contains("transfer=smpte2084"), "{graph}");
        assert!(graph.contains("matrix=bt2020"), "{graph}");
        assert!(graph.contains("primaries=bt2020"), "{graph}");
        assert!(graph.contains("range=tv"), "{graph}");
        assert!(graph.contains("scale=1920:1080"), "{graph}");
        assert!(graph.ends_with("format=yuv420p10le"), "{graph}");
        // The tone-map algorithm is ignored in passthrough mode; naming one
        // would suggest a picture transform that does not happen.
        assert!(graph.contains("tonemap=none"), "{graph}");
        for eight_bit in ["format=yuv420p,", "format=yuv420p:", "format=nv12"] {
            assert!(
                !graph.contains(eight_bit),
                "{eight_bit} beside a PQ transfer aborts ffmpeg: {graph}"
            );
        }
        assert!(!graph.contains("hwupload"), "{graph}");
        assert!(!graph.contains("libplacebo"), "{graph}");
        assert!(!graph.contains("zscale"), "{graph}");
        assert!(!graph[..reshape].contains("scale"), "{graph}");
    }

    /// Only the new rung carries a wider grade. This is the assertion that
    /// stops a future change from quietly widening an existing graph — every
    /// one of them ends in BT.709 8-bit, and `delivered_dynamic_range` says
    /// "sdr" for all of them on that basis.
    #[test]
    fn only_the_passthrough_rung_reports_a_grade_above_sdr() {
        for p in [
            Pipeline::VppQsv,
            Pipeline::TonemapVaapi,
            Pipeline::Libplacebo,
            Pipeline::TonemapOpencl,
            Pipeline::DoviTonemapx,
            Pipeline::Cpu,
        ] {
            assert_eq!(p.output_grade(), OutputGrade::Sdr, "{p:?}");
        }
        for p in [Pipeline::DoviPassthrough, Pipeline::Hdr10Passthrough] {
            assert_eq!(p.output_grade(), OutputGrade::Hdr10, "{p:?}");
        }
    }

    /// The plain HDR10 rung: the same grade as the Dolby one, a different
    /// graph, and a source set that must not overlap it.
    #[test]
    fn the_plain_hdr10_rung_touches_no_pixel_values_and_takes_no_dolby_source() {
        let p = Pipeline::Hdr10Passthrough;
        assert_eq!(p.output_grade(), OutputGrade::Hdr10);
        assert!(!p.on_gpu());
        assert_eq!(
            p.fallback(),
            None,
            "the CPU chain ends in BT.709 8-bit, so falling back to it would \
             swap the grade the client was promised"
        );
        assert!(p.decode_args().is_empty());
        assert!(p.init_args().is_empty());
        assert!(
            !p.requires_software_decode(),
            "there is no RPU side data for a hardware decode to drop, unlike \
             both Dolby graphs"
        );

        assert!(p.pairs_with(Encoder::Software));
        assert!(p.pairs_with(Encoder::Qsv));
        assert!(p.pairs_with(Encoder::Vaapi));
        assert!(Pipeline::DoviPassthrough.pairs_with(Encoder::Vaapi));
        // Reviewed VideoToolbox Main10 is independently proved by the route
        // planner; plain HDR10 has no Dolby metadata dependency.
        assert!(p.pairs_with(Encoder::VideoToolbox));
        assert!(
            !p.pairs_with(Encoder::Nvenc),
            "Nvenc has no measured HEVC Main10 route"
        );

        for source in [Some("hdr10"), Some("hdr10plus")] {
            assert!(p.handles(source), "{source:?}");
        }
        for source in [None, Some("hlg"), Some("dolby_vision")] {
            assert!(
                !p.handles(source),
                "{source:?} must not reach a graph that tags its output PQ"
            );
        }

        let graph = p
            .filters(Some(1920), 1080, Some("hdr10"))
            .expect("a chain for a real source");
        assert_eq!(graph, "scale=1920:1080,format=yuv420p10le");
        assert!(
            !graph.contains("tonemap"),
            "no tone-map, no RPU: that is the whole rung — {graph}"
        );
        assert!(
            graph.ends_with(OutputGrade::Hdr10.pixel_format()),
            "the chain has to end in the depth the encoder's transfer needs: {graph}"
        );
    }

    #[test]
    fn profile5_renderer_keeps_software_decode_and_may_encode_anywhere_probed() {
        let p = Pipeline::DoviTonemapx;
        assert!(p.pairs_with(Encoder::Software));
        // The encode half is no longer pinned: a 4K Dolby Vision source used
        // to cap every non-DV client at the software Auto rung (720p) while a
        // hardware encoder sat idle beside it. Admission is gated by the boot
        // probe (`ffmpeg::has_dovi_reshape_with`), not by this table.
        for encoder in [
            Encoder::Nvenc,
            Encoder::Qsv,
            Encoder::Vaapi,
            Encoder::VideoToolbox,
        ] {
            assert!(
                p.pairs_with(encoder),
                "encode may leave the CPU: {encoder:?}"
            );
        }
        // …but the DECODE half is the one that carries the RPU side data, and
        // it must never leave the CPU whatever the encoder is.
        assert!(p.requires_software_decode());
        assert!(
            !p.on_gpu(),
            "frames start in system memory on every pairing"
        );
        assert!(p.handles(Some("dolby_vision")));
        assert!(!p.handles(Some("hdr10")));
        assert!(p.requires_software_decode());
        assert_eq!(p.fallback(), None);

        let graph = p
            .filters(Some(1920), 1080, Some("dolby_vision"))
            .expect("Profile 5 graph");
        let reshape = graph.find("tonemapx=").expect("tonemapx transform");
        assert_eq!(reshape, 0, "Dolby reshape must precede scale: {graph}");
        assert!(graph.contains("apply_dovi=1"), "{graph}");
        assert!(graph.contains("scale=1920:1080"), "{graph}");
        assert!(graph.ends_with("format=yuv420p"), "{graph}");
        assert!(!graph.contains("hwupload"), "{graph}");
        assert!(!graph.contains("libplacebo"), "{graph}");
        assert!(!graph.contains("setparams"), "{graph}");
        assert!(!graph.contains("zscale"), "{graph}");
        assert!(!graph[..reshape].contains("scale"), "{graph}");

        // Pinned verbatim. An SDR client's Profile 5 picture is not allowed to
        // change because an HDR rung was added beside it, and "contains" style
        // assertions cannot see a reordering or an added option.
        assert_eq!(
            graph,
            "tonemapx=tonemap=bt2390:transfer=bt709:matrix=bt709:primaries=bt709:\
             range=tv:format=yuv420p:apply_dovi=1,scale=1920:1080,format=yuv420p"
        );
    }

    /// The CPU chain is the floor: it pairs with everything, handles
    /// everything, and is what everything else falls back to. Any change that
    /// makes it conditional makes some session unplayable.
    #[test]
    fn the_cpu_pipeline_is_unconditional() {
        for encoder in [
            Encoder::Software,
            Encoder::Nvenc,
            Encoder::Qsv,
            Encoder::Vaapi,
            Encoder::VideoToolbox,
        ] {
            assert!(Pipeline::Cpu.pairs_with(encoder));
        }
        for hdr in [None, Some("hdr10"), Some("hlg"), Some("dolby_vision")] {
            assert!(Pipeline::Cpu.handles(hdr), "cpu must handle {hdr:?}");
        }
        assert_eq!(Pipeline::Cpu.fallback(), None, "nothing is below it");
        assert!(Pipeline::Cpu.filters(None, 1080, Some("hdr10")).is_none());
        assert!(!Pipeline::Cpu.on_gpu());
    }

    /// Vendor graphs belong to their vendor's device. Pairing `vpp_qsv` with a
    /// VA-API encoder builds a graph that hands QSV surfaces to something that
    /// cannot read them — a failure at play time, from a combination nothing
    /// else would have rejected.
    #[test]
    fn vendor_graphs_only_pair_with_their_own_encoder() {
        assert!(Pipeline::VppQsv.pairs_with(Encoder::Qsv));
        assert!(!Pipeline::VppQsv.pairs_with(Encoder::Vaapi));
        assert!(!Pipeline::VppQsv.pairs_with(Encoder::Software));
        assert!(Pipeline::TonemapVaapi.pairs_with(Encoder::Vaapi));
        assert!(!Pipeline::TonemapVaapi.pairs_with(Encoder::Qsv));

        // The neutral ones go with any hardware encoder — but not with
        // software, where the download they'd force is the copy the whole
        // exercise exists to remove.
        for p in [Pipeline::Libplacebo, Pipeline::TonemapOpencl] {
            assert!(p.pairs_with(Encoder::Vaapi), "{p:?} + vaapi");
            assert!(p.pairs_with(Encoder::Nvenc), "{p:?} + nvenc");
            assert!(!p.pairs_with(Encoder::Software), "{p:?} + software");
        }
    }

    /// HLG and Dolby Vision route to the CPU no matter what a GPU probe said.
    /// The failure mode for both is a washed-out or wrong picture rather than
    /// an error — which is the worst kind, because it plays and nobody reports
    /// it (PERF-PLAN §5 scope guards).
    #[test]
    fn only_pq_sources_reach_a_gpu_graph() {
        // Node routing only: the macOS context owns its graphs' source classes.
        for p in CANDIDATES
            .iter()
            .filter(|p| p.on_gpu() && !p.requires_macos_context())
        {
            assert!(p.handles(Some("hdr10")), "{p:?} hdr10");
            assert!(p.handles(Some("hdr10plus")), "{p:?} hdr10+");
            assert!(!p.handles(Some("hlg")), "{p:?} must not take hlg");
            assert!(!p.handles(Some("dolby_vision")), "{p:?} must not take dv");
            // An SDR source has nothing to map, so nothing to get wrong — the
            // graph is chosen for its scaler there, not its tone-map.
            assert!(p.handles(None), "{p:?} sdr");
        }
    }

    /// A tone-map that never mentions bt709 is a tone-map that leaves the
    /// output tagged BT.2020 — which is how a correctly-mapped picture still
    /// renders wrong, and exactly what the probe's ffprobe assertion catches.
    #[test]
    fn every_gpu_graph_targets_bt709_for_hdr() {
        for p in CANDIDATES.iter().filter(|p| p.on_gpu()) {
            let hdr = p.filters(None, 1080, Some("hdr10")).expect("a graph");
            if p.requires_macos_context() {
                // Never offered an SDR source: the context scales those with
                // its own SDR graph.
                assert!(hdr.contains("transfer=bt709"), "{p:?}: {hdr}");
                continue;
            }
            assert!(
                hdr.contains("bt709") || hdr.contains("tonemap=1"),
                "{p:?} does not retarget colour: {hdr}"
            );
            assert!(hdr.contains("1080"), "{p:?} ignores the rung: {hdr}");

            // With no HDR the tone-map step drops out — the scaler is still
            // worth having on the GPU, but mapping an SDR source to SDR is
            // work that changes nothing.
            let sdr = p.filters(None, 1080, None).expect("a graph");
            assert!(
                !sdr.contains("tonemap"),
                "{p:?} tone-maps an SDR source: {sdr}"
            );
            assert!(sdr.contains("1080"), "{p:?} ignores the rung: {sdr}");
        }
    }

    /// Filters that hand GPU surfaces to an encoder that cannot read them are
    /// the classic graph bug. The two neutral pipelines download explicitly;
    /// the vendor ones deliberately do not, because their own encoder wants
    /// the surface.
    #[test]
    fn neutral_graphs_download_and_vendor_graphs_do_not() {
        for p in [Pipeline::Libplacebo, Pipeline::TonemapOpencl] {
            let g = p.filters(None, 1080, Some("hdr10")).expect("a graph");
            assert!(
                g.contains("hwdownload"),
                "{p:?} leaves frames on the GPU: {g}"
            );
        }
        for p in [Pipeline::VppQsv, Pipeline::TonemapVaapi] {
            let g = p.filters(None, 1080, Some("hdr10")).expect("a graph");
            assert!(
                !g.contains("hwdownload"),
                "{p:?} round-trips needlessly: {g}"
            );
            assert!(!g.contains("hwupload"), "{p:?} round-trips needlessly: {g}");
        }
    }

    /// A proven graph is a claim about the box, not about the session. Each
    /// of these three would otherwise fail quietly — a washed picture, a
    /// broken graph at play time, or a round trip that costs more than it
    /// saves — so the routing is decided once, up front, and logged.
    #[test]
    fn a_session_can_be_sent_back_to_the_cpu_by_what_it_is_doing() {
        let proven = Pipeline::VppQsv;

        // The happy case: proven graph, its own encoder, a heavy PQ source,
        // no burn.
        assert_eq!(
            Pipeline::for_session(proven, Encoder::Qsv, Some("hdr10"), true, false),
            Pipeline::VppQsv
        );

        // HLG and DV go back regardless of what the probe proved. (A DV
        // source whose base layer is HDR10-compatible never presents as
        // "dolby_vision" here — `transcode::routing_hdr` routes it as hdr10
        // before this question is asked. What reaches this arm is the DV
        // that has no PQ base to fall back on, and the CPU chain is right.)
        for hdr in [Some("hlg"), Some("dolby_vision")] {
            assert_eq!(
                Pipeline::for_session(proven, Encoder::Qsv, hdr, true, false),
                Pipeline::Cpu,
                "{hdr:?} must not reach a GPU graph"
            );
        }

        // A graph that cannot feed the running encoder: the encoder is what
        // changed (a hardware fallback to software mid-life, say), and the
        // combination would build a graph nothing can read.
        assert_eq!(
            Pipeline::for_session(proven, Encoder::Software, Some("hdr10"), true, false),
            Pipeline::Cpu
        );

        // A text subtitle comes down after the GPU has done the expensive
        // scale + tone-map, renders in system memory, then returns to QSV.
        assert_eq!(
            Pipeline::for_session(proven, Encoder::Qsv, Some("hdr10"), true, true),
            Pipeline::VppQsv
        );

        // A heavy SDR source still gets the GPU scaler: no tone-map to do, but
        // a 4K resize is worth having on the video-processing block.
        assert_eq!(
            Pipeline::for_session(proven, Encoder::Qsv, None, true, false),
            Pipeline::VppQsv
        );

        // A light source does not. The GPU decode/filter handoff is a risk
        // taken for a reason, and a 1080p H.264 scale is not a problem.
        assert_eq!(
            Pipeline::for_session(proven, Encoder::Qsv, Some("hdr10"), false, false),
            Pipeline::Cpu,
            "a light source is not worth the handoff"
        );

        // And a node that proved nothing stays on the CPU chain, whatever it
        // is asked.
        assert_eq!(
            Pipeline::for_session(Pipeline::Cpu, Encoder::Qsv, Some("hdr10"), true, false),
            Pipeline::Cpu
        );
    }

    /// Every route back to the CPU chain can say which one it was, and the
    /// answer never disagrees with the decision.
    ///
    /// The pairing is the point. `pipeline=cpu` on a 4K HDR session, on a box
    /// whose probe just cleared a GPU graph at 4.9×, reads as the GPU path
    /// being broken — when the usual truth is Dolby Vision, where the CPU
    /// chain is *correct*. A silent right answer and a silent wrong one look
    /// identical, so neither may be silent.
    #[test]
    fn every_route_to_the_cpu_chain_says_which_one_it_was() {
        let proven = Pipeline::VppQsv;
        let cases: &[(&str, Option<&str>, bool, bool, Encoder)] = &[
            // (what we expect the reason to mention, hdr, heavy, burns, encoder)
            (
                "Dolby Vision",
                Some("dolby_vision"),
                true,
                false,
                Encoder::Qsv,
            ),
            ("HLG", Some("hlg"), true, false, Encoder::Qsv),
            ("light source", Some("hdr10"), false, false, Encoder::Qsv),
            ("families", Some("hdr10"), true, false, Encoder::Software),
        ];
        for (expect, hdr, heavy, burns, encoder) in cases {
            let why = Pipeline::declined(proven, *encoder, *hdr, *heavy, *burns);
            let why = why.unwrap_or_else(|| panic!("{expect}: no reason given"));
            assert!(
                why.contains(expect),
                "{expect}: reason does not say so — {why:?}"
            );
            assert_eq!(
                Pipeline::for_session(proven, *encoder, *hdr, *heavy, *burns),
                Pipeline::Cpu,
                "{expect}: a reason was given but the graph was used anyway"
            );
        }

        // A session that gets the proven graph has nothing to explain…
        assert_eq!(
            Pipeline::declined(proven, Encoder::Qsv, Some("hdr10"), true, false),
            None
        );
        // …and neither does a node that never had one to decline. Reporting
        // "light source" on a software-only box would be noise on every
        // session it ever runs.
        assert_eq!(
            Pipeline::declined(Pipeline::Cpu, Encoder::Software, None, false, false),
            None
        );
    }

    #[test]
    fn interlaced_sources_decline_unqualified_gpu_filter_graphs() {
        let scan_type = ScanType::Interlaced(FieldOrder::Tff);
        let why = Pipeline::declined_with_scan(
            Pipeline::VppQsv,
            Encoder::Qsv,
            Some("hdr10"),
            true,
            false,
            scan_type,
        )
        .expect("interlaced graph is declined");
        assert!(why.contains("hardware deinterlace"), "{why}");
        assert_eq!(
            Pipeline::for_session_with_scan(
                Pipeline::VppQsv,
                Encoder::Qsv,
                Some("hdr10"),
                true,
                false,
                scan_type,
            ),
            Pipeline::Cpu
        );
        assert_eq!(
            Pipeline::for_session_with_scan(
                Pipeline::VppQsv,
                Encoder::Qsv,
                Some("hdr10"),
                true,
                false,
                ScanType::Unknown,
            ),
            Pipeline::VppQsv,
            "unknown input must not opt into a destructive filter"
        );
    }

    /// An explicit width pins every graph to the same frame the subtitle
    /// plane of a bitmap burn is scaled to. `None` keeps the keep-aspect
    /// spellings that also never upscale.
    #[test]
    fn an_exact_width_pins_the_frame_and_none_keeps_aspect() {
        let g = Pipeline::VppQsv
            .filters(Some(3840), 2160, Some("hdr10"))
            .expect("a graph");
        assert!(g.contains("vpp_qsv=w=3840:h=2160:tonemap=1"), "{g}");
        let g = Pipeline::VppQsv
            .filters(None, 1080, Some("hdr10"))
            .expect("a graph");
        assert!(g.contains("w=-1:h=1080"), "{g}");
        let g = Pipeline::TonemapOpencl
            .filters(Some(1920), 1080, Some("hdr10"))
            .expect("a graph");
        assert!(g.contains("scale=1920:1080,"), "{g}");
        let g = Pipeline::TonemapOpencl
            .filters(None, 1080, Some("hdr10"))
            .expect("a graph");
        assert!(g.contains("scale=-2:'min(1080,ih)'"), "{g}");
    }

    /// Failure goes to the floor, not to the next-best guess.
    #[test]
    fn a_failed_graph_falls_all_the_way_back() {
        for p in CANDIDATES.iter().filter(|p| p.on_gpu()) {
            assert_eq!(p.fallback(), Some(Pipeline::Cpu), "{p:?}");
        }
    }

    /// The vendor graphs need hardware surfaces to work on, so they must ask
    /// for them; the neutral ones must not, or they would override a decode
    /// choice made with knowledge they don't have.
    #[test]
    fn only_vendor_graphs_claim_the_decoder() {
        assert!(Pipeline::VppQsv.decode_args().contains(&"qsv".to_owned()));
        assert!(Pipeline::TonemapVaapi
            .decode_args()
            .contains(&"vaapi".to_owned()));
        for p in [Pipeline::Libplacebo, Pipeline::TonemapOpencl, Pipeline::Cpu] {
            assert!(
                p.decode_args().is_empty(),
                "{p:?} should not force a decoder"
            );
        }
        // …and the neutral ones need their own device instead.
        assert!(Pipeline::Libplacebo
            .init_args()
            .contains(&"vulkan=vk".to_owned()));
        assert!(Pipeline::TonemapOpencl
            .init_args()
            .contains(&"opencl=ocl".to_owned()));
        for p in [Pipeline::VppQsv, Pipeline::TonemapVaapi, Pipeline::Cpu] {
            assert!(p.init_args().is_empty(), "{p:?} needs no extra device");
        }
    }
}
