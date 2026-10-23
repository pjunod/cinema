//! Exact selected two-worker product producer. The independent checker/TLA
//! validate its certificate; this producer's adjacency is never an oracle.
//! Ordinary Yield/Retry payload transfer to the actual adapter is unclosed.
//! Coverage is for the declared representative product, including actual SQL
//! and control order, rather than actual-adapter settlement payload fidelity.
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::io::Read;
use std::path::Path;

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
const FAIR: [&str; 17] = [
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
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct Valuation {
    sql: StoreHandle,
    w: [Worker; 2],
    available: [bool; 2],
    completed: Vec<usize>,
}
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
        "SettleYield" => Action::Settle {
            job,
            token,
            disposition: 0,
        },
        "SettleRetry" => Action::Settle {
            job,
            token,
            disposition: 1,
        },
        "RetireQueue" if w.retire == "resolve" => Action::Resolve { job, token },
        "RetireQueue" => Action::Settle {
            job,
            token,
            disposition: if w.retire == "second" && w.outcome == "cancel_requested" {
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
        Action::ClockAdvance { .. } | Action::RemoveOwner { .. } => true,
        Action::AcquireDomain { node } => *node == 2,
        Action::ReleaseDomain { lease } => {
            oracle.states[value.sql].episode.leases[*lease].owner_node_id == "foreign-holder"
        }
        Action::Renew { job, token } => value.w.iter().all(|w| {
            !same_claim(w, *job, *token, oracle, value.sql)
                || (w.local_token && (w.call == "ready" || w.retire == "drain"))
        }),
        _ => false,
    }
}
fn enumerate_candidates(
    context: &Context,
    value: &Valuation,
    driver: &Driver,
    oracle: &Oracle,
) -> Result<Vec<DemandSpec>, String> {
    let mut result = vec![];
    for n in 0..2 {
        let w = &value.w[n];
        if w.stage == "claim" && w.call == "sent" {
            require((0..2).contains(&w.job), "MalformedProductClaim")?;
            result.push(DemandSpec {
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
        if w.stage != "driver" || w.call != "sent" {
            continue;
        }
        let pending = driver.pending(w);
        let ordinary = matches!(
            pending,
            "AcquireDomain"
                | "Bind"
                | "SettleYield"
                | "SettleRetry"
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
                family: "Capacity",
                worker: Some(n),
                role: "capacity",
                input,
                capture,
                sample,
            });
        }
    }
    if !context.stable {
        for input in oracle.domain_inputs(value.sql) {
            if environment_allowed(input, value, oracle) {
                result.push(DemandSpec {
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
    value: &Valuation,
    spec: &DemandSpec,
    oracle: &Oracle,
) -> Result<Option<Value>, String> {
    if let Some(reason) = spec.capture {
        return Ok(Some(json!({"kind":"capture","reason":reason})));
    }
    let episode = &oracle.states[value.sql].episode;
    if spec.sample.is_some_and(|sample| sample != episode.now) {
        return Ok(Some(json!({"kind":"sample","reason":"retirement_sample"})));
    }
    let input = spec
        .input
        .as_ref()
        .ok_or("MalformedProductDemand: no input without capture exclusion")?;
    let latest = |job: usize, token: usize| {
        !episode.tokens[job][token + 1..]
            .iter()
            .any(|later| later.claim_id == episode.tokens[job][token].claim_id)
    };
    let reason = match *input {
        Action::Claim { job, .. } if episode.claims[job] >= 2 => Some("attempt_budget"),
        Action::Renew { job, token } if !latest(job, token) => Some("ordinary_revision"),
        Action::Renew { job, token }
            if episode
                .renewed_claims
                .contains(&episode.tokens[job][token].claim_id) =>
        {
            Some("renewal_budget")
        }
        Action::Settle {
            job,
            token,
            disposition,
        } if disposition < 3 && !latest(job, token) => Some("ordinary_revision"),
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
        reason.is_none() == oracle.domain_inputs(value.sql).contains(input),
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
fn successors(
    context: &Context,
    value: &Valuation,
    driver: &Driver,
    oracle: &Oracle,
    demands: &[Demand],
) -> Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    let mut push =
        |family: &str, n: Option<usize>, choice: Value, next: Valuation, edge: Option<usize>| {
            steps.push(Step {
                branch: branch(family, n, choice),
                value: next,
                edge,
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
                        if edge.result == "claimed" {
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
                        if context.stable && lost {
                            continue;
                        }
                        let mut next = value.clone();
                        next.w[n].call = "replied".into();
                        if lost {
                            next.w[n].reply = "Unknown".into();
                        }
                        next.w[n].fault_used |= lost;
                        push("ClaimReply", Some(n), json!({"lost":lost}), next, None);
                    }
                }
                "replied" => {
                    let mut next = value.clone();
                    if w.reply == "Claimed" {
                        next.w[n].stage = "driver".into();
                        next.w[n].ctrl = driver.initial as i64;
                        next.w[n].call = "ready".into();
                        next.w[n].started = true;
                        next.w[n].local_token = true;
                    } else {
                        next.w[n] = Worker::idle();
                    }
                    push("ClaimReturn", Some(n), json!({}), next, None);
                }
                _ => return Err("MalformedProductCall".into()),
            }
            continue;
        }
        if w.stage != "driver" {
            continue;
        }
        let pending = driver.pending(w);
        if driver.states[w.ctrl as usize]["terminal"] == true && !context.stable {
            let mut next = value.clone();
            next.w[n] = Worker::idle();
            push("Recycle", Some(n), json!({}), next, None);
        }
        if driver.states[w.ctrl as usize]["terminal"] == false && w.call == "ready" {
            let mut next = value.clone();
            next.w[n].call = "sent".into();
            next.w[n].read_lock = pending == "PollWork";
            next.w[n].accepted = false;
            push("Dispatch", Some(n), json!({}), next, None);
        }
        if w.call == "sent" {
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
                    if edge.result == "accepted" {
                        if let Some(next) =
                            commit_reply(value, n, edge, "WorkSucceeded", driver, oracle)
                        {
                            push("WorkSuccess", Some(n), json!({}), next, Some(edge_id(edge)));
                        }
                    }
                }
                if !context.stable {
                    for tag in ["QueueLoss", "DomainLoss", "Pressure", "WorkFailed"] {
                        if let Some(next) = local_reply(value, n, tag, driver) {
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
                push("Reply", Some(n), json!({"lost":lost}), next, None);
            }
        }
        if w.call == "replied" {
            if let Some(to) = driver.replies.get(&(w.ctrl as usize, w.reply.clone())) {
                let mut next = value.clone();
                let nw = &mut next.w[n];
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
    if !context.stable {
        for demand in demands.iter().filter(|d| d.spec.family == "Environment") {
            if let Some(id) = demand.edge {
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
                let ambiguous =
                    edge.result == "renewed" && matching.iter().any(|n| !value.w[*n].fault_used);
                for ack in [false, true] {
                    if !ack && !ambiguous {
                        continue;
                    }
                    let mut next = value.clone();
                    move_sql(&mut next, edge, oracle);
                    if edge.result == "renewed" {
                        if let Action::Renew { job, .. } = input {
                            for n in &matching {
                                if ack {
                                    next.w[*n].token =
                                        (oracle.states[edge.to].episode.tokens[*job].len() - 1)
                                            as i64;
                                }
                                next.w[*n].fault_used |= !ack;
                            }
                        }
                    }
                    push(
                        "Environment",
                        None,
                        json!({"input":input,"ack":ack}),
                        next,
                        Some(id),
                    );
                }
            }
        }
    }
    Ok(steps)
}
fn memberships(step: &Step, oracle: &Oracle) -> Vec<String> {
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

struct ProductState {
    context: usize,
    value: Valuation,
    hash: String,
    parent: Option<(usize, Branch)>,
}
struct Budget {
    bytes: usize,
    stores: usize,
    oracle_edges: usize,
}
impl Budget {
    fn charge<T: Serialize>(&mut self, value: &T, slack: usize) -> Result<(), String> {
        self.bytes = self
            .bytes
            .checked_add(encoded(value).len())
            .and_then(|n| n.checked_add(slack))
            .ok_or("ProductResourceEstimate: byte accounting overflow; incomplete")?;
        require(self.bytes<=BUNDLE_BYTES,"ProductResourceEstimate: conservative serialized output estimate exceeds 256MiB; incomplete")
    }
    fn oracle(&mut self, oracle: &Oracle) -> Result<(), String> {
        while self.stores < oracle.states.len() {
            self.charge(&oracle.export_state(self.stores), 2)?;
            self.stores += 1;
        }
        while self.oracle_edges < oracle.edges.len() {
            self.charge(&oracle.edges[self.oracle_edges], 2)?;
            self.oracle_edges += 1;
        }
        Ok(())
    }
}
fn product_payload(context: &Context, value: &Valuation) -> Value {
    json!({"context":context.id,"sql":value.sql,"w":value.w,"available":value.available,"completed":value.completed})
}
fn product_record(id: usize, state: &ProductState, contexts: &[Context]) -> Value {
    let mut record = product_payload(&contexts[state.context], &state.value);
    record["id"] = json!(id);
    record["canonical_sha256"] = json!(state.hash);
    record
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
        json!({"complete":false,"bound":reason.split(':').next().unwrap_or("ResourceError"),"reason":sql::diagnostic_action(encoded(&json!({"reason":reason}))),"context":context.id,"store_states":oracle.states.len(),"store_edges":oracle.edges.len(),"product_states":states.len(),"expanded":expanded,"pending":queue.len(),"serialized_output_estimate":budget.bytes,"estimated_retained_bytes":budget.bytes.saturating_mul(3),"predecessor":from,"predecessor_sha256":states[from].hash,"rejected_action":action.map(|a|sql::diagnostic_action(encoded(a))),"oracle_predecessor_sha256":digest(oracle.states[states[from].value.sql].canonical.as_bytes()),"oracle_successor_sha256":oracle.failure.as_ref().and_then(|v|v.get("successor_sha256")),"trace_tail":tail,"trace_complete":states[cursor].parent.is_none()})
    );
}
async fn close_product(driver: &Driver, driver_bytes: &[u8]) -> Result<Value, String> {
    let contexts = contexts();
    let mut oracle = Oracle::new(None).await?;
    let semantic = semantic_bindings()?;
    let mut budget = Budget {
        bytes: 0,
        stores: 0,
        oracle_edges: 0,
    };
    budget.charge(&json!({"domain":sql::domain_metadata(),"schema_bindings":oracle.schema,"source_bindings":oracle.source_bindings(),"semantic_bindings":semantic}),16_384)?;
    budget.oracle(&oracle)?;
    let mut states = Vec::<ProductState>::new();
    let mut index = HashMap::<String, usize>::new();
    let mut edges = Vec::<Value>::new();
    let mut demands_out = Vec::<Value>::new();
    let mut enabled = Vec::<Value>::new();
    let mut descriptors = Vec::new();
    let mut by_context = serde_json::Map::new();
    for (context_id, context) in contexts.iter().enumerate() {
        let initial = Valuation {
            sql: oracle.initial(),
            w: [Worker::idle(), Worker::idle()],
            available: [true, true],
            completed: done(&oracle, oracle.initial()),
        };
        let initial_key = canonical(&product_payload(context, &initial));
        let initial_id = states.len();
        index.insert(initial_key.clone(), initial_id);
        states.push(ProductState {
            context: context_id,
            value: initial,
            hash: digest(initial_key.as_bytes()),
            parent: None,
        });
        budget.charge(
            &product_record(initial_id, &states[initial_id], &contexts),
            32,
        )?;
        let mut queue = VecDeque::from([initial_id]);
        let mut context_states = vec![initial_id];
        let mut expanded = 0;
        let edge_start = edges.len();
        let demand_start = demands_out.len();
        while let Some(from) = queue.pop_front() {
            let value = states[from].value.clone();
            macro_rules! charge_output {
                ($record:expr, $slack:expr) => {
                    if let Err(reason) = budget.charge($record, $slack) {
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
                };
            }
            // Complete structural candidate collection precedes every oracle lookup.
            let candidates = enumerate_candidates(context, &value, driver, &oracle)?;
            let classified = candidates
                .into_iter()
                .map(|spec| {
                    let exclusion = classify_demand(&value, &spec, &oracle)?;
                    Ok((spec, exclusion))
                })
                .collect::<Result<Vec<_>, String>>()?;
            let mut demands = Vec::new();
            for (spec, exclusion) in classified {
                let edge = if exclusion.is_none() {
                    let input = spec
                        .input
                        .as_ref()
                        .ok_or("MissingProductOracleEdge: missing eligible input")?;
                    match oracle.apply(value.sql, input).await {
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
                let record = json!({"context":context.id,"from":from,"branch":{"family":spec.family,"worker":spec.worker},"role":spec.role,"input":spec.input,"oracle_edge":edge,"exclusion":exclusion});
                charge_output!(&record, 2);
                demands_out.push(record);
                demands.push(Demand { spec, edge });
            }
            let steps = successors(context, &value, driver, &oracle, &demands)?;
            let mut planned = Vec::new();
            let mut pairs = BTreeMap::<usize, BTreeSet<String>>::new();
            let mut branch_pairs = HashSet::new();
            for step in steps {
                let key = canonical(&product_payload(context, &step.value));
                let to = if let Some(to) = index.get(&key) {
                    *to
                } else {
                    let id = states.len();
                    let state = ProductState {
                        context: context_id,
                        value: step.value.clone(),
                        hash: digest(key.as_bytes()),
                        parent: Some((from, step.branch.clone())),
                    };
                    charge_output!(&product_record(id, &state, &contexts), 32);
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
                    .extend(memberships(&step, &oracle));
                planned.push((step, to));
            }
            let mut actions = BTreeSet::new();
            for (step, to) in planned {
                let fair: Vec<_> = pairs[&to].iter().cloned().collect();
                if from != to {
                    actions.extend(fair.iter().cloned());
                }
                let record = json!({"from":from,"to":to,"branch":step.branch,"oracle_edge":step.edge,"fair_actions":fair});
                charge_output!(&record, 2);
                edges.push(record);
            }
            let row = json!({"state":from,"actions":actions});
            charge_output!(&row, 2);
            enabled.push(row);
            expanded += 1;
        }
        by_context.insert(context.id.into(),json!({"state_count":context_states.len(),"edge_count":edges.len()-edge_start,"demand_count":demands_out.len()-demand_start,"frontier_count":0}));
        descriptors.push(json!({"id":context.id,"model":"AsyncDriverRefinement","stable_success":context.stable,"mutation":context.mutation,"witness":false,"initial":initial_id,"states":context_states}));
    }
    for (id, mutation) in [
        ("boundary_local_revocation", "local-revocation"),
        ("boundary_unknown_rollback", "unknown-as-rollback"),
    ] {
        descriptors.push(json!({"id":id,"model":"BoundaryControls","stable_success":false,"mutation":mutation,"witness":false,"initial":null,"states":[]}));
    }
    let coverage = json!({"complete":true,"no_skipped_demands":true,"store_state_count":oracle.states.len(),"store_edge_count":oracle.edges.len(),"product_state_count":states.len(),"product_edge_count":edges.len(),"demand_count":demands_out.len(),"by_context":by_context});
    let bundle = json!({"schema_version":2,"scope":"selected_two_worker_product","source_boundaries":{"OrdinarySettlementPayloadTransfer":"unclosed"},"driver_graph_sha256":digest(driver_bytes),"source_bindings":oracle.source_bindings(),"semantic_bindings":semantic,"schema_bindings":oracle.schema,"domain":sql::domain_metadata(),"store":{"initial":0,"states":oracle.export_states(),"edges":oracle.edges},"contexts":descriptors,"product_states":states.iter().enumerate().map(|(id,state)|product_record(id,state,&contexts)).collect::<Vec<_>>(),"product_edges":edges,"demands":demands_out,"enabledness":enabled,"coverage":coverage});
    let bundle = oracle.normalize(&bundle);
    require(
        encoded(&bundle).len() <= BUNDLE_BYTES,
        "ProductResourceLimit: exact serialized bundle exceeds 256MiB; incomplete",
    )?;
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
async fn actual_sql_two_worker_product_is_closed() {
    let (driver, bytes) = driver_input().expect("source-bound driver inventory");
    let bundle = close_product(&driver, &bytes)
        .await
        .expect("exact selected two-worker product closure");
    let path =
        std::env::var("PLURX_ASYNC_PRODUCT_BUNDLE").expect("PLURX_ASYNC_PRODUCT_BUNDLE required");
    let encoded = encoded(&bundle);
    assert!(encoded.len() <= BUNDLE_BYTES);
    std::fs::write(path, encoded).expect("write complete product bundle");
    eprintln!("async_product_coverage={}", bundle["coverage"]);
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
                    Some(Action::Settle {
                        job: 0,
                        token,
                        disposition: 0,
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
        prefix.push(json!({"driver_from":ctrl,"request":pending,"reply":reply,"driver_to":to,"primitive":input,"normal_result":result}));
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
    let fresh = Oracle::new(None).await?;
    require(
        normal.schema == fresh.schema,
        "CausalPrefix: unmutated schema differs from fresh production fixture",
    )?;
    let (prefix, primitives) = protocol_prefix(&driver, &mut normal, control).await?;
    require(
        prefix.len() <= 32 && primitives.len() <= 32,
        "CausalPrefix: trace bound",
    )?;
    let mut mutant = Oracle::new(Some(mutation)).await?;
    require(
        normal.schema != mutant.schema,
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
    let record = json!({"schema_version":1,"scope":"declared_representative_product","ordinary_settlement_payload_transfer":"unclosed","control":control,"named_failure":named,"driver_graph_sha256":digest(&driver_bytes),"source_bindings":normal.source_bindings(),"schema_bindings":{"unmutated":digest(canonical(&normal.schema).as_bytes()),"mutated":digest(canonical(&mutant.schema).as_bytes())},"mutation":mutant.mutation,"unmutated_prefix_replay":true,"prefix":prefix,"mutated_prefix":mutated_prefix,"identities":{"normal_predecessor":primitives.first().expect("prefix").before,"normal_successor":last.after,"mutated_predecessor":mutant.states[0].canonical,"mutated_successor":failure["successor_canonical_json"]},"first_causal_violation":{"step":primitives.len()-1,"name":named}});
    require(
        encoded(&record).len() <= 128 * 1024,
        "CausalPrefix: marker exceeds 128KiB; incomplete",
    )?;
    Ok(record)
}
#[tokio::test]
async fn product_prefix_completion_mutation_fails_last_request_atomic_retirement() {
    let record = causal_control(
        "C4",
        TriggerMutation::CompletionRetirement,
        "LastRequestAtomicRetirement",
    )
    .await
    .expect("C4 actual-SQL causal prefix of the declared representative product");
    eprintln!("async_store_causal_prefix={record}");
    eprintln!(
        "async_store_control={}",
        json!({"control":"C4","named_failure":"LastRequestAtomicRetirement"})
    );
}
#[tokio::test]
async fn product_prefix_reservation_mutation_fails_reservation_conservation() {
    let record = causal_control(
        "C9",
        TriggerMutation::ReservationRelease,
        "ReservationConservation",
    )
    .await
    .expect("C9 actual-SQL control-order prefix; ordinary payload transfer unclosed");
    eprintln!("async_store_causal_prefix={record}");
    eprintln!(
        "async_store_control={}",
        json!({"control":"C9","named_failure":"ReservationConservation"})
    );
}
