//! Root-admin attestation for old v2 job witnesses with no v1 host identity.
//!
//! The helper proves the witness's kernel boot belongs to this host's protected
//! persistent journal. This is independent evidence; it never fills in or edits
//! the historical worker record. A different host kernel has no surviving old
//! tasks, even when a raw child invalidated the same-kernel anchor. The worker
//! rechecks exact bytes, node, owner, activation and expiry, then uses its normal
//! typed settlement and acknowledgement before removing worker evidence.
use super::{
    clear_at, physical_stop, read_bytes, reader, settle_records, verify_directory, Envelope,
    Identity, Record,
};
use plurx_core::store::{background_jobs_resources::BackgroundSourceOwner, Store};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{ffi::CString, io, path::Path, sync::Arc};

const MODEL: &str = "host_journal_reboot_v1";
const DIRECTORY: &str = "/run/plurx-physical-stop/reboot-v1";
const MAX_PROOFS: usize = 16;
const MAX_AGE_MS: i64 = 15 * 60_000;
const MAX_SKEW_MS: i64 = 5_000;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Proof {
    version: u32,
    model: String,
    node_id: String,
    custody_id: String,
    activation_id: String,
    machine_id: String,
    previous_boot_id: String,
    current_boot_id: String,
    witness_sha256: String,
    legacy_sha256: String,
    at_ms: i64,
    previous_boot_entry_ms: i64,
}

// Other v2 reader metadata is retained and hashed, but is not a death proof
// after a reboot. Reboot recovery depends on kernel provenance and exact owner.
#[derive(Deserialize)]
struct Witness {
    version: u32,
    reader_model: String,
    kernel_instance: String,
    activation_id: String,
    node_id: String,
    record: Record,
}

fn canonical_uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| !id.is_nil() && id.to_string() == value)
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

impl Proof {
    fn verified(&self, identity: &Identity, node: &str, now_ms: i64) -> bool {
        self.version == 1
            && self.model == MODEL
            && identity.valid()
            && self.node_id == node
            && self.machine_id == identity.machine_id
            && self.current_boot_id == identity.boot_id
            && self.previous_boot_id != self.current_boot_id
            && canonical_uuid(&self.previous_boot_id)
            && canonical_uuid(&self.current_boot_id)
            && canonical_uuid(&self.custody_id)
            && canonical_uuid(&self.activation_id)
            && [&self.witness_sha256, &self.legacy_sha256]
                .into_iter()
                .all(|value| {
                    value.len() == 64
                        && value
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                })
            && self.at_ms > 0
            && self.at_ms <= now_ms.saturating_add(MAX_SKEW_MS)
            && self.at_ms >= now_ms.saturating_sub(MAX_AGE_MS)
            && self.previous_boot_entry_ms > 0
            && self.previous_boot_entry_ms <= self.at_ms
    }
}

fn read_proofs() -> io::Result<Vec<Proof>> {
    match std::fs::symlink_metadata(DIRECTORY) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    }
    let directory = physical_stop::admin_directory(&[c"reboot-v1"])?;
    let mut names = std::fs::read_dir(DIRECTORY)?
        .take(MAX_PROOFS + 1)
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<io::Result<Vec<_>>>()?;
    if names.len() > MAX_PROOFS {
        return Err(io::Error::other(
            "host reboot proof inventory exceeds its bound",
        ));
    }
    names.sort();
    let mut proofs = Vec::new();
    for name in names {
        let Some(stem) = name.to_str().and_then(|name| name.strip_suffix(".json")) else {
            continue;
        };
        if !canonical_uuid(stem) {
            continue;
        }
        let name = CString::new(format!("{stem}.json")).map_err(io::Error::other)?;
        match physical_stop::read_admin_file(&directory, &name)
            .and_then(|bytes| serde_json::from_slice::<Proof>(&bytes).map_err(io::Error::other))
        {
            Ok(proof) if proof.custody_id == stem => proofs.push(proof),
            Ok(_) => tracing::warn!(stem, "host reboot proof names another custody"),
            Err(error) => tracing::warn!(%error, stem, "host reboot proof refused"),
        }
    }
    Ok(proofs)
}

pub(super) async fn replay(
    store: &Arc<dyn Store>,
    legacy: &Path,
    node: &str,
) -> Result<(), String> {
    let proofs = read_proofs().map_err(|error| error.to_string())?;
    if proofs.is_empty() {
        return Ok(());
    }
    let identity = Identity::current().ok_or("host reboot proof needs explicit host identity")?;
    if reader::kernel_instance().as_deref() != Some(format!("linux:{}", identity.boot_id).as_str())
    {
        return Err("host reboot proof needs the actual procfs host boot identity".into());
    }
    for proof in proofs {
        if proof.node_id != node {
            continue;
        }
        if let Err(error) = apply(
            store,
            legacy,
            node,
            &identity,
            crate::state::clock_ms(),
            &proof,
        )
        .await
        {
            tracing::warn!(%error, custody_id = %proof.custody_id, "host reboot proof retained for retry");
        }
    }
    Ok(())
}

async fn apply(
    store: &Arc<dyn Store>,
    legacy: &Path,
    node: &str,
    identity: &Identity,
    now_ms: i64,
    proof: &Proof,
) -> Result<(), String> {
    if !proof.verified(identity, node, now_ms) {
        return Err(
            "host reboot proof is stale or names another host, boot, node or activation".into(),
        );
    }
    let witnesses = legacy.with_file_name("v2");
    let name = format!("background-{}.json", proof.custody_id);
    let witness_path = witnesses.join(&name);
    if !witness_path
        .try_exists()
        .map_err(|error| error.to_string())?
    {
        return if !legacy
            .join(&name)
            .try_exists()
            .map_err(|error| error.to_string())?
        {
            Ok(())
        } else {
            Err("host reboot proof has no matching reader witness".into())
        };
    }
    verify_directory(&witnesses)?;
    verify_directory(legacy)?;
    let bytes = read_bytes(&witness_path)?;
    if digest(&bytes) != proof.witness_sha256 {
        return Err("host reboot proof reader bytes changed".into());
    }
    let witness: Witness = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    let Record::Background { token } = &witness.record else {
        return Err("host reboot proof requires a background job witness".into());
    };
    let BackgroundSourceOwner::Job { token: owner } = &token.owner else {
        return Err("host reboot proof requires a durable job owner".into());
    };
    owner.validate().map_err(|error| error.to_string())?;
    if witness.version != 2
        || witness.reader_model != "daemon_mediated_v1"
        || witness.kernel_instance != format!("linux:{}", proof.previous_boot_id)
        || witness.node_id != node
        || witness.record.node() != node
        || witness.record.name()? != name
        || token.custody_id != proof.custody_id
        || witness.activation_id != proof.activation_id
        || owner.lease_expires_ms > now_ms
    {
        return Err("host reboot proof and exact expired reader owner differ".into());
    }
    let armed_path = witnesses.join(format!("background-{}.armed", proof.custody_id));
    if read_bytes(&armed_path)? != proof.activation_id.as_bytes() {
        return Err("host reboot proof activation marker differs".into());
    }
    let present = store
        .resolve_background_source_custody(token.clone())
        .await
        .map_err(|error| error.to_string())?
        .is_some();
    let legacy_path = legacy.join(&name);
    // A prior acknowledged settlement may have removed v1 before a crash.
    // Only authoritative exact absence permits finishing that partial cleanup.
    if legacy_path
        .try_exists()
        .map_err(|error| error.to_string())?
    {
        let legacy_bytes = read_bytes(&legacy_path)?;
        if digest(&legacy_bytes) != proof.legacy_sha256 {
            return Err("host reboot proof legacy bytes changed".into());
        }
        let envelope: Envelope =
            serde_json::from_slice(&legacy_bytes).map_err(|error| error.to_string())?;
        if envelope.version != 1
            || envelope.node_id != node
            || !envelope.machine_id.is_empty()
            || !envelope.boot_id.is_empty()
            || envelope.records.len() != 1
            || !envelope.records[0].same_owner(&witness.record)
        {
            return Err("host reboot proof is not the exact missing-identity legacy owner".into());
        }
    } else if present {
        return Err("host reboot proof legacy activation is missing".into());
    }
    if present {
        settle_records(store, std::slice::from_ref(&witness.record)).await?;
        if store
            .resolve_background_source_custody(token.clone())
            .await
            .map_err(|error| error.to_string())?
            .is_some()
        {
            return Err("host reboot settlement awaits exact release acknowledgement".into());
        }
    }
    // Do not consume paths replaced while the authoritative write was awaited.
    if digest(&read_bytes(&witness_path)?) != proof.witness_sha256 {
        return Err("host reboot witness changed during settlement".into());
    }
    if legacy_path
        .try_exists()
        .map_err(|error| error.to_string())?
        && digest(&read_bytes(&legacy_path)?) != proof.legacy_sha256
    {
        return Err("host reboot legacy evidence changed during settlement".into());
    }
    clear_at(legacy, &witness.record)?;
    reader::complete_at(&witnesses, &witness.record)?;
    tracing::info!(custody_id = %proof.custody_id, "source custody recovered through verified host journal reboot proof");
    Ok(())
}

#[cfg(test)]
mod tests;
