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
                            | crate::resource_custody::codec_v2::MemberPlan::VodOutputTaskV1 {
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
        let Ok(book) = self.operations.lock() else {
            return true;
        };
        for bundle in book.bundles.values() {
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
