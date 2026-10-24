//! Startup-only closure of exact old daemon-local direct domains. No native
//! descriptor is reopened and no current-runtime refund capability is minted.
use super::*;

pub(super) struct Claim {
    pub(super) obligation: Uuid,
    pub(super) graph: Uuid,
    pub(super) prior_operation: Uuid,
    pub(super) prior_sequence: i64,
}

pub(super) fn close_previous(
    storage: &mut storage::Storage,
    options: &OpenOptions,
    mut snapshot: RecoverySnapshot,
) -> Result<RecoverySnapshot, CustodyError> {
    // Register once, preserving every old immutable identity. The held lock
    // spans registration, native proof, COMMIT and full reconstruction.
    transaction::register_owner(&mut storage.connection, &options.owner)?;
    for sources in [true, false] {
        let candidates = snapshot
            .obligations
            .iter()
            .filter_map(|record| {
                let ObligationInventory::TypedV2 { manifest, .. } = &record.inventory else {
                    return None;
                };
                let dependency = codec_v2::direct_daemon_scope(manifest).ok()?;
                (dependency.is_some() == sources && record.trial_group_id.is_none())
                    .then_some(record.obligation_id)
            })
            .collect::<Vec<_>>();
        for obligation in candidates {
            let Some(record) = snapshot
                .obligations
                .iter()
                .find(|r| r.obligation_id == obligation)
            else {
                continue;
            };
            let Ok(exit) =
                recovery::prove_previous_daemon_exit(options, &snapshot, record.owner_id)
            else {
                // A live/unknown/foreign owner remains charged. This is not a
                // reason to discard other independent positive closures.
                continue;
            };
            let ObligationInventory::TypedV2 { manifest, .. } = &record.inventory else {
                continue;
            };
            if codec_v2::direct_daemon_scope(manifest)?.is_none()
                && snapshot.obligations.iter().any(|candidate| {
                    matches!(&candidate.inventory,
                        ObligationInventory::TypedV2 { manifest, .. }
                        if codec_v2::direct_daemon_scope(manifest).ok().flatten()
                            .is_some_and(|domain| domain.uuid() == obligation))
                })
            {
                continue;
            }
            let prior_operation: Vec<u8> = storage.connection.query_row(
                "SELECT last_operation_id FROM obligations WHERE obligation_id=?1",
                [obligation.as_bytes()],
                |row| row.get(0),
            )?;
            let operation = QueuedOperation {
                id: Uuid::new_v4(),
                deadline: Instant::now() + std::time::Duration::from_secs(30),
                command: Command::ClosePreviousDirect {
                    claim: Claim {
                        obligation,
                        graph: record.graph_id,
                        prior_operation: codec::uuid_from_bytes(&prior_operation)?,
                        prior_sequence: record.sequence,
                    },
                    exit,
                },
            };
            // A storage/unknown COMMIT error aborts startup. No ledger has yet
            // been restored or exposed, and the original durable hold remains
            // authoritative until the next validated journal reconstruction.
            let OperationResult::Changed(ack) =
                transaction::execute(storage, &operation, options.owner.owner_id)?
            else {
                return Err(codec::invalid());
            };
            if ack.obligation_id != obligation || ack.phase != Phase::Settled {
                return Err(codec::invalid());
            }
            snapshot = transaction::snapshot(&storage.connection, &storage.witness)?;
        }
    }
    Ok(snapshot)
}
