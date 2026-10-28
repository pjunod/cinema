//! Positive fixed FFmpeg planning completion from the original P collector.
//! Report data can move to M; this separate capability retains the actual
//! original child, pipe-drain, object-File and hash-buffer close records.
use std::io;
use std::path::PathBuf;
use uuid::Uuid;

use crate::process::fixed_planning_engine::{
    FixedEngineFileClosed, FixedEngineHashBufferClosed, FixedEngineNativeCompletion,
    FixedEngineRootClosed, FixedEngineUnusedSlots,
};
use crate::process::observation::ProcessIdentity;
use crate::resource_custody::codec_v2 as v2;

pub(in crate::adaptive_resources) struct FixedEngineReportData {
    pub producer_path: PathBuf,
    pub producer_object_version: String,
    pub producer_sha256: String,
    pub dependencies: Vec<(PathBuf, String)>,
    pub media_digest: String,
    pub build_line: String,
    pub boot_id: Option<Uuid>,
}

#[must_use]
pub(in crate::adaptive_resources) struct FixedEngineNativeClosure {
    task: v2::FixedPlanningEngineTaskV1,
    media_digest: String,
    version_root: FixedEngineRootClosed,
    dependency_root: FixedEngineRootClosed,
    files: Vec<FixedEngineFileClosed>,
    unused_slots: FixedEngineUnusedSlots,
    hash_buffer: FixedEngineHashBufferClosed,
}

pub(in crate::adaptive_resources) struct FixedEngineKnownClosed {
    report: FixedEngineReportData,
    closure: FixedEngineNativeClosure,
}

impl FixedEngineKnownClosed {
    pub(in crate::adaptive_resources) fn from_joined_original(
        native: FixedEngineNativeCompletion,
        planned: v2::FixedPlanningEngineTaskV1,
    ) -> io::Result<Self> {
        let Some(producer_slot) = planned.file_slots.first() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "engine task has no prearmed producer File slot",
            ));
        };
        let mut dependency_slots = planned.file_slots.iter().skip(1);
        let exact_file_slots = native.closed_files.iter().all(|closed| {
            let slot = if closed.path() == native.producer_path {
                producer_slot
            } else {
                let Some(slot) = dependency_slots.next() else {
                    return false;
                };
                slot
            };
            closed.owner_id() == slot.owner_id.uuid()
                && closed.close_operation_id() == slot.close_operation_id.uuid()
        });
        if !native.version_root.verified()
            || !native.dependency_root.verified()
            || native.version_root.close_operation_id() != planned.version_close_operation_id.uuid()
            || native.dependency_root.close_operation_id()
                != planned.dependency_close_operation_id.uuid()
            || native.closed_files.is_empty()
            || native.closed_files.len() > planned.file_slots.len()
            || native
                .closed_files
                .len()
                .checked_add(usize::from(native.unused_slots.count()))
                != Some(planned.file_slots.len())
            || native.version_root.inherited_producer_file_owner_id()
                != Some(producer_slot.owner_id.uuid())
            || native.dependency_root.inherited_producer_file_owner_id()
                != Some(producer_slot.owner_id.uuid())
            || !native.hash_buffer.verified()
            || native.hash_buffer.owner_id() != planned.hash_buffer_owner_id.uuid()
            || native.hash_buffer.close_operation_id()
                != planned.hash_buffer_close_operation_id.uuid()
            || ![
                (
                    native.version_root.pipe_closures().0,
                    planned.version_stdout_owner_id,
                    planned.version_stdout_close_operation_id,
                ),
                (
                    native.version_root.pipe_closures().1,
                    planned.version_diagnostic_owner_id,
                    planned.version_diagnostic_close_operation_id,
                ),
                (
                    native.dependency_root.pipe_closures().0,
                    planned.dependency_stdout_owner_id,
                    planned.dependency_stdout_close_operation_id,
                ),
                (
                    native.dependency_root.pipe_closures().1,
                    planned.dependency_diagnostic_owner_id,
                    planned.dependency_diagnostic_close_operation_id,
                ),
            ]
            .into_iter()
            .all(|(closed, owner, operation)| {
                closed.owner_id() == owner.uuid() && closed.close_operation_id() == operation.uuid()
            })
            || native.dependencies.len() != native.closed_files.len()
            || !native.dependencies.iter().zip(&native.closed_files).all(
                |((path, version), closed)| {
                    path == closed.path() && version == closed.object_version()
                },
            )
            || !native
                .closed_files
                .windows(2)
                .all(|pair| pair[0].path() < pair[1].path())
            || !exact_file_slots
            || native
                .closed_files
                .iter()
                .filter(|closed| closed.path() == native.producer_path)
                .count()
                != 1
            || !native.closed_files.iter().any(|closed| {
                closed.path() == native.producer_path
                    && format!("s1:{}", closed.object_version()) == native.producer_object_version
                    && hex::encode(closed.sha256()) == native.producer_sha256
            })
            || native.media_digest.len() != 64
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "engine completion is not bound to original P task owners",
            ));
        }
        let media_digest = native.media_digest;
        Ok(Self {
            report: FixedEngineReportData {
                producer_path: native.producer_path,
                producer_object_version: native.producer_object_version,
                producer_sha256: native.producer_sha256,
                dependencies: native.dependencies,
                media_digest: media_digest.clone(),
                build_line: native.build_line,
                boot_id: native.boot_id,
            },
            closure: FixedEngineNativeClosure {
                task: planned,
                media_digest,
                version_root: native.version_root,
                dependency_root: native.dependency_root,
                files: native.closed_files,
                unused_slots: native.unused_slots,
                hash_buffer: native.hash_buffer,
            },
        })
    }

    pub(in crate::adaptive_resources) fn split(
        self,
    ) -> (FixedEngineReportData, FixedEngineNativeClosure) {
        (self.report, self.closure)
    }
}

impl FixedEngineNativeClosure {
    pub(in crate::adaptive_resources) fn version_member(&self) -> Uuid {
        self.task.version_root_member_id.uuid()
    }
    pub(in crate::adaptive_resources) fn dependency_member(&self) -> Uuid {
        self.task.dependency_root_member_id.uuid()
    }
    pub(in crate::adaptive_resources) fn version_identity(&self) -> ProcessIdentity {
        self.version_root.identity()
    }
    pub(in crate::adaptive_resources) fn dependency_identity(&self) -> ProcessIdentity {
        self.dependency_root.identity()
    }
    pub(in crate::adaptive_resources) fn used_file_slots(&self) -> usize {
        self.files.len()
    }
    pub(in crate::adaptive_resources) fn media_digest(&self) -> &str {
        &self.media_digest
    }
    pub(in crate::adaptive_resources) fn task(&self) -> &v2::FixedPlanningEngineTaskV1 {
        &self.task
    }

    /// P's engine-task closed fact. Every owner/close ID comes from a token
    /// bound when the original owner was opened (files, pipes, hash buffer),
    /// never from the caller. The never-opened suffix is attested by the
    /// collector's unused-slot token.
    pub(in crate::adaptive_resources) fn closed_fact(
        &self,
        join_operation_id: v2::Id,
    ) -> io::Result<v2::ClosedFact> {
        let invalid = || {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "engine closure cannot prove its planned owners",
            )
        };
        let task = &self.task;
        let used = self.files.len();
        let unused = usize::from(self.unused_slots.count());
        if used == 0
            || used.checked_add(unused) != Some(task.file_slots.len())
            || !self.version_root.verified()
            || !self.dependency_root.verified()
            || !self.hash_buffer.verified()
            || join_operation_id.uuid().is_nil()
        {
            return Err(invalid());
        }
        let id = |value: Uuid| v2::Id::new(value).map_err(|_| invalid());
        let mut owners = Vec::with_capacity(task.file_slots.len() + 5);
        for file in &self.files {
            if !task.file_slots[..used].iter().any(|slot| {
                slot.owner_id.uuid() == file.owner_id()
                    && slot.close_operation_id.uuid() == file.close_operation_id()
            }) {
                return Err(invalid());
            }
            owners.push(v2::FixedEngineOwnerDispositionV1::ActualOwnedClose {
                owner_id: id(file.owner_id())?,
                operation_id: id(file.close_operation_id())?,
            });
        }
        for slot in &task.file_slots[used..] {
            owners.push(v2::FixedEngineOwnerDispositionV1::ProvenNeverAcquired {
                owner_id: slot.owner_id,
                operation_id: slot.close_operation_id,
            });
        }
        for root in [&self.version_root, &self.dependency_root] {
            let (stdout, stderr) = root.pipe_closures();
            for pipe in [stdout, stderr] {
                if !pipe.verified() {
                    return Err(invalid());
                }
                owners.push(v2::FixedEngineOwnerDispositionV1::ActualOwnedClose {
                    owner_id: id(pipe.owner_id())?,
                    operation_id: id(pipe.close_operation_id())?,
                });
            }
        }
        owners.push(v2::FixedEngineOwnerDispositionV1::ActualOwnedClose {
            owner_id: id(self.hash_buffer.owner_id())?,
            operation_id: id(self.hash_buffer.close_operation_id())?,
        });
        Ok(v2::ClosedFact::FixedPlanningEngineTaskV2 {
            member_id: task.member_id,
            plan_sha256: v2::engine_task_plan_sha256(task).map_err(|_| invalid())?,
            join_operation_id,
            version_close_operation_id: id(self.version_root.close_operation_id())?,
            dependency_close_operation_id: id(self.dependency_root.close_operation_id())?,
            used_file_slots: u16::try_from(used).map_err(|_| invalid())?,
            owners,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::fixed_planning_engine::token_tests::{
        synthetic_completion, SyntheticFault,
    };

    // These exercise synthetic private-token invariants only. They do not
    // execute the native collector or establish real process/file closure.
    #[test]
    fn synthetic_engine_closed_fact_counts_every_used_unused_pipe_and_buffer_owner() {
        for used in [1, 2, v2::FIXED_PLANNING_ENGINE_MAX_OBJECTS] {
            let (native, planned) = synthetic_completion(used, SyntheticFault::None);
            let known = FixedEngineKnownClosed::from_joined_original(native, planned.clone())
                .expect("matching synthetic original-owner tokens");
            let (_, closure) = known.split();
            let join = v2::Id::new(Uuid::new_v4()).unwrap();
            let v2::ClosedFact::FixedPlanningEngineTaskV2 {
                member_id,
                plan_sha256,
                join_operation_id,
                used_file_slots,
                owners,
                ..
            } = closure
                .closed_fact(join)
                .expect("synthetic token closure audit")
            else {
                unreachable!()
            };
            assert_eq!(member_id, planned.member_id);
            assert_eq!(plan_sha256, v2::engine_task_plan_sha256(&planned).unwrap());
            assert_eq!(join_operation_id, join);
            assert_eq!(usize::from(used_file_slots), used);
            assert_eq!(owners.len(), planned.file_slots.len() + 5);
            for (index, slot) in planned.file_slots.iter().enumerate() {
                let expected = if index < used {
                    v2::FixedEngineOwnerDispositionV1::ActualOwnedClose {
                        owner_id: slot.owner_id,
                        operation_id: slot.close_operation_id,
                    }
                } else {
                    v2::FixedEngineOwnerDispositionV1::ProvenNeverAcquired {
                        owner_id: slot.owner_id,
                        operation_id: slot.close_operation_id,
                    }
                };
                assert_eq!(owners[index], expected);
            }
            for (owner_id, operation_id) in [
                (
                    planned.version_stdout_owner_id,
                    planned.version_stdout_close_operation_id,
                ),
                (
                    planned.version_diagnostic_owner_id,
                    planned.version_diagnostic_close_operation_id,
                ),
                (
                    planned.dependency_stdout_owner_id,
                    planned.dependency_stdout_close_operation_id,
                ),
                (
                    planned.dependency_diagnostic_owner_id,
                    planned.dependency_diagnostic_close_operation_id,
                ),
                (
                    planned.hash_buffer_owner_id,
                    planned.hash_buffer_close_operation_id,
                ),
            ] {
                assert!(owners[planned.file_slots.len()..].contains(
                    &v2::FixedEngineOwnerDispositionV1::ActualOwnedClose {
                        owner_id,
                        operation_id
                    }
                ));
            }
        }
    }

    #[test]
    fn synthetic_engine_completion_refuses_invalid_verified_tokens_and_slot_attestation() {
        for fault in [
            SyntheticFault::UnusedCount,
            SyntheticFault::RootIdentity,
            SyntheticFault::PipeOverbound,
            SyntheticFault::HashBufferOverbound,
            SyntheticFault::FileOwner,
        ] {
            let (native, planned) = synthetic_completion(2, fault);
            assert!(FixedEngineKnownClosed::from_joined_original(native, planned).is_err());
        }
    }

    #[test]
    fn synthetic_engine_closed_fact_refuses_a_changed_planned_slot_count() {
        let (native, planned) = synthetic_completion(2, SyntheticFault::None);
        let (_, mut closure) = FixedEngineKnownClosed::from_joined_original(native, planned)
            .expect("matching synthetic original-owner tokens")
            .split();
        closure.task.file_slots.pop();
        assert!(closure
            .closed_fact(v2::Id::new(Uuid::new_v4()).unwrap())
            .is_err());
    }
}
