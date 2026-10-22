//! Atomic write-ahead transitions and strict full-accounting reconstruction.
mod assets;
mod inventory;
mod typed;
use super::*;
use codec::{MemberEnvelope, SettlementRecord};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

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
    let mut owners = BTreeSet::new();
    let mut owner_records = BTreeMap::new();
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
        owner_records.insert(id, identity);
        let state: String = row.get(6)?;
        if state == "current" {
            if current != Some(id) {
                return Err(codec::invalid());
            }
            current_count += 1;
        } else if state != "retired" {
            return Err(codec::invalid());
        }
        owners.insert(id);
    }
    if current_count != usize::from(current.is_some()) {
        return Err(codec::invalid());
    }
    let mut obligations = Vec::new();
    let mut all_ids = BTreeMap::new();
    let mut legacy_ids = BTreeSet::new();
    let mut last_operations = BTreeMap::new();
    let mut graphs = BTreeSet::new();
    let mut total = ResourceVector::default();
    let mut trials = BTreeSet::new();
    let mut protected = BTreeSet::new();
    let mut stmt = connection.prepare("SELECT obligation_id,owner_id,graph_id,allocation_epoch,resources,trial_group_id,phase,sequence,last_operation_id,manifest_json,hold_reason,settlement_json FROM obligations ORDER BY obligation_id")?;
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
        if !owners.contains(&owner)
            || sequence < 1
            || sequence > next
            || resources == ResourceVector::default()
            || !graphs.insert(graph)
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
        last_operations.insert(id, (operation, graph, sequence, phase, trial));
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
            legacy_ids.insert(id);
        }
        all_ids.insert(id, phase);
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
    let mut role_kinds = BTreeMap::new();
    let mut family_counts = BTreeMap::new();
    let mut stmt = connection.prepare("SELECT family_id,role,obligation_id FROM family_roles")?;
    let mut rows = stmt.query([])?;
    let mut referenced = BTreeSet::new();
    while let Some(row) = rows.next()? {
        let family_id = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(0)?)?;
        let role = Role::parse(&row.get::<_, String>(1)?)?;
        let obligation_id = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(2)?)?;
        let phase = all_ids.get(&obligation_id).ok_or_else(codec::invalid)?;
        if let Some(existing) = role_kinds.insert(obligation_id, role) {
            if existing != role {
                return Err(codec::invalid());
            }
        }
        let count = family_counts.entry(family_id).or_insert(0usize);
        *count += 1;
        if *count > 3 {
            return Err(codec::invalid());
        }
        referenced.insert(obligation_id);
        if *phase != Phase::Settled {
            families.push(FamilyEdge {
                family_id,
                role,
                obligation_id,
            });
        }
    }
    let retained_assets = assets::load(connection, &all_ids, &referenced)?;
    let asset_ids: BTreeSet<_> = retained_assets.iter().map(|a| a.obligation_id).collect();
    let active_ids: BTreeSet<_> = all_ids
        .iter()
        .filter_map(|(id, phase)| (*phase != Phase::Settled).then_some(*id))
        .collect();
    let active_references: BTreeSet<_> = referenced
        .intersection(&active_ids)
        .copied()
        .chain(asset_ids.iter().copied())
        .collect();
    if active_references != active_ids
        || !asset_ids.is_disjoint(&referenced)
        || !legacy_ids.is_subset(&referenced)
        || trials.len() > 1
    {
        return Err(codec::invalid());
    }
    // Validate operation records too: unknown/truncated codecs cannot be ignored.
    let mut stmt =
        connection.prepare("SELECT operation_id,request_json,result_json FROM operations")?;
    let mut rows = stmt.query([])?;
    let mut operation_sequences = BTreeSet::new();
    let mut validated_last = BTreeSet::new();
    while let Some(row) = rows.next()? {
        let operation_id = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(0)?)?;
        let fingerprint: String = codec::decode(&row.get::<_, String>(1)?)?;
        if fingerprint.len() != 64 || !fingerprint.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(codec::invalid());
        }
        let result: DiskResult = codec::decode(&row.get::<_, String>(2)?)?;
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
                    || obligations.iter().any(|id| !all_ids.contains_key(id))
                    || !operation_sequences.insert(sequence)
                {
                    return Err(codec::invalid());
                }
                for id in obligations {
                    let (last, _, last_seq, last_phase, group) = last_operations[&id];
                    if group != trial || last_seq < sequence {
                        return Err(codec::invalid());
                    }
                    if last == operation_id {
                        if last_seq != sequence || last_phase != Phase::Prepared {
                            return Err(codec::invalid());
                        }
                        validated_last.insert(id);
                    }
                }
            }
            DiskResult::Changed(ack) => {
                if ack.sequence < 1
                    || ack.sequence > next
                    || !all_ids.contains_key(&ack.obligation_id)
                    || ack.operation_id != operation_id
                    || !operation_sequences.insert(ack.sequence)
                {
                    return Err(codec::invalid());
                }
                let (last, graph, last_seq, last_phase, _) = last_operations[&ack.obligation_id];
                if graph != ack.graph_id || last_seq < ack.sequence {
                    return Err(codec::invalid());
                }
                if last == operation_id {
                    if last_seq != ack.sequence || last_phase != ack.phase {
                        return Err(codec::invalid());
                    }
                    validated_last.insert(ack.obligation_id);
                }
            }
            DiskResult::RetiredWithAssets(ack) => {
                let version: i64 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
                if version != 2
                    || ack.version != 2
                    || ack.operation_id.uuid() != operation_id
                    || ack.sequence < 1
                    || ack.sequence > next
                    || !operation_sequences.insert(ack.sequence)
                    || ack.source_obligation_id == ack.target_obligation_id
                {
                    return Err(codec::invalid());
                }
                let source = last_operations
                    .get(&ack.source_obligation_id.uuid())
                    .ok_or_else(codec::invalid)?;
                let target = last_operations
                    .get(&ack.target_obligation_id.uuid())
                    .ok_or_else(codec::invalid)?;
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
                let custody: codec_v2::RetainedAssets = codec::decode(&custody_json)?;
                if custody.transfer_operation_id != ack.operation_id
                    || custody.producer_obligation_id != ack.source_obligation_id
                    || custody.producer_graph_id != ack.source_graph_id
                    || custody.target_graph_id != ack.target_graph_id
                {
                    return Err(codec::invalid());
                }
                validated_last.insert(ack.source_obligation_id.uuid());
                if target.0 == operation_id {
                    if target.2 != ack.sequence || target.3 != Phase::Sealed {
                        return Err(codec::invalid());
                    }
                    validated_last.insert(ack.target_obligation_id.uuid());
                }
            }
        }
    }
    // v1 retains all operation history; a gap is missing authority, not a
    // pruned diagnostic. A future pruning format needs an explicit checkpoint.
    if validated_last.len() != all_ids.len()
        || i64::try_from(operation_sequences.len()).ok() != Some(next)
    {
        return Err(codec::invalid());
    }
    let pending_owners = owner_records
        .into_values()
        .filter(|owner| obligations.iter().any(|o| o.owner_id == owner.owner_id))
        .collect();
    Ok(RecoverySnapshot {
        journal_id: witness.journal_id,
        installation_id: witness.installation_id,
        obligations,
        families,
        retained_assets,
        total,
        trial_groups: trials.into_iter().collect(),
        protected_paths: protected.into_iter().collect(),
        owners: pending_owners,
    })
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
enum DiskResult {
    Prepared {
        sequence: i64,
        obligations: Vec<Uuid>,
        trial: Option<Uuid>,
    },
    Changed(CommitAck),
    RetiredWithAssets(RetirementAck),
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetirementAck {
    version: u32,
    operation_id: codec_v2::Id,
    sequence: i64,
    source_obligation_id: codec_v2::Id,
    source_graph_id: codec_v2::Id,
    target_obligation_id: codec_v2::Id,
    target_graph_id: codec_v2::Id,
    asset_id: codec_v2::Id,
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
            let obligations: Vec<_> = batch.obligations.iter().zip(handles).map(|(n,h)| serde_json::json!({"id":h.obligation_id,"graph":h.graph_id,"owner":h.owner_id,"epoch":n.allocation_epoch,"resources":codec::vector_bytes(n.resources),"manifest":n.manifest})).collect();
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
                Mutation::Seal => serde_json::json!("seal"),
                Mutation::Hold { reason } => serde_json::json!({"hold":reason}),
                Mutation::CancelPrepared => serde_json::json!("cancel_prepared"),
                Mutation::Settle(proof) => serde_json::json!({"settle":proof.facts}),
            };
            serde_json::json!({"id":key.handle.obligation_id,"graph":key.handle.graph_id,"owner":key.handle.owner_id,"sequence":key.expected_sequence,"action":action})
        }
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
        codec::decode::<DiskResult>(&result)?
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
                change(&tx, storage.witness.journal_id, owner, key, mutation)?
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
    match (&operation.command, disk_result) {
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
                trial_group_id: *trial,
            }))
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
    let sequence = next_sequence(tx)?;
    for (new, h) in batch.obligations.iter().zip(handles) {
        tx.execute(
            "INSERT INTO obligations VALUES(?1,?2,?3,?4,?5,?6,'prepared',?7,?8,?9,NULL,NULL)",
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
    journal: Uuid,
    owner: Uuid,
    key: &TransitionKey,
    mutation: &Mutation,
) -> Result<DiskResult, CustodyError> {
    let h = &key.handle;
    if h.journal_id != journal || h.owner_id != owner || key.expected_sequence < 1 {
        return Err(codec::invalid());
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
        Mutation::RecordTypedAcquired { .. } => return Err(codec::invalid()),
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
