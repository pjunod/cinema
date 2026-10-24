//! A distinct audit for a private native old-daemon exit command. Audit JSON
//! is validated as history and never accepted as a live physical close proof.
use super::*;
use crate::resource_custody::codec_v2 as v2;

fn owner_for_audit(connection: &Connection, id: Uuid) -> Result<OwnerIdentity, CustodyError> {
    let row: (String, String, String, String, String) = connection.query_row(
        "SELECT node_id,cluster_id,os_boot_json,daemon_process_json,platform_domain_json FROM owners WHERE owner_id=?1",
        [id.as_bytes()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
    )?;
    let owner = OwnerIdentity {
        owner_id: id,
        node_id: row.0,
        cluster_id: row.1,
        os_boot: codec::decode(&row.2)?,
        daemon_process: codec::decode(&row.3)?,
        platform_domain: codec::decode(&row.4)?,
    };
    validate_owner(&owner)?;
    Ok(owner)
}

pub(super) fn validate_audit(
    connection: &Connection,
    obligation: Uuid,
    manifest: &v2::Manifest,
    audit: &v2::DirectDaemonExit,
) -> Result<(), CustodyError> {
    v2::validate_direct_daemon_exit(manifest, audit)?;
    let old = owner_for_audit(connection, audit.owner_id.uuid())?;
    let closing = owner_for_audit(connection, audit.closing_owner_id.uuid())?;
    let known_boot = matches!((&old.os_boot, &closing.os_boot),
        (BootIdentity::Known { value: old, provider: old_provider, version: 1 },
         BootIdentity::Known { value: current, provider: current_provider, version: 1 })
        if old == current && old_provider == "linux_kernel_boot_id" && current_provider == old_provider);
    if !known_boot
        || old.node_id != closing.node_id
        || old.cluster_id != closing.cluster_id
        || old.platform_domain.platform != "linux"
        || closing.platform_domain.platform != "linux"
        || old.platform_domain.continuity_provider.as_deref()
            != Some("linux_kernel_pid_namespace_v1")
        || old.platform_domain.continuity_provider != closing.platform_domain.continuity_provider
        || old.platform_domain.namespace.is_none()
        || old.platform_domain.namespace != closing.platform_domain.namespace
        || old.daemon_process.version != 1
        || closing.daemon_process.version != 1
        || old.daemon_process.domain != "local_process_observation_v1"
        || old.daemon_process.domain != closing.daemon_process.domain
        || old.daemon_process.pid == 0
        || closing.daemon_process.pid == 0
        || old
            .daemon_process
            .creation_marker
            .is_none_or(|value| value == 0)
        || closing
            .daemon_process
            .creation_marker
            .is_none_or(|value| value == 0)
        || old.daemon_process == closing.daemon_process
        || audit.prior_phase == Phase::Settled
    {
        return Err(codec::invalid());
    }
    validate_final_operation(
        connection,
        obligation,
        audit.prior_operation_id.uuid(),
        manifest.graph_id.uuid(),
        audit.prior_sequence,
        audit.prior_phase,
        None,
    )?;
    let (operation, graph, sequence, phase, trial) = obligation_tail(connection, obligation)?;
    if operation != audit.operation_id.uuid()
        || graph != manifest.graph_id.uuid()
        || sequence <= audit.prior_sequence
        || phase != Phase::Settled
        || trial.is_some()
    {
        return Err(codec::invalid());
    }
    if v2::direct_daemon_scope(manifest)?.is_none() {
        let mut statement = connection.prepare("SELECT obligation_id,phase,sequence,manifest_json FROM obligations WHERE obligation_id!=?1")?;
        let mut rows = statement.query([obligation.as_bytes()])?;
        while let Some(row) = rows.next()? {
            let json: String = row.get(3)?;
            let v2::ManifestDocument::V2(candidate) = v2::decode_manifest(&json)? else {
                continue;
            };
            if v2::direct_daemon_scope(&candidate)
                .ok()
                .flatten()
                .is_some_and(|domain| domain.uuid() == obligation)
                && (row.get::<_, String>(1)? != "settled" || row.get::<_, i64>(2)? >= sequence)
            {
                return Err(codec::invalid());
            }
        }
    }
    // The close's immutable request fingerprint must describe this exact
    // original head. Changing audit fields cannot produce matching history.
    let expected = serde_json::json!({"close_previous_direct_v1": {
        "obligation": obligation, "graph": graph, "owner": old.owner_id,
        "closing_owner": closing.owner_id,
        "prior_operation": audit.prior_operation_id.uuid(), "prior_sequence": audit.prior_sequence,
    }});
    let fingerprint = hex::encode(Sha256::digest(
        serde_json::to_vec(&expected).map_err(|_| codec::invalid())?,
    ));
    let recorded: String = connection.query_row(
        "SELECT request_json FROM operations WHERE operation_id=?1",
        [operation.as_bytes()],
        |row| row.get(0),
    )?;
    if codec::decode::<String>(&recorded)? != fingerprint {
        return Err(codec::invalid());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) fn close(
    tx: &Transaction<'_>,
    witness: &storage::Witness,
    current: Uuid,
    operation: Uuid,
    claim: &crate::resource_custody::direct_exit::Claim,
    exit: &crate::resource_custody::recovery::PositivePreviousOwnerExit,
) -> Result<DiskResult, CustodyError> {
    let recovered = snapshot(tx, witness)?;
    let current_owner = load_owner(tx, current, witness)?;
    let record = recovered
        .obligations
        .iter()
        .find(|record| record.obligation_id == claim.obligation)
        .ok_or_else(codec::invalid)?;
    let (last, graph, sequence, phase, trial) = obligation_tail(tx, claim.obligation)?;
    if last != claim.prior_operation
        || graph != claim.graph
        || sequence != claim.prior_sequence
        || phase == Phase::Settled
        || trial.is_some()
        || !exit.matches(&recovered, &current_owner, record.owner_id)
    {
        return Err(codec::invalid());
    }
    let ObligationInventory::TypedV2 { manifest, .. } = &record.inventory else {
        return Err(codec::invalid());
    };
    if v2::direct_daemon_scope(manifest)?.is_none()
        && recovered.obligations.iter().any(|candidate| matches!(&candidate.inventory,
            ObligationInventory::TypedV2 { manifest, .. }
            if v2::direct_daemon_scope(manifest).ok().flatten().is_some_and(|id| id.uuid() == claim.obligation)))
    { return Err(codec::invalid()); }
    let audit = v2::DirectDaemonExit {
        version: 2,
        kind: v2::DirectDaemonExitKind::DirectDaemonDomainExtinctV1,
        owner_id: manifest.owner_id,
        graph_id: manifest.graph_id,
        operation_id: v2::Id::new(operation)?,
        closing_owner_id: v2::Id::new(current)?,
        prior_operation_id: v2::Id::new(last)?,
        prior_sequence: sequence,
        prior_phase: phase,
        member_id: v2::Id::new(manifest.members[0].member_id())?,
    };
    v2::validate_direct_daemon_exit(manifest, &audit)?;
    let encoded = codec::encode(&audit)?;
    let sequence = next_sequence(tx)?;
    if tx.execute("UPDATE obligations SET phase='settled',sequence=?2,last_operation_id=?3,hold_reason=NULL,settlement_json=?4 WHERE obligation_id=?1 AND sequence=?5 AND last_operation_id=?6",
        params![claim.obligation.as_bytes(), sequence, operation.as_bytes(), encoded, claim.prior_sequence, claim.prior_operation.as_bytes()])? != 1
        || tx.execute("UPDATE members SET phase='closed',proof_json=?2 WHERE obligation_id=?1",
            params![claim.obligation.as_bytes(), encoded])? != 1
    { return Err(codec::invalid()); }
    Ok(DiskResult::Changed(CommitAck {
        operation_id: operation,
        obligation_id: claim.obligation,
        graph_id: graph,
        sequence,
        phase: Phase::Settled,
    }))
}
