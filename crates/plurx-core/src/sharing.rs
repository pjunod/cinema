//! Sharing identities and bounded protocol values. Remote IDs never become local IDs.
use crate::{error::StoreError, secrets::Secret};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use uuid::Uuid;

pub const MAX_PEERS: i64 = 32;
pub const MAX_LIBRARIES: usize = 64;
pub const MAX_INVITATION_TTL_MS: i64 = 7 * 24 * 60 * 60 * 1000;
pub const PENDING_TTL_MS: i64 = 24 * 60 * 60 * 1000;
pub const ROTATION_TTL_MS: i64 = 10 * 60 * 1000;

pub(crate) fn invalid() -> StoreError {
    StoreError::Database("invalid sharing request".into())
}

/// Canonical decimal wire identity, preserving integers above JavaScript's safe range.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct SourceId(String);
impl SourceId {
    pub fn parse(value: &str) -> Result<Self, StoreError> {
        let id = value.parse::<i64>().map_err(|_| invalid())?;
        if id < 0 || id.to_string() != value {
            return Err(invalid());
        }
        Ok(Self(value.into()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl<'de> Deserialize<'de> for SourceId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::parse(&String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharingIdentity {
    pub server_id: Uuid,
    pub catalogue_epoch: Uuid,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Endpoint {
    pub ipv4: Ipv4Addr,
    pub ipv6: Option<Ipv6Addr>,
    pub ts_fqdn: String,
    pub port: u16,
    pub spki_sha256: String,
}
pub fn is_tailnet_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let bytes = ip.octets();
            bytes[0] == 100 && (64..=127).contains(&bytes[1])
        }
        IpAddr::V6(ip) => ip.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
    }
}
pub fn validate_tailnet_name(name: &str) -> Result<(), StoreError> {
    let labels = name.strip_suffix(".ts.net").ok_or_else(invalid)?;
    if labels.len() > 240
        || labels.split('.').count() < 2
        || labels.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
    {
        return Err(invalid());
    }
    Ok(())
}
impl Endpoint {
    pub fn validate(&self) -> Result<(), StoreError> {
        validate_tailnet_name(&self.ts_fqdn)?;
        if !is_tailnet_address(self.ipv4.into())
            || self.ipv6.is_some_and(|ip| !is_tailnet_address(ip.into()))
            || self.port == 0
            || !is_hash(&self.spki_sha256)
        {
            return Err(invalid());
        }
        Ok(())
    }
}
pub fn validate_endpoints(endpoints: &[Endpoint]) -> Result<(), StoreError> {
    if endpoints.is_empty() || endpoints.len() > 4 {
        return Err(invalid());
    }
    for e in endpoints {
        e.validate()?;
    }
    Ok(())
}
pub(crate) fn is_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Debug, Clone, Copy)]
pub enum SecretDomain {
    Invitation,
    Grant,
    Delivery,
}
impl SecretDomain {
    fn tag(self) -> &'static [u8] {
        match self {
            Self::Invitation => b"sharing-invite-v1",
            Self::Grant => b"sharing-grant-v1",
            Self::Delivery => b"sharing-delivery-v1",
        }
    }
}
/// Verification hashes are domain-separated; comparisons do not short-circuit.
pub fn secret_hash(domain: SecretDomain, secret: &Secret) -> String {
    let mut h = Sha256::new();
    h.update(domain.tag());
    h.update([0]);
    h.update(secret.expose().as_bytes());
    hex::encode(h.finalize())
}
pub fn verify_secret(domain: SecretDomain, secret: &Secret, hash: &str) -> bool {
    let computed = secret_hash(domain, secret);
    {
        use subtle::ConstantTimeEq;
        bool::from(computed.as_bytes().ct_eq(hash.as_bytes()))
    }
}
/// Length-prefixed encoding is shared by pairing codes and viewer pseudonyms.
pub fn digest_parts(parts: &[&[u8]]) -> String {
    let mut h = Sha256::new();
    for part in parts {
        h.update((part.len() as u64).to_be_bytes());
        h.update(part);
    }
    hex::encode(h.finalize())
}
pub fn viewer_key(import: Uuid, viewer: Uuid) -> String {
    digest_parts(&[import.to_string().as_bytes(), viewer.to_string().as_bytes()])
}
pub fn pairing_code(
    a: Uuid,
    b: Uuid,
    invitation: Uuid,
    claim: Uuid,
    credential_hash: &str,
) -> String {
    digest_parts(&[
        b"cinema-pair-v1",
        a.to_string().as_bytes(),
        b.to_string().as_bytes(),
        invitation.to_string().as_bytes(),
        claim.to_string().as_bytes(),
        credential_hash.as_bytes(),
    ])[..16]
        .into()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantState {
    Pending,
    Active,
    Disabled,
    Revoked,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportGrant {
    pub id: Uuid,
    pub recipient_server_id: Uuid,
    pub state: GrantState,
    pub scope_generation: i64,
    pub credential_generation: i64,
    pub catalogue_generation: i64,
    pub mutation_generation: i64,
    pub pending_expires_at_ms: i64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimOutcome {
    Created(ExportGrant),
    Replay(ExportGrant),
    Consumed,
    Expired,
    Cancelled,
    Capacity,
    NotFound,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationOutcome {
    Applied,
    Conflict,
    NotFound,
    Expired,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Authorization {
    Allowed(ExportGrant),
    Pending,
    Denied,
}

#[derive(Debug, Clone)]
pub struct InvitationRecord {
    pub id: Uuid,
    pub token_hash: String,
    pub library_ids: Vec<i64>,
    pub created_at_ms: i64,
    pub expires_at_ms: i64,
}
#[derive(Debug, Clone)]
pub struct ShareClaim {
    pub invitation_id: Uuid,
    pub invitation_hash: String,
    pub claim_id: Uuid,
    pub grant_id: Uuid,
    pub recipient_server_id: Uuid,
    pub recipient_name: String,
    pub credential_hash: String,
    pub now_ms: i64,
}
impl ShareClaim {
    pub(crate) fn validate(&self) -> Result<(), StoreError> {
        if !is_hash(&self.invitation_hash)
            || !is_hash(&self.credential_hash)
            || self.recipient_name.len() > 128
            || self.recipient_name.chars().any(char::is_control)
            || self.now_ms < 0
            || self.now_ms.checked_add(PENDING_TTL_MS).is_none()
        {
            return Err(invalid());
        }
        Ok(())
    }
    pub(crate) fn digest(&self) -> String {
        digest_parts(&[
            self.claim_id.to_string().as_bytes(),
            self.recipient_server_id.to_string().as_bytes(),
            self.recipient_name.as_bytes(),
            self.credential_hash.as_bytes(),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sharing_wire_ids_preserve_huge_values_and_reject_aliases() {
        let id: SourceId =
            serde_json::from_str("\"9007199254740993\"").expect("synthetic sharing fixture");
        assert_eq!(id.as_str(), "9007199254740993");
        for value in ["01", "-1", "+1", "9223372036854775808"] {
            assert!(SourceId::parse(value).is_err());
        }
        assert!(serde_json::from_str::<SourceId>("9007199254740993").is_err());
    }
    #[test]
    fn sharing_endpoint_hints_reject_public_addresses_and_url_escapes() {
        let mut e = Endpoint {
            ipv4: "100.101.102.103"
                .parse()
                .expect("synthetic sharing fixture"),
            ipv6: None,
            ts_fqdn: "cinema.example.ts.net".into(),
            port: 32443,
            spki_sha256: "a".repeat(64),
        };
        assert!(e.validate().is_ok());
        e.ipv4 = "192.0.2.1".parse().expect("synthetic sharing fixture");
        assert!(e.validate().is_err());
        e.ipv4 = "100.101.102.103"
            .parse()
            .expect("synthetic sharing fixture");
        for name in [
            "example.org",
            "http://cinema.example.ts.net",
            "cinema.ts.net",
            "cinema.example.ts.net.evil",
        ] {
            e.ts_fqdn = name.into();
            assert!(e.validate().is_err());
        }
    }
    #[test]
    fn sharing_verification_domains_and_viewer_identity_are_distinct() {
        let token = Secret::from_cleartext("synthetic-secret");
        let hash = secret_hash(SecretDomain::Invitation, &token);
        assert!(verify_secret(SecretDomain::Invitation, &token, &hash));
        assert!(!verify_secret(SecretDomain::Grant, &token, &hash));
        assert_ne!(
            viewer_key(Uuid::from_u128(1), Uuid::from_u128(2)),
            viewer_key(Uuid::from_u128(2), Uuid::from_u128(1))
        );
        assert!(!format!("{token:?}").contains("synthetic"));
    }
}

/// The admin import handler constructs this only after pinned identity verification.
#[derive(Debug, Clone)]
pub struct NewImport {
    pub id: Uuid,
    pub source: SharingIdentity,
    pub source_name: String,
    pub claim_id: Uuid,
    pub credential: crate::secrets::SealedSecret,
    pub claim_secret: crate::secrets::SealedSecret,
    pub endpoints: Vec<Endpoint>,
    pub now_ms: i64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportOutcome {
    Created,
    AlreadyImported(Uuid),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Assignment {
    pub library_id: SourceId,
    pub user_id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportSummary {
    pub grant: ExportGrant,
    pub recipient_name: String,
    pub invitation_id: Uuid,
    pub claim_id: Uuid,
    pub library_ids: Vec<SourceId>,
    pub pairing_code: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportSummary {
    pub id: Uuid,
    pub source_server_id: Uuid,
    pub catalogue_epoch: Uuid,
    pub source_name: String,
    pub claim_id: Uuid,
    pub remote_grant_id: Option<Uuid>,
    pub state: String,
    pub assignment_generation: i64,
    pub lifecycle_generation: i64,
    pub endpoint_generation: i64,
    pub observed_endpoint_revision: Option<i64>,
    pub endpoints: Vec<Endpoint>,
}
/// Durable ciphertext is deliberately separated from the serializable status DTO.
#[derive(Debug, Clone)]
pub struct StoredImport {
    pub summary: ImportSummary,
    pub credential: crate::secrets::SealedSecret,
    pub claim: Option<crate::secrets::SealedSecret>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndpointManifest {
    pub revision: i64,
    pub endpoints: Vec<Endpoint>,
}

/// New capabilities are 256 bits; base64url has one canonical spelling.
pub fn new_secret() -> Result<Secret, StoreError> {
    use base64::Engine;
    let mut bytes = zeroize::Zeroizing::new([0u8; 32]);
    getrandom::getrandom(bytes.as_mut()).map_err(|_| invalid())?;
    Ok(Secret::from_cleartext(
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes.as_ref()),
    ))
}
pub fn validate_secret(secret: &Secret) -> Result<(), StoreError> {
    use base64::Engine;
    let decoded = zeroize::Zeroizing::new(
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(secret.expose())
            .map_err(|_| invalid())?,
    );
    if decoded.len() != 32 {
        return Err(invalid());
    }
    Ok(())
}

pub fn canonical_uuid<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Uuid, D::Error> {
    let value = String::deserialize(d)?;
    let id = Uuid::parse_str(&value).map_err(serde::de::Error::custom)?;
    if id.to_string() != value {
        return Err(serde::de::Error::custom("noncanonical sharing UUID"));
    }
    Ok(id)
}
fn bootstrap_secret<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Secret, D::Error> {
    String::deserialize(d).map(Secret::from_cleartext)
}
pub fn wire_secret<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Secret, D::Error> {
    let secret = bootstrap_secret(d)?;
    validate_secret(&secret).map_err(|_| serde::de::Error::custom("invalid sharing secret"))?;
    Ok(secret)
}

/// The bootstrap blob is deliberately not Serializable or printable.
#[derive(Debug)]
pub struct Invitation {
    pub identity: SharingIdentity,
    pub name: String,
    pub endpoints: Vec<Endpoint>,
    pub id: Uuid,
    pub secret: Secret,
    pub expires_at_ms: i64,
}
impl Invitation {
    pub fn encode(&self) -> Result<Secret, StoreError> {
        use base64::Engine;
        validate_endpoints(&self.endpoints)?;
        validate_secret(&self.secret)?;
        #[derive(Serialize)]
        struct Wire<'a> {
            version: u8,
            server_id: Uuid,
            catalogue_epoch: Uuid,
            name: &'a str,
            endpoints: &'a [Endpoint],
            invitation_id: Uuid,
            secret: &'a str,
            expires_at_ms: i64,
        }
        let wire = Wire {
            version: 1,
            server_id: self.identity.server_id,
            catalogue_epoch: self.identity.catalogue_epoch,
            name: &self.name,
            endpoints: &self.endpoints,
            invitation_id: self.id,
            secret: self.secret.expose(),
            expires_at_ms: self.expires_at_ms,
        };
        let bytes = zeroize::Zeroizing::new(serde_json::to_vec(&wire).map_err(|_| invalid())?);
        if bytes.len() > 8192 {
            return Err(invalid());
        }
        Ok(Secret::from_cleartext(format!(
            "cinema-share-v1:{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes.as_slice())
        )))
    }
    pub fn parse(blob: &Secret) -> Result<Self, StoreError> {
        use base64::Engine;
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            version: u8,
            #[serde(deserialize_with = "canonical_uuid")]
            server_id: Uuid,
            #[serde(deserialize_with = "canonical_uuid")]
            catalogue_epoch: Uuid,
            name: String,
            endpoints: Vec<Endpoint>,
            #[serde(deserialize_with = "canonical_uuid")]
            invitation_id: Uuid,
            #[serde(deserialize_with = "bootstrap_secret")]
            secret: Secret,
            expires_at_ms: i64,
        }
        let encoded = blob
            .expose()
            .strip_prefix("cinema-share-v1:")
            .ok_or_else(invalid)?;
        if encoded.len() > 10923 {
            return Err(invalid());
        }
        let bytes = zeroize::Zeroizing::new(
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(encoded)
                .map_err(|_| invalid())?,
        );
        if bytes.len() > 8192 {
            return Err(invalid());
        }
        let w: Wire = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        if w.version != 1
            || w.name.len() > 128
            || w.name.chars().any(char::is_control)
            || w.expires_at_ms < 0
        {
            return Err(invalid());
        }
        validate_endpoints(&w.endpoints)?;
        let secret = w.secret;
        validate_secret(&secret)?;
        Ok(Self {
            identity: SharingIdentity {
                server_id: w.server_id,
                catalogue_epoch: w.catalogue_epoch,
                created_at_ms: 0,
            },
            name: w.name,
            endpoints: w.endpoints,
            id: w.invitation_id,
            secret,
            expires_at_ms: w.expires_at_ms,
        })
    }
}

#[cfg(test)]
mod protocol_tests {
    use super::*;
    #[test]
    fn sharing_shared_protocol_fixture_validates_exact_wire_ids() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../../tests/sharing/protocol-cases.json"))
                .expect("synthetic sharing fixture");
        for case in fixture["cases"]
            .as_array()
            .expect("synthetic sharing fixture")
        {
            let result = SourceId::parse(
                case["input"]["source_id"]
                    .as_str()
                    .expect("synthetic sharing fixture"),
            );
            assert_eq!(
                result.is_ok(),
                case["expected"] == "accepted",
                "{}",
                case["id"]
            );
        }
    }
    #[test]
    fn sharing_invitation_is_bounded_and_redacts_bootstrap_material() {
        let i = Invitation {
            identity: SharingIdentity {
                server_id: Uuid::new_v4(),
                catalogue_epoch: Uuid::new_v4(),
                created_at_ms: 0,
            },
            name: "Synthetic".into(),
            endpoints: vec![Endpoint {
                ipv4: "100.101.102.103"
                    .parse()
                    .expect("synthetic sharing fixture"),
                ipv6: None,
                ts_fqdn: "source.example.ts.net".into(),
                port: 32443,
                spki_sha256: "a".repeat(64),
            }],
            id: Uuid::new_v4(),
            secret: new_secret().expect("synthetic sharing fixture"),
            expires_at_ms: 10000,
        };
        let blob = i.encode().expect("synthetic sharing fixture");
        let parsed = Invitation::parse(&blob).expect("synthetic sharing fixture");
        assert_eq!(parsed.id, i.id);
        assert_eq!(parsed.secret, i.secret);
        assert!(!format!("{i:?}").contains(i.secret.expose()));
        assert!(!format!("{blob:?}").contains(blob.expose()));
        assert!(Invitation::parse(&Secret::from_cleartext(format!(
            "cinema-share-v1:{}",
            "a".repeat(10924)
        )))
        .is_err());
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportRotation {
    pub import_id: Uuid,
    pub request_id: Uuid,
    pub credential: crate::secrets::SealedSecret,
    pub lifecycle_generation: i64,
    pub now_ms: i64,
}
#[derive(Debug, Clone)]
pub struct StoredImportRotation {
    pub request_id: Uuid,
    pub credential: crate::secrets::SealedSecret,
    pub expires_at_ms: i64,
}
#[derive(Debug, Clone)]
pub struct ImportClaimReceipt {
    pub import_id: Uuid,
    pub claim_id: Uuid,
    pub lifecycle_generation: i64,
    pub grant_id: Uuid,
    pub active: bool,
    pub credential: crate::secrets::SealedSecret,
    pub now_ms: i64,
}
