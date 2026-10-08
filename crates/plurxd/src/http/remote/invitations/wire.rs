//! Strict home invitation envelopes. Provider tokens never enter this API.
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub enum Version {
    #[serde(rename = "cinema.invitation.v1")]
    V1,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Register {
    pub version: Version,
    pub installation_id: String,
    pub platform: String,
    pub name: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhoneList {
    pub version: Version,
    pub after_id: Option<String>,
    pub limit: u8,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Availability {
    pub version: Version,
    #[serde(deserialize_with = "positive_generation")]
    pub expected_phone_generation: i64,
    pub permission_granted: bool,
    pub resident_active: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rebind {
    pub version: Version,
    #[serde(deserialize_with = "positive_generation")]
    pub expected_phone_generation: i64,
}

pub trait Envelope {
    fn version(&self) -> Version;
}
macro_rules! envelope {($($name:ident),*)=>{$(impl Envelope for $name{fn version(&self)->Version{self.version}})*};}
envelope!(Register, PhoneList, Availability, Rebind);

// Deserialize through u64 to reject negative and floating JSON lexemes, including
// -0 and exponent notation, before the generation reaches an authority CAS.
fn positive_generation<'de, D: serde::Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    let value = u64::deserialize(d)?;
    if !(1..=9_007_199_254_740_991).contains(&value) {
        return Err(serde::de::Error::custom("invalid generation"));
    }
    i64::try_from(value).map_err(serde::de::Error::custom)
}
