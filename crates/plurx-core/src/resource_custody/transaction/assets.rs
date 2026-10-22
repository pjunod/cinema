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
        let custody: v2::RetainedAssets = codec::decode(&row.get::<_, String>(4)?)?;
        if custody.target_obligation_id.uuid() != id
            || custody.asset_id.uuid() != asset
            || custody.rendition_id != rendition
            || custody.generation_epoch != epoch
            || family_references.contains(&id)
        {
            return Err(codec::invalid());
        }
        let phase = *all_ids.get(&id).ok_or_else(codec::invalid)?;
        if !matches!(phase, Phase::Sealed | Phase::UnknownHeld | Phase::Settled) {
            return Err(codec::invalid());
        }
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
                custody,
            });
        }
    }
    Ok(active)
}
