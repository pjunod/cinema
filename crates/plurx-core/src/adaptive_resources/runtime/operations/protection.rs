//! Namespace exclusion for unowned legacy recursive cleanup.
use super::*;
impl NodeResourceRuntime {
    /// Negative namespace guard for legacy cache sweepers. A pathname never
    /// grants cleanup authority: live/uncertain custody excludes recursive
    /// deletion, and only the actual finite owner may remove its own objects.
    /// Poisoned or busy ownership is conservatively protected until the next
    /// existing maintenance pass; no separate retry task is created.
    pub fn protects_output_path(&self, path: &Path) -> bool {
        fn overlaps(candidate: &Path, protected: &str) -> bool {
            let protected = Path::new(protected);
            candidate.starts_with(protected) || protected.starts_with(candidate)
        }
        let Ok(state) = self.ledger.inner.lock() else {
            return true;
        };
        for record in &self.recovered.obligations {
            let Some(binding) = self.recovered_bindings.get(&record.obligation_id) else {
                return true;
            };
            if !state
                .entries
                .get(&binding.reservation)
                .is_some_and(|entry| entry.binding == *binding)
            {
                continue;
            }
            match &record.inventory {
                crate::resource_custody::ObligationInventory::LegacyV1 { manifest, .. } => {
                    if manifest.members.iter().any(|member| {
                        member
                            .protected_paths
                            .iter()
                            .any(|protected| overlaps(path, protected))
                    }) {
                        return true;
                    }
                }
                crate::resource_custody::ObligationInventory::TypedV2 { manifest, .. } => {
                    for member in &manifest.members {
                        let paths = match member {
                            crate::resource_custody::codec_v2::MemberPlan::LegacyV1 { planned } => {
                                &planned.protected_paths
                            }
                            crate::resource_custody::codec_v2::MemberPlan::SourceV1 {
                                slot,
                                ..
                            }
                            | crate::resource_custody::codec_v2::MemberPlan::DirectSourceV1 {
                                slot,
                                ..
                            }
                            | crate::resource_custody::codec_v2::MemberPlan::FixedProbeDiscoverSourceV1 {
                                slot,
                                ..
                            }
                            | crate::resource_custody::codec_v2::MemberPlan::VodOutputTaskV1 {
                                slot,
                                ..
                            }
                            | crate::resource_custody::codec_v2::MemberPlan::FixedProbeTaskV1 {
                                slot,
                                ..
                            }
                            | crate::resource_custody::codec_v2::MemberPlan::FixedProbeDiscoverTaskV1 {
                                slot,
                                ..
                            }
                            | crate::resource_custody::codec_v2::MemberPlan::FixedPlanningEngineTaskV1 {
                                slot,
                                ..
                            }
                            | crate::resource_custody::codec_v2::MemberPlan::FixedProbeMetadataV1 {
                                slot,
                                ..
                            }
                            | crate::resource_custody::codec_v2::MemberPlan::ScratchAssetsV1 {
                                slot,
                                ..
                            }
                            | crate::resource_custody::codec_v2::MemberPlan::ScratchAssetsV2 {
                                slot,
                                ..
                            }
                            | crate::resource_custody::codec_v2::MemberPlan::ScratchAssetsV3 {
                                slot,
                                ..
                            } => &slot.protected_paths,
                            crate::resource_custody::codec_v2::MemberPlan::DirectReadDomainV1 {
                                ..
                            } => continue,
                        };
                        if paths.iter().any(|protected| overlaps(path, protected)) {
                            return true;
                        }
                    }
                }
            }
        }
        drop(state);
        let Ok(mut book) = self.operations.lock() else {
            return true;
        };
        let book = &mut *book;
        book.protections
            .retain(|id, _| book.bundles.contains_key(id));
        for (id, bundle) in &book.bundles {
            // A role's recorded child paths answer without the bundle lock and
            // stay until its refund, through close. Only a bundle with no
            // record falls back to reading its roles, conservatively.
            if let Some(records) = book.protections.get(id) {
                if records.iter().any(|record| record.protects(path, overlaps)) {
                    return true;
                }
                continue;
            }
            let Ok(bundle) = bundle.try_lock() else {
                return true;
            };
            for role in &bundle.roles {
                if role.refunded {
                    continue;
                }
                let Some(members) = &role.typed_members else {
                    return true;
                };
                for member in members {
                    if let crate::resource_custody::codec_v2::MemberPlan::ScratchAssetsV3 {
                        plan,
                        ..
                    } = member
                    {
                        if overlaps(path, &plan.bootstrap().protected_child_path) {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }
}

/// The protected child paths of one role, recorded when the role registers
/// and kept until its refund. Read without the bundle lock: a busy bundle no
/// longer protects every unrelated path, and a closed role whose plan was
/// dropped still protects its own. An unknown plan or a poisoned record
/// protects everything.
#[derive(Debug)]
pub(super) struct RoleProtection {
    paths: std::sync::Mutex<Option<Vec<String>>>,
    refunded: std::sync::atomic::AtomicBool,
}

impl RoleProtection {
    pub(super) fn of(
        members: Option<&[crate::resource_custody::codec_v2::MemberPlan]>,
    ) -> Arc<Self> {
        Self::new(members.map(scratch_child_paths))
    }

    /// `None` is an unknown plan: it protects every path.
    fn new(paths: Option<Vec<String>>) -> Arc<Self> {
        Arc::new(Self {
            paths: std::sync::Mutex::new(paths),
            refunded: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// A role that takes a new plan keeps protecting what it held before.
    pub(super) fn extend(&self, members: &[crate::resource_custody::codec_v2::MemberPlan]) {
        self.extend_paths(scratch_child_paths(members));
    }

    fn extend_paths(&self, added: Vec<String>) {
        let Ok(mut paths) = self.paths.lock() else {
            return;
        };
        if let Some(paths) = paths.as_mut() {
            for path in added {
                if !paths.contains(&path) {
                    paths.push(path);
                }
            }
        }
    }

    pub(super) fn refund(&self) {
        self.refunded
            .store(true, std::sync::atomic::Ordering::Release);
    }

    fn protects(&self, path: &Path, overlaps: fn(&Path, &str) -> bool) -> bool {
        if self.refunded.load(std::sync::atomic::Ordering::Acquire) {
            return false;
        }
        let Ok(paths) = self.paths.lock() else {
            return true;
        };
        match paths.as_ref() {
            Some(paths) => paths.iter().any(|protected| overlaps(path, protected)),
            None => true,
        }
    }
}

fn scratch_child_paths(members: &[crate::resource_custody::codec_v2::MemberPlan]) -> Vec<String> {
    members
        .iter()
        .filter_map(|member| match member {
            crate::resource_custody::codec_v2::MemberPlan::ScratchAssetsV3 { plan, .. } => {
                Some(plan.bootstrap().protected_child_path.clone())
            }
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod role_protection_tests {
    use super::*;

    fn overlaps(candidate: &Path, protected: &str) -> bool {
        let protected = Path::new(protected);
        candidate.starts_with(protected) || protected.starts_with(candidate)
    }

    #[test]
    fn a_recorded_role_protects_only_its_own_child_paths_until_refund() {
        let role = RoleProtection::new(Some(vec!["/cache/renditions/owned".into()]));
        // Read without any bundle lock: an unrelated legacy path is free
        // while the bundle may be busy, and the role's own path, its parent
        // and its contents stay protected through close.
        assert!(!role.protects(Path::new("/cache/renditions/legacy-copy"), overlaps));
        assert!(role.protects(Path::new("/cache/renditions/owned"), overlaps));
        assert!(role.protects(Path::new("/cache/renditions/owned/seg00001.m4s"), overlaps));
        assert!(role.protects(Path::new("/cache/renditions"), overlaps));
        role.extend_paths(vec!["/cache/renditions/retained".into()]);
        assert!(role.protects(Path::new("/cache/renditions/owned"), overlaps));
        assert!(role.protects(Path::new("/cache/renditions/retained"), overlaps));
        role.refund();
        assert!(!role.protects(Path::new("/cache/renditions/owned"), overlaps));
    }

    #[test]
    fn an_unknown_or_unreadable_record_protects_every_path() {
        let unknown = RoleProtection::new(None);
        assert!(unknown.protects(Path::new("/anything"), overlaps));
        let poisoned = RoleProtection::new(Some(Vec::new()));
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = poisoned.paths.lock().expect("fresh record");
            panic!("poison the record");
        }));
        assert!(poisoned.protects(Path::new("/anything"), overlaps));
    }
}
