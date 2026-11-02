//! Synthetic execution-protocol evidence with real held files and real jobs.
//! The recognizer scripts/model below are test doubles, never installed-model
//! or hardware qualification. Media decoding uses the actual configured tools.
use super::*;
use crate::{
    background_jobs::ActiveBackgroundJob,
    playback_cache::{CacheCandidate, CacheOwner},
};
use plurx_core::{
    domain::{AudioStream, ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult},
    store::{
        background_jobs::{
            CandidateQuery, ClaimJob, EnqueueJob, EnqueueOutcome, JobKind, JobPayload, JobRequest,
        },
        subtitle_translation::TranslationProfile,
        subtitle_translation_mutation::{TranslationAction, TranslationMutation},
        subtitle_translation_settings::{TranslationSettings, SETTINGS_KEY},
    },
};
use serde_json::json;
use std::{
    os::unix::fs::{symlink, PermissionsExt},
    path::PathBuf,
    sync::Arc,
};

const MODEL: &[u8] = b"synthetic owned model fixture, not a qualified speech model";
struct Fixture {
    state: AppState,
    file: MediaFile,
    revision: SourceRevision,
    profile: TranslationProfile,
    operation: String,
    prepare_id: String,
    prepare_payload: JobPayload,
    active: Option<ActiveBackgroundJob>,
    root: tempfile::TempDir,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.state.shutdown.cancel();
    }
}
impl Fixture {
    fn fence(&self) -> JobFence {
        self.active.as_ref().expect("owned active job").fence()
    }
    async fn finish(&mut self) {
        if let Some(active) = self.active.take() {
            active.finish().await;
        }
    }
}
fn absolute_tool(configured: String) -> PathBuf {
    if Path::new(&configured).is_absolute() {
        return configured.into();
    }
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|directory| directory.join(&configured))
        .find(|path| path.is_file())
        .and_then(|path| path.canonicalize().ok())
        .expect("configured actual media tool")
}
async fn begin(state: &AppState, job_id: &str, kind: JobKind) -> ActiveBackgroundJob {
    let candidate = state
        .store
        .job_candidates(CandidateQuery {
            node_id: state.node_id.clone(),
            kinds: vec![kind],
            after: None,
            now_ms: crate::state::clock_ms(),
            limit: 32,
        })
        .await
        .expect("candidates")
        .jobs
        .into_iter()
        .find(|candidate| candidate.id == job_id)
        .expect("owned candidate");
    let now = crate::state::clock_ms();
    let (job, deadline) = crate::background_jobs::claim_with_resolution(
        state.store.as_ref(),
        &candidate,
        ClaimJob {
            job_id: job_id.into(),
            expected_revision: candidate.revision,
            node_id: state.node_id.clone(),
            boot_id: state.subtitle_models.boot_id.clone(),
            claim_id: uuid::Uuid::new_v4().to_string(),
            kind,
            payload_version: 1,
            now_ms: now,
            dispatched_at_ms: now,
        },
    )
    .await
    .expect("claim")
    .expect("claim granted");
    ActiveBackgroundJob::start(
        Arc::clone(&state.store),
        state.jobs.execution_authority(),
        job.token.expect("claim token"),
        deadline,
        kind,
    )
    .expect("active claim")
}
async fn fixture(media: bool) -> Fixture {
    let root = crate::test_tempdir().expect("owned runtime fixture");
    let directory = root.path().canonicalize().expect("canonical owned root");
    let state = AppState::new(
        "test".into(),
        Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("SQLite schema")),
        crate::state::Dirs {
            artwork: directory.join("artwork"),
            transcode: directory.join("transcode"),
            cache: directory.join("cache"),
            subs: directory.join("subs"),
            runtime_cache: directory.join("runtime"),
            renditions: directory.join("renditions"),
        },
        "runtime-fixture-worker".into(),
        Default::default(),
        Default::default(),
        Arc::new(crate::logbuf::LogBuffer::new(32)),
    );
    let user = state
        .store
        .create_user("runtime-fixture-admin", "unused", true)
        .await
        .expect("admin");
    state
        .store
        .put_setting(
            SETTINGS_KEY,
            &serde_json::to_string(&TranslationSettings {
                enabled: true,
                ..Default::default()
            })
            .expect("complete versioned settings fixture"),
        )
        .await
        .expect("translation on");
    let library = state
        .store
        .create_library(&NewLibrary {
            name: "Runtime fixture".into(),
            kind: LibraryKind::Movies,
            paths: vec![directory.clone()],
            anime: false,
        })
        .await
        .expect("library");
    let item = state
        .store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "Owned source".into(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    let path = directory.join(if media { "source.mkv" } else { "source.bin" });
    let raw = if media {
        let mut encoder = tokio::process::Command::new(absolute_tool(crate::ffmpeg::ffmpeg_bin()));
        encoder
            .args([
                "-nostdin",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=black:size=16x16:rate=5:duration=1",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=600:sample_rate=16000:duration=1",
                "-c:v",
                "ffv1",
                "-c:a",
                "pcm_s16le",
                "-shortest",
            ])
            .arg(&path);
        child_output(&mut encoder, Duration::from_secs(15), 64 * 1024, false)
            .await
            .expect("one-second real MKV");
        let mut probe = tokio::process::Command::new(absolute_tool(crate::ffmpeg::ffprobe_bin()));
        probe.args(["-v", "error", "-show_entries", "stream=index,codec_type,start_time,start_pts,time_base,duration:format=start_time,duration", "-of", "json"]).arg(&path);
        let observed = child_output(&mut probe, Duration::from_secs(15), 64 * 1024, false)
            .await
            .expect("real source clock");
        String::from_utf8(observed.stdout).expect("probe JSON UTF-8")
    } else {
        std::fs::write(&path, vec![b'x'; 300_000]).expect("multi-chunk held source");
        json!({"format":{"start_time":"0.000"},"streams":[{"index":0,"codec_type":"audio","start_time":"0.000","duration":"1.000"}]}).to_string()
    };
    if media {
        let observed: serde_json::Value =
            serde_json::from_str(&raw).expect("measured source clock JSON");
        let clock = super::super::source_clock(&observed, 0, 1000).expect("first audio ordinal");
        assert_eq!(
            clock.source_index,
            Some(1),
            "audio ordinal zero selects actual stream one after video stream zero"
        );
    }
    let metadata = std::fs::metadata(&path).expect("source metadata");
    let mtime = metadata
        .modified()
        .expect("mtime")
        .duration_since(std::time::UNIX_EPOCH)
        .expect("epoch")
        .as_secs() as i64;
    // AudioStream.index is the ordinal within audio streams, not ffprobe's
    // global stream index. The measured fixture above has video 0 and audio 1.
    let probe = ProbeResult {
        duration_ms: Some(1000),
        video_codec: Some("ffv1".into()),
        audio_streams: vec![AudioStream {
            index: 0,
            codec: "pcm_s16le".into(),
            language: Some("deu".into()),
            default: true,
            ..Default::default()
        }],
        raw_json: Some(raw),
        ..Default::default()
    };
    let file_id = state
        .store
        .upsert_file(
            item,
            &path.to_string_lossy(),
            metadata.len() as i64,
            mtime,
            &probe,
        )
        .await
        .expect("source catalogue");
    let file = state
        .store
        .get_file(file_id)
        .await
        .expect("file read")
        .expect("file");
    let revision = SourceRevision {
        file_id,
        source_size: file.size,
        source_mtime: file.mtime,
    };
    let mut profile = super::super::profile::selected_profile();
    // The runtime accepts a typed recipe; production's registry check remains
    // in the manager. Bind this synthetic recipe to the actual tiny test model.
    profile.model_sha256 = hex::encode(Sha256::digest(MODEL));
    let now = crate::state::clock_ms();
    let receipt = state
        .store
        .mutate_subtitle_translation(TranslationMutation {
            user_id: user.id,
            request_id: uuid::Uuid::new_v4().to_string(),
            request_digest: "a".repeat(64),
            intent_expires_at_ms: now + 120_000,
            now_ms: now,
            action: TranslationAction::Create {
                source: revision.clone(),
                profile: profile.clone(),
                audio_index: 0,
                spoken_language: "de".into(),
                duration_ms: 1000,
                target_node_id: state.node_id.clone(),
                playback_id: uuid::Uuid::new_v4().to_string(),
                position_ms: 0,
                playback_rate: 1.0,
                paused: false,
                continuity_id: None,
            },
        })
        .await
        .expect("admitted synthetic recipe");
    assert_eq!(receipt["outcome"], "accepted");
    let operation = receipt["operation_id"]
        .as_str()
        .expect("operation")
        .to_owned();
    let prepare_id = receipt["job_id"].as_str().expect("prepare job").to_owned();
    let active = begin(&state, &prepare_id, JobKind::SubtitleTranslationPrepare).await;
    let prepare_payload = state
        .store
        .background_job(&prepare_id)
        .await
        .expect("prepare")
        .expect("present")
        .supported_payload()
        .expect("typed prepare");
    Fixture {
        state,
        file,
        revision,
        profile,
        operation,
        prepare_id,
        prepare_payload,
        active: Some(active),
        root,
    }
}
async fn verified(f: &Fixture) -> VerifiedSource {
    let admission = f
        .state
        .transcode
        .admit_speech()
        .await
        .expect("idle speech admission");
    verify_source(
        &f.state,
        &f.file,
        &f.revision,
        &f.fence(),
        &admission,
        &CancellationToken::new(),
    )
    .await
    .expect("complete held source verification")
}
async fn range(f: &mut Fixture, source: &VerifiedSource) -> RangeRecipe {
    assert!(f
        .fence()
        .bind_translation_source(
            f.prepare_payload.clone(),
            source.sha256().into(),
            crate::subtitle_models::artifacts::identity_key(source.identity())
        )
        .await
        .expect("authoritative source publication"));
    f.finish().await;
    let mut recipe = RangeRecipe {
        asset_id: String::new(),
        operation_id: f.operation.clone(),
        operation_epoch: 1,
        source: f.revision.clone(),
        source_sha256: source.sha256().into(),
        profile: f.profile.clone(),
        audio_index: 0,
        spoken_language: "de".into(),
        duration_ms: 1000,
        core_index: 0,
    };
    recipe.asset_id = recipe.expected_asset_id().expect("asset identity");
    let id = uuid::Uuid::new_v4().to_string();
    let now = crate::state::clock_ms();
    let outcome = f
        .state
        .store
        .enqueue_job(EnqueueJob {
            id: id.clone(),
            payload: JobPayload::SubtitleTranslationRange {
                file_id: f.file.id,
                recipe: recipe.clone(),
                producer_preparation_job_id: f.prepare_id.clone(),
                target_node_id: f.state.node_id.clone(),
            },
            dedupe_key: format!("runtime-range:{id}"),
            priority: 1,
            not_before_ms: now,
            now_ms: now,
            request: JobRequest {
                scope: "runtime-protocol-test".into(),
                request_id: uuid::Uuid::new_v4().to_string(),
                request_digest: "d".repeat(64),
                consumer_kind: "subtitle_translation".into(),
                consumer_ref: f.operation.clone(),
                target_node_id: Some(f.state.node_id.clone()),
                deadline_ms: None,
                retain_identity: false,
            },
        })
        .await
        .expect("range queue");
    assert!(matches!(outcome, EnqueueOutcome::Accepted { .. }));
    f.active = Some(begin(&f.state, &id, JobKind::SubtitleTranslationRange).await);
    recipe
}
fn synthetic_engine(f: &Fixture, script: &str) -> (Engine, ManagedModel) {
    let directory = f.root.path().canonicalize().expect("canonical root");
    let path = directory.join("recognizer");
    std::fs::write(&path, script).expect("owned recognizer script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
        .expect("executable fixture");
    let handle = std::fs::File::open(&path).expect("held executable");
    let identity = std_file_identity(&handle).expect("executable identity");
    let fixture_path = directory.join("self-test.wav");
    let fixture_bytes = include_bytes!("../subtitle_models/self-test.wav");
    std::fs::write(&fixture_path, fixture_bytes).expect("owned companion");
    let self_test_handle = std::fs::File::open(fixture_path).expect("held companion");
    let self_test_identity = std_file_identity(&self_test_handle).expect("companion identity");
    let platform = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "macos-arm64",
        ("macos", "x86_64") => "macos-x86_64",
        ("linux", "aarch64") => "linux-aarch64",
        ("linux", "x86_64") => "linux-x86_64",
        _ => panic!("synthetic engine fixture requires a supported execution host"),
    };
    let engine = Engine {
        handle,
        path,
        binary_sha256: hex::encode(Sha256::digest(script.as_bytes())),
        adapter_contract: f.profile.adapter_contract.clone(),
        platform: platform.into(),
        identity,
        self_test_handle,
        self_test_identity,
        self_test_sha256: hex::encode(Sha256::digest(fixture_bytes)),
    };
    let path = directory.join("model.bin");
    std::fs::write(&path, MODEL).expect("owned model bytes");
    let handle = std::fs::File::open(&path).expect("held model");
    let model = ManagedModel {
        identity: std_file_identity(&handle).expect("model identity"),
        handle,
        path,
        sha256: hex::encode(Sha256::digest(MODEL)),
    };
    (engine, model)
}
fn recognizer(vtt: &str) -> String {
    format!("#!/bin/sh\nset -eu\nout=\nwhile [ $# -gt 0 ]; do\n if [ \"$1\" = --output-file ]; then out=$2; shift 2; else shift; fi\ndone\nprintf '%s' '{vtt}' > \"$out.vtt\"\n")
}
fn decoded(signal: bool) -> DecodedPcm {
    let mut file = tempfile::tempfile().expect("owned PCM");
    let bytes = 32_000_u32;
    use std::io::Write;
    file.write_all(b"RIFF").expect("RIFF");
    file.write_all(&(36 + bytes).to_le_bytes()).expect("length");
    file.write_all(b"WAVEfmt \x10\0\0\0\x01\0\x01\0\x80\x3e\0\0\0\x7d\0\0\x02\0\x10\0data")
        .expect("PCM header");
    file.write_all(&bytes.to_le_bytes()).expect("data length");
    for _ in 0..16_000 {
        file.write_all(&(if signal { 1_000_i16 } else { 0_i16 }).to_le_bytes())
            .expect("PCM sample");
    }
    DecodedPcm {
        identity: std_file_identity(&file).expect("PCM identity"),
        file,
        clock: ClockObservation {
            raw_first_sample: 0,
            anchor_sample: 0,
            samples: 16_000,
        },
        has_signal: signal,
    }
}
fn window() -> Window {
    Window {
        purpose: Purpose::Range,
        core_begin_ms: 0,
        core_end_ms: 1000,
        begin_ms: 0,
        end_ms: 1000,
        audio_index: 0,
        audio_start_samples: 0,
    }
}
fn reason<T>(result: Result<T, StoreError>, expected: &str) {
    match result {
        Err(StoreError::Task(reason)) => assert_eq!(reason, expected),
        Err(error) => panic!("unexpected error: {error}"),
        Ok(_) => panic!("operation unexpectedly succeeded"),
    }
}
fn assert_no_attempts(f: &Fixture) {
    let root = f.state.runtime_cache_dir.join("subtitle-translation");
    if root.exists() {
        assert!(
            std::fs::read_dir(root)
                .expect("attempt directory")
                .all(|entry| !entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .starts_with("attempt-")),
            "attempt files must retire before return"
        );
    }
}

#[tokio::test]
async fn attempt_cache_only_discovers_owned_directories_and_evicts_inside_its_root() {
    let root = crate::test_tempdir().expect("cache fixture");
    let root = root.path().canonicalize().expect("canonical cache");
    let owner = TranslationAttempts {
        root: root.join("attempts"),
    };
    assert!(owner.candidates().await.expect("missing cache").is_empty());
    std::fs::create_dir_all(owner.root.join("attempt-one")).expect("owned attempt");
    std::fs::write(owner.root.join("attempt-one/pcm.wav"), b"private attempt")
        .expect("attempt bytes");
    std::fs::create_dir_all(owner.root.join("ordinary")).expect("unrelated directory");
    std::fs::write(owner.root.join("attempt-file"), b"not a directory").expect("ordinary file");
    let outside = root.join("outside");
    std::fs::create_dir_all(&outside).expect("outside");
    std::fs::write(outside.join("keep"), b"unrelated bytes").expect("outside bytes");
    symlink(&outside, owner.root.join("attempt-link")).expect("symlink fixture");
    let candidates = owner.candidates().await.expect("candidates");
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].path, owner.root.join("attempt-one"));
    for path in [
        outside.clone(),
        owner.root.join("ordinary"),
        owner.root.join("attempt-missing"),
    ] {
        assert!(!owner
            .evict(&CacheCandidate {
                path,
                last_used: std::time::UNIX_EPOCH
            })
            .await
            .expect("refuse non-owned candidate"));
    }
    assert!(owner
        .evict(&candidates[0])
        .await
        .expect("evict owned attempt"));
    assert!(!candidates[0].path.exists());
    let _ = owner
        .evict(&CacheCandidate {
            path: owner.root.join("attempt-link"),
            last_used: std::time::UNIX_EPOCH,
        })
        .await;
    assert_eq!(
        std::fs::read(outside.join("keep")).expect("outside preserved"),
        b"unrelated bytes"
    );
    assert!(owner.root.join("ordinary").is_dir());
}

#[tokio::test]
async fn full_source_verification_hashes_every_chunk_and_rechecks_catalogue_and_named_identity() {
    let mut f = fixture(false).await;
    let source = verified(&f).await;
    assert_eq!(
        source.sha256(),
        hex::encode(Sha256::digest(
            std::fs::read(&f.file.path).expect("source bytes")
        ))
    );
    assert_eq!(source.revision(), &f.revision);
    assert!(source.unchanged());
    source
        .recheck(&f.state, &f.file, &CancellationToken::new())
        .await
        .expect("unchanged source");
    f.state
        .store
        .upsert_file(
            f.file.item_id,
            &f.file.path.to_string_lossy(),
            f.file.size + 1,
            f.file.mtime + 1,
            &ProbeResult::default(),
        )
        .await
        .expect("scanner revision changed");
    reason(
        source
            .recheck(&f.state, &f.file, &CancellationToken::new())
            .await,
        "source_changed",
    );
    f.finish().await;
    let mut f = fixture(false).await;
    let source = verified(&f).await;
    let previous = f.file.path.with_extension("old");
    std::fs::rename(&f.file.path, &previous).expect("hold previous inode");
    std::fs::write(&f.file.path, vec![b'x'; f.file.size as usize])
        .expect("same bytes at a different inode");
    reason(
        source
            .recheck(&f.state, &f.file, &CancellationToken::new())
            .await,
        "source_changed",
    );
    f.finish().await;
}

#[tokio::test]
async fn verification_rejects_cancelled_or_mismatched_revisions_without_a_source_binding() {
    let mut f = fixture(false).await;
    let admission = f
        .state
        .transcode
        .admit_speech()
        .await
        .expect("idle admission");
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    reason(
        verify_source(
            &f.state,
            &f.file,
            &f.revision,
            &f.fence(),
            &admission,
            &cancelled,
        )
        .await,
        "worker_interrupted",
    );
    let mut changed = f.revision.clone();
    changed.source_size += 1;
    reason(
        verify_source(
            &f.state,
            &f.file,
            &changed,
            &f.fence(),
            &admission,
            &CancellationToken::new(),
        )
        .await,
        "source_changed",
    );
    let mut moved = f.file.clone();
    moved.path = f.file.path.with_extension("not-catalogued");
    reason(
        check_revision(&f.state, &moved, &f.revision, &CancellationToken::new()).await,
        "source_changed",
    );
    assert!(f
        .state
        .subtitle_models
        .translation
        .cached(&f.file)
        .await
        .is_none());
    drop(admission);
    f.finish().await;
}

#[tokio::test]
async fn infer_window_preserves_literals_skips_silence_and_refuses_bad_output_or_changed_pcm() {
    let mut f = fixture(false).await;
    let vtt = "WEBVTT\n\n00:00:00.000 --> 00:00:00.500\nA & B <literal>\n\n";
    let (engine, model) = synthetic_engine(&f, &recognizer(vtt));
    let scratch = crate::subtitle_transcription::private_attempt(
        &f.root.path().canonicalize().expect("root"),
    )
    .expect("private scratch");
    let cues = infer_window(
        &decoded(true),
        window(),
        &"a".repeat(64),
        0,
        "de",
        SpeechTask::TranslateToEnglish,
        &engine,
        &model,
        scratch.path(),
        &f.fence(),
        &CancellationToken::new(),
    )
    .await
    .expect("synthetic recognizer output");
    assert_eq!(cues.len(), 1);
    assert_eq!(cues[0].text, "A & B <literal>");
    assert_eq!((cues[0].start_ms, cues[0].end_ms), (0, 500));
    let silent = decoded(false);
    assert!(infer_window(
        &silent,
        window(),
        &"a".repeat(64),
        0,
        "de",
        SpeechTask::Transcribe,
        &engine,
        &model,
        scratch.path(),
        &f.fence(),
        &CancellationToken::new()
    )
    .await
    .expect("silent core")
    .is_empty());
    silent
        .file
        .set_len(44)
        .expect("PCM truncated after validation");
    reason(
        infer_window(
            &silent,
            window(),
            &"a".repeat(64),
            0,
            "de",
            SpeechTask::Transcribe,
            &engine,
            &model,
            scratch.path(),
            &f.fence(),
            &CancellationToken::new(),
        )
        .await,
        "output_invalid",
    );
    drop(scratch);
    let (bad_engine, bad_model) = synthetic_engine(&f, &recognizer("not a WebVTT document\n"));
    let scratch = crate::subtitle_transcription::private_attempt(
        &f.root.path().canonicalize().expect("root"),
    )
    .expect("private scratch");
    reason(
        infer_window(
            &decoded(true),
            window(),
            &"a".repeat(64),
            0,
            "de",
            SpeechTask::TranslateToEnglish,
            &bad_engine,
            &bad_model,
            scratch.path(),
            &f.fence(),
            &CancellationToken::new(),
        )
        .await,
        "output_invalid",
    );
    f.finish().await;
}

#[tokio::test]
async fn translate_range_uses_real_source_clock_and_decoder_then_removes_private_attempts() {
    let mut f = fixture(true).await;
    let source = verified(&f).await;
    let recipe = range(&mut f, &source).await;
    let (engine, model) = synthetic_engine(
        &f,
        &recognizer("WEBVTT\n\n00:00:00.000 --> 00:00:00.500\nSynthetic recognized speech\n\n"),
    );
    let admission = f
        .state
        .transcode
        .admit_speech()
        .await
        .expect("speech admission");
    let draft = translate_range(
        &f.state,
        &f.file,
        &source,
        &recipe,
        &engine,
        &model,
        &f.fence(),
        &admission,
        &CancellationToken::new(),
    )
    .await
    .expect("synthetic protocol over actual media");
    draft
        .validate(&recipe)
        .expect("typed cue publication contract");
    assert_eq!(draft.cues[0].text, "Synthetic recognized speech");
    assert_eq!((draft.start_ms, draft.end_ms), (0, 1000));
    assert_eq!(draft.provenance.source_sha256, source.sha256());
    assert_no_attempts(&f);
    drop(admission);
    f.finish().await;
}

#[tokio::test]
async fn translate_range_errors_do_not_leave_scratch_or_accept_replaced_models() {
    let mut f = fixture(true).await;
    let source = verified(&f).await;
    let recipe = range(&mut f, &source).await;
    let (engine, model) = synthetic_engine(&f, &recognizer("not a subtitle\n"));
    let admission = f
        .state
        .transcode
        .admit_speech()
        .await
        .expect("speech admission");
    reason(
        translate_range(
            &f.state,
            &f.file,
            &source,
            &recipe,
            &engine,
            &model,
            &f.fence(),
            &admission,
            &CancellationToken::new(),
        )
        .await,
        "output_invalid",
    );
    assert_no_attempts(&f);
    let mut other_source = recipe.clone();
    other_source.source_sha256 = "f".repeat(64);
    other_source.asset_id = other_source
        .expected_asset_id()
        .expect("self-consistent changed-source recipe");
    reason(
        translate_range(
            &f.state,
            &f.file,
            &source,
            &other_source,
            &engine,
            &model,
            &f.fence(),
            &admission,
            &CancellationToken::new(),
        )
        .await,
        "source_changed",
    );
    std::fs::write(&model.path, b"replacement").expect("model replaced after opening");
    reason(
        translate_range(
            &f.state,
            &f.file,
            &source,
            &recipe,
            &engine,
            &model,
            &f.fence(),
            &admission,
            &CancellationToken::new(),
        )
        .await,
        "artifact_digest_mismatch",
    );
    assert_no_attempts(&f);
    drop(admission);
    f.finish().await;
}

#[tokio::test]
async fn cancelling_translation_awaits_the_owned_recognizer_before_removing_attempt_files() {
    let mut f = fixture(true).await;
    let source = verified(&f).await;
    let recipe = range(&mut f, &source).await;
    let script = "#!/bin/sh\nset -eu\nout=\nwhile [ $# -gt 0 ]; do\n if [ \"$1\" = --output-file ]; then out=$2; shift 2; else shift; fi\ndone\nprintf '%s' $$ > \"$out.pid\"\ntouch \"$out.started\"\nexec /bin/sleep 60\n";
    let (engine, model) = synthetic_engine(&f, script);
    let admission = f
        .state
        .transcode
        .admit_speech()
        .await
        .expect("speech admission");
    let cancel = CancellationToken::new();
    let fence = f.fence();
    let mut operation = Box::pin(translate_range(
        &f.state, &f.file, &source, &recipe, &engine, &model, &fence, &admission, &cancel,
    ));
    let attempts = f.state.runtime_cache_dir.join("subtitle-translation");
    let started = async {
        loop {
            let pid = std::fs::read_dir(&attempts)
                .ok()
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .map(|entry| entry.path().join("caption.started"))
                .find(|path| path.exists());
            if let Some(started) = pid {
                // The helper finishes writing its PID before announcing start.
                return std::fs::read_to_string(started.with_file_name("caption.pid"))
                    .expect("owned announcement")
                    .parse::<i32>()
                    .expect("owned child PID");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    let pid = tokio::select! { result=&mut operation => panic!("translation ended before recognizer start: {}", match result { Ok(_) => "success".into(), Err(error) => error.to_string() }), pid=tokio::time::timeout(Duration::from_secs(15),started) => pid.expect("bounded recognizer startup") };
    cancel.cancel();
    reason(
        tokio::time::timeout(Duration::from_secs(5), operation)
            .await
            .expect("cancellation joined child"),
        "worker_interrupted",
    );
    // POSIX kill(pid, 0) works on both supported Unix hosts; this test does
    // not inspect /proc or assert Linux process-adoption behavior.
    // SAFETY: this positive PID was announced by the exact owned child and signal zero never changes it.
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        -1,
        "owned child must be reaped before return"
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    assert_no_attempts(&f);
    drop(admission);
    f.finish().await;
}

#[tokio::test]
async fn media_program_resolves_path_defaults_and_explicit_aliases_without_silent_substitution() {
    let cancel = CancellationToken::new();
    let ordinary = media_program("ffprobe", &cancel)
        .await
        .expect("normal PATH fallback must be launchable under a clean environment");
    assert!(ordinary.is_absolute());
    assert_eq!(ordinary, absolute_tool("ffprobe".into()));
    let root = crate::test_tempdir().expect("owned executable fixture");
    let root = root.path().canonicalize().expect("canonical root");
    let executable = root.join("explicit-probe");
    std::fs::write(&executable, b"#!/bin/sh\nexit 0\n").expect("explicit executable");
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
        .expect("executable mode");
    let alias = root.join("probe-alias");
    symlink(&executable, &alias).expect("explicit alias");
    assert_eq!(
        media_program(alias.to_str().expect("fixture alias"), &cancel)
            .await
            .expect("capture explicit alias"),
        executable
    );
    let missing = root.join("missing-explicit-probe");
    reason(
        media_program(missing.to_str().expect("fixture path"), &cancel).await,
        "engine_unavailable",
    );
    cancel.cancel();
    reason(
        media_program("ffprobe", &cancel).await,
        "worker_interrupted",
    );
}
