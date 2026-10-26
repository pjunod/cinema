use super::*;

fn identity() -> InitIdentity {
    InitIdentity {
        muxer_init: "a".repeat(IDENTITY_DIGEST_BYTES),
        served_init: "b".repeat(IDENTITY_DIGEST_BYTES),
        promotion: PromotionInputs::default(),
    }
}

#[test]
fn initial_event_payload_tracks_pinned_tokio_slots_and_exceeds_old_allowance() {
    let lock = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.lock"));
    let tokio = lock
        .split("[[package]]")
        .find(|package| package.lines().any(|line| line == r#"name = "tokio""#))
        .expect("pinned Tokio package");
    assert!(
        tokio.lines().any(|line| line == r#"version = "1.53.1""#),
        "re-audit private initial block cardinality when Tokio changes"
    );
    let slots = if usize::BITS == 64 { 32 } else { 16 };
    assert_eq!(INITIAL_EVENT_BLOCK_SLOTS, slots);
    assert!(slots > EVENT_CHANNEL_CAPACITY);
    let payload = slots * size_of::<SoftwareVodEvent>();
    assert_eq!(initial_event_requested_bytes(), Some(payload + 256));
    #[cfg(target_pointer_width = "64")]
    assert!(
        payload > 8192,
        "one initial block already exceeds the removed allowance"
    );
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
fn numeric_pump_component_cannot_certify_channel_or_native_peak() {
    let plan = super::super::tests::pump_plan();
    assert_eq!(
        plan.memory_claim(),
        crate::adaptive_resources::MemoryClaim::UnknownProducerPeak
    );
    assert!(
        plan.minimum_memory_bytes().expect("requested component")
            >= initial_event_requested_bytes().expect("initial payload") as u64
    );
}
