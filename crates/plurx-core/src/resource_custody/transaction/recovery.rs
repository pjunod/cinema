//! Append-only recovered S custodians. The historical R→S obligation, retained
//! asset document, and original owner are never rewritten.
use super::*;
use crate::resource_custody::codec_v2::{self as v2, scratch::AssetEventRefV1};

struct TrackedS {
    transfer_operation_id: Uuid,
    asset_id: v2::Id,
    baseline_head: AssetEventRefV1,
    current_head: AssetEventRefV1,
    current_sequence: i64,
}

pub(super) fn load_overlay(
    connection: &Connection,
    schema_version: i64,
    original_owners: &BTreeMap<Uuid, Uuid>,
    last_operations: &BTreeMap<Uuid, (Uuid, Uuid, i64, Phase, Option<Uuid>)>,
    retained_assets: &[RetainedAssetOwner],
    obligations: &[ObligationRecord],
    owners: &BTreeSet<Uuid>,
    next: i64,
) -> Result<Vec<RecoveredSHolder>, CustodyError> {
    if schema_version < 4 {
        return Ok(Vec::new());
    }
    let mut rows = connection.prepare("SELECT obligation_id,adoption_sequence,operation_id,previous_owner_id,current_owner_id,original_s_sequence,head_operation_id,head_revision,frozen_digest FROM recovered_s_holders ORDER BY obligation_id,adoption_sequence")?;
    let mut query = rows.query([])?;
    let mut holders = Vec::new();
    let mut effective = BTreeMap::<Uuid, (Uuid, i64)>::new();
    let mut tracked = BTreeMap::<Uuid, TrackedS>::new();
    while let Some(row) = query.next()? {
        if holders.len() >= codec::ACTIVE_LIMIT {
            return Err(codec::invalid());
        }
        let obligation_id = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(0)?)?;
        let adoption_sequence: i64 = row.get(1)?;
        let operation_id = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(2)?)?;
        let previous_owner_id = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(3)?)?;
        let current_owner_id = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(4)?)?;
        let original_s_sequence: i64 = row.get(5)?;
        let head_operation_id = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(6)?)?;
        let head_revision: i64 = row.get(7)?;
        let frozen_digest: String = row.get(8)?;
        let source_owner = *original_owners
            .get(&obligation_id)
            .ok_or_else(codec::invalid)?;
        let (expected_previous, previous_sequence) = effective
            .get(&obligation_id)
            .copied()
            .unwrap_or((source_owner, 0));
        let (_, _, current_sequence, phase, _) = last_operations
            .get(&obligation_id)
            .ok_or_else(codec::invalid)?;
        let asset = if let Some(asset) = retained_assets
            .iter()
            .find(|asset| asset.obligation_id == obligation_id)
        {
            asset.custody.clone()
        } else {
            let json: String = connection.query_row(
                "SELECT custody_json FROM retained_asset_owners WHERE obligation_id=?1",
                [obligation_id.as_bytes()],
                |r| r.get(0),
            )?;
            v2::decode_retained_assets(&json)?
        };
        let v2::RetainedAssetsDocument::V2(custody) = &asset else {
            return Err(codec::invalid());
        };
        let v2::TransferIdentity::ScratchAssetsV3 { snapshot, .. } = &custody.transfer.identity
        else {
            return Err(codec::invalid());
        };
        let inventory = if let Some(record) = obligations
            .iter()
            .find(|record| record.obligation_id == obligation_id)
        {
            record.inventory.clone()
        } else {
            let (manifest_json, settlement_json): (String, Option<String>) = connection.query_row(
                "SELECT manifest_json,settlement_json FROM obligations WHERE obligation_id=?1",
                [obligation_id.as_bytes()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            inventory::load(
                connection,
                obligation_id,
                source_owner,
                last_operations[&obligation_id].1,
                *phase,
                &manifest_json,
                settlement_json.as_deref(),
            )?
        };
        let ObligationInventory::TypedV2 { members, .. } = &inventory else {
            return Err(codec::invalid());
        };
        let [member] = members.as_slice() else {
            return Err(codec::invalid());
        };
        let Some(v2::Observed::ScratchAssetsV3 { state }) = &member.envelope.observed else {
            return Err(codec::invalid());
        };
        let head = AssetEventRefV1 {
            operation_id: v2::Id::new(head_operation_id)?,
            revision: u64::try_from(head_revision).map_err(|_| codec::invalid())?,
        };
        let current_head = state.asset.head.clone().ok_or_else(codec::invalid)?;
        let baseline_head = snapshot
            .state
            .asset
            .head
            .clone()
            .ok_or_else(codec::invalid)?;
        if !owners.contains(&previous_owner_id)
            || !owners.contains(&current_owner_id)
            || expected_previous != previous_owner_id
            || previous_owner_id == current_owner_id
            || adoption_sequence <= previous_sequence
            || adoption_sequence > next
            || original_s_sequence < 1
            || original_s_sequence > *current_sequence
            || !matches!(phase, Phase::Sealed | Phase::UnknownHeld | Phase::Settled)
            || (matches!(phase, Phase::Settled)
                != (state.asset.lifecycle == v2::scratch::ScratchLifecycleV1::RetiredClosed))
            || snapshot.inventory_sha256 != frozen_digest
        {
            return Err(codec::invalid());
        }
        match tracked.entry(obligation_id) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(TrackedS {
                    transfer_operation_id: custody.transfer_operation_id.uuid(),
                    asset_id: custody.asset_id,
                    baseline_head,
                    current_head,
                    current_sequence: *current_sequence,
                });
            }
            std::collections::btree_map::Entry::Occupied(entry) => {
                let prior = entry.get();
                if prior.transfer_operation_id != custody.transfer_operation_id.uuid()
                    || prior.asset_id != custody.asset_id
                    || prior.baseline_head != baseline_head
                    || prior.current_head != current_head
                    || prior.current_sequence != *current_sequence
                {
                    return Err(codec::invalid());
                }
            }
        }
        effective.insert(obligation_id, (current_owner_id, adoption_sequence));
        holders.push(RecoveredSHolder {
            obligation_id,
            adoption_sequence,
            operation_id,
            previous_owner_id,
            current_owner_id,
            original_s_sequence,
            head,
            frozen_digest,
        });
    }
    validate_overlay_history(connection, &holders, &tracked, next)?;
    Ok(holders)
}

/// Walk the append-only global sequence once. A holder's stored head is the
/// exact target head *at adoption*, not necessarily the current head after
/// later reads or closure. No audit DTO supplies a native proof here.
fn validate_overlay_history(
    connection: &Connection,
    holders: &[RecoveredSHolder],
    tracked: &BTreeMap<Uuid, TrackedS>,
    next: i64,
) -> Result<(), CustodyError> {
    let by_operation: BTreeMap<Uuid, &RecoveredSHolder> = holders
        .iter()
        .map(|holder| (holder.operation_id, holder))
        .collect();
    if by_operation.len() != holders.len() {
        return Err(codec::invalid());
    }
    let mut replay = BTreeMap::<Uuid, (i64, AssetEventRefV1)>::new();
    let mut matched = BTreeSet::new();
    let mut sequence = 0_i64;
    let mut stmt =
        connection.prepare("SELECT operation_id,result_json FROM operations ORDER BY rowid")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let operation_id = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(0)?)?;
        let result: DiskResult = codec::decode(&row.get::<_, String>(1)?)?;
        let committed_sequence = match &result {
            DiskResult::Prepared { sequence, .. } => *sequence,
            DiskResult::Changed(ack) => ack.sequence,
            DiskResult::AssetChangedV2(record) => record.ack.sequence,
            DiskResult::AssetChangedV3(record) => record.ack.sequence,
            DiskResult::RetiredWithAssets(ack) => ack.sequence,
            DiskResult::RecoveredSAdoption(ack) => ack.sequence,
        };
        sequence = sequence.checked_add(1).ok_or_else(codec::invalid)?;
        if committed_sequence != sequence {
            return Err(codec::invalid());
        }
        match result {
            DiskResult::RetiredWithAssets(ack) => {
                let id = ack.target_obligation_id.uuid();
                if let Some(target) = tracked.get(&id) {
                    if operation_id != target.transfer_operation_id
                        || ack.asset_id != target.asset_id
                        || replay
                            .insert(id, (sequence, target.baseline_head.clone()))
                            .is_some()
                    {
                        return Err(codec::invalid());
                    }
                }
            }
            DiskResult::AssetChangedV3(record) => {
                let id = record.ack.obligation_id;
                if tracked.contains_key(&id) {
                    let prior = replay.get_mut(&id).ok_or_else(codec::invalid)?;
                    if record.change.previous.as_ref() != Some(&prior.1)
                        || record.ack.sequence <= prior.0
                    {
                        return Err(codec::invalid());
                    }
                    prior.0 = sequence;
                    prior.1 = record.after.asset.head.ok_or_else(codec::invalid)?;
                }
            }
            DiskResult::AssetChangedV2(record) => {
                if tracked.contains_key(&record.ack.obligation_id) {
                    return Err(codec::invalid());
                }
            }
            DiskResult::Changed(ack) => {
                if let Some(prior) = replay.get_mut(&ack.obligation_id) {
                    if ack.sequence <= prior.0 {
                        return Err(codec::invalid());
                    }
                    prior.0 = sequence;
                }
            }
            DiskResult::RecoveredSAdoption(ack) => {
                if let Some(target) = tracked.get(&ack.obligation_id) {
                    let holder = by_operation.get(&operation_id).ok_or_else(codec::invalid)?;
                    let prior = replay.get(&ack.obligation_id).ok_or_else(codec::invalid)?;
                    if ack.sequence != holder.adoption_sequence
                        || ack.obligation_id != holder.obligation_id
                        || prior.0 != holder.original_s_sequence
                        || prior.1 != holder.head
                        || target.current_sequence < prior.0
                        || !matched.insert(operation_id)
                    {
                        return Err(codec::invalid());
                    }
                }
            }
            DiskResult::Prepared { .. } => {}
        }
    }
    if sequence != next || matched.len() != holders.len() {
        return Err(codec::invalid());
    }
    for (id, target) in tracked {
        let (last, head) = replay.get(id).ok_or_else(codec::invalid)?;
        if *last != target.current_sequence || *head != target.current_head {
            return Err(codec::invalid());
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) fn adopt(
    tx: &Transaction<'_>,
    witness: &storage::Witness,
    owner: Uuid,
    operation_id: Uuid,
    proof: &crate::adaptive_resources::runtime::recovery::NativeRetainedSImportProof,
    exit: &crate::resource_custody::recovery::PositivePreviousOwnerExit,
) -> Result<DiskResult, CustodyError> {
    let snapshot = super::snapshot(tx, witness)?;
    // Match the bounded replay parser before writing. A successful adoption
    // must remain readable on the next startup.
    if snapshot.recovered_s_holders.len() >= codec::ACTIVE_LIMIT {
        return Err(CustodyError::new(
            ErrorKind::Exhausted,
            "recovered S holder history bound reached",
        ));
    }
    let claim = proof.claim();
    let record = snapshot
        .obligations
        .iter()
        .find(|record| record.obligation_id == proof.obligation_id())
        .ok_or_else(codec::invalid)?;
    let effective = snapshot
        .recovered_s_holders
        .iter()
        .filter(|holder| holder.obligation_id == record.obligation_id)
        .max_by_key(|holder| holder.adoption_sequence)
        .map_or(record.owner_id, |holder| holder.current_owner_id);
    let current_owner = snapshot
        .owners
        .iter()
        .find(|identity| identity.owner_id == owner)
        .ok_or_else(codec::invalid)?;
    if proof.journal_id() != witness.journal_id
        || proof.installation_id() != witness.installation_id
        || proof.current_owner_id() != owner
        || proof.previous_owner_id() != effective
        || proof.obligation_id() != claim.obligation_id()
        || claim.original_owner_id() != record.owner_id
        || claim.old_owner_id() != effective
        || claim.current_head() != proof.state_head()
        || claim.inventory_sha256() != proof.frozen_digest()
        || record.sequence < 1
        || !exit.matches(&snapshot, current_owner, effective)
    {
        return Err(codec::invalid());
    }
    let chosen = crate::resource_custody::recovery::select_dormant_retained_s(
        tx,
        &snapshot,
        exit,
        proof.obligation_id(),
    )?;
    if chosen.original_owner_id() != claim.original_owner_id()
        || chosen.current_head() != claim.current_head()
        || chosen.inventory_sha256() != claim.inventory_sha256()
        || chosen.resources() != claim.resources()
    {
        return Err(codec::invalid());
    }
    let sequence = next_sequence(tx)?;
    tx.execute(
        "INSERT INTO recovered_s_holders VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![
            record.obligation_id.as_bytes(),
            sequence,
            operation_id.as_bytes(),
            effective.as_bytes(),
            owner.as_bytes(),
            record.sequence,
            proof.state_head().operation_id.uuid().as_bytes(),
            i64::try_from(proof.state_head().revision).map_err(|_| codec::invalid())?,
            proof.frozen_digest(),
        ],
    )?;
    Ok(DiskResult::RecoveredSAdoption(RecoveryAdoptionAck {
        operation_id,
        obligation_id: record.obligation_id,
        previous_owner_id: effective,
        current_owner_id: owner,
        sequence,
    }))
}

/// Existing recovered S only: current native owner plus the exact latest
/// append-only holder row authorizes target read/cleanup events. The V2
/// manifest, transfer and original obligation owner remain historical facts.
#[cfg(target_os = "linux")]
pub(super) fn change(
    tx: &Transaction<'_>,
    witness: &storage::Witness,
    owner: Uuid,
    key: &TransitionKey,
    mutation: &Mutation,
    authority: &crate::adaptive_resources::runtime::recovery::RecoveredSAuthority,
) -> Result<DiskResult, CustodyError> {
    let h = &key.handle;
    if authority.journal_id() != witness.journal_id
        || authority.installation_id() != witness.installation_id
        || authority.obligation_id() != h.obligation_id
        || authority.current_owner_id() != owner
        || authority.binding() != h.runtime_binding
        || h.journal_id != witness.journal_id
        || h.owner_id != owner
    {
        return Err(codec::invalid());
    }
    let (adoption_sequence, adoption_operation, previous_owner, current_owner, head_operation, head_revision, frozen_digest):
        (i64, Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>, i64, String) = tx.query_row(
        "SELECT adoption_sequence,operation_id,previous_owner_id,current_owner_id,head_operation_id,head_revision,frozen_digest FROM recovered_s_holders WHERE obligation_id=?1 ORDER BY adoption_sequence DESC LIMIT 1",
        [h.obligation_id.as_bytes()],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)),
    )?;
    if adoption_sequence != authority.adoption_sequence()
        || codec::uuid_from_bytes(&adoption_operation)? != authority.adoption_operation_id()
        || codec::uuid_from_bytes(&previous_owner)? != authority.previous_owner_id()
        || codec::uuid_from_bytes(&current_owner)? != owner
        || codec::uuid_from_bytes(&head_operation)? != authority.state_head().operation_id.uuid()
        || u64::try_from(head_revision).map_err(|_| codec::invalid())?
            != authority.state_head().revision
        || frozen_digest != authority.frozen_digest()
    {
        return Err(codec::invalid());
    }
    let (graph, recorded_owner, phase, sequence, manifest_json):
        (Vec<u8>, Vec<u8>, String, i64, String) = tx.query_row(
        "SELECT graph_id,owner_id,phase,sequence,manifest_json FROM obligations WHERE obligation_id=?1",
        [h.obligation_id.as_bytes()],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
    )?;
    let original_owner = codec::uuid_from_bytes(&recorded_owner)?;
    let v2::ManifestDocument::V2(manifest) = v2::decode_manifest(&manifest_json)? else {
        return Err(codec::invalid());
    };
    if codec::uuid_from_bytes(&graph)? != h.graph_id
        || original_owner != authority.original_owner_id()
        || manifest.owner_id.uuid() != original_owner
        || manifest.graph_id.uuid() != h.graph_id
        || sequence != key.expected_sequence
        || !matches!(Phase::parse(&phase)?, Phase::Sealed | Phase::UnknownHeld)
    {
        return Err(codec::invalid());
    }
    let prior = Phase::parse(&phase)?;
    match mutation {
        Mutation::AssetChangeV3 { change } => {
            scratch::bootstrap::change(tx, key, prior, &manifest, change)
        }
        Mutation::SettleTyped(proof) => {
            retained_settlement::change(tx, original_owner, key, prior, &manifest, proof)
        }
        _ => Err(codec::invalid()),
    }
}
