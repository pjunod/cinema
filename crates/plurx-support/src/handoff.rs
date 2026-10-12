//! One-report phone authority exists only after the creator's TV approval.
type InvitationRow = (String, i64, String, i64, Option<Vec<u8>>);

use crate::{
    store::{equal, hash, now, random, Database, Fault, Result},
    transport::{decode, fields, private, secret, text, Service},
};
use axum::{
    http::{header, HeaderMap, Method},
    response::{Html, IntoResponse, Response},
};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Clone, Default)]
pub struct Rates(Arc<Mutex<HashMap<String, (Instant, u32)>>>);
impl Rates {
    pub(crate) fn admit(&self, key: &str, limit: u32) -> Result<()> {
        let mut m = self.0.lock().map_err(|_| Fault::Unavailable)?;
        m.retain(|_, (at, _)| at.elapsed() < Duration::from_secs(60));
        if !m.contains_key(key) && m.len() >= 128 {
            return Err(Fault::Quota);
        }
        let entry = m.entry(key.to_owned()).or_insert((Instant::now(), 0));
        if entry.1 >= limit {
            return Err(Fault::Quota);
        }
        entry.1 += 1;
        Ok(())
    }
}
fn code(n: usize) -> Result<String> {
    let alphabet = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";
    let mut b = vec![0u8; n];
    getrandom::getrandom(&mut b).map_err(|_| Fault::Unavailable)?;
    Ok(b.into_iter()
        .map(|v| alphabet[(v as usize) % alphabet.len()] as char)
        .collect())
}
pub fn invitation(db: &Database, operator: &str, publisher: &str) -> Result<Value> {
    let code = code(10)?;
    db.with(|c|{let tx=c.transaction()?;
  let active:i64=tx.query_row("SELECT active FROM publishers WHERE id=?1",[publisher],|r|r.get(0)).optional()?.unwrap_or(0);if active!=1{return Err(Fault::NotFound);}
  tx.execute_batch("CREATE TABLE IF NOT EXISTS invitations(code TEXT PRIMARY KEY,publisher TEXT NOT NULL,expires INTEGER NOT NULL,proof TEXT NOT NULL,failed INTEGER NOT NULL DEFAULT 0,authority BLOB);")?;
  let count:i64=tx.query_row("SELECT COUNT(*) FROM invitations WHERE expires>?1",[now()],|r|r.get(0))?;if count>=1000{return Err(Fault::Quota);}
  tx.execute("INSERT INTO invitations(code,publisher,expires,proof) VALUES(?1,?2,?3,'')",params![hash(code.as_bytes()),publisher,now()+600])?;
  tx.execute("INSERT INTO audit(at,operator,action,report) VALUES(?1,?2,'invitation','')",params![now(),operator])?;tx.commit()?;
  Ok(json!({"code":code,"expires_in_ms":600000,"authority":"one_report_upload_only"}))
 })
}
pub fn execute(
    service: &Service,
    path: &str,
    method: &Method,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<Value> {
    let p = path.split('/').collect::<Vec<_>>();
    let db = &service.db;
    match (method.as_str(), p.as_slice()) {
        ("POST", ["invitations", "redeem"]) => {
            let v = decode(body)?;
            fields(&v, &["code", "proof"], &[])?;
            let code = text(&v, "code")?;
            let proof = text(&v, "proof")?;
            if code.len() != 10 || !crate::store::canonical_hash(proof) {
                return Err(Fault::Invalid);
            }
            service.rates.admit("invitation-global", 640)?;
            db.with(|c|{let tx=c.transaction()?;let row:Option<InvitationRow>=tx.query_row("SELECT publisher,expires,proof,failed,authority FROM invitations WHERE code=?1",[hash(code.as_bytes())],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
    let (publisher,expiry,prior,failed,authority)=row.ok_or(Fault::Unauthorized)?;if expiry<=now(){return Err(Fault::Expired);}
    let digest=hash(proof.as_bytes());
    if !prior.is_empty() && prior!=digest {if failed>=5{return Err(Fault::Quota);}tx.execute("UPDATE invitations SET failed=failed+1 WHERE code=?1",[hash(code.as_bytes())])?;tx.commit()?;return Err(Fault::Conflict);}
    tx.execute("UPDATE invitations SET proof=?2 WHERE code=?1",params![hash(code.as_bytes()),digest])?;
    // An ambiguous response returns the same authority; it cannot invalidate an upload already in flight.
    let code_hash=hash(code.as_bytes());let token=if let Some(blob)=authority {String::from_utf8(db.unseal(&format!("invitation/{code_hash}/authority"),&blob)?).map_err(|_|Fault::Invalid)?}else{let token=Database::mint(&tx,"invitation",&publisher,"","",&digest,now()+86400,"")?;let encrypted=db.seal(&format!("invitation/{code_hash}/authority"),token.as_bytes())?;tx.execute("UPDATE invitations SET authority=?2 WHERE code=?1",params![code_hash,encrypted])?;token};tx.commit()?;Ok(json!({"invitation":token,"expires_in_ms":86400000}))
   })
        }
        ("POST", ["handoffs"]) => {
            let v = decode(body)?;
            fields(&v, &["report_id"], &[])?;
            let id = text(&v, "report_id")?;
            let authorization = secret(headers)?;
            db.with(|c|{let tx=c.transaction()?;let creator=Database::report_auth(&tx,&authorization,id,&["creator"])?;
    let(state,revision):(String,i64)=tx.query_row("SELECT state,revision FROM reports WHERE id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?)))?;
    if state!="staged"{return Err(Fault::Conflict);}
    let active:Option<(String,i64)>=tx.query_row("SELECT id,expires FROM handoffs WHERE report=?1 AND state IN ('offered','claim_pending','approved') AND expires>?2",params![id,now()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    if let Some((existing,expires))=active {let sealed:Vec<u8>=tx.query_row("SELECT blob FROM offers WHERE id=?1",[&existing],|r|r.get(0))?;let mut offer:Value=serde_json::from_slice(&db.unseal(&format!("handoff/{existing}/offer"),&sealed)?).map_err(|_|Fault::Invalid)?;offer["expires_in_ms"]=((expires-now())*1000).into();return Ok(offer);}
    let handoff=uuid::Uuid::new_v4().to_string();let claim=random()?;let manual=code(10)?;let phrase=code(6)?;
    tx.execute("INSERT INTO handoffs(id,report,creator,secret,code,state,expires,proof,phrase,revision) VALUES(?1,?2,?3,?4,?5,'offered',?6,'',?7,?8)",params![handoff,id,creator.digest,hash(claim.as_bytes()),hash(manual.as_bytes()),now()+600,phrase,revision])?;let offer=json!({"id":handoff,"state":"offered","claim_secret":claim,"manual_code":manual,"expires_in_ms":600000,"url":format!("{}/#handoff={claim}",service.support_origin)});
    let sealed=db.seal(&format!("handoff/{handoff}/offer"),&serde_json::to_vec(&offer).map_err(|_|Fault::Invalid)?)?;tx.execute("INSERT INTO offers VALUES(?1,?2)",params![handoff,sealed])?;tx.commit()?;Ok(offer)
   })
        }
        ("POST", ["handoffs", "claim"]) => {
            let v = decode(body)?;
            fields(&v, &["proof"], &["code", "claim_secret"])?;
            let proof = text(&v, "proof")?;
            if !crate::store::canonical_hash(proof) {
                return Err(Fault::Invalid);
            }
            if v.get("code").is_some() == v.get("claim_secret").is_some() {
                return Err(Fault::Invalid);
            }
            service.rates.admit("claim-global", 640)?;
            let lookup = if let Some(value) = v.get("code").and_then(Value::as_str) {
                if value.len() != 10 {
                    return Err(Fault::Invalid);
                }
                hash(value.as_bytes())
            } else {
                let value = text(&v, "claim_secret")?;
                if !crate::store::canonical_hash(value) {
                    return Err(Fault::Invalid);
                }
                hash(value.as_bytes())
            };
            let qr = v.get("claim_secret").is_some();
            let digest = hash(proof.as_bytes());
            db.with(|c|{let tx=c.transaction()?;
    let query=if qr{"SELECT id,report,state,expires,proof,phrase,failed FROM handoffs WHERE secret=?1"}else{"SELECT id,report,state,expires,proof,phrase,failed FROM handoffs WHERE code=?1"};
    let row:Option<(String,String,String,i64,String,String,i64)>=tx.query_row(query,[lookup],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))).optional()?;
    let (id,report,state,expires,prior,phrase,failed)=row.ok_or(Fault::Unauthorized)?;if expires<=now(){return Err(Fault::Expired);}
    if state=="claim_pending" || state=="approved" {if prior!=digest{if failed>=5{return Err(Fault::Quota);}tx.execute("UPDATE handoffs SET failed=failed+1 WHERE id=?1",[&id])?;tx.commit()?;return Err(Fault::Conflict);}return Ok(json!({"id":id,"state":state,"comparison":phrase,"expires_at":expires}));}
    if state!="offered" || failed>=5{return Err(Fault::Conflict);}
    let active:i64=tx.query_row("SELECT COUNT(*) FROM reports r JOIN publishers p ON p.id=r.publisher WHERE r.id=?1 AND r.state='staged' AND p.active=1 AND r.expires>?2",params![report,now()],|r|r.get(0))?;if active!=1{return Err(Fault::Expired);}
    tx.execute("UPDATE handoffs SET state='claim_pending',proof=?2 WHERE id=?1 AND state='offered'",params![id,digest])?;tx.commit()?;
    // No report ID, contents or description is released before approval.
    Ok(json!({"id":id,"state":"claim_pending","comparison":phrase,"expires_at":expires}))
   })
        }
        ("POST", ["handoffs", id, "qr"]) => {
            let authorization = secret(headers)?;
            db.with(|c|{let report:String=c.query_row("SELECT report FROM handoffs WHERE id=?1 AND expires>?2 AND state IN ('offered','claim_pending')",params![id,now()],|r|r.get(0)).optional()?.ok_or(Fault::Expired)?;Database::report_auth(c,&authorization,&report,&["creator"])?;
                let sealed:Vec<u8>=c.query_row("SELECT blob FROM offers WHERE id=?1",[*id],|r|r.get(0))?;let offer:Value=serde_json::from_slice(&db.unseal(&format!("handoff/{id}/offer"),&sealed)?).map_err(|_|Fault::Invalid)?;let url=offer["url"].as_str().ok_or(Fault::Invalid)?;
                let code=qrcode::QrCode::new(url.as_bytes()).map_err(|_|Fault::Invalid)?;let svg=code.render::<qrcode::render::svg::Color>().min_dimensions(256,256).build();Ok(json!({"svg":svg}))
            })
        }
        ("GET", ["handoffs", id, "status"]) => {
            let authorization = secret(headers)?;
            db.with(|c|{let (report,state,proof,phrase,expires):(String,String,String,String,i64)=c.query_row("SELECT report,state,proof,phrase,expires FROM handoffs WHERE id=?1",[*id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?.ok_or(Fault::NotFound)?;Database::report_auth(c,&authorization,&report,&["creator"])?;Ok(json!({"id":id,"state":if expires<=now(){"expired"}else{&state},"claim_present":!proof.is_empty(),"comparison":phrase,"expires_at":expires}))})
        }
        ("GET", ["handoffs", id, "phone-status"]) => {
            let proof = secret(headers)?;
            let digest = hash(proof.as_bytes());
            db.with(|c|{let tx=c.transaction()?;let (report,state,bound,expires,revision):(String,String,String,i64,i64)=tx.query_row("SELECT report,state,proof,expires,revision FROM handoffs WHERE id=?1",[*id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?.ok_or(Fault::NotFound)?;
    if !equal(&bound,&digest){return Err(Fault::Unauthorized);}
    if expires<=now(){return Err(Fault::Expired);}
    if state=="claim_pending"{return Ok(json!({"state":"claim_pending","expires_at":expires}));}
    if !["approved","submitted"].contains(&state.as_str()){return Err(Fault::Unauthorized);}
    let(publisher,current,reference):(String,i64,String)=tx.query_row("SELECT publisher,revision,reference FROM reports WHERE id=?1 AND expires>?2 AND state IN ('staged','submitted')",params![report,now()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?.ok_or(Fault::Expired)?;if current!=revision{return Err(Fault::Conflict);}
    tx.execute("UPDATE caps SET active=0 WHERE kind='phone' AND report=?1 AND principal=?2",params![report,digest])?;
    let token=Database::mint(&tx,"phone",&publisher,&report,"",&digest,expires,"")?;tx.commit()?;Ok(json!({"state":state,"report_id":report,"management_grant":token,"reference":reference,"expires_at":expires}))
   })
        }
        ("POST", ["handoffs", id, "approve"]) => {
            let v = decode(body)?;
            fields(&v, &["comparison"], &[])?;
            let authorization = secret(headers)?;
            db.with(|c| {
                let tx = c.transaction()?;
                let (report, state, phrase, expires, revision): (String, String, String, i64, i64) =
                    tx.query_row(
                        "SELECT report,state,phrase,expires,revision FROM handoffs WHERE id=?1",
                        [*id],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                    )
                    .optional()?
                    .ok_or(Fault::NotFound)?;
                Database::report_auth(&tx, &authorization, &report, &["creator"])?;
                if state != "claim_pending"
                    || expires <= now()
                    || !equal(&phrase, text(&v, "comparison")?)
                {
                    return Err(Fault::Conflict);
                }
                let (current, absolute): (i64, i64) = tx.query_row(
                    "SELECT revision,created+86400 FROM reports WHERE id=?1 AND state='staged'",
                    [&report],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                if current != revision {
                    return Err(Fault::Conflict);
                }
                tx.execute(
                    "UPDATE handoffs SET state='approved',expires=?2 WHERE id=?1",
                    params![id, (now() + 1800).min(absolute)],
                )?;
                tx.commit()?;
                Ok(json!({"state":"approved"}))
            })
        }
        ("POST", ["handoffs", id, "reject"]) | ("DELETE", ["handoffs", id]) => {
            let authorization = secret(headers)?;
            let keep = if body.is_empty() {
                false
            } else {
                let v = decode(body)?;
                fields(&v, &["keep_draft"], &[])?;
                v["keep_draft"].as_bool().ok_or(Fault::Invalid)?
            };
            db.with(|c|{let tx=c.transaction()?;let(report,state):(String,String)=tx.query_row("SELECT report,state FROM handoffs WHERE id=?1",[*id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?.ok_or(Fault::NotFound)?;Database::report_auth(&tx,&authorization,&report,&["creator"])?;if state=="submitted"{return Err(Fault::Conflict);}
    db.fence("handoff",id)?;tx.execute("UPDATE handoffs SET state='revoked',secret='',code='revoked:'||id,proof='' WHERE id=?1",[*id])?;tx.execute("UPDATE caps SET active=0 WHERE kind='phone' AND report=?1",[&report])?;
    if !keep {db.delete_journal(&report)?;Database::erase_in(&tx,&report)?;}tx.commit()?;Ok(json!({"state":"revoked","draft_kept":keep}))
   })
        }
        ("GET", ["reports", id, "preview"]) => {
            let authorization = secret(headers)?;
            db.with(|c|{let cap=Database::report_auth(c,&authorization,id,&["phone"])?;let(blob,metadata):(Vec<u8>,Vec<u8>)=c.query_row("SELECT blob,metadata FROM reports WHERE id=?1",[*id],|r|Ok((r.get(0)?,r.get(1)?)))?;
    let manifest:Value=serde_json::from_slice(&db.unseal(&format!("{id}/{}/metadata",cap.publisher),&metadata)?).map_err(|_|Fault::Invalid)?;let report=plurx_core::diagnostics::inspect(&db.unseal(&format!("{id}/{}/archive",cap.publisher),&blob)?).map_err(|_|Fault::Invalid)?;
    let description=report.entries.get("description.txt").and_then(|v|std::str::from_utf8(v).ok()).unwrap_or("");Ok(json!({"manifest":manifest,"summary":report.manifest.summary(),"description":description,"privacy":report.manifest.privacy}))
   })
        }
        ("POST", ["reports", id, "redact"]) => {
            let authorization = secret(headers)?;
            let v = decode(body)?;
            let privacy: plurx_core::diagnostics::Privacy =
                serde_json::from_value(v).map_err(|_| Fault::Invalid)?;
            db.with(|c|{let tx=c.transaction()?;let cap=Database::report_auth(&tx,&authorization,id,&["phone"])?;
    let(state,blob):(String,Vec<u8>)=tx.query_row("SELECT state,blob FROM reports WHERE id=?1",[*id],|r|Ok((r.get(0)?,r.get(1)?)))?;if state!="staged"{return Err(Fault::Conflict);}
    let mut inspected=plurx_core::diagnostics::inspect(&db.unseal(&format!("{id}/{}/archive",cap.publisher),&blob)?).map_err(|_|Fault::Invalid)?;
    if (privacy.titles_included && !inspected.manifest.privacy.titles_included)||(privacy.attachments_included && !inspected.manifest.privacy.attachments_included){return Err(Fault::Unauthorized);}
    if !privacy.attachments_included{inspected.entries.remove("diagnostics/platform.json");}
    if !privacy.titles_included{if let Some(bytes)=inspected.entries.get_mut("playback.json"){let mut value:Value=serde_json::from_slice(bytes).map_err(|_|Fault::Invalid)?;if let Some(o)=value.as_object_mut(){o.remove("title");}*bytes=serde_json::to_vec(&value).map_err(|_|Fault::Invalid)?;}}
    inspected.manifest.privacy=privacy.clone();let archive=plurx_core::diagnostics::export(inspected.manifest,inspected.entries).map_err(|_|Fault::Invalid)?;
    let report=plurx_core::diagnostics::inspect(&archive).map_err(|_|Fault::Invalid)?;
    let metadata=db.seal(&format!("{id}/{}/metadata",cap.publisher),&serde_json::to_vec(&report.manifest).map_err(|_|Fault::Invalid)?)?;let sealed=db.seal(&format!("{id}/{}/archive",cap.publisher),&archive)?;
    tx.execute("UPDATE reports SET blob=?2,metadata=?3,sha256=?4,length=?5,privacy=?6,revision=revision+1 WHERE id=?1",params![id,sealed,metadata,hash(&archive),archive.len() as i64,serde_json::to_string(&privacy).map_err(|_|Fault::Invalid)?])?;
    tx.execute("UPDATE handoffs SET revision=revision+1 WHERE report=?1 AND state='approved' AND proof=?2",params![id,cap.principal])?;tx.commit()?;Ok(json!({"state":"staged","privacy":privacy}))
   })
        }
        _ => Err(Fault::NotFound),
    }
}
pub async fn page() -> Response {
    private(Html(include_str!("../web/phone.html")).into_response())
}
pub async fn script() -> Response {
    let mut r = include_str!("../web/phone.js").into_response();
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        "application/javascript".parse().expect("static"),
    );
    private(r)
}
