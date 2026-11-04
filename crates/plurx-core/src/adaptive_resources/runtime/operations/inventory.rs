//! Classify actual physical plans, never their video/audio/companion relation.
use super::*;
use crate::adaptive_resources::owner_inventory::{OwnerFamily, OwnerKind};

pub(super) fn classification(
    members: Option<&[v2::MemberPlan]>,
) -> Option<(OwnerFamily, OwnerKind)> {
    let members = members?;
    if members
        .iter()
        .any(|m| matches!(m, v2::MemberPlan::VodOutputTaskV1 { .. }))
    {
        Some((OwnerFamily::BufferedVod, OwnerKind::Producer))
    } else if members.iter().any(|m| {
        matches!(
            m,
            v2::MemberPlan::FixedProbeTaskV1 { .. }
                | v2::MemberPlan::FixedProbeDiscoverTaskV1 { .. }
                | v2::MemberPlan::FixedPlanningEngineTaskV1 { .. }
        )
    }) {
        Some((OwnerFamily::Planning, OwnerKind::Producer))
    } else if matches!(members, [v2::MemberPlan::FixedProbeMetadataV1 { .. }]) {
        Some((OwnerFamily::Planning, OwnerKind::Residency))
    } else if matches!(members, [v2::MemberPlan::DirectReadDomainV1 { .. }]) {
        Some((OwnerFamily::SourceReader, OwnerKind::Producer))
    } else if matches!(members, [v2::MemberPlan::DirectSourceV1 { .. }]) {
        Some((OwnerFamily::SourceReader, OwnerKind::Residency))
    } else {
        None
    }
}
