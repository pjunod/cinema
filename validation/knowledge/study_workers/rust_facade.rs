// This block runs only with CLEAN frozen public helpers in the protected role.
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Mutex, atomic::{AtomicU64, Ordering}};
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static SERIAL: Mutex<()> = Mutex::new(());
let _guard = SERIAL.lock().expect("RPC serialization lock");
let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
assert!(id <= 4096, "RPC call budget exhausted");
let args = (WireMediaFile::from(file), WireDeviceProfile::from(profile), WireForce::from(&force), WireRenderCaps::from(node));
let request = serde_json::json!({"protocol":2,"id":id,"method":"call","args":["plurx_core.playback","decide_forced",args],"kwargs":{}});
let mut raw = serde_json::to_vec(&request).expect("finite typed RPC request");
assert!(raw.len() < 65536, "RPC request frame too large");
raw.push(b'\n');
let mut socket = UnixStream::connect("/work/.runtime/protected-rpc/endpoint.sock").expect("protected RPC endpoint");
socket.set_read_timeout(Some(std::time::Duration::from_secs(30))).expect("RPC timeout");
socket.set_write_timeout(Some(std::time::Duration::from_secs(30))).expect("RPC timeout");
socket.write_all(&raw).expect("RPC request write");
let mut raw = Vec::new();
let mut reader = BufReader::new(socket);
// BufRead::read_until alone can allocate unbounded memory for a hostile frame.
std::io::Read::take(&mut reader, 65537).read_until(b'\n', &mut raw).expect("RPC reply read");
assert!(raw.len() <= 65536 && raw.last() == Some(&b'\n'), "RPC reply frame limit");
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Response { protocol: u8, id: u64, result: ResultBranch }
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum ResultBranch { Value(ValueBranch), Exception(ExceptionBranch) }
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ValueBranch { value: WireDecision }
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ExceptionBranch { exception: RpcException }
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RpcException { r#type: String, message: String }
let response: Response = serde_json::from_slice(&raw).expect("typed RPC reply");
assert!(response.protocol == 2 && response.id == id, "RPC identity mismatch");
match response.result {
    ResultBranch::Value(branch) => branch.value.into(),
    ResultBranch::Exception(branch) => {
        assert!(matches!(branch.exception.r#type.as_str(), "RustPanic" | "RustRpcError"), "undeclared RPC exception");
        assert!(branch.exception.message.len() <= 4096, "RPC exception limit");
        panic!("candidate {}: {}", branch.exception.r#type, branch.exception.message)
    }
}
