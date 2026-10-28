//! Atomic write-ahead transitions and strict full-accounting reconstruction.
mod assets;
mod direct_exit;
mod inventory;
mod live_typed_settlement;
mod membership;
mod recovery;
mod recovery_r;
mod retained_settlement;
mod retirement;
pub(in crate::resource_custody) mod scratch;
mod typed;
use super::*;
use crate::adaptive_resources::MemoryClaim;
use codec::{MemberEnvelope, SettlementRecord};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
#[cfg(test)]
pub(in crate::resource_custody) use typed::tests::bootstrap::frozen_source_fixture_v6;

pub(super) fn validate_owner(owner: &OwnerIdentity) -> Result<(), CustodyError> {
    codec::nonnil(owner.owner_id)?;
    codec::text(&owner.node_id)?;
    codec::text(&owner.cluster_id)?;
    codec::validate_boot(&owner.os_boot)?;
    codec::validate_process(&owner.daemon_process)?;
    codec::validate_domain(&owner.platform_domain)?;
    Ok(())
}
pub(super) fn register_owner(
    connection: &mut Connection,
    owner: &OwnerIdentity,
) -> Result<(), CustodyError> {
    validate_owner(owner)?;
    let tx = connection.transaction()?;
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM owners WHERE owner_id=?1)",
        [owner.owner_id.as_bytes()],
        |r| r.get(0),
    )?;
    if exists {
        return Err(CustodyError::new(
            ErrorKind::Conflict,
            "daemon instance identity already recorded",
        ));
    }
    tx.execute(
        "UPDATE owners SET state='retired' WHERE state='current'",
        [],
    )?;
    tx.execute(
        "INSERT INTO owners VALUES(?1,?2,?3,?4,?5,?6,'current')",
        params![
            owner.owner_id.as_bytes(),
            owner.node_id,
            owner.cluster_id,
            codec::encode(&owner.os_boot)?,
            codec::encode(&owner.daemon_process)?,
            codec::encode(&owner.platform_domain)?
        ],
    )?;
    tx.execute(
        "UPDATE journal_meta SET current_owner_id=?1 WHERE singleton=1",
        [owner.owner_id.as_bytes()],
    )?;
    tx.commit()?;
    Ok(())
}

type ObligationTail = (Uuid, Uuid, i64, Phase, Option<Uuid>);

fn obligation_tail(connection: &Connection, id: Uuid) -> Result<ObligationTail, CustodyError> {
    let row: (Vec<u8>, Vec<u8>, i64, String, Option<Vec<u8>>) = connection.query_row(
        "SELECT last_operation_id,graph_id,sequence,phase,trial_group_id FROM obligations WHERE obligation_id=?1",
        [id.as_bytes()],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )?;
    Ok((
        codec::uuid_from_bytes(&row.0)?,
        codec::uuid_from_bytes(&row.1)?,
        row.2,
        Phase::parse(&row.3)?,
        row.4.map(|v| codec::uuid_from_bytes(&v)).transpose()?,
    ))
}

fn owner_exists(connection: &Connection, id: Uuid) -> Result<bool, CustodyError> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM owners WHERE owner_id=?1)",
            [id.as_bytes()],
            |r| r.get(0),
        )
        .map_err(Into::into)
}

fn load_owner(
    connection: &Connection,
    id: Uuid,
    witness: &storage::Witness,
) -> Result<OwnerIdentity, CustodyError> {
    let row: (String, String, String, String, String) = connection.query_row(
        "SELECT node_id,cluster_id,os_boot_json,daemon_process_json,platform_domain_json FROM owners WHERE owner_id=?1",
        [id.as_bytes()],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )?;
    if row.0 != witness.node_id || row.1 != witness.cluster_id {
        return Err(codec::invalid());
    }
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

fn validate_final_operation(
    connection: &Connection,
    id: Uuid,
    operation_id: Uuid,
    graph: Uuid,
    sequence: i64,
    phase: Phase,
    trial: Option<Uuid>,
) -> Result<(), CustodyError> {
    let result: String = connection.query_row(
        "SELECT result_json FROM operations WHERE operation_id=?1",
        [operation_id.as_bytes()],
        |r| r.get(0),
    )?;
    let valid = match codec::decode::<DiskResult>(&result)? {
        DiskResult::Prepared {
            sequence: prepared,
            obligations,
            trial: group,
        } => {
            phase == Phase::Prepared
                && prepared == sequence
                && group == trial
                && obligations.contains(&id)
        }
        DiskResult::Changed(ack) => {
            ack.operation_id == operation_id
                && ack.obligation_id == id
                && ack.graph_id == graph
                && ack.sequence == sequence
                && ack.phase == phase
        }
        DiskResult::AssetChangedV2(record) => {
            let ack = record.ack;
            ack.operation_id == operation_id
                && ack.obligation_id == id
                && ack.graph_id == graph
                && ack.sequence == sequence
                && ack.phase == phase
        }
        DiskResult::AssetChangedV3(record) => {
            let ack = record.ack;
            ack.operation_id == operation_id
                && ack.obligation_id == id
                && ack.graph_id == graph
                && ack.sequence == sequence
                && ack.phase == phase
        }
        DiskResult::RetiredWithAssets(ack) => {
            ack.operation_id.uuid() == operation_id
                && ack.sequence == sequence
                && ((ack.source_obligation_id.uuid() == id
                    && ack.source_graph_id.uuid() == graph
                    && phase == Phase::Settled)
                    || (ack.target_obligation_id.uuid() == id
                        && ack.target_graph_id.uuid() == graph
                        && phase == Phase::Sealed))
        }
        DiskResult::RecoveredSAdoption(_) | DiskResult::RecoveredRAdoption(_) => false,
    };
    if valid {
        Ok(())
    } else {
        Err(codec::invalid())
    }
}

fn adoption_holder_matches(
    connection: &Connection,
    table: &'static str,
    ack: &RecoveryAdoptionAck,
    operation_id: Uuid,
) -> Result<bool, CustodyError> {
    let query = match table {
        "recovered_s_holders" => "SELECT obligation_id,adoption_sequence,previous_owner_id,current_owner_id FROM recovered_s_holders WHERE operation_id=?1",
        "recovered_r_holders" => "SELECT obligation_id,adoption_sequence,previous_owner_id,current_owner_id FROM recovered_r_holders WHERE operation_id=?1",
        _ => return Err(codec::invalid()),
    };
    type AdoptionHolderRow = (Vec<u8>, i64, Vec<u8>, Vec<u8>);
    let row: Option<AdoptionHolderRow> = connection
        .query_row(query, [operation_id.as_bytes()], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .optional()?;
    let Some((obligation, sequence, previous, current)) = row else {
        return Ok(false);
    };
    Ok(ack.operation_id == operation_id
        && ack.obligation_id == codec::uuid_from_bytes(&obligation)?
        && ack.sequence == sequence
        && ack.previous_owner_id == codec::uuid_from_bytes(&previous)?
        && ack.current_owner_id == codec::uuid_from_bytes(&current)?)
}

pub(super) fn snapshot(
    connection: &Connection,
    witness: &storage::Witness,
) -> Result<RecoverySnapshot, CustodyError> {
    let (next, current): (i64, Option<Vec<u8>>) = connection.query_row(
        "SELECT next_sequence,current_owner_id FROM journal_meta WHERE singleton=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if next < 0 {
        return Err(codec::invalid());
    }
    let current = current.map(|v| codec::uuid_from_bytes(&v)).transpose()?;
    let mut current_count = 0;
    let mut stmt = connection.prepare("SELECT owner_id,node_id,cluster_id,os_boot_json,daemon_process_json,platform_domain_json,state FROM owners")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let id = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(0)?)?;
        let node: String = row.get(1)?;
        let cluster: String = row.get(2)?;
        if node != witness.node_id || cluster != witness.cluster_id {
            return Err(codec::invalid());
        }
        let identity = OwnerIdentity {
            owner_id: id,
            node_id: node,
            cluster_id: cluster,
            os_boot: codec::decode(&row.get::<_, String>(3)?)?,
            daemon_process: codec::decode(&row.get::<_, String>(4)?)?,
            platform_domain: codec::decode(&row.get::<_, String>(5)?)?,
        };
        validate_owner(&identity)?;
        let state: String = row.get(6)?;
        if state == "current" {
            if current != Some(id) {
                return Err(codec::invalid());
            }
            current_count += 1;
        } else if state != "retired" {
            return Err(codec::invalid());
        }
    }
    if current_count != usize::from(current.is_some()) {
        return Err(codec::invalid());
    }
    let mut obligations = Vec::new();
    let mut total = ResourceVector::default();
    let mut trials = BTreeSet::new();
    let mut protected = BTreeSet::new();
    let schema_version: i64 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if !(1..=7).contains(&schema_version) {
        return Err(codec::invalid());
    }
    if schema_version >= 6 {
        membership::validate_all(connection, next)?;
    }
    let obligation_query = if schema_version >= 3 {
        "SELECT obligation_id,owner_id,graph_id,allocation_epoch,resources,trial_group_id,phase,sequence,last_operation_id,manifest_json,hold_reason,settlement_json,memory_claim FROM obligations ORDER BY obligation_id"
    } else {
        "SELECT obligation_id,owner_id,graph_id,allocation_epoch,resources,trial_group_id,phase,sequence,last_operation_id,manifest_json,hold_reason,settlement_json,'bounded_active' FROM obligations ORDER BY obligation_id"
    };
    let mut stmt = connection.prepare(obligation_query)?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let id = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(0)?)?;
        let owner = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(1)?)?;
        let graph = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(2)?)?;
        let epoch = codec::epoch_from_bytes(&row.get::<_, Vec<u8>>(3)?)?;
        let resources = codec::vector_from_bytes(&row.get::<_, Vec<u8>>(4)?)?;
        let trial = row
            .get::<_, Option<Vec<u8>>>(5)?
            .map(|v| codec::uuid_from_bytes(&v))
            .transpose()?;
        let phase = Phase::parse(&row.get::<_, String>(6)?)?;
        let sequence: i64 = row.get(7)?;
        let operation = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(8)?)?;
        let manifest_json: String = row.get(9)?;
        let reason: Option<String> = row.get(10)?;
        let settlement: Option<String> = row.get(11)?;
        let claim_key: String = row.get(12)?;
        let memory_claim = MemoryClaim::from_sql_key(&claim_key).ok_or_else(codec::invalid)?;
        if matches!(memory_claim, MemoryClaim::UnknownProducerPeak)
            && (resources.processes == 0 || resources.cpu_ns_per_second == 0)
        {
            return Err(codec::invalid());
        }
        if memory_claim == MemoryClaim::PlanningMetadata
            && !fixed_planning_metadata_manifest(&manifest_json, resources)?
        {
            return Err(codec::invalid());
        }
        if matches!(
            memory_claim,
            MemoryClaim::KnownBounded {
                active_producer: false
            }
        ) {
            let retained: Option<String> = connection
                .query_row(
                    "SELECT custody_json FROM retained_asset_owners WHERE obligation_id=?1",
                    [id.as_bytes()],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(retained) = retained else {
                return Err(codec::invalid());
            };
            if !matches!(
                codec_v2::decode_retained_assets(&retained)?,
                codec_v2::RetainedAssetsDocument::V2(_)
            ) || resources.cpu_ns_per_second != 0
                || resources.processes != 0
                || resources.hardware_slots != 0
                || resources.tuner_slots != 0
            {
                return Err(codec::invalid());
            }
        }
        if !owner_exists(connection, owner)?
            || sequence < 1
            || sequence > next
            || resources == ResourceVector::default()
        {
            return Err(codec::invalid());
        }
        if (phase == Phase::UnknownHeld) != reason.is_some()
            || (phase == Phase::Settled) != settlement.is_some()
        {
            return Err(codec::invalid());
        }
        if let Some(reason) = &reason {
            codec::text(reason)?;
        }
        validate_final_operation(connection, id, operation, graph, sequence, phase, trial)?;
        let inventory = inventory::load(
            connection,
            id,
            owner,
            graph,
            phase,
            &manifest_json,
            settlement.as_deref(),
        )?;
        if matches!(inventory, ObligationInventory::LegacyV1 { .. }) {
            let referenced: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM family_roles WHERE obligation_id=?1)",
                [id.as_bytes()],
                |r| r.get(0),
            )?;
            if !referenced {
                return Err(codec::invalid());
            }
        }
        if phase != Phase::Settled {
            if obligations.len() >= codec::ACTIVE_LIMIT {
                return Err(CustodyError::new(
                    ErrorKind::Exhausted,
                    "custody active history exceeds supported bound",
                ));
            }
            total = total.checked_add(resources).ok_or_else(codec::invalid)?;
            if let Some(group) = trial {
                trials.insert(group);
            }
            protected.extend(inventory::protected_paths(&inventory));
            obligations.push(ObligationRecord {
                obligation_id: id,
                graph_id: graph,
                owner_id: owner,
                allocation_epoch: epoch,
                resources,
                memory_claim,
                trial_group_id: trial,
                phase,
                sequence,
                inventory,
                hold_reason: reason,
                cleanup_only: true,
            });
        }
    }
    let mut families = Vec::new();
    let mut represented = BTreeSet::new();
    let mut family_hash = Sha256::new();
    let mut active_family_edge_count = 0_u64;
    let mut stmt = connection.prepare("SELECT family_id,role,obligation_id FROM family_roles ORDER BY family_id,role,obligation_id")?;
    let mut rows = stmt.query([])?;
    let mut family_cursor = None;
    let mut family_count = 0;
    while let Some(row) = rows.next()? {
        let family_id = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(0)?)?;
        let role = Role::parse(&row.get::<_, String>(1)?)?;
        let obligation_id = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(2)?)?;
        let phase: String = connection.query_row(
            "SELECT phase FROM obligations WHERE obligation_id=?1",
            [obligation_id.as_bytes()],
            |r| r.get(0),
        )?;
        let phase = Phase::parse(&phase)?;
        let conflicting_role: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM family_roles WHERE obligation_id=?1 AND role<>?2)",
            params![obligation_id.as_bytes(), role.as_str()],
            |r| r.get(0),
        )?;
        if conflicting_role {
            return Err(codec::invalid());
        }
        if family_cursor == Some(family_id) {
            family_count += 1;
        } else {
            family_cursor = Some(family_id);
            family_count = 1;
        }
        if family_count > 3 {
            return Err(codec::invalid());
        }
        if phase != Phase::Settled {
            let edge = FamilyEdge {
                family_id,
                role,
                obligation_id,
            };
            family_hash.update(edge.family_id.as_bytes());
            family_hash.update([match edge.role {
                Role::Video => 0,
                Role::Audio => 1,
                Role::Companion => 2,
            }]);
            family_hash.update(edge.obligation_id.as_bytes());
            active_family_edge_count = active_family_edge_count
                .checked_add(1)
                .ok_or_else(codec::invalid)?;
            if represented.insert(obligation_id) {
                if families.len() >= codec::ACTIVE_LIMIT {
                    return Err(codec::invalid());
                }
                families.push(edge);
            }
        }
    }
    let family_authority = FamilyAuthority::from_validated_rows(
        active_family_edge_count,
        family_hash.finalize().into(),
        &families,
    );
    let retained_assets = assets::load(connection)?;
    if trials.len() > 1 {
        return Err(codec::invalid());
    }
    for record in &obligations {
        let (family, asset): (bool, bool) = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM family_roles WHERE obligation_id=?1),EXISTS(SELECT 1 FROM retained_asset_owners WHERE obligation_id=?1)",
            [record.obligation_id.as_bytes()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if family == asset {
            return Err(codec::invalid());
        }
    }
    let recovered_s_holders = recovery::load_overlay(
        connection,
        schema_version,
        &retained_assets,
        &obligations,
        next,
    )?;
    let recovered_r_holders =
        recovery_r::load_overlay(connection, schema_version, &obligations, next)?;
    // Validate operation records too: unknown/truncated codecs cannot be ignored.
    let mut stmt = connection
        .prepare("SELECT operation_id,request_json,result_json FROM operations ORDER BY rowid")?;
    let mut rows = stmt.query([])?;
    let mut committed_sequence = 0_i64;
    let mut projected_events = 0_i64;
    let mut s_replay = recovery::ReplayS::new(schema_version);
    let mut r_replay = recovery_r::ReplayR::new(schema_version);
    while let Some(row) = rows.next()? {
        let operation_id = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(0)?)?;
        let fingerprint: String = codec::decode(&row.get::<_, String>(1)?)?;
        if fingerprint.len() != 64 || !fingerprint.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(codec::invalid());
        }
        let result: DiskResult = codec::decode(&row.get::<_, String>(2)?)?;
        let sequence = match &result {
            DiskResult::Prepared { sequence, .. } => *sequence,
            DiskResult::Changed(ack) => ack.sequence,
            DiskResult::AssetChangedV2(record) => record.ack.sequence,
            DiskResult::AssetChangedV3(record) => record.ack.sequence,
            DiskResult::RetiredWithAssets(ack) => ack.sequence,
            DiskResult::RecoveredSAdoption(ack) | DiskResult::RecoveredRAdoption(ack) => {
                ack.sequence
            }
        };
        committed_sequence = committed_sequence
            .checked_add(1)
            .ok_or_else(codec::invalid)?;
        if sequence != committed_sequence || sequence > next {
            return Err(codec::invalid());
        }
        if schema_version >= 6 && membership::validate(connection, operation_id, &result)? {
            projected_events = projected_events.checked_add(1).ok_or_else(codec::invalid)?;
        }
        s_replay.observe(connection, operation_id, &result)?;
        r_replay.observe(connection, operation_id, &result)?;
        match result {
            DiskResult::Prepared {
                sequence,
                obligations,
                trial,
            } => {
                let unique: BTreeSet<_> = obligations.iter().copied().collect();
                if sequence < 1
                    || sequence > next
                    || obligations.is_empty()
                    || obligations.len() > 6
                    || unique.len() != obligations.len()
                    || obligations
                        .iter()
                        .any(|id| obligation_tail(connection, *id).is_err())
                {
                    return Err(codec::invalid());
                }
                for id in obligations {
                    let (last, _, last_seq, last_phase, group) = obligation_tail(connection, id)?;
                    if group != trial || last_seq < sequence {
                        return Err(codec::invalid());
                    }
                    if last == operation_id
                        && (last_seq != sequence || last_phase != Phase::Prepared)
                    {
                        return Err(codec::invalid());
                    }
                }
            }
            DiskResult::Changed(ack) => {
                if ack.sequence < 1
                    || ack.sequence > next
                    || obligation_tail(connection, ack.obligation_id).is_err()
                    || ack.operation_id != operation_id
                {
                    return Err(codec::invalid());
                }
                let (last, graph, last_seq, last_phase, _) =
                    obligation_tail(connection, ack.obligation_id)?;
                if graph != ack.graph_id
                    || last_seq < ack.sequence
                    || (ack.phase == Phase::Settled
                        && (last != operation_id || last_seq != ack.sequence))
                {
                    return Err(codec::invalid());
                }
                if last == operation_id && (last_seq != ack.sequence || last_phase != ack.phase) {
                    return Err(codec::invalid());
                }
            }
            DiskResult::AssetChangedV2(record) => {
                scratch::validate_record_binding(connection, &record)?;
                let ack = &record.ack;
                if ack.sequence > next
                    || obligation_tail(connection, ack.obligation_id).is_err()
                    || ack.operation_id != operation_id
                {
                    return Err(codec::invalid());
                }
                let (last, graph, last_seq, last_phase, _) =
                    obligation_tail(connection, ack.obligation_id)?;
                if graph != ack.graph_id || last_seq < ack.sequence {
                    return Err(codec::invalid());
                }
                if last == operation_id && (last_seq != ack.sequence || last_phase != ack.phase) {
                    return Err(codec::invalid());
                }
            }
            DiskResult::AssetChangedV3(record) => {
                scratch::bootstrap::validate_record_binding(connection, &record)?;
                let ack = &record.ack;
                if ack.sequence > next
                    || obligation_tail(connection, ack.obligation_id).is_err()
                    || ack.operation_id != operation_id
                {
                    return Err(codec::invalid());
                }
                let (last, graph, last_seq, last_phase, _) =
                    obligation_tail(connection, ack.obligation_id)?;
                if graph != ack.graph_id || last_seq < ack.sequence {
                    return Err(codec::invalid());
                }
                if last == operation_id && (last_seq != ack.sequence || last_phase != ack.phase) {
                    return Err(codec::invalid());
                }
            }
            DiskResult::RetiredWithAssets(ack) => {
                let version: i64 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
                if version < 2
                    || ack.version != 2
                    || ack.operation_id.uuid() != operation_id
                    || ack.sequence < 1
                    || ack.sequence > next
                    || ack.source_obligation_id == ack.target_obligation_id
                {
                    return Err(codec::invalid());
                }
                let source = obligation_tail(connection, ack.source_obligation_id.uuid())?;
                let target = obligation_tail(connection, ack.target_obligation_id.uuid())?;
                if source.1 != ack.source_graph_id.uuid()
                    || target.1 != ack.target_graph_id.uuid()
                    || source.2 != ack.sequence
                    || source.3 != Phase::Settled
                    || source.0 != operation_id
                    || target.2 < ack.sequence
                    || source.4 != target.4
                {
                    return Err(codec::invalid());
                }
                let custody_json: String = connection.query_row("SELECT custody_json FROM retained_asset_owners WHERE obligation_id=?1 AND asset_id=?2", params![ack.target_obligation_id.uuid().as_bytes(), ack.asset_id.uuid().as_bytes()], |r| r.get(0))?;
                let custody = codec_v2::decode_retained_assets(&custody_json)?;
                let (operation, producer, producer_graph, target_graph) = match &custody {
                    codec_v2::RetainedAssetsDocument::V1(custody) => (
                        custody.transfer_operation_id,
                        custody.producer_obligation_id,
                        custody.producer_graph_id,
                        custody.target_graph_id,
                    ),
                    codec_v2::RetainedAssetsDocument::V2(custody) => (
                        custody.transfer_operation_id,
                        custody.producer_obligation_id,
                        custody.producer_graph_id,
                        custody.target_graph_id,
                    ),
                };
                if operation != ack.operation_id
                    || producer != ack.source_obligation_id
                    || producer_graph != ack.source_graph_id
                    || target_graph != ack.target_graph_id
                    || custody.target_obligation_id() != ack.target_obligation_id
                    || custody.asset_id() != ack.asset_id
                {
                    return Err(codec::invalid());
                }
                if target.0 == operation_id
                    && (target.2 != ack.sequence || target.3 != Phase::Sealed)
                {
                    return Err(codec::invalid());
                }
            }
            DiskResult::RecoveredSAdoption(ack) => {
                if !adoption_holder_matches(connection, "recovered_s_holders", &ack, operation_id)?
                {
                    return Err(codec::invalid());
                }
            }
            DiskResult::RecoveredRAdoption(ack) => {
                if !adoption_holder_matches(connection, "recovered_r_holders", &ack, operation_id)?
                {
                    return Err(codec::invalid());
                }
            }
        }
    }
    // Every operation remains present and ordered; no historical row is pruned.
    if committed_sequence != next {
        return Err(codec::invalid());
    }
    if schema_version >= 6 {
        membership::validate_count(connection, projected_events)?;
    }
    s_replay.finish()?;
    r_replay.finish()?;
    let mut needed_owners = BTreeSet::new();
    if let Some(current) = current {
        needed_owners.insert(current);
    }
    for record in &obligations {
        needed_owners.insert(record.owner_id);
    }
    for holder in &recovered_s_holders {
        needed_owners.insert(holder.previous_owner_id);
        needed_owners.insert(holder.current_owner_id);
    }
    for holder in &recovered_r_holders {
        needed_owners.insert(holder.previous_owner_id);
        needed_owners.insert(holder.current_owner_id);
    }
    let mut pending_owners = Vec::new();
    for id in needed_owners {
        pending_owners.push(load_owner(connection, id, witness)?);
    }
    Ok(RecoverySnapshot {
        journal_id: witness.journal_id,
        installation_id: witness.installation_id,
        obligations,
        families,
        family_authority,
        retained_assets,
        total,
        trial_groups: trials.into_iter().collect(),
        protected_paths: protected.into_iter().collect(),
        owners: pending_owners,
        recovered_s_holders,
        recovered_r_holders,
    })
}

pub(in crate::resource_custody) fn backfill_asset_membership(
    tx: &Transaction<'_>,
) -> Result<(), CustodyError> {
    membership::backfill(tx)
}

fn validate_settlement_record(
    manifest: &Manifest,
    members: &[MemberRecord],
    record: &SettlementRecord,
) -> Result<(), CustodyError> {
    match record {
        SettlementRecord::PreparedNeverArmed { version, owner_id }
            if *version == 1 && *owner_id == manifest.owner_id =>
        {
            if members.iter().any(|m| m.observed.is_some()) {
                return Err(codec::invalid());
            }
        }
        SettlementRecord::Physical {
            version,
            owner_id,
            graph_id,
            members: facts,
        } if *version == 1 && *owner_id == manifest.owner_id && *graph_id == manifest.graph_id => {
            validate_facts(manifest, members, facts)?
        }
        _ => return Err(codec::invalid()),
    }
    Ok(())
}
fn validate_facts(
    manifest: &Manifest,
    members: &[MemberRecord],
    facts: &[ClosedMemberFact],
) -> Result<(), CustodyError> {
    if facts.len() != manifest.members.len() {
        return Err(codec::invalid());
    }
    let mut seen = BTreeSet::new();
    for fact in facts {
        let member = members
            .iter()
            .find(|m| m.plan.member_id == fact.member_id)
            .ok_or_else(codec::invalid)?;
        if !seen.insert(fact.member_id)
            || fact.adapter != member.plan.adapter
            || fact.adapter_version != member.plan.adapter_version
            || fact.domain != member.plan.domain
        {
            return Err(codec::invalid());
        }
        codec::long_text(&fact.object_identity)?;
        codec::long_text(&fact.evidence)?;
        if member
            .observed
            .as_ref()
            .is_some_and(|o| o.object_identity != fact.object_identity)
        {
            return Err(codec::invalid());
        }
    }
    Ok(())
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
#[expect(
    clippy::large_enum_variant,
    reason = "Keep the bounded persisted result inline in its decoding owner; boxing the V3 frozen inventory adds a separate allocation to the existing structural result layout."
)]
enum DiskResult {
    Prepared {
        sequence: i64,
        obligations: Vec<Uuid>,
        trial: Option<Uuid>,
    },
    Changed(CommitAck),
    AssetChangedV2(scratch::AssetChangedV2),
    AssetChangedV3(scratch::bootstrap::AssetChangedV3),
    RetiredWithAssets(RetirementAck),
    RecoveredSAdoption(RecoveryAdoptionAck),
    RecoveredRAdoption(RecoveryAdoptionAck),
}

pub(super) fn fingerprint(command: &Command) -> Result<String, CustodyError> {
    // Explicit canonical payload excludes ephemeral deadline and receipt object.
    // Receipt identity is checked independently against its core runtime binding.
    let value = match command {
        Command::PrepareTyped {
            batch,
            handles,
            trial,
        } => {
            let obligations: Vec<_> = batch.obligations.iter().zip(handles).map(|(n,h)| {
                let mut value = serde_json::json!({"id":h.obligation_id,"graph":h.graph_id,"owner":h.owner_id,"epoch":n.allocation_epoch,"resources":codec::vector_bytes(n.resources),"manifest":n.manifest});
                if !n.memory_claim.is_default() {
                    value.as_object_mut().expect("typed obligation JSON object")
                        .insert("memory_claim".into(), serde_json::json!(n.memory_claim));
                }
                value
            }).collect();
            let families: Vec<_> = batch
                .families
                .iter()
                .map(|f| {
                    let roles: Vec<_> = f
                        .roles
                        .iter()
                        .map(|(r, t)| {
                            let id = match t {
                                RoleTarget::New(i) => handles.get(*i).map(|h| h.obligation_id),
                                RoleTarget::Retained(h) => Some(h.obligation_id),
                            };
                            serde_json::json!([r, id])
                        })
                        .collect();
                    serde_json::json!([f.family.family_id, roles])
                })
                .collect();
            serde_json::json!({"typed_prepare_v2":obligations,"families":families,"trial":trial})
        }
        Command::Prepare {
            batch,
            handles,
            trial,
        } => {
            let obligations: Vec<_> = batch.obligations.iter().zip(handles).map(|(n, h)| serde_json::json!({"id":h.obligation_id,"graph":h.graph_id,"owner":h.owner_id,"epoch":n.allocation_epoch,"resources":codec::vector_bytes(n.resources),"manifest":n.manifest})).collect();
            let families: Vec<_> = batch
                .families
                .iter()
                .map(|f| {
                    let roles: Vec<_> = f
                        .roles
                        .iter()
                        .map(|(r, t)| {
                            let id = match t {
                                RoleTarget::New(i) => handles.get(*i).map(|h| h.obligation_id),
                                RoleTarget::Retained(h) => Some(h.obligation_id),
                            };
                            serde_json::json!([r, id])
                        })
                        .collect();
                    serde_json::json!([f.family.family_id, roles])
                })
                .collect();
            serde_json::json!({"prepare":obligations,"families":families,"trial":trial})
        }
        Command::Change { key, mutation } => {
            let action = match mutation {
                Mutation::Arm { members } => serde_json::json!({"arm":members}),
                Mutation::RecordAcquired { identity } => serde_json::json!({"acquired":identity}),
                Mutation::RecordTypedAcquired { envelope } => {
                    serde_json::json!({"typed_acquired_v2":envelope})
                }
                Mutation::AssetChangeV2 { change } => {
                    codec::encode(change)?;
                    serde_json::json!({"asset_change_v2": change})
                }
                Mutation::AssetChangeV3 { change } => {
                    codec::encode(change)?;
                    serde_json::json!({"asset_change_v3": change})
                }
                Mutation::Seal => serde_json::json!("seal"),
                Mutation::Hold { reason } => serde_json::json!({"hold":reason}),
                Mutation::CancelPrepared => serde_json::json!("cancel_prepared"),
                Mutation::Settle(proof) => serde_json::json!({"settle":proof.facts}),
                Mutation::SettleTyped(proof) => serde_json::json!({
                    "settle_typed_v2": proof.settlement,
                    "receipt_binding": format!("{:?}", proof.receipt.custody_binding()),
                }),
                Mutation::RetireWithAssets(proof) => serde_json::json!({
                    "retire_with_assets_v2": {
                        "settlement":proof.settlement,"retained":proof.retained,
                        "target_manifest":proof.target_manifest,"target_resources":proof.target_resources,
                        "target_id":proof.target.obligation_id,"target_graph":proof.target.graph_id,
                    }
                }),
            };
            let value = serde_json::json!({"id":key.handle.obligation_id,"graph":key.handle.graph_id,"owner":key.handle.owner_id,"sequence":key.expected_sequence,"action":action});
            #[cfg(target_os = "linux")]
            let mut value = value;
            #[cfg(target_os = "linux")]
            if let Some(authority) = &key.handle.recovery {
                let (name, holder) = match authority {
                    RecoveryAuthority::RetainedS(authority) => (
                        "recovered_s_holder",
                        serde_json::json!({
                            "journal": authority.journal_id(),
                            "installation": authority.installation_id(),
                            "obligation": authority.obligation_id(),
                            "original_owner": authority.original_owner_id(),
                            "previous_owner": authority.previous_owner_id(),
                            "current_owner": authority.current_owner_id(),
                            "adoption_operation": authority.adoption_operation_id(),
                            "adoption_sequence": authority.adoption_sequence(),
                            "import_head": authority.state_head(),
                            "frozen_inventory": authority.frozen_digest(),
                        }),
                    ),
                    RecoveryAuthority::ProducerR(authority) => (
                        "recovered_r_holder",
                        serde_json::json!({
                            "journal": authority.journal_id(),
                            "installation": authority.installation_id(),
                            "obligation": authority.obligation_id(),
                            "graph": authority.graph_id(),
                            "original_owner": authority.original_owner_id(),
                            "current_owner": authority.current_owner_id(),
                            "adoption_operation": authority.adoption_operation_id(),
                            "adoption_sequence": authority.adoption_sequence(),
                            "import_head": authority.state_head(),
                            "root_member": authority.root_member_id(),
                            "root_pid": authority.root_pid(),
                            "root_creation_marker": authority.root_creation_marker(),
                        }),
                    ),
                };
                value
                    .as_object_mut()
                    .ok_or_else(codec::invalid)?
                    .insert(name.into(), holder);
            }
            value
        }
        #[cfg(target_os = "linux")]
        Command::ClosePreviousDirect { claim, exit } => serde_json::json!({
            "close_previous_direct_v1": {
                "obligation": claim.obligation,
                "graph": claim.graph,
                "owner": exit.owner_id(),
                "closing_owner": exit.closing_owner_id(),
                "prior_operation": claim.prior_operation,
                "prior_sequence": claim.prior_sequence,
            }
        }),
        #[cfg(target_os = "linux")]
        Command::RecoverRetainedS { proof, exit } => serde_json::json!({
            "recover_retained_s_v1": {
                "journal": proof.journal_id(),
                "installation": proof.installation_id(),
                "obligation": proof.obligation_id(),
                "previous_owner": proof.previous_owner_id(),
                "current_owner": proof.current_owner_id(),
                "head": proof.state_head(),
                "frozen_inventory": proof.frozen_digest(),
                "exit_owner": exit.owner_id(),
            }
        }),
        #[cfg(target_os = "linux")]
        Command::RecoverActiveR { proof } => serde_json::json!({
            "recover_fixed_r_v1": {
                "journal": proof.journal_id(),
                "installation": proof.installation_id(),
                "obligation": proof.obligation_id(),
                "graph": proof.graph_id(),
                "previous_owner": proof.previous_owner_id(),
                "current_owner": proof.current_owner_id(),
                "original_sequence": proof.original_sequence(),
                "head": proof.state_head(),
                "root_member": proof.root_exit().root_member_id(),
                "root_pid": proof.root_exit().pid(),
                "root_creation_marker": proof.root_exit().creation_marker(),
            }
        }),
    };
    let bytes = serde_json::to_vec(&value).map_err(|_| codec::invalid())?;
    if bytes.len() > 1024 * 1024 {
        return Err(codec::invalid());
    }
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn next_sequence(tx: &Transaction<'_>) -> Result<i64, CustodyError> {
    let prior: i64 = tx.query_row(
        "SELECT next_sequence FROM journal_meta WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    let next = prior
        .checked_add(1)
        .filter(|_| prior >= 0)
        .ok_or_else(|| CustodyError::new(ErrorKind::Exhausted, "custody sequence exhausted"))?;
    tx.execute(
        "UPDATE journal_meta SET next_sequence=?1 WHERE singleton=1",
        [next],
    )?;
    Ok(next)
}

pub(super) fn execute(
    storage: &mut storage::Storage,
    operation: &QueuedOperation,
    owner: Uuid,
) -> Result<OperationResult, CustodyError> {
    storage.verify()?;
    let remaining = operation
        .deadline
        .checked_duration_since(Instant::now())
        .ok_or_else(|| {
            CustodyError::new(ErrorKind::Deadline, "custody request deadline elapsed")
        })?;
    storage
        .connection
        .busy_timeout(remaining.min(std::time::Duration::from_millis(250)))?;
    let fingerprint = fingerprint(&operation.command)?;
    let tx = storage
        .connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let prior: Option<(String, String)> = tx
        .query_row(
            "SELECT request_json,result_json FROM operations WHERE operation_id=?1",
            [operation.id.as_bytes()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let disk_result = if let Some((prior, result)) = prior {
        if codec::decode::<String>(&prior)? != fingerprint {
            return Err(CustodyError::new(
                ErrorKind::Conflict,
                "operation replay payload differs",
            ));
        }
        let decoded = codec::decode::<DiskResult>(&result)?;
        let version: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version >= 6 {
            membership::validate(&tx, operation.id, &decoded)?;
        }
        decoded
    } else {
        let current: Vec<u8> = tx.query_row(
            "SELECT current_owner_id FROM journal_meta WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        if codec::uuid_from_bytes(&current)? != owner {
            return Err(CustodyError::new(
                ErrorKind::Conflict,
                "custody owner retired",
            ));
        }
        let result = match &operation.command {
            Command::PrepareTyped {
                batch,
                handles,
                trial,
            } => typed::prepare(
                &tx,
                storage.witness.journal_id,
                owner,
                operation.id,
                batch,
                handles,
                *trial,
            )?,
            Command::Prepare {
                batch,
                handles,
                trial,
            } => prepare(
                &tx,
                storage.witness.journal_id,
                owner,
                operation.id,
                batch,
                handles,
                *trial,
            )?,
            Command::Change { key, mutation } => {
                change(&tx, &storage.witness, owner, key, mutation)?
            }
            #[cfg(target_os = "linux")]
            Command::ClosePreviousDirect { claim, exit } => {
                direct_exit::close(&tx, &storage.witness, owner, operation.id, claim, exit)?
            }
            #[cfg(target_os = "linux")]
            Command::RecoverRetainedS { proof, exit } => {
                recovery::adopt(&tx, &storage.witness, owner, operation.id, proof, exit)?
            }
            #[cfg(target_os = "linux")]
            Command::RecoverActiveR { proof } => {
                recovery_r::adopt(&tx, &storage.witness, owner, operation.id, proof)?
            }
        };
        tx.execute(
            "INSERT INTO operations VALUES(?1,?2,?3)",
            params![
                operation.id.as_bytes(),
                codec::encode(&fingerprint)?,
                codec::encode(&result)?
            ],
        )?;
        let schema_version: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if schema_version >= 6 {
            membership::insert(&tx, operation.id, &result)?;
        }
        result
    };
    // Errors before COMMIT are a rollback (Transaction's drop performs it).
    // COMMIT errors are deliberately Unknown, even SQLite errors that usually
    // mean rollback. The exact operation is retained for positive reread/retry.
    if let Err(_error) = tx.commit() {
        let mut error = CustodyError::new(ErrorKind::Storage, "custody commit result uncertain");
        error.certainty = CommitCertainty::Unknown;
        return Err(error);
    }
    #[cfg(test)]
    if std::mem::take(&mut storage.fail_after_commit_once) {
        let mut error = CustodyError::new(
            ErrorKind::Storage,
            "injected lost custody commit acknowledgement",
        );
        error.certainty = CommitCertainty::Unknown;
        return Err(error);
    }
    storage.verify().map_err(|mut e| {
        e.certainty = CommitCertainty::Unknown;
        e
    })?;
    #[cfg(target_os = "linux")]
    if matches!(
        operation.command,
        Command::RecoverRetainedS { .. }
            | Command::RecoverActiveR { .. }
            | Command::ClosePreviousDirect { .. }
    ) {
        snapshot(&storage.connection, &storage.witness).map_err(|mut error| {
            error.certainty = CommitCertainty::Unknown;
            error
        })?;
    }
    match (&operation.command, disk_result) {
        #[cfg(target_os = "linux")]
        (Command::ClosePreviousDirect { claim, .. }, DiskResult::Changed(ack))
            if ack.operation_id == operation.id
                && ack.obligation_id == claim.obligation
                && ack.graph_id == claim.graph
                && ack.phase == Phase::Settled
                && ack.sequence > claim.prior_sequence =>
        {
            Ok(OperationResult::Changed(ack))
        }
        #[cfg(target_os = "linux")]
        (Command::RecoverRetainedS { proof, .. }, DiskResult::RecoveredSAdoption(ack))
            if ack.operation_id == operation.id
                && ack.obligation_id == proof.obligation_id()
                && ack.previous_owner_id == proof.previous_owner_id()
                && ack.current_owner_id == proof.current_owner_id() =>
        {
            Ok(OperationResult::RecoveredSAdoption(ack))
        }
        #[cfg(target_os = "linux")]
        (Command::RecoverActiveR { proof }, DiskResult::RecoveredRAdoption(ack))
            if ack.operation_id == operation.id
                && ack.obligation_id == proof.obligation_id()
                && ack.previous_owner_id == proof.previous_owner_id()
                && ack.current_owner_id == proof.current_owner_id() =>
        {
            Ok(OperationResult::RecoveredRAdoption(ack))
        }
        (
            Command::Change {
                key,
                mutation: Mutation::AssetChangeV3 { change },
            },
            DiskResult::AssetChangedV3(record),
        ) if record.ack.operation_id == operation.id
            && record.ack.obligation_id == key.handle.obligation_id
            && record.ack.graph_id == key.handle.graph_id
            && record.change == *change =>
        {
            record.validate()?;
            Ok(OperationResult::Changed(record.ack))
        }
        (
            Command::Change {
                key,
                mutation: Mutation::AssetChangeV2 { change },
            },
            DiskResult::AssetChangedV2(record),
        ) if record.ack.operation_id == operation.id
            && record.ack.obligation_id == key.handle.obligation_id
            && record.ack.graph_id == key.handle.graph_id
            && record.change == *change =>
        {
            record.validate()?;
            Ok(OperationResult::Changed(record.ack))
        }
        (
            Command::Prepare { handles, trial, .. } | Command::PrepareTyped { handles, trial, .. },
            DiskResult::Prepared {
                sequence,
                obligations,
                trial: stored_trial,
            },
        ) if obligations == handles.iter().map(|h| h.obligation_id).collect::<Vec<_>>()
            && *trial == stored_trial =>
        {
            Ok(OperationResult::Prepared(PreparedBatch {
                obligations: handles.clone(),
                sequence,
                #[cfg(test)]
                trial_group_id: *trial,
            }))
        }
        (
            Command::Change {
                key,
                mutation: Mutation::RetireWithAssets(proof),
            },
            DiskResult::RetiredWithAssets(ack),
        ) if ack.version == 2
            && ack.operation_id.uuid() == operation.id
            && ack.source_obligation_id.uuid() == key.handle.obligation_id
            && ack.source_graph_id.uuid() == key.handle.graph_id
            && ack.target_obligation_id.uuid() == proof.target.obligation_id
            && ack.target_graph_id.uuid() == proof.target.graph_id
            && ack.asset_id == proof.retained.asset_id =>
        {
            Ok(OperationResult::RetiredWithAssets {
                ack,
                target: proof.target.clone(),
            })
        }
        (Command::Change { key, .. }, DiskResult::Changed(ack))
            if ack.operation_id == operation.id
                && ack.obligation_id == key.handle.obligation_id
                && ack.graph_id == key.handle.graph_id =>
        {
            Ok(OperationResult::Changed(ack))
        }
        _ => Err(codec::invalid()),
    }
}

/// A numeric memory vector covers known allocations only. One unknown
/// producer peak occupies the node's producer slot until it settles; an
/// actually retained resident row carries no active producer claim.
fn check_memory_claim_combination(
    tx: &Transaction<'_>,
    incoming: impl IntoIterator<Item = MemoryClaim>,
) -> Result<(), CustodyError> {
    let version: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let query = if version >= 3 {
        "SELECT obligation_id,memory_claim,resources,manifest_json FROM obligations WHERE phase!='settled'"
    } else {
        "SELECT obligation_id,'bounded_active',resources,manifest_json FROM obligations WHERE phase!='settled'"
    };
    let mut active = false;
    let mut unknown = false;
    let mut statement = tx.prepare(query)?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let id = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(0)?)?;
        let key: String = row.get(1)?;
        let resources = codec::vector_from_bytes(&row.get::<_, Vec<u8>>(2)?)?;
        let claim = MemoryClaim::from_sql_key(&key).ok_or_else(codec::invalid)?;
        if claim == MemoryClaim::PlanningMetadata {
            let manifest_json: String = row.get(3)?;
            if !fixed_planning_metadata_manifest(&manifest_json, resources)? {
                return Err(codec::invalid());
            }
        }
        if matches!(claim, MemoryClaim::UnknownProducerPeak)
            && (resources.processes == 0 || resources.cpu_ns_per_second == 0)
        {
            return Err(codec::invalid());
        }
        if matches!(
            claim,
            MemoryClaim::KnownBounded {
                active_producer: false
            }
        ) {
            if resources.cpu_ns_per_second != 0
                || resources.processes != 0
                || resources.hardware_slots != 0
                || resources.tuner_slots != 0
            {
                return Err(codec::invalid());
            }
            let custody: Option<String> = tx
                .query_row(
                    "SELECT custody_json FROM retained_asset_owners WHERE obligation_id=?1",
                    [id.as_bytes()],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(custody) = custody else {
                return Err(codec::invalid());
            };
            if !matches!(
                codec_v2::decode_retained_assets(&custody)?,
                codec_v2::RetainedAssetsDocument::V2(_)
            ) {
                return Err(codec::invalid());
            }
        }
        include_memory_claim(claim, &mut active, &mut unknown)?;
    }
    for claim in incoming {
        include_memory_claim(claim, &mut active, &mut unknown)?;
    }
    Ok(())
}

fn fixed_planning_metadata_manifest(
    json: &str,
    resources: ResourceVector,
) -> Result<bool, CustodyError> {
    if resources.cpu_ns_per_second != 0
        || resources.processes != 0
        || resources.handles != 0
        || resources.scratch_bytes != 0
        || resources.hardware_slots != 0
        || resources.tuner_slots != 0
    {
        return Ok(false);
    }
    Ok(matches!(codec_v2::decode_manifest(json)?,
        codec_v2::ManifestDocument::V2(manifest)
            if matches!(manifest.members.as_slice(),
                [codec_v2::MemberPlan::FixedProbeMetadataV1 { metadata, .. }]
                    if resources.peak_memory_bytes >= metadata.retained_memory_bytes)))
}

fn include_memory_claim(
    claim: MemoryClaim,
    active: &mut bool,
    unknown: &mut bool,
) -> Result<(), CustodyError> {
    let conflict = || {
        CustodyError::new(
            ErrorKind::Conflict,
            "unknown producer peak conflicts with an active producer",
        )
    };
    match claim {
        MemoryClaim::KnownBounded {
            active_producer: false,
        } => {}
        MemoryClaim::KnownBounded {
            active_producer: true,
        } => {
            if *unknown {
                return Err(conflict());
            }
            *active = true;
        }
        MemoryClaim::UnknownProducerPeak => {
            if *active || *unknown {
                return Err(conflict());
            }
            *unknown = true;
        }
        MemoryClaim::PlanningMetadata => {}
    }
    Ok(())
}

fn prepare(
    tx: &Transaction<'_>,
    journal: Uuid,
    owner: Uuid,
    operation: Uuid,
    batch: &PrepareBatch,
    handles: &[ObligationHandle],
    trial: Option<Uuid>,
) -> Result<DiskResult, CustodyError> {
    if batch.obligations.is_empty()
        || batch.obligations.len() > 6
        || batch.obligations.len() != handles.len()
        || batch.families.is_empty()
        || batch.families.len() > 2
        || batch.trial != trial.is_some()
    {
        return Err(codec::invalid());
    }
    let active: i64 = tx.query_row(
        "SELECT count(*) FROM obligations WHERE phase!='settled'",
        [],
        |r| r.get(0),
    )?;
    if active < 0 || active as usize + handles.len() > codec::ACTIVE_LIMIT {
        return Err(CustodyError::new(
            ErrorKind::Exhausted,
            "custody active obligation limit reached",
        ));
    }
    let held_trial: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM obligations WHERE phase!='settled' AND trial_group_id IS NOT NULL)", [], |r| r.get(0))?;
    if trial.is_some() && held_trial {
        return Err(CustodyError::new(
            ErrorKind::Conflict,
            "custody trial group already held",
        ));
    }
    let mut aggregate = ResourceVector::default();
    let mut costs = tx.prepare("SELECT resources FROM obligations WHERE phase!='settled'")?;
    let mut rows = costs.query([])?;
    while let Some(row) = rows.next()? {
        aggregate = aggregate
            .checked_add(codec::vector_from_bytes(&row.get::<_, Vec<u8>>(0)?)?)
            .ok_or_else(codec::invalid)?;
    }
    let mut new_ids = BTreeSet::new();
    for (new, h) in batch.obligations.iter().zip(handles) {
        codec::validate_manifest(&new.manifest)?;
        if h.journal_id != journal
            || h.owner_id != owner
            || h.graph_id != new.manifest.graph_id
            || new.manifest.owner_id != owner
            || new.resources == ResourceVector::default()
            || !new_ids.insert(h.obligation_id)
        {
            return Err(codec::invalid());
        }
        aggregate = aggregate
            .checked_add(new.resources)
            .ok_or_else(codec::invalid)?;
    }
    check_memory_claim_combination(
        tx,
        std::iter::repeat_n(MemoryClaim::default(), batch.obligations.len()),
    )?;
    let sequence = next_sequence(tx)?;
    for (new, h) in batch.obligations.iter().zip(handles) {
        tx.execute(
            "INSERT INTO obligations (obligation_id,owner_id,graph_id,allocation_epoch,resources,trial_group_id,phase,sequence,last_operation_id,manifest_json,hold_reason,settlement_json) VALUES(?1,?2,?3,?4,?5,?6,'prepared',?7,?8,?9,NULL,NULL)",
            params![
                h.obligation_id.as_bytes(),
                owner.as_bytes(),
                h.graph_id.as_bytes(),
                new.allocation_epoch.to_le_bytes(),
                codec::vector_bytes(new.resources),
                trial.map(|t| t.as_bytes().to_vec()),
                sequence,
                operation.as_bytes(),
                codec::encode(&new.manifest)?
            ],
        )?;
        for member in &new.manifest.members {
            let envelope = MemberEnvelope {
                version: 1,
                planned: member.clone(),
                observed: None,
            };
            tx.execute(
                "INSERT INTO members VALUES(?1,?2,?3,?4,?5,'planned',?6,NULL)",
                params![
                    h.obligation_id.as_bytes(),
                    member.member_id.as_bytes(),
                    member.kind.as_str(),
                    member.adapter.as_str(),
                    member.adapter_version,
                    codec::encode(&envelope)?
                ],
            )?;
        }
    }
    insert_families(tx, journal, owner, &batch.families, handles, &new_ids)?;
    Ok(DiskResult::Prepared {
        sequence,
        obligations: handles.iter().map(|h| h.obligation_id).collect(),
        trial,
    })
}

fn insert_families(
    tx: &Transaction<'_>,
    journal: Uuid,
    owner: Uuid,
    requests: &[FamilyRequest],
    handles: &[ObligationHandle],
    new_ids: &BTreeSet<Uuid>,
) -> Result<(), CustodyError> {
    let mut families = BTreeSet::new();
    let mut referenced = BTreeSet::new();
    let mut physical_roles = BTreeMap::new();
    for family in requests {
        if family.family.journal_id != journal
            || !families.insert(family.family.family_id)
            || family.roles.is_empty()
            || family.roles.len() > 3
        {
            return Err(codec::invalid());
        }
        let mut roles = BTreeSet::new();
        for (role, target) in &family.roles {
            if !roles.insert(role.as_str()) {
                return Err(codec::invalid());
            }
            let h = match target {
                RoleTarget::New(index) => {
                    let h = handles.get(*index).ok_or_else(codec::invalid)?;
                    referenced.insert(h.obligation_id);
                    h
                }
                RoleTarget::Retained(h) => {
                    if h.journal_id != journal || h.owner_id != owner {
                        return Err(codec::invalid());
                    }
                    let row: Option<(String,Vec<u8>)> = tx.query_row("SELECT phase,graph_id FROM obligations WHERE obligation_id=?1 AND owner_id=?2", params![h.obligation_id.as_bytes(),owner.as_bytes()], |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
                    if !matches!(row, Some((ref phase, ref graph)) if ["prepared","acquisition_possible"].contains(&phase.as_str()) && graph.as_slice()==h.graph_id.as_bytes())
                    {
                        return Err(codec::invalid());
                    }
                    let existing_role: String = tx.query_row(
                        "SELECT role FROM family_roles WHERE obligation_id=?1 LIMIT 1",
                        [h.obligation_id.as_bytes()],
                        |r| r.get(0),
                    )?;
                    if existing_role != role.as_str() {
                        return Err(codec::invalid());
                    }
                    h
                }
            };
            if let Some(prior) = physical_roles.insert(h.obligation_id, role.as_str()) {
                if prior != role.as_str() {
                    return Err(codec::invalid());
                }
            }
            let prior: Option<Vec<u8>> = tx
                .query_row(
                    "SELECT obligation_id FROM family_roles WHERE family_id=?1 AND role=?2",
                    params![family.family.family_id.as_bytes(), role.as_str()],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(prior) = prior {
                if prior.as_slice() != h.obligation_id.as_bytes() {
                    return Err(codec::invalid());
                }
            } else {
                tx.execute(
                    "INSERT INTO family_roles VALUES(?1,?2,?3)",
                    params![
                        family.family.family_id.as_bytes(),
                        role.as_str(),
                        h.obligation_id.as_bytes()
                    ],
                )?;
            }
        }
        let count: i64 = tx.query_row(
            "SELECT count(*) FROM family_roles WHERE family_id=?1",
            [family.family.family_id.as_bytes()],
            |r| r.get(0),
        )?;
        if count != family.roles.len() as i64 {
            return Err(codec::invalid());
        }
    }
    if &referenced != new_ids {
        return Err(codec::invalid());
    }
    Ok(())
}

fn change(
    tx: &Transaction<'_>,
    witness: &storage::Witness,
    owner: Uuid,
    key: &TransitionKey,
    mutation: &Mutation,
) -> Result<DiskResult, CustodyError> {
    let journal = witness.journal_id;
    let h = &key.handle;
    if h.journal_id != journal || h.owner_id != owner || key.expected_sequence < 1 {
        return Err(codec::invalid());
    }
    #[cfg(target_os = "linux")]
    if let Some(authority) = &h.recovery {
        return match authority {
            RecoveryAuthority::RetainedS(authority) => {
                recovery::change(tx, witness, owner, key, mutation, authority)
            }
            RecoveryAuthority::ProducerR(authority) => {
                recovery_r::change(tx, witness, owner, key, mutation, authority)
            }
        };
    }
    let (graph, recorded_owner, prior_phase, seq, manifest): (Vec<u8>,Vec<u8>,String,i64,String) = tx.query_row("SELECT graph_id,owner_id,phase,sequence,manifest_json FROM obligations WHERE obligation_id=?1",[h.obligation_id.as_bytes()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
    if codec::uuid_from_bytes(&graph)? != h.graph_id
        || codec::uuid_from_bytes(&recorded_owner)? != owner
        || seq != key.expected_sequence
    {
        return Err(CustodyError::new(
            ErrorKind::Conflict,
            "custody transition binding or sequence differs",
        ));
    }
    let prior = Phase::parse(&prior_phase)?;
    if prior == Phase::Settled {
        return Err(CustodyError::new(
            ErrorKind::Conflict,
            "custody obligation already settled",
        ));
    }
    if let codec_v2::ManifestDocument::V2(manifest) = codec_v2::decode_manifest(&manifest)? {
        if let Mutation::RetireWithAssets(proof) = mutation {
            return retirement::change(tx, journal, owner, key, prior, &manifest, proof);
        }
        if let Mutation::SettleTyped(proof) = mutation {
            // Exactly one retained scratch member owns the retained-owner
            // lineage branch; every other live typed manifest settles here.
            return if matches!(
                manifest.members.as_slice(),
                [codec_v2::MemberPlan::ScratchAssetsV3 { .. }]
            ) {
                retained_settlement::change(tx, owner, key, prior, &manifest, proof)
            } else {
                live_typed_settlement::change(tx, owner, key, prior, &manifest, proof)
            };
        }
        return typed::change(tx, owner, key, mutation, prior, &manifest);
    }
    let manifest: Manifest = codec::decode(&manifest)?;
    let mut phase = prior;
    let mut reason: Option<String> = tx.query_row(
        "SELECT hold_reason FROM obligations WHERE obligation_id=?1",
        [h.obligation_id.as_bytes()],
        |r| r.get(0),
    )?;
    let mut settlement = None;
    match mutation {
        Mutation::RecordTypedAcquired { .. }
        | Mutation::RetireWithAssets(_)
        | Mutation::SettleTyped(_)
        | Mutation::AssetChangeV2 { .. }
        | Mutation::AssetChangeV3 { .. } => return Err(codec::invalid()),
        Mutation::Arm { members } => {
            if !matches!(prior, Phase::Prepared | Phase::AcquisitionPossible)
                || members.is_empty()
                || members.len() > 129
            {
                return Err(codec::invalid());
            }
            let mut seen = BTreeSet::new();
            for member in members {
                if !seen.insert(member) {
                    return Err(codec::invalid());
                }
                let changed = tx.execute("UPDATE members SET phase='acquisition_possible' WHERE obligation_id=?1 AND member_id=?2 AND phase='planned'",params![h.obligation_id.as_bytes(),member.as_bytes()])?;
                if changed != 1 {
                    return Err(codec::invalid());
                }
            }
            phase = Phase::AcquisitionPossible;
        }
        Mutation::RecordAcquired { identity } => {
            // A late native acquisition result may arrive after the runtime
            // sealed entry. Enrich only a previously armed member; this does
            // NOT grant permission to perform another acquisition.
            if !matches!(
                prior,
                Phase::AcquisitionPossible | Phase::Sealed | Phase::UnknownHeld
            ) {
                return Err(codec::invalid());
            }
            let plan = manifest
                .members
                .iter()
                .find(|m| m.member_id == identity.member_id)
                .ok_or_else(codec::invalid)?;
            codec::validate_observed(plan, identity)?;
            let envelope = MemberEnvelope {
                version: 1,
                planned: plan.clone(),
                observed: Some(identity.clone()),
            };
            let changed = tx.execute("UPDATE members SET phase='acquired',identity_json=?3 WHERE obligation_id=?1 AND member_id=?2 AND phase='acquisition_possible'",params![h.obligation_id.as_bytes(),identity.member_id.as_bytes(),codec::encode(&envelope)?])?;
            if changed != 1 {
                return Err(codec::invalid());
            }
        }
        Mutation::Seal => {
            if matches!(prior, Phase::UnknownHeld | Phase::Sealed) {
                return Err(codec::invalid());
            }
            phase = Phase::Sealed;
        }
        Mutation::Hold { reason: hold } => {
            codec::text(hold)?;
            if prior == Phase::UnknownHeld && reason.as_ref() == Some(hold) {
                return Err(CustodyError::new(
                    ErrorKind::Conflict,
                    "custody hold classification unchanged",
                ));
            }
            phase = Phase::UnknownHeld;
            reason = Some(hold.clone());
        }
        Mutation::CancelPrepared => {
            if prior != Phase::Prepared {
                return Err(codec::invalid());
            }
            let armed: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM members WHERE obligation_id=?1 AND phase!='planned')",
                [h.obligation_id.as_bytes()],
                |r| r.get(0),
            )?;
            if armed {
                return Err(codec::invalid());
            }
            phase = Phase::Settled;
            settlement = Some(codec::encode(&SettlementRecord::PreparedNeverArmed {
                version: 1,
                owner_id: owner,
            })?);
        }
        Mutation::Settle(proof) => {
            if !matches!(prior, Phase::Sealed | Phase::UnknownHeld) {
                return Err(codec::invalid());
            }
            // Main's private seam exposes ONLY the receipt's original binding.
            // No disk record or caller boolean can pass this comparison.
            if proof.receipt.custody_binding() != h.runtime_binding {
                return Err(CustodyError::new(
                    ErrorKind::Conflict,
                    "physical receipt binding differs",
                ));
            }
            let mut members = Vec::new();
            let mut stmt =
                tx.prepare("SELECT identity_json,phase FROM members WHERE obligation_id=?1")?;
            let mut rows = stmt.query([h.obligation_id.as_bytes()])?;
            while let Some(row) = rows.next()? {
                let envelope: MemberEnvelope = codec::decode(&row.get::<_, String>(0)?)?;
                members.push(MemberRecord {
                    plan: envelope.planned,
                    phase: row.get(1)?,
                    observed: envelope.observed,
                });
            }
            validate_facts(&manifest, &members, &proof.facts)?;
            phase = Phase::Settled;
            settlement = Some(codec::encode(&SettlementRecord::Physical {
                version: 1,
                owner_id: owner,
                graph_id: h.graph_id,
                members: proof.facts.clone(),
            })?);
        }
    }
    if let Some(record) = &settlement {
        reason = None;
        tx.execute(
            "UPDATE members SET phase='closed',proof_json=?2 WHERE obligation_id=?1",
            params![h.obligation_id.as_bytes(), record],
        )?;
    }
    let sequence = next_sequence(tx)?;
    tx.execute("UPDATE obligations SET phase=?2,sequence=?3,last_operation_id=?4,hold_reason=?5,settlement_json=?6 WHERE obligation_id=?1",params![h.obligation_id.as_bytes(),phase.as_str(),sequence,key.operation_id.as_bytes(),reason,settlement])?;
    Ok(DiskResult::Changed(CommitAck {
        operation_id: key.operation_id,
        obligation_id: h.obligation_id,
        graph_id: h.graph_id,
        sequence,
        phase,
    }))
}
