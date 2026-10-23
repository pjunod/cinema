//! Append-only custodians for imported fixed software R. The original owner
//! and typed producer history remain immutable.
use super::*;
use crate::resource_custody::codec_v2::{self as v2, scratch::AssetEventRefV1};

pub(super) fn load_overlay(
    connection: &Connection,
    schema_version: i64,
    obligations: &[ObligationRecord],
    owners: &BTreeSet<Uuid>,
    next: i64,
) -> Result<Vec<RecoveredRHolder>, CustodyError> {
    if schema_version < 5 {
        return Ok(Vec::new());
    }
    let mut statement = connection.prepare(
        "SELECT obligation_id,adoption_sequence,operation_id,previous_owner_id,current_owner_id,original_r_sequence,head_operation_id,head_revision,root_member_id,root_pid,root_creation_marker FROM recovered_r_holders ORDER BY obligation_id,adoption_sequence",
    )?;
    let mut rows = statement.query([])?;
    let mut holders = Vec::new();
    let mut effective = BTreeMap::<Uuid, (Uuid, i64)>::new();
    while let Some(row) = rows.next()? {
        if holders.len() >= codec::ACTIVE_LIMIT {
            return Err(codec::invalid());
        }
        let obligation_id = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(0)?)?;
        let adoption_sequence: i64 = row.get(1)?;
        let operation_id = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(2)?)?;
        let previous_owner_id = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(3)?)?;
        let current_owner_id = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(4)?)?;
        let original_r_sequence: i64 = row.get(5)?;
        let head = AssetEventRefV1 {
            operation_id: v2::Id::new(codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(6)?)?)?,
            revision: u64::try_from(row.get::<_, i64>(7)?).map_err(|_| codec::invalid())?,
        };
        let root_member_id = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(8)?)?;
        let root_pid = u32::try_from(row.get::<_, i64>(9)?).map_err(|_| codec::invalid())?;
        let root_creation_marker: String = row.get(10)?;
        let root_creation_marker = root_creation_marker
            .parse::<u64>()
            .map_err(|_| codec::invalid())?;
        let record = obligations
            .iter()
            .find(|r| r.obligation_id == obligation_id)
            .ok_or_else(codec::invalid)?;
        if effective.contains_key(&obligation_id) {
            // This version supports one exact old-domain handoff, not a chain
            // of reconstructed producer owners.
            return Err(codec::invalid());
        }
        let ObligationInventory::TypedV2 { manifest, members } = &record.inventory else {
            return Err(codec::invalid());
        };
        let root = manifest
            .members
            .iter()
            .find_map(|member| match member {
                v2::MemberPlan::LegacyV1 { planned }
                    if planned.kind == MemberKind::Root && planned.member_id == root_member_id =>
                {
                    Some(planned)
                }
                _ => None,
            })
            .ok_or_else(codec::invalid)?;
        let root_record = members
            .iter()
            .find(|member| member.envelope.planned.member_id() == root_member_id)
            .ok_or_else(codec::invalid)?;
        let Some(v2::Observed::LegacyV1 { identity }) = &root_record.envelope.observed else {
            return Err(codec::invalid());
        };
        let process = identity.process.as_ref().ok_or_else(codec::invalid)?;
        let scratch_head = members
            .iter()
            .find_map(|member| match &member.envelope.observed {
                Some(v2::Observed::ScratchAssetsV3 { state }) => state.asset.head.as_ref(),
                _ => None,
            })
            .ok_or_else(codec::invalid)?;
        if !owners.contains(&previous_owner_id)
            || !owners.contains(&current_owner_id)
            || record.owner_id != previous_owner_id
            || previous_owner_id == current_owner_id
            || adoption_sequence <= original_r_sequence
            || adoption_sequence > next
            || original_r_sequence < 1
            || original_r_sequence > record.sequence
            || !matches!(
                record.phase,
                Phase::AcquisitionPossible | Phase::Sealed | Phase::UnknownHeld
            )
            || !matches!(
                &root.recovery,
                RecoveryMethod::TrustedFixedSoftwareSameBootV1 { .. }
            )
            || root_record.phase != "acquired"
            || process.pid != root_pid
            || process.creation_marker != Some(root_creation_marker)
        {
            return Err(codec::invalid());
        }
        let _current_head = scratch_head;
        let holder = RecoveredRHolder {
            obligation_id,
            adoption_sequence,
            operation_id,
            previous_owner_id,
            current_owner_id,
            original_r_sequence,
            head,
            root_member_id,
            root_pid,
            root_creation_marker,
        };
        let result_json: String = connection.query_row(
            "SELECT result_json FROM operations WHERE operation_id=?1",
            [operation_id.as_bytes()],
            |row| row.get(0),
        )?;
        let DiskResult::RecoveredRAdoption(ack) = codec::decode(&result_json)? else {
            return Err(codec::invalid());
        };
        if ack.operation_id != operation_id
            || ack.obligation_id != obligation_id
            || ack.previous_owner_id != previous_owner_id
            || ack.current_owner_id != current_owner_id
            || ack.sequence != adoption_sequence
        {
            return Err(codec::invalid());
        }
        effective.insert(obligation_id, (current_owner_id, adoption_sequence));
        holders.push(holder);
    }
    validate_overlay_history(connection, &holders, obligations, next)?;
    Ok(holders)
}

/// Replay the post-adoption producer fold from its exact imported head. The
/// adoption itself does not rewrite the original R row or release its charge.
fn validate_overlay_history(
    connection: &Connection,
    holders: &[RecoveredRHolder],
    obligations: &[ObligationRecord],
    next: i64,
) -> Result<(), CustodyError> {
    if holders.is_empty() {
        return Ok(());
    }
    let mut tracked = BTreeMap::<Uuid, (i64, i64, AssetEventRefV1, bool)>::new();
    for holder in holders {
        if tracked
            .insert(
                holder.obligation_id,
                (
                    holder.original_r_sequence,
                    holder.adoption_sequence,
                    holder.head.clone(),
                    false,
                ),
            )
            .is_some()
        {
            return Err(codec::invalid());
        }
    }
    let mut sequence = 0_i64;
    let mut rows =
        connection.prepare("SELECT operation_id,result_json FROM operations ORDER BY rowid")?;
    let mut query = rows.query([])?;
    while let Some(row) = query.next()? {
        let operation_id = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(0)?)?;
        let result: DiskResult = codec::decode(&row.get::<_, String>(1)?)?;
        let committed = match &result {
            DiskResult::Prepared { sequence, .. } => *sequence,
            DiskResult::Changed(ack) => ack.sequence,
            DiskResult::AssetChangedV2(record) => record.ack.sequence,
            DiskResult::AssetChangedV3(record) => record.ack.sequence,
            DiskResult::RetiredWithAssets(ack) => ack.sequence,
            DiskResult::RecoveredSAdoption(ack) | DiskResult::RecoveredRAdoption(ack) => {
                ack.sequence
            }
        };
        sequence = sequence.checked_add(1).ok_or_else(codec::invalid)?;
        if committed != sequence {
            return Err(codec::invalid());
        }
        match result {
            DiskResult::RecoveredRAdoption(ack) if tracked.contains_key(&ack.obligation_id) => {
                let holder = holders
                    .iter()
                    .find(|holder| holder.obligation_id == ack.obligation_id)
                    .ok_or_else(codec::invalid)?;
                let state = tracked
                    .get_mut(&ack.obligation_id)
                    .ok_or_else(codec::invalid)?;
                if state.3
                    || operation_id != holder.operation_id
                    || ack.sequence != holder.adoption_sequence
                    || ack.obligation_id != holder.obligation_id
                {
                    return Err(codec::invalid());
                }
                state.3 = true;
            }
            DiskResult::AssetChangedV3(record)
                if tracked.contains_key(&record.ack.obligation_id) =>
            {
                let state = tracked
                    .get_mut(&record.ack.obligation_id)
                    .ok_or_else(codec::invalid)?;
                if record.ack.sequence > state.1 {
                    if !state.3 || record.change.previous.as_ref() != Some(&state.2) {
                        return Err(codec::invalid());
                    }
                    state.0 = record.ack.sequence;
                    state.1 = record.ack.sequence;
                    state.2 = record.after.asset.head.ok_or_else(codec::invalid)?;
                }
            }
            DiskResult::Changed(ack)
                if tracked.get(&ack.obligation_id).is_some_and(|state| state.3) =>
            {
                return Err(codec::invalid())
            }
            DiskResult::RetiredWithAssets(ack)
                if tracked
                    .get(&ack.source_obligation_id.uuid())
                    .is_some_and(|state| state.3) =>
            {
                return Err(codec::invalid())
            }
            DiskResult::AssetChangedV2(record)
                if tracked
                    .get(&record.ack.obligation_id)
                    .is_some_and(|state| state.3) =>
            {
                return Err(codec::invalid())
            }
            _ => {}
        }
    }
    if sequence != next {
        return Err(codec::invalid());
    }
    for holder in holders {
        let (last_data, _, head, adopted) = tracked
            .get(&holder.obligation_id)
            .ok_or_else(codec::invalid)?;
        let record = obligations
            .iter()
            .find(|record| record.obligation_id == holder.obligation_id)
            .ok_or_else(codec::invalid)?;
        let ObligationInventory::TypedV2 { members, .. } = &record.inventory else {
            return Err(codec::invalid());
        };
        let current_head = members
            .iter()
            .find_map(|member| match &member.envelope.observed {
                Some(v2::Observed::ScratchAssetsV3 { state }) => state.asset.head.as_ref(),
                _ => None,
            })
            .ok_or_else(codec::invalid)?;
        if !*adopted || *last_data != record.sequence || head != current_head {
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
    proof: &crate::adaptive_resources::runtime::recovery::NativeActiveRImportProof,
) -> Result<DiskResult, CustodyError> {
    let snapshot = super::snapshot(tx, witness)?;
    if snapshot.recovered_r_holders.len() >= codec::ACTIVE_LIMIT {
        return Err(CustodyError::new(
            ErrorKind::Exhausted,
            "recovered R holder history bound reached",
        ));
    }
    let claim = proof.claim();
    let record = snapshot
        .obligations
        .iter()
        .find(|record| record.obligation_id == proof.obligation_id())
        .ok_or_else(codec::invalid)?;
    let effective = snapshot
        .recovered_r_holders
        .iter()
        .filter(|holder| holder.obligation_id == record.obligation_id)
        .max_by_key(|holder| holder.adoption_sequence)
        .map_or(record.owner_id, |holder| holder.current_owner_id);
    let current = snapshot
        .owners
        .iter()
        .find(|identity| identity.owner_id == owner)
        .ok_or_else(codec::invalid)?;
    if proof.journal_id() != witness.journal_id
        || proof.installation_id() != witness.installation_id
        || proof.current_owner_id() != owner
        || proof.previous_owner_id() != effective
        || proof.original_sequence() != record.sequence
        || claim.graph_id() != record.graph_id
        || claim.resources() != record.resources
        || !proof.root_exit().matches(
            &snapshot,
            current,
            record.obligation_id,
            record.graph_id,
            proof.root_exit().root_member_id(),
        )
    {
        return Err(codec::invalid());
    }
    let chosen = crate::resource_custody::recovery::select_active_finite_r(
        tx,
        &snapshot,
        proof.root_exit(),
        current,
    )?;
    if chosen.obligation_id() != claim.obligation_id()
        || chosen.old_owner_id() != claim.old_owner_id()
        || chosen.sequence() != claim.sequence()
        || chosen.state_head() != claim.state_head()
        || chosen.scratch_member_id() != claim.scratch_member_id()
    {
        return Err(codec::invalid());
    }
    let sequence = next_sequence(tx)?;
    tx.execute(
        "INSERT INTO recovered_r_holders VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![
            record.obligation_id.as_bytes(),
            sequence,
            operation_id.as_bytes(),
            effective.as_bytes(),
            owner.as_bytes(),
            record.sequence,
            proof.state_head().operation_id.uuid().as_bytes(),
            i64::try_from(proof.state_head().revision).map_err(|_| codec::invalid())?,
            proof.root_exit().root_member_id().as_bytes(),
            i64::from(proof.root_exit().pid()),
            proof.root_exit().creation_marker().to_string(),
        ],
    )?;
    Ok(DiskResult::RecoveredRAdoption(RecoveryAdoptionAck {
        operation_id,
        obligation_id: record.obligation_id,
        previous_owner_id: effective,
        current_owner_id: owner,
        sequence,
    }))
}

/// Only the actual imported producer owner can write the exact scratch cleanup
/// fold. This grants no new acquisition and no settlement or resource credit.
#[cfg(target_os = "linux")]
pub(super) fn change(
    tx: &Transaction<'_>,
    witness: &storage::Witness,
    owner: Uuid,
    key: &TransitionKey,
    mutation: &Mutation,
    authority: &crate::adaptive_resources::runtime::recovery::RecoveredRAuthority,
) -> Result<DiskResult, CustodyError> {
    let h = &key.handle;
    if authority.journal_id() != witness.journal_id
        || authority.installation_id() != witness.installation_id
        || authority.obligation_id() != h.obligation_id
        || authority.graph_id() != h.graph_id
        || authority.current_owner_id() != owner
        || authority.binding() != h.runtime_binding
        || h.journal_id != witness.journal_id
        || h.owner_id != owner
    {
        return Err(codec::invalid());
    }
    let row: (i64, Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>, i64, Vec<u8>, i64, String) = tx.query_row(
        "SELECT adoption_sequence,operation_id,previous_owner_id,current_owner_id,head_operation_id,head_revision,root_member_id,root_pid,root_creation_marker FROM recovered_r_holders WHERE obligation_id=?1 ORDER BY adoption_sequence DESC LIMIT 1",
        [h.obligation_id.as_bytes()],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?)),
    )?;
    if row.0 != authority.adoption_sequence()
        || codec::uuid_from_bytes(&row.1)? != authority.adoption_operation_id()
        || codec::uuid_from_bytes(&row.2)? != authority.original_owner_id()
        || codec::uuid_from_bytes(&row.3)? != owner
        || codec::uuid_from_bytes(&row.4)? != authority.state_head().operation_id.uuid()
        || u64::try_from(row.5).map_err(|_| codec::invalid())? != authority.state_head().revision
        || codec::uuid_from_bytes(&row.6)? != authority.root_member_id()
        || u32::try_from(row.7).map_err(|_| codec::invalid())? != authority.root_pid()
        || row.8 != authority.root_creation_marker().to_string()
    {
        return Err(codec::invalid());
    }
    let (graph, recorded_owner, phase, sequence, manifest_json):
        (Vec<u8>, Vec<u8>, String, i64, String) = tx.query_row(
        "SELECT graph_id,owner_id,phase,sequence,manifest_json FROM obligations WHERE obligation_id=?1",
        [h.obligation_id.as_bytes()],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
    )?;
    let v2::ManifestDocument::V2(manifest) = v2::decode_manifest(&manifest_json)? else {
        return Err(codec::invalid());
    };
    let prior = Phase::parse(&phase)?;
    if codec::uuid_from_bytes(&graph)? != h.graph_id
        || codec::uuid_from_bytes(&recorded_owner)? != authority.original_owner_id()
        || manifest.owner_id.uuid() != authority.original_owner_id()
        || manifest.graph_id.uuid() != h.graph_id
        || sequence != key.expected_sequence
        || !matches!(
            prior,
            Phase::AcquisitionPossible | Phase::Sealed | Phase::UnknownHeld
        )
    {
        return Err(codec::invalid());
    }
    match mutation {
        Mutation::AssetChangeV3 { change } => {
            scratch::bootstrap::change(tx, key, prior, &manifest, change)
        }
        _ => Err(codec::invalid()),
    }
}
