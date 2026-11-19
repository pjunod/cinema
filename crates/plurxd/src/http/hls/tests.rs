/// Prepare the installed encoder before tests that exercise catalog selection
/// under a request deadline. The production lookups still verify the actual
/// executable and runtime closure when the request runs.
pub(super) async fn prepare_catalog_test_encoder() {
    crate::ffmpeg::has_dovi_rpu().await;
    crate::ffmpeg::EncodedExecutable::capture()
        .await
        .expect("catalog fixture requires an identifiable FFmpeg executable");
    crate::ffmpeg::EncodedEngine::capture(None)
        .await
        .expect("catalog fixture requires an attested FFmpeg runtime");
}

include!("tests/chunk_01.rs");
include!("tests/chunk_02.rs");
include!("tests/chunk_03.rs");
include!("tests/chunk_04.rs");
include!("tests/chunk_05.rs");
include!("tests/chunk_06.rs");

include!("tests/cached_owner_delivery.rs");
include!("tests/authority_recovery.rs");
include!("tests/processed_upgrade_runs.rs");
include!("tests/route_view.rs");
