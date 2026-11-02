//! Concrete v2 write-ahead records in the SAME journal and transaction queue.
//! These data transitions cannot issue physical-close or retained-asset credit.
use super::*;
use crate::resource_custody::codec_v2 as v2;

pub(super) fn prepare(
    tx: &Transaction<'_>,
    journal: Uuid,
    owner: Uuid,
    operation: Uuid,
    batch: &TypedPrepareBatch,
    handles: &[ObligationHandle],
    trial: Option<Uuid>,
) -> Result<DiskResult, CustodyError> {
    let version: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if !(3..=7).contains(&version)
        || batch.obligations.is_empty()
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
    let held_trial: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM obligations WHERE phase!='settled' AND trial_group_id IS NOT NULL)", [], |r|r.get(0))?;
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
    let mut ids = BTreeSet::new();
    let mut asset_ids = BTreeSet::new();
    let mut existing =
        tx.prepare("SELECT manifest_json FROM obligations WHERE phase!='settled'")?;
    let mut rows = existing.query([])?;
    while let Some(row) = rows.next()? {
        if let v2::ManifestDocument::V2(manifest) = v2::decode_manifest(&row.get::<_, String>(0)?)?
        {
            for member in manifest.members {
                match member {
                    v2::MemberPlan::ScratchAssetsV1 { scratch, .. } => {
                        asset_ids.insert(scratch.asset_id);
                    }
                    v2::MemberPlan::ScratchAssetsV2 { plan, .. } => {
                        asset_ids.insert(plan.asset_id);
                    }
                    v2::MemberPlan::ScratchAssetsV3 { plan, .. } => {
                        asset_ids.insert(plan.asset_id);
                    }
                    _ => {}
                }
            }
        }
    }
    for (new, handle) in batch.obligations.iter().zip(handles) {
        v2::validate_manifest(&new.manifest)?;
        validate_direct_read_domain_link(tx, batch, handles, new, owner)?;
        let fixed_planning_metadata =
            matches!(
                new.memory_claim,
                crate::adaptive_resources::MemoryClaim::PlanningMetadata
            ) && batch.obligations.len() == 2
                && matches!(new.manifest.members.as_slice(),
                [v2::MemberPlan::FixedProbeMetadataV1 { metadata, .. }]
                    if new.resources.peak_memory_bytes >= metadata.retained_memory_bytes)
                && new.resources.cpu_ns_per_second == 0
                && new.resources.processes == 0
                && new.resources.handles == 0
                && new.resources.scratch_bytes == 0
                && new.resources.hardware_slots == 0
                && new.resources.tuner_slots == 0
                && batch.obligations.iter().any(|other| {
                    other.memory_claim
                        == crate::adaptive_resources::MemoryClaim::UnknownProducerPeak
                        && other.manifest.members.iter().any(|member| {
                            matches!(
                                member,
                                v2::MemberPlan::FixedProbeTaskV1 { .. }
                                    | v2::MemberPlan::FixedProbeDiscoverTaskV1 { .. }
                            )
                        })
                        && (other.manifest.members.iter().any(|member| {
                            matches!(member, v2::MemberPlan::FixedProbeTaskV1 { .. })
                        }) || other.manifest.members.iter().any(|member| {
                            matches!(member, v2::MemberPlan::FixedPlanningEngineTaskV1 { .. })
                        }))
                });
        if matches!(
            new.memory_claim,
            crate::adaptive_resources::MemoryClaim::KnownBounded {
                active_producer: false
            }
        ) || (matches!(
            new.memory_claim,
            crate::adaptive_resources::MemoryClaim::PlanningMetadata
        ) && !fixed_planning_metadata)
            || (matches!(
                new.memory_claim,
                crate::adaptive_resources::MemoryClaim::UnknownProducerPeak
            ) && (new.resources.processes == 0 || new.resources.cpu_ns_per_second == 0))
            || handle.journal_id != journal
            || handle.owner_id != owner
            || handle.graph_id != new.manifest.graph_id.uuid()
            || new.manifest.owner_id.uuid() != owner
            || new.resources == ResourceVector::default()
            || !ids.insert(handle.obligation_id)
        {
            return Err(codec::invalid());
        }
        // Retained assets cannot be created by a fresh producer preparation;
        // only the conserving retirement transaction may install such ownership.
        let mut scratch_minimum = ResourceVector::default();
        for member in &new.manifest.members {
            let scratch = match member {
                v2::MemberPlan::ScratchAssetsV2 { plan, .. } => Some((plan.asset_id, &plan.limits)),
                v2::MemberPlan::ScratchAssetsV3 { plan, .. } => Some((plan.asset_id, &plan.limits)),
                _ => None,
            };
            if let Some((asset, limits)) = scratch {
                if !asset_ids.insert(asset) {
                    return Err(codec::invalid());
                }
                scratch_minimum = scratch_minimum
                    .checked_add(ResourceVector {
                        scratch_bytes: limits.reserved_storage_bytes,
                        peak_memory_bytes: limits.resident_memory_bytes,
                        handles: u64::from(limits.live_handles),
                        ..ResourceVector::default()
                    })
                    .ok_or_else(codec::invalid)?;
            }
            if let v2::MemberPlan::SourceV1 { source, .. } = member {
                for copy in &source.planned_copies {
                    match copy.owner {
                        v2::CopyOwner::RetainedAssets { .. } => return Err(codec::invalid()),
                        v2::CopyOwner::RetainedRoleHolder { .. } => {
                            // Actual shared FD handoff needs a concrete retained
                            // holder adapter, not an asserted family/member UUID.
                            return Err(codec::invalid());
                        }
                        _ => {}
                    }
                }
            }
        }
        if new.resources.checked_sub(scratch_minimum).is_none() {
            return Err(codec::invalid());
        }
        aggregate = aggregate
            .checked_add(new.resources)
            .ok_or_else(codec::invalid)?;
    }
    check_memory_claim_combination(tx, batch.obligations.iter().map(|new| new.memory_claim))?;
    let sequence = next_sequence(tx)?;
    for (new, handle) in batch.obligations.iter().zip(handles) {
        tx.execute(
            "INSERT INTO obligations (obligation_id,owner_id,graph_id,allocation_epoch,resources,trial_group_id,phase,sequence,last_operation_id,manifest_json,hold_reason,settlement_json,memory_claim) VALUES(?1,?2,?3,?4,?5,?6,'prepared',?7,?8,?9,NULL,NULL,?10)",
            params![
                handle.obligation_id.as_bytes(),
                owner.as_bytes(),
                handle.graph_id.as_bytes(),
                new.allocation_epoch.to_le_bytes(),
                codec::vector_bytes(new.resources),
                trial.map(|t| t.as_bytes().to_vec()),
                sequence,
                operation.as_bytes(),
                codec::encode(&new.manifest)?,
                new.memory_claim.sql_key(),
            ],
        )?;
        for member in &new.manifest.members {
            let envelope = v2::Envelope {
                version: 2,
                planned: member.clone(),
                observed: None,
            };
            tx.execute(
                "INSERT INTO members VALUES(?1,?2,?3,?4,1,'planned',?5,NULL)",
                params![
                    handle.obligation_id.as_bytes(),
                    member.member_id().as_bytes(),
                    member.kind().as_str(),
                    member.adapter(),
                    codec::encode(&envelope)?
                ],
            )?;
        }
    }
    insert_families(tx, journal, owner, &batch.families, handles, &ids)?;
    Ok(DiskResult::Prepared {
        sequence,
        obligations: handles.iter().map(|h| h.obligation_id).collect(),
        trial,
    })
}

/// The selected source may share only a real prepared direct-read role in
/// this same atomic batch or an exact still-held earlier obligation. A UUID
/// supplied in JSON is never accepted as evidence of a read CPU reservation.
fn validate_direct_read_domain_link(
    tx: &Transaction<'_>,
    batch: &TypedPrepareBatch,
    handles: &[ObligationHandle],
    new: &TypedNewObligation,
    owner: Uuid,
) -> Result<(), CustodyError> {
    let domains: Vec<_> = new
        .manifest
        .members
        .iter()
        .filter_map(|member| match member {
            v2::MemberPlan::DirectSourceV1 { source, .. } => {
                Some(source.read_domain_obligation_id.uuid())
            }
            _ => None,
        })
        .collect();
    if domains.is_empty() {
        return Ok(());
    }
    if domains.len() != 1 || new.manifest.members.len() != 1 || new.resources.cpu_ns_per_second != 0
    {
        return Err(codec::invalid());
    }
    let target = domains[0];
    if let Some((domain, handle)) = batch
        .obligations
        .iter()
        .zip(handles)
        .find(|(_, handle)| handle.obligation_id == target)
    {
        return validate_direct_domain_record(
            &domain.manifest,
            domain.resources,
            domain.allocation_epoch,
            new.allocation_epoch,
            owner,
            handle.graph_id,
        );
    }
    type LinkedDomainRow = (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>, String, Vec<u8>);
    let old: Option<LinkedDomainRow> = tx
        .query_row(
            "SELECT owner_id,graph_id,allocation_epoch,resources,manifest_json,obligation_id FROM obligations WHERE obligation_id=?1 AND phase!='settled'",
            [target.as_bytes()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
        )
        .optional()?;
    let Some((old_owner, old_graph, old_epoch, old_resources, old_manifest, old_id)) = old else {
        return Err(codec::invalid());
    };
    if old_owner != owner.as_bytes() || old_id != target.as_bytes() {
        return Err(codec::invalid());
    }
    let graph = Uuid::from_slice(&old_graph).map_err(|_| codec::invalid())?;
    let epoch = u64::from_le_bytes(
        old_epoch
            .as_slice()
            .try_into()
            .map_err(|_| codec::invalid())?,
    );
    let resources = codec::vector_from_bytes(&old_resources)?;
    let v2::ManifestDocument::V2(manifest) = v2::decode_manifest(&old_manifest)? else {
        return Err(codec::invalid());
    };
    validate_direct_domain_record(
        &manifest,
        resources,
        epoch,
        new.allocation_epoch,
        owner,
        graph,
    )
}

fn validate_direct_domain_record(
    manifest: &v2::Manifest,
    resources: ResourceVector,
    epoch: u64,
    source_epoch: u64,
    owner: Uuid,
    graph: Uuid,
) -> Result<(), CustodyError> {
    if epoch != source_epoch
        || manifest.owner_id.uuid() != owner
        || manifest.graph_id.uuid() != graph
        || resources.cpu_ns_per_second == 0
        || resources.handles != 0
        || resources.processes != 0
        || resources.hardware_slots != 0
        || resources.tuner_slots != 0
        || resources.scratch_bytes != 0
        || !matches!(manifest.members.as_slice(),
            [v2::MemberPlan::DirectReadDomainV1 { domain, .. }]
                if domain.owner_id.uuid() == owner && domain.graph_id.uuid() == graph
                    && domain.maximum_parallel_reads == 1)
    {
        return Err(codec::invalid());
    }
    Ok(())
}

pub(super) fn change(
    tx: &Transaction<'_>,
    owner: Uuid,
    key: &TransitionKey,
    mutation: &Mutation,
    prior: Phase,
    manifest: &v2::Manifest,
) -> Result<DiskResult, CustodyError> {
    if manifest.owner_id.uuid() != owner || manifest.graph_id.uuid() != key.handle.graph_id {
        return Err(codec::invalid());
    }
    let mut phase = prior;
    let mut settlement = None;
    let mut reason: Option<String> = tx.query_row(
        "SELECT hold_reason FROM obligations WHERE obligation_id=?1",
        [key.handle.obligation_id.as_bytes()],
        |r| r.get(0),
    )?;
    match mutation {
        Mutation::AssetChangeV2 { change } => {
            return super::scratch::change(tx, key, prior, manifest, change);
        }
        Mutation::AssetChangeV3 { change } => {
            return super::scratch::bootstrap::change(tx, key, prior, manifest, change);
        }
        Mutation::Arm { members } => {
            if !matches!(prior, Phase::Prepared | Phase::AcquisitionPossible)
                || members.is_empty()
                || members.len() > 129
                || members.iter().collect::<BTreeSet<_>>().len() != members.len()
            {
                return Err(codec::invalid());
            }
            for member in members {
                if !manifest.members.iter().any(|m|m.member_id()==*member)
                    || tx.execute("UPDATE members SET phase='acquisition_possible' WHERE obligation_id=?1 AND member_id=?2 AND phase='planned'",params![key.handle.obligation_id.as_bytes(),member.as_bytes()])? != 1 {
                    return Err(codec::invalid());
                }
            }
            phase = Phase::AcquisitionPossible;
        }
        Mutation::RecordTypedAcquired { envelope } => {
            if !matches!(
                prior,
                Phase::AcquisitionPossible | Phase::Sealed | Phase::UnknownHeld
            ) || envelope.observed.is_none()
            {
                return Err(codec::invalid());
            }
            v2::validate_envelope(manifest, envelope)?;
            // Growing scratch observations come only from the durable event
            // fold. The generic acquisition seam cannot inject an invented head.
            if matches!(
                envelope.planned,
                v2::MemberPlan::ScratchAssetsV2 { .. } | v2::MemberPlan::ScratchAssetsV3 { .. }
            ) {
                return Err(codec::invalid());
            }
            let (member_phase, json): (String, String) = tx.query_row(
                "SELECT phase,identity_json FROM members WHERE obligation_id=?1 AND member_id=?2",
                params![
                    key.handle.obligation_id.as_bytes(),
                    envelope.planned.member_id().as_bytes()
                ],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            if member_phase == "acquired" {
                // Each copy was already declared/armed before acquisition. A
                // later actual copy result can only ADD exact slots to the SAME
                // held file; it cannot replace identity or forget an old copy.
                // The planning source (parent -> parent+child) grows under the
                // same rules as an ordinary source; no other kind may grow.
                let old: v2::Envelope = codec::decode(&json)?;
                let (before, after) = match (&old.observed, &envelope.observed) {
                    (
                        Some(v2::Observed::SourceV1 { source: before }),
                        Some(v2::Observed::SourceV1 { source: after }),
                    )
                    | (
                        Some(v2::Observed::FixedProbeDiscoverSourceV1 { source: before }),
                        Some(v2::Observed::FixedProbeDiscoverSourceV1 { source: after }),
                    ) => (before, after),
                    _ => return Err(codec::invalid()),
                };
                let mut same = after.clone();
                same.copies = before.copies.clone();
                if old.planned != envelope.planned
                    || same != *before
                    || after.copies.len() <= before.copies.len()
                    || before.copies.iter().any(|c| !after.copies.contains(c))
                {
                    return Err(codec::invalid());
                }
            } else if member_phase != "acquisition_possible" {
                return Err(codec::invalid());
            }
            if tx.execute("UPDATE members SET phase='acquired',identity_json=?3 WHERE obligation_id=?1 AND member_id=?2 AND phase IN ('acquisition_possible','acquired')",params![key.handle.obligation_id.as_bytes(),envelope.planned.member_id().as_bytes(),codec::encode(envelope)?])? != 1 {
                return Err(codec::invalid());
            }
        }
        Mutation::Seal => {
            if matches!(prior, Phase::Sealed | Phase::UnknownHeld) {
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
            let acquired: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM members WHERE obligation_id=?1 AND phase!='planned')",
                [key.handle.obligation_id.as_bytes()],
                |r| r.get(0),
            )?;
            if prior != Phase::Prepared || acquired {
                return Err(codec::invalid());
            }
            settlement = Some(codec::encode(&v2::NeverArmed {
                version: 2,
                kind: v2::NeverArmedKind::PreparedNeverArmed,
                owner_id: manifest.owner_id,
                graph_id: manifest.graph_id,
                operation_id: v2::Id::new(key.operation_id)?,
            })?);
            phase = Phase::Settled;
            reason = None;
        }
        // A legacy all-closed receipt cannot retire typed assets or pretend a
        // transferred source/file was closed. No scalar/JSON-only refund seam.
        Mutation::RecordAcquired { .. }
        | Mutation::Settle(_)
        | Mutation::SettleTyped(_)
        | Mutation::RetireWithAssets(_) => return Err(codec::invalid()),
    }
    if let Some(record) = &settlement {
        tx.execute(
            "UPDATE members SET phase='closed',proof_json=?2 WHERE obligation_id=?1",
            params![key.handle.obligation_id.as_bytes(), record],
        )?;
    }
    let sequence = next_sequence(tx)?;
    tx.execute("UPDATE obligations SET phase=?2,sequence=?3,last_operation_id=?4,hold_reason=?5,settlement_json=?6 WHERE obligation_id=?1",params![key.handle.obligation_id.as_bytes(),phase.as_str(),sequence,key.operation_id.as_bytes(),reason,settlement])?;
    Ok(DiskResult::Changed(CommitAck {
        operation_id: key.operation_id,
        obligation_id: key.handle.obligation_id,
        graph_id: key.handle.graph_id,
        sequence,
        phase,
    }))
}

#[cfg(test)]
pub(in crate::resource_custody) mod tests;
