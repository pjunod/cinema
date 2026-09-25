use super::SqliteStore;
use crate::error::StoreError;
use crate::live_tv_resource::{Backend, Snapshot, Statement, Value, SNAPSHOT_SQL};
use async_trait::async_trait;

#[async_trait]
impl Backend for SqliteStore {
    async fn read_ledger(&self, user_id: i64, key: &str, now: i64) -> Result<Snapshot, StoreError> {
        let key = key.to_owned();
        self.with_conn(move |conn| {
            let payload: String =
                conn.query_row(SNAPSHOT_SQL, rusqlite::params![user_id, key, now], |row| {
                    row.get(0)
                })?;
            serde_json::from_str(&payload)
                .map_err(|e| StoreError::Database(format!("reading Live TV authority: {e}")))
        })
        .await
    }
    async fn commit_ledger(&self, statements: Vec<Statement>) -> Result<bool, StoreError> {
        self.with_conn(move |conn| {
            let txn = conn.unchecked_transaction()?;
            let mut applied = false;
            for (index, statement) in statements.into_iter().enumerate() {
                let values = statement
                    .values
                    .into_iter()
                    .map(|v| match v {
                        Value::Text(s) => rusqlite::types::Value::Text(s),
                        Value::Integer(i) => rusqlite::types::Value::Integer(i),
                    })
                    .collect::<Vec<_>>();
                let changed = txn.execute(&statement.sql, rusqlite::params_from_iter(values))?;
                if index == 0 {
                    applied = changed == 1;
                    if !applied {
                        break;
                    }
                }
            }
            txn.commit()?;
            Ok(applied)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live_tv_resource::{
        Command, Ingest, LiveTvResourceStore, Outcome, Record, Reserve, Start, StartPhase, Worker,
        HISTORY_MS,
    };
    use crate::store::SettingsStore;
    fn worker(node: &str) -> Worker {
        Worker {
            node_id: node.into(),
            boot_id: format!("boot-{node}"),
        }
    }
    fn start(id: &str, channel: &str) -> Start {
        Start {
            user_id: 1,
            request_id: id.into(),
            digest: format!("payload-{channel}"),
            generation: 1,
            device_id: "device".into(),
            channel_id: channel.into(),
            issued_at_ms: 1000,
            admission_until_ms: 301000,
            epoch: 0,
            worker: None,
            ingest_id: None,
            phase: StartPhase::Issued,
            response: None,
        }
    }
    fn reserve(id: &str, channel: &str, node: &str) -> Command {
        Command::Reserve(Reserve {
            start: start(id, channel),
            worker: worker(node),
            ingest_id: format!("ingest-{node}-{id}"),
            limit: 1,
        })
    }
    async fn store() -> SqliteStore {
        let store = SqliteStore::open_in_memory().expect("store");
        store
            .put_settings(&[("live_tv.enabled", "1"), ("live_tv.config_generation", "1")])
            .await
            .expect("settings");
        store
    }
    fn admitted(value: Outcome) -> Start {
        match value {
            Outcome::Record(Record::Start(s)) => s,
            other => panic!("not admitted: {other:?}"),
        }
    }
    async fn ingest(store: &SqliteStore) -> Ingest {
        store
            .live_tv_resource_snapshot(0, "", 1001)
            .await
            .expect("snapshot")
            .records
            .into_iter()
            .find_map(|r| match r {
                Record::Ingest(i) => Some(i),
                _ => None,
            })
            .expect("ingest")
    }
    #[tokio::test]
    async fn cluster_capacity_is_atomic_and_channel_consumers_share_the_winner() {
        let store = store().await;
        let a = "11111111111111111111111111111111";
        let b = "22222222222222222222222222222222";
        let (one, two) = tokio::join!(
            store.live_tv_resource_command(reserve(a, "2.1", "a"), 1000),
            store.live_tv_resource_command(reserve(b, "2.1", "b"), 1000)
        );
        let one = admitted(one.expect("first"));
        let two = admitted(two.expect("second"));
        assert_eq!(one.ingest_id, two.ingest_id);
        assert_eq!(one.worker, two.worker);
        assert_eq!(ingest(&store).await.consumers.len(), 2);
        assert_eq!(
            store
                .live_tv_resource_command(
                    reserve("33333333333333333333333333333333", "4.1", "b"),
                    1000
                )
                .await
                .expect("refusal"),
            Outcome::Capacity
        );
        store
            .live_tv_resource_command(
                Command::Retire {
                    user_id: 1,
                    request_id: a.into(),
                },
                1001,
            )
            .await
            .expect("retire");
        let shared = ingest(&store).await;
        assert!(!shared.draining);
        assert_eq!(shared.consumers.len(), 1);
        store
            .live_tv_resource_command(
                Command::Retire {
                    user_id: 1,
                    request_id: b.into(),
                },
                1002,
            )
            .await
            .expect("retire last");
        assert!(ingest(&store).await.draining);
        assert_eq!(
            store
                .live_tv_resource_command(
                    Command::Release {
                        ingest_id: shared.id.clone(),
                        epoch: shared.epoch,
                        worker: worker("wrong")
                    },
                    1003
                )
                .await
                .expect("stale release"),
            Outcome::Fenced
        );
        assert_eq!(
            store
                .live_tv_resource_command(
                    Command::Release {
                        ingest_id: shared.id,
                        epoch: shared.epoch,
                        worker: shared.worker
                    },
                    1003
                )
                .await
                .expect("release"),
            Outcome::Applied
        );
    }
    #[tokio::test]
    async fn ticket_retirement_survives_history_gc_without_legacy_fallback() {
        let store = store().await;
        let id = "v4_11111111111111111111111111111111";
        store
            .live_tv_resource_command(Command::Issue(start(id, "2.1")), 1000)
            .await
            .expect("issue");
        store
            .live_tv_resource_command(
                Command::Retire {
                    user_id: 1,
                    request_id: id.into(),
                },
                1001,
            )
            .await
            .expect("retire before reserve");
        assert_eq!(
            store
                .live_tv_resource_command(reserve(id, "2.1", "a"), 1002)
                .await
                .expect("retired"),
            Outcome::Retired
        );
        assert_eq!(
            store
                .live_tv_resource_command(reserve(id, "2.1", "a"), HISTORY_MS + 2000)
                .await
                .expect("expired history"),
            Outcome::Expired
        );
        assert!(store
            .live_tv_resource_snapshot(1, id, HISTORY_MS + 2000)
            .await
            .expect("snapshot")
            .records
            .iter()
            .all(|r| !matches!(r, Record::Ingest(_))));
    }
    #[tokio::test]
    async fn generation_boot_and_revision_fence_late_renewals() {
        let store = store().await;
        store
            .live_tv_resource_command(
                reserve("11111111111111111111111111111111", "2.1", "a"),
                1000,
            )
            .await
            .expect("reserve");
        let i = ingest(&store).await;
        let renew = Command::Renew {
            ingest_id: i.id.clone(),
            epoch: i.epoch,
            revision: i.revision,
            worker: i.worker.clone(),
        };
        assert!(matches!(
            store
                .live_tv_resource_command(renew.clone(), 1001)
                .await
                .expect("renew"),
            Outcome::Record(Record::Ingest(_))
        ));
        assert_eq!(
            store
                .live_tv_resource_command(renew, 1002)
                .await
                .expect("stale revision"),
            Outcome::Fenced
        );
        store
            .put_settings(&[("live_tv.config_generation", "2")])
            .await
            .expect("reconfigure");
        let i = ingest(&store).await;
        assert_eq!(
            store
                .live_tv_resource_command(
                    Command::Renew {
                        ingest_id: i.id,
                        epoch: i.epoch,
                        revision: i.revision,
                        worker: i.worker
                    },
                    1003
                )
                .await
                .expect("stale config"),
            Outcome::Fenced
        );
    }
}
