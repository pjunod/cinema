//! Named passkey operators exist only on the independent administration listener.
use crate::{
    store::{equal, hash, now, random, Database, Fault, Result},
    transport::{decode, fields, private, response, text, Service},
};
use axum::{
    body::Bytes,
    extract::{Path as RoutePath, Query, State},
    http::{header, HeaderMap},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Router,
};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use webauthn_rs::prelude::*;

enum Ceremony {
    Enroll {
        operator: String,
        state: PasskeyRegistration,
        token: String,
    },
    Login {
        operator: String,
        state: PasskeyAuthentication,
    },
}
struct Pending {
    at: Instant,
    ceremony: Ceremony,
}
#[derive(Clone, Default)]
pub struct Challenges(Arc<Mutex<HashMap<String, Pending>>>);
impl Challenges {
    fn put(&self, ceremony: Ceremony) -> Result<String> {
        let mut c = self.0.lock().map_err(|_| Fault::Unavailable)?;
        c.retain(|_, v| v.at.elapsed() < Duration::from_secs(300));
        if c.len() >= 16 {
            return Err(Fault::Quota);
        }
        let id = random()?;
        c.insert(
            id.clone(),
            Pending {
                at: Instant::now(),
                ceremony,
            },
        );
        Ok(id)
    }
    fn take(&self, id: &str) -> Result<Ceremony> {
        let p = self
            .0
            .lock()
            .map_err(|_| Fault::Unavailable)?
            .remove(id)
            .ok_or(Fault::Unauthorized)?;
        if p.at.elapsed() >= Duration::from_secs(300) {
            return Err(Fault::Expired);
        }
        Ok(p.ceremony)
    }
}
pub fn bootstrap(db: &Database, name: &str, out: &Path) -> Result<()> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
    {
        return Err(Fault::Invalid);
    }
    if out.exists() {
        return Err(Fault::Conflict);
    }
    let token = random()?;
    let id = uuid::Uuid::new_v4().to_string();
    db.with(|c| {
        let count: i64 = c.query_row("SELECT COUNT(*) FROM operators", [], |r| r.get(0))?;
        if count >= 32 {
            return Err(Fault::Quota);
        }
        let tx = c.transaction()?;
        tx.execute(
            "INSERT INTO operators VALUES(?1,?2,1,NULL)",
            params![id, name],
        )?;
        tx.execute(
            "INSERT INTO operator_tokens VALUES(?1,?2,'enroll',?3)",
            params![hash(token.as_bytes()), id, now() + 600],
        )?;
        crate::store::atomic_secret(out, token.as_bytes())?;
        tx.commit()?;
        Ok(())
    })
}

pub fn router() -> Router<Service> {
    Router::new()
        .route("/", get(page))
        .route("/operator.js", get(script))
        .route("/support.css", get(crate::transport::styles))
        .route("/operator/v1/auth/{action}", post(auth))
        .route("/operator/v1/reports", get(list))
        .route("/operator/v1/reports/{id}", get(detail).delete(delete))
        .route("/operator/v1/reports/{id}/download", get(download))
        .route("/operator/v1/reports/{id}/label", post(label))
        .route("/operator/v1/reports/{id}/retention", post(retention))
        .route("/operator/v1/invitations", post(invitation))
}
fn exact_origin(service: &Service, headers: &HeaderMap) -> Result<()> {
    if headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) != Some(&service.operator_origin) {
        Err(Fault::Unauthorized)
    } else {
        Ok(())
    }
}
fn enrollment(db: &Database, token: &str) -> Result<String> {
    db.with(|c| {
        c.query_row(
            "SELECT operator FROM operator_tokens WHERE digest=?1 AND kind='enroll' AND expires>?2",
            params![hash(token.as_bytes()), now()],
            |r| r.get(0),
        )
        .optional()?
        .ok_or(Fault::Unauthorized)
    })
}
async fn auth(
    State(service): State<Service>,
    RoutePath(action): RoutePath<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if exact_origin(&service, &headers).is_err() {
        return response(Err(Fault::Unauthorized));
    }
    let request = match decode(&body) {
        Ok(v) => v,
        Err(e) => return response(Err(e)),
    };
    let result = tokio::task::spawn_blocking(move || authenticate(&service, &action, request))
        .await
        .unwrap_or(Err(Fault::Unavailable));
    match result {
        Ok((value, Some(cookie))) => {
            let mut r = response(Ok(value));
            if let Ok(v) = cookie.parse() {
                r.headers_mut().insert(header::SET_COOKIE, v);
            }
            r
        }
        Ok((value, None)) => response(Ok(value)),
        Err(e) => response(Err(e)),
    }
}
fn authenticate(
    service: &Service,
    action: &str,
    request: Value,
) -> Result<(Value, Option<String>)> {
    match action {
        "enroll-start" => {
            fields(&request, &["token"], &[])?;
            let token = text(&request, "token")?;
            let operator = enrollment(&service.db, token)?;
            let name: String = service.db.with(|c| {
                Ok(c.query_row(
                    "SELECT name FROM operators WHERE id=?1 AND active=1 AND key IS NULL",
                    [&operator],
                    |r| r.get(0),
                )?)
            })?;
            let (challenge, state) = service
                .webauthn
                .start_passkey_registration(
                    uuid::Uuid::parse_str(&operator).map_err(|_| Fault::Invalid)?,
                    &name,
                    &name,
                    None,
                )
                .map_err(|_| Fault::Unauthorized)?;
            let id = service.challenges.put(Ceremony::Enroll {
                operator,
                state,
                token: hash(token.as_bytes()),
            })?;
            Ok((json!({"challenge":challenge,"id":id}), None))
        }
        "enroll-finish" => {
            fields(&request, &["token", "id", "credential"], &[])?;
            let operator = enrollment(&service.db, text(&request, "token")?)?;
            let pending = service.challenges.take(text(&request, "id")?)?;
            let Ceremony::Enroll {
                operator: bound,
                state,
                token,
            } = pending
            else {
                return Err(Fault::Unauthorized);
            };
            if bound != operator || !equal(&token, &hash(text(&request, "token")?.as_bytes())) {
                return Err(Fault::Unauthorized);
            }
            let credential: RegisterPublicKeyCredential =
                serde_json::from_value(request["credential"].clone())
                    .map_err(|_| Fault::Invalid)?;
            let passkey = service
                .webauthn
                .finish_passkey_registration(&credential, &state)
                .map_err(|_| Fault::Unauthorized)?;
            service.db.with(|c| {
                let tx = c.transaction()?;
                let others = tx
                    .prepare("SELECT id,key FROM operators WHERE key IS NOT NULL")?
                    .query_map([], |r| {
                        Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                for (id, blob) in others {
                    let other: Passkey = serde_json::from_slice(
                        &service
                            .db
                            .unseal(&format!("operator/{id}/passkey"), &blob)?,
                    )
                    .map_err(|_| Fault::Invalid)?;
                    if other.cred_id() == passkey.cred_id() {
                        return Err(Fault::Conflict);
                    }
                }
                let sealed = service.db.seal(
                    &format!("operator/{operator}/passkey"),
                    &serde_json::to_vec(&passkey).map_err(|_| Fault::Invalid)?,
                )?;
                if tx.execute(
                    "UPDATE operators SET key=?2 WHERE id=?1 AND active=1 AND key IS NULL",
                    params![operator, sealed],
                )? != 1
                {
                    return Err(Fault::Conflict);
                }
                tx.execute("DELETE FROM operator_tokens WHERE digest=?1", [token])?;
                tx.commit()?;
                Ok(())
            })?;
            Ok((json!({"state":"enrolled","next":"sign_in"}), None))
        }
        "login-start" => {
            fields(&request, &["name"], &[])?;
            let name = text(&request, "name")?;
            let (operator, blob): (String, Vec<u8>) = service.db.with(|c| {
                c.query_row(
                    "SELECT id,key FROM operators WHERE name=?1 AND active=1 AND key IS NOT NULL",
                    [name],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?
                .ok_or(Fault::Unauthorized)
            })?;
            let passkey: Passkey = serde_json::from_slice(
                &service
                    .db
                    .unseal(&format!("operator/{operator}/passkey"), &blob)?,
            )
            .map_err(|_| Fault::Invalid)?;
            let (challenge, state) = service
                .webauthn
                .start_passkey_authentication(&[passkey])
                .map_err(|_| Fault::Unauthorized)?;
            let id = service
                .challenges
                .put(Ceremony::Login { operator, state })?;
            Ok((json!({"challenge":challenge,"id":id}), None))
        }
        "login-finish" => {
            fields(&request, &["id", "credential"], &[])?;
            let Ceremony::Login { operator, state } =
                service.challenges.take(text(&request, "id")?)?
            else {
                return Err(Fault::Unauthorized);
            };
            let credential: PublicKeyCredential =
                serde_json::from_value(request["credential"].clone())
                    .map_err(|_| Fault::Invalid)?;
            let verified = service
                .webauthn
                .finish_passkey_authentication(&credential, &state)
                .map_err(|_| Fault::Unauthorized)?;
            let token = random()?;
            let csrf = random()?;
            service.db.with(|c| {
                let tx = c.transaction()?;
                let blob: Vec<u8> = tx.query_row(
                    "SELECT key FROM operators WHERE id=?1 AND active=1",
                    [&operator],
                    |r| r.get(0),
                )?;
                let mut passkey: Passkey = serde_json::from_slice(
                    &service
                        .db
                        .unseal(&format!("operator/{operator}/passkey"), &blob)?,
                )
                .map_err(|_| Fault::Invalid)?;
                let _ = passkey.update_credential(&verified);
                let sealed = service.db.seal(
                    &format!("operator/{operator}/passkey"),
                    &serde_json::to_vec(&passkey).map_err(|_| Fault::Invalid)?,
                )?;
                tx.execute(
                    "UPDATE operators SET key=?2 WHERE id=?1",
                    params![operator, sealed],
                )?;
                tx.execute(
                    "INSERT INTO operator_tokens VALUES(?1,?2,?3,?4)",
                    params![
                        hash(token.as_bytes()),
                        operator,
                        format!("session-{}", hash(csrf.as_bytes())),
                        now() + 900
                    ],
                )?;
                tx.commit()?;
                Ok(())
            })?;
            Ok((json!({"state":"authenticated","csrf":csrf,"expires_in_ms":900000}),Some(format!("__Host-cinema_support={token}; Secure; HttpOnly; SameSite=Strict; Path=/; Max-Age=900"))))
        }
        _ => Err(Fault::NotFound),
    }
}
fn authorized(service: &Service, headers: &HeaderMap, write: bool) -> Result<String> {
    if write {
        exact_origin(service, headers)?;
    } else if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        if origin != service.operator_origin {
            return Err(Fault::Unauthorized);
        }
    }
    let cookie = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.split(';')
                .find_map(|c| c.trim().strip_prefix("__Host-cinema_support="))
        })
        .ok_or(Fault::Unauthorized)?;
    let csrf = headers
        .get("x-csrf")
        .and_then(|v| v.to_str().ok())
        .ok_or(Fault::Unauthorized)?;
    service.db.with(|c|{let v:Option<(String,String)>=c.query_row("SELECT t.operator,t.kind FROM operator_tokens t JOIN operators o ON o.id=t.operator WHERE t.digest=?1 AND t.expires>?2 AND o.active=1",params![hash(cookie.as_bytes()),now()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
  let (operator,kind)=v.ok_or(Fault::Unauthorized)?;if equal(&kind,&format!("session-{}",hash(csrf.as_bytes()))){Ok(operator)}else{Err(Fault::Unauthorized)}
 })
}
fn audit(c: &rusqlite::Connection, operator: &str, action: &str, id: &str) -> Result<()> {
    c.execute(
        "INSERT INTO audit(at,operator,action,report) VALUES(?1,?2,?3,?4)",
        params![now(), operator, action, id],
    )?;
    Ok(())
}
#[derive(serde::Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct Filter {
    reference: String,
    platform: String,
    build: String,
    category: String,
    label: String,
    since: u64,
    until: u64,
    offset: u32,
}
async fn list(
    State(service): State<Service>,
    Query(filter): Query<Filter>,
    headers: HeaderMap,
) -> Response {
    let result=tokio::task::spawn_blocking(move||{let operator=authorized(&service,&headers,false)?;
  if filter.reference.len()>64 || filter.platform.len()>32 || filter.build.len()>128 || filter.category.len()>32 || filter.label.len()>32 || filter.offset>50000 || filter.since>i64::MAX as u64 || filter.until>i64::MAX as u64{return Err(Fault::Invalid);}
  service.db.with(|c|{
   let until=if filter.until==0{i64::MAX}else{filter.until as i64};
   let rows=c.prepare("SELECT id,publisher,reference,state,label,created,expires,metadata FROM reports WHERE state='submitted' AND expires>?1 AND (?2='' OR reference=?2) AND (?3='' OR label=?3) AND created>=?4 AND created<=?5 ORDER BY created DESC,id DESC LIMIT 501 OFFSET ?6")?.query_map(params![now(),filter.reference,filter.label,filter.since as i64,until,filter.offset],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,i64>(5)?,r.get::<_,i64>(6)?,r.get::<_,Vec<u8>>(7)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
   let available=rows.len();let mut result=Vec::new();let mut scanned=0;
   for(id,publisher,reference,state,label,created,expires,blob) in rows.into_iter().take(500) {scanned+=1;let manifest:Value=serde_json::from_slice(&service.db.unseal(&format!("{id}/{publisher}/metadata"),&blob)?).map_err(|_|Fault::Invalid)?;
    if (!filter.platform.is_empty() && manifest["client"]["platform"].as_str()!=Some(&filter.platform)) || (!filter.build.is_empty() && manifest["client"]["build"].as_str()!=Some(&filter.build)) || (!filter.category.is_empty() && manifest["category"].as_str()!=Some(&filter.category)){continue;}
    result.push(json!({"id":id,"reference":reference,"state":state,"label":label,"created":created,"expires":expires,"platform":manifest["client"]["platform"],"build":manifest["client"]["build"],"category":manifest["category"]}));if result.len()==100 {break;}
   }
   let next=if scanned<available{Some(filter.offset+scanned as u32)}else{None};audit(c,&operator,"list","")?;Ok(json!({"reports":result,"limit":100,"next_offset":next}))
  })
 }).await.unwrap_or(Err(Fault::Unavailable));
    response(result)
}
async fn detail(
    State(service): State<Service>,
    RoutePath(id): RoutePath<String>,
    headers: HeaderMap,
) -> Response {
    let result=tokio::task::spawn_blocking(move||{let operator=authorized(&service,&headers,false)?;service.db.with(|c|{
  let (publisher,blob,description):(String,Vec<u8>,Vec<u8>)=c.query_row("SELECT publisher,blob,description FROM reports WHERE id=?1 AND state='submitted' AND expires>?2",params![id,now()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?.ok_or(Fault::NotFound)?;
  let archive=service.db.unseal(&format!("{id}/{publisher}/archive"),&blob)?;let report=plurx_core::diagnostics::inspect(&archive).map_err(|_|Fault::Invalid)?;
  let description=String::from_utf8(service.db.unseal(&format!("{id}/{publisher}/description"),&description)?).map_err(|_|Fault::Invalid)?;
  let events=report.entries.into_iter().filter(|(n,_)|n.ends_with(".jsonl")).map(|(name,data)|json!({"name":name,"text":String::from_utf8(data).unwrap_or_default()})).collect::<Vec<_>>();audit(c,&operator,"view",&id)?;
  Ok(json!({"manifest":report.manifest,"summary":report.manifest.summary(),"description":description,"timeline":events}))
 })}).await.unwrap_or(Err(Fault::Unavailable));
    response(result)
}
async fn download(
    State(service): State<Service>,
    RoutePath(id): RoutePath<String>,
    headers: HeaderMap,
) -> Response {
    let result=tokio::task::spawn_blocking(move||{let operator=authorized(&service,&headers,false)?;service.db.with(|c|{let (publisher,blob):(String,Vec<u8>)=c.query_row("SELECT publisher,blob FROM reports WHERE id=?1 AND state='submitted' AND expires>?2",params![id,now()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?.ok_or(Fault::NotFound)?;audit(c,&operator,"download",&id)?;service.db.unseal(&format!("{id}/{publisher}/archive"),&blob)})}).await.unwrap_or(Err(Fault::Unavailable));
    match result {
        Ok(bytes) => {
            let mut r = bytes.into_response();
            r.headers_mut().insert(
                header::CONTENT_TYPE,
                "application/zip".parse().expect("static"),
            );
            r.headers_mut().insert(
                header::CONTENT_DISPOSITION,
                "attachment; filename=cinema-report.zip"
                    .parse()
                    .expect("static"),
            );
            private(r)
        }
        Err(e) => response(Err(e)),
    }
}
async fn delete(
    State(service): State<Service>,
    RoutePath(id): RoutePath<String>,
    headers: HeaderMap,
) -> Response {
    let result = tokio::task::spawn_blocking(move || {
        let operator = authorized(&service, &headers, true)?;
        service.db.delete_journal(&id)?;
        service.db.with(|c| {
            let tx = c.transaction()?;
            Database::erase_in(&tx, &id)?;
            audit(&tx, &operator, "delete", &id)?;
            tx.commit()?;
            Ok(json!({"state":"deleted"}))
        })
    })
    .await
    .unwrap_or(Err(Fault::Unavailable));
    response(result)
}
async fn label(
    State(service): State<Service>,
    RoutePath(id): RoutePath<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let result = tokio::task::spawn_blocking(move || {
        let operator = authorized(&service, &headers, true)?;
        let v = decode(&body)?;
        fields(&v, &["label"], &[])?;
        let label = text(&v, "label")?;
        if !["new", "investigating", "resolved"].contains(&label) {
            return Err(Fault::Invalid);
        }
        service.db.with(|c| {
            c.execute(
                "UPDATE reports SET label=?2 WHERE id=?1 AND state='submitted'",
                params![id, label],
            )?;
            audit(c, &operator, "label", &id)?;
            Ok(json!({"label":label}))
        })
    })
    .await
    .unwrap_or(Err(Fault::Unavailable));
    response(result)
}
async fn retention(
    State(service): State<Service>,
    RoutePath(id): RoutePath<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let result = tokio::task::spawn_blocking(move || {
        let operator = authorized(&service, &headers, true)?;
        let v = decode(&body)?;
        fields(&v, &["days"], &[])?;
        let days = v["days"].as_i64().ok_or(Fault::Invalid)?;
        if !(1..=90).contains(&days) {
            return Err(Fault::Invalid);
        }
        service.db.with(|c| {
            c.execute(
                "UPDATE reports SET expires=created+?2 WHERE id=?1 AND state='submitted'",
                params![id, days * 86400],
            )?;
            audit(c, &operator, "retention", &id)?;
            Ok(json!({"retention_days":days}))
        })
    })
    .await
    .unwrap_or(Err(Fault::Unavailable));
    response(result)
}
async fn invitation(State(service): State<Service>, headers: HeaderMap, body: Bytes) -> Response {
    let result = tokio::task::spawn_blocking(move || {
        let operator = authorized(&service, &headers, true)?;
        let v = decode(&body)?;
        fields(&v, &["publisher"], &[])?;
        crate::handoff::invitation(&service.db, &operator, text(&v, "publisher")?)
    })
    .await
    .unwrap_or(Err(Fault::Unavailable));
    response(result)
}
async fn page() -> Response {
    private(Html(include_str!("../web/operator.html")).into_response())
}
async fn script() -> Response {
    let mut r = include_str!("../web/operator.js").into_response();
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        "application/javascript".parse().expect("static"),
    );
    private(r)
}

#[cfg(test)]
#[path = "operator_http_tests.rs"]
mod inbox_lifecycle_tests;
