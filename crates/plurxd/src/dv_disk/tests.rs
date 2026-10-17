use std::pin::Pin;
use std::task::{Context, Poll};

use plurx_core::domain::{AudioStream, DolbyVisionFacts, SubtitleStream};

use super::*;

const TEST_NODE_ID: &str = "dv-disk-test-node";

struct BlockedHashReader;

impl tokio::io::AsyncRead for BlockedHashReader {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Poll::Pending
    }
}

impl tokio::io::AsyncSeek for BlockedHashReader {
    fn start_seek(self: Pin<&mut Self>, _position: std::io::SeekFrom) -> io::Result<()> {
        Ok(())
    }

    fn poll_complete(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<u64>> {
        Poll::Ready(Ok(0))
    }
}

async fn prepare_fresh_directory(paths: &ConversionPaths, file: &MediaFile) -> Result<(), String> {
    super::prepare_fresh_directory(paths, file, TEST_NODE_ID, &CancellationToken::new()).await
}

async fn publication_context(
    paths: &ConversionPaths,
    file: &MediaFile,
    expected_bytes: i64,
) -> Result<PublicationContext, String> {
    super::publication_context(paths, file, TEST_NODE_ID, expected_bytes).await
}

async fn verify_existing(
    file: &MediaFile,
    el_type: Option<&'static str>,
    expected_bytes: i64,
) -> Result<VerifiedReplacement, String> {
    super::verify_existing(
        file,
        TEST_NODE_ID,
        &CancellationToken::new(),
        el_type,
        expected_bytes,
    )
    .await
}

async fn recover_published(
    file: &MediaFile,
    expected_bytes: i64,
    loss: &CancellationToken,
) -> Result<PublicationOutcome, String> {
    super::recover_published(file, TEST_NODE_ID, expected_bytes, loss).await
}

async fn cleanup_after_commit(file: &MediaFile) {
    super::cleanup_after_commit(file, TEST_NODE_ID, &CancellationToken::new()).await;
}

async fn finalize_committed_original(
    paths: &ConversionPaths,
    context: &mut PublicationContext,
) -> Result<Option<String>, String> {
    super::finalize_verified_original_inner(paths, context, &CancellationToken::new()).await
}

async fn reconcile_cleanup_quarantine(
    paths: &ConversionPaths,
    file: &MediaFile,
) -> Result<(), String> {
    super::reconcile_cleanup_quarantine(paths, file, &CancellationToken::new()).await
}

async fn remove_flat_tree_expected(
    parent: &SecureDirectory,
    name: &str,
    directory: &SecureDirectory,
    expected: plurx_core::fs_secure::FileIdentity,
) -> Result<(), String> {
    super::remove_flat_tree_expected(parent, name, directory, expected, &CancellationToken::new())
        .await
}

async fn bind_child(
    parent: &SecureDirectory,
    child: &str,
    role: &str,
) -> Result<BoundMedia, String> {
    bind_child_fresh(parent, child, &CancellationToken::new(), role).await
}

fn media_file(path: PathBuf) -> MediaFile {
    let metadata = std::fs::metadata(&path).expect("source metadata");
    MediaFile {
        downloaded_subtitles: Vec::new(),
        id: 17,
        item_id: 3,
        path,
        size: metadata.len() as i64,
        mtime: modified_seconds(&metadata),
        duration_ms: Some(1_000),
        container: Some("mkv".to_owned()),
        video_codec: Some("hevc".to_owned()),
        video_codec_tag: None,
        field_order: None,
        video_profile: Some("Main 10".to_owned()),
        width: Some(3840),
        height: Some(2160),
        bit_depth: Some(10),
        hdr: Some("dolby_vision".to_owned()),
        hdr_format: Some("Dolby Vision · Profile 7".to_owned()),
        max_cll: None,
        max_fall: None,
        mastering_max_luminance: None,
        luminance_source: None,
        dolby_vision: DolbyVisionFacts::default(),
        bitrate: None,
        audio_streams: vec![],
        subtitle_streams: vec![],
        scanned_at: 1,
        audio_offset_ms: 0,
        probed: true,
    }
}

fn probe(profile: i64, el: bool, duration_ms: i64, chapters: usize) -> ProbeResult {
    ProbeResult {
        duration_ms: Some(duration_ms),
        dolby_vision: DolbyVisionFacts {
            profile: Some(profile),
            bl_compat_id: Some(if profile == 8 { 1 } else { 6 }),
            el_present: Some(el),
            rpu_present: Some(true),
            ..Default::default()
        },
        audio_streams: vec![AudioStream {
            index: 0,
            codec: "truehd".to_owned(),
            channels: Some(8),
            sample_rate: Some(48_000),
            channel_layout: None,
            language: Some("eng".to_owned()),
            title: None,
            default: true,
        }],
        subtitle_streams: vec![SubtitleStream {
            index: 0,
            codec: "hdmv_pgs_subtitle".to_owned(),
            language: Some("eng".to_owned()),
            title: None,
            default: true,
            forced: false,
            hearing_impaired: false,
        }],
        raw_json: Some(
            serde_json::json!({
                "streams": [{
                    "codec_type": "video",
                    "avg_frame_rate": "24000/1001",
                    "disposition": { "attached_pic": 0 }
                }],
                "chapters": vec![serde_json::json!({}); chapters]
            })
            .to_string(),
        ),
        ..Default::default()
    }
}

async fn bind_test_replacement(
    paths: &ConversionPaths,
    file: &MediaFile,
    bytes: &[u8],
    keep_original: bool,
) -> i64 {
    tokio::fs::write(&paths.replacement, bytes)
        .await
        .expect("replacement bytes");
    let mut scratch = require_owned_scratch(paths, file, None)
        .await
        .expect("owned scratch");
    let identity = bind_child(&scratch.directory, "replacement.mkv", "test replacement")
        .await
        .expect("replacement identity");
    let bytes_after = identity.content.size as i64;
    scratch.owner.replacement = Some(persisted_binding(&identity, TEST_NODE_ID));
    scratch.owner.expected_bytes = Some(bytes_after);
    scratch.owner.keep_original = Some(keep_original);
    scratch.owner.recovery_guard_id = (!keep_original).then(|| uuid::Uuid::new_v4().to_string());
    write_scratch_owner(&scratch.directory, &scratch.owner)
        .await
        .expect("bound manifest");
    bytes_after
}

async fn stage_and_publish_without_probe(context: &mut PublicationContext) -> Result<(), String> {
    let loss = CancellationToken::new();
    let source_parent = context.source_parent.clone();
    let source_name = context.source_name.clone();
    let source = context
        .bind_source(
            &source_parent,
            &source_name,
            &loss,
            "test source before staging",
        )
        .await?;
    let scratch = context.scratch.directory.clone();
    let staged_local = rename_expected_noreplace_between(
        &source_parent,
        &source_name,
        &source.local,
        &scratch,
        "source.p7.original",
    )
    .await?
    .ok_or_else(|| "test staged destination exists".to_owned())?;
    context.rebaseline_source(staged_local).await?;

    let replacement = context
        .bind_replacement(
            &scratch,
            "replacement.mkv",
            &loss,
            "test replacement before publish",
        )
        .await?;
    if !context.keep_original {
        let guard_local = ensure_public_replacement_proof(
            &scratch,
            "replacement.mkv",
            &replacement.local,
            &scratch,
        )
        .await?;
        context.rebaseline_replacement(guard_local).await?;
    }
    let replacement = context
        .bind_replacement(
            &scratch,
            "replacement.mkv",
            &loss,
            "test replacement after guard creation",
        )
        .await?;
    let published_local = rename_expected_noreplace_between(
        &scratch,
        "replacement.mkv",
        &replacement.local,
        &source_parent,
        &source_name,
    )
    .await?
    .ok_or_else(|| "test public destination exists".to_owned())?;
    context.rebaseline_replacement(published_local).await
}

pub(crate) async fn prepare_test_published_recovery_guard(
    file: &MediaFile,
) -> Result<RecoveryGuardIntent, String> {
    let paths = ConversionPaths::for_file(file)?;
    prepare_fresh_directory(&paths, file).await?;
    let bytes_after = bind_test_replacement(&paths, file, b"profile eight", false).await;
    let mut context = publication_context(&paths, file, bytes_after).await?;
    stage_and_publish_without_probe(&mut context).await?;
    finalize_committed_original(&paths, &mut context).await?;
    recovery_guard_intent(file)
        .await?
        .ok_or_else(|| "test destructive publication did not create a guard".to_owned())
}

#[test]
fn el_summary_is_defensive_and_never_guesses() {
    assert_eq!(parse_el_type("Profile: 7 (MEL)"), Some("mel"));
    assert_eq!(parse_el_type("profile: 7 (fel)"), Some("fel"));
    assert_eq!(parse_el_type("Profile: 8.1"), None);
    assert_eq!(parse_el_type("MEL compatible"), None);
}

#[test]
fn verification_accepts_one_frame_and_rejects_truncation() {
    let source = probe(7, true, 7_200_000, 12);
    let replacement = probe(8, false, 7_199_959, 12);
    verify_replacement(&source, &replacement).expect("within one 23.976 fps frame");

    let truncated = probe(8, false, 7_190_000, 12);
    assert!(verify_replacement(&source, &truncated)
        .expect_err("truncated output")
        .contains("more than one"));
}

#[test]
fn verification_rejects_every_mux_mismatch() {
    let source = probe(7, true, 1_000, 1);
    let mut replacement = probe(8, false, 1_000, 1);
    replacement.audio_streams.clear();
    assert!(verify_replacement(&source, &replacement)
        .expect_err("missing audio")
        .contains("audio track"));

    let mut replacement = probe(8, false, 1_000, 1);
    replacement.subtitle_streams.clear();
    assert!(verify_replacement(&source, &replacement)
        .expect_err("missing subtitle")
        .contains("subtitle track"));

    let replacement = probe(8, false, 1_000, 0);
    assert!(verify_replacement(&source, &replacement)
        .expect_err("missing chapter")
        .contains("chapter count"));
}

#[test]
fn verification_requires_explicit_profile_81_compatibility_and_rpu() {
    let source = probe(7, true, 1_000, 1);

    let mut wrong_compatibility = probe(8, false, 1_000, 1);
    wrong_compatibility.dolby_vision.bl_compat_id = Some(4);
    assert!(verify_replacement(&source, &wrong_compatibility)
        .expect_err("Profile 8.4 is not Profile 8.1")
        .contains("expected 1 for Profile 8.1"));

    let mut unknown_compatibility = probe(8, false, 1_000, 1);
    unknown_compatibility.dolby_vision.bl_compat_id = None;
    assert!(verify_replacement(&source, &unknown_compatibility)
        .expect_err("unknown compatibility is unsafe")
        .contains("expected 1 for Profile 8.1"));

    let mut missing_rpu = probe(8, false, 1_000, 1);
    missing_rpu.dolby_vision.rpu_present = Some(false);
    assert!(verify_replacement(&source, &missing_rpu)
        .expect_err("missing RPU is not Dolby Vision")
        .contains("rpu_present_flag"));

    let mut unknown_rpu = probe(8, false, 1_000, 1);
    unknown_rpu.dolby_vision.rpu_present = None;
    assert!(verify_replacement(&source, &unknown_rpu)
        .expect_err("unknown RPU presence is unsafe")
        .contains("rpu_present_flag"));
}

#[test]
fn mkvmerge_version_floor_is_parsed_from_the_supported_banner() {
    assert_eq!(
        mkvmerge_major("mkvmerge v68.0.0 ('The Curtain') 64-bit"),
        Some(68)
    );
    assert_eq!(mkvmerge_major("mkvmerge v100.1.2"), Some(100));
    assert_eq!(mkvmerge_major("unknown"), None);
}

#[test]
fn dovi_tool_version_floor_is_parsed_defensively() {
    assert_eq!(dovi_tool_version("dovi_tool 2.3.3"), Some((2, 3, 3)));
    assert_eq!(dovi_tool_version("dovi_tool v2.4.0-nightly"), None);
    assert_eq!(dovi_tool_version("dovi_tool 2.3.3 garbage"), None);
    assert_eq!(dovi_tool_version("dovi_tool 2.3.3garbage"), None);
    assert_eq!(dovi_tool_version("dovi_tool unknown"), None);
    assert_eq!(dovi_tool_version("dovi_tool nightly libdovi 99.0.0"), None);
    assert_eq!(dovi_tool_version("libdovi 99.0.0"), None);
}

#[test]
fn local_inode_fences_distinguish_swaps_but_portable_identity_survives_a_new_mount() {
    let before = LocalMediaIdentity {
        device: 1,
        inode: 10,
        size: 4_096,
        modified_seconds: 2,
        modified_nanoseconds: 3,
        changed_seconds: 4,
        changed_nanoseconds: 5,
    };
    let mut swapped = before.clone();
    swapped.device += 8;
    swapped.inode += 1;
    assert!(!before.same_inode_and_content_facts(&swapped));

    let manifest = MediaContentIdentity {
        size: 4_096,
        sha256: "successor-mount-content".to_owned(),
    };
    let rebound_on_successor = manifest.clone();
    assert_eq!(manifest, rebound_on_successor);
}

#[tokio::test]
async fn successor_mount_rebinds_equal_bytes_with_a_different_local_tuple() {
    let root = crate::test_tempdir().expect("successor mount root");
    let first = root.path().join("first.mkv");
    let successor = root.path().join("successor.mkv");
    tokio::fs::write(&first, b"node-portable media bytes")
        .await
        .expect("first bytes");
    tokio::fs::write(&successor, b"node-portable media bytes")
        .await
        .expect("successor bytes");
    let parent = SecureDirectory::open(root.path()).await.expect("parent");
    let first = bind_child(&parent, "first.mkv", "first mount")
        .await
        .expect("first binding");
    let successor = bind_child(&parent, "successor.mkv", "successor mount")
        .await
        .expect("successor binding");
    assert_ne!(first.local, successor.local);
    assert_eq!(first.content, successor.content);

    let file = media_file(root.path().join("first.mkv"));
    let owner =
        ScratchOwner::new(&file, persisted_binding(&first, "first-node")).expect("portable owner");
    assert_eq!(owner.source.content, successor.content);
}

#[tokio::test]
async fn takeover_rejects_an_impossible_size_before_hashing() {
    let root = crate::test_tempdir().expect("takeover size root");
    tokio::fs::write(root.path().join("first"), b"one")
        .await
        .expect("first");
    tokio::fs::write(root.path().join("second"), b"four")
        .await
        .expect("second");
    tokio::fs::write(root.path().join("candidate"), b"wrong")
        .await
        .expect("candidate");
    let parent = SecureDirectory::open(root.path()).await.expect("parent");
    let first = bind_child(&parent, "first", "first candidate")
        .await
        .expect("first binding");
    let second = bind_child(&parent, "second", "second candidate")
        .await
        .expect("second binding");
    reset_hash_passes();
    let error = bind_one_of(
        &parent,
        "candidate",
        &persisted_binding(&first, "old-node"),
        &persisted_binding(&second, "old-node"),
        "successor-node",
        &CancellationToken::new(),
        "impossible takeover",
    )
    .await
    .err()
    .expect("impossible size rejected");
    assert!(error.contains("does not match either manifest candidate"));
    assert!(
        hash_roles().is_empty(),
        "size rejection must precede hashing"
    );
}

#[tokio::test]
async fn fresh_publication_and_cleanup_hash_source_and_replacement_once_each() {
    reset_hash_passes();
    let root = crate::test_tempdir().expect("bounded hash root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"original profile seven")
        .await
        .expect("source");
    let file = media_file(source);
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("fresh scratch");
    let bytes_after = bind_test_replacement(&paths, &file, b"verified profile eight", false).await;
    assert_eq!(
        hash_roles(),
        ["media source", "test replacement"],
        "fresh verification must make exactly one full pass per media object"
    );

    let mut context = publication_context(&paths, &file, bytes_after)
        .await
        .expect("context");
    stage_and_publish_without_probe(&mut context)
        .await
        .expect("publish");
    finalize_committed_original(&paths, &mut context)
        .await
        .expect("post-commit discard");
    cleanup_after_commit(&file).await;
    assert_eq!(
        hash_roles(),
        ["media source", "test replacement"],
        "same-node publication and cleanup must use only local fstat fences"
    );
}

#[tokio::test]
async fn successor_recovery_hashes_each_encountered_artifact_at_most_once() {
    let root = crate::test_tempdir().expect("successor hash root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"original profile seven")
        .await
        .expect("source");
    let file = media_file(source);
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("fresh scratch");
    let bytes_after = bind_test_replacement(&paths, &file, b"verified profile eight", false).await;
    let mut original_node = publication_context(&paths, &file, bytes_after)
        .await
        .expect("original-node context");
    stage_and_publish_without_probe(&mut original_node)
        .await
        .expect("original-node publish");
    drop(original_node);

    reset_hash_passes();
    let successor_node = "successor-node-on-new-mount";
    let loss = CancellationToken::new();
    let mut successor = super::publication_context(&paths, &file, successor_node, bytes_after)
        .await
        .expect("successor context");
    let (public, public_is_source, rebound) = bind_one_of(
        &successor.source_parent,
        &successor.source_name,
        &successor.scratch.owner.source,
        &successor.replacement,
        successor_node,
        &loss,
        "successor public replacement",
    )
    .await
    .expect("successor public binding");
    assert!(!public_is_source);
    assert!(rebound);
    successor
        .rebaseline_replacement(public.local)
        .await
        .expect("rebaseline public");
    let scratch = successor.scratch.directory.clone();
    let staged = successor
        .bind_source(
            &scratch,
            "source.p7.original",
            &loss,
            "successor staged original",
        )
        .await
        .expect("successor original binding");
    successor
        .rebaseline_source(staged.local)
        .await
        .expect("rebaseline original");
    successor
        .bind_replacement(
            &successor.source_parent.clone(),
            &successor.source_name.clone(),
            &loss,
            "successor public replacement repeat",
        )
        .await
        .expect("cached public binding");
    successor
        .bind_source(
            &scratch,
            "source.p7.original",
            &loss,
            "successor staged original repeat",
        )
        .await
        .expect("cached original binding");
    assert_eq!(
        hash_roles(),
        ["successor public replacement", "successor staged original"],
        "a successor may hash each recovered artifact once, then must persist local fences"
    );
}

#[tokio::test]
async fn verify_existing_persists_source_rebaseline_before_replacement_failure() {
    let root = crate::test_tempdir().expect("verify rebaseline root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"original profile seven")
        .await
        .expect("source");
    let file = media_file(source);
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("scratch");
    let bytes_after = bind_test_replacement(&paths, &file, b"replacement-good", false).await;
    tokio::fs::write(&paths.replacement, b"replacement-evil")
        .await
        .expect("corrupt replacement with same length");
    let successor = "successor-rebaseline-node";
    reset_hash_passes();

    for _ in 0..2 {
        let error = super::verify_existing(
            &file,
            successor,
            &CancellationToken::new(),
            None,
            bytes_after,
        )
        .await
        .expect_err("corrupt replacement rejected before probe");
        assert!(error.contains("replacement does not match the manifest-bound bytes"));
    }
    assert_eq!(
        hash_roles(),
        [
            "recovery original",
            "recovery replacement",
            "recovery replacement"
        ],
        "actual recovery persists the source fence before later replacement failure"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn descriptor_bound_probe_rejects_a_pathname_swap_that_changes_inode_facts() {
    let root = crate::test_tempdir().expect("descriptor probe root");
    let source = root.path().join("movie.mkv");
    let moved = root.path().join("held.mkv");
    let captured = root.path().join("captured.bin");
    let ffprobe = root.path().join("ffprobe-test");
    tokio::fs::write(&source, b"held verified bytes")
        .await
        .expect("source");
    let parent = SecureDirectory::open(root.path()).await.expect("parent");
    let held = bind_child(&parent, "movie.mkv", "held source")
        .await
        .expect("held binding");
    tokio::fs::rename(&source, &moved)
        .await
        .expect("move pathname target");
    tokio::fs::write(&source, b"new pathname winner")
        .await
        .expect("winner");
    // Some CI filesystems expose ctime at a granularity coarse enough for
    // the rename and the following fstat to land on the same tick. Change
    // the held inode's size as well so this fixture always exercises the
    // identity fence rather than depending on timestamp resolution.
    tokio::fs::write(&moved, b"held verified bytes changed after binding")
        .await
        .expect("change held inode facts");
    let script = format!(
            "#!/bin/sh\nlast=''\nfor arg in \"$@\"; do last=\"$arg\"; done\ncat \"$last\" > '{}'\nprintf '%s\\n' '{{\"format\":{{\"duration\":\"1.0\"}},\"streams\":[],\"chapters\":[]}}'\n",
            captured.display()
        );
    crate::write_test_executable(&ffprobe, script, 0o700);

    let error = probe_bound_with(
        ffprobe.to_str().expect("UTF-8 script"),
        &held,
        &source,
        "swapped pathname probe",
        &CancellationToken::new(),
        Duration::from_secs(5),
    )
    .await
    .expect_err("pathname swap changed the held inode facts");
    assert!(error.contains("inode facts changed"));
    assert!(!path_entry_exists(&captured)
        .await
        .expect("probe did not run"));
    assert_eq!(
        tokio::fs::read(&source).await.expect("winner survives"),
        b"new pathname winner"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn descriptor_probe_deadline_kills_a_blocked_process() {
    let root = crate::test_tempdir().expect("probe deadline root");
    let source = root.path().join("movie.mkv");
    let ffprobe = root.path().join("blocked-ffprobe");
    tokio::fs::write(&source, b"held bytes")
        .await
        .expect("source");
    crate::write_test_executable(&ffprobe, "#!/bin/sh\nexec sleep 30\n", 0o700);
    let parent = SecureDirectory::open(root.path()).await.expect("parent");
    let held = bind_child(&parent, "movie.mkv", "deadline probe media")
        .await
        .expect("held media");
    let error = probe_bound_with(
        ffprobe.to_str().expect("UTF-8 script"),
        &held,
        &source,
        "deadline probe",
        &CancellationToken::new(),
        Duration::from_millis(25),
    )
    .await
    .expect_err("blocked probe deadline");
    assert!(error.contains("exceeded its deadline"));
}

#[cfg(unix)]
#[tokio::test]
async fn descriptor_probe_cancellation_kills_a_blocked_process() {
    let root = crate::test_tempdir().expect("probe cancellation root");
    let source = root.path().join("movie.mkv");
    let ffprobe = root.path().join("blocked-ffprobe");
    tokio::fs::write(&source, b"held bytes")
        .await
        .expect("source");
    crate::write_test_executable(&ffprobe, "#!/bin/sh\nexec sleep 30\n", 0o700);
    let parent = SecureDirectory::open(root.path()).await.expect("parent");
    let held = bind_child(&parent, "movie.mkv", "cancelled probe media")
        .await
        .expect("held media");
    let loss = CancellationToken::new();
    let cancel = loss.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(25)).await;
        cancel.cancel();
    });
    let error = probe_bound_with(
        ffprobe.to_str().expect("UTF-8 script"),
        &held,
        &source,
        "cancelled probe",
        &loss,
        Duration::from_secs(5),
    )
    .await
    .expect_err("blocked probe cancellation");
    assert!(error.contains("cancelled after conversion lease loss"));
}

/// The deadline the output-bound proofs run under. It only has to outlast
/// a runner that is not scheduling the reader: at 5 s a loaded runner
/// sometimes reached it first, and the proof failed on the deadline's
/// error instead of the cap's. A cap that never fires still fails, here.
#[cfg(unix)]
const OUTPUT_BOUND_TEST_DEADLINE: Duration = Duration::from_secs(60);

#[cfg(unix)]
#[tokio::test]
async fn descriptor_probe_output_is_bounded_while_the_process_runs() {
    let root = crate::test_tempdir().expect("probe output root");
    let source = root.path().join("movie.mkv");
    let ffprobe = root.path().join("noisy-ffprobe");
    tokio::fs::write(&source, b"held bytes")
        .await
        .expect("source");
    crate::write_test_executable(&ffprobe, "#!/bin/sh\nexec yes x\n", 0o700);
    let parent = SecureDirectory::open(root.path()).await.expect("parent");
    let held = bind_child(&parent, "movie.mkv", "bounded probe media")
        .await
        .expect("held media");
    let error = probe_bound_with(
        ffprobe.to_str().expect("UTF-8 script"),
        &held,
        &source,
        "noisy probe",
        &CancellationToken::new(),
        // Only a hang guard: `yes` reaches the output cap in
        // milliseconds, and the cap, not this deadline, must stop it.
        OUTPUT_BOUND_TEST_DEADLINE,
    )
    .await
    .expect_err("probe output bound");
    assert!(error.contains("ffprobe stdout exceeded"), "{error}");
}

#[tokio::test]
async fn identity_conditioned_unlink_preserves_a_swapped_winner() {
    let root = crate::test_tempdir().expect("unlink race root");
    let victim = root.path().join("victim.mkv");
    let old = root.path().join("old.mkv");
    tokio::fs::write(&victim, b"expected old bytes")
        .await
        .expect("victim");
    let parent = SecureDirectory::open(root.path()).await.expect("parent");
    let expected = child_identity(&parent, "victim.mkv")
        .await
        .expect("expected local identity");
    tokio::fs::rename(&victim, &old)
        .await
        .expect("move expected");
    tokio::fs::write(&victim, b"new winner")
        .await
        .expect("winner");

    assert!(unlink_expected_child(&parent, "victim.mkv", &expected)
        .await
        .expect_err("swapped child refused")
        .contains("expected local identity"));
    assert_eq!(
        tokio::fs::read(&victim).await.expect("winner"),
        b"new winner"
    );
    assert_eq!(
        tokio::fs::read(&old).await.expect("old survives"),
        b"expected old bytes"
    );
}

#[tokio::test]
async fn final_unlink_interval_preserves_a_swapped_quarantine_winner() {
    let root = crate::test_tempdir().expect("final unlink race root");
    let victim = root.path().join("victim.mkv");
    tokio::fs::write(&victim, b"manifest-bound victim")
        .await
        .expect("victim");
    let parent = SecureDirectory::open(root.path()).await.expect("parent");
    let parent_identity = parent.identity().await.expect("parent identity");
    let expected = child_identity(&parent, "victim.mkv")
        .await
        .expect("expected local identity");
    let quarantine = root.path().join(delete_quarantine_name("victim.mkv"));
    let moved = root.path().join("checked-quarantine-moved");
    let hook_quarantine = quarantine.clone();
    let hook_moved = moved.clone();
    FINAL_UNLINK_SWAP_HOOKS
        .lock()
        .expect("final unlink hook mutex")
        .push(NamespaceRemovalSwapHook {
            device: parent_identity.device,
            inode: parent_identity.inode,
            target: delete_quarantine_name("victim.mkv"),
            action: Box::new(move || {
                std::fs::rename(&hook_quarantine, &hook_moved)
                    .expect("move checked delete candidate");
                std::fs::write(&hook_quarantine, b"new quarantine winner")
                    .expect("install quarantine winner");
            }),
        });

    let error = unlink_expected_child(&parent, "victim.mkv", &expected)
        .await
        .expect_err("post-check swap must fail closed");
    assert!(error.contains("swapped after its final check"));
    assert_eq!(
        tokio::fs::read(&quarantine)
            .await
            .expect("winner remains linked"),
        b"new quarantine winner"
    );
    assert_eq!(
        tokio::fs::read(&moved)
            .await
            .expect("manifest victim remains linked"),
        b"manifest-bound victim"
    );
    assert!(FINAL_UNLINK_SWAP_HOOKS
        .lock()
        .expect("final unlink hook mutex")
        .is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn private_delete_anchor_rejects_wrong_mode_and_owner_before_mutation() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let root = crate::test_tempdir().expect("private anchor validation root");
    let victim = root.path().join("victim.mkv");
    let anchor = root.path().join(PRIVATE_DELETE_ANCHOR);
    tokio::fs::write(&victim, b"manifest victim")
        .await
        .expect("victim");
    tokio::fs::create_dir(&anchor).await.expect("anchor");
    std::fs::set_permissions(&anchor, std::fs::Permissions::from_mode(0o755)).expect("wrong mode");
    let parent = SecureDirectory::open(root.path()).await.expect("parent");
    let expected = child_identity(&parent, "victim.mkv")
        .await
        .expect("expected victim");
    let error = unlink_expected_child(&parent, "victim.mkv", &expected)
        .await
        .expect_err("permissive anchor refused");
    assert!(error.contains("daemon-owned 0700"), "{error}");
    assert_eq!(
        tokio::fs::read(&victim).await.expect("victim"),
        b"manifest victim"
    );

    std::fs::set_permissions(&anchor, std::fs::Permissions::from_mode(0o700))
        .expect("correct mode");
    let anchor_metadata = std::fs::metadata(&anchor).expect("anchor metadata");
    let parent_metadata = std::fs::metadata(root.path()).expect("parent metadata");
    let wrong_uid = anchor_metadata.uid().wrapping_add(1);
    assert!(
        validate_private_delete_anchor_metadata(&anchor_metadata, &parent_metadata, wrong_uid,)
            .expect_err("foreign owner refused")
            .to_string()
            .contains("daemon-owned 0700")
    );
}

#[tokio::test]
async fn successor_worker_finishes_a_crash_in_the_private_delete_anchor() {
    let root = crate::test_tempdir().expect("private anchor recovery root");
    let victim = root.path().join("victim.mkv");
    tokio::fs::write(&victim, b"manifest victim")
        .await
        .expect("victim");
    let parent = SecureDirectory::open(root.path()).await.expect("parent");
    let expected = child_identity(&parent, "victim.mkv")
        .await
        .expect("expected victim");
    let anchor = parent
        .create_child_directory(PRIVATE_DELETE_ANCHOR)
        .await
        .expect("private anchor");
    let slot = private_delete_slot("victim.mkv", PrivateDeleteKind::File);
    assert!(
        rename_expected_noreplace_between(&parent, "victim.mkv", &expected, &anchor, &slot,)
            .await
            .expect("simulate worker A crash after private move")
            .is_some()
    );

    unlink_expected_child(&parent, "victim.mkv", &expected)
        .await
        .expect("worker B resumes the deterministic private slot");
    assert!(!child_exists(&anchor, &slot).await.expect("slot absent"));
    assert!(!child_exists(&parent, "victim.mkv")
        .await
        .expect("public candidate absent"));
}

#[tokio::test]
async fn identity_conditioned_rename_preserves_a_swapped_source() {
    let root = crate::test_tempdir().expect("rename race root");
    let source = root.path().join("source.mkv");
    let old = root.path().join("old.mkv");
    tokio::fs::write(&source, b"expected old bytes")
        .await
        .expect("source");
    let parent = SecureDirectory::open(root.path()).await.expect("parent");
    let expected = child_identity(&parent, "source.mkv")
        .await
        .expect("expected local identity");
    tokio::fs::rename(&source, &old)
        .await
        .expect("move expected");
    tokio::fs::write(&source, b"new winner")
        .await
        .expect("winner");

    assert!(rename_expected_noreplace_between(
        &parent,
        "source.mkv",
        &expected,
        &parent,
        "destination.mkv",
    )
    .await
    .expect_err("swapped source refused")
    .contains("expected local identity"));
    assert_eq!(
        tokio::fs::read(&source).await.expect("winner"),
        b"new winner"
    );
    assert!(!path_entry_exists(&root.path().join("destination.mkv"))
        .await
        .expect("destination absent"));
}

#[tokio::test]
async fn post_rename_reopen_failure_restores_and_syncs_the_source() {
    let root = crate::test_tempdir().expect("rename reopen root");
    let source = root.path().join("source.mkv");
    tokio::fs::write(&source, b"expected bytes")
        .await
        .expect("source");
    let parent = SecureDirectory::open(root.path()).await.expect("parent");
    let parent_identity = parent.identity().await.expect("parent identity");
    let expected = child_identity(&parent, "source.mkv")
        .await
        .expect("source identity");
    RENAME_REOPEN_FAILURE_HOOKS
        .lock()
        .expect("rename hook mutex")
        .push(RenameReopenFailureHook {
            from_device: parent_identity.device,
            from_inode: parent_identity.inode,
            to_device: parent_identity.device,
            to_inode: parent_identity.inode,
            from_name: "source.mkv".to_owned(),
            to_name: "destination.mkv".to_owned(),
            action: Box::new(|| {}),
        });

    let error = rename_expected_noreplace_between(
        &parent,
        "source.mkv",
        &expected,
        &parent,
        "destination.mkv",
    )
    .await
    .expect_err("injected reopen failure");
    assert!(error.contains("was restored"));
    assert_eq!(
        tokio::fs::read(&source).await.expect("restored source"),
        b"expected bytes"
    );
    assert!(!path_entry_exists(&root.path().join("destination.mkv"))
        .await
        .expect("destination absent"));
}

#[tokio::test]
async fn post_rename_reopen_failure_preserves_a_blocking_source_winner() {
    let root = crate::test_tempdir().expect("rename blocked compensation root");
    let source = root.path().join("source.mkv");
    let destination = root.path().join("destination.mkv");
    tokio::fs::write(&source, b"expected bytes")
        .await
        .expect("source");
    let parent = SecureDirectory::open(root.path()).await.expect("parent");
    let parent_identity = parent.identity().await.expect("parent identity");
    let expected = child_identity(&parent, "source.mkv")
        .await
        .expect("source identity");
    let winner_path = source.clone();
    RENAME_REOPEN_FAILURE_HOOKS
        .lock()
        .expect("rename hook mutex")
        .push(RenameReopenFailureHook {
            from_device: parent_identity.device,
            from_inode: parent_identity.inode,
            to_device: parent_identity.device,
            to_inode: parent_identity.inode,
            from_name: "source.mkv".to_owned(),
            to_name: "destination.mkv".to_owned(),
            action: Box::new(move || {
                std::fs::write(&winner_path, b"new source winner").expect("source winner");
            }),
        });

    let error = rename_expected_noreplace_between(
        &parent,
        "source.mkv",
        &expected,
        &parent,
        "destination.mkv",
    )
    .await
    .expect_err("blocked compensation refused");
    assert!(error.contains("restoration was blocked"));
    assert_eq!(
        tokio::fs::read(&source).await.expect("winner"),
        b"new source winner"
    );
    assert_eq!(
        tokio::fs::read(&destination)
            .await
            .expect("recovery destination"),
        b"expected bytes"
    );
}

#[tokio::test]
async fn expected_root_cleanup_preserves_a_swapped_quarantine() {
    let root = crate::test_tempdir().expect("tree race root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"source").await.expect("source");
    let file = media_file(source);
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("scratch");
    let parent = SecureDirectory::open(root.path()).await.expect("parent");
    assert!(parent
        .rename_child_noreplace(&paths.directory_name, &paths.cleanup_name)
        .await
        .expect("initial quarantine"));
    let held = parent
        .open_child_directory(&paths.cleanup_name)
        .await
        .expect("held quarantine");
    let expected = held.identity().await.expect("expected root");
    let moved_name = format!("{}.moved", paths.cleanup_name);
    assert!(parent
        .rename_child_noreplace(&paths.cleanup_name, &moved_name)
        .await
        .expect("move expected root"));
    tokio::fs::create_dir(root.path().join(&paths.cleanup_name))
        .await
        .expect("winning root");
    tokio::fs::write(
        root.path().join(&paths.cleanup_name).join("winner"),
        b"winner",
    )
    .await
    .expect("winner child");

    assert!(
        remove_flat_tree_expected(&parent, &paths.cleanup_name, &held, expected)
            .await
            .expect_err("swapped cleanup root refused")
            .contains("quarantined identity")
    );
    assert_eq!(
        tokio::fs::read(root.path().join(&paths.cleanup_name).join("winner"))
            .await
            .expect("winner survives"),
        b"winner"
    );
}

#[tokio::test]
async fn final_rmdir_interval_preserves_a_swapped_cleanup_root() {
    let root = crate::test_tempdir().expect("final rmdir race root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"source").await.expect("source");
    let file = media_file(source);
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("scratch");
    let parent = SecureDirectory::open(root.path()).await.expect("parent");
    assert!(parent
        .rename_child_noreplace(&paths.directory_name, &paths.cleanup_name)
        .await
        .expect("quarantine root"));
    let held = parent
        .open_child_directory(&paths.cleanup_name)
        .await
        .expect("held cleanup root");
    // Empty the owned tree before the final root-removal helper. The test
    // hook targets only the post-fstat/pre-namespace-transition interval.
    for child in held
        .child_names(MAX_SCRATCH_ENTRIES)
        .await
        .expect("owned children")
    {
        tokio::fs::remove_file(root.path().join(&paths.cleanup_name).join(child))
            .await
            .expect("remove test child");
    }
    let expected = held.identity().await.expect("expected cleanup identity");
    let parent_identity = parent.identity().await.expect("parent identity");
    let cleanup = root.path().join(&paths.cleanup_name);
    let moved = root.path().join("checked-cleanup-root-moved");
    let hook_cleanup = cleanup.clone();
    let hook_moved = moved.clone();
    FINAL_RMDIR_SWAP_HOOKS
        .lock()
        .expect("final rmdir hook mutex")
        .push(NamespaceRemovalSwapHook {
            device: parent_identity.device,
            inode: parent_identity.inode,
            target: paths.cleanup_name.clone(),
            action: Box::new(move || {
                std::fs::rename(&hook_cleanup, &hook_moved).expect("move checked cleanup root");
                std::fs::create_dir(&hook_cleanup).expect("install cleanup winner");
                std::fs::write(hook_cleanup.join("winner"), b"winner").expect("winner marker");
            }),
        });

    let error = remove_flat_tree_expected(&parent, &paths.cleanup_name, &held, expected)
        .await
        .expect_err("post-check root swap must fail closed");
    assert!(error.contains("swapped after its final check"));
    assert_eq!(
        tokio::fs::read(cleanup.join("winner"))
            .await
            .expect("winner root survives"),
        b"winner"
    );
    assert!(moved.is_dir(), "manifest cleanup root remains linked");
    assert!(FINAL_RMDIR_SWAP_HOOKS
        .lock()
        .expect("final rmdir hook mutex")
        .is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn capability_probe_rejects_an_old_dovi_tool() {
    let root = crate::test_tempdir().expect("tool root");
    let dovi = root.path().join("dovi-tool");
    let mkvmerge = root.path().join("mkvmerge");
    crate::write_test_executable(&dovi, "#!/bin/sh\necho 'dovi_tool 2.2.0'\n", 0o700);
    crate::write_test_executable(
        &mkvmerge,
        "#!/bin/sh\necho \"mkvmerge v74.0.0 ('You Oughta Know') 64-bit\"\n",
        0o700,
    );
    let tools = DvDiskTools::new(
        "ffmpeg".to_owned(),
        dovi.display().to_string(),
        mkvmerge.display().to_string(),
    );
    let capabilities = probe_capabilities_with(&tools).await;
    assert!(!capabilities.available);
    assert!(capabilities
        .unavailable_reason()
        .expect("reason")
        .contains("2.3.3 or newer"));
}

#[tokio::test]
async fn capability_output_is_bounded_before_it_can_accumulate() {
    let bytes = vec![b'x'; MAX_TOOL_OUTPUT + 1];
    assert!(read_bounded_output(bytes.as_slice())
        .await
        .expect_err("oversized probe output")
        .to_string()
        .contains("exceeded"));
}

#[tokio::test]
async fn full_hash_deadline_interrupts_a_blocked_reader() {
    let mut reader = BlockedHashReader;
    let error = hash_held_exact(
        &mut reader,
        1,
        &CancellationToken::new(),
        tokio::time::Instant::now() + Duration::from_millis(20),
        "blocked deadline reader",
    )
    .await
    .expect_err("blocked hash must time out");
    assert!(error.contains("exceeded its deadline"));
}

#[tokio::test]
async fn full_hash_cancellation_interrupts_a_blocked_reader() {
    let mut reader = BlockedHashReader;
    let loss = CancellationToken::new();
    let cancel = loss.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        cancel.cancel();
    });
    let error = hash_held_exact(
        &mut reader,
        1,
        &loss,
        tokio::time::Instant::now() + Duration::from_secs(5),
        "blocked cancelled reader",
    )
    .await
    .expect_err("blocked hash must cancel");
    assert!(error.contains("cancelled after conversion lease loss"));
}

#[tokio::test]
async fn timed_out_blocking_hash_keeps_the_bounded_worker_charged() {
    let state = Arc::new(HashWorkerState::new());
    let release = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let worker_release = Arc::clone(&release);
    let loss = CancellationToken::new();
    let error = run_bounded_hash_job(
        Arc::clone(&state),
        &loss,
        tokio::time::Instant::now() + Duration::from_millis(30),
        "blocked regular file",
        move || {
            let (lock, wake) = &*worker_release;
            let mut released = lock.lock().expect("release mutex");
            while !*released {
                released = wake.wait(released).expect("release wait");
            }
            Ok(MediaContentIdentity {
                size: 0,
                sha256: String::new(),
            })
        },
    )
    .await
    .expect_err("blocked job must time out");
    assert!(error.contains("remains charged"));

    let retry = run_bounded_hash_job(
        Arc::clone(&state),
        &loss,
        tokio::time::Instant::now() + Duration::from_secs(1),
        "retry",
        || unreachable!("residual worker must reject before spawning"),
    )
    .await
    .expect_err("retry must be visibly suppressed");
    assert!(retry.contains("still occupying the bounded hash worker"));

    let (lock, wake) = &*release;
    *lock.lock().expect("release mutex") = true;
    wake.notify_all();
    tokio::time::timeout(Duration::from_secs(1), async {
        while state.residual_io.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("worker capacity becomes available after syscall returns");
}

#[tokio::test]
async fn delayed_hash_job_owns_its_descriptor_before_path_and_fd_reuse() {
    let root = crate::test_tempdir().expect("owned hash descriptor root");
    let path = root.path().join("candidate.bin");
    tokio::fs::write(&path, b"manifest-bound bytes")
        .await
        .expect("original bytes");
    let file = tokio::fs::File::open(&path).await.expect("held original");
    #[cfg(unix)]
    let owned = duplicate_hash_file(&file, "delayed owned descriptor")
        .expect("duplicate before scheduling");
    #[cfg(windows)]
    let owned = duplicate_hash_file(&file, "delayed owned descriptor")
        .await
        .expect("duplicate before scheduling");

    drop(file);
    tokio::fs::remove_file(&path)
        .await
        .expect("unlink original pathname");
    tokio::fs::write(&path, b"new inode reusing the pathname")
        .await
        .expect("replacement inode");
    // Open several descriptors after dropping the caller's handle so an
    // implementation that deferred F_DUPFD into the worker is exposed to
    // ordinary descriptor-number reuse.
    let mut reused = Vec::new();
    for _ in 0..8 {
        reused.push(std::fs::File::open(&path).expect("reused descriptor"));
    }

    let identity = hash_regular_file_exact_blocking(
        owned,
        b"manifest-bound bytes".len() as u64,
        &CancellationToken::new(),
        std::time::Instant::now() + Duration::from_secs(5),
        "delayed owned descriptor",
    )
    .expect("owned descriptor still hashes the manifest object");
    assert_eq!(identity.size, b"manifest-bound bytes".len() as u64);
    assert_eq!(
        identity.sha256,
        format!("{:x}", Sha256::digest(b"manifest-bound bytes"))
    );
    drop(reused);
}

#[cfg(unix)]
#[tokio::test]
async fn conversion_tool_output_is_bounded_while_the_process_is_running() {
    let root = crate::test_tempdir().expect("noisy conversion tool root");
    let tool = root.path().join("noisy-tool");
    crate::write_test_executable(&tool, "#!/bin/sh\nexec yes noisy-output\n", 0o700);
    let error = run_tool_with_timeout(
        tool.to_str().expect("UTF-8 tool"),
        &[],
        &CancellationToken::new(),
        OUTPUT_BOUND_TEST_DEADLINE,
    )
    .await
    .expect_err("noisy tool must be stopped at the stream cap");
    assert!(error.contains("conversion tool stdout exceeded"), "{error}");
}

#[cfg(unix)]
#[tokio::test]
async fn conversion_tool_deadline_kills_a_hung_process() {
    let root = crate::test_tempdir().expect("hung conversion tool root");
    let tool = root.path().join("hung-tool");
    crate::write_test_executable(&tool, "#!/bin/sh\nexec sleep 60\n", 0o700);
    let started = tokio::time::Instant::now();
    let error = run_tool_with_timeout(
        tool.to_str().expect("UTF-8 tool"),
        &[],
        &CancellationToken::new(),
        Duration::from_millis(30),
    )
    .await
    .expect_err("hung tool must exceed its deadline");
    assert!(error.contains("execution deadline"));
    // The tool would run for 60 s; returning well inside that is what
    // shows the deadline ended it. The bound is half the tool's life, not
    // a latency promise: a loaded runner took more than the 2 s this
    // used to allow to schedule the kill and the reap.
    assert!(started.elapsed() < Duration::from_secs(30));
}

#[cfg(unix)]
async fn wait_for_test_path(path: &Path) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if tokio::fs::symlink_metadata(path).await.is_ok() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {}",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[cfg(unix)]
async fn assert_bound_source_tool_survives_transient_swap(role: &'static str) {
    let root = crate::test_tempdir().expect("descriptor-bound tool root");
    let source_path = root.path().join("source.mkv");
    let saved_source = root.path().join("source.saved");
    let attacker_path = root.path().join("attacker.mkv");
    let output = root.path().join("output.bin");
    let ready = root.path().join("ready");
    let release = root.path().join("release");
    let consumed = root.path().join("consumed");
    let finish = root.path().join("finish");
    let tool = root.path().join("bound-source-tool");
    tokio::fs::write(&source_path, b"manifest-bound source bytes")
        .await
        .expect("source bytes");
    tokio::fs::hard_link(&source_path, &saved_source)
        .await
        .expect("saved source link");
    tokio::fs::write(&attacker_path, b"pathname-swap attacker bytes")
        .await
        .expect("attacker bytes");
    crate::write_test_executable(&tool, "#!/bin/sh\nset -eu\n: > \"$3\"\nwhile [ ! -e \"$4\" ]; do sleep 0.005; done\ncat \"$1\" > \"$2\"\n: > \"$5\"\nwhile [ ! -e \"$6\" ]; do sleep 0.005; done\n", 0o700);

    let parent = SecureDirectory::open(root.path()).await.expect("parent");
    let source = bind_child(&parent, "source.mkv", role)
        .await
        .expect("bound source");
    let program = tool.to_str().expect("UTF-8 tool path").to_owned();
    let arguments = vec![
        OsString::new(),
        output.as_os_str().to_owned(),
        ready.as_os_str().to_owned(),
        release.as_os_str().to_owned(),
        consumed.as_os_str().to_owned(),
        finish.as_os_str().to_owned(),
    ];
    let loss = CancellationToken::new();
    let task = tokio::spawn(async move {
        run_tool_with_bound_media(&program, &arguments, 0, &source, role, &loss).await
    });

    wait_for_test_path(&ready).await;
    tokio::fs::rename(&attacker_path, &source_path)
        .await
        .expect("swap attacker into reusable source pathname");
    tokio::fs::write(&release, b"")
        .await
        .expect("release tool read");
    wait_for_test_path(&consumed).await;
    tokio::fs::rename(&saved_source, &source_path)
        .await
        .expect("restore original source pathname");
    tokio::fs::write(&finish, b"").await.expect("let tool exit");

    let result = task.await.expect("tool task");
    if let Err(error) = result {
        assert!(
            error.contains("inode facts changed"),
            "unexpected fail-closed result: {error}"
        );
    }
    assert_eq!(
        tokio::fs::read(&output).await.expect("tool output"),
        b"manifest-bound source bytes"
    );
    assert_eq!(
        tokio::fs::read(&source_path)
            .await
            .expect("restored public source"),
        b"manifest-bound source bytes"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn ffmpeg_extraction_uses_manifest_bound_source_during_transient_path_swap() {
    assert_bound_source_tool_survives_transient_swap("source during ffmpeg extraction").await;
}

#[cfg(unix)]
#[tokio::test]
async fn mkvmerge_remux_uses_manifest_bound_source_during_transient_path_swap() {
    assert_bound_source_tool_survives_transient_swap("source during mkvmerge remux").await;
}

#[tokio::test]
async fn full_hash_rejects_short_and_growth_eof_streams() {
    let root = crate::test_tempdir().expect("hash EOF root");
    let short_path = root.path().join("short.bin");
    let grown_path = root.path().join("grown.bin");
    tokio::fs::write(&short_path, b"abc").await.expect("short");
    tokio::fs::write(&grown_path, b"abcde")
        .await
        .expect("grown");
    let mut short = tokio::fs::File::open(&short_path)
        .await
        .expect("open short");
    let mut grown = tokio::fs::File::open(&grown_path)
        .await
        .expect("open grown");
    let loss = CancellationToken::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    assert!(
        hash_held_exact(&mut short, 4, &loss, deadline, "short stream")
            .await
            .expect_err("short stream rejected")
            .contains("ended after 3 bytes")
    );
    assert!(
        hash_held_exact(&mut grown, 4, &loss, deadline, "grown stream")
            .await
            .expect_err("growth EOF rejected")
            .contains("grew beyond its expected 4 bytes")
    );
}

#[tokio::test]
async fn exclusive_rename_never_clobbers_a_winning_destination() {
    let root = crate::test_tempdir().expect("rename root");
    let source = root.path().join("source");
    let destination = root.path().join("destination");
    tokio::fs::write(&source, b"source")
        .await
        .expect("source bytes");
    tokio::fs::write(&destination, b"winner")
        .await
        .expect("destination bytes");
    assert!(!rename_noreplace_durable(&source, &destination)
        .await
        .expect("exclusive refusal"));
    assert_eq!(tokio::fs::read(&source).await.expect("source"), b"source");
    assert_eq!(
        tokio::fs::read(&destination).await.expect("destination"),
        b"winner"
    );

    tokio::fs::remove_file(&destination)
        .await
        .expect("remove winner");
    assert!(rename_noreplace_durable(&source, &destination)
        .await
        .expect("exclusive rename"));
    assert!(!path_entry_exists(&source).await.expect("source absence"));
    assert_eq!(
        tokio::fs::read(&destination).await.expect("destination"),
        b"source"
    );
}

#[tokio::test]
async fn scratch_cleanup_requires_the_exact_owned_marker() {
    let root = crate::test_tempdir().expect("scratch root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"profile seven")
        .await
        .expect("source bytes");
    let file = media_file(source);
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("first scratch");
    tokio::fs::write(&paths.raw, b"stale")
        .await
        .expect("stale scratch");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("owned scratch replacement");
    assert!(!path_entry_exists(&paths.raw).await.expect("stale removed"));

    tokio::fs::write(
        paths.directory.join(SCRATCH_OWNER_FILE),
        br#"{"version":1,"file_id":999,"source_path":"/other","source_object_version":"other"}"#,
    )
    .await
    .expect("foreign owner");
    tokio::fs::write(&paths.raw, b"must survive")
        .await
        .expect("foreign scratch bytes");
    assert!(prepare_fresh_directory(&paths, &file)
        .await
        .expect_err("foreign scratch refusal")
        .contains("refusing"));
    assert_eq!(
        tokio::fs::read(&paths.raw)
            .await
            .expect("preserved foreign data"),
        b"must survive"
    );
}

#[tokio::test]
async fn rollback_restores_the_staged_inode_without_clobbering() {
    let root = crate::test_tempdir().expect("rollback root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"original profile seven")
        .await
        .expect("source bytes");
    let file = media_file(source.clone());
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("scratch");
    let bytes_after = bind_test_replacement(&paths, &file, b"invalid replacement", true).await;
    assert!(rename_noreplace_durable(&source, &paths.staged_original)
        .await
        .expect("stage original"));
    assert!(rename_noreplace_durable(&paths.replacement, &source)
        .await
        .expect("publish replacement"));

    let mut context = publication_context(&paths, &file, bytes_after)
        .await
        .expect("publication context");
    let outcome = rollback_published(
        &file,
        &mut context,
        &CancellationToken::new(),
        "invalid post-publication probe".to_owned(),
    )
    .await
    .expect("rollback");
    assert!(matches!(
        outcome,
        PublicationOutcome::SafelyRolledBack { .. }
    ));
    assert_eq!(
        tokio::fs::read(&source).await.expect("restored source"),
        b"original profile seven"
    );
    assert!(!path_entry_exists(&paths.staged_original)
        .await
        .expect("staged absence"));
}

#[tokio::test]
async fn rollback_restores_an_original_already_moved_to_retention() {
    let root = crate::test_tempdir().expect("retained rollback root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"original profile seven")
        .await
        .expect("source bytes");
    let file = media_file(source.clone());
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("scratch");
    let bytes_after = bind_test_replacement(&paths, &file, b"invalid replacement", true).await;
    assert!(rename_noreplace_durable(&source, &paths.staged_original)
        .await
        .expect("stage original"));
    assert!(rename_noreplace_durable(&paths.replacement, &source)
        .await
        .expect("publish replacement"));
    let mut context = publication_context(&paths, &file, bytes_after)
        .await
        .expect("publication context");
    retain_original_before_commit(&paths, &mut context, &CancellationToken::new())
        .await
        .expect("retain original");

    let outcome = rollback_published(
        &file,
        &mut context,
        &CancellationToken::new(),
        "post-retention validation failed".to_owned(),
    )
    .await
    .expect("rollback retained original");

    assert!(matches!(
        outcome,
        PublicationOutcome::SafelyRolledBack { .. }
    ));
    assert_eq!(
        tokio::fs::read(&source).await.expect("restored source"),
        b"original profile seven"
    );
    assert!(!path_entry_exists(&paths.retained_original)
        .await
        .expect("retained moved back"));
}

#[tokio::test]
async fn committed_finalization_retains_a_staged_original_after_commit() {
    let root = crate::test_tempdir().expect("recovery root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"original profile seven")
        .await
        .expect("source bytes");
    let file = media_file(source.clone());
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("scratch");
    let bytes_after = bind_test_replacement(&paths, &file, b"profile eight", true).await;
    assert!(rename_noreplace_durable(&source, &paths.staged_original)
        .await
        .expect("stage original"));
    assert!(rename_noreplace_durable(&paths.replacement, &source)
        .await
        .expect("published bytes"));

    let mut context = publication_context(&paths, &file, bytes_after)
        .await
        .expect("publication context");
    let retained = retain_original_before_commit(&paths, &mut context, &CancellationToken::new())
        .await
        .expect("retention")
        .expect("retained path");
    finalize_committed_original(&paths, &mut context)
        .await
        .expect("post-commit retention validation");
    assert_eq!(retained, paths.retained_original.to_str().expect("UTF-8"));
    assert_eq!(
        tokio::fs::read(&paths.retained_original)
            .await
            .expect("retained bytes"),
        b"original profile seven"
    );
}

#[tokio::test]
async fn verified_resume_refuses_a_new_source_inode() {
    let root = crate::test_tempdir().expect("resume root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"source A")
        .await
        .expect("source A");
    let file = media_file(source.clone());
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("scratch");
    let bytes_after = bind_test_replacement(&paths, &file, b"replacement A", true).await;

    let replacement_source = root.path().join("source-B");
    tokio::fs::write(&replacement_source, b"source B")
        .await
        .expect("source B");
    tokio::fs::rename(&replacement_source, &source)
        .await
        .expect("install source B");
    let error = verify_existing(&file, None, bytes_after)
        .await
        .expect_err("new source must be refused");
    assert!(error.contains("manifest-bound bytes"));
    assert_eq!(
        tokio::fs::read(&source).await.expect("source survives"),
        b"source B"
    );
}

#[tokio::test]
async fn published_recovery_does_not_rollback_a_verified_unpublished_attempt() {
    let root = crate::test_tempdir().expect("unpublished recovery root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"original profile seven")
        .await
        .expect("source");
    let file = media_file(source.clone());
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("scratch");
    let bytes_after = bind_test_replacement(&paths, &file, b"profile eight", false).await;

    assert!(
        recover_published(&file, bytes_after, &CancellationToken::new())
            .await
            .expect_err("not published yet")
            .contains("not the manifest-bound published replacement")
    );
    assert_eq!(
        tokio::fs::read(&source).await.expect("original survives"),
        b"original profile seven"
    );
    assert_eq!(
        tokio::fs::read(&paths.replacement)
            .await
            .expect("verified replacement survives"),
        b"profile eight"
    );
}

#[tokio::test]
async fn verified_resume_refuses_a_swapped_replacement_inode() {
    let root = crate::test_tempdir().expect("replacement root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"source A")
        .await
        .expect("source A");
    let file = media_file(source);
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("scratch");
    let bytes_after = bind_test_replacement(&paths, &file, b"replacement A", true).await;

    let swapped = paths.directory.join("swapped.mkv");
    tokio::fs::write(&swapped, b"replacement B")
        .await
        .expect("replacement B");
    tokio::fs::rename(&swapped, &paths.replacement)
        .await
        .expect("swap replacement");
    let error = verify_existing(&file, None, bytes_after)
        .await
        .expect_err("new replacement must be refused");
    assert!(error.contains("manifest-bound bytes"));
}

#[tokio::test]
async fn rollback_preserves_a_newer_public_winner() {
    let root = crate::test_tempdir().expect("winner root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"original profile seven")
        .await
        .expect("source");
    let file = media_file(source.clone());
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("scratch");
    let bytes_after = bind_test_replacement(&paths, &file, b"verified replacement", true).await;
    assert!(rename_noreplace_durable(&source, &paths.staged_original)
        .await
        .expect("stage source"));
    assert!(rename_noreplace_durable(&paths.replacement, &source)
        .await
        .expect("publish replacement"));
    tokio::fs::rename(&source, root.path().join("old-published"))
        .await
        .expect("move published");
    tokio::fs::write(&source, b"newer public winner")
        .await
        .expect("winner");

    let mut context = publication_context(&paths, &file, bytes_after)
        .await
        .expect("context");
    let error = rollback_published(
        &file,
        &mut context,
        &CancellationToken::new(),
        "probe failed".to_owned(),
    )
    .await
    .expect_err("winner must block rollback");
    assert!(error.contains("does not match either manifest candidate"));
    assert_eq!(
        tokio::fs::read(&source).await.expect("winner survives"),
        b"newer public winner"
    );
    assert!(path_entry_exists(&paths.staged_original)
        .await
        .expect("staged survives"));
}

#[tokio::test]
async fn raced_source_abort_is_crash_convergent() {
    let root = crate::test_tempdir().expect("race root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"source A")
        .await
        .expect("source A");
    let file = media_file(source.clone());
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("scratch");
    let bytes_after = bind_test_replacement(&paths, &file, b"replacement A", true).await;

    let held_a = root.path().join("source-A-held");
    tokio::fs::rename(&source, &held_a)
        .await
        .expect("external source A move");
    tokio::fs::write(&source, b"source B")
        .await
        .expect("source B");
    assert!(rename_noreplace_durable(&source, &paths.staged_original)
        .await
        .expect("worker stages raced source B"));

    let mut context = publication_context(&paths, &file, bytes_after)
        .await
        .expect("context");
    assert!(matches!(
        abort_raced_source_staging(&mut context, &CancellationToken::new())
            .await
            .expect("safe race abort"),
        PublicationOutcome::SafelyRolledBack { .. }
    ));
    assert_eq!(
        tokio::fs::read(&source).await.expect("restored B"),
        b"source B"
    );
    assert_eq!(
        tokio::fs::read(&held_a).await.expect("external A survives"),
        b"source A"
    );

    // Simulate a process crash before the ledger moves verified -> failed.
    let mut recovered = publication_context(&paths, &file, bytes_after)
        .await
        .expect("recovered context");
    let reason = recovered
        .scratch
        .owner
        .rollback_reason
        .clone()
        .expect("persisted reason");
    assert!(matches!(
        rollback_published(&file, &mut recovered, &CancellationToken::new(), reason,)
            .await
            .expect("converged retry"),
        PublicationOutcome::SafelyRolledBack { .. }
    ));
    assert_eq!(
        tokio::fs::read(&source).await.expect("B still public"),
        b"source B"
    );
}

#[tokio::test]
async fn raced_source_abort_recovers_a_crash_before_restoration() {
    let root = crate::test_tempdir().expect("early race root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"source A")
        .await
        .expect("source A");
    let file = media_file(source.clone());
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("scratch");
    let bytes_after = bind_test_replacement(&paths, &file, b"replacement A", true).await;
    tokio::fs::rename(&source, root.path().join("source-A-held"))
        .await
        .expect("external source A move");
    tokio::fs::write(&source, b"source B")
        .await
        .expect("source B");
    assert!(rename_noreplace_durable(&source, &paths.staged_original)
        .await
        .expect("worker stages raced source B"));

    let mut interrupted = publication_context(&paths, &file, bytes_after)
        .await
        .expect("context");
    let staged = bind_child(
        &interrupted.scratch.directory,
        "source.p7.original",
        "interrupted raced source",
    )
    .await
    .expect("staged B identity");
    let reason = "source pathname changed during staging".to_owned();
    interrupted.scratch.owner.rollback_reason = Some(reason.clone());
    interrupted.scratch.owner.rollback_restore = Some(persisted_binding(&staged, TEST_NODE_ID));
    write_scratch_owner(&interrupted.scratch.directory, &interrupted.scratch.owner)
        .await
        .expect("persist interrupted abort");
    drop(interrupted);

    let mut recovered = publication_context(&paths, &file, bytes_after)
        .await
        .expect("recovered context");
    assert!(matches!(
        rollback_published(&file, &mut recovered, &CancellationToken::new(), reason,)
            .await
            .expect("recover interrupted abort"),
        PublicationOutcome::SafelyRolledBack { .. }
    ));
    assert_eq!(
        tokio::fs::read(&source).await.expect("B restored"),
        b"source B"
    );
    assert!(!path_entry_exists(&paths.replacement)
        .await
        .expect("replacement removed"));
}

#[tokio::test]
async fn publication_preserves_the_staged_original_until_commit() {
    let root = crate::test_tempdir().expect("cancel root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"original profile seven")
        .await
        .expect("source");
    let file = media_file(source.clone());
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("scratch");
    let bytes_after = bind_test_replacement(&paths, &file, b"profile eight", false).await;
    assert!(rename_noreplace_durable(&source, &paths.staged_original)
        .await
        .expect("stage source"));
    assert!(rename_noreplace_durable(&paths.replacement, &source)
        .await
        .expect("publish replacement"));
    let context = publication_context(&paths, &file, bytes_after)
        .await
        .expect("context");
    assert!(persisted_original_path(&paths, &context)
        .expect("persisted policy")
        .is_none());
    assert!(path_entry_exists(&paths.staged_original)
        .await
        .expect("staged survives"));
}

#[tokio::test]
async fn post_commit_finalization_preserves_original_when_public_winner_changes() {
    let root = crate::test_tempdir().expect("post-commit winner root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"original profile seven")
        .await
        .expect("source");
    let file = media_file(source.clone());
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("scratch");
    let bytes_after = bind_test_replacement(&paths, &file, b"profile eight", false).await;
    let mut context = publication_context(&paths, &file, bytes_after)
        .await
        .expect("context");
    stage_and_publish_without_probe(&mut context)
        .await
        .expect("publish");

    tokio::fs::rename(&source, root.path().join("published-held"))
        .await
        .expect("move published replacement");
    tokio::fs::write(&source, b"newer public winner")
        .await
        .expect("winner");
    let error = finalize_committed_original(&paths, &mut context)
        .await
        .expect_err("ambiguous finalization refused");
    assert!(
        error.contains("public replacement before verified original deletion"),
        "{error}"
    );
    assert_eq!(
        tokio::fs::read(paths.directory.join(DISCARD_PENDING_FILE))
            .await
            .expect("original preserved"),
        b"original profile seven"
    );
    assert_eq!(
        tokio::fs::read(&source).await.expect("winner preserved"),
        b"newer public winner"
    );
}

#[tokio::test]
async fn post_commit_interval_swap_retains_the_hard_linked_replacement_proof() {
    let root = crate::test_tempdir().expect("post-commit interval root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"original profile seven")
        .await
        .expect("source");
    let file = media_file(source.clone());
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("scratch");
    let bytes_after = bind_test_replacement(&paths, &file, b"profile eight", false).await;
    let mut context = publication_context(&paths, &file, bytes_after)
        .await
        .expect("context");
    stage_and_publish_without_probe(&mut context)
        .await
        .expect("publish");
    let scratch_identity = context
        .scratch
        .directory
        .identity()
        .await
        .expect("scratch identity");
    let moved_public = root.path().join("verified-public-held");
    let hook_source = source.clone();
    FINALIZE_SWAP_HOOKS
        .lock()
        .expect("hook mutex")
        .push(FinalizeSwapHook {
            device: scratch_identity.device,
            inode: scratch_identity.inode,
            target: DISCARD_PENDING_FILE,
            action: Box::new(move || {
                std::fs::rename(&hook_source, &moved_public).expect("swap checked public");
                std::fs::write(&hook_source, b"newer public winner").expect("new winner");
            }),
        });

    let error = finalize_committed_original(&paths, &mut context)
        .await
        .expect_err("interval swap must retain proof");
    assert!(error.contains("public replacement after original deletion"));
    assert!(
        !child_exists(&context.scratch.directory, DISCARD_PENDING_FILE)
            .await
            .expect("original quarantine was finalized")
    );
    assert_eq!(
        tokio::fs::read(paths.directory.join(PUBLIC_PROOF_FILE))
            .await
            .expect("replacement proof survives"),
        b"profile eight"
    );
    assert_eq!(
        tokio::fs::read(&source).await.expect("winner survives"),
        b"newer public winner"
    );
    assert!(!FINALIZE_SWAP_HOOKS
        .lock()
        .expect("hook mutex")
        .iter()
        .any(|hook| hook.device == scratch_identity.device
            && hook.inode == scratch_identity.inode
            && hook.target == DISCARD_PENDING_FILE));
}

#[tokio::test]
async fn discard_pending_post_fstat_swap_preserves_both_originals() {
    let root = crate::test_tempdir().expect("discard-pending interval root");
    let scratch_path = root.path().join("scratch");
    tokio::fs::create_dir(&scratch_path).await.expect("scratch");
    let public_path = root.path().join("movie.mkv");
    let pending_path = scratch_path.join(DISCARD_PENDING_FILE);
    let moved_path = scratch_path.join("checked-original-moved");
    tokio::fs::write(&public_path, b"profile eight")
        .await
        .expect("public");
    tokio::fs::write(&pending_path, b"manifest original")
        .await
        .expect("pending");
    let parent = SecureDirectory::open(root.path()).await.expect("parent");
    let scratch = SecureDirectory::open(&scratch_path).await.expect("scratch");
    let scratch_identity = scratch.identity().await.expect("scratch identity");
    let expected_pending = child_identity(&scratch, DISCARD_PENDING_FILE)
        .await
        .expect("pending identity");
    let expected_public = child_identity(&parent, "movie.mkv")
        .await
        .expect("public identity");
    let hook_pending = pending_path.clone();
    let hook_moved = moved_path.clone();
    FINALIZE_SWAP_HOOKS
        .lock()
        .expect("hook mutex")
        .push(FinalizeSwapHook {
            device: scratch_identity.device,
            inode: scratch_identity.inode,
            target: DISCARD_PENDING_FILE,
            action: Box::new(move || {
                std::fs::rename(&hook_pending, &hook_moved).expect("move checked original");
                std::fs::write(&hook_pending, b"new pending winner").expect("new pending winner");
            }),
        });

    let error = unlink_original_if_public_matches(
        &scratch,
        DISCARD_PENDING_FILE,
        &expected_pending,
        &parent,
        "movie.mkv",
        &expected_public,
    )
    .await
    .expect_err("post-fstat pending swap must fail closed");
    assert!(error.contains("expected local identity"), "{error}");
    assert_eq!(
        tokio::fs::read(&pending_path)
            .await
            .expect("winner survives"),
        b"new pending winner"
    );
    assert_eq!(
        tokio::fs::read(&moved_path)
            .await
            .expect("manifest original survives"),
        b"manifest original"
    );
    assert_eq!(
        tokio::fs::read(&public_path)
            .await
            .expect("public survives"),
        b"profile eight"
    );
}

#[tokio::test]
async fn verified_finalization_retains_the_tracked_scratch_recovery_guard() {
    let root = crate::test_tempdir().expect("tracked recovery guard root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"original profile seven")
        .await
        .expect("source");
    let file = media_file(source.clone());
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("scratch");
    let bytes_after = bind_test_replacement(&paths, &file, b"profile eight", false).await;
    let mut context = publication_context(&paths, &file, bytes_after)
        .await
        .expect("context");
    stage_and_publish_without_probe(&mut context)
        .await
        .expect("publish");
    finalize_committed_original(&paths, &mut context)
        .await
        .expect("verified finalization");
    assert!(
        !child_exists(&context.scratch.directory, DISCARD_PENDING_FILE)
            .await
            .expect("original quarantine was finalized")
    );
    assert!(child_exists(&context.scratch.directory, PUBLIC_PROOF_FILE)
        .await
        .expect("tracked guard remains"));
    assert_eq!(
        tokio::fs::read(paths.directory.join(PUBLIC_PROOF_FILE))
            .await
            .expect("tracked guard survives"),
        b"profile eight"
    );
    let intent = recovery_guard_intent(&file)
        .await
        .expect("guard intent")
        .expect("discard policy has guard");
    assert_eq!(
        intent.recovery_path,
        paths
            .directory
            .join(PUBLIC_PROOF_FILE)
            .to_str()
            .expect("UTF-8 recovery guard path")
    );
}

#[tokio::test]
async fn orphan_guard_cleanup_converges_through_guard_then_scratch() {
    let root = crate::test_tempdir().expect("orphan recovery guard root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"original profile seven")
        .await
        .expect("source");
    let file = media_file(source.clone());
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("scratch");
    let bytes_after = bind_test_replacement(&paths, &file, b"profile eight", false).await;
    let mut context = publication_context(&paths, &file, bytes_after)
        .await
        .expect("context");
    stage_and_publish_without_probe(&mut context)
        .await
        .expect("publish");
    finalize_committed_original(&paths, &mut context)
        .await
        .expect("finalize original");
    let intent = recovery_guard_intent(&file)
        .await
        .expect("guard intent")
        .expect("discard guard");
    let source_path = source.to_str().expect("utf8 source");
    let loss = CancellationToken::new();

    remove_orphan_recovery_guard(
        file.id,
        source_path,
        &intent.recovery_path,
        &intent.guard_id,
        TEST_NODE_ID,
        &loss,
    )
    .await
    .expect("remove orphan guard");
    assert!(!path_entry_exists(Path::new(&intent.recovery_path))
        .await
        .expect("guard absent"));
    let parent = SecureDirectory::open(root.path())
        .await
        .expect("witness parent");
    assert_eq!(
        read_recovery_guard_witness(&parent, file.id, source_path, &intent.guard_id)
            .await
            .expect("guard witness")
            .expect("attested guard witness")
            .state,
        RecoveryGuardWitnessState::GuardAttested
    );
    remove_orphan_recovery_scratch(file.id, source_path, &intent.guard_id, &loss)
        .await
        .expect("remove orphan scratch");
    assert!(!path_entry_exists(&paths.directory)
        .await
        .expect("active scratch absent"));
    assert!(
        !path_entry_exists(root.path().join(&paths.cleanup_name).as_path())
            .await
            .expect("cleanup scratch absent")
    );
    assert_eq!(
        read_recovery_guard_witness(&parent, file.id, source_path, &intent.guard_id)
            .await
            .expect("terminal witness")
            .expect("retained tombstone")
            .state,
        RecoveryGuardWitnessState::ScratchRemoved,
        "the tiny terminal tombstone outlives the large recovery inode"
    );
    attest_orphan_recovery_tombstone(file.id, source_path, &intent.guard_id)
        .await
        .expect("terminal tombstone attestation");
}

#[tokio::test]
async fn orphan_guard_cleanup_converges_when_intent_precedes_proof_creation() {
    let root = crate::test_tempdir().expect("unmaterialized orphan guard root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"original profile seven")
        .await
        .expect("source");
    let file = media_file(source.clone());
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("scratch");
    bind_test_replacement(&paths, &file, b"profile eight", false).await;
    let intent = recovery_guard_intent(&file)
        .await
        .expect("guard intent")
        .expect("discard guard");
    let source_path = source.to_str().expect("utf8 source");
    let loss = CancellationToken::new();

    let scratch = require_recovery_guard_scratch(&paths, file.id, source_path, &intent.guard_id)
        .await
        .expect("owned unmaterialized scratch");
    assert!(!logical_child_exists(&scratch.directory, PUBLIC_PROOF_FILE)
        .await
        .expect("proof absent"));
    assert!(
        !logical_child_exists(&scratch.directory, "source.p7.original")
            .await
            .expect("staged original absent")
    );
    assert!(
        !logical_child_exists(&scratch.directory, DISCARD_PENDING_FILE)
            .await
            .expect("pending original absent")
    );

    remove_orphan_recovery_guard(
        file.id,
        source_path,
        &intent.recovery_path,
        &intent.guard_id,
        TEST_NODE_ID,
        &loss,
    )
    .await
    .expect("attest unmaterialized guard removal");
    let parent = SecureDirectory::open(root.path())
        .await
        .expect("witness parent");
    assert_eq!(
        read_recovery_guard_witness(&parent, file.id, source_path, &intent.guard_id)
            .await
            .expect("guard witness")
            .expect("attested guard witness")
            .state,
        RecoveryGuardWitnessState::GuardAttested
    );
    assert_eq!(
        tokio::fs::read(&source).await.expect("source survives"),
        b"original profile seven"
    );

    remove_orphan_recovery_scratch(file.id, source_path, &intent.guard_id, &loss)
        .await
        .expect("remove unmaterialized orphan scratch");
    assert!(!path_entry_exists(&paths.directory)
        .await
        .expect("scratch absent"));
    assert_eq!(
        read_recovery_guard_witness(&parent, file.id, source_path, &intent.guard_id)
            .await
            .expect("terminal witness")
            .expect("retained tombstone")
            .state,
        RecoveryGuardWitnessState::ScratchRemoved
    );
    attest_orphan_recovery_tombstone(file.id, source_path, &intent.guard_id)
        .await
        .expect("terminal tombstone attestation");
}

async fn assert_unmaterialized_guard_preserves_original_child(original_child: &str) {
    let root = crate::test_tempdir().expect("unmaterialized guard refusal root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"original profile seven")
        .await
        .expect("source");
    let file = media_file(source.clone());
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("scratch");
    bind_test_replacement(&paths, &file, b"profile eight", false).await;
    let intent = recovery_guard_intent(&file)
        .await
        .expect("guard intent")
        .expect("discard guard");
    let scratch = require_recovery_guard_scratch(
        &paths,
        file.id,
        source.to_str().expect("utf8 source"),
        &intent.guard_id,
    )
    .await
    .expect("owned scratch");
    let before_owner = scratch
        .directory
        .read_bounded_child(SCRATCH_OWNER_FILE, MAX_SCRATCH_OWNER_BYTES)
        .await
        .expect("owner bytes before refusal");
    assert!(rename_expected_noreplace_between(
        &SecureDirectory::open(root.path())
            .await
            .expect("source parent"),
        "movie.mkv",
        &scratch.owner.source.local.identity,
        &scratch.directory,
        original_child,
    )
    .await
    .expect("stage original-bearing child")
    .is_some());

    let error = remove_orphan_recovery_guard(
        file.id,
        source.to_str().expect("utf8 source"),
        &intent.recovery_path,
        &intent.guard_id,
        TEST_NODE_ID,
        &CancellationToken::new(),
    )
    .await
    .expect_err("an original-bearing scratch must fail closed");
    assert!(error.contains("scratch retains an original"), "{error}");
    assert_eq!(
        tokio::fs::read(paths.directory.join(original_child))
            .await
            .expect("original survives"),
        b"original profile seven"
    );
    assert_eq!(
        tokio::fs::read(&paths.replacement)
            .await
            .expect("replacement survives"),
        b"profile eight"
    );
    assert_eq!(
        scratch
            .directory
            .read_bounded_child(SCRATCH_OWNER_FILE, MAX_SCRATCH_OWNER_BYTES)
            .await
            .expect("owner bytes after refusal"),
        before_owner
    );
    assert!(
        read_recovery_guard_witness(
            &SecureDirectory::open(root.path()).await.expect("parent"),
            file.id,
            source.to_str().expect("utf8 source"),
            &intent.guard_id,
        )
        .await
        .expect("witness read")
        .is_none(),
        "refusal must not advance the filesystem side of the guard ledger"
    );
}

#[tokio::test]
async fn unmaterialized_guard_refuses_a_staged_original() {
    assert_unmaterialized_guard_preserves_original_child("source.p7.original").await;
}

#[tokio::test]
async fn unmaterialized_guard_refuses_a_discard_pending_original() {
    assert_unmaterialized_guard_preserves_original_child(DISCARD_PENDING_FILE).await;
}

#[tokio::test]
async fn orphan_guard_cleanup_recovers_a_crash_after_guard_quarantine() {
    let root = crate::test_tempdir().expect("orphan guard quarantine root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"original profile seven")
        .await
        .expect("source");
    let file = media_file(source.clone());
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("scratch");
    let bytes_after = bind_test_replacement(&paths, &file, b"profile eight", false).await;
    let mut context = publication_context(&paths, &file, bytes_after)
        .await
        .expect("context");
    stage_and_publish_without_probe(&mut context)
        .await
        .expect("publish");
    finalize_committed_original(&paths, &mut context)
        .await
        .expect("finalize original");
    let intent = recovery_guard_intent(&file)
        .await
        .expect("guard intent")
        .expect("discard guard");
    let guard_quarantine = delete_quarantine_name(PUBLIC_PROOF_FILE);
    assert!(context
        .scratch
        .directory
        .rename_child_noreplace(PUBLIC_PROOF_FILE, &guard_quarantine)
        .await
        .expect("quarantine guard"));
    sync_directory(&paths.directory)
        .await
        .expect("durable guard quarantine");

    remove_orphan_recovery_guard(
        file.id,
        source.to_str().expect("utf8 source"),
        &intent.recovery_path,
        &intent.guard_id,
        TEST_NODE_ID,
        &CancellationToken::new(),
    )
    .await
    .expect("recover and remove orphan guard");
    assert!(!child_exists(&context.scratch.directory, PUBLIC_PROOF_FILE)
        .await
        .expect("guard absent"));
    assert!(!child_exists(&context.scratch.directory, &guard_quarantine)
        .await
        .expect("guard quarantine absent"));
}

#[tokio::test]
async fn orphan_guard_cleanup_recovers_a_crash_inside_the_private_anchor() {
    let root = crate::test_tempdir().expect("orphan guard private-anchor root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"original profile seven")
        .await
        .expect("source");
    let file = media_file(source.clone());
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("scratch");
    let bytes_after = bind_test_replacement(&paths, &file, b"profile eight", false).await;
    let mut context = publication_context(&paths, &file, bytes_after)
        .await
        .expect("context");
    stage_and_publish_without_probe(&mut context)
        .await
        .expect("publish");
    finalize_committed_original(&paths, &mut context)
        .await
        .expect("finalize original");
    let intent = recovery_guard_intent(&file)
        .await
        .expect("guard intent")
        .expect("discard guard");
    let guard = child_identity(&context.scratch.directory, PUBLIC_PROOF_FILE)
        .await
        .expect("guard identity");
    let anchor = context
        .scratch
        .directory
        .create_child_directory(PRIVATE_DELETE_ANCHOR)
        .await
        .expect("private anchor");
    let slot = private_delete_slot(PUBLIC_PROOF_FILE, PrivateDeleteKind::File);
    assert!(rename_expected_noreplace_between(
        &context.scratch.directory,
        PUBLIC_PROOF_FILE,
        &guard,
        &anchor,
        &slot,
    )
    .await
    .expect("simulate crash after guard enters anchor")
    .is_some());

    remove_orphan_recovery_guard(
        file.id,
        source.to_str().expect("utf8 source"),
        &intent.recovery_path,
        &intent.guard_id,
        TEST_NODE_ID,
        &CancellationToken::new(),
    )
    .await
    .expect("successor retires anchored guard");
    assert!(!child_exists(&anchor, &slot)
        .await
        .expect("private slot absent"));
    assert!(read_recovery_guard_witness(
        &SecureDirectory::open(root.path()).await.expect("parent"),
        file.id,
        source.to_str().expect("utf8 source"),
        &intent.guard_id,
    )
    .await
    .expect("witness")
    .is_some());
}

#[cfg(unix)]
#[tokio::test]
async fn orphan_scratch_cleanup_recovers_a_crash_inside_the_private_anchor() {
    let root = crate::test_tempdir().expect("orphan scratch private-anchor root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"original profile seven")
        .await
        .expect("source");
    let file = media_file(source.clone());
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("scratch");
    let bytes_after = bind_test_replacement(&paths, &file, b"profile eight", false).await;
    let mut context = publication_context(&paths, &file, bytes_after)
        .await
        .expect("context");
    stage_and_publish_without_probe(&mut context)
        .await
        .expect("publish");
    finalize_committed_original(&paths, &mut context)
        .await
        .expect("finalize original");
    let intent = recovery_guard_intent(&file)
        .await
        .expect("guard intent")
        .expect("discard guard");
    let loss = CancellationToken::new();
    remove_orphan_recovery_guard(
        file.id,
        source.to_str().expect("utf8 source"),
        &intent.recovery_path,
        &intent.guard_id,
        TEST_NODE_ID,
        &loss,
    )
    .await
    .expect("remove guard before scratch phase");
    let parent = SecureDirectory::open(root.path()).await.expect("parent");
    assert!(parent
        .rename_child_noreplace(&paths.directory_name, &paths.cleanup_name)
        .await
        .expect("quarantine scratch"));
    let cleanup_path = root.path().join(&paths.cleanup_name);
    tokio::fs::remove_file(cleanup_path.join(SCRATCH_OWNER_FILE))
        .await
        .expect("worker A removed final marker");
    tokio::fs::remove_dir(cleanup_path.join(PRIVATE_DELETE_ANCHOR))
        .await
        .expect("worker A removed empty inner anchor");
    let anchor = parent
        .create_child_directory(PRIVATE_DELETE_ANCHOR)
        .await
        .expect("parent private anchor");
    let slot = private_delete_slot(&paths.cleanup_name, PrivateDeleteKind::Directory);
    let cleanup_name = CString::new(paths.cleanup_name.clone()).expect("cleanup name");
    let slot_name = CString::new(slot.clone()).expect("private slot name");
    assert!(
        rename_noreplace_raw(parent.raw_fd(), &cleanup_name, anchor.raw_fd(), &slot_name,)
            .expect("simulate worker A crash after private root move")
    );
    sync_rename_parents(&parent, &anchor).expect("durable private root move");

    remove_orphan_recovery_scratch(
        file.id,
        source.to_str().expect("utf8 source"),
        &intent.guard_id,
        &loss,
    )
    .await
    .expect("worker B retires anchored empty root");
    assert!(!child_exists(&anchor, &slot)
        .await
        .expect("root slot absent"));
    assert_eq!(
        read_recovery_guard_witness(
            &parent,
            file.id,
            source.to_str().expect("utf8 source"),
            &intent.guard_id,
        )
        .await
        .expect("terminal witness")
        .expect("witness exists")
        .state,
        RecoveryGuardWitnessState::ScratchRemoved
    );
}

#[tokio::test]
async fn orphan_scratch_cleanup_recovers_a_crash_after_owner_quarantine() {
    let root = crate::test_tempdir().expect("orphan owner quarantine root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"original profile seven")
        .await
        .expect("source");
    let file = media_file(source.clone());
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("scratch");
    let bytes_after = bind_test_replacement(&paths, &file, b"profile eight", false).await;
    let mut context = publication_context(&paths, &file, bytes_after)
        .await
        .expect("context");
    stage_and_publish_without_probe(&mut context)
        .await
        .expect("publish");
    finalize_committed_original(&paths, &mut context)
        .await
        .expect("finalize original");
    let intent = recovery_guard_intent(&file)
        .await
        .expect("guard intent")
        .expect("discard guard");
    let source_path = source.to_str().expect("utf8 source");
    let loss = CancellationToken::new();
    remove_orphan_recovery_guard(
        file.id,
        source_path,
        &intent.recovery_path,
        &intent.guard_id,
        TEST_NODE_ID,
        &loss,
    )
    .await
    .expect("remove orphan guard");

    let parent = SecureDirectory::open(root.path())
        .await
        .expect("secure source parent");
    assert!(parent
        .rename_child_noreplace(&paths.directory_name, &paths.cleanup_name)
        .await
        .expect("quarantine scratch"));
    sync_directory(root.path())
        .await
        .expect("durable scratch quarantine");
    let cleanup_path = root.path().join(&paths.cleanup_name);
    let cleanup = SecureDirectory::open(&cleanup_path)
        .await
        .expect("cleanup scratch");
    let owner_quarantine = delete_quarantine_name(SCRATCH_OWNER_FILE);
    assert!(cleanup
        .rename_child_noreplace(SCRATCH_OWNER_FILE, &owner_quarantine)
        .await
        .expect("quarantine owner marker"));
    sync_directory(&cleanup_path)
        .await
        .expect("durable owner quarantine");

    remove_orphan_recovery_scratch(file.id, source_path, &intent.guard_id, &loss)
        .await
        .expect("recover marker and remove scratch");
    assert!(!path_entry_exists(&cleanup_path)
        .await
        .expect("cleanup scratch absent"));
}

#[tokio::test]
async fn durable_guard_restores_a_missing_public_link_without_clobbering() {
    let root = crate::test_tempdir().expect("guard restoration root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"original profile seven")
        .await
        .expect("source");
    let file = media_file(source.clone());
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("scratch");
    let bytes_after = bind_test_replacement(&paths, &file, b"profile eight", false).await;
    let mut context = publication_context(&paths, &file, bytes_after)
        .await
        .expect("context");
    stage_and_publish_without_probe(&mut context)
        .await
        .expect("publish");
    finalize_committed_original(&paths, &mut context)
        .await
        .expect("finalize original");
    let moved = root.path().join("published-moved-away");
    tokio::fs::rename(&source, &moved)
        .await
        .expect("remove public name");
    let scratch = context.scratch.directory.clone();
    let guard = context
        .bind_replacement(
            &scratch,
            PUBLIC_PROOF_FILE,
            &CancellationToken::new(),
            "test recovery guard",
        )
        .await
        .expect("guard");
    let restored = restore_public_from_recovery_guard(
        &scratch,
        &guard.local,
        &context.source_parent,
        &context.source_name,
    )
    .await
    .expect("restore public link");
    assert_eq!(restored.device, guard.local.device);
    assert_eq!(restored.inode, guard.local.inode);
    assert_eq!(
        tokio::fs::read(&source).await.expect("restored bytes"),
        b"profile eight"
    );
    assert!(path_entry_exists(&paths.directory.join(PUBLIC_PROOF_FILE))
        .await
        .expect("guard remains"));
}

#[tokio::test]
async fn guard_restoration_preserves_a_winner_swapped_after_link_creation() {
    let root = crate::test_tempdir().expect("guard restoration race root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"original profile seven")
        .await
        .expect("source");
    let file = media_file(source.clone());
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("scratch");
    let bytes_after = bind_test_replacement(&paths, &file, b"profile eight", false).await;
    let mut context = publication_context(&paths, &file, bytes_after)
        .await
        .expect("context");
    stage_and_publish_without_probe(&mut context)
        .await
        .expect("publish");
    finalize_committed_original(&paths, &mut context)
        .await
        .expect("finalize original");
    tokio::fs::remove_file(&source)
        .await
        .expect("remove public link");
    let scratch = context.scratch.directory.clone();
    let guard = context
        .bind_replacement(
            &scratch,
            PUBLIC_PROOF_FILE,
            &CancellationToken::new(),
            "raced recovery guard",
        )
        .await
        .expect("guard");
    let scratch_identity = scratch.identity().await.expect("scratch identity");
    let moved = root.path().join("created-recovery-link-moved");
    let hook_source = source.clone();
    let hook_moved = moved.clone();
    *PUBLIC_RESTORE_SWAP_HOOK
        .lock()
        .expect("public restore hook mutex") = Some(PublicRestoreSwapHook {
        device: scratch_identity.device,
        inode: scratch_identity.inode,
        action: Box::new(move || {
            std::fs::rename(&hook_source, &hook_moved).expect("move just-created recovery link");
            std::fs::write(&hook_source, b"new public winner")
                .expect("install newer public winner");
        }),
    });

    let error = restore_public_from_recovery_guard(
        &scratch,
        &guard.local,
        &context.source_parent,
        &context.source_name,
    )
    .await
    .expect_err("post-link public swap must fail closed");
    assert!(error.contains("preserving the observed winner"));
    assert_eq!(
        tokio::fs::read(&source).await.expect("winner survives"),
        b"new public winner"
    );
    assert_eq!(
        tokio::fs::read(&moved)
            .await
            .expect("created link survives"),
        b"profile eight"
    );
    assert_eq!(
        tokio::fs::read(paths.directory.join(PUBLIC_PROOF_FILE))
            .await
            .expect("guard survives"),
        b"profile eight"
    );
    assert!(PUBLIC_RESTORE_SWAP_HOOK
        .lock()
        .expect("public restore hook mutex")
        .is_none());
}

#[tokio::test]
async fn poisoned_public_proof_is_removed_and_a_restored_public_path_can_retry() {
    let root = crate::test_tempdir().expect("proof creation poison root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"original profile seven")
        .await
        .expect("source");
    let file = media_file(source.clone());
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("scratch");
    let bytes_after = bind_test_replacement(&paths, &file, b"profile eight", false).await;
    let mut context = publication_context(&paths, &file, bytes_after)
        .await
        .expect("context");
    let scratch_identity = context
        .scratch
        .directory
        .identity()
        .await
        .expect("scratch identity");
    let moved_replacement = paths
        .directory
        .join("verified-replacement-held-proof-create");
    let hook_source = paths.replacement.clone();
    let hook_moved = moved_replacement.clone();
    *PROOF_CREATION_SWAP_HOOK
        .lock()
        .expect("proof creation hook mutex") = Some(ProofCreationSwapHook {
        device: scratch_identity.device,
        inode: scratch_identity.inode,
        action: Box::new(move || {
            std::fs::rename(&hook_source, &hook_moved).expect("move checked replacement");
            std::fs::write(&hook_source, b"poisoned inode").expect("poison replacement");
        }),
    });
    let scratch = context.scratch.directory.clone();
    let replacement = context
        .bind_replacement(
            &scratch,
            "replacement.mkv",
            &CancellationToken::new(),
            "guard source",
        )
        .await
        .expect("replacement");
    let error =
        ensure_public_replacement_proof(&scratch, "replacement.mkv", &replacement.local, &scratch)
            .await
            .expect_err("swapped proof creation must fail");
    assert!(error.contains("public replacement changed"));
    assert!(!child_exists(&context.scratch.directory, PUBLIC_PROOF_FILE)
        .await
        .expect("poison proof removed"));
    assert!(path_entry_exists(&source)
        .await
        .expect("original remains durable"));
    tokio::fs::remove_file(&paths.replacement)
        .await
        .expect("remove poison replacement");
    tokio::fs::rename(&moved_replacement, &paths.replacement)
        .await
        .expect("restore verified replacement");
    stage_and_publish_without_probe(&mut context)
        .await
        .expect("restored replacement publishes");
    finalize_committed_original(&paths, &mut context)
        .await
        .expect("restored public retries successfully");
    assert!(path_entry_exists(&paths.directory.join(PUBLIC_PROOF_FILE))
        .await
        .expect("recovery guard exists"));
    assert!(PROOF_CREATION_SWAP_HOOK
        .lock()
        .expect("proof creation hook mutex")
        .is_none());
}

#[tokio::test]
async fn unrelated_preexisting_guard_is_refused_before_original_staging() {
    let root = crate::test_tempdir().expect("guard collision root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"original profile seven")
        .await
        .expect("source");
    let file = media_file(source.clone());
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("scratch");
    let bytes_after = bind_test_replacement(&paths, &file, b"profile eight", false).await;
    tokio::fs::write(paths.directory.join(PUBLIC_PROOF_FILE), b"unrelated guard")
        .await
        .expect("unrelated guard");
    let mut context = publication_context(&paths, &file, bytes_after)
        .await
        .expect("context");
    let scratch = context.scratch.directory.clone();
    let replacement = context
        .bind_replacement(
            &scratch,
            "replacement.mkv",
            &CancellationToken::new(),
            "guard collision replacement",
        )
        .await
        .expect("replacement");
    let error =
        ensure_public_replacement_proof(&scratch, "replacement.mkv", &replacement.local, &scratch)
            .await
            .expect_err("unrelated guard refused");
    assert!(error.contains("unrelated pre-existing"));
    assert_eq!(
        tokio::fs::read(&source).await.expect("original remains"),
        b"original profile seven"
    );
    assert_eq!(
        tokio::fs::read(paths.directory.join(PUBLIC_PROOF_FILE))
            .await
            .expect("unrelated guard remains"),
        b"unrelated guard"
    );
}

#[tokio::test]
async fn persisted_keep_true_survives_a_live_false_policy_flip() {
    let root = crate::test_tempdir().expect("keep root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"original profile seven")
        .await
        .expect("source");
    let file = media_file(source.clone());
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("scratch");
    let bytes_after = bind_test_replacement(&paths, &file, b"profile eight", true).await;
    assert!(rename_noreplace_durable(&source, &paths.staged_original)
        .await
        .expect("stage"));
    assert!(rename_noreplace_durable(&paths.replacement, &source)
        .await
        .expect("publish"));
    let mut context = publication_context(&paths, &file, bytes_after)
        .await
        .expect("manifest policy");
    assert!(
        context.keep_original,
        "live false must not replace persisted true"
    );
    assert!(
        retain_original_before_commit(&paths, &mut context, &CancellationToken::new(),)
            .await
            .expect("pre-commit retain")
            .is_some()
    );
    assert!(finalize_committed_original(&paths, &mut context)
        .await
        .expect("post-commit validate")
        .is_some());
    assert!(path_entry_exists(&paths.retained_original)
        .await
        .expect("retained"));
}

#[tokio::test]
async fn persisted_keep_false_survives_a_live_true_policy_flip() {
    let root = crate::test_tempdir().expect("discard root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"original profile seven")
        .await
        .expect("source");
    let file = media_file(source.clone());
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("scratch");
    let bytes_after = bind_test_replacement(&paths, &file, b"profile eight", false).await;
    let mut context = publication_context(&paths, &file, bytes_after)
        .await
        .expect("manifest policy");
    stage_and_publish_without_probe(&mut context)
        .await
        .expect("publish");
    assert!(
        !context.keep_original,
        "live true must not replace persisted false"
    );
    assert!(finalize_committed_original(&paths, &mut context)
        .await
        .expect("finalize")
        .is_none());
    assert!(!path_entry_exists(&paths.staged_original)
        .await
        .expect("staged gone"));
    assert!(!path_entry_exists(&paths.retained_original)
        .await
        .expect("not retained"));
}

#[tokio::test]
async fn deterministic_cleanup_quarantine_is_reconciled_on_retry() {
    let root = crate::test_tempdir().expect("cleanup root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"source").await.expect("source");
    let file = media_file(source);
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("scratch");
    tokio::fs::write(&paths.raw, vec![b'x'; 1024])
        .await
        .expect("large child");
    let parent = open_parent(&paths.directory, "scratch")
        .await
        .expect("parent");
    assert!(parent
        .rename_child_noreplace(&paths.directory_name, &paths.cleanup_name)
        .await
        .expect("quarantine"));
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("retry reconciles quarantine");
    assert!(!path_entry_exists(&root.path().join(&paths.cleanup_name))
        .await
        .expect("cleanup gone"));
    assert!(path_entry_exists(&paths.directory)
        .await
        .expect("fresh scratch"));
}

#[tokio::test]
async fn private_cleanup_quarantine_converges_after_marker_removal_crash() {
    let root = crate::test_tempdir().expect("private cleanup crash root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"source").await.expect("source");
    let file = media_file(source);
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("scratch");
    let parent = open_parent(&paths.directory, "scratch")
        .await
        .expect("parent");
    let scratch = require_owned_scratch(&paths, &file, None)
        .await
        .expect("owned scratch");
    let expected = scratch.directory.identity().await.expect("root identity");
    let private_name = format!("{}.{}", paths.cleanup_name, uuid::Uuid::new_v4());
    let state = CleanupQuarantineState::new(&file, private_name.clone(), expected);
    write_cleanup_state(&parent, &paths, &state)
        .await
        .expect("cleanup intent");
    assert!(parent
        .rename_child_noreplace(&paths.directory_name, &private_name)
        .await
        .expect("private rename"));
    let private = parent
        .open_child_directory(&private_name)
        .await
        .expect("private root");
    let marker = child_identity(&private, SCRATCH_OWNER_FILE)
        .await
        .expect("marker identity");
    unlink_expected_child(&private, SCRATCH_OWNER_FILE, &marker)
        .await
        .expect("simulate crash after marker cleanup");

    reconcile_cleanup_quarantine(&paths, &file)
        .await
        .expect("sidecar-owned retry");
    assert!(!child_exists(&parent, &private_name)
        .await
        .expect("private root gone"));
    assert!(!child_exists(&parent, &paths.cleanup_state_name)
        .await
        .expect("sidecar gone"));
}

#[tokio::test]
async fn two_cleanup_workers_cannot_overwrite_the_first_workers_sidecar_uuid() {
    let root = crate::test_tempdir().expect("two-worker cleanup root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"source").await.expect("source");
    let file = media_file(source);
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("scratch");
    let parent = open_parent(&paths.directory, "scratch")
        .await
        .expect("parent");
    let scratch = require_owned_scratch(&paths, &file, None)
        .await
        .expect("owned scratch");
    let root_identity = scratch.directory.identity().await.expect("root identity");
    let worker_a_name = format!("{}.{}", paths.cleanup_name, uuid::Uuid::new_v4());
    let worker_a = CleanupQuarantineState::new(&file, worker_a_name.clone(), root_identity);
    create_cleanup_state(&parent, &paths, &worker_a)
        .await
        .expect("worker A sidecar");
    assert!(parent
        .rename_child_noreplace(&paths.directory_name, &worker_a_name)
        .await
        .expect("worker A rename before crash"));

    // Failpoint: A has crashed after its rename while B (which would only
    // be possible without the shared file lease) attempts a new UUID.
    let worker_b_name = format!("{}.{}", paths.cleanup_name, uuid::Uuid::new_v4());
    let worker_b = CleanupQuarantineState::new(&file, worker_b_name.clone(), root_identity);
    assert!(create_cleanup_state(&parent, &paths, &worker_b)
        .await
        .expect_err("worker B must not overwrite A")
        .contains("already belongs to another worker"));
    let persisted: CleanupQuarantineState = serde_json::from_slice(
        &parent
            .read_bounded_child(&paths.cleanup_state_name, MAX_CLEANUP_STATE_BYTES)
            .await
            .expect("persisted A sidecar"),
    )
    .expect("decode sidecar");
    assert_eq!(persisted.quarantine_name, worker_a_name);
    assert!(child_exists(&parent, &worker_a_name)
        .await
        .expect("A quarantine survives"));
    assert!(!child_exists(&parent, &worker_b_name)
        .await
        .expect("B quarantine absent"));
}

#[tokio::test]
async fn lease_loss_after_cleanup_intent_cannot_mutate_successor_cleanup() {
    let root = crate::test_tempdir().expect("cleanup lease-loss root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"source").await.expect("source");
    let file = media_file(source);
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("scratch");
    let scratch = require_owned_scratch(&paths, &file, None)
        .await
        .expect("owned scratch");
    let root_identity = scratch.directory.identity().await.expect("root identity");
    let stale_loss = CancellationToken::new();
    let hook_loss = stale_loss.clone();
    *CLEANUP_INTENT_HOOK
        .lock()
        .expect("cleanup intent hook mutex") = Some(CleanupIntentHook {
        device: root_identity.device,
        inode: root_identity.inode,
        action: Box::new(move || hook_loss.cancel()),
    });

    let error = remove_owned_scratch(&paths, &file, None, &stale_loss)
        .await
        .expect_err("stale cleanup must stop after intent");
    assert!(error.contains("lease was lost before cleanup quarantine rename"));
    assert!(path_entry_exists(&paths.directory)
        .await
        .expect("stale worker left active root"));
    assert!(path_entry_exists(
        &paths
            .directory
            .parent()
            .expect("scratch parent")
            .join(&paths.cleanup_state_name),
    )
    .await
    .expect("stale intent is durable"));

    let successor_loss = CancellationToken::new();
    remove_owned_scratch(&paths, &file, None, &successor_loss)
        .await
        .expect("successor reconciles and cleans");
    assert!(!path_entry_exists(&paths.directory)
        .await
        .expect("successor removed root"));
    assert!(!path_entry_exists(
        &paths
            .directory
            .parent()
            .expect("scratch parent")
            .join(&paths.cleanup_state_name),
    )
    .await
    .expect("successor removed intent"));
    assert!(CLEANUP_INTENT_HOOK
        .lock()
        .expect("cleanup intent hook mutex")
        .is_none());
}

#[tokio::test]
async fn private_cleanup_quarantine_preserves_a_swapped_root() {
    let root = crate::test_tempdir().expect("private cleanup race root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"source").await.expect("source");
    let file = media_file(source);
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("scratch");
    let parent = open_parent(&paths.directory, "scratch")
        .await
        .expect("parent");
    let scratch = require_owned_scratch(&paths, &file, None)
        .await
        .expect("owned scratch");
    let expected = scratch.directory.identity().await.expect("root identity");
    let private_name = format!("{}.{}", paths.cleanup_name, uuid::Uuid::new_v4());
    let state = CleanupQuarantineState::new(&file, private_name.clone(), expected);
    write_cleanup_state(&parent, &paths, &state)
        .await
        .expect("cleanup intent");
    assert!(parent
        .rename_child_noreplace(&paths.directory_name, &private_name)
        .await
        .expect("private rename"));
    let moved_name = format!("{private_name}.moved");
    assert!(parent
        .rename_child_noreplace(&private_name, &moved_name)
        .await
        .expect("move owned root"));
    tokio::fs::create_dir(root.path().join(&private_name))
        .await
        .expect("swapped winner root");
    tokio::fs::write(root.path().join(&private_name).join("winner"), b"winner")
        .await
        .expect("winner child");

    assert!(reconcile_cleanup_quarantine(&paths, &file)
        .await
        .expect_err("swapped root refused")
        .contains("unowned conversion scratch"));
    assert_eq!(
        tokio::fs::read(root.path().join(&private_name).join("winner"))
            .await
            .expect("winner survives"),
        b"winner"
    );
    assert!(child_exists(&parent, &moved_name)
        .await
        .expect("owned root preserved"));
}

#[tokio::test]
async fn private_cleanup_recovers_an_interrupted_sidecar_unlink() {
    let root = crate::test_tempdir().expect("sidecar cleanup crash root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"source").await.expect("source");
    let file = media_file(source);
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("scratch");
    let parent = open_parent(&paths.directory, "scratch")
        .await
        .expect("parent");
    let scratch = require_owned_scratch(&paths, &file, None)
        .await
        .expect("owned scratch");
    let expected = scratch.directory.identity().await.expect("root identity");
    let private_name = format!("{}.{}", paths.cleanup_name, uuid::Uuid::new_v4());
    let state = CleanupQuarantineState::new(&file, private_name.clone(), expected);
    write_cleanup_state(&parent, &paths, &state)
        .await
        .expect("cleanup intent");
    assert!(parent
        .rename_child_noreplace(&paths.directory_name, &private_name)
        .await
        .expect("private rename"));
    let private = parent
        .open_child_directory(&private_name)
        .await
        .expect("private root");
    remove_flat_tree_expected(&parent, &private_name, &private, expected)
        .await
        .expect("root removed before sidecar");
    let interrupted_name = format!(".plurx-delete-{}", paths.cleanup_state_name);
    assert!(parent
        .rename_child_noreplace(&paths.cleanup_state_name, &interrupted_name)
        .await
        .expect("simulate interrupted state unlink"));

    reconcile_cleanup_quarantine(&paths, &file)
        .await
        .expect("restore and finish state unlink");
    assert!(!child_exists(&parent, &paths.cleanup_state_name)
        .await
        .expect("state absent"));
    assert!(!child_exists(&parent, &interrupted_name)
        .await
        .expect("delete quarantine absent"));
}

#[tokio::test]
async fn committed_cleanup_reconciles_a_quarantine_without_active_scratch() {
    let root = crate::test_tempdir().expect("committed cleanup root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"published source")
        .await
        .expect("source");
    let file = media_file(source);
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("scratch");
    let parent = open_parent(&paths.directory, "scratch")
        .await
        .expect("parent");
    assert!(parent
        .rename_child_noreplace(&paths.directory_name, &paths.cleanup_name)
        .await
        .expect("crash quarantine"));
    assert!(!path_entry_exists(&paths.directory)
        .await
        .expect("active absent"));

    cleanup_after_commit(&file).await;

    assert!(!path_entry_exists(&root.path().join(&paths.cleanup_name))
        .await
        .expect("quarantine removed"));
}

#[tokio::test]
async fn committed_cleanup_lease_loss_preserves_all_recovery_artifacts() {
    let root = crate::test_tempdir().expect("cancelled cleanup root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"original profile seven")
        .await
        .expect("source");
    let file = media_file(source.clone());
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("scratch");
    let bytes_after = bind_test_replacement(&paths, &file, b"profile eight", false).await;
    let mut context = publication_context(&paths, &file, bytes_after)
        .await
        .expect("context");
    stage_and_publish_without_probe(&mut context)
        .await
        .expect("publish");
    drop(context);
    let loss = CancellationToken::new();
    loss.cancel();
    super::cleanup_after_commit(&file, TEST_NODE_ID, &loss).await;
    assert!(path_entry_exists(&paths.staged_original)
        .await
        .expect("staged original preserved"));
    assert_eq!(
        tokio::fs::read(&source)
            .await
            .expect("published replacement"),
        b"profile eight"
    );
    assert!(path_entry_exists(&paths.directory)
        .await
        .expect("scratch preserved"));
}

#[tokio::test]
async fn publication_prunes_intermediates_but_keeps_original_until_ledger_commit() {
    let root = crate::test_tempdir().expect("prune root");
    let source = root.path().join("movie.mkv");
    tokio::fs::write(&source, b"profile seven")
        .await
        .expect("source");
    let file = media_file(source.clone());
    let paths = ConversionPaths::for_file(&file).expect("paths");
    prepare_fresh_directory(&paths, &file)
        .await
        .expect("scratch");
    for artifact in [&paths.raw, &paths.converted, &paths.rpu] {
        tokio::fs::write(artifact, vec![b'x'; 1_024])
            .await
            .expect("intermediate");
    }
    let bytes_after = bind_test_replacement(&paths, &file, b"profile eight", false).await;
    let mut context = publication_context(&paths, &file, bytes_after)
        .await
        .expect("context");
    stage_and_publish_without_probe(&mut context)
        .await
        .expect("publish");
    prune_published_scratch(&context, &CancellationToken::new())
        .await
        .expect("pre-commit prune");

    let mut entries = tokio::fs::read_dir(&paths.directory)
        .await
        .expect("scratch entries");
    let mut names = Vec::new();
    while let Some(entry) = entries.next_entry().await.expect("next entry") {
        names.push(entry.file_name());
    }
    names.sort();
    assert_eq!(
        names,
        [
            OsString::from(PRIVATE_DELETE_ANCHOR),
            OsString::from(SCRATCH_OWNER_FILE),
            OsString::from(PUBLIC_PROOF_FILE),
            OsString::from("source.p7.original"),
        ]
    );

    // Simulate the fenced verified->committed transition before allowing
    // the retention policy to dispose of the original.
    finalize_committed_original(&paths, &mut context)
        .await
        .expect("post-commit discard original");
    assert!(!path_entry_exists(&paths.staged_original)
        .await
        .expect("staged original removed after commit"));
}

#[cfg(unix)]
#[test]
fn non_utf8_paths_are_refused_before_scratch_is_derived() {
    use std::os::unix::ffi::OsStringExt;

    let root = crate::test_tempdir().expect("path root");
    let valid = root.path().join("placeholder.mkv");
    std::fs::write(&valid, b"source").expect("placeholder");
    let mut file = media_file(valid);
    let mut path = root.path().to_path_buf();
    path.push(std::ffi::OsString::from_vec(vec![b'm', 0xff, b'v']));
    file.path = path;
    assert!(ConversionPaths::for_file(&file)
        .expect_err("non-UTF-8 refusal")
        .contains("not valid UTF-8"));
}

#[tokio::test]
async fn a_missing_tool_is_named_before_queue_work_can_start() {
    let missing = crate::test_temp_path(format!("missing-dovi-{}", uuid::Uuid::new_v4()));
    let tools = DvDiskTools::new(
        "ffmpeg".to_owned(),
        missing.display().to_string(),
        missing.display().to_string(),
    );
    let capabilities = probe_capabilities_with(&tools).await;
    assert!(!capabilities.available);
    assert!(capabilities
        .unavailable_reason()
        .expect("reason")
        .contains("dovi_tool"));
}
