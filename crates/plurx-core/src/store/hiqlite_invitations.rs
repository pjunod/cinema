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
struct InvitationBinding {
    digest: String,
}
impl From<&mut Row<'_>> for InvitationBinding {
    fn from(r: &mut Row<'_>) -> Self {
        Self {
            digest: r.get("token_digest"),
        }
    }
}
struct InvitationRevision {
    revision: i64,
}
impl From<&mut Row<'_>> for InvitationRevision {
    fn from(r: &mut Row<'_>) -> Self {
        Self {
            revision: r.get("last_revision"),
        }
    }
}
impl From<&mut Row<'_>> for InvitationScope {
    fn from(r: &mut Row<'_>) -> Self {
        Self {
            consent: InvitationConsent::from(&mut *r),
            phone_generation: r.get("phone_generation"),
            permission_granted: r.get::<i64>("permission_granted") == 1,
            resident_active: r.get::<i64>("resident_active") == 1,
            phone_digest: r.get("token_digest"),
            receiver_hash: r.get("receiver_hash"),
            grant_hash: r.get("grant_hash"),
        }
    }
}
impl From<&mut Row<'_>> for InvitationEvent {
    fn from(r: &mut Row<'_>) -> Self {
        Self {
            id: r.get("id"),
            user_id: r.get("user_id"),
            enrollment_id: r.get("enrollment_id"),
            phone_id: r.get("phone_id"),
            receiver_id: r.get("receiver_id"),
            foreground_id: r.get("foreground_id"),
            grant_id: r.get("grant_id"),
            phone_generation: r.get("phone_generation"),
            consent_generation: r.get("consent_generation"),
            transport_generation: r.get("transport_generation"),
            created_at: r.get("created_at"),
            expires_at: r.get("expires_at"),
            phase: r.get("phase"),
            outcome: r.get("outcome"),
            revision: r.get("revision"),
        }
    }
}
impl From<&mut Row<'_>> for InvitationBrokerHealth {
    fn from(r: &mut Row<'_>) -> Self {
        Self {
            budget: r.get("budget"),
            invalid: r.get("invalid"),
            mismatched: r.get("mismatched"),
        }
    }
}
#[async_trait]
impl InvitationStore for HiqliteAuthStore {
    async fn invitation_broker_health(
        &self,
        scope_hash: &str,
    ) -> Result<InvitationBrokerHealth, StoreError> {
        if !valid_digest(scope_hash) {
            return Err(StoreError::Identity("invalid broker scope".into()));
        }
        self.client()
            .query_consistent_map::<InvitationBrokerHealth, _>(
                broker_health_query(),
                params!(scope_hash.to_owned()),
            )
            .await?
            .pop()
            .ok_or_else(|| StoreError::Identity("missing broker scope verdict".into()))
    }
    async fn invitation_cleanup_budget(&self, user: i64) -> Result<i64, StoreError> {
        validate_invitation_ids(&[], user)?;
        Ok(self
            .client()
            .query_consistent_map::<InvitationBudget, _>(CLEANUP_BUDGET, params!(user))
            .await?
            .pop()
            .ok_or_else(|| StoreError::Identity("missing cleanup budget".into()))?
            .budget)
    }
    async fn start_invitation_transport(
        &self,
        r: StartInvitationTransport,
    ) -> Result<bool, StoreError> {
        r.validate()?;
        let reference = r.reference.encode()?;
        let status = if r.provider_available {
            "pending"
        } else {
            "provider_unconfigured"
        };
        let account = self.invitation_cleanup_budget(r.user_id).await?;
        let global = self
            .client()
            .query_consistent_map::<InvitationBudget, _>(GLOBAL_CLEANUP_BUDGET, params!())
            .await?
            .pop()
            .ok_or_else(|| StoreError::Identity("missing global cleanup budget".into()))?
            .budget;
        let invalid = self
            .client()
            .query_consistent_map::<InvitationInvalid, _>(CLEANUP_INVALID, params!(r.user_id))
            .await?
            .pop()
            .ok_or_else(|| StoreError::Identity("missing cleanup verification".into()))?
            .invalid;
        if invalid > 0 || account > 100000 || global > 100000 {
            return Err(StoreError::Identity(
                "broker cleanup requires migration remediation".into(),
            ));
        }
        if r.provider_available && (account >= 100000 || global >= 100000) {
            return Ok(false);
        }
        let results = self
            .client()
            .txn(vec![
                (
                    start_cleanup_query(),
                    params!(
                        r.phone_id.clone(),
                        r.receiver_id.clone(),
                        r.user_id,
                        r.phone_hash.clone(),
                        r.grant_id.clone(),
                        r.grant_hash.clone(),
                        r.expected_phone_generation,
                        r.expected_consent_generation,
                        reference.clone(),
                        status,
                        r.login_digest.clone(),
                        r.now
                    ),
                ),
                (
                    start_query(),
                    params!(
                        r.phone_id,
                        r.receiver_id,
                        r.user_id,
                        r.phone_hash,
                        r.grant_id,
                        r.grant_hash,
                        r.expected_phone_generation,
                        r.expected_consent_generation,
                        reference,
                        status,
                        r.login_digest,
                        r.now
                    ),
                ),
            ])
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        if results.len() != 2 {
            return Err(StoreError::Identity(
                "incomplete transport transaction".into(),
            ));
        }
        Ok(results[1] > 0)
    }
    async fn confirm_invitation_transport(
        &self,
        r: ConfirmInvitationTransport,
    ) -> Result<bool, StoreError> {
        r.validate()?;
        let reference = r.reference.encode()?;
        Ok(self
            .execute(
                CONFIRM,
                params!(
                    r.phone_id,
                    r.receiver_id,
                    r.user_id,
                    r.phone_hash,
                    r.grant_id,
                    r.grant_hash,
                    r.phone_generation,
                    r.consent_generation,
                    r.transport_generation,
                    r.login_digest,
                    reference,
                    r.reference.ticket_id
                ),
            )
            .await?
            > 0)
    }
    async fn queue_invitation_reference(
        &self,
        id: &str,
        user: i64,
        reference: BrokerReference,
        now: i64,
    ) -> Result<bool, StoreError> {
        validate_invitation_ids(&[id], user)?;
        if now < 0 {
            return Err(StoreError::Identity("invalid cleanup clock".into()));
        }
        let reference = reference.encode()?;
        let invalid = self
            .client()
            .query_consistent_map::<InvitationInvalid, _>(
                cleanup_invalid_query(),
                params!(id.to_owned(), user),
            )
            .await?
            .pop()
            .ok_or_else(|| StoreError::Identity("missing cleanup verdict".into()))?
            .invalid;
        if invalid > 0 {
            return Err(StoreError::Identity(
                "migration_remediation: invalid broker cleanup reference".into(),
            ));
        }
        let results = self
            .client()
            .txn([
                (
                    queue_reference_query(),
                    params!(id.to_owned(), user, now, reference.clone()),
                ),
                (
                    clear_reference_query(),
                    params!(id.to_owned(), user, reference),
                ),
            ])
            .await?;
        let mut changed = false;
        for (index, result) in results.into_iter().enumerate() {
            let count = result.map_err(database_error)?;
            if index == 1 {
                changed = count > 0;
            }
        }
        Ok(changed)
    }
    async fn queue_invitation_cleanup(
        &self,
        id: &str,
        user: i64,
        now: i64,
    ) -> Result<(), StoreError> {
        validate_invitation_ids(&[id], user)?;
        if now < 0 {
            return Err(StoreError::Identity("invalid cleanup clock".into()));
        }
        let id = id.to_owned();
        let invalid = self
            .client()
            .query_consistent_map::<InvitationInvalid, _>(
                cleanup_invalid_query(),
                params!(id.to_owned(), user),
            )
            .await?
            .pop()
            .ok_or_else(|| StoreError::Identity("missing cleanup verdict".into()))?
            .invalid;
        if invalid > 0 {
            return Err(StoreError::Identity(
                "migration_remediation: invalid broker cleanup reference".into(),
            ));
        }
        let results = self
            .client()
            .txn(vec![
                (QUEUE_SCOPE_CLEANUP, params!(id.clone(), user, now)),
                (CLEAR_QUEUED_REFERENCE, params!(id, user)),
            ])
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        if results.len() != 2 {
            return Err(StoreError::Identity(
                "incomplete cleanup transaction".into(),
            ));
        }
        Ok(())
    }
    async fn invitation_revocations(&self) -> Result<Vec<InvitationRevocation>, StoreError> {
        Ok(self
            .client()
            .query_consistent_map::<InvitationRevocation, _>(REVOKE_WORK, params!())
            .await?)
    }
    async fn record_invitation_revocation(
        &self,
        id: &str,
        finished: bool,
    ) -> Result<(), StoreError> {
        BrokerReference::decode(id)?;
        let id = id.to_owned();
        let query = if finished {
            "DELETE FROM invitation_broker_revocations WHERE id=$1"
        } else {
            "UPDATE invitation_broker_revocations SET attempts=min(attempts+1,9007199254740991) WHERE id=$1"
        };
        self.execute(query, params!(id)).await?;
        Ok(())
    }
    async fn attempt_invitation(&self, r: AdmitInvitation) -> Result<bool, StoreError> {
        if !validate_admission(&r)? {
            return Ok(false);
        }
        let p = r.phone_login;
        let tv = r.receiver_login;
        Ok(self
            .client()
            .execute(
                attempt_query(),
                params!(
                    r.id,
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
            )
            .await?
            > 0)
    }
    async fn finish_invitation(
        &self,
        id: &str,
        outcome: InvitationDispatchOutcome,
    ) -> Result<(), StoreError> {
        validate_invitation_ids(&[id], 1)?;
        let id = id.to_owned();
        self.execute(FINISH, params!(id, outcome.as_str())).await?;
        Ok(())
    }

    async fn invitation_phone_binding(
        &self,
        id: &str,
        user: i64,
        hash: &str,
    ) -> Result<Option<String>, StoreError> {
        validate_invitation_ids(&[id], user)?;
        if !valid_digest(hash) {
            return Err(StoreError::Identity("invalid phone proof".into()));
        }
        Ok(self
            .client()
            .query_consistent_map::<InvitationBinding, _>(BINDING, params!(id, user, hash))
            .await?
            .pop()
            .map(|v| v.digest))
    }
    async fn rebind_invitation_phone(
        &self,
        id: &str,
        user: i64,
        hash: &str,
        expected: i64,
        digest: &str,
    ) -> Result<bool, StoreError> {
        validate_invitation_ids(&[id], user)?;
        if !valid_digest(hash) || !valid_digest(digest) || expected <= 0 {
            return Err(StoreError::Identity("invalid phone rebind".into()));
        }
        Ok(self
            .execute(REBIND, params!(digest, id, user, hash, expected))
            .await?
            > 0)
    }
    async fn invitation_consents(
        &self,
        phone: &str,
        user: i64,
        after: &str,
    ) -> Result<Vec<InvitationConsent>, StoreError> {
        validate_invitation_ids(&[phone], user)?;
        if !after.is_empty() {
            validate_invitation_ids(&[after], user)?;
        }
        Ok(self
            .client()
            .query_consistent_map::<InvitationConsent, _>(
                LIST_CONSENTS,
                params!(phone, user, after),
            )
            .await?)
    }
    async fn invitation_scope(
        &self,
        phone: &str,
        receiver: &str,
        user: i64,
    ) -> Result<Option<InvitationScope>, StoreError> {
        validate_invitation_ids(&[phone, receiver], user)?;
        Ok(self
            .client()
            .query_consistent_map::<InvitationScope, _>(
                scope_query(true),
                params!(phone, receiver, user),
            )
            .await?
            .pop())
    }
    async fn invitation_receiver_scopes(
        &self,
        receiver: &str,
        user: i64,
    ) -> Result<Vec<InvitationScope>, StoreError> {
        validate_invitation_ids(&[receiver], user)?;
        Ok(self
            .client()
            .query_consistent_map::<InvitationScope, _>(scope_query(false), params!(receiver, user))
            .await?)
    }
    async fn invitation_event(
        &self,
        phone: &str,
        user: i64,
        id: &str,
        now: i64,
    ) -> Result<Option<InvitationEvent>, StoreError> {
        validate_invitation_ids(&[phone, id], user)?;
        Ok(self
            .client()
            .query_consistent_map::<InvitationEvent, _>(
                event_query(true),
                params!(phone, user, now, id),
            )
            .await?
            .pop())
    }
    async fn invitation_events(
        &self,
        phone: &str,
        user: i64,
        after: i64,
        now: i64,
    ) -> Result<Vec<InvitationEvent>, StoreError> {
        validate_invitation_ids(&[phone], user)?;
        if !(0..=9007199254740991).contains(&after) {
            return Err(StoreError::Identity("invalid invitation cursor".into()));
        }
        Ok(self
            .client()
            .query_consistent_map::<InvitationEvent, _>(
                event_query(false),
                params!(phone, user, now, after),
            )
            .await?)
    }
    async fn invitation_revision(&self, phone: &str, user: i64) -> Result<Option<i64>, StoreError> {
        validate_invitation_ids(&[phone], user)?;
        Ok(self
            .client()
            .query_consistent_map::<InvitationRevision, _>(REVISION, params!(phone, user))
            .await?
            .pop()
            .map(|v| v.revision))
    }

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

struct InvitationBudget {
    budget: i64,
}
impl From<&mut Row<'_>> for InvitationBudget {
    fn from(r: &mut Row<'_>) -> Self {
        Self {
            budget: r.get("budget"),
        }
    }
}
struct InvitationInvalid {
    invalid: i64,
}
impl From<&mut Row<'_>> for InvitationInvalid {
    fn from(r: &mut Row<'_>) -> Self {
        Self {
            invalid: r.get("invalid"),
        }
    }
}
impl From<&mut Row<'_>> for InvitationRevocation {
    fn from(r: &mut Row<'_>) -> Self {
        Self {
            id: r.get("id"),
            user_id: r.get("user_id"),
            enrollment_id: r.get("enrollment_id"),
            generation: r.get("generation"),
            attempts: r.get("attempts"),
            created_at: r.get("created_at"),
        }
    }
}
