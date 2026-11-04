//! Explicit command adapter. Normal local boot never enters this module.

use plurx_core::cluster::migration::native_promotion::{
    promote_native_storage, NativePromotionReceipt,
};
use plurx_core::config::Config;
use plurx_core::storage::NativeDirectoryLock;

pub(crate) async fn create(config: &Config) -> anyhow::Result<NativePromotionReceipt> {
    config.validate_cluster_runtime()?;
    let lock = NativeDirectoryLock::acquire(&config.storage.data_dir).await?;
    let observation = crate::StartupObservationHttp::new(config.server.bind);
    let outcome = promote_native_storage(config, Some(&observation), lock).await;
    let result = match outcome {
        Ok((selected, receipt)) => {
            let selected = super::PendingSelection::new(selected);
            let drained = selected.shutdown().await;
            if let Err(error) = drained {
                // PendingSelection retains the actual resources on failure
                // or cancellation during this awaited shutdown.
                Err(error.into())
            } else {
                drop(selected.into_selected());
                Ok(receipt)
            }
        }
        Err(error) => Err(error.into()),
    };
    observation.stop_and_drain().await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::storage::mode::{StorageManifest, StorageMode};
    use plurx_core::storage::open_native_store;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn managed_cluster_create_preserves_native_source_keys_and_restart_writes() {
        tokio::time::timeout(std::time::Duration::from_secs(240), async {
            let directory = crate::test_tempdir().expect("native installation");
            let listeners = (0..3)
                .map(|_| std::net::TcpListener::bind("127.0.0.1:0").expect("test listener"))
                .collect::<Vec<_>>();
            let addresses = listeners
                .iter()
                .map(|listener| listener.local_addr().expect("test address"))
                .collect::<Vec<_>>();
            let mut config = Config::default();
            config.storage.data_dir = directory.path().to_path_buf();
            config.server.bind = addresses[0];
            config.cluster.raft_bind = addresses[1];
            config.cluster.api_bind = addresses[2];
            drop(listeners);
            let native = open_native_store(&config)
                .await
                .expect("genuine native startup");
            let source = native
                .sqlite
                .prepare_native_source_before_serving(
                    &native.identity.node_id,
                    &native.credential_key,
                )
                .await
                .expect("native Source and sealed purpose keys");
            source.revoke();
            drop(source);
            native
                .store
                .put_setting("promotion.native-value", "native application bytes")
                .await
                .expect("native write");
            let telemetry=rusqlite::Connection::open(directory.path().join("plurx.db")).expect("native local observations");
            telemetry.execute("INSERT INTO playback_events(id,at_unix_ms,event) VALUES(77,123,'preserved-mode-transition')",[]).expect("local event");
            telemetry.execute("INSERT INTO rendition_plans VALUES('retained-native-plan',4,1024,55,'argv',1,90000,6,1,?1,123)",[&[0u8,255,16,0][..]]).expect("packed normative plan bytes");
            drop(telemetry);
            let identity = native.identity.clone();
            let master_key_id = native.credential_key.id().to_owned();
            drop(native);
            let receipt = create(&config)
                .await
                .expect("explicit managed create through actual Raft import");
            assert_eq!(receipt.source.instance_id, identity.instance_id);
            assert_eq!(receipt.source.node_id, identity.node_id);
            assert_eq!(receipt.target.mode, StorageMode::Cluster);
            for table in ["playback_events","rendition_plans"] {
                assert!(receipt.local_telemetry.tables.iter().any(|entry|entry.table==table&&entry.rows==1),"live node-local {table} parity");
            }
            for table in [
                "sharing_catalogue_keys",
                "sharing_file_locator_keys",
                "sharing_source_schema_installation",
            ] {
                assert!(
                    receipt
                        .import
                        .tables
                        .iter()
                        .any(|entry| entry.table == table && entry.row_count > 0),
                    "nonempty native {table} copied and hash verified"
                );
            }
            assert_eq!(
                StorageManifest::read(directory.path()).expect("published mode"),
                Some(receipt.target)
            );
            assert!(
                !directory.path().join("plurx.db").exists(),
                "old binaries cannot import a stale canonical native database"
            );
            config.storage.mode = Some(StorageMode::Cluster);
            for boot in 0..2 {
                let observation = crate::StartupObservationHttp::new(config.server.bind);
                let lock = NativeDirectoryLock::acquire(directory.path())
                    .await
                    .expect("restart lease");
                let selected = super::super::open(&config, Some(&observation), lock)
                    .await
                    .expect("ordinary explicit cluster restart");
                assert_eq!(selected.identity.cluster_id, identity.instance_id);
                assert_eq!(selected.identity.node_id, identity.node_id);
                assert_eq!(selected.credential_key.id(), master_key_id);
                assert_eq!(
                    selected
                        .store
                        .get_setting("promotion.native-value")
                        .await
                        .expect("imported setting"),
                    Some("native application bytes".to_owned())
                );
                let local=rusqlite::Connection::open(directory.path().join("hiqlite/telemetry.db")).expect("live local sidecar");
                assert_eq!(local.query_row("SELECT event FROM playback_events WHERE id=77",[],|row|row.get::<_,String>(0)).expect("live event"),"preserved-mode-transition");
                assert_eq!(local.query_row("SELECT entries_packed FROM rendition_plans WHERE rendition_key='retained-native-plan'",[],|row|row.get::<_,Vec<u8>>(0)).expect("live normative bytes"),vec![0,255,16,0]);
                drop(local);
                if boot == 0 {
                    selected
                        .store
                        .put_setting("promotion.cluster-value", "accepted after handoff")
                        .await
                        .expect("real cluster write");
                } else {
                    assert_eq!(
                        selected
                            .store
                            .get_setting("promotion.cluster-value")
                            .await
                            .expect("restart setting"),
                        Some("accepted after handoff".to_owned())
                    );
                }
                if boot == 1 {
                    let expected=plurx_core::error::StoreError::Migration("test handled post-open refusal".into());
                    let message=expected.to_string();
                    let error=super::super::PendingSelection::new(selected).refuse(expected).await;
                    assert_eq!(error.to_string(),message,"actual terminal drain returns the original refusal");
                    let lease=NativeDirectoryLock::try_acquire(directory.path()).expect("terminal refusal released its actual root lease");
                    drop(lease);
                } else {
                    selected.shutdown().await.expect("terminal cluster drain");
                    drop(selected);
                }
                observation.stop_and_drain().await;
            }
        })
        .await
        .expect("managed create and restart bounded");
    }
}
