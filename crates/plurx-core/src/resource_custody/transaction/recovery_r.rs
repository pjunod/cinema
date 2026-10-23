//! Append-only custodians for imported fixed software R. The original owner
//! and typed producer history remain immutable.
use super::*;
use crate::resource_custody::codec_v2::{self as v2, scratch::AssetEventRefV1};

pub(super) fn load_overlay(
    connection: &Connection,
    schema_version: i64,
    obligations: &[ObligationRecord],
    owners: &BTreeSet<Uuid>,
    next: i64,
) -> Result<Vec<RecoveredRHolder>, CustodyError> {
    if schema_version < 5 {
        return Ok(Vec::new());
    }
    let mut statement = connection.prepare(
        "SELECT obligation_id,adoption_sequence,operation_id,previous_owner_id,current_owner_id,original_r_sequence,head_operation_id,head_revision,root_member_id,root_pid,root_creation_marker FROM recovered_r_holders ORDER BY obligation_id,adoption_sequence",
    )?;
    let mut rows = statement.query([])?;
    let mut holders = Vec::new();
    let mut effective = BTreeMap::<Uuid, (Uuid, i64)>::new();
    while let Some(row) = rows.next()? {
        if holders.len() >= codec::ACTIVE_LIMIT {
            return Err(codec::invalid());
        }
        let obligation_id = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(0)?)?;
        let adoption_sequence: i64 = row.get(1)?;
        let operation_id = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(2)?)?;
        let previous_owner_id = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(3)?)?;
        let current_owner_id = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(4)?)?;
        let original_r_sequence: i64 = row.get(5)?;
        let head = AssetEventRefV1 {
            operation_id: v2::Id::new(codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(6)?)?)?,
            revision: u64::try_from(row.get::<_, i64>(7)?).map_err(|_| codec::invalid())?,
        };
        let root_member_id = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(8)?)?;
        let root_pid = u32::try_from(row.get::<_, i64>(9)?).map_err(|_| codec::invalid())?;
        let root_creation_marker: String = row.get(10)?;
        let root_creation_marker = root_creation_marker
            .parse::<u64>()
            .map_err(|_| codec::invalid())?;
        let record = obligations
            .iter()
            .find(|r| r.obligation_id == obligation_id)
            .ok_or_else(codec::invalid)?;
        let (expected_owner, previous_sequence) = effective
            .get(&obligation_id)
            .copied()
            .unwrap_or((record.owner_id, 0));
        let ObligationInventory::TypedV2 { manifest, members } = &record.inventory else {
            return Err(codec::invalid());
        };
        let root = manifest
            .members
            .iter()
            .find_map(|member| match member {
                v2::MemberPlan::LegacyV1 { planned }
                    if planned.kind == MemberKind::Root && planned.member_id == root_member_id =>
                {
                    Some(planned)
                }
                _ => None,
            })
            .ok_or_else(codec::invalid)?;
        let root_record = members
            .iter()
            .find(|member| member.envelope.planned.member_id() == root_member_id)
            .ok_or_else(codec::invalid)?;
        let Some(v2::Observed::LegacyV1 { identity }) = &root_record.envelope.observed else {
            return Err(codec::invalid());
        };
        let process = identity.process.as_ref().ok_or_else(codec::invalid)?;
        let scratch_head = members
            .iter()
            .find_map(|member| match &member.envelope.observed {
                Some(v2::Observed::ScratchAssetsV3 { state }) => state.asset.head.as_ref(),
                _ => None,
            })
            .ok_or_else(codec::invalid)?;
        if !owners.contains(&previous_owner_id)
            || !owners.contains(&current_owner_id)
            || expected_owner != previous_owner_id
            || previous_owner_id == current_owner_id
            || adoption_sequence <= previous_sequence
            || adoption_sequence > next
            || original_r_sequence < 1
            || original_r_sequence > record.sequence
            || !matches!(
                record.phase,
                Phase::AcquisitionPossible | Phase::Sealed | Phase::UnknownHeld
            )
            || !matches!(
                &root.recovery,
                RecoveryMethod::TrustedFixedSoftwareSameBootV1 { .. }
            )
            || root_record.phase != "acquired"
            || process.pid != root_pid
            || process.creation_marker != Some(root_creation_marker)
            || head != *scratch_head
        {
            return Err(codec::invalid());
        }
        // No producer writer is admitted until its distinct overlay path
        // validates all later event heads. Thus the current historical R must
        // still equal the exact imported state at this stage.
        if original_r_sequence != record.sequence {
            return Err(codec::invalid());
        }
        let holder = RecoveredRHolder {
            obligation_id,
            adoption_sequence,
            operation_id,
            previous_owner_id,
            current_owner_id,
            original_r_sequence,
            head,
            root_member_id,
            root_pid,
            root_creation_marker,
        };
        let result_json: String = connection.query_row(
            "SELECT result_json FROM operations WHERE operation_id=?1",
            [operation_id.as_bytes()],
            |row| row.get(0),
        )?;
        let DiskResult::RecoveredRAdoption(ack) = codec::decode(&result_json)? else {
            return Err(codec::invalid());
        };
        if ack.operation_id != operation_id
            || ack.obligation_id != obligation_id
            || ack.previous_owner_id != previous_owner_id
            || ack.current_owner_id != current_owner_id
            || ack.sequence != adoption_sequence
        {
            return Err(codec::invalid());
        }
        effective.insert(obligation_id, (current_owner_id, adoption_sequence));
        holders.push(holder);
    }
    Ok(holders)
}

#[cfg(target_os = "linux")]
pub(super) fn adopt(
    tx: &Transaction<'_>,
    witness: &storage::Witness,
    owner: Uuid,
    operation_id: Uuid,
    proof: &crate::adaptive_resources::runtime::recovery::NativeActiveRImportProof,
) -> Result<DiskResult, CustodyError> {
    let snapshot = super::snapshot(tx, witness)?;
    if snapshot.recovered_r_holders.len() >= codec::ACTIVE_LIMIT {
        return Err(CustodyError::new(
            ErrorKind::Exhausted,
            "recovered R holder history bound reached",
        ));
    }
    let claim = proof.claim();
    let record = snapshot
        .obligations
        .iter()
        .find(|record| record.obligation_id == proof.obligation_id())
        .ok_or_else(codec::invalid)?;
    let effective = snapshot
        .recovered_r_holders
        .iter()
        .filter(|holder| holder.obligation_id == record.obligation_id)
        .max_by_key(|holder| holder.adoption_sequence)
        .map_or(record.owner_id, |holder| holder.current_owner_id);
    let current = snapshot
        .owners
        .iter()
        .find(|identity| identity.owner_id == owner)
        .ok_or_else(codec::invalid)?;
    if proof.journal_id() != witness.journal_id
        || proof.installation_id() != witness.installation_id
        || proof.current_owner_id() != owner
        || proof.previous_owner_id() != effective
        || proof.original_sequence() != record.sequence
        || claim.graph_id() != record.graph_id
        || claim.resources() != record.resources
        || !proof.root_exit().matches(
            &snapshot,
            current,
            record.obligation_id,
            record.graph_id,
            proof.root_exit().root_member_id(),
        )
    {
        return Err(codec::invalid());
    }
    let chosen = crate::resource_custody::recovery::select_active_finite_r(
        tx,
        &snapshot,
        proof.root_exit(),
        current,
    )?;
    if chosen.obligation_id() != claim.obligation_id()
        || chosen.old_owner_id() != claim.old_owner_id()
        || chosen.sequence() != claim.sequence()
        || chosen.state_head() != claim.state_head()
        || chosen.scratch_member_id() != claim.scratch_member_id()
    {
        return Err(codec::invalid());
    }
    let sequence = next_sequence(tx)?;
    tx.execute(
        "INSERT INTO recovered_r_holders VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![
            record.obligation_id.as_bytes(),
            sequence,
            operation_id.as_bytes(),
            effective.as_bytes(),
            owner.as_bytes(),
            record.sequence,
            proof.state_head().operation_id.uuid().as_bytes(),
            i64::try_from(proof.state_head().revision).map_err(|_| codec::invalid())?,
            proof.root_exit().root_member_id().as_bytes(),
            i64::from(proof.root_exit().pid()),
            proof.root_exit().creation_marker().to_string(),
        ],
    )?;
    Ok(DiskResult::RecoveredRAdoption(RecoveryAdoptionAck {
        operation_id,
        obligation_id: record.obligation_id,
        previous_owner_id: effective,
        current_owner_id: owner,
        sequence,
    }))
}

/// Producer cleanup writes require a separate proof carrying actual old-domain
/// extinction and current native ownership through the close operation. Until
/// that adapter is installed, an adopted R remains held and read-only to the
/// journal. In particular, an audit DTO cannot settle or release its charge.
#[cfg(target_os = "linux")]
pub(super) fn change(
    _tx: &Transaction<'_>,
    _witness: &storage::Witness,
    _owner: Uuid,
    _key: &TransitionKey,
    _mutation: &Mutation,
    _authority: &crate::adaptive_resources::runtime::recovery::RecoveredRAuthority,
) -> Result<DiskResult, CustodyError> {
    Err(CustodyError::new(
        ErrorKind::InvalidCommand,
        "recovered producer cleanup proof unavailable",
    ))
}
