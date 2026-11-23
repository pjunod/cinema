// The shared video of a split copy presentation: one key per source and
// video recipe, whatever soundtrack, offset or audio conversion a viewer has.

#[tokio::test]
async fn split_copy_video_key_ignores_every_audio_choice_but_muxed_copy_still_forks() {
    let base = crate::test_tempdir().expect("split key fixture");
    let rendition = synthetic_rendition(base.path()).await;
    let mut muxed = rendition.recipe.clone();
    let identity = crate::fragindex::identity_for(&muxed.file, muxed.video);
    muxed.audio_excluded = false;
    muxed.audio_index = Some(1);
    let english = rendition_key(&muxed, &identity);
    muxed.audio_index = Some(2);
    let french = rendition_key(&muxed, &identity);
    assert_ne!(english, french, "a muxed copy is still one rendition per soundtrack");

    let mut video = muxed.clone();
    video.audio_excluded = true;
    let shared = rendition_key(&video, &identity);
    for (audio_index, aac, offset) in [(Some(1), false, 0), (Some(2), true, 0), (None, false, 250), (Some(2), false, -120)] {
        video.audio_index = audio_index;
        video.aac = aac;
        video.file.audio_offset_ms = offset;
        assert_eq!(rendition_key(&video, &identity), shared, "{audio_index:?} {aac} {offset}");
    }
    assert_ne!(shared, english);
    assert_ne!(shared, french);
    // The video recipe itself still forks the shared video.
    video.video = video.video.with_dolby_vision_conversion(true);
    assert_ne!(rendition_key(&video, &identity), shared);
}

#[tokio::test]
async fn split_copy_video_plan_has_no_audio_tail_or_audio_bytes() {
    let base = crate::test_tempdir().expect("split plan fixture");
    let rendition = synthetic_rendition(base.path()).await;
    let mut recipe = rendition.recipe.clone();
    let index = synthetic_index(24);
    let video_ms = index_video_ms(&index);
    recipe.audio_excluded = true;
    let tracks = track_durations(&index, &recipe, video_ms + 3_000);
    assert_eq!(tracks.audio_ms, tracks.video_ms);
    assert_eq!(tracks.audio_bits_per_second, 0);
    // A muxed copy that maps real audio plans the container's audio tail; a
    // silent source would end with indexed video either way.
    recipe.audio_excluded = false;
    recipe.file.audio_streams.push(plurx_core::domain::AudioStream {
        codec: "aac".into(),
        channels: Some(2),
        ..Default::default()
    });
    assert_eq!(track_durations(&index, &recipe, video_ms + 3_000).audio_ms, video_ms + 3_000);
}

#[test]
fn split_copy_producer_maps_video_only() {
    let mut recipe_file = media_file_at(PathBuf::from("/media/film.mkv"), 0);
    recipe_file.video_codec = Some("h264".into());
    recipe_file.audio_offset_ms = 400;
    recipe_file.audio_streams = vec![
        plurx_core::domain::AudioStream { codec: "aac".into(), index: 1, ..Default::default() },
        plurx_core::domain::AudioStream { codec: "aac".into(), index: 2, ..Default::default() },
    ];
    let args = plurx_core::transcode::copy_video_only_pipe_args(
        &recipe_file,
        0.0,
        plurx_core::transcode::Pacing::unpaced(),
        plurx_core::transcode::CopyVideoOptions::new(false, false),
    );
    assert!(args.iter().any(|arg| arg == "-an"));
    assert_eq!(args.iter().filter(|arg| *arg == "-i").count(), 1);
}

// P49 AC-5: stale identity. Its soundtracks are bound to the exact source
// object; the shared video must be too, or a same-path replacement that keeps
// size and mtime would adopt the old file's pictures.
#[tokio::test]
async fn split_copy_video_key_binds_the_exact_source_object() {
    let base = crate::test_tempdir().expect("split identity fixture");
    let rendition = synthetic_rendition(base.path()).await;
    let mut video = rendition.recipe.clone();
    video.audio_excluded = true;
    video.file.video_codec = Some("h264".into());
    video.source_object_version = Some("object:1:10:4096:100:0:100:0".into());
    let key = |recipe: &Recipe| {
        rendition_key(recipe, &crate::fragindex::identity_for(&recipe.file, recipe.video))
    };
    let shared = key(&video);

    let mut replaced = video.clone();
    replaced.source_object_version = Some("object:1:11:4096:100:0:200:0".into());
    assert_ne!(key(&replaced), shared, "a replaced object with the same size and mtime");
    let mut resized = video.clone();
    resized.file.size += 1;
    assert_ne!(key(&resized), shared, "a resized source");
    let mut touched = video.clone();
    touched.file.mtime += 1;
    assert_ne!(key(&touched), shared, "a rewritten source");
    let mut other = video.clone();
    other.file.id += 1;
    assert_ne!(key(&other), shared, "another catalogue file");

    // Language never forks it.
    let mut french = video.clone();
    french.audio_index = Some(2);
    assert_eq!(key(&french), shared);
}

#[test]
fn split_family_requires_one_source_object() {
    use super::vod_serve_split::split_family_shares_source;
    assert!(split_family_shares_source(Some("v"), ["v", "v"]));
    assert!(!split_family_shares_source(Some("v"), ["v", "w"]), "a soundtrack from another revision");
    assert!(!split_family_shares_source(None, ["v"]), "a video with no proven source");
}

// P49 AC-5: disk-full. A full cache refuses the shared video's segment before
// any byte lands, and the retry after room converges to one committed object.
#[tokio::test]
async fn split_copy_video_refuses_a_full_cache_without_residue_then_converges() {
    use crate::vodgen::Sink;
    let base = crate::test_tempdir().expect("disk-full fixture");
    let serve = bare_serve(base.path());
    let budget = crate::playback_cache::PlaybackCacheBudget::new(
        Arc::clone(&serve.shared.store),
        vec![base.path().to_path_buf()],
    );
    budget.configure_for_test(64 << 10);
    let mut rendition = synthetic_rendition(base.path()).await;
    Arc::get_mut(&mut rendition).expect("unshared fixture").recipe.audio_excluded = true;
    let sink = RenditionSink {
        retirement: tokio_util::sync::CancellationToken::new(),
        shared: Arc::clone(&serve.shared),
        rendition: Arc::clone(&rendition),
        epoch: 0,
    };
    let segment = rendition.dir.path().join(segment_name(0));
    let temporary = rendition.dir.path().join(format!("{}.tmp", segment_name(0)));
    let refused = sink.materialize(0, vec![7; 128 << 10]).await.expect_err("over the cache");
    assert_eq!(refused.kind(), io::ErrorKind::StorageFull, "{refused}");
    assert!(!segment.exists() && !temporary.exists(), "nothing partial after a full cache");
    assert!(!rendition.manifest.lock().await.state(0).is_some_and(|state| state.is_materialized()));

    budget.configure_for_test(1 << 20);
    sink.materialize(0, vec![7; 128 << 10]).await.expect("retry with room");
    assert_eq!(std::fs::read(&segment).expect("committed").len(), 128 << 10);
    assert!(!temporary.exists());
    assert!(rendition.manifest.lock().await.state(0).is_some_and(|state| state.is_materialized()));
}

// P49 AC-5: crash-before-rename. A segment whose bytes reached its temporary
// but were never renamed is not adopted at restart; the retry replaces it with
// one committed object, and the segment a viewer is reading is untouched.
#[tokio::test]
async fn split_copy_video_converges_after_a_crash_before_rename_without_touching_a_reader() {
    use crate::vodgen::Sink;
    let base = crate::test_tempdir().expect("crash fixture");
    let serve = bare_serve(base.path());
    let mut rendition = synthetic_rendition(base.path()).await;
    Arc::get_mut(&mut rendition).expect("unshared fixture").recipe.audio_excluded = true;
    let sink = RenditionSink {
        retirement: tokio_util::sync::CancellationToken::new(),
        shared: Arc::clone(&serve.shared),
        rendition: Arc::clone(&rendition),
        epoch: 0,
    };
    sink.materialize(0, vec![1; 4096]).await.expect("committed segment");
    rendition.attach_reader("english", 0).await;
    let crashed = rendition.dir.path().join(format!("{}.tmp", segment_name(1)));
    std::fs::write(&crashed, vec![9; 100]).expect("bytes that never reached their name");

    {
        let mut manifest = rendition.manifest.lock().await;
        rendition.dir.reconcile(&mut manifest, 0).await.expect("restart recovery");
        assert!(manifest.state(0).is_some_and(|state| state.is_materialized()));
        assert!(
            !manifest.state(1).is_some_and(|state| state.is_materialized()),
            "a temporary is never adopted"
        );
    }
    sink.materialize(1, vec![2; 4096]).await.expect("retry after the crash");
    assert!(!crashed.exists(), "the retry's rename consumed the crash residue");
    assert_eq!(std::fs::read(rendition.dir.path().join(segment_name(1))).expect("seg 1"), vec![2; 4096]);
    assert_eq!(std::fs::read(rendition.dir.path().join(segment_name(0))).expect("seg 0"), vec![1; 4096]);
    rendition.detach_media_reader(&serve.shared.pool, "english").await;
}
