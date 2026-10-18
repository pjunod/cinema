use super::*;
use plurx_core::testfixtures;

/// The equality the whole landing mechanism rests on, asserted by
/// `cargo test` rather than by a script that ran once.
///
/// M0 §12.2 measured that the video-only index pipe and the full
/// production pipe agree fragment for fragment. Not on everything: the
/// wire length differs by tens of kilobytes because the production pipe
/// carries audio. What matches is the video sample bytes, and that is what
/// `segplan::match_landing` compares — so if this test ever fails, a
/// repositioned producer can no longer be placed and every far seek turns
/// into a typed producer failure.
#[tokio::test]
async fn the_index_records_the_quantity_a_production_generation_reproduces() {
    for kind in ["closed-gop", "open-gop", "clean-cra", "h264"] {
        let IndexOutcome::Built(index) = index_stream(
            std::io::Cursor::new(super::tests::index_pipe_bytes(kind)),
            SourceIdentity::new(1, 1, "fingerprint"),
            None,
            DolbyVisionPass::Untouched,
        )
        .await
        else {
            panic!("{kind}: the index pipe must index");
        };

        // The production pipe, audio and all — what a repositioned
        // generation really emits. It cannot go through `index_stream`,
        // which refuses a non-video track on purpose, so it is read
        // directly here.
        let (produced_video, produced_wire) = production_fragments(kind);

        assert_eq!(
            index.rows.len(),
            produced_video.len(),
            "{kind}: fragment counts must agree"
        );
        let video: Vec<u32> = index.rows.iter().map(|row| row.video_bytes).collect();
        assert_eq!(
            video, produced_video,
            "{kind}: video sample bytes are what the landing matcher \
                 compares, so they must be identical across audio branches"
        );

        let wire: Vec<u32> = index.rows.iter().map(|row| row.bytes).collect();
        assert_ne!(
            wire, produced_wire,
            "{kind}: if the wire lengths ever DID agree, the fixture stopped \
                 carrying audio and this test stopped proving anything"
        );
    }
}

/// The production pipe's per-fragment (video sample bytes, wire length).
fn production_fragments(kind: &str) -> (Vec<u32>, Vec<u32>) {
    let bytes = testfixtures::pipe(kind);
    let mut reader = FragmentReader::new();
    reader.push(&bytes);
    let mut init: Option<Init> = None;
    let mut video = Vec::new();
    let mut wire = Vec::new();
    while let Ok(Some(unit)) = reader.next_unit() {
        match unit {
            Unit::Init(parsed) => init = Some(parsed),
            Unit::Fragment(fragment) => {
                let init = init.as_ref().expect("moov before fragments");
                let track_id = init.video().expect("a video track").id;
                let track = fragment.track(track_id).expect("video in the fragment");
                video.push(u32::try_from(track.byte_len()).unwrap_or(u32::MAX));
                wire.push(u32::try_from(fragment.len()).unwrap_or(u32::MAX));
            }
            Unit::Trailer => {}
        }
    }
    (video, wire)
}
