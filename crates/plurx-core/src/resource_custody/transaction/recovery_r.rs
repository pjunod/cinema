//! Append-only custodians for imported fixed software R. The original owner
//! and typed producer history remain immutable.
use super::*;
use crate::resource_custody::codec_v2::{self as v2, scratch::AssetEventRefV1};

struct OverlayRecord {
    owner_id: Uuid,
    phase: Phase,
    sequence: i64,
    inventory: ObligationInventory,
}

fn next_holder_matches_previous(
    previous: Option<&RecoveredRHolder>,
    original_owner: Uuid,
    next: &RecoveredRHolder,
) -> bool {
    match previous {
        None => next.previous_owner_id == original_owner,
        Some(previous) => {
            next.obligation_id == previous.obligation_id
                && next.previous_owner_id == previous.current_owner_id
                && next.adoption_sequence > previous.adoption_sequence
                && next.original_r_sequence == previous.original_r_sequence
                && next.head == previous.head
                && next.root_member_id == previous.root_member_id
                && next.root_pid == previous.root_pid
                && next.root_creation_marker == previous.root_creation_marker
        }
    }
}

fn predecessor_unchanged_since_adoption(
    previous: &RecoveredRHolder,
    last_data_sequence: i64,
    last_operation_sequence: i64,
) -> bool {
    last_data_sequence == previous.original_r_sequence
        && last_operation_sequence == previous.adoption_sequence
}

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
    let mut effective = BTreeMap::<Uuid, RecoveredRHolder>::new();
    let mut recovered = BTreeMap::<Uuid, OverlayRecord>::new();
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
        if !recovered.contains_key(&obligation_id) {
            let record = if let Some(active) = obligations
                .iter()
                .find(|r| r.obligation_id == obligation_id)
            {
                OverlayRecord {
                    owner_id: active.owner_id,
                    phase: active.phase,
                    sequence: active.sequence,
                    inventory: active.inventory.clone(),
                }
            } else {
                let (owner, graph, phase, sequence, manifest_json, settlement_json):
                    (Vec<u8>, Vec<u8>, String, i64, String, Option<String>) = connection.query_row(
                        "SELECT owner_id,graph_id,phase,sequence,manifest_json,settlement_json FROM obligations WHERE obligation_id=?1",
                        [obligation_id.as_bytes()],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
                    )?;
                let owner = codec::uuid_from_bytes(&owner)?;
                let graph = codec::uuid_from_bytes(&graph)?;
                let phase = Phase::parse(&phase)?;
                if phase != Phase::Settled || settlement_json.is_none() {
                    return Err(codec::invalid());
                }
                OverlayRecord {
                    owner_id: owner,
                    phase,
                    sequence,
                    inventory: inventory::load(
                        connection,
                        obligation_id,
                        owner,
                        graph,
                        phase,
                        &manifest_json,
                        settlement_json.as_deref(),
                    )?,
                }
            };
            recovered.insert(obligation_id, record);
        }
        let record = recovered.get(&obligation_id).ok_or_else(codec::invalid)?;
        let previous = effective.get(&obligation_id);
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
        if !owners.contains(&previous_owner_id)
            || !owners.contains(&current_owner_id)
            || !next_holder_matches_previous(previous, record.owner_id, &holder)
            || previous_owner_id == current_owner_id
            || adoption_sequence <= original_r_sequence
            || adoption_sequence > next
            || original_r_sequence < 1
            || original_r_sequence > record.sequence
            || !matches!(
                record.phase,
                Phase::AcquisitionPossible | Phase::Sealed | Phase::UnknownHeld | Phase::Settled
            )
            || !matches!(
                &root.recovery,
                RecoveryMethod::TrustedFixedSoftwareSameBootV1 { .. }
            )
            || (record.phase != Phase::Settled && root_record.phase != "acquired")
            || process.pid != root_pid
            || process.creation_marker != Some(root_creation_marker)
        {
            return Err(codec::invalid());
        }
        let _current_head = scratch_head;
        if record.phase == Phase::Settled {
            validate_settled_holder(connection, &holder, record)?;
        }
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
        effective.insert(obligation_id, holder.clone());
        holders.push(holder);
    }
    validate_overlay_history(connection, &holders, &recovered, next)?;
    Ok(holders)
}

fn validate_settled_holder(
    connection: &Connection,
    holder: &RecoveredRHolder,
    record: &OverlayRecord,
) -> Result<(), CustodyError> {
    let settlement_json: String = connection.query_row(
        "SELECT settlement_json FROM obligations WHERE obligation_id=?1",
        [holder.obligation_id.as_bytes()],
        |row| row.get(0),
    )?;
    let v2::SettlementDocument::Physical(settlement) = v2::decode_settlement(&settlement_json)?
    else {
        return Err(codec::invalid());
    };
    let ObligationInventory::TypedV2 { manifest, .. } = &record.inventory else {
        return Err(codec::invalid());
    };
    validate_settled_dispositions(manifest, &settlement, holder)
}

fn validate_settled_dispositions(
    manifest: &v2::Manifest,
    settlement: &v2::Settlement,
    holder: &RecoveredRHolder,
) -> Result<(), CustodyError> {
    if settlement.owner_id != manifest.owner_id || settlement.graph_id != manifest.graph_id {
        return Err(codec::invalid());
    }
    if settlement.members.len() != manifest.members.len() {
        return Err(codec::invalid());
    }
    let mut roots = 0;
    let mut tasks = 0;
    let mut scratch = 0;
    for (plan, disposition) in manifest.members.iter().zip(&settlement.members) {
        let v2::MemberDisposition::Closed { member_id, fact } = disposition else {
            return Err(codec::invalid());
        };
        if member_id.uuid() != plan.member_id() {
            return Err(codec::invalid());
        }
        match (plan, fact) {
            (v2::MemberPlan::SourceV1 { source, .. }, v2::ClosedFact::SourceV1 { holder_id, copies })
                if holder_id == &source.holder_id
                    && copies.len() == source.planned_copies.len()
                    && copies.iter().all(|copy| matches!(copy,
                        v2::CopyClosed::DomainExtinct { domain, .. } if domain == &source.filesystem_domain)) => {}
            (v2::MemberPlan::LegacyV1 { planned }, v2::ClosedFact::LegacyV1 { fact })
                if planned.member_id == holder.root_member_id
                    && fact.object_identity == format!("{}:{}", holder.root_pid, holder.root_creation_marker) => {
                roots += 1;
            }
            (v2::MemberPlan::VodOutputTaskV1 { task, .. }, v2::ClosedFact::VodOutputTaskDomainExtinctV1 {
                task: closed, root_member_id, root_pid, root_creation_marker, ..
            }) if closed == task
                && root_member_id.uuid() == holder.root_member_id
                && *root_pid == holder.root_pid
                && *root_creation_marker == holder.root_creation_marker => {
                tasks += 1;
            }
            (v2::MemberPlan::ScratchAssetsV3 { plan, .. }, v2::ClosedFact::ScratchAssetsV3 { plan: closed, .. })
                if plan == closed => {
                scratch += 1;
            }
            _ => return Err(codec::invalid()),
        }
    }
    if roots != 1 || tasks != 1 || scratch != 1 {
        return Err(codec::invalid());
    }
    Ok(())
}

/// Replay the post-adoption producer fold from its exact imported head. The
/// adoption itself does not rewrite the original R row or release its charge.
fn validate_overlay_history(
    connection: &Connection,
    holders: &[RecoveredRHolder],
    obligations: &BTreeMap<Uuid, OverlayRecord>,
    next: i64,
) -> Result<(), CustodyError> {
    if holders.is_empty() {
        return Ok(());
    }
    let mut chains = BTreeMap::<Uuid, Vec<&RecoveredRHolder>>::new();
    for holder in holders {
        let chain = chains.entry(holder.obligation_id).or_default();
        chain.try_reserve(1).map_err(|_| codec::invalid())?;
        chain.push(holder);
    }
    let mut tracked = BTreeMap::<Uuid, (i64, i64, AssetEventRefV1, usize, bool)>::new();
    for (id, chain) in &chains {
        let first = chain.first().ok_or_else(codec::invalid)?;
        tracked.insert(
            *id,
            (first.original_r_sequence, 0, first.head.clone(), 0, false),
        );
    }
    let mut sequence = 0_i64;
    let mut rows =
        connection.prepare("SELECT operation_id,result_json FROM operations ORDER BY rowid")?;
    let mut query = rows.query([])?;
    while let Some(row) = query.next()? {
        let operation_id = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(0)?)?;
        let result: DiskResult = codec::decode(&row.get::<_, String>(1)?)?;
        let committed = match &result {
            DiskResult::Prepared { sequence, .. } => *sequence,
            DiskResult::Changed(ack) => ack.sequence,
            DiskResult::AssetChangedV2(record) => record.ack.sequence,
            DiskResult::AssetChangedV3(record) => record.ack.sequence,
            DiskResult::RetiredWithAssets(ack) => ack.sequence,
            DiskResult::RecoveredSAdoption(ack) | DiskResult::RecoveredRAdoption(ack) => {
                ack.sequence
            }
        };
        sequence = sequence.checked_add(1).ok_or_else(codec::invalid)?;
        if committed != sequence {
            return Err(codec::invalid());
        }
        match result {
            DiskResult::RecoveredRAdoption(ack) if tracked.contains_key(&ack.obligation_id) => {
                let state = tracked
                    .get_mut(&ack.obligation_id)
                    .ok_or_else(codec::invalid)?;
                let chain = chains.get(&ack.obligation_id).ok_or_else(codec::invalid)?;
                let holder = chain.get(state.3).ok_or_else(codec::invalid)?;
                if state.4
                    || (state.3 > 0
                        && !predecessor_unchanged_since_adoption(
                            chain[state.3 - 1],
                            state.0,
                            state.1,
                        ))
                    || operation_id != holder.operation_id
                    || ack.sequence != holder.adoption_sequence
                    || ack.obligation_id != holder.obligation_id
                    || ack.previous_owner_id != holder.previous_owner_id
                    || ack.current_owner_id != holder.current_owner_id
                {
                    return Err(codec::invalid());
                }
                state.1 = ack.sequence;
                state.3 += 1;
            }
            DiskResult::AssetChangedV3(record)
                if tracked.contains_key(&record.ack.obligation_id) =>
            {
                let state = tracked
                    .get_mut(&record.ack.obligation_id)
                    .ok_or_else(codec::invalid)?;
                let first = chains
                    .get(&record.ack.obligation_id)
                    .and_then(|chain| chain.first())
                    .ok_or_else(codec::invalid)?;
                if record.ack.sequence > first.adoption_sequence {
                    if state.3 == 0
                        || state.3
                            != chains
                                .get(&record.ack.obligation_id)
                                .ok_or_else(codec::invalid)?
                                .len()
                        || state.4
                        || record.change.previous.as_ref() != Some(&state.2)
                    {
                        return Err(codec::invalid());
                    }
                    state.0 = record.ack.sequence;
                    state.1 = record.ack.sequence;
                    state.2 = record.after.asset.head.ok_or_else(codec::invalid)?;
                }
            }
            DiskResult::Changed(ack)
                if tracked
                    .get(&ack.obligation_id)
                    .is_some_and(|state| state.3 > 0) =>
            {
                let state = tracked
                    .get_mut(&ack.obligation_id)
                    .ok_or_else(codec::invalid)?;
                if state.4
                    || state.3
                        != chains
                            .get(&ack.obligation_id)
                            .ok_or_else(codec::invalid)?
                            .len()
                    || ack.phase != Phase::Settled
                    || ack.sequence <= state.1
                {
                    return Err(codec::invalid());
                }
                state.0 = ack.sequence;
                state.1 = ack.sequence;
                state.4 = true;
            }
            DiskResult::RetiredWithAssets(ack)
                if tracked
                    .get(&ack.source_obligation_id.uuid())
                    .is_some_and(|state| state.3 > 0) =>
            {
                return Err(codec::invalid())
            }
            DiskResult::AssetChangedV2(record)
                if tracked
                    .get(&record.ack.obligation_id)
                    .is_some_and(|state| state.3 > 0) =>
            {
                return Err(codec::invalid())
            }
            _ => {}
        }
    }
    if sequence != next {
        return Err(codec::invalid());
    }
    for (id, chain) in &chains {
        let (last_data, _, head, adopted, settled) = tracked.get(id).ok_or_else(codec::invalid)?;
        let record = obligations.get(id).ok_or_else(codec::invalid)?;
        let ObligationInventory::TypedV2 { members, .. } = &record.inventory else {
            return Err(codec::invalid());
        };
        let current_head = members
            .iter()
            .find_map(|member| match &member.envelope.observed {
                Some(v2::Observed::ScratchAssetsV3 { state }) => state.asset.head.as_ref(),
                _ => None,
            })
            .ok_or_else(codec::invalid)?;
        if *adopted != chain.len()
            || *last_data != record.sequence
            || *settled != (record.phase == Phase::Settled)
            || head != current_head
        {
            return Err(codec::invalid());
        }
    }
    Ok(())
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
        || proof.original_owner_id() != record.owner_id
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
        || chosen.original_owner_id() != claim.original_owner_id()
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

/// Only the actual imported producer owner can write the exact scratch cleanup
/// fold or settle after native closure. This grants no new acquisition.
#[cfg(target_os = "linux")]
pub(super) fn change(
    tx: &Transaction<'_>,
    witness: &storage::Witness,
    owner: Uuid,
    key: &TransitionKey,
    mutation: &Mutation,
    authority: &crate::adaptive_resources::runtime::recovery::RecoveredRAuthority,
) -> Result<DiskResult, CustodyError> {
    let h = &key.handle;
    if authority.journal_id() != witness.journal_id
        || authority.installation_id() != witness.installation_id
        || authority.obligation_id() != h.obligation_id
        || authority.graph_id() != h.graph_id
        || authority.current_owner_id() != owner
        || authority.binding() != h.runtime_binding
        || h.journal_id != witness.journal_id
        || h.owner_id != owner
    {
        return Err(codec::invalid());
    }
    let row: (i64, Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>, i64, Vec<u8>, i64, String) = tx.query_row(
        "SELECT adoption_sequence,operation_id,previous_owner_id,current_owner_id,head_operation_id,head_revision,root_member_id,root_pid,root_creation_marker FROM recovered_r_holders WHERE obligation_id=?1 ORDER BY adoption_sequence DESC LIMIT 1",
        [h.obligation_id.as_bytes()],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?)),
    )?;
    if row.0 != authority.adoption_sequence()
        || codec::uuid_from_bytes(&row.1)? != authority.adoption_operation_id()
        || codec::uuid_from_bytes(&row.2)? != authority.previous_owner_id()
        || codec::uuid_from_bytes(&row.3)? != owner
        || codec::uuid_from_bytes(&row.4)? != authority.state_head().operation_id.uuid()
        || u64::try_from(row.5).map_err(|_| codec::invalid())? != authority.state_head().revision
        || codec::uuid_from_bytes(&row.6)? != authority.root_member_id()
        || u32::try_from(row.7).map_err(|_| codec::invalid())? != authority.root_pid()
        || row.8 != authority.root_creation_marker().to_string()
    {
        return Err(codec::invalid());
    }
    let (graph, recorded_owner, phase, sequence, manifest_json):
        (Vec<u8>, Vec<u8>, String, i64, String) = tx.query_row(
        "SELECT graph_id,owner_id,phase,sequence,manifest_json FROM obligations WHERE obligation_id=?1",
        [h.obligation_id.as_bytes()],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
    )?;
    let v2::ManifestDocument::V2(manifest) = v2::decode_manifest(&manifest_json)? else {
        return Err(codec::invalid());
    };
    let prior = Phase::parse(&phase)?;
    if codec::uuid_from_bytes(&graph)? != h.graph_id
        || codec::uuid_from_bytes(&recorded_owner)? != authority.original_owner_id()
        || manifest.owner_id.uuid() != authority.original_owner_id()
        || manifest.graph_id.uuid() != h.graph_id
        || sequence != key.expected_sequence
        || !matches!(
            prior,
            Phase::AcquisitionPossible | Phase::Sealed | Phase::UnknownHeld
        )
    {
        return Err(codec::invalid());
    }
    match mutation {
        Mutation::AssetChangeV3 { change } => {
            scratch::bootstrap::change(tx, key, prior, &manifest, change)
        }
        Mutation::SettleTyped(proof) => settle(tx, key, prior, &manifest, proof, authority),
        _ => Err(codec::invalid()),
    }
}

#[cfg(target_os = "linux")]
fn settle(
    tx: &Transaction<'_>,
    key: &TransitionKey,
    prior: Phase,
    manifest: &v2::Manifest,
    proof: &PhysicalTypedSettlement,
    authority: &crate::adaptive_resources::runtime::recovery::RecoveredRAuthority,
) -> Result<DiskResult, CustodyError> {
    if !matches!(
        prior,
        Phase::AcquisitionPossible | Phase::Sealed | Phase::UnknownHeld
    ) || proof.receipt.custody_binding() != key.handle.runtime_binding
        || proof.settlement.version != 2
        || proof.settlement.owner_id != manifest.owner_id
        || proof.settlement.graph_id != manifest.graph_id
        || proof.settlement.operation_id.uuid() != key.operation_id
        || manifest.owner_id.uuid() != authority.original_owner_id()
        || manifest.members.len() != proof.settlement.members.len()
    {
        return Err(codec::invalid());
    }
    let mut envelopes = Vec::new();
    let mut scratch = None;
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
        if phase != "planned" && phase != "acquired" {
            return Err(codec::invalid());
        }
        let envelope: v2::Envelope = codec::decode(&identity_json)?;
        v2::validate_envelope(manifest, &envelope)?;
        if envelope.planned != *planned {
            return Err(codec::invalid());
        }
        match (planned, fact) {
            (
                v2::MemberPlan::SourceV1 { source, .. },
                v2::ClosedFact::SourceV1 { holder_id, copies },
            ) if holder_id == &source.holder_id
                && copies.len() == source.planned_copies.len()
                && copies.iter().all(|copy| {
                    matches!(copy,
                        v2::CopyClosed::DomainExtinct { domain, operation_id, .. }
                            if domain == &source.filesystem_domain
                                && operation_id.uuid() == key.operation_id)
                }) => {}
            (v2::MemberPlan::LegacyV1 { planned }, v2::ClosedFact::LegacyV1 { fact })
                if planned.kind == MemberKind::Root
                    && planned.member_id == authority.root_member_id()
                    && fact.member_id == planned.member_id
                    && fact.domain == planned.domain
                    && fact.object_identity
                        == format!(
                            "{}:{}",
                            authority.root_pid(),
                            authority.root_creation_marker()
                        )
                    && !fact.evidence.is_empty() => {}
            (
                v2::MemberPlan::VodOutputTaskV1 { task, .. },
                v2::ClosedFact::VodOutputTaskDomainExtinctV1 {
                    task: closed,
                    root_member_id,
                    root_pid,
                    root_creation_marker,
                    operation_id,
                },
            ) if closed == task
                && root_member_id.uuid() == authority.root_member_id()
                && *root_pid == authority.root_pid()
                && *root_creation_marker == authority.root_creation_marker()
                && operation_id.uuid() == key.operation_id => {}
            (
                v2::MemberPlan::ScratchAssetsV3 { slot, plan },
                v2::ClosedFact::ScratchAssetsV3 {
                    plan: closed,
                    exact_head,
                    operation_id,
                },
            ) if slot.member_id == *member_id
                && closed == plan
                && exact_head.operation_id == *operation_id =>
            {
                let Some(v2::Observed::ScratchAssetsV3 { state }) = &envelope.observed else {
                    return Err(codec::invalid());
                };
                if phase != "acquired" || state.asset.head.as_ref() != Some(exact_head) {
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
                    || !fold.files().is_empty()
                {
                    return Err(codec::invalid());
                }
                scratch = Some(slot.member_id);
            }
            _ => return Err(codec::invalid()),
        }
        envelopes.push(envelope);
    }
    if scratch.map(v2::Id::uuid) != Some(authority.scratch_member_id()) {
        return Err(codec::invalid());
    }
    v2::validate_settlement(manifest, &envelopes, &proof.settlement)?;
    let settlement_json = codec::encode(&proof.settlement)?;
    let sequence = next_sequence(tx)?;
    if tx.execute(
        "UPDATE obligations SET phase='settled',sequence=?2,last_operation_id=?3,hold_reason=NULL,settlement_json=?4 WHERE obligation_id=?1 AND sequence=?5",
        params![key.handle.obligation_id.as_bytes(), sequence, key.operation_id.as_bytes(), settlement_json, key.expected_sequence],
    )? != 1 {
        return Err(codec::invalid());
    }
    if tx.execute(
        "UPDATE members SET phase='closed',proof_json=?2 WHERE obligation_id=?1 AND phase IN ('planned','acquired')",
        params![key.handle.obligation_id.as_bytes(), codec::encode(&proof.settlement)?],
    )? != manifest.members.len() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovered_r_overlay_requires_exact_root_task_and_scratch_dispositions() {
        let (manifest, _, mut settlement) = v2::tests::never_acquired_v3_fixture();
        let v2::MemberPlan::LegacyV1 { planned: root } = &manifest.members[0] else {
            unreachable!()
        };
        let v2::MemberPlan::SourceV1 { source, .. } = &manifest.members[1] else {
            unreachable!()
        };
        let v2::MemberPlan::ScratchAssetsV3 { plan: scratch, .. } = &manifest.members[2] else {
            unreachable!()
        };
        let v2::MemberPlan::VodOutputTaskV1 { task, .. } = &manifest.members[3] else {
            unreachable!()
        };
        let operation_id = settlement.operation_id;
        let head = AssetEventRefV1 {
            operation_id: v2::Id::new(Uuid::new_v4()).unwrap(),
            revision: 1,
        };
        settlement.members[0] = v2::MemberDisposition::Closed {
            member_id: v2::Id::new(root.member_id).unwrap(),
            fact: v2::ClosedFact::LegacyV1 {
                fact: crate::resource_custody::ClosedMemberFact {
                    member_id: root.member_id,
                    adapter: root.adapter.clone(),
                    adapter_version: root.adapter_version,
                    domain: root.domain.clone(),
                    object_identity: "123:456".into(),
                    evidence: "typed overlay fixture only".into(),
                },
            },
        };
        settlement.members[1] = v2::MemberDisposition::Closed {
            member_id: source.member_id,
            fact: v2::ClosedFact::SourceV1 {
                holder_id: source.holder_id,
                copies: source
                    .planned_copies
                    .iter()
                    .cloned()
                    .map(|copy| v2::CopyClosed::DomainExtinct {
                        copy,
                        operation_id,
                        domain: source.filesystem_domain.clone(),
                    })
                    .collect(),
            },
        };
        settlement.members[2] = v2::MemberDisposition::Closed {
            member_id: v2::Id::new(manifest.members[2].member_id()).unwrap(),
            fact: v2::ClosedFact::ScratchAssetsV3 {
                plan: scratch.clone(),
                exact_head: head.clone(),
                operation_id: head.operation_id,
            },
        };
        settlement.members[3] = v2::MemberDisposition::Closed {
            member_id: task.member_id,
            fact: v2::ClosedFact::VodOutputTaskDomainExtinctV1 {
                task: task.clone(),
                root_member_id: task.root_member_id,
                root_pid: 123,
                root_creation_marker: 456,
                operation_id,
            },
        };
        let holder = RecoveredRHolder {
            obligation_id: Uuid::new_v4(),
            adoption_sequence: 2,
            operation_id: Uuid::new_v4(),
            previous_owner_id: manifest.owner_id.uuid(),
            current_owner_id: Uuid::new_v4(),
            original_r_sequence: 1,
            head,
            root_member_id: root.member_id,
            root_pid: 123,
            root_creation_marker: 456,
        };
        // This is an audit-overlay shape test; it creates no native exit proof.
        validate_settled_dispositions(&manifest, &settlement, &holder).unwrap();

        let mut wrong_root = holder.clone();
        wrong_root.root_pid = 124;
        assert!(validate_settled_dispositions(&manifest, &settlement, &wrong_root).is_err());

        let mut wrong_scratch = settlement.clone();
        let v2::MemberDisposition::Closed {
            fact: v2::ClosedFact::ScratchAssetsV3 { plan, .. },
            ..
        } = &mut wrong_scratch.members[2]
        else {
            unreachable!()
        };
        plan.asset_id = v2::Id::new(Uuid::new_v4()).unwrap();
        assert!(validate_settled_dispositions(&manifest, &wrong_scratch, &holder).is_err());

        let mut wrong_task = settlement;
        let v2::MemberDisposition::Closed {
            fact: v2::ClosedFact::VodOutputTaskDomainExtinctV1 { root_member_id, .. },
            ..
        } = &mut wrong_task.members[3]
        else {
            unreachable!()
        };
        *root_member_id = v2::Id::new(Uuid::new_v4()).unwrap();
        assert!(validate_settled_dispositions(&manifest, &wrong_task, &holder).is_err());
    }

    #[test]
    fn recovered_r_handoff_requires_contiguous_owner_and_unchanged_fold() {
        let original = Uuid::new_v4();
        let obligation_id = Uuid::new_v4();
        let head = AssetEventRefV1 {
            operation_id: v2::Id::new(Uuid::new_v4()).unwrap(),
            revision: 1,
        };
        let first = RecoveredRHolder {
            obligation_id,
            adoption_sequence: 11,
            operation_id: Uuid::new_v4(),
            previous_owner_id: original,
            current_owner_id: Uuid::new_v4(),
            original_r_sequence: 5,
            head: head.clone(),
            root_member_id: Uuid::new_v4(),
            root_pid: 123,
            root_creation_marker: 456,
        };
        let second = RecoveredRHolder {
            adoption_sequence: 12,
            operation_id: Uuid::new_v4(),
            previous_owner_id: first.current_owner_id,
            current_owner_id: Uuid::new_v4(),
            ..first.clone()
        };
        assert!(next_holder_matches_previous(None, original, &first));
        assert!(next_holder_matches_previous(
            Some(&first),
            original,
            &second
        ));
        assert!(predecessor_unchanged_since_adoption(&first, 5, 11));
        assert!(!predecessor_unchanged_since_adoption(&first, 12, 12));

        let mut skipped_owner = second.clone();
        skipped_owner.previous_owner_id = original;
        assert!(!next_holder_matches_previous(
            Some(&first),
            original,
            &skipped_owner
        ));
        let mut changed_head = second.clone();
        changed_head.head.revision = 2;
        assert!(!next_holder_matches_previous(
            Some(&first),
            original,
            &changed_head
        ));
        let mut changed_root = second;
        changed_root.root_creation_marker += 1;
        assert!(!next_holder_matches_previous(
            Some(&first),
            original,
            &changed_root
        ));
    }
}
