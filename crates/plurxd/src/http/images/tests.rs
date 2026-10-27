use std::sync::atomic::{AtomicUsize, Ordering};

use axum::body::Bytes;
use axum::extract::Path;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::Router;
use plurx_core::store::SqliteStore;

use super::*;
#[tokio::test]
async fn sharing_art_opened_digest_refuses_symlinks_and_rehashes_inode_changes() {
    let _serial = super::super::shared_artwork::ART_FIXTURES.lock().await;
    let (_, state) = super::super::tests::test_app_with_state();
    let directory = crate::test_tempdir().expect("owned images");
    let path = directory.path().join("private.png");
    let bytes = b"\x89PNG\r\n\x1a\nfirst";
    std::fs::write(&path, bytes).expect("first");
    let first = shared_opened_art(
        &state,
        path.clone(),
        "private.png",
        super::super::shared_artwork::ArtBodyLease::source(false).expect("lease"),
    )
    .await
    .expect("read")
    .expect("asset");
    assert_eq!(first.digest, <[u8; 32]>::from(Sha256::digest(bytes)));
    std::fs::write(&path, b"\x89PNG\r\n\x1a\nother").expect("same inode same size rewrite");
    assert!(!first.still_current().await);
    drop(first);
    let second = shared_opened_art(
        &state,
        path.clone(),
        "private.png",
        super::super::shared_artwork::ArtBodyLease::source(false).expect("lease"),
    )
    .await
    .expect("read")
    .expect("asset");
    assert_ne!(second.digest, <[u8; 32]>::from(Sha256::digest(bytes)));
    drop(second);
    let replacement = directory.path().join("replacement");
    std::fs::write(&replacement, bytes).expect("replace");
    std::fs::rename(replacement, &path).expect("rename");
    assert_eq!(
        shared_opened_art(
            &state,
            path.clone(),
            "private.png",
            super::super::shared_artwork::ArtBodyLease::source(false).expect("lease")
        )
        .await
        .expect("read")
        .expect("asset")
        .digest,
        <[u8; 32]>::from(Sha256::digest(bytes))
    );
    #[cfg(unix)]
    {
        std::fs::remove_file(&path).expect("remove");
        std::os::unix::fs::symlink(directory.path().join("outside"), &path).expect("symlink");
        assert!(shared_opened_art(
            &state,
            path.clone(),
            "private.png",
            super::super::shared_artwork::ArtBodyLease::source(false).expect("lease")
        )
        .await
        .expect("closed")
        .is_none());
    }
    let huge = std::fs::File::create(directory.path().join("huge.png")).expect("huge");
    huge.set_len(MAX_ARTWORK_BYTES + 1).expect("size");
    assert!(shared_opened_art(
        &state,
        directory.path().join("huge.png"),
        "huge.png",
        super::super::shared_artwork::ArtBodyLease::source(false).expect("lease")
    )
    .await
    .expect("closed")
    .is_none());
}

#[test]
fn artwork_inventory_deduplicates_bare_names_and_rejects_paths() {
    let references = artwork_references(vec![
        ArtworkInventoryItem {
            id: 10,
            poster_path: Some("shared.jpg".to_owned()),
            backdrop_path: Some("nested/rejected.jpg".to_owned()),
        },
        ArtworkInventoryItem {
            id: 20,
            poster_path: Some("shared.jpg".to_owned()),
            backdrop_path: Some("hero.jpg".to_owned()),
        },
    ]);
    assert_eq!(references.len(), 2);
    assert_eq!(references[0].filename, "hero.jpg");
    assert_eq!(references[0].item_id, 20);
    assert_eq!(references[1].filename, "shared.jpg");
    assert_eq!(references[1].item_id, 10);
}

#[test]
fn orphan_sweep_only_matches_exact_plurx_artwork_names() {
    let digest = "a".repeat(64);
    assert!(managed_artwork_filename("84-poster.jpg"));
    assert!(managed_artwork_filename("84-backdrop.png"));
    assert!(managed_artwork_filename(&format!(
        "84-poster-{digest}.webp"
    )));
    assert!(!managed_artwork_filename("2024-family.jpg"));
    assert!(!managed_artwork_filename(".84-poster.jpg"));
    assert!(!managed_artwork_filename("84-poster-short.jpg"));
    assert!(!managed_artwork_filename("0-poster.jpg"));
    assert!(!managed_artwork_filename("-84-poster.jpg"));
}

#[test]
fn bounded_orphan_classifier_authenticates_both_generation_formats() {
    let bytes = b"immutable artwork generation";
    let digest = hex::encode(Sha256::digest(bytes));
    let scoped = format!("84-poster-c{}.jpg", &digest[..16]);
    let curator = format!("84-poster-{digest}.webp");

    assert!(content_addressed_artwork_name(&scoped));
    assert!(content_addressed_artwork_name(&curator));
    assert!(artwork_bytes_match_name(&scoped, bytes));
    assert!(artwork_bytes_match_name(&curator, bytes));
    assert!(!artwork_bytes_match_name(&scoped, b"corrupt"));
    assert!(!artwork_bytes_match_name(&curator, b"corrupt"));

    assert!(!content_addressed_artwork_name(&format!(
        "84-backdrop-{digest}.jpg"
    )));
    assert!(!content_addressed_artwork_name(&format!(
        "family-poster-{digest}.jpg"
    )));
    assert!(!content_addressed_artwork_name(
        "84-poster-c0123456789abcdef.txt"
    ));
}

#[tokio::test]
async fn a_verified_hit_is_not_rehashed_and_a_validator_skips_the_read() {
    let coordinator = ArtworkCoordinator::new();
    let directory = crate::test_tempdir().expect("artwork");
    let filename = "84-poster.jpg";
    tokio::fs::write(directory.path().join(filename), b"legacy artwork")
        .await
        .expect("artwork");

    let first = serve_local_artwork(
        &coordinator,
        directory.path(),
        filename,
        &HeaderMap::new(),
        ArtworkRoute::User,
    )
    .await
    .expect("first response");
    let etag = first.headers()[header::ETAG].clone();
    assert_eq!(coordinator.hashes.load(Ordering::SeqCst), 1);
    drop(first);

    let second = serve_local_artwork(
        &coordinator,
        directory.path(),
        filename,
        &HeaderMap::new(),
        ArtworkRoute::User,
    )
    .await
    .expect("cached response");
    assert_eq!(coordinator.hashes.load(Ordering::SeqCst), 1);
    drop(second);

    let mut conditional = HeaderMap::new();
    conditional.insert(header::IF_NONE_MATCH, etag.clone());
    let response = serve_local_artwork(
        &coordinator,
        directory.path(),
        filename,
        &conditional,
        ArtworkRoute::User,
    )
    .await
    .expect("conditional response");
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(response.headers()[header::ETAG], etag);
    assert_eq!(coordinator.hashes.load(Ordering::SeqCst), 1);
    assert_eq!(
        coordinator.local_bytes.available_permits(),
        LOCAL_READ_BUDGET_KIB
    );
}

#[cfg(unix)]
#[tokio::test]
async fn an_in_place_rewrite_with_a_new_ctime_is_reverified_and_quarantined() {
    let coordinator = ArtworkCoordinator::new();
    let directory = crate::test_tempdir().expect("artwork");
    let original = b"authenticated artwork";
    let digest = hex::encode(Sha256::digest(original));
    let filename = format!("84-poster-{digest}.webp");
    let path = directory.path().join(&filename);
    tokio::fs::write(&path, original).await.expect("original");
    let first = serve_local_artwork(
        &coordinator,
        directory.path(),
        &filename,
        &HeaderMap::new(),
        ArtworkRoute::User,
    )
    .await
    .expect("verified original");
    drop(first);
    tokio::time::sleep(Duration::from_millis(2)).await;
    tokio::fs::write(&path, b"corrupted artwork!!!!")
        .await
        .expect("in-place replacement");

    let error = serve_local_artwork(
        &coordinator,
        directory.path(),
        &filename,
        &HeaderMap::new(),
        ArtworkRoute::User,
    )
    .await
    .expect_err("corrupt rewrite is refused");
    assert!(matches!(error, ApiError::NotFound("image")));
    assert!(!path.exists());
    assert_eq!(coordinator.hashes.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_replaced_inode_is_reverified_and_install_evicts_the_entry() {
    let coordinator = ArtworkCoordinator::new();
    let directory = crate::test_tempdir().expect("artwork");
    let filename = "84-poster.jpg";
    tokio::fs::write(directory.path().join(filename), b"old legacy artwork")
        .await
        .expect("old artwork");
    let first = serve_local_artwork(
        &coordinator,
        directory.path(),
        filename,
        &HeaderMap::new(),
        ArtworkRoute::User,
    )
    .await
    .expect("old response");
    let old_etag = first.headers()[header::ETAG].clone();
    drop(first);
    assert!(coordinator
        .verified
        .lock()
        .await
        .entries
        .contains_key(filename));

    install_artwork(
        &coordinator,
        directory.path(),
        filename,
        b"new legacy artwork",
    )
    .await
    .expect("new artwork");
    assert!(!coordinator
        .verified
        .lock()
        .await
        .entries
        .contains_key(filename));
    let second = serve_local_artwork(
        &coordinator,
        directory.path(),
        filename,
        &HeaderMap::new(),
        ArtworkRoute::User,
    )
    .await
    .expect("new response");
    assert_ne!(second.headers()[header::ETAG], old_etag);
    assert_eq!(coordinator.hashes.load(Ordering::SeqCst), 2);
}

#[test]
fn orphan_classifier_only_accepts_exact_corrupt_quarantine_shape() {
    let digest = "a".repeat(64);
    let managed = format!("84-poster-{digest}.webp");
    let quarantine = format!(".{managed}.corrupt-{}", "b".repeat(32));
    assert!(corrupt_artwork_quarantine_name(&quarantine));
    assert!(temporary_artwork_orphan_name(&quarantine));
    assert!(!temporary_artwork_orphan_name(&format!(
        ".README.txt.corrupt-{}",
        "b".repeat(32)
    )));
    assert!(!temporary_artwork_orphan_name(&format!(
        ".{managed}.corrupt-too-short"
    )));
}

#[test]
fn w300_never_upscales_a_small_source() {
    let mut png = vec![0_u8; 24];
    png[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
    png[16..20].copy_from_slice(&200_u32.to_be_bytes());
    png[20..24].copy_from_slice(&120_u32.to_be_bytes());
    assert!(should_serve_original(ArtworkSize::W300, "poster.png", &png));
    assert!(!should_serve_original(
        ArtworkSize::W300,
        "poster.jpg",
        &png
    ));
}

#[test]
fn derivative_key_is_the_verified_source_digest() {
    // Both sides of every assertion here are written out, not derived from
    // the same expression as the value under test: an assertion that
    // recomputes `[..32]` from `hex::encode(first)` holds whatever prefix
    // width the code actually uses, and so could not have caught the
    // sweep's 16-against-32 comparison.
    let first: [u8; 32] = Sha256::digest(b"first legacy bytes").into();
    let second: [u8; 32] = Sha256::digest(b"second legacy bytes").into();
    let first_key =
        derivative_filename(first, ArtworkSize::W500, "84-poster.jpg").expect("first key");
    let second_key =
        derivative_filename(second, ArtworkSize::W500, "84-poster.jpg").expect("second key");
    assert_eq!(
        first_key, "84-poster.jpg-w500-107457d3b119e162368e875adc2c8cf3.jpg",
        "the key names its source and the first 32 hex characters of the source digest"
    );
    assert_eq!(
        second_key,
        "84-poster.jpg-w500-9343c668c053f08a3e30c21aa418332f.jpg"
    );
    assert_ne!(first_key, second_key, "changed source bytes mint a new key");

    let parsed = parse_derivative_name(&first_key).expect("parse");
    assert_eq!(parsed.source, "84-poster.jpg");
    assert_eq!(parsed.prefix, "107457d3b119e162368e875adc2c8cf3");
    // The comparison width the sweep uses has to be one a `-c{16hex}`
    // source can supply.
    assert_eq!(DERIVATIVE_PREFIX_MATCH, 16);
    assert_eq!(
        &parsed.prefix[..DERIVATIVE_PREFIX_MATCH],
        "107457d3b119e162"
    );

    // A `{item}-{kind}-c{16hex}` source publishes exactly the sixteen
    // characters the sweep compares, and a derivative of it round-trips.
    let backdrop = format!("84-backdrop-c{}.jpg", "107457d3b119e162");
    let backdrop_key =
        derivative_filename(first, ArtworkSize::W780, &backdrop).expect("backdrop key");
    let parsed = parse_derivative_name(&backdrop_key).expect("parse backdrop");
    assert_eq!(parsed.source, backdrop);
    assert_eq!(
        &parsed.prefix[..DERIVATIVE_PREFIX_MATCH],
        "107457d3b119e162"
    );

    assert_eq!(parse_derivative_name("84-poster.jpg"), None);
    assert_eq!(
        parse_derivative_name(&format!(
            "84-poster.jpg-w999-{}.jpg",
            "107457d3b119e162368e875adc2c8cf3"
        )),
        None
    );
    assert!(superseded_derivative_name(
        "107457d3b119e162368e875adc2c8cf3-w300.jpg"
    ));
    assert!(!superseded_derivative_name(&first_key));
}

#[cfg(unix)]
#[tokio::test]
async fn a_failed_ffmpeg_leaves_no_derivative() {
    let coordinator = ArtworkCoordinator::new();
    let directory = crate::test_tempdir().expect("artwork");
    let source_name = "84-poster.png";
    let source_path = directory.path().join(source_name);
    let source = b"not decoded because the child fails first";
    tokio::fs::write(&source_path, source)
        .await
        .expect("source");
    let derived_dir = ensure_derived_dir(directory.path())
        .await
        .expect("derived directory");
    let digest: [u8; 32] = Sha256::digest(source).into();
    let derived_name =
        derivative_filename(digest, ArtworkSize::W300, source_name).expect("derivative name");
    let source_identity = open_local_artwork(source_path.clone())
        .await
        .expect("source identity")
        .identity;
    generate_derivative_with_bin(
        &coordinator,
        DerivativeJob {
            runtime_cache: directory.path(),
            source_path: &source_path,
            source_name,
            source_identity,
            derived_dir: &derived_dir,
            derived_name: &derived_name,
            size: ArtworkSize::W300,
        },
        "/bin/false",
    )
    .await
    .expect_err("failed child");
    assert!(!derived_dir.join(derived_name).exists());
    assert!(
        std::fs::read_dir(derived_dir)
            .expect("derived entries")
            .next()
            .is_none(),
        "failed generation must remove its temporary output"
    );
}

#[tokio::test]
async fn ffmpeg_derivative_is_bounded_and_preserves_the_original() {
    let coordinator = ArtworkCoordinator::new();
    let directory = crate::test_tempdir().expect("artwork");
    let source_name = "84-poster.png";
    let source_path = directory.path().join(source_name);
    let status = std::process::Command::new(crate::ffmpeg::ffmpeg_bin())
        .args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=red:s=640x360",
            "-frames:v",
            "1",
            "-y",
        ])
        .arg(&source_path)
        .status()
        .expect("fixture ffmpeg");
    assert!(status.success());
    let original = tokio::fs::read(&source_path).await.expect("source bytes");
    let source =
        match read_verified_local_artwork(&coordinator, source_path.clone(), source_name).await {
            LocalArtworkRead::Verified(source) => source,
            _ => panic!("verified fixture"),
        };
    let digest = source.digest;
    drop(source);
    let derived_dir = ensure_derived_dir(directory.path())
        .await
        .expect("derived directory");
    let derived_name =
        derivative_filename(digest, ArtworkSize::W300, source_name).expect("derivative name");
    let source_identity = open_local_artwork(source_path.clone())
        .await
        .expect("source identity")
        .identity;
    generate_derivative(
        &coordinator,
        DerivativeJob {
            runtime_cache: directory.path(),
            source_path: &source_path,
            source_name,
            source_identity,
            derived_dir: &derived_dir,
            derived_name: &derived_name,
            size: ArtworkSize::W300,
        },
    )
    .await
    .expect("generate derivative");
    let derived = tokio::fs::read(derived_dir.join(&derived_name))
        .await
        .expect("derived bytes");
    assert_eq!(image_dimensions(&derived_name, &derived), Some((300, 168)));
    assert!(derived.len() as u64 <= MAX_ARTWORK_BYTES);
    assert_eq!(
        tokio::fs::read(source_path).await.expect("source survives"),
        original
    );
}

#[tokio::test]
async fn failed_corruption_restore_leaves_a_sweep_eligible_quarantine() {
    let coordinator = ArtworkCoordinator::new();
    let directory = crate::test_tempdir().expect("artwork");
    let digest = hex::encode(Sha256::digest(b"authenticated artwork"));
    let filename = format!("84-poster-{digest}.webp");
    let path = directory.path().join(&filename);
    std::fs::write(&path, b"corrupt original").expect("corrupt original");
    let expected = artwork_file_identity(&std::fs::metadata(&path).expect("metadata"));
    let swap_directory = directory.path().to_path_buf();
    let swap_filename = filename.clone();

    quarantine_corrupt_artwork_with(
        &coordinator,
        directory.path(),
        &filename,
        expected,
        Some(Box::new(move |quarantine| {
            std::fs::remove_file(swap_directory.join(quarantine))
                .expect("replace quarantine inode");
            std::fs::write(swap_directory.join(quarantine), b"changed quarantine")
                .expect("changed quarantine");
            std::fs::write(swap_directory.join(&swap_filename), b"newer target")
                .expect("competing target");
        })),
    )
    .await;

    assert_eq!(std::fs::read(&path).expect("new target"), b"newer target");
    let quarantines = std::fs::read_dir(directory.path())
        .expect("artwork entries")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|entry| entry != &filename)
        .collect::<Vec<_>>();
    assert_eq!(quarantines.len(), 1);
    assert!(
        temporary_artwork_orphan_name(&quarantines[0]),
        "a failed no-replace restore must remain eligible for bounded cleanup"
    );
}

#[tokio::test]
async fn versioned_repair_name_invalidates_old_bytes_on_every_voter() {
    let old = "84-poster-aaaaaaaaaaaaaaaa.jpg";
    let new = "84-poster-bbbbbbbbbbbbbbbb.jpg";
    let voters = [
        crate::test_tempdir().expect("voter one"),
        crate::test_tempdir().expect("voter two"),
    ];
    for voter in &voters {
        tokio::fs::write(voter.path().join(old), b"old edition")
            .await
            .expect("old cover");
    }
    let references = artwork_references(vec![ArtworkInventoryItem {
        id: 84,
        poster_path: Some(new.to_owned()),
        backdrop_path: None,
    }]);
    assert_eq!(references.len(), 1);
    assert_eq!(references[0].filename, new);
    for voter in &voters {
        assert!(voter.path().join(old).is_file(), "stale bytes still exist");
        assert!(
            tokio::fs::metadata(voter.path().join(&references[0].filename))
                .await
                .is_err(),
            "the replicated content-versioned name must be missing on every stale voter"
        );
    }
}

#[tokio::test]
async fn orphan_sweep_honors_publication_reservations_and_removes_old_generations() {
    let store = SqliteStore::open_in_memory().expect("store");
    let directory = crate::test_tempdir().expect("artwork");
    let orphan = directory
        .path()
        .join(format!("84-poster-{}.jpg", "a".repeat(64)));
    tokio::fs::write(&orphan, b"superseded")
        .await
        .expect("orphan");
    tokio::fs::write(directory.path().join("README.txt"), b"not managed")
        .await
        .expect("sentinel");
    let reservation =
        plurx_core::metadata::reserve_artwork_publication(orphan.clone()).expect("reserve");
    assert_eq!(
        sweep_orphan_artwork(&store, directory.path(), Duration::ZERO).await,
        0,
        "an active publisher owns the path"
    );
    assert!(orphan.is_file());
    drop(reservation);

    assert_eq!(
        sweep_orphan_artwork(&store, directory.path(), Duration::ZERO).await,
        1
    );
    assert!(!orphan.exists());
    assert!(directory.path().join("README.txt").is_file());
}

#[tokio::test]
async fn orphan_candidate_rechecks_age_after_a_competing_publication() {
    let store = SqliteStore::open_in_memory().expect("store");
    let directory = crate::test_tempdir().expect("artwork");
    let filename = format!("84-poster-{}.jpg", "b".repeat(64));
    let path = directory.path().join(&filename);
    let grace = Duration::from_secs(60);
    tokio::fs::write(&path, b"old generation")
        .await
        .expect("old candidate");
    let old_modified = std::time::SystemTime::now()
        .checked_sub(Duration::from_secs(120))
        .expect("old modified time");
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("open old candidate")
        .set_times(std::fs::FileTimes::new().set_modified(old_modified))
        .expect("age old candidate");
    assert!(orphan_candidate_old_enough(&path, grace).await);

    plurx_core::metadata::write_artwork_atomically(&path, b"fresh generation")
        .await
        .expect("competing publication");
    assert!(
        !remove_orphan_candidate(&store, &filename, &path, grace).await,
        "the discovery-time age must not authorize deletion after replacement"
    );
    assert_eq!(
        tokio::fs::read(&path).await.expect("fresh file survives"),
        b"fresh generation"
    );
}

#[tokio::test]
async fn simultaneous_same_filename_misses_fetch_and_install_once() {
    let coordinator = ArtworkCoordinator::new();
    let directory = Arc::new(crate::test_tempdir().expect("artwork directory"));
    let hits = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::new();
    for _ in 0..PEER_FETCH_CONCURRENCY {
        let coordinator = Arc::clone(&coordinator);
        let directory = Arc::clone(&directory);
        let hits = Arc::clone(&hits);
        tasks.push(tokio::spawn(async move {
            fetch_and_materialize(
                &coordinator,
                directory.path(),
                "singleflight.jpg",
                move |_| async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    Some(b"one peer response".to_vec())
                },
            )
            .await
            .expect("artwork admission")
            .expect("materialized bytes")
        }));
    }
    for task in tasks {
        let bytes = task.await.expect("request task");
        assert_eq!(bytes.as_ref(), b"one peer response");
    }
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert_eq!(
        tokio::fs::read(directory.path().join("singleflight.jpg"))
            .await
            .expect("installed artwork"),
        b"one peer response"
    );
}

#[tokio::test]
async fn distinct_filename_fetches_obey_the_global_buffer_bound() {
    let coordinator = ArtworkCoordinator::new();
    let directory = Arc::new(crate::test_tempdir().expect("artwork directory"));
    let active = Arc::new(AtomicUsize::new(0));
    let maximum = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::new();
    for ordinal in 0..24 {
        let coordinator = Arc::clone(&coordinator);
        let directory = Arc::clone(&directory);
        let active = Arc::clone(&active);
        let maximum = Arc::clone(&maximum);
        tasks.push(tokio::spawn(async move {
            let filename = format!("bounded-{ordinal}.jpg");
            fetch_and_materialize(&coordinator, directory.path(), &filename, move |_| {
                let active = Arc::clone(&active);
                let maximum = Arc::clone(&maximum);
                async move {
                    let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                    maximum.fetch_max(now, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    active.fetch_sub(1, Ordering::SeqCst);
                    Some(vec![ordinal as u8 + 1])
                }
            })
            .await
            .map(|bytes| bytes.expect("bounded fetch"))
        }));
    }
    let mut admitted = 0usize;
    let mut rejected = 0usize;
    for task in tasks {
        match task.await.expect("request task") {
            Ok(_) => admitted += 1,
            Err(ArtworkCapacity) => rejected += 1,
        }
    }
    assert_eq!(maximum.load(Ordering::SeqCst), PEER_FETCH_CONCURRENCY);
    assert_eq!(admitted, PEER_FETCH_CONCURRENCY);
    assert_eq!(rejected, 24 - PEER_FETCH_CONCURRENCY);
}

#[tokio::test]
async fn a_derive_permit_wait_is_timed_into_the_image_materialize_pool() {
    use crate::telemetry::{admission_waits_for_test, AdmissionPool};
    let coordinator = ArtworkCoordinator::new();
    let before = admission_waits_for_test(AdmissionPool::ImageMaterialize);
    let _permit = coordinator
        .derive_permit()
        .await
        .expect("an idle pool admits");
    let after = admission_waits_for_test(AdmissionPool::ImageMaterialize);
    assert!(after > before, "{before} -> {after}");
}

/// A derive that queues behind a full pool and is abandoned there (its
/// request dropped) is still one observation.
#[tokio::test]
async fn an_abandoned_derive_permit_wait_is_still_timed() {
    use crate::telemetry::{admission_waits_on_this_thread, AdmissionPool};
    use std::future::Future;
    let coordinator = ArtworkCoordinator::new();
    let held = Arc::clone(&coordinator.derive_permits)
        .acquire_many_owned(
            u32::try_from(coordinator.derive_permits.available_permits()).expect("permit count"),
        )
        .await
        .expect("take the whole pool");
    let before = admission_waits_on_this_thread(AdmissionPool::ImageMaterialize);
    {
        let mut queued = Box::pin(coordinator.derive_permit());
        let parked = std::future::poll_fn(|cx| {
            std::task::Poll::Ready(queued.as_mut().poll(cx).is_pending())
        })
        .await;
        assert!(parked, "the derive queued behind the full pool");
    }
    assert_eq!(
        admission_waits_on_this_thread(AdmissionPool::ImageMaterialize) - before,
        1,
        "the abandoned wait is recorded once"
    );
    drop(held);
}

#[tokio::test]
async fn local_reads_are_bounded_by_bytes_not_count() {
    let coordinator = ArtworkCoordinator::new();
    let mut small = Vec::new();
    for _ in 0..12 {
        small.push(
            coordinator
                .local_permit(100 * 1024)
                .await
                .expect("twelve small reads fit the byte budget"),
        );
    }
    drop(small);

    let mut large = Vec::new();
    for _ in 0..4 {
        large.push(
            coordinator
                .local_permit(MAX_ARTWORK_BYTES)
                .await
                .expect("four maximum-size reads fit"),
        );
    }
    assert!(
        coordinator.local_permit(MAX_ARTWORK_BYTES).await.is_none(),
        "the fifth maximum-size read must time out at the byte budget"
    );
    drop(large.pop());
    assert!(coordinator.local_permit(MAX_ARTWORK_BYTES).await.is_some());
}

#[tokio::test]
async fn saturated_response_capacity_defers_reconciliation_repairs() {
    let coordinator = ArtworkCoordinator::new();
    let directory = crate::test_tempdir().expect("artwork directory");
    let mut held = Vec::new();
    for _ in 0..PEER_FETCH_CONCURRENCY {
        held.push(coordinator.peer_permit().expect("hold peer permit"));
    }
    tokio::fs::write(directory.path().join("local.jpg"), b"local")
        .await
        .expect("local artwork");
    let local = serve_local_artwork(
        &coordinator,
        directory.path(),
        "local.jpg",
        &HeaderMap::new(),
        ArtworkRoute::User,
    )
    .await
    .expect("peer saturation must not refuse local bytes");
    drop(local);
    let outcome = artwork_pull_outcome(
        fetch_and_materialize(&coordinator, directory.path(), "capacity.jpg", |_| async {
            Some(b"peer bytes".to_vec())
        })
        .await,
    );
    assert_eq!(outcome, ArtworkPullOutcome::DeferredCapacity);

    let item_id = 42;
    let mut unresolved = BTreeMap::from([(item_id, vec!["capacity.jpg".to_owned()])]);
    let mut repair_after = HashMap::from([(item_id, Instant::now())]);
    suppress_deferred_artwork_repairs(
        BTreeSet::from([item_id]),
        &mut unresolved,
        &mut repair_after,
    );
    assert!(unresolved.is_empty());
    assert!(repair_after.is_empty());
    drop(held);
}

#[test]
fn missing_artwork_becomes_repairable_after_the_grace_pass() {
    let item_id = 77;
    let missing = || BTreeMap::from([(item_id, vec!["77-poster.jpg".to_owned()])]);
    let first_pass = Instant::now();
    let mut repair_after = HashMap::new();
    assert!(due_artwork_repairs(missing(), &mut repair_after, first_pass).is_empty());
    assert_eq!(
        repair_after.get(&item_id).copied(),
        Some(first_pass + SOURCE_REPAIR_GRACE)
    );

    let after_grace = first_pass + SOURCE_REPAIR_GRACE + Duration::from_millis(1);
    retain_recent_artwork_repairs(&mut repair_after, after_grace);
    let due = due_artwork_repairs(missing(), &mut repair_after, after_grace);
    assert_eq!(due.get(&item_id), Some(&vec!["77-poster.jpg".to_owned()]));
}

#[cfg(unix)]
#[tokio::test]
async fn provider_repair_validation_rejects_symlinks_and_oversized_outputs() {
    let directory = crate::test_tempdir().expect("artwork directory");
    let outside = tempfile::NamedTempFile::new().expect("outside artwork");
    std::fs::write(outside.path(), b"outside bytes").expect("outside bytes");
    std::os::unix::fs::symlink(outside.path(), directory.path().join("symlink.jpg"))
        .expect("symlink output");
    assert!(!valid_materialized_artwork(directory.path(), "symlink.jpg").await);

    let oversized =
        std::fs::File::create(directory.path().join("oversized.jpg")).expect("oversized output");
    oversized
        .set_len(MAX_ARTWORK_BYTES + 1)
        .expect("extend sparse artwork");
    assert!(!valid_materialized_artwork(directory.path(), "oversized.jpg").await);
}

#[test]
fn versioned_artwork_is_private_and_immutable() {
    let response = artwork_response(
        FsPath::new("287-poster.jpg"),
        b"\xff\xd8\xff artwork".to_vec(),
    );
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "image/jpeg");
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "private, max-age=604800, immutable"
    );
}

#[test]
fn peer_url_keeps_a_reverse_proxy_prefix_and_escapes_the_filename() {
    let url = peer_artwork_url("https://plurx-b.test/cinema/", "poster one.jpg").expect("peer URL");
    assert_eq!(
        url.as_str(),
        "https://plurx-b.test/cinema/api/v1/cluster/artwork/poster%20one.jpg"
    );
    assert!(peer_artwork_url("file:///tmp/plurx", "poster.jpg").is_none());
}

#[test]
fn peer_proof_headers_are_complete_and_typed() {
    let mut headers = HeaderMap::new();
    headers.insert(NODE_ID_HEADER, "node-b".parse().expect("node"));
    headers.insert(TIMESTAMP_HEADER, "12345".parse().expect("time"));
    headers.insert(SIGNATURE_HEADER, "aabb".parse().expect("signature"));
    assert_eq!(
        peer_auth_from_headers(&headers),
        Some(ArtworkPeerAuth {
            node_id: "node-b".to_owned(),
            timestamp_ms: 12_345,
            signature: "aabb".to_owned(),
        })
    );
    headers.remove(SIGNATURE_HEADER);
    assert!(peer_auth_from_headers(&headers).is_none());
}

#[tokio::test]
async fn sharing_art_single_peer_collector_keeps_owned_capacity_and_refuses_invalid_bytes() {
    let _fixture = super::super::shared_artwork::ART_FIXTURES.lock().await;
    async fn peer(Path(name): Path<String>, headers: HeaderMap) -> Response {
        if headers
            .get(SIGNATURE_HEADER)
            .and_then(|value| value.to_str().ok())
            != Some("proof")
        {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        if name == "redirect.jpg" {
            return (StatusCode::FOUND, [(header::LOCATION, "/original.jpg")]).into_response();
        }
        let bytes = if name == "invalid.jpg" {
            Bytes::from_static(b"not an image")
        } else {
            Bytes::from_static(b"\xff\xd8\xff bounded peer")
        };
        ([(header::CONTENT_TYPE, "image/jpeg")], bytes).into_response()
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let address = listener.local_addr().expect("address");
    let server = tokio::spawn(async move {
        axum::serve(listener, Router::new().route("/art/{name}", get(peer)))
            .await
            .expect("server");
    });
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .expect("client");
    let lease = super::super::shared_artwork::ArtBodyLease::source(false).expect("lease");
    let owner = Arc::new(SharedArtDownloadOwner {
        _lease: lease,
        _local: None,
        _peer: None,
        _workspace: super::super::shared_artwork::ArtBodyLease::source_collection_workspace()
            .expect("workspace"),
    });
    let auth = ArtworkPeerAuth {
        node_id: "peer".to_owned(),
        timestamp_ms: 123,
        signature: "proof".to_owned(),
    };
    for name in ["invalid.jpg", "redirect.jpg"] {
        assert!(shared_peer_snapshot(
            &client,
            reqwest::Url::parse(&format!("http://{address}/art/{name}")).expect("url"),
            &auth,
            name,
            owner.clone()
        )
        .await
        .is_none());
    }
    let download = shared_peer_snapshot(
        &client,
        reqwest::Url::parse(&format!("http://{address}/art/original.jpg")).expect("url"),
        &auth,
        "original.jpg",
        owner.clone(),
    )
    .await
    .expect("validated collector");
    assert_eq!(download.as_ref(), b"\xff\xd8\xff bounded peer");
    drop(owner);
    assert_eq!(super::super::shared_artwork::source_capacity().0, 3);
    let directory = crate::test_tempdir().expect("directory");
    plurx_core::fs_secure::atomic_write_child_owned(directory.path(), "original.jpg", download)
        .await
        .expect("owned publication");
    assert_eq!(
        tokio::fs::read(directory.path().join("original.jpg"))
            .await
            .expect("bytes"),
        b"\xff\xd8\xff bounded peer"
    );
    assert_eq!(super::super::shared_artwork::source_capacity().0, 4);
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn a_peer_miss_sends_node_proof_and_materializes_complete_bytes() {
    let coordinator = ArtworkCoordinator::new();
    async fn peer(
        Path(filename): Path<String>,
        headers: HeaderMap,
    ) -> (StatusCode, HeaderMap, Bytes) {
        let authorized = headers
            .get(NODE_ID_HEADER)
            .and_then(|value| value.to_str().ok())
            == Some("node-b")
            && headers
                .get(TIMESTAMP_HEADER)
                .and_then(|value| value.to_str().ok())
                == Some("12345")
            && headers
                .get(SIGNATURE_HEADER)
                .and_then(|value| value.to_str().ok())
                == Some("aabb");
        if !authorized || filename != "poster one.jpg" {
            return (StatusCode::UNAUTHORIZED, HeaderMap::new(), Bytes::new());
        }
        let mut response_headers = HeaderMap::new();
        response_headers.insert(header::CONTENT_TYPE, "image/jpeg".parse().expect("mime"));
        (
            StatusCode::OK,
            response_headers,
            Bytes::from_static(b"\xff\xd8\xff peer artwork"),
        )
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let address = listener.local_addr().expect("address");
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route("/cinema/api/v1/cluster/artwork/{filename}", get(peer)),
        )
        .await
        .expect("peer server");
    });

    let client = artwork_client().expect("client");
    let bytes = fetch_peer_artwork_with_auth(
        &client,
        &["not a URL".to_owned(), format!("http://{address}/cinema")],
        "poster one.jpg",
        &ArtworkPeerAuth {
            node_id: "node-b".to_owned(),
            timestamp_ms: 12_345,
            signature: "aabb".to_owned(),
        },
    )
    .await
    .expect("peer bytes");
    assert_eq!(bytes, b"\xff\xd8\xff peer artwork");

    let directory = crate::test_tempdir().expect("artwork directory");
    install_artwork(&coordinator, directory.path(), "poster one.jpg", &bytes)
        .await
        .expect("materialize");
    assert_eq!(
        tokio::fs::read(directory.path().join("poster one.jpg"))
            .await
            .expect("cached artwork"),
        bytes
    );
    server.abort();
}

// ---- M2 derivative route ------------------------------------------------
// `serve_derivative` needs a real `AppState`: the store behind the orphan
// sweep, the artwork root, and the dedicated runtime cache the child runs
// against.
pub(super) fn derivative_state() -> AppState {
    let store = SqliteStore::open_in_memory().expect("store");
    let base = crate::test_temp_path(format!("plurx-derived-{}", uuid::Uuid::new_v4()));
    let dirs = crate::state::Dirs {
        artwork: base.join("artwork"),
        transcode: base.join("transcode"),
        cache: base.join("cache"),
        subs: base.join("subs"),
        runtime_cache: base.join("runtime"),
        renditions: base.join("renditions"),
    };
    std::fs::create_dir_all(&dirs.artwork).expect("artwork directory");
    AppState::new(
        "test".into(),
        Arc::new(store),
        dirs,
        "test-node".into(),
        Default::default(),
        Default::default(),
        Arc::new(crate::logbuf::LogBuffer::new(64)),
    )
}

/// One real image, written by the shipped ffmpeg, as the plan's §5.2
/// "generated fixtures only" requires.
pub(super) fn write_test_image(path: &FsPath, source: &str, extra: &[&str]) {
    let mut command = std::process::Command::new(crate::ffmpeg::ffmpeg_bin());
    command.args([
        "-nostdin",
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "lavfi",
        "-i",
        source,
    ]);
    command.args(extra);
    command.args(["-y"]).arg(path);
    let status = command.status().expect("fixture ffmpeg");
    assert!(status.success(), "fixture ffmpeg failed for {source}");
}

async fn response_body(response: Response) -> Vec<u8> {
    axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body")
        .to_vec()
}

fn header_value(response: &Response, name: &str) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

#[tokio::test]
async fn a_failed_ffmpeg_serves_the_original() {
    // Bytes no decoder will accept, so the real child exits non-zero and
    // the handler takes its fallback path.
    let state = derivative_state();
    let source_name = "84-poster.jpg";
    let original = b"\xff\xd8\xff not actually a JPEG".to_vec();
    tokio::fs::write(state.artwork_dir.join(source_name), &original)
        .await
        .expect("source");

    let response = serve_derivative(&state, source_name, &HeaderMap::new(), ArtworkSize::W300)
        .await
        .expect("a failed resize must not break the grid");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header_value(&response, "x-plurx-artwork").as_deref(),
        Some("original-fallback")
    );
    // The heart of it: these are the *original* bytes under a `?size=`
    // URL. Caching them as `immutable` for a week would make every cold
    // grid load poison its own cache with the full-size image and never
    // ask for the derivative that generation will eventually produce.
    assert_eq!(
        header_value(&response, "cache-control").as_deref(),
        Some("private, max-age=0, must-revalidate"),
        "a substituted original must revalidate, not be frozen for a week"
    );
    assert_eq!(response_body(response).await, original);
    assert!(!state
        .artwork_dir
        .join(DERIVED_DIR)
        .join("84-poster.jpg-w300")
        .exists());
}

#[tokio::test]
async fn derivatives_are_single_flight() {
    let state = derivative_state();
    let source_name = "84-backdrop.png";
    let source_path = state.artwork_dir.join(source_name);
    write_test_image(&source_path, "testsrc2=size=1280x720", &["-frames:v", "1"]);

    let headers = HeaderMap::new();
    let requests =
        (0..10).map(|_| serve_derivative(&state, source_name, &headers, ArtworkSize::W300));
    let responses = futures_util::future::join_all(requests).await;
    for response in responses {
        let response = response.expect("derivative");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            header_value(&response, "x-plurx-artwork"),
            Some("original-fallback".to_owned()),
            "cold requests return originals while one durable intent prepares the variant"
        );
    }
    assert_eq!(state.artwork_fetch.derivations.load(Ordering::SeqCst), 0);
    assert!(worker::run_one(&state).await.expect("worker pass"));
    assert_eq!(state.artwork_fetch.derivations.load(Ordering::SeqCst), 1);
    assert!(!worker::run_one(&state)
        .await
        .expect("no duplicate execution"));
    let ready = serve_derivative(&state, source_name, &headers, ArtworkSize::W300)
        .await
        .expect("ready variant");
    assert_eq!(header_value(&ready, "x-plurx-artwork"), None);
}

#[tokio::test]
async fn a_warm_derivative_hit_does_not_read_the_source() {
    let state = derivative_state();
    let source_name = "84-backdrop.png";
    let source_path = state.artwork_dir.join(source_name);
    write_test_image(&source_path, "testsrc2=size=1920x1080", &["-frames:v", "1"]);
    let source_kib = tokio::fs::metadata(&source_path)
        .await
        .expect("source metadata")
        .len()
        .div_ceil(1024);

    // A cold request records demand; the worker publishes verified bytes.
    drop(
        serve_derivative(&state, source_name, &HeaderMap::new(), ArtworkSize::W300)
            .await
            .expect("cold request"),
    );
    assert!(worker::run_one(&state).await.expect("worker pass"));
    let first = serve_derivative(&state, source_name, &HeaderMap::new(), ArtworkSize::W300)
        .await
        .expect("first request generates");
    assert_eq!(first.status(), StatusCode::OK);
    let etag = header_value(&first, "etag").expect("derivative ETag");
    let derived_len = response_body(first).await.len() as u64;
    assert!(
        derived_len.div_ceil(1024) < source_kib,
        "the fixture must shrink, or this test proves nothing"
    );

    // Leave exactly one KiB less budget than the source needs. Anything
    // that reads the original now is refused; the derivative still fits.
    let held = state
        .artwork_fetch
        .local_permit((LOCAL_READ_BUDGET_KIB as u64 - (source_kib - 1)) * 1024)
        .await
        .expect("squeeze the byte budget");
    assert!(
        state
            .artwork_fetch
            .local_permit(source_kib * 1024)
            .await
            .is_none(),
        "the remaining budget must be too small for the original"
    );

    let warm = serve_derivative(&state, source_name, &HeaderMap::new(), ArtworkSize::W300)
        .await
        .expect("a warm derivative must not need the original's byte budget");
    assert_eq!(warm.status(), StatusCode::OK);
    assert_eq!(response_body(warm).await.len() as u64, derived_len);

    // Plan objective 3 on the route M2 exists for: a validator match costs
    // no read and no permit.
    let mut headers = HeaderMap::new();
    headers.insert(header::IF_NONE_MATCH, etag.parse().expect("validator"));
    let not_modified = serve_derivative(&state, source_name, &headers, ArtworkSize::W300)
        .await
        .expect("conditional derivative request");
    assert_eq!(not_modified.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(
        header_value(&not_modified, "etag").as_deref(),
        Some(etag.as_str())
    );
    drop(held);
}

#[tokio::test]
async fn peer_route_refuses_size() {
    let state = derivative_state();
    let source_name = "84-poster.jpg";
    let original = b"\xff\xd8\xff peer original".to_vec();
    tokio::fs::write(state.artwork_dir.join(source_name), &original)
        .await
        .expect("source");

    let refused = serve_verified_peer_artwork(&state, source_name, Some("w300"), &HeaderMap::new())
        .await
        .expect_err("peers replicate originals and derive locally");
    assert!(
        matches!(refused, ApiError::BadRequest(_)),
        "a bucket on the cluster route is a 400, not a silent original"
    );

    let served = serve_verified_peer_artwork(&state, source_name, None, &HeaderMap::new())
        .await
        .expect("the peer route still serves originals");
    assert_eq!(served.status(), StatusCode::OK);
    assert_eq!(response_body(served).await, original);
    assert!(
        !state.artwork_dir.join(DERIVED_DIR).exists(),
        "the peer route must not create a derivative store"
    );
}

#[tokio::test]
async fn an_animated_gif_yields_one_png_frame() {
    let state = derivative_state();
    let source_name = "84-poster.gif";
    let source_path = state.artwork_dir.join(source_name);
    write_test_image(
        &source_path,
        "testsrc2=size=640x360:rate=10",
        &["-frames:v", "20"],
    );
    let source_identity = open_local_artwork(source_path.clone())
        .await
        .expect("source identity")
        .identity;
    let digest: [u8; 32] = Sha256::digest(tokio::fs::read(&source_path).await.expect("gif")).into();
    let derived_name =
        derivative_filename(digest, ArtworkSize::W300, source_name).expect("derived name");
    assert!(
        derived_name.ends_with(".png"),
        "an animated source must not stay a GIF: {derived_name}"
    );
    let derived_dir = ensure_derived_dir(&state.artwork_dir)
        .await
        .expect("derived directory");
    generate_derivative(
        &state.artwork_fetch,
        DerivativeJob {
            runtime_cache: &state.runtime_cache_dir,
            source_path: &source_path,
            source_name,
            source_identity,
            derived_dir: &derived_dir,
            derived_name: &derived_name,
            size: ArtworkSize::W300,
        },
    )
    .await
    .expect("generate from an animated GIF");
    let derived = tokio::fs::read(derived_dir.join(&derived_name))
        .await
        .expect("derived bytes");
    assert_eq!(&derived[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(image_dimensions(&derived_name, &derived), Some((300, 168)));
}

#[tokio::test]
async fn an_alpha_png_stays_png() {
    let state = derivative_state();
    let source_name = "84-poster.png";
    let source_path = state.artwork_dir.join(source_name);
    write_test_image(
        &source_path,
        "color=c=red@0.5:s=640x360",
        &["-frames:v", "1", "-pix_fmt", "rgba"],
    );
    let source = tokio::fs::read(&source_path).await.expect("source");
    // IHDR colour type 6 is truecolour with alpha; the fixture must have
    // one or the assertion below proves nothing.
    assert_eq!(source[25], 6, "the fixture source must carry alpha");
    let source_identity = open_local_artwork(source_path.clone())
        .await
        .expect("source identity")
        .identity;
    let digest: [u8; 32] = Sha256::digest(&source).into();
    let derived_name =
        derivative_filename(digest, ArtworkSize::W500, source_name).expect("derived name");
    assert!(derived_name.ends_with(".png"));
    let derived_dir = ensure_derived_dir(&state.artwork_dir)
        .await
        .expect("derived directory");
    generate_derivative(
        &state.artwork_fetch,
        DerivativeJob {
            runtime_cache: &state.runtime_cache_dir,
            source_path: &source_path,
            source_name,
            source_identity,
            derived_dir: &derived_dir,
            derived_name: &derived_name,
            size: ArtworkSize::W500,
        },
    )
    .await
    .expect("generate from an alpha PNG");
    let derived = tokio::fs::read(derived_dir.join(&derived_name))
        .await
        .expect("derived bytes");
    assert_eq!(&derived[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(
        derived[25], 6,
        "an alpha source must keep its alpha channel, or the grid gains black corners"
    );
    assert_eq!(image_dimensions(&derived_name, &derived), Some((500, 282)));
}

#[tokio::test]
async fn a_source_replaced_during_generation_is_not_committed() {
    let state = derivative_state();
    let source_name = "84-poster.png";
    let source_path = state.artwork_dir.join(source_name);
    write_test_image(&source_path, "color=c=red:s=640x360", &["-frames:v", "1"]);
    let stale_identity = open_local_artwork(source_path.clone())
        .await
        .expect("identity")
        .identity;
    let derived_dir = ensure_derived_dir(&state.artwork_dir)
        .await
        .expect("derived directory");
    let derived_name =
        derivative_filename([7_u8; 32], ArtworkSize::W300, source_name).expect("derived name");
    // Replace the source with a *decodable* image through the same
    // temp+rename install path a repair uses. Undecodable bytes would stop
    // the child on their own and prove nothing about the fence; these let
    // ffmpeg succeed, so only the identity check can refuse the rename.
    let replacement = state.artwork_dir.join("replacement.png");
    write_test_image(
        &replacement,
        "color=c=yellow:s=800x450",
        &["-frames:v", "1"],
    );
    let replacement_bytes = tokio::fs::read(&replacement).await.expect("replacement");
    plurx_core::metadata::write_artwork_atomically(&source_path, &replacement_bytes)
        .await
        .expect("replace source");
    generate_derivative(
        &state.artwork_fetch,
        DerivativeJob {
            runtime_cache: &state.runtime_cache_dir,
            source_path: &source_path,
            source_name,
            source_identity: stale_identity,
            derived_dir: &derived_dir,
            derived_name: &derived_name,
            size: ArtworkSize::W300,
        },
    )
    .await
    .expect_err("a replaced source must not commit a derivative under the old key");
    assert!(!derived_dir.join(&derived_name).exists());
}

#[tokio::test]
async fn an_exhausted_byte_budget_does_not_look_like_a_changed_source() {
    let state = derivative_state();
    let source_name = "84-poster.png";
    let source_path = state.artwork_dir.join(source_name);
    write_test_image(&source_path, "color=c=blue:s=640x360", &["-frames:v", "1"]);
    let source_identity = open_local_artwork(source_path.clone())
        .await
        .expect("identity")
        .identity;
    let digest: [u8; 32] = Sha256::digest(tokio::fs::read(&source_path).await.expect("src")).into();
    let derived_dir = ensure_derived_dir(&state.artwork_dir)
        .await
        .expect("derived directory");
    let derived_name =
        derivative_filename(digest, ArtworkSize::W300, source_name).expect("derived name");

    // Every local byte permit is spoken for, exactly as sixteen concurrent
    // backdrop requests would leave it. The post-generation fence must not
    // read the source of truth a second time here: the caller already holds
    // the admitted read that produced this key, and a refusal from a second
    // read is not evidence that anything changed.
    let held = state
        .artwork_fetch
        .local_permit(LOCAL_READ_BUDGET_KIB as u64 * 1024)
        .await
        .expect("hold the whole byte budget");
    generate_derivative(
        &state.artwork_fetch,
        DerivativeJob {
            runtime_cache: &state.runtime_cache_dir,
            source_path: &source_path,
            source_name,
            source_identity,
            derived_dir: &derived_dir,
            derived_name: &derived_name,
            size: ArtworkSize::W300,
        },
    )
    .await
    .expect("a saturated byte budget must not discard a correct derivative");
    assert!(
        derived_dir.join(&derived_name).is_file(),
        "the generation must commit and make forward progress"
    );
    drop(held);
}

#[test]
fn the_derivative_child_caches_in_the_runtime_cache_not_the_artwork_root() {
    // `XDG_CACHE_HOME` decides where libraries loaded by ffmpeg write their
    // bookkeeping. Pointed at the served artwork root it puts
    // library-created files inside the directory `sweep_content_orphans`
    // enumerates every pass; `state.rs` keeps `runtime_cache` separate
    // precisely so scratch cleanup cannot reach another managed root.
    let artwork = FsPath::new("/var/lib/plurx/artwork");
    let runtime_cache = FsPath::new("/var/lib/plurx/runtime");
    let command = derivative_command(
        "ffmpeg",
        &artwork.join("84-poster.jpg"),
        runtime_cache,
        ArtworkSize::W300,
        "jpg",
        "mjpeg",
    );
    let cache_home = command
        .as_std()
        .get_envs()
        .find(|(name, _)| *name == std::ffi::OsStr::new("XDG_CACHE_HOME"))
        .and_then(|(_, value)| value)
        .map(std::path::PathBuf::from)
        .expect("the child is given a cache home");
    assert_eq!(cache_home, runtime_cache.join("gpu-cache"));
    assert_ne!(cache_home, artwork);
}

#[tokio::test]
async fn the_derivative_child_leaves_nothing_in_the_served_artwork_root() {
    // `XDG_CACHE_HOME` must be the dedicated runtime cache, never the
    // directory `sweep_content_orphans` enumerates every pass.
    let state = derivative_state();
    let source_name = "84-poster.png";
    let source_path = state.artwork_dir.join(source_name);
    write_test_image(&source_path, "color=c=green:s=640x360", &["-frames:v", "1"]);
    let response = serve_derivative(&state, source_name, &HeaderMap::new(), ArtworkSize::W300)
        .await
        .expect("derivative");
    assert_eq!(response.status(), StatusCode::OK);
    drop(response);
    assert!(worker::run_one(&state).await.expect("worker pass"));
    let entries = std::fs::read_dir(&state.artwork_dir)
        .expect("artwork entries")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        entries,
        BTreeSet::from([source_name.to_owned(), DERIVED_DIR.to_owned()]),
        "nothing but the source and the derivative store belongs in the artwork root"
    );
}

pub(super) async fn age_past_the_orphan_grace(path: &FsPath) {
    let old = std::time::SystemTime::now()
        .checked_sub(CONTENT_ORPHAN_GRACE + Duration::from_secs(60))
        .expect("aged timestamp");
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open candidate")
        .set_times(std::fs::FileTimes::new().set_modified(old))
        .expect("age candidate");
}

#[tokio::test]
async fn orphaned_derivatives_are_swept_after_grace() {
    let state = derivative_state();
    let library = state
        .store
        .create_library(&plurx_core::domain::NewLibrary {
            name: "Films".into(),
            kind: plurx_core::domain::LibraryKind::Movies,
            paths: vec!["/films".into()],
            anime: false,
        })
        .await
        .expect("library");
    let mut item_ids = Vec::new();
    for title in ["Live backdrop", "Unverified legacy", "Verified legacy"] {
        item_ids.push(
            state
                .store
                .insert_item(&plurx_core::domain::NewItem {
                    library_id: library.id,
                    kind: plurx_core::domain::ItemKind::Movie,
                    parent_id: None,
                    title: title.into(),
                    year: Some(2024),
                    season_number: None,
                    episode_number: None,
                })
                .await
                .expect("item"),
        );
    }

    // A content-addressed backdrop: the family that publishes only sixteen
    // hex characters, and the one every TV hero uses.
    let backdrop_bytes = b"live backdrop bytes";
    let backdrop_digest: [u8; 32] = Sha256::digest(backdrop_bytes).into();
    let backdrop = format!(
        "{}-backdrop-c{}.jpg",
        item_ids[0],
        &hex::encode(backdrop_digest)[..16]
    );
    // A legacy source nothing has verified since boot.
    let unverified = format!("{}-poster.jpg", item_ids[1]);
    // A legacy source whose bytes have been verified, and whose derivative
    // was made from a *previous* generation of those bytes.
    let verified = format!("{}-poster.jpg", item_ids[2]);
    let verified_bytes = b"current legacy poster bytes";
    let verified_digest: [u8; 32] = Sha256::digest(verified_bytes).into();

    for (item_id, poster, backdrop_path) in [
        (item_ids[0], None, Some(backdrop.clone())),
        (item_ids[1], Some(unverified.clone()), None),
        (item_ids[2], Some(verified.clone()), None),
    ] {
        state
            .store
            .apply_metadata(
                item_id,
                &plurx_core::domain::MetadataPatch {
                    poster_path: poster,
                    backdrop_path,
                    ..Default::default()
                },
            )
            .await
            .expect("patch");
    }

    tokio::fs::write(state.artwork_dir.join(&verified), verified_bytes)
        .await
        .expect("verified source");
    let identity = open_local_artwork(state.artwork_dir.join(&verified))
        .await
        .expect("identity")
        .identity;
    state
        .artwork_fetch
        .remember(&verified, identity, verified_digest)
        .await;

    let derived_dir = ensure_derived_dir(&state.artwork_dir)
        .await
        .expect("derived directory");
    let live_backdrop = derivative_filename(backdrop_digest, ArtworkSize::W780, &backdrop)
        .expect("backdrop derivative");
    let live_unverified = derivative_filename([1_u8; 32], ArtworkSize::W300, &unverified)
        .expect("unverified derivative");
    let live_verified = derivative_filename(verified_digest, ArtworkSize::W500, &verified)
        .expect("verified derivative");
    let stale_verified =
        derivative_filename([2_u8; 32], ArtworkSize::W500, &verified).expect("stale derivative");
    let unreferenced = derivative_filename([3_u8; 32], ArtworkSize::W300, "999-poster.jpg")
        .expect("unreferenced derivative");
    let stranded_temporary = format!(".{live_backdrop}.{}.tmp", uuid::Uuid::new_v4().simple());
    let superseded = format!("{}-w300.jpg", "4".repeat(32));

    let kept = [&live_backdrop, &live_unverified, &live_verified];
    let removed = [
        &stale_verified,
        &unreferenced,
        &stranded_temporary,
        &superseded,
    ];
    for name in kept.iter().chain(removed.iter()) {
        let path = derived_dir.join(name.as_str());
        tokio::fs::write(&path, b"derived bytes")
            .await
            .expect("derivative");
        age_past_the_orphan_grace(&path).await;
    }

    assert_eq!(
        sweep_derived_orphans(&state).await,
        removed.len(),
        "exactly the dead derivatives are reclaimed"
    );
    for name in kept {
        assert!(
            derived_dir.join(name.as_str()).is_file(),
            "a live derivative was swept: {name}"
        );
    }
    for name in removed {
        assert!(
            !derived_dir.join(name.as_str()).exists(),
            "a dead entry survived: {name}"
        );
    }

    // Nothing is removed before the grace, and a fresh derivative of a
    // source with no verified digest is not an orphan either.
    let fresh =
        derivative_filename([5_u8; 32], ArtworkSize::W300, &unverified).expect("fresh derivative");
    tokio::fs::write(derived_dir.join(&fresh), b"fresh")
        .await
        .expect("fresh derivative");
    assert_eq!(sweep_derived_orphans(&state).await, 0);
    assert!(derived_dir.join(&fresh).is_file());
}
