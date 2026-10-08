use super::hiqlite::{database_error, HiqliteAuthStore};
use crate::{error::StoreError, store::invitations::*};
use async_trait::async_trait;
use hiqlite::{macros::params, Row};
impl From<&mut Row<'_>> for InvitationPhone {
    fn from(r: &mut Row<'_>) -> Self {
        Self {
            id: r.get("id"),
            user_id: r.get("user_id"),
            name: r.get("name"),
            platform: r.get("platform"),
            generation: r.get("generation"),
            created_at: r.get("created_at"),
            permission_granted: r.get::<i64>("permission_granted") == 1,
            resident_active: r.get::<i64>("resident_active") == 1,
        }
    }
}
impl From<&mut Row<'_>> for InvitationConsent {
    fn from(r: &mut Row<'_>) -> Self {
        Self {
            id: r.get("id"),
            phone_id: r.get("phone_id"),
            receiver_id: r.get("receiver_id"),
            user_id: r.get("user_id"),
            grant_id: r.get("grant_id"),
            enabled: r.get::<i64>("enabled") == 1,
            transport: r.get("transport"),
            generation: r.get("generation"),
            transport_generation: r.get("transport_generation"),
            broker_enrollment: r.get("broker_enrollment"),
            broker_ticket: r.get("broker_ticket"),
            transport_status: r.get("transport_status"),
            transport_phone_generation: r.get("transport_phone_generation"),
        }
    }
}
impl From<&mut Row<'_>> for InvitationLogin {
    fn from(r: &mut Row<'_>) -> Self {
        Self {
            user_id: r.get("user_id"),
            digest: String::new(),
            last_seen_at: r.get("last_seen_at"),
            expiry_enabled: r.get("expiry_enabled"),
            expiry_days: r.get("expiry_days"),
            expiry_since: r.get("expiry_since"),
        }
    }
}
#[async_trait]
impl InvitationStore for HiqliteAuthStore {
    async fn create_invitation_phone(&self, n: NewInvitationPhone) -> Result<bool, StoreError> {
        n.validate()?;
        let p = n.phone;
        Ok(self
            .execute(
                CREATE_PHONE,
                params!(
                    p.id,
                    p.user_id,
                    p.name,
                    p.platform,
                    n.secret_hash,
                    n.token_digest,
                    p.created_at
                ),
            )
            .await?
            > 0)
    }
    async fn invitation_phone(
        &self,
        id: &str,
        user: i64,
    ) -> Result<Option<InvitationPhone>, StoreError> {
        Ok(self
            .client()
            .query_consistent_map::<InvitationPhone, _>(PHONE_METADATA, params!(id, user))
            .await?
            .pop())
    }
    async fn invitation_phone_authority(
        &self,
        id: &str,
        user: i64,
        hash: &str,
    ) -> Result<Option<InvitationPhone>, StoreError> {
        if !valid_id(id) || !valid_digest(hash) {
            return Err(StoreError::Identity("invalid phone proof".into()));
        }
        Ok(self
            .client()
            .query_consistent_map::<InvitationPhone, _>(PHONE_AUTHORITY, params!(id, user, hash))
            .await?
            .pop())
    }
    async fn invitation_phones(
        &self,
        user: i64,
        after: &str,
    ) -> Result<Vec<InvitationPhone>, StoreError> {
        self.client()
            .query_consistent_map::<InvitationPhone, _>(LIST_PHONES, params!(user, after))
            .await
            .map_err(database_error)
    }
    async fn set_invitation_availability(
        &self,
        id: &str,
        user: i64,
        hash: &str,
        expected: i64,
        permission: bool,
        resident: bool,
    ) -> Result<bool, StoreError> {
        if !valid_id(id) || !valid_digest(hash) || expected <= 0 || (!permission && resident) {
            return Err(StoreError::Identity("invalid phone availability".into()));
        }
        Ok(self
            .execute(
                AVAILABILITY,
                params!(
                    i64::from(permission),
                    i64::from(resident),
                    id,
                    user,
                    hash,
                    expected
                ),
            )
            .await?
            > 0)
    }
    async fn revoke_invitation_phone(&self, id: &str, user: i64) -> Result<(), StoreError> {
        self.execute(DELETE_PHONE, params!(id, user)).await?;
        Ok(())
    }
    async fn save_invitation_consent(&self, r: SaveInvitationConsent) -> Result<bool, StoreError> {
        r.validate()?;
        Ok(if let Some((gid, gh, transport)) = r.enable {
            self.execute(
                CONSENT_ENABLE,
                params!(
                    r.id,
                    r.phone_id,
                    r.receiver_id,
                    r.user_id,
                    r.phone_hash,
                    r.expected_generation,
                    gid,
                    gh,
                    transport.as_str(),
                    r.expected_phone_generation
                ),
            )
            .await?
                > 0
        } else {
            self.execute(
                CONSENT_DISABLE,
                params!(
                    r.phone_id,
                    r.receiver_id,
                    r.user_id,
                    r.phone_hash,
                    r.expected_generation,
                    r.expected_phone_generation
                ),
            )
            .await?
                > 0
        })
    }
    async fn invitation_consent(
        &self,
        p: &str,
        r: &str,
        user: i64,
    ) -> Result<Option<InvitationConsent>, StoreError> {
        Ok(self
            .client()
            .query_consistent_map::<InvitationConsent, _>(CONSENT, params!(p, r, user))
            .await?
            .pop())
    }
    async fn invitation_login(
        &self,
        digest: &str,
        now: i64,
    ) -> Result<Option<InvitationLogin>, StoreError> {
        if !valid_digest(digest) {
            return Err(StoreError::Identity("invalid invitation login".into()));
        }
        let mut row = self
            .client()
            .query_consistent_map::<InvitationLogin, _>(LOGIN, params!(digest))
            .await?
            .pop();
        if let Some(r) = row.as_mut() {
            r.digest = digest.into()
        }
        Ok(row.filter(|r| r.live(now)))
    }
    async fn admit_invitation(
        &self,
        r: AdmitInvitation,
    ) -> Result<InvitationAdmission, StoreError> {
        if !validate_admission(&r)? {
            return Ok(InvitationAdmission::Refused);
        }
        let p = r.phone_login;
        let tv = r.receiver_login;
        let results = self
            .client()
            .txn(vec![
                (
                    ADMIT,
                    params!(
                        r.id.clone(),
                        r.enrollment_id,
                        r.foreground_id,
                        r.receiver_hash,
                        r.phone_generation,
                        r.consent_generation,
                        r.transport_generation,
                        p.user_id,
                        p.digest,
                        p.last_seen_at,
                        tv.digest,
                        tv.last_seen_at,
                        p.expiry_enabled,
                        p.expiry_days,
                        p.expiry_since,
                        r.now
                    ),
                ),
                (ADVANCE_REVISION, params!(r.id.clone())),
                (COOLDOWN, params!(r.id)),
            ])
            .await?;
        // txn rolls back on any statement error; inspect the entire response
        // as well so a malformed/incomplete acknowledgement is never success.
        let results = results
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        if results.len() != 3 {
            return Err(StoreError::Identity(
                "incomplete invitation transaction result".into(),
            ));
        }
        let admitted = results[0];
        Ok(if admitted > 0 {
            InvitationAdmission::Admitted
        } else {
            InvitationAdmission::Refused
        })
    }
}
