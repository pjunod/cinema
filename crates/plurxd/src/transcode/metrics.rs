use super::*;

/// Store-free, lock-free projection used by the Prometheus handler.
#[derive(Clone)]
pub(crate) struct TranscodeMetrics {
    pub(super) active_sessions: Arc<AtomicUsize>,
    pub(super) active_cache: crate::cachekeep::ActiveCacheMetrics,
    pub(super) decode_facts: Arc<crate::decode_facts::DecodeFactMetrics>,
    pub(super) caps: EncoderCaps,
    pub(super) codec_qualification: Arc<CodecQualificationMetrics>,
}

pub(super) struct CodecQualificationMetrics {
    pub(super) encoder_sessions:
        [[AtomicU64; QUALIFICATION_GRADES.len()]; QUALIFICATION_ENCODERS.len()],
    pub(super) pipeline_sessions: [AtomicU64; QUALIFICATION_PIPELINES.len()],
}

impl Default for CodecQualificationMetrics {
    fn default() -> Self {
        Self {
            encoder_sessions: std::array::from_fn(|_| std::array::from_fn(|_| AtomicU64::new(0))),
            pipeline_sessions: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}

impl CodecQualificationMetrics {
    pub(super) fn encoder_slot(encoder: Encoder) -> usize {
        match encoder {
            Encoder::Software => 0,
            Encoder::Nvenc => 1,
            Encoder::Qsv => 2,
            Encoder::Vaapi => 3,
            Encoder::VideoToolbox => 4,
        }
    }

    pub(super) fn grade_slot(grade: OutputGrade) -> usize {
        match grade {
            OutputGrade::Sdr => 0,
            OutputGrade::Hdr10 => 1,
        }
    }

    pub(super) fn pipeline_slot(pipeline: Pipeline) -> usize {
        match pipeline {
            Pipeline::VppQsv => 0,
            Pipeline::TonemapVaapi => 1,
            Pipeline::Libplacebo => 2,
            Pipeline::TonemapOpencl => 3,
            Pipeline::DoviTonemapx => 4,
            Pipeline::DoviPassthrough => 5,
            Pipeline::Hdr10Passthrough => 6,
            Pipeline::Cpu => 7,
            Pipeline::LibplaceboVaapi => 8,
        }
    }

    pub(super) fn record_encoder(&self, encoder: Encoder, grade: OutputGrade) {
        self.encoder_sessions[Self::encoder_slot(encoder)][Self::grade_slot(grade)]
            .fetch_add(1, Relaxed);
    }

    pub(super) fn record_pipeline(&self, pipeline: Pipeline) {
        self.pipeline_sessions[Self::pipeline_slot(pipeline)].fetch_add(1, Relaxed);
    }

    pub(super) fn prometheus(&self, caps: &EncoderCaps) -> String {
        let mut out = String::from(
            "# HELP plurx_encoder_available Whether boot validation test-encoded through this family.\n\
             # TYPE plurx_encoder_available gauge\n",
        );
        for encoder in QUALIFICATION_ENCODERS {
            out.push_str(&format!(
                "plurx_encoder_available{{family=\"{}\"}} {}\n",
                encoder.family_name(),
                u8::from(caps.available(encoder)),
            ));
        }
        out.push_str(
            "# HELP plurx_encoder_sessions_total Accepted encoding starts after their serving owner is registered, by family and output grade. Process-local; use reset-aware increase() or uninterrupted uptime for interval totals.\n\
             # TYPE plurx_encoder_sessions_total counter\n",
        );
        for encoder in QUALIFICATION_ENCODERS {
            for grade in QUALIFICATION_GRADES {
                out.push_str(&format!(
                    "plurx_encoder_sessions_total{{family=\"{}\",grade=\"{}\"}} {}\n",
                    encoder.family_name(),
                    grade.name(),
                    self.encoder_sessions[Self::encoder_slot(encoder)][Self::grade_slot(grade)]
                        .load(Relaxed),
                ));
            }
        }
        out.push_str(
            "# HELP plurx_tone_map_pipeline_sessions_total Accepted encoding starts after their serving owner is registered, by resolved video pipeline. Process-local; use reset-aware increase() or uninterrupted uptime for interval totals.\n\
             # TYPE plurx_tone_map_pipeline_sessions_total counter\n",
        );
        for pipeline in QUALIFICATION_PIPELINES {
            out.push_str(&format!(
                "plurx_tone_map_pipeline_sessions_total{{pipeline=\"{}\"}} {}\n",
                pipeline.name(),
                self.pipeline_sessions[Self::pipeline_slot(pipeline)].load(Relaxed),
            ));
        }
        out
    }
}

/// Bounded local facts published in the cluster media snapshot. None of these
/// fields require reading a library source path.
pub(crate) struct MediaNodeRuntime {
    pub(crate) scratch_bytes_free: u64,
    pub(crate) scratch_target_bytes: u64,
    pub(crate) active_sessions: usize,
    pub(crate) session_pressure_limit: usize,
    pub(crate) encoder_families: Vec<String>,
    pub(crate) max_target_height: i64,
    pub(crate) decoders: Vec<String>,
    pub(crate) tone_map_pipelines: Vec<String>,
    pub(crate) hardware_slots_used: usize,
    pub(crate) hardware_slots_max: usize,
    pub(crate) software_threads_used: usize,
    pub(crate) software_threads_max: usize,
    pub(crate) live_waiting: bool,
    pub(crate) background_active: bool,
}

/// Node-local answer to a diagnostics-only offer request. Calculating it does
/// not reserve capacity or open the media source.
pub(crate) struct MediaOfferProbe {
    pub(crate) active_sessions: usize,
    pub(crate) session_pressure_limit: usize,
    pub(crate) scratch_bytes_free: u64,
    pub(crate) decoder_supported: bool,
    pub(crate) target_supported: bool,
    pub(crate) cache_hit: bool,
    pub(crate) free_hardware_slots: usize,
    pub(crate) free_software_threads: usize,
    pub(crate) encoder: String,
    pub(crate) pipeline: String,
    pub(crate) recent_speed: Option<f64>,
}

impl TranscodeMetrics {
    pub(crate) fn snapshot(&self) -> (usize, usize) {
        (
            self.active_sessions.load(Relaxed),
            self.active_cache.active_entries(),
        )
    }

    pub(crate) fn decode_facts_prometheus(&self) -> String {
        self.decode_facts.prometheus()
    }

    pub(crate) fn codec_qualification_prometheus(&self) -> String {
        self.codec_qualification.prometheus(&self.caps)
    }
}
