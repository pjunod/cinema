//! Single transactional publication boundary for encrypted diagnostic evidence.
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    XChaCha20Poly1305, XNonce,
};
use plurx_core::diagnostics::{self, Privacy};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use subtle::ConstantTimeEq;

pub const CHUNK: usize = 1024 * 1024;
pub const ARCHIVE: usize = diagnostics::ARCHIVE_BYTES;
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0)
}
pub fn random() -> Result<String, Fault> {
    let mut b = [0u8; 32];
    getrandom::getrandom(&mut b).map_err(|_| Fault::Unavailable)?;
    Ok(hex::encode(b))
}
pub fn hash(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
pub fn canonical_hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub fn equal(a: &str, b: &str) -> bool {
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

#[derive(Debug, Clone, Copy)]
pub enum Fault {
    Invalid,
    Unauthorized,
    NotFound,
    Conflict,
    Expired,
    Quota,
    Unavailable,
}
impl From<rusqlite::Error> for Fault {
    fn from(_: rusqlite::Error) -> Self {
        Self::Unavailable
    }
}
impl From<std::io::Error> for Fault {
    fn from(_: std::io::Error) -> Self {
        Self::Unavailable
    }
}
pub type Result<T, E = Fault> = std::result::Result<T, E>;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantRequest {
    pub report_id: String,
    pub sha256: String,
    pub length: u32,
    pub privacy: Privacy,
}
impl GrantRequest {
    pub fn validate(&self) -> Result<()> {
        if diagnostics::canonical_uuid(&self.report_id)
            && canonical_hash(&self.sha256)
            && self.length > 0
            && self.length as usize <= ARCHIVE
        {
            Ok(())
        } else {
            Err(Fault::Invalid)
        }
    }
}
#[derive(Clone)]
pub struct Authority {
    pub digest: String,
    pub kind: String,
    pub publisher: String,
    pub report: String,
    pub parent: String,
    pub expires: i64,
    pub principal: String,
}
#[derive(Clone)]
pub struct Database {
    inner: Arc<Mutex<Connection>>,
    cipher: Arc<XChaCha20Poly1305>,
    key_id: String,
    dir: PathBuf,
    pub limits: Arc<crate::limits::Limits>,
}

pub fn atomic_secret(path: &Path, data: &[u8]) -> Result<()> {
    if path.exists() {
        return Err(Fault::Conflict);
    }
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_extension("pending");
    let mut f = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(data)?;
    f.sync_all()?;
    fs::rename(&tmp, path)?;
    if let Some(p) = path.parent() {
        fs::File::open(p)?.sync_all()?;
    }
    Ok(())
}
impl Database {
    pub fn open(dir: &Path, key_file: &Path) -> Result<Self> {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        fs::create_dir_all(dir)?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        let meta = fs::symlink_metadata(key_file)?;
        if !meta.is_file()
            || meta.permissions().mode() & 0o077 != 0
            || meta.len() != 32
            || meta.uid() != fs::metadata(dir)?.uid()
        {
            return Err(Fault::Invalid);
        }
        let key = fs::read(key_file)?;
        if key.len() != 32 {
            return Err(Fault::Invalid);
        }
        let key_id = hash(&key);
        let db = Connection::open(dir.join("support.db"))?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA secure_delete=ON; PRAGMA foreign_keys=ON; PRAGMA auto_vacuum=INCREMENTAL; PRAGMA max_page_count=11534336; PRAGMA journal_size_limit=33554432; PRAGMA wal_autocheckpoint=256;
   CREATE TABLE IF NOT EXISTS control(id INTEGER PRIMARY KEY CHECK(id=1),generation TEXT NOT NULL,key_id TEXT NOT NULL);
   CREATE TABLE IF NOT EXISTS publishers(id TEXT PRIMARY KEY,credential TEXT NOT NULL,active INTEGER NOT NULL,origins TEXT NOT NULL);
   CREATE TABLE IF NOT EXISTS caps(digest TEXT PRIMARY KEY,kind TEXT NOT NULL,publisher TEXT NOT NULL,report TEXT NOT NULL,parent TEXT NOT NULL,expires INTEGER NOT NULL,principal TEXT NOT NULL,active INTEGER NOT NULL,request TEXT NOT NULL);
   CREATE INDEX IF NOT EXISTS caps_report ON caps(report,kind);
   CREATE TABLE IF NOT EXISTS reports(id TEXT PRIMARY KEY,publisher TEXT NOT NULL,source TEXT NOT NULL,capture_scope TEXT NOT NULL,sha256 TEXT NOT NULL,length INTEGER NOT NULL,privacy TEXT NOT NULL,state TEXT NOT NULL,created INTEGER NOT NULL,touched INTEGER NOT NULL,expires INTEGER NOT NULL,reference TEXT UNIQUE NOT NULL,blob BLOB,metadata BLOB,description BLOB,revision INTEGER NOT NULL DEFAULT 1,label TEXT NOT NULL DEFAULT 'new');
   CREATE TABLE IF NOT EXISTS chunks(report TEXT NOT NULL,idx INTEGER NOT NULL,sha256 TEXT NOT NULL,blob BLOB NOT NULL,PRIMARY KEY(report,idx),FOREIGN KEY(report) REFERENCES reports(id) ON DELETE CASCADE);
   CREATE TABLE IF NOT EXISTS submissions(source TEXT NOT NULL,publisher TEXT NOT NULL,at INTEGER NOT NULL);
   CREATE TABLE IF NOT EXISTS handoffs(id TEXT PRIMARY KEY,report TEXT NOT NULL,creator TEXT NOT NULL,secret TEXT NOT NULL,code TEXT UNIQUE NOT NULL,state TEXT NOT NULL,expires INTEGER NOT NULL,proof TEXT NOT NULL,phrase TEXT NOT NULL,revision INTEGER NOT NULL,failed INTEGER NOT NULL DEFAULT 0);
   CREATE TABLE IF NOT EXISTS invitations(code TEXT PRIMARY KEY,publisher TEXT NOT NULL,expires INTEGER NOT NULL,proof TEXT NOT NULL,failed INTEGER NOT NULL DEFAULT 0,authority BLOB);
   CREATE TABLE IF NOT EXISTS offers(id TEXT PRIMARY KEY,blob BLOB NOT NULL);
   CREATE TABLE IF NOT EXISTS audit(id INTEGER PRIMARY KEY,at INTEGER NOT NULL,operator TEXT NOT NULL,action TEXT NOT NULL,report TEXT NOT NULL);
   CREATE TABLE IF NOT EXISTS operators(id TEXT PRIMARY KEY,name TEXT UNIQUE NOT NULL,active INTEGER NOT NULL,key BLOB);
   CREATE TABLE IF NOT EXISTS operator_tokens(digest TEXT PRIMARY KEY,operator TEXT NOT NULL,kind TEXT NOT NULL,expires INTEGER NOT NULL);")?;
        let guard = dir.join("generation.guard");
        let control: Option<(String, String)> = db
            .query_row(
                "SELECT generation,key_id FROM control WHERE id=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((generation, stored_key)) = control {
            if stored_key != key_id
                || fs::read_to_string(&guard)
                    .map(|s| s.trim() != generation)
                    .unwrap_or(true)
            {
                return Err(Fault::Conflict);
            }
        } else {
            if guard.exists() {
                return Err(Fault::Conflict);
            }
            let generation = random()?;
            atomic_secret(&guard, generation.as_bytes())?;
            db.execute(
                "INSERT INTO control VALUES(1,?1,?2)",
                params![generation, key_id],
            )?;
        }
        fs::set_permissions(dir.join("support.db"), fs::Permissions::from_mode(0o600))?;
        let result = Self {
            inner: Arc::new(Mutex::new(db)),
            cipher: Arc::new(XChaCha20Poly1305::new_from_slice(&key).map_err(|_| Fault::Invalid)?),
            key_id,
            dir: dir.to_owned(),
            limits: Arc::new(crate::limits::Limits::default()),
        };
        result.reconcile()?;
        Ok(result)
    }
    pub fn set_limits(&mut self, limits: crate::limits::Limits) {
        self.limits = Arc::new(limits);
    }
    pub fn with<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        let mut db = self.inner.lock().map_err(|_| Fault::Unavailable)?;
        f(&mut db)
    }
    pub fn seal(&self, aad: &str, plain: &[u8]) -> Result<Vec<u8>> {
        let mut nonce = [0u8; 24];
        getrandom::getrandom(&mut nonce).map_err(|_| Fault::Unavailable)?;
        let encrypted = self
            .cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: plain,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| Fault::Unavailable)?;
        let mut result = b"CD1".to_vec();
        result.extend_from_slice(self.key_id.as_bytes());
        result.extend_from_slice(&nonce);
        result.extend_from_slice(&encrypted);
        Ok(result)
    }
    pub fn unseal(&self, aad: &str, sealed: &[u8]) -> Result<Vec<u8>> {
        if sealed.len() < 107
            || &sealed[..3] != b"CD1"
            || !equal(
                std::str::from_utf8(&sealed[3..67]).map_err(|_| Fault::Invalid)?,
                &self.key_id,
            )
        {
            return Err(Fault::Invalid);
        }
        self.cipher
            .decrypt(
                XNonce::from_slice(&sealed[67..91]),
                Payload {
                    msg: &sealed[91..],
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| Fault::Invalid)
    }
    pub fn enroll_publisher(&self, id: &str, origins: &[String], out: &Path) -> Result<()> {
        if id.is_empty()
            || id.len() > 64
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        {
            return Err(Fault::Invalid);
        }
        if out.exists() || origins.is_empty() || origins.len() > 32 {
            return Err(Fault::Invalid);
        }
        for origin in origins {
            crate::transport::checked_origin(origin, true)?;
        }
        let secret = random()?;
        self.with(|db| {
            let tx = db.transaction()?;
            let count: i64 = tx.query_row("SELECT COUNT(*) FROM publishers", [], |r| r.get(0))?;
            if count >= i64::from(self.limits.publishers) {
                return Err(Fault::Quota);
            }
            tx.execute(
                "INSERT INTO publishers VALUES(?1,?2,1,?3)",
                params![
                    id,
                    hash(secret.as_bytes()),
                    serde_json::to_string(origins).map_err(|_| Fault::Invalid)?
                ],
            )?;
            atomic_secret(out, secret.as_bytes())?;
            tx.commit()?;
            Ok(())
        })
    }
    pub fn publisher(&self, id: &str, secret: &str) -> Result<()> {
        self.with(|db| {
            let expected: Option<String> = db
                .query_row(
                    "SELECT credential FROM publishers WHERE id=?1 AND active=1",
                    [id],
                    |r| r.get(0),
                )
                .optional()?;
            if expected.is_some_and(|v| equal(&v, &hash(secret.as_bytes()))) {
                Ok(())
            } else {
                Err(Fault::Unauthorized)
            }
        })
    }
    pub fn authority(&self, secret: &str) -> Result<Authority> {
        self.with(|db| Self::authority_in(db, secret))
    }
    pub(crate) fn authority_in(db: &Connection, secret: &str) -> Result<Authority> {
        if !canonical_hash(secret) {
            return Err(Fault::Unauthorized);
        }
        let digest = hash(secret.as_bytes());
        let mut current = digest.clone();
        let mut leaf = None;
        for _ in 0..4 {
            let cap:Option<Authority>=db.query_row("SELECT digest,kind,publisher,report,parent,expires,principal FROM caps WHERE digest=?1 AND active=1",[&current],|r|Ok(Authority{digest:r.get(0)?,kind:r.get(1)?,publisher:r.get(2)?,report:r.get(3)?,parent:r.get(4)?,expires:r.get(5)?,principal:r.get(6)?})).optional()?;
            let cap = cap.ok_or(Fault::Unauthorized)?;
            if cap.expires <= now() {
                return Err(Fault::Expired);
            }
            let active: i64 = db
                .query_row(
                    "SELECT active FROM publishers WHERE id=?1",
                    [&cap.publisher],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or(0);
            if active != 1 {
                return Err(Fault::Unauthorized);
            }
            if leaf.is_none() {
                leaf = Some(cap.clone());
            }
            if cap.parent.is_empty() {
                return leaf.ok_or(Fault::Unauthorized);
            }
            current = cap.parent;
        }
        Err(Fault::Unauthorized)
    }
    #[allow(clippy::too_many_arguments)] // Each bound authority field is explicit at its minting call site.
    pub(crate) fn mint(
        db: &Connection,
        kind: &str,
        publisher: &str,
        report: &str,
        parent: &str,
        principal: &str,
        expires: i64,
        request: &str,
    ) -> Result<String> {
        let count: i64 = db.query_row("SELECT COUNT(*) FROM caps", [], |r| r.get(0))?;
        if count >= 50000 {
            return Err(Fault::Quota);
        }
        let secret = random()?;
        db.execute(
            "INSERT INTO caps VALUES(?1,?2,?3,?4,?5,?6,?7,1,?8)",
            params![
                hash(secret.as_bytes()),
                kind,
                publisher,
                report,
                parent,
                expires,
                principal,
                request
            ],
        )?;
        Ok(secret)
    }
    pub fn enrollment(&self, publisher: &str, secret: &str, principal: &str) -> Result<Value> {
        self.publisher(publisher, secret)?;
        if !canonical_hash(principal) {
            return Err(Fault::Invalid);
        }
        self.with(|db|{let tx=db.transaction()?;let prior=tx.prepare("SELECT digest FROM caps WHERE publisher=?1 AND principal=?2 AND kind='enrollment' AND active=1")?.query_map(params![publisher,principal],|r|r.get::<_,String>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;for digest in prior {self.fence("cap",&digest)?;}tx.execute("UPDATE caps SET active=0 WHERE publisher=?1 AND principal=?2 AND kind='enrollment'",params![publisher,principal])?;let token=Self::mint(&tx,"enrollment",publisher,"","",principal,now()+86400,"")?;tx.commit()?;Ok(json!({"enrollment":token,"expires_in_ms":86400000,"revocation_delay_ms":86400000}))})
    }
    pub fn revoke_enrollment(&self, publisher: &str, secret: &str, principal: &str) -> Result<()> {
        self.publisher(publisher, secret)?;
        self.with(|db|{let prior=db.prepare("SELECT digest FROM caps WHERE publisher=?1 AND principal=?2 AND kind='enrollment' AND active=1")?.query_map(params![publisher,principal],|r|r.get::<_,String>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;for digest in prior {self.fence("cap",&digest)?;}db.execute("UPDATE caps SET active=0 WHERE publisher=?1 AND principal=?2 AND kind='enrollment'",params![publisher,principal])?;Ok(())})
    }
    pub fn grant(
        &self,
        publisher: Option<(&str, &str)>,
        secret: Option<&str>,
        request: &GrantRequest,
    ) -> Result<Value> {
        request.validate()?;
        let (publisher, parent, _principal, expires) = if let Some((id, credential)) = publisher {
            self.publisher(id, credential)?;
            (id.to_owned(), String::new(), String::new(), now() + 1800)
        } else {
            let cap = self.authority(secret.ok_or(Fault::Unauthorized)?)?;
            if !["enrollment", "invitation"].contains(&cap.kind.as_str()) {
                return Err(Fault::Unauthorized);
            }
            (
                cap.publisher,
                cap.digest,
                cap.principal,
                cap.expires.min(now() + 1800),
            )
        };
        let encoded = serde_json::to_string(request).map_err(|_| Fault::Invalid)?;
        self.with(|db|{let tx=db.transaction()?;
   let total:i64=tx.query_row("SELECT COUNT(*) FROM caps",[],|r|r.get(0))?;
   let pub_count:i64=tx.query_row("SELECT COUNT(*) FROM caps WHERE publisher=?1 AND active=1 AND expires>?2",params![publisher,now()],|r|r.get(0))?;
   let parent_count:i64=tx.query_row("SELECT COUNT(*) FROM caps WHERE parent=?1 AND active=1 AND expires>?2",params![parent,now()],|r|r.get(0))?;
   if total>=i64::from(self.limits.capability_rows) || pub_count>=2000 || (!parent.is_empty() && parent_count>=20){return Err(Fault::Quota);}
   if !parent.is_empty(){let active:i64=tx.query_row("SELECT active FROM caps WHERE digest=?1",[&parent],|r|r.get(0))?;if active!=1{return Err(Fault::Unauthorized);}}
   let existing:Option<(String,String,String)>=tx.query_row("SELECT publisher,source,sha256 FROM reports WHERE id=?1",[&request.report_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
   if existing.is_some(){return Err(Fault::Conflict);}
   if !parent.is_empty(){let kind:String=tx.query_row("SELECT kind FROM caps WHERE digest=?1",[&parent],|r|r.get(0))?;if kind=="invitation"{let used:i64=tx.query_row("SELECT COUNT(*) FROM caps WHERE parent=?1 AND kind='upload' AND report!=?2",params![parent,request.report_id],|r|r.get(0))?;if used>0{return Err(Fault::Unauthorized);}}}
   let capture=uuid::Uuid::new_v4().to_string();
   let token=Self::mint(&tx,"upload",&publisher,&request.report_id,&parent,&capture,expires,&encoded)?;tx.commit()?;Ok(json!({"upload_grant":token,"report_id":request.report_id,"expires_in_ms":(expires-now())*1000}))
  })
    }
    pub fn reserve(&self, secret: &str, request: &GrantRequest) -> Result<Value> {
        request.validate()?;
        self.with(|db|{let tx=db.transaction()?;let cap=Self::authority_in(&tx,secret)?;let source=if cap.parent.is_empty(){String::new()}else{tx.query_row("SELECT principal FROM caps WHERE digest=?1",[&cap.parent],|r|r.get::<_,String>(0))?};if cap.kind!="upload" || cap.report!=request.report_id{return Err(Fault::Unauthorized);}
   let bound:String=tx.query_row("SELECT request FROM caps WHERE digest=?1",[&cap.digest],|r|r.get(0))?;if bound!=serde_json::to_string(request).map_err(|_|Fault::Invalid)?{return Err(Fault::Conflict);}
   let existing:Option<(String,String,i64,String,String)>=tx.query_row("SELECT source,sha256,length,state,capture_scope FROM reports WHERE id=?1",[&request.report_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
   if let Some((source,sha,length,state,capture))=existing {if capture!=cap.principal || source!=if cap.parent.is_empty(){String::new()}else{tx.query_row("SELECT principal FROM caps WHERE digest=?1",[&cap.parent],|r|r.get::<_,String>(0))?} || sha!=request.sha256 || length!=i64::from(request.length) || ["deleted","expired"].contains(&state.as_str()){return Err(Fault::Conflict);}}else{
    let rows:i64=tx.query_row("SELECT COUNT(*) FROM reports",[],|r|r.get(0))?;if rows>=i64::from(self.limits.report_rows){return Err(Fault::Quota);}
    let global:i64=tx.query_row("SELECT COALESCE(SUM(length),0) FROM reports WHERE state NOT IN ('deleted','expired')",[],|r|r.get(0))?;
    let publisher_bytes:i64=tx.query_row("SELECT COALESCE(SUM(length),0) FROM reports WHERE publisher=?1 AND state NOT IN ('deleted','expired')",[&cap.publisher],|r|r.get(0))?;
    let pending:i64=tx.query_row("SELECT COUNT(*) FROM reports WHERE publisher=?1 AND state IN ('reserved','uploading','validating','staged')",[&cap.publisher],|r|r.get(0))?;
    let source_pending:i64=tx.query_row("SELECT COUNT(*) FROM reports WHERE source=?1 AND publisher=?2 AND state IN ('reserved','uploading','validating','staged')",params![source,cap.publisher],|r|r.get(0))?;
    let daily:i64=tx.query_row("SELECT COUNT(*) FROM submissions WHERE publisher=?1 AND at>?2",params![cap.publisher,now()-86400],|r|r.get(0))?;
    let source_daily:i64=tx.query_row("SELECT COUNT(*) FROM submissions WHERE source=?1 AND publisher=?2 AND at>?3",params![source,cap.publisher,now()-86400],|r|r.get(0))?;
    if global+i64::from(request.length)>self.limits.bytes_global as i64 || publisher_bytes+i64::from(request.length)>self.limits.bytes_per_publisher as i64 || pending>=i64::from(self.limits.uploads_per_publisher) || (!cap.parent.is_empty() && source_pending>=i64::from(self.limits.uploads_per_enrollment)) || daily>=i64::from(self.limits.submissions_per_publisher_day) || (!cap.parent.is_empty() && source_daily>=i64::from(self.limits.submissions_per_enrollment_day)){return Err(Fault::Quota);}
    let reference=format!("C-{}",&uuid::Uuid::new_v4().simple().to_string()[..12]);
    tx.execute("INSERT INTO reports(id,publisher,source,sha256,length,privacy,state,created,touched,expires,reference,capture_scope) VALUES(?1,?2,?3,?4,?5,?6,'reserved',?7,?7,?8,?9,?10)",params![request.report_id,cap.publisher,source,request.sha256,request.length,serde_json::to_string(&request.privacy).map_err(|_|Fault::Invalid)?,now(),now()+86400,reference,cap.principal])?;
   }
   // Ambiguous reservation retries recover this report, never another report.
   tx.execute("UPDATE caps SET active=0 WHERE report=?1 AND kind='creator'",[&request.report_id])?;
   let management=Self::mint(&tx,"creator",&cap.publisher,&request.report_id,&cap.parent,&cap.principal,now()+86400,"")?;tx.commit()?;
   Ok(json!({"upload_id":request.report_id,"management_grant":management,"chunk_bytes":CHUNK}))
  })
    }
    pub(crate) fn report_auth(
        db: &Connection,
        secret: &str,
        id: &str,
        kinds: &[&str],
    ) -> Result<Authority> {
        let cap = Self::authority_in(db, secret)?;
        if cap.report != id || !kinds.contains(&cap.kind.as_str()) {
            return Err(Fault::Unauthorized);
        }
        let publisher:Option<String>=db.query_row("SELECT publisher FROM reports WHERE id=?1 AND state NOT IN ('deleted','expired') AND expires>?2",params![id,now()],|r|r.get(0)).optional()?;
        if publisher.as_deref() != Some(&cap.publisher) {
            return Err(Fault::NotFound);
        }
        if cap.kind != "phone" {
            let capture: String =
                db.query_row("SELECT capture_scope FROM reports WHERE id=?1", [id], |r| {
                    r.get(0)
                })?;
            if capture != cap.principal {
                return Err(Fault::Unauthorized);
            }
        }
        if cap.kind == "phone" {
            let approved:i64=db.query_row("SELECT COUNT(*) FROM handoffs WHERE report=?1 AND state IN ('approved','submitted') AND proof=?2 AND expires>?3 AND revision=(SELECT revision FROM reports WHERE id=?1)",params![id,cap.principal,now()],|r|r.get(0))?;
            if approved != 1 {
                return Err(Fault::Unauthorized);
            }
        }
        Ok(cap)
    }
    pub fn refresh(&self, secret: &str, id: &str) -> Result<Value> {
        self.with(|db| {
            let tx = db.transaction()?;
            let cap = Self::report_auth(&tx, secret, id, &["creator"])?;
            let (sha256, length, privacy, expiry, state): (String, u32, String, i64, String) = tx
                .query_row(
                "SELECT sha256,length,privacy,expires,state FROM reports WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )?;
            if !["reserved", "uploading"].contains(&state.as_str()) {
                return Err(Fault::Conflict);
            }
            let request = GrantRequest {
                report_id: id.into(),
                sha256,
                length,
                privacy: serde_json::from_str(&privacy).map_err(|_| Fault::Invalid)?,
            };
            tx.execute(
                "UPDATE caps SET active=0 WHERE report=?1 AND kind='upload'",
                [id],
            )?;
            let expiry = expiry.min(cap.expires).min(now() + 1800);
            let token = Self::mint(
                &tx,
                "upload",
                &cap.publisher,
                id,
                &cap.parent,
                &cap.principal,
                expiry,
                &serde_json::to_string(&request).map_err(|_| Fault::Invalid)?,
            )?;
            tx.commit()?;
            Ok(json!({"upload_grant":token,"expires_in_ms":(expiry-now())*1000}))
        })
    }
    pub fn chunk(
        &self,
        secret: &str,
        id: &str,
        index: u32,
        digest: &str,
        bytes: &[u8],
    ) -> Result<Value> {
        if !canonical_hash(digest) || !equal(digest, &hash(bytes)) {
            return Err(Fault::Invalid);
        }
        self.with(|db| {
            let tx = db.transaction()?;
            let cap = Self::report_auth(&tx, secret, id, &["upload"])?;
            let (length, state): (i64, String) =
                tx.query_row("SELECT length,state FROM reports WHERE id=?1", [id], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?;
            let offset = index as usize * CHUNK;
            if offset >= length as usize || bytes.len() != (length as usize - offset).min(CHUNK) {
                return Err(Fault::Invalid);
            }
            if !["reserved", "uploading"].contains(&state.as_str()) {
                return Err(Fault::Conflict);
            }
            let prior: Option<String> = tx
                .query_row(
                    "SELECT sha256 FROM chunks WHERE report=?1 AND idx=?2",
                    params![id, index],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(prior) = prior {
                if !equal(&prior, digest) {
                    return Err(Fault::Conflict);
                }
            } else {
                let sealed = self.seal(&format!("{id}/{}/chunk/{index}", cap.publisher), bytes)?;
                tx.execute(
                    "INSERT INTO chunks VALUES(?1,?2,?3,?4)",
                    params![id, index, digest, sealed],
                )?;
            }
            tx.execute(
                "UPDATE reports SET state='uploading',touched=?2 WHERE id=?1",
                params![id, now()],
            )?;
            tx.commit()?;
            Ok(json!({"index":index,"received":true}))
        })
    }
    pub fn status(&self, secret: &str, id: &str) -> Result<Value> {
        self.with(|db| {
            Self::report_auth(db, secret, id, &["upload", "creator", "phone"])?;
            let (state, expires, reference): (String, i64, String) = db.query_row(
                "SELECT state,expires,reference FROM reports WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            let indexes = db
                .prepare("SELECT idx FROM chunks WHERE report=?1 ORDER BY idx")?
                .query_map([id], |r| r.get::<_, u32>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(json!({"state":state,"reference":reference,"expires_at":expires,"received":indexes}))
        })
    }
    pub fn complete(&self, secret: &str, id: &str) -> Result<Value> {
        self.with(|db| {
            let tx = db.transaction()?;
            let cap = Self::report_auth(&tx, secret, id, &["upload"])?;
            let (length, digest, state, privacy): (i64, String, String, String) = tx.query_row(
                "SELECT length,sha256,state,privacy FROM reports WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
            if ["staged", "submitted"].contains(&state.as_str()) {
                tx.commit()?;
                return Ok(json!({"state":state,"report_id":id}));
            }
            let chunks = tx
                .prepare("SELECT idx,blob FROM chunks WHERE report=?1 ORDER BY idx")?
                .query_map([id], |r| Ok((r.get::<_, u32>(0)?, r.get::<_, Vec<u8>>(1)?)))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            if chunks.len() != (length as usize).div_ceil(CHUNK) {
                return Err(Fault::Conflict);
            }
            let mut archive = Vec::with_capacity(length as usize);
            for (wanted, (index, blob)) in chunks.into_iter().enumerate() {
                if index as usize != wanted {
                    return Err(Fault::Conflict);
                }
                archive.extend_from_slice(
                    &self.unseal(&format!("{id}/{}/chunk/{index}", cap.publisher), &blob)?,
                );
                if archive.len() > ARCHIVE {
                    return Err(Fault::Invalid);
                }
            }
            if archive.len() != length as usize || !equal(&hash(&archive), &digest) {
                return Err(Fault::Invalid);
            }
            let inspected = diagnostics::inspect(&archive).map_err(|_| Fault::Invalid)?;
            if inspected.manifest.report_id != id
                || serde_json::to_string(&inspected.manifest.privacy).map_err(|_| Fault::Invalid)?
                    != privacy
            {
                return Err(Fault::Invalid);
            }
            let metadata = serde_json::to_vec(&inspected.manifest).map_err(|_| Fault::Invalid)?;
            Self::authority_in(&tx, secret)?;
            let encrypted = self.seal(&format!("{id}/{}/archive", cap.publisher), &archive)?;
            let metadata = self.seal(&format!("{id}/{}/metadata", cap.publisher), &metadata)?;
            tx.execute(
                "UPDATE reports SET state='staged',blob=?2,metadata=?3,touched=?4 WHERE id=?1",
                params![id, encrypted, metadata, now()],
            )?;
            tx.execute("DELETE FROM chunks WHERE report=?1", [id])?;
            tx.commit()?;
            Ok(json!({"state":"staged","report_id":id}))
        })
    }
    pub fn submit(&self, secret: &str, id: &str, description: &str) -> Result<Value> {
        if description.len() > 16384
            || description.chars().count() > 4000
            || description
                .chars()
                .any(|c| c.is_control() && !['\n', '\r', '\t'].contains(&c))
        {
            return Err(Fault::Invalid);
        }
        self.with(|db|{let tx=db.transaction()?;let cap=Self::report_auth(&tx,secret,id,&["creator","phone"])?;
   let state:String=tx.query_row("SELECT state FROM reports WHERE id=?1",[id],|r|r.get(0))?;
   if state!="submitted" {
    if state!="staged"{return Err(Fault::Conflict);}
    let count:i64=tx.query_row("SELECT COUNT(*) FROM submissions WHERE publisher=?1 AND at>?2",params![cap.publisher,now()-86400],|r|r.get(0))?;if count>=i64::from(self.limits.submissions_per_publisher_day){return Err(Fault::Quota);}
    let source:String=tx.query_row("SELECT source FROM reports WHERE id=?1",[id],|r|r.get(0))?;let own_count:i64=tx.query_row("SELECT COUNT(*) FROM submissions WHERE source=?1 AND publisher=?2 AND at>?3",params![source,cap.publisher,now()-86400],|r|r.get(0))?;if !source.is_empty() && own_count>=i64::from(self.limits.submissions_per_enrollment_day){return Err(Fault::Quota);}
    let encrypted=self.seal(&format!("{id}/{}/description",cap.publisher),description.as_bytes())?;
    tx.execute("UPDATE reports SET state='submitted',description=?2,touched=?3,expires=?4 WHERE id=?1",params![id,encrypted,now(),now()+30*86400])?;
    tx.execute("INSERT INTO submissions SELECT source,publisher,?2 FROM reports WHERE id=?1",params![id,now()])?;
    tx.execute("UPDATE handoffs SET state='submitted' WHERE report=?1 AND state='approved'",[id])?;
   }
   let reference:String=tx.query_row("SELECT reference FROM reports WHERE id=?1",[id],|r|r.get(0))?;tx.commit()?;Ok(json!({"state":"submitted","reference":reference,"report_id":id}))
  })
    }
    pub(crate) fn erase_in(db: &Connection, id: &str) -> Result<()> {
        db.execute(
            "DELETE FROM offers WHERE id IN (SELECT id FROM handoffs WHERE report=?1)",
            [id],
        )?;
        db.execute("DELETE FROM chunks WHERE report=?1", [id])?;
        db.execute("UPDATE reports SET blob=NULL,metadata=NULL,description=NULL,state='deleted',privacy='{}',length=0 WHERE id=?1",[id])?;
        db.execute("UPDATE caps SET active=0,request='' WHERE report=?1", [id])?;
        db.execute("UPDATE handoffs SET state='revoked',secret='',code='revoked:'||id,proof=''  WHERE report=?1",[id])?;
        Ok(())
    }
    pub fn erase(&self, secret: &str, id: &str) -> Result<()> {
        self.with(|db| {
            Self::report_auth(db, secret, id, &["creator", "phone"])?;
            Ok(())
        })?;
        self.delete_journal(id)?;
        self.with(|db| {
            let tx = db.transaction()?;
            Self::erase_in(&tx, id)?;
            tx.commit()?;
            Ok(())
        })
    }
    pub fn delete_journal(&self, id: &str) -> Result<()> {
        if !diagnostics::canonical_uuid(id) {
            return Err(Fault::Invalid);
        }
        use std::os::unix::fs::OpenOptionsExt;
        let path = self.dir.join("deletions.journal");
        if fs::metadata(&path).is_ok_and(|m| m.len() >= 8 * 1024 * 1024) {
            return Err(Fault::Quota);
        }
        let mut journal = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&path)?;
        writeln!(journal, "{id}")?;
        journal.sync_all()?;
        fs::File::open(&self.dir)?.sync_all()?;
        Ok(())
    }
    pub fn fence(&self, kind: &str, id: &str) -> Result<()> {
        if !["publisher", "operator", "cap", "handoff"].contains(&kind)
            || id.is_empty()
            || id.len() > 64
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        {
            return Err(Fault::Invalid);
        }
        use std::os::unix::fs::OpenOptionsExt;
        let path = self.dir.join("revocations.journal");
        if fs::metadata(&path).is_ok_and(|m| m.len() >= 8 * 1024 * 1024) {
            return Err(Fault::Quota);
        }
        let mut f = OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(path)?;
        writeln!(f, "{kind}|{id}")?;
        f.sync_all()?;
        fs::File::open(&self.dir)?.sync_all()?;
        Ok(())
    }
    pub fn reconcile(&self) -> Result<()> {
        let mut deleted = String::new();
        match fs::File::open(self.dir.join("deletions.journal")) {
            Ok(file) => {
                file.take(8 * 1024 * 1024 + 1)
                    .read_to_string(&mut deleted)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(Fault::Unavailable),
        }
        if deleted.len() > 8 * 1024 * 1024 {
            return Err(Fault::Quota);
        }
        let mut revoked = String::new();
        match fs::File::open(self.dir.join("revocations.journal")) {
            Ok(f) => {
                f.take(8 * 1024 * 1024 + 1).read_to_string(&mut revoked)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(Fault::Unavailable),
        }
        if revoked.len() > 8 * 1024 * 1024 {
            return Err(Fault::Quota);
        }
        self.with(|db|{let tx=db.transaction()?;for line in revoked.lines(){let(kind,id)=line.split_once('|').ok_or(Fault::Invalid)?;match kind {"publisher"=>{tx.execute("UPDATE publishers SET active=0 WHERE id=?1",[id])?;},"operator"=>{tx.execute("UPDATE operators SET active=0,key=NULL WHERE name=?1",[id])?;},"cap"=>{tx.execute("UPDATE caps SET active=0 WHERE digest=?1",[id])?;},"handoff"=>{tx.execute("UPDATE handoffs SET state='revoked',secret='',code='revoked:'||id,proof='' WHERE id=?1",[id])?;},_=>return Err(Fault::Invalid)}}
   for id in deleted.lines(){if !diagnostics::canonical_uuid(id){return Err(Fault::Invalid);}Self::erase_in(&tx,id)?;}
   let expired=tx.prepare("SELECT id FROM reports WHERE state NOT IN ('deleted','expired') AND (expires<=?1 OR (state IN ('reserved','uploading','validating') AND touched<=?2)) LIMIT 1000")?.query_map(params![now(),now()-3600],|r|r.get::<_,String>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
   for id in expired {self.delete_journal(&id)?;Self::erase_in(&tx,&id)?;}
   tx.execute("DELETE FROM reports WHERE state IN ('deleted','expired') AND created<?1",[now()-90*86400])?;
   tx.execute("DELETE FROM offers WHERE id IN (SELECT id FROM handoffs WHERE expires<?1)",[now()-86400])?;
   tx.execute("DELETE FROM handoffs WHERE expires<?1",[now()-86400])?;
   tx.execute("DELETE FROM caps WHERE expires<?1 OR active=0",[now()-86400])?;tx.execute("DELETE FROM submissions WHERE at<?1",[now()-86400])?;
   tx.execute("UPDATE handoffs SET state='expired',secret='',code='revoked:'||id,proof=''  WHERE expires<?1 AND state NOT IN ('submitted','revoked','expired')",[now()])?;
   tx.execute("DELETE FROM invitations WHERE expires<?1",[now()-86400])?;
   tx.execute("DELETE FROM operator_tokens WHERE expires<?1",[now()])?;tx.execute("DELETE FROM audit WHERE at<?1",[now()-90*86400])?;tx.commit()?;db.execute_batch("DELETE FROM audit WHERE id NOT IN (SELECT id FROM audit ORDER BY id DESC LIMIT 100000); PRAGMA incremental_vacuum(256); PRAGMA wal_checkpoint(TRUNCATE);")?;Ok(())})
    }
}
