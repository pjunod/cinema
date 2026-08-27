//! Backend-neutral behavioral contract for the durable store boundary.
//!
//! Every scenario receives only `Arc<dyn Store>` and runs against both SQLite
//! modes. With `hiqlite-contract-tests`, the same scenarios also run through a
//! remote client backed by three separate voter processes.

#[cfg(feature = "hiqlite-contract-tests")]
use std::borrow::Cow;
use std::collections::BTreeSet;
use std::future::Future;
#[cfg(feature = "hiqlite-contract-tests")]
use std::io::{BufRead, BufReader, Write};
#[cfg(feature = "hiqlite-contract-tests")]
use std::net::TcpListener;
#[cfg(all(feature = "hiqlite-contract-tests", unix))]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
#[cfg(feature = "hiqlite-contract-tests")]
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(feature = "hiqlite-contract-tests")]
use hiqlite::tls::ServerTlsConfig;
#[cfg(feature = "hiqlite-contract-tests")]
use hiqlite::{Client, Node, NodeConfig, Row};
use plurx_core::cluster::coordination::{Lease, LeaseClaim};
#[cfg(feature = "hiqlite-contract-tests")]
use plurx_core::cluster::migration::{
    connect_activated_store, prepare_sqlite_import, select_daemon_store, ActivationMarker,
    SelectedBackend, ACTIVATED_SOURCE_FILENAME, ACTIVATION_MARKER_FILENAME, HIQLITE_ACTIVE_DIRNAME,
    HIQLITE_WAL_SIZE_BYTES, HIQLITE_WAL_USABLE_PAYLOAD_BYTES,
};
#[cfg(feature = "hiqlite-contract-tests")]
use plurx_core::config::Config;
use plurx_core::domain::{
    scopes, ArtworkAttempt, BookMetadataPatch, BookMetadataSource, CacheConsumerKind,
    CacheConsumerPin, CacheManifestCheck, CacheStorageMember, CredentialGeneration, ItemEdit,
    ItemKind, ItemSort, LibraryKind, MediaSessionActivation, MediaSessionRenewal,
    MediaSessionRequestClaim, MediaSessionTakeover, MediaSessionTerminalAck, MetadataPatch,
    NetworkPriorObservation, NewItem, NewLibrary, NewOfflinePackage, NewPretranscodeJob,
    OfflineCreateOutcome, OfflineLeaseOutcome, PlaybackEvent, PlaybackEventQuery,
    PretranscodeRequirements, PretranscodeWorkerCapabilities, ProbeResult, ReadingStateWrite,
    TraktAuth,
};
use plurx_core::error::StoreError;
use plurx_core::fmp4::CutClass;
use plurx_core::secrets::CredentialKey;
use plurx_core::segplan::{
    FragmentIndex, IndexRow, PlanCut, PlanEntry, PlanEntryKind, SegmentPlan, SourceIdentity,
    SEGPLAN_VERSION,
};
use plurx_core::store::{
    cluster_fragment_index_key, ArtworkRepairFence, ClusterFragmentIndexStore, LibraryStore,
    MediaStore, NewAnalysisRequest, NewClusterFragmentIndexJob, OutboxEntry, PublicationStore,
    ReconcileOutcome, RootFingerprintStatus, SqliteStore, Store,
};
#[cfg(feature = "hiqlite-contract-tests")]
use plurx_core::store::{
    ApiKeyStore, CoordinationStore, FencedPublicationStore, HiqliteAuthStore, MediaSessionStore,
    OfflinePackageStore, PlaybackTelemetryStore, PretranscodeJobStore, ReadingStore, SettingsStore,
    TraktStore, TranscodeCacheStore, UserStore, WatchStore, AUTH_SCHEMA_MIGRATION_SOURCE,
    AUTH_SCHEMA_VERSION,
};
#[cfg(feature = "cluster-read-cost-validation")]
use plurx_core::store::{CatalogueReader, MetricsStore};
#[cfg(feature = "hiqlite-contract-tests")]
use serde::{Deserialize, Serialize};
#[cfg(feature = "hiqlite-contract-tests")]
use tokio::io::AsyncReadExt;

#[cfg(feature = "hiqlite-contract-tests")]
const CONTRACT_RAFT_SECRET: &str = "plurx-store-contract-raft";
#[cfg(feature = "hiqlite-contract-tests")]
const CONTRACT_API_SECRET: &str = "plurx-store-contract-api";
#[cfg(feature = "hiqlite-contract-tests")]
const CONTRACT_INSTANCE_ID: &str = "00000000-0000-4000-8000-000000000090";

#[cfg(feature = "hiqlite-contract-tests")]
static HIQLITE_CASE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[cfg(feature = "hiqlite-contract-tests")]
struct I64Value {
    value: i64,
}

#[cfg(feature = "hiqlite-contract-tests")]
impl From<&mut Row<'_>> for I64Value {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            value: row.get("value"),
        }
    }
}

#[cfg(feature = "hiqlite-contract-tests")]
#[derive(Debug, PartialEq, Eq)]
struct CacheTouchTimes {
    last_used_at: i64,
    last_seen_at: i64,
}

#[cfg(feature = "hiqlite-contract-tests")]
impl From<&mut Row<'_>> for CacheTouchTimes {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            last_used_at: row.get("last_used_at"),
            last_seen_at: row.get("last_seen_at"),
        }
    }
}

const SETTINGS_METHODS: &[&str] = &[
    "ping",
    "get_setting",
    "get_or_init_setting",
    "get_setting_pair",
    "settings_snapshot",
    "put_setting",
    "put_setting_if_absent",
    "put_setting_if_absent_if_artwork_repair_current",
    "prune_unreferenced_book_cover_origins",
    "put_settings",
    "instance_id",
];
const USER_METHODS: &[&str] = &[
    "count_users",
    "create_user",
    "get_user",
    "get_user_by_username",
    "list_users",
    "list_users_page",
    "delete_user",
    "count_admins",
    "set_password",
    "set_admin",
    "delete_tokens_for_user",
    "create_token",
    "user_for_token",
    "delete_token",
];
const LIBRARY_METHODS: &[&str] = &[
    "create_library",
    "update_library",
    "delete_library",
    "set_library_schedule",
    "mark_library_scanned",
    "get_library",
    "list_libraries",
];
const MEDIA_METHODS: &[&str] = &[
    "item_by_external_id",
    "find_movie",
    "find_book",
    "find_show",
    "find_season",
    "find_episode",
    "find_child_item",
    "insert_item",
    "get_item",
    "item_titles",
    "get_item_children",
    "home_preview_pages",
    "list_top_items_in_genre",
    "list_top_items",
    "recently_added",
    "search_items",
    "apply_metadata",
    "apply_metadata_if_artwork_repair_current",
    "apply_book_metadata",
    "apply_book_metadata_if_current",
    "book_items",
    "related_book_editions",
    "items_needing_metadata",
    "episodes_for_show",
    "items_needing_artwork",
    "items_missing_artwork",
    "items_with_artwork",
    "items_with_artwork_page",
    "artwork_filename_is_referenced",
    "referenced_artwork_filenames",
    "items_missing_genres",
    "update_item_fields",
    "set_nfo_seeded",
    "get_file_by_path",
    "upsert_file",
    "get_file",
    "media_shape",
    "files_for_item",
    "child_counts",
    "item_max_heights",
    "item_media_facts",
    "set_file_audio_offset",
    "get_file_probe_json",
    "merge_file_probe_chapters",
    "files_missing_probe",
    "library_file_paths",
    "ensure_library_root_fingerprint",
    "reset_library_root_fingerprint",
    "rebuild_search_index",
    "reconcile_library",
    "delete_files",
    "prune_empty_items",
];
const WATCH_METHODS: &[&str] = &[
    "watch_state",
    "watch_map",
    "put_progress",
    "put_progress_if_current",
    "put_progress_at",
    "set_watched",
    "set_watched_tree",
    "watch_rollup",
    "watch_rollups",
    "continue_watching",
    "next_up",
    "apply_remote_watch",
];
const READING_METHODS: &[&str] = &[
    "reading_state",
    "current_reading_state",
    "put_reading_state",
    "delete_reading_state",
];
const TRAKT_METHODS: &[&str] = &[
    "get_trakt_auth",
    "list_trakt_auth",
    "put_trakt_auth",
    "delete_trakt_auth",
    "delete_trakt_auth_if_current",
    "update_trakt_tokens",
    "set_trakt_sync",
    "trakt_sync_candidates",
];
const API_KEY_METHODS: &[&str] = &[
    "create_api_key",
    "list_api_keys",
    "api_key_for_hash",
    "touch_api_key",
    "delete_api_key",
    "set_api_key_disabled",
];
const OUTBOX_METHODS: &[&str] = &[
    "enqueue_watched",
    "due_watched",
    "settle_watched",
    "watched_outbox_counts",
];
const CACHE_METHODS: &[&str] = &[
    "cache_hit",
    "claim_cache_entry",
    "touch_cache_claim",
    "complete_cache_entry",
    "touch_cache_entry",
    "cache_by_age",
    "cache_manifest_candidates",
    "mark_cache_manifests_checked",
    "stale_cache_claims",
    "all_cache_rows",
    "cache_ownership_inventory",
    "cache_candidate_owners",
    "invalidate_cache_entry",
    "forget_cache_entry",
    "cache_bytes",
];
const SHARED_CACHE_METHODS: &[&str] = &[
    "put_cache_storage_member",
    "cache_storage_member",
    "mark_cache_storage_suspect",
    "shared_cache_hit",
    "touch_shared_cache_entry",
    "claim_shared_cache_entry",
    "complete_shared_cache_entry",
    "abandon_shared_cache_entry",
    "finalize_abandoned_shared_cache_entry",
    "stale_shared_cache_claims",
    "acquire_cache_consumer_pin",
    "renew_cache_consumer_pins",
    "release_cache_consumer_pin",
    "prune_expired_cache_consumer_pins",
    "shared_cache_gc_candidates",
    "retire_shared_cache_generation",
    "finalize_retired_shared_cache_generation",
];
const PRETRANSCODE_METHODS: &[&str] = &[
    "pretranscode_job",
    "enqueue_pretranscode_job",
    "claim_pretranscode_job",
    "pretranscode_staging_jobs",
    "active_pretranscode_job_ids",
    "renew_pretranscode_job",
    "yield_pretranscode_job",
    "fail_pretranscode_job",
    "cancel_pretranscode_job",
    "complete_pretranscode_job",
];
const OFFLINE_METHODS: &[&str] = &[
    "create_offline_package",
    "offline_package_for_user",
    "renew_offline_package_for_user",
    "offline_activity_packages",
    "offline_package_stats",
    "reset_interrupted_offline_packages",
    "claim_next_offline_package",
    "requeue_offline_package",
    "set_offline_package_recipe",
    "update_offline_progress",
    "fail_offline_package",
    "invalidate_ready_offline_package",
    "put_offline_lease",
    "offline_package_for_lease",
    "mark_offline_package_ready",
    "delete_offline_package",
    "expire_offline_packages",
    // Node removal (`CLUSTERING-PLAN.md` §6.7). Cluster-only behavior: the
    // SQLite backend implements these inertly because a single-node install
    // has no node to remove, so the real contract lives in the replicated
    // case and in `plurx-cluster-check`.
    "unresolved_offline_packages",
    "offline_transfers_in_flight",
    "request_offline_source_probes",
    "pending_offline_source_probes",
    "answer_offline_source_probe",
    "outstanding_offline_source_probes",
    "verified_offline_source_nodes",
    "resolve_offline_packages_for_removal",
];
const TELEMETRY_METHODS: &[&str] = &[
    "record_playback_event",
    "prune_playback_events",
    "playback_events",
];
const FRAGMENT_INDEX_METHODS: &[&str] = &[
    "put_fragment_index",
    "fragment_index",
    "forget_fragment_index",
    // The orphan sweep's two halves. Node-local on one side and replicated on
    // the other, which is the reason the sweep exists rather than a hook in
    // `delete_files`.
    "vod_row_file_ids",
    "surviving_file_ids",
];
const RENDITION_PLAN_METHODS: &[&str] = &[
    "put_rendition_plan",
    "rendition_plan",
    "forget_rendition_plans",
];
const NETWORK_PRIOR_METHODS: &[&str] = &[
    "observe_network_prior",
    "network_prior",
    "prune_network_priors",
];
const COORDINATION_METHODS: &[&str] = &["acquire_lease", "renew_lease", "release_lease"];
const MEDIA_SESSION_METHODS: &[&str] = &[
    "claim_media_session_request",
    "assign_media_session_request_owner",
    "activate_media_session",
    "fail_media_session_request",
    "media_session_route",
    "media_session_route_by_incarnation",
    "record_media_session_terminal_ack",
    "media_session_terminal_ack",
    "renew_media_sessions",
    "expired_media_sessions",
    "claim_media_session_takeover",
    "end_media_session",
    "maintain_media_sessions",
    "owned_media_sessions",
];
const FENCED_PUBLICATION_METHODS: &[&str] = &[
    "put_setting_fenced",
    "put_setting_if_absent_fenced",
    "put_setting_if_absent_if_artwork_repair_current_fenced",
    "mark_library_scanned_fenced",
    "insert_item_fenced",
    "apply_metadata_fenced",
    "apply_metadata_if_artwork_repair_current_fenced",
    "apply_book_metadata_fenced",
    "apply_book_metadata_if_current_fenced",
    "set_nfo_seeded_fenced",
    "upsert_file_fenced",
    "ensure_library_root_fingerprint_fenced",
    "reconcile_library_fenced",
    "claim_cache_entry_fenced",
    "touch_cache_claim_fenced",
    "complete_cache_entry_fenced",
    "forget_cache_entry_fenced",
];
const METRICS_METHODS: &[&str] = &["prometheus_store_snapshot"];

struct StoreFixture {
    name: &'static str,
    store: Arc<dyn Store>,
    _directory: Option<tempfile::TempDir>,
}

fn sqlite_fixtures() -> Vec<StoreFixture> {
    let directory = tempfile::tempdir().expect("file-backed contract directory");
    let file_store =
        SqliteStore::open(&directory.path().join("plurx.db")).expect("file-backed contract store");
    vec![
        StoreFixture {
            name: "memory",
            store: Arc::new(SqliteStore::open_in_memory().expect("in-memory contract store")),
            _directory: None,
        },
        StoreFixture {
            name: "file",
            store: Arc::new(file_store),
            _directory: Some(directory),
        },
    ]
}

async fn for_each_backend<F, Fut>(mut contract: F)
where
    F: FnMut(Arc<dyn Store>, &'static str) -> Fut,
    Fut: Future<Output = ()>,
{
    for fixture in sqlite_fixtures() {
        contract(Arc::clone(&fixture.store), fixture.name).await;
    }

    #[cfg(feature = "hiqlite-contract-tests")]
    {
        let _case = HIQLITE_CASE.lock().await;
        let cluster = ContractCluster::start().await;
        let store = open_contract_hiqlite_store(&cluster).await;
        store
            .validation_reset_contract_state()
            .await
            .expect("reset replicated contract state");
        contract(Arc::new(store), "hiqlite-3-voter").await;
    }
}

#[tokio::test]
async fn prometheus_store_snapshot_is_one_backend_neutral_aggregate() {
    for_each_backend(|store, backend| async move {
        store
            .create_user("metrics-user", "hash", false)
            .await
            .unwrap_or_else(|error| panic!("{backend}: seed metrics user: {error}"));
        store
            .create_library(&NewLibrary {
                name: "Metrics Library".to_owned(),
                kind: LibraryKind::Movies,
                paths: Vec::new(),
                anime: false,
            })
            .await
            .unwrap_or_else(|error| panic!("{backend}: seed metrics library: {error}"));
        let (offline_user, offline_file) = seed_file(&store, "metrics-offline").await;
        let mut offline = offline_request(
            "metrics-package",
            "metrics-request",
            offline_user,
            offline_file,
        );
        offline.node_id = "metrics-node".to_owned();
        assert!(matches!(
            store
                .create_offline_package(&offline, 10, 100_000, 100_000)
                .await
                .unwrap_or_else(|error| panic!("{backend}: seed offline metrics: {error}")),
            OfflineCreateOutcome::Created(_)
        ));
        store
            .enqueue_watched(r#"{"type":"movie","watched":true}"#)
            .await
            .unwrap_or_else(|error| panic!("{backend}: seed outbox metrics: {error}"));
        let snapshot = store
            .prometheus_store_snapshot("metrics-node", 1_000)
            .await
            .unwrap_or_else(|error| panic!("{backend}: read metrics snapshot: {error}"));
        assert_eq!(snapshot.users, 2, "{backend}");
        assert_eq!(snapshot.libraries, 2, "{backend}");
        assert_eq!(snapshot.offline.queued, 1, "{backend}");
        assert_eq!(snapshot.offline.queued_bytes, 5_000, "{backend}");
        assert_eq!(snapshot.offline.preparing, 0, "{backend}");
        assert_eq!(snapshot.offline.ready, 0, "{backend}");
        assert_eq!(snapshot.offline.failed, 0, "{backend}");
        assert_eq!(snapshot.watched_outbox, (1, 0, 0), "{backend}");
    })
    .await;
}

#[tokio::test]
async fn monotone_lease_contract_runs_through_dyn_store() {
    for_each_backend(|store, backend| async move {
        let first = acquired(
            store
                .acquire_lease("scan:library:7", "node-a", 100, 200)
                .await
                .unwrap_or_else(|error| panic!("{backend}: acquire absent lease: {error}")),
            backend,
        );
        assert_eq!(first.fence, 1, "{backend}");
        assert_eq!(first.revision, 1, "{backend}");

        assert_eq!(
            store
                .acquire_lease("scan:library:7", "node-b", 150, 250)
                .await
                .unwrap_or_else(|error| panic!("{backend}: inspect held lease: {error}")),
            LeaseClaim::Held {
                owner_node_id: "node-a".to_owned(),
                fence: 1,
                expires_at_unix_ms: 200,
            },
            "{backend}"
        );

        let renewed = store
            .renew_lease(&first, 150, 300)
            .await
            .unwrap_or_else(|error| panic!("{backend}: renew current lease: {error}"))
            .unwrap_or_else(|| panic!("{backend}: current lease must renew"));
        assert_eq!(renewed.expires_at_unix_ms, 300, "{backend}");
        assert_eq!(renewed.revision, 2, "{backend}");

        assert!(
            store.renew_lease(&renewed, 160, 300).await.is_err(),
            "{backend}: renewal must advance the token expiry"
        );
        assert!(
            !store
                .release_lease(&first, 160)
                .await
                .unwrap_or_else(|error| panic!("{backend}: delayed release result: {error}")),
            "{backend}: a pre-renewal token must not release its same-fence successor"
        );
        assert!(
            store
                .renew_lease(&first, 160, 310)
                .await
                .unwrap_or_else(|error| panic!("{backend}: delayed renew result: {error}"))
                .is_none(),
            "{backend}: a pre-renewal token must not replace its same-fence successor"
        );

        let latest = store
            .renew_lease(&renewed, 160, 330)
            .await
            .unwrap_or_else(|error| panic!("{backend}: second renewal result: {error}"))
            .unwrap_or_else(|| panic!("{backend}: second renewal must succeed"));
        assert_eq!(latest.revision, 3, "{backend}");
        assert!(
            store
                .renew_lease(&renewed, 170, 320)
                .await
                .unwrap_or_else(|error| panic!("{backend}: reordered renewal result: {error}"))
                .is_none(),
            "{backend}: a reordered renewal must not regress expiry"
        );
        assert!(
            !store
                .release_lease(&renewed, 170)
                .await
                .unwrap_or_else(|error| panic!("{backend}: stale same-fence release: {error}")),
            "{backend}: a superseded same-fence token must not release the lease"
        );

        let wrong_fence = Lease {
            fence: 2,
            ..latest.clone()
        };
        assert!(
            store
                .renew_lease(&wrong_fence, 160, 340)
                .await
                .unwrap_or_else(|error| panic!("{backend}: stale renew result: {error}"))
                .is_none(),
            "{backend}: wrong fence must not renew"
        );
        assert!(
            !store
                .release_lease(&wrong_fence, 160)
                .await
                .unwrap_or_else(|error| panic!("{backend}: stale release result: {error}")),
            "{backend}: wrong fence must not release"
        );
        assert!(
            store
                .release_lease(&latest, 180)
                .await
                .unwrap_or_else(|error| panic!("{backend}: release current lease: {error}")),
            "{backend}"
        );
        assert!(
            store
                .renew_lease(&latest, 190, 340)
                .await
                .unwrap_or_else(|error| panic!("{backend}: post-release renewal result: {error}"))
                .is_none(),
            "{backend}: a delayed renewal must not resurrect a released lease"
        );

        let second = acquired(
            store
                .acquire_lease("scan:library:7", "node-b", 180, 260)
                .await
                .unwrap_or_else(|error| panic!("{backend}: reacquire released lease: {error}")),
            backend,
        );
        assert_eq!(second.fence, 2, "{backend}");
        assert_eq!(second.revision, 5, "{backend}");
        assert!(
            store
                .renew_lease(&latest, 190, 350)
                .await
                .unwrap_or_else(|error| panic!("{backend}: predecessor renew result: {error}"))
                .is_none(),
            "{backend}: predecessor must remain stale"
        );

        let third = acquired(
            store
                .acquire_lease("scan:library:7", "node-a", 260, 360)
                .await
                .unwrap_or_else(|error| panic!("{backend}: expiry-boundary takeover: {error}")),
            backend,
        );
        assert_eq!(third.fence, 3, "{backend}");
        assert_eq!(third.revision, 6, "{backend}");
        assert_eq!(third.owner_node_id, "node-a", "{backend}");

        assert!(
            store
                .release_lease(&third, 400)
                .await
                .unwrap_or_else(|error| panic!("{backend}: release expired lease: {error}")),
            "{backend}: exact expired token should release without extending expiry"
        );
        assert!(
            store
                .renew_lease(&third, 350, 450)
                .await
                .unwrap_or_else(|error| panic!("{backend}: renewal after late release: {error}"))
                .is_none(),
            "{backend}: a late release must change the same-fence CAS revision"
        );
        let fourth = acquired(
            store
                .acquire_lease("scan:library:7", "node-b", 380, 480)
                .await
                .unwrap_or_else(|error| panic!("{backend}: takeover after late release: {error}")),
            backend,
        );
        assert_eq!(fourth.fence, 4, "{backend}");
        assert_eq!(fourth.revision, 8, "{backend}");

        let aba_first = acquired(
            store
                .acquire_lease("aba-release", "node-a", 100, 200)
                .await
                .unwrap_or_else(|error| panic!("{backend}: acquire ABA fixture: {error}")),
            backend,
        );
        let aba_renewed = store
            .renew_lease(&aba_first, 150, 300)
            .await
            .unwrap_or_else(|error| panic!("{backend}: renew ABA fixture: {error}"))
            .unwrap_or_else(|| panic!("{backend}: ABA fixture must renew"));
        assert!(
            store
                .release_lease(&aba_renewed, 200)
                .await
                .unwrap_or_else(|error| panic!("{backend}: release ABA fixture: {error}")),
            "{backend}"
        );
        assert!(
            store
                .renew_lease(&aba_first, 150, 350)
                .await
                .unwrap_or_else(|error| panic!("{backend}: delayed ABA renewal: {error}"))
                .is_none(),
            "{backend}: recurring expiry must not make an older revision current"
        );

        assert!(
            store.acquire_lease("", "node-a", 1, 2).await.is_err(),
            "{backend}: empty resource must fail before a write"
        );
        assert!(
            store.acquire_lease("probe", "node-a", 2, 2).await.is_err(),
            "{backend}: non-future expiry must fail before a write"
        );
    })
    .await;
}

#[tokio::test]
async fn media_session_contract_runs_through_dyn_store() {
    for_each_backend(|store, backend| async move {
        let first_user = store
            .create_user("session-user-a", "hash", false)
            .await
            .unwrap_or_else(|error| panic!("{backend}: create first session user: {error}"));
        let second_user = store
            .create_user("session-user-b", "hash", false)
            .await
            .unwrap_or_else(|error| panic!("{backend}: create second session user: {error}"));
        let (_, shared_file_id) = seed_file(&store, "session-shared-pin-contract").await;
        let shared_storage_id = format!("shared:session-lifecycle:{backend}");
        let shared_recipe = format!("shared-session-pin-{backend}");
        let shared_generation_id = "shared-session-generation";
        store
            .put_cache_storage_member(&CacheStorageMember {
                storage_id: shared_storage_id.clone(),
                node_id: format!("{backend}-session-reader"),
                storage_class: "shared".to_owned(),
                verified_at_ms: 80,
                verification_state: "verified".to_owned(),
            })
            .await
            .unwrap_or_else(|error| panic!("{backend}: verify session storage: {error}"));
        assert!(store
            .claim_shared_cache_entry(
                &shared_recipe,
                shared_file_id,
                1,
                &shared_storage_id,
                shared_generation_id,
                "session/shared/generation",
                81,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: claim session generation: {error}")));
        assert!(store
            .complete_shared_cache_entry(
                &shared_recipe,
                &shared_storage_id,
                shared_generation_id,
                4_096,
                &"f".repeat(64),
                82,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: complete session generation: {error}")));
        let shared_generation = store
            .shared_cache_hit(&shared_recipe, &shared_storage_id)
            .await
            .unwrap_or_else(|error| panic!("{backend}: read session generation: {error}"))
            .expect("session generation");
        let shared_gc_lease = match store
            .acquire_lease(
                &format!("shared-cache-gc:{shared_storage_id}"),
                "session-gc-owner",
                83,
                1_000,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: acquire session GC lease: {error}"))
        {
            LeaseClaim::Acquired(lease) => lease,
            LeaseClaim::Held { .. } => panic!("{backend}: fresh session GC lease was held"),
        };
        let fingerprint = "a".repeat(64);
        let conflicting = "b".repeat(64);
        let incarnation_a = "00000000-0000-4000-8000-0000000000a1";
        let incarnation_a_retry = "00000000-0000-4000-8000-0000000000a2";
        let session_a = "00000000-0000-4000-8000-0000000000b1";

        assert!(
            store
                .claim_media_session_request(
                    first_user.id,
                    "uppercase-fingerprint",
                    &"A".repeat(64),
                    "uppercase-playback",
                    "00000000-0000-4000-8000-0000000000f1",
                    1,
                    2,
                )
                .await
                .is_err(),
            "{backend}: fingerprints must use one canonical lowercase encoding"
        );

        let expired_incarnation = "00000000-0000-4000-8000-0000000000d1";
        let recovered_incarnation = "00000000-0000-4000-8000-0000000000d2";
        assert!(matches!(
            store
                .claim_media_session_request(
                    first_user.id,
                    "expired-attempt",
                    &fingerprint,
                    "expired-attempt-playback",
                    expired_incarnation,
                    1,
                    2,
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: claim expiring request: {error}")),
            MediaSessionRequestClaim::Acquired { .. }
        ));
        assert!(matches!(
            store
                .claim_media_session_request(
                    first_user.id,
                    "expired-attempt",
                    &fingerprint,
                    "expired-attempt-playback",
                    recovered_incarnation,
                    2,
                    12,
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: reclaim exact expired request: {error}")),
            MediaSessionRequestClaim::Acquired { incarnation_id }
                if incarnation_id == recovered_incarnation
        ));
        assert!(store
            .fail_media_session_request(
                first_user.id,
                "expired-attempt",
                recovered_incarnation,
                3,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: settle expired request fixture: {error}")));

        let failed_incarnation = "00000000-0000-4000-8000-0000000000e1";
        let retried_incarnation = "00000000-0000-4000-8000-0000000000e2";
        assert!(matches!(
            store
                .claim_media_session_request(
                    first_user.id,
                    "retryable-attempt",
                    &fingerprint,
                    "retryable-attempt-playback",
                    failed_incarnation,
                    10,
                    20,
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: claim retry fixture: {error}")),
            MediaSessionRequestClaim::Acquired { incarnation_id }
                if incarnation_id == failed_incarnation
        ));
        assert!(store
            .fail_media_session_request(first_user.id, "retryable-attempt", failed_incarnation, 11,)
            .await
            .unwrap_or_else(|error| panic!("{backend}: fail retry fixture: {error}")));
        assert!(matches!(
            store
                .claim_media_session_request(
                    first_user.id,
                    "retryable-attempt",
                    &fingerprint,
                    "retryable-attempt-playback",
                    retried_incarnation,
                    12,
                    22,
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: reacquire failed request: {error}")),
            MediaSessionRequestClaim::Acquired { incarnation_id }
                if incarnation_id == retried_incarnation
        ));
        assert_eq!(
            store
                .claim_media_session_request(
                    first_user.id,
                    "retryable-attempt",
                    &conflicting,
                    "retryable-attempt-playback",
                    failed_incarnation,
                    13,
                    23,
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: conflict retried request: {error}")),
            MediaSessionRequestClaim::Conflict,
            "{backend}"
        );
        assert!(
            store
                .fail_media_session_request(
                    first_user.id,
                    "retryable-attempt",
                    retried_incarnation,
                    14,
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: settle retry fixture: {error}"))
        );

        assert_eq!(
            store
                .claim_media_session_request(
                    first_user.id,
                    "attempt-a",
                    &fingerprint,
                    "shared-playback",
                    incarnation_a,
                    100,
                    200,
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: claim session request: {error}")),
            MediaSessionRequestClaim::Acquired {
                incarnation_id: incarnation_a.to_owned(),
            },
            "{backend}"
        );
        assert!(matches!(
            store
                .claim_media_session_request(
                    first_user.id,
                    "attempt-a",
                    &fingerprint,
                    "shared-playback",
                    incarnation_a_retry,
                    110,
                    210,
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: replay in-flight request: {error}")),
            MediaSessionRequestClaim::InFlight { incarnation_id, .. }
                if incarnation_id == incarnation_a
        ));
        assert_eq!(
            store
                .claim_media_session_request(
                    first_user.id,
                    "attempt-a",
                    &conflicting,
                    "shared-playback",
                    incarnation_a_retry,
                    110,
                    210,
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: conflict session request: {error}")),
            MediaSessionRequestClaim::Conflict,
            "{backend}"
        );
        assert!(store
            .assign_media_session_request_owner(
                first_user.id,
                "attempt-a",
                incarnation_a,
                "node-a",
                120,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: assign request owner: {error}")));

        let first = store
            .activate_media_session(&MediaSessionActivation {
                incarnation_id: incarnation_a.to_owned(),
                session_id: session_a.to_owned(),
                user_id: first_user.id,
                playback_id: "shared-playback".to_owned(),
                expected_predecessor_incarnation_id: None,
                fence_predecessor: false,
                request_id: Some("attempt-a".to_owned()),
                request_fingerprint: fingerprint.clone(),
                owner_node_id: "node-a".to_owned(),
                recipe_json: r#"{"version":1}"#.to_owned(),
                response_json: r#"{"session":"a"}"#.to_owned(),
                media_origin_ms: 12_500,
                now_ms: 130,
                lease_expires_at_ms: 330,
            })
            .await
            .unwrap_or_else(|error| panic!("{backend}: activate first session: {error}"))
            .unwrap_or_else(|| panic!("{backend}: first activation must win"));
        assert!(first.predecessor.is_none(), "{backend}");
        assert_eq!(first.route.owner_epoch, 1, "{backend}");
        assert!(store
            .acquire_cache_consumer_pin(
                &CacheConsumerPin {
                    storage_id: shared_storage_id.clone(),
                    recipe_hash: shared_recipe.clone(),
                    generation_id: shared_generation_id.to_owned(),
                    consumer_kind: CacheConsumerKind::MediaSession,
                    consumer_id: incarnation_a.to_owned(),
                    consumer_epoch: 1,
                    expires_at_ms: 330,
                },
                131,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: pin predecessor session: {error}")));
        assert!(matches!(
            store
                .claim_media_session_request(
                    first_user.id,
                    "attempt-a",
                    &fingerprint,
                    "shared-playback",
                    incarnation_a_retry,
                    140,
                    240,
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: replay resolved request: {error}")),
            MediaSessionRequestClaim::Resolved(route) if route.session_id == session_a
        ));

        let session_b = "00000000-0000-4000-8000-0000000000b2";
        let incarnation_b = "00000000-0000-4000-8000-0000000000a3";
        let second = store
            .activate_media_session(&MediaSessionActivation {
                incarnation_id: incarnation_b.to_owned(),
                session_id: session_b.to_owned(),
                user_id: second_user.id,
                playback_id: "shared-playback".to_owned(),
                expected_predecessor_incarnation_id: None,
                fence_predecessor: false,
                request_id: None,
                request_fingerprint: fingerprint.clone(),
                owner_node_id: "node-b".to_owned(),
                recipe_json: "{}".to_owned(),
                response_json: "{}".to_owned(),
                // Write-once at activation. Every generation's frontier is
                // measured from this zero, so a claim that rewrote it would
                // silently reinterpret every offset already published.
                media_origin_ms: 90_000,
                now_ms: 145,
                lease_expires_at_ms: 345,
            })
            .await
            .unwrap_or_else(|error| panic!("{backend}: activate cross-user session: {error}"))
            .unwrap_or_else(|| panic!("{backend}: cross-user activation must win"));
        assert!(second.predecessor.is_none(), "{backend}");
        assert_eq!(
            store
                .owned_media_sessions("node-b", 344)
                .await
                .unwrap_or_else(|error| panic!("{backend}: list live node-b session: {error}"))
                .len(),
            1,
            "{backend}"
        );

        let session_a2 = "00000000-0000-4000-8000-0000000000b3";
        let incarnation_a2 = "00000000-0000-4000-8000-0000000000a4";
        let superseding = store
            .activate_media_session(&MediaSessionActivation {
                incarnation_id: incarnation_a2.to_owned(),
                session_id: session_a2.to_owned(),
                user_id: first_user.id,
                playback_id: "shared-playback".to_owned(),
                expected_predecessor_incarnation_id: Some(incarnation_a.to_owned()),
                fence_predecessor: true,
                request_id: None,
                request_fingerprint: fingerprint.clone(),
                owner_node_id: "node-a".to_owned(),
                recipe_json: "{}".to_owned(),
                response_json: "{}".to_owned(),
                media_origin_ms: 0,
                now_ms: 150,
                lease_expires_at_ms: 350,
            })
            .await
            .unwrap_or_else(|error| panic!("{backend}: supersede session: {error}"))
            .unwrap_or_else(|| panic!("{backend}: superseding activation must win"));
        assert_eq!(
            superseding
                .predecessor
                .as_ref()
                .map(|route| route.session_id.as_str()),
            Some(session_a),
            "{backend}"
        );
        assert!(!store
            .release_cache_consumer_pin(
                &shared_storage_id,
                &shared_recipe,
                shared_generation_id,
                CacheConsumerKind::MediaSession,
                incarnation_a,
                1,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: inspect predecessor pin: {error}")),
            "{backend}: fencing a predecessor must delete its shared-cache pin"
        );
        assert_eq!(
            store
                .media_session_route(session_a)
                .await
                .unwrap_or_else(|error| panic!("{backend}: inspect predecessor: {error}"))
                .map(|route| route.state),
            Some("ended".to_owned()),
            "{backend}"
        );
        assert_eq!(
            store
                .media_session_route_by_incarnation(incarnation_a2)
                .await
                .unwrap_or_else(|error| panic!("{backend}: inspect current incarnation: {error}"))
                .map(|route| route.session_id),
            Some(session_a2.to_owned()),
            "{backend}"
        );

        let stale = store
            .activate_media_session(&MediaSessionActivation {
                incarnation_id: "00000000-0000-4000-8000-0000000000a5".to_owned(),
                session_id: "00000000-0000-4000-8000-0000000000b4".to_owned(),
                user_id: first_user.id,
                playback_id: "shared-playback".to_owned(),
                expected_predecessor_incarnation_id: Some(incarnation_a.to_owned()),
                fence_predecessor: true,
                request_id: None,
                request_fingerprint: fingerprint.clone(),
                owner_node_id: "node-a".to_owned(),
                recipe_json: "{}".to_owned(),
                response_json: "{}".to_owned(),
                media_origin_ms: 0,
                now_ms: 160,
                lease_expires_at_ms: 360,
            })
            .await
            .unwrap_or_else(|error| panic!("{backend}: stale successor verdict: {error}"));
        assert!(
            stale.is_none(),
            "{backend}: stale predecessor CAS must lose"
        );
        let stale_legacy = store
            .activate_media_session(&MediaSessionActivation {
                incarnation_id: "00000000-0000-4000-8000-0000000000a8".to_owned(),
                session_id: "00000000-0000-4000-8000-0000000000b6".to_owned(),
                user_id: first_user.id,
                playback_id: "shared-playback".to_owned(),
                expected_predecessor_incarnation_id: None,
                fence_predecessor: true,
                request_id: None,
                request_fingerprint: fingerprint.clone(),
                owner_node_id: "node-a".to_owned(),
                recipe_json: "{}".to_owned(),
                response_json: "{}".to_owned(),
                media_origin_ms: 0,
                now_ms: 161,
                lease_expires_at_ms: 361,
            })
            .await
            .unwrap_or_else(|error| panic!("{backend}: stale legacy successor verdict: {error}"));
        assert!(
            stale_legacy.is_none(),
            "{backend}: a legacy reopen must require the pointer to remain absent"
        );
        assert_eq!(
            store
                .media_session_route(session_a2)
                .await
                .unwrap_or_else(|error| panic!("{backend}: inspect fenced successor: {error}"))
                .map(|route| route.state),
            Some("active".to_owned()),
            "{backend}: a delayed reopen must not end the current successor"
        );

        assert!(store
            .acquire_cache_consumer_pin(
                &CacheConsumerPin {
                    storage_id: shared_storage_id.clone(),
                    recipe_hash: shared_recipe.clone(),
                    generation_id: shared_generation_id.to_owned(),
                    consumer_kind: CacheConsumerKind::MediaSession,
                    consumer_id: incarnation_a2.to_owned(),
                    consumer_epoch: 1,
                    expires_at_ms: 350,
                },
                190,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: pin active media session: {error}")));
        assert!(store
            .retire_shared_cache_generation(&shared_generation, 199, &shared_gc_lease)
            .await
            .unwrap_or_else(|error| panic!("{backend}: active session pin retirement: {error}"))
            .is_none());

        assert_eq!(
            store
                .renew_media_sessions(
                    "node-a",
                    &[MediaSessionRenewal {
                        incarnation_id: incarnation_a2.to_owned(),
                        owner_epoch: 1,
                        produced_playable_through_ms: 20_000,
                        fetched_through_ms: 10_000,
                        media_sequence: 5,
                    }],
                    200,
                    400,
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: renew current session: {error}")),
            vec![incarnation_a2.to_owned()],
            "{backend}"
        );
        let renewed_route = store
            .media_session_route(session_a2)
            .await
            .unwrap_or_else(|error| panic!("{backend}: inspect renewed frontiers: {error}"))
            .unwrap_or_else(|| panic!("{backend}: renewed route disappeared"));
        assert_eq!(
            renewed_route.produced_playable_through_ms, 20_000,
            "{backend}"
        );
        assert_eq!(renewed_route.fetched_through_ms, 10_000, "{backend}");
        assert_eq!(renewed_route.media_sequence, 5, "{backend}");

        // Monotone progress (§8.8). A generation that restarts its local
        // encoder resets its own index and will heartbeat a lower frontier;
        // the replicated high-water mark must absorb that, not follow it.
        assert_eq!(
            store
                .renew_media_sessions(
                    "node-a",
                    &[MediaSessionRenewal {
                        incarnation_id: incarnation_a2.to_owned(),
                        owner_epoch: 1,
                        produced_playable_through_ms: 4_000,
                        fetched_through_ms: 1_000,
                        media_sequence: 1,
                    }],
                    201,
                    // The same expiry the live renewal set: this fixture is
                    // about the frontier columns, and must not disturb the
                    // exact-expiry refusal asserted below.
                    400,
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: renew with a regressed frontier: {error}")),
            vec![incarnation_a2.to_owned()],
            "{backend}: a regressed frontier still renews the lease"
        );
        let held = store
            .media_session_route(session_a2)
            .await
            .unwrap_or_else(|error| panic!("{backend}: inspect held frontiers: {error}"))
            .unwrap_or_else(|| panic!("{backend}: renewed route disappeared"));
        assert_eq!(held.produced_playable_through_ms, 20_000, "{backend}");
        assert_eq!(held.fetched_through_ms, 10_000, "{backend}");
        assert_eq!(
            held.media_sequence, 5,
            "{backend}: the sequence high-water mark is what a successor \
             starts above; it may never move backwards"
        );
        // P6's guarantee, restored: renewing the session renews its typed
        // reader pin, so the generation it is reading cannot be retired out
        // from under it. The renewal batch's placeholder scramble broke this
        // and the assertion was inverted to match; the assertion is the
        // contract, not the observation.
        assert!(store
            .retire_shared_cache_generation(&shared_generation, 360, &shared_gc_lease)
            .await
            .unwrap_or_else(|error| panic!("{backend}: renewed session pin retirement: {error}"))
            .is_none());
        assert!(store
            .renew_media_sessions(
                "node-a",
                &[MediaSessionRenewal {
                    incarnation_id: incarnation_a2.to_owned(),
                    owner_epoch: 1,
                    produced_playable_through_ms: 30_000,
                    fetched_through_ms: 15_000,
                    media_sequence: 6,
                }],
                400,
                500,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: reject exact-expiry renewal: {error}"))
            .is_empty());

        let owned_a = store
            .owned_media_sessions("node-a", 399)
            .await
            .unwrap_or_else(|error| panic!("{backend}: list node-a sessions: {error}"));
        assert_eq!(owned_a.len(), 1, "{backend}");
        assert_eq!(owned_a[0].session_id, session_a2, "{backend}");
        assert!(
            store
                .owned_media_sessions("node-b", 399)
                .await
                .unwrap_or_else(|error| panic!("{backend}: list expired node-b sessions: {error}"))
                .is_empty(),
            "{backend}: owner inventory must exclude an expired lease"
        );

        let expired = store
            .expired_media_sessions(399, 64)
            .await
            .unwrap_or_else(|error| panic!("{backend}: list takeover candidates: {error}"));
        let expired_b = expired
            .iter()
            .find(|route| route.session_id == session_b)
            .unwrap_or_else(|| panic!("{backend}: expired route must be offered for takeover"));
        assert_eq!(expired_b.owner_epoch, 1, "{backend}");
        assert_eq!(expired_b.media_origin_ms, 90_000, "{backend}");
        let takeover = MediaSessionTakeover {
            incarnation_id: incarnation_b.to_owned(),
            expected_owner_node_id: "node-b".to_owned(),
            expected_owner_epoch: 1,
            next_owner_node_id: "node-c".to_owned(),
            now_ms: 399,
            lease_expires_at_ms: 600,
        };
        let taken = store
            .claim_media_session_takeover(&takeover)
            .await
            .unwrap_or_else(|error| panic!("{backend}: claim expired session: {error}"))
            .unwrap_or_else(|| panic!("{backend}: eligible survivor must win takeover"));
        assert_eq!(taken.session_id, session_b, "{backend}");
        assert_eq!(taken.owner_node_id, "node-c", "{backend}");
        assert_eq!(taken.owner_epoch, 2, "{backend}");
        assert_eq!(taken.discontinuity_sequence, 1, "{backend}");
        assert_eq!(
            taken.media_origin_ms, 90_000,
            "{backend}: a claim moves ownership, never the timeline's zero"
        );
        assert!(store
            .claim_media_session_takeover(&takeover)
            .await
            .unwrap_or_else(|error| panic!("{backend}: replay takeover CAS: {error}"))
            .is_none());
        assert!(store
            .renew_media_sessions(
                "node-b",
                &[MediaSessionRenewal {
                    incarnation_id: incarnation_b.to_owned(),
                    owner_epoch: 1,
                    produced_playable_through_ms: 1,
                    fetched_through_ms: 1,
                    media_sequence: 1,
                }],
                400,
                700,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: stale owner renewal: {error}"))
            .is_empty());

        let ended = store
            .end_media_session(session_a2, 410)
            .await
            .unwrap_or_else(|error| panic!("{backend}: end current session: {error}"))
            .unwrap_or_else(|| panic!("{backend}: ended route exists"));
        assert_eq!(ended.session_id, session_a2, "{backend}");
        assert!(store
            .owned_media_sessions("node-a", 410)
            .await
            .unwrap_or_else(|error| panic!("{backend}: list sessions after end: {error}"))
            .is_empty());
        assert!(!store
            .release_cache_consumer_pin(
                &shared_storage_id,
                &shared_recipe,
                shared_generation_id,
                CacheConsumerKind::MediaSession,
                incarnation_a2,
                1,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: inspect ended session pin: {error}")),
            "{backend}: ending a session must delete its shared-cache pin"
        );
        assert!(store
            .retire_shared_cache_generation(&shared_generation, 411, &shared_gc_lease)
            .await
            .unwrap_or_else(|error| panic!(
                "{backend}: retire generation after session end: {error}"
            ))
            .is_some());
        assert!(matches!(
            store
                .claim_media_session_request(
                    first_user.id,
                    "attempt-a",
                    &fingerprint,
                    "shared-playback",
                    incarnation_a_retry,
                    420,
                    520,
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: replay superseded request: {error}")),
            MediaSessionRequestClaim::Resolved(route)
                if route.session_id == session_a && route.state == "ended"
        ));

        let expired_activation_incarnation = "00000000-0000-4000-8000-0000000000a6";
        assert!(matches!(
            store
                .claim_media_session_request(
                    first_user.id,
                    "expired-activation",
                    &fingerprint,
                    "expired-playback",
                    expired_activation_incarnation,
                    430,
                    440,
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: claim expiring activation: {error}")),
            MediaSessionRequestClaim::Acquired { .. }
        ));
        assert!(store
            .assign_media_session_request_owner(
                first_user.id,
                "expired-activation",
                expired_activation_incarnation,
                "node-a",
                439,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: assign expiring activation: {error}")));
        assert!(store
            .activate_media_session(&MediaSessionActivation {
                incarnation_id: expired_activation_incarnation.to_owned(),
                session_id: "00000000-0000-4000-8000-0000000000b5".to_owned(),
                user_id: first_user.id,
                playback_id: "expired-playback".to_owned(),
                expected_predecessor_incarnation_id: None,
                fence_predecessor: false,
                request_id: Some("expired-activation".to_owned()),
                request_fingerprint: fingerprint.clone(),
                owner_node_id: "node-a".to_owned(),
                recipe_json: "{}".to_owned(),
                response_json: "{}".to_owned(),
                media_origin_ms: 0,
                now_ms: 440,
                lease_expires_at_ms: 640,
            })
            .await
            .unwrap_or_else(|error| panic!("{backend}: reject expired activation: {error}"))
            .is_none());

        let expired_owner_incarnation = "00000000-0000-4000-8000-0000000000a7";
        assert!(matches!(
            store
                .claim_media_session_request(
                    first_user.id,
                    "expired-owner",
                    &fingerprint,
                    "expired-owner-playback",
                    expired_owner_incarnation,
                    450,
                    460,
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: claim expiring owner: {error}")),
            MediaSessionRequestClaim::Acquired { .. }
        ));
        assert!(!store
            .assign_media_session_request_owner(
                first_user.id,
                "expired-owner",
                expired_owner_incarnation,
                "node-a",
                460,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: reject expired owner: {error}")));

        // Expired routes remain active for one bounded takeover window, then
        // maintenance terminals an unclaimed or abandoned successor.
        let maintenance_now = 60_601;
        store
            .maintain_media_sessions(maintenance_now)
            .await
            .unwrap_or_else(|error| panic!("{backend}: expire active sessions: {error}"));
        assert!(store
            .owned_media_sessions("node-b", maintenance_now)
            .await
            .unwrap_or_else(|error| panic!("{backend}: inspect expired inventory: {error}"))
            .is_empty());
        assert_eq!(
            store
                .media_session_route(session_b)
                .await
                .unwrap_or_else(|error| panic!("{backend}: inspect expired route: {error}"))
                .map(|route| route.state),
            Some("ended".to_owned()),
            "{backend}: expired active routes must become terminal"
        );

        let prune_now = maintenance_now + 24 * 60 * 60 * 1_000 + 1;
        store
            .maintain_media_sessions(prune_now)
            .await
            .unwrap_or_else(|error| panic!("{backend}: prune ended sessions: {error}"));
        assert!(store
            .media_session_route(session_b)
            .await
            .unwrap_or_else(|error| panic!("{backend}: inspect pruned route: {error}"))
            .is_none());
    })
    .await;
}

#[tokio::test]
async fn terminal_control_ack_is_immutable_and_outlives_route_settlement() {
    for_each_backend(|store, backend| async move {
        let user = store
            .create_user("terminal-ack-user", "hash", false)
            .await
            .unwrap_or_else(|error| panic!("{backend}: create user: {error}"));
        let incarnation = "00000000-0000-4000-8000-00000000f001";
        let session = "00000000-0000-4000-8000-00000000f002";
        store
            .activate_media_session(&MediaSessionActivation {
                incarnation_id: incarnation.to_owned(),
                session_id: session.to_owned(),
                user_id: user.id,
                playback_id: "terminal-ack-playback".to_owned(),
                expected_predecessor_incarnation_id: None,
                fence_predecessor: false,
                request_id: None,
                request_fingerprint: "f".repeat(64),
                owner_node_id: "node-terminal".to_owned(),
                recipe_json: "{}".to_owned(),
                response_json: "{}".to_owned(),
                media_origin_ms: 0,
                now_ms: 1_000,
                lease_expires_at_ms: 10_000,
            })
            .await
            .unwrap_or_else(|error| panic!("{backend}: activate: {error}"))
            .unwrap_or_else(|| panic!("{backend}: activation must win"));
        let acknowledgement = MediaSessionTerminalAck {
            incarnation_id: incarnation.to_owned(),
            session_id: session.to_owned(),
            owner_node_id: "node-terminal".to_owned(),
            owner_epoch: 1,
            client_instance_id: "00000000-0000-4000-8000-00000000f003".to_owned(),
            sequence: 7,
            request_fingerprint: "e".repeat(64),
            response_json: "{\"lease\":\"ended\"}".to_owned(),
            expires_at_ms: 61_000,
            updated_at_ms: 1_000,
        };
        assert!(store
            .record_media_session_terminal_ack(&acknowledgement)
            .await
            .unwrap_or_else(|error| panic!("{backend}: record acknowledgement: {error}")));
        assert!(store
            .record_media_session_terminal_ack(&acknowledgement)
            .await
            .unwrap_or_else(|error| panic!("{backend}: repeat acknowledgement: {error}")));

        let mut conflict = acknowledgement.clone();
        conflict.sequence += 1;
        assert!(!store
            .record_media_session_terminal_ack(&conflict)
            .await
            .unwrap_or_else(|error| panic!(
                "{backend}: reject conflicting acknowledgement: {error}"
            )));

        store
            .end_media_session(session, 2_000)
            .await
            .unwrap_or_else(|error| panic!("{backend}: settle route: {error}"));
        assert_eq!(
            store
                .media_session_terminal_ack(session, 60_999)
                .await
                .unwrap_or_else(|error| panic!(
                    "{backend}: read retained acknowledgement: {error}"
                )),
            Some(acknowledgement.clone()),
            "{backend}: settlement cannot erase the replay window"
        );
        assert!(store
            .media_session_terminal_ack(session, 61_000)
            .await
            .unwrap_or_else(|error| panic!("{backend}: read expired acknowledgement: {error}"))
            .is_none());
        store
            .maintain_media_sessions(61_000)
            .await
            .unwrap_or_else(|error| panic!("{backend}: maintain acknowledgements: {error}"));
        assert!(store
            .media_session_terminal_ack(session, 2_000)
            .await
            .unwrap_or_else(|error| panic!("{backend}: inspect pruned acknowledgement: {error}"))
            .is_none());
    })
    .await;
}

/// Ending an incarnation that has just been taken over must act on the owner
/// it really has.
///
/// The route read that drives the end is not inside the mutation, so a claim
/// can commit between the two. If the dependent statements trust that stale
/// snapshot, the end clamps a lease the dead node no longer holds, leaves the
/// successor's shared-cache pin behind, and hands the caller a route naming a
/// host that is gone — so the abort is sent into the void while the
/// replacement encoder keeps running and keeps its admission slot.
#[tokio::test]
async fn ending_a_taken_over_session_acts_on_the_current_owner() {
    for_each_backend(|store, backend| async move {
        let user = store
            .create_user("takeover-end-user", "hash", false)
            .await
            .unwrap_or_else(|error| panic!("{backend}: create user: {error}"));
        let fingerprint = "e".repeat(64);
        let incarnation = "00000000-0000-4000-8000-00000000e001";
        let session = "00000000-0000-4000-8000-00000000e002";

        store
            .activate_media_session(&MediaSessionActivation {
                incarnation_id: incarnation.to_owned(),
                session_id: session.to_owned(),
                user_id: user.id,
                playback_id: "takeover-end-playback".to_owned(),
                expected_predecessor_incarnation_id: None,
                fence_predecessor: false,
                request_id: None,
                request_fingerprint: fingerprint.clone(),
                owner_node_id: "node-old".to_owned(),
                recipe_json: "{}".to_owned(),
                response_json: "{}".to_owned(),
                media_origin_ms: 4_000,
                now_ms: 1_000,
                lease_expires_at_ms: 2_000,
            })
            .await
            .unwrap_or_else(|error| panic!("{backend}: activate: {error}"))
            .unwrap_or_else(|| panic!("{backend}: activation must win"));

        let taken = store
            .claim_media_session_takeover(&MediaSessionTakeover {
                incarnation_id: incarnation.to_owned(),
                expected_owner_node_id: "node-old".to_owned(),
                expected_owner_epoch: 1,
                next_owner_node_id: "node-new".to_owned(),
                now_ms: 2_000,
                lease_expires_at_ms: 14_000,
            })
            .await
            .unwrap_or_else(|error| panic!("{backend}: claim: {error}"))
            .unwrap_or_else(|| panic!("{backend}: the survivor must win the claim"));
        assert_eq!(taken.owner_node_id, "node-new", "{backend}");
        assert_eq!(taken.owner_epoch, 2, "{backend}");

        let ended = store
            .end_media_session(session, 3_000)
            .await
            .unwrap_or_else(|error| panic!("{backend}: end: {error}"))
            .unwrap_or_else(|| panic!("{backend}: ending a live session returns its route"));
        assert_eq!(
            ended.owner_node_id, "node-new",
            "{backend}: the end must name the owner it is actually stopping"
        );
        assert_eq!(ended.owner_epoch, 2, "{backend}");

        let route = store
            .media_session_route(session)
            .await
            .unwrap_or_else(|error| panic!("{backend}: read ended route: {error}"))
            .unwrap_or_else(|| panic!("{backend}: the ended row is retained"));
        assert_eq!(route.state, "ended", "{backend}");
        assert!(
            route.lease_expires_at_ms <= 3_000,
            "{backend}: an ended incarnation keeps no live lease"
        );
        assert!(
            store
                .owned_media_sessions("node-new", 3_000)
                .await
                .unwrap_or_else(|error| panic!("{backend}: successor inventory: {error}"))
                .is_empty(),
            "{backend}: the successor must not still own an ended incarnation"
        );

        // The ended incarnation's coordination lease is clamped, not merely
        // its session row: nothing may still hold `session:<incarnation>` at
        // the successor's fence. A third node acquiring it cleanly is what
        // proves the clamp landed on the owner the end actually had.
        let reacquired = store
            .acquire_lease(
                &format!("session:{incarnation}"),
                "node-third",
                3_200,
                9_000,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: reacquire ended lease: {error}"));
        assert!(
            matches!(reacquired, LeaseClaim::Acquired(_)),
            "{backend}: an ended incarnation leaves no live lease behind: {reacquired:?}"
        );

        // Idempotent, and still reporting the same owner.
        let again = store
            .end_media_session(session, 3_100)
            .await
            .unwrap_or_else(|error| panic!("{backend}: repeat end: {error}"))
            .unwrap_or_else(|| panic!("{backend}: repeat end still resolves the route"));
        assert_eq!(again.owner_node_id, "node-new", "{backend}");

        // §7.2: a delete racing a takeover resolves to ended. Once ended, no
        // survivor may claim the incarnation back into life.
        assert!(
            store
                .expired_media_sessions(4_000, 64)
                .await
                .unwrap_or_else(|error| panic!("{backend}: post-end inventory: {error}"))
                .iter()
                .all(|route| route.incarnation_id != incarnation),
            "{backend}: an ended incarnation is not offered for takeover"
        );
        assert!(
            store
                .claim_media_session_takeover(&MediaSessionTakeover {
                    incarnation_id: incarnation.to_owned(),
                    expected_owner_node_id: "node-new".to_owned(),
                    expected_owner_epoch: 2,
                    next_owner_node_id: "node-third".to_owned(),
                    now_ms: 4_000,
                    lease_expires_at_ms: 16_000,
                })
                .await
                .unwrap_or_else(|error| panic!("{backend}: claim an ended incarnation: {error}"))
                .is_none(),
            "{backend}: no replacement child may be created for an ended incarnation"
        );
    })
    .await;
}

#[tokio::test]
async fn media_session_same_playback_replacement_is_admitted_at_user_cap() {
    for_each_backend(|store, backend| async move {
        let user = store
            .create_user("session-cap-user", "hash", false)
            .await
            .unwrap_or_else(|error| panic!("{backend}: create capped session user: {error}"));
        let fingerprint = "d".repeat(64);
        let mut predecessor = None;
        for index in 0_u128..64 {
            let incarnation_id = uuid::Uuid::from_u128(0x1000 + index).to_string();
            let session_id = uuid::Uuid::from_u128(0x2000 + index).to_string();
            let playback_id = format!("cap-playback-{index}");
            store
                .activate_media_session(&MediaSessionActivation {
                    incarnation_id: incarnation_id.clone(),
                    session_id,
                    user_id: user.id,
                    playback_id: playback_id.clone(),
                    expected_predecessor_incarnation_id: None,
                    fence_predecessor: false,
                    request_id: None,
                    request_fingerprint: fingerprint.clone(),
                    owner_node_id: "cap-node".to_owned(),
                    recipe_json: "{}".to_owned(),
                    response_json: "{}".to_owned(),
                    media_origin_ms: 0,
                    now_ms: 100,
                    lease_expires_at_ms: 10_000,
                })
                .await
                .unwrap_or_else(|error| panic!("{backend}: seed capped session: {error}"))
                .unwrap_or_else(|| panic!("{backend}: capped session {index} must activate"));
            if index == 0 {
                predecessor = Some(incarnation_id);
            }
        }

        let first_attempt = uuid::Uuid::from_u128(0x3000).to_string();
        assert!(matches!(
            store
                .claim_media_session_request(
                    user.id,
                    "cap-replacement",
                    &fingerprint,
                    "cap-playback-0",
                    &first_attempt,
                    1_000,
                    1_001,
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: claim capped replacement: {error}")),
            MediaSessionRequestClaim::Acquired { .. }
        ));

        let replacement = uuid::Uuid::from_u128(0x3001).to_string();
        assert!(matches!(
            store
                .claim_media_session_request(
                    user.id,
                    "cap-replacement",
                    &fingerprint,
                    "cap-playback-0",
                    &replacement,
                    1_001,
                    2_000,
                )
                .await
                .unwrap_or_else(|error| {
                    panic!("{backend}: reclaim expired capped replacement: {error}")
                }),
            MediaSessionRequestClaim::Acquired { incarnation_id }
                if incarnation_id == replacement
        ));
        assert!(store
            .assign_media_session_request_owner(
                user.id,
                "cap-replacement",
                &replacement,
                "cap-node",
                1_002,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: own capped replacement: {error}")));
        let outcome = store
            .activate_media_session(&MediaSessionActivation {
                incarnation_id: replacement,
                session_id: uuid::Uuid::from_u128(0x4000).to_string(),
                user_id: user.id,
                playback_id: "cap-playback-0".to_owned(),
                expected_predecessor_incarnation_id: predecessor,
                fence_predecessor: true,
                request_id: Some("cap-replacement".to_owned()),
                request_fingerprint: fingerprint,
                owner_node_id: "cap-node".to_owned(),
                recipe_json: "{}".to_owned(),
                response_json: "{}".to_owned(),
                media_origin_ms: 0,
                now_ms: 1_003,
                lease_expires_at_ms: 10_000,
            })
            .await
            .unwrap_or_else(|error| panic!("{backend}: activate capped replacement: {error}"))
            .unwrap_or_else(|| panic!("{backend}: capped replacement must activate"));
        assert!(outcome.predecessor.is_some(), "{backend}");
    })
    .await;
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn hiqlite_media_activation_requires_its_lease_mutation() {
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let store = open_contract_hiqlite_store(&cluster).await;
    store
        .validation_reset_contract_state()
        .await
        .expect("reset replicated media activation contract state");
    let raw = Client::remote(
        cluster.addresses.clone(),
        true,
        true,
        CONTRACT_API_SECRET.to_owned(),
        true,
        None,
    )
    .await
    .expect("connect raw replicated media activation client");
    let user = store
        .create_user("lease-fence-user", "hash", false)
        .await
        .expect("create lease-fence user");
    let fingerprint = "c".repeat(64);

    let max_incarnation = "00000000-0000-4000-8000-0000000000c1";
    assert!(matches!(
        store
            .claim_media_session_request(
                user.id,
                "max-revision-attempt",
                &fingerprint,
                "max-revision-playback",
                max_incarnation,
                100,
                200,
            )
            .await
            .expect("claim max-revision activation"),
        MediaSessionRequestClaim::Acquired { .. }
    ));
    assert!(store
        .assign_media_session_request_owner(
            user.id,
            "max-revision-attempt",
            max_incarnation,
            "removed-node",
            110,
        )
        .await
        .expect("assign max-revision owner"));
    raw.execute(
        "INSERT INTO job_leases
            (resource, owner_node_id, fence, revision, expires_at_ms, updated_at_ms)
         VALUES ($1, $2, 1, 9223372036854775807, $3, $4)",
        hiqlite::params!(
            format!("session:{max_incarnation}"),
            "removed-node",
            320_i64,
            120_i64
        ),
    )
    .await
    .expect("seed exhausted media lease");
    assert!(store
        .activate_media_session(&MediaSessionActivation {
            incarnation_id: max_incarnation.to_owned(),
            session_id: "00000000-0000-4000-8000-0000000000c2".to_owned(),
            user_id: user.id,
            playback_id: "max-revision-playback".to_owned(),
            expected_predecessor_incarnation_id: None,
            fence_predecessor: false,
            request_id: Some("max-revision-attempt".to_owned()),
            request_fingerprint: fingerprint.clone(),
            owner_node_id: "removed-node".to_owned(),
            recipe_json: "{}".to_owned(),
            response_json: "{}".to_owned(),
            media_origin_ms: 0,
            now_ms: 120,
            lease_expires_at_ms: 320,
        })
        .await
        .expect("reject exhausted media lease")
        .is_none());

    let removed_incarnation = "00000000-0000-4000-8000-0000000000c3";
    assert!(matches!(
        store
            .claim_media_session_request(
                user.id,
                "removed-owner-attempt",
                &fingerprint,
                "removed-owner-playback",
                removed_incarnation,
                130,
                230,
            )
            .await
            .expect("claim removed-owner activation"),
        MediaSessionRequestClaim::Acquired { .. }
    ));
    assert!(store
        .assign_media_session_request_owner(
            user.id,
            "removed-owner-attempt",
            removed_incarnation,
            "removed-node",
            140,
        )
        .await
        .expect("assign removed owner"));
    raw.execute(
        "INSERT INTO job_leases
            (resource, owner_node_id, fence, revision, expires_at_ms, updated_at_ms)
         VALUES ($1, $2, 1, 7, $3, $4)",
        hiqlite::params!(
            format!("session:{removed_incarnation}"),
            "removed-node",
            350_i64,
            150_i64
        ),
    )
    .await
    .expect("seed retained removed-owner media lease");
    raw.execute(
        "INSERT INTO settings (key, value, updated_at) VALUES ($1, '', $2)",
        hiqlite::params!("internal.cluster_job_owner_removed.removed-node", 150_i64),
    )
    .await
    .expect("mark media owner removed");
    assert!(store
        .activate_media_session(&MediaSessionActivation {
            incarnation_id: removed_incarnation.to_owned(),
            session_id: "00000000-0000-4000-8000-0000000000c4".to_owned(),
            user_id: user.id,
            playback_id: "removed-owner-playback".to_owned(),
            expected_predecessor_incarnation_id: None,
            fence_predecessor: false,
            request_id: Some("removed-owner-attempt".to_owned()),
            request_fingerprint: fingerprint,
            owner_node_id: "removed-node".to_owned(),
            recipe_json: "{}".to_owned(),
            response_json: "{}".to_owned(),
            media_origin_ms: 0,
            now_ms: 150,
            lease_expires_at_ms: 350,
        })
        .await
        .expect("reject removed-owner media lease")
        .is_none());
}

#[tokio::test]
async fn fenced_publication_contract_runs_through_dyn_store() {
    for_each_backend(|store, backend| async move {
        let library = store
            .create_library(&NewLibrary {
                name: "Fenced Contract Library".to_owned(),
                kind: LibraryKind::Books,
                paths: vec![PathBuf::from("/contract/fenced")],
                anime: false,
            })
            .await
            .unwrap_or_else(|error| panic!("{backend}: create library: {error}"));
        let clock = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("contract clock after epoch")
            .as_millis()
            .min(i64::MAX as u128) as i64;
        let first = acquired(
            store
                .acquire_lease(
                    "scan:library:fenced",
                    "node-a",
                    clock,
                    clock.saturating_add(90_000),
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: acquire publication lease: {error}")),
            backend,
        );
        let mut current = first.clone();
        let replacement = publication_successor(&current);
        store
            .put_setting_fenced("contract.fenced", "first", &current, &replacement)
            .await
            .unwrap_or_else(|error| panic!("{backend}: valid publication: {error}"));
        current = replacement;
        let replacement = publication_successor(&current);
        assert!(
            store
                .put_setting_if_absent_fenced(
                    "contract.fenced.immutable",
                    "first",
                    &current,
                    &replacement,
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: immutable publication: {error}")),
            "{backend}: first immutable publication must win"
        );
        current = replacement;
        let replacement = publication_successor(&current);
        assert!(
            !store
                .put_setting_if_absent_fenced(
                    "contract.fenced.immutable",
                    "replacement",
                    &current,
                    &replacement,
                )
                .await
                .unwrap_or_else(|error| {
                    panic!("{backend}: duplicate immutable publication: {error}")
                }),
            "{backend}: immutable publication must retain its first value"
        );
        current = replacement;
        let replacement = publication_successor(&current);
        let baseline_book = store
            .insert_item_fenced(
                &NewItem {
                    library_id: library.id,
                    kind: ItemKind::Book,
                    parent_id: None,
                    title: "Baseline Book".to_owned(),
                    year: Some(2025),
                    season_number: None,
                    episode_number: None,
                },
                &current,
                &replacement,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: baseline fenced insert: {error}"));
        current = replacement;
        let baseline_before_conditional = store
            .get_item(baseline_book)
            .await
            .unwrap_or_else(|error| panic!("{backend}: read conditional baseline: {error}"))
            .unwrap_or_else(|| panic!("{backend}: conditional baseline disappeared"));
        let replacement = publication_successor(&current);
        assert!(
            store
                .apply_book_metadata_if_current_fenced(
                    &baseline_before_conditional,
                    &BookMetadataPatch {
                        title: None,
                        author: None,
                        work_id: None,
                        edition_id: None,
                        poster_path: None,
                        source: BookMetadataSource::Epub,
                        required_origin: None,
                    },
                    None,
                    &current,
                    &replacement,
                )
                .await
                .unwrap_or_else(|error| {
                    panic!("{backend}: conditional book publication: {error}")
                }),
            "{backend}: current snapshot and lease must publish"
        );
        current = replacement;
        let baseline_after_conditional = store
            .get_item(baseline_book)
            .await
            .unwrap_or_else(|error| panic!("{backend}: reread conditional baseline: {error}"))
            .unwrap_or_else(|| panic!("{backend}: conditional baseline disappeared"));
        let replacement = publication_successor(&current);
        let baseline_file = store
            .upsert_file_fenced(
                baseline_book,
                "/contract/fenced/baseline.epub",
                42,
                7,
                &ProbeResult::default(),
                &current,
                &replacement,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: baseline fenced file: {error}"));
        current = replacement;
        let replacement = publication_successor(&current);
        assert_eq!(
            store
                .ensure_library_root_fingerprint_fenced(
                    library.id,
                    "fenced-root",
                    true,
                    &current,
                    &replacement,
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: baseline fenced root: {error}")),
            RootFingerprintStatus::Established
        );
        current = replacement;
        let replacement = publication_successor(&current);
        assert!(
            store
                .claim_cache_entry_fenced(
                    "contract-fenced-cache",
                    baseline_file,
                    1,
                    "node-a",
                    "contract/fenced-cache/f1",
                    &current,
                    &replacement,
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: baseline cache claim: {error}")),
            "{backend}: baseline cache claim must be new"
        );
        current = replacement;

        let renewed = store
            .renew_lease(
                &current,
                clock.saturating_add(1),
                current.expires_at_unix_ms.saturating_add(90_000),
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: renew publication lease: {error}"))
            .unwrap_or_else(|| panic!("{backend}: publication lease must renew"));
        let stale_replacement = publication_successor(&first);
        macro_rules! assert_stale {
            ($future:expr, $operation:literal) => {
                assert!(
                    matches!($future.await, Err(StoreError::FenceRejected { .. })),
                    "{}: stale {} publication must reject",
                    backend,
                    $operation
                );
            };
        }
        assert_stale!(
            store.put_setting_fenced(
                "contract.fenced",
                "stale-revision",
                &first,
                &stale_replacement,
            ),
            "setting"
        );
        assert_stale!(
            store.put_setting_if_absent_fenced(
                "contract.fenced.stale-immutable",
                "stale-revision",
                &first,
                &stale_replacement,
            ),
            "immutable setting"
        );
        assert_stale!(
            store.mark_library_scanned_fenced(library.id, true, &first, &stale_replacement),
            "scan stamp"
        );
        assert_stale!(
            store.insert_item_fenced(
                &NewItem {
                    library_id: library.id,
                    kind: ItemKind::Book,
                    parent_id: None,
                    title: "Stale Insert".to_owned(),
                    year: None,
                    season_number: None,
                    episode_number: None,
                },
                &first,
                &stale_replacement,
            ),
            "item insert"
        );
        assert_stale!(
            store.apply_metadata_fenced(
                baseline_book,
                &MetadataPatch {
                    overview: Some("stale overview".to_owned()),
                    ..MetadataPatch::default()
                },
                &first,
                &stale_replacement,
            ),
            "metadata"
        );
        assert_stale!(
            store.apply_book_metadata_fenced(
                baseline_book,
                &BookMetadataPatch {
                    title: None,
                    author: Some("Stale Author".to_owned()),
                    work_id: Some("stale-work".to_owned()),
                    edition_id: None,
                    poster_path: None,
                    source: BookMetadataSource::Curator,
                    required_origin: None,
                },
                &first,
                &stale_replacement,
            ),
            "book metadata"
        );
        assert_stale!(
            store.apply_book_metadata_if_current_fenced(
                &baseline_after_conditional,
                &BookMetadataPatch {
                    title: None,
                    author: Some("Stale Conditional Author".to_owned()),
                    work_id: None,
                    edition_id: None,
                    poster_path: None,
                    source: BookMetadataSource::Curator,
                    required_origin: None,
                },
                None,
                &first,
                &stale_replacement,
            ),
            "conditional book metadata"
        );
        assert_stale!(
            store.set_nfo_seeded_fenced(baseline_book, &first, &stale_replacement),
            "nfo stamp"
        );
        assert_stale!(
            store.upsert_file_fenced(
                baseline_book,
                "/contract/fenced/baseline.epub",
                999,
                99,
                &ProbeResult::default(),
                &first,
                &stale_replacement,
            ),
            "file upsert"
        );
        assert_stale!(
            store.ensure_library_root_fingerprint_fenced(
                library.id,
                "stale-root",
                true,
                &first,
                &stale_replacement,
            ),
            "root fingerprint"
        );
        assert_stale!(
            store.reconcile_library_fenced(
                library.id,
                "fenced-root",
                &[baseline_file],
                1,
                &first,
                &stale_replacement,
            ),
            "reconcile"
        );
        assert_stale!(
            store.claim_cache_entry_fenced(
                "stale-cache-claim",
                baseline_file,
                1,
                "node-a",
                "contract/stale-cache/f1",
                &first,
                &stale_replacement,
            ),
            "cache claim"
        );
        assert_stale!(
            store.touch_cache_claim_fenced(
                "contract-fenced-cache",
                "node-a",
                &first,
                &stale_replacement,
            ),
            "cache claim heartbeat"
        );
        assert_stale!(
            store.complete_cache_entry_fenced(
                "contract-fenced-cache",
                "node-a",
                "contract/fenced-cache/stale",
                999,
                &first,
                &stale_replacement,
            ),
            "cache completion"
        );
        assert_stale!(
            store.forget_cache_entry_fenced(
                "contract-fenced-cache",
                "node-a",
                "local",
                &first,
                &stale_replacement,
            ),
            "cache forget"
        );
        assert_eq!(
            store
                .get_setting("contract.fenced")
                .await
                .expect("read first fenced value"),
            Some("first".to_owned()),
            "{backend}: same-fence stale revision must not mutate"
        );
        let library_after_stale = store
            .get_library(library.id)
            .await
            .unwrap_or_else(|error| panic!("{backend}: read stale library: {error}"))
            .unwrap_or_else(|| panic!("{backend}: stale library disappeared"));
        assert_eq!(library_after_stale.last_scan_at, None, "{backend}");
        assert_eq!(library_after_stale.last_refresh_at, None, "{backend}");
        let books_after_stale = store
            .book_items(library.id, None)
            .await
            .unwrap_or_else(|error| panic!("{backend}: read stale books: {error}"));
        assert_eq!(books_after_stale.len(), 1, "{backend}: stale insert leaked");
        let baseline_after_stale = store
            .get_item(baseline_book)
            .await
            .unwrap_or_else(|error| panic!("{backend}: read stale item: {error}"))
            .unwrap_or_else(|| panic!("{backend}: baseline item disappeared"));
        assert_eq!(baseline_after_stale.overview, None, "{backend}");
        assert_eq!(baseline_after_stale.author, None, "{backend}");
        assert_eq!(baseline_after_stale.book_work_id, None, "{backend}");
        assert_eq!(baseline_after_stale.nfo_seeded_at, None, "{backend}");
        let file_after_stale = store
            .get_file(baseline_file)
            .await
            .unwrap_or_else(|error| panic!("{backend}: read stale file: {error}"))
            .unwrap_or_else(|| panic!("{backend}: baseline file disappeared"));
        assert_eq!(file_after_stale.size, 42, "{backend}");
        assert_eq!(file_after_stale.mtime, 7, "{backend}");
        assert_eq!(
            store
                .ensure_library_root_fingerprint(library.id, "fenced-root", false)
                .await
                .unwrap_or_else(|error| panic!("{backend}: read fenced root: {error}")),
            RootFingerprintStatus::Matched,
            "{backend}: stale root mutation leaked"
        );
        assert!(
            store
                .cache_hit("contract-fenced-cache", "node-a")
                .await
                .unwrap_or_else(|error| panic!("{backend}: stale cache hit check: {error}"))
                .is_none(),
            "{backend}: stale completion made an incomplete claim serveable"
        );
        let cache_rows = store
            .all_cache_rows("node-a")
            .await
            .unwrap_or_else(|error| panic!("{backend}: stale cache rows: {error}"));
        assert_eq!(cache_rows.len(), 1, "{backend}: stale cache claim leaked");
        assert_eq!(
            cache_rows[0].relative_dir, "contract/fenced-cache/f1",
            "{backend}: stale cache generation replaced the live claim"
        );

        let successor = acquired(
            store
                .acquire_lease(
                    "scan:library:fenced",
                    "node-b",
                    renewed.expires_at_unix_ms,
                    renewed.expires_at_unix_ms.saturating_add(90_000),
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: successor acquire: {error}")),
            backend,
        );
        let mut successor_current = successor.clone();
        let replacement = publication_successor(&successor_current);
        assert!(
            store
                .claim_cache_entry_fenced(
                    "contract-fenced-cache",
                    baseline_file,
                    1,
                    "node-a",
                    "contract/fenced-cache/f2",
                    &successor_current,
                    &replacement,
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: successor cache takeover: {error}")),
            "{backend}: successor must take over the incomplete generation"
        );
        successor_current = replacement;
        let stale_renewed_replacement = publication_successor(&renewed);
        assert!(matches!(
            store
                .forget_cache_entry_fenced(
                    "contract-fenced-cache",
                    "node-a",
                    "local",
                    &renewed,
                    &stale_renewed_replacement,
                )
                .await,
            Err(StoreError::FenceRejected { .. })
        ));
        let takeover_rows = store
            .all_cache_rows("node-a")
            .await
            .unwrap_or_else(|error| panic!("{backend}: read cache takeover: {error}"));
        assert_eq!(takeover_rows.len(), 1, "{backend}");
        assert_eq!(
            takeover_rows[0].relative_dir, "contract/fenced-cache/f2",
            "{backend}: stale incomplete-generation cleanup removed the successor claim"
        );
        assert!(matches!(
            store
                .put_setting_fenced(
                    "contract.fenced",
                    "stale-owner",
                    &renewed,
                    &stale_renewed_replacement,
                )
                .await,
            Err(StoreError::FenceRejected { .. })
        ));
        let replacement = publication_successor(&successor_current);
        store
            .put_setting_fenced(
                "contract.fenced",
                "successor",
                &successor_current,
                &replacement,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: successor publication: {error}"));
        successor_current = replacement;

        let replacement = publication_successor(&successor_current);
        let book = store
            .insert_item_fenced(
                &NewItem {
                    library_id: library.id,
                    kind: ItemKind::Book,
                    parent_id: None,
                    title: "Fenced Book".to_owned(),
                    year: Some(2026),
                    season_number: None,
                    episode_number: None,
                },
                &successor_current,
                &replacement,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: fenced insert: {error}"));
        successor_current = replacement;
        let replacement = publication_successor(&successor_current);
        store
            .apply_metadata_fenced(
                book,
                &MetadataPatch {
                    overview: Some("published under lease".to_owned()),
                    ..MetadataPatch::default()
                },
                &successor_current,
                &replacement,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: fenced metadata: {error}"));
        successor_current = replacement;
        let replacement = publication_successor(&successor_current);
        store
            .apply_book_metadata_fenced(
                book,
                &BookMetadataPatch {
                    title: None,
                    author: Some("Lease Owner".to_owned()),
                    work_id: Some("work:fenced".to_owned()),
                    edition_id: Some("edition:fenced".to_owned()),
                    poster_path: None,
                    source: BookMetadataSource::Curator,
                    required_origin: None,
                },
                &successor_current,
                &replacement,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: fenced book metadata: {error}"));
        successor_current = replacement;
        let replacement = publication_successor(&successor_current);
        store
            .set_nfo_seeded_fenced(book, &successor_current, &replacement)
            .await
            .unwrap_or_else(|error| panic!("{backend}: fenced nfo stamp: {error}"));
        successor_current = replacement;
        let replacement = publication_successor(&successor_current);
        store
            .upsert_file_fenced(
                book,
                "/contract/fenced/book.epub",
                42,
                7,
                &ProbeResult::default(),
                &successor_current,
                &replacement,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: fenced file upsert: {error}"));
        successor_current = replacement;
        let replacement = publication_successor(&successor_current);
        assert_eq!(
            store
                .ensure_library_root_fingerprint_fenced(
                    library.id,
                    "fenced-root",
                    true,
                    &successor_current,
                    &replacement,
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: fenced root: {error}")),
            RootFingerprintStatus::Matched
        );
        successor_current = replacement;
        let replacement = publication_successor(&successor_current);
        assert!(matches!(
            store
                .reconcile_library_fenced(
                    library.id,
                    "fenced-root",
                    &[],
                    0,
                    &successor_current,
                    &replacement,
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: fenced reconcile: {error}")),
            ReconcileOutcome::Applied { .. }
        ));
        successor_current = replacement;
        let replacement = publication_successor(&successor_current);
        store
            .mark_library_scanned_fenced(library.id, true, &successor_current, &replacement)
            .await
            .unwrap_or_else(|error| panic!("{backend}: fenced scan stamp: {error}"));
        successor_current = replacement;
        let replacement = publication_successor(&successor_current);
        assert!(
            store
                .claim_cache_entry_fenced(
                    "contract-fenced-cache",
                    baseline_file,
                    1,
                    "node-a",
                    "contract/fenced-cache/f2",
                    &successor_current,
                    &replacement,
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: successor cache claim: {error}")),
            "{backend}: successor must take over an incomplete generation"
        );
        successor_current = replacement;
        let replacement = publication_successor(&successor_current);
        store
            .touch_cache_claim_fenced(
                "contract-fenced-cache",
                "node-a",
                &successor_current,
                &replacement,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: successor cache touch: {error}"));
        successor_current = replacement;
        let replacement = publication_successor(&successor_current);
        store
            .complete_cache_entry_fenced(
                "contract-fenced-cache",
                "node-a",
                "contract/fenced-cache/f2",
                4242,
                &successor_current,
                &replacement,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: successor cache completion: {error}"));
        successor_current = replacement;
        let cache_hit = store
            .cache_hit("contract-fenced-cache", "node-a")
            .await
            .unwrap_or_else(|error| panic!("{backend}: successor cache hit: {error}"))
            .unwrap_or_else(|| panic!("{backend}: successor cache completion not serveable"));
        assert_eq!(cache_hit.relative_dir, "contract/fenced-cache/f2");
        assert_eq!(cache_hit.bytes, 4242);
        let replacement = publication_successor(&successor_current);
        store
            .forget_cache_entry_fenced(
                "contract-fenced-cache",
                "node-a",
                "local",
                &successor_current,
                &replacement,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: successor cache forget: {error}"));
        assert!(
            store
                .cache_hit("contract-fenced-cache", "node-a")
                .await
                .unwrap_or_else(|error| panic!("{backend}: forgotten cache lookup: {error}"))
                .is_none(),
            "{backend}: fenced cache forget left a serveable row"
        );
        assert_eq!(
            store
                .get_setting("contract.fenced")
                .await
                .expect("read successor fenced value"),
            Some("successor".to_owned()),
            "{backend}: successor wins after stale owner resumes"
        );

        let expiring_now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("publication expiry clock after epoch")
            .as_millis()
            .min(i64::MAX as u128) as i64;
        let expired = acquired(
            store
                .acquire_lease(
                    "contract:expired-publication",
                    "node-expired",
                    expiring_now,
                    expiring_now.saturating_add(100),
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: acquire expiring lease: {error}")),
            backend,
        );
        let expired_replacement = publication_successor(&expired);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(matches!(
            store
                .put_setting_fenced(
                    "contract.expired-publication",
                    "must-not-commit",
                    &expired,
                    &expired_replacement,
                )
                .await,
            Err(StoreError::FenceRejected { .. })
        ));
        assert_eq!(
            store
                .get_setting("contract.expired-publication")
                .await
                .unwrap_or_else(|error| panic!("{backend}: read expired publication: {error}")),
            None,
            "{backend}: an expired predecessor committed a fenced publication"
        );
    })
    .await;
}

#[tokio::test]
async fn artwork_repair_publication_fails_closed_without_a_job_lease() {
    let store = SqliteStore::open_in_memory().expect("store");
    let library = store
        .create_library(&NewLibrary {
            name: "Repair Fence Library".to_owned(),
            kind: LibraryKind::Books,
            paths: vec![],
            anime: false,
        })
        .await
        .expect("library");
    let item_id = store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Book,
            parent_id: None,
            title: "Repair Fence Book".to_owned(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("book");
    let item = store
        .get_item(item_id)
        .await
        .expect("read book")
        .expect("book exists");
    let repair_fence = ArtworkRepairFence {
        item_id,
        owner_node_id: "node-a".to_owned(),
        leader_term: 1,
        generation: 1,
    };
    let publisher = PublicationStore::unfenced(&store);

    for result in [
        publisher
            .put_setting_if_absent_if_artwork_repair_current(
                "repair.origin",
                "value",
                item_id,
                &repair_fence,
            )
            .await,
        publisher
            .apply_metadata_if_artwork_repair_current(
                item_id,
                &MetadataPatch::default(),
                &repair_fence,
            )
            .await,
        publisher
            .apply_book_metadata_if_current(
                &item,
                &BookMetadataPatch {
                    title: None,
                    author: None,
                    work_id: None,
                    edition_id: None,
                    poster_path: None,
                    source: BookMetadataSource::Curator,
                    required_origin: None,
                },
                Some(&repair_fence),
            )
            .await,
    ] {
        assert!(
            matches!(result, Err(StoreError::Task(message)) if message.contains("singleton job lease")),
            "an artwork repair mutation must not accept a per-item fence alone"
        );
    }
}

#[tokio::test]
async fn distributed_pretranscode_contract_runs_through_dyn_store() {
    for_each_backend(|store, backend| async move {
        let library = store
            .create_library(&NewLibrary {
                name: "Pretranscode Contract Library".to_owned(),
                kind: LibraryKind::Movies,
                paths: vec![PathBuf::from("/contract/pretranscode")],
                anime: false,
            })
            .await
            .unwrap_or_else(|error| panic!("{backend}: create library: {error}"));
        let mut files = Vec::new();
        for ordinal in 1..=5 {
            let item = store
                .insert_item(&NewItem {
                    library_id: library.id,
                    kind: ItemKind::Movie,
                    parent_id: None,
                    title: format!("Queue Movie {ordinal}"),
                    year: Some(2026),
                    season_number: None,
                    episode_number: None,
                })
                .await
                .unwrap_or_else(|error| panic!("{backend}: insert item {ordinal}: {error}"));
            files.push(
                store
                    .upsert_file(
                        item,
                        &format!("/contract/pretranscode/movie-{ordinal}.mkv"),
                        10_000 + ordinal,
                        20_000 + ordinal,
                        &ProbeResult::default(),
                    )
                    .await
                    .unwrap_or_else(|error| panic!("{backend}: insert file {ordinal}: {error}")),
            );
        }
        // The concurrent claim below deliberately deletes one of files[0..=2].
        // Reserve files[4] for fixtures that must remain readable afterward so
        // their outcome does not depend on which worker wins which claim.
        let stable_source_file = files[4];
        let stable_source_size = 10_005;
        let stable_source_mtime = 20_005;

        let requirements = serde_json::to_string(&PretranscodeRequirements {
            version: PretranscodeRequirements::VERSION,
            decoder: "h264".to_owned(),
            acceptable_encoder_families: vec!["software".to_owned()],
            output_contract: "hls-v1".to_owned(),
            tone_map: false,
            output_grade: "sdr".to_owned(),
            scratch_bytes: 1_024,
        })
        .expect("serialize requirements");
        let capable = PretranscodeWorkerCapabilities {
            version: PretranscodeRequirements::VERSION,
            decoders: vec!["h264".to_owned()],
            encoder_families: vec!["software".to_owned()],
            max_target_height: 2_160,
            output_contracts: vec!["hls-v1".to_owned()],
            tone_map: false,
            output_grades: vec!["sdr".to_owned()],
            scratch_bytes: 2_048,
        };
        let incompatible = PretranscodeWorkerCapabilities {
            encoder_families: vec!["unsupported-hardware".to_owned()],
            ..capable.clone()
        };

        let queue_clock = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("contract clock after epoch")
            .as_millis()
            .min(i64::MAX as u128) as i64;
        let queue_time = |tick: i64| queue_clock.saturating_add(tick.saturating_mul(1_000));
        let first_candidate_lease = acquired(
            store
                .acquire_lease(
                    "pretranscode:candidates",
                    "scheduler-a",
                    queue_clock,
                    queue_clock.saturating_add(90_000),
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: acquire candidate lease: {error}")),
            backend,
        );
        let mut candidate_lease = store
            .renew_lease(
                &first_candidate_lease,
                queue_clock.saturating_add(1),
                queue_clock.saturating_add(180_000),
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: renew candidate lease: {error}"))
            .unwrap_or_else(|| panic!("{backend}: candidate lease did not renew"));
        let make_job = |ordinal: usize, id: &str| NewPretranscodeJob {
            id: id.to_owned(),
            dedupe_key: format!("pretranscode-contract-{ordinal}"),
            file_id: files[ordinal - 1],
            source_size: 10_000 + ordinal as i64,
            source_mtime: 20_000 + ordinal as i64,
            target_height: 720,
            policy_generation: "contract-v1".to_owned(),
            requirements_json: requirements.clone(),
            reason: "recent".to_owned(),
            priority: 400 - ordinal as i64,
            not_before_ms: 160,
            created_at_ms: 160 + ordinal as i64,
        };
        let jobs = [
            make_job(1, "00000000-0000-4000-8000-000000000101"),
            make_job(2, "00000000-0000-4000-8000-000000000102"),
            make_job(3, "00000000-0000-4000-8000-000000000103"),
        ];
        let stale_candidate_replacement = publication_successor(&first_candidate_lease);
        assert!(matches!(
            store
                .enqueue_pretranscode_job(
                    &jobs[0],
                    &first_candidate_lease,
                    &stale_candidate_replacement,
                )
                .await,
            Err(StoreError::FenceRejected { .. })
        ));
        for job in &jobs {
            assert!(
                enqueue_with_successor(store.as_ref(), job, &mut candidate_lease)
                    .await
                    .unwrap_or_else(|error| panic!("{backend}: enqueue job: {error}")),
                "{backend}: each distinct job must enqueue"
            );
        }
        assert!(
            !enqueue_with_successor(store.as_ref(), &jobs[0], &mut candidate_lease)
                .await
                .unwrap_or_else(|error| panic!("{backend}: dedupe enqueue: {error}")),
            "{backend}: an active dedupe key must be unique"
        );
        let mut active_ids = store
            .active_pretranscode_job_ids()
            .await
            .unwrap_or_else(|error| panic!("{backend}: active job inventory: {error}"));
        active_ids.sort_unstable();
        assert_eq!(
            active_ids,
            jobs.iter().map(|job| job.id.clone()).collect::<Vec<_>>(),
            "{backend}: active inventory must include every queued job"
        );
        assert!(
            store
                .claim_pretranscode_job(
                    "node-x",
                    &incompatible,
                    &[],
                    queue_time(200),
                    queue_time(500),
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: incompatible claim: {error}"))
                .is_none(),
            "{backend}: an incompatible worker claimed a job"
        );

        let (claim_a, claim_b, claim_c) = tokio::join!(
            store.claim_pretranscode_job("node-a", &capable, &[], queue_time(200), queue_time(500)),
            store.claim_pretranscode_job("node-b", &capable, &[], queue_time(200), queue_time(500)),
            store.claim_pretranscode_job("node-c", &capable, &[], queue_time(200), queue_time(500)),
        );
        let claimed = [
            ("node-a", claim_a),
            ("node-b", claim_b),
            ("node-c", claim_c),
        ]
        .into_iter()
        .map(|(node, result)| {
            result
                .unwrap_or_else(|error| panic!("{backend}: claim for {node}: {error}"))
                .unwrap_or_else(|| panic!("{backend}: no job for {node}"))
        })
        .collect::<Vec<_>>();
        assert_eq!(
            claimed
                .iter()
                .map(|job| job.id.as_str())
                .collect::<BTreeSet<_>>()
                .len(),
            3,
            "{backend}: workers must claim distinct rows"
        );
        for job in &claimed {
            let staging = store
                .pretranscode_staging_jobs(&job.owner_node_id)
                .await
                .unwrap_or_else(|error| panic!("{backend}: staging ownership: {error}"));
            assert!(
                staging.contains(&job.id),
                "{backend}: claimed staging is unowned"
            );
        }
        assert!(
            store
                .cache_hit("contract-recipe-a", "node-a")
                .await
                .unwrap_or_else(|error| panic!("{backend}: pre-publication cache lookup: {error}"))
                .is_none(),
            "{backend}: claiming must not expose a cache location"
        );

        let renewed_a = store
            .renew_pretranscode_job(&claimed[0], queue_time(250), queue_time(700))
            .await
            .unwrap_or_else(|error| panic!("{backend}: renew worker: {error}"))
            .unwrap_or_else(|| panic!("{backend}: worker claim did not renew"));
        assert!(store
            .complete_pretranscode_job(
                &claimed[1],
                "contract-recipe-b",
                1,
                "contract/pretranscode/b",
                4_096,
                None,
                &"b".repeat(64),
                queue_time(260),
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: complete second job: {error}")));
        assert!(
            !store
                .complete_pretranscode_job(
                    &claimed[1],
                    "contract-recipe-b",
                    1,
                    "contract/pretranscode/b",
                    4_096,
                    None,
                    &"b".repeat(64),
                    queue_time(261),
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: replay completion: {error}")),
            "{backend}: a terminal queue token replayed successfully"
        );
        assert_eq!(
            store
                .delete_files(&[claimed[2].file_id])
                .await
                .unwrap_or_else(|error| panic!("{backend}: delete claimed source: {error}")),
            1,
            "{backend}: source fixture should delete"
        );
        assert!(
            !store
                .complete_pretranscode_job(
                    &claimed[2],
                    "contract-recipe-c",
                    1,
                    "contract/pretranscode/c",
                    4_096,
                    None,
                    &"c".repeat(64),
                    queue_time(270),
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: deleted-source completion: {error}")),
            "{backend}: deleted source work became ready"
        );

        let successor = store
            .claim_pretranscode_job("node-d", &capable, &[], queue_time(701), queue_time(1_000))
            .await
            .unwrap_or_else(|error| panic!("{backend}: expired takeover: {error}"))
            .unwrap_or_else(|| panic!("{backend}: expired job was not restarted"));
        assert_eq!(successor.id, renewed_a.id, "{backend}");
        assert_eq!(successor.fence, renewed_a.fence + 1, "{backend}");
        assert!(
            !store
                .pretranscode_staging_jobs("node-a")
                .await
                .unwrap_or_else(|error| panic!("{backend}: predecessor staging: {error}"))
                .contains(&successor.id),
            "{backend}: takeover left predecessor staging authoritative"
        );
        assert!(
            store
                .pretranscode_staging_jobs("node-d")
                .await
                .unwrap_or_else(|error| panic!("{backend}: successor staging: {error}"))
                .contains(&successor.id),
            "{backend}: takeover did not transfer staging identity"
        );
        assert!(
            !store
                .complete_pretranscode_job(
                    &renewed_a,
                    "contract-recipe-a",
                    1,
                    "contract/pretranscode/stale-a",
                    8_192,
                    None,
                    &"a".repeat(64),
                    queue_time(702),
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: stale completion: {error}")),
            "{backend}: stale worker published after takeover"
        );
        assert!(
            store
                .cache_hit("contract-recipe-a", "node-a")
                .await
                .unwrap_or_else(|error| panic!("{backend}: stale cache lookup: {error}"))
                .is_none(),
            "{backend}: stale completion leaked a location"
        );
        assert!(
            store
                .complete_pretranscode_job(
                    &successor,
                    "contract-recipe-a",
                    1,
                    "contract/pretranscode/d",
                    8_192,
                    None,
                    &"d".repeat(64),
                    queue_time(703),
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: successor completion: {error}")),
            "{backend}: current worker could not publish"
        );
        let ready = store
            .cache_hit("contract-recipe-a", "node-d")
            .await
            .unwrap_or_else(|error| panic!("{backend}: atomic cache lookup: {error}"))
            .unwrap_or_else(|| panic!("{backend}: ready job has no cache location"));
        assert_eq!(ready.relative_dir, "contract/pretranscode/d", "{backend}");
        assert_eq!(ready.bytes, 8_192, "{backend}");
        assert_eq!(
            ready.manifest_digest,
            Some("d".repeat(64)),
            "{backend}: fenced cache location lost its manifest authority"
        );

        store
            .forget_cache_entry("contract-recipe-a", "node-d", "local")
            .await
            .unwrap_or_else(|error| panic!("{backend}: evict ready cache: {error}"));
        let retry = NewPretranscodeJob {
            id: "00000000-0000-4000-8000-000000000104".to_owned(),
            dedupe_key: successor.dedupe_key.clone(),
            file_id: successor.file_id,
            source_size: successor.source_size,
            source_mtime: successor.source_mtime,
            target_height: successor.target_height,
            policy_generation: successor.policy_generation.clone(),
            requirements_json: successor.requirements_json.clone(),
            reason: successor.reason.clone(),
            priority: successor.priority,
            not_before_ms: 704,
            created_at_ms: 704,
        };
        assert!(
            enqueue_with_successor(store.as_ref(), &retry, &mut candidate_lease)
                .await
                .unwrap_or_else(|error| panic!("{backend}: re-enqueue evicted job: {error}")),
            "{backend}: eviction must make the generation eligible again"
        );
        let refused = store
            .claim_pretranscode_job("node-e", &capable, &[], queue_time(705), queue_time(1_005))
            .await
            .unwrap_or_else(|error| panic!("{backend}: claim re-enqueued job: {error}"))
            .unwrap_or_else(|| panic!("{backend}: re-enqueued job was not claimable"));
        assert_eq!(refused.dedupe_key, retry.dedupe_key, "{backend}");
        assert!(store
            .yield_pretranscode_job(&refused, queue_time(706), queue_time(706))
            .await
            .unwrap_or_else(|error| panic!("{backend}: unreadable-node yield: {error}")));
        assert!(
            store
                .claim_pretranscode_job(
                    "node-e",
                    &capable,
                    std::slice::from_ref(&refused.id),
                    queue_time(706),
                    queue_time(1_006),
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: local refusal claim: {error}"))
                .is_none(),
            "{backend}: a node immediately reclaimed the source it had refused"
        );
        let reclaimed = store
            .claim_pretranscode_job("node-f", &capable, &[], queue_time(706), queue_time(1_006))
            .await
            .unwrap_or_else(|error| panic!("{backend}: mounted peer claim: {error}"))
            .unwrap_or_else(|| panic!("{backend}: local refusal blocked a mounted peer"));
        assert_eq!(reclaimed.id, refused.id, "{backend}");
        assert_eq!(
            reclaimed.attempts, 0,
            "{backend}: a node-local source refusal consumed the global failure budget"
        );

        let mut failing = reclaimed;
        for attempt in 1..=5 {
            let failed_at = queue_time(710 + attempt * 2);
            assert!(
                store
                    .fail_pretranscode_job(&failing, "contract_failure", failed_at, failed_at)
                    .await
                    .unwrap_or_else(|error| panic!("{backend}: fail attempt {attempt}: {error}")),
                "{backend}: current failure settlement was rejected"
            );
            if attempt < 5 {
                failing = store
                    .claim_pretranscode_job(
                        "node-e",
                        &capable,
                        &[],
                        failed_at + 1_000,
                        failed_at + 101_000,
                    )
                    .await
                    .unwrap_or_else(|error| {
                        panic!("{backend}: reclaim failed attempt {attempt}: {error}")
                    })
                    .unwrap_or_else(|| panic!("{backend}: failed row did not requeue"));
                assert_eq!(failing.attempts, attempt, "{backend}");
            }
        }
        let terminal_retry = NewPretranscodeJob {
            id: "00000000-0000-4000-8000-000000000105".to_owned(),
            created_at_ms: 730,
            not_before_ms: 730,
            ..retry
        };
        assert!(
            !enqueue_with_successor(store.as_ref(), &terminal_retry, &mut candidate_lease)
                .await
                .unwrap_or_else(|error| panic!("{backend}: terminal dedupe enqueue: {error}")),
            "{backend}: five failures must remain terminal for this dedupe generation"
        );

        let incompatible_requirements = serde_json::to_string(&PretranscodeRequirements {
            acceptable_encoder_families: vec!["unsupported-hardware".to_owned()],
            ..serde_json::from_str::<PretranscodeRequirements>(&requirements)
                .expect("parse compatible requirements")
        })
        .expect("serialize incompatible requirements");
        for ordinal in 0..=128_i64 {
            let compatible_tail = ordinal == 128;
            let starvation_job = NewPretranscodeJob {
                id: uuid::Uuid::new_v4().to_string(),
                dedupe_key: format!("claim-pagination-{ordinal}"),
                file_id: stable_source_file,
                source_size: stable_source_size,
                source_mtime: stable_source_mtime,
                target_height: 720,
                policy_generation: "pagination-v1".to_owned(),
                requirements_json: if compatible_tail {
                    requirements.clone()
                } else {
                    incompatible_requirements.clone()
                },
                reason: "recent".to_owned(),
                priority: 10_000 - ordinal,
                not_before_ms: 800,
                created_at_ms: 800 + ordinal,
            };
            assert!(
                enqueue_with_successor(store.as_ref(), &starvation_job, &mut candidate_lease)
                    .await
                    .unwrap_or_else(|error| panic!("{backend}: enqueue pagination row: {error}")),
                "{backend}: pagination fixture row did not enqueue"
            );
        }
        let paged_claim = store
            .claim_pretranscode_job(
                "node-pagination",
                &capable,
                &[],
                queue_time(950),
                queue_time(1_250),
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: paged capability claim: {error}"))
            .unwrap_or_else(|| panic!("{backend}: compatible row after page one starved"));
        assert_eq!(
            paged_claim.dedupe_key, "claim-pagination-128",
            "{backend}: claim did not preserve highest-compatible ordering across pages"
        );

        let legacy_recipe = "contract-legacy-cache-reuse";
        assert!(store
            .claim_cache_entry(
                legacy_recipe,
                stable_source_file,
                1,
                "node-legacy",
                "contract/pretranscode/legacy",
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: legacy cache claim: {error}")));
        store
            .complete_cache_entry(legacy_recipe, "node-legacy", 16_384)
            .await
            .unwrap_or_else(|error| panic!("{backend}: legacy cache complete: {error}"));
        assert_eq!(
            store
                .cache_hit(legacy_recipe, "node-legacy")
                .await
                .unwrap_or_else(|error| panic!("{backend}: legacy cache lookup: {error}"))
                .and_then(|entry| entry.manifest_digest),
            None,
            "{backend}: fixture must exercise the explicit legacy manifest path"
        );
        let legacy_job = NewPretranscodeJob {
            id: "00000000-0000-4000-8000-000000000106".to_owned(),
            dedupe_key: "pretranscode-legacy-cache-reuse".to_owned(),
            file_id: stable_source_file,
            source_size: stable_source_size,
            source_mtime: stable_source_mtime,
            target_height: 720,
            policy_generation: "legacy-upgrade-v1".to_owned(),
            requirements_json: requirements.clone(),
            reason: "recent".to_owned(),
            priority: 20_000,
            not_before_ms: 960,
            created_at_ms: 960,
        };
        assert!(
            enqueue_with_successor(store.as_ref(), &legacy_job, &mut candidate_lease)
                .await
                .unwrap_or_else(|error| panic!("{backend}: enqueue legacy reuse: {error}"))
        );
        let legacy_claim = store
            .claim_pretranscode_job(
                "node-legacy",
                &capable,
                &[],
                queue_time(961),
                queue_time(1_261),
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: claim legacy reuse: {error}"))
            .unwrap_or_else(|| panic!("{backend}: legacy reuse was not claimable"));
        let adopted_digest = "e".repeat(64);
        assert!(store
            .complete_pretranscode_job(
                &legacy_claim,
                legacy_recipe,
                1,
                "contract/pretranscode/legacy",
                16_896,
                Some(16_384),
                &adopted_digest,
                queue_time(962),
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: bind legacy manifest: {error}")));
        let adopted = store
            .cache_hit(legacy_recipe, "node-legacy")
            .await
            .unwrap_or_else(|error| panic!("{backend}: bound legacy lookup: {error}"))
            .unwrap_or_else(|| panic!("{backend}: adopted legacy location disappeared"));
        assert_eq!(
            adopted.manifest_digest,
            Some(adopted_digest.clone()),
            "{backend}: exact legacy location did not adopt its first fenced manifest"
        );
        assert_eq!(
            adopted.bytes, 16_896,
            "{backend}: adopted manifest bytes were not charged to the cache budget"
        );
        let compact_ready = store
            .pretranscode_job(&legacy_claim.id)
            .await
            .unwrap_or_else(|error| panic!("{backend}: inspect ready job: {error}"))
            .unwrap_or_else(|| panic!("{backend}: ready job disappeared"));
        assert_eq!(compact_ready.state, "ready", "{backend}");
        assert!(compact_ready.owner_node_id.is_empty(), "{backend}");
        assert_eq!(compact_ready.lease_expires_ms, 0, "{backend}");
        assert!(compact_ready.policy_generation.is_empty(), "{backend}");
        assert_eq!(compact_ready.requirements_json, "{}", "{backend}");

        let wrong_digest = "f".repeat(64);
        let wrong_check = CacheManifestCheck {
            recipe_hash: legacy_recipe.to_owned(),
            node_id: "node-legacy".to_owned(),
            storage_class: "local".to_owned(),
            relative_dir: "contract/pretranscode/legacy".to_owned(),
            manifest_digest: wrong_digest.clone(),
            next_object_index: 8,
            observed_at: 123,
        };
        assert_eq!(
            store
                .mark_cache_manifests_checked(std::slice::from_ref(&wrong_check))
                .await
                .unwrap_or_else(|error| panic!("{backend}: stale scrub cursor: {error}")),
            0,
            "{backend}: stale manifest identity advanced a replacement cursor"
        );
        let exact_check = CacheManifestCheck {
            manifest_digest: adopted_digest.clone(),
            ..wrong_check
        };
        assert_eq!(
            store
                .mark_cache_manifests_checked(std::slice::from_ref(&exact_check))
                .await
                .unwrap_or_else(|error| panic!("{backend}: exact scrub cursor: {error}")),
            1,
            "{backend}: exact manifest cursor did not advance"
        );
        assert_eq!(
            store
                .cache_hit(legacy_recipe, "node-legacy")
                .await
                .unwrap_or_else(|error| panic!("{backend}: scrubbed cache lookup: {error}"))
                .map(|entry| entry.scrub_object_index),
            Some(8),
            "{backend}: scrub cursor was not durable"
        );
        assert!(!store
            .invalidate_cache_entry(
                legacy_recipe,
                "node-legacy",
                "local",
                "contract/pretranscode/replacement",
                Some(&adopted_digest),
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: stale relative invalidation: {error}")));
        assert!(!store
            .invalidate_cache_entry(
                legacy_recipe,
                "node-legacy",
                "local",
                "contract/pretranscode/legacy",
                Some(&wrong_digest),
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: stale digest invalidation: {error}")));
        assert!(store
            .cache_hit(legacy_recipe, "node-legacy")
            .await
            .unwrap_or_else(|error| panic!("{backend}: cache after stale CAS: {error}"))
            .is_some());
        assert!(store
            .invalidate_cache_entry(
                legacy_recipe,
                "node-legacy",
                "local",
                "contract/pretranscode/legacy",
                Some(&adopted_digest),
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: exact invalidation: {error}")));
        assert!(store
            .cache_hit(legacy_recipe, "node-legacy")
            .await
            .unwrap_or_else(|error| panic!("{backend}: invalidated cache lookup: {error}"))
            .is_none());

        let conflicting_recipe = "contract-recipe-identity-collision";
        assert!(store
            .claim_cache_entry(
                conflicting_recipe,
                files[3],
                1,
                "recipe-owner",
                "contract/pretranscode/recipe-owner",
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: seed conflicting recipe: {error}")));
        let collision_job = make_job(5, "00000000-0000-4000-8000-000000000108");
        assert!(
            enqueue_with_successor(store.as_ref(), &collision_job, &mut candidate_lease)
                .await
                .unwrap_or_else(|error| panic!("{backend}: enqueue collision job: {error}"))
        );
        let collision_claim = store
            .claim_pretranscode_job(
                "node-collision",
                &capable,
                &[],
                queue_time(963),
                queue_time(1_263),
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: claim collision job: {error}"))
            .unwrap_or_else(|| panic!("{backend}: collision job was not claimable"));
        assert_eq!(collision_claim.id, collision_job.id, "{backend}");
        assert!(
            !store
                .complete_pretranscode_job(
                    &collision_claim,
                    conflicting_recipe,
                    1,
                    "contract/pretranscode/collision",
                    4_096,
                    None,
                    &"9".repeat(64),
                    queue_time(964),
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: conflicting completion: {error}")),
            "{backend}: completion rebound a recipe hash owned by a different file"
        );
        assert!(store
            .cache_hit(conflicting_recipe, "node-collision")
            .await
            .unwrap_or_else(|error| panic!("{backend}: collision cache lookup: {error}"))
            .is_none());
        assert_eq!(
            store
                .pretranscode_job(&collision_claim.id)
                .await
                .unwrap_or_else(|error| panic!("{backend}: collision job lookup: {error}"))
                .map(|job| job.state),
            Some("running".to_owned()),
            "{backend}: rejected recipe identity still settled the queue job"
        );
        let replacement_job = NewPretranscodeJob {
            id: "00000000-0000-4000-8000-000000000107".to_owned(),
            not_before_ms: 963,
            created_at_ms: 963,
            ..legacy_job
        };
        assert!(
            enqueue_with_successor(store.as_ref(), &replacement_job, &mut candidate_lease,)
                .await
                .unwrap_or_else(|error| panic!("{backend}: enqueue corrupt replacement: {error}"))
        );
    })
    .await;
}

#[tokio::test]
async fn curator_origin_pruning_is_bounded_and_reference_safe() {
    for_each_backend(|store, backend| async move {
        let library = store
            .create_library(&NewLibrary {
                name: "Curator Origin Retention".to_owned(),
                kind: LibraryKind::Books,
                paths: vec![PathBuf::from("/contract/curator-origin")],
                anime: false,
            })
            .await
            .unwrap_or_else(|error| panic!("{backend}: create library: {error}"));
        let item_id = store
            .insert_item(&NewItem {
                library_id: library.id,
                kind: ItemKind::Book,
                parent_id: None,
                title: "Origin Retention".to_owned(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .unwrap_or_else(|error| panic!("{backend}: insert item: {error}"));
        let filename = format!("{item_id}-poster-{}.jpg", "a".repeat(64));
        let origin = serde_json::json!({
            "edition_id": "curator:item:retention:ebook",
            "url": "https://covers.openlibrary.org/b/id/1-L.jpg",
            "filename": filename,
        })
        .to_string();
        for suffix in ["first", "second"] {
            assert!(store
                .put_setting_if_absent(
                    &format!("internal.book_cover_origin.{item_id}.{suffix}"),
                    &origin,
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: insert origin: {error}")));
        }
        let expected = store
            .get_item(item_id)
            .await
            .unwrap_or_else(|error| panic!("{backend}: read origin item: {error}"))
            .unwrap_or_else(|| panic!("{backend}: origin item disappeared"));
        assert_eq!(
            store
                .prune_unreferenced_book_cover_origins(&filename)
                .await
                .unwrap_or_else(|error| panic!("{backend}: interleaved prune: {error}")),
            2
        );
        let origin_key = format!("internal.book_cover_origin.{item_id}.first");
        let guarded_patch = BookMetadataPatch {
            title: None,
            author: None,
            work_id: Some("curator:work:retention".to_owned()),
            edition_id: Some("curator:item:retention:ebook".to_owned()),
            poster_path: Some(filename.clone()),
            source: BookMetadataSource::Curator,
            required_origin: Some((origin_key.clone(), origin.clone())),
        };
        assert!(
            !store
                .apply_book_metadata_if_current(&expected, &guarded_patch, None)
                .await
                .unwrap_or_else(|error| panic!("{backend}: pruned-origin CAS: {error}")),
            "{backend}: item publication committed after its exact origin was pruned"
        );
        assert!(store
            .put_setting_if_absent(&origin_key, &origin)
            .await
            .unwrap_or_else(|error| panic!("{backend}: restore origin: {error}")));
        assert!(store
            .apply_book_metadata_if_current(&expected, &guarded_patch, None)
            .await
            .unwrap_or_else(|error| panic!("{backend}: guarded origin CAS: {error}")));
        assert_eq!(
            store
                .prune_unreferenced_book_cover_origins(&filename)
                .await
                .unwrap_or_else(|error| panic!("{backend}: protected prune: {error}")),
            0,
            "{backend}: a referenced generation lost its repair origins"
        );
        store
            .apply_metadata(
                item_id,
                &MetadataPatch {
                    poster_path: Some(format!("{item_id}-poster-{}.jpg", "b".repeat(64))),
                    ..Default::default()
                },
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: supersede generation: {error}"));
        assert_eq!(
            store
                .prune_unreferenced_book_cover_origins(&filename)
                .await
                .unwrap_or_else(|error| panic!("{backend}: orphan prune: {error}")),
            1,
            "{backend}: obsolete immutable origins grew without a bound"
        );
    })
    .await;
}

fn acquired(outcome: LeaseClaim, backend: &str) -> Lease {
    match outcome {
        LeaseClaim::Acquired(lease) => lease,
        held => panic!("{backend}: expected acquired lease, got {held:?}"),
    }
}

fn publication_successor(lease: &Lease) -> Lease {
    lease
        .publication_successor()
        .expect("publication successor")
}

async fn enqueue_with_successor(
    store: &dyn Store,
    job: &NewPretranscodeJob,
    lease: &mut Lease,
) -> Result<bool, StoreError> {
    let replacement = publication_successor(lease);
    let result = store
        .enqueue_pretranscode_job(job, lease, &replacement)
        .await;
    if result.is_ok() {
        *lease = replacement;
    }
    result
}

async fn assert_distinct_pretranscode_claims_from_separate_handles(
    stores: [Arc<dyn Store>; 3],
    backend: &str,
) {
    let seed = &stores[0];
    let library = seed
        .create_library(&NewLibrary {
            name: "Separate Queue Claim Library".to_owned(),
            kind: LibraryKind::Movies,
            paths: vec![PathBuf::from("/contract/separate-pretranscode")],
            anime: false,
        })
        .await
        .unwrap_or_else(|error| panic!("{backend}: create library: {error}"));
    let requirements = serde_json::to_string(&PretranscodeRequirements {
        version: PretranscodeRequirements::VERSION,
        decoder: "h264".to_owned(),
        acceptable_encoder_families: vec!["software".to_owned()],
        output_contract: "hls-v1".to_owned(),
        tone_map: false,
        output_grade: "sdr".to_owned(),
        scratch_bytes: 1,
    })
    .expect("serialize requirements");
    let queue_clock = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("contract clock after epoch")
        .as_millis()
        .min(i64::MAX as u128) as i64;
    let mut lease = acquired(
        seed.acquire_lease(
            "separate-queue-seed",
            "scheduler",
            queue_clock,
            queue_clock.saturating_add(90_000),
        )
        .await
        .unwrap_or_else(|error| panic!("{backend}: acquire seed lease: {error}")),
        backend,
    );
    const ROUNDS: i64 = 8;
    for ordinal in 1_i64..=ROUNDS * 3 {
        let item = seed
            .insert_item(&NewItem {
                library_id: library.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: format!("Separate Queue Movie {ordinal}"),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .unwrap_or_else(|error| panic!("{backend}: insert item: {error}"));
        let file_id = seed
            .upsert_file(
                item,
                &format!("/contract/separate-pretranscode/{ordinal}.mkv"),
                10_000 + ordinal,
                20_000 + ordinal,
                &ProbeResult::default(),
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: insert file: {error}"));
        let job = NewPretranscodeJob {
            id: uuid::Uuid::new_v4().to_string(),
            dedupe_key: format!("separate-queue-{ordinal}"),
            file_id,
            source_size: 10_000 + ordinal,
            source_mtime: 20_000 + ordinal,
            target_height: 720,
            policy_generation: "separate-v1".to_owned(),
            requirements_json: requirements.clone(),
            reason: "recent".to_owned(),
            priority: 100,
            not_before_ms: 110,
            created_at_ms: 110 + ordinal,
        };
        assert!(enqueue_with_successor(seed.as_ref(), &job, &mut lease)
            .await
            .unwrap_or_else(|error| panic!("{backend}: enqueue: {error}")));
    }
    let capabilities = PretranscodeWorkerCapabilities {
        version: PretranscodeRequirements::VERSION,
        decoders: vec!["h264".to_owned()],
        encoder_families: vec!["software".to_owned()],
        max_target_height: 2_160,
        output_contracts: vec!["hls-v1".to_owned()],
        tone_map: false,
        output_grades: vec!["sdr".to_owned()],
        scratch_bytes: 2,
    };
    let mut ids = BTreeSet::new();
    for round in 0..ROUNDS {
        let start = Arc::new(tokio::sync::Barrier::new(3));
        let claim = |store: Arc<dyn Store>, node: String, start: Arc<tokio::sync::Barrier>| {
            let capabilities = capabilities.clone();
            async move {
                start.wait().await;
                store
                    .claim_pretranscode_job(&node, &capabilities, &[], 200 + round, 500 + round)
                    .await
            }
        };
        let (a, b, c) = tokio::join!(
            claim(
                Arc::clone(&stores[0]),
                format!("separate-a-{round}"),
                Arc::clone(&start)
            ),
            claim(
                Arc::clone(&stores[1]),
                format!("separate-b-{round}"),
                Arc::clone(&start)
            ),
            claim(Arc::clone(&stores[2]), format!("separate-c-{round}"), start),
        );
        for result in [a, b, c] {
            let id = result
                .unwrap_or_else(|error| panic!("{backend}: separate claim: {error}"))
                .unwrap_or_else(|| panic!("{backend}: separate claimant found no work"))
                .id;
            assert!(
                ids.insert(id),
                "{backend}: separate clients duplicated a claim"
            );
        }
    }
    assert_eq!(
        ids.len(),
        (ROUNDS * 3) as usize,
        "{backend}: separate clients duplicated a claim"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn separate_sqlite_connections_claim_distinct_pretranscode_rows() {
    let directory = tempfile::tempdir().expect("separate SQLite queue directory");
    let path = directory.path().join("plurx.db");
    let stores: [Arc<dyn Store>; 3] = [
        Arc::new(SqliteStore::open(&path).expect("first SQLite queue store")),
        Arc::new(SqliteStore::open(&path).expect("second SQLite queue store")),
        Arc::new(SqliteStore::open(&path).expect("third SQLite queue store")),
    ];
    assert_distinct_pretranscode_claims_from_separate_handles(stores, "sqlite-separate").await;
}

#[cfg(feature = "hiqlite-contract-tests")]
async fn open_contract_hiqlite_store(cluster: &ContractCluster) -> HiqliteAuthStore {
    let client = Client::remote(
        cluster.addresses.clone(),
        true,
        true,
        CONTRACT_API_SECRET.to_owned(),
        false,
        None,
    )
    .await
    .expect("connect contract client to three voters");
    let telemetry_path = cluster._root.path().join("contract-client-telemetry.db");
    for attempt in 1..=REPLICATED_DEADLINE_ATTEMPTS {
        match classify_replicated(
            HiqliteAuthStore::bootstrap(client.clone(), CONTRACT_INSTANCE_ID, &telemetry_path)
                .await,
        ) {
            ReplicatedOutcome::Ready(store) => return store,
            ReplicatedOutcome::Fault(error) => panic!("bootstrap contract store: {error}"),
            ReplicatedOutcome::Deadline if attempt < REPLICATED_DEADLINE_ATTEMPTS => {
                // Bootstrap is intentionally idempotent: every schema statement
                // and identity seed tolerates an already-committed predecessor.
                // A cancelled client future can therefore be retried without
                // weakening production's three-second operation deadline.
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            ReplicatedOutcome::Deadline => break,
        }
    }
    panic!(
        "{}",
        replicated_deadline_diagnosis("bootstrap contract store", REPLICATED_DEADLINE_ATTEMPTS,)
    )
}

#[cfg(feature = "hiqlite-contract-tests")]
async fn contract_applied_index(client: &Client) -> u64 {
    client
        .metrics_db()
        .await
        .expect("read replicated-store metrics")
        .last_applied
        .expect("replicated store has an applied index")
        .index
}

#[cfg(feature = "hiqlite-contract-tests")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ContractLeaderPoint {
    leader_id: u64,
    term: u64,
    committed_index: u64,
}

#[cfg(feature = "hiqlite-contract-tests")]
async fn contract_leader_point(client: &Client) -> ContractLeaderPoint {
    let watermark = client
        .db_quorum_watermark()
        .await
        .expect("obtain a leader-issued quorum commit watermark");
    ContractLeaderPoint {
        leader_id: watermark.leader_id,
        term: watermark.term,
        committed_index: watermark.committed_index,
    }
}

#[cfg(feature = "hiqlite-contract-tests")]
fn contract_stable_leader_delta(before: ContractLeaderPoint, after: ContractLeaderPoint) -> u64 {
    assert_eq!(
        (after.leader_id, after.term),
        (before.leader_id, before.term),
        "replicated-write entry accounting requires one stable leader and term"
    );
    after.committed_index.saturating_sub(before.committed_index)
}

#[cfg(feature = "hiqlite-contract-tests")]
async fn contract_cache_touch_times(
    client: &Client,
    recipe_hash: &str,
    node_id: &str,
) -> CacheTouchTimes {
    let mut rows = client
        .query_consistent_map::<CacheTouchTimes, _>(
            "SELECT last_used_at, last_seen_at FROM transcode_cache_locations \
             WHERE recipe_hash = $1 AND node_id = $2 AND storage_class = 'local'",
            hiqlite::params!(recipe_hash, node_id),
        )
        .await
        .expect("read cache touch timestamps");
    assert_eq!(rows.len(), 1, "cache timestamp fixture must be unique");
    rows.pop().expect("cache timestamp row")
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manifest_scrub_cursor_batch_costs_one_consensus_entry() {
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let store = open_contract_hiqlite_store(&cluster).await;
    store
        .validation_reset_contract_state()
        .await
        .expect("reset manifest cursor state");
    let library = store
        .create_library(&NewLibrary {
            name: "Manifest Cursor Contract".to_owned(),
            kind: LibraryKind::Movies,
            paths: vec![],
            anime: false,
        })
        .await
        .expect("manifest cursor library");
    let item = store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "Manifest Cursor Movie".to_owned(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("manifest cursor item");
    let file = store
        .upsert_file(
            item,
            "/contract/manifest-cursor/movie.mkv",
            1,
            1,
            &ProbeResult::default(),
        )
        .await
        .expect("manifest cursor file");
    let queue_clock = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("contract clock after epoch")
        .as_millis()
        .min(i64::MAX as u128) as i64;
    let mut lease = acquired(
        store
            .acquire_lease(
                "manifest-cursor-candidates",
                "scheduler",
                queue_clock,
                queue_clock.saturating_add(90_000),
            )
            .await
            .expect("manifest cursor lease"),
        "hiqlite manifest cursor",
    );
    let requirements = serde_json::to_string(&PretranscodeRequirements {
        version: PretranscodeRequirements::VERSION,
        decoder: "h264".to_owned(),
        acceptable_encoder_families: vec!["software".to_owned()],
        output_contract: "hls-v1".to_owned(),
        tone_map: false,
        output_grade: "sdr".to_owned(),
        scratch_bytes: 1,
    })
    .expect("manifest cursor requirements");
    let capabilities = PretranscodeWorkerCapabilities {
        version: PretranscodeRequirements::VERSION,
        decoders: vec!["h264".to_owned()],
        encoder_families: vec!["software".to_owned()],
        max_target_height: 2_160,
        output_contracts: vec!["hls-v1".to_owned()],
        tone_map: false,
        output_grades: vec!["sdr".to_owned()],
        scratch_bytes: 2,
    };
    let fixtures = [
        (
            "00000000-0000-4000-8000-000000000501",
            "manifest-cursor-a",
            "ma/manifest-cursor-a",
            "a".repeat(64),
        ),
        (
            "00000000-0000-4000-8000-000000000502",
            "manifest-cursor-b",
            "mb/manifest-cursor-b",
            "b".repeat(64),
        ),
    ];
    for (ordinal, (job_id, recipe, relative, digest)) in fixtures.iter().enumerate() {
        let fixture_job = NewPretranscodeJob {
            id: (*job_id).to_owned(),
            dedupe_key: format!("manifest-cursor-{ordinal}"),
            file_id: file,
            source_size: 1,
            source_mtime: 1,
            target_height: 720,
            policy_generation: "cursor-v1".to_owned(),
            requirements_json: requirements.clone(),
            reason: "recent".to_owned(),
            priority: 100,
            not_before_ms: 110,
            created_at_ms: 110 + ordinal as i64,
        };
        assert!(enqueue_with_successor(&store, &fixture_job, &mut lease)
            .await
            .expect("enqueue manifest cursor job"));
        let claimed = store
            .claim_pretranscode_job("manifest-node", &capabilities, &[], 120, 1_000)
            .await
            .expect("claim manifest cursor job")
            .expect("manifest cursor job");
        assert_eq!(claimed.id, *job_id);
        assert!(store
            .complete_pretranscode_job(&claimed, recipe, 1, relative, 100, None, digest, 130)
            .await
            .expect("complete manifest cursor job"));
    }
    let checks = fixtures
        .iter()
        .map(|(_, recipe, relative, digest)| CacheManifestCheck {
            recipe_hash: (*recipe).to_owned(),
            node_id: "manifest-node".to_owned(),
            storage_class: "local".to_owned(),
            relative_dir: (*relative).to_owned(),
            manifest_digest: digest.clone(),
            next_object_index: 8,
            observed_at: 123,
        })
        .collect::<Vec<_>>();
    let observer = Client::remote(
        cluster.addresses.clone(),
        true,
        true,
        CONTRACT_API_SECRET.to_owned(),
        false,
        None,
    )
    .await
    .expect("connect manifest cursor observer");
    let before = contract_leader_point(&observer).await;
    assert_eq!(
        store
            .mark_cache_manifests_checked(&checks)
            .await
            .expect("advance manifest cursors"),
        2
    );
    assert_eq!(
        contract_stable_leader_delta(before, contract_leader_point(&observer).await),
        1,
        "one scrub page must be one consensus transaction"
    );
    for (_, recipe, _, _) in fixtures {
        assert_eq!(
            store
                .cache_hit(recipe, "manifest-node")
                .await
                .expect("manifest cursor cache lookup")
                .expect("manifest cursor location")
                .scrub_object_index,
            8
        );
    }
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn separate_replicated_clients_claim_distinct_pretranscode_rows() {
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let mut opened = Vec::<Arc<dyn Store>>::new();
    for ordinal in 0..3 {
        // These are direct voter addresses, not proxies. Rotating the roster
        // deliberately puts a follower first for two clients; ordinary remote
        // mode must discover the leader instead of pinning that first voter.
        let mut addresses = cluster.addresses.clone();
        addresses.rotate_left(ordinal);
        let client = Client::remote(
            addresses,
            true,
            true,
            CONTRACT_API_SECRET.to_owned(),
            false,
            None,
        )
        .await
        .unwrap_or_else(|error| panic!("connect queue client {ordinal}: {error}"));
        let telemetry = cluster
            ._root
            .path()
            .join(format!("separate-queue-{ordinal}-telemetry.db"));
        let store = if ordinal == 0 {
            let store = HiqliteAuthStore::bootstrap(client, CONTRACT_INSTANCE_ID, &telemetry)
                .await
                .expect("bootstrap separate queue store");
            store
                .validation_reset_contract_state()
                .await
                .expect("reset separate queue state");
            store
        } else {
            HiqliteAuthStore::open(client, &telemetry)
                .await
                .unwrap_or_else(|error| panic!("open queue client {ordinal}: {error}"))
        };
        opened.push(Arc::new(store));
    }
    assert_eq!(opened.len(), 3, "three queue clients");
    let stores = [opened.remove(0), opened.remove(0), opened.remove(0)];
    assert_distinct_pretranscode_claims_from_separate_handles(stores, "hiqlite-separate").await;
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn separate_clients_racing_an_expired_lease_choose_one_fenced_owner() {
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let bootstrap = open_contract_hiqlite_store(&cluster).await;
    bootstrap
        .validation_reset_contract_state()
        .await
        .expect("reset replicated lease-race state");
    let seed = acquired(
        bootstrap
            .acquire_lease("lease-race", "departed-node", 100, 200)
            .await
            .expect("seed expired lease"),
        "hiqlite seed",
    );
    assert_eq!(seed.fence, 1);

    let addresses_a = cluster.addresses.clone();
    let mut addresses_b = cluster.addresses.clone();
    addresses_b.rotate_left(1);
    let client_a = Client::remote(
        addresses_a,
        true,
        true,
        CONTRACT_API_SECRET.to_owned(),
        false,
        None,
    )
    .await
    .expect("connect first racing client");
    let client_b = Client::remote(
        addresses_b,
        true,
        true,
        CONTRACT_API_SECRET.to_owned(),
        false,
        None,
    )
    .await
    .expect("connect second racing client");
    let store_a = HiqliteAuthStore::open(
        client_a,
        &cluster._root.path().join("lease-race-a-telemetry.db"),
    )
    .await
    .expect("open first racing store");
    let store_b = HiqliteAuthStore::open(
        client_b,
        &cluster._root.path().join("lease-race-b-telemetry.db"),
    )
    .await
    .expect("open second racing store");

    let (outcome_a, outcome_b) = tokio::join!(
        store_a.acquire_lease("lease-race", "node-a", 200, 400),
        store_b.acquire_lease("lease-race", "node-b", 200, 400),
    );
    let outcome_a = outcome_a.expect("first race result");
    let outcome_b = outcome_b.expect("second race result");
    let winner = match (outcome_a, outcome_b) {
        (
            LeaseClaim::Acquired(winner),
            LeaseClaim::Held {
                owner_node_id,
                fence,
                expires_at_unix_ms,
            },
        )
        | (
            LeaseClaim::Held {
                owner_node_id,
                fence,
                expires_at_unix_ms,
            },
            LeaseClaim::Acquired(winner),
        ) => {
            assert_eq!(winner.fence, 2);
            assert_eq!(owner_node_id, winner.owner_node_id);
            assert_eq!(fence, winner.fence);
            assert_eq!(expires_at_unix_ms, winner.expires_at_unix_ms);
            winner
        }
        outcomes => panic!("expected one acquired and one held outcome, got {outcomes:?}"),
    };

    let renewed = store_a
        .renew_lease(&winner, 250, 500)
        .await
        .expect("renew race winner through first client")
        .expect("race winner remains current");
    assert!(
        !store_b
            .release_lease(&winner, 300)
            .await
            .expect("delayed cross-client release result"),
        "the pre-renewal token must not release its same-fence successor"
    );
    assert!(
        store_b
            .renew_lease(&winner, 300, 450)
            .await
            .expect("delayed cross-client renewal result")
            .is_none(),
        "the pre-renewal token must not replace its same-fence successor"
    );
    assert!(
        store_b
            .release_lease(&renewed, 600)
            .await
            .expect("late cross-client release of exact token"),
        "an exact expired token can release without extending its expiry"
    );
    assert!(
        store_a
            .renew_lease(&renewed, 450, 650)
            .await
            .expect("delayed renewal after late cross-client release")
            .is_none(),
        "the late release must invalidate the same-fence token before takeover"
    );
    let successor = acquired(
        store_a
            .acquire_lease("lease-race", "successor-node", 550, 700)
            .await
            .expect("take over after a release whose clock is ahead"),
        "hiqlite cross-client release",
    );
    assert_eq!(successor.fence, 3);

    let aba_first = acquired(
        store_a
            .acquire_lease("lease-aba", "node-a", 100, 200)
            .await
            .expect("acquire cross-client ABA fixture"),
        "hiqlite ABA fixture",
    );
    let aba_renewed = store_a
        .renew_lease(&aba_first, 150, 300)
        .await
        .expect("renew cross-client ABA fixture")
        .expect("cross-client ABA fixture remains current");
    assert!(store_b
        .release_lease(&aba_renewed, 200)
        .await
        .expect("release cross-client ABA fixture"));
    assert!(
        store_a
            .renew_lease(&aba_first, 150, 350)
            .await
            .expect("delayed cross-client ABA renewal")
            .is_none(),
        "a recurring expiry must not recreate an older token across clients"
    );
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn separate_clients_cannot_interleave_cache_takeover_with_stale_cleanup() {
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let bootstrap = open_contract_hiqlite_store(&cluster).await;
    bootstrap
        .validation_reset_contract_state()
        .await
        .expect("reset replicated cache-takeover state");
    let library = bootstrap
        .create_library(&NewLibrary {
            name: "Cache Takeover Contract".to_owned(),
            kind: LibraryKind::Movies,
            paths: vec![PathBuf::from("/contract/cache-takeover")],
            anime: false,
        })
        .await
        .expect("create cache-takeover library");
    let item = bootstrap
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "Cache Takeover".to_owned(),
            year: Some(2026),
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("create cache-takeover item");
    let file = bootstrap
        .upsert_file(
            item,
            "/contract/cache-takeover/movie.mkv",
            42,
            7,
            &ProbeResult::default(),
        )
        .await
        .expect("create cache-takeover file");
    let queue_clock = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("contract clock after epoch")
        .as_millis()
        .min(i64::MAX as u128) as i64;
    let departed = acquired(
        bootstrap
            .acquire_lease(
                "candidate:pretranscode",
                "departed-node",
                queue_clock,
                queue_clock.saturating_add(90_000),
            )
            .await
            .expect("acquire departed cache producer lease"),
        "hiqlite cache takeover seed",
    );
    let observer = Client::remote(
        cluster.addresses.clone(),
        true,
        true,
        CONTRACT_API_SECRET.to_owned(),
        false,
        None,
    )
    .await
    .expect("connect cache transaction observer");
    let mut departed_current = departed.clone();
    let replacement = publication_successor(&departed_current);
    let before_claim = contract_applied_index(&observer).await;
    assert!(bootstrap
        .claim_cache_entry_fenced(
            "cache-takeover-recipe",
            file,
            1,
            "cache-node",
            "ca/cache-takeover-recipe-f1",
            &departed_current,
            &replacement,
        )
        .await
        .expect("claim departed incomplete generation"));
    departed_current = replacement;
    assert_eq!(
        contract_applied_index(&observer)
            .await
            .saturating_sub(before_claim),
        1,
        "a fenced cache claim must be one consensus transaction"
    );
    let replacement = publication_successor(&departed_current);
    assert!(bootstrap
        .claim_cache_entry_fenced(
            "cache-forget-transaction-recipe",
            file,
            1,
            "cache-node",
            "ca/cache-forget-transaction-recipe-f1",
            &departed_current,
            &replacement,
        )
        .await
        .expect("claim cache generation for forget transaction contract"));
    departed_current = replacement;
    let replacement = publication_successor(&departed_current);
    let before_forget = contract_applied_index(&observer).await;
    bootstrap
        .forget_cache_entry_fenced(
            "cache-forget-transaction-recipe",
            "cache-node",
            "local",
            &departed_current,
            &replacement,
        )
        .await
        .expect("forget cache generation in one transaction");
    departed_current = replacement;
    assert_eq!(
        contract_applied_index(&observer)
            .await
            .saturating_sub(before_forget),
        1,
        "a fenced cache forget must be one consensus transaction"
    );

    let client_a = Client::remote(
        cluster.addresses.clone(),
        true,
        true,
        CONTRACT_API_SECRET.to_owned(),
        false,
        None,
    )
    .await
    .expect("connect stale cleanup client");
    let mut addresses_b = cluster.addresses.clone();
    addresses_b.rotate_left(1);
    let client_b = Client::remote(
        addresses_b,
        true,
        true,
        CONTRACT_API_SECRET.to_owned(),
        false,
        None,
    )
    .await
    .expect("connect successor claim client");
    let store_a = HiqliteAuthStore::open(
        client_a,
        &cluster._root.path().join("cache-takeover-a-telemetry.db"),
    )
    .await
    .expect("open stale cleanup store");
    let store_b = HiqliteAuthStore::open(
        client_b,
        &cluster._root.path().join("cache-takeover-b-telemetry.db"),
    )
    .await
    .expect("open successor claim store");
    let start = Arc::new(tokio::sync::Barrier::new(2));
    let cleanup_start = Arc::clone(&start);
    let departed_replacement = publication_successor(&departed_current);
    let stale_cleanup = async {
        cleanup_start.wait().await;
        store_a
            .forget_cache_entry_fenced(
                "cache-takeover-recipe",
                "cache-node",
                "local",
                &departed_current,
                &departed_replacement,
            )
            .await
    };
    let takeover_start = Arc::clone(&start);
    let successor_claim = async {
        takeover_start.wait().await;
        // The stale cleanup is itself a fenced publication and may advance
        // the exact lease token by one revision before this acquisition is
        // applied. Take over at that latest possible expiry so both legal
        // transaction orders remain part of the race instead of treating the
        // cleanup winner as an unexpected held lease.
        let takeover_at = departed_replacement.expires_at_unix_ms;
        let successor = acquired(
            store_b
                .acquire_lease(
                    "candidate:pretranscode",
                    "successor-node",
                    takeover_at,
                    takeover_at.saturating_add(90_000),
                )
                .await
                .expect("acquire successor cache producer lease"),
            "hiqlite cache takeover successor",
        );
        let replacement = publication_successor(&successor);
        store_b
            .claim_cache_entry_fenced(
                "cache-takeover-recipe",
                file,
                1,
                "cache-node",
                "ca/cache-takeover-recipe-f2",
                &successor,
                &replacement,
            )
            .await
    };
    let (cleanup_result, claim_result) = tokio::join!(stale_cleanup, successor_claim);
    assert!(
        cleanup_result.is_ok() || matches!(cleanup_result, Err(StoreError::FenceRejected { .. })),
        "the old transaction either commits before takeover or rejects after it"
    );
    assert!(claim_result.expect("successor cache claim transaction"));
    let rows = store_b
        .all_cache_rows("cache-node")
        .await
        .expect("read cache rows after concurrent takeover");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].relative_dir, "ca/cache-takeover-recipe-f2");
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fenced_cache_publication_never_regresses_activity_timestamps() {
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let client = Client::remote(
        cluster.addresses.clone(),
        true,
        true,
        CONTRACT_API_SECRET.to_owned(),
        false,
        None,
    )
    .await
    .expect("connect fenced cache clock observer");
    let telemetry = cluster._root.path().join("fenced-cache-clock-telemetry.db");
    let store = Arc::new(
        HiqliteAuthStore::validation_bootstrap_at(
            client.clone(),
            CONTRACT_INSTANCE_ID,
            &telemetry,
            1_000,
        )
        .await
        .expect("bootstrap fixed-clock fenced cache store"),
    );
    store
        .validation_reset_contract_state()
        .await
        .expect("reset fenced cache clock state");
    let dynamic: Arc<dyn Store> = store.clone();
    let (_, file_id) = seed_file(&dynamic, "fenced-cache-clock").await;
    let lease_clock = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("fenced cache lease clock after epoch")
        .as_millis()
        .min(i64::MAX as u128) as i64;
    let mut lease = acquired(
        store
            .acquire_lease(
                "candidate:fenced-cache-clock",
                "clock-node",
                lease_clock,
                lease_clock.saturating_add(100_000),
            )
            .await
            .expect("acquire fenced cache clock lease"),
        "hiqlite fenced cache clock",
    );
    let replacement = publication_successor(&lease);
    assert!(store
        .claim_cache_entry_fenced(
            "fenced-cache-clock-recipe",
            file_id,
            1,
            "clock-node",
            "fc/fenced-cache-clock-recipe",
            &lease,
            &replacement,
        )
        .await
        .expect("seed fenced cache clock claim"));
    lease = replacement;
    client
        .execute(
            "UPDATE transcode_cache_locations SET last_used_at = $1, last_seen_at = $1 \
             WHERE recipe_hash = $2 AND node_id = $3",
            hiqlite::params!(9_000_i64, "fenced-cache-clock-recipe", "clock-node"),
        )
        .await
        .expect("advance cache timestamps ahead of the store clock");

    let replacement = publication_successor(&lease);
    assert!(store
        .claim_cache_entry_fenced(
            "fenced-cache-clock-recipe",
            file_id,
            1,
            "clock-node",
            "fc/fenced-cache-clock-recipe",
            &lease,
            &replacement,
        )
        .await
        .expect("repeat fenced cache claim after clock rollback"));
    lease = replacement;
    assert_eq!(
        contract_cache_touch_times(&client, "fenced-cache-clock-recipe", "clock-node").await,
        CacheTouchTimes {
            last_used_at: 9_000,
            last_seen_at: 9_000,
        },
        "fenced claim publication must retain newer activity timestamps"
    );

    let replacement = publication_successor(&lease);
    store
        .touch_cache_claim_fenced(
            "fenced-cache-clock-recipe",
            "clock-node",
            &lease,
            &replacement,
        )
        .await
        .expect("touch fenced cache claim after clock rollback");
    lease = replacement;
    let replacement = publication_successor(&lease);
    store
        .complete_cache_entry_fenced(
            "fenced-cache-clock-recipe",
            "clock-node",
            "fc/fenced-cache-clock-recipe",
            4_096,
            &lease,
            &replacement,
        )
        .await
        .expect("complete fenced cache entry after clock rollback");
    lease = replacement;
    assert_eq!(
        contract_cache_touch_times(&client, "fenced-cache-clock-recipe", "clock-node").await,
        CacheTouchTimes {
            last_used_at: 9_000,
            last_seen_at: 9_000,
        },
        "fenced touch and completion must retain newer activity timestamps"
    );

    let requirements = serde_json::to_string(&PretranscodeRequirements {
        version: PretranscodeRequirements::VERSION,
        decoder: "h264".to_owned(),
        acceptable_encoder_families: vec!["software".to_owned()],
        output_contract: "hls-v1".to_owned(),
        tone_map: false,
        output_grade: "sdr".to_owned(),
        scratch_bytes: 1,
    })
    .expect("fenced cache clock requirements");
    let job = NewPretranscodeJob {
        id: "00000000-0000-4000-8000-000000000599".to_owned(),
        dedupe_key: "fenced-cache-clock-job".to_owned(),
        file_id,
        source_size: 10_000,
        source_mtime: 1,
        target_height: 720,
        policy_generation: "clock-v1".to_owned(),
        requirements_json: requirements,
        reason: "recent".to_owned(),
        priority: 100,
        not_before_ms: 1_000,
        created_at_ms: 1_000,
    };
    assert!(enqueue_with_successor(store.as_ref(), &job, &mut lease)
        .await
        .expect("enqueue fixed-clock pretranscode publication"));
    let capabilities = PretranscodeWorkerCapabilities {
        version: PretranscodeRequirements::VERSION,
        decoders: vec!["h264".to_owned()],
        encoder_families: vec!["software".to_owned()],
        max_target_height: 2_160,
        output_contracts: vec!["hls-v1".to_owned()],
        tone_map: false,
        output_grades: vec!["sdr".to_owned()],
        scratch_bytes: 2,
    };
    let claimed = store
        .claim_pretranscode_job("clock-node", &capabilities, &[], 1_000, 10_000)
        .await
        .expect("claim fixed-clock pretranscode publication")
        .expect("fixed-clock pretranscode job");
    assert!(store
        .complete_pretranscode_job(
            &claimed,
            "fenced-cache-clock-recipe",
            1,
            "fc/fenced-cache-clock-recipe",
            4_096,
            None,
            &"c".repeat(64),
            1_000,
        )
        .await
        .expect("complete fixed-clock pretranscode publication"));
    assert_eq!(
        contract_cache_touch_times(&client, "fenced-cache-clock-recipe", "clock-node").await,
        CacheTouchTimes {
            last_used_at: 9_000,
            last_seen_at: 9_000,
        },
        "pretranscode completion must retain newer activity timestamps"
    );
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn token_activity_refresh_has_a_fixed_clock_concurrent_write_budget() {
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let client = Client::remote(
        cluster.addresses.clone(),
        true,
        true,
        CONTRACT_API_SECRET.to_owned(),
        false,
        None,
    )
    .await
    .expect("connect activity-budget client");
    let telemetry = cluster
        ._root
        .path()
        .join("auth-activity-budget-telemetry.db");
    let store = HiqliteAuthStore::validation_bootstrap_at(
        client.clone(),
        CONTRACT_INSTANCE_ID,
        &telemetry,
        1_000,
    )
    .await
    .expect("bootstrap fixed-clock activity-budget store");
    store
        .validation_reset_contract_state()
        .await
        .expect("reset replicated activity-budget state");
    let user = store
        .create_user("activity-budget", "hash", false)
        .await
        .expect("create activity-budget user");
    store
        .create_token("activity-budget-token", user.id, None)
        .await
        .expect("create activity-budget token");
    client
        .execute(
            "UPDATE tokens SET last_seen_at = $1 WHERE token_hash = $2",
            hiqlite::params!(1_i64, "activity-budget-token"),
        )
        .await
        .expect("make token activity refresh due");

    let before = contract_leader_point(&client).await;
    let barrier = Arc::new(tokio::sync::Barrier::new(121));
    let mut requests = tokio::task::JoinSet::new();
    for _ in 0..120 {
        let store = store.clone();
        let barrier = Arc::clone(&barrier);
        requests.spawn(async move {
            barrier.wait().await;
            store
                .user_for_token("activity-budget-token")
                .await
                .expect("authenticate token")
                .expect("resolve token user")
                .id
        });
    }
    barrier.wait().await;
    while let Some(result) = requests.join_next().await {
        assert_eq!(result.expect("join authentication request"), user.id);
    }
    let after_concurrent = contract_leader_point(&client).await;

    assert_eq!(
        contract_stable_leader_delta(before, after_concurrent),
        1,
        "one process may append one token touch for 120 simultaneous requests"
    );

    for _ in 0..120 {
        assert_eq!(
            store
                .user_for_token("activity-budget-token")
                .await
                .expect("authenticate warm token")
                .expect("resolve warm token user")
                .id,
            user.id
        );
    }
    assert_eq!(
        contract_stable_leader_delta(after_concurrent, contract_leader_point(&client).await),
        0,
        "warm sequential authentication must append no activity entries"
    );
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn token_activity_refresh_burst_is_bounded_by_independent_store_count() {
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let client = Client::remote(
        cluster.addresses.clone(),
        true,
        true,
        CONTRACT_API_SECRET.to_owned(),
        false,
        None,
    )
    .await
    .expect("connect multi-process activity-budget client");
    let mut stores = Vec::new();
    for ordinal in 0..3 {
        let mut addresses = cluster.addresses.clone();
        addresses.rotate_left(ordinal);
        let store_client = Client::remote(
            addresses,
            true,
            true,
            CONTRACT_API_SECRET.to_owned(),
            false,
            None,
        )
        .await
        .expect("connect independent token activity-budget client");
        let telemetry = cluster
            ._root
            .path()
            .join(format!("auth-activity-budget-process-{ordinal}.db"));
        stores.push(
            HiqliteAuthStore::validation_bootstrap_at(
                store_client,
                CONTRACT_INSTANCE_ID,
                &telemetry,
                1_000,
            )
            .await
            .expect("bootstrap independent activity-budget store"),
        );
    }
    stores[0]
        .validation_reset_contract_state()
        .await
        .expect("reset replicated multi-process activity-budget state");
    let user = stores[0]
        .create_user("multi-process-activity-budget", "hash", false)
        .await
        .expect("create multi-process activity-budget user");
    stores[0]
        .create_token("multi-process-activity-budget-token", user.id, None)
        .await
        .expect("create multi-process activity-budget token");
    client
        .execute(
            "UPDATE tokens SET last_seen_at = $1 WHERE token_hash = $2",
            hiqlite::params!(1_i64, "multi-process-activity-budget-token"),
        )
        .await
        .expect("make multi-process token activity refresh due");

    let seeded: Vec<I64Value> = client
        .query_consistent_map(
            "SELECT last_seen_at AS value FROM tokens WHERE token_hash = $1",
            hiqlite::params!("multi-process-activity-budget-token"),
        )
        .await
        .expect("confirm the due token timestamp through the measurement leader");
    assert_eq!(seeded.len(), 1);
    assert_eq!(seeded[0].value, 1);
    let before = contract_leader_point(&client).await;
    let barrier = Arc::new(tokio::sync::Barrier::new(121));
    let mut requests = tokio::task::JoinSet::new();
    for ordinal in 0..120 {
        // Clones within each group share a gate. The three independently
        // bootstrapped stores model serving processes with separate gates.
        let store = stores[ordinal % stores.len()].clone();
        let barrier = Arc::clone(&barrier);
        requests.spawn(async move {
            barrier.wait().await;
            store
                .user_for_token("multi-process-activity-budget-token")
                .await
                .expect("authenticate multi-process token")
                .expect("resolve multi-process token user")
                .id
        });
    }
    barrier.wait().await;
    while let Some(result) = requests.join_next().await {
        assert_eq!(result.expect("join multi-process request"), user.id);
    }
    let delta = contract_stable_leader_delta(before, contract_leader_point(&client).await);
    assert!(
        (1..=3).contains(&delta),
        "120 simultaneous requests on three independent Stores appended {delta} activity entries"
    );
    let timestamps: Vec<I64Value> = client
        .query_consistent_map(
            "SELECT last_seen_at AS value FROM tokens WHERE token_hash = $1",
            hiqlite::params!("multi-process-activity-budget-token"),
        )
        .await
        .expect("read the durable multi-process token activity timestamp");
    assert_eq!(timestamps.len(), 1);
    assert_eq!(
        timestamps[0].value, 1_000,
        "all accepted touches converge on one durable timestamp change"
    );
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_key_activity_refresh_burst_is_bounded_by_independent_store_count() {
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let client = Client::remote(
        cluster.addresses.clone(),
        true,
        true,
        CONTRACT_API_SECRET.to_owned(),
        false,
        None,
    )
    .await
    .expect("connect multi-process API-key activity-budget client");
    let mut stores = Vec::new();
    for ordinal in 0..3 {
        let mut addresses = cluster.addresses.clone();
        addresses.rotate_left(ordinal);
        let store_client = Client::remote(
            addresses,
            true,
            true,
            CONTRACT_API_SECRET.to_owned(),
            false,
            None,
        )
        .await
        .expect("connect independent API-key activity-budget client");
        let telemetry = cluster
            ._root
            .path()
            .join(format!("api-key-activity-budget-process-{ordinal}.db"));
        stores.push(
            HiqliteAuthStore::validation_bootstrap_at(
                store_client,
                CONTRACT_INSTANCE_ID,
                &telemetry,
                1_000,
            )
            .await
            .expect("bootstrap independent API-key activity-budget store"),
        );
    }
    stores[0]
        .validation_reset_contract_state()
        .await
        .expect("reset replicated multi-process API-key activity-budget state");
    let key = stores[0]
        .create_api_key(
            "multi-process-activity-budget",
            "multi-process-api-key-activity-budget-hash",
            &[scopes::SCAN_TRIGGER.to_owned()],
        )
        .await
        .expect("create multi-process activity-budget API key");

    let seeded: Vec<I64Value> = client
        .query_consistent_map(
            "SELECT COUNT(*) AS value FROM api_keys WHERE id = $1 AND last_used_at IS NULL",
            hiqlite::params!(key.id),
        )
        .await
        .expect("confirm the due API-key timestamp through the measurement leader");
    assert_eq!(seeded.len(), 1);
    assert_eq!(seeded[0].value, 1);
    let before = contract_leader_point(&client).await;
    let barrier = Arc::new(tokio::sync::Barrier::new(121));
    let mut requests = tokio::task::JoinSet::new();
    for ordinal in 0..120 {
        // Clones within each group share a gate. The three independently
        // bootstrapped stores model serving processes with separate gates.
        let store = stores[ordinal % stores.len()].clone();
        let barrier = Arc::clone(&barrier);
        requests.spawn(async move {
            barrier.wait().await;
            let key = store
                .api_key_for_hash("multi-process-api-key-activity-budget-hash")
                .await
                .expect("look up multi-process API key")
                .expect("resolve multi-process API key");
            assert!(!key.disabled);
            assert!(key.allows(scopes::SCAN_TRIGGER));
            store
                .touch_api_key(key.id)
                .await
                .expect("touch multi-process API key");
        });
    }
    barrier.wait().await;
    while let Some(result) = requests.join_next().await {
        result.expect("join multi-process API-key request");
    }
    let delta = contract_stable_leader_delta(before, contract_leader_point(&client).await);
    assert!(
        (1..=3).contains(&delta),
        "120 simultaneous requests on three independent Stores appended {delta} API-key activity entries"
    );
    let timestamps: Vec<I64Value> = client
        .query_consistent_map(
            "SELECT last_used_at AS value FROM api_keys WHERE id = $1",
            hiqlite::params!(key.id),
        )
        .await
        .expect("read the durable multi-process API-key activity timestamp");
    assert_eq!(timestamps.len(), 1);
    assert_eq!(
        timestamps[0].value, 1_000,
        "all accepted API-key touches converge on one durable timestamp change"
    );
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_key_activity_refresh_is_bounded_and_disabled_keys_do_not_touch() {
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let client = Client::remote(
        cluster.addresses.clone(),
        true,
        true,
        CONTRACT_API_SECRET.to_owned(),
        false,
        None,
    )
    .await
    .expect("connect API-key activity-budget client");
    let telemetry = cluster
        ._root
        .path()
        .join("api-key-activity-budget-telemetry.db");
    let store = HiqliteAuthStore::validation_bootstrap_at(
        client.clone(),
        CONTRACT_INSTANCE_ID,
        &telemetry,
        1_000,
    )
    .await
    .expect("bootstrap fixed-clock API-key activity-budget store");
    store
        .validation_reset_contract_state()
        .await
        .expect("reset replicated API-key activity-budget state");
    let key = store
        .create_api_key(
            "activity-budget",
            "api-key-activity-budget-hash",
            &[scopes::SCAN_TRIGGER.to_owned()],
        )
        .await
        .expect("create activity-budget API key");

    let before = contract_leader_point(&client).await;
    let barrier = Arc::new(tokio::sync::Barrier::new(121));
    let mut requests = tokio::task::JoinSet::new();
    for _ in 0..120 {
        let store = store.clone();
        let barrier = Arc::clone(&barrier);
        requests.spawn(async move {
            barrier.wait().await;
            let key = store
                .api_key_for_hash("api-key-activity-budget-hash")
                .await
                .expect("look up API key")
                .expect("resolve API key");
            assert!(!key.disabled);
            assert!(key.allows(scopes::SCAN_TRIGGER));
            store.touch_api_key(key.id).await.expect("touch API key");
        });
    }
    barrier.wait().await;
    while let Some(result) = requests.join_next().await {
        result.expect("join API-key request");
    }
    let after_concurrent = contract_leader_point(&client).await;
    assert_eq!(
        contract_stable_leader_delta(before, after_concurrent),
        1,
        "one process may append one API-key touch for 120 simultaneous requests"
    );
    assert_eq!(
        store
            .api_key_for_hash("api-key-activity-budget-hash")
            .await
            .expect("look up touched API key")
            .expect("resolve touched API key")
            .last_used_at,
        Some(1_000)
    );

    assert!(store
        .set_api_key_disabled(key.id, true)
        .await
        .expect("disable API key"));
    let after_disable = contract_leader_point(&client).await;
    for _ in 0..120 {
        let disabled = store
            .api_key_for_hash("api-key-activity-budget-hash")
            .await
            .expect("look up disabled API key")
            .expect("resolve disabled API key");
        assert!(disabled.disabled);
    }
    assert_eq!(
        contract_stable_leader_delta(after_disable, contract_leader_point(&client).await),
        0,
        "disabled-key checks must not append activity entries"
    );

    assert!(store
        .delete_api_key(key.id)
        .await
        .expect("delete disabled API key"));
    let after_delete = contract_leader_point(&client).await;
    for _ in 0..120 {
        assert!(store
            .api_key_for_hash("api-key-activity-budget-hash")
            .await
            .expect("look up deleted API key")
            .is_none());
    }
    assert_eq!(
        contract_stable_leader_delta(after_delete, contract_leader_point(&client).await),
        0,
        "deleted-key checks must not append activity entries"
    );
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replicated_v5_store_migrates_atomically_through_v11_on_daemon_open() {
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let client = Client::remote(
        cluster.addresses.clone(),
        true,
        true,
        CONTRACT_API_SECRET.to_owned(),
        false,
        None,
    )
    .await
    .expect("connect migration client");
    let telemetry = cluster._root.path().join("schema-migration-telemetry.db");
    let current = HiqliteAuthStore::bootstrap(client.clone(), CONTRACT_INSTANCE_ID, &telemetry)
        .await
        .expect("bootstrap current schema");
    current
        .validation_reset_contract_state()
        .await
        .expect("empty migration fixture");
    current
        .put_setting("migration.proof", "survives")
        .await
        .expect("seed unrelated replicated row");
    drop(current);

    let results = client
        .txn([
            (
                "DROP TRIGGER IF EXISTS transcode_cache_location_identity_au",
                hiqlite::params!(),
            ),
            (
                "DROP TRIGGER IF EXISTS transcode_cache_location_identity_ai",
                hiqlite::params!(),
            ),
            (
                "DROP INDEX IF EXISTS transcode_cache_storage_lru",
                hiqlite::params!(),
            ),
            (
                "DROP INDEX IF EXISTS transcode_cache_storage_generation",
                hiqlite::params!(),
            ),
            ("DROP TABLE cache_consumer_pins", hiqlite::params!()),
            ("DROP TABLE cache_storage_members", hiqlite::params!()),
            (
                "ALTER TABLE transcode_cache_locations DROP COLUMN generation_id",
                hiqlite::params!(),
            ),
            (
                "ALTER TABLE transcode_cache_locations DROP COLUMN storage_id",
                hiqlite::params!(),
            ),
            ("DROP TABLE media_playback_pointers", hiqlite::params!()),
            ("DROP TABLE media_sessions", hiqlite::params!()),
            ("DROP TABLE media_session_requests", hiqlite::params!()),
            (
                "DROP TRIGGER IF EXISTS pretranscode_jobs_cancel_source",
                hiqlite::params!(),
            ),
            (
                "DROP INDEX IF EXISTS pretranscode_jobs_active",
                hiqlite::params!(),
            ),
            (
                "DROP INDEX IF EXISTS pretranscode_jobs_staging",
                hiqlite::params!(),
            ),
            (
                "DROP INDEX IF EXISTS pretranscode_jobs_dedupe",
                hiqlite::params!(),
            ),
            (
                "DROP INDEX IF EXISTS pretranscode_jobs_due",
                hiqlite::params!(),
            ),
            ("DROP TABLE IF EXISTS pretranscode_jobs", hiqlite::params!()),
            (
                "ALTER TABLE transcode_cache_locations DROP COLUMN manifest_digest",
                hiqlite::params!(),
            ),
            (
                "ALTER TABLE transcode_cache_locations DROP COLUMN scrub_object_index",
                hiqlite::params!(),
            ),
            ("DROP TABLE IF EXISTS job_leases", hiqlite::params!()),
            (
                "DROP INDEX IF EXISTS idx_items_book_work",
                hiqlite::params!(),
            ),
            ("ALTER TABLE items DROP COLUMN author", hiqlite::params!()),
            (
                "ALTER TABLE items DROP COLUMN book_work_id",
                hiqlite::params!(),
            ),
            (
                "ALTER TABLE items DROP COLUMN book_edition_id",
                hiqlite::params!(),
            ),
            (
                "ALTER TABLE items DROP COLUMN book_metadata_source",
                hiqlite::params!(),
            ),
            (
                "DROP INDEX IF EXISTS idx_reading_updated",
                hiqlite::params!(),
            ),
            ("DROP TABLE IF EXISTS reading_state", hiqlite::params!()),
            (
                "UPDATE cluster_meta SET schema_version = $1 WHERE singleton = 1",
                hiqlite::params!(AUTH_SCHEMA_MIGRATION_SOURCE),
            ),
        ])
        .await
        .expect("construct exact v5 fixture");
    results
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("commit exact v5 fixture");

    let strict_error = match HiqliteAuthStore::open(client.clone(), &telemetry).await {
        Ok(_) => panic!("maintenance open must not own schema migration"),
        Err(error) => error,
    };
    assert!(
        strict_error
            .to_string()
            .contains("schema 5 is incompatible"),
        "{strict_error}"
    );

    let migrated = HiqliteAuthStore::open_or_migrate(client.clone(), &telemetry)
        .await
        .expect("daemon v5 through v11 migration");
    assert_eq!(
        migrated
            .get_setting("migration.proof")
            .await
            .expect("read migration proof")
            .as_deref(),
        Some("survives")
    );

    for (sql, expected) in [
        (
            "SELECT schema_version AS value FROM cluster_meta WHERE singleton = 1",
            AUTH_SCHEMA_VERSION,
        ),
        (
            "SELECT COUNT(*) AS value FROM pragma_table_info('reading_state')",
            9,
        ),
        (
            "SELECT COUNT(*) AS value FROM sqlite_master \
             WHERE type = 'index' AND name = 'idx_reading_updated'",
            1,
        ),
        (
            "SELECT COUNT(*) AS value FROM pragma_table_info('items') \
             WHERE name IN ('author', 'book_work_id', 'book_edition_id', \
                            'book_metadata_source')",
            4,
        ),
        (
            "SELECT COUNT(*) AS value FROM sqlite_master \
             WHERE type = 'index' AND name = 'idx_items_book_work'",
            1,
        ),
        (
            "SELECT COUNT(*) AS value FROM pragma_table_info('job_leases')",
            6,
        ),
    ] {
        let rows: Vec<I64Value> = client
            .query_consistent_map(sql, hiqlite::params!())
            .await
            .expect("inspect migrated schema");
        assert_eq!(rows.len(), 1, "{sql}");
        assert_eq!(rows[0].value, expected, "{sql}");
    }
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replicated_v6_store_migrates_atomically_to_v11_on_daemon_open() {
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let client = Client::remote(
        cluster.addresses.clone(),
        true,
        true,
        CONTRACT_API_SECRET.to_owned(),
        false,
        None,
    )
    .await
    .expect("connect v6 migration client");
    let telemetry = cluster
        ._root
        .path()
        .join("schema-v6-migration-telemetry.db");
    let current = HiqliteAuthStore::bootstrap(client.clone(), CONTRACT_INSTANCE_ID, &telemetry)
        .await
        .expect("bootstrap current schema");
    current
        .put_setting("migration.v6.proof", "survives")
        .await
        .expect("seed unrelated replicated row");
    drop(current);

    let results = client
        .txn([
            (
                "DROP TRIGGER IF EXISTS transcode_cache_location_identity_au",
                hiqlite::params!(),
            ),
            (
                "DROP TRIGGER IF EXISTS transcode_cache_location_identity_ai",
                hiqlite::params!(),
            ),
            (
                "DROP INDEX IF EXISTS transcode_cache_storage_lru",
                hiqlite::params!(),
            ),
            (
                "DROP INDEX IF EXISTS transcode_cache_storage_generation",
                hiqlite::params!(),
            ),
            ("DROP TABLE cache_consumer_pins", hiqlite::params!()),
            ("DROP TABLE cache_storage_members", hiqlite::params!()),
            (
                "ALTER TABLE transcode_cache_locations DROP COLUMN generation_id",
                hiqlite::params!(),
            ),
            (
                "ALTER TABLE transcode_cache_locations DROP COLUMN storage_id",
                hiqlite::params!(),
            ),
            ("DROP TABLE media_playback_pointers", hiqlite::params!()),
            ("DROP TABLE media_sessions", hiqlite::params!()),
            ("DROP TABLE media_session_requests", hiqlite::params!()),
            (
                "DROP TRIGGER IF EXISTS pretranscode_jobs_cancel_source",
                hiqlite::params!(),
            ),
            (
                "DROP INDEX IF EXISTS pretranscode_jobs_active",
                hiqlite::params!(),
            ),
            (
                "DROP INDEX IF EXISTS pretranscode_jobs_staging",
                hiqlite::params!(),
            ),
            (
                "DROP INDEX IF EXISTS pretranscode_jobs_dedupe",
                hiqlite::params!(),
            ),
            (
                "DROP INDEX IF EXISTS pretranscode_jobs_due",
                hiqlite::params!(),
            ),
            ("DROP TABLE IF EXISTS pretranscode_jobs", hiqlite::params!()),
            (
                "ALTER TABLE transcode_cache_locations DROP COLUMN manifest_digest",
                hiqlite::params!(),
            ),
            (
                "ALTER TABLE transcode_cache_locations DROP COLUMN scrub_object_index",
                hiqlite::params!(),
            ),
            ("DROP TABLE IF EXISTS job_leases", hiqlite::params!()),
            (
                "DROP INDEX IF EXISTS idx_items_book_work",
                hiqlite::params!(),
            ),
            ("ALTER TABLE items DROP COLUMN author", hiqlite::params!()),
            (
                "ALTER TABLE items DROP COLUMN book_work_id",
                hiqlite::params!(),
            ),
            (
                "ALTER TABLE items DROP COLUMN book_edition_id",
                hiqlite::params!(),
            ),
            (
                "ALTER TABLE items DROP COLUMN book_metadata_source",
                hiqlite::params!(),
            ),
            (
                "UPDATE cluster_meta SET schema_version = 6 WHERE singleton = 1",
                hiqlite::params!(),
            ),
        ])
        .await
        .expect("construct exact v6 fixture");
    results
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("commit exact v6 fixture");

    let strict_error = match HiqliteAuthStore::open(client.clone(), &telemetry).await {
        Ok(_) => panic!("maintenance open must not own schema migration"),
        Err(error) => error,
    };
    assert!(
        strict_error
            .to_string()
            .contains("schema 6 is incompatible"),
        "{strict_error}"
    );

    let migrated = HiqliteAuthStore::open_or_migrate(client.clone(), &telemetry)
        .await
        .expect("daemon v6 to v11 migration");
    assert_eq!(
        migrated
            .get_setting("migration.v6.proof")
            .await
            .expect("read migration proof")
            .as_deref(),
        Some("survives")
    );

    for (sql, expected) in [
        (
            "SELECT schema_version AS value FROM cluster_meta WHERE singleton = 1",
            AUTH_SCHEMA_VERSION,
        ),
        (
            "SELECT COUNT(*) AS value FROM pragma_table_info('items') \
             WHERE name IN ('author', 'book_work_id', 'book_edition_id', \
                            'book_metadata_source')",
            4,
        ),
        (
            "SELECT COUNT(*) AS value FROM sqlite_master \
             WHERE type = 'index' AND name = 'idx_items_book_work'",
            1,
        ),
        (
            "SELECT COUNT(*) AS value FROM pragma_table_info('job_leases')",
            6,
        ),
    ] {
        let rows: Vec<I64Value> = client
            .query_consistent_map(sql, hiqlite::params!())
            .await
            .expect("inspect migrated schema");
        assert_eq!(rows.len(), 1, "{sql}");
        assert_eq!(rows[0].value, expected, "{sql}");
    }
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replicated_v7_store_migrates_atomically_to_v11_on_daemon_open() {
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let client = Client::remote(
        cluster.addresses.clone(),
        true,
        true,
        CONTRACT_API_SECRET.to_owned(),
        false,
        None,
    )
    .await
    .expect("connect v7 migration client");
    let telemetry = cluster
        ._root
        .path()
        .join("schema-v7-migration-telemetry.db");
    let current = HiqliteAuthStore::bootstrap(client.clone(), CONTRACT_INSTANCE_ID, &telemetry)
        .await
        .expect("bootstrap current schema");
    current
        .validation_reset_contract_state()
        .await
        .expect("empty v7 migration fixture");
    current
        .put_setting("migration.v7.proof", "survives")
        .await
        .expect("seed unrelated replicated row");
    drop(current);

    client
        .txn([
            (
                "DROP TRIGGER IF EXISTS transcode_cache_location_identity_au",
                hiqlite::params!(),
            ),
            (
                "DROP TRIGGER IF EXISTS transcode_cache_location_identity_ai",
                hiqlite::params!(),
            ),
            (
                "DROP INDEX IF EXISTS transcode_cache_storage_lru",
                hiqlite::params!(),
            ),
            (
                "DROP INDEX IF EXISTS transcode_cache_storage_generation",
                hiqlite::params!(),
            ),
            ("DROP TABLE cache_consumer_pins", hiqlite::params!()),
            ("DROP TABLE cache_storage_members", hiqlite::params!()),
            (
                "ALTER TABLE transcode_cache_locations DROP COLUMN generation_id",
                hiqlite::params!(),
            ),
            (
                "ALTER TABLE transcode_cache_locations DROP COLUMN storage_id",
                hiqlite::params!(),
            ),
            ("DROP TABLE media_playback_pointers", hiqlite::params!()),
            ("DROP TABLE media_sessions", hiqlite::params!()),
            ("DROP TABLE media_session_requests", hiqlite::params!()),
            (
                "DROP TRIGGER IF EXISTS pretranscode_jobs_cancel_source",
                hiqlite::params!(),
            ),
            (
                "DROP INDEX IF EXISTS pretranscode_jobs_active",
                hiqlite::params!(),
            ),
            (
                "DROP INDEX IF EXISTS pretranscode_jobs_staging",
                hiqlite::params!(),
            ),
            (
                "DROP INDEX IF EXISTS pretranscode_jobs_dedupe",
                hiqlite::params!(),
            ),
            (
                "DROP INDEX IF EXISTS pretranscode_jobs_due",
                hiqlite::params!(),
            ),
            ("DROP TABLE IF EXISTS pretranscode_jobs", hiqlite::params!()),
            (
                "ALTER TABLE transcode_cache_locations DROP COLUMN manifest_digest",
                hiqlite::params!(),
            ),
            (
                "ALTER TABLE transcode_cache_locations DROP COLUMN scrub_object_index",
                hiqlite::params!(),
            ),
            ("DROP TABLE job_leases", hiqlite::params!()),
            (
                "UPDATE cluster_meta SET schema_version = 7 WHERE singleton = 1",
                hiqlite::params!(),
            ),
        ])
        .await
        .expect("construct exact v7 fixture")
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("commit exact v7 fixture");

    let strict_error = match HiqliteAuthStore::open(client.clone(), &telemetry).await {
        Ok(_) => panic!("maintenance open must not own schema migration"),
        Err(error) => error,
    };
    assert!(
        strict_error
            .to_string()
            .contains("schema 7 is incompatible"),
        "{strict_error}"
    );

    let migrated = HiqliteAuthStore::open_or_migrate(client.clone(), &telemetry)
        .await
        .expect("daemon v7 to v11 migration");
    assert_eq!(
        migrated
            .get_setting("migration.v7.proof")
            .await
            .expect("read v7 migration proof")
            .as_deref(),
        Some("survives")
    );
    for (sql, expected) in [
        (
            "SELECT schema_version AS value FROM cluster_meta WHERE singleton = 1",
            AUTH_SCHEMA_VERSION,
        ),
        (
            "SELECT COUNT(*) AS value FROM pragma_table_info('job_leases')",
            6,
        ),
    ] {
        let rows: Vec<I64Value> = client
            .query_consistent_map(sql, hiqlite::params!())
            .await
            .expect("inspect migrated v7 schema");
        assert_eq!(rows.len(), 1, "{sql}");
        assert_eq!(rows[0].value, expected, "{sql}");
    }
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replicated_v8_store_migrates_exactly_to_v11_on_daemon_open() {
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let client = Client::remote(
        cluster.addresses.clone(),
        true,
        true,
        CONTRACT_API_SECRET.to_owned(),
        false,
        None,
    )
    .await
    .expect("connect v8 migration client");
    let telemetry = cluster
        ._root
        .path()
        .join("schema-v8-migration-telemetry.db");
    let current = HiqliteAuthStore::bootstrap(client.clone(), CONTRACT_INSTANCE_ID, &telemetry)
        .await
        .expect("bootstrap current schema");
    current
        .validation_reset_contract_state()
        .await
        .expect("empty v8 migration fixture");
    current
        .put_setting("migration.v8.proof", "survives")
        .await
        .expect("seed unrelated replicated row");
    drop(current);

    client
        .txn([
            (
                "DROP TRIGGER IF EXISTS transcode_cache_location_identity_au",
                hiqlite::params!(),
            ),
            (
                "DROP TRIGGER IF EXISTS transcode_cache_location_identity_ai",
                hiqlite::params!(),
            ),
            (
                "DROP INDEX IF EXISTS transcode_cache_storage_lru",
                hiqlite::params!(),
            ),
            (
                "DROP INDEX IF EXISTS transcode_cache_storage_generation",
                hiqlite::params!(),
            ),
            ("DROP TABLE cache_consumer_pins", hiqlite::params!()),
            ("DROP TABLE cache_storage_members", hiqlite::params!()),
            (
                "ALTER TABLE transcode_cache_locations DROP COLUMN generation_id",
                hiqlite::params!(),
            ),
            (
                "ALTER TABLE transcode_cache_locations DROP COLUMN storage_id",
                hiqlite::params!(),
            ),
            ("DROP TABLE media_playback_pointers", hiqlite::params!()),
            ("DROP TABLE media_sessions", hiqlite::params!()),
            ("DROP TABLE media_session_requests", hiqlite::params!()),
            (
                "DROP TRIGGER IF EXISTS pretranscode_jobs_cancel_source",
                hiqlite::params!(),
            ),
            (
                "DROP INDEX IF EXISTS pretranscode_jobs_active",
                hiqlite::params!(),
            ),
            (
                "DROP INDEX IF EXISTS pretranscode_jobs_staging",
                hiqlite::params!(),
            ),
            (
                "DROP INDEX IF EXISTS pretranscode_jobs_dedupe",
                hiqlite::params!(),
            ),
            (
                "DROP INDEX IF EXISTS pretranscode_jobs_due",
                hiqlite::params!(),
            ),
            ("DROP TABLE IF EXISTS pretranscode_jobs", hiqlite::params!()),
            (
                "ALTER TABLE transcode_cache_locations DROP COLUMN manifest_digest",
                hiqlite::params!(),
            ),
            (
                "ALTER TABLE transcode_cache_locations DROP COLUMN scrub_object_index",
                hiqlite::params!(),
            ),
            (
                "UPDATE cluster_meta SET schema_version = 8 WHERE singleton = 1",
                hiqlite::params!(),
            ),
        ])
        .await
        .expect("construct exact v8 fixture")
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("commit exact v8 fixture");

    let strict_error = match HiqliteAuthStore::open(client.clone(), &telemetry).await {
        Ok(_) => panic!("maintenance open must not own schema migration"),
        Err(error) => error,
    };
    assert!(
        strict_error
            .to_string()
            .contains("schema 8 is incompatible"),
        "{strict_error}"
    );

    let migrated = HiqliteAuthStore::open_or_migrate(client.clone(), &telemetry)
        .await
        .expect("daemon v8 to v11 migration");
    assert_eq!(
        migrated
            .get_setting("migration.v8.proof")
            .await
            .expect("read v8 migration proof")
            .as_deref(),
        Some("survives")
    );
    for (sql, expected) in [
        (
            "SELECT schema_version AS value FROM cluster_meta WHERE singleton = 1",
            AUTH_SCHEMA_VERSION,
        ),
        (
            "SELECT COUNT(*) AS value FROM pragma_table_info('pretranscode_jobs')",
            24,
        ),
        (
            "SELECT COUNT(*) AS value FROM pragma_table_info('transcode_cache_locations') \
             WHERE name = 'manifest_digest'",
            1,
        ),
        (
            "SELECT COUNT(*) AS value FROM pragma_table_info('transcode_cache_locations') \
             WHERE name = 'scrub_object_index'",
            1,
        ),
        (
            "SELECT COUNT(*) AS value FROM sqlite_master WHERE type = 'index' \
             AND name IN ('pretranscode_jobs_due', 'pretranscode_jobs_dedupe', \
                          'pretranscode_jobs_staging', 'pretranscode_jobs_active')",
            4,
        ),
        (
            "SELECT COUNT(*) AS value FROM sqlite_master WHERE type = 'trigger' \
             AND name = 'pretranscode_jobs_cancel_source'",
            1,
        ),
        (
            "SELECT COUNT(*) AS value FROM sqlite_master WHERE type = 'table' \
             AND name IN ('media_session_requests', 'media_playback_pointers', 'media_sessions')",
            3,
        ),
        (
            "SELECT COUNT(*) AS value FROM sqlite_master WHERE type = 'index' \
             AND name IN ('media_session_requests_expiry', 'media_sessions_owner', \
                          'media_sessions_user', 'media_sessions_expiry', \
                          'media_sessions_retention')",
            5,
        ),
    ] {
        let rows: Vec<I64Value> = client
            .query_consistent_map(sql, hiqlite::params!())
            .await
            .expect("inspect migrated v11 schema");
        assert_eq!(rows.len(), 1, "{sql}");
        assert_eq!(rows[0].value, expected, "{sql}");
    }
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replicated_v9_store_migrates_exactly_to_v11_on_daemon_open() {
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let client = Client::remote(
        cluster.addresses.clone(),
        true,
        true,
        CONTRACT_API_SECRET.to_owned(),
        true,
        None,
    )
    .await
    .expect("connect v9 migration client");
    let telemetry = cluster
        ._root
        .path()
        .join("schema-v9-migration-telemetry.db");
    let current = HiqliteAuthStore::bootstrap(client.clone(), CONTRACT_INSTANCE_ID, &telemetry)
        .await
        .expect("bootstrap current schema");
    current
        .validation_reset_contract_state()
        .await
        .expect("empty v9 migration fixture");
    current
        .put_setting("migration.v9.proof", "survives")
        .await
        .expect("seed unrelated replicated row");
    drop(current);

    client
        .txn([
            (
                "DROP TRIGGER IF EXISTS transcode_cache_location_identity_au",
                hiqlite::params!(),
            ),
            (
                "DROP TRIGGER IF EXISTS transcode_cache_location_identity_ai",
                hiqlite::params!(),
            ),
            (
                "DROP INDEX IF EXISTS transcode_cache_storage_lru",
                hiqlite::params!(),
            ),
            (
                "DROP INDEX IF EXISTS transcode_cache_storage_generation",
                hiqlite::params!(),
            ),
            ("DROP TABLE cache_consumer_pins", hiqlite::params!()),
            ("DROP TABLE cache_storage_members", hiqlite::params!()),
            (
                "ALTER TABLE transcode_cache_locations DROP COLUMN generation_id",
                hiqlite::params!(),
            ),
            (
                "ALTER TABLE transcode_cache_locations DROP COLUMN storage_id",
                hiqlite::params!(),
            ),
            ("DROP TABLE media_playback_pointers", hiqlite::params!()),
            ("DROP TABLE media_sessions", hiqlite::params!()),
            ("DROP TABLE media_session_requests", hiqlite::params!()),
            (
                "UPDATE cluster_meta SET schema_version = 9 WHERE singleton = 1",
                hiqlite::params!(),
            ),
        ])
        .await
        .expect("construct exact v9 fixture")
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("commit exact v9 fixture");

    let strict_error = match HiqliteAuthStore::open(client.clone(), &telemetry).await {
        Ok(_) => panic!("maintenance open must not own schema migration"),
        Err(error) => error,
    };
    assert!(
        strict_error
            .to_string()
            .contains("schema 9 is incompatible"),
        "{strict_error}"
    );

    let migrated = HiqliteAuthStore::open_or_migrate(client.clone(), &telemetry)
        .await
        .expect("daemon v9 to v11 migration");
    assert_eq!(
        migrated
            .get_setting("migration.v9.proof")
            .await
            .expect("read v9 migration proof")
            .as_deref(),
        Some("survives")
    );
    for (sql, expected) in [
        (
            "SELECT schema_version AS value FROM cluster_meta WHERE singleton = 1",
            AUTH_SCHEMA_VERSION,
        ),
        (
            "SELECT COUNT(*) AS value FROM sqlite_master WHERE type = 'table' \
             AND name IN ('media_session_requests', 'media_playback_pointers', 'media_sessions')",
            3,
        ),
        (
            "SELECT COUNT(*) AS value FROM sqlite_master WHERE type = 'index' \
             AND name IN ('media_session_requests_expiry', 'media_sessions_owner', \
                          'media_sessions_user', 'media_sessions_expiry', \
                          'media_sessions_retention')",
            5,
        ),
    ] {
        let rows: Vec<I64Value> = client
            .query_consistent_map(sql, hiqlite::params!())
            .await
            .expect("inspect migrated v11 session schema");
        assert_eq!(rows.len(), 1, "{sql}");
        assert_eq!(rows[0].value, expected, "{sql}");
    }
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replicated_v10_store_migrates_exactly_to_v11_on_daemon_open() {
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let client = Client::remote(
        cluster.addresses.clone(),
        true,
        true,
        CONTRACT_API_SECRET.to_owned(),
        false,
        None,
    )
    .await
    .expect("connect v10 migration client");
    let telemetry = cluster
        ._root
        .path()
        .join("schema-v10-migration-telemetry.db");
    let current = HiqliteAuthStore::bootstrap(client.clone(), CONTRACT_INSTANCE_ID, &telemetry)
        .await
        .expect("bootstrap current schema");
    current
        .validation_reset_contract_state()
        .await
        .expect("empty v10 migration fixture");
    current
        .put_setting("migration.v10.proof", "survives")
        .await
        .expect("seed unrelated replicated row");
    drop(current);

    client
        .txn([
            (
                "DROP TRIGGER transcode_cache_location_identity_au",
                hiqlite::params!(),
            ),
            (
                "DROP TRIGGER transcode_cache_location_identity_ai",
                hiqlite::params!(),
            ),
            ("DROP INDEX transcode_cache_storage_lru", hiqlite::params!()),
            (
                "DROP INDEX transcode_cache_storage_generation",
                hiqlite::params!(),
            ),
            ("DROP TABLE cache_consumer_pins", hiqlite::params!()),
            ("DROP TABLE cache_storage_members", hiqlite::params!()),
            (
                "ALTER TABLE transcode_cache_locations DROP COLUMN generation_id",
                hiqlite::params!(),
            ),
            (
                "ALTER TABLE transcode_cache_locations DROP COLUMN storage_id",
                hiqlite::params!(),
            ),
            (
                "UPDATE cluster_meta SET schema_version = 10 WHERE singleton = 1",
                hiqlite::params!(),
            ),
        ])
        .await
        .expect("construct exact v10 fixture")
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("commit exact v10 fixture");

    let strict_error = match HiqliteAuthStore::open(client.clone(), &telemetry).await {
        Ok(_) => panic!("maintenance open must not own schema migration"),
        Err(error) => error,
    };
    assert!(
        strict_error
            .to_string()
            .contains("schema 10 is incompatible"),
        "{strict_error}"
    );
    let migrated = HiqliteAuthStore::open_or_migrate(client.clone(), &telemetry)
        .await
        .expect("daemon v10 to v11 migration");
    assert_eq!(
        migrated
            .get_setting("migration.v10.proof")
            .await
            .expect("read v10 migration proof")
            .as_deref(),
        Some("survives")
    );
    for (sql, expected) in [
        (
            "SELECT schema_version AS value FROM cluster_meta WHERE singleton = 1",
            AUTH_SCHEMA_VERSION,
        ),
        (
            "SELECT COUNT(*) AS value FROM pragma_table_info('transcode_cache_locations') \
             WHERE name IN ('storage_id', 'generation_id')",
            2,
        ),
        (
            "SELECT COUNT(*) AS value FROM sqlite_master WHERE type = 'table' \
             AND name IN ('cache_storage_members', 'cache_consumer_pins')",
            2,
        ),
        (
            "SELECT COUNT(*) AS value FROM sqlite_master WHERE type = 'index' \
             AND name IN ('transcode_cache_storage_generation', \
                          'transcode_cache_storage_lru', 'cache_storage_members_node', \
                          'cache_consumer_pins_expiry')",
            4,
        ),
        (
            "SELECT COUNT(*) AS value FROM sqlite_master WHERE type = 'trigger' \
             AND name IN ('transcode_cache_location_identity_ai', \
                          'transcode_cache_location_identity_au')",
            2,
        ),
    ] {
        let rows: Vec<I64Value> = client
            .query_consistent_map(sql, hiqlite::params!())
            .await
            .expect("inspect migrated v11 shared-cache schema");
        assert_eq!(rows.len(), 1, "{sql}");
        assert_eq!(rows[0].value, expected, "{sql}");
    }
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replicated_v11_and_v12_migrations_are_atomic_restartable_and_stepwise() {
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let client = Client::remote(
        cluster.addresses.clone(),
        true,
        true,
        CONTRACT_API_SECRET.to_owned(),
        false,
        None,
    )
    .await
    .expect("connect v12 migration client");
    let telemetry = cluster
        ._root
        .path()
        .join("schema-v12-migration-telemetry.db");
    let current = HiqliteAuthStore::bootstrap(client.clone(), CONTRACT_INSTANCE_ID, &telemetry)
        .await
        .expect("bootstrap current schema");
    current
        .validation_reset_contract_state()
        .await
        .expect("empty v12 migration fixture");
    current
        .put_setting("migration.v12.proof", "survives")
        .await
        .expect("seed unrelated replicated row");
    drop(current);

    client
        .txn([
            (
                "DROP TRIGGER analysis_requests_bound_terminal_history",
                hiqlite::params!(),
            ),
            (
                "DROP TRIGGER analysis_requests_supersede_source",
                hiqlite::params!(),
            ),
            (
                "DROP TRIGGER analysis_requests_cancel_source",
                hiqlite::params!(),
            ),
            (
                "DROP INDEX analysis_requests_one_active_source",
                hiqlite::params!(),
            ),
            ("DROP INDEX analysis_requests_status", hiqlite::params!()),
            ("DROP INDEX analysis_requests_due", hiqlite::params!()),
            ("DROP TABLE analysis_requests", hiqlite::params!()),
            (
                "UPDATE cluster_meta SET schema_version = 12 WHERE singleton = 1",
                hiqlite::params!(),
            ),
        ])
        .await
        .expect("construct exact v12 fixture")
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("commit exact v12 fixture");

    let strict_error = match HiqliteAuthStore::open(client.clone(), &telemetry).await {
        Ok(_) => panic!("maintenance open must not own schema migration"),
        Err(error) => error,
    };
    assert!(
        strict_error
            .to_string()
            .contains("schema 12 is incompatible"),
        "{strict_error}"
    );
    let migrated = HiqliteAuthStore::open_or_migrate(client.clone(), &telemetry)
        .await
        .expect("daemon v12 to v13 migration");
    assert_eq!(
        migrated
            .get_setting("migration.v12.proof")
            .await
            .expect("read v12 migration proof")
            .as_deref(),
        Some("survives")
    );
    for (sql, expected) in [
        (
            "SELECT schema_version AS value FROM cluster_meta WHERE singleton = 1",
            AUTH_SCHEMA_VERSION,
        ),
        (
            "SELECT COUNT(*) AS value FROM sqlite_master WHERE type = 'table' \
             AND name = 'analysis_requests'",
            1,
        ),
        (
            "SELECT COUNT(*) AS value FROM sqlite_master WHERE type = 'index' \
             AND name IN ('analysis_requests_due', 'analysis_requests_status', \
                          'analysis_requests_one_active_source')",
            3,
        ),
        (
            "SELECT COUNT(*) AS value FROM sqlite_master WHERE type = 'trigger' \
             AND name IN ('analysis_requests_cancel_source', \
                          'analysis_requests_supersede_source', \
                          'analysis_requests_bound_terminal_history')",
            3,
        ),
    ] {
        let rows: Vec<I64Value> = client
            .query_consistent_map(sql, hiqlite::params!())
            .await
            .expect("inspect migrated v13 analysis schema");
        assert_eq!(rows.len(), 1, "{sql}");
        assert_eq!(rows[0].value, expected, "{sql}");
    }
    drop(migrated);

    client
        .txn([
            (
                "DROP TRIGGER analysis_requests_bound_terminal_history",
                hiqlite::params!(),
            ),
            (
                "DROP TRIGGER analysis_requests_supersede_source",
                hiqlite::params!(),
            ),
            (
                "DROP TRIGGER analysis_requests_cancel_source",
                hiqlite::params!(),
            ),
            (
                "DROP INDEX analysis_requests_one_active_source",
                hiqlite::params!(),
            ),
            ("DROP INDEX analysis_requests_status", hiqlite::params!()),
            ("DROP INDEX analysis_requests_due", hiqlite::params!()),
            ("DROP TABLE analysis_requests", hiqlite::params!()),
            (
                "DROP TRIGGER cluster_fragment_indexes_cancel_source",
                hiqlite::params!(),
            ),
            (
                "DROP INDEX cluster_fragment_index_locations_node",
                hiqlite::params!(),
            ),
            (
                "DROP INDEX cluster_fragment_index_artifacts_file",
                hiqlite::params!(),
            ),
            (
                "DROP INDEX cluster_fragment_index_jobs_due",
                hiqlite::params!(),
            ),
            (
                "DROP TABLE cluster_fragment_index_locations",
                hiqlite::params!(),
            ),
            (
                "DROP TABLE cluster_fragment_index_artifacts",
                hiqlite::params!(),
            ),
            ("DROP TABLE cluster_fragment_index_jobs", hiqlite::params!()),
            (
                "DROP TABLE cluster_fragment_index_sources",
                hiqlite::params!(),
            ),
            (
                "UPDATE cluster_meta SET schema_version = 11 WHERE singleton = 1",
                hiqlite::params!(),
            ),
        ])
        .await
        .expect("construct exact v11 fixture")
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("commit exact v11 fixture");

    // A malformed pre-existing object models an interrupted/non-transactional
    // v11 deployment. The v12 transaction must fail closed without leaving
    // any of its other schema objects or advancing the marker.
    client
        .execute(
            "CREATE TABLE cluster_fragment_index_jobs (
                cache_key TEXT PRIMARY KEY
             ) STRICT",
            hiqlite::params!(),
        )
        .await
        .expect("seed conflicting partial v12 object");
    let interrupted_v12 = match HiqliteAuthStore::open_or_migrate(client.clone(), &telemetry).await
    {
        Ok(_) => panic!("malformed v12 predecessor must not open"),
        Err(error) => error,
    };
    assert!(!interrupted_v12.to_string().is_empty());
    for (sql, expected) in [
        (
            "SELECT schema_version AS value FROM cluster_meta WHERE singleton = 1",
            11,
        ),
        (
            "SELECT COUNT(*) AS value FROM sqlite_master WHERE type = 'table' \
             AND name = 'cluster_fragment_index_sources'",
            0,
        ),
        (
            "SELECT COUNT(*) AS value FROM sqlite_master WHERE type = 'index' \
             AND name IN ('cluster_fragment_index_jobs_due', \
                          'cluster_fragment_index_artifacts_file', \
                          'cluster_fragment_index_locations_node')",
            0,
        ),
    ] {
        let rows: Vec<I64Value> = client
            .query_consistent_map(sql, hiqlite::params!())
            .await
            .expect("inspect rolled-back v12 migration");
        assert_eq!(rows.len(), 1, "{sql}");
        assert_eq!(rows[0].value, expected, "{sql}");
    }
    client
        .execute("DROP TABLE cluster_fragment_index_jobs", hiqlite::params!())
        .await
        .expect("remove conflicting partial v12 object");

    // The broken release could also crash after committing a correct object
    // but before publishing schema version 12. The replacement migration is
    // idempotent for that exact historical shape and completes the remainder
    // of the generation in its transaction.
    client
        .execute(
            "CREATE TABLE cluster_fragment_index_sources (
                node_id          TEXT NOT NULL,
                file_id          INTEGER NOT NULL,
                object_version   TEXT NOT NULL,
                source_size      INTEGER NOT NULL,
                source_mtime     INTEGER NOT NULL,
                source_sha256    TEXT NOT NULL,
                observed_at_ms   INTEGER NOT NULL,
                PRIMARY KEY (node_id, file_id)
             ) STRICT",
            hiqlite::params!(),
        )
        .await
        .expect("seed correctly shaped partial v12 object");

    // Let v12 commit, then force v13 to fail. The durable marker must stop at
    // 12 rather than jumping from 11 to the newest schema, and no v13 index or
    // trigger may escape its failed transaction.
    client
        .execute(
            "CREATE TABLE analysis_requests (request_id TEXT PRIMARY KEY) STRICT",
            hiqlite::params!(),
        )
        .await
        .expect("seed conflicting partial v13 object");
    let interrupted_v13 = match HiqliteAuthStore::open_or_migrate(client.clone(), &telemetry).await
    {
        Ok(_) => panic!("malformed v13 predecessor must not open"),
        Err(error) => error,
    };
    assert!(!interrupted_v13.to_string().is_empty());
    for (sql, expected) in [
        (
            "SELECT schema_version AS value FROM cluster_meta WHERE singleton = 1",
            12,
        ),
        (
            "SELECT COUNT(*) AS value FROM sqlite_master WHERE type = 'table' \
             AND name IN ('cluster_fragment_index_sources', \
                          'cluster_fragment_index_jobs', \
                          'cluster_fragment_index_artifacts', \
                          'cluster_fragment_index_locations')",
            4,
        ),
        (
            "SELECT COUNT(*) AS value FROM sqlite_master WHERE type = 'index' \
             AND name IN ('cluster_fragment_index_jobs_due', \
                          'cluster_fragment_index_artifacts_file', \
                          'cluster_fragment_index_locations_node')",
            3,
        ),
        (
            "SELECT COUNT(*) AS value FROM sqlite_master WHERE type IN ('index', 'trigger') \
             AND (name LIKE 'analysis_requests_%')",
            0,
        ),
    ] {
        let rows: Vec<I64Value> = client
            .query_consistent_map(sql, hiqlite::params!())
            .await
            .expect("inspect committed v12 and rolled-back v13 migration");
        assert_eq!(rows.len(), 1, "{sql}");
        assert_eq!(rows[0].value, expected, "{sql}");
    }
    client
        .execute("DROP TABLE analysis_requests", hiqlite::params!())
        .await
        .expect("remove conflicting partial v13 object");

    let migrated = HiqliteAuthStore::open_or_migrate(client.clone(), &telemetry)
        .await
        .expect("daemon resumes v12 to v13 migration");
    assert_eq!(
        migrated
            .get_setting("migration.v12.proof")
            .await
            .expect("read v11 migration proof")
            .as_deref(),
        Some("survives")
    );
    for (sql, expected) in [
        (
            "SELECT schema_version AS value FROM cluster_meta WHERE singleton = 1",
            AUTH_SCHEMA_VERSION,
        ),
        (
            "SELECT COUNT(*) AS value FROM sqlite_master WHERE type = 'table' \
             AND name IN ('cluster_fragment_index_sources', \
                          'cluster_fragment_index_jobs', \
                          'cluster_fragment_index_artifacts', \
                          'cluster_fragment_index_locations', 'analysis_requests')",
            5,
        ),
        (
            "SELECT COUNT(*) AS value FROM sqlite_master WHERE type = 'trigger' \
             AND name IN ('cluster_fragment_indexes_cancel_source', \
                          'analysis_requests_cancel_source', \
                          'analysis_requests_supersede_source', \
                          'analysis_requests_bound_terminal_history')",
            4,
        ),
    ] {
        let rows: Vec<I64Value> = client
            .query_consistent_map(sql, hiqlite::params!())
            .await
            .expect("inspect migrated v11-through-v13 schema");
        assert_eq!(rows.len(), 1, "{sql}");
        assert_eq!(rows[0].value, expected, "{sql}");
    }
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replicated_analysis_handoff_is_atomic_and_fenced() {
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let client = Client::remote(
        cluster.addresses.clone(),
        true,
        true,
        CONTRACT_API_SECRET.to_owned(),
        false,
        None,
    )
    .await
    .expect("connect analysis handoff client");
    let telemetry = cluster._root.path().join("analysis-handoff-telemetry.db");
    let store = HiqliteAuthStore::bootstrap(client.clone(), CONTRACT_INSTANCE_ID, &telemetry)
        .await
        .expect("bootstrap analysis handoff store");
    store
        .validation_reset_contract_state()
        .await
        .expect("empty analysis handoff fixture");
    client
        .txn([
            (
                "INSERT INTO libraries (id, name, kind, paths, created_at) \
                 VALUES (1, 'Films', 'movies', '[]', 1)",
                hiqlite::params!(),
            ),
            (
                "INSERT INTO items
                   (id, library_id, kind, title, sort_title, added_at, updated_at) \
                 VALUES (1, 1, 'movie', 'One', 'one', 1, 1)",
                hiqlite::params!(),
            ),
            (
                "INSERT INTO files (id, item_id, path, size, mtime, scanned_at) \
                 VALUES (1, 1, '/one.mkv', 100, 10, 1)",
                hiqlite::params!(),
            ),
        ])
        .await
        .expect("seed analysis source")
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("commit analysis source");

    client
        .execute(
            "INSERT INTO analysis_requests
              (request_id, file_id, source_size, source_mtime, component,
               force_rebuild, target_node_id, state, fence, attempts,
               not_before_ms, created_at_ms, updated_at_ms)
             VALUES ($1, 1, 100, 10, 'fragment_index', 0, 'node-a',
                     'ready', 1, 1, 1, 1, 1)",
            hiqlite::params!("ambiguous-request"),
        )
        .await
        .expect("seed a terminal request from an ambiguous committed attempt");
    let ambiguous = store
        .enqueue_analysis_request(&NewAnalysisRequest {
            request_id: "ambiguous-request".to_owned(),
            file_id: 1,
            source_size: 100,
            source_mtime: 10,
            component: "fragment_index".to_owned(),
            force_rebuild: false,
            target_node_id: "node-a".to_owned(),
            not_before_ms: 2,
            created_at_ms: 2,
        })
        .await
        .expect("recover the caller row after a committed insert retry");
    assert_eq!(ambiguous.request_id, "ambiguous-request");
    assert_eq!(ambiguous.state, "ready");

    store
        .enqueue_analysis_request(&NewAnalysisRequest {
            request_id: "analysis-request".to_owned(),
            file_id: 1,
            source_size: 100,
            source_mtime: 10,
            component: "fragment_index".to_owned(),
            force_rebuild: false,
            target_node_id: "node-a".to_owned(),
            not_before_ms: 10,
            created_at_ms: 10,
        })
        .await
        .expect("enqueue analysis request");
    let stale = store
        .claim_analysis_request("node-a", 10, 20)
        .await
        .expect("claim stale owner")
        .expect("stale owner");
    let current = store
        .claim_analysis_request("node-a", 20, 1_020)
        .await
        .expect("reclaim request")
        .expect("current owner");
    let source_sha256 = "a".repeat(64);
    let pipeline_sha256 = "b".repeat(64);
    let job = NewClusterFragmentIndexJob {
        cache_key: cluster_fragment_index_key(&source_sha256, &pipeline_sha256)
            .expect("content key"),
        file_id: 1,
        source_size: 100,
        source_mtime: 10,
        source_sha256,
        pipeline_sha256,
        not_before_ms: 21,
        created_at_ms: 21,
    };
    assert!(!store
        .submit_fragment_index_analysis(&stale, &job, 21)
        .await
        .expect("stale handoff"));
    assert!(store
        .cluster_fragment_index_job(&job.cache_key)
        .await
        .expect("read stale job")
        .is_none());
    assert!(store
        .submit_fragment_index_analysis(&current, &job, 21)
        .await
        .expect("current handoff"));
    let submitted = store.analysis_requests(10).await.expect("list requests");
    assert_eq!(submitted[0].state, "submitted");
    assert_eq!(submitted[0].result_cache_key, job.cache_key);
    assert_eq!(
        store
            .cluster_fragment_index_job(&submitted[0].result_cache_key)
            .await
            .expect("read committed job")
            .expect("worker job")
            .state,
        "queued"
    );

    client
        .txn([
            (
                "UPDATE cluster_fragment_index_jobs
                    SET state = 'ready', owner_node_id = NULL, lease_expires_ms = NULL,
                        updated_at_ms = 22 WHERE cache_key = $1",
                hiqlite::params!(&job.cache_key),
            ),
            (
                "INSERT INTO cluster_fragment_index_artifacts
                  (cache_key, file_id, source_size, source_mtime, source_sha256,
                   pipeline_sha256, blob_sha256, bytes, built_by_node_id, built_at_ms)
                 VALUES ($1, 1, 100, 10, $2, $3, $4, 10, 'node-a', 22)",
                hiqlite::params!(
                    &job.cache_key,
                    &job.source_sha256,
                    &job.pipeline_sha256,
                    "c".repeat(64)
                ),
            ),
            (
                "UPDATE files SET mtime = 20 WHERE id = 1",
                hiqlite::params!(),
            ),
        ])
        .await
        .expect("publish artifact and replace scanner generation")
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("commit ready replacement fixture");
    store
        .settle_analysis_requests(23)
        .await
        .expect("settle original request");

    store
        .enqueue_analysis_request(&NewAnalysisRequest {
            request_id: "replacement-request".to_owned(),
            file_id: 1,
            source_size: 100,
            source_mtime: 20,
            component: "fragment_index".to_owned(),
            force_rebuild: false,
            target_node_id: "node-a".to_owned(),
            not_before_ms: 30,
            created_at_ms: 30,
        })
        .await
        .expect("enqueue replacement request");
    let replacement_claim = store
        .claim_analysis_request("node-a", 30, 1_030)
        .await
        .expect("claim replacement")
        .expect("replacement owner");
    let mut replacement_job = job.clone();
    replacement_job.source_mtime = 20;
    replacement_job.not_before_ms = 31;
    replacement_job.created_at_ms = 31;
    assert!(store
        .submit_fragment_index_analysis(&replacement_claim, &replacement_job, 31)
        .await
        .expect("join ready artifact after scanner replacement"));
    assert_eq!(
        store
            .settle_analysis_requests(32)
            .await
            .expect("settle replacement request"),
        1
    );

    store
        .enqueue_analysis_request(&NewAnalysisRequest {
            request_id: "force-request".to_owned(),
            file_id: 1,
            source_size: 100,
            source_mtime: 20,
            component: "fragment_index".to_owned(),
            force_rebuild: true,
            target_node_id: "node-a".to_owned(),
            not_before_ms: 40,
            created_at_ms: 40,
        })
        .await
        .expect("enqueue forced rebuild");
    let force_claim = store
        .claim_analysis_request("node-a", 40, 1_040)
        .await
        .expect("claim forced rebuild")
        .expect("forced owner");
    let mut forced_job = replacement_job.clone();
    forced_job.not_before_ms = 41;
    forced_job.created_at_ms = 41;
    assert!(store
        .submit_fragment_index_analysis(&force_claim, &forced_job, 41)
        .await
        .expect("force reopen ready job"));
    assert_eq!(
        store
            .cluster_fragment_index_job(&job.cache_key)
            .await
            .expect("read forced worker")
            .expect("forced worker")
            .state,
        "queued"
    );
    assert!(store
        .cluster_fragment_index_artifact(&job.cache_key)
        .await
        .expect("read artifact during rebuild")
        .is_some());
    let force_submitted = store
        .analysis_requests(20)
        .await
        .expect("read forced request after handoff")
        .into_iter()
        .find(|request| request.request_id == "force-request")
        .expect("forced request after handoff");
    assert_eq!(force_submitted.state, "submitted");
    assert!(force_submitted.owner_node_id.is_empty());

    client
        .execute(
            "UPDATE cluster_fragment_index_jobs
                SET state = 'ready', owner_node_id = NULL, lease_expires_ms = NULL,
                    updated_at_ms = 42 WHERE cache_key = $1",
            hiqlite::params!(&job.cache_key),
        )
        .await
        .expect("finish forced worker before stale replay");
    assert!(!store
        .submit_fragment_index_analysis(&force_claim, &forced_job, 41)
        .await
        .expect("replay accepted forced handoff"));
    let after_replay = store
        .cluster_fragment_index_job(&job.cache_key)
        .await
        .expect("read worker after stale replay")
        .expect("worker after stale replay");
    assert_eq!(after_replay.state, "ready");
    assert_eq!(after_replay.file_id, 1);
    assert_eq!(after_replay.updated_at_ms, 42);
    client
        .execute(
            "UPDATE cluster_fragment_index_jobs
                SET state = 'queued', updated_at_ms = 43 WHERE cache_key = $1",
            hiqlite::params!(&job.cache_key),
        )
        .await
        .expect("restore queued source-replacement fixture");

    client
        .txn([
            ("DELETE FROM files WHERE id = 1", hiqlite::params!()),
            (
                "INSERT INTO files (id, item_id, path, size, mtime, scanned_at) \
                 VALUES (2, 1, '/replacement.mkv', 100, 30, 50)",
                hiqlite::params!(),
            ),
        ])
        .await
        .expect("replace file identity while forced job is queued")
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("commit file replacement");
    store
        .enqueue_analysis_request(&NewAnalysisRequest {
            request_id: "rebind-request".to_owned(),
            file_id: 2,
            source_size: 100,
            source_mtime: 30,
            component: "fragment_index".to_owned(),
            force_rebuild: false,
            target_node_id: "node-a".to_owned(),
            not_before_ms: 50,
            created_at_ms: 50,
        })
        .await
        .expect("enqueue surviving file");
    let rebind_claim = store
        .claim_analysis_request("node-a", 50, 1_050)
        .await
        .expect("claim surviving file")
        .expect("surviving owner");
    let mut rebound_job = job.clone();
    rebound_job.file_id = 2;
    rebound_job.source_mtime = 30;
    rebound_job.not_before_ms = 51;
    rebound_job.created_at_ms = 51;
    assert!(store
        .submit_fragment_index_analysis(&rebind_claim, &rebound_job, 51)
        .await
        .expect("rebind cancelled content"));
    let rebound = store
        .cluster_fragment_index_job(&job.cache_key)
        .await
        .expect("read rebound worker")
        .expect("rebound worker");
    assert_eq!(rebound.state, "queued");
    assert_eq!(rebound.file_id, 2);
    assert_eq!(rebound.created_at_ms, 51);
    assert!(store
        .cluster_fragment_index_artifact(&job.cache_key)
        .await
        .expect("read retained artifact after rebind")
        .is_some());
}

#[cfg(feature = "hiqlite-contract-tests")]
#[derive(Clone, Debug, Serialize, Deserialize)]
struct ContractNodeSpec {
    id: u64,
    raft: String,
    api: String,
}

#[cfg(feature = "hiqlite-contract-tests")]
#[derive(Clone, Debug, Serialize, Deserialize)]
struct ContractNodeLaunch {
    node_id: u64,
    root: PathBuf,
    nodes: Vec<ContractNodeSpec>,
}

#[cfg(feature = "hiqlite-contract-tests")]
struct ContractNodeProcess {
    _child: Child,
    _input: Option<ChildStdin>,
    _output: ChildStdout,
}

#[cfg(feature = "hiqlite-contract-tests")]
struct ContractCluster {
    addresses: Vec<String>,
    _root: tempfile::TempDir,
    _nodes: Vec<ContractNodeProcess>,
}

#[cfg(feature = "hiqlite-contract-tests")]
#[derive(Debug)]
enum ContractStartError {
    PortCollision,
    Failed(String),
}

#[cfg(feature = "hiqlite-contract-tests")]
struct ContractStartupEvent {
    node_id: u64,
    result: Result<(), ContractStartError>,
    output: Option<ChildStdout>,
}

#[cfg(feature = "hiqlite-contract-tests")]
impl ContractCluster {
    async fn start() -> Self {
        const ATTEMPTS: usize = 5;
        install_contract_crypto_provider();
        for attempt in 1..=ATTEMPTS {
            match Self::try_start().await {
                Ok(cluster) => return cluster,
                Err(ContractStartError::PortCollision) if attempt < ATTEMPTS => continue,
                Err(ContractStartError::PortCollision) => {
                    panic!("three-voter contract exhausted {ATTEMPTS} port-bind attempts")
                }
                Err(ContractStartError::Failed(error)) => {
                    panic!("three-voter contract startup failed: {error}")
                }
            }
        }
        unreachable!("contract startup loop returns or panics")
    }

    async fn try_start() -> Result<Self, ContractStartError> {
        let root = tempfile::tempdir().expect("three-voter contract root");
        // Select the complete six-port set while all probe listeners coexist,
        // so one call cannot contain duplicate port numbers. The listeners
        // must be released before Hiqlite can bind; try_start reports that
        // remaining cross-process race and start() reallocates the whole set.
        let mut ports = contract_free_ports(6).into_iter();
        let specs = (1..=3)
            .map(|id| ContractNodeSpec {
                id,
                raft: format!(
                    "127.0.0.1:{}",
                    ports.next().expect("reserved contract Raft port")
                ),
                api: format!(
                    "127.0.0.1:{}",
                    ports.next().expect("reserved contract API port")
                ),
            })
            .collect::<Vec<_>>();
        let executable = std::env::current_exe().expect("contract test executable");
        let mut starting = Vec::new();
        let (event_tx, event_rx) = std::sync::mpsc::channel();
        for node_id in 1..=3 {
            let launch = ContractNodeLaunch {
                node_id,
                root: root.path().to_path_buf(),
                nodes: specs.clone(),
            };
            let mut child = Command::new(&executable)
                .arg("hiqlite_contract_node_process")
                .arg("--ignored")
                .arg("--exact")
                .arg("--nocapture")
                .env(
                    "PLURX_CONTRACT_NODE_LAUNCH",
                    serde_json::to_string(&launch).expect("serialize node launch"),
                )
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn contract voter");
            let input = child.stdin.take().expect("contract voter stdin");
            let output = child.stdout.take().expect("contract voter stdout");
            let stdout_tx = event_tx.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(output);
                let result = loop {
                    let mut line = String::new();
                    match reader.read_line(&mut line) {
                        Ok(0) => {
                            break Err(ContractStartError::Failed(format!(
                                "contract voter {node_id} exited before ready"
                            )))
                        }
                        Ok(_) if line.trim() == format!("PLURX_CONTRACT_NODE_READY {node_id}") => {
                            break Ok(())
                        }
                        Ok(_) if line.starts_with("PLURX_CONTRACT_NODE_PORT_COLLISION ") => {
                            break Err(ContractStartError::PortCollision)
                        }
                        Ok(_) if line.starts_with("PLURX_CONTRACT_NODE_START_FAILED ") => {
                            break Err(ContractStartError::Failed(line.trim().to_owned()))
                        }
                        Ok(_) => {}
                        Err(error) => {
                            break Err(ContractStartError::Failed(format!(
                                "read contract voter {node_id} startup: {error}"
                            )))
                        }
                    }
                };
                let _ = stdout_tx.send(ContractStartupEvent {
                    node_id,
                    result,
                    output: Some(reader.into_inner()),
                });
            });
            starting.push((node_id, child, Some(input)));
        }

        let mut outputs = std::collections::BTreeMap::new();
        while outputs.len() < starting.len() {
            let event = match event_rx.recv_timeout(Duration::from_secs(60)) {
                Ok(event) => event,
                Err(error) => {
                    stop_contract_starting(&mut starting);
                    return Err(ContractStartError::Failed(format!(
                        "contract startup readiness timeout: {error}"
                    )));
                }
            };
            if let Err(error) = event.result {
                stop_contract_starting(&mut starting);
                return Err(error);
            }
            if let Some(output) = event.output {
                outputs.insert(event.node_id, output);
            }
        }
        let mut nodes = Vec::new();
        for (node_id, child, input) in starting {
            nodes.push(ContractNodeProcess {
                _child: child,
                _input: input,
                _output: outputs
                    .remove(&node_id)
                    .expect("ready contract voter output"),
            });
        }
        Ok(Self {
            addresses: specs.into_iter().map(|node| node.api).collect(),
            _root: root,
            _nodes: nodes,
        })
    }
}

#[cfg(feature = "hiqlite-contract-tests")]
fn stop_contract_starting(starting: &mut [(u64, Child, Option<ChildStdin>)]) {
    for (_, child, input) in starting.iter_mut() {
        drop(input.take());
        let _ = child.kill();
    }
    for (_, child, _) in starting.iter_mut() {
        let _ = child.wait();
    }
}

#[cfg(feature = "hiqlite-contract-tests")]
impl Drop for ContractCluster {
    fn drop(&mut self) {
        // Ask every voter to stop before removing their shared temporary data
        // root, then reap them so a serial suite cannot accumulate old voters
        // while its next isolated cluster is under load.
        for node in &mut self._nodes {
            drop(node._input.take());
        }
        for node in &mut self._nodes {
            node._child.wait().expect("reap three-voter contract child");
        }
    }
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "spawned by the backend-neutral contract factory"]
async fn hiqlite_contract_node_process() {
    install_contract_crypto_provider();
    let launch: ContractNodeLaunch = serde_json::from_str(
        &std::env::var("PLURX_CONTRACT_NODE_LAUNCH").expect("contract node launch"),
    )
    .expect("decode contract node launch");
    let _ = ServerTlsConfig::server_config_self_signed("127.0.0.1").await;
    let data_dir = launch.root.join(format!("node-{}", launch.node_id));
    std::fs::create_dir_all(&data_dir).expect("contract node data directory");
    let client = match hiqlite::start_node(NodeConfig {
        node_id: launch.node_id,
        nodes: launch
            .nodes
            .iter()
            .map(|node| Node {
                id: node.id,
                addr_raft: node.raft.clone(),
                addr_api: node.api.clone(),
            })
            .collect(),
        listen_addr_api: Cow::Borrowed("127.0.0.1"),
        listen_addr_raft: Cow::Borrowed("127.0.0.1"),
        data_dir: Cow::Owned(data_dir.to_string_lossy().into_owned()),
        filename_db: Cow::Borrowed("contract.db"),
        secret_raft: CONTRACT_RAFT_SECRET.to_owned(),
        secret_api: CONTRACT_API_SECRET.to_owned(),
        tls_raft: Some(ServerTlsConfig::TlsAutoCertificates),
        tls_api: Some(ServerTlsConfig::TlsAutoCertificates),
        health_check_delay_secs: 0,
        wal_size: HIQLITE_WAL_SIZE_BYTES,
        raft_config: NodeConfig::default_raft_config(10_000),
        ..Default::default()
    })
    .await
    {
        Ok(client) => client,
        Err(error) => {
            let message = error.to_string();
            let lower = message.to_ascii_lowercase();
            if lower.contains("address already in use")
                || lower.contains("addrinuse")
                || lower.contains("os error 48")
                || lower.contains("os error 98")
            {
                println!("PLURX_CONTRACT_NODE_PORT_COLLISION {}", launch.node_id);
            } else {
                println!(
                    "PLURX_CONTRACT_NODE_START_FAILED {} {}",
                    launch.node_id,
                    message.replace(['\r', '\n'], " ")
                );
            }
            std::io::stdout().flush().expect("flush contract failure");
            return;
        }
    };
    if tokio::time::timeout(Duration::from_secs(45), client.wait_until_healthy_db())
        .await
        .is_err()
    {
        println!(
            "PLURX_CONTRACT_NODE_START_FAILED {} health timeout",
            launch.node_id
        );
        std::io::stdout().flush().expect("flush contract failure");
        return;
    }
    println!("PLURX_CONTRACT_NODE_READY {}", launch.node_id);
    std::io::stdout().flush().expect("flush contract readiness");
    let mut sink = Vec::new();
    tokio::io::stdin()
        .read_to_end(&mut sink)
        .await
        .expect("wait for contract parent");
    std::process::exit(0);
}

#[cfg(feature = "hiqlite-contract-tests")]
fn contract_free_ports(count: usize) -> Vec<u16> {
    let listeners = (0..count)
        .map(|_| TcpListener::bind("127.0.0.1:0").expect("bind contract port"))
        .collect::<Vec<_>>();
    listeners
        .iter()
        .map(|listener| listener.local_addr().expect("contract port address").port())
        .collect()
}

#[cfg(feature = "hiqlite-contract-tests")]
fn install_contract_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[cfg(feature = "hiqlite-contract-tests")]
fn populated_current_import_fixture(data_dir: &std::path::Path) -> PathBuf {
    let path = data_dir.join("plurx.db");
    drop(SqliteStore::open(&path).expect("create current SQLite fixture"));

    let connection = rusqlite::Connection::open(&path).expect("open SQLite import fixture");
    connection
        .execute(
            "UPDATE settings SET value = ?1, updated_at = 101 WHERE key = 'instance.id'",
            [CONTRACT_INSTANCE_ID],
        )
        .expect("set import fixture identity");
    connection
        .execute_batch(
            "INSERT INTO settings (key, value, updated_at)
                 VALUES ('migration.fixture', 'v14', 102);
             INSERT INTO users (id, username, password_hash, is_admin, created_at)
                 VALUES (7, 'Import Admin', 'fixture-password-hash', 1, 103);
             INSERT INTO tokens (token_hash, user_id, device, created_at, last_seen_at)
                 VALUES ('fixture-token-hash', 7, 'fixture-device', 104, 105);
             INSERT INTO api_keys
                 (id, name, key_hash, scopes, created_at, last_used_at, disabled)
                 VALUES (8, 'fixture key', 'fixture-key-hash', '[\"status:read\"]',
                         106, 107, 0);
             INSERT INTO libraries
                 (id, name, kind, paths, anime, created_at, scan_interval_mins,
                  refresh_interval_mins, last_scan_at, last_refresh_at)
                 VALUES (9, 'Imported Shows', 'shows', '[\"/fixture/shows\"]', 0,
                         108, 30, 60, 109, 110);
             INSERT INTO items
                 (id, library_id, kind, parent_id, title, sort_title, year, overview,
                  added_at, updated_at, tags, genres)
                 VALUES (20, 9, 'show', NULL, 'Imported Show', 'Imported Show', 2024,
                         'fixture parent', 111, 112, '[\"fixture\"]', '[\"Drama\"]');
             INSERT INTO items
                 (id, library_id, kind, parent_id, title, sort_title, season_number,
                  added_at, updated_at, tags, genres)
                 VALUES (10, 9, 'season', 20, 'Season 1', 'Season 1', 1,
                         113, 114, '[]', '[]');
             INSERT INTO files
                 (id, item_id, path, size, mtime, duration_ms, container, video_codec,
                  audio_streams, subtitle_streams, scanned_at, audio_offset_ms)
                 VALUES (30, 10, '/fixture/shows/season-1.mkv', 4096, 115, 3600000,
                         'matroska', 'h264', '[]', '[]', 116, 25);
             INSERT INTO watch_state
                 (user_id, item_id, position_ms, duration_ms, watched, updated_at)
                 VALUES (7, 10, 120000, 3600000, 0, 117);
             INSERT INTO reading_state
                 (user_id, item_id, file_id, file_size, file_mtime, locator_json,
                  progression_millis, completed, updated_at)
                 VALUES (7, 10, 30, 4096, 115,
                         '{\"version\":1,\"href\":\"chapter-2.xhtml\"}', 250000, 0, 118);
             INSERT INTO watched_outbox
                 (id, payload, attempts, last_error, status, next_at, created_at,
                  updated_at, claim_until)
                 VALUES (40, '{\"fixture\":true}', 1, '', 'pending', 120, 121, 122, 130);
             INSERT INTO transcode_cache_recipes
                 (recipe_hash, file_id, recipe_version, created_at)
                 VALUES ('fixture-recipe', 30, 1, 123);
             INSERT INTO transcode_cache_locations
                 (recipe_hash, node_id, storage_class, relative_dir, bytes, complete,
                  manifest_digest, last_used_at, last_seen_at)
                 VALUES ('fixture-recipe', 'fixture-node', 'local', 'fixture-recipe',
                         2048, 1,
                         'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                         124, 125);
             INSERT INTO pretranscode_jobs
                 (id, dedupe_key, file_id, source_size, source_mtime, target_height,
                  policy_generation, requirements_json, reason, priority, state,
                  owner_node_id, staging_node_id, fence, lease_expires_ms, attempts,
                  not_before_ms, created_at_ms, updated_at_ms)
                 VALUES
                 ('00000000-0000-4000-8000-000000000201', 'fixture-queued', 30, 4096,
                  115, 720, 'fixture-policy',
                  '{\"version\":1,\"decoder\":\"h264\",\"acceptable_encoder_families\":[\"software\"],\"output_contract\":\"hls-v1\",\"tone_map\":false,\"output_grade\":\"sdr\",\"scratch_bytes\":1024}',
                  'recent', 300, 'queued', NULL, NULL, 0, NULL, 0, 100, 126, 126),
                 ('00000000-0000-4000-8000-000000000202', 'fixture-running', 30, 4096,
                  115, 720, 'fixture-policy',
                  '{\"version\":1,\"decoder\":\"h264\",\"acceptable_encoder_families\":[\"software\"],\"output_contract\":\"hls-v1\",\"tone_map\":false,\"output_grade\":\"sdr\",\"scratch_bytes\":1024}',
                  'next_up', 400, 'running', 'departed-node', 'departed-node', 2, 500,
                  1, 100, 127, 127),
                 ('00000000-0000-4000-8000-000000000203', 'fixture-ready', 30, 4096,
                  115, 720, 'fixture-policy',
                  '{\"version\":1,\"decoder\":\"h264\",\"acceptable_encoder_families\":[\"software\"],\"output_contract\":\"hls-v1\",\"tone_map\":false,\"output_grade\":\"sdr\",\"scratch_bytes\":1024}',
                  'in_progress', 500, 'ready', 'fixture-node', 'fixture-node', 3, NULL,
                  1, 100, 128, 128);
             UPDATE pretranscode_jobs
                SET recipe_hash = 'fixture-recipe', storage_id = 'fixture-node',
                    relative_dir = 'fixture-recipe',
                    manifest_digest = 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'
              WHERE dedupe_key = 'fixture-ready';
             INSERT INTO offline_packages
                 (id, request_id, user_id, file_id, node_id, source_path, source_size,
                  source_mtime, effective_rate_control, target_height, subtitle_mode,
                  state, phase, expires_at)
                 VALUES ('fixture-package', 'fixture-request', 7, 30, 'fixture-node',
                         '/fixture/shows/season-1.mkv', 4096, 115, 'qvbr:21', 720, 'none',
                         'ready', 'complete', 999999);
             INSERT INTO offline_package_leases
                 (token_hash, package_id, created_at, last_access_at, expires_at)
                 VALUES ('fixture-lease-hash', 'fixture-package', 126, 127, 999999);
             INSERT INTO library_roots (library_id, fingerprint)
                 VALUES (9, 'fixture-root-fingerprint');
             INSERT INTO scan_reconcile_guards (library_id) VALUES (9);
             INSERT INTO scan_reconcile_items (library_id, item_id) VALUES (9, 10);
             INSERT INTO job_leases
                 (resource, owner_node_id, fence, revision, expires_at_ms, updated_at_ms)
                 VALUES ('imported-lease', 'departed-node', 9, 12, 500, 400);
             INSERT INTO playback_events
                 (at_unix_ms, user_id, file_id, event, detail)
                 VALUES (131000, 7, 30, 'fixture-local-only', 'must not replicate');",
        )
        .expect("populate SQLite import fixture");
    // Sealed, not cleartext: this fixture stands in for a real install that has
    // booted this build, and such an install has no cleartext bearer column
    // left. Seeding cleartext here would make the import gate below untestable
    // by making the happy path itself the thing that must be refused.
    seed_sealed_trakt_fixture_row(&connection, &fixture_credential_key());
    for ordinal in 0..70 {
        connection
            .execute(
                "INSERT INTO settings (key, value, updated_at) VALUES (?1, ?2, ?3)",
                rusqlite::params![
                    format!("migration.page.{ordinal:03}"),
                    format!("value-{ordinal:03}"),
                    200 + ordinal,
                ],
            )
            .expect("populate paged parity settings");
        for storage_class in ["local", "shared"] {
            connection
                .execute(
                    "INSERT INTO transcode_cache_locations
                         (recipe_hash, node_id, storage_class, relative_dir, bytes, complete,
                          last_used_at, last_seen_at)
                     VALUES ('fixture-recipe', ?1, ?2, ?3, ?4, 1, ?5, ?6)",
                    rusqlite::params![
                        format!("fixture-page-node-{ordinal:03}"),
                        storage_class,
                        format!("fixture-page-location-{ordinal:03}-{storage_class}"),
                        3_000 + ordinal,
                        300 + ordinal,
                        400 + ordinal,
                    ],
                )
                .expect("populate composite-key paged parity locations");
        }
    }
    connection
        .execute_batch(
            "UPDATE transcode_cache_locations
                SET storage_id = 'shared:fixture', generation_id = 'fixture-generation'
              WHERE recipe_hash = 'fixture-recipe'
                AND node_id = 'fixture-page-node-000' AND storage_class = 'shared';
             INSERT INTO cache_storage_members
                (storage_id, node_id, storage_class, verified_at_ms, verification_state)
                VALUES ('shared:fixture', 'fixture-node', 'shared', 500, 'verified');
             INSERT INTO cache_consumer_pins
                (storage_id, recipe_hash, generation_id, consumer_kind,
                 consumer_id, consumer_epoch, expires_at_ms)
                VALUES ('shared:fixture', 'fixture-recipe', 'fixture-generation',
                        'offline_package', 'fixture-package', 1, 999999000);",
        )
        .expect("populate shared cache import facts");
    drop(connection);
    path
}

/// The row-count chunk bound #279 shipped and #282 replaced. Present only so
/// the fixtures below can assert they sit *past* it: a fixture the old bound
/// would also have carried proves nothing about a byte bound.
#[cfg(feature = "hiqlite-contract-tests")]
const SUPERSEDED_ROW_CHUNK_BOUND: usize = 16;
/// Usable payload measured under the former 2 MiB production tuning.
#[cfg(feature = "hiqlite-contract-tests")]
const INCIDENT_WAL_USABLE_PAYLOAD_BYTES: usize = 2_097_118;

/// The retained #279 band: adjacent rows with full-size probe documents.
#[cfg(feature = "hiqlite-contract-tests")]
const LARGE_PROBE_FILE_COUNT: i64 = 64;
#[cfg(feature = "hiqlite-contract-tests")]
const LARGE_PROBE_PADDING_BYTES: usize = 48 * 1024;

/// The band a row count cannot bound, and the reason this contract is about
/// bytes: [`SUPERSEDED_ROW_CHUNK_BOUND`] adjacent rows of this size serialize
/// past [`INCIDENT_WAL_USABLE_PAYLOAD_BYTES`], which is #290's production
/// panic. Importing them proves the builder still splits the incident shape on
/// bytes after the WAL is raised.
#[cfg(feature = "hiqlite-contract-tests")]
const OVERSIZED_PROBE_FILE_COUNT: i64 = 18;
#[cfg(feature = "hiqlite-contract-tests")]
const OVERSIZED_PROBE_PADDING_BYTES: usize = 144 * 1024;

/// The importer's single-row ceiling, mirroring its derivation from
/// [`HIQLITE_WAL_USABLE_PAYLOAD_BYTES`] less its encoding reserve and
/// transaction envelope. A row above this cannot be submitted in any
/// transaction, so import refuses the backup.
#[cfg(feature = "hiqlite-contract-tests")]
const CONTRACT_IMPORT_MAX_ROW_BYTES: usize = HIQLITE_WAL_USABLE_PAYLOAD_BYTES - 64 * 1024 - 256;

/// A single probe document larger than the whole WAL payload capacity, so it is
/// unimportable under any bound rather than merely past the reserve. Import must
/// refuse it instead of handing it to the WAL writer.
#[cfg(feature = "hiqlite-contract-tests")]
const UNIMPORTABLE_PROBE_PADDING_BYTES: usize = HIQLITE_WAL_USABLE_PAYLOAD_BYTES + 1024;

/// The premises the probe fixtures rest on, checked where they are declared so
/// a later size tweak cannot quietly turn either regression into a test of
/// something easier than the bound it was written for.
#[cfg(feature = "hiqlite-contract-tests")]
const _: () = {
    assert!(
        OVERSIZED_PROBE_PADDING_BYTES * SUPERSEDED_ROW_CHUNK_BOUND
            > INCIDENT_WAL_USABLE_PAYLOAD_BYTES,
        "the oversized band must exceed what the superseded row bound would have submitted, \
         or the regression re-proves the row count instead of the byte bound"
    );
    assert!(
        LARGE_PROBE_PADDING_BYTES * SUPERSEDED_ROW_CHUNK_BOUND < HIQLITE_WAL_USABLE_PAYLOAD_BYTES,
        "the retained #279 band must stay inside the superseded bound, so the two bands \
         test different things"
    );
    assert!(
        UNIMPORTABLE_PROBE_PADDING_BYTES > HIQLITE_WAL_USABLE_PAYLOAD_BYTES,
        "the refused row must exceed the WAL itself, so the refusal is unarguable rather \
         than an artefact of the reserve held back from it"
    );
    assert!(
        CONTRACT_IMPORT_MAX_ROW_BYTES < HIQLITE_WAL_USABLE_PAYLOAD_BYTES,
        "the single-row ceiling must sit under the capacity it is derived from"
    );
};
#[cfg(feature = "hiqlite-contract-tests")]
const UNIMPORTABLE_PROBE_FILE_ID: i64 = 3_001;

#[cfg(feature = "hiqlite-contract-tests")]
fn large_probe_json(ordinal: i64, padding_bytes: usize) -> String {
    serde_json::json!({
        "format": {
            "filename": format!("/fixture/shows/large-probe-{ordinal:03}.mkv"),
            "tags": {
                "comment": "x".repeat(padding_bytes),
            },
        },
        "ordinal": ordinal,
    })
    .to_string()
}

/// Seeds `count` `files` rows whose `probe_json` is padded to `padding_bytes`,
/// numbered from `first_id` so several bands can share one fixture.
#[cfg(feature = "hiqlite-contract-tests")]
fn seed_probe_band(path: &std::path::Path, first_id: i64, count: i64, padding_bytes: usize) {
    let mut connection = rusqlite::Connection::open(path).expect("open large-probe fixture");
    let transaction = connection
        .transaction()
        .expect("begin large-probe fixture transaction");
    {
        let mut insert = transaction
            .prepare(
                "INSERT INTO files
                     (id, item_id, path, size, mtime, probe_json, scanned_at)
                 VALUES (?1, 10, ?2, ?3, ?4, ?5, ?6)",
            )
            .expect("prepare large-probe file insert");
        for ordinal in 0..count {
            let id = first_id + ordinal;
            insert
                .execute(rusqlite::params![
                    id,
                    format!("/fixture/shows/large-probe-{id:04}.mkv"),
                    10_000 + id,
                    500 + id,
                    large_probe_json(id, padding_bytes),
                    600 + id,
                ])
                .expect("insert large-probe file");
        }
    }
    transaction
        .commit()
        .expect("commit large-probe fixture transaction");
}

/// The key the import fixtures seal under.
///
/// Fixed bytes rather than `generate()` so a fixture built in one test can be
/// opened in another, and so an envelope in a failure message is reproducible.
/// It guards nothing real — the values it seals are the string `fixture-access`.
#[cfg(feature = "hiqlite-contract-tests")]
fn fixture_credential_key() -> CredentialKey {
    CredentialKey::from_bytes([0x2b; 32])
}

#[cfg(feature = "hiqlite-contract-tests")]
const FIXTURE_TRAKT_USER: i64 = 7;
#[cfg(feature = "hiqlite-contract-tests")]
const FIXTURE_TRAKT_ACCESS: &str = "fixture-access";
#[cfg(feature = "hiqlite-contract-tests")]
const FIXTURE_TRAKT_REFRESH: &str = "fixture-refresh";

#[cfg(feature = "hiqlite-contract-tests")]
fn seed_sealed_trakt_fixture_row(connection: &rusqlite::Connection, key: &CredentialKey) {
    let access = key
        .seal_trakt(FIXTURE_TRAKT_USER, FIXTURE_TRAKT_ACCESS)
        .expect("seal fixture access token");
    let refresh = key
        .seal_trakt(FIXTURE_TRAKT_USER, FIXTURE_TRAKT_REFRESH)
        .expect("seal fixture refresh token");
    connection
        .execute(
            "INSERT INTO trakt_auth
                 (user_id, access_token, refresh_token, expires_at, trakt_username,
                  connected_at, last_sync_at, last_activities)
             VALUES (?1, ?2, ?3, 999999, 'fixture-user', 118, 119, '{\"movies\":1}')
             ON CONFLICT(user_id) DO UPDATE SET
                 access_token = excluded.access_token,
                 refresh_token = excluded.refresh_token",
            rusqlite::params![FIXTURE_TRAKT_USER, access.as_stored(), refresh.as_stored(),],
        )
        .expect("seed sealed Trakt fixture row");
}

/// Rewrite the fixture's Trakt row the way a pre-encryption build wrote it.
///
/// Raw SQL on purpose: `put_trakt_auth` now refuses an unsealed credential, so
/// the only honest way to produce a legacy backup is to go around the boundary
/// that did not exist when those rows were written.
#[cfg(feature = "hiqlite-contract-tests")]
fn make_trakt_fixture_row_cleartext(path: &std::path::Path) {
    rusqlite::Connection::open(path)
        .expect("open fixture to downgrade its Trakt row")
        .execute(
            "UPDATE trakt_auth SET access_token = ?1, refresh_token = ?2 WHERE user_id = ?3",
            rusqlite::params![
                FIXTURE_TRAKT_ACCESS,
                FIXTURE_TRAKT_REFRESH,
                FIXTURE_TRAKT_USER,
            ],
        )
        .expect("write legacy cleartext Trakt row");
}

#[cfg(feature = "hiqlite-contract-tests")]
fn populated_v14_import_fixture(data_dir: &std::path::Path) -> PathBuf {
    let path = populated_current_import_fixture(data_dir);
    let connection = rusqlite::Connection::open(&path).expect("open current SQLite fixture");
    // Recreate the exact v15-v19 schema differences so this is also a valid
    // input to ordinary SQLite startup migration, not merely a current-schema
    // database carrying an older user_version. The activation coordinator now
    // runs that ordinary upgrade before publishing its immutable backup.
    connection
        .execute_batch(
            "PRAGMA foreign_keys = OFF;
             DROP TRIGGER transcode_cache_location_identity_au;
             DROP TRIGGER transcode_cache_location_identity_ai;
             DROP INDEX transcode_cache_storage_lru;
             DROP INDEX transcode_cache_storage_generation;
             DROP TABLE cache_consumer_pins;
             DROP TABLE cache_storage_members;
             DROP INDEX rendition_plans_by_file;
             DROP TABLE rendition_plans;
             DROP TABLE fragment_indexes;
             ALTER TABLE transcode_cache_locations DROP COLUMN generation_id;
             ALTER TABLE transcode_cache_locations DROP COLUMN storage_id;
             DROP TRIGGER library_roots_paths_au;
             DROP TABLE scan_reconcile_items;
             DROP TABLE scan_reconcile_guards;
             DROP TABLE library_roots;
             DROP TABLE playback_events;
             DROP TABLE network_priors;
             DROP TABLE reading_state;
             DROP TABLE media_session_requests;
             DROP TABLE media_playback_pointers;
             DROP TABLE media_sessions;
             DROP TABLE job_leases;
             DROP TRIGGER pretranscode_jobs_cancel_source;
             DROP INDEX pretranscode_jobs_active;
             DROP INDEX pretranscode_jobs_staging;
             DROP INDEX pretranscode_jobs_dedupe;
             DROP INDEX pretranscode_jobs_due;
             DROP TABLE pretranscode_jobs;
             ALTER TABLE transcode_cache_locations DROP COLUMN manifest_digest;
             ALTER TABLE transcode_cache_locations DROP COLUMN scrub_object_index;
             DROP INDEX idx_items_book_work;
             ALTER TABLE items DROP COLUMN book_metadata_source;
             ALTER TABLE items DROP COLUMN book_edition_id;
             ALTER TABLE items DROP COLUMN book_work_id;
             ALTER TABLE items DROP COLUMN author;
             ALTER TABLE offline_packages DROP COLUMN effective_rate_control;
             DROP INDEX watched_outbox_due;
             ALTER TABLE watched_outbox RENAME TO watched_outbox_current;
             CREATE TABLE watched_outbox (
                 id INTEGER PRIMARY KEY,
                 payload TEXT NOT NULL,
                 attempts INTEGER NOT NULL DEFAULT 0,
                 last_error TEXT NOT NULL DEFAULT '',
                 status TEXT NOT NULL DEFAULT 'pending'
                     CHECK (status IN ('pending', 'ok', 'failed')),
                 next_at INTEGER NOT NULL DEFAULT 0,
                 created_at INTEGER NOT NULL DEFAULT (unixepoch()),
                 updated_at INTEGER NOT NULL DEFAULT (unixepoch())
             ) STRICT;
             INSERT INTO watched_outbox
                 (id, payload, attempts, last_error, status, next_at, created_at, updated_at)
                 SELECT id, payload, attempts, last_error, status, next_at, created_at, updated_at
                 FROM watched_outbox_current;
             DROP TABLE watched_outbox_current;
             CREATE INDEX watched_outbox_due ON watched_outbox(status, next_at);
             PRAGMA user_version = 14;
             PRAGMA foreign_keys = ON;",
        )
        .expect("downgrade fixture shape to v14");
    drop(connection);
    path
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn populated_v14_sqlite_import_has_exact_three_voter_parity() {
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let store = open_contract_hiqlite_store(&cluster).await;
    store
        .validation_reset_contract_state()
        .await
        .expect("reset replicated import target");

    let source = tempfile::tempdir().expect("SQLite import fixture directory");
    populated_v14_import_fixture(source.path());
    let prepared = prepare_sqlite_import(source.path()).expect("prepare v14 import backup");
    assert_eq!(prepared.schema_version, 14);

    let report = store
        .import_sqlite_backup(
            &prepared.backup_path,
            &prepared.backup_sha256,
            prepared.schema_version,
        )
        .await
        .expect("import populated v14 backup");
    assert_eq!(report.source_schema_version, 14);
    assert_eq!(report.backup_sha256, prepared.backup_sha256);
    assert_eq!(report.tables.len(), 29);
    assert_eq!(report.search_rows, 2);
    assert_eq!(
        report
            .tables
            .iter()
            .find(|digest| digest.table == "reading_state")
            .expect("reading-state digest")
            .row_count,
        0,
        "a v14 source predates reading state"
    );
    assert!(
        report
            .tables
            .iter()
            .find(|digest| digest.table == "settings")
            .expect("settings digest")
            .row_count
            > 64,
        "settings parity must cross the 64-row keyset page boundary"
    );
    assert_eq!(
        report
            .tables
            .iter()
            .find(|digest| digest.table == "transcode_cache_locations")
            .expect("transcode cache locations digest")
            .row_count,
        141,
        "the leading fixture row plus two storage classes per node must split a \
         three-column key between parity pages"
    );
    assert!(report.imported_rows >= 16);
    for table in [
        "library_roots",
        "scan_reconcile_guards",
        "scan_reconcile_items",
        "job_leases",
    ] {
        assert_eq!(
            report
                .tables
                .iter()
                .find(|digest| digest.table == table)
                .expect("v14 compatibility table digest")
                .row_count,
            0,
            "{table} must be empty for a v14 source"
        );
    }
    assert_eq!(store.count_users().await.expect("imported user count"), 1);
    assert_eq!(
        store
            .offline_package_for_user("fixture-package", 7)
            .await
            .expect("read imported v14 package")
            .expect("imported v14 package")
            .effective_rate_control,
        "vbr",
        "a pre-v18 source must receive the only truthful legacy identity"
    );
    assert_eq!(
        store
            .get_file(30)
            .await
            .expect("read imported file")
            .expect("imported file")
            .audio_offset_ms,
        25
    );
    assert_eq!(
        store
            .watch_state(7, 10)
            .await
            .expect("read imported watch state")
            .expect("imported watch state")
            .position_ms,
        120_000
    );
    assert!(
        store
            .playback_events(&PlaybackEventQuery::default())
            .await
            .expect("read node-local telemetry")
            .is_empty(),
        "SQLite playback telemetry must never enter replicated state"
    );

    let retry = store
        .import_sqlite_backup(
            &prepared.backup_path,
            &prepared.backup_sha256,
            prepared.schema_version,
        )
        .await
        .expect_err("a populated target must refuse a merge-style retry");
    assert!(retry.to_string().contains("target is not fresh"));
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn populated_current_sqlite_import_preserves_new_durable_rows_only() {
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let store = open_contract_hiqlite_store(&cluster).await;
    store
        .validation_reset_contract_state()
        .await
        .expect("reset replicated current-schema import target");

    let source = tempfile::tempdir().expect("current SQLite import fixture directory");
    populated_current_import_fixture(source.path());
    let prepared = prepare_sqlite_import(source.path()).expect("prepare current import backup");
    assert_eq!(
        prepared.schema_version,
        plurx_core::store::SQLITE_SCHEMA_VERSION
    );

    let checksum_error = store
        .import_sqlite_backup(
            &prepared.backup_path,
            &"0".repeat(64),
            prepared.schema_version,
        )
        .await
        .expect_err("wrong content hash must fail before import");
    assert!(checksum_error.to_string().contains("checksum changed"));

    let report = store
        .import_sqlite_backup(
            &prepared.backup_path,
            &prepared.backup_sha256,
            prepared.schema_version,
        )
        .await
        .expect("import populated current backup");
    assert_eq!(report.search_rows, 2);
    assert_eq!(
        report
            .tables
            .iter()
            .find(|digest| digest.table == "reading_state")
            .expect("reading-state digest")
            .row_count,
        1
    );
    let reading = store
        .reading_state(7, 10, 30)
        .await
        .expect("read imported reading state")
        .expect("imported reading state");
    assert_eq!(reading.progression_millis, 250_000);
    assert_eq!(reading.file_size, 4_096);
    assert_eq!(reading.file_mtime, 115);
    assert_eq!(
        report
            .tables
            .iter()
            .find(|digest| digest.table == "pretranscode_jobs")
            .expect("pretranscode queue digest")
            .row_count,
        3,
        "queued, running, and ready generations must all enter parity"
    );
    let imported_cache = store
        .cache_hit("fixture-recipe", "fixture-node")
        .await
        .expect("read imported cache location")
        .expect("imported ready cache location");
    assert_eq!(
        imported_cache.manifest_digest.as_deref(),
        Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        "the authoritative generation digest must survive import"
    );
    assert_eq!(
        imported_cache.scrub_object_index, 0,
        "a fresh imported generation must start at the first scrub object"
    );
    let imported_capabilities = PretranscodeWorkerCapabilities {
        version: PretranscodeRequirements::VERSION,
        decoders: vec!["h264".to_owned()],
        encoder_families: vec!["software".to_owned()],
        max_target_height: 2_160,
        output_contracts: vec!["hls-v1".to_owned()],
        tone_map: false,
        output_grades: vec!["sdr".to_owned()],
        scratch_bytes: 2_048,
    };
    let imported_claim = store
        .claim_pretranscode_job("import-worker", &imported_capabilities, &[], 1_000, 2_000)
        .await
        .expect("claim imported queue work")
        .expect("an imported due job must remain runnable");
    assert_eq!(
        imported_claim.dedupe_key, "fixture-running",
        "the expired higher-priority running generation should be reclaimed first"
    );
    assert_eq!(
        store
            .offline_package_for_user("fixture-package", 7)
            .await
            .expect("read imported current package")
            .expect("imported current package")
            .effective_rate_control,
        "qvbr:21"
    );
    for table in [
        "library_roots",
        "scan_reconcile_guards",
        "scan_reconcile_items",
        "job_leases",
    ] {
        assert_eq!(
            report
                .tables
                .iter()
                .find(|digest| digest.table == table)
                .expect("current compatibility table digest")
                .row_count,
            1,
            "{table} must survive a current-schema import"
        );
    }
    let imported_lease = acquired(
        store
            .acquire_lease("imported-lease", "surviving-node", 500, 800)
            .await
            .expect("take over imported expired lease"),
        "current SQLite import",
    );
    assert_eq!(imported_lease.fence, 10);
    assert!(
        store
            .playback_events(&PlaybackEventQuery::default())
            .await
            .expect("read node-local telemetry")
            .is_empty(),
        "source playback telemetry must never enter the Hiqlite sidecar"
    );
}

/// The exact text `crates/plurx-core/src/store/hiqlite.rs` produces when its
/// private per-operation `STORE_TIMEOUT` expires.
///
/// Duplicated rather than imported because it is a production internal, and
/// pinned against the real source by
/// [`a_replicated_deadline_is_never_reported_as_a_wal_size_violation`] so a
/// reworded deadline cannot silently stop being recognized as one.
const REPLICATED_DEADLINE_MESSAGE: &str = "replicated store operation timed out";

/// The one claim the byte-bound contract is entitled to make. Nothing that did
/// not actually observe an oversized Raft transaction may print it.
///
/// This is the live wording of
/// [`large_probe_json_import_respects_the_production_wal_limit`]'s verdict.
/// #282 replaced #279's row cap with a byte budget and reworded it from
/// `large probe_json rows must fit the production 2 MiB WAL`; the claim being
/// guarded is unchanged, so a later rewording should move this constant rather
/// than reintroduce a second sentence for the same verdict.
const WAL_SIZE_VERDICT: &str = "byte-bounded transactions must carry probe rows a row count cannot";

/// How many times a replicated contract step re-attempts after the store
/// reports its per-operation deadline.
///
/// This is a margin, not a measured budget. `STORE_TIMEOUT` is three seconds
/// per operation, and it is a production safety bound this suite must not
/// relax. Under `make validate` this gate runs beside every other check on one
/// host, where a three-second slice is reachable by scheduling pressure alone;
/// the same import that needs ~10s in isolation has been observed spending
/// 47-52s losing a race against that deadline. Re-attempts cost nothing on a
/// quiet machine because the first one succeeds, and the size contract itself
/// is decided by the WAL writer's byte check, never by how long the host took.
const REPLICATED_DEADLINE_ATTEMPTS: u32 = 5;

/// What a replicated call actually told us, split by whether it is evidence
/// about the contract under test.
enum ReplicatedOutcome<T> {
    /// The operation answered.
    Ready(T),
    /// The production per-operation deadline expired before the store answered.
    ///
    /// This says nothing about payload size, row parity, or any other durable
    /// promise — it is the host reporting that it was too busy to finish in
    /// three seconds. It is never a contract verdict on its own.
    Deadline,
    /// The store returned a real answer. This is the contract's verdict.
    Fault(StoreError),
}

fn classify_replicated<T>(result: Result<T, StoreError>) -> ReplicatedOutcome<T> {
    match result {
        Ok(value) => ReplicatedOutcome::Ready(value),
        Err(StoreError::Database(message)) if message.contains("timed out") => {
            ReplicatedOutcome::Deadline
        }
        Err(error) => ReplicatedOutcome::Fault(error),
    }
}

/// The panic text for a genuine size verdict.
///
/// An oversized transaction reaches the client as `ClientWriteError: panicked`
/// because `hiqlite-wal` refuses the write in the leader with
/// `` `data` length must not exceed `wal_size` ``. That is a byte comparison in
/// the writer, so it is a structural fact about the payload and is reported the
/// same way on an idle and a saturated host.
fn wal_size_diagnosis(error: &StoreError) -> String {
    format!("{WAL_SIZE_VERDICT}: {error}")
}

/// The panic text for a host that never let the import run to completion.
///
/// Deliberately does not contain [`WAL_SIZE_VERDICT`]: nothing here observed a
/// transaction size, so claiming the WAL bound was violated would be a
/// fabricated verdict — the defect #368 exists to remove.
fn replicated_deadline_diagnosis(label: &str, attempts: u32) -> String {
    format!(
        "{label}: the replicated store reported {REPLICATED_DEADLINE_MESSAGE:?} on all \
         {attempts} attempts while the voters stayed reachable. The host never let this \
         operation finish, so the production 2 MiB WAL bound was neither proved nor \
         violated and this run is not durable-state evidence either way. This is a \
         load-sensitive cluster check: see \"Load-sensitive cluster checks\" in \
         docs/VALIDATION.md, then rerun `make cluster-check` on an otherwise idle machine."
    )
}

/// The panic text for a deadline whose voter then failed a consistent read.
///
/// This is the case that keeps deadline tolerance honest. An oversized
/// transaction kills the leader's write task, and a client racing that death
/// can see the deadline before it sees `ClientWriteError`. A merely busy voter
/// still answers a consistent read, so a failed readiness probe separates the
/// two by cluster state rather than by elapsed time, and this one does earn the
/// size verdict.
fn unreachable_voter_diagnosis(probe: &StoreError) -> String {
    format!(
        "{WAL_SIZE_VERDICT}: the import hit the replicated deadline and the voter then \
         failed a consistent readiness read ({probe}), which is what an oversized Raft \
         transaction does to the leader — a busy host still answers this probe"
    )
}

/// Run one replicated read, re-attempting only while the store reports its
/// per-operation deadline. Any real error is the contract's answer and fails
/// immediately.
#[cfg(feature = "hiqlite-contract-tests")]
async fn replicated_read<T, F, Fut>(label: &str, mut operation: F) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, StoreError>>,
{
    for _ in 0..REPLICATED_DEADLINE_ATTEMPTS {
        match classify_replicated(operation().await) {
            ReplicatedOutcome::Ready(value) => return value,
            ReplicatedOutcome::Fault(error) => panic!("{label}: {error}"),
            ReplicatedOutcome::Deadline => continue,
        }
    }
    panic!(
        "{}",
        replicated_deadline_diagnosis(label, REPLICATED_DEADLINE_ATTEMPTS)
    );
}

/// A busy host must never be able to print the WAL-size verdict.
///
/// Regression for #368: `make validate` reported
/// `Database("replicated store operation timed out")` as [`WAL_SIZE_VERDICT`]
/// on a branch whose entire diff was two JSON/text files, so a saturated worker
/// was indistinguishable from a real durable-state regression. Runs without the
/// cluster on purpose — the rule it protects must not itself be load-sensitive.
#[test]
fn a_replicated_deadline_is_never_reported_as_a_wal_size_violation() {
    assert!(
        include_str!("../src/store/hiqlite.rs").contains(REPLICATED_DEADLINE_MESSAGE),
        "the production per-operation deadline no longer produces {REPLICATED_DEADLINE_MESSAGE:?}; \
         update REPLICATED_DEADLINE_MESSAGE, or every timeout starts being reported as a \
         durable-state contract violation again"
    );

    let deadline = classify_replicated::<()>(Err(StoreError::Database(
        REPLICATED_DEADLINE_MESSAGE.to_owned(),
    )));
    assert!(
        matches!(deadline, ReplicatedOutcome::Deadline),
        "the production deadline text must classify as a deadline, not as a store fault"
    );
    let diagnosis =
        replicated_deadline_diagnosis("large-probe import", REPLICATED_DEADLINE_ATTEMPTS);
    assert!(
        !diagnosis.contains(WAL_SIZE_VERDICT),
        "a timeout must not be presented as a WAL-size violation, got: {diagnosis}"
    );
    assert!(
        diagnosis.contains(REPLICATED_DEADLINE_MESSAGE) && diagnosis.contains("docs/VALIDATION.md"),
        "a timeout must name itself and where its diagnosis is written down, got: {diagnosis}"
    );

    // The real violation still gets the verdict: `hiqlite-wal` refuses the
    // oversized write in the leader and the client sees this exact string.
    let violation = classify_replicated::<()>(Err(StoreError::Database(
        "ClientWriteError: panicked".to_owned(),
    )));
    let ReplicatedOutcome::Fault(error) = violation else {
        panic!("an oversized-transaction write error must classify as a store fault");
    };
    assert!(
        wal_size_diagnosis(&error).contains(WAL_SIZE_VERDICT),
        "an oversized Raft transaction must still be reported as a WAL-size violation"
    );
    assert!(
        unreachable_voter_diagnosis(&error).contains(WAL_SIZE_VERDICT),
        "a deadline whose voter stopped answering must be reported as a WAL-size violation"
    );
    assert!(
        matches!(
            classify_replicated(Ok::<_, StoreError>(())),
            ReplicatedOutcome::Ready(())
        ),
        "a successful replicated call must stay a success"
    );
}

/// Import transactions must be bounded by serialized bytes, not by a row count.
///
/// The fixture is built to defeat a row count on purpose: the oversized band's
/// rows are large enough that [`SUPERSEDED_ROW_CHUNK_BOUND`] adjacent ones
/// exceed the production WAL payload capacity, which is exactly the entry that
/// panicked node `m6` into an HTTP-healthy unreplicated boot (#290). That
/// premise is asserted where the fixture sizes are declared, so passing means
/// the builder split on bytes rather than that the row count was favourable.
///
/// The #279 band is retained alongside it: the byte bound must not regress the
/// ordinary large-library case that motivated the row cap.
#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn large_probe_json_import_respects_the_production_wal_limit() {
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let store = open_contract_hiqlite_store(&cluster).await;
    store
        .validation_reset_contract_state()
        .await
        .expect("reset replicated large-probe import target");

    let source = tempfile::tempdir().expect("large-probe SQLite fixture directory");
    let fixture_path = populated_current_import_fixture(source.path());
    seed_probe_band(
        &fixture_path,
        1_000,
        LARGE_PROBE_FILE_COUNT,
        LARGE_PROBE_PADDING_BYTES,
    );
    seed_probe_band(
        &fixture_path,
        2_000,
        OVERSIZED_PROBE_FILE_COUNT,
        OVERSIZED_PROBE_PADDING_BYTES,
    );
    let prepared = prepare_sqlite_import(source.path()).expect("prepare large-probe import backup");

    // Each attempt re-proves the whole contract from an empty target, so a
    // deadline costs time and never partial state. Only the WAL writer's own
    // byte refusal, or a voter that stops answering, decides the size contract.
    let mut report = None;
    for _ in 0..REPLICATED_DEADLINE_ATTEMPTS {
        let attempt = match classify_replicated(store.validation_reset_contract_state().await) {
            ReplicatedOutcome::Ready(()) => classify_replicated(
                store
                    .import_sqlite_backup(
                        &prepared.backup_path,
                        &prepared.backup_sha256,
                        prepared.schema_version,
                    )
                    .await,
            ),
            ReplicatedOutcome::Deadline => ReplicatedOutcome::Deadline,
            ReplicatedOutcome::Fault(error) => {
                panic!("reset replicated large-probe import target: {error}")
            }
        };
        match attempt {
            ReplicatedOutcome::Ready(imported) => {
                report = Some(imported);
                break;
            }
            ReplicatedOutcome::Fault(error) => panic!("{}", wal_size_diagnosis(&error)),
            ReplicatedOutcome::Deadline => {
                if let ReplicatedOutcome::Fault(probe) = classify_replicated(store.ping().await) {
                    panic!("{}", unreachable_voter_diagnosis(&probe));
                }
            }
        }
    }
    let report = report.unwrap_or_else(|| {
        panic!(
            "{}",
            replicated_deadline_diagnosis("large-probe import", REPLICATED_DEADLINE_ATTEMPTS)
        )
    });
    assert_eq!(
        report
            .tables
            .iter()
            .find(|digest| digest.table == "files")
            .expect("files digest")
            .row_count,
        (LARGE_PROBE_FILE_COUNT + OVERSIZED_PROBE_FILE_COUNT) as u64 + 1,
        "exact file parity must include the existing fixture row and both probe bands"
    );

    // `probe_json` is durable media metadata: splitting a transaction may not
    // drop, truncate, or rewrite a byte of it.
    for (first_id, count, padding) in [
        (1_000, LARGE_PROBE_FILE_COUNT, LARGE_PROBE_PADDING_BYTES),
        (
            2_000,
            OVERSIZED_PROBE_FILE_COUNT,
            OVERSIZED_PROBE_PADDING_BYTES,
        ),
    ] {
        for ordinal in 0..count {
            let id = first_id + ordinal;
            let stored = replicated_read("read imported large probe", || {
                store.get_file_probe_json(id)
            })
            .await;
            assert_eq!(
                stored.as_deref(),
                Some(large_probe_json(id, padding).as_str()),
                "large probe_json row {id} must retain exact bytes"
            );
        }
    }
}

/// A row no transaction could carry is refused before submission.
///
/// The alternative is #290's production failure: the WAL writer panics on the
/// oversized entry, `plurxd` exits mid-import, and the restart serves
/// unreplicated SQLite while reporting healthy. A refusal keeps the node alive
/// and tells the operator which row to look at, so this asserts both — that the
/// error is actionable, and that the voter still answers afterwards.
#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_probe_row_larger_than_the_wal_is_refused_instead_of_crashing_the_node() {
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let store = open_contract_hiqlite_store(&cluster).await;
    store
        .validation_reset_contract_state()
        .await
        .expect("reset replicated unimportable-probe target");

    let source = tempfile::tempdir().expect("unimportable-probe SQLite fixture directory");
    let fixture_path = populated_current_import_fixture(source.path());
    seed_probe_band(
        &fixture_path,
        UNIMPORTABLE_PROBE_FILE_ID,
        1,
        UNIMPORTABLE_PROBE_PADDING_BYTES,
    );
    let prepared =
        prepare_sqlite_import(source.path()).expect("prepare unimportable-probe import backup");

    let refusal = store
        .import_sqlite_backup(
            &prepared.backup_path,
            &prepared.backup_sha256,
            prepared.schema_version,
        )
        .await
        .expect_err("a row larger than the WAL payload capacity must not be submitted")
        .to_string();
    assert!(
        refusal.contains("files")
            && refusal.contains(&format!("row id={UNIMPORTABLE_PROBE_FILE_ID}")),
        "the refusal must name the table and the row an operator has to fix: {refusal}"
    );
    assert!(
        refusal.contains(&CONTRACT_IMPORT_MAX_ROW_BYTES.to_string()),
        "the refusal must state the limit the row exceeded: {refusal}"
    );
    assert!(
        !refusal.contains("xxxx"),
        "a refusal must not quote the imported payload: {refusal}"
    );

    assert_eq!(
        store
            .get_file_probe_json(UNIMPORTABLE_PROBE_FILE_ID)
            .await
            .expect("query the voter after a refused import"),
        None,
        "the refused row must not be in replicated state, and the voter must still answer"
    );
}

/// A legacy backup whose Trakt row is still cleartext must be refused, and
/// refused before anything at all is committed to Raft.
///
/// This is the one production path that does not go through a durable writer:
/// import copies source columns straight into the target, so the write-time
/// `persistable_credential` gate never sees them. Without the source audit a
/// pre-encryption backup would fan a usable bearer token out to every voter,
/// into committed log entries that deleting the row cannot reach.
///
/// The second half is the part that gives this teeth. Asserting only that the
/// import errored would still pass if the refusal happened after the rows were
/// submitted, which is the failure that matters here.
#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cleartext_trakt_row_is_refused_before_any_row_reaches_raft() {
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let store = open_contract_hiqlite_store(&cluster).await;
    store
        .validation_reset_contract_state()
        .await
        .expect("reset replicated cleartext-import target");

    let source = tempfile::tempdir().expect("cleartext import fixture directory");
    let fixture_path = populated_current_import_fixture(source.path());
    make_trakt_fixture_row_cleartext(&fixture_path);
    let prepared = prepare_sqlite_import(source.path()).expect("prepare cleartext import backup");

    let refusal = store
        .import_sqlite_backup(
            &prepared.backup_path,
            &prepared.backup_sha256,
            prepared.schema_version,
        )
        .await
        .expect_err("a cleartext Trakt row must not be importable into replicated state");
    let refusal = refusal.to_string();
    assert!(
        refusal.contains("trakt_auth") && refusal.contains("not sealed"),
        "refusal must name the table and the reason: {refusal}"
    );
    assert!(
        !refusal.contains(FIXTURE_TRAKT_ACCESS) && !refusal.contains(FIXTURE_TRAKT_REFRESH),
        "a refusal must not quote the credential it refused"
    );

    assert!(
        store
            .list_trakt_auth()
            .await
            .expect("read replicated Trakt rows after refusal")
            .is_empty(),
        "the refused cleartext credential must not be in replicated state"
    );
    assert!(
        store
            .list_libraries()
            .await
            .expect("read replicated libraries after refusal")
            .is_empty(),
        "refusal must precede the first table import, not clean up after it"
    );

    // Sealing the source is the documented remedy, and it must actually work:
    // an operator who boots this build on the SQLite install gets rows this
    // accepts, without having to reconnect the account.
    seed_sealed_trakt_fixture_row(
        &rusqlite::Connection::open(&fixture_path).expect("reopen fixture to seal its Trakt row"),
        &fixture_credential_key(),
    );
    let resealed = prepare_sqlite_import(source.path()).expect("prepare resealed import backup");
    store
        .import_sqlite_backup(
            &resealed.backup_path,
            &resealed.backup_sha256,
            resealed.schema_version,
        )
        .await
        .expect("a sealed source must import");

    let imported = store
        .get_trakt_auth(FIXTURE_TRAKT_USER)
        .await
        .expect("read imported Trakt row")
        .expect("imported Trakt row");
    assert!(
        imported.access_token.is_wrapped() && imported.refresh_token.is_wrapped(),
        "the replicated row must hold envelopes"
    );
    assert_eq!(
        fixture_credential_key()
            .open_trakt(FIXTURE_TRAKT_USER, &imported.access_token)
            .expect("open imported access token")
            .expose(),
        FIXTURE_TRAKT_ACCESS,
        "import must preserve a working credential, not just an opaque string"
    );
}

#[cfg(feature = "hiqlite-contract-tests")]
fn one_voter_config(data_dir: &std::path::Path) -> Config {
    let mut config = Config::default();
    let mut ports = contract_free_ports(2).into_iter();
    config.storage.data_dir = data_dir.to_owned();
    config.cluster.raft_bind = format!(
        "0.0.0.0:{}",
        ports.next().expect("reserved one-voter Raft port")
    )
    .parse()
    .expect("raft bind");
    config.cluster.api_bind = format!(
        "0.0.0.0:{}",
        ports.next().expect("reserved one-voter API port")
    )
    .parse()
    .expect("api bind");
    config.cluster.advertise_host = "127.0.0.1".to_owned();
    config
}

#[cfg(feature = "hiqlite-contract-tests")]
#[derive(Clone, Debug, Serialize, Deserialize)]
struct ActivationNodeLaunch {
    data_dir: PathBuf,
    raft_bind: String,
    api_bind: String,
    source_version: i64,
    first_boot: bool,
}

#[cfg(feature = "hiqlite-contract-tests")]
impl ActivationNodeLaunch {
    fn config(&self) -> Config {
        let mut config = Config::default();
        config.storage.data_dir = self.data_dir.clone();
        config.cluster.raft_bind = self.raft_bind.parse().expect("raft bind");
        config.cluster.api_bind = self.api_bind.parse().expect("api bind");
        config.cluster.advertise_host = "127.0.0.1".to_owned();
        config
    }
}

#[cfg(feature = "hiqlite-contract-tests")]
struct ActivationNodeProcess {
    child: Child,
    input: ChildStdin,
    _output: ChildStdout,
}

#[cfg(feature = "hiqlite-contract-tests")]
impl ActivationNodeProcess {
    fn start(launch: &ActivationNodeLaunch) -> Self {
        let executable = std::env::current_exe().expect("activation test executable");
        let mut child = Command::new(executable)
            .arg("hiqlite_activation_node_process")
            .arg("--ignored")
            .arg("--exact")
            .arg("--nocapture")
            .env(
                "PLURX_ACTIVATION_NODE_LAUNCH",
                serde_json::to_string(launch).expect("serialize activation launch"),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn activation voter");
        let input = child.stdin.take().expect("activation voter stdin");
        let output = child.stdout.take().expect("activation voter stdout");
        let mut reader = BufReader::new(output);
        let mut line = String::new();
        loop {
            line.clear();
            let bytes = reader
                .read_line(&mut line)
                .expect("read activation voter startup");
            assert!(bytes > 0, "activation voter exited before ready");
            if line.trim() == "PLURX_ACTIVATION_NODE_READY" {
                break;
            }
        }
        Self {
            child,
            input,
            _output: reader.into_inner(),
        }
    }

    fn stop(self) {
        drop(self.input);
        let status = self
            .child
            .wait_with_output()
            .expect("wait activation voter");
        assert!(
            status.status.success(),
            "activation voter failed: {status:?}"
        );
    }
}

#[cfg(feature = "hiqlite-contract-tests")]
fn run_injected_activation(launch: &ActivationNodeLaunch, failpoint: &str) {
    let executable = std::env::current_exe().expect("activation test executable");
    let output = Command::new(executable)
        .arg("hiqlite_activation_node_process")
        .arg("--ignored")
        .arg("--exact")
        .arg("--nocapture")
        .env(
            "PLURX_ACTIVATION_NODE_LAUNCH",
            serde_json::to_string(launch).expect("serialize activation launch"),
        )
        .env("PLURX_CLUSTER_ACTIVATION_FAILPOINT", failpoint)
        .output()
        .expect("run injected activation voter");
    assert_eq!(output.status.code(), Some(86), "injected process status");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("rollback command: plurxd run"),
        "injected failure must name rollback command: {stderr}"
    );
}

#[cfg(feature = "hiqlite-contract-tests")]
async fn assert_one_voter_activation(data_dir: &std::path::Path, source_version: i64) {
    // The raw importer fixture is pre-sealed to test that boundary in
    // isolation. A real legacy activation starts before credential encryption,
    // so put this source back in that shape and require the coordinator to run
    // the ordinary one-time sealing upgrade before it publishes the backup.
    make_trakt_fixture_row_cleartext(&data_dir.join("plurx.db"));
    let config = one_voter_config(data_dir);
    let launch = ActivationNodeLaunch {
        data_dir: data_dir.to_owned(),
        raft_bind: config.cluster.raft_bind.to_string(),
        api_bind: config.cluster.api_bind.to_string(),
        source_version,
        first_boot: true,
    };
    let first = ActivationNodeProcess::start(&launch);

    let active = data_dir.join(HIQLITE_ACTIVE_DIRNAME);
    let marker: ActivationMarker = serde_json::from_slice(
        &std::fs::read(active.join(ACTIVATION_MARKER_FILENAME)).expect("activation marker"),
    )
    .expect("decode activation marker");
    assert_eq!(marker.cluster_id, CONTRACT_INSTANCE_ID);
    assert_eq!(marker.source_schema_version, source_version);
    assert!(!marker.table_hashes.is_empty());
    assert!(data_dir.join("plurx.db").exists(), "legacy source retained");
    #[cfg(unix)]
    for private in [
        data_dir.join("secret_raft"),
        data_dir.join("secret_api"),
        active.join(ACTIVATION_MARKER_FILENAME),
    ] {
        let mode = std::fs::metadata(&private)
            .unwrap_or_else(|error| panic!("metadata for {}: {error}", private.display()))
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode & 0o077, 0, "{} must be owner-only", private.display());
    }
    assert!(
        std::fs::read_dir(data_dir.join("migration"))
            .expect("migration backups")
            .any(|entry| entry
                .expect("migration entry")
                .path()
                .extension()
                .is_some_and(|ext| ext == "db")),
        "immutable SQLite backup retained"
    );

    let second = connect_activated_store(&config)
        .await
        .expect("second replicated reader");
    assert_eq!(
        second
            .watch_state(7, 10)
            .await
            .expect("read watch state through second client")
            .expect("replicated watch state")
            .position_ms,
        321_000
    );
    drop(second);
    first.stop();

    let reopened = ActivationNodeProcess::start(&ActivationNodeLaunch {
        first_boot: false,
        ..launch
    });
    let second = connect_activated_store(&config)
        .await
        .expect("second reader after subsequent run");
    assert_eq!(
        second
            .watch_state(7, 10)
            .await
            .expect("read reopened watch state")
            .expect("reopened watch state")
            .position_ms,
        321_000
    );
    drop(second);
    reopened.stop();
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn populated_v14_and_current_sources_activate_once_and_reopen_replicated() {
    let _case = HIQLITE_CASE.lock().await;
    install_contract_crypto_provider();

    let v14 = tempfile::tempdir().expect("v14 activation fixture");
    populated_v14_import_fixture(v14.path());
    assert_one_voter_activation(v14.path(), plurx_core::store::SQLITE_SCHEMA_VERSION).await;

    let current = tempfile::tempdir().expect("current activation fixture");
    populated_current_import_fixture(current.path());
    assert_one_voter_activation(current.path(), plurx_core::store::SQLITE_SCHEMA_VERSION).await;
}

/// A direct pre-encryption upgrade seals Trakt before publishing its backup.
///
/// The importer correctly refuses cleartext, but that refusal would turn every
/// linked legacy install into a dead end unless the activation coordinator ran
/// the ordinary SQLite credential upgrade first. This covers both halves: a
/// key-resolution refusal leaves no incoming target, then the same directory
/// activates and its replicated envelope still opens under the node-local key.
#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_upgrade_seals_legacy_trakt_before_any_import_state_exists() {
    let _case = HIQLITE_CASE.lock().await;
    install_contract_crypto_provider();

    let fixture = tempfile::tempdir().expect("legacy Trakt activation fixture");
    let source_path = populated_current_import_fixture(fixture.path());
    make_trakt_fixture_row_cleartext(&source_path);
    let config = one_voter_config(fixture.path());

    // A directory at the configured key path is an explicit key-resolution
    // failure. It occurs before the coordinator writes its attempt marker or
    // creates the incoming target, so the legacy source remains recoverable.
    let key_path = config.cluster.credential_key_path(&config.storage.data_dir);
    std::fs::create_dir(&key_path).expect("block credential-key loading");
    let refusal = select_daemon_store(&config)
        .await
        .err()
        .expect("an unusable credential-key path must refuse activation")
        .to_string();
    assert!(refusal.contains("credential key"), "{refusal}");
    assert!(
        !refusal.contains(FIXTURE_TRAKT_ACCESS) && !refusal.contains(FIXTURE_TRAKT_REFRESH),
        "the refusal must not quote either bearer credential: {refusal}"
    );
    assert!(
        !fixture.path().join("hiqlite.incoming").exists(),
        "a pre-import refusal must leave no partial incoming target"
    );
    assert!(
        !fixture.path().join(HIQLITE_ACTIVE_DIRNAME).exists(),
        "a pre-import refusal must not publish an active target"
    );
    std::fs::remove_dir(&key_path).expect("unblock credential-key loading");

    let selected = select_daemon_store(&config)
        .await
        .expect("direct legacy upgrade must activate after key recovery");
    assert_eq!(selected.backend, SelectedBackend::Replicated);
    let imported = selected
        .store
        .get_trakt_auth(FIXTURE_TRAKT_USER)
        .await
        .expect("read imported Trakt row")
        .expect("linked Trakt row survives activation");
    assert!(
        imported.access_token.is_wrapped() && imported.refresh_token.is_wrapped(),
        "replicated durable state must contain envelopes"
    );
    assert_eq!(
        selected
            .credential_key
            .open_trakt(FIXTURE_TRAKT_USER, &imported.access_token)
            .expect("open imported access token")
            .expose(),
        FIXTURE_TRAKT_ACCESS
    );
    assert_eq!(
        selected
            .credential_key
            .open_trakt(FIXTURE_TRAKT_USER, &imported.refresh_token)
            .expect("open imported refresh token")
            .expose(),
        FIXTURE_TRAKT_REFRESH
    );

    let (source_access, source_refresh): (String, String) =
        rusqlite::Connection::open(&source_path)
            .expect("reopen sealed rollback source")
            .query_row(
                "SELECT access_token, refresh_token FROM trakt_auth WHERE user_id = ?1",
                [FIXTURE_TRAKT_USER],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("read sealed rollback row");
    for (label, stored, expected) in [
        ("access", source_access, FIXTURE_TRAKT_ACCESS),
        ("refresh", source_refresh, FIXTURE_TRAKT_REFRESH),
    ] {
        assert_ne!(stored, expected, "{label} token remained cleartext");
        assert!(
            stored.starts_with("plxenc:v1:"),
            "{label} token is not a v1 envelope"
        );
    }
    selected.shutdown().await.expect("stop activated voter");
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn killed_activation_boundaries_recover_sqlite_or_completed_target() {
    let _case = HIQLITE_CASE.lock().await;
    install_contract_crypto_provider();

    for failpoint in ["after-quiescence", "after-incoming", "after-marker"] {
        let fixture = tempfile::tempdir().expect("interrupted activation fixture");
        let source = populated_current_import_fixture(fixture.path());
        make_trakt_fixture_row_cleartext(&source);
        let config = one_voter_config(fixture.path());
        let launch = ActivationNodeLaunch {
            data_dir: fixture.path().to_owned(),
            raft_bind: config.cluster.raft_bind.to_string(),
            api_bind: config.cluster.api_bind.to_string(),
            source_version: plurx_core::store::SQLITE_SCHEMA_VERSION,
            first_boot: true,
        };
        run_injected_activation(&launch, failpoint);

        let recovered = select_daemon_store(&config)
            .await
            .unwrap_or_else(|error| panic!("recover {failpoint}: {error}"));
        assert_eq!(recovered.backend, SelectedBackend::SqliteRecovery);
        assert_eq!(
            recovered
                .store
                .watch_state(7, 10)
                .await
                .expect("read unchanged SQLite watch state")
                .expect("SQLite watch state")
                .position_ms,
            120_000
        );
        assert!(!fixture.path().join("hiqlite.incoming").exists());
        assert!(!fixture.path().join("hiqlite").exists());
    }

    let fixture = tempfile::tempdir().expect("post-rename activation fixture");
    let source = populated_current_import_fixture(fixture.path());
    make_trakt_fixture_row_cleartext(&source);
    let config = one_voter_config(fixture.path());
    let launch = ActivationNodeLaunch {
        data_dir: fixture.path().to_owned(),
        raft_bind: config.cluster.raft_bind.to_string(),
        api_bind: config.cluster.api_bind.to_string(),
        source_version: plurx_core::store::SQLITE_SCHEMA_VERSION,
        first_boot: true,
    };
    run_injected_activation(&launch, "after-rename");
    assert!(fixture.path().join(HIQLITE_ACTIVE_DIRNAME).is_dir());
    assert!(!fixture.path().join("hiqlite.incoming").exists());

    let resumed = ActivationNodeProcess::start(&launch);
    let second = connect_activated_store(&config)
        .await
        .expect("read completed target after rename crash");
    assert_eq!(second.count_users().await.expect("user count"), 1);
    drop(second);
    resumed.stop();
}

/// An ambiguous active target must fail closed, never fall back to SQLite.
///
/// This is the property whose regression is worst: silently preferring the
/// retained source would boot the daemon on pre-activation state while the
/// replicated target sat right there. Every case here is reached before a voter
/// starts, so the refusal is the only thing under test.
#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_ambiguous_active_target_refuses_rather_than_reverting_to_sqlite() {
    let _case = HIQLITE_CASE.lock().await;
    install_contract_crypto_provider();

    let valid_marker = |data_dir: &std::path::Path| -> ActivationMarker {
        let prepared = prepare_sqlite_import(data_dir).expect("prepare source");
        ActivationMarker {
            marker_version: 1,
            cluster_id: prepared.cluster_id.clone(),
            source_backup_sha256: prepared.backup_sha256.clone(),
            source_schema_version: prepared.schema_version,
            replicated_schema_version: AUTH_SCHEMA_VERSION,
            imported_rows: 1,
            table_hashes: Vec::new(),
            admitted_role: None,
        }
    };

    /// How one case makes the target ambiguous.
    type Corrupt = Box<dyn Fn(&std::path::Path)>;

    // Each case: how the target is made ambiguous, and what the operator reads.
    let cases: Vec<(&str, Corrupt)> = vec![
        (
            "activation marker",
            Box::new(|data_dir: &std::path::Path| {
                let active = data_dir.join(HIQLITE_ACTIVE_DIRNAME);
                std::fs::create_dir_all(&active).expect("active target");
                std::fs::write(active.join(ACTIVATION_MARKER_FILENAME), b"{ not json")
                    .expect("corrupt marker");
            }),
        ),
        (
            ACTIVATION_MARKER_FILENAME,
            Box::new(|data_dir: &std::path::Path| {
                std::fs::create_dir_all(data_dir.join(HIQLITE_ACTIVE_DIRNAME))
                    .expect("active target without a marker");
            }),
        ),
        (
            "Hiqlite activation marker",
            Box::new(move |data_dir: &std::path::Path| {
                let active = data_dir.join(HIQLITE_ACTIVE_DIRNAME);
                std::fs::create_dir_all(&active).expect("active target");
                let mut marker = valid_marker(data_dir);
                // Structurally sound, semantically impossible.
                marker.marker_version = 99;
                std::fs::write(
                    active.join(ACTIVATION_MARKER_FILENAME),
                    serde_json::to_vec(&marker).expect("serialize marker"),
                )
                .expect("write marker");
            }),
        ),
        (
            "directory",
            Box::new(|data_dir: &std::path::Path| {
                std::fs::write(data_dir.join(HIQLITE_ACTIVE_DIRNAME), b"not a target")
                    .expect("active path that is not a directory");
            }),
        ),
    ];

    for (expected, corrupt) in cases {
        let fixture = tempfile::tempdir().expect("ambiguous target fixture");
        populated_current_import_fixture(fixture.path());
        corrupt(fixture.path());

        let error = select_daemon_store(&one_voter_config(fixture.path()))
            .await
            .err()
            .expect("an ambiguous active target must not open");
        let error = error.to_string();
        assert!(
            error.contains(expected),
            "refusal must name what is wrong ({expected}): {error}"
        );
        assert!(
            !fixture.path().join("hiqlite.incoming").exists(),
            "a refusal must not stage a new import: {error}"
        );
    }
}

/// Losing the replicated target must not silently re-import the stale source.
///
/// After activation `plurx.db` is a rollback source, not a current one. A data
/// directory that lost `hiqlite/` is byte-indistinguishable from one that never
/// activated, so without the breadcrumb the next boot would quietly discard
/// every write since activation — the failure an operator would never see.
#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lost_replicated_target_refuses_to_reimport_the_retained_source() {
    let _case = HIQLITE_CASE.lock().await;
    install_contract_crypto_provider();

    let fixture = tempfile::tempdir().expect("activated source fixture");
    let source = populated_current_import_fixture(fixture.path());
    make_trakt_fixture_row_cleartext(&source);
    let config = one_voter_config(fixture.path());
    let launch = ActivationNodeLaunch {
        data_dir: fixture.path().to_owned(),
        raft_bind: config.cluster.raft_bind.to_string(),
        api_bind: config.cluster.api_bind.to_string(),
        source_version: plurx_core::store::SQLITE_SCHEMA_VERSION,
        first_boot: true,
    };
    ActivationNodeProcess::start(&launch).stop();

    let breadcrumb = fixture.path().join(ACTIVATED_SOURCE_FILENAME);
    assert!(
        breadcrumb.is_file(),
        "activation must record that this source handed over authority"
    );
    std::fs::remove_dir_all(fixture.path().join(HIQLITE_ACTIVE_DIRNAME)).expect("lose the target");

    let error = select_daemon_store(&config)
        .await
        .err()
        .expect("a lost target must not silently re-import")
        .to_string();
    assert!(error.contains("already activated"), "{error}");
    // The refusal is only useful if it names both what it protected and the
    // exact way out; an operator who cannot act on it will delete something.
    assert!(error.contains("plurx.db"), "{error}");
    assert!(
        error.contains(&breadcrumb.display().to_string()),
        "refusal must name the file to delete to accept the rollback: {error}"
    );
    assert!(
        !fixture.path().join("hiqlite.incoming").exists(),
        "the refusal must not stage an import"
    );

    // And the documented way out actually works: with the breadcrumb gone the
    // directory activates again rather than staying permanently wedged. The
    // failpoint stops that attempt as soon as it proves the guard is passed.
    std::fs::remove_file(&breadcrumb).expect("accept the rollback");
    run_injected_activation(&launch, "after-quiescence");
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "spawned by the one-voter activation contract"]
async fn hiqlite_activation_node_process() {
    install_contract_crypto_provider();
    let launch: ActivationNodeLaunch = serde_json::from_str(
        &std::env::var("PLURX_ACTIVATION_NODE_LAUNCH").expect("activation launch"),
    )
    .expect("decode activation launch");
    let selected = select_daemon_store(&launch.config())
        .await
        .expect("select one-voter store");
    assert_eq!(selected.backend, SelectedBackend::Replicated);
    assert_eq!(selected.identity.cluster_id, CONTRACT_INSTANCE_ID);
    assert_eq!(selected.store.count_users().await.expect("user count"), 1);
    if launch.first_boot {
        selected
            .store
            .put_progress(7, 10, 321_000, Some(3_600_000))
            .await
            .expect("write watch progress through primary client");
    } else {
        assert_eq!(
            selected
                .store
                .watch_state(7, 10)
                .await
                .expect("read reopened watch state")
                .expect("reopened watch state")
                .position_ms,
            321_000
        );
    }
    let marker: ActivationMarker = serde_json::from_slice(
        &std::fs::read(
            launch
                .data_dir
                .join(HIQLITE_ACTIVE_DIRNAME)
                .join(ACTIVATION_MARKER_FILENAME),
        )
        .expect("activation marker"),
    )
    .expect("decode marker");
    assert_eq!(marker.source_schema_version, launch.source_version);
    println!("PLURX_ACTIVATION_NODE_READY");
    std::io::stdout()
        .flush()
        .expect("flush activation readiness");
    let mut sink = Vec::new();
    tokio::io::stdin()
        .read_to_end(&mut sink)
        .await
        .expect("wait for activation parent");
    selected.shutdown().await.expect("stop activation voter");
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqlite_import_verification_refusals_have_teeth() {
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let store = open_contract_hiqlite_store(&cluster).await;
    store
        .validation_reset_contract_state()
        .await
        .expect("reset replicated verification target");

    let mismatch = tempfile::tempdir().expect("identity mismatch fixture directory");
    let mismatch_path = populated_current_import_fixture(mismatch.path());
    rusqlite::Connection::open(&mismatch_path)
        .expect("open identity mismatch fixture")
        .execute(
            "UPDATE settings SET value = 'different-cluster' WHERE key = 'instance.id'",
            [],
        )
        .expect("change source identity");
    let mismatch = prepare_sqlite_import(mismatch.path()).expect("prepare identity mismatch");
    let identity_error = store
        .import_sqlite_backup(
            &mismatch.backup_path,
            &mismatch.backup_sha256,
            mismatch.schema_version,
        )
        .await
        .expect_err("identity mismatch must refuse import");
    assert!(matches!(
        identity_error,
        plurx_core::error::StoreError::Identity(_)
    ));

    let too_old = tempfile::tempdir().expect("old-schema fixture directory");
    let too_old_path = populated_current_import_fixture(too_old.path());
    rusqlite::Connection::open(&too_old_path)
        .expect("open old-schema fixture")
        .pragma_update(None, "user_version", 13)
        .expect("mark fixture as schema v13");
    let too_old = prepare_sqlite_import(too_old.path()).expect("prepare old-schema backup");
    let schema_error = store
        .import_sqlite_backup(
            &too_old.backup_path,
            &too_old.backup_sha256,
            too_old.schema_version,
        )
        .await
        .expect_err("schema v13 must refuse clustering import");
    assert!(schema_error
        .to_string()
        .contains("supports SQLite schemas v14"));

    let cycle = tempfile::tempdir().expect("item-cycle fixture directory");
    let cycle_path = populated_current_import_fixture(cycle.path());
    rusqlite::Connection::open(&cycle_path)
        .expect("open item-cycle fixture")
        .execute("UPDATE items SET parent_id = 10 WHERE id = 20", [])
        .expect("create an FK-clean item cycle");
    let cycle = prepare_sqlite_import(cycle.path()).expect("prepare item-cycle backup");
    let cycle_error = store
        .import_sqlite_backup(
            &cycle.backup_path,
            &cycle.backup_sha256,
            cycle.schema_version,
        )
        .await
        .expect_err("an item cycle must refuse import");
    assert!(cycle_error.to_string().contains("parent_id cycle"));

    store
        .validation_reset_contract_state()
        .await
        .expect("discard partial cycle import");
    let parity = tempfile::tempdir().expect("parity-fault fixture directory");
    populated_current_import_fixture(parity.path());
    let parity = prepare_sqlite_import(parity.path()).expect("prepare parity-fault backup");
    let parity_error = store
        .validation_import_sqlite_backup_with_parity_fault(
            &parity.backup_path,
            &parity.backup_sha256,
            parity.schema_version,
        )
        .await
        .expect_err("target corruption must fail parity");
    assert!(parity_error
        .to_string()
        .contains("table settings failed SQLite-to-Hiqlite parity"));
}

#[test]
fn contract_inventory_matches_every_store_method() {
    let source = include_str!("../src/store/mod.rs")
        .split_once("pub trait Store:")
        .expect("Store composite boundary")
        .0;
    let declared = source
        .lines()
        .filter_map(|line| line.strip_prefix("    async fn "))
        .filter_map(|line| line.split_once('(').map(|(name, _)| name))
        .collect::<BTreeSet<_>>();
    let covered = [
        SETTINGS_METHODS,
        USER_METHODS,
        LIBRARY_METHODS,
        MEDIA_METHODS,
        WATCH_METHODS,
        READING_METHODS,
        TRAKT_METHODS,
        API_KEY_METHODS,
        OUTBOX_METHODS,
        CACHE_METHODS,
        SHARED_CACHE_METHODS,
        PRETRANSCODE_METHODS,
        OFFLINE_METHODS,
        TELEMETRY_METHODS,
        NETWORK_PRIOR_METHODS,
        FRAGMENT_INDEX_METHODS,
        RENDITION_PLAN_METHODS,
        COORDINATION_METHODS,
        MEDIA_SESSION_METHODS,
        FENCED_PUBLICATION_METHODS,
        METRICS_METHODS,
    ]
    .into_iter()
    .flatten()
    .copied()
    .collect::<BTreeSet<_>>();

    assert_eq!(declared.len(), 232, "review the Store method count");
    assert_eq!(
        covered, declared,
        "the declared async method name inventory changed"
    );
}

#[tokio::test]
async fn rendition_plan_contract_runs_through_dyn_store() {
    for_each_backend(|store, backend| async move {
        let identity = SourceIdentity::new(4_096, 1_700_000_000_000, "fingerprint");
        let entry = |index: u32, start: u64, kind: PlanEntryKind, cut: PlanCut| PlanEntry {
            index,
            kind,
            start_ticks: start,
            duration_ticks: 112_128,
            est_bytes: 48_000_000 + u64::from(index),
            cut,
            fragments: if kind == PlanEntryKind::Video { 4 } else { 0 },
        };
        let plan = SegmentPlan {
            version: SEGPLAN_VERSION,
            timescale: 16_000,
            entries: vec![
                entry(0, 0, PlanEntryKind::Video, PlanCut::Clean),
                entry(1, 112_128, PlanEntryKind::Video, PlanCut::ByteCeiling),
                entry(2, 224_256, PlanEntryKind::AudioTail, PlanCut::EndOfStream),
            ],
            target_duration: 15,
        };

        assert_eq!(
            store
                .rendition_plan("rk", &identity)
                .await
                .unwrap_or_else(|error| panic!("{backend}: read a missing plan: {error}")),
            None,
            "backend {backend}"
        );

        assert!(
            store
                .put_rendition_plan("rk", 42, &plan, &identity)
                .await
                .unwrap_or_else(|error| panic!("{backend}: store a plan: {error}")),
            "backend {backend}: storing the first plan under a key is the write"
        );
        let read = store
            .rendition_plan("rk", &identity)
            .await
            .unwrap_or_else(|error| panic!("{backend}: read the plan: {error}"))
            .unwrap_or_else(|| panic!("{backend}: the plan it just stored"));
        assert_eq!(read, plan, "backend {backend}");

        // A plan is a decision a client already holds a playlist for, so a
        // second one under the same key is refused rather than applied. A
        // backend that upserted here would re-cut a rendition under the index
        // a viewer is mid-seek against.
        let mut recut = plan.clone();
        recut.entries[0].duration_ticks = 999;
        assert!(
            !store
                .put_rendition_plan("rk", 42, &recut, &identity)
                .await
                .unwrap_or_else(|error| panic!("{backend}: store a second plan: {error}")),
            "backend {backend}: a second plan under one key must not be stored"
        );
        assert_eq!(
            store
                .rendition_plan("rk", &identity)
                .await
                .unwrap_or_else(|error| panic!("{backend}: re-read: {error}"))
                .unwrap_or_else(|| panic!("{backend}: still stored")),
            plan,
            "backend {backend}: the first plan is still the plan"
        );

        // Invalidation by mismatch, the same discipline the index keeps.
        let moved_on = SourceIdentity::new(8_192, 1_700_000_000_000, "fingerprint");
        assert_eq!(
            store
                .rendition_plan("rk", &moved_on)
                .await
                .unwrap_or_else(|error| panic!("{backend}: read a resized source: {error}")),
            None,
            "backend {backend}"
        );
        let repiped = SourceIdentity::new(4_096, 1_700_000_000_000, "other-pipeline");
        assert_eq!(
            store
                .rendition_plan("rk", &repiped)
                .await
                .unwrap_or_else(|error| panic!("{backend}: read a repiped source: {error}")),
            None,
            "backend {backend}"
        );

        // One file carries several renditions; forgetting the file takes all
        // of them and leaves another file's alone.
        store
            .put_rendition_plan("rk-720", 42, &plan, &identity)
            .await
            .unwrap_or_else(|error| panic!("{backend}: store a second rendition: {error}"));
        store
            .put_rendition_plan("rk-other", 43, &plan, &identity)
            .await
            .unwrap_or_else(|error| panic!("{backend}: store another file's: {error}"));
        assert_eq!(
            store
                .forget_rendition_plans(42)
                .await
                .unwrap_or_else(|error| panic!("{backend}: forget the file's plans: {error}")),
            2,
            "backend {backend}"
        );
        assert!(
            store
                .rendition_plan("rk-other", &identity)
                .await
                .unwrap_or_else(|error| panic!("{backend}: another file's plan: {error}"))
                .is_some(),
            "backend {backend}: forgetting one file must not take another's"
        );
        assert_eq!(
            store
                .forget_rendition_plans(42)
                .await
                .unwrap_or_else(|error| panic!("{backend}: forget twice: {error}")),
            0,
            "backend {backend}"
        );

        // The sweep's two queries. Both backends must answer the same way, and
        // they reach the answer differently: SQLite reads one database, while
        // hiqlite asks its own sidecar what it holds and the replicated table
        // which of those still exist. That split is the whole reason the sweep
        // is per-node rather than a hook inside `delete_files`.
        let held = store
            .vod_row_file_ids(512)
            .await
            .unwrap_or_else(|error| panic!("{backend}: list held rows: {error}"));
        assert!(
            held.contains(&43),
            "backend {backend}: the surviving file's plan must still be listed, got {held:?}"
        );
        assert!(
            held.windows(2).all(|pair| pair[0] < pair[1]),
            "backend {backend}: ordered so consecutive sweeps make progress"
        );

        // Neither file id was ever inserted into `files`, so both are orphans
        // — which is exactly what the sweep is looking for.
        let alive = store
            .surviving_file_ids(&held)
            .await
            .unwrap_or_else(|error| panic!("{backend}: check survivors: {error}"));
        assert!(
            alive.is_empty(),
            "backend {backend}: no rows in `files`, so nothing survives, got {alive:?}"
        );
    })
    .await;
}

#[tokio::test]
async fn fragment_index_contract_runs_through_dyn_store() {
    for_each_backend(|store, backend| async move {
        let identity = SourceIdentity::new(4_096, 1_700_000_000_000, "fingerprint");
        let index = FragmentIndex::new(
            16_000,
            vec![
                IndexRow {
                    dts: 0,
                    duration: 28_016,
                    bytes: 104_452,
                    video_bytes: 103_836,
                    class: CutClass::CleanIdr,
                },
                IndexRow {
                    dts: 28_016,
                    duration: 28_032,
                    bytes: 110_038,
                    video_bytes: 109_422,
                    class: CutClass::Dirty,
                },
            ],
            "abc123",
            identity.clone(),
        );

        assert_eq!(
            store
                .fragment_index(42, &identity)
                .await
                .unwrap_or_else(|error| panic!("{backend}: read a missing index: {error}")),
            None,
            "backend {backend}"
        );

        store
            .put_fragment_index(42, &index)
            .await
            .unwrap_or_else(|error| panic!("{backend}: store an index: {error}"));
        let read = store
            .fragment_index(42, &identity)
            .await
            .unwrap_or_else(|error| panic!("{backend}: read the index: {error}"))
            .unwrap_or_else(|| panic!("{backend}: the index it just stored"));
        assert_eq!(read, index, "backend {backend}");

        // Invalidation is by mismatch, never by deletion: a changed file or a
        // changed video pipeline simply stops matching. A backend that ignored
        // either half would serve byte counts describing a stream that is no
        // longer produced, and the landing matcher compares exactly those.
        let moved_on = SourceIdentity::new(8_192, 1_700_000_000_000, "fingerprint");
        assert_eq!(
            store
                .fragment_index(42, &moved_on)
                .await
                .unwrap_or_else(|error| panic!("{backend}: read a resized source: {error}")),
            None,
            "backend {backend}"
        );
        let repiped = SourceIdentity::new(4_096, 1_700_000_000_000, "other-pipeline");
        assert_eq!(
            store
                .fragment_index(42, &repiped)
                .await
                .unwrap_or_else(|error| panic!("{backend}: read a repiped source: {error}")),
            None,
            "backend {backend}"
        );

        assert!(
            store
                .forget_fragment_index(42)
                .await
                .unwrap_or_else(|error| panic!("{backend}: forget the index: {error}")),
            "backend {backend}"
        );
        assert!(
            !store
                .forget_fragment_index(42)
                .await
                .unwrap_or_else(|error| panic!("{backend}: forget it twice: {error}")),
            "backend {backend}"
        );
    })
    .await;
}

#[tokio::test]
async fn playback_telemetry_contract_runs_through_dyn_store() {
    for_each_backend(|store, backend| async move {
        let first = PlaybackEvent {
            at_unix_ms: 1_700_000_000_000,
            user_id: Some(7),
            session_id: Some("session-a".to_owned()),
            event: "ttff".to_owned(),
            ms: Some(684),
            ..PlaybackEvent::default()
        };
        let second = PlaybackEvent {
            at_unix_ms: 1_700_000_001_000,
            user_id: Some(7),
            session_id: Some("session-a".to_owned()),
            event: "stall".to_owned(),
            ms: Some(4584),
            ..PlaybackEvent::default()
        };
        let first_id = store
            .record_playback_event(&first)
            .await
            .expect("record first telemetry row");
        let second_id = store
            .record_playback_event(&second)
            .await
            .expect("record second telemetry row");
        assert!(first_id > 0 && second_id > first_id, "backend {backend}");

        let ttff = store
            .playback_events(&PlaybackEventQuery {
                event: Some("ttff".to_owned()),
                limit: 10,
                ..PlaybackEventQuery::default()
            })
            .await
            .expect("query telemetry by event");
        assert_eq!(ttff.len(), 1, "backend {backend}");
        assert_eq!(ttff[0].id, first_id, "backend {backend}");

        let newest = store
            .playback_events(&PlaybackEventQuery {
                since_ms: Some(first.at_unix_ms),
                limit: 1,
                ..PlaybackEventQuery::default()
            })
            .await
            .expect("query newest telemetry row");
        assert_eq!(newest.len(), 1, "backend {backend}");
        assert_eq!(newest[0].id, second_id, "backend {backend}");

        assert_eq!(
            store
                .prune_playback_events(second.at_unix_ms, 1)
                .await
                .expect("bounded telemetry prune"),
            1,
            "backend {backend}"
        );
        let remaining = store
            .playback_events(&PlaybackEventQuery {
                limit: 10,
                ..PlaybackEventQuery::default()
            })
            .await
            .expect("query remaining telemetry");
        assert_eq!(remaining.len(), 1, "backend {backend}");
        assert_eq!(remaining[0].id, second_id, "backend {backend}");
    })
    .await;
}

#[tokio::test]
async fn network_prior_contract_runs_through_dyn_store() {
    for_each_backend(|store, backend| async move {
        let credential_generation = CredentialGeneration::from(format!(
            "store-contract-generation-{}",
            if backend.contains("hiqlite") { 3 } else { 2 }
        ));
        let key = format!(
            "192.0.{}.0/24",
            if backend.contains("hiqlite") { 3 } else { 2 }
        );
        let prior = store
            .observe_network_prior(&NetworkPriorObservation {
                user_id: 42,
                credential_generation: credential_generation.clone(),
                client_class: "chrome".to_owned(),
                network_fingerprint: key.clone(),
                throughput_kbps: Some(8_000),
                starved_rung_height: Some(1080),
                observed_at_ms: 1_700_000_000_000,
            })
            .await
            .unwrap_or_else(|error| panic!("{backend}: observe prior: {error}"));
        assert_eq!(prior.sustained_kbps, Some(8_000), "{backend}");
        assert_eq!(prior.worst_rung_height, Some(1080), "{backend}");
        assert_eq!(
            prior.starved_at_ms,
            Some(1_700_000_000_000),
            "{backend}: the verdict's expiry stamp is part of the durable contract"
        );
        let loaded = store
            .network_prior(credential_generation.as_str(), "chrome", &key)
            .await
            .unwrap_or_else(|error| panic!("{backend}: load prior: {error}"))
            .expect("stored prior");
        assert_eq!(loaded, prior, "{backend}");
        assert_eq!(
            store
                .prune_network_priors(1_700_000_000_001, 1)
                .await
                .unwrap_or_else(|error| panic!("{backend}: prune prior: {error}")),
            1,
            "{backend}"
        );
        assert!(store
            .network_prior(credential_generation.as_str(), "chrome", &key)
            .await
            .expect("post-prune lookup")
            .is_none());
    })
    .await;
}

#[tokio::test]
async fn settings_contract_runs_through_dyn_store() {
    for_each_backend(|store, backend| async move {
        store.ping().await.expect("ping");
        assert_eq!(store.get_setting("contract.key").await.expect("get"), None);
        assert_eq!(
            store
                .get_or_init_setting("contract.seed", "first seed")
                .await
                .expect("seed setting"),
            "first seed",
            "backend {backend}"
        );
        assert_eq!(
            store
                .get_or_init_setting("contract.seed", "discarded seed")
                .await
                .expect("read seeded winner"),
            "first seed",
            "backend {backend}"
        );
        store
            .put_setting("contract.key", "first")
            .await
            .expect("insert setting");
        store
            .put_setting("contract.key", "second")
            .await
            .expect("update setting");
        assert_eq!(
            store.get_setting("contract.key").await.expect("get"),
            Some("second".to_owned()),
            "backend {backend}"
        );
        assert!(
            store
                .put_setting_if_absent("contract.immutable", "first")
                .await
                .expect("insert immutable setting"),
            "backend {backend}"
        );
        assert!(
            !store
                .put_setting_if_absent("contract.immutable", "second")
                .await
                .expect("retain immutable setting"),
            "backend {backend}"
        );
        assert_eq!(
            store
                .get_setting("contract.immutable")
                .await
                .expect("get immutable setting"),
            Some("first".to_owned()),
            "backend {backend}"
        );
        store
            .put_settings(&[("contract.left", "L"), ("contract.right", "R")])
            .await
            .expect("publish related settings");
        assert_eq!(
            store.get_setting("contract.left").await.expect("left"),
            Some("L".to_owned()),
            "backend {backend}"
        );
        assert_eq!(
            store.get_setting("contract.right").await.expect("right"),
            Some("R".to_owned()),
            "backend {backend}"
        );
        assert_eq!(
            store
                .get_setting_pair("contract.left", "contract.right")
                .await
                .expect("pair"),
            (Some("L".to_owned()), Some("R".to_owned())),
            "backend {backend}"
        );
        let settings = store.settings_snapshot().await.expect("settings snapshot");
        assert_eq!(
            settings.get("contract.key").map(String::as_str),
            Some("second")
        );
        assert_eq!(settings.get("contract.left").map(String::as_str), Some("L"));
        assert_eq!(
            settings.get("contract.right").map(String::as_str),
            Some("R")
        );
        let instance_id = store.instance_id().await.expect("instance id");
        uuid::Uuid::parse_str(&instance_id).expect("new instance ids are UUIDs");
        assert_eq!(
            store.instance_id().await.expect("stable instance id"),
            instance_id,
            "backend {backend}"
        );
    })
    .await;
}

/// Replicated Store-primitive gates are operation-count based rather than a
/// timing threshold: CI scheduler noise can move p95 without changing the
/// code, while an accidental per-key or per-package loop deterministically
/// changes a fixed call count into N. This does not invoke the HTTP handlers:
/// the Settings sequence below is the two Store calls used by `settings_dto`
/// with a configured cache, and the Activity sequence is its joined offline
/// package primitive. Authentication and process-local state are outside this
/// boundary. The counters record attempted client API calls, not network RTTs.
#[cfg(feature = "cluster-read-cost-validation")]
#[tokio::test]
async fn clustered_page_read_primitives_have_bounded_client_calls() {
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let store = open_contract_hiqlite_store(&cluster).await;
    store
        .validation_reset_contract_state()
        .await
        .expect("reset replicated contract state");
    let values: Vec<(String, String)> = (0..64)
        .map(|index| (format!("page.setting.{index}"), format!("value-{index}")))
        .collect();
    let borrowed: Vec<(&str, &str)> = values
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    store
        .put_settings(&borrowed)
        .await
        .expect("seed page settings");

    store.validation_reset_operation_counts();
    let snapshot = store.settings_snapshot().await.expect("settings snapshot");
    let cache_bytes = store.cache_bytes("node-a").await.expect("cache bytes");
    let counts = store.validation_operation_counts();

    assert_eq!(
        snapshot
            .iter()
            .filter(|(key, _)| key.starts_with("page.setting."))
            .count(),
        64
    );
    assert_eq!(cache_bytes, 0);
    assert_eq!(
        counts.consistent_query_calls, 2,
        "one settings snapshot plus one cache aggregate"
    );
    assert_eq!(counts.non_consistent_query_calls, 0);
    assert_eq!(
        counts.write_calls, 0,
        "the Settings Store-read sequence must be read-only"
    );

    store.validation_reset_operation_counts();
    let metrics = store
        .prometheus_store_snapshot("node-a", 1)
        .await
        .expect("aggregate Prometheus Store sample");
    let counts = store.validation_operation_counts();
    assert_eq!(metrics.libraries, 0);
    assert_eq!(metrics.users, 0);
    assert_eq!(counts.consistent_query_calls, 1);
    assert_eq!(counts.non_consistent_query_calls, 0);
    assert_eq!(counts.write_calls, 0);

    // Home's catalog primitive is one window query whether the roster has
    // one library or fifty. Mixed Home rows keep every annotation primitive
    // non-vacuous so this also gates the handler-equivalent Store sequence.
    let dynamic_store: Arc<dyn Store> = Arc::new(store.clone());
    let home_user = dynamic_store
        .create_user("Page Home Viewer", "contract-hash", false)
        .await
        .expect("seed Home viewer");
    let mut home_library_count = 0;
    for target in [1, 10, 50] {
        while home_library_count < target {
            let index = home_library_count;
            let library = dynamic_store
                .create_library(&NewLibrary {
                    name: format!("Page Home Library {index}"),
                    kind: LibraryKind::Home,
                    paths: vec![PathBuf::from(format!("/page-home/{index}"))],
                    anime: false,
                })
                .await
                .expect("seed Home library");
            let folder = dynamic_store
                .insert_item(&NewItem {
                    library_id: library.id,
                    kind: ItemKind::Folder,
                    parent_id: None,
                    title: format!("Page Home Folder {index}"),
                    year: None,
                    season_number: None,
                    episode_number: None,
                })
                .await
                .expect("seed Home folder");
            for (parent_id, suffix) in [(None, "root"), (Some(folder), "nested")] {
                dynamic_store
                    .insert_item(&NewItem {
                        library_id: library.id,
                        kind: ItemKind::Video,
                        parent_id,
                        title: format!("Page Home Video {index} {suffix}"),
                        year: None,
                        season_number: None,
                        episode_number: None,
                    })
                    .await
                    .expect("seed Home video");
            }
            home_library_count += 1;
        }

        store.validation_reset_operation_counts();
        let pages = dynamic_store
            .home_preview_pages(24)
            .await
            .expect("Home preview pages");
        let counts = store.validation_operation_counts();
        assert_eq!(pages.len(), target);
        assert!(pages.iter().all(|page| page.items.len() == 2));
        assert_eq!(
            counts.consistent_query_calls, 1,
            "Home preview authority reads grew with {target} libraries"
        );
        assert_eq!(counts.non_consistent_query_calls, 0);
        assert_eq!(counts.write_calls, 0);

        store.validation_reset_operation_counts();
        let libraries = dynamic_store
            .list_libraries()
            .await
            .expect("Home libraries");
        let pages = dynamic_store
            .home_preview_pages(24)
            .await
            .expect("Home preview pages");
        let items = pages
            .iter()
            .flat_map(|page| page.items.iter())
            .collect::<Vec<_>>();
        let item_ids = items.iter().map(|item| item.id).collect::<Vec<_>>();
        let badged = items
            .iter()
            .filter(|item| matches!(item.kind, ItemKind::Movie | ItemKind::Video))
            .map(|item| item.id)
            .collect::<Vec<_>>();
        let folders = items
            .iter()
            .filter(|item| item.kind == ItemKind::Folder)
            .map(|item| item.id)
            .collect::<Vec<_>>();
        let containers = items
            .iter()
            .filter(|item| {
                matches!(
                    item.kind,
                    ItemKind::Show | ItemKind::Season | ItemKind::Folder
                )
            })
            .map(|item| item.id)
            .collect::<Vec<_>>();
        let _ = tokio::try_join!(
            dynamic_store.watch_map(home_user.id, &item_ids),
            dynamic_store.item_max_heights(&badged),
            dynamic_store.child_counts(&folders),
            dynamic_store.watch_rollups(home_user.id, &containers),
        )
        .expect("Home page-wide annotations");
        let counts = store.validation_operation_counts();
        assert_eq!(libraries.len(), target);
        assert_eq!(
            counts.consistent_query_calls, 6,
            "the complete Home Store sequence grew with {target} libraries"
        );
        assert_eq!(counts.non_consistent_query_calls, 0);
        assert_eq!(counts.write_calls, 0);
    }

    // Activity's offline row count must not affect its store-call count. The
    // joined query also carries the fields the HTTP DTO needs, so the handler
    // has no reason to issue per-package user, file, or item lookups.
    let (user_id, file_id) = seed_file(&dynamic_store, "page-activity").await;
    for index in 0..25 {
        let request = offline_request(
            &format!("page-package-{index}"),
            &format!("page-request-{index}"),
            user_id,
            file_id,
        );
        assert!(matches!(
            store
                .create_offline_package(&request, 50, 1_000_000, 1_000_000)
                .await
                .expect("seed activity package"),
            OfflineCreateOutcome::Created(_)
        ));
    }
    store.validation_reset_operation_counts();
    let activity = store
        .offline_activity_packages("offline-node", 1, 0, 50)
        .await
        .expect("activity packages");
    let counts = store.validation_operation_counts();
    assert_eq!(activity.len(), 25);
    assert!(activity.iter().all(|row| row.item_id.is_some()));
    assert!(activity
        .iter()
        .all(|row| row.title == "page-activity Movie"));
    assert_eq!(counts.consistent_query_calls, 1);
    assert_eq!(counts.non_consistent_query_calls, 0);
    assert_eq!(counts.write_calls, 0);

    // Counter non-vacuity: prove each instrumented call family changes only
    // its own field, including one transaction as one attempted write call.
    store.validation_reset_operation_counts();
    let metric_before = store.validation_successful_metric_counts();
    store
        .get_setting("page.setting.0")
        .await
        .expect("single read");
    let counts = store.validation_operation_counts();
    let metric_after = store.validation_successful_metric_counts();
    assert_eq!(counts.consistent_query_calls, 1);
    assert_eq!(counts.non_consistent_query_calls, 0);
    assert_eq!(counts.write_calls, 0);
    assert_eq!(
        metric_after.consistent_query_calls,
        metric_before.consistent_query_calls + 1
    );
    assert_eq!(
        metric_after.non_consistent_query_calls,
        metric_before.non_consistent_query_calls
    );
    assert_eq!(metric_after.write_calls, metric_before.write_calls);

    store.validation_reset_operation_counts();
    let metric_before = store.validation_successful_metric_counts();
    store.validation_local_dump().await.expect("local dump");
    let counts = store.validation_operation_counts();
    let metric_after = store.validation_successful_metric_counts();
    assert_eq!(counts.consistent_query_calls, 0);
    assert!(counts.non_consistent_query_calls > 0);
    assert_eq!(counts.write_calls, 0);
    assert_eq!(
        metric_after.consistent_query_calls,
        metric_before.consistent_query_calls
    );
    assert_eq!(
        metric_after.non_consistent_query_calls,
        metric_before.non_consistent_query_calls + counts.non_consistent_query_calls
    );
    assert_eq!(metric_after.write_calls, metric_before.write_calls);

    store.validation_reset_operation_counts();
    let metric_before = store.validation_successful_metric_counts();
    store
        .put_setting("page.counter.write", "one")
        .await
        .expect("single write");
    let counts = store.validation_operation_counts();
    let metric_after = store.validation_successful_metric_counts();
    assert_eq!(counts.consistent_query_calls, 0);
    assert_eq!(counts.non_consistent_query_calls, 0);
    assert_eq!(counts.write_calls, 1);
    assert_eq!(
        metric_after.consistent_query_calls,
        metric_before.consistent_query_calls
    );
    assert_eq!(
        metric_after.non_consistent_query_calls,
        metric_before.non_consistent_query_calls
    );
    assert_eq!(metric_after.write_calls, metric_before.write_calls + 1);

    store.validation_reset_operation_counts();
    let metric_before = store.validation_successful_metric_counts();
    store
        .put_settings(&[("page.counter.left", "L"), ("page.counter.right", "R")])
        .await
        .expect("transaction write");
    let counts = store.validation_operation_counts();
    let metric_after = store.validation_successful_metric_counts();
    assert_eq!(counts.consistent_query_calls, 0);
    assert_eq!(counts.non_consistent_query_calls, 0);
    assert_eq!(counts.write_calls, 1);
    assert_eq!(
        metric_after.consistent_query_calls,
        metric_before.consistent_query_calls
    );
    assert_eq!(
        metric_after.non_consistent_query_calls,
        metric_before.non_consistent_query_calls
    );
    assert_eq!(metric_after.write_calls, metric_before.write_calls + 1);

    store.validation_reset_operation_counts();
    let success_before = store.validation_successful_metric_counts();
    let failure_before = store.validation_failed_write_metric_count();
    store
        .validation_duplicate_instance_id_transaction()
        .await
        .expect_err("the seeded instance-id uniqueness constraint must reject a duplicate");
    let counts = store.validation_operation_counts();
    let success_after = store.validation_successful_metric_counts();
    let failure_after = store.validation_failed_write_metric_count();
    assert_eq!(counts.write_calls, 1);
    assert_eq!(success_after.write_calls, success_before.write_calls);
    assert_eq!(failure_after, failure_before + 1);
}

/// Every P3b catalogue operation must return the same serialized value through
/// its local and Authority paths against one real replicated fixture. The
/// genre and media-shape cases intentionally cover their existing
/// multi-statement semantics while the fixture is quiescent; bounded reads do
/// not claim a stronger snapshot than Authority.
#[cfg(feature = "cluster-read-cost-validation")]
#[tokio::test]
async fn bounded_catalogue_reader_matches_authority_and_falls_back_exactly_once() {
    use plurx_core::cluster::migration::status::{PassiveRaftMetrics, ReplicationMonitor};

    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let store = Arc::new(open_contract_hiqlite_store(&cluster).await);
    store
        .validation_reset_contract_state()
        .await
        .expect("reset bounded catalogue target");

    let fixture = tempfile::tempdir().expect("bounded catalogue fixture");
    let source = populated_current_import_fixture(fixture.path());
    let connection = rusqlite::Connection::open(&source).expect("open catalogue fixture");
    connection
        .execute_batch(
            "UPDATE files
                SET height = 1080, probe_json = '{\"streams\":[]}'
              WHERE id = 30;
             INSERT INTO items
                (id, library_id, kind, parent_id, title, sort_title,
                 season_number, episode_number, added_at, updated_at, tags, genres)
             VALUES
                (11, 9, 'episode', 10, 'Imported Episode', 'Imported Episode',
                 1, 1, 201, 202, '[]', '[\"Drama\"]');",
        )
        .expect("seed non-vacuous catalogue rows");
    drop(connection);
    let prepared = prepare_sqlite_import(fixture.path()).expect("prepare catalogue fixture");
    store
        .import_sqlite_backup(
            &prepared.backup_path,
            &prepared.backup_sha256,
            prepared.schema_version,
        )
        .await
        .expect("import bounded catalogue fixture");

    let authority_store: Arc<dyn Store> = store.clone();
    let authority = CatalogueReader::authority(Arc::clone(&authority_store));
    assert!(
        !authority
            .recently_added(Some(9), 20)
            .await
            .expect("Authority recent seed")
            .is_empty(),
        "recently-added parity must exercise an eligible row"
    );
    assert_eq!(
        authority
            .item_max_heights(&[10])
            .await
            .expect("Authority height seed")
            .get(&10),
        Some(&1080),
        "height parity must exercise a non-null aggregate"
    );
    assert_eq!(
        authority
            .get_file_probe_json(30)
            .await
            .expect("Authority probe seed")
            .as_deref(),
        Some(r#"{"streams":[]}"#),
        "probe parity must exercise a non-null JSON payload"
    );

    macro_rules! assert_catalogue_parity {
        ($label:literal, $method:ident($($arg:expr),* $(,)?)) => {{
            let expected = serde_json::to_value(
                authority
                    .$method($($arg),*)
                    .await
                    .unwrap_or_else(|error| panic!("Authority {}: {error}", $label)),
            )
            .unwrap_or_else(|error| panic!("serialize Authority {}: {error}", $label));

            store.validation_reset_operation_counts();
            let reader = CatalogueReader::validation_replicated(
                Arc::clone(&authority_store),
                Arc::clone(&store),
                PassiveRaftMetrics::validation_bounded_ready(),
                64,
            );
            let actual = serde_json::to_value(
                reader
                    .$method($($arg),*)
                    .await
                    .unwrap_or_else(|error| panic!("bounded {}: {error}", $label)),
            )
            .unwrap_or_else(|error| panic!("serialize bounded {}: {error}", $label));
            assert_eq!(actual, expected, "{} local/Authority parity", $label);
            let counts = store.validation_operation_counts();
            assert_eq!(
                counts.consistent_query_calls, 0,
                "{} unexpectedly fell back to Authority",
                $label
            );
            assert!(
                counts.non_consistent_query_calls > 0,
                "{} did not exercise its local SQL",
                $label
            );
        }};
    }

    assert_catalogue_parity!("get library", get_library(9));
    assert_catalogue_parity!("list libraries", list_libraries());
    assert_catalogue_parity!("get item", get_item(20));
    assert_catalogue_parity!("item children", get_item_children(20));
    assert_catalogue_parity!(
        "genre page",
        list_top_items_in_genre(9, ItemSort::Title, 0, 20, Some("Drama"))
    );
    assert_catalogue_parity!("home previews", home_preview_pages(8));
    assert_catalogue_parity!("recently added", recently_added(Some(9), 20));
    assert_catalogue_parity!("get file", get_file(30));
    assert_catalogue_parity!("files for item", files_for_item(10));
    assert_catalogue_parity!("child counts", child_counts(&[10, 20]));
    assert_catalogue_parity!("item max heights", item_max_heights(&[10, 20]));
    assert_catalogue_parity!("item media facts", item_media_facts(&[10, 20]));
    assert_catalogue_parity!("media shape", media_shape());
    assert_catalogue_parity!("probe JSON", get_file_probe_json(30));

    let expected = authority
        .get_item(20)
        .await
        .expect("Authority fallback value");

    store.validation_reset_operation_counts();
    store.validation_fail_next_non_consistent_query();
    let query_error_reader = CatalogueReader::validation_replicated(
        Arc::clone(&authority_store),
        Arc::clone(&store),
        PassiveRaftMetrics::validation_bounded_ready(),
        64,
    );
    let actual = query_error_reader
        .get_item(20)
        .await
        .expect("local query error must fall back");
    assert_eq!(
        serde_json::to_value(actual).expect("serialize query-error result"),
        serde_json::to_value(&expected).expect("serialize expected item")
    );
    let counts = store.validation_operation_counts();
    assert_eq!(counts.non_consistent_query_calls, 1);
    assert_eq!(counts.consistent_query_calls, 1);

    store.validation_reset_operation_counts();
    let proof_loss_reader = CatalogueReader::validation_replicated(
        Arc::clone(&authority_store),
        Arc::clone(&store),
        PassiveRaftMetrics::validation_bounded_ready(),
        64,
    );
    proof_loss_reader.validation_revoke_after_next_local();
    let actual = proof_loss_reader
        .get_item(20)
        .await
        .expect("proof loss must discard and fall back");
    assert_eq!(
        serde_json::to_value(actual).expect("serialize proof-loss result"),
        serde_json::to_value(&expected).expect("serialize expected item")
    );
    let counts = store.validation_operation_counts();
    assert_eq!(counts.non_consistent_query_calls, 1);
    assert_eq!(counts.consistent_query_calls, 1);

    store.validation_reset_operation_counts();
    let no_proof_reader = CatalogueReader::validation_replicated(
        authority_store,
        Arc::clone(&store),
        ReplicationMonitor::sqlite().metrics_handle(),
        64,
    );
    no_proof_reader
        .get_item(20)
        .await
        .expect("missing proof must use Authority");
    let counts = store.validation_operation_counts();
    assert_eq!(counts.non_consistent_query_calls, 0);
    assert_eq!(counts.consistent_query_calls, 1);
}

#[cfg(feature = "cluster-read-cost-validation")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replaceable_cache_touch_burst_has_one_physical_write_budget() {
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let observer = Client::remote(
        cluster.addresses.clone(),
        true,
        true,
        CONTRACT_API_SECRET.to_owned(),
        false,
        None,
    )
    .await
    .expect("connect replaceable-touch consensus observer");
    let store = Arc::new(open_contract_hiqlite_store(&cluster).await);
    store
        .validation_reset_contract_state()
        .await
        .expect("reset replaceable-touch state");
    let dynamic: Arc<dyn Store> = store.clone();
    let (_, file_id) = seed_file(&dynamic, "replaceable-touch").await;
    assert!(store
        .claim_cache_entry(
            "replaceable-touch-recipe",
            file_id,
            1,
            "cache-node",
            "re/replaceable-touch-recipe",
        )
        .await
        .expect("seed replaceable cache claim"));

    store.validation_reset_operation_counts();
    let before_burst = contract_leader_point(&observer).await;
    let start = Arc::new(tokio::sync::Barrier::new(81));
    let mut touches = tokio::task::JoinSet::new();
    for _ in 0..80 {
        let store = Arc::clone(&store);
        let start = Arc::clone(&start);
        touches.spawn(async move {
            start.wait().await;
            store
                .touch_cache_claim("replaceable-touch-recipe", "cache-node")
                .await
        });
    }
    start.wait().await;
    while let Some(result) = touches.join_next().await {
        result
            .expect("cache touch task")
            .expect("cache touch result");
    }
    assert_eq!(
        store.validation_operation_counts().write_calls,
        1,
        "80 equal cache touches must submit one physical write"
    );
    let after_burst = contract_leader_point(&observer).await;
    assert_eq!(
        contract_stable_leader_delta(before_burst, after_burst),
        1,
        "80 equal cache touches must commit one Raft entry"
    );

    store.validation_reset_operation_counts();
    store
        .complete_cache_entry("replaceable-touch-recipe", "cache-node", 4_096)
        .await
        .expect("terminal cache completion");
    assert_eq!(
        store.validation_operation_counts().write_calls,
        1,
        "terminal completion must bypass the replaceable gate"
    );
    let after_completion = contract_leader_point(&observer).await;
    assert_eq!(
        contract_stable_leader_delta(after_burst, after_completion),
        1,
        "terminal completion must commit its own Raft entry"
    );

    store.validation_reset_operation_counts();
    for index in 0..8 {
        store
            .touch_cache_entry(&format!("uncoalesced-control-{index}"), "cache-node")
            .await
            .expect("distinct control touch");
    }
    assert_eq!(
        store.validation_operation_counts().write_calls,
        8,
        "distinct identities are the uncoalesced load control"
    );
    let after_control = contract_leader_point(&observer).await;
    assert_eq!(
        contract_stable_leader_delta(after_completion, after_control),
        8,
        "the distinct-identity control must physically exceed the burst budget"
    );

    store.validation_reset_operation_counts();
    store
        .forget_cache_entry("replaceable-touch-recipe", "cache-node", "local")
        .await
        .expect("terminal cache removal");
    assert_eq!(
        store.validation_operation_counts().write_calls,
        1,
        "terminal removal must bypass the replaceable gate"
    );
    assert_eq!(
        contract_stable_leader_delta(after_control, contract_leader_point(&observer).await),
        1,
        "terminal removal must commit its own Raft entry"
    );
}

#[tokio::test]
async fn user_contract_runs_through_dyn_store() {
    for_each_backend(|store, backend| async move {
        assert_eq!(store.count_users().await.expect("count"), 0);
        let admin = store
            .create_user("Admin", "hash-1", true)
            .await
            .expect("create admin");
        let viewer = store
            .create_user("Viewer", "hash-2", false)
            .await
            .expect("create viewer");
        assert_eq!(store.count_users().await.expect("count"), 2);
        assert_eq!(store.count_admins().await.expect("admins"), 1);
        assert_eq!(
            store
                .get_user(admin.id)
                .await
                .expect("get")
                .expect("admin")
                .username,
            "Admin"
        );
        assert_eq!(
            store
                .get_user_by_username("viewer")
                .await
                .expect("lookup")
                .expect("viewer")
                .id,
            viewer.id
        );
        assert_eq!(store.list_users().await.expect("list").len(), 2);
        let first_page = store.list_users_page(0, 1).await.expect("first user page");
        assert_eq!(first_page.len(), 1);
        let second_page = store
            .list_users_page(first_page[0].id, 1)
            .await
            .expect("second user page");
        assert_eq!(second_page.len(), 1);
        assert_ne!(first_page[0].id, second_page[0].id);
        assert!(store
            .set_password(viewer.id, "hash-3")
            .await
            .expect("password"));
        assert!(store.set_admin(viewer.id, true).await.expect("promote"));
        assert_eq!(store.count_admins().await.expect("admins"), 2);

        store
            .create_token("token-one", viewer.id, Some("contract"))
            .await
            .expect("token");
        store
            .create_token("token-two", viewer.id, None)
            .await
            .expect("token");
        assert_eq!(
            store
                .user_for_token("token-one")
                .await
                .expect("resolve")
                .expect("token user")
                .id,
            viewer.id,
            "backend {backend}"
        );
        assert!(store.delete_token("token-one").await.expect("delete token"));
        assert_eq!(
            store
                .delete_tokens_for_user(viewer.id)
                .await
                .expect("delete user tokens"),
            1
        );
        assert!(store.delete_user(admin.id).await.expect("delete user"));
    })
    .await;
}

#[tokio::test]
async fn api_key_contract_runs_through_dyn_store() {
    for_each_backend(|store, backend| async move {
        let expected_scopes = vec![
            scopes::SCAN_TRIGGER.to_owned(),
            scopes::STATUS_READ.to_owned(),
        ];
        let key = store
            .create_api_key("automation", "key-hash", &expected_scopes)
            .await
            .expect("create key");
        assert_eq!(key.scopes, expected_scopes);
        let listed = store.list_api_keys().await.expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].scopes, expected_scopes, "backend {backend}");
        let looked_up = store
            .api_key_for_hash("key-hash")
            .await
            .expect("lookup")
            .expect("key");
        assert_eq!(looked_up.id, key.id);
        assert_eq!(looked_up.scopes, expected_scopes);
        store.touch_api_key(key.id).await.expect("touch");
        assert!(store
            .api_key_for_hash("key-hash")
            .await
            .expect("lookup touched key")
            .expect("touched key")
            .last_used_at
            .is_some());
        assert!(store
            .set_api_key_disabled(key.id, true)
            .await
            .expect("disable"));
        let disabled = store
            .api_key_for_hash("key-hash")
            .await
            .expect("lookup disabled key")
            .expect("disabled key");
        assert!(disabled.disabled);
        assert!(!disabled.allows(scopes::SCAN_TRIGGER));
        assert!(store.delete_api_key(key.id).await.expect("delete"));
    })
    .await;
}

#[tokio::test]
async fn library_contract_runs_through_dyn_store() {
    for_each_backend(|store, backend| async move {
        let library = store
            .create_library(&NewLibrary {
                name: "Contract Movies".into(),
                kind: LibraryKind::Movies,
                paths: vec![PathBuf::from("/contract/movies")],
                anime: false,
            })
            .await
            .expect("create library");
        assert_eq!(
            store
                .get_library(library.id)
                .await
                .expect("get")
                .expect("library")
                .name,
            "Contract Movies"
        );
        let updated = store
            .update_library(
                library.id,
                &NewLibrary {
                    name: "Contract Films".into(),
                    kind: LibraryKind::Movies,
                    paths: vec![PathBuf::from("/contract/films")],
                    anime: false,
                },
            )
            .await
            .expect("update")
            .expect("updated library");
        let scheduled = store
            .set_library_schedule(updated.id, 15, 1_440)
            .await
            .expect("schedule")
            .expect("scheduled library");
        assert_eq!(
            (
                scheduled.scan_interval_mins,
                scheduled.refresh_interval_mins
            ),
            (15, 1_440)
        );
        store
            .mark_library_scanned(scheduled.id, true)
            .await
            .expect("mark scanned");
        let scanned = store
            .get_library(scheduled.id)
            .await
            .expect("get scanned library")
            .expect("scanned library");
        assert!(scanned.last_scan_at.is_some());
        assert!(scanned.last_refresh_at.is_some());
        assert_eq!(store.list_libraries().await.expect("list").len(), 1);
        assert!(
            store.delete_library(scheduled.id).await.expect("delete"),
            "backend {backend}"
        );
    })
    .await;
}

#[tokio::test]
async fn media_contract_runs_through_dyn_store() {
    for_each_backend(|store, backend| async move {
        let movies = store
            .create_library(&NewLibrary {
                name: "Media Contract Movies".into(),
                kind: LibraryKind::Movies,
                paths: vec![PathBuf::from("/contract/movies")],
                anime: false,
            })
            .await
            .expect("movie library");
        let shows = store
            .create_library(&NewLibrary {
                name: "Media Contract Shows".into(),
                kind: LibraryKind::Shows,
                paths: vec![PathBuf::from("/contract/shows")],
                anime: false,
            })
            .await
            .expect("show library");
        let home = store
            .create_library(&NewLibrary {
                name: "Media Contract Home".into(),
                kind: LibraryKind::Home,
                paths: vec![PathBuf::from("/contract/home")],
                anime: false,
            })
            .await
            .expect("home library");
        let books = store
            .create_library(&NewLibrary {
                name: "Media Contract Books".into(),
                kind: LibraryKind::Books,
                paths: vec![PathBuf::from("/contract/books")],
                anime: false,
            })
            .await
            .expect("books library");

        let movie = store
            .insert_item(&NewItem {
                library_id: movies.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: "The Contract Movie".into(),
                year: Some(2024),
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("movie");
        let empty_movie = store
            .insert_item(&NewItem {
                library_id: movies.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: "Empty Contract Movie".into(),
                year: Some(2023),
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("empty movie");
        let show = store
            .insert_item(&NewItem {
                library_id: shows.id,
                kind: ItemKind::Show,
                parent_id: None,
                title: "Contract Show".into(),
                year: Some(2024),
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("show");
        let season = store
            .insert_item(&NewItem {
                library_id: shows.id,
                kind: ItemKind::Season,
                parent_id: Some(show),
                title: "Season 1".into(),
                year: None,
                season_number: Some(1),
                episode_number: None,
            })
            .await
            .expect("season");
        let episode = store
            .insert_item(&NewItem {
                library_id: shows.id,
                kind: ItemKind::Episode,
                parent_id: Some(season),
                title: "Pilot".into(),
                year: None,
                season_number: Some(1),
                episode_number: Some(1),
            })
            .await
            .expect("episode");
        let folder = store
            .insert_item(&NewItem {
                library_id: home.id,
                kind: ItemKind::Folder,
                parent_id: None,
                title: "Trips".into(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("home folder");
        let ebook = store
            .insert_item(&NewItem {
                library_id: books.id,
                kind: ItemKind::Book,
                parent_id: None,
                title: "Shared Contract Title".into(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("ebook");
        let audiobook = store
            .insert_item(&NewItem {
                library_id: books.id,
                kind: ItemKind::Audiobook,
                parent_id: None,
                title: "Shared Contract Title".into(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("audiobook");

        assert_eq!(
            store
                .find_movie(movies.id, "The Contract Movie", Some(2024))
                .await
                .expect("find movie")
                .expect("movie")
                .id,
            movie
        );
        assert_eq!(
            store
                .find_show(shows.id, "Contract Show", Some(2024))
                .await
                .expect("find show")
                .expect("show")
                .id,
            show
        );
        assert_eq!(
            store
                .find_book(
                    books.id,
                    ItemKind::Book,
                    "Shared Contract Title",
                    None,
                    None
                )
                .await
                .expect("find ebook")
                .expect("ebook")
                .id,
            ebook
        );
        assert_eq!(
            store
                .find_book(
                    books.id,
                    ItemKind::Audiobook,
                    "Shared Contract Title",
                    None,
                    None,
                )
                .await
                .expect("find audiobook")
                .expect("audiobook")
                .id,
            audiobook
        );
        assert!(
            store
                .related_book_editions(ebook, "curator:work:shared")
                .await
                .expect("unlinked editions")
                .is_empty(),
            "title equality alone must never relate editions on {backend}"
        );
        store
            .upsert_file(
                ebook,
                "/contract/books/shared.epub",
                4096,
                77,
                &ProbeResult::default(),
            )
            .await
            .expect("ebook file");
        store
            .apply_book_metadata(
                ebook,
                &BookMetadataPatch {
                    title: Some("Package Title".into()),
                    author: Some("Package Author".into()),
                    work_id: None,
                    edition_id: Some("urn:isbn:package".into()),
                    poster_path: Some("books/epub-cover.jpg".into()),
                    source: BookMetadataSource::Epub,
                    required_origin: None,
                },
            )
            .await
            .expect("EPUB metadata");
        store
            .apply_book_metadata(
                ebook,
                &BookMetadataPatch {
                    title: Some("Curator Title".into()),
                    author: Some("Curator Author".into()),
                    work_id: Some("curator:work:shared".into()),
                    edition_id: Some("curator:edition:ebook".into()),
                    poster_path: Some("curator-cover.jpg".into()),
                    source: BookMetadataSource::Curator,
                    required_origin: None,
                },
            )
            .await
            .expect("Curator metadata");
        store
            .apply_book_metadata(
                ebook,
                &BookMetadataPatch {
                    title: Some("Late Package Title".into()),
                    author: Some("Late Package Author".into()),
                    work_id: Some("untrusted:fuzzy-link".into()),
                    edition_id: Some("urn:isbn:late".into()),
                    poster_path: Some("books/late-cover.jpg".into()),
                    source: BookMetadataSource::Epub,
                    required_origin: None,
                },
            )
            .await
            .expect("lower-precedence EPUB refresh");
        store
            .apply_book_metadata(
                audiobook,
                &BookMetadataPatch {
                    title: Some("Curator Title".into()),
                    author: Some("Curator Author".into()),
                    work_id: Some("curator:work:shared".into()),
                    edition_id: Some("curator:edition:audiobook".into()),
                    poster_path: None,
                    source: BookMetadataSource::Curator,
                    required_origin: None,
                },
            )
            .await
            .expect("audiobook relation");

        let enriched = store
            .get_item(ebook)
            .await
            .expect("read enriched ebook")
            .expect("enriched ebook");
        assert_eq!(enriched.title, "Curator Title");
        assert_eq!(enriched.author.as_deref(), Some("Curator Author"));
        assert_eq!(
            enriched.book_work_id.as_deref(),
            Some("curator:work:shared")
        );
        assert_eq!(
            enriched.book_edition_id.as_deref(),
            Some("curator:edition:ebook")
        );
        assert_eq!(enriched.poster_path.as_deref(), Some("curator-cover.jpg"));
        assert_eq!(enriched.book_metadata_source.as_deref(), Some("curator"));
        store
            .apply_book_metadata(
                ebook,
                &BookMetadataPatch {
                    title: Some("Newer Pairing Title".into()),
                    author: Some("Newer Pairing Author".into()),
                    work_id: Some("curator:work:newer".into()),
                    edition_id: None,
                    poster_path: None,
                    source: BookMetadataSource::Curator,
                    required_origin: None,
                },
            )
            .await
            .expect("same-edition concurrent pairing");
        assert!(
            !store
                .apply_book_metadata_if_current(
                    &enriched,
                    &BookMetadataPatch {
                        title: Some("Stale Pairing Title".into()),
                        author: Some("Stale Pairing Author".into()),
                        work_id: Some("curator:work:stale".into()),
                        edition_id: enriched.book_edition_id.clone(),
                        poster_path: enriched.poster_path.clone(),
                        source: BookMetadataSource::Curator,
                        required_origin: None,
                    },
                    None,
                )
                .await
                .expect("stale same-edition conditional pairing"),
            "same-edition/same-poster stale pairing must lose the full-field CAS on {backend}"
        );
        let newest_pairing = store
            .get_item(ebook)
            .await
            .expect("read newest pairing")
            .expect("newest pairing");
        assert_eq!(newest_pairing.title, "Newer Pairing Title", "{backend}");
        assert_eq!(
            newest_pairing.author.as_deref(),
            Some("Newer Pairing Author"),
            "{backend}"
        );
        assert_eq!(
            newest_pairing.book_work_id.as_deref(),
            Some("curator:work:newer"),
            "{backend}"
        );
        store
            .apply_book_metadata(
                ebook,
                &BookMetadataPatch {
                    title: Some("Curator Title".into()),
                    author: Some("Curator Author".into()),
                    work_id: Some("curator:work:shared".into()),
                    edition_id: None,
                    poster_path: None,
                    source: BookMetadataSource::Curator,
                    required_origin: None,
                },
            )
            .await
            .expect("restore media contract pairing");
        let artwork_items = store
            .items_with_artwork()
            .await
            .expect("inventory artwork references");
        assert_eq!(artwork_items.len(), 1, "backend {backend}");
        assert_eq!(artwork_items[0].id, ebook, "backend {backend}");
        assert_eq!(
            artwork_items[0].poster_path.as_deref(),
            Some("curator-cover.jpg"),
            "backend {backend}"
        );
        assert_eq!(
            store
                .find_book(
                    books.id,
                    ItemKind::Book,
                    "stale scanner title",
                    None,
                    Some("/contract/books/shared.epub"),
                )
                .await
                .expect("find enriched ebook by path")
                .expect("enriched ebook identity")
                .id,
            ebook,
            "metadata title replacement must not duplicate a scanned path on {backend}"
        );
        let editions = store
            .related_book_editions(ebook, "curator:work:shared")
            .await
            .expect("related editions");
        assert_eq!(editions.len(), 1, "backend {backend}");
        assert_eq!(editions[0].id, audiobook, "backend {backend}");
        assert_eq!(
            store
                .book_items(books.id, None)
                .await
                .expect("book items")
                .len(),
            2,
            "backend {backend}"
        );
        assert_eq!(
            store
                .find_season(show, 1)
                .await
                .expect("find season")
                .expect("season")
                .id,
            season
        );
        assert_eq!(
            store
                .find_episode(season, 1)
                .await
                .expect("find episode")
                .expect("episode")
                .id,
            episode
        );
        assert_eq!(
            store
                .find_child_item(home.id, None, ItemKind::Folder, "Trips")
                .await
                .expect("find child")
                .expect("folder")
                .id,
            folder
        );
        assert_eq!(
            store.get_item(movie).await.expect("get").expect("movie").id,
            movie
        );
        let titles = store
            .item_titles(&[show, movie, movie, i64::MAX])
            .await
            .expect("bounded item titles");
        assert_eq!(titles.len(), 2, "duplicates and missing ids on {backend}");
        assert_eq!(
            titles.get(&movie).map(String::as_str),
            Some("The Contract Movie"),
            "movie title on {backend}"
        );
        assert_eq!(
            titles.get(&show).map(String::as_str),
            Some("Contract Show"),
            "show title on {backend}"
        );
        assert!(
            store
                .item_titles(&[])
                .await
                .expect("empty item title set")
                .is_empty(),
            "empty title input on {backend}"
        );
        assert_eq!(
            store.get_item_children(show).await.expect("children").len(),
            1
        );

        store
            .apply_metadata(
                movie,
                &MetadataPatch {
                    overview: Some("A backend-neutral movie".into()),
                    tmdb_id: Some(42),
                    imdb_id: Some("tt0000042".into()),
                    runtime_ms: Some(7_200_000),
                    genres: Some(vec!["Drama".into()]),
                    enriched: true,
                    artwork: Some(ArtworkAttempt::Failed("contract fixture".into())),
                    ..Default::default()
                },
            )
            .await
            .expect("movie metadata");
        store
            .apply_metadata(
                show,
                &MetadataPatch {
                    tmdb_id: Some(84),
                    enriched: true,
                    ..Default::default()
                },
            )
            .await
            .expect("show metadata");
        assert_eq!(
            store
                .item_by_external_id(ItemKind::Movie, Some(42), None)
                .await
                .expect("external id")
                .expect("external movie")
                .id,
            movie
        );
        let metadata_queue = store
            .items_needing_metadata(Some(movies.id), false, None)
            .await
            .expect("metadata queue");
        assert!(metadata_queue.iter().any(|item| item.id == empty_movie));
        assert!(!metadata_queue.iter().any(|item| item.id == movie));
        assert_eq!(
            store.episodes_for_show(show).await.expect("episodes").len(),
            1
        );
        let home_artwork = store
            .items_needing_artwork(home.id, false, None)
            .await
            .expect("home artwork");
        assert!(home_artwork.iter().any(|item| item.id == folder));
        assert!(!home_artwork.iter().any(|item| item.id == movie));
        let missing_artwork = store
            .items_missing_artwork(Some(movies.id), 0, 10)
            .await
            .expect("missing artwork");
        assert!(missing_artwork.iter().any(|item| item.id == movie));
        assert!(!missing_artwork.iter().any(|item| item.id == show));
        let missing_genres = store
            .items_missing_genres(0, 10)
            .await
            .expect("missing genres");
        assert!(missing_genres.iter().any(|item| item.id == show));
        assert!(!missing_genres.iter().any(|item| item.id == movie));
        let edited = store
            .update_item_fields(
                folder,
                &ItemEdit {
                    title: Some("Edited Trips".into()),
                    tags: Some(vec!["family".into()]),
                    ..Default::default()
                },
            )
            .await
            .expect("edit")
            .expect("edited folder");
        assert_eq!(edited.title, "Edited Trips");
        store.set_nfo_seeded(folder).await.expect("NFO stamp");
        assert!(store
            .get_item(folder)
            .await
            .expect("get NFO-stamped item")
            .expect("NFO-stamped item")
            .nfo_seeded_at
            .is_some());

        let movie_file = store
            .upsert_file(
                movie,
                "/contract/movies/movie.mkv",
                1_000,
                10,
                &ProbeResult {
                    duration_ms: Some(7_200_000),
                    container: Some("mkv".into()),
                    video_codec: Some("hevc".into()),
                    width: Some(3_840),
                    height: Some(2_160),
                    bitrate: Some(20_000_000),
                    raw_json: Some(r#"{"format":{"filename":"movie.mkv"}}"#.into()),
                    ..Default::default()
                },
            )
            .await
            .expect("movie file");
        let empty_file = store
            .upsert_file(
                empty_movie,
                "/contract/movies/empty.mkv",
                2_000,
                20,
                &ProbeResult::default(),
            )
            .await
            .expect("unprobed file");
        let episode_file = store
            .upsert_file(
                episode,
                "/contract/shows/pilot.mkv",
                3_000,
                30,
                &ProbeResult {
                    duration_ms: Some(3_600_000),
                    container: Some("mkv".into()),
                    video_codec: Some("h264".into()),
                    height: Some(1_080),
                    ..Default::default()
                },
            )
            .await
            .expect("episode file");
        assert_eq!(
            store
                .get_file_by_path("/contract/movies/movie.mkv")
                .await
                .expect("file by path")
                .expect("file")
                .id,
            movie_file
        );
        assert_eq!(
            store
                .get_file(movie_file)
                .await
                .expect("file")
                .expect("file")
                .id,
            movie_file
        );
        assert!(store.media_shape().await.expect("media shape").probed >= 2);
        assert_eq!(store.files_for_item(movie).await.expect("files").len(), 1);
        assert_eq!(
            store
                .child_counts(&[show])
                .await
                .expect("counts")
                .get(&show),
            Some(&1)
        );
        assert_eq!(
            store
                .item_max_heights(&[movie])
                .await
                .expect("heights")
                .get(&movie),
            Some(&2_160)
        );
        assert!(store
            .item_media_facts(&[movie])
            .await
            .expect("facts")
            .contains_key(&movie));
        store
            .set_file_audio_offset(movie_file, 125)
            .await
            .expect("audio offset");
        assert_eq!(
            store
                .get_file(movie_file)
                .await
                .expect("get offset file")
                .expect("offset file")
                .audio_offset_ms,
            125
        );
        assert!(store
            .get_file_probe_json(movie_file)
            .await
            .expect("probe JSON")
            .expect("probe JSON")
            .contains("movie.mkv"));
        store
            .merge_file_probe_chapters(movie_file, r#"[{"start_time":"0.0","end_time":"10.0"}]"#)
            .await
            .expect("merge chapters");
        assert!(store
            .get_file_probe_json(movie_file)
            .await
            .expect("probe JSON")
            .expect("probe JSON")
            .contains("chapters"));
        let missing_probes = store
            .files_missing_probe(Some(movies.id))
            .await
            .expect("missing probes");
        assert!(missing_probes.iter().any(|file| file.id == empty_file));
        assert!(!missing_probes.iter().any(|file| file.id == movie_file));
        assert!(!missing_probes.iter().any(|file| file.id == episode_file));
        assert_eq!(
            store
                .library_file_paths(movies.id)
                .await
                .expect("paths")
                .len(),
            2
        );

        let genre_page = store
            .list_top_items_in_genre(movies.id, ItemSort::Title, 0, 20, Some("drama"))
            .await
            .expect("genre page");
        assert!(genre_page.items.iter().any(|item| item.id == movie));
        assert!(!genre_page.items.iter().any(|item| item.id == empty_movie));
        assert_eq!(
            store
                .list_top_items(movies.id, ItemSort::Added, 0, 20)
                .await
                .expect("page")
                .total,
            2
        );
        let previews = store
            .home_preview_pages(24)
            .await
            .expect("home preview pages");
        for library in [&movies, &shows, &home, &books] {
            let preview = previews
                .iter()
                .find(|page| page.library_id == library.id)
                .unwrap_or_else(|| panic!("missing preview for {} on {backend}", library.name));
            let ordinary = store
                .list_top_items(library.id, ItemSort::Added, 0, 24)
                .await
                .expect("ordinary added page");
            assert_eq!(preview.total, ordinary.total, "backend {backend}");
            assert_eq!(
                preview.items.iter().map(|item| item.id).collect::<Vec<_>>(),
                ordinary
                    .items
                    .iter()
                    .map(|item| item.id)
                    .collect::<Vec<_>>(),
                "Home membership/order drifted for {} on {backend}",
                library.name
            );
        }
        let bounded = store
            .home_preview_pages(1)
            .await
            .expect("bounded home previews");
        assert!(bounded.iter().all(|page| page.items.len() <= 1));
        assert_eq!(
            bounded
                .iter()
                .find(|page| page.library_id == movies.id)
                .expect("movie preview")
                .total,
            2,
            "the preview limit must not truncate the total"
        );
        assert!(!store
            .recently_added(None, 20)
            .await
            .expect("recent")
            .is_empty());
        let search_ids = store
            .search_items("backend-neutral", 20)
            .await
            .expect("search")
            .into_iter()
            .map(|item| item.item.id)
            .collect::<BTreeSet<_>>();
        assert_eq!(search_ids, BTreeSet::from([movie]));

        assert_eq!(
            store
                .ensure_library_root_fingerprint(movies.id, "contract-root", true)
                .await
                .expect("establish root"),
            RootFingerprintStatus::Established
        );
        assert_eq!(
            store
                .ensure_library_root_fingerprint(movies.id, "contract-root", true)
                .await
                .expect("match root"),
            RootFingerprintStatus::Matched
        );
        assert!(matches!(
            store
                .reconcile_library(movies.id, "stale-root", &[empty_file], 1)
                .await
                .expect("reject root"),
            ReconcileOutcome::RefusedRoot { .. }
        ));
        assert!(store
            .get_file(empty_file)
            .await
            .expect("kept file")
            .is_some());
        assert_eq!(
            store
                .reconcile_library(movies.id, "contract-root", &[empty_file], 0)
                .await
                .expect("reject bound"),
            ReconcileOutcome::RefusedPrune {
                requested: 1,
                limit: 0
            }
        );
        assert!(store
            .get_file(empty_file)
            .await
            .expect("kept file")
            .is_some());
        assert!(matches!(
            store
                .reconcile_library(movies.id, "contract-root", &[empty_file], 1)
                .await
                .expect("bounded reconcile"),
            ReconcileOutcome::Applied {
                deleted_files: 1,
                pruned_items: 1..
            }
        ));
        assert!(store
            .reset_library_root_fingerprint(movies.id)
            .await
            .expect("reset root"));
        assert_eq!(
            store
                .ensure_library_root_fingerprint(movies.id, "contract-root", false)
                .await
                .expect("refuse empty root establishment"),
            RootFingerprintStatus::Unestablished
        );
        assert_eq!(
            store
                .ensure_library_root_fingerprint(movies.id, "contract-root", true)
                .await
                .expect("re-establish root"),
            RootFingerprintStatus::Established
        );
        assert!(store.rebuild_search_index().await.expect("rebuild search") > 0);
        assert_eq!(store.delete_files(&[]).await.expect("empty delete"), 0);
        let legacy_orphan = store
            .insert_item(&NewItem {
                library_id: movies.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: "Legacy Prune Contract Orphan".into(),
                year: Some(2022),
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("legacy prune orphan");
        assert!(
            store
                .prune_empty_items(movies.id)
                .await
                .expect("legacy prune")
                >= 1,
            "backend {backend} did not report its real orphan prune"
        );
        assert!(
            store
                .get_item(legacy_orphan)
                .await
                .expect("get pruned orphan")
                .is_none(),
            "backend {backend} reported a prune without removing the fixture"
        );
        assert!(store
            .get_file(episode_file)
            .await
            .expect("episode file")
            .is_some());
        assert!(
            store
                .get_item(movie)
                .await
                .expect("movie remains")
                .is_some(),
            "backend {backend}"
        );
    })
    .await;
}

#[tokio::test]
async fn home_preview_contract_runs_through_dyn_store() {
    for_each_backend(|store, backend| async move {
        assert!(
            store
                .home_preview_pages(24)
                .await
                .expect("empty Home previews")
                .is_empty(),
            "backend {backend}"
        );

        let empty = store
            .create_library(&NewLibrary {
                name: "Home Preview Empty".into(),
                kind: LibraryKind::Movies,
                paths: vec![PathBuf::from("/preview/empty")],
                anime: false,
            })
            .await
            .expect("empty preview library");
        let movies = store
            .create_library(&NewLibrary {
                name: "Home Preview Movies".into(),
                kind: LibraryKind::Movies,
                paths: vec![PathBuf::from("/preview/movies")],
                anime: false,
            })
            .await
            .expect("movie preview library");
        for index in 0..30 {
            store
                .insert_item(&NewItem {
                    library_id: movies.id,
                    kind: ItemKind::Movie,
                    parent_id: None,
                    title: format!("Preview Contract Movie {index:02}"),
                    year: None,
                    season_number: None,
                    episode_number: None,
                })
                .await
                .expect("preview contract movie");
        }

        let home = store
            .create_library(&NewLibrary {
                name: "Home Preview Videos".into(),
                kind: LibraryKind::Home,
                paths: vec![PathBuf::from("/preview/home")],
                anime: false,
            })
            .await
            .expect("home preview library");
        let insert_home = |kind, parent_id, title: &str| NewItem {
            library_id: home.id,
            kind,
            parent_id,
            title: title.into(),
            year: None,
            season_number: None,
            episode_number: None,
        };
        let folder = store
            .insert_item(&insert_home(ItemKind::Folder, None, "Root folder"))
            .await
            .expect("root folder");
        let root_video = store
            .insert_item(&insert_home(ItemKind::Video, None, "Root video"))
            .await
            .expect("root video");
        let root_photo = store
            .insert_item(&insert_home(ItemKind::Photo, None, "Root photo"))
            .await
            .expect("root photo");
        let nested_video = store
            .insert_item(&insert_home(ItemKind::Video, Some(folder), "Nested video"))
            .await
            .expect("nested video");
        let nested_photo = store
            .insert_item(&insert_home(ItemKind::Photo, Some(folder), "Nested photo"))
            .await
            .expect("nested photo");

        let books = store
            .create_library(&NewLibrary {
                name: "Home Preview Books".into(),
                kind: LibraryKind::Books,
                paths: vec![PathBuf::from("/preview/books")],
                anime: false,
            })
            .await
            .expect("book preview library");
        for (kind, title) in [
            (ItemKind::Book, "Preview Book"),
            (ItemKind::Audiobook, "Preview Audiobook"),
        ] {
            store
                .insert_item(&NewItem {
                    library_id: books.id,
                    kind,
                    parent_id: None,
                    title: title.into(),
                    year: None,
                    season_number: None,
                    episode_number: None,
                })
                .await
                .expect("preview book");
        }

        // A wider internal request is still pinned to Home's one public card
        // budget. The HTTP layer composes the absent empty page from the
        // separate authoritative library roster.
        let previews = store
            .home_preview_pages(99)
            .await
            .expect("populated Home previews");
        assert!(
            previews.iter().all(|page| page.library_id != empty.id),
            "the Store result contains only populated pages on {backend}"
        );
        let movie_preview = previews
            .iter()
            .find(|page| page.library_id == movies.id)
            .expect("movie preview");
        let ordinary = store
            .list_top_items(movies.id, ItemSort::Added, 0, 24)
            .await
            .expect("ordinary movie page");
        assert_eq!(movie_preview.total, 30, "backend {backend}");
        assert_eq!(movie_preview.items.len(), 24, "backend {backend}");
        assert_eq!(
            movie_preview
                .items
                .iter()
                .map(|item| item.id)
                .collect::<Vec<_>>(),
            ordinary
                .items
                .iter()
                .map(|item| item.id)
                .collect::<Vec<_>>(),
            "Added ordering drifted on {backend}"
        );

        let home_preview = previews
            .iter()
            .find(|page| page.library_id == home.id)
            .expect("home-video preview");
        assert_eq!(home_preview.total, 3, "backend {backend}");
        let home_ids = home_preview
            .items
            .iter()
            .map(|item| item.id)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            home_ids,
            BTreeSet::from([folder, root_video, root_photo]),
            "root predicate drifted on {backend}"
        );
        assert!(!home_ids.contains(&nested_video));
        assert!(!home_ids.contains(&nested_photo));

        let book_preview = previews
            .iter()
            .find(|page| page.library_id == books.id)
            .expect("book preview");
        assert_eq!(book_preview.total, 2, "backend {backend}");
        assert_eq!(
            book_preview
                .items
                .iter()
                .map(|item| item.kind.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["book", "audiobook"]),
            "book roots drifted on {backend}"
        );
    })
    .await;
}

#[tokio::test]
async fn reading_state_contract_runs_through_dyn_store() {
    for_each_backend(|store, backend| async move {
        let user = store
            .create_user("reading-contract", "hash", false)
            .await
            .expect("user");
        let books = store
            .create_library(&NewLibrary {
                name: "Reading Contract Books".into(),
                kind: LibraryKind::Books,
                paths: vec![PathBuf::from("/reading/books")],
                anime: false,
            })
            .await
            .expect("books");
        let book = store
            .insert_item(&NewItem {
                library_id: books.id,
                kind: ItemKind::Book,
                parent_id: None,
                title: "The Reading Contract".into(),
                year: Some(2026),
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("book");
        let file_id = store
            .upsert_file(
                book,
                "/reading/books/contract.epub",
                4_096,
                100,
                &ProbeResult::default(),
            )
            .await
            .expect("book file");

        assert!(store
            .reading_state(user.id, book, file_id)
            .await
            .expect("raw reading state")
            .is_none());
        assert!(store
            .current_reading_state(user.id, book)
            .await
            .expect("current reading state")
            .is_none());

        let newer = store
            .put_reading_state(
                user.id,
                book,
                &ReadingStateWrite {
                    file_id,
                    file_size: 4_096,
                    file_mtime: 100,
                    locator_json: r#"{"version":1,"href":"chapter-3.xhtml"}"#.into(),
                    progression_millis: 600_000,
                    completed: false,
                    recorded_at: Some(200),
                },
            )
            .await
            .expect("newer offline state");
        assert_eq!(newer.progression_millis, 600_000);

        let stale = store
            .put_reading_state(
                user.id,
                book,
                &ReadingStateWrite {
                    file_id,
                    file_size: 4_096,
                    file_mtime: 100,
                    locator_json: r#"{"version":1,"href":"chapter-1.xhtml"}"#.into(),
                    progression_millis: 100_000,
                    completed: false,
                    recorded_at: Some(100),
                },
            )
            .await
            .expect("stale offline state returns winner");
        assert_eq!(stale, newer, "backend {backend} rewound newer state");
        assert_eq!(
            store
                .current_reading_state(user.id, book)
                .await
                .expect("current state")
                .expect("stored state"),
            newer
        );

        let rescanned_file_id = store
            .upsert_file(
                book,
                "/reading/books/contract.epub",
                4_100,
                101,
                &ProbeResult::default(),
            )
            .await
            .expect("rescan book file");
        assert_eq!(rescanned_file_id, file_id);
        assert!(
            store
                .current_reading_state(user.id, book)
                .await
                .expect("stale revision lookup")
                .is_none(),
            "backend {backend} exposed state for an obsolete file revision"
        );
        assert!(store
            .reading_state(user.id, book, file_id)
            .await
            .expect("raw stale state remains")
            .is_some());

        let current = store
            .put_reading_state(
                user.id,
                book,
                &ReadingStateWrite {
                    file_id,
                    file_size: 4_100,
                    file_mtime: 101,
                    locator_json: r#"{"version":1,"href":"chapter-4.xhtml"}"#.into(),
                    progression_millis: 950_000,
                    completed: true,
                    // A replaced edition starts a new ordering epoch. Its
                    // first offline write must not lose to a timestamp that
                    // belongs to the now-stale revision.
                    recorded_at: Some(50),
                },
            )
            .await
            .expect("online revision state");
        assert!(current.completed);
        assert_eq!(current.updated_at, 50);
        assert_eq!(
            store
                .current_reading_state(user.id, book)
                .await
                .expect("current revision lookup")
                .expect("current revision state"),
            current
        );

        store
            .delete_reading_state(user.id, book, file_id)
            .await
            .expect("delete reading state");
        store
            .delete_reading_state(user.id, book, file_id)
            .await
            .expect("idempotent delete");
        assert!(store
            .reading_state(user.id, book, file_id)
            .await
            .expect("deleted state")
            .is_none());
    })
    .await;
}

#[tokio::test]
async fn watch_contract_runs_through_dyn_store() {
    for_each_backend(|store, backend| async move {
        let user = store
            .create_user("watch-contract", "hash", false)
            .await
            .expect("user");
        let movies = store
            .create_library(&NewLibrary {
                name: "Watch Contract Movies".into(),
                kind: LibraryKind::Movies,
                paths: vec![PathBuf::from("/watch/movies")],
                anime: false,
            })
            .await
            .expect("movies");
        let shows = store
            .create_library(&NewLibrary {
                name: "Watch Contract Shows".into(),
                kind: LibraryKind::Shows,
                paths: vec![PathBuf::from("/watch/shows")],
                anime: false,
            })
            .await
            .expect("shows");
        let books = store
            .create_library(&NewLibrary {
                name: "Watch Contract Books".into(),
                kind: LibraryKind::Books,
                paths: vec![PathBuf::from("/watch/books")],
                anime: false,
            })
            .await
            .expect("books");
        let movie = store
            .insert_item(&NewItem {
                library_id: movies.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: "Watch Contract Movie".into(),
                year: Some(2024),
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("movie");
        let show = store
            .insert_item(&NewItem {
                library_id: shows.id,
                kind: ItemKind::Show,
                parent_id: None,
                title: "Watch Contract Show".into(),
                year: Some(2024),
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("show");
        let season = store
            .insert_item(&NewItem {
                library_id: shows.id,
                kind: ItemKind::Season,
                parent_id: Some(show),
                title: "Season 1".into(),
                year: None,
                season_number: Some(1),
                episode_number: None,
            })
            .await
            .expect("season");
        let mut episodes = Vec::new();
        for number in 1..=2 {
            let episode = store
                .insert_item(&NewItem {
                    library_id: shows.id,
                    kind: ItemKind::Episode,
                    parent_id: Some(season),
                    title: format!("Episode {number}"),
                    year: None,
                    season_number: Some(1),
                    episode_number: Some(number),
                })
                .await
                .expect("episode");
            store
                .upsert_file(
                    episode,
                    &format!("/watch/shows/e{number}.mkv"),
                    1_000,
                    i64::from(number),
                    &ProbeResult {
                        duration_ms: Some(1_000),
                        container: Some("mkv".into()),
                        ..Default::default()
                    },
                )
                .await
                .expect("episode file");
            episodes.push(episode);
        }
        store
            .upsert_file(
                movie,
                "/watch/movies/movie.mkv",
                1_000,
                1,
                &ProbeResult {
                    duration_ms: Some(10_000),
                    container: Some("mkv".into()),
                    ..Default::default()
                },
            )
            .await
            .expect("movie file");
        let audiobook = store
            .insert_item(&NewItem {
                library_id: books.id,
                kind: ItemKind::Audiobook,
                parent_id: None,
                title: "Multipart Contract Book".into(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("audiobook");
        for (part, duration_ms) in [(1, 10_000), (2, 20_000)] {
            store
                .upsert_file(
                    audiobook,
                    &format!("/watch/books/Multipart Contract Book/Part {part}.mp3"),
                    1_000,
                    part,
                    &ProbeResult {
                        duration_ms: Some(duration_ms),
                        container: Some("mp3".into()),
                        ..Default::default()
                    },
                )
                .await
                .expect("audiobook part");
        }

        let audiobook_progress = store
            .put_progress(user.id, audiobook, 25_000, Some(20_000))
            .await
            .expect("global audiobook progress");
        assert_eq!(
            audiobook_progress.position_ms, 25_000,
            "backend {backend} clamped the book timeline to one part"
        );
        assert_eq!(audiobook_progress.duration_ms, Some(30_000));
        assert!(!audiobook_progress.watched);
        let audiobook_progress = store
            .put_progress_if_current(
                user.id,
                audiobook,
                &audiobook_progress,
                26_000,
                Some(20_000),
            )
            .await
            .expect("compare-and-set audiobook progress")
            .expect("current audiobook progress");
        assert_eq!(audiobook_progress.position_ms, 26_000);

        assert!(store
            .watch_state(user.id, movie)
            .await
            .expect("watch state")
            .is_none());
        assert!(store
            .watch_map(user.id, &[movie])
            .await
            .expect("watch map")
            .is_empty());
        let leading = store
            .put_progress(user.id, movie, 4_000, Some(10_000))
            .await
            .expect("progress");
        let trailing = store
            .put_progress_if_current(user.id, movie, &leading, 5_000, Some(10_000))
            .await
            .expect("compare-and-set progress")
            .expect("unchanged row accepts trailing progress");
        assert_eq!(trailing.position_ms, 5_000);
        store
            .set_watched(user.id, movie, false)
            .await
            .expect("competing manual write");
        assert!(store
            .put_progress_if_current(user.id, movie, &trailing, 6_000, Some(10_000))
            .await
            .expect("stale compare-and-set")
            .is_none());
        store
            .put_progress(user.id, movie, 4_000, Some(10_000))
            .await
            .expect("restore progress");
        store
            .put_progress_at(user.id, episodes[0], 200, Some(1_000), Some(1))
            .await
            .expect("dated progress");
        assert!(store
            .watch_state(user.id, movie)
            .await
            .expect("watch state")
            .is_some());
        assert_eq!(
            store
                .watch_map(user.id, &[movie])
                .await
                .expect("watch map")
                .len(),
            1
        );
        let continuing = store
            .continue_watching(user.id, 10)
            .await
            .expect("continue watching");
        assert!(continuing.iter().any(|item| item.item.id == movie));
        assert!(!continuing.iter().any(|item| item.item.id == episodes[1]));

        store
            .set_watched(user.id, movie, true)
            .await
            .expect("set watched");
        let changed = store
            .set_watched_tree(user.id, show, true)
            .await
            .expect("set watched tree");
        assert_eq!(changed.len(), 2);
        let rollup = store.watch_rollup(user.id, show).await.expect("rollup");
        assert_eq!((rollup.watched, rollup.leaves), (2, 2));
        assert_eq!(
            store
                .watch_rollups(user.id, &[show, season])
                .await
                .expect("rollups")
                .len(),
            2
        );

        store
            .set_watched_tree(user.id, show, false)
            .await
            .expect("clear tree");
        store
            .apply_remote_watch(user.id, episodes[0], true, 1_000, Some(1_000), 10)
            .await
            .expect("remote watch");
        let next_up = store.next_up(user.id, 10).await.expect("next up");
        assert_eq!(
            next_up
                .into_iter()
                .map(|item| item.item.id)
                .collect::<Vec<_>>(),
            vec![episodes[1]],
            "backend {backend}"
        );
    })
    .await;
}

#[tokio::test]
async fn trakt_contract_runs_through_dyn_store() {
    for_each_backend(|store, backend| async move {
        let user = store
            .create_user("trakt-contract", "hash", false)
            .await
            .expect("user");
        let library = store
            .create_library(&NewLibrary {
                name: "Trakt Contract Movies".into(),
                kind: LibraryKind::Movies,
                paths: vec![PathBuf::from("/trakt/movies")],
                anime: false,
            })
            .await
            .expect("library");
        let movie = store
            .insert_item(&NewItem {
                library_id: library.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: "Trakt Contract Movie".into(),
                year: Some(2024),
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("movie");
        store
            .apply_metadata(
                movie,
                &MetadataPatch {
                    tmdb_id: Some(4242),
                    imdb_id: Some("tt0004242".into()),
                    runtime_ms: Some(7_200_000),
                    enriched: true,
                    ..Default::default()
                },
            )
            .await
            .expect("metadata");
        store
            .put_progress(user.id, movie, 1_000, Some(7_200_000))
            .await
            .expect("watch state");

        // Both backends persist the Trakt bearer pair as envelopes. The
        // credential only ever exists in the clear on either side of this
        // boundary, never inside it — see CLUSTERING-PLAN.md §3.2.
        let key = CredentialKey::generate();
        assert!(store.get_trakt_auth(user.id).await.expect("get").is_none());
        store
            .put_trakt_auth(&TraktAuth {
                user_id: user.id,
                access_token: key.seal_trakt(user.id, "access-1").expect("seal access"),
                refresh_token: key.seal_trakt(user.id, "refresh-1").expect("seal refresh"),
                expires_at: 100,
                trakt_username: Some("contract".into()),
                connected_at: 1,
                last_sync_at: 0,
                last_activities: None,
            })
            .await
            .expect("put auth");
        let linked = store
            .get_trakt_auth(user.id)
            .await
            .expect("get linked auth")
            .expect("linked auth");
        assert!(linked.is_wrapped(), "the stored bearer pair must be sealed");
        assert!(
            !linked.access_token.as_stored().contains("access-1")
                && !linked.refresh_token.as_stored().contains("refresh-1"),
            "a durable Trakt row must not hold its bearer credential in the clear"
        );
        assert_eq!(
            linked
                .reveal_access_token(&key)
                .expect("open access")
                .expose(),
            "access-1"
        );
        assert_eq!(
            linked
                .reveal_refresh_token(&key)
                .expect("open refresh")
                .expose(),
            "refresh-1"
        );
        assert_eq!(linked.expires_at, 100);
        assert_eq!(linked.trakt_username.as_deref(), Some("contract"));
        assert_eq!(store.list_trakt_auth().await.expect("list").len(), 1);
        // The rotation compare-and-set runs on the stored envelope, so it stays
        // an exact equality check that no voter has to decrypt anything to make.
        let stale_refresh = linked.refresh_token.clone();
        assert!(store
            .update_trakt_tokens(
                user.id,
                &stale_refresh,
                &key.seal_trakt(user.id, "access-2").expect("seal access"),
                &key.seal_trakt(user.id, "refresh-2").expect("seal refresh"),
                200,
            )
            .await
            .expect("update tokens"));
        assert!(!store
            .update_trakt_tokens(
                user.id,
                &stale_refresh,
                &key.seal_trakt(user.id, "loser").expect("seal access"),
                &key.seal_trakt(user.id, "loser-refresh")
                    .expect("seal refresh"),
                300,
            )
            .await
            .expect("reject stale refresh"));
        assert!(!store
            .delete_trakt_auth_if_current(user.id, &stale_refresh)
            .await
            .expect("reject stale unlink"));
        let refreshed = store
            .get_trakt_auth(user.id)
            .await
            .expect("get refreshed auth")
            .expect("refreshed auth");
        assert_eq!(
            refreshed
                .reveal_access_token(&key)
                .expect("open access")
                .expose(),
            "access-2"
        );
        assert_eq!(
            refreshed
                .reveal_refresh_token(&key)
                .expect("open refresh")
                .expose(),
            "refresh-2"
        );
        assert_eq!(refreshed.expires_at, 200);
        store
            .set_trakt_sync(user.id, 50, Some(r#"{"movies":{"watched_at":50}}"#))
            .await
            .expect("set sync");
        let synced = store
            .get_trakt_auth(user.id)
            .await
            .expect("get synced auth")
            .expect("synced auth");
        assert_eq!(synced.last_sync_at, 50);
        assert_eq!(
            synced.last_activities.as_deref(),
            Some(r#"{"movies":{"watched_at":50}}"#)
        );
        let candidates = store
            .trakt_sync_candidates(user.id)
            .await
            .expect("candidates");
        assert_eq!(
            candidates
                .into_iter()
                .map(|candidate| candidate.item_id)
                .collect::<Vec<_>>(),
            vec![movie],
            "backend {backend}"
        );
        store.delete_trakt_auth(user.id).await.expect("delete auth");
        assert!(store.get_trakt_auth(user.id).await.expect("get").is_none());
    })
    .await;
}

#[tokio::test]
async fn watched_outbox_contract_runs_through_dyn_store() {
    for_each_backend(|store, backend| async move {
        let id = store
            .enqueue_watched(r#"{"type":"movie","watched":true}"#)
            .await
            .expect("enqueue");
        let mut due = store.due_watched(10).await.expect("due");
        assert_eq!(due.len(), 1, "backend {backend}");
        assert!(
            store
                .due_watched(10)
                .await
                .expect("claimed row is not due")
                .is_empty(),
            "backend {backend} returned one claim to two workers"
        );
        let entry = due.pop().expect("entry");
        assert_eq!(entry.id, id);
        store
            .settle_watched(&OutboxEntry {
                attempts: 1,
                status: "ok".into(),
                ..entry
            })
            .await
            .expect("settle");
        assert_eq!(
            store.watched_outbox_counts().await.expect("counts"),
            (0, 1, 0)
        );
    })
    .await;
}

async fn seed_file(store: &Arc<dyn Store>, prefix: &str) -> (i64, i64) {
    let user = store
        .create_user(&format!("{prefix}-user"), "hash", false)
        .await
        .expect("seed user");
    let library = store
        .create_library(&NewLibrary {
            name: format!("{prefix} Library"),
            kind: LibraryKind::Movies,
            paths: vec![PathBuf::from(format!("/{prefix}"))],
            anime: false,
        })
        .await
        .expect("seed library");
    let item = store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Movie,
            parent_id: None,
            title: format!("{prefix} Movie"),
            year: Some(2024),
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("seed item");
    let file = store
        .upsert_file(
            item,
            &format!("/{prefix}/movie.mkv"),
            10_000,
            1,
            &ProbeResult {
                duration_ms: Some(7_200_000),
                container: Some("mkv".into()),
                ..Default::default()
            },
        )
        .await
        .expect("seed file");
    (user.id, file)
}

fn unix_seconds() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time after Unix epoch")
            .as_secs(),
    )
    .expect("Unix time fits i64")
}

async fn wait_until_after(second: i64) {
    while unix_seconds() <= second {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn transcode_cache_contract_runs_through_dyn_store() {
    for_each_backend(|store, backend| async move {
        let (_, file) = seed_file(&store, "cache-contract").await;
        let node = "cache-node";
        assert!(store
            .cache_hit("recipe", node)
            .await
            .expect("miss")
            .is_none());
        assert!(store
            .claim_cache_entry("recipe", file, 1, node, "aa/recipe")
            .await
            .expect("claim"));
        assert!(!store
            .claim_cache_entry("recipe", file, 1, node, "bb/loser")
            .await
            .expect("duplicate claim"));
        let claimed = store
            .all_cache_rows(node)
            .await
            .expect("claimed row")
            .pop()
            .expect("one claimed row");
        wait_until_after(claimed.last_used_at).await;
        let stale_cutoff = unix_seconds();
        assert_eq!(
            store
                .stale_cache_claims(node, stale_cutoff)
                .await
                .expect("stale claim before touch")
                .len(),
            1
        );
        store
            .touch_cache_claim("recipe", node)
            .await
            .expect("touch claim");
        assert!(store
            .stale_cache_claims(node, i64::MIN)
            .await
            .expect("fresh claims")
            .is_empty());
        assert!(store
            .stale_cache_claims(node, stale_cutoff)
            .await
            .expect("touched claim")
            .is_empty());
        assert_eq!(
            store
                .stale_cache_claims(node, i64::MAX)
                .await
                .expect("stale claims")
                .len(),
            1
        );
        assert_eq!(store.all_cache_rows(node).await.expect("all rows").len(), 1);
        store
            .complete_cache_entry("recipe", node, 4_096)
            .await
            .expect("complete");
        let ownership = store
            .cache_ownership_inventory(node)
            .await
            .expect("complete ownership inventory");
        assert!(ownership.complete);
        assert_eq!(ownership.rows.len(), 1);
        assert_eq!(ownership.rows[0].relative_dir, "aa/recipe");
        let candidates = store
            .cache_candidate_owners(node, &["aa/recipe".to_owned()], &[])
            .await
            .expect("candidate ownership recheck");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].recipe_hash, "recipe");
        assert!(store
            .cache_hit("recipe", node)
            .await
            .expect("hit")
            .is_some());
        let completed_at = store
            .cache_hit("recipe", node)
            .await
            .expect("completed hit")
            .expect("completed cache row")
            .last_used_at;
        wait_until_after(completed_at).await;
        store
            .touch_cache_entry("recipe", node)
            .await
            .expect("touch entry");
        assert!(
            store
                .cache_hit("recipe", node)
                .await
                .expect("touched hit")
                .expect("touched cache row")
                .last_used_at
                > completed_at
        );
        assert_eq!(store.cache_by_age(node, 10).await.expect("by age").len(), 1);
        assert_eq!(store.cache_bytes(node).await.expect("bytes"), 4_096);
        store
            .forget_cache_entry("recipe", node, "local")
            .await
            .expect("forget");
        assert!(
            store
                .all_cache_rows(node)
                .await
                .expect("all rows")
                .is_empty(),
            "backend {backend}"
        );
    })
    .await;
}

#[tokio::test]
async fn shared_cache_pin_and_fenced_gc_contract_runs_through_dyn_store() {
    for_each_backend(|store, backend| async move {
        let (_, file_id) = seed_file(&store, "shared-cache-contract").await;
        let storage_id = format!("cluster:contract:{backend}:shared");
        let member = CacheStorageMember {
            storage_id: storage_id.clone(),
            node_id: format!("{backend}-reader"),
            storage_class: "shared".to_owned(),
            verified_at_ms: 100,
            verification_state: "verified".to_owned(),
        };
        store
            .put_cache_storage_member(&member)
            .await
            .unwrap_or_else(|error| panic!("{backend}: verify shared storage: {error}"));
        assert_eq!(
            store
                .cache_storage_member(&storage_id, &member.node_id)
                .await
                .unwrap_or_else(|error| panic!("{backend}: read shared member: {error}")),
            Some(member.clone()),
            "{backend}"
        );

        let abandoned_recipe = format!("shared-abandoned-recipe-{backend}");
        assert!(store
            .claim_shared_cache_entry(
                &abandoned_recipe,
                file_id,
                1,
                &storage_id,
                "generation-abandon",
                "shared/abandon/generation",
                101,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: claim abandoned generation: {error}")));
        assert!(store
            .stale_shared_cache_claims(&storage_id, 100, 8)
            .await
            .unwrap_or_else(|error| panic!("{backend}: list fresh claims: {error}"))
            .is_empty());
        let stale_claims = store
            .stale_shared_cache_claims(&storage_id, 101, 8)
            .await
            .unwrap_or_else(|error| panic!("{backend}: list stale claims: {error}"));
        assert_eq!(stale_claims.len(), 1, "{backend}");
        assert_eq!(stale_claims[0].generation_id, "generation-abandon");
        assert!(!store
            .abandon_shared_cache_entry(
                &abandoned_recipe,
                &storage_id,
                "generation-successor",
                "shared/abandon/generation-successor",
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: reject stale abandon: {error}")));
        assert!(store
            .abandon_shared_cache_entry(
                &abandoned_recipe,
                &storage_id,
                "generation-abandon",
                "shared/abandon/generation",
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: exact abandon: {error}")));
        assert_eq!(
            store
                .stale_shared_cache_claims(&storage_id, 0, 8)
                .await
                .unwrap_or_else(|error| panic!("{backend}: list abandoned claim: {error}"))
                .len(),
            1,
            "{backend}"
        );
        assert!(store
            .finalize_abandoned_shared_cache_entry(
                &abandoned_recipe,
                &storage_id,
                "generation-abandon",
                "shared/abandon/generation",
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: finalize abandoned claim: {error}")));
        assert!(store
            .stale_shared_cache_claims(&storage_id, i64::MAX, 8)
            .await
            .unwrap_or_else(|error| panic!("{backend}: list finalized claims: {error}"))
            .is_empty());

        let recipe_hash = format!("shared-cache-recipe-{backend}");
        let generation_id = "generation-a";
        assert!(store
            .claim_shared_cache_entry(
                &recipe_hash,
                file_id,
                1,
                &storage_id,
                generation_id,
                "shared/recipe/generation-a",
                110,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: claim shared generation: {error}")));
        assert!(!store
            .claim_shared_cache_entry(
                &recipe_hash,
                file_id,
                1,
                &storage_id,
                "generation-loser",
                "shared/recipe/generation-loser",
                111,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: duplicate shared claim: {error}")));
        assert!(store
            .complete_shared_cache_entry(
                &recipe_hash,
                &storage_id,
                generation_id,
                4_096,
                &"a".repeat(64),
                120,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: complete shared generation: {error}")));
        let generation = store
            .shared_cache_hit(&recipe_hash, &storage_id)
            .await
            .unwrap_or_else(|error| panic!("{backend}: shared cache hit: {error}"))
            .unwrap_or_else(|| panic!("{backend}: shared generation disappeared"));
        assert_eq!(generation.generation_id, generation_id, "{backend}");
        assert!(store
            .touch_shared_cache_entry(&recipe_hash, &storage_id, generation_id, 150)
            .await
            .unwrap_or_else(|error| panic!("{backend}: touch shared generation: {error}")));
        assert_eq!(
            store
                .shared_cache_hit(&recipe_hash, &storage_id)
                .await
                .unwrap_or_else(|error| panic!("{backend}: read touched generation: {error}"))
                .expect("touched generation")
                .last_used_at,
            150,
            "{backend}: shared cache uses millisecond timestamps"
        );

        let mut lease = match store
            .acquire_lease(
                &format!("shared-cache-gc:{storage_id}"),
                "gc-owner",
                125,
                2_000,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: acquire shared GC lease: {error}"))
        {
            LeaseClaim::Acquired(lease) => lease,
            LeaseClaim::Held { .. } => panic!("{backend}: fresh shared GC lease was held"),
        };

        let session_pin = CacheConsumerPin {
            storage_id: storage_id.clone(),
            recipe_hash: recipe_hash.clone(),
            generation_id: generation_id.to_owned(),
            consumer_kind: CacheConsumerKind::MediaSession,
            consumer_id: "session-incarnation".to_owned(),
            consumer_epoch: 2,
            expires_at_ms: 1_000,
        };
        assert!(store
            .acquire_cache_consumer_pin(&session_pin, 130)
            .await
            .unwrap_or_else(|error| panic!("{backend}: acquire session pin: {error}")));
        assert!(!store
            .acquire_cache_consumer_pin(
                &CacheConsumerPin {
                    consumer_epoch: 1,
                    expires_at_ms: 1_500,
                    ..session_pin.clone()
                },
                131,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: reject stale pin epoch: {error}")));
        assert!(store
            .shared_cache_gc_candidates(&storage_id, 200, 10)
            .await
            .unwrap_or_else(|error| panic!("{backend}: pinned GC candidates: {error}"))
            .is_empty());
        assert!(store
            .retire_shared_cache_generation(&generation, 200, &lease)
            .await
            .unwrap_or_else(|error| panic!("{backend}: reject pinned retirement: {error}"))
            .is_none());
        assert_eq!(
            store
                .renew_cache_consumer_pins(
                    &[CacheConsumerPin {
                        expires_at_ms: 1_500,
                        ..session_pin.clone()
                    }],
                    201,
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: renew session pin: {error}")),
            1,
            "{backend}"
        );
        assert_eq!(
            store
                .renew_cache_consumer_pins(
                    &[CacheConsumerPin {
                        expires_at_ms: 1_200,
                        ..session_pin.clone()
                    }],
                    202,
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: replay older renewal: {error}")),
            1,
            "{backend}"
        );
        assert!(store
            .shared_cache_gc_candidates(&storage_id, 1_300, 10)
            .await
            .unwrap_or_else(|error| panic!("{backend}: monotone pin candidates: {error}"))
            .is_empty());
        assert!(store
            .release_cache_consumer_pin(
                &storage_id,
                &recipe_hash,
                generation_id,
                CacheConsumerKind::MediaSession,
                &session_pin.consumer_id,
                session_pin.consumer_epoch,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: release session pin: {error}")));

        let lookup_pin = CacheConsumerPin {
            consumer_id: "lookup-crash-bridge".to_owned(),
            consumer_epoch: 1,
            expires_at_ms: 215,
            ..session_pin.clone()
        };
        assert!(store
            .acquire_cache_consumer_pin(&lookup_pin, 210)
            .await
            .unwrap_or_else(|error| panic!("{backend}: acquire crash bridge pin: {error}")));
        assert_eq!(
            store
                .prune_expired_cache_consumer_pins(&storage_id, 214, 8)
                .await
                .unwrap_or_else(|error| panic!("{backend}: early pin prune: {error}")),
            0,
            "{backend}: live bridge pin must survive pruning"
        );
        assert_eq!(
            store
                .prune_expired_cache_consumer_pins(&storage_id, 215, 8)
                .await
                .unwrap_or_else(|error| panic!("{backend}: expired pin prune: {error}")),
            1,
            "{backend}: a crashed lookup owner must not leak its expired pin"
        );
        assert!(!store
            .release_cache_consumer_pin(
                &storage_id,
                &recipe_hash,
                generation_id,
                CacheConsumerKind::MediaSession,
                &lookup_pin.consumer_id,
                lookup_pin.consumer_epoch,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: inspect pruned bridge pin: {error}")));

        let racing_pin = CacheConsumerPin {
            consumer_kind: CacheConsumerKind::OfflineDownload,
            consumer_id: "racing-reader".to_owned(),
            consumer_epoch: 1,
            expires_at_ms: 1_600,
            ..session_pin.clone()
        };
        let pin_store = Arc::clone(&store);
        let retire_store = Arc::clone(&store);
        let pin_future = async {
            pin_store
                .acquire_cache_consumer_pin(&racing_pin, 220)
                .await
                .unwrap_or_else(|error| panic!("{backend}: racing pin: {error}"))
        };
        let retire_future = async {
            retire_store
                .retire_shared_cache_generation(&generation, 220, &lease)
                .await
                .unwrap_or_else(|error| panic!("{backend}: racing retirement: {error}"))
        };
        let (pin_won, gc_successor) = tokio::join!(pin_future, retire_future);
        let gc_won = gc_successor.is_some();
        assert_ne!(
            pin_won, gc_won,
            "{backend}: pin acquisition and retirement must choose exactly one winner"
        );
        if pin_won {
            assert!(store
                .release_cache_consumer_pin(
                    &storage_id,
                    &recipe_hash,
                    generation_id,
                    CacheConsumerKind::OfflineDownload,
                    &racing_pin.consumer_id,
                    racing_pin.consumer_epoch,
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: release racing pin: {error}")));
            lease = store
                .retire_shared_cache_generation(&generation, 221, &lease)
                .await
                .unwrap_or_else(|error| panic!("{backend}: retire race survivor: {error}"))
                .unwrap_or_else(|| panic!("{backend}: race survivor was not retired"));
        } else {
            lease = gc_successor.expect("GC race winner successor");
        }
        assert!(store
            .shared_cache_hit(&recipe_hash, &storage_id)
            .await
            .unwrap_or_else(|error| panic!("{backend}: retired shared lookup: {error}"))
            .is_none());
        let cleanup_candidates = store
            .shared_cache_gc_candidates(&storage_id, 222, 10)
            .await
            .unwrap_or_else(|error| panic!("{backend}: retired cleanup candidate: {error}"));
        assert_eq!(cleanup_candidates.len(), 1, "{backend}");
        assert!(cleanup_candidates[0].cleanup_pending, "{backend}");
        let mut wrong_generation = cleanup_candidates[0].clone();
        wrong_generation.relative_dir.push_str("-wrong");
        assert!(store
            .finalize_retired_shared_cache_generation(&wrong_generation, 223, &lease)
            .await
            .unwrap_or_else(|error| panic!("{backend}: wrong tombstone finalization: {error}"))
            .is_none());
        let renewal_now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("contract clock after epoch")
            .as_millis()
            .min(i64::MAX as u128) as i64;
        let renewal_expiry = lease.expires_at_unix_ms.saturating_add(1_000);
        lease = store
            .renew_lease(&lease, renewal_now, renewal_expiry)
            .await
            .unwrap_or_else(|error| panic!("{backend}: renew after wrong finalization: {error}"))
            .unwrap_or_else(|| panic!("{backend}: wrong finalization advanced the GC lease"));
        lease = store
            .finalize_retired_shared_cache_generation(&cleanup_candidates[0], 223, &lease)
            .await
            .unwrap_or_else(|error| panic!("{backend}: finalize retired generation: {error}"))
            .unwrap_or_else(|| panic!("{backend}: retired generation was not finalized"));
        assert!(store
            .shared_cache_gc_candidates(&storage_id, 224, 10)
            .await
            .unwrap_or_else(|error| panic!("{backend}: finalized GC candidates: {error}"))
            .is_empty());
        assert!(store
            .finalize_retired_shared_cache_generation(&cleanup_candidates[0], 224, &lease)
            .await
            .unwrap_or_else(|error| panic!("{backend}: repeat finalization: {error}"))
            .is_none());
        let renewal_now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("contract clock after epoch")
            .as_millis()
            .min(i64::MAX as u128) as i64;
        let renewal_expiry = lease.expires_at_unix_ms.saturating_add(1_000);
        lease = store
            .renew_lease(&lease, renewal_now, renewal_expiry)
            .await
            .unwrap_or_else(|error| panic!("{backend}: renew after repeat finalization: {error}"))
            .unwrap_or_else(|| panic!("{backend}: absent finalization advanced the GC lease"));

        let offline_recipe = format!("shared-offline-recipe-{backend}");
        assert!(store
            .claim_shared_cache_entry(
                &offline_recipe,
                file_id,
                1,
                &storage_id,
                "generation-offline",
                "shared/offline/generation",
                230,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: claim offline generation: {error}")));
        assert!(store
            .complete_shared_cache_entry(
                &offline_recipe,
                &storage_id,
                "generation-offline",
                8_192,
                &"b".repeat(64),
                240,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: complete offline generation: {error}")));
        let offline_generation = store
            .shared_cache_hit(&offline_recipe, &storage_id)
            .await
            .unwrap_or_else(|error| panic!("{backend}: offline shared hit: {error}"))
            .expect("offline generation");
        let package_pin = CacheConsumerPin {
            storage_id: storage_id.clone(),
            recipe_hash: offline_recipe.clone(),
            generation_id: "generation-offline".to_owned(),
            consumer_kind: CacheConsumerKind::OfflinePackage,
            consumer_id: "offline-package".to_owned(),
            consumer_epoch: 1,
            expires_at_ms: 1_700,
        };
        let offline_pins = [
            package_pin.clone(),
            CacheConsumerPin {
                consumer_kind: CacheConsumerKind::OfflineDownload,
                consumer_id: "offline-download".to_owned(),
                ..package_pin
            },
        ];
        for pin in &offline_pins {
            assert!(store
                .acquire_cache_consumer_pin(pin, 250)
                .await
                .unwrap_or_else(|error| panic!("{backend}: acquire offline pin: {error}")));
        }
        for pin in &offline_pins {
            assert!(store
                .retire_shared_cache_generation(&offline_generation, 251, &lease)
                .await
                .unwrap_or_else(|error| panic!("{backend}: offline pin retirement: {error}"))
                .is_none());
            assert!(store
                .release_cache_consumer_pin(
                    &pin.storage_id,
                    &pin.recipe_hash,
                    &pin.generation_id,
                    pin.consumer_kind,
                    &pin.consumer_id,
                    pin.consumer_epoch,
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: release offline pin: {error}")));
        }
        assert!(store
            .retire_shared_cache_generation(&offline_generation, 252, &lease)
            .await
            .unwrap_or_else(|error| {
                panic!("{backend}: retire unpinned offline generation: {error}")
            })
            .is_some());

        assert!(store
            .mark_cache_storage_suspect(&storage_id, &member.node_id, 300)
            .await
            .unwrap_or_else(|error| panic!("{backend}: mark shared storage suspect: {error}")));
        assert_eq!(
            store
                .cache_storage_member(&storage_id, &member.node_id)
                .await
                .unwrap_or_else(|error| panic!("{backend}: read suspect member: {error}"))
                .map(|member| member.verification_state),
            Some("suspect".to_owned()),
            "{backend}"
        );
    })
    .await;
}

#[tokio::test]
async fn offline_lifecycles_pin_shared_generations_through_dyn_store() {
    for_each_backend(|store, backend| async move {
        let (user_id, file_id) = seed_file(&store, "offline-shared-pin-contract").await;
        let storage_id = format!("shared:offline-lifecycle:{backend}");
        store
            .put_cache_storage_member(&CacheStorageMember {
                storage_id: storage_id.clone(),
                node_id: format!("{backend}-offline-reader"),
                storage_class: "shared".to_owned(),
                verified_at_ms: 100,
                verification_state: "verified".to_owned(),
            })
            .await
            .unwrap_or_else(|error| panic!("{backend}: verify shared storage: {error}"));
        let mut lease = match store
            .acquire_lease(
                &format!("shared-cache-gc:{storage_id}"),
                "offline-gc-owner",
                110,
                10_000,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: acquire shared GC lease: {error}"))
        {
            LeaseClaim::Acquired(lease) => lease,
            LeaseClaim::Held { .. } => panic!("{backend}: fresh shared GC lease was held"),
        };

        let package_recipe = format!("offline-package-pin-{backend}");
        let package_generation_id = "offline-package-generation";
        assert!(store
            .claim_shared_cache_entry(
                &package_recipe,
                file_id,
                1,
                &storage_id,
                package_generation_id,
                "offline/package/generation",
                120,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: claim package generation: {error}")));
        assert!(store
            .complete_shared_cache_entry(
                &package_recipe,
                &storage_id,
                package_generation_id,
                1_024,
                &"c".repeat(64),
                130,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: complete package generation: {error}")));
        let package_generation = store
            .shared_cache_hit(&package_recipe, &storage_id)
            .await
            .unwrap_or_else(|error| panic!("{backend}: read package generation: {error}"))
            .expect("package generation");
        let package = offline_request(
            "shared-package-pin",
            "shared-package-pin-request",
            user_id,
            file_id,
        );
        assert!(matches!(
            store
                .create_offline_package(&package, 100, 100_000, 100_000)
                .await
                .unwrap_or_else(|error| panic!("{backend}: create shared package: {error}")),
            OfflineCreateOutcome::Created(_)
        ));
        assert!(store
            .mark_offline_package_ready(
                &package.id,
                &package.node_id,
                &package_recipe,
                1_024,
                60_000,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: ready shared package: {error}")));
        assert!(store
            .retire_shared_cache_generation(&package_generation, 140, &lease)
            .await
            .unwrap_or_else(|error| panic!("{backend}: package pin retirement: {error}"))
            .is_none());
        assert!(store
            .delete_offline_package(&package.id, user_id)
            .await
            .unwrap_or_else(|error| panic!("{backend}: delete shared package: {error}")));
        lease = store
            .retire_shared_cache_generation(&package_generation, 150, &lease)
            .await
            .unwrap_or_else(|error| {
                panic!("{backend}: retire generation after package deletion: {error}")
            })
            .unwrap_or_else(|| panic!("{backend}: package generation was not retired"));

        let download_recipe = format!("offline-download-pin-{backend}");
        let download_generation_id = "offline-download-generation";
        let download = offline_request(
            "shared-download-pin",
            "shared-download-pin-request",
            user_id,
            file_id,
        );
        assert!(matches!(
            store
                .create_offline_package(&download, 160, 100_000, 100_000)
                .await
                .unwrap_or_else(|error| panic!("{backend}: create download package: {error}")),
            OfflineCreateOutcome::Created(_)
        ));
        assert!(store
            .mark_offline_package_ready(
                &download.id,
                &download.node_id,
                &download_recipe,
                2_048,
                60_000,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: ready download package: {error}")));
        let token_hash = "e".repeat(64);
        assert!(matches!(
            store
                .put_offline_lease(&download.id, user_id, &token_hash, 20_000)
                .await
                .unwrap_or_else(|error| panic!("{backend}: create download lease: {error}")),
            OfflineLeaseOutcome::Created(_)
        ));
        assert!(store
            .claim_shared_cache_entry(
                &download_recipe,
                file_id,
                1,
                &storage_id,
                download_generation_id,
                "offline/download/generation",
                170,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: claim download generation: {error}")));
        assert!(store
            .complete_shared_cache_entry(
                &download_recipe,
                &storage_id,
                download_generation_id,
                2_048,
                &"d".repeat(64),
                180,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: complete download generation: {error}")));
        let download_generation = store
            .shared_cache_hit(&download_recipe, &storage_id)
            .await
            .unwrap_or_else(|error| panic!("{backend}: read download generation: {error}"))
            .expect("download generation");
        assert!(store
            .release_cache_consumer_pin(
                &storage_id,
                &download_recipe,
                download_generation_id,
                CacheConsumerKind::OfflinePackage,
                &download.id,
                1,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: isolate download pin: {error}")));
        assert!(store
            .retire_shared_cache_generation(&download_generation, 190, &lease)
            .await
            .unwrap_or_else(|error| panic!("{backend}: download pin retirement: {error}"))
            .is_none());
        assert!(store
            .delete_offline_package(&download.id, user_id)
            .await
            .unwrap_or_else(|error| panic!("{backend}: delete download package: {error}")));
        assert!(store
            .retire_shared_cache_generation(&download_generation, 200, &lease)
            .await
            .unwrap_or_else(|error| panic!(
                "{backend}: retire generation after download deletion: {error}"
            ))
            .is_some());
    })
    .await;
}

fn offline_request(id: &str, request_id: &str, user_id: i64, file_id: i64) -> NewOfflinePackage {
    NewOfflinePackage {
        id: id.into(),
        request_id: request_id.into(),
        user_id,
        file_id,
        node_id: "offline-node".into(),
        source_path: "/offline-contract/movie.mkv".into(),
        source_size: 10_000,
        source_mtime: 1,
        effective_rate_control: "qvbr:21".into(),
        target_height: 1_080,
        output_width: Some(1_920),
        output_height: Some(1_080),
        audio_index: Some(0),
        audio_offset_ms: 0,
        subtitle_index: None,
        subtitle_language: None,
        subtitle_mode: "none".into(),
        estimated_bytes: 5_000,
        reserved_bytes: 5_000,
        expires_at: 10_000,
    }
}

#[tokio::test]
async fn offline_package_contract_runs_through_dyn_store() {
    for_each_backend(|store, backend| async move {
        let (user_id, file_id) = seed_file(&store, "offline-contract").await;
        let first = offline_request("package-1", "request-1", user_id, file_id);
        let OfflineCreateOutcome::Created(created) = store
            .create_offline_package(&first, 10, 100_000, 100_000)
            .await
            .expect("create package")
        else {
            panic!("backend {backend} did not create package");
        };
        assert_eq!(created.effective_rate_control, "qvbr:21");
        assert!(matches!(
            store
                .create_offline_package(&first, 10, 100_000, 100_000)
                .await
                .expect("idempotent create"),
            OfflineCreateOutcome::Existing(_)
        ));
        let mut changed_server_policy = first.clone();
        changed_server_policy.effective_rate_control = "vbr".into();
        let OfflineCreateOutcome::Existing(existing) = store
            .create_offline_package(&changed_server_policy, 10, 100_000, 100_000)
            .await
            .expect("server-derived rate-control retry")
        else {
            panic!("backend {backend} broke idempotency after a server policy change");
        };
        assert_eq!(existing.effective_rate_control, "qvbr:21");
        let mut changed_expiry = first.clone();
        changed_expiry.expires_at += 1;
        let OfflineCreateOutcome::Existing(existing) = store
            .create_offline_package(&changed_expiry, 10, 100_000, 100_000)
            .await
            .expect("server-clock retry")
        else {
            panic!("backend {backend} rejected a retry after server-derived expiry advanced");
        };
        assert_eq!(existing.expires_at, first.expires_at);
        let mut changed_estimate = first.clone();
        changed_estimate.estimated_bytes += 1;
        assert_eq!(
            store
                .create_offline_package(&changed_estimate, 10, 100_000, 100_000)
                .await
                .expect("estimate conflict"),
            OfflineCreateOutcome::RequestConflict,
            "backend {backend} accepted changed estimate under one request id"
        );
        let mut changed_reservation = first.clone();
        changed_reservation.reserved_bytes += 1;
        assert_eq!(
            store
                .create_offline_package(&changed_reservation, 10, 100_000, 100_000)
                .await
                .expect("reservation conflict"),
            OfflineCreateOutcome::RequestConflict,
            "backend {backend} accepted changed reservation under one request id"
        );
        let mut other_node =
            offline_request("package-other-node", "request-other-node", user_id, file_id);
        other_node.node_id = "other-offline-node".into();
        assert!(
            matches!(
                store
                    .create_offline_package(&other_node, 10, 100_000, 5_000)
                    .await
                    .expect("per-node admission"),
                OfflineCreateOutcome::Created(_)
            ),
            "backend {backend} charged another node's bytes to the local budget"
        );
        let mut negative =
            offline_request("package-negative", "request-negative", user_id, file_id);
        negative.reserved_bytes = -1;
        assert!(matches!(
            store
                .create_offline_package(&negative, 10, 100_000, 100_000)
                .await
                .expect("negative reservation refusal"),
            OfflineCreateOutcome::ByteLimit { .. }
        ));
        let mut overflow =
            offline_request("package-overflow", "request-overflow", user_id, file_id);
        overflow.reserved_bytes = i64::MAX;
        assert!(matches!(
            store
                .create_offline_package(&overflow, 10, i64::MAX, i64::MAX)
                .await
                .expect("overflow-safe reservation refusal"),
            OfflineCreateOutcome::ByteLimit { .. }
        ));
        assert!(store
            .offline_package_for_user(&first.id, user_id)
            .await
            .expect("package lookup")
            .is_some());
        let renewed = store
            .renew_offline_package_for_user(&first.id, user_id, 20_000)
            .await
            .expect("renew package")
            .expect("renewed package");
        assert_eq!(renewed.expires_at, 20_000);
        let activity = store
            .offline_activity_packages("offline-node", 1, 0, 10)
            .await
            .expect("activity")
            .pop()
            .expect("live activity package");
        assert_eq!(
            activity.item_id,
            store
                .get_file(file_id)
                .await
                .expect("file")
                .map(|f| f.item_id)
        );
        assert_eq!(activity.title, "offline-contract Movie");
        assert_eq!(activity.user_name, "offline-contract-user");
        let stats = store
            .offline_package_stats("offline-node", 1)
            .await
            .expect("stats");
        assert_eq!(stats.queued, 1);
        let claimed = store
            .claim_next_offline_package("offline-node")
            .await
            .expect("claim")
            .expect("package");
        assert_eq!(claimed.id, first.id);
        assert_eq!(claimed.effective_rate_control, "qvbr:21");
        assert_eq!(
            store
                .reset_interrupted_offline_packages("offline-node")
                .await
                .expect("reset"),
            1
        );
        assert_eq!(
            store
                .offline_package_for_user(&first.id, user_id)
                .await
                .expect("reset package lookup")
                .expect("reset package")
                .state,
            "queued"
        );
        store
            .claim_next_offline_package("offline-node")
            .await
            .expect("claim after reset")
            .expect("package after reset");
        assert!(store
            .requeue_offline_package(&first.id, "offline-node")
            .await
            .expect("requeue"));
        store
            .claim_next_offline_package("offline-node")
            .await
            .expect("claim")
            .expect("package");
        assert!(store
            .set_offline_package_recipe(&first.id, "offline-recipe")
            .await
            .expect("set recipe"));
        assert_eq!(
            store
                .offline_package_for_user(&first.id, user_id)
                .await
                .expect("recipe package lookup")
                .expect("recipe package")
                .recipe_hash
                .as_deref(),
            Some("offline-recipe")
        );
        assert!(store
            .update_offline_progress(&first.id, "offline-node", "video", 500)
            .await
            .expect("progress"));
        let progressing = store
            .offline_package_for_user(&first.id, user_id)
            .await
            .expect("progress package lookup")
            .expect("progress package");
        assert_eq!(progressing.phase, "video");
        assert_eq!(progressing.progress_millis, 500);
        assert!(store
            .mark_offline_package_ready(
                &first.id,
                "offline-node",
                "offline-recipe",
                4_000,
                7_200_000
            )
            .await
            .expect("ready"));
        assert!(matches!(
            store
                .put_offline_lease(&first.id, user_id, "lease-hash", 30_000)
                .await
                .expect("lease"),
            OfflineLeaseOutcome::Created(_)
        ));
        assert!(matches!(
            store
                .put_offline_lease(&first.id, user_id, "lease-hash", 40_000)
                .await
                .expect("renew lease"),
            OfflineLeaseOutcome::Renewed(_)
        ));
        assert!(store
            .offline_package_for_lease("lease-hash", 1, 50_000)
            .await
            .expect("lease lookup")
            .is_some());
        assert!(!store
            .invalidate_ready_offline_package(
                &first.id,
                "offline-node",
                "wrong-recipe",
                "cache_integrity",
                "corrupt generation",
            )
            .await
            .expect("stale ready invalidation"));
        assert!(store
            .invalidate_ready_offline_package(
                &first.id,
                "offline-node",
                "offline-recipe",
                "cache_integrity",
                "corrupt generation",
            )
            .await
            .expect("exact ready invalidation"));
        let invalidated = store
            .offline_package_for_user(&first.id, user_id)
            .await
            .expect("invalidated package lookup")
            .expect("invalidated package");
        assert_eq!(invalidated.state, "failed");
        assert_eq!(invalidated.phase, "integrity");
        assert_eq!(invalidated.error_code.as_deref(), Some("cache_integrity"));
        assert!(store
            .offline_package_for_lease("lease-hash", 1, 50_000)
            .await
            .expect("failed lease lookup")
            .is_none());

        let failed = offline_request("package-2", "request-2", user_id, file_id);
        store
            .create_offline_package(&failed, 10, 100_000, 100_000)
            .await
            .expect("create failed fixture");
        assert!(store
            .fail_offline_package(
                &failed.id,
                "offline-node",
                "video",
                "encoder",
                "contract failure"
            )
            .await
            .expect("fail package"));
        let failed_package = store
            .offline_package_for_user(&failed.id, user_id)
            .await
            .expect("failed package lookup")
            .expect("failed package");
        assert_eq!(failed_package.state, "failed");
        assert_eq!(failed_package.error_code.as_deref(), Some("encoder"));
        assert_eq!(
            failed_package.error_message.as_deref(),
            Some("contract failure")
        );

        let mut expired = offline_request("package-3", "request-3", user_id, file_id);
        expired.expires_at = 1;
        store
            .create_offline_package(&expired, 10, 100_000, 100_000)
            .await
            .expect("create expired fixture");
        assert!(store.expire_offline_packages(2).await.expect("expire") >= 1);
        assert!(
            store
                .delete_offline_package(&first.id, user_id)
                .await
                .expect("delete"),
            "backend {backend}"
        );
    })
    .await;
}
