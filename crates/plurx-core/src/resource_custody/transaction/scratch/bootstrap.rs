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
pub(in crate::resource_custody::transaction) fn restore(
    connection: &Connection,
    obligation: Uuid,
    graph: Uuid,
    member: v2::Id,
    plan: &ScratchPlanV3,
    expected: Option<&ScratchStateV3>,
) -> Result<BootstrapFold, CustodyError> {
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
        } => !matches!(operation, AssetIoV1::CloseReader { .. }),
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
