---------------------- MODULE AsyncDriverRefinement ----------------------
EXTENDS Naturals, Integers, Sequences, FiniteSets, TLC, SourceTables
CONSTANTS StableSuccess, Mutation, Witness
VARIABLES sql, w, available, completed
vars == <<sql, w, available, completed>>
Nodes == 0..1
Idle == [stage |-> "idle", job |-> -1, attempt |-> "", token |-> -1, lease |-> -1,
         ctrl |-> -1, call |-> "ready", reply |-> "", accepted |-> FALSE,
         localToken |-> FALSE, retire |-> "off", inner |-> "ready",
         original |-> -1, resolved |-> -1, outcome |-> "", sample |-> -1,
         firstSample |-> -1, resolveSample |-> -1, afterSample |-> -1,
         started |-> FALSE, dropped |-> FALSE, joined |-> FALSE,
         workStarted |-> FALSE, faultUsed |-> FALSE, admission |-> FALSE, readLock |-> FALSE]
D(n) == DriverStates[w[n].ctrl + 1]
S == StoreStates[sql + 1]
Edges == {e \in StoreEdges : e.src = sql}
CurrentToken(n, e) == e.job = w[n].job /\ e.token = w[n].token
CurrentLease(n, e) == e.lease = w[n].lease
ReplyEdge(n, tag) == {e \in DriverEdges : e.src = w[n].ctrl /\ e.reply = tag}
DoneJobs(s) == {j \in Nodes : StoreStates[s+1].jobs[j+1].done}
Init == /\ sql = StoreInitial
        /\ w = [n \in Nodes |-> Idle]
        /\ available = [n \in Nodes |-> TRUE]
        /\ completed = DoneJobs(sql)

Admission(n) ==
 /\ w[n].stage = "idle" /\ available[n]
 /\ \E j \in Nodes :
     /\ S.jobs[j+1].state = "queued" /\ ~S.jobs[j+1].done
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
        ![n].attempt = IF success THEN StoreStates[e.dst+1].tokens[w[n].job+1][Len(StoreStates[e.dst+1].tokens[w[n].job+1])] ELSE ""]
 /\ UNCHANGED <<available, completed>>
ClaimWin(n) == ClaimApplyResult(n, TRUE)
ClaimApply(n) == ClaimWin(n) \/ ClaimApplyResult(n, FALSE)
ClaimReply(n) ==
 /\ w[n].stage = "claim" /\ w[n].call = "applied"
 /\ \E lost \in IF StableSuccess THEN {FALSE} ELSE BOOLEAN :
     w' = [w EXCEPT ![n].call = "replied",
       ![n].reply = IF lost THEN "Unknown" ELSE @,
       ![n].faultUsed = @ \/ lost]
 /\ UNCHANGED <<sql, available, completed>>
ClaimReturn(n) ==
 /\ w[n].stage = "claim" /\ w[n].call = "replied"
 /\ w' = [w EXCEPT ![n] = IF w[n].reply = "Claimed"
       THEN [@ EXCEPT !.stage = "driver", !.ctrl = DriverInitial,
             !.call = "ready", !.started = TRUE, !.localToken = TRUE]
       ELSE Idle]
 /\ UNCHANGED <<sql, available, completed>>

Dispatch(n) ==
 /\ w[n].stage = "driver" /\ ~D(n).terminal /\ w[n].call = "ready"
 /\ w' = [w EXCEPT ![n].call = "sent",
       ![n].readLock = D(n).pending = "PollWork", ![n].accepted = FALSE]
 /\ UNCHANGED <<sql, available, completed>>
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
 /\ w' = [w EXCEPT ![n].call = "applied", ![n].reply = tag, ![n].accepted = FALSE]
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
   \/ /\ D(n).pending \in {"SettleYield", "SettleRetry"} /\ e.op = "settle"
      /\ CurrentToken(n,e) /\ e.disposition = IF D(n).pending = "SettleYield" THEN 0 ELSE 1
      /\ CommitReply(n,e,"Done")
   \/ /\ D(n).pending \in {"ReleaseRefused","ReleaseRetry","ReleaseNone","ReleaseInterrupted"}
      /\ e.op = "release_domain" /\ CurrentLease(n,e)
      /\ CommitReply(n,e,"ReleaseReturned")
AmbiguousWithoutApply(n) ==
 /\ ~StableSuccess
 /\ w[n].stage = "driver" /\ w[n].call = "sent" /\ ~w[n].faultUsed
 /\ D(n).pending \in {"AcquireDomain","Bind","SettleYield","SettleRetry",
     "ReleaseRefused","ReleaseRetry","ReleaseNone","ReleaseInterrupted"}
 /\ LET tag == CASE D(n).pending = "AcquireDomain" -> "DomainRefused"
                 [] D(n).pending = "Bind" -> "BindRefused"
                 [] D(n).pending \in {"SettleYield","SettleRetry"} -> "Done"
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
 /\ w' = [w EXCEPT ![n].retire = "drain"]
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
       /\ e.disposition = IF w[n].retire = "second" /\ w[n].outcome = "cancel_requested" THEN 2 ELSE 3
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
    /\ e.result = "accepted" /\ CommitReply(n,e,"WorkSucceeded")
WorkOther(n) ==
 /\ ~StableSuccess /\ w[n].stage = "driver" /\ w[n].call = "sent"
 /\ D(n).pending = "PollWork"
 /\ \E tag \in {"QueueLoss","DomainLoss","Pressure","WorkFailed"} : LocalReply(n,tag)
ExternalReturn(n) == WorkSuccess(n) \/ WorkOther(n)
JoinReturn(n) ==
 /\ w[n].stage = "driver" /\ w[n].call = "sent" /\ D(n).pending = "JoinWork"
 /\ Mutation # "withheld-join"
 /\ \E tag \in IF D(n).work = "completed_succeeded" THEN {"JoinedSucceeded"}
               ELSE IF D(n).work = "completed_failed" THEN {"JoinedFailed"}
               ELSE {"JoinedFailed","JoinedSucceeded"} : LocalReply(n,tag)
Reply(n) ==
 /\ w[n].stage = "driver" /\ w[n].call = "applied"
 /\ \E lost \in IF ~w[n].faultUsed /\ D(n).pending \in
     {"AcquireDomain","Bind","SettleYield","SettleRetry","PollWork",
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
        ![n].localToken = IF ~lost /\ w[n].accepted /\
          D(n).pending \in {"SettleYield","SettleRetry"} THEN FALSE ELSE @]
 /\ UNCHANGED <<sql, available, completed>>
DriverReturn(n) ==
 /\ w[n].stage = "driver" /\ w[n].call = "replied"
 /\ \E e \in ReplyEdge(n,w[n].reply) :
    /\ w' = [w EXCEPT ![n].ctrl = e.dst, ![n].call = "ready",
        ![n].joined = @ \/ D(n).pending = "JoinWork",
        ![n].workStarted = @ \/ (D(n).pending = "Bind" /\ w[n].reply = "BindAccepted"),
        ![n].dropped = @ \/ D(n).pending = "DropAdmission",
        ![n].admission = IF D(n).pending = "DropAdmission" \/
          (Mutation = "drop-before-join" /\ D(n).pending = "CancelWork") THEN FALSE ELSE @]
 /\ UNCHANGED <<sql, available, completed>>
Recycle(n) ==
 /\ w[n].stage = "driver" /\ D(n).terminal
 /\ ~StableSuccess
 /\ w' = [w EXCEPT ![n] = Idle]
 /\ UNCHANGED <<sql, available, completed>>

SameClaim(n,e) == w[n].stage \in {"claim","driver"} /\ w[n].job = e.job
 /\ w[n].token >= 0 /\ S.tokens[e.job+1][w[n].token+1] = S.tokens[e.job+1][e.token+1]
TokenLockAvailable(e) == \A n \in Nodes : SameClaim(n,e) =>
 w[n].localToken /\ (w[n].call = "ready" \/ w[n].retire = "drain")
Environment ==
 /\ ~StableSuccess
 /\ \E e \in Edges :
    /\ e.op \in {"clock_advance","remove_owner","acquire_domain","release_domain","renew"}
    /\ (e.op # "acquire_domain" \/ e.node = 2)
    /\ (e.op # "release_domain" \/ StoreStates[sql+1].leases[e.lease+1].owner = 2)
    /\ (e.op # "renew" \/ TokenLockAvailable(e))
    /\ sql' = e.dst
    /\ completed' = completed \cup DoneJobs(e.dst)
    /\ \E ack \in IF e.op = "renew" /\ e.result = "renewed" /\
          (\E n \in Nodes : SameClaim(n,e) /\ ~w[n].faultUsed) THEN BOOLEAN ELSE {TRUE} :
       w' = IF e.op = "renew" /\ e.result = "renewed"
         THEN [n \in Nodes |-> IF SameClaim(n,e) THEN [w[n] EXCEPT
            !.token = IF ack THEN Len(StoreStates[e.dst+1].tokens[e.job+1])-1 ELSE @,
            !.faultUsed = @ \/ ~ack] ELSE w[n]] ELSE w
    /\ UNCHANGED available
MakeAvailable(n) ==
 /\ ~available[n] /\ available' = [available EXCEPT ![n] = TRUE]
 /\ UNCHANGED <<sql,w,completed>>
RemoveAvailability(n) ==
 /\ available[n] /\ available' = [available EXCEPT ![n] = FALSE]
 /\ UNCHANGED <<sql,w,completed>>
Availability == \E n \in Nodes : MakeAvailable(n) \/ RemoveAvailability(n)
PhysicalOpportunity == \A n \in Nodes : WF_vars(MakeAvailable(n))
WorkerNext(n) == Admission(n) \/ ClaimSend(n) \/ ClaimApply(n) \/ ClaimReply(n) \/ ClaimReturn(n)
 \/ Dispatch(n) \/ StoreApply(n) \/ AmbiguousWithoutApply(n) \/ SimpleApply(n) \/ ExternalReturn(n)
 \/ JoinReturn(n) \/ RetireLocal(n) \/ RetireDrain(n) \/ RetireCallReturn(n) \/ Reply(n) \/ DriverReturn(n) \/ Recycle(n)
Next == Environment \/ Availability \/ (\E n \in Nodes : WorkerNext(n))
FairRuntime == \A n \in Nodes :
 WF_vars(ClaimSend(n)) /\ (IF Mutation = "no-claim-win" THEN TRUE ELSE WF_vars(ClaimApply(n))) /\ WF_vars(ClaimReply(n)) /\ WF_vars(ClaimReturn(n))
 /\ WF_vars(Dispatch(n)) /\ WF_vars(StoreApply(n)) /\ WF_vars(SimpleApply(n))
 /\ WF_vars(Reply(n)) /\ WF_vars(DriverReturn(n))
 /\ WF_vars(RetireLocal(n)) /\ WF_vars(RetireCallReturn(n))
FairExternal == \A n \in Nodes :
 (IF Mutation = "old-poll-policy" THEN TRUE ELSE WF_vars(ExternalReturn(n)))
 /\ (IF Mutation = "withheld-join" THEN TRUE ELSE WF_vars(JoinReturn(n)))
 /\ WF_vars(RetireDrain(n))
AdmissionSF == \A n \in Nodes :
 IF Mutation = "weak-admission" THEN WF_vars(Admission(n)) ELSE SF_vars(Admission(n))
DurableClaimWin == \A n \in Nodes :
 IF Mutation = "no-claim-win" THEN TRUE ELSE SF_vars(ClaimWin(n))
Spec == Init /\ [][Next]_vars /\ FairRuntime /\ FairExternal
 /\ (IF StableSuccess THEN PhysicalOpportunity /\ AdmissionSF /\ DurableClaimWin ELSE TRUE)
TypeOK == /\ sql \in 0..(Len(StoreStates)-1)
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
AllStoreSafety == \A i \in 1..Len(StoreStates) :
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
Safety == DriverRank /\ AllStoreSafety /\ NoStaleCommit /\ MonotoneFence /\ TypeOK /\ NoTwoSourceReaders /\ ResultAtomicity /\ LastRequestAtomicRetirement
 /\ ReservationConservation /\ AdmissionUntilJoined /\ JoinBeforeRelease /\ NoFictitiousRelease /\ GuardBeforeRevoke
(* No finite-D trace can promise to execute an operation whose input budget is
   already excluded. This premise is about an actual available relation edge,
   never about the desired cleanup state. All cutoff states remain in Safety. *)
RawRequestEdge(n,e) ==
 \/ /\ D(n).pending = "RetireQueue" /\ RetireEdge(n,e)
 \/ /\ D(n).pending = "AcquireDomain" /\ e.op = "acquire_domain" /\ e.node = n
 \/ /\ D(n).pending = "Bind" /\ e.op = "bind" /\ CurrentToken(n,e) /\ CurrentLease(n,e)
 \/ /\ D(n).pending \in {"SettleYield","SettleRetry"} /\ e.op = "settle" /\ CurrentToken(n,e)
        /\ e.disposition = IF D(n).pending = "SettleYield" THEN 0 ELSE 1
 \/ /\ D(n).pending \in {"ReleaseRefused","ReleaseRetry","ReleaseNone","ReleaseInterrupted"}
        /\ e.op = "release_domain" /\ CurrentLease(n,e)
WithinEpisodeCapacity == \A n \in Nodes :
 w[n].stage = "driver" /\ w[n].call = "sent" /\
 D(n).pending \in {"AcquireDomain","Bind","SettleYield","SettleRetry",
 "ReleaseRefused","ReleaseRetry","ReleaseNone","ReleaseInterrupted","RetireQueue"}
 /\ (D(n).pending # "RetireQueue" \/
     (w[n].retire \in {"first","resolve","second"} /\ w[n].inner \in {"ready","sent"}))
 => \E e \in Edges : RawRequestEdge(n,e)
AttemptCleanup == \A n \in Nodes, j \in Nodes, a \in ClaimIds :
 (w[n].stage = "driver" /\ w[n].started /\ w[n].job = j /\ w[n].attempt = a)
 ~> (w[n].stage = "driver" /\ w[n].dropped /\ w[n].job = j /\ w[n].attempt = a)
ConditionalAttemptCleanup == ([]WithinEpisodeCapacity) => AttemptCleanup
BothSelectedCompletion == <> (DoneJobs(sql) = Nodes)
BothDoneAndDropped == DoneJobs(sql) = Nodes /\ \A n \in Nodes : w[n].dropped /\ ~w[n].admission
NonvacuityWitness == ~BothDoneAndDropped
=============================================================================
