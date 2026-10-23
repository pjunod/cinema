---------------------- MODULE AsyncDriverRefinement ----------------------
EXTENDS Naturals, Integers, Sequences, FiniteSets, TLC, SourceTables
CONSTANTS StableSuccess, Mutation, Witness
VARIABLES sql, w, available, completed, mono_now_ms, wall_anchors, mono_anchors, claim_loss_keys
vars == <<sql, w, available, completed, mono_now_ms, wall_anchors, mono_anchors, claim_loss_keys>>
Nodes == 0..1
OrdinaryIdle == [phase |-> "off",payload |-> "",captured_token |-> "",lost_seen |-> "",
 deadline_seen |-> "",guard_mono_ms |-> "",request_now_ms |-> "",api_outcome |-> "",observed_outcome |-> ""]
Idle == [stage |-> "idle", job |-> -1, attempt |-> "", token |-> -1, lease |-> -1,
         ctrl |-> -1, call |-> "ready", reply |-> "", accepted |-> FALSE,
         localToken |-> FALSE, retire |-> "off", inner |-> "ready",
         original |-> -1, resolved |-> -1, outcome |-> "", sample |-> -1,
         firstSample |-> -1, resolveSample |-> -1, afterSample |-> -1,
         started |-> FALSE, dropped |-> FALSE, joined |-> FALSE,
         workStarted |-> FALSE, faultUsed |-> FALSE, admission |-> FALSE, readLock |-> FALSE,claim_snapshot |-> "",
 claim_issued_mono_ms |-> "",claim_request_now_ms |-> "",local_deadline_mono_ms |-> "",
 queue_lost |-> FALSE,cancelling |-> FALSE,driver_settlement |-> "",adapter_reply |-> "",ordinary |-> OrdinaryIdle,claim_request |-> "",claim_recovery |-> [phase |-> "off",wake_mono_ms |-> ""]]
D(n) == DriverStates[w[n].ctrl + 1]
S == StoreStates[sql + 1]
Edges == {e \in StoreEdges : e.src = sql}
CurrentToken(n, e) == e.job = w[n].job /\ e.token = w[n].token
CurrentLease(n, e) == e.lease = w[n].lease
ReplyEdge(n, tag) == {e \in DriverEdges : e.src = w[n].ctrl /\ e.reply = tag}
DoneJobs(s) == {j \in Nodes : StoreStates[s+1].jobs[j+1].done}
SeqSet(xs) == {xs[k] : k \in 1..Len(xs)}
LeaseMs == 30000
(* Importer independently certifies all inputs and candidate arithmetic fit the
   pinned TLC signed-int representation; source i64 saturation is unreachable.
   Failure rejects the artifact globally, never excludes a Next transition. *)
SatAdd(a,b) == a+b
Deadline(issued,expiry,requestNow) == issued + IF expiry-requestNow < 0 THEN 0
 ELSE IF expiry-requestNow > LeaseMs THEN LeaseMs ELSE expiry-requestNow
Min(Ss) == CHOOSE x \in Ss : \A y \in Ss : x <= y
NextPoint(now,anchors) == LET later == {x \in {a+d : a \in anchors,d \in {(-1),0,1}} : x > now /\ x >= 0}
 IN IF later = {} THEN "" ELSE Min(later)
WallSources(si,ws) == LET ii == StoreStates[si+1].input_domain IN
 {ii.jobs[j+1].expiry : j \in {k \in Nodes : ii.jobs[k+1].expiry # ""}}
 \cup {ii.jobs[j+1].not_before_ms : j \in {k \in Nodes : ii.jobs[k+1].state = "queued"}}
 \cup UNION {{t.lease_expires_ms : t \in SeqSet(ii.captured_tokens[j+1])} : j \in Nodes}
 \cup {l.expires_at_unix_ms : l \in SeqSet(ii.captured_leases)}
 \cup (IF ii.domain = "" THEN {} ELSE {ii.domain.expiry})
 \cup UNION {IF ws[n].driver_settlement = "" THEN {} ELSE {ws[n].driver_settlement.not_before_ms} : n \in Nodes}
 \cup UNION {IF ws[n].ordinary.payload = "" THEN {} ELSE {ws[n].ordinary.payload.not_before_ms} : n \in Nodes}
MonoSources(ws) == UNION {(IF ws[n].claim_issued_mono_ms = "" \/ (ws[n].token < 0 /\ ws[n].claim_recovery.phase = "off") THEN {} ELSE {ws[n].claim_issued_mono_ms+LeaseMs})
 \cup (IF ws[n].claim_recovery.wake_mono_ms = "" THEN {} ELSE {ws[n].claim_recovery.wake_mono_ms})
 \cup (IF ws[n].local_deadline_mono_ms = "" THEN {} ELSE {ws[n].local_deadline_mono_ms}) : n \in Nodes}
AnchorFrame == /\ wall_anchors' = wall_anchors \cup WallSources(sql',w')
 /\ mono_anchors' = mono_anchors \cup MonoSources(w')
LossKey(n) == [job |-> w[n].job,node |-> IF w[n].claim_request.node_id = "node-a" THEN 0 ELSE 1,claim_id |-> w[n].claim_request.claim_id]
LossFrame == claim_loss_keys' = claim_loss_keys \cup {LossKey(n) : n \in {k \in Nodes :
 w[k].stage = "claim" /\ w[k].call = "applied" /\ w'[k].call = "replied" /\ w'[k].reply = "Unknown"}}
BaseStep(A) == A /\ UNCHANGED mono_now_ms /\ AnchorFrame /\ LossFrame
OrdinaryLocked(n) == w[n].ordinary.phase \in {"check_lost","check_deadline","take_token","sample","sent","applied","finish"}
OrdinaryInput(n) == [op |-> "ordinary_settle",job |-> w[n].job,token |-> w[n].ordinary.captured_token,
 kind |-> w[n].ordinary.payload.kind,not_before_ms |-> w[n].ordinary.payload.not_before_ms,
 request_now_ms |-> w[n].ordinary.request_now_ms,error_code |-> w[n].ordinary.payload.error_code,checkpoint |-> ""]
HeldDomain(n) == w[n].stage = "driver" /\ w[n].lease >= 0
 /\ (D(n).domain \/ D(n).pending = "AcquireDomain")
 /\ ~(D(n).pending \in {"ReleaseRefused","ReleaseRetry","ReleaseNone","ReleaseInterrupted"} /\ w[n].call \in {"applied","replied"})
StableWall(t) == ~StableSuccess \/ \A n \in Nodes :
 /\ ((w[n].localToken \/ (w[n].stage = "claim" /\ w[n].token >= 0)) =>
      t < S.input_domain.captured_tokens[w[n].job+1][w[n].token+1].lease_expires_ms)
 /\ (HeldDomain(n) => t < S.input_domain.captured_leases[w[n].lease+1].expires_at_unix_ms)
 /\ (w[n].ordinary.phase \in {"waiting_lock","check_lost","check_deadline","take_token","sample"} => t <= w[n].ordinary.payload.not_before_ms)
StableMono(u) == ~StableSuccess \/ \A n \in Nodes :
 /\ ((w[n].stage = "driver" /\ w[n].started /\ ~w[n].dropped) => u < w[n].local_deadline_mono_ms)
 /\ ((w[n].stage = "claim" /\ w[n].token >= 0) =>
       u < w[n].claim_issued_mono_ms+LeaseMs /\ u < w[n].local_deadline_mono_ms)
Init == /\ sql = StoreInitial
        /\ w = [n \in Nodes |-> Idle]
        /\ available = [n \in Nodes |-> TRUE]
        /\ completed = DoneJobs(sql)
        /\ mono_now_ms = 0 /\ wall_anchors = WallSources(StoreInitial,[n \in Nodes |-> Idle])
        /\ mono_anchors = {} /\ claim_loss_keys = {}

Admission(n) ==
 /\ w[n].stage = "idle" /\ available[n]
 /\ \E j \in Nodes :
     /\ S.jobs[j+1].state = "queued" /\ ~S.jobs[j+1].done /\ S.input_domain.jobs[j+1].not_before_ms <= S.now
     /\ (~StableSuccess \/ j = n)
     /\ w' = [w EXCEPT ![n] = [Idle EXCEPT !.stage = "claim", !.job = j, !.admission = TRUE]]
 /\ UNCHANGED <<sql, available, completed>>
ClaimSend(n) ==
 /\ w[n].stage = "claim" /\ w[n].call = "ready"
 /\ w' = [w EXCEPT ![n].call = "sent"]
 /\ UNCHANGED <<sql, available, completed>>
ClaimApplyResult(n, success) ==
 /\ w[n].stage = "claim" /\ w[n].call = "sent"
 /\ \E e \in Edges :
    /\ e.op = "claim" /\ e.node = n /\ e.job = w[n].job
    /\ (e.result = "claimed") = success
    /\ sql' = e.dst
    /\ w' = [w EXCEPT ![n].call = "applied",
        ![n].reply = IF success THEN "Claimed" ELSE "Refused",
        ![n].token = IF success THEN Len(StoreStates[e.dst+1].tokens[w[n].job+1])-1 ELSE -1,
        ![n].attempt = IF success THEN StoreStates[e.dst+1].tokens[w[n].job+1][Len(StoreStates[e.dst+1].tokens[w[n].job+1])] ELSE "",
        ![n].claim_snapshot = IF success THEN e.claim_snapshot ELSE "",
        ![n].claim_request = e.claim_request,
        ![n].claim_issued_mono_ms = mono_now_ms,
        ![n].claim_request_now_ms = S.now,
        ![n].local_deadline_mono_ms = IF success THEN Deadline(mono_now_ms,e.claim_snapshot.token_capture.lease_expires_ms,S.now) ELSE ""]
 /\ UNCHANGED <<available, completed>>
ClaimWin(n) == ClaimApplyResult(n, TRUE)
ClaimApply(n) == ClaimWin(n) \/ ClaimApplyResult(n, FALSE)
DirectTimely(n) == w[n].token < 0 \/ (mono_now_ms < w[n].claim_issued_mono_ms+LeaseMs /\ mono_now_ms < w[n].local_deadline_mono_ms)
ClaimReply(n) ==
 /\ w[n].stage = "claim" /\ w[n].call = "applied"
 /\ \E lost \in IF StableSuccess \/ w[n].faultUsed \/ LossKey(n) \in claim_loss_keys THEN {FALSE} ELSE BOOLEAN :
     LET observed == IF DirectTimely(n) THEN w[n].reply ELSE "Refused" IN
     w' = [w EXCEPT ![n].call = "replied",![n].reply = IF lost THEN "Unknown" ELSE observed,
       ![n].faultUsed = @ \/ lost,
       ![n].claim_recovery = [phase |-> IF lost THEN "ready" ELSE "off",wake_mono_ms |-> ""],
       ![n].claim_snapshot = IF lost \/ observed # "Claimed" THEN "" ELSE @,
       ![n].local_deadline_mono_ms = IF lost \/ observed # "Claimed" THEN "" ELSE @,
       ![n].claim_request = IF ~lost /\ observed # "Claimed" THEN "" ELSE @,
       ![n].claim_issued_mono_ms = IF ~lost /\ observed # "Claimed" THEN "" ELSE @,
       ![n].claim_request_now_ms = IF ~lost /\ observed # "Claimed" THEN "" ELSE @]
 /\ UNCHANGED <<sql, available, completed>>
RecoveryReady(n) == w[n].stage = "claim" /\ w[n].reply = "Unknown"
 /\ (w[n].claim_recovery.phase = "ready" \/
      (w[n].claim_recovery.phase = "waiting" /\ mono_now_ms >= w[n].claim_recovery.wake_mono_ms))
 /\ mono_now_ms < w[n].claim_issued_mono_ms+LeaseMs
InitialResolveInput(n) == [op |-> "resolve_initial_claim",job |-> w[n].job,
 request |-> [job_id |-> w[n].claim_request.job_id,node_id |-> w[n].claim_request.node_id,
 boot_id |-> w[n].claim_request.boot_id,claim_id |-> w[n].claim_request.claim_id,
 fence |-> "",dispatched_at_ms |-> w[n].claim_request.dispatched_at_ms,now_ms |-> S.now]]
InitialResolutions(n) == {e \in Edges : e.input = InitialResolveInput(n)}
RecoveryReads(e) == {r \in StoreEdges : r.src = e.dst /\ r.input = [op |-> "read_claim_job",job |-> e.job]}
RecoveryCancels(e) == {c \in StoreEdges : c.src = e.dst /\ c.input = [op |-> "settle",job |-> e.job,token |-> e.resolved,disposition |-> 2]}
IdentifierChars == "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789._:-"
Identifier(g) == Len(g) \in 1..256 /\ \A i \in 1..Len(g) :
 SubSeq(g,i,i) \in {SubSeq(IdentifierChars,k,k) : k \in 1..Len(IdentifierChars)}
ReadSupported(job,request,token) == job # "" /\ job.token = token /\ job.payload_version = 1
 /\ DOMAIN job.payload = {"kind","library_id","generation"}
 /\ job.payload.kind = request.kind /\ job.payload.kind \in {"library_scan","metadata_refresh"}
 /\ job.payload.library_id > 0 /\ Identifier(job.payload.generation)
RecoveredSnapshot(n,job) == [job |-> w[n].job,raw_job_id |-> job.id,failed_attempts |-> job.failed_attempts,
 claim_id |-> job.token.claim_id,token_capture |-> job.token]
RecoveryWorker(n,e,r) == LET
 t == IF e.resolved = -1 THEN "" ELSE StoreStates[e.dst+1].input_domain.captured_tokens[w[n].job+1][e.resolved+1]
 job == IF r = "" THEN "" ELSE r.job_read
 good == e.result = "running" /\ ReadSupported(job,w[n].claim_request,t)
 deadline == IF good THEN Deadline(w[n].claim_issued_mono_ms,t.lease_expires_ms,w[n].claim_request.now_ms) ELSE 0
 terminal == good \/ e.result \in {"cancel_requested","settled","lost_ownership"}
 wake == IF mono_now_ms+250 < w[n].claim_issued_mono_ms+LeaseMs THEN mono_now_ms+250 ELSE w[n].claim_issued_mono_ms+LeaseMs
 IN [w[n] EXCEPT
  !.reply = IF terminal THEN IF good /\ mono_now_ms < deadline THEN "Claimed" ELSE "Refused" ELSE "Unknown",
  !.claim_recovery = [phase |-> IF terminal THEN "off" ELSE "waiting",wake_mono_ms |-> IF terminal THEN "" ELSE wake],
  !.token = IF terminal THEN IF good /\ mono_now_ms < deadline THEN e.resolved ELSE -1 ELSE @,
  !.attempt = IF terminal THEN IF good /\ mono_now_ms < deadline THEN t.claim_id ELSE "" ELSE @,
  !.claim_snapshot = IF good /\ mono_now_ms < deadline THEN RecoveredSnapshot(n,job) ELSE "",
  !.local_deadline_mono_ms = IF good /\ mono_now_ms < deadline THEN deadline ELSE ""]
ClaimRecover(n) ==
 /\ w[n].stage = "claim" /\ w[n].reply = "Unknown" /\ w[n].claim_recovery.phase # "off"
 /\ \/ /\ mono_now_ms >= w[n].claim_issued_mono_ms+LeaseMs
        /\ w' = [w EXCEPT ![n].reply = "Refused",![n].token = -1,![n].attempt = "",![n].claim_snapshot = "",![n].local_deadline_mono_ms = "",![n].claim_recovery = [phase |-> "off",wake_mono_ms |-> ""]]
        /\ UNCHANGED <<sql,available,completed>>
     \/ /\ RecoveryReady(n)
        /\ \E e \in InitialResolutions(n) :
           \E r \in IF e.result = "running" THEN RecoveryReads(e) ELSE {""} :
           \E c \in IF e.result = "cancel_requested" THEN RecoveryCancels(e) ELSE {""} :
            LET final == IF r # "" THEN r.dst ELSE IF c # "" THEN c.dst ELSE e.dst IN
            /\ sql' = final /\ completed' = completed \cup DoneJobs(final)
            /\ w' = [w EXCEPT ![n] = RecoveryWorker(n,e,r)] /\ UNCHANGED available
ClaimReturn(n) ==
 /\ w[n].stage = "claim" /\ w[n].call = "replied" /\ w[n].reply # "Unknown"
 /\ w' = [w EXCEPT ![n] = IF w[n].reply = "Claimed" /\ mono_now_ms < w[n].local_deadline_mono_ms
       THEN [@ EXCEPT !.stage = "driver", !.ctrl = DriverInitial,
             !.call = "ready", !.started = TRUE, !.localToken = TRUE]
       ELSE Idle]
 /\ UNCHANGED <<sql, available, completed>>

Dispatch(n) ==
 /\ w[n].stage = "driver" /\ ~D(n).terminal /\ w[n].call = "ready"
 /\ w' = [w EXCEPT ![n].call = "sent",![n].readLock = D(n).pending = "PollWork",
       ![n].accepted = FALSE,![n].adapter_reply = "",
       ![n].ordinary = IF D(n).pending \in {"SettleYield","SettleRetry"}
        THEN [OrdinaryIdle EXCEPT !.phase = "waiting_lock",!.payload =
         [kind |-> w[n].driver_settlement.kind,not_before_ms |-> w[n].driver_settlement.not_before_ms,
          error_code |-> IF D(n).pending = "SettleRetry" THEN "library_execution_failed" ELSE "",checkpoint |-> ""]] ELSE @]
 /\ UNCHANGED <<sql,available,completed>>
CommitReply(n, e, tag) ==
 /\ ReplyEdge(n, tag) # {}
 /\ sql' = e.dst
 /\ completed' = completed \cup DoneJobs(e.dst)
 /\ w' = [w EXCEPT ![n].call = "applied", ![n].reply = tag,
       ![n].accepted = e.op = "settle" /\ e.result = "accepted",
       ![n].lease = IF e.op = "acquire_domain" /\ e.result = "acquired"
                    THEN Len(StoreStates[e.dst+1].leases)-1 ELSE @]
 /\ UNCHANGED available
LocalReply(n, tag) ==
 /\ ReplyEdge(n, tag) # {}
 /\ w' = [w EXCEPT ![n].call = "applied", ![n].reply = tag, ![n].accepted = FALSE,![n].queue_lost = @ \/ tag = "QueueLoss"]
 /\ UNCHANGED <<sql, available, completed>>
StoreApply(n) ==
 /\ w[n].stage = "driver" /\ w[n].call = "sent"
 /\ \E e \in Edges :
   \/ /\ D(n).pending = "AcquireDomain" /\ e.op = "acquire_domain" /\ e.node = n
      /\ (~StableSuccess \/ e.result = "acquired")
      /\ CommitReply(n, e, IF e.result = "acquired" THEN "DomainAcquired" ELSE "DomainRefused")
   \/ /\ D(n).pending = "Bind" /\ e.op = "bind"
      /\ CurrentToken(n,e) /\ CurrentLease(n,e)
      /\ (~StableSuccess \/ e.result = "accepted")
      /\ CommitReply(n,e,IF e.result = "accepted" THEN "BindAccepted" ELSE "BindRefused")
   \/ /\ D(n).pending \in {"ReleaseRefused","ReleaseRetry","ReleaseNone","ReleaseInterrupted"}
      /\ e.op = "release_domain" /\ CurrentLease(n,e)
      /\ CommitReply(n,e,"ReleaseReturned")
AmbiguousWithoutApply(n) ==
 /\ ~StableSuccess
 /\ w[n].stage = "driver" /\ w[n].call = "sent" /\ ~w[n].faultUsed
 /\ D(n).pending \in {"AcquireDomain","Bind",
     "ReleaseRefused","ReleaseRetry","ReleaseNone","ReleaseInterrupted"}
 /\ LET tag == CASE D(n).pending = "AcquireDomain" -> "DomainRefused"
                 [] D(n).pending = "Bind" -> "BindRefused"
                 [] OTHER -> "ReleaseReturned" IN
    /\ ReplyEdge(n,tag) # {}
    /\ w' = [w EXCEPT ![n].call = "applied", ![n].reply = tag, ![n].faultUsed = TRUE, ![n].accepted = FALSE]
 /\ UNCHANGED <<sql,available,completed>>
SimpleApply(n) ==
 /\ w[n].stage = "driver" /\ w[n].call = "sent"
 /\ D(n).pending \in {"RevokeDomain","CancelWork","DropPublisher","DropAdmission"}
 /\ ~w[n].readLock
 /\ LocalReply(n,"Done")

(* finish drains the heartbeat, then retire takes the local token under its
   mutex. A held leaf queue handle is not this Option<Token>. Every sample,
   primitive dispatch/application/reply/observation is a separate action.
   Logical resolve is an actual API edge, not closure of its two inner reads.
   The imported primitive uses current graph time: captured-time races outside
   that restriction remain explicit conformance/capacity boundaries. *)
Retiring(n) == w[n].stage = "driver" /\ w[n].call = "sent"
 /\ D(n).pending = "RetireQueue"
RetireBegin(n) ==
 /\ Retiring(n) /\ w[n].retire = "off" /\ ~w[n].readLock
 /\ w' = [w EXCEPT ![n].retire = "drain",![n].queue_lost = TRUE]
 /\ UNCHANGED <<sql,available,completed>>
RetireDrain(n) ==
 /\ Retiring(n) /\ w[n].retire = "drain"
 /\ w' = [w EXCEPT ![n].retire = "take"]
 /\ UNCHANGED <<sql,available,completed>>
RetireTake(n) ==
 /\ Retiring(n) /\ w[n].retire = "take"
 /\ w' = [w EXCEPT ![n].retire = IF w[n].localToken THEN "sample-first" ELSE "done",
       ![n].original = w[n].token, ![n].localToken = FALSE]
 /\ UNCHANGED <<sql,available,completed>>
RetireSample(n) ==
 /\ Retiring(n) /\ w[n].retire \in {"sample-first","sample-resolve","sample-after"}
 /\ \E clockError \in IF w[n].faultUsed THEN {FALSE} ELSE BOOLEAN :
    w' = [w EXCEPT ![n].sample = IF clockError THEN -1 ELSE S.now,
       ![n].firstSample = IF w[n].retire = "sample-first" THEN (IF clockError THEN -1 ELSE S.now) ELSE @,
       ![n].resolveSample = IF w[n].retire = "sample-resolve" THEN (IF clockError THEN -1 ELSE S.now) ELSE @,
       ![n].afterSample = IF w[n].retire = "sample-after" THEN (IF clockError THEN -1 ELSE S.now) ELSE @,
       ![n].inner = "ready", ![n].faultUsed = @ \/ clockError,
       ![n].retire = IF clockError THEN "warn" ELSE
         CASE @ = "sample-first" -> "first"
           [] @ = "sample-resolve" -> "resolve"
           [] w[n].outcome \in {"running","cancel_requested"} -> "second"
           [] OTHER -> "done"]
 /\ UNCHANGED <<sql,available,completed>>
RetireSend(n) ==
 /\ Retiring(n) /\ w[n].retire \in {"first","resolve","second"} /\ w[n].inner = "ready"
 /\ w' = [w EXCEPT ![n].inner = "sent"]
 /\ UNCHANGED <<sql,available,completed>>
(* Some(original.fence) makes dispatched_at_ms irrelevant in resolve_claim:
   its sole source use is the fence=None window guard. This reviewed source
   lemma permits the graph representative dispatch=now while firstSample is
   retained; resolve.now_ms must still equal application time. *)
RetireEdge(n,e) ==
 /\ e.job = w[n].job /\ w[n].sample = S.now
 /\ IF w[n].retire = "resolve"
     THEN e.op = "resolve" /\ e.token = w[n].original
     ELSE e.op = "settle"
       /\ e.token = IF w[n].retire = "first" THEN w[n].original ELSE w[n].resolved
       /\ e.disposition = IF w[n].cancelling \/ (w[n].retire = "second" /\ w[n].outcome = "cancel_requested") THEN 2 ELSE 3
RetireApply(n) ==
 /\ Retiring(n) /\ w[n].retire \in {"first","resolve","second"} /\ w[n].inner = "sent"
 /\ \E e \in Edges :
    /\ RetireEdge(n,e)
    /\ sql' = e.dst /\ completed' = completed \cup DoneJobs(e.dst)
    /\ w' = [w EXCEPT ![n].inner = "applied", ![n].outcome = e.result,
          ![n].resolved = IF e.op = "resolve" THEN e.resolved ELSE @]
 /\ UNCHANGED available
RetireError(n) ==
 /\ Retiring(n) /\ w[n].retire \in {"first","resolve","second"}
 /\ w[n].inner = "sent" /\ ~w[n].faultUsed
 /\ w' = [w EXCEPT ![n].inner = "applied", ![n].outcome = "error", ![n].faultUsed = TRUE]
 /\ UNCHANGED <<sql,available,completed>>
RetireCallReturn(n) == RetireApply(n) \/ RetireError(n)
RetireReply(n) ==
 /\ Retiring(n) /\ w[n].retire \in {"first","resolve","second"} /\ w[n].inner = "applied"
 /\ \E lost \in IF w[n].faultUsed THEN {FALSE} ELSE BOOLEAN :
    w' = [w EXCEPT ![n].inner = "replied", ![n].faultUsed = @ \/ lost,
          ![n].outcome = IF lost THEN "error" ELSE @]
 /\ UNCHANGED <<sql,available,completed>>
RetireObserve(n) ==
 /\ Retiring(n) /\ w[n].retire \in {"first","resolve","second"} /\ w[n].inner = "replied"
 /\ w' = [w EXCEPT ![n].retire =
       CASE @ = "first" -> IF w[n].outcome = "accepted" THEN "done" ELSE "sample-resolve"
         [] @ = "resolve" -> IF w[n].outcome = "error" THEN "warn" ELSE "sample-after"
         [] OTHER -> IF w[n].outcome = "error" THEN "warn" ELSE "done"]
 /\ UNCHANGED <<sql,available,completed>>
RetireFinish(n) ==
 /\ Retiring(n) /\ w[n].retire \in {"done","warn"}
 /\ LocalReply(n,"Done")
RetireLocal(n) == RetireBegin(n) \/ RetireTake(n) \/ RetireSample(n)
 \/ RetireSend(n) \/ RetireReply(n) \/ RetireObserve(n) \/ RetireFinish(n)

(* Publication is one opaque supported-work boundary, with its actual durable
   COMPLETE transaction. Its read guard spans send/apply/reply, never revoke.
   Successful external work includes the paired request completion. Inner
   catalogue inserts and lock scheduling are conformance contracts, not SQL
   predicates copied into this model. *)
WorkSuccess(n) ==
 /\ w[n].stage = "driver" /\ w[n].call = "sent" /\ D(n).pending = "PollWork"
 /\ \E e \in Edges :
    /\ e.op = "complete" /\ CurrentToken(n,e) /\ CurrentLease(n,e)
    /\ ~w[n].queue_lost /\ e.result = "accepted" /\ CommitReply(n,e,"WorkSucceeded")
WorkOther(n) ==
 /\ ~StableSuccess /\ w[n].stage = "driver" /\ w[n].call = "sent"
 /\ D(n).pending = "PollWork"
 /\ \E tag \in {"QueueLoss","DomainLoss","Pressure","WorkFailed"} :
    ((tag = "QueueLoss") = w[n].queue_lost) /\ LocalReply(n,tag)
ExternalReturn(n) == WorkSuccess(n) \/ WorkOther(n)
JoinReturn(n) ==
 /\ w[n].stage = "driver" /\ w[n].call = "sent" /\ D(n).pending = "JoinWork"
 /\ Mutation # "withheld-join"
 /\ \E tag \in IF D(n).work = "completed_succeeded" THEN {"JoinedSucceeded"}
               ELSE IF D(n).work = "completed_failed" THEN {"JoinedFailed"}
               ELSE {"JoinedFailed","JoinedSucceeded"} : LocalReply(n,tag)
Reply(n) ==
 /\ w[n].stage = "driver" /\ w[n].call = "applied" /\ D(n).pending \notin {"SettleYield","SettleRetry"}
 /\ \E lost \in IF ~w[n].faultUsed /\ D(n).pending \in
     {"AcquireDomain","Bind","PollWork",
      "ReleaseRefused","ReleaseRetry","ReleaseNone","ReleaseInterrupted"}
     /\ (~StableSuccess \/ D(n).pending \in
       {"PollWork","ReleaseRefused","ReleaseRetry","ReleaseNone","ReleaseInterrupted"})
     THEN BOOLEAN ELSE {FALSE} :
     w' = [w EXCEPT ![n].call = "replied", ![n].readLock = FALSE,
        ![n].reply = IF ~lost THEN @ ELSE
          CASE @ = "WorkSucceeded" -> "WorkFailed"
            [] @ = "DomainAcquired" -> "DomainRefused"
            [] @ = "BindAccepted" -> "BindRefused"
            [] OTHER -> @,
        ![n].faultUsed = @ \/ lost,
        ![n].adapter_reply = IF (IF lost THEN CASE w[n].reply = "WorkSucceeded" -> "WorkFailed"
          [] w[n].reply = "DomainAcquired" -> "DomainRefused" [] w[n].reply = "BindAccepted" -> "BindRefused" [] OTHER -> w[n].reply ELSE w[n].reply)
          \in {"DomainAcquired","DomainRefused","ReleaseReturned"} THEN "" ELSE
          [tag |-> IF lost /\ w[n].reply = "WorkSucceeded" THEN "WorkFailed" ELSE w[n].reply,now_ms |-> "",retry_delay_ms |-> ""],
        ![n].localToken = @]
 /\ UNCHANGED <<sql, available, completed>>
AdapterSample(n) ==
 /\ w[n].stage = "driver" /\ w[n].call = "replied" /\ w[n].adapter_reply = ""
 /\ w' = [w EXCEPT ![n].adapter_reply = [tag |-> w[n].reply,
   now_ms |-> IF w[n].reply = "DomainAcquired" \/ D(n).pending = "ReleaseNone" THEN 0 ELSE S.now,
   retry_delay_ms |-> IF D(n).pending = "ReleaseRetry" THEN RetryDelays[<<w[n].claim_snapshot.raw_job_id,w[n].claim_snapshot.failed_attempts>>] ELSE 0]]
 /\ UNCHANGED <<sql,available,completed>>
OrdinaryActive(n) == w[n].stage = "driver" /\ w[n].call = "sent" /\ D(n).pending \in {"SettleYield","SettleRetry"}
OrdinaryLock(n) == /\ OrdinaryActive(n) /\ w[n].ordinary.phase = "waiting_lock"
 /\ w' = [w EXCEPT ![n].ordinary.phase = "check_lost"] /\ UNCHANGED <<sql,available,completed>>
OrdinaryGuard(n) == /\ OrdinaryActive(n) /\ w[n].ordinary.phase \in {"check_lost","check_deadline","take_token"}
 /\ w' = [w EXCEPT ![n].ordinary = CASE w[n].ordinary.phase = "check_lost" ->
   [@ EXCEPT !.lost_seen = w[n].queue_lost,!.phase = IF w[n].queue_lost THEN "finish" ELSE "check_deadline",
      !.observed_outcome = IF w[n].queue_lost THEN "local_lost" ELSE ""]
 [] w[n].ordinary.phase = "check_deadline" -> [@ EXCEPT !.deadline_seen = w[n].local_deadline_mono_ms,
      !.guard_mono_ms = mono_now_ms,!.phase = IF mono_now_ms >= w[n].local_deadline_mono_ms THEN "finish" ELSE "take_token",
      !.observed_outcome = IF mono_now_ms >= w[n].local_deadline_mono_ms THEN "local_expired" ELSE ""]
 [] OTHER -> [@ EXCEPT !.captured_token = IF w[n].localToken THEN w[n].token ELSE "",
      !.phase = IF w[n].localToken THEN "sample" ELSE "finish",!.observed_outcome = IF w[n].localToken THEN "" ELSE "local_no_token"]]
 /\ UNCHANGED <<sql,available,completed>>
OrdinarySample(n) == /\ OrdinaryActive(n) /\ w[n].ordinary.phase = "sample"
 /\ \E err \in IF w[n].faultUsed THEN {FALSE} ELSE BOOLEAN :
    w' = [w EXCEPT ![n].ordinary.phase = IF err THEN "finish" ELSE "sent",
      ![n].ordinary.request_now_ms = IF err THEN "" ELSE S.now,
      ![n].ordinary.observed_outcome = IF err THEN "clock_error" ELSE "",![n].faultUsed = @ \/ err]
 /\ UNCHANGED <<sql,available,completed>>
OrdinaryFinish(n) == /\ OrdinaryActive(n) /\ w[n].ordinary.phase = "finish"
 /\ w' = [w EXCEPT ![n].ordinary.phase = "done",![n].localToken = IF w[n].ordinary.observed_outcome = "accepted" THEN FALSE ELSE @,
       ![n].call = "replied",![n].reply = "Done",![n].adapter_reply = [tag |-> "Done",now_ms |-> "",retry_delay_ms |-> ""]]
 /\ UNCHANGED <<sql,available,completed>>
OrdinaryLocal(n) == OrdinaryGuard(n) \/ OrdinarySample(n) \/ OrdinaryFinish(n)
OrdinaryApply(n) == /\ OrdinaryActive(n) /\ w[n].ordinary.phase = "sent"
 /\ \E e \in Edges : /\ e.input = OrdinaryInput(n) /\ sql' = e.dst
   /\ completed' = completed \cup DoneJobs(e.dst)
   /\ w' = [w EXCEPT ![n].ordinary.phase = "applied",![n].ordinary.api_outcome = e.result]
 /\ UNCHANGED available
OrdinaryError(n) == /\ OrdinaryActive(n) /\ w[n].ordinary.phase = "sent" /\ ~w[n].faultUsed
 /\ w' = [w EXCEPT ![n].ordinary.phase = "finish",![n].ordinary.observed_outcome = "transport_error",![n].faultUsed = TRUE]
 /\ UNCHANGED <<sql,available,completed>>
OrdinaryCallReturn(n) == OrdinaryApply(n) \/ OrdinaryError(n)
OrdinaryObserve(n) == /\ OrdinaryActive(n) /\ w[n].ordinary.phase = "applied"
 /\ \E lost \in IF w[n].faultUsed THEN {FALSE} ELSE BOOLEAN :
   w' = [w EXCEPT ![n].ordinary.phase = "finish",![n].ordinary.observed_outcome = IF lost THEN "transport_error" ELSE w[n].ordinary.api_outcome,
        ![n].faultUsed = @ \/ lost]
 /\ UNCHANGED <<sql,available,completed>>
QueueLossSignal(n) == /\ ~StableSuccess /\ w[n].stage = "driver" /\ w[n].started /\ ~w[n].dropped /\ ~w[n].queue_lost
 /\ w' = [w EXCEPT ![n].queue_lost = TRUE] /\ UNCHANGED <<sql,available,completed>>
DriverReturn(n) ==
 /\ w[n].stage = "driver" /\ w[n].call = "replied" /\ w[n].adapter_reply # ""
 /\ \E e \in ReplyEdge(n,w[n].reply) :
    /\ w' = [w EXCEPT ![n].ctrl = e.dst, ![n].call = "ready",
        ![n].driver_settlement = IF w[n].reply = "DomainRefused" \/ D(n).pending \in {"ReleaseRefused","ReleaseInterrupted","ReleaseRetry"}
         THEN [kind |-> IF D(n).pending = "ReleaseRetry" THEN "retry" ELSE "yield",
               not_before_ms |-> SatAdd(w[n].adapter_reply.now_ms,
                 IF D(n).pending = "ReleaseRetry" THEN w[n].adapter_reply.retry_delay_ms ELSE IF D(n).pending = "ReleaseInterrupted" THEN 1000 ELSE 5000)] ELSE IF w[n].reply = "Done" THEN @ ELSE "",
        ![n].adapter_reply = "",![n].joined = @ \/ D(n).pending = "JoinWork",
        ![n].workStarted = @ \/ (D(n).pending = "Bind" /\ w[n].reply = "BindAccepted"),
        ![n].dropped = @ \/ D(n).pending = "DropAdmission",
        ![n].admission = IF D(n).pending = "DropAdmission" \/
          (Mutation = "drop-before-join" /\ D(n).pending = "CancelWork") THEN FALSE ELSE @]
 /\ UNCHANGED <<sql, available, completed>>
Recycle(n) ==
 /\ w[n].stage = "driver" /\ D(n).terminal
 /\ (~StableSuccess \/ (w[n].dropped /\ ~S.jobs[w[n].job+1].done /\ S.input_domain.budgets.claims[w[n].job+1] < 2))
 /\ w' = [w EXCEPT ![n] = Idle]
 /\ UNCHANGED <<sql, available, completed>>

SameClaim(n,e) == w[n].stage \in {"claim","driver"} /\ w[n].job = e.job
 /\ w[n].token >= 0 /\ S.tokens[e.job+1][w[n].token+1] = S.tokens[e.job+1][e.token+1]
TokenLockAvailable(e) == \A n \in Nodes : SameClaim(n,e) =>
 w[n].localToken /\ ~w[n].queue_lost /\ e.token = w[n].token /\ ~OrdinaryLocked(n)
 /\ (w[n].call = "ready" \/ w[n].retire = "drain" \/ w[n].ordinary.phase \in {"waiting_lock","done"})
Matched(e) == {n \in Nodes : SameClaim(n,e)}
ResolveAfter(e) == {r \in StoreEdges : r.src = e.dst /\ r.input = [op |-> "resolve",job |-> e.job,token |-> e.token]}
RenewWorker(n,e,r,ack,err) ==
 IF e.op # "renew" \/ n \notin Matched(e) THEN w[n] ELSE
 LET used == w[n].faultUsed \/ ~ack \/ err IN
 IF err THEN [w[n] EXCEPT !.faultUsed = used,!.queue_lost = TRUE] ELSE
 LET outcome == IF r = "" THEN "running" ELSE r.result IN
 IF outcome \notin {"running","cancel_requested"}
 THEN [w[n] EXCEPT !.faultUsed = used,!.localToken = FALSE,!.queue_lost = TRUE] ELSE
 LET ti == IF r = "" THEN Len(StoreStates[e.dst+1].tokens[e.job+1])-1 ELSE r.resolved
     si == IF r = "" THEN e.dst ELSE r.dst
     token == StoreStates[si+1].input_domain.captured_tokens[e.job+1][ti+1]
     nextDeadline == Deadline(mono_now_ms,token.lease_expires_ms,S.now)
 IN IF outcome = "cancel_requested"
 THEN [w[n] EXCEPT !.faultUsed = used,!.token = ti,!.localToken = TRUE,!.cancelling = TRUE,!.queue_lost = TRUE]
 ELSE IF w[n].queue_lost \/ mono_now_ms >= w[n].local_deadline_mono_ms
 THEN [w[n] EXCEPT !.faultUsed = used,!.token = ti,!.localToken = TRUE,!.queue_lost = TRUE]
 ELSE [w[n] EXCEPT !.faultUsed = used,!.token = ti,!.localToken = TRUE,
       !.local_deadline_mono_ms = nextDeadline,!.queue_lost = @ \/ mono_now_ms >= nextDeadline]
Environment ==
 /\ ~StableSuccess
 /\ \E e \in Edges :
    /\ e.op \in {"remove_owner","acquire_domain","release_domain","renew"}
    /\ (e.op # "acquire_domain" \/ e.node = 2)
    /\ (e.op # "release_domain" \/ S.leases[e.lease+1].owner = 2)
    /\ (e.op # "renew" \/ TokenLockAvailable(e))
    /\ \E ack \in IF e.op = "renew" /\ e.result = "renewed" /\
          (\E n \in Nodes : SameClaim(n,e) /\ ~w[n].faultUsed) THEN BOOLEAN ELSE {TRUE} :
      LET need == IF e.op = "renew" THEN Matched(e) # {} /\ (e.result # "renewed" \/ ~ack) ELSE FALSE IN
      \E err \in IF need /\ ack /\ (\A n \in Matched(e) : ~w[n].faultUsed) THEN BOOLEAN ELSE {FALSE} :
      \E r \in IF need /\ ~err THEN ResolveAfter(e) ELSE {""} :
       /\ sql' = IF r = "" THEN e.dst ELSE r.dst
       /\ completed' = completed \cup DoneJobs(IF r = "" THEN e.dst ELSE r.dst)
       /\ w' = [n \in Nodes |-> RenewWorker(n,e,r,ack,err)]
    /\ UNCHANGED available
MakeAvailable(n) ==
 /\ ~available[n] /\ available' = [available EXCEPT ![n] = TRUE]
 /\ UNCHANGED <<sql,w,completed>>
RemoveAvailability(n) ==
 /\ available[n] /\ available' = [available EXCEPT ![n] = FALSE]
 /\ UNCHANGED <<sql,w,completed>>
Availability == \E n \in Nodes : MakeAvailable(n) \/ RemoveAvailability(n)
PhysicalOpportunity == \A n \in Nodes : WF_vars(MakeAvailable(n))
WorkerNext(n) == Admission(n) \/ ClaimSend(n) \/ ClaimApply(n) \/ ClaimReply(n) \/ ClaimRecover(n) \/ ClaimReturn(n)
 \/ Dispatch(n) \/ StoreApply(n) \/ AmbiguousWithoutApply(n) \/ SimpleApply(n) \/ ExternalReturn(n)
 \/ JoinReturn(n) \/ RetireLocal(n) \/ RetireDrain(n) \/ RetireCallReturn(n) \/ Reply(n) \/ DriverReturn(n) \/ Recycle(n)
WallAdvance == LET t == NextPoint(S.now,wall_anchors) IN
 /\ t # "" /\ StableWall(t) /\ \E e \in Edges :
   /\ e.input = [op |-> "clock_advance",now |-> t] /\ sql' = e.dst
   /\ completed' = completed \cup DoneJobs(e.dst) /\ UNCHANGED <<w,available>>
MonoAdvance == LET u == NextPoint(mono_now_ms,mono_anchors) IN
 /\ u # "" /\ StableMono(u) /\ mono_now_ms' = u /\ UNCHANGED <<sql,w,available,completed,claim_loss_keys>>
ClaimRecoveryWake == (\E n \in Nodes : w[n].claim_recovery.phase = "waiting" /\ mono_now_ms < w[n].claim_recovery.wake_mono_ms)
 /\ MonoAdvance /\ AnchorFrame
EligibilityAdvance == /\ StableSuccess /\ \E j \in Nodes : S.jobs[j+1].state = "queued" /\ ~S.jobs[j+1].done /\ S.input_domain.jobs[j+1].not_before_ms > S.now
 /\ WallAdvance
WorkerActual(n) == WorkerNext(n) \/ AdapterSample(n) \/ OrdinaryLock(n) \/ OrdinaryLocal(n)
 \/ OrdinaryCallReturn(n) \/ OrdinaryObserve(n) \/ QueueLossSignal(n)
Next == BaseStep(Environment \/ Availability \/ WallAdvance \/ (\E n \in Nodes : WorkerActual(n)))
 \/ (MonoAdvance /\ AnchorFrame)
FairRuntime == \A n \in Nodes :
 WF_vars(BaseStep(ClaimSend(n))) /\ (IF Mutation = "no-claim-win" THEN TRUE ELSE WF_vars(BaseStep(ClaimApply(n))))
 /\ WF_vars(BaseStep(ClaimReply(n))) /\ WF_vars(BaseStep(ClaimRecover(n))) /\ WF_vars(BaseStep(ClaimReturn(n)))
 /\ WF_vars(BaseStep(Dispatch(n))) /\ WF_vars(BaseStep(StoreApply(n))) /\ WF_vars(BaseStep(SimpleApply(n)))
 /\ WF_vars(BaseStep(Reply(n))) /\ WF_vars(BaseStep(DriverReturn(n)))
 /\ WF_vars(BaseStep(RetireLocal(n))) /\ WF_vars(BaseStep(RetireCallReturn(n)))
 /\ WF_vars(BaseStep(AdapterSample(n))) /\ WF_vars(BaseStep(OrdinaryLock(n)))
 /\ WF_vars(BaseStep(OrdinaryLocal(n))) /\ WF_vars(BaseStep(OrdinaryCallReturn(n))) /\ WF_vars(BaseStep(OrdinaryObserve(n)))
FairExternal == \A n \in Nodes :
 (IF Mutation = "old-poll-policy" THEN TRUE ELSE WF_vars(BaseStep(ExternalReturn(n))))
 /\ (IF Mutation = "withheld-join" THEN TRUE ELSE WF_vars(BaseStep(JoinReturn(n))))
 /\ WF_vars(BaseStep(RetireDrain(n)))
AdmissionSF == \A n \in Nodes : IF Mutation = "weak-admission" THEN WF_vars(BaseStep(Admission(n))) ELSE SF_vars(BaseStep(Admission(n)))
DurableClaimWin == \A n \in Nodes : IF Mutation = "no-claim-win" THEN TRUE ELSE SF_vars(BaseStep(ClaimWin(n)))
Spec == Init /\ [][Next]_vars /\ FairRuntime /\ FairExternal /\ WF_vars(ClaimRecoveryWake)
 /\ (IF StableSuccess THEN (\A n \in Nodes : WF_vars(BaseStep(MakeAvailable(n)))) /\ AdmissionSF /\ DurableClaimWin
      /\ WF_vars(BaseStep(EligibilityAdvance)) ELSE TRUE)
TypeOK == /\ sql \in 0..(Len(StoreStates)-1) /\ Cardinality(claim_loss_keys) <= 8
 /\ mono_now_ms \in Nat /\ wall_anchors \subseteq Nat /\ mono_anchors \subseteq Nat
 /\ (\A n \in Nodes : w[n].stage \in {"idle","claim","driver"})
 /\ (\A n \in Nodes : w[n].localToken \in BOOLEAN /\ w[n].accepted \in BOOLEAN)
 /\ (\A n \in Nodes : w[n].retire \in {"off","drain","take","sample-first","first",
     "sample-resolve","resolve","sample-after","second","done","warn"})
 /\ (\A n \in Nodes : w[n].stage = "driver" =>
     w[n].ctrl \in 0..(Len(DriverStates)-1) /\ w[n].attempt \in ClaimIds)
(* Counts unexpired durable queue authority, not physical readers that may
   remain live after expiry while cancellation/join is pending. *)
NoTwoSourceReaders == Cardinality({j \in Nodes : S.jobs[j+1].active}) <= 1
ResultAtomicity == completed \subseteq DoneJobs(sql)
LastRequestAtomicRetirement == \A j \in Nodes : S.jobs[j+1].done =>
 S.jobs[j+1].state = "succeeded" /\ S.jobs[j+1].reservations = 0
ReservationConservation == \A j \in Nodes :
 (S.jobs[j+1].owner = -1 => S.jobs[j+1].reservations = 0)
 /\ (S.jobs[j+1].active => S.jobs[j+1].reservations = 3)
AdmissionUntilJoined == \A n \in Nodes :
 w[n].stage = "driver" /\ w[n].workStarted /\ ~w[n].joined =>
 w[n].admission
JoinBeforeRelease == \A n \in Nodes :
 w[n].stage = "driver" /\ w[n].workStarted /\ ~D(n).domain => w[n].joined
NoFictitiousRelease == \A n \in Nodes : w[n].dropped => w[n].joined \/ ~w[n].workStarted
(* Structural consistency check: the summarized read guard is cleared at
   Reply before a later DriverReturn can mark revocation. This is not an
   independent proof of the adapter publication-lock implementation. *)
GuardBeforeRevoke == \A n \in Nodes : w[n].stage = "driver" => ~(D(n).revoked /\ w[n].readLock)
ImportedOracleSafety == \A i \in 1..Len(StoreStates) :
 LET s == StoreStates[i] IN
 /\ Cardinality({j \in Nodes : s.jobs[j+1].active}) <= 1
 /\ \A j \in Nodes :
    /\ (s.jobs[j+1].done => s.jobs[j+1].state = "succeeded" /\
        s.jobs[j+1].owner = -1 /\ s.jobs[j+1].reservations = 0)
    /\ (s.jobs[j+1].owner = -1 => s.jobs[j+1].reservations = 0)
    /\ (s.jobs[j+1].active => s.jobs[j+1].reservations = 3)
DriverRank == \A e \in DriverEdges : DriverStates[e.dst+1].rank < DriverStates[e.src+1].rank
NoStaleCommit == \A e \in StoreEdges : e.fresh
MonotoneFence == \A e \in StoreEdges : e.monotone
Safety == DriverRank /\ ImportedOracleSafety /\ NoStaleCommit /\ MonotoneFence /\ TypeOK /\ NoTwoSourceReaders /\ ResultAtomicity /\ LastRequestAtomicRetirement
 /\ ReservationConservation /\ AdmissionUntilJoined /\ JoinBeforeRelease /\ NoFictitiousRelease /\ GuardBeforeRevoke
(* No finite-D trace can promise to execute an operation whose input budget is
   already excluded. This premise is about an actual available relation edge,
   never about the desired cleanup state. All cutoff states remain in Safety. *)
RawRequestEdge(n,e) ==
 \/ /\ D(n).pending = "RetireQueue" /\ RetireEdge(n,e)
 \/ /\ D(n).pending = "AcquireDomain" /\ e.op = "acquire_domain" /\ e.node = n
 \/ /\ D(n).pending = "Bind" /\ e.op = "bind" /\ CurrentToken(n,e) /\ CurrentLease(n,e)
 \/ /\ D(n).pending \in {"ReleaseRefused","ReleaseRetry","ReleaseNone","ReleaseInterrupted"}
        /\ e.op = "release_domain" /\ CurrentLease(n,e)
 \/ /\ D(n).pending \in {"SettleYield","SettleRetry"} /\ e.input = OrdinaryInput(n)
WithinEpisodeCapacity == \A n \in Nodes :
 IF OrdinaryActive(n) THEN (w[n].ordinary.phase # "sent" \/ (\E e \in Edges : e.input = OrdinaryInput(n))) ELSE
 (w[n].stage = "driver" /\ w[n].call = "sent" /\
 D(n).pending \in {"AcquireDomain","Bind","ReleaseRefused","ReleaseRetry","ReleaseNone","ReleaseInterrupted","RetireQueue"}
 /\ (D(n).pending # "RetireQueue" \/
     (w[n].retire \in {"first","resolve","second"} /\ w[n].inner \in {"ready","sent"})))
 => \E e \in Edges : RawRequestEdge(n,e)
AttemptCleanup == \A n \in Nodes, j \in Nodes, a \in ClaimIds :
 (w[n].stage = "driver" /\ w[n].started /\ w[n].job = j /\ w[n].attempt = a)
 ~> (w[n].stage = "driver" /\ w[n].dropped /\ w[n].job = j /\ w[n].attempt = a)
ConditionalAttemptCleanup == ([]WithinEpisodeCapacity) => AttemptCleanup
BothSelectedCompletion == <> (DoneJobs(sql) = Nodes)
BothDoneAndDropped == DoneJobs(sql) = Nodes /\ \A n \in Nodes : w[n].dropped /\ ~w[n].admission
NonvacuityWitness == ~BothDoneAndDropped

(* Coverage predicates are obligations, never guards on Init/Next/fairness.
   DInputs is independently derived before consulting Edges. Capacity-only
   ready/sent retirement requests are included to prevent vacuous cleanup. *)
I == S.input_domain
NodeNames == <<"node-a","node-b","foreign-holder">>
Tok(j) == I.captured_tokens[j+1]
Ti(j) == 0..(Len(Tok(j))-1)
Li == 0..(Len(I.captured_leases)-1)
Latest(j,i) == ~\E k \in (i+1)..(Len(Tok(j))-1) : Tok(j)[k+1].claim_id = Tok(j)[i+1].claim_id
Claims(j) == IF I.budgets.claims[j+1] < 2
 THEN {[op |-> "claim",job |-> j,node |-> n] : n \in Nodes} ELSE {}
Renewals(j,i) == IF Latest(j,i) /\ Tok(j)[i+1].claim_id \notin SeqSet(I.budgets.renewed_claims)
 THEN {[op |-> "renew",job |-> j,token |-> i]} ELSE {}
Settlements(j,i) == {[op |-> "settle",job |-> j,token |-> i,disposition |-> d] : d \in {2,3}}
Bindings(j,i) == IF Latest(j,i)
 THEN {[op |-> op,job |-> j,token |-> i,lease |-> l] :
       op \in {"bind","complete"},l \in {k \in Li : I.captured_leases[k+1].owner_node_id = Tok(j)[i+1].node_id}}
 ELSE {}
TokenInputs(j,i) == Renewals(j,i) \cup Settlements(j,i) \cup Bindings(j,i)
 \cup {[op |-> "resolve",job |-> j,token |-> i]}
OwnsQueue(n) == n = 2 \/ (\E j \in Nodes : I.jobs[j+1].state = "running" /\ I.jobs[j+1].owner = NodeNames[n+1])
Acquires == {[op |-> "acquire_domain",node |-> n] : n \in {k \in 0..2 : ~I.budgets.domain_acquired[k+1] /\ OwnsQueue(k)}}
Releases == {[op |-> "release_domain",lease |-> l] : l \in Li}
Removals == {[op |-> "remove_owner",node |-> n] : n \in {k \in Nodes : ~I.budgets.removed[k+1]}}
DInputs == UNION {Claims(j) \cup UNION {TokenInputs(j,i) : i \in Ti(j)} : j \in Nodes}
 \cup Acquires \cup Releases \cup Removals

CallInput(n,pending) ==
 CASE pending = "AcquireDomain" -> [op |-> "acquire_domain",node |-> n]
 [] pending = "Bind" -> IF w[n].token = -1 \/ w[n].lease = -1 THEN ""
       ELSE [op |-> "bind",job |-> w[n].job,token |-> w[n].token,lease |-> w[n].lease]
 [] pending \in {"SettleYield","SettleRetry"} -> OrdinaryInput(n)
 [] pending \in {"ReleaseRefused","ReleaseRetry","ReleaseNone","ReleaseInterrupted"} ->
       IF w[n].lease = -1 THEN "" ELSE [op |-> "release_domain",lease |-> w[n].lease]
 [] pending = "PollWork" -> IF w[n].token = -1 \/ w[n].lease = -1 THEN ""
       ELSE [op |-> "complete",job |-> w[n].job,token |-> w[n].token,lease |-> w[n].lease]
 [] pending = "RetireQueue" ->
       LET t == IF w[n].retire \in {"first","resolve"} THEN w[n].original ELSE w[n].resolved IN
       IF t = -1 THEN "" ELSE IF w[n].retire = "resolve"
       THEN [op |-> "resolve",job |-> w[n].job,token |-> t]
       ELSE [op |-> "settle",job |-> w[n].job,token |-> t,
             disposition |-> IF w[n].cancelling \/ (w[n].retire = "second" /\ w[n].outcome = "cancel_requested") THEN 2 ELSE 3]
 [] OTHER -> ""
SqlPending == {"AcquireDomain","Bind",
 "ReleaseRefused","ReleaseRetry","ReleaseNone","ReleaseInterrupted"}
RetirePending(n) == D(n).pending = "RetireQueue" /\ w[n].retire \in {"first","resolve","second"}
CapacityGate(n) == w[n].stage = "driver" /\ w[n].call = "sent"
 /\ (D(n).pending \in SqlPending \/ (RetirePending(n) /\ w[n].inner \in {"ready","sent"}))
SampleOK(n) == D(n).pending # "RetireQueue" \/ w[n].sample = I.now
ContextInputs == DInputs
 \cup {InitialResolveInput(n) : n \in {k \in Nodes : RecoveryReady(k)}}
 \cup {OrdinaryInput(n) : n \in {k \in Nodes : OrdinaryActive(k) /\ w[k].ordinary.phase = "sent"}}
 \cup (LET t == NextPoint(S.now,wall_anchors) IN IF t # "" /\ StableWall(t) THEN {[op |-> "clock_advance",now |-> t]} ELSE {})
WorkerDemands(n) ==
 (IF RecoveryReady(n) THEN {[source |-> sql,input |-> InitialResolveInput(n),sample_ok |-> TRUE]} ELSE {})
 \cup  (IF w[n].stage = "claim" /\ w[n].call = "sent"
  THEN {[source |-> sql,input |-> [op |-> "claim",job |-> w[n].job,node |-> n],sample_ok |-> TRUE]} ELSE {})
 \cup (IF w[n].stage = "driver" /\ w[n].call = "sent"
  THEN (IF D(n).pending \in SqlPending \cup {"PollWork"} \/
           (RetirePending(n) /\ w[n].inner = "sent") \/ (OrdinaryActive(n) /\ w[n].ordinary.phase = "sent")
        THEN {[source |-> sql,input |-> CallInput(n,D(n).pending),sample_ok |-> SampleOK(n)]} ELSE {}) ELSE {})
 \cup (IF CapacityGate(n) THEN {[source |-> sql,input |-> CallInput(n,D(n).pending),sample_ok |-> SampleOK(n)]} ELSE {})
EnvInputs == IF StableSuccess THEN {} ELSE {a \in DInputs :
 /\ a.op \in {"remove_owner","acquire_domain","release_domain","renew"}
 /\ IF a.op = "acquire_domain" THEN a.node = 2 ELSE TRUE
 /\ IF a.op = "release_domain" THEN I.captured_leases[a.lease+1].owner_node_id = "foreign-holder" ELSE TRUE
 /\ IF a.op = "renew" THEN TokenLockAvailable(a) ELSE TRUE}
PrimaryDemands == UNION {WorkerDemands(n) : n \in Nodes}
 \cup {[source |-> sql,input |-> a,sample_ok |-> TRUE] : a \in EnvInputs}
 \cup (LET t == NextPoint(S.now,wall_anchors) IN IF t # "" THEN
        {[source |-> sql,input |-> [op |-> "clock_advance",now |-> t],sample_ok |-> StableWall(t)]} ELSE {})
DependentRenewals == {e \in Edges : e.op = "renew" /\ e.input \in EnvInputs /\ Matched(e) # {}
 /\ (e.result # "renewed" \/ \E n \in Matched(e) : ~w[n].faultUsed)}
RenewDependentDemands == {[source |-> e.dst,input |-> [op |-> "resolve",job |-> e.job,token |-> e.token],sample_ok |-> TRUE] : e \in DependentRenewals}
ClaimDependentDemands == UNION {UNION {
 (IF e.result = "running" THEN {[source |-> e.dst,input |-> [op |-> "read_claim_job",job |-> e.job],sample_ok |-> TRUE]} ELSE {})
 \cup (IF e.result = "cancel_requested" THEN {[source |-> e.dst,input |-> [op |-> "settle",job |-> e.job,token |-> e.resolved,disposition |-> 2],sample_ok |-> TRUE]} ELSE {})
 : e \in InitialResolutions(n)} : n \in {k \in Nodes : RecoveryReady(k)}}
DependentDemands == RenewDependentDemands \cup ClaimDependentDemands
DemandCovered ==
 /\ (\A d \in PrimaryDemands : (d.input \in ContextInputs /\ d.sample_ok) => Cardinality({e \in StoreEdges : e.src = d.source /\ e.input = d.input}) = 1)
 /\ (\A d \in DependentDemands : Cardinality({e \in StoreEdges : e.src = d.source /\ e.input = d.input}) = 1)
DWithinEpisodeCapacity == \A n \in Nodes : IF OrdinaryActive(n) THEN TRUE ELSE
 CapacityGate(n) => CallInput(n,D(n).pending) \in DInputs /\ SampleOK(n)
CapacityAgrees == WithinEpisodeCapacity = DWithinEpisodeCapacity
ContextId == CASE Mutation = "weak-admission" -> "weak_admission"
 [] Mutation = "no-claim-win" -> "no_claim_win"
 [] Mutation = "withheld-join" -> "withheld_join"
 [] Mutation = "old-poll-policy" -> "old_poll_policy"
 [] Mutation = "drop-before-join" -> "drop_before_join"
 [] StableSuccess -> "stable" [] OTHER -> "base"
CertW(p) == [n \in Nodes |-> p.workers[n+1]]
CertAvailable(p) == [n \in Nodes |-> p.available[n+1]]
CertMatch(p) == p.context = ContextId /\ sql = p.sql /\ w = CertW(p)
 /\ available = CertAvailable(p) /\ completed = SeqSet(p.completed)
 /\ mono_now_ms = p.mono_now_ms /\ wall_anchors = SeqSet(p.wall_anchors) /\ mono_anchors = SeqSet(p.mono_anchors) /\ claim_loss_keys = SeqSet(p.claim_loss_keys)
CompletedSequence == (IF 0 \in completed THEN <<0>> ELSE <<>>) \o (IF 1 \in completed THEN <<1>> ELSE <<>>)
RECURSIVE Sorted(_)
Sorted(Ss) == IF Ss = {} THEN <<>> ELSE <<Min(Ss)>> \o Sorted(Ss \ {Min(Ss)})
LossUniverse == <<[job |-> 0,node |-> 0,claim_id |-> "00000000-0000-4000-8000-000000000010"],
 [job |-> 0,node |-> 0,claim_id |-> "00000000-0000-4000-8000-000000000011"],
 [job |-> 0,node |-> 1,claim_id |-> "00000000-0000-4000-8000-000000000010"],
 [job |-> 0,node |-> 1,claim_id |-> "00000000-0000-4000-8000-000000000011"],
 [job |-> 1,node |-> 0,claim_id |-> "00000000-0000-4000-8000-000000000014"],
 [job |-> 1,node |-> 0,claim_id |-> "00000000-0000-4000-8000-000000000015"],
 [job |-> 1,node |-> 1,claim_id |-> "00000000-0000-4000-8000-000000000014"],
 [job |-> 1,node |-> 1,claim_id |-> "00000000-0000-4000-8000-000000000015"]>>
RECURSIVE LossSequence(_)
LossSequence(i) == IF i > Len(LossUniverse) THEN <<>> ELSE
 (IF LossUniverse[i] \in claim_loss_keys THEN <<LossUniverse[i]>> ELSE <<>>) \o LossSequence(i+1)
CurrentKey == <<ContextId,sql,<<w[0],w[1]>>,<<available[0],available[1]>>,CompletedSequence,mono_now_ms,Sorted(wall_anchors),Sorted(mono_anchors),LossSequence(1)>>
CurrentCert == IF CurrentKey \in DOMAIN ProductIndex
 THEN {ProductStates[ProductIndex[CurrentKey]+1]} ELSE {}
CertifiedReachable == Cardinality(CurrentCert) = 1
TargetEquals(q) == sql' = q.sql /\ w' = CertW(q)
 /\ available' = CertAvailable(q) /\ completed' = SeqSet(q.completed)
 /\ mono_now_ms' = q.mono_now_ms /\ wall_anchors' = SeqSet(q.wall_anchors) /\ mono_anchors' = SeqSet(q.mono_anchors) /\ claim_loss_keys' = SeqSet(q.claim_loss_keys)
CertTargets(p) == {ProductStates[e.dst+1] : e \in SeqSet(ProductOutgoing[p.id+1])}
PairLabels(p,q) == UNION {SeqSet(e.fair) : e \in {x \in SeqSet(ProductOutgoing[p.id+1]) : x.dst = q.id}}
EnabledLabels(p) == SeqSet(ProductEnabled[p.id+1].actions)
FairFamilies == {"ClaimSend","ClaimApply","ClaimReply","ClaimReturn","Dispatch","StoreApply",
 "SimpleApply","Reply","DriverReturn","RetireLocal","RetireCallReturn","ExternalReturn",
 "JoinReturn","RetireDrain","MakeAvailable","Admission","ClaimWin","AdapterSample","OrdinaryLock","OrdinaryLocal","OrdinaryCallReturn","OrdinaryObserve","ClaimRecover"}
RawFamilyAction(f,n) == CASE f = "ClaimSend" -> ClaimSend(n)
 [] f = "ClaimApply" -> ClaimApply(n) [] f = "ClaimReply" -> ClaimReply(n)
 [] f = "ClaimReturn" -> ClaimReturn(n) [] f = "Dispatch" -> Dispatch(n)
 [] f = "StoreApply" -> StoreApply(n) [] f = "SimpleApply" -> SimpleApply(n)
 [] f = "Reply" -> Reply(n) [] f = "DriverReturn" -> DriverReturn(n)
 [] f = "RetireLocal" -> RetireLocal(n) [] f = "RetireCallReturn" -> RetireCallReturn(n)
 [] f = "ExternalReturn" -> ExternalReturn(n) [] f = "JoinReturn" -> JoinReturn(n)
 [] f = "RetireDrain" -> RetireDrain(n) [] f = "MakeAvailable" -> MakeAvailable(n)
 [] f = "Admission" -> Admission(n) [] f = "ClaimWin" -> ClaimWin(n)
 [] f = "AdapterSample" -> AdapterSample(n) [] f = "OrdinaryLock" -> OrdinaryLock(n)
 [] f = "OrdinaryLocal" -> OrdinaryLocal(n) [] f = "OrdinaryCallReturn" -> OrdinaryCallReturn(n)
 [] f = "OrdinaryObserve" -> OrdinaryObserve(n) [] f = "ClaimRecover" -> ClaimRecover(n)
FamilyAction(f,n) == BaseStep(RawFamilyAction(f,n))
FairLabel(f,n) == f \o "/" \o ToString(n)
(* ENABLED with ALL next variables fixed evaluates the actual action on that
   state pair. These invariants do not constrain or replace executable Next. *)
CertificateNextAgreement == \A p \in CurrentCert : \A q \in CertTargets(p) :
 ENABLED (Next /\ TargetEquals(q))
FairPairAgreement == \A p \in CurrentCert : \A q \in CertTargets(p) :
 /\ (\A f \in FairFamilies,n \in Nodes : (ENABLED (FamilyAction(f,n) /\ TargetEquals(q))) = (FairLabel(f,n) \in PairLabels(p,q)))
 /\ (ENABLED (BaseStep(EligibilityAdvance) /\ TargetEquals(q))) = ("EligibilityAdvance/global" \in PairLabels(p,q))
 /\ (ENABLED (ClaimRecoveryWake /\ TargetEquals(q))) = ("ClaimRecoveryWake/global" \in PairLabels(p,q))
FairEnablednessAgreement == \A p \in CurrentCert :
 /\ (\A f \in FairFamilies,n \in Nodes : (ENABLED <<FamilyAction(f,n)>>_vars) = (FairLabel(f,n) \in EnabledLabels(p)))
 /\ (ENABLED <<BaseStep(EligibilityAdvance)>>_vars) = ("EligibilityAdvance/global" \in EnabledLabels(p))
 /\ (ENABLED <<ClaimRecoveryWake>>_vars) = ("ClaimRecoveryWake/global" \in EnabledLabels(p))
=============================================================================
