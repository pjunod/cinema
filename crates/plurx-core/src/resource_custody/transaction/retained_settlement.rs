//! Settle one fully retired typed retained-S obligation without refunding an
//! unverified projection. The journal reconstructs the same event fold inside
//! this SQLite transaction before accepting its terminal head.
use super::*;
use crate::resource_custody::codec_v2 as v2;

pub(super) fn change(
    tx: &Transaction<'_>,
    owner: Uuid,
    key: &TransitionKey,
    prior: Phase,
    manifest: &v2::Manifest,
    proof: &PhysicalTypedSettlement,
) -> Result<DiskResult, CustodyError> {
    if !matches!(prior, Phase::Sealed | Phase::UnknownHeld)
        || proof.receipt.custody_binding() != key.handle.runtime_binding
        || manifest.owner_id.uuid() != owner
        || manifest.graph_id.uuid() != key.handle.graph_id
        || proof.settlement.version != 2
        || proof.settlement.owner_id != manifest.owner_id
        || proof.settlement.graph_id != manifest.graph_id
        || proof.settlement.operation_id.uuid() != key.operation_id
        || manifest.members.len() != 1
    {
        return Err(codec::invalid());
    }
    let (slot, plan) = match &manifest.members[0] {
        v2::MemberPlan::ScratchAssetsV3 { slot, plan } => (slot, plan),
        _ => return Err(codec::invalid()),
    };
    let (fact_plan, exact_head, close_operation) = match proof.settlement.members.as_slice() {
        [v2::MemberDisposition::Closed {
            member_id,
            fact:
                v2::ClosedFact::ScratchAssetsV3 {
                    plan,
                    exact_head,
                    operation_id,
                },
        }] if *member_id == slot.member_id => (plan, exact_head, operation_id),
        _ => return Err(codec::invalid()),
    };
    if fact_plan != plan || close_operation != &exact_head.operation_id || exact_head.revision == 0
    {
        return Err(codec::invalid());
    }

    let (member_phase, envelope_json): (String, String) = tx.query_row(
        "SELECT phase,identity_json FROM members WHERE obligation_id=?1 AND member_id=?2",
        params![
            key.handle.obligation_id.as_bytes(),
            slot.member_id.uuid().as_bytes()
        ],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if member_phase != "acquired" {
        return Err(codec::invalid());
    }
    let envelope: v2::Envelope = codec::decode(&envelope_json)?;
    v2::validate_envelope(manifest, &envelope)?;
    let Some(v2::Observed::ScratchAssetsV3 { state }) = &envelope.observed else {
        return Err(codec::invalid());
    };
    if state.asset.head.as_ref() != Some(exact_head)
        || state.asset.lifecycle != v2::scratch::ScratchLifecycleV1::RetiredClosed
    {
        return Err(codec::invalid());
    }
    v2::validate_settlement(manifest, std::slice::from_ref(&envelope), &proof.settlement)?;

    // This must be the one original retained-owner row for S. Preserve that
    // immutable lineage row; phase=settled makes the loader omit it as active.
    let (asset_bytes, rendition_id, generation_epoch, custody_json):
        (Vec<u8>, String, Vec<u8>, String) = tx.query_row(
        "SELECT asset_id,rendition_id,generation_epoch,custody_json FROM retained_asset_owners WHERE obligation_id=?1",
        [key.handle.obligation_id.as_bytes()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    let asset_id = codec::uuid_from_bytes(&asset_bytes)?;
    let generation_epoch = codec::epoch_from_bytes(&generation_epoch)?;
    let v2::RetainedAssetsDocument::V2(custody) = v2::decode_retained_assets(&custody_json)? else {
        return Err(codec::invalid());
    };
    v2::validate_retained_assets_v2(manifest, &custody)?;
    let v2::TransferIdentity::ScratchAssetsV3 {
        plan: original_plan,
        snapshot,
    } = &custody.transfer.identity
    else {
        return Err(codec::invalid());
    };
    if custody.target_obligation_id.uuid() != key.handle.obligation_id
        || custody.target_graph_id.uuid() != key.handle.graph_id
        || custody.target_owner_id.uuid() != owner
        || custody.asset_id.uuid() != asset_id
        || custody.rendition_id != rendition_id
        || custody.generation_epoch != generation_epoch
        || original_plan != plan
        || snapshot.member_id != slot.member_id
        || custody.transfer.target_member_id != slot.member_id
        || slot.owned_by.is_some()
        || !slot.depends_on.is_empty()
    {
        return Err(codec::invalid());
    }

    // Rebuild the latest append-only state from original journal operations,
    // not from the settlement request's data-only head projection.
    let fold = scratch::bootstrap::restore(
        tx,
        key.handle.obligation_id,
        key.handle.graph_id,
        slot.member_id,
        plan,
        Some(state),
    )?;
    let closed = fold.state();
    if closed != *state
        || closed.asset.head.as_ref() != Some(exact_head)
        || closed.asset.lifecycle != v2::scratch::ScratchLifecycleV1::RetiredClosed
        || closed.asset.directory.is_some()
        || closed.asset.totals != v2::scratch::AssetTotalsV1::default()
        || closed.bootstrap.phase != v2::scratch::bootstrap::BootstrapPhase::Closed
        || closed.bootstrap.current.is_some()
        || closed.bootstrap.parent.is_some()
        || closed.bootstrap.possible_child.is_some()
        || closed.bootstrap.retained_parent_synced
        || !fold.files().is_empty()
    {
        return Err(codec::invalid());
    }

    let settlement_json = codec::encode(&proof.settlement)?;
    let sequence = next_sequence(tx)?;
    let changed = tx.execute(
        "UPDATE obligations SET phase='settled',sequence=?2,last_operation_id=?3,hold_reason=NULL,settlement_json=?4 WHERE obligation_id=?1 AND sequence=?5",
        params![
            key.handle.obligation_id.as_bytes(),
            sequence,
            key.operation_id.as_bytes(),
            settlement_json,
            key.expected_sequence
        ],
    )?;
    if changed != 1 {
        return Err(codec::invalid());
    }
    tx.execute(
        "UPDATE members SET phase='closed',proof_json=?2 WHERE obligation_id=?1 AND member_id=?3 AND phase='acquired'",
        params![
            key.handle.obligation_id.as_bytes(),
            codec::encode(&proof.settlement)?,
            slot.member_id.uuid().as_bytes()
        ],
    )?;
    Ok(DiskResult::Changed(CommitAck {
        operation_id: key.operation_id,
        obligation_id: key.handle.obligation_id,
        graph_id: key.handle.graph_id,
        sequence,
        phase: Phase::Settled,
    }))
}
