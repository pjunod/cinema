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
        transport_phone_generation: r.get(12)?,
    })
}
fn scope(r: &rusqlite::Row<'_>) -> rusqlite::Result<InvitationScope> {
    Ok(InvitationScope {
        consent: consent(r)?,
        phone_generation: r.get(13)?,
        permission_granted: r.get::<_, i64>(14)? == 1,
        resident_active: r.get::<_, i64>(15)? == 1,
        phone_digest: r.get(16)?,
        receiver_hash: r.get(17)?,
        grant_hash: r.get(18)?,
    })
}
fn event(r: &rusqlite::Row<'_>) -> rusqlite::Result<InvitationEvent> {
    Ok(InvitationEvent {
        id: r.get(0)?,
        user_id: r.get(1)?,
        enrollment_id: r.get(2)?,
        phone_id: r.get(3)?,
        receiver_id: r.get(4)?,
        foreground_id: r.get(5)?,
        grant_id: r.get(6)?,
        phone_generation: r.get(7)?,
        consent_generation: r.get(8)?,
        transport_generation: r.get(9)?,
        created_at: r.get(10)?,
        expires_at: r.get(11)?,
        phase: r.get(12)?,
        outcome: r.get(13)?,
        revision: r.get(14)?,
    })
}
#[async_trait]
impl InvitationStore for SqliteStore {
    async fn invitation_broker_health(
        &self,
        scope_hash: &str,
    ) -> Result<InvitationBrokerHealth, StoreError> {
        if !valid_digest(scope_hash) {
            return Err(StoreError::Identity("invalid broker scope".into()));
        }
        let scope_hash = scope_hash.to_owned();
        self.with_read(move |c| {
            Ok(c.query_row(&broker_health_query(), [scope_hash], |r| {
                Ok(InvitationBrokerHealth {
                    budget: r.get(0)?,
                    invalid: r.get(1)?,
                    mismatched: r.get(2)?,
                })
            })?)
        })
        .await
    }
    async fn invitation_cleanup_budget(&self, user: i64) -> Result<i64, StoreError> {
        validate_invitation_ids(&[], user)?;
        self.with_read(move |c| Ok(c.query_row(CLEANUP_BUDGET, [user], |r| r.get(0))?))
            .await
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
        self.with_conn(move |c| {
            let tx = c.unchecked_transaction()?;
            let account: i64 = tx.query_row(CLEANUP_BUDGET, [r.user_id], |v| v.get(0))?;
            let global: i64 = tx.query_row(GLOBAL_CLEANUP_BUDGET, [], |v| v.get(0))?;
            let invalid: i64 = tx.query_row(CLEANUP_INVALID, [r.user_id], |v| v.get(0))?;
            if invalid > 0 || account > 100000 || global > 100000 {
                return Err(StoreError::Identity(
                    "broker cleanup requires migration remediation".into(),
                ));
            }
            if r.provider_available && (account >= 100000 || global >= 100000) {
                return Ok(false);
            }
            let values = params![
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
                r.login_digest
            ];
            tx.execute(&start_cleanup_query(), values)?;
            let changed = tx.execute(&start_query(), values)? > 0;
            tx.commit()?;
            Ok(changed)
        })
        .await
    }
    async fn confirm_invitation_transport(
        &self,
        r: ConfirmInvitationTransport,
    ) -> Result<bool, StoreError> {
        r.validate()?;
        let reference = r.reference.encode()?;
        self.with_conn(move |c| {
            Ok(c.execute(
                CONFIRM,
                params![
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
                ],
            )? > 0)
        })
        .await
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
        self.with_conn(move |c| {
            let tx = c.unchecked_transaction()?;
            tx.execute(QUEUE_SCOPE_CLEANUP, params![id, user, now])?;
            tx.execute(CLEAR_QUEUED_REFERENCE, params![id, user])?;
            tx.commit()?;
            Ok(())
        })
        .await
    }
    async fn invitation_revocations(&self) -> Result<Vec<InvitationRevocation>, StoreError> {
        self.with_read(move |c| {
            Ok(c.prepare(REVOKE_WORK)?
                .query_map([], |r| {
                    Ok(InvitationRevocation {
                        id: r.get(0)?,
                        user_id: r.get(1)?,
                        enrollment_id: r.get(2)?,
                        generation: r.get(3)?,
                        attempts: r.get(4)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?)
        })
        .await
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
        self.with_conn(move |c| {
            c.execute(query, params![id])?;
            Ok(())
        })
        .await
    }
    async fn attempt_invitation(&self, r: AdmitInvitation) -> Result<bool, StoreError> {
        if !validate_admission(&r)? {
            return Ok(false);
        }
        let p = r.phone_login;
        let tv = r.receiver_login;
        self.with_conn(move |c| {
            Ok(c.execute(
                &attempt_query(),
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
            )? > 0)
        })
        .await
    }
    async fn finish_invitation(
        &self,
        id: &str,
        outcome: InvitationDispatchOutcome,
    ) -> Result<(), StoreError> {
        validate_invitation_ids(&[id], 1)?;
        let id = id.to_owned();
        self.with_conn(move |c| {
            c.execute(FINISH, params![id, outcome.as_str()])?;
            Ok(())
        })
        .await
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
        let (id, hash) = (id.to_owned(), hash.to_owned());
        self.with_read(move |c| {
            Ok(c.query_row(BINDING, params![id, user, hash], |r| r.get(0))
                .optional()?)
        })
        .await
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
        let (id, hash, digest) = (id.to_owned(), hash.to_owned(), digest.to_owned());
        self.with_conn(move |c| {
            Ok(c.execute(REBIND, params![digest, id, user, hash, expected])? > 0)
        })
        .await
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
        let (phone, after) = (phone.to_owned(), after.to_owned());
        self.with_read(move |c| {
            Ok(c.prepare(LIST_CONSENTS)?
                .query_map(params![phone, user, after], consent)?
                .collect::<Result<Vec<_>, _>>()?)
        })
        .await
    }
    async fn invitation_scope(
        &self,
        phone: &str,
        receiver: &str,
        user: i64,
    ) -> Result<Option<InvitationScope>, StoreError> {
        validate_invitation_ids(&[phone, receiver], user)?;
        let (phone, receiver) = (phone.to_owned(), receiver.to_owned());
        self.with_read(move |c| {
            Ok(
                c.query_row(&scope_query(true), params![phone, receiver, user], scope)
                    .optional()?,
            )
        })
        .await
    }
    async fn invitation_receiver_scopes(
        &self,
        receiver: &str,
        user: i64,
    ) -> Result<Vec<InvitationScope>, StoreError> {
        validate_invitation_ids(&[receiver], user)?;
        let receiver = receiver.to_owned();
        self.with_read(move |c| {
            Ok(c.prepare(&scope_query(false))?
                .query_map(params![receiver, user], scope)?
                .collect::<Result<Vec<_>, _>>()?)
        })
        .await
    }
    async fn invitation_event(
        &self,
        phone: &str,
        user: i64,
        id: &str,
        now: i64,
    ) -> Result<Option<InvitationEvent>, StoreError> {
        validate_invitation_ids(&[phone, id], user)?;
        let (phone, id) = (phone.to_owned(), id.to_owned());
        self.with_read(move |c| {
            Ok(
                c.query_row(&event_query(true), params![phone, user, now, id], event)
                    .optional()?,
            )
        })
        .await
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
        let phone = phone.to_owned();
        self.with_read(move |c| {
            Ok(c.prepare(&event_query(false))?
                .query_map(params![phone, user, now, after], event)?
                .collect::<Result<Vec<_>, _>>()?)
        })
        .await
    }
    async fn invitation_revision(&self, phone: &str, user: i64) -> Result<Option<i64>, StoreError> {
        validate_invitation_ids(&[phone], user)?;
        let phone = phone.to_owned();
        self.with_read(move |c| {
            Ok(c.query_row(REVISION, params![phone, user], |r| r.get(0))
                .optional()?)
        })
        .await
    }

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
                tx.execute(ADVANCE_REVISION, [&r.id])?;
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
