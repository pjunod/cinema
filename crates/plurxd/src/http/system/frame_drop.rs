//! Numeric, bounded evidence only; never playback authority or a free-form log.
use serde::{Deserialize, Serialize};
use serde_json::{Number, Value};

type Row<const N: usize> = [Option<Number>; N];

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ClientFrameDrop {
    version: u8,
    id: String,
    attachment_id: u64,
    clock_origin_ms: f64,
    trigger_ms: f64,
    observed_from_ms: f64,
    drop_from: u64,
    drop_to: u64,
    end_ms: Option<f64>,
    end_reason: String,
    samples: Vec<Row<11>>,
    frames: Vec<Row<7>>,
    post_frames: Vec<Row<7>>,
    events: Vec<Row<3>>,
    resources: Vec<Row<7>>,
    trimmed: [u64; 4],
    long_tasks_supported: bool,
}

impl ClientFrameDrop {
    pub(super) fn value(&self) -> Option<Value> {
        const MAX: f64 = 9_007_199_254_740_991.0;
        let number = |n: f64| n.is_finite() && n.abs() <= MAX;
        fn rows<const N: usize>(rows: &[Row<N>], max: usize) -> bool {
            rows.len() <= max
                && rows.iter().flatten().flatten().all(|n| {
                    n.as_f64()
                        .is_some_and(|v| v.is_finite() && v.abs() <= 9_007_199_254_740_991.0)
                })
                && rows.iter().all(|row| row[0].is_some())
        }
        if self.version != 1
            || !self.id.starts_with("fd-")
            || !(4..=64).contains(&self.id.len())
            || !self
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || self.attachment_id == 0
            || self.attachment_id > 9_007_199_254_740_991
            || !number(self.clock_origin_ms)
            || self.clock_origin_ms < 0.0
            || !number(self.trigger_ms)
            || !number(self.observed_from_ms)
            || self.observed_from_ms < 0.0
            || self.observed_from_ms > self.trigger_ms
            || !self
                .end_ms
                .is_some_and(|n| number(n) && n >= self.trigger_ms)
            || self.drop_to <= self.drop_from
            || self.drop_to > 9_007_199_254_740_991
            || !matches!(
                self.end_reason.as_str(),
                "complete" | "stopped" | "replaced" | "pagehide" | "counter_reset"
            )
            || self.samples.is_empty()
            || !rows(&self.samples, 168)
            || !rows(&self.frames, 512)
            || !rows(&self.post_frames, 128)
            || !rows(&self.events, 128)
            || !rows(&self.resources, 16)
        {
            return None;
        }
        let value = serde_json::to_value(self).ok()?;
        (serde_json::to_vec(&value).ok()?.len() <= 49_152).then_some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn report() -> Value {
        json!({"version":1,"id":"fd-test-1","attachment_id":1,
            "clock_origin_ms":1792609848896.0,"trigger_ms":106983.0,
            "observed_from_ms":105000.0,"drop_from":0,"drop_to":1,
            "end_ms":116983.0,"end_reason":"complete",
            "samples":[[106983,4576968,2074,1,0,4,8,6000,0,2055,null]],
            "frames":[[106900,106900,4576900,2054,106916,106900,null]],
            "post_frames":[],"events":[[106800,15,null]],"resources":[[1,553,106000,106010,106100,10000,10300]],
            "trimmed":[0,0,0,0],"long_tasks_supported":false})
    }

    #[test]
    fn frame_drop_incident_preserves_numeric_history_in_existing_telemetry() {
        let value = report();
        let ev: crate::http::system::ClientLog = serde_json::from_value(json!({
            "event":"frame_drop","file_id":"9007199254740993","session_id":"original-session",
            "frame_drop":value
        }))
        .expect("valid frame-drop test fixture");
        let event = crate::http::system::client_playback_event(&ev, 1);
        assert_eq!(event.file_id, Some(9_007_199_254_740_993));
        let extra: Value = serde_json::from_str(
            event
                .extra
                .as_deref()
                .expect("valid frame-drop test fixture"),
        )
        .expect("valid frame-drop test fixture");
        assert_eq!(extra["frame_drop"], value);
        assert_eq!(
            extra["frame_drop"]["observed_from_ms"].as_f64(),
            Some(105000.0)
        );
        assert_eq!(extra["frame_drop"]["frames"][0][6], Value::Null);
    }

    #[test]
    fn frame_drop_incident_rejects_overflow_unknown_strings_and_invalid_bounds() {
        for (field, replacement) in [
            ("version", json!(2)),
            ("id", json!("https://private/?token=CANARY")),
            ("attachment_id", json!(0)),
            ("observed_from_ms", json!(107000)),
            ("drop_to", json!(0)),
            ("end_ms", json!(100000)),
            ("frames", json!(vec![[Some(1); 7]; 513])),
            ("post_frames", json!(vec![[Some(1); 7]; 129])),
            ("samples", json!(vec![[Some(1); 11]; 169])),
            ("events", json!(vec![[Some(1); 3]; 129])),
            ("resources", json!(vec![[Some(1); 7]; 17])),
        ] {
            let mut value = report();
            value[field] = replacement;
            let incident: ClientFrameDrop =
                serde_json::from_value(value).expect("valid frame-drop test fixture");
            assert!(incident.value().is_none(), "accepted invalid {field}");
        }
        for field in ["url", "title", "token"] {
            let mut value = report();
            value[field] = json!("CANARY");
            assert!(serde_json::from_value::<ClientFrameDrop>(value).is_err());
        }
        let mut value = report();
        value["frames"] = json!([[0, 0, "CANARY", 0, 0, 0, 0]]);
        assert!(serde_json::from_value::<ClientFrameDrop>(value).is_err());
        let mut value = report();
        value["frames"] = json!(vec![[Some(9_007_199_254_740_000_u64); 7]; 512]);
        assert!(serde_json::from_value::<ClientFrameDrop>(value)
            .expect("valid frame-drop test fixture")
            .value()
            .is_none());
    }
}
