//! Opaque wire identity; allocation and entity retirement belong to Store.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct WireId(Uuid);

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("expected a nonzero 32-hex or dashed UUID")]
pub struct InvalidWireId;

impl WireId {
    pub fn random() -> Self {
        Self(Uuid::new_v4())
    }
    pub fn parse(value: &str) -> Result<Self, InvalidWireId> {
        if !matches!(value.len(), 32 | 36) {
            return Err(InvalidWireId);
        }
        let id = Uuid::parse_str(value).map_err(|_| InvalidWireId)?;
        if id.is_nil() {
            return Err(InvalidWireId);
        }
        Ok(Self(id))
    }
    pub fn to_hex(self) -> String {
        self.0.simple().to_string()
    }
}
impl TryFrom<String> for WireId {
    type Error = InvalidWireId;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}
impl From<WireId> for String {
    fn from(value: WireId) -> Self {
        value.to_hex()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wire_id_normalizes_spelling_and_rejects_nonidentity_forms() {
        let dashed = "12345678-9ABC-4DEF-8123-456789ABCDEF";
        let id = WireId::parse(dashed).expect("valid UUID");
        assert_eq!(id.to_hex(), "123456789abc4def8123456789abcdef");
        assert_eq!(id, WireId::parse(&id.to_hex()).expect("simple UUID"));
        for invalid in [
            "1",
            "00000000000000000000000000000000",
            "urn:uuid:12345678-9abc-4def-8123-456789abcdef",
            "123456789abc4def8123456789abcdeg",
        ] {
            assert!(WireId::parse(invalid).is_err());
        }
        assert!(serde_json::from_str::<WireId>("\"1\"").is_err());
        assert_eq!(
            serde_json::to_string(&id).expect("serialize"),
            format!("\"{}\"", id.to_hex())
        );
    }
}
