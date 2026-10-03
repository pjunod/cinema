//! Closed file revisions and server-only current file snapshot witnesses.
use crate::{
    error::StoreError,
    secrets::{CredentialKey, SealedSecret, SharingSecretPurpose},
    sharing::{invalid, SharingIdentity, SourceId},
};
use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;
use zeroize::Zeroize;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct FileRevision(String);
impl FileRevision {
    pub fn parse(value: &str) -> Result<Self, StoreError> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(invalid());
        }
        Ok(Self(value.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl<'de> Deserialize<'de> for FileRevision {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl serde::de::Visitor<'_> for Visitor {
            type Value = FileRevision;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a canonical opaque file revision")
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                FileRevision::parse(value).map_err(|_| E::custom("invalid file revision"))
            }
        }
        deserializer.deserialize_str(Visitor)
    }
}

/// Never serialized or formatted. Only a current authorized Store query may
/// construct this exact private projection. A wire digest is not this witness.
#[derive(Clone)]
pub struct SourceFileWitness {
    pub(crate) server: Uuid,
    pub(crate) epoch: Uuid,
    pub(crate) library: SourceId,
    pub(crate) item: SourceId,
    pub(crate) file: SourceId,
    projection: String,
}
impl SourceFileWitness {
    pub(crate) fn from_current_projection(
        server: Uuid,
        epoch: Uuid,
        library: SourceId,
        item: SourceId,
        file: SourceId,
        projection: String,
    ) -> Result<Self, StoreError> {
        if projection.is_empty() || projection.len() > 2 * 1024 * 1024 {
            return Err(invalid());
        }
        Ok(Self {
            server,
            epoch,
            library,
            item,
            file,
            projection,
        })
    }
    /// The admission transaction must regenerate and compare this projection
    /// together with the bound identities, not accept an old hash alone.
    pub(crate) fn canonical_projection(&self) -> &str {
        &self.projection
    }
}

/// Stable random purpose material persisted sealed to a Source/epoch. No raw
/// key export, Serialize or Debug implementation. Sealing-master rewrap keeps
/// this material; peer credential rotation never changes it.
pub struct CatalogueRevisionKey {
    server: Uuid,
    epoch: Uuid,
    key: [u8; 32],
}
impl Drop for CatalogueRevisionKey {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}
fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        value.push(DIGITS[usize::from(byte >> 4)] as char);
        value.push(DIGITS[usize::from(byte & 15)] as char);
    }
    value
}
impl CatalogueRevisionKey {
    /// Cryptographic factory only. Qualified schema activation owns durable
    /// insertion; catalogue reads must never call this factory to repair state.
    pub fn generate_sealed(
        credential: &CredentialKey,
        identity: SharingIdentity,
    ) -> Result<SealedSecret, StoreError> {
        let mut bytes = zeroize::Zeroizing::new([0_u8; 32]);
        getrandom::getrandom(bytes.as_mut()).map_err(|_| invalid())?;
        let clear = zeroize::Zeroizing::new(hex(bytes.as_ref()));
        credential
            .seal_sharing(
                SharingSecretPurpose::CatalogueRevision,
                identity.server_id,
                identity.catalogue_epoch,
                &clear,
            )
            .map_err(|_| invalid())
    }
    pub fn open(
        credential: &CredentialKey,
        identity: SharingIdentity,
        envelope: &SealedSecret,
    ) -> Result<Self, StoreError> {
        let clear = credential
            .open_sharing(
                SharingSecretPurpose::CatalogueRevision,
                identity.server_id,
                identity.catalogue_epoch,
                envelope,
            )
            .map_err(|_| invalid())?;
        if clear.expose().len() != 64
            || !clear
                .expose()
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(invalid());
        }
        let mut key = [0_u8; 32];
        for (index, byte) in key.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&clear.expose()[index * 2..index * 2 + 2], 16)
                .map_err(|_| invalid())?;
        }
        Ok(Self {
            server: identity.server_id,
            epoch: identity.catalogue_epoch,
            key,
        })
    }
    pub fn file_revision(&self, witness: &SourceFileWitness) -> Result<FileRevision, StoreError> {
        if self.server != witness.server || self.epoch != witness.epoch {
            return Err(invalid());
        }
        let root = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &self.key);
        let derived = ring::hmac::sign(&root, b"cinema-sharing-file-revision-key-v1");
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, derived.as_ref());
        let mut mac = ring::hmac::Context::with_key(&key);
        mac.update(b"cinema-sharing-file-revision-v1\0");
        mac.update(witness.server.as_bytes());
        mac.update(witness.epoch.as_bytes());
        for id in [&witness.library, &witness.item, &witness.file] {
            mac.update(id.as_str().as_bytes());
            mac.update(&[0]);
        }
        mac.update(witness.canonical_projection().as_bytes());
        FileRevision::parse(&hex(mac.sign().as_ref()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn witness(identity: &SharingIdentity, projection: &str) -> SourceFileWitness {
        SourceFileWitness::from_current_projection(
            identity.server_id,
            identity.catalogue_epoch,
            SourceId::parse("1").expect("library"),
            SourceId::parse("9007199254740993").expect("item"),
            SourceId::parse("9223372036854775807").expect("file"),
            projection.to_owned(),
        )
        .expect("current private fixture")
    }
    #[test]
    fn sharing_catalogue_file_revision_is_stable_through_rewrap_and_binds_private_snapshot() {
        let identity = SharingIdentity {
            server_id: Uuid::new_v4(),
            catalogue_epoch: Uuid::new_v4(),
            created_at_ms: 1000,
        };
        let old = CredentialKey::from_bytes([19; 32]);
        let new = CredentialKey::from_bytes([20; 32]);
        let envelope =
            CatalogueRevisionKey::generate_sealed(&old, identity.clone()).expect("purpose factory");
        let revision_key = CatalogueRevisionKey::open(&old, identity.clone(), &envelope)
            .expect("purpose material");
        let current = witness(
            &identity,
            "[1,\"/private/synthetic.mkv\",20,1000,\"exact probe snapshot\"]",
        );
        let revision = revision_key.file_revision(&current).expect("revision");
        let clear = old
            .open_sharing(
                SharingSecretPurpose::CatalogueRevision,
                identity.server_id,
                identity.catalogue_epoch,
                &envelope,
            )
            .expect("rewrap input");
        let rewrapped = new
            .seal_sharing(
                SharingSecretPurpose::CatalogueRevision,
                identity.server_id,
                identity.catalogue_epoch,
                clear.expose(),
            )
            .expect("rewrap envelope");
        let replacement = CatalogueRevisionKey::open(&new, identity.clone(), &rewrapped)
            .expect("same stable material");
        assert_eq!(
            revision,
            replacement
                .file_revision(&current)
                .expect("stable revision")
        );
        assert_ne!(
            revision,
            replacement
                .file_revision(&witness(&identity, "changed private probe/path snapshot"))
                .expect("changed revision")
        );
        let mut other = witness(&identity, current.canonical_projection());
        other.file = SourceId::parse("2").expect("other file");
        assert_ne!(
            revision,
            replacement.file_revision(&other).expect("bound file")
        );
        other.epoch = Uuid::new_v4();
        assert!(replacement.file_revision(&other).is_err());
        for invalid in ["A".repeat(64), "0".repeat(65), "/private/path".into()] {
            assert!(
                serde_json::from_value::<FileRevision>(serde_json::Value::String(invalid)).is_err()
            );
        }
        assert!(!serde_json::to_string(&revision)
            .expect("wire revision")
            .contains("private"));
    }
}
