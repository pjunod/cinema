//! Closed opaque artwork references. MAC verification identifies an intended
//! resource; current Store authority and a fresh opened-byte digest are still
//! required. Production callers capture their trusted clock, never request time.
use crate::{
    error::StoreError,
    sharing::{invalid, SourceId},
    sharing_catalogue::SharedReference,
    sharing_catalogue_details::CatalogueRevisionKey,
    sharing_file_locators::FileLocatorKey,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Deserializer, Serialize};
use subtle::ConstantTimeEq;
use uuid::Uuid;

pub const ART_RESOURCE_LIFETIME_MS: i64 = 300_000;
pub const MAX_ART_BYTES: usize = 15 * 1024 * 1024;
const SOURCE_PAYLOAD: usize = 75;
const SOURCE_BYTES: usize = SOURCE_PAYLOAD + 32;
const RECEIVER_PAYLOAD: usize = 65 + SOURCE_BYTES;
const RECEIVER_BYTES: usize = RECEIVER_PAYLOAD + 32;
const MAX_CLOCK_MS: i64 = 9_007_199_254_740_991;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArtKind {
    Poster,
    Backdrop,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArtVariant {
    Original,
    W300,
    W500,
    W780,
}
impl ArtKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Poster => "poster",
            Self::Backdrop => "backdrop",
        }
    }
    fn byte(self) -> u8 {
        match self {
            Self::Poster => 0,
            Self::Backdrop => 1,
        }
    }
    fn parse(byte: u8) -> Result<Self, StoreError> {
        match byte {
            0 => Ok(Self::Poster),
            1 => Ok(Self::Backdrop),
            _ => Err(invalid()),
        }
    }
}
impl ArtVariant {
    fn byte(self) -> u8 {
        match self {
            Self::Original => 0,
            Self::W300 => 1,
            Self::W500 => 2,
            Self::W780 => 3,
        }
    }
    fn parse(byte: u8) -> Result<Self, StoreError> {
        match byte {
            0 => Ok(Self::Original),
            1 => Ok(Self::W300),
            2 => Ok(Self::W500),
            3 => Ok(Self::W780),
            _ => Err(invalid()),
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::W300 => "w300",
            Self::W500 => "w500",
            Self::W780 => "w780",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceArtwork {
    pub kind: ArtKind,
    pub variant: ArtVariant,
    pub resource: SourceArtResource,
}
pub fn bounded_art<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<SourceArtwork>, D::Error> {
    struct Art;
    impl<'de> serde::de::Visitor<'de> for Art {
        type Value = Vec<SourceArtwork>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("at most eight closed artwork resources")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            let mut values = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(8));
            while let Some(value) = seq.next_element::<SourceArtwork>()? {
                if values.len() == 8 {
                    return Err(serde::de::Error::custom("too many artwork resources"));
                }
                values.push(value);
            }
            Ok(values)
        }
    }
    d.deserialize_seq(Art)
}
#[derive(Clone, PartialEq, Eq)]
pub struct SourceArtReference {
    pub server_id: Uuid,
    pub catalogue_epoch: Uuid,
    pub grant_id: Uuid,
    pub library_id: SourceId,
    pub item_id: SourceId,
    pub kind: ArtKind,
    pub variant: ArtVariant,
    pub expires_at_ms: i64,
}
#[derive(Clone, PartialEq, Eq)]
pub struct ReceiverArtReference {
    pub item: SharedReference,
    pub user_id: i64,
    pub lifecycle_generation: i64,
    pub source: SourceArtResource,
}

/// Diagnostic formatting redacts the opaque token, including nested DTOs.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct SourceArtResource(String);
impl std::fmt::Debug for SourceArtResource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SourceArtResource([redacted])")
    }
}
impl SourceArtResource {
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn parse(value: &str) -> Result<Self, StoreError> {
        let bytes = decode(value, SOURCE_BYTES)?;
        source_reference(&bytes)?;
        Ok(Self(value.to_owned()))
    }
    /// Syntax inspection only. B may inspect a fresh authenticated Source
    /// reply, but must not treat this as verification of the Source MAC.
    pub fn reference_unverified(&self) -> Result<SourceArtReference, StoreError> {
        source_reference(&decode(&self.0, SOURCE_BYTES)?)
    }
}
impl<'de> Deserialize<'de> for SourceArtResource {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Resource;
        impl serde::de::Visitor<'_> for Resource {
            type Value = SourceArtResource;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a closed Source artwork resource")
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                SourceArtResource::parse(value).map_err(|_| E::custom("invalid artwork resource"))
            }
        }
        d.deserialize_str(Resource)
    }
}
pub struct ReceiverArtResource {
    encoded: String,
    import: Uuid,
}
impl ReceiverArtResource {
    pub fn as_str(&self) -> &str {
        &self.encoded
    }
    pub fn url(&self) -> String {
        format!(
            "/api/v1/shared/imports/{}/art/{}",
            self.import, self.encoded
        )
    }
}
fn decode(value: &str, bytes: usize) -> Result<Vec<u8>, StoreError> {
    let encoded = (bytes * 4).div_ceil(3);
    if value.len() != encoded
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
    {
        return Err(invalid());
    }
    let result = URL_SAFE_NO_PAD.decode(value).map_err(|_| invalid())?;
    if result.len() != bytes || URL_SAFE_NO_PAD.encode(&result) != value {
        return Err(invalid());
    }
    Ok(result)
}
fn uuid(bytes: &[u8], offset: usize) -> Result<Uuid, StoreError> {
    Uuid::from_slice(&bytes[offset..offset + 16])
        .ok()
        .filter(|id| !id.is_nil())
        .ok_or_else(invalid)
}
fn integer(bytes: &[u8], offset: usize) -> Result<i64, StoreError> {
    let result = i64::from_be_bytes(
        bytes[offset..offset + 8]
            .try_into()
            .map_err(|_| invalid())?,
    );
    if result <= 0 {
        return Err(invalid());
    }
    Ok(result)
}
fn id(bytes: &[u8], offset: usize) -> Result<SourceId, StoreError> {
    SourceId::parse(&integer(bytes, offset)?.to_string())
}
fn positive_id(value: &SourceId) -> Result<i64, StoreError> {
    value
        .as_str()
        .parse::<i64>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(invalid)
}
fn clock(now: i64, expires: i64) -> Result<(), StoreError> {
    if now <= 0
        || now > MAX_CLOCK_MS
        || expires <= now
        || expires > MAX_CLOCK_MS
        || expires - now > ART_RESOURCE_LIFETIME_MS
    {
        return Err(invalid());
    }
    Ok(())
}
/// Thirty-second expiry buckets permit the existing thirty-second metadata
/// cache to reuse identical typed bytes. The derived expiry is always within
/// five minutes of the actual trusted clock, including at bucket boundaries.
pub fn artwork_expiry(now: i64) -> Result<i64, StoreError> {
    let expires = now
        .checked_div(30_000)
        .and_then(|n| n.checked_mul(30_000))
        .and_then(|n| n.checked_add(ART_RESOURCE_LIFETIME_MS))
        .ok_or_else(invalid)?;
    clock(now, expires)?;
    Ok(expires)
}
fn source_reference(bytes: &[u8]) -> Result<SourceArtReference, StoreError> {
    if bytes.len() != SOURCE_BYTES || bytes[0] != 1 {
        return Err(invalid());
    }
    let expires_at_ms = integer(bytes, 67)?;
    if expires_at_ms > MAX_CLOCK_MS {
        return Err(invalid());
    }
    Ok(SourceArtReference {
        server_id: uuid(bytes, 1)?,
        catalogue_epoch: uuid(bytes, 17)?,
        grant_id: uuid(bytes, 33)?,
        library_id: id(bytes, 49)?,
        item_id: id(bytes, 57)?,
        kind: ArtKind::parse(bytes[65])?,
        variant: ArtVariant::parse(bytes[66])?,
        expires_at_ms,
    })
}
impl CatalogueRevisionKey {
    pub fn issue_art(
        &self,
        reference: &SourceArtReference,
        now_ms: i64,
    ) -> Result<SourceArtResource, StoreError> {
        if self.art_context() != (reference.server_id, reference.catalogue_epoch)
            || reference.server_id.is_nil()
            || reference.catalogue_epoch.is_nil()
            || reference.grant_id.is_nil()
        {
            return Err(invalid());
        }
        clock(now_ms, reference.expires_at_ms)?;
        let mut bytes = Vec::with_capacity(SOURCE_BYTES);
        bytes.push(1);
        for value in [
            reference.server_id,
            reference.catalogue_epoch,
            reference.grant_id,
        ] {
            bytes.extend_from_slice(value.as_bytes());
        }
        for value in [&reference.library_id, &reference.item_id] {
            bytes.extend_from_slice(&positive_id(value)?.to_be_bytes());
        }
        bytes.extend_from_slice(&[reference.kind.byte(), reference.variant.byte()]);
        bytes.extend_from_slice(&reference.expires_at_ms.to_be_bytes());
        let signature = self.art_signature(&bytes);
        bytes.extend_from_slice(signature.as_ref());
        Ok(SourceArtResource(URL_SAFE_NO_PAD.encode(bytes)))
    }
    pub fn verify_art(
        &self,
        resource: &SourceArtResource,
        grant: Uuid,
        now_ms: i64,
    ) -> Result<SourceArtReference, StoreError> {
        let bytes = decode(resource.as_str(), SOURCE_BYTES)?;
        if !bool::from(
            bytes[SOURCE_PAYLOAD..].ct_eq(self.art_signature(&bytes[..SOURCE_PAYLOAD]).as_ref()),
        ) {
            return Err(invalid());
        }
        let reference = source_reference(&bytes)?;
        if self.art_context() != (reference.server_id, reference.catalogue_epoch)
            || reference.grant_id != grant
        {
            return Err(invalid());
        }
        clock(now_ms, reference.expires_at_ms)?;
        Ok(reference)
    }
}
impl FileLocatorKey {
    pub fn issue_art(
        &self,
        reference: &ReceiverArtReference,
        now_ms: i64,
    ) -> Result<ReceiverArtResource, StoreError> {
        let source = reference.source.reference_unverified()?;
        let item = &reference.item;
        if item.import_id.is_nil()
            || reference.user_id <= 0
            || reference.lifecycle_generation <= 0
            || source.server_id != item.server_id
            || source.catalogue_epoch != item.catalogue_epoch
            || source.library_id != item.library_id
            || source.item_id != item.item_id
        {
            return Err(invalid());
        }
        clock(now_ms, source.expires_at_ms)?;
        let (receiver, epoch) = self.art_context();
        let mut bytes = Vec::with_capacity(RECEIVER_BYTES);
        bytes.push(1);
        for value in [receiver, epoch, item.import_id] {
            bytes.extend_from_slice(value.as_bytes());
        }
        bytes.extend_from_slice(&reference.user_id.to_be_bytes());
        bytes.extend_from_slice(&reference.lifecycle_generation.to_be_bytes());
        bytes.extend_from_slice(&decode(reference.source.as_str(), SOURCE_BYTES)?);
        let signature = self.art_signature(&bytes);
        bytes.extend_from_slice(signature.as_ref());
        Ok(ReceiverArtResource {
            encoded: URL_SAFE_NO_PAD.encode(bytes),
            import: item.import_id,
        })
    }
    pub fn verify_art(
        &self,
        encoded: &str,
        import: Uuid,
        user: i64,
        now_ms: i64,
    ) -> Result<ReceiverArtReference, StoreError> {
        let bytes = decode(encoded, RECEIVER_BYTES)?;
        if bytes[0] != 1
            || !bool::from(
                bytes[RECEIVER_PAYLOAD..]
                    .ct_eq(self.art_signature(&bytes[..RECEIVER_PAYLOAD]).as_ref()),
            )
            || self.art_context() != (uuid(&bytes, 1)?, uuid(&bytes, 17)?)
            || uuid(&bytes, 33)? != import
            || integer(&bytes, 49)? != user
        {
            return Err(invalid());
        }
        let source =
            SourceArtResource::parse(&URL_SAFE_NO_PAD.encode(&bytes[65..RECEIVER_PAYLOAD]))?;
        let reference = source.reference_unverified()?;
        clock(now_ms, reference.expires_at_ms)?;
        Ok(ReceiverArtReference {
            item: SharedReference {
                import_id: import,
                server_id: reference.server_id,
                catalogue_epoch: reference.catalogue_epoch,
                library_id: reference.library_id,
                item_id: reference.item_id,
            },
            user_id: user,
            lifecycle_generation: integer(&bytes, 57)?,
            source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{secrets::CredentialKey, sharing::SharingIdentity};
    #[test]
    fn sharing_art_resources_bind_full_identity_user_lifecycle_and_expiry_through_rewrap() {
        let identity = |id| SharingIdentity {
            server_id: id,
            catalogue_epoch: Uuid::new_v4(),
            created_at_ms: 1000,
        };
        let source = identity(Uuid::new_v4());
        let receiver = identity(Uuid::new_v4());
        let old = CredentialKey::from_bytes([13; 32]);
        let new = CredentialKey::from_bytes([14; 32]);
        let envelope =
            CatalogueRevisionKey::generate_sealed(&old, source.clone()).expect("Source material");
        let source_key =
            CatalogueRevisionKey::open(&old, source.clone(), &envelope).expect("Source key");
        let receiver_envelope =
            FileLocatorKey::generate_sealed(&old, &receiver).expect("B material");
        let receiver_key =
            FileLocatorKey::open(&old, &receiver, &receiver_envelope).expect("B key");
        let now = 1_700_000_000_001;
        let reference = SourceArtReference {
            server_id: source.server_id,
            catalogue_epoch: source.catalogue_epoch,
            grant_id: Uuid::new_v4(),
            library_id: SourceId::parse("9007199254740993").expect("library"),
            item_id: SourceId::parse("9223372036854775807").expect("item"),
            kind: ArtKind::Poster,
            variant: ArtVariant::W300,
            expires_at_ms: artwork_expiry(now).expect("trusted expiry"),
        };
        let resource = source_key
            .issue_art(&reference, now)
            .expect("Source resource");
        assert!(source_key
            .verify_art(&resource, reference.grant_id, now)
            .is_ok());
        assert!(source_key
            .verify_art(&resource, Uuid::new_v4(), now)
            .is_err());
        assert!(source_key
            .verify_art(&resource, reference.grant_id, reference.expires_at_ms)
            .is_err());
        let item = SharedReference {
            import_id: Uuid::new_v4(),
            server_id: source.server_id,
            catalogue_epoch: source.catalogue_epoch,
            library_id: reference.library_id.clone(),
            item_id: reference.item_id.clone(),
        };
        let b_reference = ReceiverArtReference {
            item: item.clone(),
            user_id: 42,
            lifecycle_generation: 7,
            source: resource.clone(),
        };
        let item_json = serde_json::json!({"item_id":reference.item_id,"library_id":reference.library_id,"parent_id":null,"kind":"movie","title":"Fixture","sort_title":"fixture","year":null,"overview":null,"genres":[],"season_number":null,"episode_number":null,"art":[{"kind":reference.kind,"variant":reference.variant,"resource":resource}]});
        let typed: crate::sharing_catalogue::SourceCatalogueItem =
            serde_json::from_value(item_json.clone()).expect("closed artwork wire");
        assert!(typed.validate().is_ok());
        let mut bad = item_json.clone();
        bad["art"] = serde_json::json!((0..9)
            .map(|_| item_json["art"][0].clone())
            .collect::<Vec<_>>());
        assert!(
            serde_json::from_value::<crate::sharing_catalogue::SourceCatalogueItem>(bad).is_err()
        );
        let mut bad = item_json.clone();
        bad["art"][0]["path"] = serde_json::json!("/private/never");
        assert!(
            serde_json::from_value::<crate::sharing_catalogue::SourceCatalogueItem>(bad).is_err()
        );
        let mut bad = item_json.clone();
        bad["art"][0]["variant"] = serde_json::json!("original");
        assert!(
            serde_json::from_value::<crate::sharing_catalogue::SourceCatalogueItem>(bad)
                .expect("closed enum")
                .validate()
                .is_err()
        );
        let mut bad = item_json.clone();
        bad["library_id"] = serde_json::json!("1");
        assert!(
            serde_json::from_value::<crate::sharing_catalogue::SourceCatalogueItem>(bad)
                .expect("closed ID")
                .validate()
                .is_err()
        );
        let b = receiver_key
            .issue_art(&b_reference, now)
            .expect("B resource");
        let checked = receiver_key
            .verify_art(b.as_str(), item.import_id, 42, now)
            .expect("B verification");
        assert_eq!(checked.lifecycle_generation, 7);
        assert!(checked.item == item);
        assert!(checked.source == resource);
        assert!(receiver_key
            .verify_art(b.as_str(), item.import_id, 43, now)
            .is_err());
        assert!(receiver_key
            .verify_art(b.as_str(), Uuid::new_v4(), 42, now)
            .is_err());
        assert!(receiver_key
            .verify_art(b.as_str(), item.import_id, 42, reference.expires_at_ms)
            .is_err());
        assert!(!format!("{resource:?}").contains(resource.as_str()));
        for changed in 0..SOURCE_PAYLOAD {
            let mut bytes = decode(resource.as_str(), SOURCE_BYTES).expect("bytes");
            bytes[changed] ^= 1;
            let changed = SourceArtResource(URL_SAFE_NO_PAD.encode(bytes));
            assert!(source_key
                .verify_art(&changed, reference.grant_id, now)
                .is_err());
        }
        for value in [
            "".to_owned(),
            "x".repeat(4096),
            format!("{}=", resource.as_str()),
            resource.as_str().replace('-', "+"),
        ] {
            if value != resource.as_str() {
                assert!(SourceArtResource::parse(&value).is_err());
            }
        }
        assert!(SourceArtResource::parse(b.as_str()).is_err());
        assert!(receiver_key
            .verify_art(resource.as_str(), item.import_id, 42, now)
            .is_err());
        let clear = old
            .open_sharing(
                crate::secrets::SharingSecretPurpose::CatalogueRevision,
                source.server_id,
                source.catalogue_epoch,
                &envelope,
            )
            .expect("Source rewrap input");
        let rewrapped = new
            .seal_sharing(
                crate::secrets::SharingSecretPurpose::CatalogueRevision,
                source.server_id,
                source.catalogue_epoch,
                clear.expose(),
            )
            .expect("Source rewrap envelope");
        let source_after =
            CatalogueRevisionKey::open(&new, source, &rewrapped).expect("rewrapped Source");
        assert!(source_after.issue_art(&reference, now).expect("stable") == resource);
        let clear = old
            .open_sharing(
                crate::secrets::SharingSecretPurpose::FileLocator,
                receiver.server_id,
                receiver.catalogue_epoch,
                &receiver_envelope,
            )
            .expect("B rewrap input");
        let envelope = new
            .seal_sharing(
                crate::secrets::SharingSecretPurpose::FileLocator,
                receiver.server_id,
                receiver.catalogue_epoch,
                clear.expose(),
            )
            .expect("B rewrap envelope");
        let after = FileLocatorKey::open(&new, &receiver, &envelope).expect("same B material");
        assert_eq!(
            after
                .issue_art(&b_reference, now)
                .expect("stable B")
                .as_str(),
            b.as_str()
        );
        for changed in 0..RECEIVER_PAYLOAD {
            let mut bytes = decode(b.as_str(), RECEIVER_BYTES).expect("B bytes");
            bytes[changed] ^= 1;
            assert!(after
                .verify_art(&URL_SAFE_NO_PAD.encode(bytes), item.import_id, 42, now)
                .is_err());
        }
        for time in [0, -1, MAX_CLOCK_MS, i64::MAX] {
            assert!(artwork_expiry(time).is_err());
        }
    }
}
