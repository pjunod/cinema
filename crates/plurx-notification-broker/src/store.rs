use crate::{
    wire::{self, Delivery, Platform, Ticket, TicketIssued, TicketStatus, VERSION},
    Error, Result,
};
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    ChaCha20Poly1305, Nonce,
};
use rusqlite::{
    params, Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior,
};
use sha2::{Digest, Sha256};
use std::{path::Path, time::Duration};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

const SCHEMA: &str = "
CREATE TABLE marker(singleton INTEGER PRIMARY KEY CHECK(singleton=1), generation TEXT NOT NULL, key_hash TEXT NOT NULL, configuration_hash TEXT NOT NULL, clock_floor INTEGER NOT NULL);
CREATE TABLE publishers(id TEXT PRIMARY KEY, proof_hash TEXT NOT NULL, server TEXT NOT NULL, apple TEXT, android TEXT, credential TEXT NOT NULL, active INTEGER NOT NULL);
CREATE TABLE capabilities(publisher TEXT NOT NULL, generation TEXT NOT NULL, id TEXT NOT NULL, binding TEXT, expiry INTEGER NOT NULL, state TEXT NOT NULL, nonce BLOB, ciphertext BLOB, PRIMARY KEY(publisher,generation,id));
CREATE INDEX ticket_claim ON capabilities(id,generation);
CREATE TABLE deliveries(publisher TEXT NOT NULL, generation TEXT NOT NULL, invitation TEXT NOT NULL, enrollment TEXT NOT NULL, binding TEXT NOT NULL, state TEXT NOT NULL, PRIMARY KEY(publisher,generation,invitation));
CREATE TABLE cooldowns(publisher TEXT NOT NULL, generation TEXT NOT NULL, installation TEXT NOT NULL, attempted_at INTEGER NOT NULL, PRIMARY KEY(publisher,generation,installation));
CREATE TABLE rates(publisher TEXT NOT NULL, generation TEXT NOT NULL, bucket INTEGER NOT NULL, count INTEGER NOT NULL, PRIMARY KEY(publisher,generation));";

#[derive(Clone, PartialEq, Eq)]
pub struct PublisherRecord {
    pub id: String,
    pub proof_hash: String,
    pub credential: String,
    pub server: String,
    pub apple: Option<String>,
    pub android: Option<String>,
}

pub struct Store {
    connection: Connection,
    generation: String,
    key_hash: String,
    configuration_hash: String,
    cipher: ChaCha20Poly1305,
    limits: Limits,
    expected: std::collections::HashMap<String, PublisherRecord>,
}
struct Limits {
    identities: i64,
    pending: i64,
    active: i64,
    deliveries: i64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            identities: 100_000,
            pending: 4096,
            active: 4096,
            deliveries: 100_000,
        }
    }
}
#[derive(Clone)]
struct Capability {
    binding: Option<Ticket>,
    expiry: i64,
    state: String,
    nonce: Option<Vec<u8>>,
    ciphertext: Option<Vec<u8>>,
}
pub struct Attempt {
    pub publisher: String,
    pub enrollment: String,
    pub platform: Platform,
    pub device_token: Zeroizing<String>,
    pub delivery: Delivery,
}
pub enum Admission {
    Duplicate,
    Denied,
    Attempt(Box<Attempt>),
}
fn sql<T>(value: rusqlite::Result<T>) -> Result<T> {
    value.map_err(|_| Error::unavailable())
}
fn encoded<T: serde::Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|_| Error::unavailable())
}
fn capacity(count: i64, max: i64) -> Result<()> {
    if count >= max {
        Err(Error::new(429, "retention_limit"))
    } else {
        Ok(())
    }
}
impl Store {
    pub fn initialize(
        path: &Path,
        generation: &str,
        key: &[u8; 32],
        configuration_hash: &str,
        publishers: &[PublisherRecord],
        now: i64,
    ) -> Result<()> {
        wire::uuid(generation)?;
        if publishers.len() > 64 {
            return Err(Error::new(429, "retention_limit"));
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(path).map_err(|_| Error::unavailable())?;
        let mut connection = sql(Connection::open(path))?;
        sql(connection.execute_batch("PRAGMA synchronous=FULL;"))?;
        let connection = sql(connection.transaction_with_behavior(TransactionBehavior::Immediate))?;
        sql(connection.execute_batch(SCHEMA))?;
        sql(connection.execute(
            "INSERT INTO marker VALUES(1,?1,?2,?3,?4)",
            params![
                generation,
                hex::encode(Sha256::digest(key)),
                configuration_hash,
                now
            ],
        ))?;
        for publisher in publishers {
            Self::put_publisher(&connection, publisher)?;
        }
        sql(connection.commit())?;
        Ok(())
    }
    pub fn open(
        path: &Path,
        generation: &str,
        key: &[u8; 32],
        configuration_hash: &str,
    ) -> Result<Self> {
        let connection = sql(Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE,
        ))?;
        sql(connection.busy_timeout(Duration::from_millis(250)))?;
        sql(connection.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;",
        ))?;
        let key_hash = hex::encode(Sha256::digest(key));
        let marker: (String, String, String) = sql(connection.query_row(
            "SELECT generation,key_hash,configuration_hash FROM marker WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ))?;
        if marker
            != (
                generation.to_owned(),
                key_hash.clone(),
                configuration_hash.to_owned(),
            )
        {
            return Err(Error::new(503, "restore_fence_required"));
        }
        let mut statement = sql(connection.prepare("SELECT id FROM publishers WHERE active=1"))?;
        let ids = sql(statement.query_map([], |row| row.get::<_, String>(0)))?
            .collect::<rusqlite::Result<Vec<_>>>();
        drop(statement);
        let mut expected = std::collections::HashMap::new();
        for id in sql(ids)? {
            if let Some((record, true)) = Self::publisher(&connection, &id)? {
                expected.insert(id, record);
            }
        }
        Ok(Self {
            expected,
            connection,
            generation: generation.to_owned(),
            key_hash,
            configuration_hash: configuration_hash.to_owned(),
            cipher: ChaCha20Poly1305::new(key.into()),
            limits: Limits::default(),
        })
    }
    pub fn restore_fence(
        path: &Path,
        generation: &str,
        key: &[u8; 32],
        configuration_hash: &str,
        publishers: &[PublisherRecord],
        now: i64,
    ) -> Result<()> {
        wire::uuid(generation)?;
        let mut connection = sql(Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE,
        ))?;
        let transaction =
            sql(connection.transaction_with_behavior(TransactionBehavior::Immediate))?;
        let old: (String, String) = sql(transaction.query_row(
            "SELECT generation,key_hash FROM marker WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ))?;
        let key_hash = hex::encode(Sha256::digest(key));
        if old.0 == generation || old.1 == key_hash {
            return Err(Error::new(409, "rotation_required"));
        }
        for publisher in publishers {
            let id = &publisher.id;
            let hash = &publisher.proof_hash;
            let old_hash: Option<String> = sql(transaction
                .query_row(
                    "SELECT proof_hash FROM publishers WHERE id=?1",
                    [id],
                    |row| row.get(0),
                )
                .optional())?;
            if old_hash.as_deref() == Some(hash) {
                return Err(Error::new(409, "rotation_required"));
            }
        }
        sql(transaction.execute_batch("DELETE FROM capabilities; DELETE FROM deliveries; DELETE FROM cooldowns; DELETE FROM rates; DELETE FROM publishers;"))?;
        for publisher in publishers {
            Self::put_publisher(&transaction, publisher)?;
        }
        sql(transaction.execute("UPDATE marker SET generation=?1,key_hash=?2,configuration_hash=?3,clock_floor=?4 WHERE singleton=1",params![generation,key_hash,configuration_hash,now]))?;
        sql(transaction.commit())?;
        Ok(())
    }
    fn put_publisher(connection: &Connection, publisher: &PublisherRecord) -> Result<()> {
        sql(connection.execute("INSERT INTO publishers VALUES(?1,?2,?3,?4,?5,?6,1) ON CONFLICT(id) DO UPDATE SET proof_hash=excluded.proof_hash,server=excluded.server,apple=excluded.apple,android=excluded.android,credential=excluded.credential,active=1",params![publisher.id,publisher.proof_hash,publisher.server,publisher.apple,publisher.android,publisher.credential]))?;
        Ok(())
    }
    fn publisher(connection: &Connection, id: &str) -> Result<Option<(PublisherRecord, bool)>> {
        sql(connection.query_row("SELECT proof_hash,server,apple,android,credential,active FROM publishers WHERE id=?1",[id],|row|Ok((PublisherRecord {id:id.to_owned(),proof_hash:row.get(0)?,server:row.get(1)?,apple:row.get(2)?,android:row.get(3)?,credential:row.get(4)?},row.get::<_,i64>(5)?==1))).optional())
    }
    /// Configuration changes preserve IDs and attempted delivery history. Only
    /// removed/changed routing realms revoke capabilities; signing-key/proof
    /// rotation within the same realm preserves them.
    pub fn reconcile(&mut self, publishers: &[PublisherRecord], now: i64) -> Result<()> {
        let generation = self.generation.clone();
        let transaction = self.transaction(now)?;
        let existing: i64 =
            sql(transaction.query_row("SELECT count(*) FROM publishers", [], |row| row.get(0)))?;
        let mut added = 0;
        for publisher in publishers {
            if Self::publisher(&transaction, &publisher.id)?.is_none() {
                added += 1;
            }
        }
        if existing + added > 64 {
            return Err(Error::new(429, "retention_limit"));
        }
        let mut statement = sql(transaction.prepare("SELECT id FROM publishers WHERE active=1"))?;
        let ids = sql(statement.query_map([], |row| row.get::<_, String>(0)))?
            .collect::<rusqlite::Result<Vec<_>>>();
        drop(statement);
        for id in sql(ids)? {
            if !publishers.iter().any(|publisher| publisher.id == id) {
                sql(transaction.execute("UPDATE publishers SET active=0 WHERE id=?1", [&id]))?;
                sql(transaction.execute("UPDATE capabilities SET state='revoked',nonce=NULL,ciphertext=NULL WHERE publisher=?1 AND generation=?2",params![id,generation]))?;
            }
        }
        for publisher in publishers {
            if let Some((old, active)) = Self::publisher(&transaction, &publisher.id)? {
                if !active || old.server != publisher.server {
                    sql(transaction.execute("UPDATE capabilities SET state='revoked',nonce=NULL,ciphertext=NULL WHERE publisher=?1 AND generation=?2",params![publisher.id,generation]))?;
                } else {
                    for (platform, changed) in [
                        ("apple", old.apple != publisher.apple),
                        ("android", old.android != publisher.android),
                    ] {
                        if changed {
                            sql(transaction.execute("UPDATE capabilities SET state='revoked',nonce=NULL,ciphertext=NULL WHERE publisher=?1 AND generation=?2 AND json_extract(binding,'$.platform')=?3",params![publisher.id,generation,platform]))?;
                        }
                    }
                }
            }
            Self::put_publisher(&transaction, publisher)?;
        }
        sql(transaction.commit())?;
        self.expected = publishers
            .iter()
            .map(|publisher| (publisher.id.clone(), publisher.clone()))
            .collect();
        Ok(())
    }
    fn require_publisher(
        transaction: &Transaction<'_>,
        expected: Option<&PublisherRecord>,
    ) -> Result<()> {
        let expected = expected.ok_or_else(|| Error::new(401, "unauthorized"))?;
        if !Self::publisher(transaction, &expected.id)?
            .is_some_and(|(current, active)| active && current == *expected)
        {
            return Err(Error::new(401, "unauthorized"));
        }
        Ok(())
    }
    fn compatible(record: &PublisherRecord, ticket: &Ticket) -> bool {
        record.server == ticket.server_instance_id
            && match ticket.platform {
                Platform::Apple => record.apple.is_some(),
                Platform::Android => record.android.is_some(),
            }
    }
    pub fn authorized(&mut self, expected: &PublisherRecord, now: i64) -> Result<()> {
        let transaction = self.transaction(now)?;
        if !Self::publisher(&transaction, &expected.id)?
            .is_some_and(|(current, active)| active && current == *expected)
        {
            return Err(Error::new(401, "unauthorized"));
        }
        sql(transaction.commit())?;
        Ok(())
    }
    /// Rechecked after provider authentication and immediately before visible
    /// send, so a revoke while OAuth is pending cannot authorize later delivery.
    pub fn eligible(
        &mut self,
        expected: &PublisherRecord,
        delivery: &Delivery,
        now: i64,
    ) -> Result<bool> {
        if delivery.validate(now).is_err() {
            return Ok(false);
        }
        let generation = self.generation.clone();
        let transaction = self.transaction(now)?;
        let authorized = Self::publisher(&transaction, &expected.id)?
            .is_some_and(|(current, active)| active && current == *expected);
        let eligible = Self::capability(
            &transaction,
            &expected.id,
            &generation,
            &delivery.enrollment_id,
        )?
        .is_some_and(|entry| {
            entry.state == "claimed"
                && entry.binding.as_ref().is_some_and(|ticket| {
                    delivery.matches(ticket) && Self::compatible(expected, ticket)
                })
        });
        sql(transaction.commit())?;
        Ok(authorized && eligible)
    }
    fn transaction(&mut self, now: i64) -> Result<Transaction<'_>> {
        if now <= 0 || now as u64 > wire::MAX_SAFE - 120 {
            return Err(Error::unavailable());
        }
        let transaction = sql(self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate))?;
        let marker:(String,String,String,i64)=sql(transaction.query_row("SELECT generation,key_hash,configuration_hash,clock_floor FROM marker WHERE singleton=1",[],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))))?;
        if marker.0 != self.generation
            || marker.1 != self.key_hash
            || marker.2 != self.configuration_hash
            || now < marker.3
        {
            return Err(Error::new(503, "restore_fence_required"));
        }
        sql(transaction.execute("UPDATE marker SET clock_floor=?1 WHERE singleton=1", [now]))?;
        Ok(transaction)
    }
    pub fn rate(&mut self, publisher: &str, now: i64) -> Result<()> {
        let generation = self.generation.clone();
        let expected = self.expected.get(publisher).cloned();
        let transaction = self.transaction(now)?;
        Self::require_publisher(&transaction, expected.as_ref())?;
        let bucket = now / 60;
        let current: Option<(i64, i64)> = sql(transaction
            .query_row(
                "SELECT bucket,count FROM rates WHERE publisher=?1 AND generation=?2",
                params![publisher, generation],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional())?;
        let count = match current {
            Some((old, count)) if old == bucket => count + 1,
            Some((old, _)) if old > bucket => return Err(Error::unavailable()),
            _ => 1,
        };
        if count > 120 {
            return Err(Error::new(429, "rate_limited"));
        }
        sql(transaction.execute("INSERT INTO rates VALUES(?1,?2,?3,?4) ON CONFLICT(publisher,generation) DO UPDATE SET bucket=excluded.bucket,count=excluded.count",params![publisher,generation,bucket,count]))?;
        sql(transaction.commit())?;
        Ok(())
    }
    fn capability(
        transaction: &Transaction<'_>,
        publisher: &str,
        generation: &str,
        id: &str,
    ) -> Result<Option<Capability>> {
        type StoredCapability = (
            Option<String>,
            i64,
            String,
            Option<Vec<u8>>,
            Option<Vec<u8>>,
        );
        let row:Option<StoredCapability>=sql(transaction.query_row("SELECT binding,expiry,state,nonce,ciphertext FROM capabilities WHERE publisher=?1 AND generation=?2 AND id=?3",params![publisher,generation,id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?))).optional())?;
        row.map(|(binding, expiry, state, nonce, ciphertext)| {
            Ok(Capability {
                binding: binding
                    .map(|value| serde_json::from_str(&value).map_err(|_| Error::unavailable()))
                    .transpose()?,
                expiry,
                state,
                nonce,
                ciphertext,
            })
        })
        .transpose()
    }
    pub fn issue(&mut self, publisher: &str, ticket: &Ticket, now: i64) -> Result<TicketIssued> {
        ticket.validate()?;
        let generation = self.generation.clone();
        let limits = (self.limits.identities, self.limits.pending);
        let expected = self.expected.get(publisher).cloned();
        let transaction = self.transaction(now)?;
        Self::require_publisher(&transaction, expected.as_ref())?;
        if !expected
            .as_ref()
            .is_some_and(|record| Self::compatible(record, ticket))
        {
            return Err(Error::new(403, "scope_mismatch"));
        }
        if let Some(old) =
            Self::capability(&transaction, publisher, &generation, &ticket.ticket_id)?
        {
            if old.binding.as_ref() != Some(ticket) || old.state == "revoked" {
                return Err(Error::new(409, "ticket_conflict"));
            }
            sql(transaction.commit())?;
            return Ok(TicketIssued {
                version: VERSION,
                ticket_id: ticket.ticket_id.clone(),
                expires_at: old.expiry,
                status: "pending",
            });
        }
        let count: i64 = sql(transaction.query_row(
            "SELECT count(*) FROM capabilities WHERE publisher=?1 AND generation=?2",
            params![publisher, generation],
            |row| row.get(0),
        ))?;
        capacity(count, limits.0)?;
        let pending:i64=sql(transaction.query_row("SELECT count(*) FROM capabilities WHERE publisher=?1 AND generation=?2 AND state='pending' AND expiry>?3",params![publisher,generation,now],|row|row.get(0)))?;
        capacity(pending, limits.1)?;
        let expiry = now + 120;
        sql(transaction.execute("INSERT INTO capabilities(publisher,generation,id,binding,expiry,state) VALUES(?1,?2,?3,?4,?5,'pending')",params![publisher,generation,ticket.ticket_id,encoded(ticket)?,expiry]))?;
        sql(transaction.commit())?;
        Ok(TicketIssued {
            version: VERSION,
            ticket_id: ticket.ticket_id.clone(),
            expires_at: expiry,
            status: "pending",
        })
    }
    fn aad(publisher: &str, generation: &str, ticket: &Ticket) -> Result<Vec<u8>> {
        Ok(format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            publisher,
            generation,
            ticket.ticket_id,
            ticket.platform.as_str(),
            ticket.transport_generation,
            encoded(ticket)?
        )
        .into_bytes())
    }
    pub fn claim(
        &mut self,
        ticket_id: &str,
        hash: &[u8; 32],
        platform: Platform,
        token: &str,
        publishers: &[String],
        now: i64,
    ) -> Result<()> {
        wire::uuid(ticket_id)?;
        wire::device_token(platform, token)?;
        let generation = self.generation.clone();
        let active_max = self.limits.active;
        let cipher = self.cipher.clone();
        let expected = self.expected.clone();
        let transaction = self.transaction(now)?;
        let mut statement=sql(transaction.prepare("SELECT publisher FROM capabilities WHERE generation=?1 AND id=?2 AND binding IS NOT NULL LIMIT 65"))?;
        let names = sql(statement.query_map(params![generation, ticket_id], |row| {
            row.get::<_, String>(0)
        }))?
        .collect::<rusqlite::Result<Vec<_>>>();
        drop(statement);
        let names = sql(names)?;
        let mut matching = None;
        for publisher in names {
            if !publishers.contains(&publisher)
                || Self::require_publisher(&transaction, expected.get(&publisher)).is_err()
            {
                continue;
            }
            let entry = Self::capability(&transaction, &publisher, &generation, ticket_id)?
                .ok_or_else(Error::unavailable)?;
            let binding = entry.binding.as_ref().ok_or_else(Error::unavailable)?;
            if bool::from(wire::digest(&binding.ticket_secret_hash)?.ct_eq(hash)) {
                if matching.is_some() {
                    return Err(Error::new(401, "unauthorized"));
                }
                matching = Some((publisher, entry));
            }
        }
        let (publisher, entry) = matching.ok_or_else(|| Error::new(401, "unauthorized"))?;
        let ticket = entry.binding.ok_or_else(Error::unavailable)?;
        if entry.state != "pending"
            || entry.expiry <= now
            || ticket.platform != platform
            || !expected
                .get(&publisher)
                .is_some_and(|record| Self::compatible(record, &ticket))
        {
            return Err(Error::new(401, "unauthorized"));
        }
        let active:i64=sql(transaction.query_row("SELECT count(*) FROM capabilities WHERE publisher=?1 AND generation=?2 AND state='claimed'",params![publisher,generation],|row|row.get(0)))?;
        capacity(active, active_max)?;
        let mut nonce = [0u8; 12];
        getrandom::getrandom(&mut nonce).map_err(|_| Error::unavailable())?;
        let aad = Self::aad(&publisher, &generation, &ticket)?;
        let ciphertext = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: token.as_bytes(),
                    aad: &aad,
                },
            )
            .map_err(|_| Error::unavailable())?;
        sql(transaction.execute("UPDATE capabilities SET state='claimed',nonce=?1,ciphertext=?2 WHERE publisher=?3 AND generation=?4 AND id=?5",params![nonce.as_slice(),ciphertext,publisher,generation,ticket_id]))?;
        sql(transaction.commit())?;
        Ok(())
    }
    pub fn status(&mut self, publisher: &str, id: &str, now: i64) -> Result<TicketStatus> {
        let generation = self.generation.clone();
        let expected = self.expected.get(publisher).cloned();
        let transaction = self.transaction(now)?;
        Self::require_publisher(&transaction, expected.as_ref())?;
        let entry = Self::capability(&transaction, publisher, &generation, id)?
            .ok_or_else(|| Error::new(404, "not_found"))?;
        let ticket = entry.binding.ok_or_else(|| Error::new(404, "not_found"))?;
        let state = match entry.state.as_str() {
            "claimed" => "claimed",
            "revoked" => "revoked",
            "pending" if entry.expiry <= now => "expired",
            "pending" => "pending",
            _ => return Err(Error::unavailable()),
        };
        sql(transaction.commit())?;
        Ok(TicketStatus {
            version: VERSION,
            enrollment_id: (state == "claimed").then(|| ticket.ticket_id.clone()),
            ticket_id: ticket.ticket_id,
            status: state,
            expires_at: entry.expiry,
            server_instance_id: ticket.server_instance_id,
            installation_id: ticket.installation_id,
            receiver_id: ticket.receiver_id,
            consent_id: ticket.consent_id,
            phone_generation: ticket.phone_generation,
            consent_generation: ticket.consent_generation,
            transport_generation: ticket.transport_generation,
            platform: ticket.platform,
        })
    }
    pub fn revoke(&mut self, publisher: &str, id: &str, now: i64) -> Result<()> {
        wire::uuid(id)?;
        let generation = self.generation.clone();
        let max = self.limits.identities;
        let expected = self.expected.get(publisher).cloned();
        let transaction = self.transaction(now)?;
        Self::require_publisher(&transaction, expected.as_ref())?;
        if Self::capability(&transaction, publisher, &generation, id)?.is_none() {
            let count: i64 = sql(transaction.query_row(
                "SELECT count(*) FROM capabilities WHERE publisher=?1 AND generation=?2",
                params![publisher, generation],
                |row| row.get(0),
            ))?;
            capacity(count, max)?;
            sql(transaction.execute("INSERT INTO capabilities(publisher,generation,id,expiry,state) VALUES(?1,?2,?3,0,'revoked')",params![publisher,generation,id]))?;
        } else {
            sql(transaction.execute("UPDATE capabilities SET state='revoked',nonce=NULL,ciphertext=NULL WHERE publisher=?1 AND generation=?2 AND id=?3",params![publisher,generation,id]))?;
        }
        sql(transaction.commit())?;
        Ok(())
    }
    pub fn admit(&mut self, publisher: &str, delivery: &Delivery, now: i64) -> Result<Admission> {
        delivery.validate(now)?;
        let generation = self.generation.clone();
        let max = self.limits.deliveries;
        let cipher = self.cipher.clone();
        let expected = self.expected.get(publisher).cloned();
        let transaction = self.transaction(now)?;
        Self::require_publisher(&transaction, expected.as_ref())?;
        let entry = Self::capability(
            &transaction,
            publisher,
            &generation,
            &delivery.enrollment_id,
        )?
        .ok_or_else(|| Error::new(404, "not_found"))?;
        let ticket = entry.binding.ok_or_else(|| Error::new(404, "not_found"))?;
        if entry.state != "claimed"
            || !delivery.matches(&ticket)
            || !expected
                .as_ref()
                .is_some_and(|record| Self::compatible(record, &ticket))
        {
            return Err(Error::new(409, "stale_generation"));
        }
        let old:Option<String>=sql(transaction.query_row("SELECT binding FROM deliveries WHERE publisher=?1 AND generation=?2 AND invitation=?3",params![publisher,generation,delivery.invitation_id],|row|row.get(0)).optional())?;
        let binding = encoded(delivery)?;
        if let Some(old) = old {
            if old != binding {
                return Err(Error::new(409, "delivery_conflict"));
            }
            sql(transaction.commit())?;
            return Ok(Admission::Duplicate);
        }
        let count: i64 = sql(transaction.query_row(
            "SELECT count(*) FROM deliveries WHERE publisher=?1 AND generation=?2",
            params![publisher, generation],
            |row| row.get(0),
        ))?;
        capacity(count, max)?;
        let last:Option<i64>=sql(transaction.query_row("SELECT attempted_at FROM cooldowns WHERE publisher=?1 AND generation=?2 AND installation=?3",params![publisher,generation,delivery.installation_id],|row|row.get(0)).optional())?;
        let cooling = last.is_some_and(|last| now.checked_sub(last).is_none_or(|age| age < 1800));
        sql(transaction.execute(
            "INSERT INTO deliveries VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                publisher,
                generation,
                delivery.invitation_id,
                delivery.enrollment_id,
                binding,
                if cooling { "denied" } else { "attempted" }
            ],
        ))?;
        if cooling {
            sql(transaction.commit())?;
            return Ok(Admission::Denied);
        }
        let nonce = entry.nonce.ok_or_else(Error::unavailable)?;
        if nonce.len() != 12 {
            return Err(Error::unavailable());
        }
        let ciphertext = entry.ciphertext.ok_or_else(Error::unavailable)?;
        let aad = Self::aad(publisher, &generation, &ticket)?;
        let token = Zeroizing::new(
            cipher
                .decrypt(
                    Nonce::from_slice(&nonce),
                    Payload {
                        msg: &ciphertext,
                        aad: &aad,
                    },
                )
                .map_err(|_| Error::unavailable())?,
        );
        let device_token =
            Zeroizing::new(String::from_utf8(token.to_vec()).map_err(|_| Error::unavailable())?);
        sql(transaction.execute("INSERT INTO cooldowns VALUES(?1,?2,?3,?4) ON CONFLICT(publisher,generation,installation) DO UPDATE SET attempted_at=excluded.attempted_at",params![publisher,generation,delivery.installation_id,now]))?;
        // FULL synchronous commit precedes all external provider I/O. A crash or
        // cancellation leaves attempted durable; no restart may retry it.
        sql(transaction.commit())?;
        Ok(Admission::Attempt(Box::new(Attempt {
            publisher: publisher.to_owned(),
            enrollment: delivery.enrollment_id.clone(),
            platform: ticket.platform,
            device_token,
            delivery: delivery.clone(),
        })))
    }
    pub fn finish(
        &mut self,
        publisher: &str,
        invitation: &str,
        state: &str,
        revoke: bool,
        enrollment: &str,
        now: i64,
    ) -> Result<()> {
        if !["accepted", "denied", "unknown"].contains(&state) {
            return Err(Error::unavailable());
        }
        let generation = self.generation.clone();
        let expected = self.expected.get(publisher).cloned();
        let transaction = self.transaction(now)?;
        Self::require_publisher(&transaction, expected.as_ref())?;
        sql(transaction.execute("UPDATE deliveries SET state=?1 WHERE publisher=?2 AND generation=?3 AND invitation=?4 AND state='attempted'",params![state,publisher,generation,invitation]))?;
        if revoke {
            sql(transaction.execute("UPDATE capabilities SET state='revoked',nonce=NULL,ciphertext=NULL WHERE publisher=?1 AND generation=?2 AND id=?3",params![publisher,generation,enrollment]))?;
        }
        sql(transaction.commit())?;
        Ok(())
    }
}
#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
