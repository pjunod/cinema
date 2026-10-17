use super::*;
use crate::playback_principal::{PlaybackPrincipal, SharingViewerKey};

#[test]
fn sharing_background_consumer_keys_preserve_local_retries_and_isolate_grants_and_viewers() {
    let mut interest = AnalysisViewerInterest {
        analysis_request_id: "analysis".into(),
        requested_generation: "generation".into(),
        pipeline_version: "pipeline".into(),
        video_identity: "video".into(),
        target_node_id: "node".into(),
        principal: PlaybackPrincipal::LocalUser { user_id: 7 },
        playback_id: "playback".into(),
        now_ms: 1,
    };
    let local = interest.consumer_id();
    assert_eq!(
        local,
        crate::segplan::argv_fingerprint(&[
            "playback-analysis-v1".into(),
            "7".into(),
            "playback".into(),
            "analysis".into()
        ])
    );
    let grant = uuid::Uuid::new_v4();
    interest.principal = PlaybackPrincipal::Sharing {
        grant_id: grant,
        viewer_key: SharingViewerKey::parse(&"a".repeat(64)).expect("canonical viewer"),
    };
    let first = interest.consumer_id();
    assert_ne!(local, first);
    interest.principal = PlaybackPrincipal::Sharing {
        grant_id: grant,
        viewer_key: SharingViewerKey::parse(&"b".repeat(64)).expect("canonical viewer"),
    };
    assert_ne!(first, interest.consumer_id());
    interest.principal = PlaybackPrincipal::Sharing {
        grant_id: uuid::Uuid::new_v4(),
        viewer_key: SharingViewerKey::parse(&"a".repeat(64)).expect("canonical viewer"),
    };
    assert_ne!(first, interest.consumer_id());
    let mut artifact = ArtifactViewerInterest {
        cache_key: "c".repeat(64),
        file_id: 1,
        target_node_id: "node".into(),
        principal: PlaybackPrincipal::LocalUser { user_id: 7 },
        playback_id: "playback".into(),
        now_ms: 1,
    };
    assert_eq!(
        artifact.consumer_id(),
        crate::segplan::argv_fingerprint(&[
            "playback-artifact-v1".into(),
            "7".into(),
            "playback".into(),
            "c".repeat(64),
            "node".into()
        ])
    );
    let old = artifact.consumer_id();
    artifact.principal = interest.principal;
    assert_ne!(old, artifact.consumer_id());
}
