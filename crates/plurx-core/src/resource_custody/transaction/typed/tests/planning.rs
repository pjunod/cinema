//! Durable discovered-source copy growth, using audit DTOs only. No receipt
//! or native launch authority is created by this transaction fixture.
use super::*;

fn planning_source_fixture() -> (Fixture, v2::Envelope) {
    let mut f = fixture(false);
    let original = typed_manifest(&f);
    let (manifest, envelopes, _) = v2::tests::planning::planning_fixture();
    // Bind every nested owner/graph association to this real journal fixture.
    let rebind = |json: String| {
        json.replace(
            &manifest.owner_id.uuid().to_string(),
            &original.owner_id.uuid().to_string(),
        )
        .replace(
            &manifest.graph_id.uuid().to_string(),
            &original.graph_id.uuid().to_string(),
        )
    };
    let manifest: v2::Manifest = codec::decode(&rebind(codec::encode(&manifest).unwrap())).unwrap();
    let full: v2::Envelope = codec::decode(&rebind(
        codec::encode(
            envelopes
                .iter()
                .find(|e| matches!(e.planned, v2::MemberPlan::FixedProbeDiscoverSourceV1 { .. }))
                .unwrap(),
        )
        .unwrap(),
    ))
    .unwrap();
    let Command::PrepareTyped { batch, .. } = &mut f.command.command else {
        unreachable!()
    };
    batch.obligations[0].manifest = manifest;
    (f, full)
}

#[test]
fn discovered_source_growth_commits_original_parent_and_child_but_refuses_copy_removal() {
    let (mut f, full) = planning_source_fixture();
    let prepared = prepare(&mut f);
    let handle = &prepared.obligations[0];
    let mut parent = full.clone();
    let Some(v2::Observed::FixedProbeDiscoverSourceV1 { source }) = &mut parent.observed else {
        unreachable!()
    };
    source.copies.truncate(1);
    let armed = change_command(
        handle,
        sequence(&f),
        Mutation::Arm {
            members: vec![full.planned.member_id()],
        },
    );
    execute(&mut f.storage, &armed, f.owner).unwrap();
    let acquired = change_command(
        handle,
        sequence(&f),
        Mutation::RecordTypedAcquired {
            envelope: parent.clone(),
        },
    );
    execute(&mut f.storage, &acquired, f.owner).unwrap();
    let sealed = change_command(handle, sequence(&f), Mutation::Seal);
    execute(&mut f.storage, &sealed, f.owner).unwrap();
    let growth = change_command(
        handle,
        sequence(&f),
        Mutation::RecordTypedAcquired {
            envelope: full.clone(),
        },
    );
    execute(&mut f.storage, &growth, f.owner).unwrap();
    let after = sequence(&f);
    // Retry the exact operation, not a new parent-copy acquisition.
    execute(&mut f.storage, &growth, f.owner).unwrap();
    assert_eq!(sequence(&f), after);
    let removal = change_command(
        handle,
        after,
        Mutation::RecordTypedAcquired { envelope: parent },
    );
    assert!(execute(&mut f.storage, &removal, f.owner).is_err());
    assert_eq!(sequence(&f), after);
    let snapshot = snapshot(&f.storage.connection, &f.storage.witness).unwrap();
    assert_eq!(snapshot.total, vector(10));
    let ObligationInventory::TypedV2 { members, .. } = &snapshot.obligations[0].inventory else {
        unreachable!()
    };
    assert!(members.iter().any(|member| member.envelope == full));
}
