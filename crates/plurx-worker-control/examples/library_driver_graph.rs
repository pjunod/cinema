//! Inventory of the actual production driver's finite control quotient.
//! No temporal or safety theorem is checked by running this example.
use plurx_worker_control::library_driver::{Driver, Reply, Request, REPLY_CLASSES};

fn reply_name(reply: Reply) -> &'static str {
    match reply {
        Reply::Domain {
            acquired: false, ..
        } => "DomainRefused",
        Reply::Domain { acquired: true, .. } => "DomainAcquired",
        Reply::Bound { accepted: false } => "BindRefused",
        Reply::Bound { accepted: true } => "BindAccepted",
        Reply::Work(plurx_worker_control::library_driver::WorkOutcome::QueueLoss) => "QueueLoss",
        Reply::Work(plurx_worker_control::library_driver::WorkOutcome::DomainLoss) => "DomainLoss",
        Reply::Work(plurx_worker_control::library_driver::WorkOutcome::Pressure) => "Pressure",
        Reply::Work(plurx_worker_control::library_driver::WorkOutcome::Completed { ok: false }) => {
            "WorkFailed"
        }
        Reply::Work(plurx_worker_control::library_driver::WorkOutcome::Completed { ok: true }) => {
            "WorkSucceeded"
        }
        Reply::Joined { ok: false } => "JoinedFailed",
        Reply::Joined { ok: true } => "JoinedSucceeded",
        Reply::Released { .. } => "ReleaseReturned",
        Reply::Done => "Done",
    }
}

fn request_name(request: Option<Request>) -> String {
    request.map_or("null".into(), |request| format!("\"{:?}\"", request.kind()))
}

fn main() {
    let mut states = vec![Driver::new()];
    let mut edges = Vec::new();
    let mut rejected = Vec::new();
    let mut cursor = 0;
    while cursor < states.len() {
        let state = states[cursor];
        for reply in REPLY_CLASSES {
            let mut next = state;
            match next.drive(reply) {
                Ok(request) => {
                    let to = match states
                        .iter()
                        .position(|state| state.control() == next.control())
                    {
                        Some(index) => index,
                        None => {
                            states.push(next);
                            states.len() - 1
                        }
                    };
                    edges.push(format!(
                        "{{\"from\":{cursor},\"reply\":\"{}\",\"to\":{to},\"request\":{}}}",
                        reply_name(reply),
                        request_name(request)
                    ));
                }
                Err(_) => rejected.push(format!(
                    "{{\"state\":{cursor},\"reply\":\"{}\",\"unchanged\":{}}}",
                    reply_name(reply),
                    next.control() == state.control() && next.request() == state.request()
                )),
            }
        }
        cursor += 1;
    }
    println!("{{\"schema_version\":1,\"scope\":\"production drive finite control inventory, not qualification\",\"initial\":0,");
    println!("\"domain\":{{\"reply_classes\":[{}],\"clock_representative\":0,\"numeric_payload\":\"opaque i64; control erases Settle values only; lifecycle computes signed saturation\",\"closure\":\"BFS applies every Reply class to every discovered control state, including terminal rejection\"}},", REPLY_CLASSES.iter().map(|reply|format!("\"{}\"",reply_name(*reply))).collect::<Vec<_>>().join(","));
    println!("\"states\":[");
    for (id, driver) in states.iter().enumerate() {
        let control = driver.control();
        let remaining = control
            .remaining
            .iter()
            .filter_map(|kind| kind.map(|kind| format!("\"{kind:?}\"")))
            .collect::<Vec<_>>()
            .join(",");
        println!("{}{{\"id\":{id},\"phase\":\"{:?}\",\"held\":{{\"admission\":{},\"queue\":{},\"domain\":{},\"publisher\":{}}},\"work\":\"{:?}\",\"revoked\":{},\"cancelled\":{},\"pending\":{},\"remaining\":[{remaining}],\"terminal\":{}}}", if id==0 {""} else {","}, control.controller,control.held.admission,control.held.queue,control.held.domain,control.held.publisher,control.work,control.revoked,control.cancelled,request_name(driver.request()),control.terminal());
    }
    println!(
        "],\"edges\":[{}],\"rejected\":[{}]}}",
        edges.join(","),
        rejected.join(",")
    );
}
