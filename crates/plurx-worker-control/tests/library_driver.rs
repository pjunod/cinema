use plurx_worker_control::library_driver::{
    Driver, Environment, Reply, Request, WorkOutcome, REPLY_CLASSES,
};
use std::future::{ready, Future};
use std::pin::pin;
use std::task::{Context, Poll, Waker};

fn inventory() -> Vec<Driver> {
    let mut states = vec![Driver::new()];
    let mut cursor = 0;
    while cursor < states.len() {
        let state = states[cursor];
        for reply in REPLY_CLASSES {
            let mut next = state;
            match next.drive(reply) {
                Ok(_) => {
                    if !states.iter().any(|state| state.control() == next.control()) {
                        states.push(next);
                    }
                }
                Err(_) => {
                    assert_eq!(next.control(), state.control());
                    assert_eq!(next.request(), state.request());
                }
            }
        }
        cursor += 1;
    }
    states
}

#[test]
fn every_reply_is_accepted_or_rejected_without_mutation_including_terminal() {
    let states = inventory();
    assert!(states.iter().any(|driver| driver.control().terminal()));
    for state in &states {
        for reply in REPLY_CLASSES {
            let mut next = *state;
            if next.drive(reply).is_ok() {
                assert!(states.iter().any(|state| state.control() == next.control()));
            } else {
                assert_eq!(state.control(), next.control());
                assert_eq!(state.request(), next.request());
            }
        }
    }
}

#[test]
fn numeric_payload_does_not_select_control_edges() {
    // Finite representative evidence for the reviewed quotient argument. The
    // all-i64 arithmetic theorem remains the existing lifecycle Kani harness.
    for state in inventory() {
        for reply in REPLY_CLASSES {
            let mut representative = state;
            let expected = representative
                .drive(reply)
                .map(|request| request.map(Request::kind));
            for now_ms in [i64::MIN, -1, 0, 1, i64::MAX] {
                for retry_delay_ms in [i64::MIN, -1, 0, 1, i64::MAX] {
                    let numeric = match reply {
                        Reply::Domain { acquired, .. } => Reply::Domain { acquired, now_ms },
                        Reply::Released { .. } => Reply::Released {
                            now_ms,
                            retry_delay_ms,
                        },
                        reply => reply,
                    };
                    let mut varied = state;
                    assert_eq!(
                        varied
                            .drive(numeric)
                            .map(|request| request.map(Request::kind)),
                        expected
                    );
                    assert_eq!(varied.control(), representative.control());
                }
            }
        }
    }
}

struct ReadyEnvironment {
    requests: Vec<Request>,
    cancelled: bool,
    joined: bool,
}

impl Environment for ReadyEnvironment {
    fn perform(&mut self, request: Request) -> impl Future<Output = Reply> + Send {
        self.requests.push(request);
        ready(match request {
            Request::AcquireDomain => Reply::Domain {
                acquired: true,
                now_ms: 123,
            },
            Request::Bind => Reply::Bound { accepted: true },
            Request::PollWork => Reply::Work(WorkOutcome::Pressure),
            Request::CancelWork => {
                self.cancelled = true;
                Reply::Done
            }
            Request::JoinWork => {
                assert!(self.cancelled);
                self.joined = true;
                Reply::Joined { ok: false }
            }
            Request::ReleaseDomain { .. } => {
                assert!(self.joined);
                Reply::Released {
                    now_ms: 456,
                    retry_delay_ms: 789,
                }
            }
            Request::DropAdmission => {
                assert!(self.joined);
                Reply::Done
            }
            _ => Reply::Done,
        })
    }
}

#[test]
fn production_executor_consumes_real_environment_replies_in_order() {
    let mut environment = ReadyEnvironment {
        requests: vec![],
        cancelled: false,
        joined: false,
    };
    let mut cx = Context::from_waker(Waker::noop());
    {
        let mut run = pin!(plurx_worker_control::library_driver::run_driver(
            &mut environment
        ));
        assert_eq!(run.as_mut().poll(&mut cx), Poll::Ready(Ok(())));
    }
    assert_eq!(environment.requests.last(), Some(&Request::DropAdmission));
    let cancel = environment
        .requests
        .iter()
        .position(|r| *r == Request::CancelWork)
        .expect("cancel");
    let join = environment
        .requests
        .iter()
        .position(|r| *r == Request::JoinWork)
        .expect("join");
    assert!(cancel < join);
}
