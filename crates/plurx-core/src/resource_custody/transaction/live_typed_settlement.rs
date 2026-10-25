//! Settle one live multi-member typed V2 obligation (planning P, finite
//! cancellation, copy close). Every planned member must carry a closed fact
//! that `validate_settlement` accepts against the durable envelopes in this
//! SQLite transaction. A single retained scratch member keeps its dedicated
//! branch (`retained_settlement`), which owns the retained-owner lineage row.
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
        || manifest.members.is_empty()
        || manifest.members.len() != proof.settlement.members.len()
    {
        return Err(codec::invalid());
    }
    let mut envelopes = Vec::with_capacity(manifest.members.len());
    for (planned, disposition) in manifest.members.iter().zip(&proof.settlement.members) {
        let v2::MemberDisposition::Closed { member_id, fact } = disposition else {
            return Err(codec::invalid());
        };
        if member_id.uuid() != planned.member_id() {
            return Err(codec::invalid());
        }
        let (phase, identity_json): (String, String) = tx.query_row(
            "SELECT phase,identity_json FROM members WHERE obligation_id=?1 AND member_id=?2",
            params![
                key.handle.obligation_id.as_bytes(),
                member_id.uuid().as_bytes()
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if !matches!(
            phase.as_str(),
            "planned" | "acquisition_possible" | "acquired"
        ) {
            return Err(codec::invalid());
        }
        let envelope: v2::Envelope = codec::decode(&identity_json)?;
        v2::validate_envelope(manifest, &envelope)?;
        if envelope.planned != *planned {
            return Err(codec::invalid());
        }
        // A scratch head in the request is data only. Rebuild the durable
        // event fold and require the same terminal, empty state.
        if let (
            v2::MemberPlan::ScratchAssetsV3 { slot, plan },
            v2::ClosedFact::ScratchAssetsV3 {
                plan: closed_plan,
                exact_head,
                operation_id,
            },
        ) = (planned, fact)
        {
            let Some(v2::Observed::ScratchAssetsV3 { state }) = &envelope.observed else {
                return Err(codec::invalid());
            };
            if phase != "acquired"
                || closed_plan != plan
                || exact_head.operation_id != *operation_id
                || state.asset.head.as_ref() != Some(exact_head)
            {
                return Err(codec::invalid());
            }
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
        }
        envelopes.push(envelope);
    }
    v2::validate_settlement(manifest, &envelopes, &proof.settlement)?;

    let settlement_json = codec::encode(&proof.settlement)?;
    let sequence = next_sequence(tx)?;
    if tx.execute(
        "UPDATE obligations SET phase='settled',sequence=?2,last_operation_id=?3,hold_reason=NULL,settlement_json=?4 WHERE obligation_id=?1 AND sequence=?5",
        params![
            key.handle.obligation_id.as_bytes(),
            sequence,
            key.operation_id.as_bytes(),
            settlement_json,
            key.expected_sequence
        ],
    )? != 1
    {
        return Err(codec::invalid());
    }
    if tx.execute(
        "UPDATE members SET phase='closed',proof_json=?2 WHERE obligation_id=?1 AND phase IN ('planned','acquisition_possible','acquired')",
        params![key.handle.obligation_id.as_bytes(), settlement_json],
    )? != manifest.members.len()
    {
        return Err(codec::invalid());
    }
    Ok(DiskResult::Changed(CommitAck {
        operation_id: key.operation_id,
        obligation_id: key.handle.obligation_id,
        graph_id: key.handle.graph_id,
        sequence,
        phase: Phase::Settled,
    }))
}
