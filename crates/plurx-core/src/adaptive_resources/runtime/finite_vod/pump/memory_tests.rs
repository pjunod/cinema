use super::*;

fn identity() -> InitIdentity {
    InitIdentity {
        muxer_init: "a".repeat(IDENTITY_DIGEST_BYTES),
        served_init: "b".repeat(IDENTITY_DIGEST_BYTES),
        promotion: PromotionInputs::default(),
    }
}

#[tokio::test]
async fn fixed_mailbox_preserves_backpressure_and_receives_pending_event_after_writer_closes() {
    use tokio::sync::mpsc::error::TryRecvError;

    let (sender, mut reader) = events::channel();
    let materialized = |entry| SoftwareVodEvent::Materialized {
        entry,
        bytes: 1,
        digest: [0; 32],
    };
    sender.send(materialized(0)).await.expect("first event");
    let second = sender.send(materialized(1));
    tokio::pin!(second);
    assert!(futures_util::poll!(&mut second).is_pending());
    assert!(matches!(
        reader.try_recv(),
        Ok(SoftwareVodEvent::Materialized { entry: 0, .. })
    ));
    second.await.expect("space in same mailbox");

    assert!(matches!(
        reader.recv().await,
        Some(SoftwareVodEvent::Materialized { entry: 1, .. })
    ));
    sender.send(materialized(2)).await.expect("final event");
    drop(reader);
    assert!(sender.send(materialized(3)).await.is_err());

    let (sender, mut reader) = events::channel();
    sender.send(materialized(4)).await.expect("original event");
    drop(sender);
    assert!(matches!(
        reader.recv().await,
        Some(SoftwareVodEvent::Materialized { entry: 4, .. })
    ));
    assert!(matches!(reader.try_recv(), Err(TryRecvError::Disconnected)));
}

#[test]
fn initialized_event_has_one_bounded_clone_without_promotion_backing() {
    let original = identity();
    let SoftwareVodEvent::Initialized(cloned) = initialized_event(&original).expect("AVC identity")
    else {
        panic!("initialization event");
    };
    assert_eq!(original, cloned);
    assert_ne!(original.muxer_init.as_ptr(), cloned.muxer_init.as_ptr());
    assert_ne!(original.served_init.as_ptr(), cloned.served_init.as_ptr());
    assert_eq!(original.requested_heap_bytes(), Some(128));
    assert_eq!(cloned.requested_heap_bytes(), Some(128));
    assert_eq!(cloned.promotion.requested_heap_bytes(), Some(0));
}

#[test]
fn rich_or_oversized_identity_is_refused_before_event_clone() {
    let mut rich = identity();
    rich.promotion.parameter_sets.push(vec![0; 1024]);
    assert!(initialized_event(&rich).is_err());
    assert_eq!(rich.promotion.parameter_sets[0].len(), 1024);
    let mut oversized = identity();
    oversized.muxer_init.push('a');
    assert!(initialized_event(&oversized).is_err());
    let mut retained_capacity = identity();
    retained_capacity.served_init.reserve(1024);
    assert!(initialized_event(&retained_capacity).is_err());
    let mut empty_backing = identity();
    empty_backing.promotion.hdr10_sei.reserve(1);
    assert_eq!(empty_backing.promotion, PromotionInputs::default());
    assert!(initialized_event(&empty_backing).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn fixed_mailbox_does_not_certify_the_producer_peak() {
    let plan = super::super::tests::pump_plan();
    assert_eq!(
        plan.memory_claim(),
        crate::adaptive_resources::MemoryClaim::UnknownProducerPeak
    );
    assert!(
        plan.minimum_memory_bytes().expect("requested component")
            >= initial_event_requested_bytes().expect("fixed mailbox") as u64
    );
}
