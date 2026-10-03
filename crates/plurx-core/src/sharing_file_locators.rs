//! Stable, receiver-signed file locators. Verification identifies a reference;
//! it never proves current viewer, grant, file, worker or delivery authority.
use crate::{
    error::StoreError,
    secrets::{CredentialKey, SealedSecret, SharingSecretPurpose},
    sharing::{invalid, SharingIdentity, SourceId},
    sharing_catalogue::SharedReference,
    sharing_catalogue_details::FileRevision,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use uuid::Uuid;
use zeroize::Zeroize;

const PAYLOAD_BYTES: usize = 145;
const SIGNATURE_BYTES: usize = 32;
const ENCODED_BYTES: usize = 236;
const DOMAIN: &[u8] = b"plurx.sharing.file-locator.v1\0";

#[derive(Clone, PartialEq, Eq)]
pub struct FileLocatorReference {
    pub item: SharedReference,
    pub lifecycle_generation: i64,
    pub file_id: SourceId,
    pub revision: FileRevision,
}

/// No diagnostic or wire implementation. The sole exposed string is already
/// signed; callers cannot mutate a reference inside an issued locator.
pub struct SharedFileLocator {
    encoded: String,
    import_id: Uuid,
}
impl SharedFileLocator {
    pub fn as_str(&self) -> &str {
        &self.encoded
    }
    pub fn file_base(&self) -> String {
        format!(
            "/api/v1/shared/imports/{}/files/{}",
            self.import_id, self.encoded
        )
    }
}

/// Random purpose material must be persisted sealed by coordinated activation.
/// Reads do not generate or repair it. Master rewrap keeps the signing bytes.
pub struct FileLocatorKey {
    receiver: Uuid,
    epoch: Uuid,
    key: [u8; 32],
}
impl Drop for FileLocatorKey {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}
fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|byte| {
            [
                char::from(DIGITS[usize::from(byte >> 4)]),
                char::from(DIGITS[usize::from(byte & 15)]),
            ]
        })
        .collect()
}
fn unhex(value: &str) -> Result<[u8; 32], StoreError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid());
    }
    let mut bytes = [0; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).map_err(|_| invalid())?;
    }
    Ok(bytes)
}
fn positive_id(value: &SourceId) -> Result<i64, StoreError> {
    value
        .as_str()
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(invalid)
}
impl FileLocatorKey {
    pub(crate) fn art_context(&self) -> (Uuid, Uuid) {
        (self.receiver, self.epoch)
    }
    pub(crate) fn art_signature(&self, bytes: &[u8]) -> ring::hmac::Tag {
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &self.key);
        let mut context = ring::hmac::Context::with_key(&key);
        context.update(b"plurx.sharing.receiver-art-resource.v1\0");
        context.update(bytes);
        context.sign()
    }
    pub fn generate_sealed(
        credential: &CredentialKey,
        identity: &SharingIdentity,
    ) -> Result<SealedSecret, StoreError> {
        if identity.server_id.is_nil() || identity.catalogue_epoch.is_nil() {
            return Err(invalid());
        }
        let mut bytes = zeroize::Zeroizing::new([0_u8; 32]);
        getrandom::getrandom(bytes.as_mut()).map_err(|_| invalid())?;
        let clear = zeroize::Zeroizing::new(hex(bytes.as_ref()));
        credential
            .seal_sharing(
                SharingSecretPurpose::FileLocator,
                identity.server_id,
                identity.catalogue_epoch,
                &clear,
            )
            .map_err(|_| invalid())
    }
    pub fn open(
        credential: &CredentialKey,
        identity: &SharingIdentity,
        envelope: &SealedSecret,
    ) -> Result<Self, StoreError> {
        if identity.server_id.is_nil() || identity.catalogue_epoch.is_nil() {
            return Err(invalid());
        }
        let clear = credential
            .open_sharing(
                SharingSecretPurpose::FileLocator,
                identity.server_id,
                identity.catalogue_epoch,
                envelope,
            )
            .map_err(|_| invalid())?;
        Ok(Self {
            receiver: identity.server_id,
            epoch: identity.catalogue_epoch,
            key: unhex(clear.expose())?,
        })
    }
    fn signature(&self, bytes: &[u8]) -> ring::hmac::Tag {
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &self.key);
        let mut context = ring::hmac::Context::with_key(&key);
        context.update(DOMAIN);
        context.update(bytes);
        context.sign()
    }
    pub fn issue(&self, reference: &FileLocatorReference) -> Result<SharedFileLocator, StoreError> {
        let item = &reference.item;
        if reference.lifecycle_generation <= 0
            || [item.import_id, item.server_id, item.catalogue_epoch]
                .iter()
                .any(Uuid::is_nil)
        {
            return Err(invalid());
        }
        let mut bytes = Vec::with_capacity(PAYLOAD_BYTES + SIGNATURE_BYTES);
        bytes.push(1);
        for id in [
            self.receiver,
            self.epoch,
            item.import_id,
            item.server_id,
            item.catalogue_epoch,
        ] {
            bytes.extend_from_slice(id.as_bytes());
        }
        bytes.extend_from_slice(&reference.lifecycle_generation.to_be_bytes());
        for id in [&item.library_id, &item.item_id, &reference.file_id] {
            bytes.extend_from_slice(&positive_id(id)?.to_be_bytes());
        }
        bytes.extend_from_slice(&unhex(reference.revision.as_str())?);
        let signature = self.signature(&bytes);
        bytes.extend_from_slice(signature.as_ref());
        Ok(SharedFileLocator {
            encoded: URL_SAFE_NO_PAD.encode(bytes),
            import_id: item.import_id,
        })
    }
    /// Scope arguments must come from the current authenticated import read.
    /// Even a valid reference needs fresh assignments/grant/file authority.
    pub fn verify(
        &self,
        encoded: &str,
        expected_import: Uuid,
        expected_lifecycle: i64,
    ) -> Result<FileLocatorReference, StoreError> {
        if encoded.len() != ENCODED_BYTES
            || expected_import.is_nil()
            || expected_lifecycle <= 0
            || !encoded
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
        {
            return Err(invalid());
        }
        let bytes = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| invalid())?;
        if bytes.len() != PAYLOAD_BYTES + SIGNATURE_BYTES
            || URL_SAFE_NO_PAD.encode(&bytes) != encoded
        {
            return Err(invalid());
        }
        use subtle::ConstantTimeEq;
        if !bool::from(
            bytes[PAYLOAD_BYTES..].ct_eq(self.signature(&bytes[..PAYLOAD_BYTES]).as_ref()),
        ) {
            return Err(invalid());
        }
        let id =
            |offset: usize| Uuid::from_slice(&bytes[offset..offset + 16]).map_err(|_| invalid());
        let integer = |offset: usize| -> Result<i64, StoreError> {
            let value = i64::from_be_bytes(
                bytes[offset..offset + 8]
                    .try_into()
                    .map_err(|_| invalid())?,
            );
            if value <= 0 {
                return Err(invalid());
            }
            Ok(value)
        };
        if bytes[0] != 1
            || id(1)? != self.receiver
            || id(17)? != self.epoch
            || id(33)? != expected_import
            || integer(81)? != expected_lifecycle
            || id(49)?.is_nil()
            || id(65)?.is_nil()
        {
            return Err(invalid());
        }
        Ok(FileLocatorReference {
            item: SharedReference {
                import_id: expected_import,
                server_id: id(49)?,
                catalogue_epoch: id(65)?,
                library_id: SourceId::parse(&integer(89)?.to_string())?,
                item_id: SourceId::parse(&integer(97)?.to_string())?,
            },
            lifecycle_generation: expected_lifecycle,
            file_id: SourceId::parse(&integer(105)?.to_string())?,
            revision: FileRevision::parse(&hex(&bytes[113..PAYLOAD_BYTES]))?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (
        SharingIdentity,
        CredentialKey,
        SealedSecret,
        FileLocatorReference,
    ) {
        let identity = SharingIdentity {
            server_id: Uuid::new_v4(),
            catalogue_epoch: Uuid::new_v4(),
            created_at_ms: 1,
        };
        let master = CredentialKey::from_bytes([31; 32]);
        let envelope =
            FileLocatorKey::generate_sealed(&master, &identity).expect("explicit key factory");
        let reference = FileLocatorReference {
            item: SharedReference {
                import_id: Uuid::new_v4(),
                server_id: Uuid::new_v4(),
                catalogue_epoch: Uuid::new_v4(),
                library_id: SourceId::parse("9007199254740993").expect("exact library"),
                item_id: SourceId::parse("9223372036854775807").expect("exact item"),
            },
            lifecycle_generation: 1,
            file_id: SourceId::parse("7").expect("file"),
            revision: FileRevision::parse(&"a".repeat(64)).expect("opaque revision"),
        };
        (identity, master, envelope, reference)
    }
    #[test]
    fn sharing_file_locator_binds_full_source_and_import_lifecycle_without_numeric_collision() {
        let (identity, master, envelope, reference) = fixture();
        let key = FileLocatorKey::open(&master, &identity, &envelope).expect("existing key");
        let first = key.issue(&reference).expect("issued reference");
        assert!(
            key.verify(first.as_str(), reference.item.import_id, 1)
                .expect("verified identity")
                == reference
        );
        assert!(first.file_base().starts_with(&format!(
            "/api/v1/shared/imports/{}/files/",
            reference.item.import_id
        )));
        assert!(first
            .as_str()
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte)));
        let mut other = reference.clone();
        other.item.server_id = Uuid::new_v4();
        assert_ne!(
            first.as_str(),
            key.issue(&other).expect("other Source same file7").as_str()
        );
        other = reference.clone();
        other.revision = FileRevision::parse(&"b".repeat(64)).expect("new revision");
        assert_ne!(
            first.as_str(),
            key.issue(&other).expect("replacement file7").as_str()
        );
        assert!(key.verify(first.as_str(), Uuid::new_v4(), 1).is_err());
        assert!(key
            .verify(first.as_str(), reference.item.import_id, 2)
            .is_err());
        other = reference.clone();
        other.file_id = SourceId::parse("9223372036854775807").expect("exact Source file");
        let huge = key.issue(&other).expect("huge exact file");
        assert!(
            key.verify(huge.as_str(), other.item.import_id, 1)
                .expect("exact roundtrip")
                == other
        );
        other.file_id = SourceId::parse("0").expect("untrusted ID grammar");
        assert!(key.issue(&other).is_err());
    }
    #[test]
    fn sharing_file_locator_refuses_tampering_and_signed_malformed_payloads() {
        let (identity, master, envelope, reference) = fixture();
        let key = FileLocatorKey::open(&master, &identity, &envelope).expect("existing key");
        let locator = key.issue(&reference).expect("reference");
        let bytes = URL_SAFE_NO_PAD
            .decode(locator.as_str())
            .expect("canonical fixed frame");
        for index in 0..bytes.len() {
            let mut changed = bytes.clone();
            changed[index] ^= 1;
            assert!(
                key.verify(
                    &URL_SAFE_NO_PAD.encode(changed),
                    reference.item.import_id,
                    1
                )
                .is_err(),
                "changed byte{index}"
            );
        }
        for (offset, value) in [(0, 0_u8), (0, 2), (89, 255), (97, 255), (105, 255)] {
            let mut changed = bytes.clone();
            changed[offset] = value;
            let signature = key.signature(&changed[..PAYLOAD_BYTES]);
            changed[PAYLOAD_BYTES..].copy_from_slice(signature.as_ref());
            assert!(
                key.verify(
                    &URL_SAFE_NO_PAD.encode(changed),
                    reference.item.import_id,
                    1
                )
                .is_err(),
                "invalid signed field{offset}"
            );
        }
        for invalid in [
            String::new(),
            format!("{}=", locator.as_str()),
            format!("{}\n", locator.as_str()),
            locator.as_str().replace('_', "/") + "/",
            "a".repeat(2049),
            format!("{}.x", locator.as_str()),
            format!("https://source/{}", locator.as_str()),
        ] {
            assert!(key.verify(&invalid, reference.item.import_id, 1).is_err());
        }
    }
    #[test]
    fn sharing_file_locator_key_rewrap_preserves_reference_and_refuses_purpose_or_receiver_swap() {
        let (identity, master, envelope, reference) = fixture();
        let old = FileLocatorKey::open(&master, &identity, &envelope).expect("old master");
        let locator = old.issue(&reference).expect("stable reference");
        let replacement_master = CredentialKey::from_bytes([32; 32]);
        let clear = master
            .open_sharing(
                SharingSecretPurpose::FileLocator,
                identity.server_id,
                identity.catalogue_epoch,
                &envelope,
            )
            .expect("rewrap clear only");
        let rewrapped = replacement_master
            .seal_sharing(
                SharingSecretPurpose::FileLocator,
                identity.server_id,
                identity.catalogue_epoch,
                clear.expose(),
            )
            .expect("rewrap same material");
        let new = FileLocatorKey::open(&replacement_master, &identity, &rewrapped)
            .expect("new envelope same signing material");
        assert_eq!(
            locator.as_str(),
            new.issue(&reference).expect("stable locator").as_str()
        );
        assert!(
            new.verify(locator.as_str(), reference.item.import_id, 1)
                .expect("pre-rewrap locator remains valid")
                == reference
        );
        assert!(FileLocatorKey::open(&replacement_master, &identity, &envelope).is_err());
        let wrong_purpose = master
            .seal_sharing(
                SharingSecretPurpose::CatalogueRevision,
                identity.server_id,
                identity.catalogue_epoch,
                clear.expose(),
            )
            .expect("other purpose material");
        assert!(FileLocatorKey::open(&master, &identity, &wrong_purpose).is_err());
        let mut wrong_identity = identity.clone();
        wrong_identity.server_id = Uuid::new_v4();
        assert!(FileLocatorKey::open(&master, &wrong_identity, &envelope).is_err());
        wrong_identity = identity.clone();
        wrong_identity.catalogue_epoch = Uuid::new_v4();
        assert!(FileLocatorKey::open(&master, &wrong_identity, &envelope).is_err());
        // Even deliberately reusing signing bytes under a different receiver
        // cannot transfer a locator: the receiver/epoch are signed fields.
        for other in [
            SharingIdentity {
                server_id: Uuid::new_v4(),
                ..identity.clone()
            },
            SharingIdentity {
                catalogue_epoch: Uuid::new_v4(),
                ..identity.clone()
            },
        ] {
            let scoped = master
                .seal_sharing(
                    SharingSecretPurpose::FileLocator,
                    other.server_id,
                    other.catalogue_epoch,
                    clear.expose(),
                )
                .expect("same material deliberately scoped elsewhere");
            let other_key =
                FileLocatorKey::open(&master, &other, &scoped).expect("other receiver key");
            assert!(other_key
                .verify(locator.as_str(), reference.item.import_id, 1)
                .is_err());
        }
        let replacement_key =
            FileLocatorKey::generate_sealed(&master, &identity).expect("explicit different key");
        assert!(FileLocatorKey::open(&master, &identity, &replacement_key)
            .expect("different purpose key")
            .verify(locator.as_str(), reference.item.import_id, 1)
            .is_err());
    }
}
