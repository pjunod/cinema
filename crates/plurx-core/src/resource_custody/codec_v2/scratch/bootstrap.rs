//! Additive configured-root audit shapes. No serialized value authorizes IO.
//! Historical ScratchAssetsV2 and SQL-v2 bytes remain unchanged.
use super::*;
mod fold;
mod snapshot;
pub(crate) use fold::BootstrapFold;
pub(crate) use snapshot::{
    frozen_snapshot_ref, validate_frozen_snapshot, validate_frozen_snapshot_ref,
    validate_frozen_snapshot_ref_shape, FrozenScratchRefV1, FrozenScratchV3,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ScratchPlanV3 {
    pub version: u32,
    pub asset_id: Id,
    pub rendition_id: String,
    pub generation_epoch: u64,
    pub filesystem_domain: FilesystemDomain,
    pub directory: DirectoryOriginV1,
    pub outputs: OutputNamesV1,
    pub limits: ScratchLimitsV1,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
pub(crate) enum DirectoryOriginV1 {
    ConfiguredRootBootstrapV1 { bootstrap: RootBootstrapPlanV1 },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RootBootstrapPlanV1 {
    pub version: u32,
    pub authority: ConfiguredRootAuthorityV1,
    pub components: Vec<String>,
    pub child_component: String,
    pub protected_child_path: String,
    pub bounds: BootstrapBoundsV1,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RootPurpose {
    RenditionCacheV1,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RootTraversal {
    NamespaceRootToConfiguredRootV1,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DescendantMounts {
    RefuseV1,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConfiguredRootAuthorityV1 {
    pub installation_id: Id,
    pub purpose: RootPurpose,
    pub resolved_absolute_root: String,
    pub traversal: RootTraversal,
    pub descendant_mounts: DescendantMounts,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BootstrapBoundsV1 {
    pub max_components: u16,
    pub max_path_bytes: u32,
    pub max_component_bytes: u16,
    pub peak_directory_handles: u16,
    pub pending_native_jobs: u16,
    pub resident_memory_bytes: u64,
    pub child_directory_reserve_bytes: u64,
    pub parent_entry_reserve_bytes: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BootstrapPhase {
    Unstarted,
    Traversing,
    ParentReady,
    ChildPossible,
    SinkReady,
    Closing,
    Closed,
    UnknownHeld,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BootstrapDirectoryKind {
    NamespaceAnchorV1,
    InfrastructureTraversalV1,
    ConfiguredParentV1,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BootstrapDirectoryV1 {
    pub observed: DirectoryObservedV1,
    pub kind: BootstrapDirectoryKind,
    pub component_index: Option<u16>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChildEffect {
    None,
    Unchanged,
    Created,
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InventoryCertainty {
    Exact,
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PossibleChildV1 {
    pub create_io_id: Id,
    pub parent: OwnedDirectoryRefV1,
    pub child_component: String,
    pub namespace_effect: ChildEffect,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BootstrapStateV1 {
    pub phase: BootstrapPhase,
    pub next_component: u16,
    pub current: Option<BootstrapDirectoryV1>,
    pub parent: Option<BootstrapDirectoryV1>,
    pub possible_child: Option<PossibleChildV1>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ScratchStateV3 {
    pub version: u32,
    pub asset: ScratchStateV2,
    pub bootstrap: BootstrapStateV1,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
pub(crate) enum BootstrapIoV1 {
    OpenNamespaceRoot {
        copy_id: Id,
    },
    OpenNextComponent {
        index: u16,
        parent: OwnedDirectoryRefV1,
        copy_id: Id,
    },
    CreateRenditionChild {
        parent: OwnedDirectoryRefV1,
        copy_id: Id,
    },
    CloseBootstrapHandles {
        copy_ids: Vec<Id>,
    },
    ResolvePossibleChild {
        create_io_id: Id,
        parent: OwnedDirectoryRefV1,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BootstrapResultV1 {
    pub outcome: NativeOutcomeV1,
    pub acquired: Option<DirectoryObservedV1>,
    pub closed_copy_ids: Vec<Id>,
    pub child_effect: ChildEffect,
    pub inventory: InventoryCertainty,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
pub(crate) enum AssetEventV3 {
    ArmBootstrapV1 {
        io_id: Id,
        operation: BootstrapIoV1,
    },
    ObserveBootstrapV1 {
        io_id: Id,
        native_join_operation_id: Id,
        result: BootstrapResultV1,
    },
    OutputV2 {
        event: AssetEventV1,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AssetChangeV3 {
    pub scratch_member_id: Id,
    pub asset_id: Id,
    pub generation_epoch: u64,
    pub previous: Option<AssetEventRefV1>,
    pub event: AssetEventV3,
}
impl ScratchPlanV3 {
    pub(crate) fn bootstrap(&self) -> &RootBootstrapPlanV1 {
        let DirectoryOriginV1::ConfiguredRootBootstrapV1 { bootstrap } = &self.directory;
        bootstrap
    }
    pub(super) fn output_view(&self) -> OutputPlanView<'_> {
        OutputPlanView {
            asset_id: self.asset_id,
            generation_epoch: self.generation_epoch,
            filesystem_domain: &self.filesystem_domain,
            outputs: &self.outputs,
            limits: &self.limits,
            directory_mode: None,
        }
    }
}

pub(crate) fn validate_plan(plan: &ScratchPlanV3) -> Result<(), CustodyError> {
    codec::text(&plan.rendition_id)?;
    plan.filesystem_domain.validate()?;
    validate_outputs_limits(plan.generation_epoch, &plan.outputs, &plan.limits)?;
    let root = plan.bootstrap();
    let b = &root.bounds;
    let l = &plan.limits;
    if plan.version != 3 || root.version != 1 || root.components.is_empty()
        || root.components.len() > usize::from(b.max_components)
        || b.max_components == 0 || b.max_components > 64
        || b.max_path_bytes == 0 || b.max_path_bytes > 4096
        || b.max_component_bytes == 0 || b.max_component_bytes > 255
        || b.peak_directory_handles != 2 || b.pending_native_jobs != 1
        || b.resident_memory_bytes == 0 || b.resident_memory_bytes > l.resident_memory_bytes
        || b.child_directory_reserve_bytes == 0 || b.parent_entry_reserve_bytes == 0
        || b.child_directory_reserve_bytes < l.directory_charge()
        // Parent+child+one publication fd; source/process/reader partitions are
        // checked by the runtime against the SAME total admitted reservation.
        || l.live_handles < 3
        || root.authority.resolved_absolute_root.len() > b.max_path_bytes as usize
        || root.protected_child_path.len() > b.max_path_bytes as usize
        || root.child_component.len() > usize::from(b.max_component_bytes)
    {
        return Err(codec::invalid());
    }
    component(&root.child_component)?;
    // Validate raw spelling before joining; Path::components normalizes away
    // repeated separators and '.', which must not acquire traversal authority.
    let mut length = 0usize;
    for name in &root.components {
        component(name)?;
        if name.len() > usize::from(b.max_component_bytes) {
            return Err(codec::invalid());
        }
        length = length
            .checked_add(1)
            .and_then(|n| n.checked_add(name.len()))
            .ok_or_else(codec::invalid)?;
    }
    if length > b.max_path_bytes as usize {
        return Err(codec::invalid());
    }
    let rebuilt = format!("/{}", root.components.join("/"));
    if root.authority.resolved_absolute_root != rebuilt
        || root.protected_child_path != format!("{rebuilt}/{}", root.child_component)
        || b.child_directory_reserve_bytes
            .checked_add(b.parent_entry_reserve_bytes)
            .and_then(|n| n.checked_add(l.storage_for(l.one_file_logical_bytes).ok()?))
            .is_none_or(|n| n > l.reserved_storage_bytes)
    {
        return Err(codec::invalid());
    }
    codec::encode(plan)?;
    Ok(())
}

/// Only the bounded projection shape, never evidence that native work occurred.
/// Journal restore must replay the complete lineage and compare the entire state.
pub(crate) fn validate_projection(
    plan: &ScratchPlanV3,
    state: &ScratchStateV3,
) -> Result<(), CustodyError> {
    if state.version != 3
        || usize::from(state.bootstrap.next_component) > plan.bootstrap().components.len()
    {
        return Err(codec::invalid());
    }
    validate_output_projection(plan.output_view(), &state.asset)?;
    codec::encode(state)?;
    Ok(())
}
