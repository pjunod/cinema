use super::*;
fn artwork_test_address(n: u32) -> std::net::IpAddr {
    std::net::IpAddr::V4(std::net::Ipv4Addr::from(0x0a00_0000 + n))
}
#[test]
fn jellyfin_artwork_budget_keys_ipv6_per_64_and_ipv4_per_address() {
    let mut budget = JellyfinArtworkBudget::default();
    let now = Instant::now();
    let host: std::net::IpAddr = "2001:db8:1:2::1".parse().expect("v6");
    let same_64: std::net::IpAddr = "2001:db8:1:2:ffff::9".parse().expect("v6");
    let other_64: std::net::IpAddr = "2001:db8:1:3::1".parse().expect("v6");
    for _ in 0..20 {
        assert!(budget.admit(host, now));
    }
    assert!(
        !budget.admit(same_64, now),
        "one /64 shares one miss budget"
    );
    assert!(budget.admit(other_64, now));
    for v4 in ["192.0.2.1", "192.0.2.2"] {
        assert!(budget.admit(v4.parse().expect("v4"), now));
    }
    let mapped: std::net::IpAddr = "::ffff:192.0.2.1".parse().expect("mapped");
    assert_eq!(
        artwork_budget_key(mapped),
        "192.0.2.1".parse::<std::net::IpAddr>().expect("v4")
    );
}
#[tokio::test]
async fn jellyfin_artwork_cold_demands_deduplicate_without_hashing_or_decoding_originals() {
    let state = super::tests::derivative_state();
    let name = "1-poster-c123456789abcdef0.png";
    tokio::fs::create_dir_all(&state.artwork_dir)
        .await
        .expect("fixture directory");
    tokio::fs::write(
        state.artwork_dir.join(name),
        b"synthetic bytes that must not be decoded inline",
    )
    .await
    .expect("fixture source");
    let mapped = plurx_core::store::JellyfinCatalogArtwork {
        wire_id: uuid::Uuid::new_v4().simple().to_string(),
        filename: name.into(),
        generation: uuid::Uuid::new_v4().simple().to_string(),
    };
    for _ in 0..4 {
        let response = serve_jellyfin_artwork(
            &state,
            mapped.clone(),
            false,
            500,
            &HeaderMap::new(),
            artwork_test_address(0),
        )
        .await
        .expect("cold request");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    }
    assert_eq!(state.artwork_fetch.hashes.load(Ordering::SeqCst), 0);
    assert_eq!(state.artwork_fetch.derivations.load(Ordering::SeqCst), 0);
    assert_eq!(state.artwork_fetch.jellyfin_demands.lock().await.len(), 1);
    // Admission is refused at the fixed queue bound; existing work remains.
    for n in 1..256u32 {
        let mut another = mapped.clone();
        another.wire_id = uuid::Uuid::new_v4().simple().to_string();
        serve_jellyfin_artwork(
            &state,
            another,
            false,
            500,
            &HeaderMap::new(),
            artwork_test_address(n),
        )
        .await
        .expect("bounded demand");
    }
    let mut overflow = mapped;
    overflow.wire_id = uuid::Uuid::new_v4().simple().to_string();
    assert!(serve_jellyfin_artwork(
        &state,
        overflow,
        false,
        500,
        &HeaderMap::new(),
        artwork_test_address(0)
    )
    .await
    .is_err());
    assert_eq!(state.artwork_fetch.jellyfin_demands.lock().await.len(), 256);
    assert_eq!(state.artwork_fetch.hashes.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn jellyfin_artwork_owner_discards_queued_demand_after_switch_generation_changes() {
    use plurx_core::domain::{ItemKind, LibraryKind, MetadataPatch, NewItem, NewLibrary};
    use plurx_core::store::JellyfinEntityKind;
    let state = super::tests::derivative_state();
    let library = state
        .store
        .create_library(&NewLibrary {
            name: "Mapped artwork".into(),
            kind: LibraryKind::Movies,
            paths: vec!["/synthetic".into()],
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
            title: "Mapped".into(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    state
        .store
        .apply_metadata(
            item,
            &MetadataPatch {
                poster_path: Some("1-poster-c123456789abcdef0.png".into()),
                ..Default::default()
            },
        )
        .await
        .expect("poster mapping");
    let wire = state
        .store
        .jellyfin_entity_ids(JellyfinEntityKind::Item, &[item])
        .await
        .expect("wire")[0]
        .wire_id
        .clone();
    state
        .store
        .set_jellyfin_compatibility(true)
        .await
        .expect("enable");
    let old = state
        .store
        .jellyfin_catalog_artwork(wire.clone(), false)
        .await
        .expect("mapping")
        .expect("enabled mapping");
    serve_jellyfin_artwork(
        &state,
        old.clone(),
        false,
        500,
        &HeaderMap::new(),
        artwork_test_address(0),
    )
    .await
    .expect("queued miss");
    assert_eq!(state.artwork_fetch.jellyfin_demands.lock().await.len(), 1);
    state
        .store
        .set_jellyfin_compatibility(false)
        .await
        .expect("disable");
    state
        .store
        .set_jellyfin_compatibility(true)
        .await
        .expect("reenable");
    let current = state
        .store
        .jellyfin_catalog_artwork(wire, false)
        .await
        .expect("mapping")
        .expect("new mapping");
    assert_ne!(old.generation, current.generation);
    assert!(jellyfin_artwork_pass(&state).await);
    assert!(state.artwork_fetch.jellyfin_demands.lock().await.is_empty());
    assert_eq!(state.artwork_fetch.hashes.load(Ordering::SeqCst), 0);
    assert_eq!(state.artwork_fetch.derivations.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn jellyfin_artwork_native_owner_publishes_warm_private_revalidated_derivatives() {
    use plurx_core::domain::{ItemKind, LibraryKind, MetadataPatch, NewItem, NewLibrary};
    use plurx_core::store::JellyfinEntityKind;
    let state = super::tests::derivative_state();
    let library = state
        .store
        .create_library(&NewLibrary {
            name: "Mapped artwork".into(),
            kind: LibraryKind::Movies,
            paths: vec!["/synthetic".into()],
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
            title: "Mapped".into(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    state
        .store
        .apply_metadata(
            item,
            &MetadataPatch {
                poster_path: Some("1-poster.png".into()),
                ..Default::default()
            },
        )
        .await
        .expect("poster mapping");
    let wire = state
        .store
        .jellyfin_entity_ids(JellyfinEntityKind::Item, &[item])
        .await
        .expect("wire")[0]
        .wire_id
        .clone();
    state
        .store
        .set_jellyfin_compatibility(true)
        .await
        .expect("enable");
    let old = state
        .store
        .jellyfin_catalog_artwork(wire.clone(), false)
        .await
        .expect("mapping")
        .expect("enabled mapping");
    super::tests::write_test_image(
        &state.artwork_dir.join(&old.filename),
        "testsrc2=size=960x540",
        &["-frames:v", "1"],
    );
    serve_jellyfin_artwork(
        &state,
        old.clone(),
        false,
        500,
        &HeaderMap::new(),
        artwork_test_address(0),
    )
    .await
    .expect("cold miss");
    assert_eq!(state.artwork_fetch.hashes.load(Ordering::SeqCst), 0);
    assert_eq!(state.artwork_fetch.derivations.load(Ordering::SeqCst), 0);
    assert!(jellyfin_artwork_pass(&state).await);
    assert!(state.artwork_fetch.jellyfin_demands.lock().await.is_empty());
    assert!(worker::run_one(&state)
        .await
        .expect("native derivative owner"));
    let derivations = state.artwork_fetch.derivations.load(Ordering::SeqCst);
    let warm = serve_jellyfin_artwork(
        &state,
        old.clone(),
        false,
        500,
        &HeaderMap::new(),
        artwork_test_address(0),
    )
    .await
    .expect("warm derivative");
    assert_eq!(warm.status(), StatusCode::OK);
    assert_eq!(
        warm.headers()[header::CACHE_CONTROL],
        "private, max-age=0, must-revalidate"
    );
    let etag = warm.headers()[header::ETAG].clone();
    assert!(!axum::body::to_bytes(warm.into_body(), 8 * 1024 * 1024)
        .await
        .expect("warm bytes")
        .is_empty());
    // Native serving verifies the published derivative on its first read.
    // Subsequent validator hits must not hash either source or derivative.
    let hashes = state.artwork_fetch.hashes.load(Ordering::SeqCst);
    let mut conditional = HeaderMap::new();
    conditional.insert(header::IF_NONE_MATCH, etag);
    let unchanged = serve_jellyfin_artwork(
        &state,
        old,
        false,
        500,
        &conditional,
        artwork_test_address(0),
    )
    .await
    .expect("conditional derivative");
    assert_eq!(unchanged.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(
        unchanged.headers()[header::CACHE_CONTROL],
        "private, max-age=0, must-revalidate"
    );
    assert_eq!(state.artwork_fetch.hashes.load(Ordering::SeqCst), hashes);
    assert_eq!(
        state.artwork_fetch.derivations.load(Ordering::SeqCst),
        derivations
    );
    assert!(state.artwork_fetch.jellyfin_demands.lock().await.is_empty());
}
#[test]
fn jellyfin_artwork_budget_bounds_addresses_without_evicting_active_windows() {
    let mut budget = JellyfinArtworkBudget::default();
    let now = Instant::now();
    let address = std::net::IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 2, 1));
    for _ in 0..20 {
        assert!(budget.admit(address, now));
    }
    assert!(!budget.admit(address, now));
    for n in 1..4096 {
        assert!(budget.admit(std::net::IpAddr::V4(std::net::Ipv4Addr::from(n)), now));
    }
    assert_eq!(budget.addresses.len(), 4096);
    assert!(!budget.admit(std::net::IpAddr::V4(std::net::Ipv4Addr::from(5000)), now));
    assert!(!budget.admit(address, now + Duration::from_secs(59)));
    assert!(budget.admit(address, now + Duration::from_secs(60)));
    assert!(budget.admit(
        std::net::IpAddr::V4(std::net::Ipv4Addr::from(5000)),
        now + Duration::from_secs(60)
    ));
    assert_eq!(budget.addresses.len(), 2);
}
