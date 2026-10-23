//! Additive V3 bootstrap/output lineage in the SAME operations table/worker.
//! Known audit results do not manufacture retained native handles or receipts.
use super::*;
use crate::resource_custody::codec_v2::scratch::bootstrap::validate_projection;
use crate::resource_custody::codec_v2::scratch::bootstrap::*;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::resource_custody::transaction) struct AssetChangedV3 {
    pub version: u32,
    pub ack: CommitAck,
    pub change: AssetChangeV3,
    pub after: ScratchStateV3,
}
impl AssetChangedV3 {
    pub(in crate::resource_custody::transaction) fn validate(&self) -> Result<(), CustodyError> {
        if self.version != 3
            || self.after.version != 3
            || self.after.asset.version != 2
            || self.after.asset.asset_id != self.change.asset_id
            || self.ack.sequence < 1
            || self.after.asset.head.as_ref()
                != Some(&AssetEventRefV1 {
                    operation_id: v2::Id::new(self.ack.operation_id)?,
                    revision: self.after.asset.revision,
                })
            || self.after.asset.revision
                != self
                    .change
                    .previous
                    .as_ref()
                    .map_or(Some(1), |h| h.revision.checked_add(1))
                    .ok_or_else(codec::invalid)?
            || matches!(self.ack.phase, Phase::Prepared | Phase::Settled)
        {
            return Err(codec::invalid());
        }
        Ok(())
    }
}

pub(in crate::resource_custody::transaction) fn validate_record_binding(
    connection: &Connection,
    record: &AssetChangedV3,
) -> Result<(), CustodyError> {
    record.validate()?;
    let json: String = connection.query_row(
        "SELECT identity_json FROM members WHERE obligation_id=?1 AND member_id=?2",
        params![
            record.ack.obligation_id.as_bytes(),
            record.change.scratch_member_id.uuid().as_bytes()
        ],
        |r| r.get(0),
    )?;
    let envelope: v2::Envelope = codec::decode(&json)?;
    let v2::MemberPlan::ScratchAssetsV3 { slot, plan } = &envelope.planned else {
        return Err(codec::invalid());
    };
    if slot.member_id != record.change.scratch_member_id
        || plan.asset_id != record.change.asset_id
        || plan.generation_epoch != record.change.generation_epoch
    {
        return Err(codec::invalid());
    }
    validate_projection(plan, &record.after)
}

/// Stream the complete append-only history without an unbounded chain stack.
/// Bootstrap and output events share ONE head; a different member/version cannot
/// silently continue this asset. Full state equality includes possible/unknown
/// copies and partial child effects, not just the compact output revision.
pub(in crate::resource_custody) fn restore(
    connection: &Connection,
    obligation: Uuid,
    graph: Uuid,
    member: v2::Id,
    plan: &ScratchPlanV3,
    expected: Option<&ScratchStateV3>,
) -> Result<BootstrapFold, CustodyError> {
    let retained_json: Option<String> = connection
        .query_row(
            "SELECT custody_json FROM retained_asset_owners WHERE asset_id=?1",
            [plan.asset_id.uuid().as_bytes()],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(json) = retained_json {
        let v2::RetainedAssetsDocument::V2(custody) = v2::decode_retained_assets(&json)? else {
            return Err(codec::invalid());
        };
        return restore_transferred(
            connection, obligation, graph, member, plan, expected, &custody,
        );
    }
    let mut fold = BootstrapFold::empty(plan, v2::Id::new(obligation)?, member)?;
    let mut prior_sequence = 0;
    let mut statement =
        connection.prepare("SELECT operation_id,result_json FROM operations ORDER BY rowid")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let result: DiskResult = codec::decode(&row.get::<_, String>(1)?)?;
        let record = match result {
            DiskResult::AssetChangedV3(record) => record,
            DiskResult::AssetChangedV2(record) if record.change.asset_id == plan.asset_id => {
                return Err(codec::invalid());
            }
            _ => continue,
        };
        record.validate()?;
        let operation = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(0)?)?;
        if record.ack.operation_id != operation {
            return Err(codec::invalid());
        }
        if record.ack.obligation_id != obligation || record.change.scratch_member_id != member {
            if record.change.asset_id == plan.asset_id {
                return Err(codec::invalid());
            }
            continue;
        }
        if record.ack.graph_id != graph || record.ack.sequence <= prior_sequence {
            return Err(codec::invalid());
        }
        prior_sequence = record.ack.sequence;
        fold = fold.apply(plan, v2::Id::new(operation)?, &record.change)?;
        if fold.state() != record.after {
            return Err(codec::invalid());
        }
    }
    let state = fold.state();
    if expected.is_some_and(|expected| *expected != state)
        || (expected.is_none() && state.asset.revision != 0)
    {
        return Err(codec::invalid());
    }
    Ok(fold)
}

pub(in crate::resource_custody::transaction) fn change(
    tx: &Transaction<'_>,
    key: &TransitionKey,
    prior: Phase,
    manifest: &v2::Manifest,
    change: &AssetChangeV3,
) -> Result<DiskResult, CustodyError> {
    // UnknownHeld permits reconciliation/cleanup, never a fresh acquisition.
    let acquisition = match &change.event {
        AssetEventV3::ArmBootstrapV1 { operation, .. } => matches!(
            operation,
            BootstrapIoV1::OpenNamespaceRoot { .. }
                | BootstrapIoV1::OpenNextComponent { .. }
                | BootstrapIoV1::CreateRenditionChild { .. }
        ),
        AssetEventV3::OutputV2 {
            event: AssetEventV1::ArmIo { operation, .. },
        } => !matches!(
            operation,
            AssetIoV1::CloseReader { .. }
                | AssetIoV1::RemoveOwned { .. }
                | AssetIoV1::SyncFiles { .. }
        ),
        _ => false,
    };
    if !matches!(
        prior,
        Phase::AcquisitionPossible | Phase::Sealed | Phase::UnknownHeld
    ) || (prior == Phase::UnknownHeld && acquisition)
    {
        return Err(codec::invalid());
    }
    codec::encode(change)?;
    let member = manifest
        .members
        .iter()
        .find(|m| m.member_id() == change.scratch_member_id.uuid())
        .ok_or_else(codec::invalid)?;
    let v2::MemberPlan::ScratchAssetsV3 { plan, .. } = member else {
        return Err(codec::invalid());
    };
    let (phase, json): (String, String) = tx.query_row(
        "SELECT phase,identity_json FROM members WHERE obligation_id=?1 AND member_id=?2",
        params![
            key.handle.obligation_id.as_bytes(),
            change.scratch_member_id.uuid().as_bytes()
        ],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let mut envelope: v2::Envelope = codec::decode(&json)?;
    v2::validate_envelope(manifest, &envelope)?;
    if envelope.planned != *member || !matches!(phase.as_str(), "acquisition_possible" | "acquired")
    {
        return Err(codec::invalid());
    }
    let expected = match &envelope.observed {
        Some(v2::Observed::ScratchAssetsV3 { state }) => Some(state),
        None => None,
        _ => return Err(codec::invalid()),
    };
    let after = restore(
        tx,
        key.handle.obligation_id,
        key.handle.graph_id,
        change.scratch_member_id,
        plan,
        expected,
    )?
    .apply(plan, v2::Id::new(key.operation_id)?, change)?
    .state();
    let retained_json: Option<String> = tx
        .query_row(
            "SELECT custody_json FROM retained_asset_owners WHERE asset_id=?1",
            [plan.asset_id.uuid().as_bytes()],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(json) = retained_json {
        let v2::RetainedAssetsDocument::V2(custody) = v2::decode_retained_assets(&json)? else {
            return Err(codec::invalid());
        };
        if custody.target_obligation_id.uuid() != key.handle.obligation_id
            || custody.target_graph_id.uuid() != key.handle.graph_id
            || custody.transfer.target_member_id != change.scratch_member_id
        {
            return Err(codec::invalid());
        }
        let resources: Vec<u8> = tx.query_row(
            "SELECT resources FROM obligations WHERE obligation_id=?1",
            [key.handle.obligation_id.as_bytes()],
            |row| row.get(0),
        )?;
        super::super::assets::validate_v2_current_totals(
            &after,
            codec::vector_from_bytes(&resources)?,
        )?;
    }
    envelope.observed = Some(v2::Observed::ScratchAssetsV3 {
        state: after.clone(),
    });
    v2::validate_envelope(manifest, &envelope)?;
    let sequence = next_sequence(tx)?;
    let record = AssetChangedV3 {
        version: 3,
        ack: CommitAck {
            operation_id: key.operation_id,
            obligation_id: key.handle.obligation_id,
            graph_id: key.handle.graph_id,
            sequence,
            phase: prior,
        },
        change: change.clone(),
        after,
    };
    record.validate()?;
    // execute() inserts the exact result before committing this SAME transaction.
    tx.execute("UPDATE members SET phase='acquired',identity_json=?3 WHERE obligation_id=?1 AND member_id=?2",
        params![key.handle.obligation_id.as_bytes(), change.scratch_member_id.uuid().as_bytes(), codec::encode(&envelope)?])?;
    tx.execute(
        "UPDATE obligations SET sequence=?2,last_operation_id=?3 WHERE obligation_id=?1",
        params![
            key.handle.obligation_id.as_bytes(),
            sequence,
            key.operation_id.as_bytes()
        ],
    )?;
    Ok(DiskResult::AssetChangedV3(record))
}

fn transfer_ack_matches(
    ack: &RetirementAck,
    operation: Uuid,
    custody: &v2::RetainedAssetsV2,
) -> bool {
    ack.version == 2
        && ack.operation_id.uuid() == operation
        && ack.operation_id == custody.transfer_operation_id
        && ack.source_obligation_id == custody.producer_obligation_id
        && ack.source_graph_id == custody.producer_graph_id
        && ack.target_obligation_id == custody.target_obligation_id
        && ack.target_graph_id == custody.target_graph_id
        && ack.asset_id == custody.asset_id
}

/// Verify the immutable producer row before deriving any target seed. Only the
/// original journal fold, never the serialized compact ref, supplies files.
fn validate_transfer_source(
    connection: &Connection,
    custody: &v2::RetainedAssetsV2,
    plan: &ScratchPlanV3,
) -> Result<(), CustodyError> {
    let v2::TransferIdentity::ScratchAssetsV3 {
        plan: transferred,
        snapshot,
    } = &custody.transfer.identity
    else {
        return Err(codec::invalid());
    };
    if transferred != plan
        || snapshot.member_id != custody.transfer.source_member_id
        || snapshot.obligation_id != custody.producer_obligation_id
        || custody.transfer.source_member_id == custody.transfer.target_member_id
        || custody.producer_obligation_id == custody.target_obligation_id
    {
        return Err(codec::invalid());
    }
    let chained: i64 = connection.query_row(
        "SELECT COUNT(*) FROM retained_asset_owners WHERE obligation_id=?1",
        [custody.producer_obligation_id.uuid().as_bytes()],
        |r| r.get(0),
    )?;
    if chained != 0 {
        return Err(codec::invalid());
    }
    let (phase, graph, manifest_json, settlement_json): (String, Vec<u8>, String, String) =
        connection.query_row(
            "SELECT phase,graph_id,manifest_json,settlement_json FROM obligations WHERE obligation_id=?1",
            [custody.producer_obligation_id.uuid().as_bytes()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
    let v2::ManifestDocument::V2(manifest) = v2::decode_manifest(&manifest_json)? else {
        return Err(codec::invalid());
    };
    if phase != "settled"
        || codec::uuid_from_bytes(&graph)? != custody.producer_graph_id.uuid()
        || manifest.graph_id != custody.producer_graph_id
    {
        return Err(codec::invalid());
    }
    let identity_json: String = connection.query_row(
        "SELECT identity_json FROM members WHERE obligation_id=?1 AND member_id=?2",
        params![
            custody.producer_obligation_id.uuid().as_bytes(),
            custody.transfer.source_member_id.uuid().as_bytes()
        ],
        |r| r.get(0),
    )?;
    let envelope: v2::Envelope = codec::decode(&identity_json)?;
    v2::validate_envelope(&manifest, &envelope)?;
    if !matches!((&envelope.planned, &envelope.observed),
        (v2::MemberPlan::ScratchAssetsV3 { slot, plan: original }, Some(v2::Observed::ScratchAssetsV3 { state }))
            if slot.member_id == custody.transfer.source_member_id && original == plan && state == &snapshot.state)
    {
        return Err(codec::invalid());
    }
    let settlement: v2::Settlement = codec::decode(&settlement_json)?;
    if settlement.operation_id != custody.transfer_operation_id
        || settlement.graph_id != custody.producer_graph_id
        || settlement.members.iter().filter(|d| matches!(d,
            v2::MemberDisposition::Transferred { .. })).count() != 1
        || settlement.members.iter().filter(|d| matches!(d,
            v2::MemberDisposition::Transferred { transfer, target_obligation_id, target_graph_id, asset_id, operation_id }
                if transfer == &custody.transfer
                    && *target_obligation_id == custody.target_obligation_id
                    && *target_graph_id == custody.target_graph_id
                    && *asset_id == custody.asset_id
                    && *operation_id == custody.transfer_operation_id)).count() != 1
    {
        return Err(codec::invalid());
    }
    Ok(())
}

fn replay_producer_until_transfer(
    connection: &Connection,
    custody: &v2::RetainedAssetsV2,
    plan: &ScratchPlanV3,
) -> Result<BootstrapFold, CustodyError> {
    validate_transfer_source(connection, custody, plan)?;
    let mut fold = BootstrapFold::empty(
        plan,
        custody.producer_obligation_id,
        custody.transfer.source_member_id,
    )?;
    let mut boundary = false;
    let mut prior_sequence = 0;
    let mut stmt =
        connection.prepare("SELECT operation_id,result_json FROM operations ORDER BY rowid")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let operation = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(0)?)?;
        let result: DiskResult = codec::decode(&row.get::<_, String>(1)?)?;
        match result {
            DiskResult::AssetChangedV3(record) if record.change.asset_id == plan.asset_id => {
                record.validate()?;
                if record.ack.operation_id != operation {
                    return Err(codec::invalid());
                }
                if !boundary {
                    if record.ack.obligation_id != custody.producer_obligation_id.uuid()
                        || record.ack.graph_id != custody.producer_graph_id.uuid()
                        || record.change.scratch_member_id != custody.transfer.source_member_id
                        || record.ack.sequence <= prior_sequence
                    {
                        return Err(codec::invalid());
                    }
                    prior_sequence = record.ack.sequence;
                    fold = fold.apply(plan, v2::Id::new(operation)?, &record.change)?;
                    if fold.state() != record.after {
                        return Err(codec::invalid());
                    }
                } else if record.ack.obligation_id != custody.target_obligation_id.uuid()
                    || record.ack.graph_id != custody.target_graph_id.uuid()
                    || record.change.scratch_member_id != custody.transfer.target_member_id
                {
                    return Err(codec::invalid());
                }
            }
            DiskResult::AssetChangedV2(record) if record.change.asset_id == plan.asset_id => {
                return Err(codec::invalid());
            }
            DiskResult::RetiredWithAssets(ack) if ack.asset_id == plan.asset_id => {
                if boundary
                    || !transfer_ack_matches(&ack, operation, custody)
                    || ack.sequence <= prior_sequence
                {
                    return Err(codec::invalid());
                }
                boundary = true;
            }
            _ => {}
        }
    }
    let v2::TransferIdentity::ScratchAssetsV3 { snapshot, .. } = &custody.transfer.identity else {
        return Err(codec::invalid());
    };
    if !boundary || fold.state() != snapshot.state {
        return Err(codec::invalid());
    }
    let full = fold.frozen_snapshot(plan)?;
    validate_frozen_snapshot_ref(plan, snapshot, &full)?;
    Ok(fold)
}

fn restore_transferred(
    connection: &Connection,
    obligation: Uuid,
    graph: Uuid,
    member: v2::Id,
    plan: &ScratchPlanV3,
    expected: Option<&ScratchStateV3>,
    custody: &v2::RetainedAssetsV2,
) -> Result<BootstrapFold, CustodyError> {
    let producer = obligation == custody.producer_obligation_id.uuid()
        && graph == custody.producer_graph_id.uuid()
        && member == custody.transfer.source_member_id;
    let target = obligation == custody.target_obligation_id.uuid()
        && graph == custody.target_graph_id.uuid()
        && member == custody.transfer.target_member_id;
    if producer == target || plan.asset_id != custody.asset_id {
        return Err(codec::invalid());
    }
    let original = replay_producer_until_transfer(connection, custody, plan)?;
    if producer {
        if expected != Some(&original.state()) {
            return Err(codec::invalid());
        }
        return Ok(original);
    }
    let v2::TransferIdentity::ScratchAssetsV3 { snapshot, .. } = &custody.transfer.identity else {
        return Err(codec::invalid());
    };
    let mut fold = original.adopt_frozen(
        plan,
        snapshot,
        custody.target_obligation_id,
        custody.transfer.target_member_id,
    )?;
    let mut boundary = false;
    let mut prior_sequence = 0;
    let mut stmt =
        connection.prepare("SELECT operation_id,result_json FROM operations ORDER BY rowid")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let operation = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(0)?)?;
        let result: DiskResult = codec::decode(&row.get::<_, String>(1)?)?;
        match result {
            DiskResult::AssetChangedV3(record) if record.change.asset_id == plan.asset_id => {
                record.validate()?;
                if record.ack.operation_id != operation {
                    return Err(codec::invalid());
                }
                if !boundary {
                    if record.ack.obligation_id != custody.producer_obligation_id.uuid()
                        || record.ack.graph_id != custody.producer_graph_id.uuid()
                        || record.change.scratch_member_id != custody.transfer.source_member_id
                    {
                        return Err(codec::invalid());
                    }
                } else {
                    if record.ack.obligation_id != obligation
                        || record.ack.graph_id != graph
                        || record.change.scratch_member_id != member
                        || record.ack.sequence <= prior_sequence
                    {
                        return Err(codec::invalid());
                    }
                    prior_sequence = record.ack.sequence;
                    fold = fold.apply(plan, v2::Id::new(operation)?, &record.change)?;
                    if fold.state() != record.after {
                        return Err(codec::invalid());
                    }
                }
            }
            DiskResult::AssetChangedV2(record) if record.change.asset_id == plan.asset_id => {
                return Err(codec::invalid());
            }
            DiskResult::RetiredWithAssets(ack) if ack.asset_id == plan.asset_id => {
                if boundary || !transfer_ack_matches(&ack, operation, custody) {
                    return Err(codec::invalid());
                }
                prior_sequence = ack.sequence;
                boundary = true;
            }
            _ => {}
        }
    }
    let state = fold.state();
    if !boundary || expected != Some(&state) {
        return Err(codec::invalid());
    }
    Ok(fold)
}
