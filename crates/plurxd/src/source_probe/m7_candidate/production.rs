//! Held NP1 baseline, ROI controls, encode and author/readback.

use super::journal::{self, AuthoredProof, Binding, EncodeProof, GateProof, Intent, Journal};
use super::{
    hash, EncoderGrid, FrozenRecipe, Hash, RateAttempt, RateDecision, RateStop, Region, RoiControl,
    FRAMES, MAX_ENCODED_BYTES,
};
use crate::{
    dv_tools::ToolSet,
    ffmpeg::EncodedExecutable,
    fragment_index_cluster::SourceFence,
    producer_spawn::{ChildStdio, ChildStdios, Descriptors},
    source_probe::m7_comparison::{
        observe_census, AuthoredClip, CensusRequest, FrozenProtocol, HeldTitle, PreparedPlanes,
        TitleContext,
    },
};
use dolby_vision::rpu::{dovi_rpu::DoviRpu, vdr_dm_data::VdrDmData};
use plurx_core::{
    fmp4::{self, FragmentReader, Unit},
    fs_secure::{open_directory_nofollow_blocking, open_read_nofollow_blocking},
    hevc_configuration::Trace,
    process::{
        bounded,
        source_lifetime::{self, ArgumentProfile},
        ChildWork,
    },
    store::dv_m7_attempts::{M7CandidateIntent, M7CandidateReservation},
    transcode::{
        dv_m7_receipt::M7SourceProbe,
        dv_m7_scoring::capture::{
            AuthoringFrameBinding, CaptureLayout, CaptureRole, FrameEvidence,
        },
        dv_processing::dv_author_profile81_window,
        Rational,
    },
};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    fs::File,
    io::{Read, Write},
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

const ENCODE_LIMIT: Duration = Duration::from_secs(900);
const TRACE_LIMIT: Duration = Duration::from_secs(60);
const MAX_ENCODE_LOG: usize = 1024 * 1024;
const MAX_TRACE_LOG: usize = 8 * 1024 * 1024;
const MAX_LIB_BYTES: u64 = 256 * 1024 * 1024;
const PLAN_HEADER: usize = 212;
const PLAN_ROW: usize = 204;
const RECEIPT_CFG: usize = 256;
const RECEIPT_ROW: usize = 364;
const RECEIPT_BYTES: usize =
    PLAN_HEADER + 8 + RECEIPT_CFG + FRAMES as usize * (8 + RECEIPT_ROW) + 24;
const MOVFLAGS: &str = "+empty_moov+default_base_moof+frag_keyframe+frag_discont";

#[derive(Debug, thiserror::Error)]
pub(in crate::source_probe) enum PrepareError {
    #[error("NP1 preparation authority differs: {0}")]
    Authority(&'static str),
    #[error("NP1 preparation failed: {0}")]
    Execution(String),
    #[error("NP1 preparation cancelled")]
    Cancelled,
    #[error("NP1 did not land a rate-only candidate: {0:?}")]
    RateInconclusive(RateStop),
    #[error("NP1 actual authored video packet rate missed the frozen band")]
    ActualRate,
    #[error("NP1 candidate slot {slot} was consumed without a recoverable verified result")]
    IncompleteAttempt { slot: u8 },
}

type Result<T> = std::result::Result<T, PrepareError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Packet {
    evidence: FrameEvidence,
    size: u32,
    sync: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct PacketBook {
    packets: Vec<Packet>,
    payload_bytes: u64,
}

struct CompletedEncode {
    bytes: Vec<u8>,
    packets: PacketBook,
    closure: Hash,
    receipt_sha: Hash,
    trace_configuration: String,
}

#[expect(
    clippy::too_many_arguments,
    reason = "candidate authoring binds the title claim, frozen recipe, held planes, scratch and cancellation"
)]
pub(in crate::source_probe) async fn prepare_authored(
    context: &TitleContext<'_>,
    title: &HeldTitle,
    frozen: &FrozenProtocol,
    recipe: &FrozenRecipe,
    planes: PreparedPlanes,
    source_dv_level: u8,
    scratch: &TempDir,
    trial_directory: &Path,
    cancel: &CancellationToken,
) -> Result<AuthoredClip> {
    let grid = recipe.encoder_grid();
    if context.demand.playback_requested
        || context.admission.is_probe_only()
        || planes.layout.role != CaptureRole::SourceFel
        || planes.frames.len() != FRAMES as usize
        || !planes.composed.unchanged()
        || !title.source.unchanged()
        || planes.source_object_version != title.source.object_version()
        || planes.source_sha256 != title.content_sha256
        || planes.probe_sha256 != title.probe_sha256
        || frozen.source_sha256 != title.content_sha256
        || frozen.probe_sha256 != title.probe_sha256
        || !(1..=13).contains(&source_dv_level)
        || frozen.recipe_sha256 != recipe.recipe_sha256()
        || Rational::new(grid.timescale, grid.frame_ticks) != Some(frozen.encoder_rate)
    {
        return Err(PrepareError::Authority(
            "title, source planes or frozen recipe",
        ));
    }
    check_cancel(cancel)?;
    let (encoder_cli, encoder_cli_sha) = context.tools.m7_encoder().ok_or(
        PrepareError::Authority("verified independent M7 encoder CLI and closure"),
    )?;
    let ffmpeg = EncodedExecutable::capture_at(encoder_cli.to_owned())
        .await
        .map_err(PrepareError::Execution)?;
    if ffmpeg.digest != encoder_cli_sha {
        return Err(PrepareError::Authority(
            "M7 encoder CLI differs from its bundle",
        ));
    }
    let (muxer, mux_sha) = context
        .tools
        .muxer()
        .ok_or(PrepareError::Authority("verified ROI muxer"))?;
    let mux_sha = parse_hash(mux_sha)?;
    if !ffmpeg.is_current().await
        || !context.tools.is_current().await
        || !context.tools.muxer_is_current().await
        || !context.tools.m7_encoder_is_current().await
    {
        return Err(PrepareError::Authority("encoder tools moved"));
    }

    let scope = super::super::m7_title::candidate_scope(context, title);
    let probe = M7SourceProbe::from_probe_json(&context.demand.probe_json).map_err(execution)?;
    let mut ledger = context
        .fence
        .m7_candidate_slots(&probe, &scope)
        .await
        .map_err(execution)?
        .ok_or(PrepareError::Authority(
            "live replicated candidate protocol anchor",
        ))?;
    if ledger.source_sha256 != frozen.source_sha256
        || ledger.protocol_sha256 != frozen.protocol_sha256
        || ledger.recipe_sha256 != frozen.recipe_sha256
        || ledger.maximum_attempts != recipe.rate.maximum_attempts
    {
        return if ledger.slots.is_empty() {
            Err(PrepareError::Authority(
                "replicated candidate anchor differs",
            ))
        } else {
            Err(PrepareError::IncompleteAttempt { slot: 0 })
        };
    }
    if ledger.slots.len() > usize::from(recipe.rate.maximum_attempts) {
        return Err(PrepareError::Authority(
            "replicated attempt count exceeds frozen cap",
        ));
    }
    let journal = blocking({
        let path = trial_directory.to_owned();
        let source = frozen.source_sha256;
        let protocol = frozen.protocol_sha256;
        move || {
            Journal::open(&path, source, protocol)
                .map(Arc::new)
                .map_err(journal_error)
        }
    })
    .await?;

    let source_rpus: Vec<_> = planes
        .frames
        .iter()
        .map(|row| row.source_rpu.clone())
        .collect();
    let package_sha = package_identity(&planes)?;
    let local_binding = Binding {
        source: frozen.source_sha256,
        probe: frozen.probe_sha256,
        protocol: frozen.protocol_sha256,
        recipe: frozen.recipe_sha256,
        package: package_sha,
        bundle: parse_hash(&context.tools.backend_digest())?,
        muxer: mux_sha,
        ffmpeg: parse_hash(&ffmpeg.digest)?,
        source_generation: hash(title.source.object_version().as_bytes()),
        source_dv_level,
        maximum_attempts: recipe.rate.maximum_attempts,
    };
    let runner = EncoderRun {
        context,
        ffmpeg: &ffmpeg,
        muxer,
        recipe,
        frozen,
        planes: &planes,
        package_sha,
        scratch,
        cancel,
    };
    let recorded = blocking({
        let journal = Arc::clone(&journal);
        move || journal.gate(local_binding).map_err(journal_error)
    })
    .await;
    let gate = match (recorded, ledger.slots.is_empty()) {
        (Ok(Some(proof)), _) => proof,
        (Ok(None) | Err(PrepareError::Authority(_)), false) => {
            return Err(PrepareError::IncompleteAttempt { slot: 0 });
        }
        (Ok(None), true) => {
            check_publish(context, title, cancel).await?;
            let gate = runner.verify_controls(local_binding).await?;
            check_publish(context, title, cancel).await?;
            blocking({
                let journal = Arc::clone(&journal);
                let gate = gate.clone();
                let allowed = active(context, cancel);
                move || {
                    journal
                        .save_gate(local_binding, &gate, allowed)
                        .map_err(journal_error)
                }
            })
            .await?;
            gate
        }
        (Err(error), _) => return Err(error),
    };
    let gate_sha = gate.identity();
    let closure = gate.closure;
    let mut attempts = Vec::new();
    let mut candidate = None;
    loop {
        check_cancel(cancel)?;
        match recipe.rate_decision(&attempts).map_err(execution)? {
            RateDecision::Encode(target) => {
                let slot = attempts.len() as u8;
                let intent = Intent::new(local_binding, gate_sha, closure, slot, target);
                let replicated = M7CandidateIntent {
                    source_sha256: frozen.source_sha256,
                    protocol_sha256: frozen.protocol_sha256,
                    recipe_sha256: frozen.recipe_sha256,
                    binding_sha256: intent.identity(),
                    slot,
                    target_kbps: target,
                    maximum_attempts: recipe.rate.maximum_attempts,
                };
                let encoded = if slot_route(&ledger.slots, &replicated)? == SlotRoute::Reuse {
                    let journal = Arc::clone(&journal);
                    let raster = planes.layout.raster;
                    blocking(move || restore_encoded(&journal, intent, grid, raster)).await?
                } else {
                    // Capacity/preemption before reservation consumes nothing.
                    // Once committed, a lost reply or crash cannot authorize
                    // this slot again, even if there are no local files.
                    blocking({
                        let journal = Arc::clone(&journal);
                        move || journal.admit_bytes().map_err(journal_error)
                    })
                    .await?;
                    check_publish(context, title, cancel).await?;
                    match context
                        .fence
                        .reserve_m7_candidate_slot(&probe, &scope, replicated.clone())
                        .await
                        .map_err(execution)?
                    {
                        M7CandidateReservation::Reserved => ledger.slots.push(replicated),
                        M7CandidateReservation::AlreadyConsumed => {
                            return Err(PrepareError::Authority(
                                "concurrent replicated slot reservation",
                            ));
                        }
                        M7CandidateReservation::Fenced => return Err(PrepareError::Cancelled),
                        M7CandidateReservation::Conflict => {
                            return Err(PrepareError::IncompleteAttempt { slot });
                        }
                    }
                    blocking({
                        let journal = Arc::clone(&journal);
                        let allowed = active(context, cancel);
                        move || journal.begin(intent, allowed).map_err(journal_error)
                    })
                    .await?;
                    check_publish(context, title, cancel).await?;
                    let encoded = runner.run(target, RoiControl::Candidate, gate_sha).await?;
                    if encoded.closure != closure {
                        return Err(PrepareError::IncompleteAttempt { slot });
                    }
                    check_publish(context, title, cancel).await?;
                    blocking({
                        let journal = Arc::clone(&journal);
                        let allowed = active(context, cancel);
                        move || seal_encoded(&journal, intent, encoded, allowed)
                    })
                    .await?
                };
                attempts.push(RateAttempt {
                    target_kbps: target,
                    encoded_video_packet_bytes: encoded.packets.payload_bytes,
                });
                candidate = Some((intent, encoded));
            }
            RateDecision::Inconclusive(reason) => {
                return Err(PrepareError::RateInconclusive(reason))
            }
            RateDecision::PredictedInBand { .. } => {
                if attempts.len() < ledger.slots.len() {
                    return Err(PrepareError::IncompleteAttempt {
                        slot: attempts.len() as u8,
                    });
                }
                break;
            }
        }
    }
    let (intent, candidate) =
        candidate.ok_or(PrepareError::Authority("completed encoded candidate"))?;
    let encoded_sha = hash(&candidate.bytes);
    let encoded_vcl_sha256: Vec<_> = candidate
        .packets
        .packets
        .iter()
        .map(|packet| packet.evidence.bl_vcl_sha256)
        .collect();
    let raster = planes.layout.raster;
    let encoded = candidate.bytes;
    let stop = cancel.clone();
    let (recovered, authored, packets) = blocking({
        let journal = Arc::clone(&journal);
        move || {
            check_cancel(&stop)?;
            let recovered = journal
                .authored(intent, encoded_sha)
                .map_err(journal_error)?;
            let (proof, authored) = match recovered {
                Some((proof, bytes)) => (Some(proof), bytes),
                None => (
                    None,
                    dv_author_profile81_window(
                        &encoded,
                        (u32::from(raster[0]), u32::from(raster[1])),
                        grid.timescale,
                        0,
                        grid.frame_ticks,
                        &source_rpus,
                        source_dv_level,
                    )
                    .map_err(execution)?,
                ),
            };
            let packets = packet_book(&authored, grid, raster, true, Some(&source_rpus)).map_err(
                |error| {
                    if proof.is_some() {
                        PrepareError::IncompleteAttempt { slot: intent.slot }
                    } else {
                        error
                    }
                },
            )?;
            check_cancel(&stop)?;
            Ok((proof, authored, packets))
        }
    })
    .await?;
    if !packets
        .packets
        .iter()
        .zip(&encoded_vcl_sha256)
        .all(|(clip, orig)| &clip.evidence.bl_vcl_sha256 == orig)
    {
        return Err(PrepareError::Authority(
            "authored VCL differs from encoder packets",
        ));
    }

    let authored_sha256 = hash(&authored);
    let authored_extent = authored.len() as u64;
    let authored_dir = tempfile::tempdir_in(scratch.path()).map_err(execution)?;
    let authored_path = authored_dir.path().join("authored.mp4");
    let source = owned_file(authored_path.clone(), authored).await?;
    let candidate_layout = CaptureLayout {
        role: CaptureRole::AuthoredCandidate,
        video_index: 0,
        time_base: Rational::new(1, grid.timescale).expect("frozen positive timescale"),
        raster,
        roi: planes.layout.roi,
    };
    let authored_census = observe_census(
        context,
        &source,
        CensusRequest {
            layout: candidate_layout,
            first: 0,
            last: u64::from(FRAMES - 1),
            first_pts: 0,
            end_pts: i64::from(FRAMES) * i64::from(grid.frame_ticks),
        },
        scratch,
    )
    .await
    .map_err(execution)?;
    if !authored_census
        .iter()
        .copied()
        .eq(packets.packets.iter().map(|row| row.evidence))
    {
        return Err(PrepareError::Authority(
            "independent authored packet census",
        ));
    }
    let (trace_configuration, trace_closure) = trace_clip(
        context,
        &ffmpeg,
        &source.handle,
        &authored_path,
        true,
        scratch,
    )
    .await?;
    if trace_configuration != candidate.trace_configuration || trace_closure != closure {
        return Err(PrepareError::Authority(
            "authored active codec configuration",
        ));
    }

    let crop_first_source_ordinal = planes.frames[0].source.ordinal;
    let bindings = planes
        .frames
        .iter()
        .zip(&authored_census)
        .map(|(original, candidate)| AuthoringFrameBinding {
            source: original.source,
            candidate: *candidate,
            crop_first_source_ordinal,
            crop_frames: FRAMES,
        })
        .collect();
    let mut receipt = Sha256::new();
    receipt.update(b"plurx.m7.np1.authoring-receipt.v1\0");
    for digest in [
        frozen.source_sha256,
        frozen.probe_sha256,
        frozen.protocol_sha256,
        frozen.recipe_sha256,
        package_sha,
        mux_sha,
        closure,
        gate_sha,
        candidate.receipt_sha,
        intent.identity(),
        encoded_sha,
        authored_sha256,
    ] {
        receipt.update(digest);
    }
    for attempt in attempts {
        receipt.update(attempt.target_kbps.to_le_bytes());
        receipt.update(attempt.encoded_video_packet_bytes.to_le_bytes());
    }
    receipt.update(packets.payload_bytes.to_le_bytes());
    for frame in &authored_census {
        receipt.update(frame.ordinal.to_le_bytes());
        receipt.update(frame.pts.to_le_bytes());
        receipt.update(frame.duration.to_le_bytes());
        receipt.update(frame.access_unit_sha256);
        receipt.update(frame.bl_vcl_sha256);
        receipt.update(frame.rpu_sha256);
    }
    check_cancel(cancel)?;
    if !source.unchanged()
        || !title.source.unchanged()
        || !planes.composed.unchanged()
        || !ffmpeg.is_current().await
        || !context.tools.is_current().await
        || !context.tools.muxer_is_current().await
        || !context.tools.m7_encoder_is_current().await
    {
        return Err(PrepareError::Authority(
            "source or encoder changed before authored custody",
        ));
    }
    let proof = AuthoredProof {
        intent,
        encoded_mp4_sha: encoded_sha,
        authored_mp4_sha: authored_sha256,
        extent: authored_extent,
        authored_payload_bytes: packets.payload_bytes,
        packet_census: packet_identity(&packets),
        receipt_sha: receipt.finalize().into(),
    };
    match recovered {
        Some(saved) if saved == proof => {}
        Some(_) => return Err(PrepareError::IncompleteAttempt { slot: intent.slot }),
        None => {
            check_publish(context, title, cancel).await?;
            blocking({
                let journal = Arc::clone(&journal);
                let allowed = active(context, cancel);
                move || {
                    let bytes = read_owned(&authored_path, MAX_ENCODED_BYTES)?;
                    journal
                        .seal_authored(&proof, &bytes, allowed)
                        .map_err(journal_error)
                }
            })
            .await?;
        }
    }
    if !recipe.authored_rate_in_band(packets.payload_bytes) {
        return Err(PrepareError::ActualRate);
    }
    // Keep the named private clip under the caller's scratch until comparison
    // and its child reaps finish. Removing the link changes SourceFence.
    let _ = authored_dir.keep();
    Ok(AuthoredClip {
        source,
        layout: candidate_layout,
        frames: authored_census,
        bindings,
        encoded_vcl_sha256,
        protocol_sha256: frozen.protocol_sha256,
        recipe_sha256: frozen.recipe_sha256,
        encoder_sha256: mux_sha,
        encoded_sha256: encoded_sha,
        authored_sha256,
        authoring_receipt_sha256: proof.receipt_sha,
    })
}

struct EncoderRun<'a, 'b> {
    context: &'a TitleContext<'b>,
    ffmpeg: &'a EncodedExecutable,
    muxer: &'a Path,
    recipe: &'a FrozenRecipe,
    frozen: &'a FrozenProtocol,
    planes: &'a PreparedPlanes,
    package_sha: Hash,
    scratch: &'a TempDir,
    cancel: &'a CancellationToken,
}

impl EncoderRun<'_, '_> {
    async fn verify_controls(&self, binding: Binding) -> Result<GateProof> {
        let baseline = baseline(
            self.context,
            self.ffmpeg,
            self.recipe,
            self.planes,
            self.scratch,
        )
        .await?;
        let target = self.recipe.rate.initial_target_kbps;
        let none = self.run(target, RoiControl::None, [0; 32]).await?;
        verify_control(&baseline, &none)?;
        let none_receipt = none.receipt_sha;
        drop(none);
        let zero = self.run(target, RoiControl::Zero, [0; 32]).await?;
        verify_control(&baseline, &zero)?;
        Ok(GateProof {
            binding: binding.identity(),
            closure: baseline.closure,
            baseline_receipt: baseline.receipt_sha,
            none_receipt,
            zero_receipt: zero.receipt_sha,
            elementary: packet_identity(&baseline.packets),
            trace_configuration: baseline.trace_configuration,
        })
    }

    async fn run(
        &self,
        target: u32,
        control: RoiControl,
        gate_sha: Hash,
    ) -> Result<CompletedEncode> {
        check_cancel(self.cancel)?;
        let grid = self.recipe.encoder_grid();
        let dir = tempfile::tempdir_in(self.scratch.path()).map_err(execution)?;
        let plan = Plan::make(self, target, control, gate_sha)?;
        let plan_path = dir.path().join("roi.plan");
        let plan_file = owned_file(plan_path.clone(), plan.bytes.clone()).await?;
        let mut command = tokio::process::Command::new(self.muxer);
        command.arg("--m7-encode-np1-v1");
        let output = run_child(
            self.context.tools,
            &mut command,
            &dir,
            &[
                (&self.planes.composed.handle, self.planes.composed_path(), 6),
                (&plan_file.handle, &plan_path, 7),
            ],
            ArgumentProfile::Default,
            ENCODE_LIMIT,
            MAX_ENCODE_LOG,
        )
        .await?;
        let closure = loaded_closure(self.context.tools, &output.stderr).await?;
        if control == RoiControl::Candidate {
            validate_x265_log(&output.stderr)?;
        }
        let mp4 = dir.path().join("encoded.mp4");
        let trace_path = mp4.clone();
        let receipt = dir.path().join("encode.receipt");
        let raster = self.planes.layout.raster;
        let opts = self
            .recipe
            .encoder_options(target, control)
            .map_err(execution)?;
        let (bytes, packets, receipt_sha, encoded_file) = blocking(move || {
            let bytes = read_owned(&mp4, MAX_ENCODED_BYTES)?;
            let packets = packet_book(&bytes, grid, raster, false, None)?;
            let receipt = read_owned(&receipt, RECEIPT_BYTES as u64)?;
            plan.validate_receipt(&receipt, &bytes, &packets, &opts, target)?;
            let held = open_read_nofollow_blocking(&mp4).map_err(execution)?;
            Ok((bytes, packets, hash(&receipt), held))
        })
        .await?;
        let (trace_configuration, trace_closure) = trace_clip(
            self.context,
            self.ffmpeg,
            &encoded_file,
            &trace_path,
            control == RoiControl::Candidate,
            self.scratch,
        )
        .await?;
        if closure != trace_closure {
            return Err(PrepareError::Authority(
                "trace uses a different encoder closure",
            ));
        }
        if !plan_file.unchanged() || !self.planes.composed.unchanged() {
            return Err(PrepareError::Authority("held encoder inputs changed"));
        }
        let mut bound = Sha256::new();
        bound.update(b"plurx.m7.np1.completed-encode.v1\0");
        bound.update(receipt_sha);
        bound.update(hash(&output.stderr));
        bound.update(trace_configuration.as_bytes());
        bound.update(closure);
        Ok(CompletedEncode {
            bytes,
            packets,
            closure,
            receipt_sha: bound.finalize().into(),
            trace_configuration,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlotRoute {
    Reuse,
    Launch,
}

fn slot_route(held: &[M7CandidateIntent], proposal: &M7CandidateIntent) -> Result<SlotRoute> {
    let slot = usize::from(proposal.slot);
    if proposal.slot >= proposal.maximum_attempts || slot > held.len() {
        return Err(PrepareError::IncompleteAttempt {
            slot: proposal.slot,
        });
    }
    match held.get(slot) {
        Some(expected) if expected == proposal => Ok(SlotRoute::Reuse),
        Some(_) => Err(PrepareError::IncompleteAttempt {
            slot: proposal.slot,
        }),
        None => Ok(SlotRoute::Launch),
    }
}

fn verify_control(baseline: &CompletedEncode, control: &CompletedEncode) -> Result<()> {
    if control.closure != baseline.closure
        || control.packets != baseline.packets
        || control.trace_configuration != baseline.trace_configuration
    {
        return Err(PrepareError::Authority(
            "independent same-input elementary stream control",
        ));
    }
    Ok(())
}

fn packet_identity(packets: &PacketBook) -> Hash {
    let mut digest = Sha256::new();
    digest.update(b"plurx.m7.np1.packet-census.v1\0");
    digest.update(packets.payload_bytes.to_le_bytes());
    for packet in &packets.packets {
        let frame = packet.evidence;
        digest.update(frame.ordinal.to_le_bytes());
        digest.update(frame.pts.to_le_bytes());
        digest.update(frame.duration.to_le_bytes());
        digest.update(frame.time_base.numerator().to_le_bytes());
        digest.update(frame.time_base.denominator().to_le_bytes());
        digest.update(frame.access_unit_sha256);
        digest.update(frame.rpu_sha256);
        digest.update(frame.bl_vcl_sha256);
        digest.update(packet.size.to_le_bytes());
        digest.update([packet.sync as u8]);
    }
    digest.finalize().into()
}

fn restore_encoded(
    journal: &Journal,
    intent: Intent,
    grid: EncoderGrid,
    raster: [u16; 2],
) -> Result<CompletedEncode> {
    let recovered = journal.recover(intent).map_err(journal_error)?;
    let packets = packet_book(&recovered.bytes, grid, raster, false, None)
        .map_err(|_| PrepareError::IncompleteAttempt { slot: intent.slot })?;
    if recovered.proof.packet_census != packet_identity(&packets)
        || recovered.proof.encoded_payload_bytes != packets.payload_bytes
    {
        return Err(PrepareError::IncompleteAttempt { slot: intent.slot });
    }
    Ok(CompletedEncode {
        bytes: recovered.bytes,
        packets,
        closure: intent.closure,
        receipt_sha: recovered.proof.receipt_sha,
        trace_configuration: recovered.proof.trace_configuration,
    })
}

fn seal_encoded(
    journal: &Journal,
    intent: Intent,
    encoded: CompletedEncode,
    allowed: impl Fn() -> bool,
) -> Result<CompletedEncode> {
    let proof = EncodeProof {
        intent,
        extent: encoded.bytes.len() as u64,
        mp4_sha: hash(&encoded.bytes),
        encoded_payload_bytes: encoded.packets.payload_bytes,
        receipt_sha: encoded.receipt_sha,
        packet_census: packet_identity(&encoded.packets),
        trace_configuration: encoded.trace_configuration.clone(),
    };
    journal
        .seal_encoded(&proof, &encoded.bytes, allowed)
        .map_err(journal_error)?;
    Ok(encoded)
}

fn active(
    context: &TitleContext<'_>,
    cancel: &CancellationToken,
) -> impl Fn() -> bool + Send + 'static {
    let cancel = cancel.clone();
    let lost = context.fence.loss_token();
    let shutdown = context.shutdown.clone();
    move || !cancel.is_cancelled() && !lost.is_cancelled() && !shutdown.is_cancelled()
}

async fn check_publish(
    context: &TitleContext<'_>,
    title: &HeldTitle,
    cancel: &CancellationToken,
) -> Result<()> {
    check_cancel(cancel)?;
    if !active(context, cancel)()
        || !context
            .fence
            .source_may_continue()
            .await
            .map_err(execution)?
    {
        return Err(PrepareError::Cancelled);
    }
    if !title.source.unchanged() {
        return Err(PrepareError::Authority(
            "held source changed during candidate custody",
        ));
    }
    Ok(())
}

fn journal_error(error: journal::Error) -> PrepareError {
    match error {
        journal::Error::Consumed(slot) => PrepareError::IncompleteAttempt { slot },
        journal::Error::Binding(reason) => PrepareError::Authority(reason),
        journal::Error::Io(error) if error.kind() == std::io::ErrorKind::Interrupted => {
            PrepareError::Cancelled
        }
        error => execution(error),
    }
}

pub(in crate::source_probe) async fn reclaim_candidate_media(
    trial_directory: &Path,
    source: Hash,
    protocol: Hash,
) -> Result<()> {
    let path = trial_directory.to_owned();
    blocking(move || {
        let journal = Journal::open(&path, source, protocol).map_err(journal_error)?;
        journal.reclaim(source, protocol).map_err(journal_error)
    })
    .await
}

fn package_identity(planes: &PreparedPlanes) -> Result<Hash> {
    let mut digest = Sha256::new();
    digest.update(b"plurx.m7.np1.full-plane-input.v1\0");
    digest.update(planes.source_sha256);
    digest.update(planes.probe_sha256);
    digest.update(hash(planes.source_object_version.as_bytes()));
    for value in planes.layout.raster {
        digest.update(value.to_le_bytes());
    }
    for row in &planes.frames {
        let source = row.source;
        let map = &row.benefit_map;
        if hash(&row.source_rpu) != source.rpu_sha256
            || map.source_ordinal != source.ordinal
            || map.source_pts != source.pts
            || map.source_duration != source.duration
            || map.source_time_base != source.time_base
            || map.source_rpu_sha256 != source.rpu_sha256
            || map.source_access_unit_sha256 != source.access_unit_sha256
            || map.source_bl_vcl_sha256 != source.bl_vcl_sha256
            || map.original_bl_sha256 != row.original_bl_sha256
            || map.composed_sha256 != row.composed_sha256
        {
            return Err(PrepareError::Authority(
                "full-plane map is not attached to its source frame",
            ));
        }
        digest.update(source.ordinal.to_le_bytes());
        digest.update(source.pts.to_le_bytes());
        digest.update(source.duration.to_le_bytes());
        digest.update(source.time_base.numerator().to_le_bytes());
        digest.update(source.time_base.denominator().to_le_bytes());
        digest.update(source.access_unit_sha256);
        digest.update(source.rpu_sha256);
        digest.update(source.bl_vcl_sha256);
        digest.update(row.original_bl_sha256);
        digest.update(row.composed_sha256);
        digest.update(hash(&serde_json::to_vec(map).map_err(execution)?));
    }
    Ok(digest.finalize().into())
}

struct Plan {
    bytes: Vec<u8>,
    rows: Vec<(usize, Hash)>,
    roi_total: u64,
}

impl Plan {
    fn make(
        run: &EncoderRun<'_, '_>,
        target: u32,
        control: RoiControl,
        gate_sha: Hash,
    ) -> Result<Self> {
        let recipe = run.recipe;
        let grid = recipe.encoder_grid();
        let [width, height] = run.planes.layout.raster;
        let hdr = recipe.hdr;
        let mut bytes = b"PLXM7R01".to_vec();
        let mode = match control {
            RoiControl::None => 0,
            RoiControl::Zero => 1,
            RoiControl::Candidate => 2,
        };
        for value in [
            mode,
            u32::from(width),
            u32::from(height),
            grid.frames,
            grid.timescale,
            grid.frame_ticks,
            target,
            recipe.rate.initial_target_kbps,
            recipe.rate.authored_band_kbps[0],
            recipe.rate.authored_band_kbps[1],
            hdr.max_cll_fall.is_some() as u32,
            0,
        ] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        for xy in hdr.primaries_rgbw {
            bytes.extend_from_slice(&xy.to_le_bytes());
        }
        bytes.extend_from_slice(&hdr.max_1e4_nits.to_le_bytes());
        bytes.extend_from_slice(&hdr.min_1e4_nits.to_le_bytes());
        for light in hdr.max_cll_fall.unwrap_or_default() {
            bytes.extend_from_slice(&light.to_le_bytes());
        }
        for digest in [
            recipe.recipe_sha256(),
            run.frozen.protocol_sha256,
            run.package_sha,
            gate_sha,
        ] {
            bytes.extend_from_slice(&digest);
        }

        let mut rows = Vec::with_capacity(FRAMES as usize);
        let mut roi_total = 0;
        let zero = Region {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
            qoffset_num: 0,
            qoffset_den: 1,
        };
        let max_regions = usize::from(width).div_ceil(16) * usize::from(height).div_ceil(16) + 1;
        for (index, frame) in run.planes.frames.iter().enumerate() {
            let start = bytes.len();
            let evidence = frame.source;
            let roi = match control {
                RoiControl::None => &[],
                RoiControl::Zero => std::slice::from_ref(&zero),
                RoiControl::Candidate => frame.benefit_map.regions.as_slice(),
            };
            if roi.len() > max_regions {
                return Err(PrepareError::Authority("bounded ordered ROI plan"));
            }
            bytes.extend_from_slice(&evidence.ordinal.to_le_bytes());
            bytes.extend_from_slice(&evidence.pts.to_le_bytes());
            bytes.extend_from_slice(&evidence.duration.to_le_bytes());
            bytes.extend_from_slice(&evidence.time_base.numerator().to_le_bytes());
            bytes.extend_from_slice(&evidence.time_base.denominator().to_le_bytes());
            bytes.extend_from_slice(&(index as i64 * i64::from(grid.frame_ticks)).to_le_bytes());
            bytes.extend_from_slice(&(roi.len() as u32).to_le_bytes());
            for digest in [
                evidence.access_unit_sha256,
                evidence.rpu_sha256,
                evidence.bl_vcl_sha256,
                frame.original_bl_sha256,
                frame.composed_sha256,
            ] {
                bytes.extend_from_slice(&digest);
            }
            let roi_start = bytes.len();
            for region in roi {
                for boundary in [region.left, region.top, region.right, region.bottom] {
                    bytes.extend_from_slice(&boundary.to_le_bytes());
                }
                bytes.extend_from_slice(&region.qoffset_num.to_le_bytes());
                bytes.extend_from_slice(&region.qoffset_den.to_le_bytes());
            }
            rows.push((start, hash(&bytes[roi_start..])));
            roi_total += roi.len() as u64;
        }
        bytes.extend_from_slice(b"END1");
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&u64::from(FRAMES).to_le_bytes());
        bytes.extend_from_slice(&roi_total.to_le_bytes());
        Ok(Self {
            bytes,
            rows,
            roi_total,
        })
    }

    fn validate_receipt(
        &self,
        receipt: &[u8],
        encoded: &[u8],
        book: &PacketBook,
        options: &[(&str, String)],
        target: u32,
    ) -> Result<()> {
        if receipt.len() != RECEIPT_BYTES
            || &receipt[..8] != b"PLXM7E01"
            || receipt[8..PLAN_HEADER] != self.bytes[8..PLAN_HEADER]
        {
            return Err(PrepareError::Authority("exact encoder receipt header"));
        }
        let mut reader = &receipt[PLAN_HEADER..];
        let cfg = chunk(&mut reader, b"CFG1", RECEIPT_CFG)?;
        let mut option_hash = Sha256::new();
        for (key, value) in options {
            option_hash.update(key.as_bytes());
            option_hash.update([0]);
            option_hash.update(value.as_bytes());
            option_hash.update([0]);
        }
        if [
            u32_at(cfg, 0) >> 16,
            u32_at(cfg, 4) >> 16,
            u32_at(cfg, 8) >> 16,
        ] != [63, 63, 61]
            || u32_at(cfg, 12) != 1
            || [
                u32_at(cfg, 16),
                u32_at(cfg, 20),
                u32_at(cfg, 24),
                u32_at(cfg, 28),
            ] != [1, FRAMES, 0, 136_000_000]
            || u64_at(cfg, 32) != u64::from(target) * 1000
            || u64_at(cfg, 40) != 125_000_000
            || u64_at(cfg, 48) != encoded.len() as u64
            || u64_at(cfg, 56) != book.payload_bytes
            || cfg[64..96] != hash(&self.bytes)
            || cfg[96..128] != <Hash>::from(option_hash.finalize())
            || cfg[128..160] == [0; 32]
            || cfg[160..192] == [0; 32]
            || cfg[192..224] != hash(encoded)
            || cfg[224..256] == [0; 32]
        {
            return Err(PrepareError::Authority(
                "effective encoder controls or readback bytes",
            ));
        }
        for ((offset, roi_sha), packet) in self.rows.iter().zip(&book.packets) {
            let row = chunk(&mut reader, b"AU01", RECEIPT_ROW)?;
            if row[..PLAN_ROW] != self.bytes[*offset..*offset + PLAN_ROW]
                || row[204..236] != *roi_sha
                || row[236..268] == [0; 32]
                || row[268..300] != packet.evidence.access_unit_sha256
                || row[300..332] != packet.evidence.bl_vcl_sha256
                || u64_at(row, 332) != packet.evidence.pts as u64
                || u64_at(row, 340) != packet.evidence.pts as u64
                || u64_at(row, 348) != packet.evidence.duration as u64
                || u32_at(row, 356) != packet.size
                || u32_at(row, 360) != packet.sync as u32
            {
                return Err(PrepareError::Authority(
                    "exact encoder receipt packet/readback",
                ));
            }
        }
        let tail = chunk(&mut reader, b"END1", 16)?;
        if u64_at(tail, 0) != u64::from(FRAMES)
            || u64_at(tail, 8) != self.roi_total
            || !reader.is_empty()
        {
            return Err(PrepareError::Authority("complete encoder receipt"));
        }
        Ok(())
    }
}

async fn baseline(
    context: &TitleContext<'_>,
    ffmpeg: &EncodedExecutable,
    recipe: &FrozenRecipe,
    planes: &PreparedPlanes,
    scratch: &TempDir,
) -> Result<CompletedEncode> {
    let grid = recipe.encoder_grid();
    let [width, height] = planes.layout.raster;
    let dir = tempfile::tempdir_in(scratch.path()).map_err(execution)?;
    let mut command = tokio::process::Command::new(&ffmpeg.path);
    command
        .args(["-nostdin", "-loglevel", "verbose", "-filter_threads", "1", "-threads", "1",
            "-f", "rawvideo", "-pixel_format", "yuv420p10le", "-video_size"])
        .arg(format!("{width}x{height}"))
        .arg("-framerate").arg(format!("{}/{}", grid.timescale, grid.frame_ticks))
        .args(["-fd", "6", "-i", "fd:", "-map", "0:v:0", "-an", "-sn", "-vf"])
        .arg(format!(
            "setparams=range=tv:color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc,format=yuv420p10le,settb=expr=1/{},setpts=N*{}",
            grid.timescale, grid.frame_ticks
        ))
        .args(["-c:v", "libx265"]);
    for (key, value) in recipe
        .encoder_options(recipe.rate.initial_target_kbps, RoiControl::None)
        .map_err(execution)?
    {
        let arg = match key {
            "profile" => "-profile:v".to_owned(),
            "b" | "threads" => format!("-{key}:v"),
            _ => format!("-{key}"),
        };
        command.arg(arg).arg(value);
    }
    command
        .args([
            "-force_key_frames",
            "expr:eq(mod(n,48),0)",
            "-fps_mode",
            "passthrough",
            "-enc_time_base",
        ])
        .arg(format!("1:{}", grid.timescale))
        .args([
            "-frames:v",
            "48",
            "-movflags",
            MOVFLAGS,
            "-video_track_timescale",
        ])
        .arg(grid.timescale.to_string())
        .args(["-tag:v", "hvc1", "-f", "mp4", "pipe:1"]);
    // The CLI's rawvideo fd protocol owns a fresh offset. Re-open only this
    // job-local staged file and check against the still-held identity.
    let source = open_read_nofollow_blocking(planes.composed_path()).map_err(execution)?;
    let source =
        SourceFence::clone_held(&source, planes.composed.object_version()).map_err(execution)?;
    let output = run_child(
        context.tools,
        &mut command,
        &dir,
        &[(&source.handle, planes.composed_path(), 6)],
        ArgumentProfile::Ffmpeg { drm_device: None },
        ENCODE_LIMIT,
        MAX_ENCODE_LOG,
    )
    .await?;
    let closure = loaded_closure(context.tools, &output.stderr).await?;
    let log = std::str::from_utf8(&output.stderr).map_err(execution)?;
    let unused: Vec<_> = log
        .lines()
        .filter_map(|line| {
            let (_, rest) = line.split_once("Codec AVOption ")?;
            if !rest.contains("has not been used") {
                return None;
            }
            rest.split_whitespace().next()
        })
        .collect();
    if unused != ["sc_threshold"] {
        return Err(PrepareError::Authority("independent CLI unused options"));
    }
    let log_sha = hash(&output.stderr);
    let path = dir.path().join("stdout.bin");
    let trace_path = path.clone();
    let (bytes, packets, file) = blocking(move || {
        let bytes = read_owned(&path, MAX_ENCODED_BYTES)?;
        let packets = packet_book(&bytes, grid, [width, height], false, None)?;
        let file = open_read_nofollow_blocking(&path).map_err(execution)?;
        Ok((bytes, packets, file))
    })
    .await?;
    let (trace_configuration, trace_closure) =
        trace_clip(context, ffmpeg, &file, &trace_path, false, scratch).await?;
    if closure != trace_closure || !source.unchanged() {
        return Err(PrepareError::Authority(
            "independent CLI closure or held input changed",
        ));
    }
    let mut receipt = Sha256::new();
    receipt.update(b"plurx.m7.np1.cli-baseline.v1\0");
    receipt.update(hash(&bytes));
    receipt.update(log_sha);
    receipt.update(trace_configuration.as_bytes());
    Ok(CompletedEncode {
        bytes,
        packets,
        closure,
        receipt_sha: receipt.finalize().into(),
        trace_configuration,
    })
}

async fn run_child(
    tools: &ToolSet,
    command: &mut tokio::process::Command,
    dir: &TempDir,
    inputs: &[(&File, &Path, i32)],
    profile: ArgumentProfile,
    deadline: Duration,
    max_log: usize,
) -> Result<bounded::Output> {
    let path = dir.path().join("stdout.bin");
    let stdout = File::create_new(&path).map_err(execution)?;
    let directory = open_directory_nofollow_blocking(dir.path()).map_err(execution)?;
    let _directory_owner =
        source_lifetime::register_daemon_file(&directory, dir.path()).map_err(execution)?;
    let _stdout_owner = source_lifetime::register_daemon_file(&stdout, &path).map_err(execution)?;
    let mut fds = vec![(directory.as_raw_fd(), 4)];
    let _input_owners = inputs
        .iter()
        .map(|(file, path, _)| source_lifetime::register_daemon_file(file, path).map_err(execution))
        .collect::<Result<Vec<_>>>()?;
    fds.extend(
        inputs
            .iter()
            .map(|(file, _, target)| (file.as_raw_fd(), *target)),
    );
    command.current_dir(dir.path());
    let manifest = Descriptors::from_mappings(&fds, Some(4))
        .prepare_with_profile(
            command,
            ChildStdios {
                stdin: ChildStdio::Null,
                stdout: ChildStdio::File(&stdout),
                stderr: ChildStdio::Piped,
            },
            profile,
        )
        .map_err(execution)?;
    let mut environment = tools.renderer_env.clone();
    if cfg!(target_os = "linux") {
        environment.push(("LD_DEBUG".into(), OsString::from("libs")));
    } else {
        environment.push(("DYLD_PRINT_LIBRARIES".into(), OsString::from("1")));
    }
    let result = source_lifetime::mediated(bounded::output_command_bundle_env_to_file(
        command,
        deadline,
        max_log,
        MAX_ENCODED_BYTES as usize,
        ChildWork::background("Dolby Vision NP1 encoder or proof"),
        &environment,
        stdout,
        Some(manifest),
    ))
    .await
    .map_err(|err| {
        if err.kind() == std::io::ErrorKind::Interrupted {
            PrepareError::Cancelled
        } else {
            execution(err)
        }
    })?;
    if !result.status.success() {
        let prefix = std::str::from_utf8(&result.stderr).unwrap_or_default();
        let last = prefix.lines().next_back().unwrap_or_default();
        return Err(execution(format!("NP1 child {}: {last}", result.status)));
    }
    Ok(result)
}

async fn loaded_closure(tools: &ToolSet, stderr: &[u8]) -> Result<Hash> {
    let log = std::str::from_utf8(stderr).map_err(execution)?;
    let mut paths: [Option<PathBuf>; 4] = Default::default();
    for line in log.lines() {
        let Some(path) = loaded_path(line) else {
            continue;
        };
        let Some(name) = path.file_name().and_then(|p| p.to_str()) else {
            continue;
        };
        let Some(slot) = ["libavcodec.", "libavformat.", "libavutil.", "libx265."]
            .iter()
            .position(|stem| name.starts_with(stem))
        else {
            continue;
        };
        if paths[slot].as_deref().is_some_and(|prior| prior != path) {
            return Err(PrepareError::Authority(
                "unique loaded libav/libx265 closure",
            ));
        }
        paths[slot] = Some(path.to_owned());
    }
    let mut identity = Sha256::new();
    identity.update(b"plurx.m7.np1.encoder-closure.v1\0");
    for path in paths {
        let path = path.ok_or(PrepareError::Authority(
            "observable shared libav/libx265 closure",
        ))?;
        let file = blocking(move || {
            let canonical = path.canonicalize().map_err(execution)?;
            let file = open_read_nofollow_blocking(&canonical).map_err(execution)?;
            let held = SourceFence::clone_current_held(&file).map_err(execution)?;
            if held.handle.metadata().map_err(execution)?.len() > MAX_LIB_BYTES {
                return Err(PrepareError::Authority("bounded encoder library"));
            }
            let mut digest = Sha256::new();
            let mut input = (&held.handle).take(MAX_LIB_BYTES + 1);
            let mut count = 0;
            let mut bytes = [0_u8; 65536];
            loop {
                let next = input.read(&mut bytes).map_err(execution)?;
                if next == 0 {
                    break;
                }
                count += next as u64;
                digest.update(&bytes[..next]);
            }
            if count == 0 || count > MAX_LIB_BYTES || !held.unchanged() {
                return Err(PrepareError::Authority("loaded encoder library moved"));
            }
            Ok((canonical, <Hash>::from(digest.finalize())))
        })
        .await?;
        let expected = tools
            .verified_encoder_library(&file.0)
            .ok_or(PrepareError::Authority(
                "loaded critical encoder library outside verified bundle",
            ))?;
        if parse_hash(expected)? != file.1 {
            return Err(PrepareError::Authority(
                "loaded critical encoder library differs",
            ));
        }
        identity.update(file.1);
    }
    Ok(identity.finalize().into())
}

fn loaded_path(line: &str) -> Option<&Path> {
    let path = if cfg!(target_os = "linux") {
        line.split_once("calling init:")?.1.trim()
    } else if line.starts_with("dyld[") || line.starts_with("dyld:") {
        let (_, path) = line.split_once(" /")?;
        return Some(Path::new(&line[line.len() - path.len() - 1..]));
    } else {
        return None;
    };
    Path::new(path).is_absolute().then(|| Path::new(path))
}

async fn trace_clip(
    context: &TitleContext<'_>,
    ffmpeg: &EncodedExecutable,
    clip: &File,
    path: &Path,
    enforce_qg: bool,
    scratch: &TempDir,
) -> Result<(String, Hash)> {
    let dir = tempfile::tempdir_in(scratch.path()).map_err(execution)?;
    let mut command = tokio::process::Command::new(&ffmpeg.path);
    command.args([
        "-nostdin",
        "-hide_banner",
        "-loglevel",
        "verbose",
        "-xerror",
        "-fd",
        "6",
        "-i",
        "fd:",
        "-map",
        "0:v:0",
        "-c:v",
        "copy",
        "-bsf:v",
        "trace_headers",
        "-f",
        "null",
        "pipe:1",
    ]);
    let output = run_child(
        context.tools,
        &mut command,
        &dir,
        &[(clip, path, 6)],
        ArgumentProfile::Ffmpeg { drm_device: None },
        TRACE_LIMIT,
        MAX_TRACE_LOG,
    )
    .await?;
    let closure = loaded_closure(context.tools, &output.stderr).await?;
    let mut trace = Trace::default();
    trace.feed(&output.stderr);
    if trace.source_fault().is_some() {
        return Err(PrepareError::Authority("coded HEVC trace read error"));
    }
    let exact = trace
        .finish_range(|_| {})
        .map_err(PrepareError::Execution)?;
    if exact.packets != u64::from(FRAMES) || exact.slices != u64::from(FRAMES) {
        return Err(PrepareError::Authority("complete coded HEVC trace"));
    }
    if enforce_qg {
        validate_hevc_trace(&output.stderr)?;
    }
    Ok((exact.configuration_sha256, closure))
}

fn validate_x265_log(stderr: &[u8]) -> Result<()> {
    let log = std::str::from_utf8(stderr).map_err(execution)?;
    if log.to_ascii_lowercase().contains("skipping roi") {
        return Err(PrepareError::Authority("libx265 skipped ROI"));
    }
    let aq: Vec<_> = log
        .lines()
        .filter_map(|line| {
            line.split_once("AQ: mode / str / qg-size / cu-tree")?
                .1
                .split_once(':')
        })
        .map(|(_, value)| value)
        .collect();
    let [aq] = aq.as_slice() else {
        return Err(PrepareError::Authority("one actual x265 AQ tuple"));
    };
    let settings = aq
        .split('/')
        .map(|field| field.trim().parse::<f64>())
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(execution)?;
    if settings != [2.0, 0.0, 32.0, 1.0] {
        return Err(PrepareError::Authority(
            "actual x265 AQ mode/strength/QG/cutree",
        ));
    }
    let tools: Vec<_> = log
        .lines()
        .filter_map(|line| {
            line.contains("x265")
                .then(|| line.split_once("tools:").map(|(_, tools)| tools))
                .flatten()
        })
        .collect();
    for (key, expected) in [("rdoq", 2.0), ("psy-rd", 2.0), ("psy-rdoq", 1.0)] {
        let actual: Vec<_> = tools
            .iter()
            .flat_map(|tools| tools.split_whitespace())
            .filter_map(|word| word.split_once('='))
            .filter(|(name, _)| *name == key)
            .map(|(_, value)| value.parse::<f64>())
            .collect::<std::result::Result<_, _>>()
            .map_err(execution)?;
        if actual != [expected] {
            return Err(PrepareError::Authority(
                "actual inherited x265 psy/rdoq controls",
            ));
        }
    }
    Ok(())
}

fn validate_hevc_trace(stderr: &[u8]) -> Result<()> {
    let trace = std::str::from_utf8(stderr).map_err(execution)?;
    let field = |name| -> Result<u32> {
        let mut values = trace.lines().filter_map(|line| {
            let (_, line) = line.split_once("[trace_headers @ ")?;
            let (_, line) = line.split_once("] ")?;
            let (bits, value) = line.rsplit_once("= ")?;
            let mut fields = bits.split_whitespace();
            fields.next()?.parse::<u32>().ok()?;
            (fields.next()? == name).then(|| value.trim().parse::<u32>())
        });
        let first = values
            .next()
            .ok_or(PrepareError::Authority("missing coded SPS/PPS field"))?
            .map_err(execution)?;
        if values.any(|other| !matches!(other, Ok(value) if value == first)) {
            return Err(PrepareError::Authority("conflicting coded SPS/PPS fields"));
        }
        Ok(first)
    };
    let ctu = field("log2_min_luma_coding_block_size_minus3")?
        .checked_add(3)
        .and_then(|c| c.checked_add(field("log2_diff_max_min_luma_coding_block_size").ok()?))
        .ok_or(PrepareError::Authority("coded CTU size"))?;
    if field("cu_qp_delta_enabled_flag")? != 1
        || ctu.checked_sub(field("diff_cu_qp_delta_depth")?) != Some(5)
    {
        return Err(PrepareError::Authority(
            "effective coded QP group is not 32",
        ));
    }
    for (name, expected) in [
        ("video_full_range_flag", 0),
        ("colour_primaries", 9),
        ("transfer_characteristics", 16),
        ("matrix_coefficients", 9),
        ("chroma_sample_loc_type_top_field", 2),
        ("chroma_sample_loc_type_bottom_field", 2),
    ] {
        if field(name)? != expected {
            return Err(PrepareError::Authority(
                "coded BT.2020 PQ limited TopLeft declaration",
            ));
        }
    }
    Ok(())
}

fn packet_book(
    encoded: &[u8],
    grid: EncoderGrid,
    raster: [u16; 2],
    authored: bool,
    source_rpus: Option<&[Vec<u8>]>,
) -> Result<PacketBook> {
    let mut reader = FragmentReader::new();
    reader.push(encoded);
    let Some(Unit::Init(init)) = reader.next_unit().map_err(execution)? else {
        return Err(PrepareError::Authority("clip init"));
    };
    let video = init
        .video()
        .ok_or(PrepareError::Authority("single video track"))?;
    let facts = fmp4::hevc_hdr10_sample_entry_facts(&init).map_err(execution)?;
    let entry = fmp4::copy_video_sample_entry_facts(&init).map_err(execution)?;
    let dv = fmp4::dolby_vision_record(&init).map_err(execution)?;
    if init.tracks.len() != 1
        || [facts.width, facts.height] != raster
        || video.timescale != grid.timescale
        || video.nal_length_size != 4
        || entry.sample_entry != if authored { *b"hev1" } else { *b"hvc1" }
        || if authored {
            !dv.is_some_and(|d| {
                d.profile == 8
                    && d.bl_signal_compatibility_id == 1
                    && d.rpu_present
                    && d.bl_present
                    && !d.el_present
            })
        } else {
            dv.is_some()
        }
    {
        return Err(PrepareError::Authority(
            "exact single-layer clip format and clock",
        ));
    }
    let mut packets = Vec::with_capacity(grid.frames as usize);
    let mut payload_bytes = 0;
    let time_base = Rational::new(1, grid.timescale).expect("admitted timebase");
    while let Some(unit) = reader.next_unit().map_err(execution)? {
        let fragment = match unit {
            Unit::Fragment(fragment) => fragment,
            Unit::Trailer => continue,
            _ => return Err(PrepareError::Authority("unexpected fragment unit")),
        };
        let track = fragment
            .track(video.id)
            .ok_or(PrepareError::Authority("missing video run"))?;
        let start = packets.len() as u64 * u64::from(grid.frame_ticks);
        let duration = track.sample_count() as u64 * u64::from(grid.frame_ticks);
        fmp4::validate_encoded_grid(
            &fragment,
            &init,
            grid.timescale,
            start,
            duration,
            grid.frame_ticks,
        )
        .map_err(execution)?;
        for run in &track.runs {
            let mut offset = run.data_offset;
            for sample in &run.samples {
                let ordinal = packets.len();
                let end = offset
                    .checked_add(sample.size as usize)
                    .ok_or(PrepareError::Authority("bounded coded sample"))?;
                if ordinal >= grid.frames as usize
                    || offset < fragment.mdat_payload.start
                    || end > fragment.mdat_payload.end
                    || sample.cto != 0
                    || sample.duration != grid.frame_ticks
                    || sample.is_sync() != (ordinal == 0)
                {
                    return Err(PrepareError::Authority(
                        "one coded AU at the exact encoder grid",
                    ));
                }
                let au = fragment
                    .bytes
                    .get(offset..end)
                    .ok_or(PrepareError::Authority("coded AU bytes"))?;
                offset = end;
                let (vcl, rpu) = packet_nals(au)?;
                let rpu_sha256 = match (authored, rpu) {
                    (false, None) => [0; 32],
                    (true, Some(candidate)) => {
                        let source = source_rpus
                            .and_then(|rpus| rpus.get(ordinal))
                            .ok_or(PrepareError::Authority("same-frame source RPU"))?;
                        verify_dm(source, candidate)?;
                        hash(candidate)
                    }
                    _ => return Err(PrepareError::Authority("authored-only fresh RPU")),
                };
                packets.push(Packet {
                    evidence: FrameEvidence {
                        ordinal: ordinal as u64,
                        pts: ordinal as i64 * i64::from(grid.frame_ticks),
                        duration: i64::from(grid.frame_ticks),
                        time_base,
                        access_unit_sha256: hash(au),
                        rpu_sha256,
                        bl_vcl_sha256: vcl,
                    },
                    size: sample.size,
                    sync: sample.is_sync(),
                });
                payload_bytes += u64::from(sample.size);
            }
        }
    }
    if packets.len() != grid.frames as usize || !reader.saw_trailer() || reader.buffered() != 0 {
        return Err(PrepareError::Authority(
            "complete independently parsed clip",
        ));
    }
    Ok(PacketBook {
        packets,
        payload_bytes,
    })
}

fn packet_nals(mut packet: &[u8]) -> Result<(Hash, Option<&[u8]>)> {
    let mut vcl = Sha256::new();
    let mut first_slices = 0;
    let mut rpu = None;
    while !packet.is_empty() {
        let prefix = packet
            .get(..4)
            .ok_or(PrepareError::Authority("complete NAL prefix"))?;
        let size = u32::from_be_bytes(prefix.try_into().expect("prefix size")) as usize;
        let end = size
            .checked_add(4)
            .ok_or(PrepareError::Authority("bounded NAL extent"))?;
        let framed = packet
            .get(..end)
            .ok_or(PrepareError::Authority("complete coded NAL"))?;
        let nal = &framed[4..];
        if nal.len() < 2 || nal[0] & 0x81 != 0 || nal[1] >> 3 != 0 || nal[1] & 7 == 0 {
            return Err(PrepareError::Authority("single-layer HEVC header"));
        }
        match (nal[0] >> 1) & 63 {
            0..=31 => {
                first_slices += nal.get(2).is_some_and(|byte| byte & 0x80 != 0) as usize;
                vcl.update(framed);
            }
            62 if nal.len() <= 4097 && rpu.is_none() => rpu = Some(nal),
            62 | 63 => return Err(PrepareError::Authority("only one RPU and no EL")),
            _ => {}
        }
        packet = &packet[end..];
    }
    if first_slices != 1 {
        return Err(PrepareError::Authority("one coded picture in the AU"));
    }
    Ok((vcl.finalize().into(), rpu))
}

fn verify_dm(source: &[u8], candidate: &[u8]) -> Result<()> {
    let parse = |raw: &[u8]| {
        std::panic::catch_unwind(|| DoviRpu::parse_unspec62_nalu(raw))
            .ok()
            .and_then(|v| v.ok())
            .ok_or(PrepareError::Authority("fresh source and authored RPU"))
    };
    let source = parse(source)?;
    let candidate = parse(candidate)?;
    if source.dovi_profile != 7
        || candidate.dovi_profile != 8
        || !candidate.header.disable_residual_flag
        || source.header.use_prev_vdr_rpu_flag
        || candidate.header.use_prev_vdr_rpu_flag
        || match (source.vdr_dm_data, candidate.vdr_dm_data) {
            (Some(source), Some(candidate)) => dm_color(&source) != dm_color(&candidate),
            _ => true,
        }
    {
        return Err(PrepareError::Authority("same-frame authored DM color"));
    }
    Ok(())
}

fn dm_color(dm: &VdrDmData) -> ([i16; 9], [u32; 3], [i16; 9], [u32; 9]) {
    (
        [
            dm.ycc_to_rgb_coef0,
            dm.ycc_to_rgb_coef1,
            dm.ycc_to_rgb_coef2,
            dm.ycc_to_rgb_coef3,
            dm.ycc_to_rgb_coef4,
            dm.ycc_to_rgb_coef5,
            dm.ycc_to_rgb_coef6,
            dm.ycc_to_rgb_coef7,
            dm.ycc_to_rgb_coef8,
        ],
        [
            dm.ycc_to_rgb_offset0,
            dm.ycc_to_rgb_offset1,
            dm.ycc_to_rgb_offset2,
        ],
        [
            dm.rgb_to_lms_coef0,
            dm.rgb_to_lms_coef1,
            dm.rgb_to_lms_coef2,
            dm.rgb_to_lms_coef3,
            dm.rgb_to_lms_coef4,
            dm.rgb_to_lms_coef5,
            dm.rgb_to_lms_coef6,
            dm.rgb_to_lms_coef7,
            dm.rgb_to_lms_coef8,
        ],
        [
            u32::from(dm.signal_eotf),
            u32::from(dm.signal_eotf_param0),
            u32::from(dm.signal_eotf_param1),
            dm.signal_eotf_param2,
            u32::from(dm.signal_bit_depth),
            u32::from(dm.signal_color_space),
            u32::from(dm.signal_chroma_format),
            u32::from(dm.signal_full_range_flag),
            u32::from(dm.compressed),
        ],
    )
}

async fn owned_file(path: PathBuf, bytes: Vec<u8>) -> Result<SourceFence> {
    blocking(move || {
        let mut output = File::create_new(&path).map_err(execution)?;
        output.write_all(&bytes).map_err(execution)?;
        output.flush().map_err(execution)?;
        drop(output);
        let input = open_read_nofollow_blocking(&path).map_err(execution)?;
        SourceFence::clone_current_held(&input).map_err(execution)
    })
    .await
}

fn read_owned(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let input = open_read_nofollow_blocking(path).map_err(execution)?;
    let len = input.metadata().map_err(execution)?.len();
    if !(1..=limit).contains(&len) {
        return Err(PrepareError::Authority("bounded private file extent"));
    }
    let mut bytes = Vec::with_capacity(len as usize);
    input
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(execution)?;
    if bytes.len() as u64 != len {
        return Err(PrepareError::Authority("private file changed during read"));
    }
    Ok(bytes)
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f).await.map_err(execution)?
}

fn chunk<'a>(bytes: &mut &'a [u8], tag: &[u8; 4], length: usize) -> Result<&'a [u8]> {
    if bytes.len() < 8 + length || &bytes[..4] != tag || u32_at(bytes, 4) as usize != length {
        return Err(PrepareError::Authority("bounded encode receipt chunk"));
    }
    let body = &bytes[8..8 + length];
    *bytes = &bytes[8 + length..];
    Ok(body)
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("checked receipt offset"),
    )
}

fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(
        bytes[offset..offset + 8]
            .try_into()
            .expect("checked receipt offset"),
    )
}

fn parse_hash(value: &str) -> Result<Hash> {
    let mut digest = [0; 32];
    hex::decode_to_slice(value, &mut digest).map_err(execution)?;
    Ok(digest)
}

fn check_cancel(cancel: &CancellationToken) -> Result<()> {
    if cancel.is_cancelled() || bounded::check_cancellation().is_err() {
        Err(PrepareError::Cancelled)
    } else {
        Ok(())
    }
}

fn execution(error: impl std::fmt::Display) -> PrepareError {
    PrepareError::Execution(error.to_string())
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "fixed slot intents and expected route lookups"
)]
mod durable_attempt_tests {
    use super::*;
    use std::cell::Cell;

    fn proposal(slot: u8) -> M7CandidateIntent {
        M7CandidateIntent {
            source_sha256: [1; 32],
            protocol_sha256: [2; 32],
            recipe_sha256: [3; 32],
            binding_sha256: [slot + 10; 32],
            slot,
            target_kbps: 115_000 + u32::from(slot),
            maximum_attempts: 3,
        }
    }

    #[test]
    fn reusing_occupied_slots_never_calls_the_launch_branch_or_overruns_the_cap() {
        let held = [proposal(0), proposal(1), proposal(2)];
        let launches = Cell::new(0);
        let fake_runner = || launches.set(launches.get() + 1);
        for occupied in &held {
            if slot_route(&held, occupied).unwrap() == SlotRoute::Launch {
                fake_runner();
            }
        }
        assert_eq!(launches.get(), 0);
        assert!(matches!(
            slot_route(&held, &proposal(3)),
            Err(PrepareError::IncompleteAttempt { slot: 3 })
        ));
        let mut different = proposal(1);
        different.binding_sha256[0] ^= 1;
        assert!(matches!(
            slot_route(&held, &different),
            Err(PrepareError::IncompleteAttempt { slot: 1 })
        ));
        assert_eq!(
            slot_route(&held[..2], &proposal(2)).unwrap(),
            SlotRoute::Launch
        );
    }
}
