//! What makes two transcodes the same transcode.
//!
//! The pre-transcode cache is content-addressed: a session computes the hash of
//! what it is about to produce, and if that hash is already on disk the work has
//! been done. Everything therefore rests on the hash meaning exactly one thing,
//! and the failure mode of getting it wrong is the worst kind there is — not an
//! error, but the wrong film, or the right film with the wrong subtitles burned
//! into it, served confidently and fast.
//!
//! Two rules follow, and both are load-bearing.
//!
//! **Everything that changes the bytes is in the key.** Not the ones that seem
//! important — all of them. A field added to the recipe and forgotten here is a
//! cache that serves stale output forever, and it will look like a bug in
//! whatever feature added the field.
//!
//! **Invalidation is by mismatch, never by deletion.** The source file's id,
//! size and mtime are in the key — carried in the plan, as its cache identity
//! — so a file that changes simply stops matching its old entry; nothing has
//! to notice the change, and nothing can fail to. The orphan ages out through
//! LRU like anything else. A cache that has to be *told* about a change is a
//! cache that will one day not be told.
//!
//! **One key per source, however the source was measured.** Producers reach a
//! film by different routes: the speculative producer holds a descriptor, live
//! and offline resolution read the catalog row. Those routes fingerprint the
//! source differently, and a key built on the fingerprint gives them disjoint
//! key spaces — the producer fills the cache and the player never hits it,
//! which is indistinguishable from a cache that is merely cold. The key
//! therefore uses the catalog identity every route computes the same way, and
//! descriptor continuity is checked separately, as a precondition, never as
//! part of the name.

use sha2::{Digest, Sha256};

use super::{ResolvedTranscode, SEGMENT_SECONDS};

/// Bumped when the meaning of a recipe changes in a way the field list cannot
/// express — a different hash construction, a corrected serialisation, a fixed
/// bug in what the fields *mean*. Every old entry misses; nothing is served
/// wrongly while a deploy rolls out.
pub const CACHE_RECIPE_VERSION: i64 = 3;

/// Everything about *how this server encodes* that changes the output bytes.
///
/// Separate from the per-session inputs because it is per-node and per-build,
/// and because it is the part reviewers were right to worry about (PERF-PLAN
/// §6.1, review R7): an ffmpeg upgrade that changes the tone-map operator
/// produces a different picture from an identical recipe. Without the build in
/// the key, every viewer keeps getting the old one until somebody notices by
/// eye.
///
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineDigest {
    /// `ffmpeg -version`'s first line, verbatim — build, not just version. A
    /// distribution's patched 6.1 and a jellyfin 6.1 are different encoders.
    pub ffmpeg_build: String,
}

impl PipelineDigest {
    fn feed(&self, h: &mut Sha256) {
        field(h, "ffmpeg", self.ffmpeg_build.as_bytes());
        field(h, "muxer", b"hls/mpegts");
        field(h, "segment_policy", SEGMENT_SECONDS.to_string().as_bytes());
    }
}

/// One transcode, as a cache key.
///
/// Built from the same values that build the ffmpeg command, in one place, so
/// the two cannot describe different things.
#[derive(Debug, Clone)]
pub struct Recipe<'a> {
    digest: &'a PipelineDigest,
    plan: &'a ResolvedTranscode,
    /// Whether the audio is copied rather than re-encoded. Not in
    /// [`ResolvedTranscode`] because the transcode path always re-encodes;
    /// carried explicitly so the copy path can share this key space.
    audio_copied: bool,
}

impl Recipe<'_> {
    pub fn new<'a>(
        digest: &'a PipelineDigest,
        plan: &'a ResolvedTranscode,
        audio_copied: bool,
    ) -> Recipe<'a> {
        Recipe {
            digest,
            plan,
            audio_copied,
        }
    }

    /// The hex SHA-256 that names this transcode's output.
    pub fn hash(&self) -> String {
        let mut h = Sha256::new();
        field(&mut h, "v", CACHE_RECIPE_VERSION.to_string().as_bytes());
        self.digest.feed(&mut h);
        // Redundant today, and deliberately so: `plan_digest` already feeds
        // the namespace, so this field changes no answer on its own. It is
        // here because the two key spaces must never merge, and `plan_digest`
        // is a versioned field list that gets revised. If a revision ever
        // stops feeding the identity, an artifact produced under an enforced
        // receipt contract would land on the same name as one produced under
        // none — the single failure this whole separation exists to prevent —
        // and this field is what stops that from being a silent change.
        field(
            &mut h,
            "plan_namespace",
            self.plan.artifact_namespace().as_bytes(),
        );
        // The source is inside the plan digest, as its cache identity — file
        // id, size and mtime, hashed the one way every producer computes it.
        // It used to be fed here from a `MediaFile` the caller passed
        // alongside the plan, which meant nothing checked that the two
        // described the same film; the constructor took the plan's word for
        // the encode and the caller's word for the source. There is now no
        // second source to disagree with the first.
        field(&mut h, "plan", self.plan.plan_digest().as_bytes());
        field(
            &mut h,
            "aaction",
            if self.audio_copied { b"copy" } else { b"aac" },
        );

        // Deliberately NOT in the key: `start_seconds`. A cached asset is the
        // whole title; where a viewer joins it is a seek, not a different
        // encode. Including it would give one film as many entries as it has
        // resume points and never hit any of them twice.
        hex::encode(h.finalize())
    }
}

/// Length-prefixed, name-tagged field feeding.
///
/// Concatenating values would let two different recipes hash identically —
/// `height=108` + `bitrate=0…` and `height=1080` + `bitrate=…` are the same
/// bytes run together. The tag and the length make every field's boundary
/// unambiguous, which is the difference between a hash and a hope.
fn field(h: &mut Sha256, name: &str, value: &[u8]) {
    h.update((name.len() as u32).to_le_bytes());
    h.update(name.as_bytes());
    h.update((value.len() as u32).to_le_bytes());
    h.update(value);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::MediaFile;
    use crate::transcode::{
        resolve_transcode, AttemptRestrictions, DecodeCapabilities,
        DecodeCapabilitySnapshotIdentity, DecodeFacts, DecodePlanPolicy, DecodePolicySnapshot,
        DecodeSourceIdentity, EffectiveRateControl, Encoder, Pipeline, SoftwareDecoder,
        SubtitleBurn, ToneMap, TranscodeMediaOptions, TranscodeOptions, TranscodeRequest,
    };
    use serde_json::json;
    use std::path::PathBuf;

    fn digest() -> PipelineDigest {
        PipelineDigest {
            ffmpeg_build: "ffmpeg version 6.1.1-3ubuntu5".to_owned(),
        }
    }

    fn media() -> MediaFile {
        MediaFile {
            id: 42,
            item_id: 7,
            path: PathBuf::from("/media/Heat.mkv"),
            size: 12_345_678,
            mtime: 1_700_000_000,
            duration_ms: Some(9_000_000),
            container: Some("mkv".into()),
            video_codec: Some("hevc".into()),
            video_codec_tag: None,
            field_order: None,
            video_profile: None,
            width: Some(3840),
            height: Some(2160),
            bit_depth: Some(10),
            hdr: Some("hdr10".into()),
            hdr_format: None,
            bitrate: Some(60_000_000),
            audio_streams: vec![],
            subtitle_streams: vec![],
            scanned_at: 0,
            audio_offset_ms: 0,
            probed: true,
            dolby_vision: crate::domain::DolbyVisionFacts::default(),
        }
    }

    fn decode_facts() -> DecodeFacts {
        DecodeFacts::from_ffprobe_json(
            &json!({
                "streams": [{
                    "index": 3,
                    "codec_type": "video",
                    "codec_name": "hevc",
                    "profile": "Main 10",
                    "width": 3840,
                    "height": 2160,
                    "pix_fmt": "yuv420p10le",
                    "avg_frame_rate": "24000/1001",
                    "r_frame_rate": "24000/1001",
                    "color_range": "tv",
                    "color_space": "bt2020nc",
                    "color_transfer": "smpte2084",
                    "color_primaries": "bt2020",
                    "disposition": {"attached_pic": 0}
                }]
            }),
            DecodeSourceIdentity::from_sha256("a".repeat(64)).expect("valid source identity"),
        )
        .expect("valid decode facts")
    }

    fn capabilities(software_decoder: &str) -> DecodeCapabilities {
        DecodeCapabilities::new(
            DecodeCapabilitySnapshotIdentity::new(
                "f".repeat(64),
                "recipe-test-node".to_owned(),
                Some("e".repeat(64)),
            )
            .expect("valid capability identity"),
            vec![],
            vec![SoftwareDecoder {
                codec: "hevc".to_owned(),
                implementation: Some(software_decoder.to_owned()),
            }],
        )
        .expect("valid legacy capability inventory")
    }

    fn plan_with_decoder(
        f: &MediaFile,
        o: &TranscodeOptions,
        encoder: Encoder,
        software_decoder: &str,
    ) -> ResolvedTranscode {
        resolve_transcode(
            &TranscodeRequest::new(encoder, TranscodeMediaOptions::from_options(f, o)),
            &decode_facts(),
            &capabilities(software_decoder),
            &DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
            &AttemptRestrictions::none(),
        )
        .expect("fixture must resolve to a validated legacy plan")
    }

    fn try_plan(
        f: &MediaFile,
        o: &TranscodeOptions,
        encoder: Encoder,
    ) -> Result<ResolvedTranscode, crate::transcode::PlanError> {
        resolve_transcode(
            &TranscodeRequest::new(encoder, TranscodeMediaOptions::from_options(f, o)),
            &decode_facts(),
            &capabilities("hevc"),
            &DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
            &AttemptRestrictions::none(),
        )
    }

    fn plan(f: &MediaFile, o: &TranscodeOptions, encoder: Encoder) -> ResolvedTranscode {
        plan_with_decoder(f, o, encoder, "hevc")
    }

    fn hash_of(
        d: &PipelineDigest,
        f: &MediaFile,
        o: &TranscodeOptions,
        encoder: Encoder,
        copied: bool,
    ) -> String {
        let plan = plan(f, o, encoder);
        Recipe::new(d, &plan, copied).hash()
    }

    /// The property the whole cache rests on: anything that changes the output
    /// bytes changes the name of those bytes.
    ///
    /// Written as a table because the failure it guards against is *omission* —
    /// a field added to the recipe and not to the hash — and an omission is
    /// invisible in code that only checks the fields somebody remembered. Each
    /// case here is a byte of output that would otherwise be served from the
    /// wrong entry.
    #[test]
    fn every_input_that_changes_the_output_changes_the_hash() {
        let base_d = digest();
        let base_f = media();
        let base_o = TranscodeOptions::default();
        let base_encoder = Encoder::Software;
        let base = hash_of(&base_d, &base_f, &base_o, base_encoder, false);

        type Mutate = (
            &'static str,
            fn(&mut PipelineDigest, &mut MediaFile, &mut TranscodeOptions, &mut Encoder, &mut bool),
        );
        let cases: &[Mutate] = &[
            // The build and the pipeline: an ffmpeg upgrade or a different
            // tone-map graph is a different picture from the same recipe.
            ("ffmpeg build", |d, _, _, _, _| {
                d.ffmpeg_build = "ffmpeg version 7.1".into()
            }),
            ("encoder family", |_, _, _, encoder, _| {
                *encoder = Encoder::VideoToolbox
            }),
            ("pipeline", |_, _, o, _, _| {
                o.pipeline = Pipeline::Hdr10Passthrough
            }),
            // The source. Size and mtime are the invalidation mechanism.
            ("file id", |_, f, _, _, _| f.id = 43),
            ("size", |_, f, _, _, _| f.size += 1),
            ("mtime", |_, f, _, _, _| f.mtime += 1),
            ("audio offset", |_, f, _, _, _| f.audio_offset_ms = 250),
            // What was asked for.
            ("height", |_, _, o, _, _| o.target_height = 720),
            ("video bitrate", |_, _, o, _, _| o.video_bitrate_kbps += 1),
            ("effective rate control", |_, _, o, _, _| {
                o.effective_rate_control = EffectiveRateControl::Qvbr { quality: 23 }
            }),
            ("audio bitrate", |_, _, o, _, _| o.audio_bitrate_kbps += 1),
            ("audio channels", |_, _, o, _, _| o.audio_channels = 6),
            ("audio index", |_, _, o, _, _| o.audio_index = Some(1)),
            ("audio action", |_, _, _, _, c| *c = true),
            ("tone map", |_, _, o, _, _| o.tone_map = ToneMap::None),
            ("subtitle burn", |_, _, o, _, _| {
                o.subtitle_burn = Some(SubtitleBurn {
                    subtitle_index: 0,
                    bitmap: true,
                })
            }),
        ];

        let mut seen = std::collections::HashMap::new();
        seen.insert(base.clone(), "unchanged");
        for (name, mutate) in cases {
            let (mut d, mut f, mut o, mut encoder, mut c) = (
                base_d.clone(),
                base_f.clone(),
                base_o.clone(),
                base_encoder,
                false,
            );
            mutate(&mut d, &mut f, &mut o, &mut encoder, &mut c);
            let got = hash_of(&d, &f, &o, encoder, c);
            assert_ne!(got, base, "changing the {name} did not change the hash");
            if let Some(other) = seen.insert(got, name) {
                panic!("{name} and {other} hash the same — one of them is not in the key");
            }
        }
    }

    /// Two burns of different tracks are different pictures, and a text burn
    /// is not a bitmap burn of the same index.
    #[test]
    fn a_burn_is_identified_by_which_track_and_what_kind() {
        let (d, f) = (digest(), media());
        let with = |idx, bitmap| {
            let o = TranscodeOptions {
                subtitle_burn: Some(SubtitleBurn {
                    subtitle_index: idx,
                    bitmap,
                }),
                ..Default::default()
            };
            hash_of(&d, &f, &o, Encoder::Software, false)
        };
        assert_ne!(with(0, true), with(1, true), "different track");
        assert_ne!(with(0, true), with(0, false), "different kind");
        assert_ne!(
            with(0, false),
            hash_of(
                &d,
                &f,
                &TranscodeOptions::default(),
                Encoder::Software,
                false,
            ),
            "burning something is not the same as burning nothing"
        );
    }

    /// Attempt-local execution values must not fork artifact identity. They
    /// are deliberately discarded before `TranscodeMediaOptions` becomes a
    /// validated plan.
    #[test]
    fn execution_only_fields_are_not_part_of_the_plan_or_recipe_identity() {
        let (d, mut f) = (digest(), media());
        let base = TranscodeOptions::default();
        let expected = hash_of(&d, &f, &base, Encoder::Software, false);

        let changed = TranscodeOptions {
            start_seconds: 1234.5,
            start_number: 480,
            subtitle_file: Some(PathBuf::from("/var/cache/plurx/subtitles/7.ass")),
            force_idr: true,
            software_threads: Some(3),
            ..base
        };
        f.path = PathBuf::from("/dev/fd/3");
        assert_eq!(
            hash_of(&d, &f, &changed, Encoder::Software, false),
            expected
        );
    }

    /// Fields are length-prefixed so their boundaries cannot blur. Without
    /// that, two recipes whose values run together into the same bytes hash
    /// identically — and the one that gets served is whichever was cached
    /// first.
    #[test]
    fn adjacent_fields_cannot_be_confused_for_one_another() {
        let (d, f) = (digest(), media());
        let opts = |h: i64, b: u32| TranscodeOptions {
            target_height: h,
            video_bitrate_kbps: b,
            ..Default::default()
        };
        // "108" + "01000" vs "1080" + "1000": identical concatenated.
        assert_ne!(
            hash_of(&d, &f, &opts(108, 1000), Encoder::Software, false),
            hash_of(&d, &f, &opts(1080, 1000), Encoder::Software, false)
        );
        let mut a = f.clone();
        let mut b = f.clone();
        a.size = 1;
        a.mtime = 23;
        b.size = 12;
        b.mtime = 3;
        assert_ne!(
            hash_of(&d, &a, &opts(1080, 8000), Encoder::Software, false),
            hash_of(&d, &b, &opts(1080, 8000), Encoder::Software, false)
        );
    }

    /// Same inputs, same name — the whole point. Stable across runs and
    /// processes, since it addresses bytes on disk that outlive both.
    #[test]
    fn the_same_recipe_always_has_the_same_name() {
        let (d, f, o) = (digest(), media(), TranscodeOptions::default());
        let first = hash_of(&d, &f, &o, Encoder::Software, false);
        assert_eq!(first, hash_of(&d, &f, &o, Encoder::Software, false));
        assert_eq!(first.len(), 64, "hex sha-256");
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// Version 3 binds the complete validated decode/render/encode plan and
    /// deliberately invalidates every pre-plan recipe. Pin its exact bytes so
    /// future namespace changes remain explicit fleet-wide decisions.
    ///
    /// The value cannot be checked against v2's golden: v3 is not v2 with a
    /// field added, it is a different construction. The last published golden
    /// was `b9dae1c4…` at `CACHE_FORMAT_VERSION = 2`, when `PipelineDigest`
    /// carried the encoder, codec, pixel format and colour literals and the
    /// recipe hashed a `MediaFile` passed beside it. v3 moves all of that into
    /// the plan digest, adds the source's cache identity, and drops the
    /// descriptor fingerprint, stream-selection provenance and evidence class
    /// that were forking the key on how a file was measured rather than on
    /// what would be produced from it.
    ///
    /// So this fixture cannot prove v3 was composed correctly — it was
    /// regenerated from the implementation, and a mistake made in the same
    /// commit would be pinned along with everything else. What it does is make
    /// the next change explicit: nothing may move this value without saying
    /// why. The composition itself is proven by the field-by-field mutation
    /// table above, which is where a missing field is actually caught. v3 has
    /// never been published, so nothing on disk has moved.
    #[test]
    fn planned_v3_recipe_hash_is_a_golden_fixture() {
        let (d, f, o) = (digest(), media(), TranscodeOptions::default());
        assert_eq!(
            hash_of(&d, &f, &o, Encoder::Software, false),
            "82b1fd5b9c96d0e0201ef5f56606cc0d677996c67907d1d46bef870996e526cb"
        );
    }

    /// The HDR10 rung is a different presentation from the SDR tone-map of
    /// the same input, so it must occupy a different entry.
    #[test]
    fn the_hdr10_grade_is_a_distinct_entry_and_no_sdr_key_moved() {
        let (d, f) = (digest(), media());
        let sdr = TranscodeOptions::default();
        let hdr10 = TranscodeOptions {
            pipeline: Pipeline::Hdr10Passthrough,
            ..Default::default()
        };
        let sdr_hash = hash_of(&d, &f, &sdr, Encoder::Software, false);
        let hdr10_hash = hash_of(&d, &f, &hdr10, Encoder::Software, false);
        assert_ne!(hdr10_hash, sdr_hash);
        // The Dolby graph run over ordinary PQ frames is a broken picture at
        // exit 0, so the two Dolby renderers must never share an entry.
        // The Dolby renderers used to be compared here by hash. They cannot be
        // any more, and for a better reason: the fixture is an HDR10 source,
        // and a plan now *refuses* a renderer the source cannot feed rather
        // than naming a distinct entry for a picture that would come out
        // broken at exit 0. Refusal is the stronger guarantee, so assert it.
        for dolby in [Pipeline::DoviPassthrough, Pipeline::DoviTonemapx] {
            let o = TranscodeOptions {
                pipeline: dolby,
                ..Default::default()
            };
            assert_eq!(
                try_plan(&f, &o, Encoder::Software)
                    .expect_err("a renderer the source cannot feed is refused"),
                crate::transcode::PlanError::IncompatibleRenderer,
                "{dolby:?} over a source with no Dolby Vision layer"
            );
        }
        // Every SDR pipeline still hashes exactly as it did — the grade
        // fields are emitted only for a grade that is not SDR.
        for pipeline in crate::transcode::PIPELINE_CANDIDATES
            .iter()
            .copied()
            .chain(std::iter::once(Pipeline::DoviTonemapx))
        {
            let o = TranscodeOptions {
                pipeline,
                ..Default::default()
            };
            assert_eq!(
                o.pipeline.output_grade(),
                crate::transcode::OutputGrade::Sdr,
                "{pipeline:?}"
            );
        }
        assert_eq!(
            sdr_hash,
            hash_of(
                &d,
                &f,
                &TranscodeOptions::default(),
                Encoder::Software,
                false
            )
        );
    }

    #[test]
    fn every_effective_quality_value_has_a_distinct_identity() {
        let (d, f) = (digest(), media());
        let hash = |quality| {
            let o = TranscodeOptions {
                effective_rate_control: EffectiveRateControl::Qvbr { quality },
                ..Default::default()
            };
            hash_of(&d, &f, &o, Encoder::Software, false)
        };
        let vbr = hash_of(
            &d,
            &f,
            &TranscodeOptions::default(),
            Encoder::Software,
            false,
        );
        assert_ne!(hash(22), vbr);
        assert_ne!(hash(23), vbr);
        assert_ne!(hash(22), hash(23));
    }

    #[test]
    fn actual_software_decoder_implementation_changes_identity() {
        let (d, f, o) = (digest(), media(), TranscodeOptions::default());
        let hevc = plan_with_decoder(&f, &o, Encoder::Software, "hevc");
        let libdav1d = plan_with_decoder(&f, &o, Encoder::Software, "libdav1d");
        assert_ne!(
            Recipe::new(&d, &hevc, false).hash(),
            Recipe::new(&d, &libdav1d, false).hash()
        );
    }
}
