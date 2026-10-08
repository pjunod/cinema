use super::SqliteStore;
use crate::{error::StoreError, store::invitations::*};
use async_trait::async_trait;
use rusqlite::{params, OptionalExtension};
fn phone(r: &rusqlite::Row<'_>) -> rusqlite::Result<InvitationPhone> {
    Ok(InvitationPhone {
        id: r.get(0)?,
        user_id: r.get(1)?,
        name: r.get(2)?,
        platform: r.get(3)?,
        generation: r.get(4)?,
        created_at: r.get(5)?,
        permission_granted: r.get::<_, i64>(6)? == 1,
        resident_active: r.get::<_, i64>(7)? == 1,
    })
}
fn consent(r: &rusqlite::Row<'_>) -> rusqlite::Result<InvitationConsent> {
    Ok(InvitationConsent {
        id: r.get(0)?,
        phone_id: r.get(1)?,
        receiver_id: r.get(2)?,
        user_id: r.get(3)?,
        grant_id: r.get(4)?,
        enabled: r.get::<_, i64>(5)? == 1,
        transport: r.get(6)?,
        generation: r.get(7)?,
        transport_generation: r.get(8)?,
        broker_enrollment: r.get(9)?,
        broker_ticket: r.get(10)?,
        transport_status: r.get(11)?,
    })
}
#[async_trait]
impl InvitationStore for SqliteStore {
    async fn create_invitation_phone(&self, n: NewInvitationPhone) -> Result<bool, StoreError> {
        n.validate()?;
        self.with_conn(move |c| {
            let p = n.phone;
            Ok(c.execute(
                CREATE_PHONE,
                params![
                    p.id,
                    p.user_id,
                    p.name,
                    p.platform,
                    n.secret_hash,
                    n.token_digest,
                    p.created_at
                ],
            )? > 0)
        })
        .await
    }
    async fn invitation_phone(
        &self,
        id: &str,
        user: i64,
    ) -> Result<Option<InvitationPhone>, StoreError> {
        let id = id.to_owned();
        self.with_read(move |c| {
            Ok(c.query_row(PHONE_METADATA, params![id, user], phone)
                .optional()?)
        })
        .await
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
        let (id, hash) = (id.to_owned(), hash.to_owned());
        self.with_read(move |c| {
            Ok(c.query_row(PHONE_AUTHORITY, params![id, user, hash], phone)
                .optional()?)
        })
        .await
    }
    async fn invitation_phones(
        &self,
        user: i64,
        after: &str,
    ) -> Result<Vec<InvitationPhone>, StoreError> {
        let after = after.to_owned();
        self.with_read(move |c| {
            Ok(c.prepare(LIST_PHONES)?
                .query_map(params![user, after], phone)?
                .collect::<Result<Vec<_>, _>>()?)
        })
        .await
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
        let (id, hash) = (id.to_owned(), hash.to_owned());
        self.with_conn(move |c| {
            Ok(c.execute(
                AVAILABILITY,
                params![
                    i64::from(permission),
                    i64::from(resident),
                    id,
                    user,
                    hash,
                    expected
                ],
            )? > 0)
        })
        .await
    }
    async fn revoke_invitation_phone(&self, id: &str, user: i64) -> Result<(), StoreError> {
        let id = id.to_owned();
        self.with_conn(move |c| {
            c.execute(DELETE_PHONE, params![id, user])?;
            Ok(())
        })
        .await
    }
    async fn save_invitation_consent(&self, r: SaveInvitationConsent) -> Result<bool, StoreError> {
        r.validate()?;
        self.with_conn(move |c| {
            Ok(if let Some((gid, gh, transport)) = r.enable {
                c.execute(
                    CONSENT_ENABLE,
                    params![
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
                    ],
                )? > 0
            } else {
                c.execute(
                    CONSENT_DISABLE,
                    params![
                        r.phone_id,
                        r.receiver_id,
                        r.user_id,
                        r.phone_hash,
                        r.expected_generation,
                        r.expected_phone_generation
                    ],
                )? > 0
            })
        })
        .await
    }
    async fn invitation_consent(
        &self,
        p: &str,
        r: &str,
        user: i64,
    ) -> Result<Option<InvitationConsent>, StoreError> {
        let (p, r) = (p.to_owned(), r.to_owned());
        self.with_read(move |c| {
            Ok(c.query_row(CONSENT, params![p, r, user], consent)
                .optional()?)
        })
        .await
    }
    async fn invitation_login(
        &self,
        digest: &str,
        now: i64,
    ) -> Result<Option<InvitationLogin>, StoreError> {
        if !valid_digest(digest) {
            return Err(StoreError::Identity("invalid invitation login".into()));
        }
        let digest = digest.to_owned();
        self.with_read(move |c| {
            let row = c
                .query_row(LOGIN, [&digest], |r| {
                    Ok(InvitationLogin {
                        user_id: r.get(0)?,
                        digest: digest.clone(),
                        last_seen_at: r.get(1)?,
                        expiry_enabled: r.get(2)?,
                        expiry_days: r.get(3)?,
                        expiry_since: r.get(4)?,
                    })
                })
                .optional()?;
            Ok(row.filter(|r| r.live(now)))
        })
        .await
    }
    async fn admit_invitation(
        &self,
        r: AdmitInvitation,
    ) -> Result<InvitationAdmission, StoreError> {
        if !validate_admission(&r)? {
            return Ok(InvitationAdmission::Refused);
        }
        self.with_conn(move |c| {
            let tx = c.unchecked_transaction()?;
            let p = r.phone_login;
            let tv = r.receiver_login;
            let admitted = tx.execute(
                ADMIT,
                params![
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
                ],
            )? > 0;
            if admitted {
                tx.execute(COOLDOWN, [&r.id])?;
            }
            tx.commit()?;
            Ok(if admitted {
                InvitationAdmission::Admitted
            } else {
                InvitationAdmission::Refused
            })
        })
        .await
    }
}
