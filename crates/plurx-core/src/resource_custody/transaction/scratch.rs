//! Bounded growing-output events in the existing journal/operations rows.
//! SQL history is audit authority; it never manufactures native proof.
use super::*;
use crate::resource_custody::codec_v2::{self as v2, scratch::*};
pub(super) mod bootstrap;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AssetChangedV2 {
    pub version: u32,
    pub ack: CommitAck,
    pub change: AssetChangeV2,
    pub after: ScratchStateV2,
}
impl AssetChangedV2 {
    pub(super) fn validate(&self) -> Result<(), CustodyError> {
        if self.version != 2
            || self.after.version != 2
            || self.after.asset_id != self.change.asset_id
            || self.ack.sequence < 1
            || self.after.head.as_ref()
                != Some(&AssetEventRefV1 {
                    operation_id: v2::Id::new(self.ack.operation_id)?,
                    revision: self.after.revision,
                })
            || self.after.revision
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

pub(super) fn validate_record_binding(
    connection: &Connection,
    record: &AssetChangedV2,
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
    let v2::MemberPlan::ScratchAssetsV2 { slot, plan } = &envelope.planned else {
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

/// Streaming rowid order avoids an unbounded chain stack/JSON movie inventory
/// and SQLite temporary-file sorting. Rows are append-only; refs/revisions and
/// global sequence are checked, so insertion order is not accepted as proof.
/// Current cost is O(journal history) per fold, plus bounded live-file lookups.
/// This first path does not accept a cross-obligation transfer lineage yet.
pub(super) fn restore(
    connection: &Connection,
    obligation: Uuid,
    graph: Uuid,
    member: v2::Id,
    plan: &ScratchPlanV2,
    expected: Option<&ScratchStateV2>,
) -> Result<ScratchFold, CustodyError> {
    let mut fold = ScratchFold::empty(plan)?;
    let mut prior_sequence = 0;
    let mut statement =
        connection.prepare("SELECT operation_id,result_json FROM operations ORDER BY rowid")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let result: DiskResult = codec::decode(&row.get::<_, String>(1)?)?;
        let record = match result {
            DiskResult::AssetChangedV2(record) => record,
            DiskResult::AssetChangedV3(record) if record.change.asset_id == plan.asset_id => {
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
            // The same asset cannot grow through another owner/member. The R->S
            // extension must explicitly validate that boundary, never ignore it.
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
        if fold.state != record.after {
            return Err(codec::invalid());
        }
    }
    if expected.is_some_and(|state| *state != fold.state)
        || (expected.is_none() && fold.state.revision != 0)
    {
        return Err(codec::invalid());
    }
    Ok(fold)
}

pub(super) fn change(
    tx: &Transaction<'_>,
    key: &TransitionKey,
    prior: Phase,
    manifest: &v2::Manifest,
    change: &AssetChangeV2,
) -> Result<DiskResult, CustodyError> {
    if !matches!(
        prior,
        Phase::AcquisitionPossible | Phase::Sealed | Phase::UnknownHeld
    ) || (prior == Phase::UnknownHeld
        && matches!(&change.event,
        AssetEventV1::ArmIo { operation, .. } if !matches!(operation, AssetIoV1::CloseReader { .. })))
    {
        return Err(codec::invalid());
    }
    codec::encode(change)?;
    let member = manifest
        .members
        .iter()
        .find(|m| m.member_id() == change.scratch_member_id.uuid())
        .ok_or_else(codec::invalid)?;
    let v2::MemberPlan::ScratchAssetsV2 { plan, .. } = member else {
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
        Some(v2::Observed::ScratchAssetsV2 { state }) => Some(state),
        None => None,
        _ => return Err(codec::invalid()),
    };
    let before = restore(
        tx,
        key.handle.obligation_id,
        key.handle.graph_id,
        change.scratch_member_id,
        plan,
        expected,
    )?;
    let after = before
        .apply(plan, v2::Id::new(key.operation_id)?, change)?
        .state;
    envelope.observed = Some(v2::Observed::ScratchAssetsV2 {
        state: after.clone(),
    });
    v2::validate_envelope(manifest, &envelope)?;
    let sequence = next_sequence(tx)?;
    let ack = CommitAck {
        operation_id: key.operation_id,
        obligation_id: key.handle.obligation_id,
        graph_id: key.handle.graph_id,
        sequence,
        phase: prior,
    };
    let record = AssetChangedV2 {
        version: 2,
        ack,
        change: change.clone(),
        after,
    };
    record.validate()?;
    // Result row is inserted by execute() before the SAME transaction commits.
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
    Ok(DiskResult::AssetChangedV2(record))
}
