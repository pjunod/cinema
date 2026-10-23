//! Append-only recovered S custodians. The historical R→S obligation, retained
//! asset document, and original owner are never rewritten.
use super::*;
use crate::resource_custody::codec_v2::{self as v2, scratch::AssetEventRefV1};

#[derive(Clone, PartialEq, Eq)]
struct TrackedS {
    transfer_operation_id: Uuid,
    asset_id: v2::Id,
    baseline_head: AssetEventRefV1,
    current_head: AssetEventRefV1,
    current_sequence: i64,
    current_phase: Phase,
}

pub(super) fn load_overlay(
    connection: &Connection,
    schema_version: i64,
    retained_assets: &[RetainedAssetOwner],
    obligations: &[ObligationRecord],
    next: i64,
) -> Result<Vec<RecoveredSHolder>, CustodyError> {
    if schema_version < 4 {
        return Ok(Vec::new());
    }
    let mut rows = connection.prepare("SELECT obligation_id,adoption_sequence,operation_id,previous_owner_id,current_owner_id,original_s_sequence,head_operation_id,head_revision,frozen_digest FROM recovered_s_holders ORDER BY obligation_id,adoption_sequence")?;
    let mut query = rows.query([])?;
    let mut holders = Vec::new();
    let mut previous: Option<RecoveredSHolder> = None;
    let mut group_target: Option<TrackedS> = None;
    while let Some(row) = query.next()? {
        let obligation_id = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(0)?)?;
        if previous
            .as_ref()
            .is_some_and(|holder| holder.obligation_id != obligation_id)
        {
            let last = previous.take().ok_or_else(codec::invalid)?;
            group_target = None;
            let (_, _, _, phase, _) = obligation_tail(connection, last.obligation_id)?;
            if phase != Phase::Settled {
                if holders.len() >= codec::ACTIVE_LIMIT {
                    return Err(codec::invalid());
                }
                holders.push(last);
            }
        }
        let adoption_sequence: i64 = row.get(1)?;
        let operation_id = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(2)?)?;
        let previous_owner_id = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(3)?)?;
        let current_owner_id = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(4)?)?;
        let original_s_sequence: i64 = row.get(5)?;
        let head_operation_id = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(6)?)?;
        let head_revision: i64 = row.get(7)?;
        let frozen_digest: String = row.get(8)?;
        let result_json: String = connection.query_row(
            "SELECT result_json FROM operations WHERE operation_id=?1",
            [operation_id.as_bytes()],
            |operation| operation.get(0),
        )?;
        let DiskResult::RecoveredSAdoption(ack) = codec::decode(&result_json)? else {
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
        // The first row validates the complete immutable target inventory.
        // Subsequent holder rows in this ordered group only advance the owner
        // chain; global operation replay checks their exact adoption heads.
        if let Some(target) = &group_target {
            let prior = previous.as_ref().ok_or_else(codec::invalid)?;
            if !owner_exists(connection, previous_owner_id)?
                || !owner_exists(connection, current_owner_id)?
                || prior.current_owner_id != previous_owner_id
                || previous_owner_id == current_owner_id
                || adoption_sequence <= prior.adoption_sequence
                || adoption_sequence > next
                || original_s_sequence < 1
                || original_s_sequence > target.current_sequence
                || !matches!(
                    target.current_phase,
                    Phase::Sealed | Phase::UnknownHeld | Phase::Settled
                )
                || frozen_digest != prior.frozen_digest
            {
                return Err(codec::invalid());
            }
            previous = Some(RecoveredSHolder {
                obligation_id,
                adoption_sequence,
                operation_id,
                previous_owner_id,
                current_owner_id,
                original_s_sequence,
                head: AssetEventRefV1 {
                    operation_id: v2::Id::new(head_operation_id)?,
                    revision: u64::try_from(head_revision).map_err(|_| codec::invalid())?,
                },
                frozen_digest,
            });
            continue;
        }
        let source_owner: Vec<u8> = connection.query_row(
            "SELECT owner_id FROM obligations WHERE obligation_id=?1",
            [obligation_id.as_bytes()],
            |r| r.get(0),
        )?;
        let source_owner = codec::uuid_from_bytes(&source_owner)?;
        let (expected_previous, previous_sequence) =
            previous.as_ref().map_or((source_owner, 0), |holder| {
                (holder.current_owner_id, holder.adoption_sequence)
            });
        let (_, graph, current_sequence, phase, _) = obligation_tail(connection, obligation_id)?;
        let asset = if let Some(asset) = retained_assets
            .iter()
            .find(|asset| asset.obligation_id == obligation_id)
        {
            asset.custody.clone()
        } else {
            let json: String = connection.query_row(
                "SELECT custody_json FROM retained_asset_owners WHERE obligation_id=?1",
                [obligation_id.as_bytes()],
                |r| r.get(0),
            )?;
            v2::decode_retained_assets(&json)?
        };
        let v2::RetainedAssetsDocument::V2(custody) = &asset else {
            return Err(codec::invalid());
        };
        let v2::TransferIdentity::ScratchAssetsV3 { snapshot, .. } = &custody.transfer.identity
        else {
            return Err(codec::invalid());
        };
        let inventory = if let Some(record) = obligations
            .iter()
            .find(|record| record.obligation_id == obligation_id)
        {
            record.inventory.clone()
        } else {
            let (manifest_json, settlement_json): (String, Option<String>) = connection.query_row(
                "SELECT manifest_json,settlement_json FROM obligations WHERE obligation_id=?1",
                [obligation_id.as_bytes()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            inventory::load(
                connection,
                obligation_id,
                source_owner,
                graph,
                phase,
                &manifest_json,
                settlement_json.as_deref(),
            )?
        };
        let ObligationInventory::TypedV2 { members, .. } = &inventory else {
            return Err(codec::invalid());
        };
        let [member] = members.as_slice() else {
            return Err(codec::invalid());
        };
        let Some(v2::Observed::ScratchAssetsV3 { state }) = &member.envelope.observed else {
            return Err(codec::invalid());
        };
        let head = AssetEventRefV1 {
            operation_id: v2::Id::new(head_operation_id)?,
            revision: u64::try_from(head_revision).map_err(|_| codec::invalid())?,
        };
        let current_head = state.asset.head.clone().ok_or_else(codec::invalid)?;
        let baseline_head = snapshot
            .state
            .asset
            .head
            .clone()
            .ok_or_else(codec::invalid)?;
        if !owner_exists(connection, previous_owner_id)?
            || !owner_exists(connection, current_owner_id)?
            || expected_previous != previous_owner_id
            || previous_owner_id == current_owner_id
            || adoption_sequence <= previous_sequence
            || adoption_sequence > next
            || original_s_sequence < 1
            || original_s_sequence > current_sequence
            || !matches!(phase, Phase::Sealed | Phase::UnknownHeld | Phase::Settled)
            || (matches!(phase, Phase::Settled)
                != (state.asset.lifecycle == v2::scratch::ScratchLifecycleV1::RetiredClosed))
            || snapshot.inventory_sha256 != frozen_digest
        {
            return Err(codec::invalid());
        }
        let target = TrackedS {
            transfer_operation_id: custody.transfer_operation_id.uuid(),
            asset_id: custody.asset_id,
            baseline_head,
            current_head,
            current_sequence,
            current_phase: phase,
        };
        if group_target.as_ref().is_some_and(|prior| prior != &target) {
            return Err(codec::invalid());
        }
        group_target = Some(target);
        previous = Some(RecoveredSHolder {
            obligation_id,
            adoption_sequence,
            operation_id,
            previous_owner_id,
            current_owner_id,
            original_s_sequence,
            head,
            frozen_digest,
        });
    }
    if let Some(last) = previous {
        let (_, _, _, phase, _) = obligation_tail(connection, last.obligation_id)?;
        if phase != Phase::Settled {
            if holders.len() >= codec::ACTIVE_LIMIT {
                return Err(codec::invalid());
            }
            holders.push(last);
        }
    }
    Ok(holders)
}

/// A holder's stored head is the exact target head at adoption, not necessarily
/// the current head after later reads or closure. No audit DTO supplies proof.
struct SReplayState {
    target: TrackedS,
    last_sequence: i64,
    head: AssetEventRefV1,
}

#[derive(Default)]
pub(super) struct ReplayS {
    enabled: bool,
    in_flight: BTreeMap<Uuid, SReplayState>,
}

impl ReplayS {
    pub(super) fn new(schema_version: i64) -> Self {
        Self {
            enabled: schema_version >= 4,
            in_flight: BTreeMap::new(),
        }
    }

    pub(super) fn observe(
        &mut self,
        connection: &Connection,
        operation_id: Uuid,
        result: &DiskResult,
    ) -> Result<(), CustodyError> {
        if !self.enabled {
            return Ok(());
        }
        match result {
            DiskResult::RetiredWithAssets(ack) => {
                let id = ack.target_obligation_id.uuid();
                if !has_s_holder(connection, id)? {
                    return Ok(());
                }
                let target = load_tracked_s(connection, id)?;
                if operation_id != target.transfer_operation_id
                    || ack.asset_id != target.asset_id
                    || self.in_flight.contains_key(&id)
                    || self.in_flight.len() >= codec::ACTIVE_LIMIT
                {
                    return Err(codec::invalid());
                }
                self.in_flight.insert(
                    id,
                    SReplayState {
                        last_sequence: ack.sequence,
                        head: target.baseline_head.clone(),
                        target,
                    },
                );
            }
            DiskResult::AssetChangedV3(record)
                if has_s_holder(connection, record.ack.obligation_id)? =>
            {
                let state = self
                    .in_flight
                    .get_mut(&record.ack.obligation_id)
                    .ok_or_else(codec::invalid)?;
                if record.change.previous.as_ref() != Some(&state.head)
                    || record.ack.sequence <= state.last_sequence
                {
                    return Err(codec::invalid());
                }
                state.last_sequence = record.ack.sequence;
                state.head = record.after.asset.head.clone().ok_or_else(codec::invalid)?;
            }
            DiskResult::AssetChangedV2(record)
                if has_s_holder(connection, record.ack.obligation_id)? =>
            {
                return Err(codec::invalid());
            }
            DiskResult::Changed(ack) => {
                if let Some(state) = self.in_flight.get_mut(&ack.obligation_id) {
                    if ack.sequence <= state.last_sequence {
                        return Err(codec::invalid());
                    }
                    state.last_sequence = ack.sequence;
                    if ack.phase == Phase::Settled {
                        if state.last_sequence != state.target.current_sequence
                            || state.head != state.target.current_head
                        {
                            return Err(codec::invalid());
                        }
                        self.in_flight.remove(&ack.obligation_id);
                    }
                }
            }
            DiskResult::RecoveredSAdoption(ack) => {
                let state = self
                    .in_flight
                    .get(&ack.obligation_id)
                    .ok_or_else(codec::invalid)?;
                let row: (i64, i64, Vec<u8>, i64, Vec<u8>, Vec<u8>) = connection.query_row(
                    "SELECT adoption_sequence,original_s_sequence,head_operation_id,head_revision,previous_owner_id,current_owner_id FROM recovered_s_holders WHERE operation_id=?1 AND obligation_id=?2",
                    params![operation_id.as_bytes(), ack.obligation_id.as_bytes()],
                    |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)),
                )?;
                let head = AssetEventRefV1 {
                    operation_id: v2::Id::new(codec::uuid_from_bytes(&row.2)?)?,
                    revision: u64::try_from(row.3).map_err(|_| codec::invalid())?,
                };
                if ack.operation_id != operation_id
                    || ack.sequence != row.0
                    || ack.previous_owner_id != codec::uuid_from_bytes(&row.4)?
                    || ack.current_owner_id != codec::uuid_from_bytes(&row.5)?
                    || state.last_sequence != row.1
                    || state.head != head
                    || state.target.current_sequence < state.last_sequence
                    || (state.target.current_phase == Phase::Settled
                        && ack.sequence >= state.target.current_sequence)
                {
                    return Err(codec::invalid());
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn finish(self) -> Result<(), CustodyError> {
        for state in self.in_flight.into_values() {
            if state.last_sequence != state.target.current_sequence
                || state.head != state.target.current_head
            {
                return Err(codec::invalid());
            }
        }
        Ok(())
    }
}

fn has_s_holder(connection: &Connection, id: Uuid) -> Result<bool, CustodyError> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM recovered_s_holders WHERE obligation_id=?1)",
            [id.as_bytes()],
            |r| r.get(0),
        )
        .map_err(Into::into)
}

fn load_tracked_s(connection: &Connection, id: Uuid) -> Result<TrackedS, CustodyError> {
    let custody_json: String = connection.query_row(
        "SELECT custody_json FROM retained_asset_owners WHERE obligation_id=?1",
        [id.as_bytes()],
        |r| r.get(0),
    )?;
    let v2::RetainedAssetsDocument::V2(custody) = v2::decode_retained_assets(&custody_json)? else {
        return Err(codec::invalid());
    };
    let v2::TransferIdentity::ScratchAssetsV3 { snapshot, .. } = &custody.transfer.identity else {
        return Err(codec::invalid());
    };
    let baseline_head = snapshot
        .state
        .asset
        .head
        .clone()
        .ok_or_else(codec::invalid)?;
    let (owner, graph, phase, sequence, manifest_json, settlement_json):
        (Vec<u8>, Vec<u8>, String, i64, String, Option<String>) = connection.query_row(
        "SELECT owner_id,graph_id,phase,sequence,manifest_json,settlement_json FROM obligations WHERE obligation_id=?1",
        [id.as_bytes()],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)),
    )?;
    let inventory = inventory::load(
        connection,
        id,
        codec::uuid_from_bytes(&owner)?,
        codec::uuid_from_bytes(&graph)?,
        Phase::parse(&phase)?,
        &manifest_json,
        settlement_json.as_deref(),
    )?;
    let ObligationInventory::TypedV2 { members, .. } = inventory else {
        return Err(codec::invalid());
    };
    let [member] = members.as_slice() else {
        return Err(codec::invalid());
    };
    let Some(v2::Observed::ScratchAssetsV3 { state }) = &member.envelope.observed else {
        return Err(codec::invalid());
    };
    Ok(TrackedS {
        transfer_operation_id: custody.transfer_operation_id.uuid(),
        asset_id: custody.asset_id,
        baseline_head,
        current_head: state.asset.head.clone().ok_or_else(codec::invalid)?,
        current_sequence: sequence,
        current_phase: Phase::parse(&phase)?,
    })
}

#[cfg(test)]
mod streaming_tests {
    use super::*;

    fn replay_adoptions(
        connection: &Connection,
        id: Uuid,
        target: &TrackedS,
    ) -> Result<(), CustodyError> {
        let mut replay = ReplayS::new(4);
        replay.in_flight.insert(
            id,
            SReplayState {
                target: target.clone(),
                last_sequence: 1,
                head: target.baseline_head.clone(),
            },
        );
        let mut statement =
            connection.prepare("SELECT operation_id,result_json FROM operations ORDER BY rowid")?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let operation = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(0)?)?;
            let result: DiskResult = codec::decode(&row.get::<_, String>(1)?)?;
            if matches!(result, DiskResult::RecoveredSAdoption(_)) {
                replay.observe(connection, operation, &result)?;
            }
        }
        replay.finish()
    }

    #[test]
    fn historical_s_replay_validates_more_than_1024_holders_and_the_oldest_identity() {
        let connection = Connection::open_in_memory().expect("fixture database");
        connection
            .execute_batch(
                "CREATE TABLE operations(operation_id BLOB PRIMARY KEY, result_json TEXT NOT NULL);
                 CREATE TABLE recovered_s_holders(
                    obligation_id BLOB NOT NULL,
                    adoption_sequence INTEGER NOT NULL,
                    operation_id BLOB NOT NULL UNIQUE,
                    previous_owner_id BLOB NOT NULL,
                    current_owner_id BLOB NOT NULL,
                    original_s_sequence INTEGER NOT NULL,
                    head_operation_id BLOB NOT NULL,
                    head_revision INTEGER NOT NULL,
                    frozen_digest TEXT NOT NULL,
                    PRIMARY KEY(obligation_id,adoption_sequence));",
            )
            .expect("fixture tables");
        let obligation = Uuid::new_v4();
        let asset = v2::Id::new(Uuid::new_v4()).unwrap();
        let transfer = Uuid::new_v4();
        let head = AssetEventRefV1 {
            operation_id: v2::Id::new(Uuid::new_v4()).unwrap(),
            revision: 1,
        };
        let retirement = DiskResult::RetiredWithAssets(RetirementAck {
            version: 2,
            operation_id: v2::Id::new(transfer).unwrap(),
            sequence: 1,
            source_obligation_id: v2::Id::new(Uuid::new_v4()).unwrap(),
            source_graph_id: v2::Id::new(Uuid::new_v4()).unwrap(),
            target_obligation_id: v2::Id::new(obligation).unwrap(),
            target_graph_id: v2::Id::new(Uuid::new_v4()).unwrap(),
            asset_id: asset,
        });
        connection
            .execute(
                "INSERT INTO operations VALUES(?1,?2)",
                params![transfer.as_bytes(), codec::encode(&retirement).unwrap()],
            )
            .unwrap();
        let mut prior_owner = Uuid::new_v4();
        for sequence in 2..=1026_i64 {
            let operation = Uuid::new_v4();
            let current_owner = Uuid::new_v4();
            let ack = RecoveryAdoptionAck {
                operation_id: operation,
                obligation_id: obligation,
                previous_owner_id: prior_owner,
                current_owner_id: current_owner,
                sequence,
            };
            connection
                .execute(
                    "INSERT INTO operations VALUES(?1,?2)",
                    params![
                        operation.as_bytes(),
                        codec::encode(&DiskResult::RecoveredSAdoption(ack)).unwrap()
                    ],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO recovered_s_holders VALUES(?1,?2,?3,?4,?5,1,?6,1,?7)",
                    params![
                        obligation.as_bytes(),
                        sequence,
                        operation.as_bytes(),
                        prior_owner.as_bytes(),
                        current_owner.as_bytes(),
                        head.operation_id.uuid().as_bytes(),
                        "0".repeat(64)
                    ],
                )
                .unwrap();
            prior_owner = current_owner;
        }
        let target = TrackedS {
            transfer_operation_id: transfer,
            asset_id: asset,
            baseline_head: head.clone(),
            current_head: head,
            current_sequence: 1,
            current_phase: Phase::Sealed,
        };
        replay_adoptions(&connection, obligation, &target).unwrap();
        connection
            .execute(
                "UPDATE recovered_s_holders SET previous_owner_id=?1 WHERE obligation_id=?2 AND adoption_sequence=2",
                params![Uuid::new_v4().as_bytes(), obligation.as_bytes()],
            )
            .unwrap();
        assert!(replay_adoptions(&connection, obligation, &target).is_err());
    }
}

#[cfg(target_os = "linux")]
pub(super) fn adopt(
    tx: &Transaction<'_>,
    witness: &storage::Witness,
    owner: Uuid,
    operation_id: Uuid,
    proof: &crate::adaptive_resources::runtime::recovery::NativeRetainedSImportProof,
    exit: &crate::resource_custody::recovery::PositivePreviousOwnerExit,
) -> Result<DiskResult, CustodyError> {
    let snapshot = super::snapshot(tx, witness)?;
    // Match the bounded replay parser before writing. A successful adoption
    // must remain readable on the next startup.
    if snapshot.recovered_s_holders.len() >= codec::ACTIVE_LIMIT {
        return Err(CustodyError::new(
            ErrorKind::Exhausted,
            "recovered S holder history bound reached",
        ));
    }
    let claim = proof.claim();
    let record = snapshot
        .obligations
        .iter()
        .find(|record| record.obligation_id == proof.obligation_id())
        .ok_or_else(codec::invalid)?;
    let effective = snapshot
        .recovered_s_holders
        .iter()
        .filter(|holder| holder.obligation_id == record.obligation_id)
        .max_by_key(|holder| holder.adoption_sequence)
        .map_or(record.owner_id, |holder| holder.current_owner_id);
    let current_owner = snapshot
        .owners
        .iter()
        .find(|identity| identity.owner_id == owner)
        .ok_or_else(codec::invalid)?;
    if proof.journal_id() != witness.journal_id
        || proof.installation_id() != witness.installation_id
        || proof.current_owner_id() != owner
        || proof.previous_owner_id() != effective
        || proof.obligation_id() != claim.obligation_id()
        || claim.original_owner_id() != record.owner_id
        || claim.old_owner_id() != effective
        || claim.current_head() != proof.state_head()
        || claim.inventory_sha256() != proof.frozen_digest()
        || record.sequence < 1
        || !exit.matches(&snapshot, current_owner, effective)
    {
        return Err(codec::invalid());
    }
    let chosen = crate::resource_custody::recovery::select_dormant_retained_s(
        tx,
        &snapshot,
        exit,
        proof.obligation_id(),
    )?;
    if chosen.original_owner_id() != claim.original_owner_id()
        || chosen.current_head() != claim.current_head()
        || chosen.inventory_sha256() != claim.inventory_sha256()
        || chosen.resources() != claim.resources()
    {
        return Err(codec::invalid());
    }
    let sequence = next_sequence(tx)?;
    tx.execute(
        "INSERT INTO recovered_s_holders VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![
            record.obligation_id.as_bytes(),
            sequence,
            operation_id.as_bytes(),
            effective.as_bytes(),
            owner.as_bytes(),
            record.sequence,
            proof.state_head().operation_id.uuid().as_bytes(),
            i64::try_from(proof.state_head().revision).map_err(|_| codec::invalid())?,
            proof.frozen_digest(),
        ],
    )?;
    Ok(DiskResult::RecoveredSAdoption(RecoveryAdoptionAck {
        operation_id,
        obligation_id: record.obligation_id,
        previous_owner_id: effective,
        current_owner_id: owner,
        sequence,
    }))
}

/// Existing recovered S only: current native owner plus the exact latest
/// append-only holder row authorizes target read/cleanup events. The V2
/// manifest, transfer and original obligation owner remain historical facts.
#[cfg(target_os = "linux")]
pub(super) fn change(
    tx: &Transaction<'_>,
    witness: &storage::Witness,
    owner: Uuid,
    key: &TransitionKey,
    mutation: &Mutation,
    authority: &crate::adaptive_resources::runtime::recovery::RecoveredSAuthority,
) -> Result<DiskResult, CustodyError> {
    let h = &key.handle;
    if authority.journal_id() != witness.journal_id
        || authority.installation_id() != witness.installation_id
        || authority.obligation_id() != h.obligation_id
        || authority.current_owner_id() != owner
        || authority.binding() != h.runtime_binding
        || h.journal_id != witness.journal_id
        || h.owner_id != owner
    {
        return Err(codec::invalid());
    }
    let (adoption_sequence, adoption_operation, previous_owner, current_owner, head_operation, head_revision, frozen_digest):
        (i64, Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>, i64, String) = tx.query_row(
        "SELECT adoption_sequence,operation_id,previous_owner_id,current_owner_id,head_operation_id,head_revision,frozen_digest FROM recovered_s_holders WHERE obligation_id=?1 ORDER BY adoption_sequence DESC LIMIT 1",
        [h.obligation_id.as_bytes()],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)),
    )?;
    if adoption_sequence != authority.adoption_sequence()
        || codec::uuid_from_bytes(&adoption_operation)? != authority.adoption_operation_id()
        || codec::uuid_from_bytes(&previous_owner)? != authority.previous_owner_id()
        || codec::uuid_from_bytes(&current_owner)? != owner
        || codec::uuid_from_bytes(&head_operation)? != authority.state_head().operation_id.uuid()
        || u64::try_from(head_revision).map_err(|_| codec::invalid())?
            != authority.state_head().revision
        || frozen_digest != authority.frozen_digest()
    {
        return Err(codec::invalid());
    }
    let (graph, recorded_owner, phase, sequence, manifest_json):
        (Vec<u8>, Vec<u8>, String, i64, String) = tx.query_row(
        "SELECT graph_id,owner_id,phase,sequence,manifest_json FROM obligations WHERE obligation_id=?1",
        [h.obligation_id.as_bytes()],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
    )?;
    let original_owner = codec::uuid_from_bytes(&recorded_owner)?;
    let v2::ManifestDocument::V2(manifest) = v2::decode_manifest(&manifest_json)? else {
        return Err(codec::invalid());
    };
    if codec::uuid_from_bytes(&graph)? != h.graph_id
        || original_owner != authority.original_owner_id()
        || manifest.owner_id.uuid() != original_owner
        || manifest.graph_id.uuid() != h.graph_id
        || sequence != key.expected_sequence
        || !matches!(Phase::parse(&phase)?, Phase::Sealed | Phase::UnknownHeld)
    {
        return Err(codec::invalid());
    }
    let prior = Phase::parse(&phase)?;
    match mutation {
        Mutation::AssetChangeV3 { change } => {
            scratch::bootstrap::change(tx, key, prior, &manifest, change)
        }
        Mutation::SettleTyped(proof) => {
            retained_settlement::change(tx, original_owner, key, prior, &manifest, proof)
        }
        _ => Err(codec::invalid()),
    }
}
