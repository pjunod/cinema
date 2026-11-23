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
    recipe.audio_excluded = false;
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
