//! Ordinary-probe publication is distinct from atomic probe-document grafts.
use super::super::{acquired, for_each_backend, for_each_strandable_backend, StrandedFieldOrder};
use super::{active, bound, demand, enqueue, probe, publication, reprobe, seed};
use plurx_core::{
    domain::MediaFile,
    scan::dv_title::PROBE_FIELD,
    store::{background_jobs::*, background_jobs_probe::*, Store},
};
use std::sync::Arc;

#[derive(Clone, Copy, Debug)]
enum Decoration {
    Chapters,
    Hevc,
    Encoding,
}
const DECORATIONS: [Decoration; 3] = [Decoration::Chapters, Decoration::Hevc, Decoration::Encoding];

async fn decorate(store: &Arc<dyn Store>, file: &MediaFile, kind: Decoration, value: i32) {
    let body = format!(r#"{{"witness":{value}}}"#);
    match kind {
        Decoration::Chapters => store
            .merge_file_probe_chapters(file.id, &format!("[{body}]"))
            .await
            .expect("chapter graft"),
        Decoration::Hevc => assert!(store
            .merge_file_probe_hevc_parameter_sets(file.id, file.size, file.mtime, &body)
            .await
            .expect("HEVC graft")),
        Decoration::Encoding => assert!(store
            .merge_file_probe_content_encoding(file.id, file.size, file.mtime, &body)
            .await
            .expect("encoding graft")),
    }
}
async fn aggregate(store: &Arc<dyn Store>, file: i64) -> serde_json::Value {
    let raw = store
        .get_file_probe_json(file)
        .await
        .expect("probe")
        .expect("probe");
    serde_json::from_str::<serde_json::Value>(&raw).expect("JSON")[PROBE_FIELD].clone()
}

#[tokio::test]
async fn title_decorations_preserve_queued_running_published_and_cancelled_generations() {
    for_each_backend(|store, backend| async move {
        let scanner = "a".repeat(64);
        let mut now = 1_000;
        for kind in DECORATIONS {
            for state in ["queued", "running", "published", "cancelled"] {
                let file = seed(&store, &format!("graft-{kind:?}-{state}")).await;
                let current = bound(&store, file.id, &scanner).await;
                let EnqueueOutcome::Accepted { job_id, .. } =
                    enqueue(&store, &current, &scanner, now).await
                else {
                    panic!("{backend}: fresh demand");
                };
                let mut job = store
                    .background_job(&job_id)
                    .await
                    .expect("job")
                    .expect("job");
                if state != "queued" {
                    // Let the shared claim helper use the same live-attempt fencing.
                    job = super::claim(&store, &job_id, now).await;
                }
                if state == "published" {
                    assert!(matches!(
                        store
                            .publish_dv_title_job(publication(&current, &job, &scanner, now + 1))
                            .await
                            .expect("publish"),
                        JobPublishOutcome::Published { .. }
                    ));
                } else if state == "cancelled" {
                    store
                        .cancel_job(CancelJob {
                            job_id: job_id.clone(),
                            now_ms: now + 1,
                        })
                        .await
                        .expect("manual cancel");
                }
                let before = store
                    .background_job(&job_id)
                    .await
                    .expect("job")
                    .expect("job");
                let before_aggregate = aggregate(&store, file.id).await;
                assert_eq!(before_aggregate.is_null(), state != "published");
                // A changed graft followed by an identical write and another changed graft.
                for value in [1, 1, 2] {
                    decorate(&store, &file, kind, value).await;
                    assert_eq!(
                        aggregate(&store, file.id).await,
                        before_aggregate,
                        "{backend}: {kind:?}/{state} aggregate"
                    );
                    let after = store
                        .background_job(&job_id)
                        .await
                        .expect("job")
                        .expect("job");
                    assert_eq!(after.state, before.state, "{backend}: {kind:?}/{state}");
                    assert_eq!(after.token, before.token, "{backend}: exact attempt");
                    assert_eq!(
                        after.revision, before.revision,
                        "{backend}: exact job revision"
                    );
                    assert_eq!(
                        store
                            .bind_dv_title_demand(current.probe_revision, &scanner)
                            .await
                            .expect("same scanner"),
                        Some(current.probe_revision)
                    );
                    let eligible = store.dv_title_demands(&scanner, 0).await.expect("demand");
                    if matches!(state, "published" | "cancelled") {
                        assert!(
                            !eligible.iter().any(|d| d.file_id == file.id),
                            "{backend}: {kind:?} cannot undo terminal suppression"
                        );
                        assert!(
                            !matches!(
                                enqueue(&store, &current, &scanner, now + 2).await,
                                EnqueueOutcome::Accepted { .. }
                            ),
                            "no new title job"
                        );
                    } else {
                        let after = eligible
                            .iter()
                            .find(|d| d.file_id == file.id)
                            .expect("same demand");
                        assert_eq!(after.probe_revision, current.probe_revision);
                        // Exact same-scanner binding above must not rotate the revision.
                    }
                }
                if state == "running" {
                    assert!(matches!(
                        store
                            .publish_dv_title_job(publication(&current, &job, &scanner, now + 3))
                            .await
                            .expect("unchanged attempt still publishes"),
                        JobPublishOutcome::Published { .. }
                    ));
                } else if state == "queued" {
                    store
                        .cancel_job(CancelJob {
                            job_id,
                            now_ms: now + 3,
                        })
                        .await
                        .expect("retire queued");
                }
                // Same ordinary facts are a new probe even if their timestamp does not move.
                reprobe(&store, &file).await;
                assert!(
                    demand(&store, file.id, &scanner).await.probe_revision > current.probe_revision
                );
                assert!(aggregate(&store, file.id).await.is_null());
                now += 40_000;
            }
        }
    })
    .await;
}

#[tokio::test]
async fn title_source_only_invalidation_cannot_be_undone_by_any_decoration() {
    for_each_strandable_backend(|store, backend, raw| async move {
        let scanner = "a".repeat(64);
        for (index, kind) in DECORATIONS.into_iter().enumerate() {
            let mut file = seed(&store, &format!("source-graft-{kind:?}")).await;
            let now = 1_000 + index as i64 * 40_000;
            let (current, job) = active(&store, file.id, &scanner, now).await;
            // Retain the real ordinary facts, as a source-only external catalogue update does.
            match raw.as_ref() {
                StrandedFieldOrder::Sqlite(path) => {
                    let conn = rusqlite::Connection::open(path).expect("raw SQLite");
                    conn.execute(
                        "UPDATE files SET size=size+1,mtime=mtime+1 WHERE id=?1",
                        rusqlite::params![file.id],
                    )
                    .expect("source-only change");
                }
                #[cfg(feature = "hiqlite-contract-tests")]
                StrandedFieldOrder::Hiqlite(client) => {
                    client
                        .execute(
                            "UPDATE files SET size=size+1,mtime=mtime+1 WHERE id=$1",
                            hiqlite::params!(file.id),
                        )
                        .await
                        .expect("replicated source-only change");
                }
            }
            file.size += 1;
            file.mtime += 1;
            for value in [1, 1, 2] {
                decorate(&store, &file, kind, value).await;
                assert!(
                    !store
                        .dv_title_demands(&scanner, 0)
                        .await
                        .expect("no stale demand")
                        .iter()
                        .any(|d| d.file_id == file.id),
                    "{backend}: {kind:?}"
                );
                assert!(aggregate(&store, file.id).await.is_null());
            }
            assert!(matches!(
                store
                    .publish_dv_title_job(publication(&current, &job, &scanner, now + 1))
                    .await
                    .expect("stale attempt"),
                JobPublishOutcome::LostOwnership
            ));
            reprobe(&store, &file).await;
            let refreshed = demand(&store, file.id, &scanner).await;
            assert!(refreshed.probe_revision > current.probe_revision);
            assert_eq!(
                (refreshed.source_size, refreshed.source_mtime),
                (file.size, file.mtime)
            );
        }
    })
    .await;
}

#[tokio::test]
async fn title_fresh_source_and_identical_probe_upserts_rotate_atomically_in_both_paths() {
    for_each_backend(|store, backend| async move {
        let scanner = "a".repeat(64);
        for fenced in [false, true] {
            let mut file = seed(&store, &format!("fresh-source-{fenced}")).await;
            let mut previous = bound(&store, file.id, &scanner).await.probe_revision;
            let now = i64::try_from(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("clock")
                    .as_millis(),
            )
            .expect("Unix milliseconds");
            let mut lease = if fenced {
                Some(acquired(
                    store
                        .acquire_lease("repair:probe", "direct-probe", now, now + 200_000)
                        .await
                        .expect("coordinator"),
                    backend,
                ))
            } else {
                None
            };
            for change_source in [false, true, false] {
                if change_source {
                    file.size += 1;
                    file.mtime += 1;
                }
                if fenced {
                    let current = lease.as_ref().expect("fenced lease");
                    let replacement = current.publication_successor().expect("successor");
                    store
                        .upsert_file_fenced(
                            file.item_id,
                            file.path.to_str().expect("path"),
                            file.size,
                            file.mtime,
                            &probe(),
                            current,
                            &replacement,
                        )
                        .await
                        .expect("fenced fresh probe");
                    lease = Some(replacement);
                } else {
                    reprobe(&store, &file).await;
                }
                let fresh = demand(&store, file.id, &scanner).await;
                assert!(
                    fresh.probe_revision > previous,
                    "{backend}: fenced={fenced}"
                );
                assert_eq!(
                    (fresh.source_size, fresh.source_mtime),
                    (file.size, file.mtime)
                );
                previous = fresh.probe_revision;
            }
        }
    })
    .await;
}

#[tokio::test]
async fn title_command_probe_application_rotates_only_on_admitted_ordinary_publication() {
    for_each_backend(|store, backend| async move {
        let scanner = "a".repeat(64);
        let file = seed(&store, "command-probe-title").await;
        let (current, title_job) = active(&store, file.id, &scanner, 1_000).await;
        assert!(matches!(
            store
                .publish_dv_title_job(publication(&current, &title_job, &scanner, 1_001))
                .await
                .expect("title publish"),
            JobPublishOutcome::Published { .. }
        ));
        let coordinator = acquired(
            store
                .acquire_lease("repair:probe", "probe-coordinator", 1_000, 200_000)
                .await
                .expect("coordinator"),
            backend,
        );
        let probe_coordinator = ProbeCoordinator::from_lease(&coordinator);
        let probe_digest = "a".repeat(64);
        let payload = JobPayload::MediaProbe {
            file_id: file.id,
            source_generation: generation(
                file.id,
                file.size,
                file.mtime,
                &probe_coordinator,
                &probe_digest,
            ),
            probe_digest,
            source_size: file.size,
            source_mtime: file.mtime,
            coordinator: probe_coordinator,
        };
        let id = uuid::Uuid::new_v4().to_string();
        assert!(matches!(
            store
                .enqueue_job(EnqueueJob {
                    id: id.clone(),
                    payload: payload.clone(),
                    dedupe_key: format!("probe:{id}"),
                    priority: 1,
                    not_before_ms: 2_000,
                    now_ms: 2_000,
                    request: JobRequest {
                        scope: "probe-contract".into(),
                        request_id: id.clone(),
                        request_digest: "b".repeat(64),
                        consumer_kind: "probe".into(),
                        consumer_ref: file.id.to_string(),
                        target_node_id: None,
                        deadline_ms: None,
                        retain_identity: false
                    },
                })
                .await
                .expect("probe enqueue"),
            EnqueueOutcome::Accepted { .. }
        ));
        let ClaimOutcome::Claimed { job } = store
            .claim_artifact_job(ClaimJob {
                job_id: id.clone(),
                expected_revision: 0,
                node_id: "leaf-worker".into(),
                boot_id: uuid::Uuid::new_v4().to_string(),
                claim_id: uuid::Uuid::new_v4().to_string(),
                kind: JobKind::MediaProbe,
                payload_version: 1,
                now_ms: 2_000,
                dispatched_at_ms: 2_000,
            })
            .await
            .expect("probe claim")
        else {
            panic!("{backend}: probe claim");
        };
        let publication = PublishProbeJob {
            token: job.token.expect("probe token"),
            output: ProbeOutput {
                source: payload,
                probe: probe(),
            },
            now_ms: 2_001,
        };
        assert!(matches!(
            store
                .publish_probe_job(publication.clone())
                .await
                .expect("facts publish"),
            JobPublishOutcome::Published { .. }
        ));
        // Worker result publication is not catalogue application.
        assert!(!aggregate(&store, file.id).await.is_null());
        let apply = ApplyProbeJob {
            job_id: id,
            lease: coordinator,
            now_ms: 2_002,
        };
        let mut refused = apply.clone();
        refused.lease.fence += 1;
        assert!(!store
            .apply_probe_job(refused)
            .await
            .expect("wrong coordinator"));
        assert!(!aggregate(&store, file.id).await.is_null());
        assert!(store
            .apply_probe_job(apply.clone())
            .await
            .expect("ordinary publication"));
        let fresh = demand(&store, file.id, &scanner).await;
        assert!(fresh.probe_revision > current.probe_revision);
        assert!(aggregate(&store, file.id).await.is_null());
        assert!(matches!(
            store
                .publish_probe_job(publication)
                .await
                .expect("worker replay"),
            JobPublishOutcome::AlreadyPublished { .. }
        ));
        assert_eq!(
            demand(&store, file.id, &scanner).await.probe_revision,
            fresh.probe_revision
        );
        assert!(store
            .apply_probe_job(apply)
            .await
            .expect("identical catalogue reprobe"));
        assert!(demand(&store, file.id, &scanner).await.probe_revision > fresh.probe_revision);
    })
    .await;
}
