// Candidate-role binary only. Its output is untrusted function data.
use std::io::{BufRead, BufReader, Write};
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    protocol: u8,
    id: u64,
    method: String,
    args: (String, String, (WireMediaFile, WireDeviceProfile, WireForce, WireRenderCaps)),
    kwargs: std::collections::BTreeMap<String, serde_json::Value>,
}
fn main() {
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let mut last_id = 0_u64;
    for _ in 0..4096 {
        let mut raw = Vec::new();
        std::io::Read::take(&mut reader, 65537).read_until(b'\n', &mut raw).expect("request read");
        if raw.is_empty() { return; }
        assert!(raw.len() <= 65536 && raw.last() == Some(&b'\n'), "request frame limit");
        let request: Request = serde_json::from_slice(&raw).expect("typed request");
        assert!(request.protocol == 2 && request.id == last_id + 1 && request.method == "call", "request identity");
        assert!(request.args.0 == "plurx_core.playback" && request.args.1 == "decide_forced" && request.kwargs.is_empty(), "closed operation");
        last_id = request.id;
        let (file, profile, force, node) = request.args.2;
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let result = plurx_core::playback::decide_forced(&file.into(), &profile.into(), force.into(), &node.into());
            WireDecision::from(&result)
        }));
        let branch = match outcome {
            Ok(value) => serde_json::json!({"value":value}),
            Err(_) => serde_json::json!({"exception":{"type":"RustPanic","message":"candidate decision panicked"}}),
        };
        let mut reply = serde_json::to_vec(&serde_json::json!({"protocol":2,"id":last_id,"result":branch})).expect("reply encode");
        assert!(reply.len() < 65536, "reply frame limit");
        reply.push(b'\n');
        std::io::stdout().write_all(&reply).expect("reply write");
        std::io::stdout().flush().expect("reply flush");
    }
}
