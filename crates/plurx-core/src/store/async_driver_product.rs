//! Exact selected two-worker product producer. The independent checker/TLA
//! validate its certificate; this producer's adjacency is never an oracle.
//! Schema 4 losslessly shares actual lifecycle payloads, raw retry identity and separate
//! wall/monotonic samples. Qualification remains pending reviewed execution.
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use super::background_jobs::JOB_LEASE_MS;
use plurx_worker_control::library_driver::{Driver as RealDriver, Reply as RealReply, WorkOutcome};
use plurx_worker_control::library_lifecycle::Settlement;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::async_driver_refinement::{
    self as sql, Action, Oracle, OracleEdge, StoreHandle, TriggerMutation,
};

const BUNDLE_BYTES: usize = 256 * 1024 * 1024;
const REPLIES: [&str; 13] = [
    "DomainRefused",
    "DomainAcquired",
    "BindRefused",
    "BindAccepted",
    "QueueLoss",
    "DomainLoss",
    "Pressure",
    "WorkFailed",
    "WorkSucceeded",
    "JoinedFailed",
    "JoinedSucceeded",
    "ReleaseReturned",
    "Done",
];
const FAIR: [&str; 23] = [
    "AdapterSample",
    "OrdinaryLock",
    "OrdinaryLocal",
    "OrdinaryCallReturn",
    "OrdinaryObserve",
    "ClaimRecover",
    "ClaimSend",
    "ClaimApply",
    "ClaimReply",
    "ClaimReturn",
    "Dispatch",
    "StoreApply",
    "SimpleApply",
    "Reply",
    "DriverReturn",
    "RetireLocal",
    "RetireCallReturn",
    "ExternalReturn",
    "JoinReturn",
    "RetireDrain",
    "MakeAvailable",
    "Admission",
    "ClaimWin",
];

fn require(ok: bool, message: &str) -> Result<(), String> {
    if ok {
        Ok(())
    } else {
        Err(message.into())
    }
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn encoded<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("typed JSON")
}
fn canonical<T: Serialize>(value: &T) -> String {
    fn sort(value: &mut Value) {
        match value {
            Value::Object(map) => {
                for value in map.values_mut() {
                    sort(value);
                }
                map.sort_keys();
            }
            Value::Array(array) => {
                for value in array {
                    sort(value);
                }
            }
            _ => {}
        }
    }
    let mut value = serde_json::to_value(value).expect("canonical product JSON");
    sort(&mut value);
    encoded(&value)
}

struct Driver {
    initial: usize,
    states: Vec<Value>,
    replies: HashMap<(usize, String), usize>,
}
impl Driver {
    fn parse(bytes: &[u8]) -> Result<Self, String> {
        let value: Value =
            serde_json::from_slice(bytes).map_err(|e| format!("DriverInventory: {e}"))?;
        require(value["schema_version"] == 1, "DriverInventory: schema")?;
        fn fields(value: &Value, names: &[&str]) -> bool {
            value.as_object().is_some_and(|object| {
                object.len() == names.len() && names.iter().all(|name| object.contains_key(*name))
            })
        }
        require(
            fields(
                &value,
                &[
                    "schema_version",
                    "scope",
                    "initial",
                    "domain",
                    "states",
                    "edges",
                    "rejected",
                ],
            ),
            "DriverInventory: unknown root fields",
        )?;
        require(
            fields(
                &value["domain"],
                &[
                    "reply_classes",
                    "clock_representative",
                    "numeric_payload",
                    "closure",
                ],
            ),
            "DriverInventory: unknown domain fields",
        )?;
        let states = value["states"]
            .as_array()
            .ok_or("DriverInventory: states")?
            .clone();
        let initial = value["initial"]
            .as_u64()
            .ok_or("DriverInventory: initial")? as usize;
        require(initial < states.len(), "DriverInventory: initial range")?;
        let mut replies = HashMap::new();
        let mut rejected = HashSet::new();
        let expected_work = [
            "Idle",
            "Running",
            "Completed { ok: false }",
            "Completed { ok: true }",
            "Joined { ok: false }",
            "Joined { ok: true }",
        ];
        let requests = [
            "AcquireDomain",
            "Bind",
            "PollWork",
            "RevokeDomain",
            "CancelWork",
            "JoinWork",
            "DropPublisher",
            "ReleaseRefused",
            "ReleaseRetry",
            "ReleaseNone",
            "ReleaseInterrupted",
            "SettleYield",
            "SettleRetry",
            "RetireQueue",
            "DropAdmission",
        ];
        for (id, state) in states.iter().enumerate() {
            require(
                fields(
                    state,
                    &[
                        "id",
                        "phase",
                        "held",
                        "work",
                        "revoked",
                        "cancelled",
                        "pending",
                        "remaining",
                        "terminal",
                    ],
                ),
                "DriverInventory: unknown state fields",
            )?;
            require(
                fields(
                    &state["held"],
                    &["admission", "queue", "domain", "publisher"],
                ),
                "DriverInventory: held fields",
            )?;
            require(
                state["held"]
                    .as_object()
                    .expect("held")
                    .values()
                    .all(Value::is_boolean)
                    && state["revoked"].is_boolean()
                    && state["cancelled"].is_boolean(),
                "DriverInventory: held/control flags",
            )?;
            require(
                state["phase"]
                    .as_str()
                    .is_some_and(|phase| !phase.is_empty()),
                "DriverInventory: phase",
            )?;
            require(
                state["remaining"].as_array().is_some_and(|remaining| {
                    remaining
                        .iter()
                        .all(|request| request.as_str().is_some_and(|tag| requests.contains(&tag)))
                }),
                "DriverInventory: remaining requests",
            )?;
            require(state["id"] == id, "DriverInventory: contiguous state IDs")?;
            require(
                expected_work.contains(&state["work"].as_str().unwrap_or("")),
                "DriverInventory: unknown work",
            )?;
            require(state["terminal"].is_boolean(), "DriverInventory: terminal")?;
            require(
                state["pending"].is_null()
                    || requests.contains(&state["pending"].as_str().unwrap_or("")),
                "DriverInventory: pending",
            )?;
            require(
                state["terminal"] == state["pending"].is_null(),
                "DriverInventory: terminal request",
            )?;
        }
        for edge in value["edges"].as_array().ok_or("DriverInventory: edges")? {
            require(
                fields(edge, &["from", "reply", "to", "request"]),
                "DriverInventory: edge fields",
            )?;
            let from = edge["from"].as_u64().ok_or("DriverInventory: from")? as usize;
            let to = edge["to"].as_u64().ok_or("DriverInventory: to")? as usize;
            let reply = edge["reply"].as_str().ok_or("DriverInventory: reply")?;
            require(
                from < states.len() && to < states.len() && REPLIES.contains(&reply),
                "DriverInventory: edge range/tag",
            )?;
            require(
                edge["request"] == states[to]["pending"],
                "DriverInventory: returned pending",
            )?;
            require(
                replies.insert((from, reply.into()), to).is_none(),
                "DriverInventory: duplicate reply",
            )?;
        }
        for edge in value["rejected"]
            .as_array()
            .ok_or("DriverInventory: rejected")?
        {
            require(
                fields(edge, &["state", "reply", "unchanged"]),
                "DriverInventory: rejected fields",
            )?;
            let from = edge["state"]
                .as_u64()
                .ok_or("DriverInventory: rejected state")? as usize;
            let reply = edge["reply"]
                .as_str()
                .ok_or("DriverInventory: rejected reply")?;
            require(
                from < states.len() && REPLIES.contains(&reply) && edge["unchanged"] == true,
                "DriverInventory: rejection",
            )?;
            require(
                rejected.insert((from, reply.to_owned())),
                "DriverInventory: duplicate rejection",
            )?;
        }
        let declared: Vec<&str> = value["domain"]["reply_classes"]
            .as_array()
            .ok_or("DriverInventory: reply domain")?
            .iter()
            .filter_map(Value::as_str)
            .collect();
        require(
            declared == REPLIES,
            "DriverInventory: exhaustive reply vocabulary",
        )?;
        for id in 0..states.len() {
            for reply in REPLIES {
                let key = (id, reply.to_owned());
                require(
                    replies.contains_key(&key) != rejected.contains(&key),
                    "DriverInventory: accepted/rejected partition",
                )?;
            }
        }
        fn rank(
            id: usize,
            replies: &HashMap<(usize, String), usize>,
            visiting: &mut HashSet<usize>,
            memo: &mut HashMap<usize, usize>,
        ) -> Result<usize, String> {
            if let Some(rank) = memo.get(&id) {
                return Ok(*rank);
            }
            require(visiting.insert(id), "DriverInventory: cyclic control")?;
            let mut maximum = 0;
            for ((from, _), to) in replies {
                if *from == id {
                    maximum = maximum.max(rank(*to, replies, visiting, memo)? + 1);
                }
            }
            visiting.remove(&id);
            memo.insert(id, maximum);
            Ok(maximum)
        }
        rank(initial, &replies, &mut HashSet::new(), &mut HashMap::new())?;
        let mut reached = HashSet::from([initial]);
        let mut queue = VecDeque::from([initial]);
        while let Some(id) = queue.pop_front() {
            for ((from, _), to) in &replies {
                if *from == id && reached.insert(*to) {
                    queue.push_back(*to);
                }
            }
        }
        require(
            reached.len() == states.len(),
            "DriverInventory: unreachable control",
        )?;
        Ok(Self {
            initial,
            states,
            replies,
        })
    }
    fn pending(&self, worker: &Worker) -> &str {
        self.states[worker.ctrl as usize]["pending"]
            .as_str()
            .unwrap_or("")
    }
    fn accepts(&self, worker: &Worker, reply: &str) -> bool {
        self.replies
            .contains_key(&(worker.ctrl as usize, reply.into()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedSettlement {
    kind: String,
    not_before_ms: i64,
}
impl From<Settlement> for SavedSettlement {
    fn from(value: Settlement) -> Self {
        match value {
            Settlement::Yield { not_before_ms } => Self {
                kind: "yield".into(),
                not_before_ms,
            },
            Settlement::Retry { not_before_ms } => Self {
                kind: "retry".into(),
                not_before_ms,
            },
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OrdinaryPayload {
    kind: String,
    not_before_ms: i64,
    error_code: Option<String>,
    checkpoint: Option<Value>,
}
impl From<SavedSettlement> for OrdinaryPayload {
    fn from(value: SavedSettlement) -> Self {
        Self {
            error_code: (value.kind == "retry").then(|| "library_execution_failed".into()),
            kind: value.kind,
            not_before_ms: value.not_before_ms,
            checkpoint: None,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AdapterReply {
    tag: String,
    now_ms: Option<i64>,
    retry_delay_ms: Option<i64>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ordinary {
    phase: String,
    payload: Option<OrdinaryPayload>,
    captured_token: Option<usize>,
    lost_seen: Option<bool>,
    deadline_seen: Option<i64>,
    guard_mono_ms: Option<i64>,
    request_now_ms: Option<i64>,
    api_outcome: Option<String>,
    observed_outcome: Option<String>,
}
impl Ordinary {
    fn off() -> Self {
        Self {
            phase: "off".into(),
            payload: None,
            captured_token: None,
            lost_seen: None,
            deadline_seen: None,
            guard_mono_ms: None,
            request_now_ms: None,
            api_outcome: None,
            observed_outcome: None,
        }
    }
    fn locked(&self) -> bool {
        matches!(
            self.phase.as_str(),
            "check_lost"
                | "check_deadline"
                | "take_token"
                | "sample"
                | "sent"
                | "applied"
                | "finish"
        )
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaimRecovery {
    phase: String,
    wake_mono_ms: Option<i64>,
}
impl ClaimRecovery {
    fn off() -> Self {
        Self {
            phase: "off".into(),
            wake_mono_ms: None,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct ClaimLossKey {
    job: usize,
    node: usize,
    claim_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Worker {
    stage: String,
    job: i64,
    attempt: String,
    token: i64,
    lease: i64,
    ctrl: i64,
    call: String,
    reply: String,
    accepted: bool,
    local_token: bool,
    retire: String,
    inner: String,
    original: i64,
    resolved: i64,
    outcome: String,
    sample: i64,
    first_sample: i64,
    resolve_sample: i64,
    after_sample: i64,
    started: bool,
    dropped: bool,
    joined: bool,
    work_started: bool,
    fault_used: bool,
    admission: bool,
    read_lock: bool,
    #[serde(rename = "claim_snapshot")]
    claim_snapshot: Option<sql::ClaimSnapshot>,
    #[serde(rename = "claim_request")]
    claim_request: Option<Value>,
    #[serde(rename = "claim_recovery")]
    claim_recovery: ClaimRecovery,
    #[serde(rename = "claim_issued_mono_ms")]
    claim_issued_mono_ms: Option<i64>,
    #[serde(rename = "claim_request_now_ms")]
    claim_request_now_ms: Option<i64>,
    #[serde(rename = "local_deadline_mono_ms")]
    local_deadline_mono_ms: Option<i64>,
    #[serde(rename = "queue_lost")]
    queue_lost: bool,
    #[serde(rename = "driver_settlement")]
    driver_settlement: Option<SavedSettlement>,
    #[serde(rename = "adapter_reply")]
    adapter_reply: Option<AdapterReply>,
    ordinary: Ordinary,
    cancelling: bool,
}
impl Worker {
    fn idle() -> Self {
        Self {
            stage: "idle".into(),
            job: -1,
            attempt: "".into(),
            token: -1,
            lease: -1,
            ctrl: -1,
            call: "ready".into(),
            reply: "".into(),
            accepted: false,
            local_token: false,
            retire: "off".into(),
            inner: "ready".into(),
            original: -1,
            resolved: -1,
            outcome: "".into(),
            sample: -1,
            first_sample: -1,
            resolve_sample: -1,
            after_sample: -1,
            started: false,
            dropped: false,
            joined: false,
            work_started: false,
            fault_used: false,
            admission: false,
            read_lock: false,
            claim_snapshot: None,
            claim_request: None,
            claim_recovery: ClaimRecovery::off(),
            claim_issued_mono_ms: None,
            claim_request_now_ms: None,
            local_deadline_mono_ms: None,
            queue_lost: false,
            driver_settlement: None,
            adapter_reply: None,
            ordinary: Ordinary::off(),
            cancelling: false,
        }
    }
}
#[derive(Clone, Debug, Serialize)]
struct Valuation {
    sql: StoreHandle,
    w: [Worker; 2],
    available: [bool; 2],
    completed: Vec<usize>,
    mono_now_ms: i64,
    wall_anchors: Vec<i64>,
    mono_anchors: Vec<i64>,
    claim_loss_keys: Vec<ClaimLossKey>,
    #[serde(skip)]
    real_drivers: [Option<RealDriver>; 2],
}
impl PartialEq for Valuation {
    fn eq(&self, other: &Self) -> bool {
        canonical(self) == canonical(other)
    }
}
impl Eq for Valuation {}
#[derive(Clone)]
struct Context {
    id: &'static str,
    stable: bool,
    mutation: &'static str,
}
fn contexts() -> Vec<Context> {
    [
        ("base", false, "none"),
        ("stable", true, "none"),
        ("weak_admission", true, "weak-admission"),
        ("no_claim_win", true, "no-claim-win"),
        ("withheld_join", false, "withheld-join"),
        ("old_poll_policy", false, "old-poll-policy"),
        ("drop_before_join", false, "drop-before-join"),
    ]
    .into_iter()
    .map(|(id, stable, mutation)| Context {
        id,
        stable,
        mutation,
    })
    .collect()
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct Branch {
    family: String,
    worker: Option<usize>,
    choice: Value,
}
fn branch(family: &str, worker: Option<usize>, choice: Value) -> Branch {
    Branch {
        family: family.into(),
        worker,
        choice,
    }
}
struct DemandSpec {
    oracle_from: StoreHandle,
    family: &'static str,
    worker: Option<usize>,
    role: &'static str,
    input: Option<Action>,
    capture: Option<&'static str>,
    sample: Option<i64>,
}
struct Demand {
    spec: DemandSpec,
    edge: Option<usize>,
}
struct Step {
    branch: Branch,
    value: Valuation,
    edge: Option<usize>,
    resolution_edge: Option<usize>,
    read_edge: Option<usize>,
    cleanup_edge: Option<usize>,
}

fn done(oracle: &Oracle, handle: StoreHandle) -> Vec<usize> {
    (0..2)
        .filter(|j| oracle.states[handle].view["jobs"][j]["request_done"] == true)
        .collect()
}
fn move_sql(value: &mut Valuation, edge: &OracleEdge, oracle: &Oracle) {
    value.sql = edge.to;
    value.completed.extend(done(oracle, edge.to));
    value.completed.sort();
    value.completed.dedup();
}
fn slot(
    value: i64,
    len: usize,
    reason: &'static str,
) -> Result<(Option<usize>, Option<&'static str>), String> {
    if value == -1 {
        return Ok((None, Some(reason)));
    }
    require(
        value >= 0 && (value as usize) < len,
        "MalformedProductCapture: positive/out-of-range slot",
    )?;
    Ok((Some(value as usize), None))
}
type CandidateInput = (Option<Action>, Option<&'static str>, Option<i64>);
fn next_neighbour(anchors: &[i64], now: i64) -> Option<i64> {
    anchors
        .iter()
        .flat_map(|a| [a.checked_sub(1), Some(*a), a.checked_add(1)])
        .flatten()
        .filter(|t| *t >= 0 && *t > now)
        .min()
}
fn deadline(issued: i64, expiry: i64, request: i64) -> i64 {
    issued
        .checked_add(expiry.saturating_sub(request).clamp(0, JOB_LEASE_MS))
        .expect("finite selected monotonic deadline")
}
fn retain_anchors(value: &mut Valuation, oracle: &Oracle) {
    let state = &oracle.states[value.sql];
    for token in state.episode.tokens.iter().flatten() {
        value.wall_anchors.push(token.lease_expires_ms);
    }
    for lease in &state.episode.leases {
        value.wall_anchors.push(lease.expires_at_unix_ms);
    }
    if let Some(expiry) = state.view["domain"]["expiry"].as_i64() {
        value.wall_anchors.push(expiry);
    }
    for job in state.view["jobs"].as_array().expect("jobs") {
        if let Some(expiry) = job["expiry"].as_i64() {
            value.wall_anchors.push(expiry);
        }
        if job["state"] == "queued" {
            value
                .wall_anchors
                .push(job["not_before_ms"].as_i64().expect("queued eligibility"));
        }
    }
    for w in &value.w {
        if let Some(payload) = &w.driver_settlement {
            value.wall_anchors.push(payload.not_before_ms);
        }
        if let Some(payload) = &w.ordinary.payload {
            value.wall_anchors.push(payload.not_before_ms);
        }
        if let Some(issued) = w
            .claim_issued_mono_ms
            .filter(|_| w.claim_snapshot.is_some() || w.claim_recovery.phase != "off")
        {
            value.mono_anchors.push(
                issued
                    .checked_add(JOB_LEASE_MS)
                    .expect("finite initial deadline"),
            );
        }
        if let Some(wake) = w.claim_recovery.wake_mono_ms {
            value.mono_anchors.push(wake);
        }
        if let Some(local) = w.local_deadline_mono_ms {
            value.mono_anchors.push(local);
        }
    }
    value.wall_anchors.sort_unstable();
    value.wall_anchors.dedup();
    value.mono_anchors.sort_unstable();
    value.mono_anchors.dedup();
}
fn stable_wall(value: &Valuation, t: i64, driver: &Driver, oracle: &Oracle) -> bool {
    let episode = &oracle.states[value.sql].episode;
    value.w.iter().all(|w| {
        let live = w.local_token || (w.stage == "claim" && w.reply == "Claimed");
        let queue = !live
            || (w.token >= 0
                && t < episode.tokens[w.job as usize][w.token as usize].lease_expires_ms);
        let held_domain = w.stage == "driver"
            && (driver.states[w.ctrl as usize]["held"]["domain"] == true
                || (driver.pending(w) == "AcquireDomain" && w.lease >= 0))
            && !(driver.pending(w).starts_with("Release")
                && matches!(w.call.as_str(), "applied" | "replied"));
        let lease = !held_domain
            || (w.lease >= 0 && t < episode.leases[w.lease as usize].expires_at_unix_ms);
        let payload = !matches!(
            w.ordinary.phase.as_str(),
            "waiting_lock" | "check_lost" | "check_deadline" | "take_token" | "sample"
        ) || w
            .ordinary
            .payload
            .as_ref()
            .is_none_or(|p| t <= p.not_before_ms);
        queue && lease && payload
    })
}
fn stable_mono(value: &Valuation, t: i64) -> bool {
    value.w.iter().all(|w| {
        let local = !(w.started && !w.dropped) || w.local_deadline_mono_ms.is_some_and(|d| t < d);
        let claim = w.stage != "claim"
            || w.reply != "Claimed"
            || (w.claim_issued_mono_ms.is_some_and(|i| t < i + JOB_LEASE_MS)
                && w.local_deadline_mono_ms.is_some_and(|d| t < d));
        local && claim
    })
}
fn concrete_reply(reply: &AdapterReply) -> Result<RealReply, String> {
    Ok(match reply.tag.as_str() {
        "DomainAcquired" | "DomainRefused" => RealReply::Domain {
            acquired: reply.tag == "DomainAcquired",
            now_ms: reply.now_ms.ok_or("MissingAdapterWallSample")?,
        },
        "BindAccepted" | "BindRefused" => RealReply::Bound {
            accepted: reply.tag == "BindAccepted",
        },
        "QueueLoss" => RealReply::Work(WorkOutcome::QueueLoss),
        "DomainLoss" => RealReply::Work(WorkOutcome::DomainLoss),
        "Pressure" => RealReply::Work(WorkOutcome::Pressure),
        "WorkFailed" | "WorkSucceeded" => RealReply::Work(WorkOutcome::Completed {
            ok: reply.tag == "WorkSucceeded",
        }),
        "JoinedFailed" | "JoinedSucceeded" => RealReply::Joined {
            ok: reply.tag == "JoinedSucceeded",
        },
        "ReleaseReturned" => RealReply::Released {
            now_ms: reply.now_ms.ok_or("MissingReleaseWallSample")?,
            retry_delay_ms: reply.retry_delay_ms.ok_or("MissingRetryDelay")?,
        },
        "Done" => RealReply::Done,
        _ => return Err("UnknownConcreteAdapterReply".into()),
    })
}
fn verify_control(real: RealDriver, expected: &Value) -> Result<(), String> {
    let c = real.control();
    require(
        expected["phase"] == format!("{:?}", c.controller)
            && expected["work"] == format!("{:?}", c.work)
            && expected["revoked"] == c.revoked
            && expected["cancelled"] == c.cancelled
            && expected["terminal"] == c.terminal(),
        "ActualDriverControlAgreement",
    )?;
    require(
        expected["held"]
            == json!({"admission":c.held.admission,"queue":c.held.queue,"domain":c.held.domain,"publisher":c.held.publisher}),
        "ActualDriverHeldAgreement",
    )?;
    let pending = real.request().map(|r| format!("{:?}", r.kind()));
    require(
        expected["pending"] == json!(pending)
            && expected["remaining"]
                == json!(c
                    .remaining
                    .iter()
                    .flatten()
                    .map(|r| format!("{r:?}"))
                    .collect::<Vec<_>>()),
        "ActualDriverRequestAgreement",
    )?;
    Ok(())
}
fn verify_real(value: &Valuation, driver: &Driver) -> Result<(), String> {
    for n in 0..2 {
        let w = &value.w[n];
        if w.stage != "driver" {
            require(value.real_drivers[n].is_none(), "UnexpectedDriverLifetime")?;
            continue;
        }
        let real = value.real_drivers[n].ok_or("MissingActualDriver")?;
        verify_control(real, &driver.states[w.ctrl as usize])?;
        require(
            w.driver_settlement == real.settlement().map(Into::into),
            "ActualDriverSavedPayloadAgreement",
        )?;
        if matches!(
            w.ordinary.phase.as_str(),
            "waiting_lock"
                | "check_lost"
                | "check_deadline"
                | "take_token"
                | "sample"
                | "sent"
                | "applied"
                | "finish"
        ) {
            require(
                w.ordinary.payload == w.driver_settlement.clone().map(Into::into),
                "ActualAdapterPayloadAgreement",
            )?;
        }
    }
    Ok(())
}
fn ordinary_input(value: &Valuation, n: usize) -> Result<Action, String> {
    let w = &value.w[n];
    let o = &w.ordinary;
    let p = o.payload.as_ref().ok_or("MissingOrdinaryPayload")?;
    Ok(Action::OrdinarySettle {
        job: w.job as usize,
        token: o.captured_token.ok_or("MissingOrdinaryToken")?,
        kind: p.kind.clone(),
        not_before_ms: p.not_before_ms,
        request_now_ms: o.request_now_ms.ok_or("MissingOrdinaryRequestSample")?,
        error_code: p.error_code.clone(),
        checkpoint: p.checkpoint.clone(),
    })
}
fn input_for(
    value: &Valuation,
    n: usize,
    pending: &str,
    oracle: &Oracle,
) -> Result<CandidateInput, String> {
    let w = &value.w[n];
    let episode = &oracle.states[value.sql].episode;
    if pending == "AcquireDomain" {
        return Ok((Some(Action::AcquireDomain { node: n }), None, None));
    }
    if pending.starts_with("Release") {
        let (lease, missing) = slot(w.lease, episode.leases.len(), "missing_lease")?;
        return Ok((
            lease.map(|lease| Action::ReleaseDomain { lease }),
            missing,
            None,
        ));
    }
    require(
        (0..2).contains(&w.job),
        "MalformedProductCapture: selected job",
    )?;
    let job = w.job as usize;
    let retirement = pending == "RetireQueue";
    let token = if retirement {
        if w.retire == "second" {
            w.resolved
        } else {
            w.original
        }
    } else {
        w.token
    };
    let (token, missing) = slot(token, episode.tokens[job].len(), "missing_token")?;
    if let Some(missing) = missing {
        return Ok((
            None,
            Some(missing),
            if retirement { Some(w.sample) } else { None },
        ));
    }
    let token = token.expect("checked token");
    let sample = if retirement { Some(w.sample) } else { None };
    let action = match pending {
        "ClaimApply" => Action::Claim { job, node: n },
        "RetireQueue" if w.retire == "resolve" => Action::Resolve { job, token },
        "RetireQueue" => Action::Settle {
            job,
            token,
            disposition: if w.cancelling
                || (w.retire == "second" && w.outcome == "cancel_requested")
            {
                2
            } else {
                3
            },
        },
        "Bind" | "PollWork" => {
            let (lease, missing) = slot(w.lease, episode.leases.len(), "missing_lease")?;
            if let Some(missing) = missing {
                return Ok((None, Some(missing), sample));
            }
            if pending == "Bind" {
                Action::Bind {
                    job,
                    token,
                    lease: lease.expect("lease"),
                }
            } else {
                Action::Complete {
                    job,
                    token,
                    lease: lease.expect("lease"),
                }
            }
        }
        _ => return Err("UnknownProductRequest".into()),
    };
    Ok((Some(action), None, sample))
}
fn same_claim(w: &Worker, job: usize, token: usize, oracle: &Oracle, handle: StoreHandle) -> bool {
    matches!(w.stage.as_str(), "claim" | "driver")
        && w.job == job as i64
        && w.token >= 0
        && oracle.states[handle].episode.tokens[job][w.token as usize].claim_id
            == oracle.states[handle].episode.tokens[job][token].claim_id
}
fn environment_allowed(action: &Action, value: &Valuation, oracle: &Oracle) -> bool {
    match action {
        Action::RemoveOwner { .. } => true,
        Action::ClockAdvance { .. } => false,
        Action::AcquireDomain { node } => *node == 2,
        Action::ReleaseDomain { lease } => {
            oracle.states[value.sql].episode.leases[*lease].owner_node_id == "foreign-holder"
        }
        Action::Renew { job, token } => value.w.iter().all(|w| {
            !same_claim(w, *job, *token, oracle, value.sql)
                || (w.token == *token as i64
                    && w.local_token
                    && !w.queue_lost
                    && !w.ordinary.locked()
                    && (w.call == "ready"
                        || w.retire == "drain"
                        || matches!(w.ordinary.phase.as_str(), "waiting_lock" | "done")))
        }),
        _ => false,
    }
}
fn claim_limit(w: &Worker) -> Result<i64, String> {
    w.claim_issued_mono_ms
        .and_then(|issued| issued.checked_add(JOB_LEASE_MS))
        .ok_or_else(|| "MissingInitialClaimDeadline".into())
}
fn recovery_ready(value: &Valuation, w: &Worker) -> bool {
    w.claim_recovery.phase == "ready"
        || (w.claim_recovery.phase == "waiting"
            && w.claim_recovery
                .wake_mono_ms
                .is_some_and(|wake| value.mono_now_ms >= wake))
}
fn initial_resolution(value: &Valuation, n: usize, oracle: &Oracle) -> Result<Action, String> {
    let w = &value.w[n];
    let request: super::background_jobs::ClaimJob =
        serde_json::from_value(w.claim_request.clone().ok_or("MissingActualClaimRequest")?)
            .map_err(|e| format!("MalformedActualClaimRequest: {e}"))?;
    Ok(Action::ResolveInitialClaim {
        job: w.job as usize,
        request: sql::InitialClaimResolution {
            job_id: request.job_id,
            node_id: request.node_id,
            boot_id: request.boot_id,
            claim_id: request.claim_id,
            fence: None,
            dispatched_at_ms: request.dispatched_at_ms,
            now_ms: oracle.states[value.sql].episode.now,
        },
    })
}
fn claim_loss_key(w: &Worker, n: usize) -> Result<ClaimLossKey, String> {
    let request = w.claim_request.as_ref().ok_or("MissingClaimLossRequest")?;
    Ok(ClaimLossKey {
        job: w.job as usize,
        node: n,
        claim_id: request["claim_id"]
            .as_str()
            .ok_or("MissingClaimLossIdentity")?
            .into(),
    })
}
fn enumerate_candidates(
    context: &Context,
    value: &Valuation,
    driver: &Driver,
    oracle: &Oracle,
) -> Result<Vec<DemandSpec>, String> {
    verify_real(value, driver)?;
    let mut result = vec![];
    for n in 0..2 {
        let w = &value.w[n];
        if w.stage == "claim" && recovery_ready(value, w) && value.mono_now_ms < claim_limit(w)? {
            result.push(DemandSpec {
                oracle_from: value.sql,
                family: "ClaimRecover",
                worker: Some(n),
                role: "apply",
                input: Some(initial_resolution(value, n, oracle)?),
                capture: None,
                sample: None,
            });
        }
        if w.stage == "claim" && w.call == "sent" {
            require((0..2).contains(&w.job), "MalformedProductClaim")?;
            result.push(DemandSpec {
                oracle_from: value.sql,
                family: "ClaimApply",
                worker: Some(n),
                role: "apply",
                input: Some(Action::Claim {
                    job: w.job as usize,
                    node: n,
                }),
                capture: None,
                sample: None,
            });
        }
        if w.stage != "driver" || (w.call != "sent" && !matches!(w.ordinary.phase.as_str(), "sent"))
        {
            continue;
        }
        let pending = driver.pending(w);
        if matches!(pending, "SettleYield" | "SettleRetry") {
            if w.ordinary.phase == "sent" {
                for (family, role) in [("OrdinaryCallReturn", "apply"), ("Capacity", "capacity")] {
                    result.push(DemandSpec {
                        oracle_from: value.sql,
                        family,
                        worker: Some(n),
                        role,
                        input: Some(ordinary_input(value, n)?),
                        capture: None,
                        sample: None,
                    });
                }
            }
            continue;
        }
        let ordinary = matches!(
            pending,
            "AcquireDomain"
                | "Bind"
                | "ReleaseRefused"
                | "ReleaseRetry"
                | "ReleaseNone"
                | "ReleaseInterrupted"
        );
        if ordinary
            || pending == "PollWork"
            || (pending == "RetireQueue"
                && matches!(w.retire.as_str(), "first" | "resolve" | "second")
                && w.inner == "sent")
        {
            let (input, capture, sample) = input_for(value, n, pending, oracle)?;
            result.push(DemandSpec {
                oracle_from: value.sql,
                family: if ordinary {
                    "StoreApply"
                } else if pending == "PollWork" {
                    "WorkSuccess"
                } else {
                    "RetireApply"
                },
                worker: Some(n),
                role: "apply",
                input,
                capture,
                sample,
            });
        }
        if ordinary
            || (pending == "RetireQueue"
                && matches!(w.retire.as_str(), "first" | "resolve" | "second")
                && matches!(w.inner.as_str(), "ready" | "sent"))
        {
            let (input, capture, sample) = input_for(value, n, pending, oracle)?;
            result.push(DemandSpec {
                oracle_from: value.sql,
                family: "Capacity",
                worker: Some(n),
                role: "capacity",
                input,
                capture,
                sample,
            });
        }
    }
    if let Some(now) = next_neighbour(&value.wall_anchors, oracle.states[value.sql].episode.now) {
        // Inventory the structural clock input even when a stable authority
        // window blocks it; classification certifies that exclusion explicitly.
        result.push(DemandSpec {
            oracle_from: value.sql,
            family: "WallAdvance",
            worker: None,
            role: "apply",
            input: Some(Action::ClockAdvance { now }),
            capture: None,
            sample: None,
        });
    }
    if !context.stable {
        for input in oracle.domain_inputs(value.sql) {
            if environment_allowed(input, value, oracle) {
                result.push(DemandSpec {
                    oracle_from: value.sql,
                    family: "Environment",
                    worker: None,
                    role: "apply",
                    input: Some(input.clone()),
                    capture: None,
                    sample: None,
                });
            }
        }
    }
    Ok(result)
}
fn classify_demand(
    context: &Context,
    driver: &Driver,
    value: &Valuation,
    spec: &DemandSpec,
    oracle: &Oracle,
) -> Result<Option<Value>, String> {
    if let Some(reason) = spec.capture {
        return Ok(Some(json!({"kind":"capture","reason":reason})));
    }
    let episode = &oracle.states[spec.oracle_from].episode;
    if spec.sample.is_some_and(|sample| sample != episode.now) {
        return Ok(Some(json!({"kind":"sample","reason":"retirement_sample"})));
    }
    let input = spec
        .input
        .as_ref()
        .ok_or("MalformedProductDemand: no input without capture exclusion")?;
    if matches!(input, Action::ResolveInitialClaim { .. }) {
        let n = spec.worker.ok_or("InitialClaimRecoveryWorker")?;
        require(
            spec.family == "ClaimRecover"
                && spec.oracle_from == value.sql
                && recovery_ready(value, &value.w[n])
                && value.mono_now_ms < claim_limit(&value.w[n])?
                && *input == initial_resolution(value, n, oracle)?,
            "InitialClaimRecoveryInputAgreement",
        )?;
        return Ok(None);
    }
    if let Action::ReadClaimJob { job } = input {
        let n = spec.worker.ok_or("InitialClaimReadWorker")?;
        require(
            spec.family == "ClaimRecoverRead" && value.w[n].job == *job as i64,
            "InitialClaimReadInputAgreement",
        )?;
        return Ok(None);
    }
    if let Action::OrdinarySettle { .. } = input {
        let n = spec.worker.ok_or("ContextualOrdinaryWorker")?;
        require(
            value.w[n].ordinary.phase == "sent" && *input == ordinary_input(value, n)?,
            "ContextualOrdinaryInputAgreement",
        )?;
        return Ok(None);
    }
    if let Action::ClockAdvance { now } = input {
        require(
            spec.family == "WallAdvance"
                && Some(*now) == next_neighbour(&value.wall_anchors, episode.now),
            "ContextualWallInputAgreement",
        )?;
        return Ok(
            (context.stable && !stable_wall(value, *now, driver, oracle))
                .then(|| json!({"kind":"domain","reason":"stable_wall_window"})),
        );
    }
    let latest = |job: usize, token: usize| {
        !episode.tokens[job][token + 1..]
            .iter()
            .any(|later| later.claim_id == episode.tokens[job][token].claim_id)
    };
    let reason = match input.clone() {
        Action::Claim { job, .. } if episode.claims[job] >= 2 => Some("attempt_budget"),
        Action::Renew { job, token } if !latest(job, token) => Some("ordinary_revision"),
        Action::Renew { job, token }
            if episode
                .renewed_claims
                .contains(&episode.tokens[job][token].claim_id) =>
        {
            Some("renewal_budget")
        }
        Action::Bind { job, token, .. } | Action::Complete { job, token, .. }
            if !latest(job, token) =>
        {
            Some("ordinary_revision")
        }
        Action::Bind { job, token, lease } | Action::Complete { job, token, lease }
            if episode.leases[lease].owner_node_id != episode.tokens[job][token].node_id =>
        {
            Some("lease_owner")
        }
        Action::AcquireDomain { node } if episode.domain_acquired[node] => Some("domain_budget"),
        Action::AcquireDomain { node }
            if node < 2
                && !(0..2).any(|job| {
                    oracle.states[value.sql].view["jobs"][job]["state"] == "running"
                        && oracle.states[value.sql].view["jobs"][job]["owner"]
                            == ["node-a", "node-b"][node]
                }) =>
        {
            Some("domain_owner")
        }
        Action::RemoveOwner { node } if episode.removed[node] => Some("owner_removed"),
        Action::ClockAdvance { .. } if !oracle.domain_inputs(value.sql).contains(input) => {
            Some("no_clock_boundary")
        }
        _ => None,
    };
    require(
        reason.is_none() == oracle.domain_inputs(spec.oracle_from).contains(input),
        "OracleInputDomain: independent classification disagreement",
    )?;
    Ok(reason.map(|reason| json!({"kind":"domain","reason":reason})))
}

fn outcome<'a>(
    demands: &[Demand],
    family: &str,
    worker: Option<usize>,
    action: Option<&Action>,
    oracle: &'a Oracle,
) -> Option<&'a OracleEdge> {
    demands
        .iter()
        .find(|d| {
            d.spec.family == family
                && d.spec.worker == worker
                && action.is_none_or(|a| d.spec.input.as_ref() == Some(a))
        })
        .and_then(|d| d.edge)
        .map(|id| &oracle.edges[id])
}
fn local_reply(value: &Valuation, n: usize, tag: &str, driver: &Driver) -> Option<Valuation> {
    if !driver.accepts(&value.w[n], tag) {
        return None;
    }
    let mut next = value.clone();
    next.w[n].call = "applied".into();
    next.w[n].reply = tag.into();
    next.w[n].accepted = false;
    Some(next)
}
fn commit_reply(
    value: &Valuation,
    n: usize,
    edge: &OracleEdge,
    tag: &str,
    driver: &Driver,
    oracle: &Oracle,
) -> Option<Valuation> {
    let mut next = local_reply(value, n, tag, driver)?;
    move_sql(&mut next, edge, oracle);
    next.w[n].accepted = matches!(edge.action, Action::Settle { .. }) && edge.result == "accepted";
    if matches!(edge.action, Action::AcquireDomain { .. }) && edge.result == "acquired" {
        next.w[n].lease = (oracle.states[edge.to].episode.leases.len() - 1) as i64;
    }
    Some(next)
}
fn ordinary_steps(
    value: &Valuation,
    n: usize,
    oracle: &Oracle,
    demands: &[Demand],
) -> Result<Vec<Step>, String> {
    let mut steps = vec![];
    let w = &value.w[n];
    let o = &w.ordinary;
    let mut add = |family: &str, choice: Value, mut next: Valuation, edge: Option<usize>| {
        retain_anchors(&mut next, oracle);
        steps.push(Step {
            branch: branch(family, Some(n), choice),
            value: next,
            edge,
            resolution_edge: None,
            read_edge: None,
            cleanup_edge: None,
        });
    };
    let advance = |phase: &str| {
        let mut next = value.clone();
        next.w[n].ordinary.phase = phase.into();
        next
    };
    match o.phase.as_str() {
        "waiting_lock" if !w.read_lock => {
            add("OrdinaryLock", json!({}), advance("check_lost"), None)
        }
        "check_lost" => {
            let mut next = advance(if w.queue_lost {
                "finish"
            } else {
                "check_deadline"
            });
            next.w[n].ordinary.lost_seen = Some(w.queue_lost);
            if w.queue_lost {
                next.w[n].ordinary.observed_outcome = Some("local_lost".into());
            }
            add("OrdinaryLocal", json!({"phase":o.phase}), next, None);
        }
        "check_deadline" => {
            let expiry = w.local_deadline_mono_ms.ok_or("MissingLocalDeadline")?;
            let expired = value.mono_now_ms >= expiry;
            let mut next = advance(if expired { "finish" } else { "take_token" });
            let no = &mut next.w[n].ordinary;
            no.deadline_seen = Some(expiry);
            no.guard_mono_ms = Some(value.mono_now_ms);
            if expired {
                no.observed_outcome = Some("local_expired".into());
            }
            add("OrdinaryLocal", json!({"phase":o.phase}), next, None);
        }
        "take_token" => {
            let mut next = advance(if w.local_token { "sample" } else { "finish" });
            if w.local_token {
                require(w.token >= 0, "MissingLocalTokenCapture")?;
                next.w[n].ordinary.captured_token = Some(w.token as usize);
            } else {
                next.w[n].ordinary.observed_outcome = Some("local_no_token".into());
            }
            add("OrdinaryLocal", json!({"phase":o.phase}), next, None);
        }
        "sample" => {
            for clock_error in [false, true] {
                if clock_error && w.fault_used {
                    continue;
                }
                let mut next = advance(if clock_error { "finish" } else { "sent" });
                if clock_error {
                    next.w[n].ordinary.observed_outcome = Some("clock_error".into());
                    next.w[n].fault_used = true;
                } else {
                    next.w[n].ordinary.request_now_ms = Some(oracle.states[value.sql].episode.now);
                }
                add(
                    "OrdinaryLocal",
                    json!({"phase":o.phase,"clock_error":clock_error}),
                    next,
                    None,
                );
            }
        }
        "sent" => {
            let demand = demands
                .iter()
                .find(|d| d.spec.family == "OrdinaryCallReturn" && d.spec.worker == Some(n))
                .ok_or("MissingOrdinaryApplyDemand")?;
            let id = demand.edge.ok_or("MissingOrdinaryApplyEdge")?;
            let edge = &oracle.edges[id];
            require(
                matches!(
                    edge.result.as_str(),
                    "accepted"
                        | "refused"
                        | "validation_error_yield_time"
                        | "validation_error_retry_time"
                ),
                "UnknownOrdinaryOutcome",
            )?;
            let mut next = advance("applied");
            move_sql(&mut next, edge, oracle);
            next.w[n].ordinary.api_outcome = Some(edge.result.clone());
            add(
                "OrdinaryCallReturn",
                json!({"kind":"apply"}),
                next,
                Some(id),
            );
            if !w.fault_used {
                let mut next = advance("finish");
                next.w[n].fault_used = true;
                next.w[n].ordinary.observed_outcome = Some("transport_error".into());
                add(
                    "OrdinaryCallReturn",
                    json!({"kind":"error_before_apply"}),
                    next,
                    None,
                );
            }
        }
        "applied" => {
            for lost in [false, true] {
                if lost && w.fault_used {
                    continue;
                }
                let mut next = advance("finish");
                next.w[n].fault_used |= lost;
                next.w[n].ordinary.observed_outcome = if lost {
                    Some("transport_error".into())
                } else {
                    o.api_outcome.clone()
                };
                add("OrdinaryObserve", json!({"lost":lost}), next, None);
            }
        }
        "finish" => {
            let mut next = advance("done");
            if o.observed_outcome.as_deref() == Some("accepted") {
                next.w[n].local_token = false;
            }
            next.w[n].call = "replied".into();
            next.w[n].reply = "Done".into();
            next.w[n].adapter_reply = Some(AdapterReply {
                tag: "Done".into(),
                now_ms: None,
                retry_delay_ms: None,
            });
            add("OrdinaryLocal", json!({"phase":o.phase}), next, None);
        }
        "off" | "done" | "waiting_lock" => {}
        _ => return Err("UnknownOrdinaryPhase".into()),
    }
    for step in &steps {
        if step.edge.is_none() {
            require(step.value.sql == value.sql, "OrdinaryNoCallChangedStore")?;
        }
    }
    Ok(steps)
}
fn claim_recovery_steps(
    value: &Valuation,
    n: usize,
    oracle: &Oracle,
    demands: &[Demand],
) -> Result<Vec<Step>, String> {
    let w = &value.w[n];
    if w.claim_recovery.phase == "off" {
        return Ok(vec![]);
    }
    require(
        w.stage == "claim" && w.call == "replied" && w.reply == "Unknown",
        "InitialClaimRecoveryLifetime",
    )?;
    let limit = claim_limit(w)?;
    let mut next = value.clone();
    let mut read_edge = None;
    let mut cleanup_edge = None;
    let mut primary = None;
    let kind;
    let mut terminal = false;
    if value.mono_now_ms >= limit {
        kind = "timeout";
        next.w[n].reply = "Refused".into();
        terminal = true;
    } else if recovery_ready(value, w) {
        kind = "resolve";
        let id = demands
            .iter()
            .find(|d| d.spec.family == "ClaimRecover" && d.spec.worker == Some(n))
            .and_then(|d| d.edge)
            .ok_or("MissingInitialClaimResolutionEdge")?;
        primary = Some(id);
        let resolved = &oracle.edges[id];
        move_sql(&mut next, resolved, oracle);
        match resolved.result.as_str() {
            "running" => {
                let read = demands
                    .iter()
                    .find(|d| {
                        d.spec.family == "ClaimRecoverRead"
                            && d.spec.worker == Some(n)
                            && d.spec.oracle_from == resolved.to
                    })
                    .and_then(|d| d.edge)
                    .ok_or("MissingInitialClaimReadEdge")?;
                read_edge = Some(read);
                let read = &oracle.edges[read];
                move_sql(&mut next, read, oracle);
                let token_index = resolved
                    .resolved_token
                    .ok_or("MissingInitialResolvedCapture")?;
                let token = &oracle.states[resolved.to].episode.tokens[w.job as usize][token_index];
                let request: super::background_jobs::ClaimJob = serde_json::from_value(
                    w.claim_request.clone().ok_or("MissingActualClaimRequest")?,
                )
                .map_err(|e| format!("MalformedActualClaimRequest: {e}"))?;
                match read.result.as_str() {
                    "present" => {
                        let job: super::background_jobs::BackgroundJob = serde_json::from_value(
                            read.job_read.clone().ok_or("MissingActualInitialJobRead")?,
                        )
                        .map_err(|e| format!("MalformedActualInitialJobRead: {e}"))?;
                        if job.token.as_ref() == Some(token)
                            && job
                                .supported_payload()
                                .is_ok_and(|p| p.kind() == request.kind)
                        {
                            let confirmed = deadline(
                                w.claim_issued_mono_ms.ok_or("MissingInitialClaimIssue")?,
                                token.lease_expires_ms,
                                request.now_ms,
                            );
                            terminal = true;
                            if value.mono_now_ms < confirmed {
                                next.w[n].reply = "Claimed".into();
                                next.w[n].token = token_index as i64;
                                next.w[n].attempt = token.claim_id.clone();
                                next.w[n].local_deadline_mono_ms = Some(confirmed);
                                next.w[n].claim_snapshot = Some(sql::ClaimSnapshot {
                                    job: w.job as usize,
                                    raw_job_id: job.id,
                                    failed_attempts: job.failed_attempts,
                                    claim_id: token.claim_id.clone(),
                                    token_capture: token.clone(),
                                });
                            } else {
                                next.w[n].reply = "Refused".into();
                            }
                        }
                    }
                    "missing" => {
                        require(read.job_read.is_none(), "UnexpectedMissingInitialJobRead")?
                    }
                    _ => return Err("UnexpectedInitialJobReadResult".into()),
                }
            }
            "cancel_requested" => {
                let cleanup = demands
                    .iter()
                    .find(|d| {
                        d.spec.family == "ClaimRecoverCancel"
                            && d.spec.worker == Some(n)
                            && d.spec.oracle_from == resolved.to
                    })
                    .and_then(|d| d.edge)
                    .ok_or("MissingInitialClaimCancelEdge")?;
                cleanup_edge = Some(cleanup);
                let cleanup = &oracle.edges[cleanup];
                require(
                    matches!(cleanup.result.as_str(), "accepted" | "refused"),
                    "UnexpectedInitialClaimCancelResult",
                )?;
                move_sql(&mut next, cleanup, oracle);
                next.w[n].reply = "Refused".into();
                terminal = true;
            }
            "settled" | "lost_ownership" => {
                next.w[n].reply = "Refused".into();
                terminal = true;
            }
            "expired_or_pruned" => {}
            _ => return Err("UnexpectedInitialClaimResolutionResult".into()),
        }
    } else {
        return Ok(vec![]);
    }
    if terminal {
        next.w[n].claim_recovery = ClaimRecovery::off();
        if next.w[n].reply != "Claimed" {
            next.w[n].token = -1;
            next.w[n].attempt.clear();
            next.w[n].claim_snapshot = None;
            next.w[n].local_deadline_mono_ms = None;
        }
    } else {
        next.w[n].claim_recovery.phase = "waiting".into();
        next.w[n].claim_recovery.wake_mono_ms = Some(
            value
                .mono_now_ms
                .checked_add(250)
                .ok_or("InitialClaimWakeOverflow")?
                .min(limit),
        );
    }
    retain_anchors(&mut next, oracle);
    Ok(vec![Step {
        branch: branch("ClaimRecover", Some(n), json!({"kind":kind})),
        value: next,
        edge: primary,
        resolution_edge: None,
        read_edge,
        cleanup_edge,
    }])
}
fn successors(
    context: &Context,
    value: &Valuation,
    driver: &Driver,
    oracle: &Oracle,
    demands: &[Demand],
) -> Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    let mut push = |family: &str,
                    n: Option<usize>,
                    choice: Value,
                    mut next: Valuation,
                    edge: Option<usize>| {
        retain_anchors(&mut next, oracle);
        steps.push(Step {
            branch: branch(family, n, choice),
            value: next,
            edge,
            resolution_edge: None,
            read_edge: None,
            cleanup_edge: None,
        });
    };
    let edge_id = |edge: &OracleEdge| {
        demands
            .iter()
            .find_map(|d| d.edge.filter(|id| std::ptr::eq(&oracle.edges[*id], edge)))
            .expect("demanded oracle edge")
    };
    for n in 0..2 {
        let w = &value.w[n];
        let mut next = value.clone();
        next.available[n] = !value.available[n];
        push(
            if value.available[n] {
                "RemoveAvailability"
            } else {
                "MakeAvailable"
            },
            Some(n),
            json!({}),
            next,
            None,
        );
        if w.stage == "idle" && value.available[n] {
            for job in 0..2 {
                if oracle.states[value.sql].view["jobs"][job]["state"] == "queued"
                    && oracle.states[value.sql].view["jobs"][job]["request_done"] == false
                    && oracle.states[value.sql].view["jobs"][job]["not_before_ms"]
                        .as_i64()
                        .is_some_and(|t| t <= oracle.states[value.sql].episode.now)
                    && (!context.stable || job == n)
                {
                    let mut next = value.clone();
                    next.w[n] = Worker::idle();
                    next.w[n].stage = "claim".into();
                    next.w[n].job = job as i64;
                    next.w[n].admission = true;
                    push("Admission", Some(n), json!({"job":job}), next, None);
                }
            }
        }
        if w.stage == "claim" {
            match w.call.as_str() {
                "ready" => {
                    let mut next = value.clone();
                    next.w[n].call = "sent".into();
                    push("ClaimSend", Some(n), json!({}), next, None);
                }
                "sent" => {
                    if let Some(edge) = outcome(demands, "ClaimApply", Some(n), None, oracle) {
                        let mut next = value.clone();
                        next.sql = edge.to;
                        let nw = &mut next.w[n];
                        nw.call = "applied".into();
                        nw.claim_issued_mono_ms = Some(value.mono_now_ms);
                        nw.claim_request_now_ms = Some(oracle.states[value.sql].episode.now);
                        nw.claim_request = Some(
                            edge.claim_request
                                .clone()
                                .ok_or("MissingActualClaimRequest")?,
                        );
                        if edge.result == "claimed" {
                            let snapshot = edge
                                .claim_snapshot
                                .clone()
                                .ok_or("MissingActualClaimSnapshot")?;
                            nw.local_deadline_mono_ms = Some(deadline(
                                value.mono_now_ms,
                                snapshot.token_capture.lease_expires_ms,
                                oracle.states[value.sql].episode.now,
                            ));
                            nw.claim_snapshot = Some(snapshot);
                            let token =
                                oracle.states[edge.to].episode.tokens[w.job as usize].len() - 1;
                            nw.reply = "Claimed".into();
                            nw.token = token as i64;
                            nw.attempt = oracle.states[edge.to].episode.tokens[w.job as usize]
                                [token]
                                .claim_id
                                .clone();
                        } else {
                            nw.reply = "Refused".into();
                            nw.token = -1;
                            nw.attempt = "".into();
                        }
                        push("ClaimApply", Some(n), json!({}), next, Some(edge_id(edge)));
                    }
                }
                "applied" => {
                    for lost in [false, true] {
                        let key = claim_loss_key(w, n)?;
                        if lost
                            && (context.stable
                                || w.fault_used
                                || value.claim_loss_keys.contains(&key))
                        {
                            continue;
                        }
                        let mut next = value.clone();
                        next.w[n].call = "replied".into();
                        if lost {
                            next.w[n].reply = "Unknown".into();
                            next.w[n].claim_snapshot = None;
                            next.w[n].local_deadline_mono_ms = None;
                            next.w[n].claim_recovery.phase = "ready".into();
                            next.claim_loss_keys.push(key);
                            next.claim_loss_keys.sort();
                            require(next.claim_loss_keys.len() <= 8, "ClaimLossInputDomain")?;
                        } else if w.reply != "Claimed"
                            || !(w
                                .claim_issued_mono_ms
                                .is_some_and(|i| value.mono_now_ms < i + JOB_LEASE_MS)
                                && w.local_deadline_mono_ms
                                    .is_some_and(|d| value.mono_now_ms < d))
                        {
                            // claim_with_resolution checks delivery, separately from the
                            // ActiveBackgroundJob::start guard after ClaimReturn scheduling.
                            next.w[n].reply = "Refused".into();
                            next.w[n].claim_snapshot = None;
                            next.w[n].local_deadline_mono_ms = None;
                            // Source non-error None returns without retaining a new clock landmark.
                            next.w[n].claim_request = None;
                            next.w[n].claim_issued_mono_ms = None;
                            next.w[n].claim_request_now_ms = None;
                        }
                        next.w[n].fault_used |= lost;
                        push("ClaimReply", Some(n), json!({"lost":lost}), next, None);
                    }
                }
                "replied" if w.reply != "Unknown" => {
                    let mut next = value.clone();
                    let timely = w
                        .claim_issued_mono_ms
                        .is_some_and(|i| value.mono_now_ms < i + JOB_LEASE_MS)
                        && w.local_deadline_mono_ms
                            .is_some_and(|d| value.mono_now_ms < d);
                    if w.reply == "Claimed" && timely {
                        next.w[n].stage = "driver".into();
                        next.w[n].ctrl = driver.initial as i64;
                        next.w[n].call = "ready".into();
                        next.w[n].started = true;
                        next.w[n].local_token = true;
                        next.w[n].queue_lost = false;
                        next.real_drivers[n] = Some(RealDriver::new());
                    } else {
                        next.w[n] = Worker::idle();
                    }
                    push("ClaimReturn", Some(n), json!({}), next, None);
                }
                "replied" => {}
                _ => return Err("MalformedProductCall".into()),
            }
            for step in claim_recovery_steps(value, n, oracle, demands)? {
                push(
                    "ClaimRecover",
                    Some(n),
                    step.branch.choice,
                    step.value,
                    step.edge,
                );
            }
            continue;
        }
        if w.stage != "driver" {
            continue;
        }
        let pending = driver.pending(w);
        if !context.stable && w.started && !w.dropped && !w.queue_lost {
            let mut next = value.clone();
            next.w[n].queue_lost = true;
            push("QueueLossSignal", Some(n), json!({}), next, None);
        }
        if driver.states[w.ctrl as usize]["terminal"] == true
            && (!context.stable
                || (w.dropped
                    && !value.completed.contains(&(w.job as usize))
                    && oracle.states[value.sql].episode.claims[w.job as usize] < 2))
        {
            let mut next = value.clone();
            next.w[n] = Worker::idle();
            next.real_drivers[n] = None;
            push("Recycle", Some(n), json!({}), next, None);
        }
        if matches!(pending, "SettleYield" | "SettleRetry") {
            if w.call == "ready" {
                let concrete = value.real_drivers[n]
                    .ok_or("MissingActualDriver")?
                    .request();
                let Some(plurx_worker_control::library_driver::Request::Settle(payload)) = concrete
                else {
                    return Err("ActualOrdinaryRequestMismatch".into());
                };
                let mut next = value.clone();
                next.w[n].call = "sent".into();
                next.w[n].ordinary = Ordinary::off();
                next.w[n].ordinary.phase = "waiting_lock".into();
                next.w[n].ordinary.payload = Some(SavedSettlement::from(payload).into());
                next.w[n].adapter_reply = None;
                push("Dispatch", Some(n), json!({}), next, None);
            } else {
                for step in ordinary_steps(value, n, oracle, demands)? {
                    push(
                        &step.branch.family,
                        step.branch.worker,
                        step.branch.choice,
                        step.value,
                        step.edge,
                    );
                }
            }
        }
        if driver.states[w.ctrl as usize]["terminal"] == false
            && w.call == "ready"
            && !matches!(pending, "SettleYield" | "SettleRetry")
        {
            let mut next = value.clone();
            next.w[n].call = "sent".into();
            next.w[n].read_lock = pending == "PollWork";
            next.w[n].accepted = false;
            next.w[n].adapter_reply = None;
            push("Dispatch", Some(n), json!({}), next, None);
        }
        if w.call == "sent" && !matches!(pending, "SettleYield" | "SettleRetry") {
            if let Some(edge) = outcome(demands, "StoreApply", Some(n), None, oracle) {
                let tag = match pending {
                    "AcquireDomain" => {
                        if edge.result == "acquired" {
                            "DomainAcquired"
                        } else {
                            "DomainRefused"
                        }
                    }
                    "Bind" => {
                        if edge.result == "accepted" {
                            "BindAccepted"
                        } else {
                            "BindRefused"
                        }
                    }
                    "SettleYield" | "SettleRetry" => "Done",
                    _ => "ReleaseReturned",
                };
                let filtered = context.stable
                    && matches!(pending, "AcquireDomain" | "Bind")
                    && !matches!(edge.result.as_str(), "acquired" | "accepted");
                if !filtered {
                    if let Some(next) = commit_reply(value, n, edge, tag, driver, oracle) {
                        push("StoreApply", Some(n), json!({}), next, Some(edge_id(edge)));
                    }
                }
            }
            if !context.stable
                && !w.fault_used
                && matches!(
                    pending,
                    "AcquireDomain"
                        | "Bind"
                        | "SettleYield"
                        | "SettleRetry"
                        | "ReleaseRefused"
                        | "ReleaseRetry"
                        | "ReleaseNone"
                        | "ReleaseInterrupted"
                )
            {
                let tag = match pending {
                    "AcquireDomain" => "DomainRefused",
                    "Bind" => "BindRefused",
                    "SettleYield" | "SettleRetry" => "Done",
                    _ => "ReleaseReturned",
                };
                if let Some(mut next) = local_reply(value, n, tag, driver) {
                    next.w[n].fault_used = true;
                    push("AmbiguousWithoutApply", Some(n), json!({}), next, None);
                }
            }
            if !w.read_lock
                && matches!(
                    pending,
                    "RevokeDomain" | "CancelWork" | "DropPublisher" | "DropAdmission"
                )
            {
                if let Some(next) = local_reply(value, n, "Done", driver) {
                    push("SimpleApply", Some(n), json!({}), next, None);
                }
            }
            if pending == "PollWork" {
                if let Some(edge) = outcome(demands, "WorkSuccess", Some(n), None, oracle) {
                    if edge.result == "accepted" && !w.queue_lost {
                        if let Some(next) =
                            commit_reply(value, n, edge, "WorkSucceeded", driver, oracle)
                        {
                            push("WorkSuccess", Some(n), json!({}), next, Some(edge_id(edge)));
                        }
                    }
                }
                if !context.stable {
                    for tag in ["QueueLoss", "DomainLoss", "Pressure", "WorkFailed"] {
                        // QueueLoss observes the separately latched signal;
                        // once ready it outranks every other work outcome.
                        if (tag == "QueueLoss") != w.queue_lost {
                            continue;
                        }
                        if let Some(mut next) = local_reply(value, n, tag, driver) {
                            if tag == "QueueLoss" {
                                next.w[n].queue_lost = true;
                            }
                            push("WorkOther", Some(n), json!({"reply":tag}), next, None);
                        }
                    }
                }
            }
            if pending == "JoinWork" && context.mutation != "withheld-join" {
                let tags = match driver.states[w.ctrl as usize]["work"]
                    .as_str()
                    .expect("validated work")
                {
                    "Completed { ok: true }" => vec!["JoinedSucceeded"],
                    "Completed { ok: false }" => vec!["JoinedFailed"],
                    _ => vec!["JoinedFailed", "JoinedSucceeded"],
                };
                for tag in tags {
                    if let Some(next) = local_reply(value, n, tag, driver) {
                        push("JoinReturn", Some(n), json!({"reply":tag}), next, None);
                    }
                }
            }
            if pending == "RetireQueue" {
                match w.retire.as_str() {
                    "off" if !w.read_lock => {
                        let mut next = value.clone();
                        next.w[n].retire = "drain".into();
                        next.w[n].queue_lost = true;
                        push("RetireBegin", Some(n), json!({}), next, None);
                    }
                    "drain" => {
                        let mut next = value.clone();
                        next.w[n].retire = "take".into();
                        push("RetireDrain", Some(n), json!({}), next, None);
                    }
                    "take" => {
                        let mut next = value.clone();
                        next.w[n].retire = if w.local_token {
                            "sample-first"
                        } else {
                            "done"
                        }
                        .into();
                        next.w[n].original = w.token;
                        next.w[n].local_token = false;
                        push("RetireTake", Some(n), json!({}), next, None);
                    }
                    "sample-first" | "sample-resolve" | "sample-after" => {
                        for clock_error in [false, true] {
                            if clock_error && w.fault_used {
                                continue;
                            }
                            let mut next = value.clone();
                            let nw = &mut next.w[n];
                            let sample = if clock_error {
                                -1
                            } else {
                                oracle.states[value.sql].episode.now
                            };
                            nw.sample = sample;
                            nw.inner = "ready".into();
                            nw.fault_used |= clock_error;
                            match w.retire.as_str() {
                                "sample-first" => nw.first_sample = sample,
                                "sample-resolve" => nw.resolve_sample = sample,
                                _ => nw.after_sample = sample,
                            };
                            nw.retire = if clock_error {
                                "warn"
                            } else {
                                match w.retire.as_str() {
                                    "sample-first" => "first",
                                    "sample-resolve" => "resolve",
                                    _ => {
                                        if matches!(
                                            w.outcome.as_str(),
                                            "running" | "cancel_requested"
                                        ) {
                                            "second"
                                        } else {
                                            "done"
                                        }
                                    }
                                }
                            }
                            .into();
                            push(
                                "RetireSample",
                                Some(n),
                                json!({"clock_error":clock_error}),
                                next,
                                None,
                            );
                        }
                    }
                    "first" | "resolve" | "second" => match w.inner.as_str() {
                        "ready" => {
                            let mut next = value.clone();
                            next.w[n].inner = "sent".into();
                            push("RetireSend", Some(n), json!({}), next, None);
                        }
                        "sent" => {
                            if let Some(edge) =
                                outcome(demands, "RetireApply", Some(n), None, oracle)
                            {
                                let mut next = value.clone();
                                move_sql(&mut next, edge, oracle);
                                next.w[n].inner = "applied".into();
                                next.w[n].outcome = edge.result.clone();
                                if matches!(edge.action, Action::Resolve { .. }) {
                                    next.w[n].resolved =
                                        edge.resolved_token.map_or(-1, |i| i as i64);
                                }
                                push("RetireApply", Some(n), json!({}), next, Some(edge_id(edge)));
                            }
                            if !w.fault_used {
                                let mut next = value.clone();
                                next.w[n].inner = "applied".into();
                                next.w[n].outcome = "error".into();
                                next.w[n].fault_used = true;
                                push("RetireError", Some(n), json!({}), next, None);
                            }
                        }
                        "applied" => {
                            for lost in [false, true] {
                                if lost && w.fault_used {
                                    continue;
                                }
                                let mut next = value.clone();
                                next.w[n].inner = "replied".into();
                                next.w[n].fault_used |= lost;
                                if lost {
                                    next.w[n].outcome = "error".into();
                                }
                                push("RetireReply", Some(n), json!({"lost":lost}), next, None);
                            }
                        }
                        "replied" => {
                            let mut next = value.clone();
                            next.w[n].retire = match w.retire.as_str() {
                                "first" => {
                                    if w.outcome == "accepted" {
                                        "done"
                                    } else {
                                        "sample-resolve"
                                    }
                                }
                                "resolve" => {
                                    if w.outcome == "error" {
                                        "warn"
                                    } else {
                                        "sample-after"
                                    }
                                }
                                _ => {
                                    if w.outcome == "error" {
                                        "warn"
                                    } else {
                                        "done"
                                    }
                                }
                            }
                            .into();
                            push("RetireObserve", Some(n), json!({}), next, None);
                        }
                        _ => return Err("MalformedRetirementInner".into()),
                    },
                    "done" | "warn" => {
                        if let Some(next) = local_reply(value, n, "Done", driver) {
                            push("RetireFinish", Some(n), json!({}), next, None);
                        }
                    }
                    _ => {}
                }
            }
        }
        if w.call == "applied" {
            let allow_loss = !w.fault_used
                && matches!(
                    pending,
                    "AcquireDomain"
                        | "Bind"
                        | "SettleYield"
                        | "SettleRetry"
                        | "PollWork"
                        | "ReleaseRefused"
                        | "ReleaseRetry"
                        | "ReleaseNone"
                        | "ReleaseInterrupted"
                )
                && (!context.stable
                    || matches!(
                        pending,
                        "PollWork"
                            | "ReleaseRefused"
                            | "ReleaseRetry"
                            | "ReleaseNone"
                            | "ReleaseInterrupted"
                    ));
            for lost in [false, true] {
                if lost && !allow_loss {
                    continue;
                }
                let mut next = value.clone();
                let nw = &mut next.w[n];
                nw.call = "replied".into();
                nw.read_lock = false;
                nw.fault_used |= lost;
                if lost {
                    nw.reply = match w.reply.as_str() {
                        "WorkSucceeded" => "WorkFailed",
                        "DomainAcquired" => "DomainRefused",
                        "BindAccepted" => "BindRefused",
                        _ => &w.reply,
                    }
                    .into();
                }
                if !lost && w.accepted && matches!(pending, "SettleYield" | "SettleRetry") {
                    nw.local_token = false;
                }
                nw.adapter_reply = if matches!(
                    nw.reply.as_str(),
                    "DomainAcquired" | "DomainRefused" | "ReleaseReturned"
                ) {
                    None
                } else {
                    Some(AdapterReply {
                        tag: nw.reply.clone(),
                        now_ms: None,
                        retry_delay_ms: None,
                    })
                };
                push("Reply", Some(n), json!({"lost":lost}), next, None);
            }
        }
        if w.call == "replied"
            && w.adapter_reply.is_none()
            && matches!(
                w.reply.as_str(),
                "DomainAcquired" | "DomainRefused" | "ReleaseReturned"
            )
        {
            let zero = w.reply == "DomainAcquired" || pending == "ReleaseNone";
            let retry = if pending == "ReleaseRetry" {
                let snapshot = w
                    .claim_snapshot
                    .as_ref()
                    .ok_or("MissingImmutableClaimSnapshot")?;
                plurx_worker_control::retry_policy::retry_delay_ms(
                    &snapshot.raw_job_id,
                    snapshot.failed_attempts,
                )
            } else {
                0
            };
            let mut next = value.clone();
            next.w[n].adapter_reply = Some(AdapterReply {
                tag: w.reply.clone(),
                now_ms: Some(if zero {
                    0
                } else {
                    oracle.states[value.sql].episode.now
                }),
                retry_delay_ms: Some(retry),
            });
            push("AdapterSample", Some(n), json!({}), next, None);
        }
        if let Some(adapter) = w.adapter_reply.as_ref().filter(|_| w.call == "replied") {
            if let Some(to) = driver.replies.get(&(w.ctrl as usize, w.reply.clone())) {
                let mut next = value.clone();
                let mut real = value.real_drivers[n].ok_or("MissingActualDriver")?;
                real.drive(concrete_reply(adapter)?)
                    .map_err(|_| "ActualDriverRejectedConcreteReply")?;
                next.real_drivers[n] = Some(real);
                let nw = &mut next.w[n];
                nw.driver_settlement = real.settlement().map(Into::into);
                nw.adapter_reply = None;
                nw.ctrl = *to as i64;
                nw.call = "ready".into();
                nw.joined |= pending == "JoinWork";
                nw.work_started |= pending == "Bind" && w.reply == "BindAccepted";
                nw.dropped |= pending == "DropAdmission";
                if pending == "DropAdmission"
                    || (context.mutation == "drop-before-join" && pending == "CancelWork")
                {
                    nw.admission = false;
                }
                push("DriverReturn", Some(n), json!({}), next, None);
            }
        }
    }
    if let Some(edge) = outcome(demands, "WallAdvance", None, None, oracle) {
        let mut next = value.clone();
        move_sql(&mut next, edge, oracle);
        let Action::ClockAdvance { now } = edge.action else {
            return Err("WallAdvanceOracleAction".into());
        };
        push(
            "WallAdvance",
            None,
            json!({"now":now}),
            next,
            Some(edge_id(edge)),
        );
    }
    if let Some(now) = next_neighbour(&value.mono_anchors, value.mono_now_ms) {
        if !context.stable || stable_mono(value, now) {
            let mut next = value.clone();
            next.mono_now_ms = now;
            push("MonoAdvance", None, json!({"now":now}), next, None);
        }
    }
    if !context.stable {
        for demand in demands.iter().filter(|d| d.spec.family == "Environment") {
            let Some(id) = demand.edge else {
                continue;
            };
            let edge = &oracle.edges[id];
            let input = demand.spec.input.as_ref().expect("environment input");
            let matching = if let Action::Renew { job, token } = input {
                value
                    .w
                    .iter()
                    .enumerate()
                    .filter_map(|(n, w)| {
                        same_claim(w, *job, *token, oracle, value.sql).then_some(n)
                    })
                    .collect::<Vec<_>>()
            } else {
                vec![]
            };
            for ack in [false, true] {
                if !ack
                    && !(edge.result == "renewed"
                        && !matching.is_empty()
                        && matching.iter().all(|n| !value.w[*n].fault_used))
                {
                    continue;
                }
                let needs_resolve = !matching.is_empty() && (edge.result != "renewed" || !ack);
                for resolve_error in [false, true] {
                    if resolve_error
                        && (!needs_resolve
                            || !ack
                            || matching.iter().any(|n| value.w[*n].fault_used))
                    {
                        continue;
                    }
                    let resolution = if needs_resolve && !resolve_error {
                        let Action::Renew { job, token } = input else {
                            return Err("RenewResolutionInput".into());
                        };
                        let expected = Action::Resolve {
                            job: *job,
                            token: *token,
                        };
                        Some(
                            demands
                                .iter()
                                .find(|d| {
                                    d.spec.family == "EnvironmentResolve"
                                        && d.spec.oracle_from == edge.to
                                        && d.spec.input.as_ref() == Some(&expected)
                                })
                                .and_then(|d| d.edge)
                                .ok_or("MissingRenewalResolutionEdge")?,
                        )
                    } else {
                        None
                    };
                    let mut next = value.clone();
                    move_sql(&mut next, edge, oracle);
                    if let Some(r) = resolution {
                        move_sql(&mut next, &oracle.edges[r], oracle);
                    }
                    for n in &matching {
                        let nw = &mut next.w[*n];
                        let old = &value.w[*n];
                        nw.fault_used |= !ack || resolve_error;
                        if resolve_error {
                            nw.queue_lost = true;
                            continue;
                        }
                        let token = if edge.result == "renewed" && ack {
                            let Action::Renew { job, .. } = input else {
                                return Err("RenewInput".into());
                            };
                            Some((
                                oracle.states[edge.to].episode.tokens[*job].len() - 1,
                                "running",
                            ))
                        } else {
                            let r = &oracle.edges[resolution.ok_or("MissingRenewResolution")?];
                            match r.result.as_str() {
                                "running" | "cancel_requested" => Some((
                                    r.resolved_token.ok_or("MissingActualResolvedToken")?,
                                    r.result.as_str(),
                                )),
                                "settled" | "lost_ownership" | "expired_or_pruned" => None,
                                _ => return Err("UnknownRenewResolutionResult".into()),
                            }
                        };
                        if let Some((token, tag)) = token {
                            nw.token = token as i64;
                            nw.local_token = true;
                            if tag == "cancel_requested" {
                                nw.cancelling = true;
                                nw.queue_lost = true;
                                continue;
                            }
                            if old.queue_lost
                                || value.mono_now_ms
                                    >= old
                                        .local_deadline_mono_ms
                                        .ok_or("MissingRenewWatchDeadline")?
                            {
                                nw.queue_lost = true;
                                continue;
                            }
                            let returned =
                                &oracle.states[next.sql].episode.tokens[old.job as usize][token];
                            let local = deadline(
                                value.mono_now_ms,
                                returned.lease_expires_ms,
                                oracle.states[value.sql].episode.now,
                            );
                            nw.local_deadline_mono_ms = Some(local);
                            if value.mono_now_ms >= local {
                                nw.queue_lost = true;
                            }
                        } else {
                            nw.local_token = false;
                            nw.queue_lost = true;
                        }
                    }
                    push(
                        "Environment",
                        None,
                        json!({"input":input,"ack":ack,"resolve_error":resolve_error}),
                        next,
                        Some(id),
                    );
                }
            }
        }
    }
    for step in &mut steps {
        if step.branch.family == "ClaimRecover" && step.branch.choice["kind"] == "resolve" {
            let n = step.branch.worker.ok_or("InitialRecoveryWorker")?;
            let primary = &oracle.edges[step.edge.ok_or("MissingInitialRecoveryEvidence")?];
            let family = match primary.result.as_str() {
                "running" => Some("ClaimRecoverRead"),
                "cancel_requested" => Some("ClaimRecoverCancel"),
                _ => None,
            };
            if let Some(family) = family {
                let id = demands
                    .iter()
                    .find(|d| {
                        d.spec.family == family
                            && d.spec.worker == Some(n)
                            && d.spec.oracle_from == primary.to
                    })
                    .and_then(|d| d.edge)
                    .ok_or("MissingInitialRecoveryDependentEvidence")?;
                if family == "ClaimRecoverRead" {
                    step.read_edge = Some(id);
                } else {
                    step.cleanup_edge = Some(id);
                }
            }
        }
        if step.branch.family != "Environment" || step.branch.choice["resolve_error"] == true {
            continue;
        }
        let Some(id) = step.edge else {
            continue;
        };
        let edge = &oracle.edges[id];
        let Action::Renew { job, token } = edge.action else {
            continue;
        };
        let matching = value
            .w
            .iter()
            .any(|w| same_claim(w, job, token, oracle, value.sql));
        if matching && (edge.result != "renewed" || step.branch.choice["ack"] == false) {
            step.resolution_edge = demands
                .iter()
                .find(|d| {
                    d.spec.family == "EnvironmentResolve"
                        && d.spec.oracle_from == edge.to
                        && d.spec.input.as_ref() == Some(&Action::Resolve { job, token })
                })
                .and_then(|d| d.edge);
            require(
                step.resolution_edge.is_some(),
                "MissingRenewalResolutionEvidence",
            )?;
        }
    }
    for step in &steps {
        verify_real(&step.value, driver)?;
    }
    Ok(steps)
}
fn memberships(step: &Step, value: &Valuation, context: &Context, oracle: &Oracle) -> Vec<String> {
    if step.branch.family == "MonoAdvance"
        && value.w.iter().any(|w| {
            w.claim_recovery.phase == "waiting"
                && w.claim_recovery
                    .wake_mono_ms
                    .is_some_and(|wake| value.mono_now_ms < wake)
        })
    {
        return vec!["ClaimRecoveryWake/global".into()];
    }
    if step.branch.family == "WallAdvance"
        && context.stable
        && oracle.states[value.sql].view["jobs"]
            .as_array()
            .expect("jobs")
            .iter()
            .any(|j| {
                j["state"] == "queued"
                    && j["request_done"] == false
                    && j["not_before_ms"]
                        .as_i64()
                        .is_some_and(|t| t > oracle.states[value.sql].episode.now)
            })
    {
        return vec!["EligibilityAdvance/global".into()];
    }
    let Some(n) = step.branch.worker else {
        return vec![];
    };
    let f = step.branch.family.as_str();
    let mut labels = BTreeSet::new();
    if FAIR.contains(&f) {
        labels.insert(format!("{f}/{n}"));
    }
    if matches!(
        f,
        "RetireBegin"
            | "RetireTake"
            | "RetireSample"
            | "RetireSend"
            | "RetireReply"
            | "RetireObserve"
            | "RetireFinish"
    ) {
        labels.insert(format!("RetireLocal/{n}"));
    }
    if matches!(f, "RetireApply" | "RetireError") {
        labels.insert(format!("RetireCallReturn/{n}"));
    }
    if matches!(f, "WorkSuccess" | "WorkOther") {
        labels.insert(format!("ExternalReturn/{n}"));
    }
    if f == "ClaimApply"
        && step
            .edge
            .is_some_and(|id| oracle.edges[id].result == "claimed")
    {
        labels.insert(format!("ClaimWin/{n}"));
    }
    labels.into_iter().collect()
}

const WORKER_COLUMNS: [&str; 37] = [
    "accepted",
    "adapter_reply",
    "admission",
    "afterSample",
    "attempt",
    "call",
    "cancelling",
    "claim_issued_mono_ms",
    "claim_recovery",
    "claim_request",
    "claim_request_now_ms",
    "claim_snapshot",
    "ctrl",
    "driver_settlement",
    "dropped",
    "faultUsed",
    "firstSample",
    "inner",
    "job",
    "joined",
    "lease",
    "localToken",
    "local_deadline_mono_ms",
    "ordinary",
    "original",
    "outcome",
    "queue_lost",
    "readLock",
    "reply",
    "resolveSample",
    "resolved",
    "retire",
    "sample",
    "stage",
    "started",
    "token",
    "workStarted",
];
const WORKER_VALUE_REFS: [&str; 6] = [
    "claim_snapshot",
    "claim_request",
    "claim_recovery",
    "driver_settlement",
    "adapter_reply",
    "ordinary",
];

/// IDs are append-stable references to exact values, never semantic quotients.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct ProductKey {
    context: usize,
    sql: StoreHandle,
    workers: [usize; 2],
    available: usize,
    completed: usize,
    mono_now_ms: i64,
    wall_anchors: usize,
    mono_anchors: usize,
    claim_loss_keys: usize,
}
struct ProductState {
    key: ProductKey,
    real_drivers: [Option<RealDriver>; 2],
    hash: String,
    parent: Option<(usize, Branch)>,
}
impl ProductState {
    fn record(&self) -> Value {
        let k = self.key;
        json!([
            k.context,
            k.sql,
            k.workers[0],
            k.workers[1],
            k.available,
            k.completed,
            k.mono_now_ms,
            k.wall_anchors,
            k.mono_anchors,
            k.claim_loss_keys,
            self.hash
        ])
    }
    fn valuation(&self, pools: &WirePools) -> Result<Valuation, String> {
        let k = self.key;
        Ok(Valuation {
            sql: k.sql,
            w: [
                pools.worker_values[k.workers[0]].clone(),
                pools.worker_values[k.workers[1]].clone(),
            ],
            available: pools.decode(k.available)?,
            completed: pools.decode(k.completed)?,
            mono_now_ms: k.mono_now_ms,
            wall_anchors: pools.decode(k.wall_anchors)?,
            mono_anchors: pools.decode(k.mono_anchors)?,
            claim_loss_keys: pools.decode(k.claim_loss_keys)?,
            real_drivers: self.real_drivers,
        })
    }
}

#[derive(Default)]
struct WirePools {
    values: Vec<Value>,
    value_index: HashMap<String, usize>,
    value_elapsed: Duration,
    worker_elapsed: Duration,
    key_elapsed: Duration,
    workers: Vec<Value>,
    worker_values: Vec<Worker>,
    worker_index: HashMap<String, usize>,
}
impl WirePools {
    fn value<T: Serialize>(&mut self, value: &T, budget: &mut Budget) -> Result<usize, String> {
        let started = Instant::now();
        let result = self.value_inner(value, budget);
        self.value_elapsed = self.value_elapsed.saturating_add(started.elapsed());
        result
    }
    fn value_inner<T: Serialize>(
        &mut self,
        value: &T,
        budget: &mut Budget,
    ) -> Result<usize, String> {
        let value = serde_json::to_value(value).map_err(|e| format!("ProductWire: {e}"))?;
        let key = canonical(&value);
        // HashMap's full String equality resolves collisions; SHA is not an identity key.
        if let Some(id) = self.value_index.get(&key) {
            return Ok(*id);
        }
        budget.charge("value_pool", &value, 1)?;
        let id = self.values.len();
        self.values.push(value);
        self.value_index.insert(key, id);
        Ok(id)
    }
    fn worker(&mut self, worker: &Worker, budget: &mut Budget) -> Result<usize, String> {
        let started = Instant::now();
        let result = self.worker_inner(worker, budget);
        self.worker_elapsed = self.worker_elapsed.saturating_add(started.elapsed());
        result
    }
    fn worker_inner(&mut self, worker: &Worker, budget: &mut Budget) -> Result<usize, String> {
        let logical = serde_json::to_value(worker).map_err(|e| format!("ProductWire: {e}"))?;
        require(
            logical.as_object().map(|v| v.len()) == Some(WORKER_COLUMNS.len()),
            "ProductWire: Worker fields changed",
        )?;
        let mut row = Vec::with_capacity(WORKER_COLUMNS.len());
        for column in WORKER_COLUMNS {
            let value = logical
                .get(column)
                .ok_or("ProductWire: missing Worker column")?;
            if WORKER_VALUE_REFS.contains(&column) {
                row.push(json!(self.value(value, budget)?));
            } else {
                require(
                    !value.is_array() && !value.is_object(),
                    "ProductWire: non-scalar Worker cell",
                )?;
                row.push(value.clone());
            }
        }
        let row = Value::Array(row);
        let key = canonical(&row);
        if let Some(id) = self.worker_index.get(&key) {
            require(
                self.worker_values[*id] == *worker,
                "ProductWire: unequal Worker alias",
            )?;
            return Ok(*id);
        }
        budget.charge("worker_pool", &row, 1)?;
        let id = self.workers.len();
        self.workers.push(row);
        self.worker_values.push(worker.clone());
        self.worker_index.insert(key, id);
        Ok(id)
    }
    fn key(
        &mut self,
        context: usize,
        value: &Valuation,
        budget: &mut Budget,
    ) -> Result<ProductKey, String> {
        let started = Instant::now();
        let result = self.key_inner(context, value, budget);
        self.key_elapsed = self.key_elapsed.saturating_add(started.elapsed());
        result
    }
    fn key_inner(
        &mut self,
        context: usize,
        value: &Valuation,
        budget: &mut Budget,
    ) -> Result<ProductKey, String> {
        Ok(ProductKey {
            context,
            sql: value.sql,
            workers: [
                self.worker(&value.w[0], budget)?,
                self.worker(&value.w[1], budget)?,
            ],
            available: self.value(&value.available, budget)?,
            completed: self.value(&value.completed, budget)?,
            mono_now_ms: value.mono_now_ms,
            wall_anchors: self.value(&value.wall_anchors, budget)?,
            mono_anchors: self.value(&value.mono_anchors, budget)?,
            claim_loss_keys: self.value(&value.claim_loss_keys, budget)?,
        })
    }
    fn decode<T: serde::de::DeserializeOwned>(&self, id: usize) -> Result<T, String> {
        serde_json::from_value(self.values[id].clone()).map_err(|e| format!("ProductWire: {e}"))
    }
}

struct ByteLimit<W> {
    inner: W,
    bytes: usize,
    limit: usize,
}
impl<W: Write> Write for ByteLimit<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let next = self.bytes.checked_add(bytes.len()).ok_or_else(|| {
            std::io::Error::other("ProductResourceLimit: byte accounting overflow")
        })?;
        if next > self.limit {
            return Err(std::io::Error::other(
                "ProductResourceLimit: exact serialized output exceeds byte bound; incomplete",
            ));
        }
        let written = self.inner.write(bytes)?;
        self.bytes += written;
        Ok(written)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}
fn serialized_size<T: Serialize>(value: &T, limit: usize) -> Result<usize, String> {
    let mut writer = ByteLimit {
        inner: std::io::sink(),
        bytes: 0,
        limit,
    };
    serde_json::to_writer(&mut writer, value).map_err(|e| format!("ProductResourceLimit: {e}"))?;
    Ok(writer.bytes)
}
#[derive(Default)]
struct Budget {
    charge_elapsed: Duration,
    bytes: usize,
    stores: usize,
    oracle_edges: usize,
    component_bytes: BTreeMap<&'static str, usize>,
    component_records: BTreeMap<&'static str, usize>,
}
impl Budget {
    fn charge<T: Serialize>(
        &mut self,
        component: &'static str,
        value: &T,
        framing: usize,
    ) -> Result<(), String> {
        let started = Instant::now();
        let result = self.charge_inner(component, value, framing);
        self.charge_elapsed = self.charge_elapsed.saturating_add(started.elapsed());
        result
    }
    fn charge_inner<T: Serialize>(
        &mut self,
        component: &'static str,
        value: &T,
        framing: usize,
    ) -> Result<(), String> {
        let bytes = serialized_size(value, BUNDLE_BYTES)?
            .checked_add(framing)
            .ok_or("ProductResourceEstimate: byte accounting overflow; incomplete")?;
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .ok_or("ProductResourceEstimate: byte accounting overflow; incomplete")?;
        let total = self.component_bytes.entry(component).or_default();
        *total = total
            .checked_add(bytes)
            .ok_or("ProductResourceEstimate: component byte overflow")?;
        let count = self.component_records.entry(component).or_default();
        *count = count
            .checked_add(1)
            .ok_or("ProductResourceEstimate: component count overflow")?;
        require(self.bytes <= BUNDLE_BYTES, "ProductResourceEstimate: conservative serialized output estimate exceeds 256MiB; incomplete")
    }
    fn oracle(&mut self, oracle: &Oracle) -> Result<(), String> {
        while self.stores < oracle.states.len() {
            self.charge("store_states", &oracle.export_product_state(self.stores), 1)?;
            self.stores += 1;
        }
        while self.oracle_edges < oracle.edges.len() {
            self.charge("store_edges", &oracle.edges[self.oracle_edges], 1)?;
            self.oracle_edges += 1;
        }
        Ok(())
    }
}
fn product_payload(context: &Context, value: &Valuation) -> Value {
    json!({"context":context.id,"sql":value.sql,"w":value.w,"available":value.available,"completed":value.completed,"mono_now_ms":value.mono_now_ms,"wall_anchors":value.wall_anchors,"mono_anchors":value.mono_anchors,"claim_loss_keys":value.claim_loss_keys})
}
fn metadata_envelope(
    oracle: &Oracle,
    semantic: &BTreeMap<String, String>,
    driver_bytes: &[u8],
    contexts: &[Context],
) -> Value {
    // Max-width counts/IDs conservatively reserve all metadata/framing. Every
    // subsequently appended array entry and context member is charged separately.
    let mut descriptors = Vec::new();
    let mut by_context = serde_json::Map::new();
    for context in contexts {
        descriptors.push(json!({"id":context.id,"model":"AsyncDriverRefinement","stable_success":context.stable,"mutation":context.mutation,"witness":false,"initial":usize::MAX,"states":[]}));
        by_context.insert(context.id.into(), json!({"state_count":usize::MAX,"edge_count":usize::MAX,"demand_count":usize::MAX,"frontier_count":usize::MAX}));
    }
    for (id, mutation) in [
        ("boundary_local_revocation", "local-revocation"),
        ("boundary_unknown_rollback", "unknown-as-rollback"),
    ] {
        descriptors.push(json!({"id":id,"model":"BoundaryControls","stable_success":false,"mutation":mutation,"witness":false,"initial":null,"states":[]}));
    }
    json!({"schema_version":4,"scope":"selected_two_worker_product","source_boundaries":{"ordinary_settlement_payload_transfer":{"status":"pending","domain":"selected_two_worker_normal_clock"}},"fixture_identity":oracle.fixture_identity(),"driver_graph_sha256":digest(driver_bytes),"source_bindings":oracle.source_bindings(),"semantic_bindings":semantic,"schema_bindings":oracle.schema,"domain":sql::domain_metadata(),"store":{"initial":0,"states":[],"edges":[]},"contexts":descriptors,"value_pool":[],"worker_pool":[],"product_states":[],"product_edges":[],"demands":[],"enabledness":[],"coverage":{"complete":false,"no_skipped_demands":false,"store_state_count":usize::MAX,"store_edge_count":usize::MAX,"product_state_count":usize::MAX,"product_edge_count":usize::MAX,"demand_count":usize::MAX,"by_context":by_context}})
}
fn semantic_bindings() -> Result<BTreeMap<String, String>, String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let package = root.join("verification/async-driver-refinement");
    let names = [
        "checker.py",
        "product-schema.json",
        "AsyncDriverRefinement.tla",
        "BoundaryControls.tla",
        "safety.cfg",
        "cleanup.cfg",
        "selected-success.cfg",
        "nonvacuity.cfg",
        "control-weak-admission.cfg",
        "control-no-claim-win.cfg",
        "control-withheld-join.cfg",
        "control-old-poll-policy.cfg",
        "control-drop-before-join.cfg",
        "control-local-revocation.cfg",
        "control-unknown-as-rollback.cfg",
    ];
    names
        .into_iter()
        .map(|name| {
            let bytes =
                std::fs::read(package.join(name)).map_err(|e| format!("SemanticBindings: {e}"))?;
            Ok((
                format!("verification/async-driver-refinement/{name}"),
                digest(&bytes),
            ))
        })
        .collect()
}
struct Cutoff<'a> {
    context: &'a Context,
    from: usize,
    states: &'a [ProductState],
    queue: &'a VecDeque<usize>,
    expanded: usize,
    oracle: &'a Oracle,
    budget: &'a Budget,
}
fn cutoff(snapshot: Cutoff<'_>, reason: &str, action: Option<&Action>) {
    let Cutoff {
        context,
        from,
        states,
        queue,
        expanded,
        oracle,
        budget,
    } = snapshot;
    let mut tail = Vec::new();
    let mut cursor = from;
    while tail.len() < 8 {
        if let Some((parent, branch)) = &states[cursor].parent {
            tail.push(sql::diagnostic_action(encoded(branch)));
            cursor = *parent;
        } else {
            break;
        }
    }
    tail.reverse();
    // Failure-only: never serialize the full frontier, rows or canonical keys.
    eprintln!(
        "async_product_cutoff={}",
        json!({"complete":false,"bound":reason.split(':').next().unwrap_or("ResourceError"),"reason":sql::diagnostic_action(encoded(&json!({"reason":reason}))),"context":context.id,"store_states":oracle.states.len(),"store_edges":oracle.edges.len(),"product_states":states.len(),"expanded":expanded,"pending":queue.len(),"serialized_output_estimate":budget.bytes,"attempted_wire_component_bytes":budget.component_bytes,"attempted_wire_component_records":budget.component_records,"predecessor":from,"predecessor_sha256":states[from].hash,"rejected_action":action.map(|a|sql::diagnostic_action(encoded(a))),"oracle_predecessor_sha256":digest(oracle.states[states[from].key.sql].canonical.as_bytes()),"oracle_successor_sha256":oracle.failure.as_ref().and_then(|v|v.get("successor_sha256")),"trace_tail":tail,"trace_complete":states[cursor].parent.is_none()})
    );
}
// Bounded host diagnostics only; never a field in valuations, pools or certificate.
const DIAGNOSTIC_RECORDS: usize = 128;
const DIAGNOSTIC_RECORD_BYTES: usize = 8192;
struct RuntimeDiagnostics {
    started: Instant,
    last_progress: Instant,
    records: usize,
    candidates: Duration,
    successors: Duration,
    valuations: Duration,
    hashing: Duration,
}
impl RuntimeDiagnostics {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            started: now,
            last_progress: now,
            records: 0,
            candidates: Duration::ZERO,
            successors: Duration::ZERO,
            valuations: Duration::ZERO,
            hashing: Duration::ZERO,
        }
    }
    fn record(&mut self, phase: &str, facts: Value) -> Option<String> {
        if self.records >= DIAGNOSTIC_RECORDS {
            return None;
        }
        self.records += 1;
        let value =
            json!({"phase":phase,"elapsed_ms":self.started.elapsed().as_millis(),"facts":facts});
        let text = serde_json::to_string(&value).ok()?;
        if text.len() + "async_product_runtime=".len() < DIAGNOSTIC_RECORD_BYTES {
            Some(text)
        } else {
            Some(encoded(
                &json!({"phase":"diagnostic_record_omitted","reason":"record_byte_bound"}),
            ))
        }
    }
    fn emit(&mut self, phase: &str, facts: Value) {
        if let Some(text) = self.record(phase, facts) {
            // A closed diagnostic pipe must not change exploration.
            let _ = writeln!(std::io::stderr().lock(), "async_product_runtime={text}");
        }
    }
}
async fn close_product(
    driver: &Driver,
    driver_bytes: &[u8],
    runtime: &mut RuntimeDiagnostics,
) -> Result<Value, String> {
    runtime.emit("oracle_initialization", json!({}));
    let contexts = contexts();
    let mut oracle = Oracle::new(None).await?;
    runtime.emit(
        "oracle_initialized",
        json!({"store_states":oracle.states.len(),"store_edges":oracle.edges.len()}),
    );
    let semantic = semantic_bindings()?;
    let mut budget = Budget::default();
    budget.charge(
        "metadata",
        &metadata_envelope(&oracle, &semantic, driver_bytes, &contexts),
        0,
    )?;
    let mut pools = WirePools::default();
    budget.oracle(&oracle)?;
    let mut states = Vec::<ProductState>::new();
    let mut index = HashMap::<ProductKey, usize>::new();
    let mut edges = Vec::<Value>::new();
    let mut demands_out = Vec::<Value>::new();
    let mut enabled = Vec::<Value>::new();
    let mut descriptors = Vec::new();
    let mut by_context = serde_json::Map::new();
    for (context_id, context) in contexts.iter().enumerate() {
        let mut initial = Valuation {
            sql: oracle.initial(),
            w: [Worker::idle(), Worker::idle()],
            available: [true, true],
            completed: done(&oracle, oracle.initial()),
            mono_now_ms: 0,
            wall_anchors: vec![],
            mono_anchors: vec![],
            claim_loss_keys: vec![],
            real_drivers: [None, None],
        };
        retain_anchors(&mut initial, &oracle);
        let initial_key = pools.key(context_id, &initial, &mut budget)?;
        let initial_id = states.len();
        index.insert(initial_key, initial_id);
        let hash_started = Instant::now();
        let initial_hash = digest(canonical(&product_payload(context, &initial)).as_bytes());
        runtime.hashing = runtime.hashing.saturating_add(hash_started.elapsed());
        states.push(ProductState {
            key: initial_key,
            real_drivers: initial.real_drivers,
            hash: initial_hash,
            parent: None,
        });
        budget.charge("product_states", &states[initial_id].record(), 1)?;
        budget.charge("context_membership", &initial_id, 1)?;
        let mut queue = VecDeque::from([initial_id]);
        let mut context_states = vec![initial_id];
        let mut expanded = 0;
        let edge_start = edges.len();
        let demand_start = demands_out.len();
        macro_rules! progress {
            ($phase:expr, $active:expr) => {
                runtime.emit($phase, json!({"context":context.id,"expanded":expanded,
                    "context_states":context_states.len(),"pending":queue.len(),"active":$active,
                    "product_states":states.len(),"product_edges":edges.len(),"demands":demands_out.len(),
                    "store_states":oracle.states.len(),"store_edges":oracle.edges.len(),
                    "values":pools.values.len(),"workers":pools.workers.len(),
                    "budget_bytes":budget.bytes,"component_bytes":budget.component_bytes,"component_records":budget.component_records,
                    "oracle":oracle.diagnostic_summary(),"timing_scope":"cumulative_since_test_entry",
                    "timing_ms":{"new_state_hash":runtime.hashing.as_millis(),"candidates":runtime.candidates.as_millis(),"successors":runtime.successors.as_millis(),
                        "valuation_decode":runtime.valuations.as_millis(),"pool_value_inclusive":pools.value_elapsed.as_millis(),
                        "pool_worker_inclusive":pools.worker_elapsed.as_millis(),"pool_key_inclusive":pools.key_elapsed.as_millis(),
                        "record_accounting":budget.charge_elapsed.as_millis()}}));
            };
        }
        progress!("context_enter", 0);
        while let Some(from) = queue.pop_front() {
            if expanded == 0 || runtime.last_progress.elapsed() >= Duration::from_secs(30) {
                progress!("expansion", 1);
                runtime.last_progress = Instant::now();
            }
            let valuation_started = Instant::now();
            let value = states[from].valuation(&pools)?;
            runtime.valuations = runtime
                .valuations
                .saturating_add(valuation_started.elapsed());
            macro_rules! checked_output {
                ($operation:expr) => {
                    match $operation {
                        Ok(value) => value,
                        Err(reason) => {
                            cutoff(
                                Cutoff {
                                    context,
                                    from,
                                    states: &states,
                                    queue: &queue,
                                    expanded,
                                    oracle: &oracle,
                                    budget: &budget,
                                },
                                &reason,
                                None,
                            );
                            return Err(reason);
                        }
                    }
                };
            }
            macro_rules! charge_output {
                ($component:expr, $record:expr) => {
                    checked_output!(budget.charge($component, $record, 1))
                };
            }
            // Complete structural candidate collection precedes every oracle lookup.
            let candidate_started = Instant::now();
            let candidates = enumerate_candidates(context, &value, driver, &oracle)?;
            let classified = candidates
                .into_iter()
                .map(|spec| {
                    let exclusion = classify_demand(context, driver, &value, &spec, &oracle)?;
                    Ok((spec, exclusion))
                })
                .collect::<Result<Vec<_>, String>>()?;
            runtime.candidates = runtime
                .candidates
                .saturating_add(candidate_started.elapsed());
            let mut demands = Vec::new();
            for (spec, exclusion) in classified {
                let edge = if exclusion.is_none() {
                    let input = spec
                        .input
                        .as_ref()
                        .ok_or("MissingProductOracleEdge: missing eligible input")?;
                    match oracle.apply(spec.oracle_from, input).await {
                        Ok(id) => Some(id),
                        Err(reason) => {
                            cutoff(
                                Cutoff {
                                    context,
                                    from,
                                    states: &states,
                                    queue: &queue,
                                    expanded,
                                    oracle: &oracle,
                                    budget: &budget,
                                },
                                &reason,
                                Some(input),
                            );
                            return Err(reason);
                        }
                    }
                } else {
                    None
                };
                if let Err(reason) = budget.oracle(&oracle) {
                    cutoff(
                        Cutoff {
                            context,
                            from,
                            states: &states,
                            queue: &queue,
                            expanded,
                            oracle: &oracle,
                            budget: &budget,
                        },
                        &reason,
                        spec.input.as_ref(),
                    );
                    return Err(reason);
                }
                let branch_ref = checked_output!(pools.value(
                    &json!({"family":spec.family,"worker":spec.worker}),
                    &mut budget
                ));
                let input_ref = checked_output!(pools.value(&spec.input, &mut budget));
                let exclusion_ref = checked_output!(pools.value(&exclusion, &mut budget));
                let record = json!([
                    context_id,
                    from,
                    spec.oracle_from,
                    branch_ref,
                    spec.role,
                    input_ref,
                    edge,
                    exclusion_ref
                ]);
                charge_output!("demands", &record);
                demands_out.push(record);
                demands.push(Demand { spec, edge });
            }
            let renewal_dependencies = demands
                .iter()
                .filter_map(|d| {
                    if d.spec.family != "Environment" {
                        return None;
                    }
                    let id = d.edge?;
                    let edge = &oracle.edges[id];
                    let Action::Renew { job, token } = edge.action else {
                        return None;
                    };
                    let matching = value
                        .w
                        .iter()
                        .any(|w| same_claim(w, job, token, &oracle, value.sql));
                    let needs = edge.result == "refused"
                        || (edge.result == "renewed"
                            && value.w.iter().any(|w| {
                                same_claim(w, job, token, &oracle, value.sql) && !w.fault_used
                            }));
                    (matching && needs).then_some((edge.to, Action::Resolve { job, token }))
                })
                .collect::<Vec<_>>();
            let mut seen_dependencies = HashSet::new();
            for (oracle_from, input) in renewal_dependencies {
                if !seen_dependencies.insert((oracle_from, input.clone())) {
                    continue;
                }
                let spec = DemandSpec {
                    oracle_from,
                    family: "EnvironmentResolve",
                    worker: None,
                    role: "apply",
                    input: Some(input.clone()),
                    capture: None,
                    sample: None,
                };
                let exclusion = classify_demand(context, driver, &value, &spec, &oracle)?;
                require(exclusion.is_none(), "MissingRenewalResolutionInput")?;
                let id = match oracle.apply(oracle_from, &input).await {
                    Ok(id) => id,
                    Err(reason) => {
                        cutoff(
                            Cutoff {
                                context,
                                from,
                                states: &states,
                                queue: &queue,
                                expanded,
                                oracle: &oracle,
                                budget: &budget,
                            },
                            &reason,
                            Some(&input),
                        );
                        return Err(reason);
                    }
                };
                budget.oracle(&oracle)?;
                let branch_ref = checked_output!(
                    pools.value(&json!({"family":spec.family,"worker":null}), &mut budget)
                );
                let input_ref = checked_output!(pools.value(&input, &mut budget));
                let exclusion_ref = checked_output!(pools.value(&Value::Null, &mut budget));
                let record = json!([
                    context_id,
                    from,
                    oracle_from,
                    branch_ref,
                    "apply",
                    input_ref,
                    id,
                    exclusion_ref
                ]);
                charge_output!("demands", &record);
                demands_out.push(record);
                demands.push(Demand {
                    spec,
                    edge: Some(id),
                });
            }
            let recovery_dependencies = demands
                .iter()
                .filter_map(|d| {
                    if d.spec.family != "ClaimRecover" {
                        return None;
                    }
                    let edge = &oracle.edges[d.edge?];
                    let n = d.spec.worker?;
                    let job = value.w[n].job as usize;
                    match edge.result.as_str() {
                        "running" => {
                            Some((edge.to, n, "ClaimRecoverRead", Action::ReadClaimJob { job }))
                        }
                        "cancel_requested" => Some((
                            edge.to,
                            n,
                            "ClaimRecoverCancel",
                            Action::Settle {
                                job,
                                token: edge
                                    .resolved_token
                                    .expect("actual cancellation cleanup capture"),
                                disposition: 2,
                            },
                        )),
                        _ => None,
                    }
                })
                .collect::<Vec<_>>();
            for (oracle_from, n, family, input) in recovery_dependencies {
                let spec = DemandSpec {
                    oracle_from,
                    family,
                    worker: Some(n),
                    role: "apply",
                    input: Some(input.clone()),
                    capture: None,
                    sample: None,
                };
                let exclusion = classify_demand(context, driver, &value, &spec, &oracle)?;
                require(exclusion.is_none(), "MissingInitialRecoveryDependentInput")?;
                let id = match oracle.apply(oracle_from, &input).await {
                    Ok(id) => id,
                    Err(reason) => {
                        cutoff(
                            Cutoff {
                                context,
                                from,
                                states: &states,
                                queue: &queue,
                                expanded,
                                oracle: &oracle,
                                budget: &budget,
                            },
                            &reason,
                            Some(&input),
                        );
                        return Err(reason);
                    }
                };
                budget.oracle(&oracle)?;
                let branch_ref =
                    checked_output!(pools.value(&json!({"family":family,"worker":n}), &mut budget));
                let input_ref = checked_output!(pools.value(&input, &mut budget));
                let exclusion_ref = checked_output!(pools.value(&Value::Null, &mut budget));
                let record = json!([
                    context_id,
                    from,
                    oracle_from,
                    branch_ref,
                    "apply",
                    input_ref,
                    id,
                    exclusion_ref
                ]);
                charge_output!("demands", &record);
                demands_out.push(record);
                demands.push(Demand {
                    spec,
                    edge: Some(id),
                });
            }
            let successor_started = Instant::now();
            let steps = successors(context, &value, driver, &oracle, &demands)?;
            runtime.successors = runtime
                .successors
                .saturating_add(successor_started.elapsed());
            let mut planned = Vec::new();
            let mut pairs = BTreeMap::<usize, BTreeSet<String>>::new();
            let mut branch_pairs = HashSet::new();
            for step in steps {
                let key = checked_output!(pools.key(context_id, &step.value, &mut budget));
                let to = if let Some(to) = index.get(&key) {
                    *to
                } else {
                    let id = states.len();
                    let hash_started = Instant::now();
                    let hash = digest(canonical(&product_payload(context, &step.value)).as_bytes());
                    runtime.hashing = runtime.hashing.saturating_add(hash_started.elapsed());
                    let state = ProductState {
                        key,
                        real_drivers: step.value.real_drivers,
                        hash,
                        parent: Some((from, step.branch.clone())),
                    };
                    charge_output!("product_states", &state.record());
                    charge_output!("context_membership", &id);
                    index.insert(key, id);
                    states.push(state);
                    queue.push_back(id);
                    context_states.push(id);
                    id
                };
                require(
                    branch_pairs.insert((canonical(&step.branch), to)),
                    "ProductSuccessors: duplicate finite branch/target",
                )?;
                pairs
                    .entry(to)
                    .or_default()
                    .extend(memberships(&step, &value, context, &oracle));
                planned.push((step, to));
            }
            let mut actions = BTreeSet::new();
            for (step, to) in planned {
                let fair: Vec<_> = pairs[&to].iter().cloned().collect();
                if from != to {
                    actions.extend(fair.iter().cloned());
                }
                let branch_ref = checked_output!(pools.value(&step.branch, &mut budget));
                let fair_ref = checked_output!(pools.value(&fair, &mut budget));
                let record = json!([
                    from,
                    to,
                    branch_ref,
                    step.edge,
                    step.resolution_edge,
                    step.read_edge,
                    step.cleanup_edge,
                    fair_ref
                ]);
                charge_output!("product_edges", &record);
                edges.push(record);
            }
            let actions_ref = checked_output!(pools.value(&actions, &mut budget));
            let row = json!([from, actions_ref]);
            charge_output!("enabledness", &row);
            enabled.push(row);
            expanded += 1;
        }
        progress!("context_exit", 0);
        by_context.insert(context.id.into(),json!({"state_count":context_states.len(),"edge_count":edges.len()-edge_start,"demand_count":demands_out.len()-demand_start,"frontier_count":0}));
        descriptors.push(json!({"id":context.id,"model":"AsyncDriverRefinement","stable_success":context.stable,"mutation":context.mutation,"witness":false,"initial":initial_id,"states":context_states}));
    }
    for (id, mutation) in [
        ("boundary_local_revocation", "local-revocation"),
        ("boundary_unknown_rollback", "unknown-as-rollback"),
    ] {
        descriptors.push(json!({"id":id,"model":"BoundaryControls","stable_success":false,"mutation":mutation,"witness":false,"initial":null,"states":[]}));
    }
    runtime.emit("closure_complete", json!({"oracle":oracle.diagnostic_summary(),"product_states":states.len(),"product_edges":edges.len(),"demands":demands_out.len()}));
    runtime.emit("assembly_start", json!({}));
    let assembly_started = Instant::now();
    let coverage = json!({"complete":true,"no_skipped_demands":true,"store_state_count":oracle.states.len(),"store_edge_count":oracle.edges.len(),"product_state_count":states.len(),"product_edge_count":edges.len(),"demand_count":demands_out.len(),"by_context":by_context});
    // Release traversal-only lookup/typed caches before assembling the wire.
    drop(index);
    drop(pools.value_index);
    drop(pools.worker_index);
    drop(pools.worker_values);
    let state_rows = states
        .into_iter()
        .map(|state| state.record())
        .collect::<Vec<_>>();
    let store_rows = (0..oracle.states.len())
        .map(|id| oracle.export_product_state(id))
        .collect::<Vec<_>>();
    let mut bundle = metadata_envelope(&oracle, &semantic, driver_bytes, &contexts);
    // Move already encoded rows rather than cloning all output through json!.
    bundle["store"]["states"] = Value::Array(store_rows);
    bundle["store"]["edges"] = serde_json::to_value(std::mem::take(&mut oracle.edges))
        .map_err(|e| format!("ProductWire: {e}"))?;
    bundle["contexts"] = Value::Array(descriptors);
    bundle["value_pool"] = Value::Array(pools.values);
    bundle["worker_pool"] = Value::Array(pools.workers);
    bundle["product_states"] = Value::Array(state_rows);
    bundle["product_edges"] = Value::Array(edges);
    bundle["demands"] = Value::Array(demands_out);
    bundle["enabledness"] = Value::Array(enabled);
    bundle["coverage"] = coverage;
    drop(oracle);
    runtime.emit(
        "assembly_complete",
        json!({"duration_ms":assembly_started.elapsed().as_millis()}),
    );
    runtime.emit("exact_size_start", json!({}));
    let size_started = Instant::now();
    let exact_bytes = serialized_size(&bundle, BUNDLE_BYTES)?;
    runtime.emit(
        "exact_size_complete",
        json!({"duration_ms":size_started.elapsed().as_millis(),"exact_bytes":exact_bytes}),
    );
    require(
        exact_bytes <= budget.bytes,
        "ProductResourceEstimate: compact accounting omitted output bytes",
    )?;
    eprintln!(
        "async_product_wire={}",
        json!({"exact_bytes":exact_bytes,"conservative_bytes":budget.bytes,"component_bytes":budget.component_bytes,"component_records":budget.component_records})
    );
    Ok(bundle)
}

fn driver_input() -> Result<(Driver, Vec<u8>), String> {
    let path = std::env::var("PLURX_ASYNC_DRIVER_GRAPH")
        .map_err(|_| "DriverInventory: PLURX_ASYNC_DRIVER_GRAPH required")?;
    let file = std::fs::File::open(path).map_err(|e| format!("DriverInventory: {e}"))?;
    let mut bytes = Vec::new();
    file.take(BUNDLE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("DriverInventory: {e}"))?;
    require(
        bytes.len() <= BUNDLE_BYTES,
        "DriverInventory: graph exceeds member bound",
    )?;
    Ok((Driver::parse(&bytes)?, bytes))
}
#[tokio::test]
#[ignore = "requires source-bound driver input; run scripts/check-async-driver-refinement"]
async fn actual_sql_two_worker_product_is_closed() {
    let mut runtime = RuntimeDiagnostics::new();
    runtime.emit("test_enter", json!({}));
    let (driver, bytes) = driver_input().expect("source-bound driver inventory");
    runtime.emit("driver_loaded", json!({"driver_bytes":bytes.len()}));
    let bundle = close_product(&driver, &bytes, &mut runtime)
        .await
        .expect("exact selected two-worker product closure");
    let path =
        std::env::var("PLURX_ASYNC_PRODUCT_BUNDLE").expect("PLURX_ASYNC_PRODUCT_BUNDLE required");
    let path = Path::new(&path);
    runtime.emit("write_start", json!({}));
    let write_started = Instant::now();
    let partial = path.with_extension("json.partial");
    let file = std::fs::File::create(&partial).expect("create incomplete product output");
    let mut writer = ByteLimit {
        inner: std::io::BufWriter::new(file),
        bytes: 0,
        limit: BUNDLE_BYTES,
    };
    serde_json::to_writer(&mut writer, &bundle).expect("bounded complete product serialization");
    writer.flush().expect("flush complete product bundle");
    drop(writer);
    std::fs::rename(&partial, path).expect("publish complete product bundle");
    runtime.emit(
        "write_complete",
        json!({"duration_ms":write_started.elapsed().as_millis()}),
    );
    eprintln!("async_product_coverage={}", bundle["coverage"]);
}

fn wire_fixture_valuation() -> Valuation {
    Valuation {
        sql: 0,
        w: [Worker::idle(), Worker::idle()],
        available: [true, true],
        completed: vec![],
        mono_now_ms: 0,
        wall_anchors: vec![1_000],
        mono_anchors: vec![],
        claim_loss_keys: vec![],
        real_drivers: [None, None],
    }
}

#[test]
fn product_wire_pools_preserve_exact_types_order_and_complex_worker_values() {
    let mut pools = WirePools::default();
    let mut budget = Budget::default();
    let values = [
        json!(null),
        json!(false),
        json!(0),
        json!(true),
        json!(1),
        json!(29_999),
        json!(30_000),
        json!("raw-job-λ"),
        json!([true, 0]),
        json!([0, true]),
        json!({"b":2,"a":1}),
    ];
    let mut ids = HashSet::new();
    for value in &values {
        let id = pools
            .value(value, &mut budget)
            .expect("valid source-bound wire fixture");
        assert!(ids.insert(id));
        assert_eq!(&pools.values[id], value);
        let bytes = budget.bytes;
        assert_eq!(
            pools
                .value(value, &mut budget)
                .expect("valid source-bound wire fixture"),
            id
        );
        assert_eq!(
            budget.bytes, bytes,
            "equal values must not be charged twice"
        );
    }
    assert_eq!(
        pools
            .value(&json!({"a":1,"b":2}), &mut budget)
            .expect("valid source-bound wire fixture"),
        pools
            .value(&values[10], &mut budget)
            .expect("valid source-bound wire fixture")
    );
    let mut worker = Worker::idle();
    worker.claim_snapshot = Some(sql::ClaimSnapshot {
        job: 0,
        raw_job_id: "00000000-0000-4000-8000-000000000001".into(),
        failed_attempts: 3,
        claim_id: "00000000-0000-4000-8000-000000000010".into(),
        token_capture: super::background_jobs::JobToken {
            job_id: "00000000-0000-4000-8000-000000000001".into(),
            node_id: "node-a".into(),
            boot_id: "00000000-0000-4000-8000-000000000002".into(),
            claim_id: "00000000-0000-4000-8000-000000000010".into(),
            fence: 1,
            revision: 2,
            lease_expires_ms: 30_000,
        },
    });
    worker.claim_request = Some(json!({"raw_job_id":"job-λ","now_ms":29_999,"deadline_ms":30_000}));
    worker.claim_recovery = ClaimRecovery {
        phase: "waiting".into(),
        wake_mono_ms: Some(29_999),
    };
    worker.driver_settlement = Some(SavedSettlement {
        kind: "retry".into(),
        not_before_ms: 30_000,
    });
    worker.adapter_reply = Some(AdapterReply {
        tag: "WorkFailed".into(),
        now_ms: Some(29_999),
        retry_delay_ms: Some(1),
    });
    worker.ordinary.payload = Some(OrdinaryPayload {
        kind: "retry".into(),
        not_before_ms: 30_000,
        error_code: Some("raw-error".into()),
        checkpoint: Some(json!({"b":true,"a":0})),
    });
    worker.ordinary.captured_token = Some(1);
    worker.ordinary.request_now_ms = Some(29_999);
    for worker in [Worker::idle(), worker] {
        let id = pools
            .worker(&worker, &mut budget)
            .expect("valid source-bound wire fixture");
        let row = pools.workers[id]
            .as_array()
            .expect("valid source-bound wire fixture");
        let decoded = WORKER_COLUMNS
            .iter()
            .zip(row)
            .map(|(column, cell)| {
                let value = if WORKER_VALUE_REFS.contains(column) {
                    pools.values[cell.as_u64().expect("valid source-bound wire fixture") as usize]
                        .clone()
                } else {
                    cell.clone()
                };
                ((*column).into(), value)
            })
            .collect::<serde_json::Map<_, _>>();
        assert_eq!(
            Value::Object(decoded),
            serde_json::to_value(&worker).expect("valid source-bound wire fixture")
        );
        assert_eq!(row.len(), 37);
        assert_eq!(
            pools
                .worker(&worker, &mut budget)
                .expect("valid source-bound wire fixture"),
            id
        );
    }
}

#[test]
fn product_wire_compact_keys_are_bijective_with_full_logical_payloads() {
    let mut pools = WirePools::default();
    let mut budget = Budget::default();
    let original = wire_fixture_valuation();
    let mut fixtures = vec![(0, original.clone()), (1, original.clone())];
    let mut changed = original.clone();
    changed.sql = 1;
    fixtures.push((0, changed));
    let mut changed = original.clone();
    changed.w[0].job = 1;
    fixtures.push((0, changed));
    let mut changed = original.clone();
    changed.w[1].attempt = "raw-attempt".into();
    fixtures.push((0, changed));
    let mut changed = original.clone();
    changed.available[0] = false;
    fixtures.push((0, changed));
    let mut changed = original.clone();
    changed.completed.push(0);
    fixtures.push((0, changed));
    let mut changed = original.clone();
    changed.mono_now_ms = 1;
    fixtures.push((0, changed));
    let mut changed = original.clone();
    changed.wall_anchors.push(1_001);
    fixtures.push((0, changed));
    let mut changed = original.clone();
    changed.mono_anchors.push(29_999);
    fixtures.push((0, changed));
    let mut changed = original.clone();
    changed.claim_loss_keys.push(ClaimLossKey {
        job: 0,
        node: 0,
        claim_id: "raw-claim".into(),
    });
    fixtures.push((0, changed));
    fixtures.push((0, original.clone()));
    let contexts = contexts();
    let mut checked = Vec::new();
    for (context, value) in fixtures {
        let logical = canonical(&product_payload(&contexts[context], &value));
        let key = pools
            .key(context, &value, &mut budget)
            .expect("valid source-bound wire fixture");
        let state = ProductState {
            key,
            real_drivers: value.real_drivers,
            hash: digest(logical.as_bytes()),
            parent: None,
        };
        let restored = state
            .valuation(&pools)
            .expect("valid source-bound wire fixture");
        assert_eq!(
            canonical(&product_payload(&contexts[context], &restored)),
            logical
        );
        assert_eq!(state.record()[10], json!(digest(logical.as_bytes())));
        for (previous_key, previous_logical) in &checked {
            assert_eq!(*previous_key == key, previous_logical == &logical);
        }
        checked.push((key, logical));
    }
    let mut live = original;
    live.real_drivers[0] = Some(RealDriver::new());
    let key = pools
        .key(0, &live, &mut budget)
        .expect("valid source-bound wire fixture");
    let state = ProductState {
        key,
        real_drivers: live.real_drivers,
        hash: String::new(),
        parent: None,
    };
    let mut restored = state
        .valuation(&pools)
        .expect("valid source-bound wire fixture");
    assert_eq!(
        format!("{:?}", restored.real_drivers),
        format!("{:?}", live.real_drivers)
    );
    restored.w[0].stage = "private mutation".into();
    assert_eq!(
        state
            .valuation(&pools)
            .expect("valid source-bound wire fixture")
            .w,
        live.w,
        "transient mutation must not change shared source values"
    );
}

#[test]
fn product_wire_byte_accounting_and_stream_writer_fail_closed() {
    let value = json!({"raw":"λ","values":[null,true,0,29_999,30_000]});
    let exact = encoded(&value).len();
    assert_eq!(
        serialized_size(&value, exact).expect("valid source-bound wire fixture"),
        exact
    );
    assert!(serialized_size(&value, exact - 1).is_err());
    let mut writer = ByteLimit {
        inner: Vec::new(),
        bytes: 0,
        limit: exact,
    };
    serde_json::to_writer(&mut writer, &value).expect("valid source-bound wire fixture");
    assert_eq!(writer.bytes, exact);
    assert_eq!(writer.inner, encoded(&value).as_bytes());
    assert!(writer.write_all(b" ").is_err());
    let mut overflow = ByteLimit {
        inner: std::io::sink(),
        bytes: usize::MAX,
        limit: usize::MAX,
    };
    assert!(overflow.write_all(b"x").is_err());
    let mut budget = Budget::default();
    budget
        .charge("pool", &value, 1)
        .expect("valid source-bound wire fixture");
    assert_eq!(budget.bytes, exact + 1);
    assert_eq!(budget.component_bytes.values().sum::<usize>(), budget.bytes);
    budget.bytes = BUNDLE_BYTES;
    assert!(budget.charge("row", &0, 1).is_err());
    budget.bytes = usize::MAX;
    assert!(budget.charge("row", &0, 1).is_err());
}

#[tokio::test]
async fn product_wire_store_export_omits_only_redundant_raw() {
    let oracle = Oracle::new(None)
        .await
        .expect("valid source-bound wire fixture");
    for (id, mut logical) in oracle.export_states().into_iter().enumerate() {
        let raw = logical
            .as_object_mut()
            .expect("valid source-bound wire fixture")
            .remove("raw")
            .expect("valid source-bound wire fixture");
        let wire = oracle.export_product_state(id);
        assert_eq!(wire, logical);
        let canonical = wire["canonical_json"]
            .as_str()
            .expect("valid source-bound wire fixture");
        assert_eq!(
            serde_json::from_str::<Value>(canonical).expect("valid source-bound wire fixture"),
            raw
        );
        assert_eq!(
            wire["canonical_sha256"],
            json!(digest(canonical.as_bytes()))
        );
        assert_eq!(canonical, oracle.states[id].canonical);
    }
}

struct NormalPrimitive {
    input: Action,
    result: String,
    before: String,
    after: String,
}
async fn protocol_prefix(
    driver: &Driver,
    normal: &mut Oracle,
    control: &str,
) -> Result<(Vec<Value>, Vec<NormalPrimitive>), String> {
    let mut prefix = Vec::new();
    let mut primitives = Vec::new();
    let mut handle = normal.initial();
    // One available idle worker takes its one admission permit and selects the
    // queued, unfinished J0. The other worker stays idle throughout this finite
    // normal-protocol witness; no interference or expired sample is assumed.
    require(
        normal.states[handle].view["jobs"][0]["state"] == "queued"
            && normal.states[handle].view["jobs"][0]["request_done"] == false,
        "CausalPrefix: admission/selection precondition",
    )?;
    let claim = Action::Claim { job: 0, node: 0 };
    let before = normal.states[handle].canonical.clone();
    let id = normal.apply(handle, &claim).await?;
    let edge = normal.edges[id].clone();
    require(
        edge.result == "claimed",
        "CausalPrefix: real claim must apply",
    )?;
    handle = edge.to;
    primitives.push(NormalPrimitive {
        input: claim.clone(),
        result: edge.result.clone(),
        before,
        after: normal.states[handle].canonical.clone(),
    });
    prefix.push(json!({"driver_from":null,"request":"Claim","reply":"Claimed","driver_to":null,"primitive":claim,"normal_result":edge.result}));
    let claim_snapshot = edge
        .claim_snapshot
        .clone()
        .ok_or("CausalPrefix: missing actual claim snapshot")?;
    prefix[0]["claim_snapshot"] = json!(claim_snapshot);
    prefix[0]["adapter_reply"] = Value::Null;
    prefix[0]["driver_settlement"] = Value::Null;
    let mut actual = RealDriver::new();
    let mut ctrl = driver.initial;
    let mut joined = false;
    let mut publisher = false;
    let mut domain = false;
    let mut work_started = false;
    for _ in 0..31 {
        let pending = driver.states[ctrl]["pending"]
            .as_str()
            .ok_or("CausalPrefix: premature terminal")?;
        require(
            driver.states[ctrl]["held"]["admission"] == true
                && driver.states[ctrl]["held"]["queue"] == true,
            "CausalPrefix: actual driver retains admission and queue through target",
        )?;
        require(
            actual
                .request()
                .is_some_and(|request| format!("{:?}", request.kind()) == pending),
            "CausalPrefix: actual Driver request",
        )?;
        let token = normal.states[handle].episode.tokens[0].len() - 1;
        let (input, reply, target) = match pending {
            "AcquireDomain" => (
                Some(Action::AcquireDomain { node: 0 }),
                "DomainAcquired",
                false,
            ),
            "Bind" => {
                require(domain, "CausalPrefix: bind owns domain")?;
                (
                    Some(Action::Bind {
                        job: 0,
                        token,
                        lease: 0,
                    }),
                    "BindAccepted",
                    false,
                )
            }
            "PollWork" => {
                require(
                    driver.states[ctrl]["held"]["publisher"] == true
                        && driver.states[ctrl]["held"]["domain"] == true
                        && driver.states[ctrl]["revoked"] == false
                        && driver.states[ctrl]["cancelled"] == false,
                    "CausalPrefix: actual driver publication authority is fresh",
                )?;
                require(
                    domain && publisher && work_started,
                    "CausalPrefix: publication ownership",
                )?;
                if control == "C4" {
                    (
                        Some(Action::Complete {
                            job: 0,
                            token,
                            lease: 0,
                        }),
                        "WorkSucceeded",
                        true,
                    )
                } else {
                    (None, "Pressure", false)
                }
            }
            "JoinWork" => {
                require(work_started, "CausalPrefix: join same started work")?;
                joined = true;
                (None, "JoinedFailed", false)
            }
            "RevokeDomain" | "CancelWork" => (None, "Done", false),
            "DropPublisher" => {
                require(
                    joined && publisher,
                    "CausalPrefix: join before publisher drop",
                )?;
                publisher = false;
                (None, "Done", false)
            }
            "ReleaseRetry" | "ReleaseInterrupted" | "ReleaseNone" | "ReleaseRefused" => {
                require(
                    joined && !publisher && domain,
                    "CausalPrefix: joined before actual release",
                )?;
                (
                    Some(Action::ReleaseDomain { lease: 0 }),
                    "ReleaseReturned",
                    false,
                )
            }
            "SettleYield" => {
                require(
                    joined && !publisher && !domain,
                    "CausalPrefix: settlement after join/release",
                )?;
                (
                    Some({
                        let Some(plurx_worker_control::library_driver::Request::Settle(settlement)) =
                            actual.request()
                        else {
                            return Err("CausalPrefix: missing concrete settlement".into());
                        };
                        let payload = OrdinaryPayload::from(SavedSettlement::from(settlement));
                        Action::OrdinarySettle {
                            job: 0,
                            token,
                            kind: payload.kind,
                            not_before_ms: payload.not_before_ms,
                            request_now_ms: normal.states[handle].episode.now,
                            error_code: payload.error_code,
                            checkpoint: payload.checkpoint,
                        }
                    }),
                    "Done",
                    control == "C9",
                )
            }
            _ => return Err(format!("CausalPrefix: unexpected normal request {pending}")),
        };
        let result = if let Some(input) = &input {
            let before = normal.states[handle].canonical.clone();
            let id = normal.apply(handle, input).await?;
            let edge = normal.edges[id].clone();
            let expected = if pending == "AcquireDomain" {
                "acquired"
            } else {
                "accepted"
            };
            require(edge.result == expected, "CausalPrefix: normal API outcome")?;
            handle = edge.to;
            primitives.push(NormalPrimitive {
                input: input.clone(),
                result: edge.result.clone(),
                before,
                after: normal.states[handle].canonical.clone(),
            });
            Some(edge.result)
        } else {
            None
        };
        let to = *driver
            .replies
            .get(&(ctrl, reply.into()))
            .ok_or("CausalPrefix: actual driver rejected reply")?;
        let adapter = AdapterReply {
            tag: reply.into(),
            now_ms: match reply {
                "DomainAcquired" => Some(0),
                "DomainRefused" => Some(normal.states[handle].episode.now),
                "ReleaseReturned" => Some(if pending == "ReleaseNone" {
                    0
                } else {
                    normal.states[handle].episode.now
                }),
                _ => None,
            },
            retry_delay_ms: match reply {
                "DomainAcquired" | "DomainRefused" => Some(0),
                "ReleaseReturned" => Some(if pending == "ReleaseRetry" {
                    plurx_worker_control::retry_policy::retry_delay_ms(
                        &claim_snapshot.raw_job_id,
                        claim_snapshot.failed_attempts,
                    )
                } else {
                    0
                }),
                _ => None,
            },
        };
        actual
            .drive(concrete_reply(&adapter)?)
            .map_err(|_| "CausalPrefix: concrete Driver reply rejected")?;
        verify_control(actual, &driver.states[to])?;
        prefix.push(json!({"driver_from":ctrl,"request":pending,"reply":reply,"driver_to":to,"primitive":input,"normal_result":result,"adapter_reply":adapter,"driver_settlement":actual.settlement().map(SavedSettlement::from),"claim_snapshot":null}));
        match pending {
            "AcquireDomain" => domain = true,
            "Bind" => {
                publisher = true;
                work_started = true;
            }
            p if p.starts_with("Release") => domain = false,
            _ => {}
        }
        ctrl = to;
        if target {
            return Ok((prefix, primitives));
        }
    }
    Err("CausalPrefix: bounded normal witness did not reach target primitive".into())
}
async fn causal_control(
    control: &str,
    mutation: TriggerMutation,
    named: &str,
) -> Result<Value, String> {
    let (driver, driver_bytes) = driver_input()?;
    let mut normal = Oracle::new(None).await?;
    let expected: Vec<Value> = super::sqlite::MIGRATIONS
        .iter()
        .enumerate()
        .map(|(index, text)| json!({"version":index+1,"sha256":digest(text.as_bytes())}))
        .collect();
    require(
        normal.schema["compiled_sqlite_migrations"] == json!(expected),
        "CausalPrefix: unmutated compiled migration binding",
    )?;
    let (prefix, primitives) = protocol_prefix(&driver, &mut normal, control).await?;
    require(
        prefix.len() <= 32 && primitives.len() <= 32,
        "CausalPrefix: trace bound",
    )?;
    let original_schema = normal.schema.clone();
    let fixture_identity = normal.fixture_identity();
    let source_bindings = normal.source_bindings();
    normal.begin_paired_mutation(mutation).await?;
    let mut mutant = normal;
    require(
        original_schema != mutant.schema,
        "CausalPrefix: source trigger mutation must change actual installed schema",
    )?;
    let mut handle = mutant.initial();
    let mut mutated_prefix = Vec::new();
    let mut final_failure = None;
    for (step, primitive) in primitives.iter().enumerate() {
        let before = mutant.states[handle].canonical.clone();
        require(
            before == primitive.before,
            "CausalPrefix: mutated predecessor diverged before causal primitive",
        )?;
        match mutant.apply(handle, &primitive.input).await {
            Ok(id) => {
                let edge = &mutant.edges[id];
                require(
                    edge.result == primitive.result,
                    "CausalPrefix: changed prior API outcome",
                )?;
                handle = edge.to;
                let after = mutant.states[handle].canonical.clone();
                require(
                    after == primitive.after,
                    "CausalPrefix: mutated rows diverged before named violation",
                )?;
                mutated_prefix.push(json!({"primitive":primitive.input,"result":edge.result,"from_sha256":digest(before.as_bytes()),"to_sha256":digest(after.as_bytes()),"invariant":null}));
            }
            Err(failure) => {
                require(
                    step + 1 == primitives.len() && failure.starts_with(named),
                    "CausalPrefix: first failure must be the final named causal violation",
                )?;
                let observation = mutant
                    .failure
                    .clone()
                    .ok_or("CausalPrefix: missing actual causal observation")?;
                require(
                    observation["result"] == primitive.result,
                    "CausalPrefix: causal API changed its outcome",
                )?;
                let after = observation["successor_canonical_json"]
                    .as_str()
                    .ok_or("CausalPrefix: successor canonical identity")?;
                require(
                    observation["predecessor_canonical_json"] == before,
                    "CausalPrefix: causal predecessor identity",
                )?;
                mutated_prefix.push(json!({"primitive":primitive.input,"result":observation["result"],"from_sha256":digest(before.as_bytes()),"to_sha256":digest(after.as_bytes()),"invariant":named}));
                final_failure = Some(observation);
                break;
            }
        }
    }
    let failure = final_failure.ok_or("CausalPrefix: mutant did not violate named invariant")?;
    let last = primitives.last().expect("nonempty normal prefix");
    let record = json!({"schema_version":1,"scope":"bounded_actual_settlement_product","ordinary_settlement_payload_transfer":"pending","fixture_identity":fixture_identity,"control":control,"named_failure":named,"driver_graph_sha256":digest(&driver_bytes),"source_bindings":source_bindings,"schema_bindings":{"unmutated":digest(canonical(&original_schema).as_bytes()),"mutated":digest(canonical(&mutant.schema).as_bytes())},"mutation":mutant.mutation,"unmutated_prefix_replay":true,"prefix":prefix,"mutated_prefix":mutated_prefix,"identities":{"normal_predecessor":primitives.first().expect("prefix").before,"normal_successor":last.after,"mutated_predecessor":mutant.states[0].canonical,"mutated_successor":failure["successor_canonical_json"]},"first_causal_violation":{"step":primitives.len()-1,"name":named}});
    require(
        encoded(&record).len() <= 128 * 1024,
        "CausalPrefix: marker exceeds 128KiB; incomplete",
    )?;
    Ok(record)
}
#[tokio::test]
#[ignore = "requires source-bound driver input; run scripts/check-async-driver-refinement"]
async fn product_prefix_completion_mutation_fails_last_request_atomic_retirement() {
    let record = causal_control(
        "C4",
        TriggerMutation::CompletionRetirement,
        "LastRequestAtomicRetirement",
    )
    .await
    .expect("C4 paired-seed actual-SQL and concrete-Driver causal prefix");
    eprintln!("async_store_causal_prefix={record}");
    eprintln!(
        "async_store_control={}",
        json!({"control":"C4","named_failure":"LastRequestAtomicRetirement"})
    );
}
#[tokio::test]
#[ignore = "requires source-bound driver input; run scripts/check-async-driver-refinement"]
async fn product_prefix_reservation_mutation_fails_reservation_conservation() {
    let record = causal_control(
        "C9",
        TriggerMutation::ReservationRelease,
        "ReservationConservation",
    )
    .await
    .expect("C9 paired-seed actual +1000 Yield and captured request-time causal prefix");
    eprintln!("async_store_causal_prefix={record}");
    eprintln!(
        "async_store_control={}",
        json!({"control":"C9","named_failure":"ReservationConservation"})
    );
}

// These witnesses exercise the real Driver/lifecycle, shared raw-ID policy and
// actual Store API. The separate daemon guard conformance calls JobFence itself;
// the frontier checks below do not claim to execute Tokio scheduling.
async fn actual_payload_setup(
    route: &str,
) -> Result<(Oracle, StoreHandle, OrdinaryPayload, sql::ClaimSnapshot), String> {
    let mut oracle = Oracle::new(None).await?;
    let id = oracle.apply(0, &Action::Claim { job: 0, node: 0 }).await?;
    require(oracle.edges[id].result == "claimed", "PayloadWitnessClaim")?;
    let snapshot = oracle.edges[id]
        .claim_snapshot
        .clone()
        .ok_or("PayloadWitnessSnapshot")?;
    let mut handle = oracle.edges[id].to;
    require(
        oracle.fixture_identity()["jobs"][0]["raw_job_id"] == snapshot.raw_job_id,
        "PayloadWitnessRawIdentity",
    )?;
    let mut real = RealDriver::new();
    let now = oracle.states[handle].episode.now;
    if route == "refused" {
        let foreign = oracle
            .apply(handle, &Action::AcquireDomain { node: 2 })
            .await?;
        require(
            oracle.edges[foreign].result == "acquired",
            "PayloadWitnessForeignAuthority",
        )?;
        handle = oracle.edges[foreign].to;
        let refused = oracle
            .apply(handle, &Action::AcquireDomain { node: 0 })
            .await?;
        require(
            oracle.edges[refused].result == "held",
            "PayloadWitnessActualDomainRefusal",
        )?;
        handle = oracle.edges[refused].to;
        real.drive(RealReply::Domain {
            acquired: false,
            now_ms: now,
        })
        .map_err(|_| "PayloadWitnessDriver")?;
    } else {
        let acquired = oracle
            .apply(handle, &Action::AcquireDomain { node: 0 })
            .await?;
        require(
            oracle.edges[acquired].result == "acquired",
            "PayloadWitnessAcquire",
        )?;
        handle = oracle.edges[acquired].to;
        real.drive(RealReply::Domain {
            acquired: true,
            now_ms: 0,
        })
        .map_err(|_| "PayloadWitnessDriver")?;
        let bind = oracle
            .apply(
                handle,
                &Action::Bind {
                    job: 0,
                    token: 0,
                    lease: 0,
                },
            )
            .await?;
        require(
            oracle.edges[bind].result == "accepted",
            "PayloadWitnessBind",
        )?;
        handle = oracle.edges[bind].to;
        real.drive(RealReply::Bound { accepted: true })
            .map_err(|_| "PayloadWitnessDriver")?;
        real.drive(RealReply::Work(if route == "retry" {
            WorkOutcome::Completed { ok: false }
        } else {
            WorkOutcome::Pressure
        }))
        .map_err(|_| "PayloadWitnessDriver")?;
        for _ in 0..6 {
            match real.request().ok_or("PayloadWitnessPrematureTerminal")? {
                plurx_worker_control::library_driver::Request::RevokeDomain
                | plurx_worker_control::library_driver::Request::CancelWork
                | plurx_worker_control::library_driver::Request::DropPublisher => {
                    real.drive(RealReply::Done)
                        .map_err(|_| "PayloadWitnessDriver")?;
                }
                plurx_worker_control::library_driver::Request::JoinWork => {
                    real.drive(RealReply::Joined { ok: false })
                        .map_err(|_| "PayloadWitnessDriver")?;
                }
                plurx_worker_control::library_driver::Request::ReleaseDomain { settlement } => {
                    let release = oracle
                        .apply(handle, &Action::ReleaseDomain { lease: 0 })
                        .await?;
                    require(
                        oracle.edges[release].result == "accepted",
                        "PayloadWitnessRelease",
                    )?;
                    handle = oracle.edges[release].to;
                    let retry = if settlement
                        == plurx_worker_control::library_lifecycle::SettlementKind::Retry
                    {
                        plurx_worker_control::retry_policy::retry_delay_ms(
                            &snapshot.raw_job_id,
                            snapshot.failed_attempts,
                        )
                    } else {
                        0
                    };
                    real.drive(RealReply::Released {
                        now_ms: now,
                        retry_delay_ms: retry,
                    })
                    .map_err(|_| "PayloadWitnessDriver")?;
                    break;
                }
                _ => return Err("PayloadWitnessUnexpectedEffect".into()),
            }
        }
    }
    let Some(plurx_worker_control::library_driver::Request::Settle(saved)) = real.request() else {
        return Err("PayloadWitnessMissingSettlement".into());
    };
    require(real.settlement() == Some(saved), "PayloadWitnessGetter")?;
    let payload = OrdinaryPayload::from(SavedSettlement::from(saved));
    let delay = match route {
        "refused" => 5000,
        "interrupted" => 1000,
        "retry" => plurx_worker_control::retry_policy::retry_delay_ms(
            &snapshot.raw_job_id,
            snapshot.failed_attempts,
        ),
        _ => return Err("PayloadWitnessRoute".into()),
    };
    require(
        payload.not_before_ms == now.saturating_add(delay),
        "PayloadWitnessActualDeadline",
    )?;
    require(
        payload.error_code.as_deref()
            == if route == "retry" {
                Some("library_execution_failed")
            } else {
                None
            },
        "PayloadWitnessErrorText",
    )?;
    Ok((oracle, handle, payload, snapshot))
}
fn captured_ordinary(payload: &OrdinaryPayload, request_now_ms: i64) -> Action {
    Action::OrdinarySettle {
        job: 0,
        token: 0,
        kind: payload.kind.clone(),
        not_before_ms: payload.not_before_ms,
        request_now_ms,
        error_code: payload.error_code.clone(),
        checkpoint: payload.checkpoint.clone(),
    }
}
#[tokio::test]
#[ignore = "requires source-bound driver input; run scripts/check-async-driver-refinement"]
async fn actual_ordinary_settlement_payload_and_time_conformance() {
    for route in ["refused", "interrupted", "retry"] {
        let (mut oracle, handle, payload, snapshot) = actual_payload_setup(route)
            .await
            .expect("actual source payload path");
        let request_now = oracle.states[handle].episode.now;
        let wire = serde_json::to_value(captured_ordinary(&payload, request_now))
            .expect("complete ordinary input wire");
        for required in ["error_code", "checkpoint"] {
            let mut missing = wire.clone();
            missing.as_object_mut().expect("action").remove(required);
            assert!(
                serde_json::from_value::<Action>(missing).is_err(),
                "nullable ordinary fields remain mandatory"
            );
        }
        let mut extra = wire;
        extra["unknown"] = json!(true);
        assert!(serde_json::from_value::<Action>(extra).is_err());
        let accepted = oracle
            .apply(handle, &captured_ordinary(&payload, request_now))
            .await
            .expect("actual source payload settlement");
        assert_eq!(oracle.edges[accepted].result, "accepted");
        assert_eq!(
            oracle.states[oracle.edges[accepted].to].view["jobs"][0]["not_before_ms"],
            payload.not_before_ms
        );
        let late = oracle
            .apply(
                handle,
                &Action::ClockAdvance {
                    now: payload.not_before_ms + 1,
                },
            )
            .await
            .expect("late sample branch");
        let late = oracle.edges[late].to;
        let invalid = oracle
            .apply(
                late,
                &captured_ordinary(&payload, payload.not_before_ms + 1),
            )
            .await
            .expect("classified actual Store validation error");
        assert_eq!(
            oracle.edges[invalid].result,
            if route == "retry" {
                "validation_error_retry_time"
            } else {
                "validation_error_yield_time"
            }
        );
        assert_eq!(
            oracle.edges[invalid].to, late,
            "validation failure preserves full canonical rows and Episode"
        );
        assert_eq!(
            oracle.edges[invalid].error.as_deref(),
            Some(if route == "retry" {
                "invalid background failure retry"
            } else {
                "invalid background checkpoint or yield time"
            })
        );
        let after_expiry = oracle
            .apply(
                handle,
                &Action::ClockAdvance {
                    now: snapshot.token_capture.lease_expires_ms + 1,
                },
            )
            .await
            .expect("backend applies after wall expiry");
        let delayed = oracle
            .apply(
                oracle.edges[after_expiry].to,
                &captured_ordinary(&payload, request_now),
            )
            .await
            .expect("saved request time is the actual CAS time");
        assert_eq!(oracle.edges[delayed].result, "accepted");
        assert_eq!(
            snapshot.failed_attempts, 0,
            "committed claim snapshot stays immutable"
        );
    }
}
#[tokio::test]
#[ignore = "requires source-bound driver input; run scripts/check-async-driver-refinement"]
async fn actual_ordinary_local_guard_and_response_conformance() {
    // Exercise the real successor relation, concrete Driver and competing
    // actual completion edge. This is a focused component witness, not BFS.
    {
        let (driver, _) = driver_input().expect("source-bound queue selector driver");
        let context = contexts()
            .into_iter()
            .find(|c| c.id == "base")
            .expect("base context");
        let mut oracle = Oracle::new(None).await.expect("actual selector fixture");
        let claim = oracle
            .apply(0, &Action::Claim { job: 0, node: 0 })
            .await
            .expect("actual claim");
        let claim = oracle.edges[claim].clone();
        assert_eq!(claim.result, "claimed");
        let snapshot = claim
            .claim_snapshot
            .clone()
            .expect("actual committed snapshot");
        let acquire = oracle
            .apply(claim.to, &Action::AcquireDomain { node: 0 })
            .await
            .expect("actual domain claim");
        assert_eq!(oracle.edges[acquire].result, "acquired");
        let bind = oracle
            .apply(
                oracle.edges[acquire].to,
                &Action::Bind {
                    job: 0,
                    token: 0,
                    lease: 0,
                },
            )
            .await
            .expect("actual source bind");
        assert_eq!(oracle.edges[bind].result, "accepted");
        let handle = oracle.edges[bind].to;
        let mut real = RealDriver::new();
        let mut ctrl = driver.initial;
        for (tag, reply) in [
            (
                "DomainAcquired",
                RealReply::Domain {
                    acquired: true,
                    now_ms: 0,
                },
            ),
            ("BindAccepted", RealReply::Bound { accepted: true }),
        ] {
            real.drive(reply).expect("actual driver reaches PollWork");
            ctrl = *driver
                .replies
                .get(&(ctrl, tag.into()))
                .expect("source-bound control edge");
            verify_control(real, &driver.states[ctrl]).expect("actual complete control");
        }
        let mut poll = Valuation {
            sql: handle,
            w: [Worker::idle(), Worker::idle()],
            available: [true, true],
            completed: vec![],
            mono_now_ms: 0,
            wall_anchors: vec![],
            mono_anchors: vec![],
            claim_loss_keys: vec![],
            real_drivers: [Some(real), None],
        };
        let w = &mut poll.w[0];
        w.stage = "driver".into();
        w.job = 0;
        w.token = 0;
        w.lease = 0;
        w.attempt = snapshot.claim_id.clone();
        w.ctrl = ctrl as i64;
        w.call = "sent".into();
        w.local_token = true;
        w.started = true;
        w.work_started = true;
        w.admission = true;
        w.read_lock = true;
        w.claim_snapshot = Some(snapshot);
        w.claim_request = claim.claim_request.clone();
        w.claim_issued_mono_ms = Some(0);
        w.claim_request_now_ms = Some(1000);
        w.local_deadline_mono_ms = Some(JOB_LEASE_MS);
        assert_eq!(driver.pending(w), "PollWork");
        retain_anchors(&mut poll, &oracle);
        let input = Action::Complete {
            job: 0,
            token: 0,
            lease: 0,
        };
        let completion = oracle
            .apply(handle, &input)
            .await
            .expect("actual competing success");
        assert_eq!(oracle.edges[completion].result, "accepted");
        let demands = vec![Demand {
            spec: DemandSpec {
                oracle_from: handle,
                family: "WorkSuccess",
                worker: Some(0),
                role: "apply",
                input: Some(input),
                capture: None,
                sample: None,
            },
            edge: Some(completion),
        }];
        let unsignaled =
            successors(&context, &poll, &driver, &oracle, &demands).expect("unsignaled successors");
        let other_tags = |steps: &[Step]| {
            steps
                .iter()
                .filter(|s| s.branch.family == "WorkOther" && s.branch.worker == Some(0))
                .map(|s| s.value.w[0].reply.clone())
                .collect::<BTreeSet<_>>()
        };
        assert_eq!(
            other_tags(&unsignaled),
            ["DomainLoss", "Pressure", "WorkFailed"]
                .map(String::from)
                .into_iter()
                .collect()
        );
        let success = unsignaled
            .iter()
            .find(|s| s.branch.family == "WorkSuccess" && s.branch.worker == Some(0))
            .expect("unsignaled actual completion remains available");
        assert_eq!(success.edge, Some(completion));
        assert_eq!(success.value.sql, oracle.edges[completion].to);
        let signal = unsignaled
            .iter()
            .find(|s| s.branch.family == "QueueLossSignal" && s.branch.worker == Some(0))
            .expect("loss signaling remains separate");
        assert!(signal.value.w[0].queue_lost);
        assert_eq!(signal.value.sql, poll.sql);
        assert_eq!(signal.value.w[0].call, "sent");
        assert_eq!(signal.value.w[0].ctrl, poll.w[0].ctrl);
        assert_eq!(signal.value.w[0].reply, poll.w[0].reply);
        assert_eq!(signal.value.w[0].read_lock, poll.w[0].read_lock);
        assert_eq!(signal.value.w[0].token, poll.w[0].token);
        assert_eq!(signal.value.w[0].local_token, poll.w[0].local_token);
        assert!(signal.edge.is_none());
        assert!(memberships(signal, &poll, &context, &oracle).is_empty());
        let signaled = successors(&context, &signal.value, &driver, &oracle, &demands)
            .expect("signaled successors");
        assert_eq!(other_tags(&signaled), BTreeSet::from(["QueueLoss".into()]));
        assert!(!signaled.iter().any(|s| s.branch.worker == Some(0)
            && matches!(s.branch.family.as_str(), "WorkSuccess" | "QueueLossSignal")));
        // A reachable stable expiry boundary still has a structural demand;
        // its explicit exclusion must prevent lookup, not erase the input.
        let expiry = oracle.states[handle].episode.tokens[0][0].lease_expires_ms;
        let boundary = oracle
            .apply(handle, &Action::ClockAdvance { now: expiry - 1 })
            .await
            .expect("actual live expiry neighbour");
        let mut stable = poll.clone();
        stable.sql = oracle.edges[boundary].to;
        retain_anchors(&mut stable, &oracle);
        let stable_context = contexts()
            .into_iter()
            .find(|c| c.id == "stable")
            .expect("stable context");
        let clock = enumerate_candidates(&stable_context, &stable, &driver, &oracle)
            .expect("complete structural candidates")
            .into_iter()
            .find(|d| d.family == "WallAdvance")
            .expect("blocked clock remains inventoried");
        assert_eq!(clock.input, Some(Action::ClockAdvance { now: expiry }));
        assert_eq!(
            classify_demand(&stable_context, &driver, &stable, &clock, &oracle)
                .expect("stable window classification"),
            Some(json!({"kind":"domain","reason":"stable_wall_window"}))
        );
        for (before, steps) in [(&poll, &unsignaled), (&signal.value, &signaled)] {
            for work in steps
                .iter()
                .filter(|s| s.branch.family == "WorkOther" && s.branch.worker == Some(0))
            {
                assert_eq!(
                    work.value.sql, handle,
                    "local work return has no SQL effect"
                );
                assert_eq!(work.value.w[0].call, "applied");
                assert_eq!(work.value.w[0].queue_lost, before.w[0].queue_lost);
                assert!(memberships(work, before, &context, &oracle)
                    .contains(&"ExternalReturn/0".into()));
                let observed = successors(&context, &work.value, &driver, &oracle, &[])
                    .expect("work reply service")
                    .into_iter()
                    .find(|s| s.branch.family == "Reply" && s.branch.worker == Some(0))
                    .expect("reply observed");
                let returned = successors(&context, &observed.value, &driver, &oracle, &[])
                    .expect("actual Driver return")
                    .into_iter()
                    .find(|s| s.branch.family == "DriverReturn" && s.branch.worker == Some(0))
                    .expect("actual reply driven");
                assert_eq!(
                    returned.value.w[0].ctrl as usize,
                    driver.replies[&(ctrl, work.value.w[0].reply.clone())]
                );
                verify_real(&returned.value, &driver)
                    .expect("real Driver matches returned control");
            }
        }
    }
    let (mut oracle, handle, payload, snapshot) = actual_payload_setup("interrupted")
        .await
        .expect("actual source payload");
    let mut value = Valuation {
        sql: handle,
        w: [Worker::idle(), Worker::idle()],
        available: [true, true],
        completed: vec![],
        mono_now_ms: 0,
        wall_anchors: vec![],
        mono_anchors: vec![],
        claim_loss_keys: vec![],
        real_drivers: [None, None],
    };
    value.w[0].job = 0;
    value.w[0].token = 0;
    value.w[0].local_token = true;
    value.w[0].local_deadline_mono_ms = Some(JOB_LEASE_MS);
    value.w[0].claim_snapshot = Some(snapshot);
    value.w[0].ordinary.payload = Some(payload);
    for (phase, lost, local, mono, expected) in [
        ("check_lost", true, true, 0, "local_lost"),
        ("check_deadline", false, true, JOB_LEASE_MS, "local_expired"),
        ("take_token", false, false, 0, "local_no_token"),
    ] {
        let mut before = value.clone();
        before.w[0].ordinary.phase = phase.into();
        before.w[0].queue_lost = lost;
        before.w[0].local_token = local;
        before.mono_now_ms = mono;
        let steps = ordinary_steps(&before, 0, &oracle, &[]).expect("finite local guard");
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].value.sql, handle);
        assert_eq!(
            steps[0].value.w[0].ordinary.observed_outcome.as_deref(),
            Some(expected)
        );
        assert_eq!(steps[0].value.w[0].local_token, local);
    }
    value.w[0].ordinary.phase = "sample".into();
    let clock = ordinary_steps(&value, 0, &oracle, &[])
        .expect("source clock error envelope")
        .into_iter()
        .find(|step| step.branch.choice["clock_error"] == true)
        .expect("clock-error exit");
    assert_eq!(clock.value.sql, handle);
    assert_eq!(
        clock.value.w[0].ordinary.observed_outcome.as_deref(),
        Some("clock_error")
    );
    assert!(clock.value.w[0].fault_used);
    for observed in [
        "accepted",
        "refused",
        "validation_error_yield_time",
        "transport_error",
    ] {
        let mut before = value.clone();
        before.w[0].ordinary.phase = "finish".into();
        before.w[0].ordinary.observed_outcome = Some(observed.into());
        before.w[0].queue_lost = true;
        before.mono_now_ms = JOB_LEASE_MS + 1;
        let finish = ordinary_steps(&before, 0, &oracle, &[]).expect("no post-await recheck");
        assert_eq!(finish[0].value.w[0].local_token, observed != "accepted");
        assert_eq!(
            finish[0].value.w[0]
                .adapter_reply
                .as_ref()
                .expect("outer adapter Done")
                .tag,
            "Done"
        );
        assert_eq!(finish[0].value.sql, handle);
    }
    // The observer consumes a real committed SQL edge. A lost response keeps
    // that successor and the local token; post-guard loss/expiry adds no CAS.
    value.w[0].ordinary.phase = "sent".into();
    value.w[0].ordinary.request_now_ms = Some(1000);
    value.w[0].ordinary.captured_token = Some(0);
    let input = ordinary_input(&value, 0).expect("captured source request");
    let id = oracle
        .apply(handle, &input)
        .await
        .expect("actual committed settlement");
    assert_eq!(oracle.edges[id].result, "accepted");
    let demands = vec![Demand {
        spec: DemandSpec {
            oracle_from: handle,
            family: "OrdinaryCallReturn",
            worker: Some(0),
            role: "apply",
            input: Some(input),
            capture: None,
            sample: None,
        },
        edge: Some(id),
    }];
    let applied = ordinary_steps(&value, 0, &oracle, &demands)
        .expect("actual API edge consumed")
        .into_iter()
        .find(|step| step.edge == Some(id))
        .expect("applied branch");
    let mut committed = applied.value;
    committed.w[0].queue_lost = true;
    committed.mono_now_ms = JOB_LEASE_MS + 1;
    for lost in [false, true] {
        let observed = ordinary_steps(&committed, 0, &oracle, &[])
            .expect("response after guard loss/expiry")
            .into_iter()
            .find(|step| step.branch.choice["lost"] == lost)
            .expect("observed branch");
        assert_eq!(
            observed.value.sql, oracle.edges[id].to,
            "lost response never rolls back committed SQL"
        );
        let finish = ordinary_steps(&observed.value, 0, &oracle, &[]).expect("source unlock");
        assert_eq!(
            finish[0].value.w[0].local_token, lost,
            "only observed accepted clears the token"
        );
        assert_eq!(finish[0].value.sql, oracle.edges[id].to);
    }
}
#[tokio::test]
#[ignore = "requires source-bound driver input; run scripts/check-async-driver-refinement"]
async fn actual_queued_eligibility_progress_witness() {
    let (mut oracle, handle, payload, _) = actual_payload_setup("refused")
        .await
        .expect("source-valid future Yield");
    let id = oracle
        .apply(handle, &captured_ordinary(&payload, 1000))
        .await
        .expect("actual yielded queue");
    let queued = oracle.edges[id].to;
    let refused = oracle
        .apply(queued, &Action::Claim { job: 0, node: 0 })
        .await
        .expect("future job cannot claim");
    assert_eq!(oracle.edges[refused].result, "refused");
    assert_eq!(oracle.edges[refused].to, queued);
    let mut wall = oracle.states[queued].episode.now;
    let mut target = queued;
    let anchors = [payload.not_before_ms];
    for _ in 0..3 {
        if wall >= payload.not_before_ms {
            break;
        }
        let next = next_neighbour(&anchors, wall).expect("next eligibility landmark");
        let edge = oracle
            .apply(target, &Action::ClockAdvance { now: next })
            .await
            .expect("actual wall progression");
        target = oracle.edges[edge].to;
        wall = next;
    }
    assert_eq!(wall, payload.not_before_ms);
    let claim = oracle
        .apply(target, &Action::Claim { job: 0, node: 0 })
        .await
        .expect("claim at stored eligibility");
    assert_eq!(oracle.edges[claim].result, "claimed");
}

#[test]
fn product_runtime_diagnostic_records_are_bounded() {
    let mut diagnostics = RuntimeDiagnostics::new();
    let mut pools = WirePools::default();
    let mut budget = Budget::default();
    let value = wire_fixture_valuation();
    let key_before = pools
        .key(0, &value, &mut budget)
        .expect("valid fixture key");
    let values_before = pools.values.clone();
    let workers_before = pools.workers.clone();
    let bytes_before = budget.bytes;
    let oversized = diagnostics
        .record(
            "fixture",
            json!({"oversized":"x".repeat(DIAGNOSTIC_RECORD_BYTES)}),
        )
        .expect("bounded diagnostic record");
    assert!(oversized.len() <= DIAGNOSTIC_RECORD_BYTES);
    assert_eq!(
        serde_json::from_str::<Value>(&oversized).expect("diagnostic JSON")["phase"],
        "diagnostic_record_omitted"
    );
    for _ in 1..DIAGNOSTIC_RECORDS {
        let text = diagnostics
            .record("expansion", json!({"expanded":42}))
            .expect("bounded diagnostic record");
        assert!(text.len() <= DIAGNOSTIC_RECORD_BYTES);
        let row: Value = serde_json::from_str(&text).expect("diagnostic JSON");
        assert_eq!(row["facts"]["expanded"], 42);
        assert!(row.get("accepted").is_none());
    }
    assert!(diagnostics.record("write_complete", json!({})).is_none());
    let key_after = pools
        .key(0, &value, &mut budget)
        .expect("valid fixture key");
    assert!(key_before == key_after);
    assert_eq!(pools.values, values_before);
    assert_eq!(pools.workers, workers_before);
    assert_eq!(budget.bytes, bytes_before);
}
