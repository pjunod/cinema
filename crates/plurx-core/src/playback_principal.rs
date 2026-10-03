//! Typed ownership for the existing media-session family.
//!
//! Owner keys are derived from a validated principal. They are never supplied
//! by a playback request and never converted into a foreign local account.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SharingViewerKey(String);
impl SharingViewerKey {
    pub fn parse(value: &str) -> Result<Self, PrincipalError> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(PrincipalError);
        }
        Ok(Self(value.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for SharingViewerKey {
    type Error = PrincipalError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}
impl From<SharingViewerKey> for String {
    fn from(value: SharingViewerKey) -> Self {
        value.0
    }
}
impl std::fmt::Debug for SharingViewerKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SharingViewerKey([redacted])")
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid playback principal")]
pub struct PrincipalError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlaybackPrincipal {
    #[serde(rename = "local")]
    LocalUser { user_id: i64 },
    Sharing {
        #[serde(deserialize_with = "crate::sharing::canonical_uuid")]
        grant_id: Uuid,
        viewer_key: SharingViewerKey,
    },
}
impl PlaybackPrincipal {
    pub fn sharing(grant_id: Uuid, viewer_key: &str) -> Result<Self, PrincipalError> {
        Ok(Self::Sharing {
            grant_id,
            viewer_key: SharingViewerKey::parse(viewer_key)?,
        })
    }
    pub fn owner_key(&self) -> String {
        match self {
            Self::LocalUser { user_id } => format!("local:{user_id}"),
            Self::Sharing {
                grant_id,
                viewer_key,
            } => format!("share:{grant_id}:{}", viewer_key.as_str()),
        }
    }
    /// Structural admission bounds only; this does not prove owner existence,
    /// export scope, sharing enablement or compatible cluster writers.
    pub fn valid_admission_shape(&self) -> bool {
        match self {
            Self::LocalUser { user_id } => *user_id > 0,
            Self::Sharing { .. } => true,
        }
    }
    pub fn local_user_id(&self) -> Option<i64> {
        match self {
            Self::LocalUser { user_id } => Some(*user_id),
            Self::Sharing { .. } => None,
        }
    }
    /// Validate the complete projection read from a row, including the stored
    /// key. A nullable user projection is never interpreted as a local user.
    pub fn from_projection(
        kind: &str,
        user_id: Option<i64>,
        grant: Option<&str>,
        viewer: Option<&str>,
        owner_key: &str,
    ) -> Result<Self, PrincipalError> {
        let principal = match (kind, user_id, grant, viewer) {
            ("local", Some(user_id), None, None) => Self::LocalUser { user_id },
            ("sharing", None, Some(grant), Some(viewer)) => {
                let id = Uuid::parse_str(grant).map_err(|_| PrincipalError)?;
                if id.to_string() != grant {
                    return Err(PrincipalError);
                }
                Self::sharing(id, viewer)?
            }
            _ => return Err(PrincipalError),
        };
        if principal.owner_key() != owner_key {
            return Err(PrincipalError);
        }
        Ok(principal)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sharing_principal_keys_do_not_collide_or_adopt_a_foreign_local_user() {
        let grant = Uuid::new_v4();
        let first = PlaybackPrincipal::sharing(grant, &"a".repeat(64)).expect("viewer");
        let second = PlaybackPrincipal::sharing(grant, &"b".repeat(64)).expect("other viewer");
        let other =
            PlaybackPrincipal::sharing(Uuid::new_v4(), &"a".repeat(64)).expect("other grant");
        assert_ne!(first.owner_key(), second.owner_key());
        assert_ne!(first.owner_key(), other.owner_key());
        assert_eq!(first.local_user_id(), None);
        let local = PlaybackPrincipal::LocalUser { user_id: i64::MIN };
        assert_eq!(local.owner_key(), "local:-9223372036854775808");
        assert_ne!(local.owner_key(), first.owner_key());
        for bad in [
            "A".repeat(64),
            "a".repeat(63),
            format!("{}:", "a".repeat(63)),
        ] {
            assert!(PlaybackPrincipal::sharing(grant, &bad).is_err());
        }
        assert!(!format!("{first:?}").contains(&"a".repeat(64)));
    }
    #[test]
    fn sharing_principal_decoder_refuses_mixed_or_noncanonical_ownership_projections() {
        let grant = Uuid::new_v4().to_string();
        let viewer = "e".repeat(64);
        let key = format!("share:{grant}:{viewer}");
        assert!(PlaybackPrincipal::from_projection(
            "sharing",
            None,
            Some(&grant),
            Some(&viewer),
            &key
        )
        .is_ok());
        assert!(PlaybackPrincipal::from_projection(
            "sharing",
            Some(1),
            Some(&grant),
            Some(&viewer),
            &key
        )
        .is_err());
        assert!(
            PlaybackPrincipal::from_projection("local", Some(1), None, None, "local:01").is_err()
        );
        assert!(PlaybackPrincipal::from_projection("local", None, None, None, "local:0").is_err());
        assert!(PlaybackPrincipal::from_projection(
            "sharing",
            None,
            Some(&grant),
            Some(&viewer),
            "local:1"
        )
        .is_err());
        assert!(PlaybackPrincipal::from_projection(
            "sharing",
            None,
            Some(&grant.to_uppercase()),
            Some(&viewer),
            &key
        )
        .is_err());
        assert!(serde_json::from_value::<PlaybackPrincipal>(
            serde_json::json!({"kind":"sharing","grant_id":grant,
            "viewer_key":viewer,"user_id":1})
        )
        .is_err());
    }
}
