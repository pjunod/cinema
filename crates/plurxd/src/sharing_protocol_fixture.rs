//! The cross-platform Shared protocol fixture, `tests/sharing/protocol-cases.json`,
//! as the daemon's tests read it. The web suite consumes the same rows; Swift
//! and Kotlin are meant to (their open rows are listed in the parity matrix).
//! A row's `layer` says which side validates it (`b` is this
//! daemon acting as the receiver, `client` a player, `both` both). The parity
//! matrix lives in docs/features/SHARED-LIBRARIES-IMPLEMENTATION.md.
use serde_json::Value;

pub(crate) fn fixture() -> Value {
    serde_json::from_str(include_str!("../../../tests/sharing/protocol-cases.json"))
        .expect("synthetic sharing fixture")
}

/// Whether the receiver validates or emits what the row describes.
pub(crate) fn b_layer(row: &Value) -> bool {
    matches!(row["layer"].as_str(), Some("b" | "both"))
}

pub(crate) fn accepted(row: &Value) -> bool {
    row["expected"] == "accepted"
}

pub(crate) fn rows<'a>(value: &'a Value, what: &str) -> &'a [Value] {
    let rows = value
        .as_array()
        .unwrap_or_else(|| panic!("fixture {what} rows"));
    assert!(!rows.is_empty(), "fixture {what} rows");
    rows
}

/// `base` with one row's mutation applied. `{session}` inside a string value
/// names the session of the side being checked.
pub(crate) fn mutated(base: &Value, row: &Value, session: &str) -> Value {
    let mut value = base.clone();
    let path = rows(&row["path"], "mutation path")
        .iter()
        .map(|segment| segment.as_str().expect("mutation path segment"))
        .collect::<Vec<_>>();
    let (last, parents) = path.split_last().expect("mutation path");
    let mut target = &mut value;
    for segment in parents {
        target = target.get_mut(*segment).expect("mutation parent");
    }
    let object = target.as_object_mut().expect("mutation parent object");
    if row["op"] == "remove" {
        object.remove(*last).expect("removed fixture field");
    } else {
        let replacement = match &row["value"] {
            Value::String(text) => Value::String(text.replace("{session}", session)),
            other => other.clone(),
        };
        object.insert((*last).to_owned(), replacement);
    }
    value
}

// Rows whose B surface is reachable from here. The rest are asserted next to
// the private function they exercise (`sharing_protocol_fixture_*` in
// sharing_playback_wire, sharing_direct_wire, shared_receiver_direct,
// shared_receiver_assets, shared_receiver_orphans and shared_receiver_control
// (B's status envelope, refusal answers, control precheck and `none`
// rebind), plus plurx-core's sharing and sharing_resources).
#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::sharing_playback_wire::SharedControlRefusal;
    use crate::sharing_client::SharedVodStatus;
    use axum::response::IntoResponse;
    use plurx_core::{
        sharing::SourceId,
        sharing_catalogue_details::FileRevision,
        sharing_receiver_sessions::{ReceiverProducerKind, RemoteSourceRecipe},
    };
    use uuid::Uuid;

    fn text<'a>(value: &'a Value, what: &str) -> &'a str {
        value.as_str().unwrap_or_else(|| panic!("fixture {what}"))
    }

    fn status_valid(status: &Value) -> bool {
        serde_json::from_value::<SharedVodStatus>(status.clone())
            .is_ok_and(|status| status.is_valid())
    }

    #[test]
    fn sharing_protocol_fixture_status_grammar() {
        let fixture = fixture();
        let shared = &fixture["shared_status"];
        let envelope = &shared["accepted"];
        let decoded: SharedVodStatus =
            serde_json::from_value(envelope["status"].clone()).expect("fixture status");
        assert!(decoded.is_valid());
        assert_eq!(
            serde_json::to_value(&decoded).expect("status wire"),
            envelope["status"],
            "the fixture names every status field B relays, and only those"
        );
        let words = rows(&shared["word_fields"], "status word field");
        let tokens = rows(&fixture["status_tokens"], "status token");
        for field in words {
            let field = text(field, "word field");
            assert!(envelope["status"].get(field).is_some(), "{field}");
            for row in tokens {
                let mut status = envelope["status"].clone();
                status[field] = row["token"].clone();
                assert_eq!(
                    status_valid(&status),
                    accepted(row),
                    "{} in {field}",
                    row["id"]
                );
            }
        }
        let mut checked = 0;
        for row in rows(&shared["mutations"], "status mutation") {
            if !b_layer(row) {
                continue;
            }
            let status = &mutated(envelope, row, "")["status"];
            assert_eq!(status_valid(status), accepted(row), "{}", row["id"]);
            checked += 1;
        }
        assert!(checked >= 10, "status rows the receiver validates");
    }

    #[test]
    fn sharing_protocol_fixture_source_control_refusals() {
        use crate::playback_control::ControlStateError as E;
        let fixture = fixture();
        let source = rows(&fixture["control_refusals"]["source"], "Source refusal");
        let mut valid = Vec::new();
        for row in source {
            let refusal = serde_json::from_value::<SharedControlRefusal>(row["source"].clone())
                .ok()
                .filter(SharedControlRefusal::is_valid);
            assert_eq!(refusal.is_some(), row["valid"] == true, "{}", row["id"]);
            valid.extend(refusal);
        }
        // Every refusal the Source mints is one of the fixture's valid rows,
        // and every valid row is one the Source mints.
        let minted = [
            E::StaleGeneration,
            E::StaleClient,
            E::StaleSequence,
            E::OwnerChanged,
            E::RateLimited(250),
            E::SessionEnded,
            E::PauseExpired,
            E::OwnerTransition,
            E::OwnerLost,
            E::Unavailable,
        ]
        .map(SharedControlRefusal::from_state);
        for refusal in &minted {
            assert!(valid.contains(refusal), "{refusal:?} has a fixture row");
        }
        for refusal in &valid {
            assert!(minted.contains(refusal), "{refusal:?} is minted");
        }
    }

    #[tokio::test]
    async fn sharing_protocol_fixture_resource_unsupported() {
        let fixture = fixture();
        let wire = &fixture["resource_unsupported"];
        // Parts, not `Response::status()`: the ownership inventory counts
        // `.status(` calls as process-capable launches.
        let (parts, body) = crate::http::shared_receiver_assets::resource_unsupported()
            .await
            .into_response()
            .into_parts();
        assert_eq!(
            u64::from(parts.status.as_u16()),
            wire["status"].as_u64().expect("fixture status")
        );
        let bytes = axum::body::to_bytes(body, 64 * 1024)
            .await
            .expect("bounded typed error");
        let body: Value = serde_json::from_slice(&bytes).expect("typed error JSON");
        assert_eq!(body["code"], wire["code"]);
        let closed = rows(&fixture["file_suffixes"], "file suffix")
            .iter()
            .filter(|row| row["b"] == "closed")
            .map(|row| row["suffix"].clone())
            .collect::<Vec<_>>();
        assert_eq!(
            closed.as_slice(),
            rows(&wire["closed_suffixes"], "closed suffix"),
            "every closed suffix answers the typed 422"
        );
    }

    #[test]
    fn sharing_protocol_fixture_per_session_playback_ids() {
        let fixture = fixture();
        let ids = &fixture["playback_ids"];
        let context = &fixture["context"];
        let request = serde_json::json!({
            "playback_id": ids["viewer_playback_id"],
            "request_id": ids["viewer_request_id"],
            "start_ms": 0,
        });
        let recipe = |source_request_id: Uuid| RemoteSourceRecipe {
            kind: ReceiverProducerKind::RemoteSource,
            version: 1,
            reference: serde_json::from_value(context["reference"].clone())
                .expect("fixture reference"),
            lifecycle_generation: context["lifecycle_generation"]
                .as_i64()
                .expect("fixture lifecycle"),
            file_id: SourceId::parse(text(&context["file_id"], "file")).expect("fixture file"),
            file_revision: FileRevision::parse(text(&context["revision"], "revision"))
                .expect("fixture revision"),
            source_request_id,
            parent_login_hash: "a".repeat(64),
            request_json: request.to_string(),
        };
        let source_request =
            Uuid::parse_str(text(&ids["source_request_id"], "request")).expect("request");
        let first = recipe(source_request);
        assert_eq!(
            crate::sharing::receiver_playback_id(&first),
            text(&ids["receiver_playback_id"], "playback")
        );
        let wrapper: Value = serde_json::from_str(
            &crate::sharing::receiver_source_wrapper(&first).expect("Source wrapper"),
        )
        .expect("Source wrapper JSON");
        let session = &wrapper["session"];
        assert_eq!(session["playback_id"], ids["receiver_playback_id"]);
        assert_eq!(session["request_id"], ids["source_request_id"]);
        assert_eq!(
            session["start_ms"], 0,
            "the rest of the request is retained"
        );
        // A reopen of the same viewer playback is its own playback on both
        // sides, so the predecessor keeps serving until the successor publishes.
        let reopen = recipe(Uuid::new_v4());
        assert_ne!(
            crate::sharing::receiver_playback_id(&reopen),
            crate::sharing::receiver_playback_id(&first)
        );
    }
}
