//! Exact selected two-worker product producer. The independent checker/TLA
//! validate its certificate; this producer's adjacency is never an oracle.
//! Schema6 persists exact identities, complete predecessor expansions and
//! resumable checkpoints. Qualification remains pending reviewed execution.
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use super::async_driver_catalog as paged;
use super::background_jobs::JOB_LEASE_MS;
use plurx_worker_control::library_driver::{Driver as RealDriver, Reply as RealReply, WorkOutcome};
use plurx_worker_control::library_lifecycle::Settlement;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::async_driver_refinement::{
    self as sql, Action, Oracle, OracleEdge, StoreHandle, TriggerMutation,
};

const MEMBER_BYTES: usize = 256 * 1024 * 1024;
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

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AdapterReply {
    tag: String,
    now_ms: Option<i64>,
    retry_delay_ms: Option<i64>,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
struct ClaimLossKey {
    job: usize,
    node: usize,
    claim_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
        .filter(|j| oracle.state(handle).view["jobs"][j]["request_done"] == true)
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
    let state = &oracle.state(value.sql);
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
    let episode = &oracle.state(value.sql).episode;
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
    let episode = &oracle.state(value.sql).episode;
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
        && oracle.state(handle).episode.tokens[job][w.token as usize].claim_id
            == oracle.state(handle).episode.tokens[job][token].claim_id
}
fn environment_allowed(action: &Action, value: &Valuation, oracle: &Oracle) -> bool {
    match action {
        Action::RemoveOwner { .. } => true,
        Action::ClockAdvance { .. } => false,
        Action::AcquireDomain { node } => *node == 2,
        Action::ReleaseDomain { lease } => {
            oracle.state(value.sql).episode.leases[*lease].owner_node_id == "foreign-holder"
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
            now_ms: oracle.state(value.sql).episode.now,
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
    if let Some(now) = next_neighbour(&value.wall_anchors, oracle.state(value.sql).episode.now) {
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
            if environment_allowed(&input, value, oracle) {
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
    let episode = &oracle.state(spec.oracle_from).episode;
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
                    oracle.state(value.sql).view["jobs"][job]["state"] == "running"
                        && oracle.state(value.sql).view["jobs"][job]["owner"]
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

fn outcome(
    demands: &[Demand],
    family: &str,
    worker: Option<usize>,
    action: Option<&Action>,
    oracle: &Oracle,
) -> Option<OracleEdge> {
    demands
        .iter()
        .find(|d| {
            d.spec.family == family
                && d.spec.worker == worker
                && action.is_none_or(|a| d.spec.input.as_ref() == Some(a))
        })
        .and_then(|d| d.edge)
        .map(|id| oracle.edge(id))
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
        next.w[n].lease = (oracle.state(edge.to).episode.leases.len() - 1) as i64;
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
                    next.w[n].ordinary.request_now_ms = Some(oracle.state(value.sql).episode.now);
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
            let edge = &oracle.edge(id);
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
        let resolved = &oracle.edge(id);
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
                let read = &oracle.edge(read);
                move_sql(&mut next, read, oracle);
                let token_index = resolved
                    .resolved_token
                    .ok_or("MissingInitialResolvedCapture")?;
                let token = &oracle.state(resolved.to).episode.tokens[w.job as usize][token_index];
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
                let cleanup = &oracle.edge(cleanup);
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
            .find_map(|d| {
                d.edge
                    .filter(|id| encoded(&oracle.edge(*id)) == encoded(edge))
            })
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
                if oracle.state(value.sql).view["jobs"][job]["state"] == "queued"
                    && oracle.state(value.sql).view["jobs"][job]["request_done"] == false
                    && oracle.state(value.sql).view["jobs"][job]["not_before_ms"]
                        .as_i64()
                        .is_some_and(|t| t <= oracle.state(value.sql).episode.now)
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
                        nw.claim_request_now_ms = Some(oracle.state(value.sql).episode.now);
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
                                oracle.state(value.sql).episode.now,
                            ));
                            nw.claim_snapshot = Some(snapshot);
                            let token =
                                oracle.state(edge.to).episode.tokens[w.job as usize].len() - 1;
                            nw.reply = "Claimed".into();
                            nw.token = token as i64;
                            nw.attempt = oracle.state(edge.to).episode.tokens[w.job as usize]
                                [token]
                                .claim_id
                                .clone();
                        } else {
                            nw.reply = "Refused".into();
                            nw.token = -1;
                            nw.attempt = "".into();
                        }
                        push("ClaimApply", Some(n), json!({}), next, Some(edge_id(&edge)));
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
                    && oracle.state(value.sql).episode.claims[w.job as usize] < 2))
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
                    if let Some(next) = commit_reply(value, n, &edge, tag, driver, oracle) {
                        push("StoreApply", Some(n), json!({}), next, Some(edge_id(&edge)));
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
                            commit_reply(value, n, &edge, "WorkSucceeded", driver, oracle)
                        {
                            push(
                                "WorkSuccess",
                                Some(n),
                                json!({}),
                                next,
                                Some(edge_id(&edge)),
                            );
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
                                oracle.state(value.sql).episode.now
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
                                move_sql(&mut next, &edge, oracle);
                                next.w[n].inner = "applied".into();
                                next.w[n].outcome = edge.result.clone();
                                if matches!(edge.action, Action::Resolve { .. }) {
                                    next.w[n].resolved =
                                        edge.resolved_token.map_or(-1, |i| i as i64);
                                }
                                push(
                                    "RetireApply",
                                    Some(n),
                                    json!({}),
                                    next,
                                    Some(edge_id(&edge)),
                                );
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
                    oracle.state(value.sql).episode.now
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
        move_sql(&mut next, &edge, oracle);
        let Action::ClockAdvance { now } = edge.action else {
            return Err("WallAdvanceOracleAction".into());
        };
        push(
            "WallAdvance",
            None,
            json!({"now":now}),
            next,
            Some(edge_id(&edge)),
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
            let edge = &oracle.edge(id);
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
                        move_sql(&mut next, &oracle.edge(r), oracle);
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
                                oracle.state(edge.to).episode.tokens[*job].len() - 1,
                                "running",
                            ))
                        } else {
                            let r = &oracle.edge(resolution.ok_or("MissingRenewResolution")?);
                            match r.result.as_str() {
                                "running" => Some((
                                    r.resolved_token.ok_or("MissingActualResolvedToken")?,
                                    "running",
                                )),
                                "cancel_requested" => Some((
                                    r.resolved_token.ok_or("MissingActualResolvedToken")?,
                                    "cancel_requested",
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
                                &oracle.state(next.sql).episode.tokens[old.job as usize][token];
                            let local = deadline(
                                value.mono_now_ms,
                                returned.lease_expires_ms,
                                oracle.state(value.sql).episode.now,
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
            let primary = &oracle.edge(step.edge.ok_or("MissingInitialRecoveryEvidence")?);
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
        let edge = &oracle.edge(id);
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
        && oracle.state(value.sql).view["jobs"]
            .as_array()
            .expect("jobs")
            .iter()
            .any(|j| {
                j["state"] == "queued"
                    && j["request_done"] == false
                    && j["not_before_ms"]
                        .as_i64()
                        .is_some_and(|t| t > oracle.state(value.sql).episode.now)
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
            .is_some_and(|id| oracle.edge(id).result == "claimed")
    {
        labels.insert(format!("ClaimWin/{n}"));
    }
    labels.into_iter().collect()
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
fn product_payload(context: &Context, value: &Valuation) -> Value {
    json!({"context":context.id,"sql":value.sql,"w":value.w,"available":value.available,"completed":value.completed,"mono_now_ms":value.mono_now_ms,"wall_anchors":value.wall_anchors,"mono_anchors":value.mono_anchors,"claim_loss_keys":value.claim_loss_keys})
}
// Bounded host diagnostics only; never a field in valuations, catalog identities or certificate.
const DIAGNOSTIC_RECORDS: usize = 128;
const DIAGNOSTIC_RECORD_BYTES: usize = 8192;
struct RuntimeDiagnostics {
    started: Instant,
    last_progress: Instant,
    records: usize,
}
impl RuntimeDiagnostics {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            started: now,
            last_progress: now,
            records: 0,
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
fn catalog_result<T>(value: paged::CatalogResult<T>) -> Result<T, String> {
    value.map_err(|error| error.to_string())
}
fn decode_paged_product(record: &paged::Record) -> Result<Valuation, String> {
    require(record.kind == "Product", "PagedProductKind")?;
    catalog_result(record.validate())?;
    let mut payload = catalog_result(paged::strict_json(
        catalog_result(paged::string(&record.record, "canonical_utf8"))?.as_bytes(),
    ))?;
    let context = payload
        .as_object_mut()
        .ok_or("PagedProductPayload")?
        .remove("context")
        .ok_or("PagedProductContext")?;
    require(
        context == record.record["context"],
        "PagedProductContextIdentity",
    )?;
    let value: Valuation =
        serde_json::from_value(payload).map_err(|error| format!("PagedProductPayload: {error}"))?;
    require(
        value
            .sql
            .checked_add(1)
            .is_some_and(|id| record.record["store_id"] == id),
        "PagedProductStoreIdentity",
    )?;
    Ok(value)
}
fn restore_actual_drivers(
    catalog: &paged::Catalog,
    product: &paged::Record,
    driver: &Driver,
    deadline: Instant,
) -> Result<Valuation, String> {
    let mut value = decode_paged_product(product)?;
    for n in 0..2 {
        if value.w[n].stage != "driver" {
            continue;
        }
        let mut cursor = product.clone();
        let mut replies = Vec::new();
        loop {
            require(
                Instant::now() < deadline,
                "ResourceIncomplete: driver replay deadline",
            )?;
            let edge_id = catalog_result(paged::number(&cursor.record, "predecessor_edge"))?;
            let edge = catalog_result(catalog.get_id("ProductEdge", edge_id))?;
            let from = catalog_result(paged::number(&edge.record, "from_product"))?;
            require(
                edge.record["to_product"] == cursor.record["id"]
                    && from < catalog_result(paged::number(&cursor.record, "id"))?,
                "PagedDriverDiscoveryChain",
            )?;
            let branch: Branch =
                serde_json::from_str(catalog_result(paged::string(&edge.record, "branch_utf8"))?)
                    .map_err(|error| format!("PagedDriverBranch: {error}"))?;
            let parent = catalog_result(catalog.get_id("Product", from))?;
            if branch.worker == Some(n) {
                if branch.family == "DriverReturn" {
                    let previous = decode_paged_product(&parent)?;
                    let reply = previous.w[n]
                        .adapter_reply
                        .clone()
                        .ok_or("MissingHistoricalActualAdapterReply")?;
                    require(
                        previous.w[n].call == "replied" && previous.w[n].reply == reply.tag,
                        "HistoricalActualAdapterReplyAgreement",
                    )?;
                    replies.push(reply);
                    // This is a checked bound from the actual finite driver DAG,
                    // not a bound on product history, clocks, or source Next.
                    require(
                        replies.len() <= driver.states.len(),
                        "ActualDriverDAGReplayBound",
                    )?;
                }
                if branch.family == "ClaimReturn" {
                    require(
                        decode_paged_product(&cursor)?.w[n].stage == "driver"
                            && decode_paged_product(&parent)?.w[n].stage == "claim",
                        "HistoricalActualDriverStart",
                    )?;
                    break;
                }
            }
            cursor = parent;
        }
        let mut real = RealDriver::new();
        for reply in replies.iter().rev() {
            real.drive(concrete_reply(reply)?)
                .map_err(|_| "HistoricalActualDriverRejectedReply")?;
        }
        value.real_drivers[n] = Some(real);
    }
    verify_real(&value, driver)?;
    Ok(value)
}
#[derive(Default)]
struct PredecessorRecords {
    candidates: Vec<u64>,
    demands: Vec<u64>,
    outcomes: Vec<u64>,
    seen_outcomes: BTreeSet<u64>,
}
struct PredecessorSource<'a> {
    catalog: &'a paged::Catalog,
    context: &'a Context,
    product: paged::ProductId,
}
impl PredecessorRecords {
    async fn oracle_demand(
        &mut self,
        source: &PredecessorSource<'_>,
        oracle: &mut Oracle,
        spec: DemandSpec,
        exclusion: Option<Value>,
        dependencies: &[u64],
    ) -> Result<Demand, String> {
        let catalog = source.catalog;
        let context = source.context;
        let predecessor = source.product.get();
        let classification = if exclusion.is_none() {
            "eligible"
        } else if exclusion
            .as_ref()
            .is_some_and(|value| value["kind"] == "sample")
        {
            "excluded_sample"
        } else {
            "excluded_domain"
        };
        let reason = exclusion.as_ref().map(canonical);
        let choice = json!({"oracle_from":spec.oracle_from,"role":spec.role,"input":spec.input,"capture":spec.capture,"sample":spec.sample});
        let descriptor = json!({"kind":"oracle_demand","family":spec.family,"worker":spec.worker,"oracle_from":spec.oracle_from,"role":spec.role,"input":spec.input,"capture":spec.capture,"sample":spec.sample});
        let candidate = catalog_result(catalog.allocate("Candidate"))?;
        catalog_result(catalog.append_record("Candidate",json!({"id":candidate,"context":context.id,"predecessor":predecessor,"ordinal":self.candidates.len(),"family":spec.family,"worker":spec.worker,"choice_utf8":canonical(&choice),"descriptor_utf8":canonical(&descriptor),"classification":classification,"reason":reason})))?;
        self.candidates.push(candidate);
        let edge = if exclusion.is_none() {
            Some(
                oracle
                    .apply(
                        spec.oracle_from,
                        spec.input.as_ref().ok_or("MissingEligibleDemandInput")?,
                    )
                    .await?,
            )
        } else {
            None
        };
        let outcome = edge.map(|id| id as u64 + 1);
        if let Some(outcome) = outcome {
            if self.seen_outcomes.insert(outcome) {
                self.outcomes.push(outcome);
            }
        }
        let role = match spec.family {
            "EnvironmentResolve" => "dependent_resolve",
            "ClaimRecoverRead" => "dependent_read",
            "ClaimRecoverCancel" => "dependent_cancel",
            _ => spec.role,
        };
        let id = catalog_result(catalog.allocate("Demand"))?;
        catalog_result(catalog.append_record("Demand",json!({"id":id,"candidate":candidate,"ordinal":0,"from_store":spec.oracle_from as u64+1,"family":spec.family,"worker":spec.worker,"role":role,"input_utf8":canonical(&spec.input),"capture":spec.capture,"sample":spec.sample,"classification":classification,"reason":reason,"outcome_id":outcome,"depends_on":dependencies})))?;
        self.demands.push(id);
        Ok(Demand { spec, edge })
    }
}
/// One complete original predecessor expansion. Capacity-only outcomes and all
/// dependent demands are retained even when no selected product edge uses them.
async fn expand_paged_predecessor(
    catalog: &paged::Catalog,
    oracle: &mut Oracle,
    driver: &Driver,
    unit: u64,
    deadline: Instant,
) -> Result<(), String> {
    let (ticket, product) =
        catalog_result(catalog.oldest_pending())?.ok_or("NoPendingPredecessor")?;
    let source = catalog_result(catalog.get_id("Product", product.get()))?;
    let context = contexts()
        .into_iter()
        .find(|context| source.record["context"] == context.id)
        .ok_or("UnknownPagedContext")?;
    let value = restore_actual_drivers(catalog, &source, driver, deadline)?;
    let transaction = catalog_result(catalog.begin_expansion(ticket, product, unit))?;
    let result=async {
        require(Instant::now()<deadline,"ResourceIncomplete: complete predecessor deadline")?;
        // Collect AND classify the whole primary structural demand set before
        // any production lookup can add a cached or novel outcome.
        let primary=enumerate_candidates(&context,&value,driver,oracle)?.into_iter().map(|spec|{
            let excluded=classify_demand(&context,driver,&value,&spec,oracle)?;Ok((spec,excluded))
        }).collect::<Result<Vec<_>,String>>()?;
        let source=PredecessorSource {catalog,context:&context,product};
        let mut records=PredecessorRecords::default();let mut demands=Vec::new();
        for (spec,exclusion) in primary {
            require(Instant::now()<deadline,"ResourceIncomplete: primary demand deadline")?;
            demands.push(records.oracle_demand(&source,oracle,spec,exclusion,&[]).await?);
        }
        let primary_count=demands.len();
        let mut renewal=Vec::<(StoreHandle,Action,BTreeSet<u64>)>::new();
        for (index,demand) in demands.iter().enumerate() {
            if demand.spec.family!="Environment" {continue;}let Some(id)=demand.edge else{continue;};let edge=oracle.edge(id);
            let Action::Renew {job,token}=edge.action else{continue;};
            let matching=value.w.iter().any(|worker|same_claim(worker,job,token,oracle,value.sql));
            let needs=edge.result=="refused"||(edge.result=="renewed"&&value.w.iter().any(|worker|same_claim(worker,job,token,oracle,value.sql)&&!worker.fault_used));
            if matching&&needs {
                let action=Action::Resolve {job,token};
                if let Some((_,_,parents))=renewal.iter_mut().find(|(from,input,_)|*from==edge.to&&*input==action) {parents.insert(records.demands[index]);}
                else{renewal.push((edge.to,action,BTreeSet::from([records.demands[index]])));}
            }
        }
        for (from,input,parents) in renewal {
            let spec=DemandSpec {oracle_from:from,family:"EnvironmentResolve",worker:None,role:"apply",input:Some(input),capture:None,sample:None};
            let exclusion=classify_demand(&context,driver,&value,&spec,oracle)?;
            require(exclusion.is_none(),"MissingRenewalResolutionInput")?;
            demands.push(records.oracle_demand(&source,oracle,spec,exclusion,&parents.into_iter().collect::<Vec<_>>()).await?);
        }
        let mut recovery=Vec::new();
        for (index,demand) in demands.iter().take(primary_count).enumerate() {
            if demand.spec.family!="ClaimRecover" {continue;}let Some(id)=demand.edge else{continue;};let edge=oracle.edge(id);let n=demand.spec.worker.ok_or("RecoveryDemandWorker")?;let job=value.w[n].job as usize;
            let next=match edge.result.as_str(){"running"=>Some(("ClaimRecoverRead",Action::ReadClaimJob {job})),"cancel_requested"=>Some(("ClaimRecoverCancel",Action::Settle {job,token:edge.resolved_token.ok_or("MissingActualCancellationCapture")?,disposition:2})),_=>None};
            if let Some((family,input))=next {recovery.push((edge.to,n,family,input,records.demands[index]));}
        }
        // Preserve original recovery-demand occurrence order. Each occurrence
        // carries the complete union of matching primary triggers.
        for (from,n,family,input,_) in &recovery {
            let parents=recovery.iter().filter(|(parent,worker,kind,action,_)|parent==from&&worker==n&&kind==family&&action==input).map(|(_,_,_,_,id)|*id).collect::<BTreeSet<_>>().into_iter().collect::<Vec<_>>();
            let spec=DemandSpec {oracle_from:*from,family,worker:Some(*n),role:"apply",input:Some(input.clone()),capture:None,sample:None};
            let exclusion=classify_demand(&context,driver,&value,&spec,oracle)?;
            require(exclusion.is_none(),"MissingInitialRecoveryDependentInput")?;
            demands.push(records.oracle_demand(&source,oracle,spec,exclusion,&parents).await?);
        }
        require(Instant::now()<deadline,"ResourceIncomplete: successor deadline")?;
        let steps=successors(&context,&value,driver,oracle,&demands)?;
        let mut branches=BTreeSet::new();let mut pairs=BTreeMap::<u64,BTreeSet<String>>::new();let mut edges=Vec::new();let mut target_stores=BTreeSet::new();
        for step in steps {
            require(Instant::now()<deadline,"ResourceIncomplete: successor append deadline")?;
            let candidate=catalog_result(catalog.allocate("Candidate"))?;
            catalog_result(catalog.append_record("Candidate",json!({"id":candidate,"context":context.id,"predecessor":product.get(),"ordinal":records.candidates.len(),"family":step.branch.family,"worker":step.branch.worker,"choice_utf8":canonical(&step.branch.choice),"descriptor_utf8":canonical(&json!({"kind":"product_step","branch":step.branch})),"classification":"eligible","reason":null})))?;
            records.candidates.push(candidate);
            let store=catalog_result(paged::StoreId::new(step.value.sql as u64+1))?;
            let to=catalog_result(catalog.intern_product(context.id,&canonical(&product_payload(&context,&step.value)),store))?;
            require(branches.insert((canonical(&step.branch),to.get())),"ProductSuccessors: duplicate finite branch/target")?;
            pairs.entry(to.get()).or_default().extend(memberships(&step,&value,&context,oracle));target_stores.insert(store.get());
            let edge=catalog_result(catalog.append_product_edge(json!({"context":context.id,"from_product":product.get(),"to_product":to.get(),"candidate":candidate,"branch_utf8":canonical(&step.branch),"primary_edge":step.edge.map(|id|id as u64+1),"resolution_edge":step.resolution_edge.map(|id|id as u64+1),"read_edge":step.read_edge.map(|id|id as u64+1),"cleanup_edge":step.cleanup_edge.map(|id|id as u64+1)})))?;
            edges.push(edge.get());
        }
        let mut fair_pairs=Vec::new();let mut enabled=BTreeSet::new();
        for (to,labels) in pairs {
            if to!=product.get(){enabled.extend(labels.iter().cloned());}
            fair_pairs.push(catalog_result(catalog.union_fair_pair(context.id,product,catalog_result(paged::ProductId::new(to))?,&labels.into_iter().collect::<Vec<_>>()))?);
        }
        let enabled=catalog_result(catalog.append_enabledness(json!({"context":context.id,"product_id":product.get(),"full_graph_labels":enabled})))?;
        let mut capacity=BTreeSet::new();for demand in &demands {if demand.spec.role=="capacity" {if let Some(id)=demand.edge {capacity.insert(oracle.edge(id).to as u64+1);}}}
        let capacity=capacity.difference(&target_stores).copied().collect::<Vec<_>>();
        Ok::<_,String>(json!({"id":catalog_result(transaction.expansion_id())?.get(),"context":context.id,"predecessor":product.get(),"fifo_ticket":ticket.get(),"candidates":records.candidates,"demands":records.demands,"oracle_evidence":records.outcomes,"product_edges":edges,"fair_pairs":fair_pairs,"enabledness":enabled,"discovered_products":transaction.discovered_products(),"capacity_only_stores":capacity,"unit_ordinal":unit,"complete":true}))
    }.await;
    // Restore while private witness handles still exist. An actual rollback or
    // endpoint failure poisons the Oracle; it is never classified as a demand.
    let restored = oracle.restore_root().await;
    match (result, restored) {
        (Ok(seal), Ok(())) => catalog_result(transaction.seal(seal)).map(|_| ()),
        (result, restored) => {
            catalog_result(transaction.abort())?;
            match restored {
                Err(reason) => Err(format!("PoisonedOracle: {reason}")),
                Ok(()) => Err(result.expect_err("failed predecessor")),
            }
        }
    }
}

fn write_paged_json<T: Serialize>(
    directory: &Path,
    name: &str,
    value: &T,
    limit: u64,
) -> Result<(), String> {
    let limit =
        usize::try_from(limit).map_err(|_| "ResourceIncomplete: output host byte domain")?;
    serialized_size(value, limit)?;
    let path = directory.join(name);
    let partial = directory.join(format!("{name}.partial"));
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&partial)
        .map_err(|error| error.to_string())?;
    let mut writer = ByteLimit {
        inner: std::io::BufWriter::new(file),
        bytes: 0,
        limit,
    };
    let text = canonical(value);
    writer
        .write_all(text.as_bytes())
        .map_err(|error| error.to_string())?;
    writer.flush().map_err(|error| error.to_string())?;
    writer
        .inner
        .get_ref()
        .sync_all()
        .map_err(|error| error.to_string())?;
    drop(writer);
    require(!path.exists(), "OutputMemberAlreadyExists")?;
    std::fs::rename(partial, path).map_err(|error| error.to_string())?;
    Ok(())
}
fn copy_paged_member(
    from: &Path,
    to: &Path,
    expected: &str,
    limit: u64,
    deadline: Instant,
) -> Result<(), String> {
    let (size, hash) = catalog_result(paged::hash_file(from, limit, deadline))?;
    require(hash == expected, "InputMemberDigest")?;
    if to.exists() {
        require(
            !to.symlink_metadata()
                .map_err(|error| error.to_string())?
                .file_type()
                .is_symlink(),
            "UnsafeOutputMemberSymlink",
        )?;
        catalog_result(paged::same_file_bytes(from, to, deadline))?;
        return Ok(());
    }
    let mut input = std::fs::File::open(from).map_err(|error| error.to_string())?;
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)
        .map_err(|error| error.to_string())?;
    let copied = std::io::copy(
        &mut std::io::Read::by_ref(&mut input).take(limit + 1),
        &mut output,
    )
    .map_err(|error| error.to_string())?;
    output.sync_all().map_err(|error| error.to_string())?;
    require(
        copied == size && catalog_result(paged::hash_file(to, limit, deadline))? == (size, hash),
        "CopiedInputMemberIdentity",
    )?;
    Ok(())
}
fn compiled_paged_sources() -> BTreeMap<String, String> {
    let mut bindings = paged::compiled_catalog_sources();
    for (path, hash) in sql::compiled_source_bindings() {
        let path = if path.starts_with("store/") || path.starts_with("cluster/") {
            format!("crates/plurx-core/src/{path}")
        } else {
            path.into()
        };
        bindings.insert(path, hash);
    }
    bindings
}
fn ordinary_unit_exhaustion(reason: &str) -> bool {
    [
        "whole atomic quantum exceeds registered unit record/byte allowance",
        "unit predecessor allowance exhausted",
    ]
    .iter()
    .any(|message| reason == paged::CatalogError::ResourceIncomplete((*message).into()).to_string())
        || [
            "ResourceIncomplete: driver replay deadline",
            "ResourceIncomplete: complete predecessor deadline",
            "ResourceIncomplete: primary demand deadline",
            "ResourceIncomplete: successor deadline",
            "ResourceIncomplete: successor append deadline",
        ]
        .contains(&reason)
}
fn temporal_counts(catalog: &paged::Catalog) -> Result<Value, String> {
    let mut counts = serde_json::Map::new();
    for (field, kind) in [
        ("physical_rows", "PhysicalRows"),
        ("episodes", "Episode"),
        ("stores", "Store"),
        ("oracle_edges", "OracleEvidence"),
        ("products", "Product"),
        ("candidates", "Candidate"),
        ("demands", "Demand"),
        ("product_edges", "ProductEdge"),
        ("fair_pairs", "FairPair"),
        ("enabledness", "Enabledness"),
        ("expansions", "Expansion"),
        ("monitor_nodes", "MonitorNode"),
        ("monitor_edges", "MonitorEdge"),
        ("components", "Scc"),
        ("boundaries", "BoundaryObservation"),
    ] {
        counts.insert(field.into(), json!(catalog_result(catalog.count(kind))?));
    }
    require(
        catalog_result(catalog.oldest_pending())?.is_none(),
        "TemporalCountsRequireClosedSource",
    )?;
    counts.insert("frontier".into(), json!(0));
    Ok(Value::Object(counts))
}
fn import_temporal_unit(
    request: &paged::QuantumRequest,
    catalog: &paged::Catalog,
    deadline: Instant,
) -> Result<bool, String> {
    use std::io::BufRead;
    let side_path = request
        .temporal_publication
        .as_ref()
        .ok_or("MissingTemporalPublication")?;
    let path = request
        .temporal_import
        .as_ref()
        .ok_or("MissingTemporalNDJSON")?;
    require(
        side_path
            .metadata()
            .map_err(|error| error.to_string())?
            .len()
            <= 1024 * 1024,
        "TemporalPublicationByteBound",
    )?;
    let side = catalog_result(paged::strict_json(
        &std::fs::read(side_path).map_err(|error| error.to_string())?,
    ))?;
    catalog_result(paged::closed_fields(
        &side,
        &[
            "schema_version",
            "source",
            "session_id",
            "resource_plan_sha256",
            "input_checkpoint_sha256",
            "input_catalog_commit",
            "ndjson_sha256",
            "ndjson_bytes",
            "record_count",
            "final",
            "stage_complete",
        ],
    ))?;
    let prior = request
        .checkpoint
        .as_ref()
        .ok_or("MissingTemporalInputCheckpoint")?;
    require(
        side["schema_version"] == 6
            && side["source"] == request.work_unit["source"]
            && side["session_id"] == request.work_unit["session_id"]
            && side["resource_plan_sha256"] == request.resource_plan.sha256()
            && side["input_checkpoint_sha256"] == prior.sha256()
            && side["input_catalog_commit"] == prior.record["committed_transaction"],
        "TemporalPublicationInputIdentity",
    )?;
    let final_batch = side["final"]
        .as_bool()
        .ok_or("MalformedTemporalFinalFlag")?;
    let complete = side["stage_complete"]
        .as_bool()
        .ok_or("MalformedTemporalStageFlag")?;
    let (bytes, hash) = catalog_result(paged::hash_file(
        path,
        catalog.limits().unit_bytes,
        deadline,
    ))?;
    require(
        side["ndjson_bytes"] == bytes && side["ndjson_sha256"] == hash,
        "TemporalPublicationActualBytes",
    )?;
    let transaction = catalog_result(catalog.begin_temporal(catalog_result(paged::number(
        &request.work_unit,
        "unit_ordinal",
    ))?))?;
    let result = (|| {
        let max_record = [
            "MonitorNode",
            "MonitorEdge",
            "Scc",
            "Counterexample",
            "Outcome",
            "Counts",
            "ContextRoot",
        ]
        .into_iter()
        .map(|kind| {
            catalog
                .limits()
                .allowance(kind)
                .map(|value| value.max_encoded_bytes)
        })
        .collect::<paged::CatalogResult<Vec<_>>>()
        .map_err(|error| error.to_string())?
        .into_iter()
        .max()
        .ok_or("MissingTemporalAllowance")?
        .min(catalog.limits().resident_bytes / 4);
        let mut reader =
            std::io::BufReader::new(std::fs::File::open(path).map_err(|error| error.to_string())?);
        let mut count = 0u64;
        let mut rank = 0;
        let mut final_counts = false;
        loop {
            require(
                Instant::now() < deadline,
                "ResourceIncomplete: temporal import deadline",
            )?;
            let mut line = Vec::new();
            let read = std::io::Read::by_ref(&mut reader)
                .take(max_record + 2)
                .read_until(b'\n', &mut line)
                .map_err(|error| error.to_string())?;
            if read == 0 {
                break;
            }
            if line.last() == Some(&b'\n') {
                line.pop();
            }
            require(
                line.len() as u64 <= max_record,
                "ResourceIncomplete: indivisible temporal record byte limit",
            )?;
            let record = catalog_result(paged::Record::parse_bounded(
                &line,
                catalog.limits().resident_bytes,
            ))?;
            require(record.bytes() == line, "NoncanonicalTemporalEnvelope")?;
            let current = match record.kind.as_str() {
                "MonitorNode" => 0,
                "MonitorEdge" => 1,
                "Scc" => 2,
                "Counterexample" => 3,
                "Outcome" => 4,
                "ContextRoot" => 5,
                "Counts" => 6,
                _ => return Err("UnexpectedTemporalRecordKind".into()),
            };
            require(current >= rank && !final_counts, "TemporalPublicationOrder")?;
            rank = current;
            if record.kind == "ContextRoot" {
                require(
                    prior.record["contexts"]
                        .as_array()
                        .ok_or("TemporalInputContexts")?
                        .contains(&record.record),
                    "FinalContextRootSourceSnapshot",
                )?;
            }
            if record.kind == "Counts" {
                require(
                    final_batch && record.record == temporal_counts(catalog)?,
                    "PrematureOrIncorrectFinalCounts",
                )?;
                let mapping: paged::CatalogResult<Value> = paged::strict_json(
                    include_str!(
                        "../../../../verification/async-driver-refinement/paged-outcomes.json"
                    )
                    .as_bytes(),
                );
                let mapping = catalog_result(mapping)?;
                let families = mapping["families"]
                    .as_array()
                    .ok_or("OutcomeFamilyInventory")?;
                require(
                    catalog_result(catalog.count("Outcome"))? == families.len() as u64,
                    "MissingFinalOutcomeFamilies",
                )?;
                for family in families {
                    let outcome = catalog_result(catalog.get(
                        "Outcome",
                        &paged::RecordKey::Text(
                            catalog_result(paged::string(family, "family"))?.into(),
                        ),
                    ))?;
                    require(
                        outcome.record["expected"] == family["expected_result"]
                            && outcome.record["actual"] == family["expected_result"],
                        "FinalOutcomeFamilyResult",
                    )?;
                }
                final_counts = true;
            }
            match catalog.get(&record.kind, &catalog_result(record.key())?) {
                Ok(existing) => require(
                    existing.bytes() == record.bytes(),
                    "TemporalImmutableRecordConflict",
                )?,
                Err(paged::CatalogError::MissingDependency(_)) => {
                    require(
                        catalog_result(catalog.count("Counts"))? == 0,
                        "TemporalRecordsAfterFinalCounts",
                    )?;
                    if matches!(record.kind.as_str(), "MonitorNode" | "MonitorEdge" | "Scc") {
                        require(
                            catalog_result(catalog.allocate(&record.kind))?
                                == catalog_result(paged::number(&record.record, "id"))?,
                            "DeterministicTemporalGlobalId",
                        )?;
                    }
                    catalog_result(catalog.append_record(&record.kind, record.record))?;
                }
                Err(error) => return Err(error.to_string()),
            }
            count = count
                .checked_add(1)
                .ok_or("TemporalRecordCounterOverflow")?;
        }
        require(
            count == catalog_result(paged::number(&side, "record_count"))?
                && (!final_batch || final_counts),
            "TemporalPublicationCompleteRecordCount",
        )?;
        require(
            catalog_result(paged::hash_file(
                path,
                catalog.limits().unit_bytes,
                deadline,
            ))? == (bytes, hash.clone()),
            "TemporalInputChangedDuringImport",
        )?;
        Ok::<_, String>(())
    })();
    match result {
        Ok(()) => {
            catalog_result(transaction.seal_temporal())?;
            Ok(complete)
        }
        Err(reason) => {
            catalog_result(transaction.abort())?;
            Err(reason)
        }
    }
}
fn publish_paged_checkpoint(
    request: &paged::QuantumRequest,
    catalog: &paged::Catalog,
    image: &paged::Record,
    mut unit: paged::Record,
    stage_complete: bool,
    blocked: bool,
    deadline: Instant,
) -> Result<(), String> {
    let snapshot =
        catalog_result(catalog.pin_read_snapshot(catalog_result(catalog.current_transaction())?))?;
    let limits = catalog.limits();
    let page_cap = paged::PAGE_BYTES
        .min(
            limits
                .allowance("Page")
                .map_err(|error| error.to_string())?
                .max_encoded_bytes,
        )
        .min(limits.resident_bytes / 4);
    let export = catalog_result(
        snapshot.export(
            &request.output_dir,
            request.checkpoint.as_ref(),
            &paged::PageLimits {
                max_records: 64,
                max_member_bytes: page_cap,
                max_manifest_node_bytes: (8 * 1024 * 1024)
                    .min(
                        limits
                            .allowance("ManifestNode")
                            .map_err(|error| error.to_string())?
                            .max_encoded_bytes,
                    )
                    .min(limits.resident_bytes / 4),
            },
            deadline,
        ),
    )?;
    let sequence = request
        .checkpoint
        .as_ref()
        .map(|prior| {
            paged::number(&prior.record, "sequence").and_then(|value| {
                value
                    .checked_add(1)
                    .ok_or_else(|| paged::CatalogError::NumericDomain("checkpoint sequence".into()))
            })
        })
        .transpose()
        .map_err(|error| error.to_string())?
        .unwrap_or(0);
    let checkpoint = catalog_result(paged::Record::new(
        "Checkpoint",
        json!({"session_id":catalog.session_id(),"sequence":sequence,"source":catalog.source(),"initial_image_id":image.record["image_sha256"],"resource_plan_sha256":catalog.resource_plan().sha256(),"unit_ordinal":unit.record["unit_ordinal"],"previous_checkpoint":request.checkpoint.as_ref().map(paged::Record::sha256),"committed_transaction":snapshot.transaction().get(),"oldest_pending_ticket":catalog_result(catalog.oldest_pending())?.map(|(ticket,_)|ticket.get()),"counts":catalog_result(snapshot.counts())?,"contexts":catalog_result(snapshot.context_snapshots())?,"page_root":export.root.id,"catalog_logical_digest":catalog_result(snapshot.logical_digest())?}),
    ))?;
    catalog_result(snapshot.close())?;
    for member in &export.members {
        let record = catalog_result(paged::Record::parse(
            &std::fs::read(request.output_dir.join(member)).map_err(|error| error.to_string())?,
        ))?;
        catalog_result(catalog.register_export(&record))?;
    }
    unit.record["status"] = json!(if stage_complete {
        "closed_unqualified"
    } else {
        "checkpointed_incomplete"
    });
    catalog_result(unit.validate())?;
    let phase = if blocked {
        paged::SessionCheckpointPhase::ResourceBlocked
    } else if stage_complete {
        paged::SessionCheckpointPhase::ClosedUnqualified
    } else {
        paged::SessionCheckpointPhase::CheckpointedIncomplete
    };
    catalog_result(catalog.record_checkpoint_phase(&checkpoint, &unit, phase))?;
    let session: paged::Record = serde_json::from_value(
        catalog_result(catalog.control("session"))?.ok_or("MissingPrivateSession")?,
    )
    .map_err(|error| error.to_string())?;
    let checkpoint_name = format!("checkpoint-{}.json", checkpoint.sha256());
    let session_name = format!("session-{}.json", session.sha256());
    write_paged_json(
        &request.output_dir,
        &checkpoint_name,
        &checkpoint,
        paged::PAGE_BYTES,
    )?;
    write_paged_json(
        &request.output_dir,
        &session_name,
        &session,
        paged::PAGE_BYTES,
    )?;
    let auxiliary = catalog_result(paged::auxiliary_directory(&request.output_dir))?;
    std::fs::create_dir_all(&auxiliary).map_err(|error| error.to_string())?;
    let mut backup = catalog_result(
        catalog.backup_checkpoint_catalog(
            &checkpoint,
            &auxiliary.join("catalog-backup.sqlite"),
            &paged::BackupLimits {
                max_bytes: catalog_result(limits.pool("snapshot_spool_bytes"))?
                    .min(catalog_result(limits.family("catalog_main"))?.database_bytes),
                deadline,
            },
        ),
    )?;
    backup["execution_origin"] = request.execution_origin.clone();
    write_paged_json(
        &request.output_dir,
        "backup-descriptor.json",
        &backup,
        1024 * 1024,
    )?;
    let mut members = export.members;
    members.extend([
        checkpoint_name,
        session_name,
        "initial-image.sqlite".into(),
        "schema-inventory.json".into(),
        "driver-graph.json".into(),
    ]);
    write_paged_json(
        &request.output_dir,
        "unit-result.json",
        &json!({"schema_version":6,"work_unit":unit.record,"checkpoint":checkpoint,"initial_image":image,"session":session,"artifacts":members,"stage_complete":stage_complete}),
        8 * 1024 * 1024,
    )?;
    Ok(())
}
async fn run_paged_quantum(
    request: paged::QuantumRequest,
    runtime: &mut RuntimeDiagnostics,
) -> Result<(), String> {
    let deadline = catalog_result(request.deadline())?;
    catalog_result(request.verify_source(&compiled_paged_sources(), deadline))?;
    let mut unit = catalog_result(request.unit())?;
    let ordinal = catalog_result(paged::number(&unit.record, "unit_ordinal"))?;
    std::fs::create_dir_all(&request.output_dir).map_err(|error| error.to_string())?;
    let fresh = request.checkpoint.is_none();
    if fresh {
        require(
            request.operation == "expand"
                && ordinal == 0
                && request.prior_checkpoint.is_none()
                && request.initial_image.is_none()
                && unit.record["expected_prior_checkpoint"].is_null(),
            "FreshInitRequiresFirstExpansion",
        )?;
        require(
            matches!(request.catalog_path.symlink_metadata(),Err(error) if error.kind()==std::io::ErrorKind::NotFound),
            "FreshInitRefusesPreexistingCatalog",
        )?;
    }
    let config = catalog_result(request.config())?;
    if !fresh {
        let readonly = catalog_result(paged::Catalog::resumed_view(&request.operation, config.clone()))?;
        let prior = catalog_result(request.check_continuation(&readonly))?;
        let (bytes, hash) = catalog_result(request.expected_backup(&prior, deadline))?;
        let receipt = catalog_result(
            readonly.validate_restore(
                &prior,
                bytes,
                &hash,
                &paged::BackupLimits {
                    max_bytes: config.resource_plan.record["total_session_disk_bytes"]
                        .as_u64()
                        .ok_or("RestoreDiskBound")?,
                    deadline,
                },
            ),
        )?;
        if request.operation == "validate_restore" {
            let image_path = catalog_result(request.initial_path(deadline))?;
            let oracle = Oracle::from_catalog(
                readonly,
                &image_path,
                &paged::BackupLimits {
                    max_bytes: paged::PAGE_BYTES,
                    deadline,
                },
            )
            .await?;
            drop(oracle);
            write_paged_json(
                &request.output_dir,
                "restore-validation.json",
                &receipt,
                1024 * 1024,
            )?;
            return Ok(());
        }
    }
    let catalog = catalog_result(paged::Catalog::disk(config))?;
    let mut oracle;
    let image = if fresh {
        oracle = Oracle::new(None).await?;
        let capture = oracle
            .capture_initial_image(
                &request.output_dir.join("initial-image.sqlite"),
                &paged::BackupLimits {
                    max_bytes: paged::PAGE_BYTES.min(catalog_result(
                        catalog.limits().pool("snapshot_spool_bytes"),
                    )?),
                    deadline,
                },
            )
            .await?;
        write_paged_json(
            &request.output_dir,
            "schema-inventory.json",
            Oracle::capture_schema(&capture),
            paged::PAGE_BYTES,
        )?;
        initialize_paged_catalog(&mut oracle, &catalog, &capture)?
    } else {
        let image = request.initial_image.clone().ok_or("MissingResumedInit")?;
        let path = catalog_result(request.initial_path(deadline))?;
        oracle = Oracle::from_catalog(
            catalog.clone(),
            &path,
            &paged::BackupLimits {
                max_bytes: paged::PAGE_BYTES,
                deadline,
            },
        )
        .await?;
        copy_paged_member(
            &path,
            &request.output_dir.join("initial-image.sqlite"),
            catalog_result(paged::string(&image.record, "image_sha256"))?,
            paged::PAGE_BYTES,
            deadline,
        )?;
        let reference: paged::RecordRef =
            serde_json::from_value(image.record["schema_inventory"].clone())
                .map_err(|error| error.to_string())?;
        let schema = catalog_result(request.dependency_member(&reference, deadline))?;
        require(
            paged::digest(paged::canonical(&oracle.schema).as_bytes()) == reference.sha256,
            "ActualRestoredInventoryBytes",
        )?;
        copy_paged_member(
            &schema,
            &request.output_dir.join("schema-inventory.json"),
            &reference.sha256,
            paged::PAGE_BYTES,
            deadline,
        )?;
        image
    };
    copy_paged_member(
        &request.driver_graph,
        &request.output_dir.join("driver-graph.json"),
        &catalog.source().driver_sha256,
        paged::PAGE_BYTES,
        deadline,
    )?;
    if request.operation == "temporal_import" {
        let complete = import_temporal_unit(&request, &catalog, deadline)?;
        oracle.restore_root().await?;
        return publish_paged_checkpoint(
            &request, &catalog, &image, unit, complete, false, deadline,
        );
    }
    require(request.operation == "expand", "UnexpectedQuantumOperation")?;
    let driver_bytes = std::fs::read(&request.driver_graph).map_err(|error| error.to_string())?;
    let driver = Driver::parse(&driver_bytes)?;
    let work_deadline = deadline
        - Duration::from_secs(
            catalog_result(paged::number(&request.resource_plan.record, "unit_seconds"))? / 4,
        );
    let mut completed = 0u64;
    let mut blocked = false;
    while catalog_result(catalog.oldest_pending())?.is_some() {
        if completed >= catalog.limits().unit_predecessors || Instant::now() >= work_deadline {
            break;
        }
        match expand_paged_predecessor(&catalog, &mut oracle, &driver, ordinal, work_deadline).await
        {
            Ok(()) => completed += 1,
            Err(reason) if reason.starts_with("ResourceIncomplete") => {
                if completed == 0 {
                    return Err(reason);
                }
                blocked = !ordinary_unit_exhaustion(&reason);
                runtime.emit("resource_incomplete",json!({"scope":if blocked {"immutable_plan"}else{"unit"},"completed_predecessors":completed}));
                break;
            }
            Err(reason) => return Err(reason),
        }
        if runtime.last_progress.elapsed() >= Duration::from_secs(30) {
            runtime.emit(
                "paged_expansion",
                json!({"completed_predecessors":completed,"oracle":oracle.diagnostic_summary()}),
            );
            runtime.last_progress = Instant::now();
        }
    }
    require(
        completed > 0 || catalog_result(catalog.oldest_pending())?.is_none(),
        "ResourceIncomplete: no complete predecessor fits fresh unit",
    )?;
    let pending = catalog_result(catalog.oldest_pending())?;
    let complete = pending.is_none();
    let (context, ticket) = if let Some((ticket, product)) = pending {
        let record = catalog_result(catalog.get_id("Product", product.get()))?;
        (record.record["context"].clone(), json!(ticket.get()))
    } else {
        (Value::Null, Value::Null)
    };
    unit.record["progress"] = json!({"context":context,"record_kind":"Expansion","after_key":if catalog_result(catalog.count("Expansion"))?>0 {json!(catalog_result(catalog.count("Expansion"))?)}else{Value::Null},"fifo_ticket":ticket,"component_id":null,"family":null,"case_index":0});
    oracle.restore_root().await?;
    publish_paged_checkpoint(
        &request, &catalog, &image, unit, complete, blocked, deadline,
    )
}

fn driver_input() -> Result<(Driver, Vec<u8>), String> {
    let path = std::env::var("PLURX_ASYNC_DRIVER_GRAPH")
        .map_err(|_| "DriverInventory: PLURX_ASYNC_DRIVER_GRAPH required")?;
    let file = std::fs::File::open(path).map_err(|e| format!("DriverInventory: {e}"))?;
    let mut bytes = Vec::new();
    file.take(MEMBER_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("DriverInventory: {e}"))?;
    require(
        bytes.len() <= MEMBER_BYTES,
        "DriverInventory: graph exceeds member bound",
    )?;
    Ok((Driver::parse(&bytes)?, bytes))
}
#[tokio::test]
#[ignore = "requires registered schema6 paged request"]
async fn actual_sql_two_worker_product_is_closed() {
    let mut runtime = RuntimeDiagnostics::new();
    runtime.emit("paged_test_enter", json!({}));
    let path = std::path::PathBuf::from(
        std::env::var_os("PLURX_ASYNC_PAGED_REQUEST")
            .expect("registered schema6 paged request required"),
    );
    let request =
        paged::QuantumRequest::read(&path).expect("closed exact source-bound quantum request");
    run_paged_quantum(request, &mut runtime)
        .await
        .expect("exact bounded paged operation; never page-as-proof acceptance");
    runtime.emit("paged_operation_complete", json!({"acceptance":false}));
}

pub(super) fn initialize_paged_catalog(
    oracle: &mut Oracle,
    catalog: &paged::Catalog,
    capture: &sql::InitialCapture,
) -> Result<paged::Record, String> {
    let transaction = catalog.begin_initial(0).map_err(|e| e.to_string())?;
    let contexts = contexts();
    catalog
        .set_control(
            "context_descriptors",
            &json!(contexts
                .iter()
                .map(|c| json!({"id":c.id,"stable_success":c.stable,"mutation":c.mutation}))
                .collect::<Vec<_>>()),
        )
        .map_err(|e| e.to_string())?;
    let image = oracle.attach_catalog_initial(catalog, capture)?;
    let mut roots = Vec::new();
    for context in contexts {
        let mut initial = Valuation {
            sql: 0,
            w: [Worker::idle(), Worker::idle()],
            available: [true, true],
            completed: done(oracle, 0),
            mono_now_ms: 0,
            wall_anchors: Vec::new(),
            mono_anchors: Vec::new(),
            claim_loss_keys: Vec::new(),
            real_drivers: [None, None],
        };
        retain_anchors(&mut initial, oracle);
        let product = catalog
            .intern_product(
                context.id,
                &canonical(&product_payload(&context, &initial)),
                paged::StoreId::new(1).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
        roots.push((context.id.into(), product.get()));
    }
    transaction
        .seal_initial(image.clone(), &roots)
        .map_err(|e| e.to_string())?;
    Ok(image)
}

/// Bounded backend equivalence and atomic resource rollback, never a closed
/// product proof. Every selected predecessor is expanded with its COMPLETE D.
#[tokio::test]
#[ignore = "requires the registered actual driver graph input"]
async fn product_paged_disk_and_reference_complete_predecessors_agree() {
    let (driver, _) = driver_input().expect("actual source-bound driver DAG");
    let directory = tempfile::tempdir().expect("owned persistent fixture scratch");
    let path = directory.path().join("initial-image.sqlite");
    let limits = paged::BackupLimits {
        max_bytes: paged::PAGE_BYTES,
        deadline: Instant::now() + Duration::from_secs(300),
    };
    let mut actual = Oracle::new(None)
        .await
        .expect("actual fresh source constructor");
    let capture = actual
        .capture_initial_image(&path, &limits)
        .await
        .expect("one exact raw Init image");
    let mut reference = Oracle::from_captured_image(&path, &capture, &limits)
        .await
        .expect("same actual image, no fresh UUIDs");
    let config = paged::fixture_configuration(directory.path().join("catalog.sqlite"));
    let disk = paged::Catalog::disk(config.clone()).expect("SQLite catalog");
    let memory = paged::Catalog::memory(config.clone(), 4096, 16 * 1024 * 1024)
        .expect("bounded independent reference");
    initialize_paged_catalog(&mut actual, &disk, &capture).expect("actual SQLite Init roots");
    initialize_paged_catalog(&mut reference, &memory, &capture)
        .expect("same actual reference Init roots");
    for expected in 1..=2 {
        assert_eq!(
            disk.oldest_pending()
                .expect("disk FIFO")
                .expect("pending")
                .1
                .get(),
            expected
        );
        assert_eq!(
            disk.oldest_pending().expect("disk FIFO"),
            memory.oldest_pending().expect("reference FIFO")
        );
        expand_paged_predecessor(&disk, &mut actual, &driver, 1, limits.deadline)
            .await
            .expect("complete actual SQLite predecessor");
        expand_paged_predecessor(&memory, &mut reference, &driver, 1, limits.deadline)
            .await
            .expect("complete reference predecessor");
        let a = disk
            .pin_read_snapshot(disk.current_transaction().expect("disk sequence"))
            .expect("disk snapshot");
        let b = memory
            .pin_read_snapshot(memory.current_transaction().expect("reference sequence"))
            .expect("reference snapshot");
        assert_eq!(
            a.counts().expect("disk counts"),
            b.counts().expect("reference counts")
        );
        assert_eq!(
            a.logical_digest().expect("disk full bytes"),
            b.logical_digest().expect("reference full bytes")
        );
        assert!(
            a.counts().expect("prefix counts")["frontier"]
                .as_u64()
                .expect("frontier")
                > 0,
            "a bounded comparison prefix is never closure"
        );
        a.close().expect("close disk snapshot");
        b.close().expect("close reference snapshot");
    }
    let mut constrained = config;
    let mut plan = constrained.resource_plan.record.clone();
    plan["unit_record_allowance"] = json!(16);
    let provisional: paged::Record =
        serde_json::from_value(json!({"schema_version":6,"kind":"ResourcePlan","record":plan}))
            .expect("operational fixture shape");
    let mut plan = provisional.record.clone();
    plan["plan_sha256"] = json!(provisional.sha256());
    constrained.resource_plan =
        paged::Record::new("ResourcePlan", plan).expect("fixed small operational quota");
    let limited =
        paged::Catalog::memory(constrained, 4096, 16 * 1024 * 1024).expect("bounded quota fixture");
    let mut limited_oracle = Oracle::from_captured_image(&path, &capture, &limits)
        .await
        .expect("same exact initial image");
    initialize_paged_catalog(&mut limited_oracle, &limited, &capture)
        .expect("Init fits the immutable operational quota");
    let initial = limited
        .pin_read_snapshot(limited.current_transaction().expect("Init commit"))
        .expect("pin Init");
    let before = initial.logical_digest().expect("Init identity");
    initial.close().expect("close Init");
    let frontier = limited.oldest_pending().expect("initial FIFO");
    let failure =
        expand_paged_predecessor(&limited, &mut limited_oracle, &driver, 1, limits.deadline)
            .await
            .expect_err("whole predecessor exceeds the immutable operational quota");
    assert!(
        failure.starts_with("ResourceIncomplete"),
        "unexpected first failure: {failure}"
    );
    assert_eq!(limited.oldest_pending().expect("retained FIFO"), frontier);
    let after = limited
        .pin_read_snapshot(limited.current_transaction().expect("unchanged commit"))
        .expect("pin rollback");
    assert_eq!(
        before,
        after.logical_digest().expect("full rollback identity")
    );
    after.close().expect("close rollback");
    limited_oracle
        .restore_root()
        .await
        .expect("actual root remains usable after catalog rollback");
}

pub(super) fn catalog_fixture_payload(id: &str) -> Value {
    let context = contexts()
        .into_iter()
        .find(|context| context.id == id)
        .expect("catalog fixture context");
    product_payload(&context, &wire_fixture_valuation())
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
    // Encoding fixture only: full canonical values replace schema5 value/worker
    // pools. No synthetic descriptor is a Store-D correctness certificate.
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
    let mut full_keys = BTreeSet::new();
    for (index, value) in values.iter().enumerate() {
        let record=paged::Record::new("Candidate",json!({"id":index+1,"context":"base","predecessor":1,"ordinal":index,"family":"encoding_fixture","worker":null,"choice_utf8":canonical(value),"descriptor_utf8":"{}","classification":"eligible","reason":null})).expect("closed full value carrier");
        let decoded = paged::Record::parse(&record.bytes()).expect("full envelope roundtrip");
        let text = decoded.record["choice_utf8"]
            .as_str()
            .expect("full canonical value");
        assert!(full_keys.insert(text.to_owned()));
        assert_eq!(
            serde_json::from_str::<Value>(text).expect("exact value"),
            *value
        );
    }
    assert_eq!(canonical(&values[10]), canonical(&json!({"a":1,"b":2})));
    assert_ne!(canonical(&values[8]), canonical(&values[9]));
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
        let encoded = canonical(&worker);
        let decoded: Worker = serde_json::from_str(&encoded).expect("all actual worker fields");
        assert_eq!(worker, decoded);
        assert_eq!(
            serde_json::to_value(&worker)
                .expect("worker")
                .as_object()
                .expect("worker object")
                .len(),
            37
        );
        let mut value = wire_fixture_valuation();
        value.w[0] = worker.clone();
        let bytes = canonical(&product_payload(&contexts()[0], &value));
        let record=paged::Record::new("Product",json!({"id":1,"context":"base","store_id":1,"canonical_utf8":bytes,"sha256":digest(bytes.as_bytes()),"predecessor_edge":null})).expect("full Product encoding");
        assert_eq!(
            decode_paged_product(&record)
                .expect("actual paged decoder")
                .w[0],
            worker
        );
    }
}

#[test]
fn product_wire_compact_keys_are_bijective_with_full_logical_payloads() {
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
    for (index, (context, value)) in fixtures.into_iter().enumerate() {
        let logical = canonical(&product_payload(&contexts[context], &value));
        let record=paged::Record::new("Product",json!({"id":index+1,"context":contexts[context].id,"store_id":value.sql+1,"canonical_utf8":logical,"sha256":digest(logical.as_bytes()),"predecessor_edge":null})).expect("closed exact Product");
        let restored = decode_paged_product(&record).expect("actual paged decoder");
        assert_eq!(
            canonical(&product_payload(&contexts[context], &restored)),
            logical
        );
        let key = canonical(&json!([
            record.record["context"],
            record.record["canonical_utf8"]
        ]));
        for (previous_key, previous_logical) in &checked {
            assert_eq!(*previous_key == key, *previous_logical == logical);
        }
        checked.push((key, logical));
        let mut wrong = record.clone();
        wrong.record["store_id"] = json!(value.sql + 2);
        assert!(decode_paged_product(&wrong).is_err());
        let mut changed = restored;
        changed.w[0].stage = "private mutation".into();
        assert_eq!(
            decode_paged_product(&record)
                .expect("immutable full value")
                .w,
            value.w
        );
        assert!(decode_paged_product(&record)
            .expect("private values are reconstructed, never trusted wire pointers")
            .real_drivers
            .iter()
            .all(Option::is_none));
    }
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
    let payload = canonical(&value);
    let record=paged::Record::new("PhysicalRows",json!({"id":1,"canonical_utf8":payload,"utf8_bytes":payload.len(),"sha256":digest(payload.as_bytes())})).expect("full outer record");
    let full = record.bytes().len() as u64;
    assert_eq!(
        record
            .encoded_len(full)
            .expect("full header/escape accounting"),
        full
    );
    assert!(record.encoded_len(full - 1).is_err());
    assert!(full > payload.len() as u64);
}

#[tokio::test]
async fn product_wire_store_export_omits_only_redundant_raw() {
    let directory = tempfile::tempdir().expect("owned actual image fixture");
    let path = directory.path().join("initial-image.sqlite");
    let limits = paged::BackupLimits {
        max_bytes: paged::PAGE_BYTES,
        deadline: Instant::now() + Duration::from_secs(300),
    };
    let mut oracle = Oracle::new(None).await.expect("actual original fixture");
    let original = oracle.state(0).canonical;
    let capture = oracle
        .capture_initial_image(&path, &limits)
        .await
        .expect("actual raw image");
    let catalog = paged::Catalog::memory(
        paged::fixture_configuration(directory.path().join("catalog.sqlite")),
        4096,
        16 * 1024 * 1024,
    )
    .expect("bounded actual catalog");
    initialize_paged_catalog(&mut oracle, &catalog, &capture).expect("actual checked Store");
    let store = catalog.get_id("Store", 1).expect("checked actual root");
    assert!(store.record.get("raw").is_none());
    assert_eq!(store.record["canonical_utf8"], original);
    let physical = catalog
        .get_id(
            "PhysicalRows",
            store.record["physical_rows_id"]
                .as_u64()
                .expect("physical ID"),
        )
        .expect("exact rows");
    let episode = catalog
        .get_id(
            "Episode",
            store.record["episode_id"].as_u64().expect("episode ID"),
        )
        .expect("exact Episode");
    let raw = serde_json::from_str::<Value>(&original).expect("original full canonical");
    assert_eq!(
        serde_json::from_str::<Value>(physical.record["canonical_utf8"].as_str().expect("rows"))
            .expect("exact raw rows"),
        raw["rows"]
    );
    assert_eq!(
        serde_json::from_str::<Value>(episode.record["canonical_utf8"].as_str().expect("Episode"))
            .expect("exact Episode"),
        raw["episode"]
    );
    assert_eq!(store.record["sha256"], digest(original.as_bytes()));
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
    // The named invariant failure is a poison-on-failure event: `apply` already
    // rolled the actual writer back to the parent and read the whole endpoint, and
    // the oracle refuses every further materialization, including a root restore.
    // The mutated fixture ends here, so require that refusal instead of a restore.
    require(
        mutant
            .restore_root()
            .await
            .err()
            .is_some_and(|error| error.starts_with("MaterializationPoisoned:")),
        "CausalPrefix: named violation must poison the mutated oracle",
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
    oracle.restore_root().await?;
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
        oracle.restore_root().await.expect("verified Init teardown");
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
        oracle
            .restore_root()
            .await
            .expect("verified selector Init teardown");
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
    oracle.restore_root().await.expect("verified Init teardown");
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
    oracle.restore_root().await.expect("verified Init teardown");
}

#[test]
fn product_runtime_diagnostic_records_are_bounded() {
    let mut diagnostics = RuntimeDiagnostics::new();
    let value = wire_fixture_valuation();
    let before = canonical(&product_payload(&contexts()[0], &value));
    let oversized = diagnostics
        .record(
            "fixture",
            json!({"oversized":"x".repeat(DIAGNOSTIC_RECORD_BYTES)}),
        )
        .expect("bounded record");
    assert!(oversized.len() <= DIAGNOSTIC_RECORD_BYTES);
    assert_eq!(
        serde_json::from_str::<Value>(&oversized).expect("JSON")["phase"],
        "diagnostic_record_omitted"
    );
    for _ in 1..DIAGNOSTIC_RECORDS {
        let text = diagnostics
            .record("expansion", json!({"expanded":42}))
            .expect("bounded record");
        assert!(text.len() <= DIAGNOSTIC_RECORD_BYTES);
        let row: Value = serde_json::from_str(&text).expect("diagnostic JSON");
        assert_eq!(row["facts"]["expanded"], 42);
        assert!(row.get("accepted").is_none());
    }
    assert!(diagnostics.record("checkpoint", json!({})).is_none());
    assert_eq!(
        before,
        canonical(&product_payload(&contexts()[0], &value)),
        "bounded host diagnostics cannot mutate exact model values"
    );
}

#[test]
fn product_schema6_factored_records_preserve_original_canonical_bytes_and_bounds() {
    let rows = r#"[["raw\nUUID",1,null],["same",2]]"#;
    let episode = r#"{"now":1000}"#;
    let original = format!("{{\"rows\":{rows},\"episode\":{episode}}}");
    let physical=paged::Record::new("PhysicalRows",json!({"id":1,"canonical_utf8":rows,"utf8_bytes":rows.len(),"sha256":digest(rows.as_bytes())})).expect("full original rows");
    let episode_record=paged::Record::new("Episode",json!({"id":1,"canonical_utf8":episode,"utf8_bytes":episode.len(),"sha256":digest(episode.as_bytes())})).expect("full original Episode");
    let store=paged::Record::new("Store",json!({"id":1,"physical_rows_id":1,"episode_id":1,"canonical_utf8":original,"sha256":digest(original.as_bytes()),"initial_image_id":"a".repeat(64),"omitted_observation_id":1,"replay_parent_edge":null,"certification":"actual_init_checked","sqlite_schema_sha256":"b".repeat(64)})).expect("opaque full canonical Store");
    for record in [&physical, &episode_record, &store] {
        let restored = paged::Record::parse(&record.bytes()).expect("full envelope roundtrip");
        assert_eq!(
            restored.record["canonical_utf8"],
            record.record["canonical_utf8"]
        );
        let full = record.bytes().len() as u64;
        assert_eq!(record.encoded_len(full).expect("exact bound"), full);
        assert!(record.encoded_len(full - 1).is_err());
    }
    let raw: Value =
        serde_json::from_str(store.record["canonical_utf8"].as_str().expect("original"))
            .expect("full payload");
    assert_eq!(
        raw["rows"],
        serde_json::from_str::<Value>(rows).expect("original rows")
    );
    assert_eq!(
        raw["episode"],
        serde_json::from_str::<Value>(episode).expect("original Episode")
    );
    let mut changed = store.clone();
    changed.record["canonical_utf8"] = json!(original.replace("1000", "1001"));
    assert!(
        changed.validate().is_err(),
        "full raw value cannot reuse its original hash"
    );
    let mut invalid = physical.clone();
    invalid.record["id"] = json!(0);
    assert!(invalid.validate().is_err());
    invalid = physical;
    invalid.record["utf8_bytes"] = json!(0);
    assert!(invalid.validate().is_err());
}

#[test]
fn product_schema6_page_dependencies_and_outcomes_preserve_full_values() {
    let source =
        paged::fixture_configuration(std::path::PathBuf::from("unused-encoding-fixture")).source;
    let branch = json!({"family":"DriverReturn","worker":0,"choice":{"raw":"job-λ","flag":false,"sample":29_999}});
    let edge=paged::Record::new("ProductEdge",json!({"id":1,"context":"base","from_product":1,"to_product":2,"candidate":7,"branch_utf8":canonical(&branch),"primary_edge":3,"resolution_edge":null,"read_edge":9,"cleanup_edge":null})).expect("full edge fields");
    let dependencies = vec![
        paged::RecordRef {
            kind: "Product".into(),
            id: paged::RecordKey::Numeric(1),
            sha256: "1".repeat(64),
        },
        paged::RecordRef {
            kind: "Product".into(),
            id: paged::RecordKey::Numeric(2),
            sha256: "2".repeat(64),
        },
        paged::RecordRef {
            kind: "Candidate".into(),
            id: paged::RecordKey::Numeric(7),
            sha256: "3".repeat(64),
        },
        paged::RecordRef {
            kind: "OracleEvidence".into(),
            id: paged::RecordKey::Numeric(3),
            sha256: "4".repeat(64),
        },
        paged::RecordRef {
            kind: "OracleEvidence".into(),
            id: paged::RecordKey::Numeric(9),
            sha256: "5".repeat(64),
        },
    ];
    let page=paged::Record::addressed("Page",json!({"session_id":uuid::Uuid::from_u128(1),"source":source,"page_id":"0".repeat(64),"kind":"ProductEdge","first_key":1,"last_key":1,"record_count":1,"utf8_bytes":0,"records":[edge],"dependencies":dependencies,"previous_page":null,"previous_checkpoint":null,"transaction_high_water":1}),paged::PAGE_BYTES).expect("closed addressed page");
    let restored = paged::Record::parse(&page.bytes()).expect("page roundtrip");
    assert_eq!(restored.bytes(), page.bytes());
    let restored_edge: paged::Record =
        serde_json::from_value(restored.record["records"][0].clone()).expect("full edge");
    assert_eq!(
        serde_json::from_str::<Value>(
            restored_edge.record["branch_utf8"]
                .as_str()
                .expect("branch")
        )
        .expect("branch payload"),
        branch
    );
    let targets = paged::record_links(&restored_edge)
        .expect("explicit full edge links")
        .into_iter()
        .map(|link| (link.kind, link.key))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        targets,
        dependencies
            .iter()
            .map(|reference| (reference.kind.clone(), reference.id.clone()))
            .collect()
    );
    let mut wrong = page.clone();
    wrong.record["dependencies"][0]["sha256"] = json!("f".repeat(64));
    assert!(
        wrong.validate().is_err(),
        "address authenticates complete dependency values"
    );
    wrong = page;
    wrong.record["record_count"] = json!(2);
    assert!(wrong.validate().is_err());
    let receipt = paged::RecordRef {
        kind: "tlc_receipt".into(),
        id: paged::RecordKey::Text("component-raw-λ".into()),
        sha256: "c".repeat(64),
    };
    let outcome=paged::Record::new("Outcome",json!({"family":"encoding-fixture","expected":"named_semantic_counterexample","actual":"named_semantic_counterexample","component_receipts":[receipt],"invocation_count":3,"witness":{"kind":"Counterexample","id":"encoding-fixture","sha256":"d".repeat(64)}})).expect("full sealed outcome carrier");
    let decoded = paged::Record::parse(&outcome.bytes()).expect("outcome roundtrip");
    assert_eq!(decoded, outcome);
    assert_eq!(
        paged::record_links(&decoded)
            .expect("all outcome refs")
            .len(),
        2
    );
}
