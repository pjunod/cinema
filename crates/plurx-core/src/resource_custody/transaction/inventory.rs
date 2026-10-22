//! Version-dispatched inventory reconstruction; audit rows never become proof.
use super::*;
use crate::resource_custody::codec_v2::{self as v2, ManifestDocument};

pub(super) fn load(
    connection: &Connection,
    id: Uuid,
    owner: Uuid,
    graph: Uuid,
    phase: Phase,
    manifest_json: &str,
    settlement_json: Option<&str>,
) -> Result<ObligationInventory, CustodyError> {
    match v2::decode_manifest(manifest_json)? {
        ManifestDocument::V1(manifest) => {
            if manifest.owner_id != owner || manifest.graph_id != graph {
                return Err(codec::invalid());
            }
            let settlement: Option<SettlementRecord> =
                settlement_json.map(codec::decode).transpose()?;
            let mut stmt = connection.prepare("SELECT member_id,kind,adapter,adapter_version,phase,identity_json,proof_json FROM members WHERE obligation_id=?1 ORDER BY member_id")?;
            let mut rows = stmt.query([id.as_bytes()])?;
            let mut members = Vec::new();
            while let Some(row) = rows.next()? {
                let mid = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(0)?)?;
                let kind: String = row.get(1)?;
                let adapter: String = row.get(2)?;
                let version: u32 = row.get(3)?;
                let member_phase: String = row.get(4)?;
                let envelope = v2::decode_legacy_envelope(&row.get::<_, String>(5)?)?;
                let proof: Option<String> = row.get(6)?;
                let plan = manifest
                    .members
                    .iter()
                    .find(|m| m.member_id == mid)
                    .ok_or_else(codec::invalid)?;
                if &envelope.planned != plan
                    || kind != plan.kind.as_str()
                    || adapter != plan.adapter.as_str()
                    || version != plan.adapter_version
                {
                    return Err(codec::invalid());
                }
                if let Some(identity) = &envelope.observed {
                    codec::validate_observed(plan, identity)?;
                }
                validate_member_phase(
                    phase,
                    &member_phase,
                    envelope.observed.is_some(),
                    proof.is_some(),
                )?;
                if let Some(proof) = proof {
                    if settlement.as_ref() != Some(&codec::decode::<SettlementRecord>(&proof)?) {
                        return Err(codec::invalid());
                    }
                }
                members.push(MemberRecord {
                    plan: plan.clone(),
                    phase: member_phase,
                    observed: envelope.observed,
                });
                if members.len() > 129 {
                    return Err(codec::invalid());
                }
            }
            if members.len() != manifest.members.len()
                || (phase == Phase::AcquisitionPossible
                    && members.iter().all(|m| m.phase == "planned"))
            {
                return Err(codec::invalid());
            }
            if let Some(record) = &settlement {
                validate_settlement_record(&manifest, &members, record)?;
            }
            Ok(ObligationInventory::LegacyV1 { manifest, members })
        }
        ManifestDocument::V2(manifest) => {
            let version: i64 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
            if version != 2 {
                return Err(codec::invalid());
            }
            if manifest.owner_id.uuid() != owner || manifest.graph_id.uuid() != graph {
                return Err(codec::invalid());
            }
            let settlement: Option<v2::Settlement> =
                settlement_json.map(codec::decode).transpose()?;
            let mut stmt = connection.prepare("SELECT member_id,kind,adapter,adapter_version,phase,identity_json,proof_json FROM members WHERE obligation_id=?1 ORDER BY member_id")?;
            let mut rows = stmt.query([id.as_bytes()])?;
            let mut members = Vec::new();
            while let Some(row) = rows.next()? {
                let mid = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(0)?)?;
                let kind: String = row.get(1)?;
                let adapter: String = row.get(2)?;
                let version: u32 = row.get(3)?;
                let member_phase: String = row.get(4)?;
                let envelope: v2::Envelope = codec::decode(&row.get::<_, String>(5)?)?;
                let proof: Option<String> = row.get(6)?;
                v2::validate_envelope(&manifest, &envelope)?;
                if mid != envelope.planned.member_id()
                    || kind != envelope.planned.kind().as_str()
                    || adapter != envelope.planned.adapter()
                    || version != 1
                {
                    return Err(codec::invalid());
                }
                validate_member_phase(
                    phase,
                    &member_phase,
                    envelope.observed.is_some(),
                    proof.is_some(),
                )?;
                if let Some(proof) = proof {
                    if settlement.as_ref() != Some(&codec::decode::<v2::Settlement>(&proof)?) {
                        return Err(codec::invalid());
                    }
                }
                members.push(TypedMemberRecord {
                    envelope,
                    phase: member_phase,
                });
                if members.len() > 129 {
                    return Err(codec::invalid());
                }
            }
            if members.len() != manifest.members.len()
                || (phase == Phase::AcquisitionPossible
                    && members.iter().all(|m| m.phase == "planned"))
            {
                return Err(codec::invalid());
            }
            if let Some(record) = &settlement {
                v2::validate_settlement(
                    &manifest,
                    &members
                        .iter()
                        .map(|m| m.envelope.clone())
                        .collect::<Vec<_>>(),
                    record,
                )?;
                for member in &record.members {
                    if let v2::MemberDisposition::Transferred {
                        transfer,
                        target_obligation_id,
                        target_graph_id,
                        asset_id,
                        operation_id,
                    } = member
                    {
                        let custody_json: String = connection.query_row("SELECT custody_json FROM retained_asset_owners WHERE obligation_id=?1 AND asset_id=?2", params![target_obligation_id.uuid().as_bytes(), asset_id.uuid().as_bytes()], |r| r.get(0))?;
                        let custody: v2::RetainedAssets = codec::decode(&custody_json)?;
                        if custody.producer_obligation_id.uuid() != id
                            || custody.producer_graph_id != manifest.graph_id
                            || custody.target_graph_id != *target_graph_id
                            || custody.transfer_operation_id != *operation_id
                            || !custody.transfers.contains(transfer)
                        {
                            return Err(codec::invalid());
                        }
                    }
                }
            }
            Ok(ObligationInventory::TypedV2 { manifest, members })
        }
    }
}
fn validate_member_phase(
    phase: Phase,
    member_phase: &str,
    observed: bool,
    proof: bool,
) -> Result<(), CustodyError> {
    if !["planned", "acquisition_possible", "acquired", "closed"].contains(&member_phase)
        || (member_phase == "acquired" && !observed)
        || (["planned", "acquisition_possible"].contains(&member_phase) && observed)
        || (member_phase == "closed") != proof
        || (phase == Phase::Prepared && member_phase != "planned")
        || (phase == Phase::Settled) != (member_phase == "closed")
    {
        return Err(codec::invalid());
    }
    Ok(())
}
pub(super) fn protected_paths(inventory: &ObligationInventory) -> Vec<PathBuf> {
    match inventory {
        ObligationInventory::LegacyV1 { manifest, .. } => manifest
            .members
            .iter()
            .flat_map(|m| &m.protected_paths)
            .map(PathBuf::from)
            .collect(),
        ObligationInventory::TypedV2 { manifest, .. } => manifest
            .members
            .iter()
            .flat_map(|m| match m {
                v2::MemberPlan::LegacyV1 { planned } => &planned.protected_paths,
                v2::MemberPlan::SourceV1 { slot, .. }
                | v2::MemberPlan::VodOutputTaskV1 { slot, .. }
                | v2::MemberPlan::ScratchAssetsV1 { slot, .. } => &slot.protected_paths,
            })
            .map(PathBuf::from)
            .collect(),
    }
}
