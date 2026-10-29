//! Planning audit fixtures and codec refusals; these never mint native receipts.
use super::super::fixed_probe::{FixedPlanningEngineFileSlotV1, FIXED_PLANNING_ENGINE_MAX_OBJECTS};
use super::*;

fn allocation(owner_id: Id) -> OwnerAllocation {
    OwnerAllocation {
        owner_id,
        memory_bytes: 4096,
        handles: 1,
    }
}
fn slot(member_id: Id, owned_by: Option<Id>, depends_on: Vec<Id>) -> MemberSlot {
    MemberSlot {
        member_id,
        owned_by,
        depends_on,
        protected_paths: vec![],
    }
}

/// A complete seven-member P audit, independently usable by journal tests.
/// Its identities describe synthetic original owners, not physical capabilities.
pub(in crate::resource_custody) fn planning_fixture() -> (Manifest, Vec<Envelope>, Settlement) {
    let seed = fixture();
    let owner_id = seed.owner_id;
    let graph_id = seed.graph_id;
    let roots = [id(), id(), id(), id()];
    let source_member = id();
    let probe_member = id();
    let engine_member = id();
    let MemberPlan::SourceV1 {
        source: old_source, ..
    } = &seed.members[1]
    else {
        unreachable!()
    };
    let domain = old_source.filesystem_domain.clone();
    let parent = SourceCopy {
        copy_id: id(),
        owner: CopyOwner::DaemonSourceCell {
            owner_id,
            graph_id,
            member_id: source_member,
        },
        acquisition_operation_id: id(),
        parent_copy_id: None,
    };
    let child = SourceCopy {
        copy_id: id(),
        owner: CopyOwner::OwnedRoot {
            graph_id,
            root_member_id: roots[1],
        },
        acquisition_operation_id: id(),
        parent_copy_id: Some(parent.copy_id),
    };
    let source = FixedProbeDiscoverSourceV1 {
        version: 1,
        holder_id: id(),
        member_id: source_member,
        media_file_id: "planning-file".into(),
        recipe_source_binding: "planning-recipe".into(),
        absolute_path: "/fixture/input.mkv".into(),
        scanner_size: 42,
        scanner_mtime: 10,
        filesystem_domain: domain.clone(),
        planned_copies: vec![parent, child],
        recovery: SourceRecovery::LocalSourceHandlesV1 {
            owner_id,
            graph_id,
            filesystem_domain: domain.clone(),
        },
    };
    let mut probe = FixedProbeDiscoverTaskV1 {
        version: 1,
        member_id: probe_member,
        source_member_id: source_member,
        version_root_member_id: roots[0],
        document_root_member_id: roots[1],
        version_close_operation_id: id(),
        document_close_operation_id: id(),
        media_file_id: source.media_file_id.clone(),
        recipe_source_binding: source.recipe_source_binding.clone(),
        configured_locator: "ffprobe".into(),
        image_owner_id: id(),
        image_close_operation_id: id(),
        snapshot_owner_id: id(),
        snapshot_close_operation_id: id(),
        source_child_owner_id: id(),
        source_child_close_operation_id: id(),
        version_stdout_owner_id: id(),
        version_stdout_close_operation_id: id(),
        stdout_owner_id: id(),
        stdout_close_operation_id: id(),
        diagnostic_owner_id: id(),
        diagnostic_close_operation_id: id(),
        inventory: inventory(),
    };
    probe.inventory.pending_io = [
        probe.image_owner_id,
        probe.snapshot_owner_id,
        probe.source_child_owner_id,
    ]
    .into_iter()
    .map(allocation)
    .collect();
    probe.inventory.readers = [
        probe.version_stdout_owner_id,
        probe.stdout_owner_id,
        probe.diagnostic_owner_id,
    ]
    .into_iter()
    .map(allocation)
    .collect();
    let mut engine = FixedPlanningEngineTaskV1 {
        version: 1,
        member_id: engine_member,
        version_root_member_id: roots[2],
        dependency_root_member_id: roots[3],
        version_close_operation_id: id(),
        dependency_close_operation_id: id(),
        configured_ffmpeg_locator: "ffmpeg".into(),
        dependency_tool_locator: "ldd".into(),
        file_slots: (0..FIXED_PLANNING_ENGINE_MAX_OBJECTS)
            .map(|_| FixedPlanningEngineFileSlotV1 {
                owner_id: id(),
                close_operation_id: id(),
            })
            .collect(),
        version_stdout_owner_id: id(),
        version_stdout_close_operation_id: id(),
        version_diagnostic_owner_id: id(),
        version_diagnostic_close_operation_id: id(),
        dependency_stdout_owner_id: id(),
        dependency_stdout_close_operation_id: id(),
        dependency_diagnostic_owner_id: id(),
        dependency_diagnostic_close_operation_id: id(),
        hash_buffer_owner_id: id(),
        hash_buffer_close_operation_id: id(),
        inventory: inventory(),
    };
    engine.inventory.pending_io = engine
        .file_slots
        .iter()
        .map(|file| allocation(file.owner_id))
        .collect();
    engine.inventory.readers = [
        engine.version_stdout_owner_id,
        engine.version_diagnostic_owner_id,
        engine.dependency_stdout_owner_id,
        engine.dependency_diagnostic_owner_id,
    ]
    .into_iter()
    .map(allocation)
    .collect();
    engine.inventory.buffers = vec![allocation(engine.hash_buffer_owner_id)];
    let root_plan = |member_id: Id| MemberPlan::LegacyV1 {
        planned: PlannedMember {
            member_id: member_id.uuid(),
            kind: MemberKind::Root,
            adapter: crate::resource_custody::Adapter::ProcessRoot,
            adapter_version: 1,
            domain: "planning-fixture".into(),
            locator: "fixed-program".into(),
            recovery: crate::resource_custody::RecoveryMethod::LiveOnly {
                reason: "fixture has no restart authority".into(),
            },
            owned_by: None,
            depends_on: vec![],
            maximum_cardinality: 1,
            protected_paths: vec![],
        },
    };
    let mut members: Vec<_> = roots.into_iter().map(root_plan).collect();
    members.extend([
        MemberPlan::FixedProbeDiscoverSourceV1 {
            slot: MemberSlot {
                protected_paths: vec![source.absolute_path.clone()],
                ..slot(source_member, None, vec![])
            },
            source: source.clone(),
        },
        MemberPlan::FixedProbeDiscoverTaskV1 {
            slot: slot(
                probe_member,
                Some(roots[1]),
                vec![source_member, roots[0], roots[1]],
            ),
            task: probe.clone(),
        },
        MemberPlan::FixedPlanningEngineTaskV1 {
            slot: slot(engine_member, Some(roots[3]), vec![roots[2], roots[3]]),
            task: engine.clone(),
        },
    ]);
    let manifest = Manifest {
        version: 2,
        owner_id,
        graph_id,
        attempt_id: None,
        members,
    };
    let envelopes = manifest
        .members
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, planned)| {
            let observed = match &planned {
                MemberPlan::LegacyV1 { planned: root } => Observed::LegacyV1 {
                    identity: crate::resource_custody::ObservedIdentity {
                        version: 1,
                        member_id: root.member_id,
                        adapter: root.adapter.clone(),
                        adapter_version: root.adapter_version,
                        domain: root.domain.clone(),
                        object_identity: format!("original-root-{index}"),
                        process: None,
                    },
                },
                MemberPlan::FixedProbeDiscoverSourceV1 { .. } => {
                    Observed::FixedProbeDiscoverSourceV1 {
                        source: SourceObserved {
                            version: 1,
                            holder_id: source.holder_id,
                            member_id: source_member,
                            copies: source.planned_copies.clone(),
                            access: SourceAccess::ReadOnlyRegular,
                            source_association: SourceAssociation {
                                media_file_id: source.media_file_id.clone(),
                                object_version: "s1:1:2:42:10:0:10:0".into(),
                                recipe_source_binding: source.recipe_source_binding.clone(),
                            },
                            native_object: NativeObject {
                                file_type: FileType::Regular,
                                device: NativeValue::Known { value: 1 },
                                inode: NativeValue::Known { value: 2 },
                                mount_id: NativeValue::Unknown {
                                    reason: "fixture unknown mount".into(),
                                },
                                filesystem_domain: domain.clone(),
                            },
                        },
                    }
                }
                MemberPlan::FixedProbeDiscoverTaskV1 { .. } => Observed::FixedProbeDiscoverTaskV1 {
                    task: FixedProbeDiscoverTaskObservedV1 {
                        task: probe.clone(),
                        executable_object_version: "s1:3:4:512:10:0:10:0".into(),
                        executable_sha256: "a".repeat(64),
                        build_digest: "b".repeat(64),
                    },
                },
                MemberPlan::FixedPlanningEngineTaskV1 { .. } => {
                    Observed::FixedPlanningEngineTaskV2 {
                        task: FixedPlanningEngineTaskObservedV2 {
                            member_id: engine.member_id,
                            plan_sha256: engine_task_plan_sha256(&engine).unwrap(),
                            used_file_slots: 2,
                            media_digest: "c".repeat(64),
                        },
                    }
                }
                _ => unreachable!(),
            };
            Envelope {
                version: 2,
                planned,
                observed: Some(observed),
            }
        })
        .collect::<Vec<_>>();
    let mut closed = envelopes[..4]
        .iter()
        .map(|envelope| {
            let MemberPlan::LegacyV1 { planned } = &envelope.planned else {
                unreachable!()
            };
            let Some(Observed::LegacyV1 { identity }) = &envelope.observed else {
                unreachable!()
            };
            MemberDisposition::Closed {
                member_id: Id::new(planned.member_id).unwrap(),
                fact: ClosedFact::LegacyV1 {
                    fact: crate::resource_custody::ClosedMemberFact {
                        member_id: planned.member_id,
                        adapter: planned.adapter.clone(),
                        adapter_version: planned.adapter_version,
                        domain: planned.domain.clone(),
                        object_identity: identity.object_identity.clone(),
                        evidence: "synthetic closed-owner audit".into(),
                    },
                },
            }
        })
        .collect::<Vec<_>>();
    let probe_owners = [
        (probe.image_owner_id, probe.image_close_operation_id),
        (probe.snapshot_owner_id, probe.snapshot_close_operation_id),
        (
            probe.source_child_owner_id,
            probe.source_child_close_operation_id,
        ),
        (
            probe.version_stdout_owner_id,
            probe.version_stdout_close_operation_id,
        ),
        (probe.stdout_owner_id, probe.stdout_close_operation_id),
        (
            probe.diagnostic_owner_id,
            probe.diagnostic_close_operation_id,
        ),
    ]
    .into_iter()
    .map(|(owner_id, operation_id)| OwnerClosed {
        owner_id,
        operation_id,
    })
    .collect();
    let mut engine_owners = engine
        .file_slots
        .iter()
        .enumerate()
        .map(|(index, file)| {
            if index < 2 {
                FixedEngineOwnerDispositionV1::ActualOwnedClose {
                    owner_id: file.owner_id,
                    operation_id: file.close_operation_id,
                }
            } else {
                FixedEngineOwnerDispositionV1::ProvenNeverAcquired {
                    owner_id: file.owner_id,
                    operation_id: file.close_operation_id,
                }
            }
        })
        .collect::<Vec<_>>();
    engine_owners.extend(
        [
            (
                engine.version_stdout_owner_id,
                engine.version_stdout_close_operation_id,
            ),
            (
                engine.version_diagnostic_owner_id,
                engine.version_diagnostic_close_operation_id,
            ),
            (
                engine.dependency_stdout_owner_id,
                engine.dependency_stdout_close_operation_id,
            ),
            (
                engine.dependency_diagnostic_owner_id,
                engine.dependency_diagnostic_close_operation_id,
            ),
            (
                engine.hash_buffer_owner_id,
                engine.hash_buffer_close_operation_id,
            ),
        ]
        .into_iter()
        .map(
            |(owner_id, operation_id)| FixedEngineOwnerDispositionV1::ActualOwnedClose {
                owner_id,
                operation_id,
            },
        ),
    );
    closed.extend([
        MemberDisposition::Closed {
            member_id: source_member,
            fact: ClosedFact::FixedProbeDiscoverSourceV1 {
                holder_id: source.holder_id,
                copies: source
                    .planned_copies
                    .iter()
                    .cloned()
                    .map(|copy| CopyClosed::ActualOwnedClose {
                        copy,
                        operation_id: id(),
                    })
                    .collect(),
            },
        },
        MemberDisposition::Closed {
            member_id: probe_member,
            fact: ClosedFact::FixedProbeDiscoverTaskV1 {
                task: probe.clone(),
                join_operation_id: id(),
                version_close_operation_id: probe.version_close_operation_id,
                document_close_operation_id: probe.document_close_operation_id,
                owners: probe_owners,
            },
        },
        MemberDisposition::Closed {
            member_id: engine_member,
            fact: ClosedFact::FixedPlanningEngineTaskV2 {
                member_id: engine.member_id,
                plan_sha256: engine_task_plan_sha256(&engine).unwrap(),
                join_operation_id: id(),
                version_close_operation_id: engine.version_close_operation_id,
                dependency_close_operation_id: engine.dependency_close_operation_id,
                used_file_slots: 2,
                owners: engine_owners,
            },
        },
    ]);
    let settlement = Settlement {
        version: 2,
        owner_id,
        graph_id,
        operation_id: id(),
        members: closed,
    };
    (manifest, envelopes, settlement)
}

#[test]
fn seven_member_planning_audit_roundtrips_and_settles_exact_original_owners() {
    let (manifest, envelopes, settlement) = planning_fixture();
    validate_manifest(&manifest).unwrap();
    assert_eq!(manifest.members.len(), 7);
    assert_eq!(
        decode_manifest(&codec::encode(&manifest).unwrap()).unwrap(),
        ManifestDocument::V2(manifest.clone())
    );
    let mut envelope_bytes = Vec::with_capacity(envelopes.len());
    for envelope in &envelopes {
        validate_envelope(&manifest, envelope).unwrap();
        let encoded = codec::encode(envelope).expect("each complete acquired envelope fits 64 KiB");
        assert!(encoded.len() <= codec::JSON_LIMIT);
        assert_eq!(codec::decode::<Envelope>(&encoded).unwrap(), *envelope);
        envelope_bytes.push(encoded.len());
    }
    let encoded = codec::encode(&settlement).expect("whole seven-member settlement fits 64 KiB");
    assert!(encoded.len() <= codec::JSON_LIMIT);
    validate_planning_settlement_size(&settlement).unwrap();
    eprintln!("complete seven-member planning audit JSON bytes: envelopes={envelope_bytes:?}, settlement={}, row_limit={}", encoded.len(), codec::JSON_LIMIT);
    assert_eq!(
        codec::decode::<Settlement>(&codec::encode(&settlement).unwrap()).unwrap(),
        settlement
    );
    validate_settlement(&manifest, &envelopes, &settlement).unwrap();
}

#[test]
fn planning_settlement_refuses_a_root_identity_from_another_original_owner() {
    let (manifest, envelopes, mut settlement) = planning_fixture();
    let MemberDisposition::Closed {
        fact: ClosedFact::LegacyV1 { fact },
        ..
    } = &mut settlement.members[0]
    else {
        unreachable!()
    };
    fact.object_identity = "another-original-root".into();
    assert!(validate_settlement(&manifest, &envelopes, &settlement).is_err());
}

#[test]
fn planning_settlement_refuses_actual_close_for_an_unused_engine_file_slot() {
    let (manifest, envelopes, mut settlement) = planning_fixture();
    let MemberDisposition::Closed {
        fact: ClosedFact::FixedPlanningEngineTaskV2 { owners, .. },
        ..
    } = &mut settlement.members[6]
    else {
        unreachable!()
    };
    let MemberPlan::FixedPlanningEngineTaskV1 { task, .. } = &manifest.members[6] else {
        unreachable!()
    };
    owners[2] = FixedEngineOwnerDispositionV1::ActualOwnedClose {
        owner_id: task.file_slots[2].owner_id,
        operation_id: task.file_slots[2].close_operation_id,
    };
    assert!(validate_settlement(&manifest, &envelopes, &settlement).is_err());
}

#[test]
fn planning_settlement_refuses_never_acquired_for_a_used_engine_file_slot() {
    let (manifest, envelopes, mut settlement) = planning_fixture();
    let MemberDisposition::Closed {
        fact: ClosedFact::FixedPlanningEngineTaskV2 { owners, .. },
        ..
    } = &mut settlement.members[6]
    else {
        unreachable!()
    };
    let MemberPlan::FixedPlanningEngineTaskV1 { task, .. } = &manifest.members[6] else {
        unreachable!()
    };
    owners[0] = FixedEngineOwnerDispositionV1::ProvenNeverAcquired {
        owner_id: task.file_slots[0].owner_id,
        operation_id: task.file_slots[0].close_operation_id,
    };
    assert!(validate_settlement(&manifest, &envelopes, &settlement).is_err());
}

#[test]
fn planning_settlement_refuses_a_missing_hash_buffer_close() {
    let (manifest, envelopes, mut settlement) = planning_fixture();
    let MemberDisposition::Closed {
        fact: ClosedFact::FixedPlanningEngineTaskV2 { owners, .. },
        ..
    } = &mut settlement.members[6]
    else {
        unreachable!()
    };
    owners.pop().unwrap();
    assert!(validate_settlement(&manifest, &envelopes, &settlement).is_err());
}

#[test]
fn compact_engine_observation_binds_original_member_task_and_manifest_slot() {
    let (manifest, envelopes, _) = planning_fixture();
    let mut wrong_member = envelopes[6].clone();
    let Some(Observed::FixedPlanningEngineTaskV2 { task }) = &mut wrong_member.observed else {
        unreachable!()
    };
    task.member_id = id();
    assert!(validate_envelope(&manifest, &wrong_member).is_err());

    let mut changed_manifest = manifest.clone();
    let MemberPlan::FixedPlanningEngineTaskV1 { task, .. } = &mut changed_manifest.members[6]
    else {
        unreachable!()
    };
    task.configured_ffmpeg_locator.push_str("-different");
    let mut changed_task = envelopes[6].clone();
    changed_task.planned = changed_manifest.members[6].clone();
    assert!(
        validate_envelope(&changed_manifest, &changed_task).is_err(),
        "the original compact observation cannot alias a changed same-ID task"
    );

    let mut changed_slot = envelopes[6].clone();
    let MemberPlan::FixedPlanningEngineTaskV1 { slot, .. } = &mut changed_slot.planned else {
        unreachable!()
    };
    slot.protected_paths.push("/different-original-slot".into());
    assert!(validate_envelope(&manifest, &changed_slot).is_err());
}

#[test]
fn compact_engine_settlement_refuses_member_plan_digest_and_close_arm_aliases() {
    let (manifest, envelopes, settlement) = planning_fixture();
    for mutation in 0..3 {
        let mut changed = settlement.clone();
        let MemberDisposition::Closed {
            fact:
                ClosedFact::FixedPlanningEngineTaskV2 {
                    member_id,
                    plan_sha256,
                    owners,
                    ..
                },
            ..
        } = &mut changed.members[6]
        else {
            unreachable!()
        };
        match mutation {
            0 => *member_id = id(),
            1 => {
                let replacement = if plan_sha256.starts_with('0') {
                    "1"
                } else {
                    "0"
                };
                plan_sha256.replace_range(..1, replacement);
            }
            _ => {
                let FixedEngineOwnerDispositionV1::ActualOwnedClose { operation_id, .. } =
                    &mut owners[0]
                else {
                    unreachable!()
                };
                *operation_id = id();
            }
        }
        assert!(validate_settlement(&manifest, &envelopes, &changed).is_err());
    }
}

#[test]
fn compact_engine_observation_and_closed_fact_refuse_unknown_fields() {
    let (_, envelopes, settlement) = planning_fixture();
    let mut observed = serde_json::to_value(envelopes[6].observed.as_ref().unwrap()).unwrap();
    observed["task"]["unexpected"] = true.into();
    assert!(serde_json::from_value::<Observed>(observed).is_err());
    let MemberDisposition::Closed { fact, .. } = &settlement.members[6] else {
        unreachable!()
    };
    let mut closed = serde_json::to_value(fact).unwrap();
    closed["unexpected"] = true.into();
    assert!(serde_json::from_value::<ClosedFact>(closed).is_err());
}

#[test]
fn original_engine_v1_observation_and_closed_fact_vocabulary_still_decodes_strictly() {
    let (manifest, _, _) = planning_fixture();
    let MemberPlan::FixedPlanningEngineTaskV1 { task, .. } = &manifest.members[6] else {
        unreachable!()
    };
    let observed = Observed::FixedPlanningEngineTaskV1 {
        task: FixedPlanningEngineTaskObservedV1 {
            task: task.clone(),
            used_file_slots: 2,
            media_digest: "c".repeat(64),
        },
    };
    let json = codec::encode(&observed).expect("single original V1 task remains bounded");
    assert_eq!(codec::decode::<Observed>(&json).unwrap(), observed);
    // Empty owners are a decoding fixture only, never a valid settlement.
    // A complete old duplicated V1 settlement exceeds the unchanged row cap.
    let closed = ClosedFact::FixedPlanningEngineTaskV1 {
        task: task.clone(),
        join_operation_id: id(),
        version_close_operation_id: task.version_close_operation_id,
        dependency_close_operation_id: task.dependency_close_operation_id,
        used_file_slots: 2,
        owners: vec![],
    };
    let json = codec::encode(&closed).expect("original V1 vocabulary remains recognized");
    assert_eq!(codec::decode::<ClosedFact>(&json).unwrap(), closed);
    let mut unknown = serde_json::to_value(&observed).unwrap();
    unknown["task"]["unexpected"] = true.into();
    assert!(serde_json::from_value::<Observed>(unknown).is_err());
}

#[test]
fn planning_settlement_preflight_keeps_the_original_global_json_limit() {
    let (_, _, mut settlement) = planning_fixture();
    let MemberDisposition::Closed {
        fact: ClosedFact::LegacyV1 { fact },
        ..
    } = &mut settlement.members[0]
    else {
        unreachable!()
    };
    fact.evidence = "x".repeat(codec::JSON_LIMIT);
    assert!(validate_planning_settlement_size(&settlement).is_err());
    assert!(codec::encode(&settlement).is_err());
}

/// Audit-only registered-but-never-entered vocabulary. It never constructs
/// a native closure or changes the fixture's synthetic authority status.
fn registered_unentered_planning_fixture() -> (Manifest, Vec<Envelope>, Settlement) {
    let (manifest, mut envelopes, mut settlement) = planning_fixture();
    for envelope in &mut envelopes {
        let member = settlement
            .members
            .iter_mut()
            .find(|member| member.member_id().uuid() == envelope.planned.member_id())
            .unwrap();
        let MemberDisposition::Closed { fact, .. } = member else {
            unreachable!()
        };
        match &envelope.planned {
            MemberPlan::LegacyV1 { planned } => {
                envelope.observed = None;
                *fact = ClosedFact::LegacyNeverAcquiredV1 {
                    planned: planned.clone(),
                    operation_id: settlement.operation_id,
                };
            }
            MemberPlan::FixedProbeDiscoverSourceV1 { source, .. } => {
                let Some(Observed::FixedProbeDiscoverSourceV1 { source: observed }) =
                    &mut envelope.observed
                else {
                    unreachable!()
                };
                observed.copies.truncate(1);
                let ClosedFact::FixedProbeDiscoverSourceV1 { copies, .. } = fact else {
                    unreachable!()
                };
                copies[1] = CopyClosed::ProvenNeverAcquired {
                    copy: source.planned_copies[1].clone(),
                    operation_id: settlement.operation_id,
                };
            }
            MemberPlan::FixedProbeDiscoverTaskV1 { task, .. } => {
                envelope.observed = None;
                *fact = ClosedFact::FixedProbeDiscoverTaskNeverEnteredV1 {
                    task: task.clone(),
                    join_operation_id: id(),
                };
            }
            MemberPlan::FixedPlanningEngineTaskV1 { task, .. } => {
                envelope.observed = None;
                *fact = ClosedFact::FixedPlanningEngineTaskNeverEnteredV1 {
                    member_id: task.member_id,
                    plan_sha256: engine_task_plan_sha256(task).unwrap(),
                    join_operation_id: id(),
                };
            }
            _ => unreachable!("seven P members"),
        }
    }
    (manifest, envelopes, settlement)
}

#[test]
fn registered_unentered_planning_has_distinct_task_facts_and_borrowed_wire_shape() {
    let (manifest, envelopes, settlement) = registered_unentered_planning_fixture();
    for envelope in &envelopes {
        validate_envelope(&manifest, envelope).unwrap();
        let borrowed = EnvelopeRef::from(envelope);
        assert_eq!(
            codec::encode(envelope).unwrap(),
            codec::encode(&borrowed).unwrap()
        );
        validate_envelope_members_ref(&manifest.members, borrowed).unwrap();
    }
    validate_settlement(&manifest, &envelopes, &settlement).unwrap();
    validate_settlement_scope_refs(
        manifest.owner_id,
        manifest.graph_id,
        7,
        envelopes.iter().map(EnvelopeRef::from),
        &settlement,
    )
    .unwrap();
    let encoded = codec::encode(&settlement).unwrap();
    assert!(encoded.contains("fixed_probe_discover_task_never_entered_v1"));
    assert!(encoded.contains("fixed_planning_engine_task_never_entered_v1"));
    assert!(!encoded.contains("fixed_probe_discover_task_never_acquired_v1"));
    assert!(!encoded.contains("fixed_planning_engine_task_never_acquired_v1"));
    assert_eq!(codec::decode::<Settlement>(&encoded).unwrap(), settlement);
    // Unknown enum tags remain a decode error, never an absence fallback.
    let future = encoded.replace(
        "fixed_planning_engine_task_never_entered_v1",
        "fixed_planning_engine_task_never_entered_v2",
    );
    assert!(decode_settlement(&future).is_err());
}

#[test]
fn registered_unentered_planning_refuses_observed_task_wrong_roots_and_shared_join() {
    let (manifest, envelopes, settlement) = registered_unentered_planning_fixture();
    let (original_manifest, acquired, _) = planning_fixture();
    let mut observed_task = envelopes.clone();
    let index = observed_task
        .iter()
        .position(|envelope| {
            matches!(
                envelope.planned,
                MemberPlan::FixedProbeDiscoverTaskV1 { .. }
            )
        })
        .unwrap();
    // Use a typed observation of the same exact plan; its presence alone
    // contradicts NeverEntered, irrespective of synthetic inventory values.
    let MemberPlan::FixedProbeDiscoverTaskV1 { task, .. } = &observed_task[index].planned else {
        unreachable!()
    };
    let mut observed = acquired
        .iter()
        .find_map(|envelope| match &envelope.observed {
            Some(Observed::FixedProbeDiscoverTaskV1 { task }) => Some(task.clone()),
            _ => None,
        })
        .unwrap();
    observed.task = task.clone();
    observed_task[index].observed = Some(Observed::FixedProbeDiscoverTaskV1 { task: observed });
    assert!(validate_settlement(&manifest, &observed_task, &settlement).is_err());
    let _ = original_manifest;
    let mut wrong_root = settlement.clone();
    let MemberDisposition::Closed {
        fact: ClosedFact::LegacyNeverAcquiredV1 { operation_id, .. },
        ..
    } = &mut wrong_root.members[0]
    else {
        unreachable!()
    };
    *operation_id = id();
    assert!(validate_settlement(&manifest, &envelopes, &wrong_root).is_err());
    let mut shared_join = settlement.clone();
    let join = shared_join
        .members
        .iter()
        .find_map(|member| match member {
            MemberDisposition::Closed {
                fact:
                    ClosedFact::FixedProbeDiscoverTaskNeverEnteredV1 {
                        join_operation_id, ..
                    },
                ..
            } => Some(*join_operation_id),
            _ => None,
        })
        .unwrap();
    for member in &mut shared_join.members {
        if let MemberDisposition::Closed {
            fact:
                ClosedFact::FixedPlanningEngineTaskNeverEnteredV1 {
                    join_operation_id, ..
                },
            ..
        } = member
        {
            *join_operation_id = join;
        }
    }
    assert!(validate_settlement(&manifest, &envelopes, &shared_join).is_err());
}

#[test]
fn registered_unentered_planning_refuses_child_handoff_and_engine_plan_substitution() {
    let (manifest, envelopes, settlement) = registered_unentered_planning_fixture();
    let mut acquired_child = envelopes.clone();
    for envelope in &mut acquired_child {
        if let (
            MemberPlan::FixedProbeDiscoverSourceV1 { source, .. },
            Some(Observed::FixedProbeDiscoverSourceV1 { source: observed }),
        ) = (&envelope.planned, &mut envelope.observed)
        {
            observed.copies = source.planned_copies.clone();
        }
    }
    assert!(validate_settlement(&manifest, &acquired_child, &settlement).is_err());
    let mut wrong_digest = settlement.clone();
    for member in &mut wrong_digest.members {
        if let MemberDisposition::Closed {
            fact: ClosedFact::FixedPlanningEngineTaskNeverEnteredV1 { plan_sha256, .. },
            ..
        } = member
        {
            *plan_sha256 = "0".repeat(64);
        }
    }
    assert!(validate_settlement(&manifest, &envelopes, &wrong_digest).is_err());
    let mut unknown_field = serde_json::to_value(&settlement).unwrap();
    let fact = unknown_field["members"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find_map(|member| {
            let fact = member.get_mut("fact")?;
            (fact.get("kind")?.as_str()? == "fixed_probe_discover_task_never_entered_v1")
                .then_some(fact)
        })
        .expect("strict registered task tag");
    fact.as_object_mut()
        .unwrap()
        .insert("native_digest".into(), "invented".into());
    assert!(codec::decode::<Settlement>(&unknown_field.to_string()).is_err());
}
