//! The second ownership kind in the same journal, not an orphan exception.
use super::*;
use crate::resource_custody::codec_v2 as v2;

pub(super) fn load(
    connection: &Connection,
    all_ids: &BTreeMap<Uuid, Phase>,
    family_references: &BTreeSet<Uuid>,
) -> Result<Vec<RetainedAssetOwner>, CustodyError> {
    let version: i64 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version == 1 {
        return Ok(Vec::new());
    }
    let mut stmt = connection.prepare("SELECT obligation_id,asset_id,rendition_id,generation_epoch,custody_json FROM retained_asset_owners ORDER BY obligation_id")?;
    let mut rows = stmt.query([])?;
    let mut active = Vec::new();
    while let Some(row) = rows.next()? {
        let id = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(0)?)?;
        let asset = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(1)?)?;
        let rendition: String = row.get(2)?;
        let epoch = codec::epoch_from_bytes(&row.get::<_, Vec<u8>>(3)?)?;
        let document = v2::decode_retained_assets(&row.get::<_, String>(4)?)?;
        if document.target_obligation_id().uuid() != id
            || document.asset_id().uuid() != asset
            || document.rendition_id() != rendition
            || document.generation_epoch() != epoch
            || family_references.contains(&id)
        {
            return Err(codec::invalid());
        }
        let phase = *all_ids.get(&id).ok_or_else(codec::invalid)?;
        if !matches!(phase, Phase::Sealed | Phase::UnknownHeld | Phase::Settled) {
            return Err(codec::invalid());
        }
        if let v2::RetainedAssetsDocument::V2(custody) = &document {
            validate_v2_row(connection, id, custody)?;
            if phase != Phase::Settled {
                active.push(RetainedAssetOwner {
                    obligation_id: id,
                    custody: document,
                });
            }
            continue;
        }
        let v2::RetainedAssetsDocument::V1(custody) = document else {
            unreachable!()
        };
        let (owner, graph, manifest_json, resources, target_epoch, trial): (Vec<u8>, Vec<u8>, String, Vec<u8>, Vec<u8>, Option<Vec<u8>>) = connection.query_row("SELECT owner_id,graph_id,manifest_json,resources,allocation_epoch,trial_group_id FROM obligations WHERE obligation_id=?1", [id.as_bytes()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?;
        let v2::ManifestDocument::V2(manifest) = v2::decode_manifest(&manifest_json)? else {
            return Err(codec::invalid());
        };
        if codec::uuid_from_bytes(&owner)? != custody.target_owner_id.uuid()
            || codec::uuid_from_bytes(&graph)? != custody.target_graph_id.uuid()
        {
            return Err(codec::invalid());
        }
        v2::validate_retained_assets(&manifest, &custody)?;
        let retained = codec::vector_from_bytes(&resources)?;
        v2::validate_retained_resources(&manifest, retained)?;
        if retained == ResourceVector::default()
            || retained.cpu_ns_per_second != 0
            || retained.processes != 0
            || retained.hardware_slots != 0
            || retained.tuner_slots != 0
        {
            return Err(codec::invalid());
        }
        let (producer_phase, producer_graph, producer_settlement, original, original_epoch, original_trial): (String, Vec<u8>, String, Vec<u8>, Vec<u8>, Option<Vec<u8>>) = connection.query_row("SELECT phase,graph_id,settlement_json,resources,allocation_epoch,trial_group_id FROM obligations WHERE obligation_id=?1", [custody.producer_obligation_id.uuid().as_bytes()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?;
        let original = codec::vector_from_bytes(&original)?;
        if producer_phase != "settled"
            || codec::uuid_from_bytes(&producer_graph)? != custody.producer_graph_id.uuid()
            || original.checked_sub(retained).is_none()
            || original_epoch != target_epoch
            || original_trial != trial
        {
            return Err(codec::invalid());
        }
        let settlement: v2::Settlement = codec::decode(&producer_settlement)?;
        if settlement.operation_id != custody.transfer_operation_id {
            return Err(codec::invalid());
        }
        let transfers: Vec<_> = settlement
            .members
            .iter()
            .filter_map(|d| match d {
                v2::MemberDisposition::Transferred {
                    transfer,
                    target_obligation_id,
                    target_graph_id,
                    asset_id,
                    operation_id,
                } if *target_obligation_id == custody.target_obligation_id
                    && *target_graph_id == custody.target_graph_id
                    && *asset_id == custody.asset_id
                    && *operation_id == custody.transfer_operation_id =>
                {
                    Some(transfer)
                }
                _ => None,
            })
            .collect();
        if transfers.len() != custody.transfers.len()
            || custody.transfers.iter().any(|t| !transfers.contains(&t))
        {
            return Err(codec::invalid());
        }
        for transfer in &custody.transfers {
            let json: String = connection.query_row(
                "SELECT identity_json FROM members WHERE obligation_id=?1 AND member_id=?2",
                params![id.as_bytes(), transfer.target_member_id.uuid().as_bytes()],
                |r| r.get(0),
            )?;
            let envelope: v2::Envelope = codec::decode(&json)?;
            match (&transfer.identity, &envelope.observed) {
                (
                    v2::TransferIdentity::ScratchAssetsV1 { scratch },
                    Some(v2::Observed::ScratchAssetsV1 { scratch: observed }),
                ) if scratch == observed => {}
                (
                    v2::TransferIdentity::SourceV1 {
                        source,
                        retained_copy_ids,
                        ..
                    },
                    Some(v2::Observed::SourceV1 { source: observed }),
                ) => {
                    if source.holder_id != observed.holder_id
                        || source.source_association != observed.source_association
                        || source.access != observed.access
                        || source.native_object != observed.native_object
                        || retained_copy_ids.len() != observed.copies.len()
                    {
                        return Err(codec::invalid());
                    }
                    for old in source
                        .copies
                        .iter()
                        .filter(|c| retained_copy_ids.contains(&c.copy_id))
                    {
                        let new = observed
                            .copies
                            .iter()
                            .find(|c| c.copy_id == old.copy_id)
                            .ok_or_else(codec::invalid)?;
                        if new.acquisition_operation_id != old.acquisition_operation_id
                            || new.parent_copy_id != old.parent_copy_id
                            || !matches!(new.owner, v2::CopyOwner::RetainedAssets { asset_id, source_member_id } if asset_id == custody.asset_id && source_member_id == transfer.target_member_id)
                        {
                            return Err(codec::invalid());
                        }
                    }
                }
                _ => return Err(codec::invalid()),
            }
        }
        if phase != Phase::Settled {
            active.push(RetainedAssetOwner {
                obligation_id: id,
                custody: v2::RetainedAssetsDocument::V1(custody),
            });
        }
    }
    Ok(active)
}

fn validate_v2_row(
    connection: &Connection,
    id: Uuid,
    custody: &v2::RetainedAssetsV2,
) -> Result<(), CustodyError> {
    let (owner, graph, manifest_json, resources, target_epoch, trial):
        (Vec<u8>, Vec<u8>, String, Vec<u8>, Vec<u8>, Option<Vec<u8>>) = connection.query_row(
            "SELECT owner_id,graph_id,manifest_json,resources,allocation_epoch,trial_group_id FROM obligations WHERE obligation_id=?1",
            [id.as_bytes()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )?;
    let v2::ManifestDocument::V2(manifest) = v2::decode_manifest(&manifest_json)? else {
        return Err(codec::invalid());
    };
    if codec::uuid_from_bytes(&owner)? != custody.target_owner_id.uuid()
        || codec::uuid_from_bytes(&graph)? != custody.target_graph_id.uuid()
    {
        return Err(codec::invalid());
    }
    v2::validate_retained_assets_v2(&manifest, custody)?;
    let v2::TransferIdentity::ScratchAssetsV3 { plan, snapshot } = &custody.transfer.identity
    else {
        return Err(codec::invalid());
    };
    let target_json: String = connection.query_row(
        "SELECT identity_json FROM members WHERE obligation_id=?1 AND member_id=?2",
        params![
            id.as_bytes(),
            custody.transfer.target_member_id.uuid().as_bytes()
        ],
        |r| r.get(0),
    )?;
    let target_envelope: v2::Envelope = codec::decode(&target_json)?;
    v2::validate_envelope(&manifest, &target_envelope)?;
    let Some(v2::Observed::ScratchAssetsV3 {
        state: target_state,
    }) = &target_envelope.observed
    else {
        return Err(codec::invalid());
    };
    if !matches!(&target_envelope.planned,
        v2::MemberPlan::ScratchAssetsV3 { slot, plan: target_plan }
            if slot.member_id == custody.transfer.target_member_id && target_plan == plan)
    {
        return Err(codec::invalid());
    }
    let restored_target = super::scratch::bootstrap::restore(
        connection,
        id,
        custody.target_graph_id.uuid(),
        custody.transfer.target_member_id,
        plan,
        Some(target_state),
    )?;
    if restored_target.state() != *target_state {
        return Err(codec::invalid());
    }
    let retained = codec::vector_from_bytes(&resources)?;
    validate_v2_current_totals(target_state, retained)?;
    let source_fold = super::scratch::bootstrap::restore(
        connection,
        custody.producer_obligation_id.uuid(),
        custody.producer_graph_id.uuid(),
        custody.transfer.source_member_id,
        plan,
        Some(&snapshot.state),
    )?;
    let full = source_fold.frozen_snapshot(plan)?;
    v2::validate_retained_resources_v2(&manifest, custody, &full, retained)?;
    let (producer_phase, producer_graph, original, original_epoch, original_trial):
        (String, Vec<u8>, Vec<u8>, Vec<u8>, Option<Vec<u8>>) = connection.query_row(
            "SELECT phase,graph_id,resources,allocation_epoch,trial_group_id FROM obligations WHERE obligation_id=?1",
            [custody.producer_obligation_id.uuid().as_bytes()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )?;
    if producer_phase != "settled"
        || codec::uuid_from_bytes(&producer_graph)? != custody.producer_graph_id.uuid()
        || codec::vector_from_bytes(&original)?
            .checked_sub(retained)
            .is_none()
        || original_epoch != target_epoch
        || original_trial != trial
    {
        return Err(codec::invalid());
    }
    Ok(())
}

/// Later retained reader events must remain within the SAME target obligation's
/// stored credit, even when the original producer plan allows a larger peak.
pub(super) fn validate_v2_current_totals(
    state: &v2::scratch::bootstrap::ScratchStateV3,
    resources: ResourceVector,
) -> Result<(), CustodyError> {
    let totals = &state.asset.totals;
    let current = ResourceVector {
        handles: u64::from(totals.handles_reserved),
        peak_memory_bytes: totals.memory_reserved_bytes,
        scratch_bytes: totals.charged_storage_bytes,
        ..ResourceVector::default()
    };
    if resources.checked_sub(current).is_none() {
        return Err(codec::invalid());
    }
    Ok(())
}
