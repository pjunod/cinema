//! SDK-bearing implementation. The native daemon does not link this module.
use super::*;
use crate::cluster::migration::{self, PendingSelectedStore};
use crate::secrets::{CredentialKey, SealedRowCensus};
use crate::storage::transition::{NativeHandoff, VerifiedArtifact};
use crate::storage::NativeStoreHandle;

#[derive(Clone)]
struct KeyPublication {
    root: PathBuf,
    request: PreparedRequest,
    key_path: PathBuf,
    original: Option<FileBinding>,
}

tokio::task_local! {
    static KEY_PUBLICATION: KeyPublication;
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyIntent {
    request_sha256: String,
    staged_name: String,
    binding: FileBinding,
}

/// Only the actual migration opener calls this, after its authoritative census.
/// No caller can provide a census through the public helper API.
pub(crate) fn open_actual_census_key(
    path: &Path,
    census: &SealedRowCensus,
) -> Result<CredentialKey, StoreError> {
    let context = KEY_PUBLICATION.try_with(Clone::clone).ok();
    let Some(context) = context else {
        return crate::secrets::open_credential_key(path, census)
            .map_err(|error| StoreError::Identity(error.to_string()));
    };
    if path_hash(path)? != path_hash(&context.key_path)? {
        return Err(refused("actual census used a different key path"));
    }
    if let Some(original) = &context.original {
        if file_binding(path)?.as_ref() != Some(original) {
            return Err(refused("existing credential key changed"));
        }
        return crate::secrets::open_credential_key(path, census)
            .map_err(|error| StoreError::Identity(error.to_string()));
    }
    let intent_path = operation_path(&context.root, &context.request.id, "key.json")?;
    let parent = path.parent().ok_or_else(|| refused("key has no parent"))?;
    let intent: KeyIntent = match read_record(&intent_path)? {
        Some(intent) => intent,
        None => {
            if file_binding(path)?.is_some() {
                return Err(refused("an unbound credential key appeared"));
            }
            // This is the actual census, not a helper-created empty substitute.
            // The established key opener refuses missing keys with sealed rows.
            let staged_name = format!(".plurx-key-{}-{}", context.request.id, uuid::Uuid::new_v4());
            let staged = parent.join(&staged_name);
            let staged_key = crate::secrets::open_credential_key(&staged, census)
                .map_err(|error| StoreError::Identity(error.to_string()))?;
            drop(staged_key);
            let binding = file_binding(&staged)?
                .ok_or_else(|| refused("staged credential key disappeared"))?;
            let intent = KeyIntent {
                request_sha256: context.request.sha256.clone(),
                staged_name,
                binding,
            };
            // Binding becomes durable before the key may occupy its canonical
            // name. A crash earlier leaves an unreferenced, never-adopted stage.
            super::super::mode::sync_directory(parent)?;
            publish_record(&intent_path, &intent)?;
            intent
        }
    };
    let prefix = format!(".plurx-key-{}-", context.request.id);
    let suffix = intent
        .staged_name
        .strip_prefix(&prefix)
        .ok_or_else(|| refused("invalid staged key name"))?;
    canonical_id(suffix)?;
    if intent.request_sha256 != context.request.sha256 {
        return Err(refused("key publication belongs to another request"));
    }
    let staged = parent.join(&intent.staged_name);
    if let Some(current) = file_binding(path)? {
        if current != intent.binding {
            return Err(refused("canonical key differs from the bound publication"));
        }
    } else {
        if file_binding(&staged)?.as_ref() != Some(&intent.binding) {
            return Err(refused("bound staged key changed or disappeared"));
        }
        // Same-directory hard-link publication is atomic and never overwrites.
        // Unsupported filesystems fail closed, preserving the bound key stage.
        fs::hard_link(&staged, path).map_err(io)?;
        super::super::mode::sync_directory(parent)?;
        if file_binding(path)?.as_ref() != Some(&intent.binding) {
            return Err(refused("published key identity differs from its stage"));
        }
    }
    crate::secrets::open_credential_key(path, census)
        .map_err(|error| StoreError::Identity(error.to_string()))
}

/// The old opener needs the credential file for its clock-only membership
/// object before admitted census. Only a helper-owned operation may resolve a
/// missing file here, using the actual opened authoritative store inventory.
/// This creates no purpose installation or serving proof. The existing admitted
/// census and key verification still run after the real startup barrier.
pub(crate) async fn prepare_key_before_clock(
    config: &Config,
    store: &crate::store::HiqliteAuthStore,
    client: &hiqlite::Client,
    node_id: u64,
) -> Result<(), StoreError> {
    use crate::store::{SharingPurposeKeyStore, SharingStore, TraktStore};
    if !KEY_PUBLICATION
        .try_with(|context| context.original.is_none())
        .unwrap_or(false)
    {
        return Ok(());
    }
    let local = client
        .local_db_raft_metrics()
        .map_err(|error| refused(error.to_string()))?;
    let require_private = || -> Result<(), StoreError> {
        let membership = local.membership_snapshot();
        let observed = local.snapshot();
        let sole = std::collections::BTreeSet::from([node_id]);
        if !membership.running
            || !membership.committed
            || membership.node_id != node_id
            || membership.members != sole
            || membership.voters != sole
            || observed.current_leader != Some(node_id)
        {
            return Err(refused(
                "pre-clock key inventory requires the actual private sole leader",
            ));
        }
        Ok(())
    };
    require_private()?;
    store.inspect_sharing_purpose_material().await?;
    let mut census = SealedRowCensus::default();
    for auth in store.list_trakt_auth().await? {
        census.observe_row(&auth.access_token, &auth.refresh_token);
    }
    census.merge(store.sharing_sealed_census().await?);
    require_private()?;
    let key = open_actual_census_key(&config.credential_key_path(), &census)?;
    store.verify_sharing_purpose_material(&key).await?;
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConversionIntent {
    request_sha256: String,
    manifest: StorageManifest,
    incoming_database: PathBuf,
    retired_directory: PathBuf,
    receipt_path: PathBuf,
    receipt_sha256: String,
    source_image: VerifiedArtifact,
    target_sha256: String,
}

impl ConversionIntent {
    fn matches(&self, handoff: &NativeHandoff) -> bool {
        self.manifest == handoff.manifest
            && self.incoming_database == handoff.incoming_database
            && self.retired_directory == handoff.retired_directory
            && self.receipt_path == handoff.receipt_path
            && self.receipt_sha256 == handoff.receipt_sha256
            && Some(&self.source_image) == handoff.source_image.as_ref()
            && self.target_sha256 == handoff.target_sha256
    }
}

/// A private singleton has no peer clock to sample. Obtain the actual applied
/// roster and leadership from this startup's SDK-backed manager, then publish
/// its checked roster ticket. The ordinary startup admission and its original
/// deadline still govern every subsequent transition.
struct PrivateSingletonClock {
    node_id: String,
}

impl migration::StartupClockObserver for PrivateSingletonClock {
    fn start(
        &self,
        manager: crate::cluster::membership::MembershipManager,
        node_id: String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), StoreError>> + Send + '_>>
    {
        Box::pin(async move {
            if self.node_id != node_id {
                return Err(refused(
                    "legacy clock observation belongs to another local node",
                ));
            }
            let roster = manager
                .clock_peers()
                .await
                .map_err(|error| refused(error.to_string()))?;
            let applied = roster
                .membership
                .as_ref()
                .ok_or_else(|| refused("missing applied clock roster"))?;
            let leadership = manager
                .clock_leadership_identity()
                .ok_or_else(|| refused("missing local SDK leadership"))?;
            let sole = std::collections::BTreeSet::from([applied.local_node]);
            if !roster.peers.is_empty()
                || applied.members != sole
                || applied.voters != sole
                || leadership.membership != *applied
                || leadership.current_leader != Some(applied.local_node)
            {
                return Err(refused(
                    "legacy helper requires the actual committed sole-member leader",
                ));
            }
            let clock = manager.clock_guard();
            let ticket = clock
                .roster_for_peer_directory(&roster)
                .map_err(|error| refused(error.to_string()))?;
            if manager.clock_leadership_identity().as_ref() != Some(&leadership)
                || !clock.publish(ticket, std::collections::BTreeMap::new())
            {
                return Err(refused(
                    "private singleton clock roster changed before observation",
                ));
            }
            Ok(())
        })
    }
}

/// Entry for the separate executable. It opens no child process. Metadata
/// arguments do not mint authority: executable, root, census and singleton SQL
/// are checked independently before any native publication.
pub async fn run(
    root: &Path,
    key: &Path,
    request_id: &str,
    request_sha256: &str,
    build: HelperBuild,
) -> Result<HelperResult, StoreError> {
    canonical_id(request_id)?;
    hex(request_sha256, 64)?;
    let lease = NativeDirectoryLock::acquire(root).await?;
    let root = lease.data_dir().to_path_buf();
    let prepared = pending(&root)?.ok_or_else(|| refused("no active migration request"))?;
    if prepared.id != request_id
        || prepared.sha256 != request_sha256
        || prepared.binding.helper != describe(build)?
    {
        return Err(refused("launched helper differs from the pinned request"));
    }
    let executable = std::env::current_exe().map_err(io)?;
    if hash_regular(&executable, 2 * 1024 * 1024 * 1024)? != prepared.binding.executable_sha256 {
        return Err(refused(
            "actual helper executable differs from the pinned artifact",
        ));
    }
    let request = read_request(&root, &prepared)?;
    if request.root != root_identity(&root)? || request.key_path_sha256 != path_hash(key)? {
        return Err(refused(
            "root or credential-key path differs from the request",
        ));
    }
    validate_key_before_open(&root, key, &prepared, &request)?;
    if let Some(result) = recover_conversion(&root, &prepared)? {
        return Ok(result);
    }
    validate_source_metadata(&root, &prepared, &request)?;
    if read_record::<serde_json::Value>(&owned_path(&root, Path::new("storage-transition.json"))?)?
        .is_some()
    {
        return Err(refused("unbound storage handoff exists"));
    }
    validate_private_source(&root)?;
    let config = request.settings.config(&root, key)?;
    let context = KeyPublication {
        root: root.clone(),
        request: prepared.clone(),
        key_path: key.to_path_buf(),
        original: request.key,
    };
    publish_record(
        &operation_path(&root, &prepared.id, "opening.json")?,
        &Opening {
            request_sha256: prepared.sha256.clone(),
        },
    )?;
    let node = fs::read_to_string(owned_path(&root, Path::new("node.id"))?).map_err(io)?;
    let observer = PrivateSingletonClock {
        node_id: node.trim().to_owned(),
    };
    KEY_PUBLICATION
        .scope(context, async {
            let selected = migration::select_daemon_store_observing_locked(
                &config,
                Some(&observer),
                lease.into_locked_file()?,
            )
            .await?;
            let selected = PendingSelectedStore::new(selected);
            let handle = convert(&config, selected, Some(&prepared)).await?;
            // No serving boot is installed here. The conversion's validation boot
            // was explicitly revoked before publication.
            handle.sqlite.close().await?;
            let result = read_record(&operation_path(&root, &prepared.id, "result.json")?)?
                .ok_or_else(|| refused("conversion completed without its durable result"));
            drop(handle);
            result
        })
        .await
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Opening {
    request_sha256: String,
}

fn validate_source_metadata(
    root: &Path,
    prepared: &PreparedRequest,
    request: &Request,
) -> Result<(), StoreError> {
    let current = metadata(root)?;
    if current == request.metadata {
        return Ok(());
    }
    let opening: Opening = read_record(&operation_path(root, &prepared.id, "opening.json")?)?
        .ok_or_else(|| refused("legacy metadata changed before helper opening"))?;
    if opening.request_sha256 != prepared.sha256
        || current[0] != request.metadata[0]
        || current[1] != request.metadata[1]
    {
        return Err(refused("legacy node or authority changed"));
    }
    let mut original = request.source_records[0]
        .clone()
        .ok_or_else(|| refused("original activation missing"))?;
    let activation: serde_json::Value =
        read_record(&owned_path(root, Path::new("hiqlite/activation.json"))?)?
            .ok_or_else(|| refused("activation disappeared during recovery"))?;
    // The only marker mutation the actual reopen performs is advancing its
    // committed schema from the closed predecessor to the current cohort.
    let old_schema = original["replicated_schema_version"].as_i64();
    let new_schema = activation["replicated_schema_version"].as_i64();
    if !matches!(
        (old_schema, new_schema),
        (Some(81), Some(81 | 82)) | (Some(82), Some(82))
    ) {
        return Err(refused("unrecognized activation schema progression"));
    }
    original["replicated_schema_version"] = activation["replicated_schema_version"].clone();
    if original != activation {
        return Err(refused("activation identity/history changed"));
    }
    let membership: Option<serde_json::Value> =
        read_record(&owned_path(root, Path::new("membership.json"))?)?;
    match (&request.source_records[1], &membership) {
        (Some(before), Some(after)) if before == after => {}
        (None, None) => {}
        (None, Some(after)) => {
            let member: crate::cluster::membership::LocalMembership =
                serde_json::from_value(after.clone())
                    .map_err(|error| refused(error.to_string()))?;
            let node = fs::read_to_string(owned_path(root, Path::new("node.id"))?).map_err(io)?;
            if member.cluster_id != activation["cluster_id"].as_str().unwrap_or_default()
                || member.node_id != node.trim()
                || member.role != crate::cluster::membership::ClusterRole::Voter
                || member.join_token_digest.is_some()
                || member.bootstrap.len() != 1
                || member.bootstrap[0] != member.local
                || !member.local.is_loopback()
                || member.raft_id != member.local.raft_id
            {
                return Err(refused(
                    "new membership is not the original private singleton",
                ));
            }
        }
        _ => return Err(refused("existing membership history changed")),
    }
    let breadcrumb: Option<serde_json::Value> =
        read_record(&owned_path(root, Path::new("hiqlite-activated.json"))?)?;
    match (&request.source_records[2], &breadcrumb) {
        (Some(before), Some(after)) if before == after => {}
        (None, None) => {}
        (None, Some(after))
            if after["cluster_id"] == activation["cluster_id"]
                && after["source_backup_sha256"] == activation["source_backup_sha256"] => {}
        _ => return Err(refused("activated-source history changed")),
    }
    Ok(())
}

fn validate_private_source(root: &Path) -> Result<(), StoreError> {
    use crate::cluster::membership::LocalMembership;
    let active = owned_path(root, Path::new("hiqlite"))?;
    crate::fs_secure::directory_identity_nofollow_blocking(&active).map_err(io)?;
    crate::fs_secure::regular_file_identity_nofollow_blocking(&owned_path(
        root,
        Path::new("hiqlite/state_machine/db/plurx.db"),
    )?)
    .map_err(io)?;
    for name in [
        "secret_raft",
        "secret_api",
        "activity_http_signing_key",
        "hiqlite/telemetry.db",
    ] {
        owned_path(root, Path::new(name))?;
    }
    let marker: migration::ActivationMarker =
        read_record(&owned_path(root, Path::new("hiqlite/activation.json"))?)?
            .ok_or_else(|| refused("existing legacy activation marker missing"))?;
    if marker.admitted_role.is_some() || ![81, 82].contains(&marker.replicated_schema_version) {
        return Err(refused("admitted or unsupported legacy source"));
    }
    let membership: Option<LocalMembership> =
        read_record(&owned_path(root, Path::new("membership.json"))?)?;
    if membership.is_some_and(|membership| {
        !membership.local.is_loopback()
            || membership.bootstrap.len() > 1
            || membership.bootstrap.iter().any(|peer| !peer.is_loopback())
            || membership.join_token_digest.is_some()
    }) {
        return Err(refused(
            "explicitly peered legacy root cannot use the singleton helper",
        ));
    }
    // These are resumable operations owned by other state machines. This helper
    // must not let the generic opener clean them or choose a bootstrap branch.
    refuse_foreign_operations(root)
}

fn validate_key_before_open(
    root: &Path,
    key: &Path,
    prepared: &PreparedRequest,
    request: &Request,
) -> Result<(), StoreError> {
    if let Some(original) = &request.key {
        if file_binding(key)?.as_ref() != Some(original) {
            return Err(refused("original key changed before legacy open"));
        }
    } else if let Some(current) = file_binding(key)? {
        let intent: KeyIntent = read_record(&operation_path(root, &prepared.id, "key.json")?)?
            .ok_or_else(|| refused("unbound key appeared before legacy open"))?;
        if intent.request_sha256 != prepared.sha256 || intent.binding != current {
            return Err(refused("appearing key is not this operation's publication"));
        }
    }
    Ok(())
}

/// The cluster-enabled daemon and helper share this sole orchestration. A
/// genuine selected store is required and its private lease is checked again.
pub async fn convert_private_selected(
    config: &Config,
    selected: PendingSelectedStore,
) -> Result<NativeStoreHandle, StoreError> {
    convert(config, selected, None).await
}

async fn convert(
    config: &Config,
    selected: PendingSelectedStore,
    operation: Option<&PreparedRequest>,
) -> Result<NativeStoreHandle, StoreError> {
    let root = &config.storage.data_dir;
    if operation.is_none() && pending(root)?.is_some() {
        return Err(selected
            .refuse(refused("a helper operation owns this conversion"))
            .await);
    }
    // This derives from SelectedStore's private actual lock, not a public File.
    let checked_lease = selected.helper_directory_lease(root)?;
    let generation = uuid::Uuid::new_v4().to_string();
    let incoming_relative = PathBuf::from(format!("native-import-{generation}"));
    let incoming = owned_path(root, &incoming_relative)?;
    let target = incoming.join("plurx.db");
    let conversion = async {
        let client = selected
            .local_client()
            .ok_or_else(|| refused("legacy private storage has no local export client"))?;
        fs::create_dir(&incoming).map_err(io)?;
        super::super::mode::sync_directory(root)?;
        super::super::mode::begin_native_adoption(&incoming)?;
        let receipt = migration::standalone_export::export_private_singleton(
            &client,
            root,
            &target,
            &selected.identity.cluster_id,
            &selected.identity.node_id,
        )
        .await?;
        let telemetry_source = owned_path(root, Path::new("hiqlite/telemetry.db"))?;
        let telemetry_target = target.clone();
        let worker_lease = checked_lease.clone();
        let node_local = tokio::task::spawn_blocking(move || {
            let _lease = worker_lease;
            super::super::telemetry::transfer_local_telemetry(&telemetry_source, &telemetry_target)
        })
        .await
        .map_err(|error| StoreError::Task(format!("joining local state conversion: {error}")))??;
        let node_path = owned_path(root, Path::new("node.id"))?;
        fs::copy(node_path, incoming.join("node.id")).map_err(io)?;
        let imported =
            super::super::open_native_store_at(&incoming, &config.credential_key_path()).await?;
        if imported.identity.instance_id != selected.identity.cluster_id
            || imported.identity.node_id != selected.identity.node_id
        {
            return Err(refused(
                "native conversion changed logical or local identity",
            ));
        }
        let source = imported
            .sqlite
            .prepare_native_source_before_serving(
                &imported.identity.node_id,
                &imported.credential_key,
            )
            .await?;
        source.revoke();
        drop(source);
        let manifest = StorageManifest::read(&incoming)?
            .ok_or_else(|| refused("verified import lost its native manifest"))?;
        imported.sqlite.close().await?;
        drop(imported);
        let checkpoint_target = target.clone();
        let worker_lease = checked_lease.clone();
        tokio::task::spawn_blocking(move || {
            let _lease = worker_lease;
            super::super::mode::checkpoint_native_image(&checkpoint_target)
        })
        .await
        .map_err(|error| StoreError::Task(format!("joining native checkpoint: {error}")))??;
        let receipt_relative = incoming_relative.join("conversion-receipt.json");
        publish_record(
            &owned_path(root, &receipt_relative)?,
            &serde_json::json!({
                "replicated": &receipt, "node_local": node_local,
            }),
        )?;
        Ok::<_, StoreError>((manifest, receipt, receipt_relative))
    }
    .await;
    selected.shutdown().await?;
    drop(checked_lease);
    let lock = selected
        .into_selected()
        .into_native_directory_lock(root.clone())?;
    let (manifest, receipt, receipt_relative) = conversion?;
    let source_path = receipt
        .source_image
        .strip_prefix(root)
        .map_err(|_| refused("export cut escaped the owned root"))?
        .to_path_buf();
    let intent = ConversionIntent {
        request_sha256: operation.map_or_else(String::new, |operation| operation.sha256.clone()),
        manifest,
        incoming_database: incoming_relative.join("plurx.db"),
        retired_directory: PathBuf::from(format!("retired-storage/{generation}")),
        receipt_sha256: hash_regular(&owned_path(root, &receipt_relative)?, RECEIPT_LIMIT)?,
        receipt_path: receipt_relative,
        source_image: VerifiedArtifact {
            path: source_path,
            sha256: receipt.source_sha256,
        },
        target_sha256: hash_regular(&target, u64::MAX)?,
    };
    if let Some(operation) = operation {
        // The operation-to-attempt-to-native-generation binding is durable
        // BEFORE NativeHandoff can retire or rename anything.
        publish_record(
            &operation_path(root, &operation.id, "conversion.json")?,
            &intent,
        )?;
    }
    finish_conversion(root, &intent)?;
    if let Some(operation) = operation {
        publish_result(root, operation, &intent)?;
    }
    super::super::open_native_store_locked(config, lock).await
}

fn finish_conversion(root: &Path, intent: &ConversionIntent) -> Result<(), StoreError> {
    if hash_regular(&owned_path(root, &intent.receipt_path)?, RECEIPT_LIMIT)?
        != intent.receipt_sha256
    {
        return Err(refused("bound conversion receipt changed"));
    }
    if StorageManifest::read(root)?.as_ref() == Some(&intent.manifest) {
        intent.manifest.validate_sqlite(root)?;
        // A published generation may have accepted writes. Do not reimport or
        // compare its live bytes with the pre-publication hash.
        if let Some(handoff) =
            read_record::<NativeHandoff>(&owned_path(root, Path::new("storage-transition.json"))?)?
        {
            if !intent.matches(&handoff) {
                return Err(refused("published generation has a foreign transition"));
            }
            super::super::transition::finish_native_handoff(root, &handoff)?;
        }
        return Ok(());
    }
    let handoff = match read_record::<NativeHandoff>(&owned_path(
        root,
        Path::new("storage-transition.json"),
    )?)? {
        Some(handoff) => handoff,
        None => {
            if hash_regular(&owned_path(root, &intent.incoming_database)?, u64::MAX)?
                != intent.target_sha256
            {
                return Err(refused("prepared native image changed"));
            }
            super::super::transition::prepare_native_handoff_with_source(
                root,
                intent.manifest.clone(),
                intent.incoming_database.clone(),
                intent.retired_directory.clone(),
                intent.receipt_path.clone(),
                intent.source_image.clone(),
            )?
        }
    };
    if !intent.matches(&handoff) {
        return Err(refused("native handoff differs from its operation binding"));
    }
    super::super::transition::finish_native_handoff(root, &handoff)?;
    Ok(())
}

fn publish_result(
    root: &Path,
    operation: &PreparedRequest,
    intent: &ConversionIntent,
) -> Result<HelperResult, StoreError> {
    let result = HelperResult {
        version: 1,
        request: operation.clone(),
        manifest: intent.manifest.clone(),
        receipt_path: intent.receipt_path.clone(),
        receipt_sha256: intent.receipt_sha256.clone(),
    };
    let path = operation_path(root, &operation.id, "result.json")?;
    if let Some(existing) = read_record::<HelperResult>(&path)? {
        if existing != result {
            return Err(refused(
                "another completion already occupies the result path",
            ));
        }
    } else {
        publish_record(&path, &result)?;
    }
    Ok(result)
}

fn recover_conversion(
    root: &Path,
    operation: &PreparedRequest,
) -> Result<Option<HelperResult>, StoreError> {
    let intent: Option<ConversionIntent> =
        read_record(&operation_path(root, &operation.id, "conversion.json")?)?;
    let Some(intent) = intent else {
        return Ok(None);
    };
    if intent.request_sha256 != operation.sha256 {
        return Err(refused("prepared conversion belongs to another request"));
    }
    finish_conversion(root, &intent)?;
    publish_result(root, operation, &intent).map(Some)
}

#[cfg(test)]
mod tests;
