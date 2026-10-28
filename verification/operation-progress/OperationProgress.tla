------------------------- MODULE OperationProgress -------------------------
EXTENDS Naturals, Sequences, TLC

CONSTANTS FaultBudget, RetryLimit, Contention, Mutation, Scenario

(* One selected durable request. The peer is a domain contender, not a second
   copy of the request pipeline. No finite event/renewal/claim horizon exists.
   A backend request and its caller are distinct: timeout does not roll back
   Apply. Locks belong to caller tasks, not to the lower replicated write. *)
VARIABLES s, peer, domainOwner, request, waiter
vars == <<s, peer, domainOwner, request, waiter>>

Init ==
  /\ s = [pc |-> "probe", admitted |-> FALSE, claim |-> FALSE,
           qstate |-> "queued", qlive |-> FALSE, domain |-> FALSE,
           relation |-> "none", bound |-> FALSE, revoked |-> FALSE,
           cancel |-> FALSE, interrupted |-> FALSE, exhausted |-> FALSE,
           faults |-> FaultBudget, retries |-> 0, releaseTries |-> 0,
           token |-> "none", tq |-> <<>>, pub |-> "none", pq |-> <<>>,
           h |-> "off", qh |-> "off", stopH |-> FALSE, stopQ |-> FALSE,
           call |-> "none", kind |-> "none", waiting |-> FALSE,
           detached |-> "none", detachedCurrent |-> FALSE, settled |-> FALSE,
           walk |-> "none", afterSettle |-> "release_join",
           eligible |-> TRUE, tick |-> FALSE, lostAck |-> FALSE,
           reconciled |-> FALSE, cleaned |-> FALSE, item |-> FALSE]
  /\ peer = "idle" /\ domainOwner = "none"
  /\ request = "pending" /\ waiter = "pending"

Current == s.domain /\ s.relation = "current" /\ ~s.revoked
           /\ domainOwner = "selected"
Publishable == Current /\ s.qlive /\ s.bound
Cleanup == ~s.admitted /\ ~s.domain /\ ~s.claim

(* FIFO intent queues prevent a periodic writer from barging ahead of a
   reader that already queued. These queues represent Tokio's lock discipline;
   their grants have their own fairness, not assumed-fair lock release. *)
ProbeReturn ==
  /\ s.pc = "probe"
  /\ s' = [s EXCEPT !.pc = "admit"]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>
Admission ==
  /\ s.pc = "admit"
  /\ s' = [s EXCEPT !.pc = "claim_send", !.admitted = TRUE,
                    !.cleaned = FALSE]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>
StartCall(kind, next) ==
  /\ s.call = "none"
  /\ s' = [s EXCEPT !.call = "sent", !.kind = kind,
                    !.waiting = TRUE, !.pc = next]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>
ClaimSend == s.pc = "claim_send" /\ StartCall("claim", "claim_wait")
DomainSend == s.pc = "domain_send" /\ StartCall("domain", "domain_wait")
BindSend == s.pc = "bind_send" /\ StartCall("bind", "bind_wait")
ItemSend == s.pc = "item_send" /\ StartCall("item", "item_wait")
CompleteSend == s.pc = "complete_send" /\ StartCall("complete", "complete_wait")
ReleaseSend == s.pc = "release_send" /\ StartCall("release", "release_wait")
SettleSend == s.pc = "settle_send" /\ StartCall("settle", "settle_wait")

(* Lower dispatch/application and reply are independent scheduled steps.
   In particular Apply is permitted after CompleteTimeout cleared waiting.
   Timeout is a finite environmental fault, never a rollback operation. *)
StoreApply ==
  /\ s.call = "sent"
  /\ s' = [s EXCEPT !.call = "applied",
       !.claim = IF s.kind = "claim" THEN TRUE ELSE @,
       !.detachedCurrent = IF s.kind \in {"claim", "release"} THEN FALSE ELSE @,
       !.settled = IF s.kind = "settle" THEN TRUE ELSE @,
       !.qstate = IF s.kind = "claim" THEN "running"
                  ELSE IF s.kind = "settle" /\ s.qlive THEN "queued" ELSE @,
       !.qlive = IF s.kind = "claim" THEN TRUE
                 ELSE IF s.kind = "settle" THEN FALSE ELSE @,
       !.domain = IF s.kind = "domain" /\ domainOwner = "none" THEN TRUE
                  ELSE IF s.kind = "release" THEN FALSE ELSE @,
       !.relation = IF s.kind = "domain" /\ domainOwner = "none" THEN "current"
                    ELSE IF s.kind = "release" THEN "none" ELSE @,
       !.bound = IF s.kind = "bind" THEN Current /\ s.qlive ELSE @,
       !.item = IF s.kind = "item" /\ Publishable THEN TRUE ELSE @]
  /\ domainOwner' = IF s.kind = "domain" /\ domainOwner = "none"
                    THEN "selected"
                    ELSE IF s.kind = "release" /\ domainOwner = "selected"
                         THEN "none" ELSE domainOwner
  /\ request' = IF s.kind = "complete" /\ Publishable THEN "done" ELSE request
  /\ waiter' = IF s.kind = "complete" /\ Publishable THEN "terminal" ELSE waiter
  /\ UNCHANGED peer
StoreReply ==
  /\ s.call = "applied"
  /\ s' = [s EXCEPT !.call = "reply"]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>
(* The caller's 3s wrapper may return before application. The detached write
   still owns its lower slot. Re-read can observe pending, then retry. *)
CompleteTimeout ==
  /\ s.pc = "complete_wait" /\ s.call \in {"sent", "applied", "reply"}
  /\ s.waiting /\ s.faults > 0 /\ s.detached = "none"
  /\ s' = [s EXCEPT !.waiting = FALSE, !.pc = "reconcile",
       !.detached = IF s.call = "sent" THEN "sent" ELSE "applied",
       !.detachedCurrent = Current /\ s.qlive,
       !.call = "none", !.kind = "none",
       !.faults = @ - 1, !.lostAck = TRUE, !.token = "none", !.pub = "none"]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>
DetachedApply ==
  /\ s.detached = "sent"
  /\ s' = [s EXCEPT !.detached = "applied"]
  /\ request' = IF s.detachedCurrent /\ Publishable THEN "done" ELSE request
  /\ waiter' = IF s.detachedCurrent /\ Publishable THEN "terminal" ELSE waiter
  /\ UNCHANGED <<peer, domainOwner>>
DrainDetached ==
  /\ s.detached = "applied"
  /\ s' = [s EXCEPT !.detached = "none", !.detachedCurrent = FALSE]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>

CallerReturn ==
  /\ s.call = "reply" /\ s.waiting
  /\ s' = [s EXCEPT !.call = "none", !.kind = "none", !.waiting = FALSE,
       !.pc = CASE s.kind = "claim" -> "domain_send"
                [] s.kind = "domain" -> IF s.domain THEN "bind_token" ELSE "settle_token"
                [] s.kind = "bind" -> IF s.bound /\ Current THEN "scan_start" ELSE "release_join"
                [] s.kind = "item" -> IF Publishable THEN "complete_token" ELSE "release_join"
                [] s.kind = "complete" -> IF request = "done" THEN "settle_token" ELSE "reconcile"
                [] s.kind = "release" -> IF s.settled THEN "finish_join" ELSE "settle_token"
                [] OTHER -> s.afterSettle,
       !.h = IF s.kind = "domain" /\ s.domain THEN "idle" ELSE @,
       !.qh = IF s.kind = "claim" THEN "idle" ELSE @,
       !.token = IF s.kind \in {"bind", "complete", "settle"} THEN "none" ELSE @,
       !.pub = IF s.kind \in {"bind", "item", "complete"} THEN "none" ELSE @,
       !.afterSettle = IF s.kind = "domain" /\ ~s.domain THEN "finish_join"
                      ELSE IF s.kind = "release" THEN "finish_join" ELSE @,
       !.eligible = IF s.kind = "domain" /\ ~s.domain THEN FALSE ELSE @]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>

WorkerTokenIntent ==
  /\ s.pc \in {"bind_token", "complete_token", "settle_token", "finish_token"}
  /\ s' = [s EXCEPT !.tq = Append(@, "worker"),
                    !.pc = CASE s.pc = "bind_token" -> "bind_mutex"
                             [] s.pc = "complete_token" -> "complete_mutex"
                             [] s.pc = "settle_token" -> "settle_mutex"
                             [] OTHER -> "finish_mutex"]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>
WorkerTokenGrant ==
  /\ s.token = "none" /\ s.tq # <<>> /\ Head(s.tq) = "worker"
  /\ s' = [s EXCEPT !.token = "worker", !.tq = Tail(@),
       !.pc = CASE s.pc = "bind_mutex" -> "bind_auth"
                [] s.pc = "complete_mutex" -> "complete_auth"
                [] s.pc = "settle_mutex" -> "settle_auth"
                [] OTHER -> "finish_retire"]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>
AuthorityReturn ==
  /\ s.pc \in {"bind_auth", "complete_auth", "settle_auth"}
  /\ s' = [s EXCEPT !.pc = CASE s.pc = "bind_auth" -> "bind_read"
                             [] s.pc = "complete_auth" -> "complete_read"
                             [] OTHER -> "settle_send"]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>
WorkerPubIntent ==
  /\ s.pc \in {"bind_read", "complete_read", "item_write"}
  /\ s' = [s EXCEPT !.pq = Append(@, "worker"),
       !.pc = CASE s.pc = "bind_read" -> "bind_lock"
                [] s.pc = "complete_read" -> "complete_lock"
                [] OTHER -> "item_lock"]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>
WorkerPubGrant ==
  /\ s.pub = "none" /\ s.pq # <<>> /\ Head(s.pq) = "worker"
  /\ s' = [s EXCEPT !.pub = "worker", !.pq = Tail(@),
       !.pc = CASE s.pc = "bind_lock" -> "bind_send"
                [] s.pc = "complete_lock" -> "complete_send"
                [] OTHER -> "item_send"]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>

ScanStart ==
  /\ s.pc = "scan_start"
  /\ s' = [s EXCEPT !.pc = "scan", !.walk = "busy"]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>
WalkReturn ==
  /\ s.walk = "busy"
  /\ s' = [s EXCEPT !.walk = "joined"]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>
ScanFinish ==
  /\ s.pc = "scan" /\ s.walk = "joined"
  /\ s' = [s EXCEPT !.pc = IF s.interrupted THEN "release_join" ELSE "item_write"]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>
Reconcile ==
  /\ s.pc = "reconcile"
  /\ s' = [s EXCEPT !.pc = IF request = "done" THEN "settle_token" ELSE "release_join",
       !.reconciled = s.reconciled \/ (request = "done" /\ s.lostAck),
       !.interrupted = IF request = "pending" THEN TRUE ELSE @,
       !.retries = IF request = "pending" THEN @ + 1 ELSE @,
       !.exhausted = s.exhausted \/ (request = "pending" /\ s.retries + 1 >= RetryLimit)]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>
(* Cancellation is a selected caller/pressure cancellation, not daemon
   shutdown. It joins WalkDir; it does not complete the durable request. *)
Cancel ==
  /\ Scenario = "cancel" /\ s.pc = "scan" /\ ~s.cancel
  /\ s' = [s EXCEPT !.cancel = TRUE, !.interrupted = TRUE, !.revoked = TRUE]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>
WorkFault ==
  /\ Scenario # "cancel" /\ s.pc = "scan" /\ ~s.interrupted /\ s.faults > 0
  /\ s' = [s EXCEPT !.interrupted = TRUE, !.revoked = TRUE,
       !.faults = @ - 1, !.retries = @ + 1,
       !.exhausted = s.retries + 1 >= RetryLimit]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>
QueueLoss ==
  /\ s.pc = "scan" /\ ~s.interrupted /\ s.faults > 0
  /\ s' = [s EXCEPT !.interrupted = TRUE, !.qlive = FALSE,
                    !.revoked = TRUE, !.faults = @ - 1]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>

(* Domain heartbeat takes publication WRITE and retains it across its own
   store request. Only the reverse-edge CONTROL also waits for token mutex. *)
DomainIntent ==
  /\ s.h = "idle" /\ ~s.stopH
  /\ s' = [s EXCEPT !.h = "lock", !.pq = Append(@, "domain")]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>
DomainGrant ==
  /\ s.h = "lock" /\ s.pub = "none" /\ s.pq # <<>> /\ Head(s.pq) = "domain"
  /\ s' = [s EXCEPT !.pub = "domain", !.pq = Tail(@),
       !.h = IF Mutation = "lock_cycle" THEN "reverse" ELSE "send"]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>
ReverseTokenGrant ==
  /\ s.h = "reverse" /\ s.token = "none"
  /\ s' = [s EXCEPT !.token = "domain", !.h = "send"]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>
DomainDispatch ==
  /\ s.h = "send"
  /\ s' = [s EXCEPT !.h = "apply"]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>
DomainApply ==
  /\ s.h = "apply"
  /\ s' = [s EXCEPT !.h = "reply"]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>
DomainReturn ==
  /\ s.h = "reply"
  /\ s' = [s EXCEPT !.h = IF s.stopH THEN "off" ELSE "idle", !.pub = "none",
       !.token = IF s.token = "domain" THEN "none" ELSE @]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>
DomainStop ==
  /\ s.stopH /\ s.h = "idle"
  /\ s' = [s EXCEPT !.h = "off"]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>
(* Lease deadline reports loss and revokes. It deliberately DOES NOT free
   publication WRITE or skip the in-flight renewal's return/join. *)
RenewalDeadline ==
  /\ s.h \in {"apply", "reply"} /\ ~s.revoked /\ s.faults > 0
  /\ s' = [s EXCEPT !.revoked = TRUE, !.interrupted = TRUE,
                    !.faults = @ - 1, !.stopH = TRUE]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>

QueueIntent ==
  /\ s.qh = "idle" /\ ~s.stopQ
  /\ s' = [s EXCEPT !.qh = "lock", !.tq = Append(@, "queue")]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>
QueueGrant ==
  /\ s.qh = "lock" /\ s.token = "none" /\ s.tq # <<>> /\ Head(s.tq) = "queue"
  /\ s' = [s EXCEPT !.token = "queue", !.tq = Tail(@), !.qh = "authority"]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>
QueueAuthorityReturn ==
  /\ s.qh = "authority"
  /\ s' = [s EXCEPT !.qh = "send"]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>
QueueDispatch ==
  /\ s.qh = "send"
  /\ s' = [s EXCEPT !.qh = "apply"]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>
QueueApply ==
  /\ s.qh = "apply"
  /\ s' = [s EXCEPT !.qh = "reply"]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>
QueueReturn ==
  /\ s.qh = "reply"
  /\ s' = [s EXCEPT !.token = "none", !.qh = IF s.stopQ THEN "off" ELSE "idle"]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>
QueueStop ==
  /\ s.stopQ /\ s.qh = "idle"
  /\ s' = [s EXCEPT !.qh = "off"]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>

ReleaseJoin ==
  /\ s.pc = "release_join"
  /\ s' = [s EXCEPT !.stopH = TRUE, !.revoked = TRUE,
       !.pc = IF s.h = "off" /\ s.call = "none" THEN
                 IF s.domain THEN "release_send"
                 ELSE IF s.settled THEN "finish_join" ELSE "settle_token"
              ELSE "release_join",
       !.afterSettle = "finish_join"]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>
(* One failed release is enough to exhibit retry/reacquire; remaining five
   source attempts are represented by the same caller-return dependency.
   The model retains the source's exact maximum of five attempted releases. *)
ReleaseFailure ==
  /\ s.pc = "release_wait" /\ s.call = "sent" /\ s.faults > 0
  /\ s' = [s EXCEPT !.call = "none", !.waiting = FALSE, !.kind = "none",
       !.releaseTries = @ + 1, !.faults = @ - 1,
       !.pc = IF s.releaseTries + 1 < 5 THEN "release_backoff" ELSE "settle_token",
       !.domain = IF s.releaseTries + 1 = 5 THEN FALSE ELSE @,
       !.relation = IF s.releaseTries + 1 = 5 THEN "none" ELSE @,
       !.eligible = FALSE]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>
ReleaseRetry ==
  /\ s.pc = "release_backoff" /\ s.eligible
  /\ s' = [s EXCEPT !.pc = "release_send"]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>
DomainExpiry ==
  /\ s.releaseTries = 5 /\ ~s.domain /\ domainOwner = "selected" /\ s.eligible
  /\ s' = [s EXCEPT !.releaseTries = 0]
  /\ domainOwner' = IF domainOwner = "selected" THEN "none" ELSE domainOwner
  /\ UNCHANGED <<peer, request, waiter>>
(* Lost settlement is no committed transition: the running queue row needs
   lease expiry. A lost ack AFTER apply has already queued it and is harmless. *)
LostSettlement ==
  /\ s.pc = "settle_wait" /\ s.call = "sent" /\ s.faults > 0
  /\ s' = [s EXCEPT !.call = "none", !.kind = "none", !.waiting = FALSE,
       !.faults = @ - 1, !.token = "none", !.settled = TRUE, !.pc = s.afterSettle]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>
FinishJoin ==
  /\ s.pc = "finish_join"
  /\ s' = [s EXCEPT !.stopQ = TRUE,
       !.pc = IF s.qh = "off" THEN "finish_token" ELSE "finish_join"]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>
FinishRetire ==
  /\ s.pc = "finish_retire"
  /\ s' = [s EXCEPT !.claim = FALSE, !.qlive = FALSE,
                    !.token = "none", !.pc = "drop"]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>
DropAdmission ==
  /\ s.pc = "drop"
  /\ s' = [s EXCEPT !.admitted = FALSE, !.cleaned = TRUE,
                    !.eligible = FALSE, !.pc = "backoff"]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>
QueueExpiry ==
  /\ Mutation # "lost_settlement" /\ ~s.claim /\ s.qstate = "running" /\ s.eligible
  /\ s' = [s EXCEPT !.qstate = "queued"]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>
Tick ==
  /\ s' = [s EXCEPT !.tick = ~@, !.eligible = TRUE]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>
Wake ==
  /\ s.pc = "backoff" /\ s.eligible /\ s.qstate = "queued"
  /\ request = "pending" /\ ~s.cancel /\ ~s.exhausted
  /\ s' = [s EXCEPT !.pc = "probe", !.revoked = FALSE,
       !.interrupted = FALSE, !.bound = FALSE, !.stopH = FALSE, !.stopQ = FALSE,
       !.walk = "none", !.releaseTries = 0, !.settled = FALSE, !.afterSettle = "release_join"]
  /\ UNCHANGED <<peer, domainOwner, request, waiter>>

(* Peer has physical admission/claim before asking for this same domain;
   failure yields/backoffs, success performs finite backend work and releases.
   Its backend return premise is explicit; no fairness selects it over us. *)
PeerClaim ==
  /\ Contention /\ peer = "idle"
  /\ peer' = "claimed"
  /\ UNCHANGED <<s, domainOwner, request, waiter>>
PeerAcquire ==
  /\ peer = "claimed"
  /\ peer' = IF domainOwner = "none" THEN "backend" ELSE "backoff"
  /\ domainOwner' = IF domainOwner = "none" THEN "peer" ELSE domainOwner
  /\ UNCHANGED <<s, request, waiter>>
PeerReturn ==
  /\ peer = "backend"
  /\ peer' = "release"
  /\ UNCHANGED <<s, domainOwner, request, waiter>>
PeerReleaseApply ==
  /\ peer = "release" /\ domainOwner = "peer"
  /\ peer' = "backoff" /\ domainOwner' = "none"
  /\ UNCHANGED <<s, request, waiter>>
PeerBackoff ==
  /\ peer = "backoff" /\ s.eligible
  /\ peer' = "idle"
  /\ UNCHANGED <<s, domainOwner, request, waiter>>

Next == ProbeReturn \/ Admission \/ ClaimSend \/ DomainSend \/ BindSend
     \/ ItemSend \/ CompleteSend \/ ReleaseSend \/ SettleSend
     \/ StoreApply \/ StoreReply \/ CallerReturn \/ CompleteTimeout \/ DetachedApply \/ DrainDetached
     \/ WorkerTokenIntent \/ WorkerTokenGrant \/ AuthorityReturn
     \/ WorkerPubIntent \/ WorkerPubGrant \/ ScanStart \/ WalkReturn \/ ScanFinish
     \/ Reconcile \/ Cancel \/ WorkFault \/ QueueLoss
     \/ DomainIntent \/ DomainGrant \/ ReverseTokenGrant \/ DomainDispatch
     \/ DomainApply \/ DomainReturn \/ DomainStop \/ RenewalDeadline
     \/ QueueIntent \/ QueueGrant \/ QueueAuthorityReturn \/ QueueDispatch
     \/ QueueApply \/ QueueReturn \/ QueueStop
     \/ ReleaseJoin \/ ReleaseFailure \/ ReleaseRetry \/ DomainExpiry
     \/ LostSettlement \/ FinishJoin \/ FinishRetire \/ DropAdmission
     \/ QueueExpiry \/ Tick \/ Wake
     \/ PeerClaim \/ PeerAcquire \/ PeerReturn \/ PeerReleaseApply \/ PeerBackoff

(* Each conjunct belongs to one separately scheduled dependency action.
   FsReturn and AuthorityReturn are environmental-return premises. Backend
   apply/reply fairness is the explicit stable quorum/transport premise. *)
Fair == /\ WF_vars(ProbeReturn) /\ WF_vars(Admission)
        /\ WF_vars(ClaimSend) /\ WF_vars(DomainSend) /\ WF_vars(BindSend)
        /\ WF_vars(ItemSend) /\ WF_vars(CompleteSend) /\ WF_vars(ReleaseSend)
        /\ WF_vars(SettleSend) /\ WF_vars(StoreApply) /\ WF_vars(StoreReply)
        /\ WF_vars(CallerReturn) /\ WF_vars(DrainDetached)
        /\ WF_vars(WorkerTokenIntent) /\ WF_vars(WorkerTokenGrant)
        /\ WF_vars(AuthorityReturn) /\ WF_vars(WorkerPubIntent) /\ WF_vars(WorkerPubGrant)
        /\ WF_vars(ScanStart)
        /\ (IF Mutation = "external_stall" THEN TRUE ELSE WF_vars(WalkReturn))
        /\ (IF Mutation = "renewal_livelock" THEN TRUE ELSE WF_vars(ScanFinish))
        /\ WF_vars(Reconcile)
        /\ WF_vars(DomainIntent) /\ WF_vars(DomainGrant) /\ WF_vars(ReverseTokenGrant)
        /\ WF_vars(DomainDispatch) /\ WF_vars(DomainApply) /\ WF_vars(DomainReturn)
        /\ WF_vars(DomainStop) /\ WF_vars(QueueIntent) /\ WF_vars(QueueGrant)
        /\ WF_vars(QueueAuthorityReturn) /\ WF_vars(QueueDispatch)
        /\ WF_vars(QueueApply) /\ WF_vars(QueueReturn) /\ WF_vars(QueueStop)
        /\ WF_vars(ReleaseJoin) /\ WF_vars(ReleaseRetry) /\ WF_vars(DomainExpiry)
        /\ WF_vars(FinishJoin) /\ WF_vars(FinishRetire) /\ WF_vars(DropAdmission)
        /\ WF_vars(QueueExpiry) /\ WF_vars(Tick) /\ WF_vars(Wake)
        /\ WF_vars(PeerClaim) /\ WF_vars(PeerAcquire) /\ WF_vars(PeerReturn)
        /\ WF_vars(PeerReleaseApply) /\ WF_vars(PeerBackoff)
Spec == Init /\ [][Next]_vars /\ Fair

TypeOK == /\ request \in {"pending", "done"} /\ waiter \in {"pending", "terminal"}
          /\ domainOwner \in {"none", "selected", "peer"}
          /\ s.faults \in 0..FaultBudget /\ s.retries \in 0..RetryLimit
          /\ Len(s.tq) <= 2 /\ Len(s.pq) <= 2
          /\ s.token \in {"none", "worker", "queue", "domain"}
          /\ s.pub \in {"none", "worker", "domain"}
ResultAtomicity == (request = "done") <=> (waiter = "terminal")
AdmissionUntilJoined == s.walk = "busy" => s.admitted
NoFictitiousRelease == s.pub = "domain" => s.h \in {"reverse", "send", "apply", "reply"}
UsefulProgress == ([](~s.cancel /\ ~s.exhausted)) => (request = "pending" ~> request = "done")
AttemptProgress == s.qstate = "running" ~> s.qstate = "queued"
AttemptCleanup == s.admitted ~> Cleanup
NoSuccessWitness == ~(request = "done" /\ Cleanup /\ s.cleaned)
NoReconcileWitness == ~(request = "done" /\ s.reconciled /\ Cleanup /\ s.cleaned)
NoCancelCleanupWitness == ~(s.cancel /\ Cleanup /\ s.cleaned /\ request = "pending")
=============================================================================
