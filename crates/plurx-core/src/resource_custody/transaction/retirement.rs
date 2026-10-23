//! One conserving journal transition: producer R settles as retained S is
//! installed. The original nonclone native receipt stays in the worker book.
use super::*;
use crate::resource_custody::codec_v2 as v2;

#[cfg(test)]
mod tests;

pub(super) fn change(
    tx: &Transaction<'_>,
    journal: Uuid,
    owner: Uuid,
    key: &TransitionKey,
    prior: Phase,
    source: &v2::Manifest,
    proof: &PhysicalAssetRetirement,
) -> Result<DiskResult, CustodyError> {
    let target = &proof.target;
    let custody = &proof.retained;
    if !matches!(prior, Phase::Sealed | Phase::UnknownHeld)
        || proof.receipt.source_binding() != key.handle.runtime_binding
        || proof.receipt.target_binding() != target.runtime_binding
        || target.journal_id != journal
        || target.owner_id != owner
        || target.obligation_id == key.handle.obligation_id
        || target.graph_id == key.handle.graph_id
        || target.graph_id != proof.target_manifest.graph_id.uuid()
        || proof.target_manifest.owner_id.uuid() != owner
        || custody.producer_obligation_id.uuid() != key.handle.obligation_id
        || custody.producer_graph_id.uuid() != key.handle.graph_id
        || custody.target_obligation_id.uuid() != target.obligation_id
        || custody.target_graph_id.uuid() != target.graph_id
        || custody.target_owner_id.uuid() != owner
        || custody.transfer_operation_id.uuid() != key.operation_id
        || proof.settlement.operation_id.uuid() != key.operation_id
    {
        return Err(codec::invalid());
    }
    v2::validate_retained_assets_v2(&proof.target_manifest, custody)?;
    let mut envelopes = Vec::new();
    let mut statement = tx.prepare(
        "SELECT identity_json,phase FROM members WHERE obligation_id=?1 ORDER BY member_id",
    )?;
    let mut rows = statement.query([key.handle.obligation_id.as_bytes()])?;
    while let Some(row) = rows.next()? {
        let phase: String = row.get(1)?;
        // This first concrete transfer has a fully acquired producer. Partial
        // starts require their disjoint never-acquired settlement adapter.
        if phase != "acquired" {
            return Err(codec::invalid());
        }
        let envelope: v2::Envelope = codec::decode(&row.get::<_, String>(0)?)?;
        v2::validate_envelope(source, &envelope)?;
        envelopes.push(envelope);
    }
    v2::validate_settlement(source, &envelopes, &proof.settlement)?;
    let transfers: Vec<_> = proof
        .settlement
        .members
        .iter()
        .filter_map(|member| match member {
            v2::MemberDisposition::Transferred {
                transfer,
                target_obligation_id,
                target_graph_id,
                asset_id,
                operation_id,
            } => Some((
                transfer,
                target_obligation_id,
                target_graph_id,
                asset_id,
                operation_id,
            )),
            _ => None,
        })
        .collect();
    if transfers.len() != 1
        || transfers[0]
            != (
                &custody.transfer,
                &custody.target_obligation_id,
                &custody.target_graph_id,
                &custody.asset_id,
                &custody.transfer_operation_id,
            )
    {
        return Err(codec::invalid());
    }
    let v2::TransferIdentity::ScratchAssetsV3 { plan, snapshot } = &custody.transfer.identity
    else {
        return Err(codec::invalid());
    };
    let source_envelope = envelopes
        .iter()
        .find(|envelope| envelope.planned.member_id() == custody.transfer.source_member_id.uuid())
        .ok_or_else(codec::invalid)?;
    let Some(v2::Observed::ScratchAssetsV3 { state }) = &source_envelope.observed else {
        return Err(codec::invalid());
    };
    // Reconstruct from original operations inside THIS transaction. A matching
    // projection/hash supplied in a request is never native or lineage proof.
    let fold = scratch::bootstrap::restore(
        tx,
        key.handle.obligation_id,
        key.handle.graph_id,
        custody.transfer.source_member_id,
        plan,
        Some(state),
    )?;
    let full = fold.frozen_snapshot(plan)?;
    v2::scratch::bootstrap::validate_frozen_snapshot_ref(plan, snapshot, &full)?;
    v2::validate_retained_resources_v2(
        &proof.target_manifest,
        custody,
        &full,
        proof.target_resources,
    )?;
    let (original, epoch, trial): (Vec<u8>, Vec<u8>, Option<Vec<u8>>) = tx.query_row(
        "SELECT resources,allocation_epoch,trial_group_id FROM obligations WHERE obligation_id=?1",
        [key.handle.obligation_id.as_bytes()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if codec::vector_from_bytes(&original)?
        .checked_sub(proof.target_resources)
        .is_none()
    {
        return Err(codec::invalid());
    }
    let target_exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM obligations WHERE obligation_id=?1 OR graph_id=?2)",
        params![target.obligation_id.as_bytes(), target.graph_id.as_bytes()],
        |row| row.get(0),
    )?;
    if target_exists {
        return Err(codec::invalid());
    }
    // Pre-encode bounded records before writing any rows. SQLite's transaction
    // still rolls back every source/target change together on any later error.
    let settlement_json = codec::encode(&proof.settlement)?;
    let custody_json = codec::encode(custody)?;
    let target_manifest_json = codec::encode(&proof.target_manifest)?;
    let target_member = &proof.target_manifest.members[0];
    let mut retained_state = snapshot.state.clone();
    retained_state.asset.lifecycle = v2::scratch::ScratchLifecycleV1::RetainedReadOnly;
    let target_envelope = v2::Envelope {
        version: 2,
        planned: target_member.clone(),
        observed: Some(v2::Observed::ScratchAssetsV3 {
            state: retained_state,
        }),
    };
    v2::validate_envelope(&proof.target_manifest, &target_envelope)?;
    let target_identity_json = codec::encode(&target_envelope)?;
    let sequence = next_sequence(tx)?;
    tx.execute(
        "INSERT INTO obligations VALUES(?1,?2,?3,?4,?5,?6,'sealed',?7,?8,?9,NULL,NULL)",
        params![
            target.obligation_id.as_bytes(),
            owner.as_bytes(),
            target.graph_id.as_bytes(),
            epoch,
            codec::vector_bytes(proof.target_resources),
            trial,
            sequence,
            key.operation_id.as_bytes(),
            target_manifest_json
        ],
    )?;
    tx.execute(
        "INSERT INTO members VALUES(?1,?2,?3,?4,1,'acquired',?5,NULL)",
        params![
            target.obligation_id.as_bytes(),
            target_member.member_id().as_bytes(),
            target_member.kind().as_str(),
            target_member.adapter(),
            target_identity_json
        ],
    )?;
    tx.execute(
        "INSERT INTO retained_asset_owners VALUES(?1,?2,?3,?4,?5)",
        params![
            target.obligation_id.as_bytes(),
            custody.asset_id.uuid().as_bytes(),
            custody.rendition_id,
            custody.generation_epoch.to_le_bytes(),
            custody_json
        ],
    )?;
    let changed = tx.execute(
        "UPDATE obligations SET phase='settled',sequence=?2,last_operation_id=?3,hold_reason=NULL,settlement_json=?4 WHERE obligation_id=?1 AND sequence=?5",
        params![key.handle.obligation_id.as_bytes(),sequence,key.operation_id.as_bytes(),
            settlement_json,key.expected_sequence],
    )?;
    if changed != 1 {
        return Err(codec::invalid());
    }
    tx.execute(
        "UPDATE members SET phase='closed',proof_json=?2 WHERE obligation_id=?1",
        params![key.handle.obligation_id.as_bytes(), settlement_json],
    )?;
    Ok(DiskResult::RetiredWithAssets(RetirementAck {
        version: 2,
        operation_id: v2::Id::new(key.operation_id)?,
        sequence,
        source_obligation_id: v2::Id::new(key.handle.obligation_id)?,
        source_graph_id: v2::Id::new(key.handle.graph_id)?,
        target_obligation_id: v2::Id::new(target.obligation_id)?,
        target_graph_id: v2::Id::new(target.graph_id)?,
        asset_id: custody.asset_id,
    }))
}
