//! Singleton bounded placement; the ordinary durable consumers own all I/O.
use super::{clock_ms, JobManager};
use plurx_core::error::StoreError;
use plurx_core::store::background_jobs::{EnqueueJob, EnqueueOutcome, JobRequest};
use sha2::{Digest, Sha256};

fn choose_target<'a>(key: &str, nodes: &'a [String], holders: &[String]) -> Option<&'a str> {
    nodes
        .iter()
        .filter(|node| !holders.contains(node))
        .max_by_key(|node| Sha256::digest(format!("{key}\0{node}")).to_vec())
        .map(String::as_str)
}

pub(super) struct Prediction {
    pub discovery: crate::produce::DiscoveryCandidate,
    pub file_id: Option<i64>,
}

impl JobManager {
    pub(super) async fn prediction_discoveries(&self) -> Result<Vec<Prediction>, StoreError> {
        use crate::produce::{self, DiscoveryCandidate};
        use plurx_core::store::background_jobs_preparation::PreparationDemand;
        let mut immediate = Vec::new();
        let mut hot = Vec::new();
        for demand in self.store.preparation_demands(clock_ms()).await? {
            match demand {
                PreparationDemand::Item { item_id, title } => hot.push(Prediction {
                    discovery: DiscoveryCandidate {
                        item_id,
                        title,
                        reason: produce::REASON_HOT,
                    },
                    file_id: None,
                }),
                PreparationDemand::Channel {
                    item_id,
                    file_id,
                    title,
                    ..
                } => immediate.push(Prediction {
                    discovery: DiscoveryCandidate {
                        item_id,
                        title,
                        reason: produce::REASON_CHANNEL,
                    },
                    file_id: Some(file_id),
                }),
                PreparationDemand::Viewer {
                    user_id,
                    next_item_id,
                    next_title,
                } => {
                    // One coherent read and at most one next episode for each
                    // currently leased viewer; dormant accounts add no work.
                    let rails = self.store.progress_rails(user_id, 1).await?;
                    for (reason, items) in [
                        (
                            produce::REASON_IN_PROGRESS,
                            rails
                                .continue_watching
                                .into_iter()
                                .map(|row| row.item)
                                .collect::<Vec<_>>(),
                        ),
                        (
                            produce::REASON_NEXT_UP,
                            if next_item_id.is_some() {
                                Vec::new()
                            } else {
                                rails
                                    .next_up
                                    .into_iter()
                                    .map(|row| row.item)
                                    .collect::<Vec<_>>()
                            },
                        ),
                    ] {
                        immediate.extend(items.into_iter().map(|item| Prediction {
                            discovery: DiscoveryCandidate {
                                item_id: item.id,
                                title: item.title,
                                reason,
                            },
                            file_id: None,
                        }));
                    }
                    if let (Some(item_id), Some(title)) = (next_item_id, next_title) {
                        immediate.push(Prediction {
                            discovery: DiscoveryCandidate {
                                item_id,
                                title,
                                reason: produce::REASON_NEXT_UP,
                            },
                            file_id: None,
                        });
                    }
                }
            }
        }
        let predictions = immediate.into_iter().chain(hot).collect::<Vec<_>>();
        let ranked = produce::rank_discovery(
            &[predictions
                .iter()
                .map(|prediction| prediction.discovery.clone())
                .collect()],
            super::PRODUCE_DISCOVERY_ITEMS,
        );
        Ok(ranked
            .into_iter()
            .map(|discovery| Prediction {
                file_id: predictions
                    .iter()
                    .find(|prediction| prediction.discovery.item_id == discovery.item_id)
                    .and_then(|prediction| prediction.file_id),
                discovery,
            })
            .collect())
    }

    pub(super) async fn prepare_predictions(&self) -> Result<(), StoreError> {
        use plurx_core::store::NewAnalysisRequest;
        let Some(lease) = self.acquire_job("candidate:prediction".into()).await? else {
            return Ok(());
        };
        let publisher = lease.publisher(self.store.as_ref());
        let result: Result<(), StoreError> = async {
            let predictions = self.prediction_discoveries().await?;
            let mut nodes = if let Some(membership) = &self.membership {
                membership.operations_peers().await.map_err(|error| StoreError::Task(error.to_string()))?
                    .into_iter().map(|peer| peer.node_id).collect::<Vec<_>>()
            } else { Vec::new() };
            nodes.push(self.coordinator.node_id().into());
            nodes.sort(); nodes.dedup();
            let target = nodes.first().expect("local node exists");
            let indexes = self.cluster_fragment_index_enabled().await;
            let subtitles = self.subtitle_source_queue_enabled().await;
            let index_pipeline = if indexes { crate::ffmpeg::fragment_index_engine_digest().await } else { String::new() };
            let subtitle_pipeline = if subtitles { super::subtitle_source_pipeline_version().await } else { String::new() };
            let now = clock_ms();
            let mut requests = Vec::new();
            let mut desired_files = Vec::new();
            for prediction in predictions {
                let file = if let Some(id) = prediction.file_id {
                    self.store.get_file(id).await?
                } else {
                    self.store.files_for_item(prediction.discovery.item_id).await?.into_iter().next()
                };
                let Some(file) = file else { continue; };
                desired_files.push(file.id);
                for (component, pipeline, node, needed) in [
                    ("fragment_index", &index_pipeline, target.as_str(), indexes && file.video_codec.is_some()),
                    ("subtitle_source", &subtitle_pipeline, "", subtitles && !file.subtitle_streams.is_empty()),
                ] {
                    if !needed || requests.len() >= 64 { continue; }
                    let id = hex::encode(Sha256::digest(format!("plurx/prediction/v1:{}:{}:{}:{component}:{pipeline}:{node}", file.id, file.size, file.mtime)));
                    requests.push(NewAnalysisRequest {
                        request_id: id.clone(), file_id: file.id, source_size: file.size, source_mtime: file.mtime,
                        component: component.into(), pipeline_version: pipeline.clone(), video_identity: String::new(),
                        requested_generation: format!("predict:{id}"), priority: "normal".into(), trigger: "background".into(),
                        force_rebuild: false, target_node_id: node.into(), not_before_ms: now, created_at_ms: now,
                    });
                }
            }
            publisher.sync_predictions(requests, desired_files).await?;
            // The replicated intent precedes domain admission. If this owner
            // crashes here another pass drains the same outbox; a retired
            // intent cannot pass either backend's insert predicate.
            for request in self.store.pending_predictions(clock_ms()).await? {
                match self.store.enqueue_analysis_request(&request).await {
                    Ok(_) => {},
                    Err(error) => tracing::debug!(%error, file = request.file_id, component = request.component, "prediction outbox deferred"),
                }
            }
            Ok(())
        }.await;
        drop(publisher);
        let release = lease.release().await;
        result?;
        release.map(|_| ())
    }

    pub(super) async fn prepare_hot_copies(&self) -> Result<(), StoreError> {
        let Some(membership) = &self.membership else {
            return Ok(());
        };
        let mut nodes = membership
            .operations_peers()
            .await
            .map_err(|error| StoreError::Task(error.to_string()))?
            .into_iter()
            .filter(|peer| peer.reachable && peer.http_base.is_some())
            .map(|peer| peer.node_id)
            .collect::<Vec<_>>();
        nodes.push(self.coordinator.node_id().into());
        nodes.sort();
        nodes.dedup();
        nodes.truncate(64);
        if nodes.len() < 2 {
            return Ok(());
        }
        let Some(lease) = self.acquire_job("candidate:hot-copy".into()).await? else {
            return Ok(());
        };
        let lost = lease.loss_token();
        let publisher = lease.publisher(self.store.as_ref());
        let result: Result<(), StoreError> = async {
            let now = clock_ms();
            let artifacts = self.store.hot_artifacts(now).await?;
            let budget = crate::cachekeep::budget_bytes_fallible(&self.store).await?;
            let mut available = std::collections::HashMap::new();
            if let Some(budget) = budget {
                for node in &nodes {
                    available.insert(
                        node.clone(),
                        budget
                            .saturating_sub(self.store.cache_bytes(node).await?)
                            .max(0),
                    );
                }
            }
            let mut admitted = 0;
            for artifact in artifacts {
                if lost.is_cancelled() || admitted >= 16 {
                    break;
                }
                let live_holders = artifact
                    .holders
                    .iter()
                    .filter(|node| nodes.contains(node))
                    .cloned()
                    .collect::<Vec<_>>();
                if live_holders.len() != 1 || artifact.pending_copy {
                    continue;
                }
                let candidates = nodes
                    .iter()
                    .filter(|node| {
                        !artifact.artifact_key.starts_with("transcode:")
                            || available
                                .get(*node)
                                .is_some_and(|free| *free >= artifact.bytes)
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                let Some(target) =
                    choose_target(&artifact.artifact_key, &candidates, &live_holders)
                else {
                    continue;
                };
                // One bounded recovery cycle per target/hour. A failed receipt
                // cannot reset its budget on every scheduler tick.
                let request_id = hex::encode(Sha256::digest(format!(
                    "{}:{target}:{}",
                    artifact.artifact_key,
                    now / 3_600_000
                )));
                let key = artifact.artifact_key;
                let (payload, digest) =
                    plurx_core::store::background_jobs::hydration_identity(&key, target)?;
                let request = EnqueueJob {
                    id: uuid::Uuid::new_v4().to_string(),
                    payload,
                    dedupe_key: format!("hydrate:{digest}"),
                    priority: 0,
                    not_before_ms: now,
                    now_ms: now,
                    request: JobRequest {
                        scope: "automatic:hot-copy".into(),
                        request_id,
                        request_digest: hex::encode(Sha256::digest(format!("{key}:{target}"))),
                        consumer_kind: "hot_copy".into(),
                        consumer_ref: key,
                        target_node_id: Some(target.into()),
                        deadline_ms: Some(
                            now.saturating_add(20 * 60 * 1000)
                                .min(artifact.demanded_at_ms.saturating_add(86_400_000)),
                        ),
                        retain_identity: false,
                    },
                };
                match publisher.enqueue_hot_copy(request).await? {
                    EnqueueOutcome::Accepted { .. } => {
                        admitted += 1;
                        if let Some(free) = available.get_mut(target) {
                            *free = free.saturating_sub(artifact.bytes).max(0);
                        }
                    }
                    EnqueueOutcome::QueueFull => break,
                    _ => {}
                }
            }
            Ok(())
        }
        .await;
        drop(publisher);
        let release = lease.release().await;
        result?;
        release.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hot_copy_target_is_stable_and_never_selects_an_existing_holder() {
        let nodes = vec!["a".into(), "b".into(), "c".into()];
        let holders = vec!["a".into()];
        let selected = choose_target("artifact", &nodes, &holders).expect("spare node");
        assert_ne!(selected, "a");
        let mut reversed = nodes.clone();
        reversed.reverse();
        assert_eq!(
            choose_target("artifact", &reversed, &holders),
            Some(selected)
        );
        assert_eq!(choose_target("artifact", &nodes, &nodes), None);
    }
}
