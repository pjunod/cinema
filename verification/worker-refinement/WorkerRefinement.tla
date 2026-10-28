------------------------ MODULE WorkerRefinement ------------------------
EXTENDS Naturals, Sequences, TLC
CONSTANTS FaultBudget, RetryLimit, Mutation, Scenario, Recurring
Workers == {"w1", "w2"}
VARIABLES workers, domainOwner, request, waiter, parity
vars == <<workers, domainOwner, request, waiter, parity>>

Init ==
  /\ workers = [w \in Workers |-> [pc |-> "probe", admitted |-> FALSE, claim |-> FALSE,
       qstate |-> "queued", qlive |-> FALSE, domain |-> FALSE,
       relation |-> "none", bound |-> FALSE, revoked |-> FALSE,
       cancel |-> FALSE, interrupted |-> FALSE, exhausted |-> FALSE,
       faults |-> IF w = "w1" THEN FaultBudget ELSE 0,
       retries |-> 0, releaseTries |-> 0, token |-> "none", tq |-> <<>>,
       pub |-> "none", pq |-> <<>>, h |-> "off", qh |-> "off",
       stopH |-> FALSE, stopQ |-> FALSE, call |-> "none", kind |-> "none",
       waiting |-> FALSE, detached |-> "none", detachedCurrent |-> FALSE,
       detachedTrue |-> FALSE, detachedKind |-> "none", settled |-> FALSE, walk |-> "none",
       afterSettle |-> "release_join", eligible |-> TRUE,
       lostAck |-> FALSE, reconciled |-> FALSE, cleaned |-> FALSE,
       completedOnce |-> FALSE, cleanedOnce |-> FALSE,
       overlapPhase |-> 0, overlapReached |-> FALSE, staleCommit |-> FALSE, lateBeforeRelease |-> FALSE, staleRejected |-> FALSE]]
  /\ domainOwner = "none"
  /\ request = [w \in Workers |-> "pending"]
  /\ waiter = [w \in Workers |-> "pending"]
  /\ parity = FALSE

(* Worker identities stand for two distinct durable jobs with job-scoped
   request rows, sharing scan:library:<id> and capacity for both admissions.
   Local revocation stops dispatch; durable rows alone govern late apply. *)
Cleanup(w) == ~workers[w].admitted /\ ~workers[w].domain /\ ~workers[w].claim

TrueAuthority(w) == workers[w].domain /\ workers[w].relation = "current"
                    /\ domainOwner = w /\ workers[w].qlive /\ workers[w].bound
DurableOwner(w) == TrueAuthority(w)
TrueDetachedAuthority(w) == workers[w].detachedTrue /\ domainOwner = w
                            /\ workers[w].qlive
DetachedAuthority(w) == IF Mutation = "stale_commit" THEN workers[w].detachedCurrent
                        ELSE TrueDetachedAuthority(w)

LocalCurrent(w) ==
  LET s == workers[w] IN s.domain /\ s.relation = "current" /\ ~s.revoked
           /\ domainOwner = w
LocalPublishable(w) ==
  LET s == workers[w] IN LocalCurrent(w) /\ s.qlive /\ s.bound
(* FIFO intent queues prevent a periodic writer from barging ahead of a
   reader that already queued. These queues represent Tokio's lock discipline;
   their grants have their own fairness, not assumed-fair lock release. *)
ProbeReturn(w) ==
  LET s == workers[w] IN
  /\ s.pc = "probe"
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.pc = "admit"]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>
Admission(w) ==
  LET s == workers[w] IN
  /\ s.pc = "admit"
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.pc = "claim_send", !.admitted = TRUE,
                    !.cleaned = FALSE]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>
StartCall(w, kind, next) ==
  LET s == workers[w] IN
  /\ s.call = "none"
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.call = "sent", !.kind = kind,
                    !.waiting = TRUE, !.pc = next]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>
ClaimSend(w) ==
  LET s == workers[w] IN s.pc = "claim_send" /\ StartCall(w, "claim", "claim_wait")
DomainSend(w) ==
  LET s == workers[w] IN s.pc = "domain_send" /\ StartCall(w, "domain", "domain_wait")
BindSend(w) ==
  LET s == workers[w] IN s.pc = "bind_send" /\ LocalCurrent(w) /\ s.qlive /\ StartCall(w, "bind", "bind_wait")
ItemSend(w) ==
  LET s == workers[w] IN s.pc = "item_send" /\ LocalPublishable(w) /\ StartCall(w, "item", "item_wait")
CompleteSend(w) ==
  LET s == workers[w] IN s.pc = "complete_send" /\ LocalPublishable(w) /\ StartCall(w, "complete", "complete_wait")
ReleaseSend(w) ==
  LET s == workers[w] IN s.pc = "release_send" /\ StartCall(w, "release", "release_wait")
SettleSend(w) ==
  LET s == workers[w] IN s.pc = "settle_send" /\ StartCall(w, "settle", "settle_wait")

(* Lower dispatch/application and reply are independent scheduled steps.
   In particular Apply is permitted after CompleteTimeout(w) cleared waiting.
   Timeout is a finite environmental fault, never a rollback operation. *)
StoreApply(w) ==
  LET s == workers[w] IN
  /\ s.call = "sent"
  /\ workers' = [workers EXCEPT
       !["w1"].overlapPhase = IF Scenario = "overlap" /\ w = "w2" /\ s.kind = "domain" /\ domainOwner = "none" /\ workers["w1"].overlapPhase = 1 THEN 2 ELSE @,
       ![w] = [s EXCEPT !.call = "applied",
       !.claim = IF s.kind = "claim" THEN TRUE ELSE @,
       !.detachedCurrent = IF s.kind \in {"claim", "release"} /\ Mutation # "stale_commit" THEN FALSE ELSE @,
       !.detachedTrue = IF s.kind \in {"claim", "release"} THEN FALSE ELSE @,
       !.settled = IF s.kind = "settle" THEN TRUE ELSE @,
       !.qstate = IF s.kind = "claim" THEN "running"
                  ELSE IF s.kind = "settle" /\ s.qlive THEN "queued" ELSE @,
       !.qlive = IF s.kind = "claim" THEN TRUE
                 ELSE IF s.kind = "settle" THEN FALSE ELSE @,
       !.domain = IF s.kind = "domain" /\ domainOwner = "none" THEN TRUE
                  ELSE IF s.kind = "release" THEN FALSE ELSE @,
       !.relation = IF s.kind = "domain" /\ domainOwner = "none" THEN "current"
                    ELSE IF s.kind = "release" THEN "none" ELSE @,
       !.bound = IF s.kind = "bind" THEN s.domain /\ domainOwner = w /\ s.qlive ELSE @,
       !.overlapPhase = IF Scenario = "overlap" /\ w = "w1" /\ s.kind = "release" /\ domainOwner = w THEN 1
                        ELSE IF Scenario = "overlap" /\ w = "w1" /\ s.kind = "domain" /\ domainOwner = "none" /\ s.overlapPhase = 2 THEN 3 ELSE @,
       !.overlapReached = s.overlapReached \/ (Scenario = "overlap" /\ w = "w1" /\ s.kind = "domain" /\ domainOwner = "none" /\ s.overlapPhase = 2),
       !.completedOnce = s.completedOnce \/ (Scenario # "normal" /\ s.kind = "complete" /\ DurableOwner(w) /\ request[w] = "pending"),
       !.staleCommit = s.staleCommit \/ (s.kind = "complete" /\ DurableOwner(w) /\ ~TrueAuthority(w) /\ request[w] = "pending")]]
  /\ domainOwner' = IF s.kind = "domain" /\ domainOwner = "none"
                    THEN w
                    ELSE IF s.kind = "release" /\ domainOwner = w
                         THEN "none" ELSE domainOwner
  /\ request' = [request EXCEPT ![w] = IF s.kind = "complete" /\ DurableOwner(w) THEN "done" ELSE request[w]]
  /\ waiter' = [waiter EXCEPT ![w] = IF s.kind = "complete" /\ DurableOwner(w) THEN "terminal" ELSE waiter[w]]
  /\ parity' = IF s.kind = "complete" /\ DurableOwner(w) /\ request[w] = "pending" THEN ~parity ELSE parity
StoreReply(w) ==
  LET s == workers[w] IN
  /\ s.call = "applied"
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.call = "reply"]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>
(* The caller's 3s wrapper may return before application. The detached write
   still owns its lower slot. Re-read can observe pending, then retry. *)
CompleteTimeout(w) ==
  LET s == workers[w] IN
  /\ s.pc = "complete_wait" /\ s.call \in {"sent", "applied", "reply"}
  /\ s.waiting /\ s.faults > 0 /\ s.detached = "none"
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.waiting = FALSE, !.pc = "reconcile",
       !.detached = IF s.call = "sent" THEN "sent" ELSE "applied", !.detachedKind = "complete",
       !.detachedCurrent = TrueAuthority(w), !.detachedTrue = TrueAuthority(w),
       !.call = "none", !.kind = "none",
       !.faults = @ - 1, !.lostAck = TRUE, !.token = "none", !.pub = "none"]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>
DetachedApply(w) ==
  LET s == workers[w] IN
  /\ s.detached = "sent" /\ s.detachedKind = "complete"
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.detached = "applied",
       !.completedOnce = s.completedOnce \/ (Scenario # "normal" /\ DetachedAuthority(w) /\ request[w] = "pending"),
       !.lateBeforeRelease = s.lateBeforeRelease \/ (Scenario = "late" /\ s.revoked /\ DetachedAuthority(w) /\ request[w] = "pending"),
       !.staleRejected = s.staleRejected \/ (Scenario = "stale" /\ ~DetachedAuthority(w) /\ ~s.detachedCurrent),
       !.staleCommit = s.staleCommit \/ (DetachedAuthority(w) /\ ~TrueDetachedAuthority(w) /\ request[w] = "pending")]]
  /\ request' = [request EXCEPT ![w] = IF DetachedAuthority(w) THEN "done" ELSE request[w]]
  /\ waiter' = [waiter EXCEPT ![w] = IF DetachedAuthority(w) THEN "terminal" ELSE waiter[w]]
  /\ UNCHANGED domainOwner
  /\ parity' = IF DetachedAuthority(w) /\ request[w] = "pending" THEN ~parity ELSE parity
DrainDetached(w) ==
  LET s == workers[w] IN
  /\ s.detached = "applied"
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.detached = "none", !.detachedKind = "none", !.detachedCurrent = FALSE, !.detachedTrue = FALSE]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>

CallerReturn(w) ==
  LET s == workers[w] IN
  /\ s.call = "reply" /\ s.waiting
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.call = "none", !.kind = "none", !.waiting = FALSE,
       !.pc = CASE s.kind = "claim" -> "domain_send"
                [] s.kind = "domain" -> IF s.domain THEN "bind_token" ELSE "settle_token"
                [] s.kind = "bind" -> IF s.bound /\ LocalCurrent(w) THEN "scan_start" ELSE "release_join"
                [] s.kind = "item" -> IF LocalPublishable(w) THEN "complete_token" ELSE "release_join"
                [] s.kind = "complete" -> IF request[w] = "done" THEN "settle_token" ELSE "reconcile"
                [] s.kind = "release" -> IF s.settled THEN "finish_join" ELSE "settle_token"
                [] OTHER -> s.afterSettle,
       !.h = IF s.kind = "domain" /\ s.domain THEN "idle" ELSE @,
       !.qh = IF s.kind = "claim" THEN "idle" ELSE @,
       !.token = IF s.kind \in {"bind", "complete", "settle"} THEN "none" ELSE @,
       !.pub = IF s.kind \in {"bind", "item", "complete"} THEN "none" ELSE @,
       !.afterSettle = IF s.kind = "domain" /\ ~s.domain THEN "finish_join"
                      ELSE IF s.kind = "release" THEN "finish_join" ELSE @,
       !.eligible = IF s.kind = "domain" /\ ~s.domain THEN FALSE ELSE @]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>

WorkerTokenIntent(w) ==
  LET s == workers[w] IN
  /\ s.pc \in {"bind_token", "complete_token", "settle_token", "finish_token"}
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.tq = Append(@, "worker"),
                    !.pc = CASE s.pc = "bind_token" -> "bind_mutex"
                             [] s.pc = "complete_token" -> "complete_mutex"
                             [] s.pc = "settle_token" -> "settle_mutex"
                             [] OTHER -> "finish_mutex"]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>
WorkerTokenGrant(w) ==
  LET s == workers[w] IN
  /\ s.token = "none" /\ s.tq # <<>> /\ Head(s.tq) = "worker"
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.token = "worker", !.tq = Tail(@),
       !.pc = CASE s.pc = "bind_mutex" -> "bind_auth"
                [] s.pc = "complete_mutex" -> "complete_auth"
                [] s.pc = "settle_mutex" -> "settle_auth"
                [] OTHER -> "finish_retire"]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>
AuthorityReturn(w) ==
  LET s == workers[w] IN
  /\ s.pc \in {"bind_auth", "complete_auth", "settle_auth"}
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.pc = CASE s.pc = "bind_auth" -> "bind_read"
                             [] s.pc = "complete_auth" -> "complete_read"
                             [] OTHER -> "settle_send"]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>
WorkerPubIntent(w) ==
  LET s == workers[w] IN
  /\ s.pc \in {"bind_read", "complete_read", "item_write"}
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.pq = Append(@, "worker"),
       !.pc = CASE s.pc = "bind_read" -> "bind_lock"
                [] s.pc = "complete_read" -> "complete_lock"
                [] OTHER -> "item_lock"]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>
WorkerPubGrant(w) ==
  LET s == workers[w] IN
  /\ s.pub = "none" /\ s.pq # <<>> /\ Head(s.pq) = "worker"
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.pub = "worker", !.pq = Tail(@),
       !.pc = CASE s.pc = "bind_lock" -> "bind_send"
                [] s.pc = "complete_lock" -> "complete_send"
                [] OTHER -> "item_send"]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>

ScanStart(w) ==
  LET s == workers[w] IN
  /\ s.pc = "scan_start"
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.pc = "scan", !.walk = "busy"]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>
WalkReturn(w) ==
  LET s == workers[w] IN
  /\ s.walk = "busy"
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.walk = "joined"]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>
ScanFinish(w) ==
  LET s == workers[w] IN
  /\ s.pc = "scan" /\ s.walk = "joined"
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.pc = IF s.interrupted THEN "release_join" ELSE "item_write"]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>
Reconcile(w) ==
  LET s == workers[w] IN
  /\ s.pc = "reconcile"
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.pc = IF request[w] = "done" THEN "settle_token" ELSE "release_join",
       !.reconciled = s.reconciled \/ (request[w] = "done" /\ s.lostAck),
       !.interrupted = IF request[w] = "pending" THEN TRUE ELSE @,
       !.retries = IF request[w] = "pending" THEN @ + 1 ELSE @,
       !.exhausted = s.exhausted \/ (request[w] = "pending" /\ s.retries + 1 >= RetryLimit)]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>
(* Cancellation is a selected caller/pressure cancellation, not daemon
   shutdown. It joins WalkDir; it does not complete the durable request[w]. *)
Cancel(w) ==
  LET s == workers[w] IN
  /\ Scenario = "cancel"
  /\ s.pc \in {"scan", "item_write", "item_lock", "item_send", "item_wait",
       "complete_token", "complete_mutex", "complete_auth", "complete_read",
       "complete_lock", "complete_send", "complete_wait", "reconcile"}
  /\ ~s.cancel /\ w = "w1"
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.cancel = TRUE, !.interrupted = TRUE, !.revoked = TRUE]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>
WorkFault(w) ==
  LET s == workers[w] IN
  /\ Scenario = "normal" /\ s.pc = "scan" /\ ~s.interrupted /\ s.faults > 0
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.interrupted = TRUE, !.revoked = TRUE,
       !.faults = @ - 1, !.retries = @ + 1,
       !.exhausted = s.retries + 1 >= RetryLimit]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>
QueueLoss(w) ==
  LET s == workers[w] IN
  /\ s.pc = "scan" /\ ~s.interrupted /\ s.faults > 0
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.interrupted = TRUE, !.qlive = FALSE,
                    !.revoked = TRUE, !.faults = @ - 1]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>

(* Domain heartbeat takes publication WRITE and retains it across its own
   store request[w]. Only the reverse-edge CONTROL also waits for token mutex. *)
DomainIntent(w) ==
  LET s == workers[w] IN
  /\ s.h = "idle" /\ ~s.stopH
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.h = "lock", !.pq = Append(@, "domain")]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>
DomainGrant(w) ==
  LET s == workers[w] IN
  /\ s.h = "lock" /\ s.pub = "none" /\ s.pq # <<>> /\ Head(s.pq) = "domain"
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.pub = "domain", !.pq = Tail(@),
       !.h = IF Mutation = "lock_cycle" THEN "reverse" ELSE "send"]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>
ReverseTokenGrant(w) ==
  LET s == workers[w] IN
  /\ s.h = "reverse" /\ s.token = "none"
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.token = "domain", !.h = "send"]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>
DomainDispatch(w) ==
  LET s == workers[w] IN
  /\ s.h = "send"
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.h = "apply"]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>
DomainApply(w) ==
  LET s == workers[w] IN
  /\ s.h = "apply"
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.h = "reply"]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>
DomainReturn(w) ==
  LET s == workers[w] IN
  /\ s.h = "reply"
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.h = IF s.stopH THEN "off" ELSE "idle", !.pub = "none",
       !.token = IF s.token = "domain" THEN "none" ELSE @]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>
DomainStop(w) ==
  LET s == workers[w] IN
  /\ s.stopH /\ s.h = "idle"
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.h = "off"]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>
(* Lease deadline reports loss and revokes. It deliberately DOES NOT free
   publication WRITE or skip the in-flight renewal's return/join. *)
RenewalDeadline(w) ==
  LET s == workers[w] IN
  /\ s.h \in {"apply", "reply"} /\ ~s.revoked /\ s.faults > 0
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.revoked = TRUE, !.interrupted = TRUE,
                    !.faults = @ - 1, !.stopH = TRUE]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>

QueueIntent(w) ==
  LET s == workers[w] IN
  /\ s.qh = "idle" /\ ~s.stopQ
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.qh = "lock", !.tq = Append(@, "queue")]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>
QueueGrant(w) ==
  LET s == workers[w] IN
  /\ s.qh = "lock" /\ s.token = "none" /\ s.tq # <<>> /\ Head(s.tq) = "queue"
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.token = "queue", !.tq = Tail(@), !.qh = "authority"]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>
QueueAuthorityReturn(w) ==
  LET s == workers[w] IN
  /\ s.qh = "authority"
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.qh = "send"]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>
QueueDispatch(w) ==
  LET s == workers[w] IN
  /\ s.qh = "send"
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.qh = "apply"]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>
QueueApply(w) ==
  LET s == workers[w] IN
  /\ s.qh = "apply"
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.qh = "reply"]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>
QueueReturn(w) ==
  LET s == workers[w] IN
  /\ s.qh = "reply"
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.token = "none", !.qh = IF s.stopQ THEN "off" ELSE "idle"]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>
QueueStop(w) ==
  LET s == workers[w] IN
  /\ s.stopQ /\ s.qh = "idle"
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.qh = "off"]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>

ReleaseJoin(w) ==
  LET s == workers[w] IN
  /\ s.pc = "release_join"
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.stopH = TRUE, !.revoked = TRUE,
       !.pc = IF s.h = "off" /\ s.call = "none" THEN
                 IF s.domain THEN "release_send"
                 ELSE IF s.settled THEN "finish_join" ELSE "settle_token"
              ELSE "release_join",
       !.afterSettle = "finish_join"]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>
(* One failed release is enough to exhibit retry/reacquire; remaining five
   source attempts are represented by the same caller-return dependency.
   The model retains the source's exact maximum of five attempted releases. *)
ReleaseFailure(w) ==
  LET s == workers[w] IN
  /\ s.pc = "release_wait" /\ s.call \in {"sent", "applied", "reply"}
  /\ s.faults > 0 /\ s.detached = "none"
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT
       !.call = "none", !.kind = "none", !.waiting = FALSE,
       !.detached = IF s.call = "sent" THEN "sent" ELSE "applied",
       !.detachedKind = "release", !.detachedTrue = s.domain /\ domainOwner = w,
       !.detachedCurrent = FALSE, !.releaseTries = @ + 1, !.faults = @ - 1,
       !.pc = IF s.releaseTries + 1 < 5 THEN "release_backoff" ELSE "settle_token",
       !.domain = IF s.releaseTries + 1 = 5 THEN FALSE ELSE @,
       !.relation = IF s.releaseTries + 1 = 5 THEN "none" ELSE @,
       !.eligible = FALSE]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>
ReleaseRetry(w) ==
  LET s == workers[w] IN
  /\ s.pc = "release_backoff" /\ s.eligible
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.pc = "release_send"]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>
DomainExpiry(w) ==
  LET s == workers[w] IN
  /\ s.releaseTries = 5 /\ ~s.domain /\ domainOwner = w /\ s.eligible
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.releaseTries = 0]]
  /\ domainOwner' = IF domainOwner = w THEN "none" ELSE domainOwner
  /\ UNCHANGED <<request, waiter, parity>>
(* Lost settlement is no committed transition: the running queue row needs
   lease expiry. A lost ack AFTER apply has already queued it and is harmless. *)
LostSettlement(w) ==
  LET s == workers[w] IN
  /\ s.pc = "settle_wait" /\ s.call = "sent" /\ s.faults > 0
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.call = "none", !.kind = "none", !.waiting = FALSE,
       !.faults = @ - 1, !.token = "none", !.settled = TRUE, !.pc = s.afterSettle]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>
FinishJoin(w) ==
  LET s == workers[w] IN
  /\ s.pc = "finish_join"
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.stopQ = TRUE,
       !.pc = IF s.qh = "off" THEN "finish_token" ELSE "finish_join"]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>
FinishRetire(w) ==
  LET s == workers[w] IN
  /\ s.pc = "finish_retire"
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.claim = FALSE,
                    !.token = "none", !.pc = "drop"]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>
DropAdmission(w) ==
  LET s == workers[w] IN
  /\ s.pc = "drop"
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.admitted = FALSE, !.cleaned = TRUE, !.cleanedOnce = Scenario # "normal",
                    !.eligible = FALSE, !.pc = "backoff"]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>
QueueExpiry(w) ==
  LET s == workers[w] IN
  /\ Mutation # "lost_settlement" /\ ~s.claim /\ s.qstate = "running" /\ s.eligible
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.qstate = "queued", !.qlive = FALSE]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>
TimeObserved(w) ==
  LET s == workers[w] IN
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.eligible = TRUE]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>
Wake(w) ==
  LET s == workers[w] IN
  /\ s.pc = "backoff" /\ s.eligible /\ s.qstate = "queued"
  /\ request[w] = "pending" /\ ~s.cancel /\ ~s.exhausted
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.pc = "probe", !.revoked = FALSE,
       !.interrupted = FALSE, !.bound = FALSE, !.stopH = FALSE, !.stopQ = FALSE,
       !.walk = "none", !.releaseTries = 0, !.settled = FALSE, !.afterSettle = "release_join"]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>



DispatchRejected(w) ==
  LET s == workers[w] IN
  /\ s.pc \in {"bind_send", "item_send", "complete_send"}
  /\ ~(LocalCurrent(w) /\ s.qlive)
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.pc = "release_join",
       !.token = "none", !.pub = "none", !.interrupted = TRUE]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>

DetachedReleaseApply(w) ==
  LET s == workers[w] IN
  /\ s.detached = "sent" /\ s.detachedKind = "release"
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.detached = "applied",
       !.domain = IF s.detachedTrue /\ domainOwner = w THEN FALSE ELSE @,
       !.relation = IF s.detachedTrue /\ domainOwner = w THEN "none" ELSE @,
       !.detachedTrue = FALSE, !.detachedCurrent = FALSE]]
  /\ domainOwner' = IF s.detachedTrue /\ domainOwner = w THEN "none" ELSE domainOwner
  /\ UNCHANGED <<request, waiter, parity>>

RenewalLostAck(w) ==
  LET s == workers[w] IN
  /\ s.h = "reply" /\ s.faults > 0 /\ ~s.revoked
  /\ workers' = [workers EXCEPT ![w] = [s EXCEPT !.faults = @ - 1,
       !.revoked = TRUE, !.interrupted = TRUE, !.stopH = TRUE]]
  /\ UNCHANGED <<domainOwner, request, waiter, parity>>

Arrival(w) ==
  /\ Recurring /\ request[w] = "done" /\ Cleanup(w)
  /\ workers' = [workers EXCEPT ![w].completedOnce = @,
       ![w].cleaned = FALSE]
  /\ request' = [request EXCEPT ![w] = "pending"]
  /\ waiter' = [waiter EXCEPT ![w] = "pending"]
  /\ UNCHANGED <<domainOwner, parity>>


Next == \E w \in Workers:
    DispatchRejected(w)
    \/ DetachedReleaseApply(w)
    \/ RenewalLostAck(w)
    \/ ProbeReturn(w)
    \/ Admission(w)
    \/ ClaimSend(w)
    \/ DomainSend(w)
    \/ BindSend(w)
    \/ ItemSend(w)
    \/ CompleteSend(w)
    \/ ReleaseSend(w)
    \/ SettleSend(w)
    \/ StoreApply(w)
    \/ StoreReply(w)
    \/ CompleteTimeout(w)
    \/ DetachedApply(w)
    \/ DrainDetached(w)
    \/ CallerReturn(w)
    \/ WorkerTokenIntent(w)
    \/ WorkerTokenGrant(w)
    \/ AuthorityReturn(w)
    \/ WorkerPubIntent(w)
    \/ WorkerPubGrant(w)
    \/ ScanStart(w)
    \/ WalkReturn(w)
    \/ ScanFinish(w)
    \/ Reconcile(w)
    \/ Cancel(w)
    \/ WorkFault(w)
    \/ QueueLoss(w)
    \/ DomainIntent(w)
    \/ DomainGrant(w)
    \/ ReverseTokenGrant(w)
    \/ DomainDispatch(w)
    \/ DomainApply(w)
    \/ DomainReturn(w)
    \/ DomainStop(w)
    \/ RenewalDeadline(w)
    \/ QueueIntent(w)
    \/ QueueGrant(w)
    \/ QueueAuthorityReturn(w)
    \/ QueueDispatch(w)
    \/ QueueApply(w)
    \/ QueueReturn(w)
    \/ QueueStop(w)
    \/ ReleaseJoin(w)
    \/ ReleaseFailure(w)
    \/ ReleaseRetry(w)
    \/ DomainExpiry(w)
    \/ LostSettlement(w)
    \/ FinishJoin(w)
    \/ FinishRetire(w)
    \/ DropAdmission(w)
    \/ QueueExpiry(w)
    \/ Wake(w)
    \/ TimeObserved(w)
    \/ Arrival(w)

Fair == \A w \in Workers:
    /\ WF_vars(DispatchRejected(w))
    /\ WF_vars(ProbeReturn(w))
    /\ WF_vars(Admission(w))
    /\ WF_vars(ClaimSend(w))
    /\ WF_vars(DomainSend(w))
    /\ WF_vars(BindSend(w))
    /\ WF_vars(ItemSend(w))
    /\ WF_vars(CompleteSend(w))
    /\ WF_vars(ReleaseSend(w))
    /\ WF_vars(SettleSend(w))
    /\ WF_vars(StoreApply(w))
    /\ WF_vars(StoreReply(w))
    /\ WF_vars(DrainDetached(w))
    /\ WF_vars(CallerReturn(w))
    /\ WF_vars(WorkerTokenIntent(w))
    /\ WF_vars(WorkerTokenGrant(w))
    /\ WF_vars(AuthorityReturn(w))
    /\ WF_vars(WorkerPubIntent(w))
    /\ WF_vars(WorkerPubGrant(w))
    /\ WF_vars(ScanStart(w))
    /\ (IF Mutation = "external_stall" /\ w = "w1" THEN TRUE ELSE WF_vars(WalkReturn(w)))
    /\ (IF Mutation = "no_useful_progress" \/ (Mutation = "renewal_livelock" /\ w = "w1") THEN TRUE ELSE WF_vars(ScanFinish(w)))
    /\ WF_vars(Reconcile(w))
    /\ WF_vars(DomainIntent(w))
    /\ WF_vars(DomainGrant(w))
    /\ WF_vars(ReverseTokenGrant(w))
    /\ WF_vars(DomainDispatch(w))
    /\ WF_vars(DomainApply(w))
    /\ WF_vars(DomainReturn(w))
    /\ WF_vars(DomainStop(w))
    /\ WF_vars(QueueIntent(w))
    /\ WF_vars(QueueGrant(w))
    /\ WF_vars(QueueAuthorityReturn(w))
    /\ WF_vars(QueueDispatch(w))
    /\ WF_vars(QueueApply(w))
    /\ WF_vars(QueueReturn(w))
    /\ WF_vars(QueueStop(w))
    /\ WF_vars(ReleaseJoin(w))
    /\ WF_vars(ReleaseRetry(w))
    /\ WF_vars(DomainExpiry(w))
    /\ WF_vars(FinishJoin(w))
    /\ WF_vars(FinishRetire(w))
    /\ WF_vars(DropAdmission(w))
    /\ WF_vars(QueueExpiry(w))
    /\ WF_vars(Wake(w))
    /\ WF_vars(TimeObserved(w))
    /\ WF_vars(Arrival(w))
Spec == Init /\ [][Next]_vars /\ Fair

TypeOK == /\ domainOwner \in Workers \cup {"none"}
          /\ request \in [Workers -> {"pending", "done"}]
          /\ waiter \in [Workers -> {"pending", "terminal"}]
          /\ \A w \in Workers:
               /\ workers[w].faults \in 0..FaultBudget
               /\ workers[w].retries \in 0..RetryLimit
               /\ Len(workers[w].tq) <= 2 /\ Len(workers[w].pq) <= 2
ResultAtomicity == \A w \in Workers: (request[w] = "done") <=> (waiter[w] = "terminal")
AdmissionUntilJoined == \A w \in Workers: workers[w].walk = "busy" => workers[w].admitted
NoFictitiousRelease == \A w \in Workers:
  workers[w].pub = "domain" => workers[w].h \in {"reverse", "send", "apply", "reply"}
NoStaleCommit == \A w \in Workers: ~workers[w].staleCommit
EachWorkerCleanup == \A w \in Workers: workers[w].admitted ~> Cleanup(w)
SelectedCleanup == workers["w1"].admitted ~> Cleanup("w1")
AttemptProgress == \A w \in Workers: workers[w].qstate = "running" ~> workers[w].qstate = "queued"
UsefulProgress == ([](\A w \in Workers: ~workers[w].cancel /\ ~workers[w].exhausted))
                  => (([]<>parity) /\ ([]<>~parity))
NoBothCompleteWitness == ~(\A w \in Workers: workers[w].completedOnce /\ workers[w].cleanedOnce /\ Cleanup(w))
NoOverlapReacquireWitness == ~(workers["w1"].overlapReached /\ (\A w \in Workers: workers[w].completedOnce /\ workers[w].cleanedOnce /\ Cleanup(w)))
NoDetachedBeforeReleaseWitness == ~workers["w1"].lateBeforeRelease
NoStaleAfterReleaseWitness == ~(workers["w1"].staleRejected /\ request["w1"] = "pending")
NoReconcileWitness == ~(workers["w1"].reconciled /\ workers["w1"].cleaned /\ Cleanup("w1"))
NoCancelCleanupWitness == ~(workers["w1"].cancel /\ workers["w1"].cleaned /\ Cleanup("w1"))
=============================================================================
