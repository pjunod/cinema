---------------------------- MODULE DurableJobs ----------------------------
EXTENDS Integers, FiniteSets, Sequences, TLC

(***************************************************************************
 Seeded bounded protocol exploration, not a refinement proof.
 All safety seeds use the same Next relation. CONTRACT.md maps source guards.
 README.md lists bounded lifecycle composition and omitted interleavings.
 Samples are absolute integer classes: before/equal/after expiry 1 and window
 2. No timestamp rebasing, clamping, forced request expiry or clock fairness
 is used by safety. Each sample may independently move forward or backward.
 ***************************************************************************)
CONSTANTS Jobs, Workers, ResourceKeys, MaxBoots, MaxClaims, Backend,
          Mutation, Scenario, Fragment, MaxEvents
ASSUME Backend \in {"sqlite", "hiqlite"}
ASSUME Mutation \in {"none", "binding_ignored", "replay_fresh_fence",
                     "renewal_revives_cancel", "retry_stalls"}
ASSUME Scenario \in {"fresh", "recovery"}
ASSUME Fragment \in {"all", "claim", "publication", "binding", "renewal", "retention"}
ASSUME MaxBoots >= 2 /\ MaxClaims >= 2

Samples == 0..3
LeaseMs == 1
DomainTtlMs == 2
WindowMs == 2
PubTtlMs == 3
YieldMs == 2
RetryMs == 3
No == "none"
NoOwner == [node |-> No, boot |-> 0, claim |-> 0]
NoToken == [job |-> No, node |-> No, boot |-> 0, claim |-> 0,
            fence |-> 0, revision |-> 0, exp |-> 0]
NoLease == [owner |-> No, fence |-> 0, revision |-> 0, exp |-> 0]
NoBinding == [exists |-> FALSE, job |-> No, node |-> No, boot |-> 0,
              claim |-> 0, jobFence |-> 0, domainFence |-> 0]
NoRequest == [kind |-> No]
Max(a,b) == IF a >= b THEN a ELSE b
Min(a,b) == IF a <= b THEN a ELSE b

VARIABLES fragment, jobs, attempts, held, domain, binding, removed,
          workerBoot, tokens, localLease, phase, requests, publications,
          items, nextClaim, ledger, environment, renewed, domainChanged,
          replayed, resolved, pruned, compacted, retired, quorum,
          now, polled, oldReply, recoveryDelay, claimRecords, callStarted, events
vars == <<fragment, jobs, attempts, held, domain, binding, removed,
          workerBoot, tokens, localLease, phase, requests, publications,
          items, nextClaim, ledger, environment, renewed, domainChanged,
          replayed, resolved, pruned, compacted, retired, quorum,
          now, polled, oldReply, recoveryDelay, claimRecords, callStarted, events>>

JobType == [exists : BOOLEAN,
            state : {"queued","running","cancelling","cancelled","failed"},
            owner : {NoOwner} \cup [node : Workers, boot : 1..MaxBoots, claim : 1..MaxClaims],
            fence : 1..5, revision : 1..8, exp : 0..8, notBefore : 0..5,
            demand : BOOLEAN, finished : BOOLEAN, receipt : BOOLEAN,
            retain : BOOLEAN, userScope : BOOLEAN, legacy : BOOLEAN,
            ageEligible : BOOLEAN, pressureEligible : BOOLEAN]
FreshJob == [exists |-> TRUE, state |-> "queued", owner |-> NoOwner,
             fence |-> 1, revision |-> 1, exp |-> 0, notBefore |-> 0,
             demand |-> TRUE, finished |-> FALSE, receipt |-> TRUE,
             retain |-> FALSE, userScope |-> FALSE, legacy |-> FALSE,
             ageEligible |-> FALSE, pressureEligible |-> FALSE]
Token(j) == [job |-> j, node |-> jobs[j].owner.node, boot |-> jobs[j].owner.boot,
             claim |-> jobs[j].owner.claim, fence |-> jobs[j].fence,
             revision |-> jobs[j].revision, exp |-> jobs[j].exp]
StableTuple(j) == [job |-> j, node |-> jobs[j].owner.node, boot |-> jobs[j].owner.boot,
                   claim |-> jobs[j].owner.claim, fence |-> jobs[j].fence]
TokenTuple(t) == [job |-> t.job, node |-> t.node, boot |-> t.boot,
                 claim |-> t.claim, fence |-> t.fence]
BindingFor(j,d) == [exists |-> TRUE, job |-> j, node |-> jobs[j].owner.node,
                   boot |-> jobs[j].owner.boot, claim |-> jobs[j].owner.claim,
                   jobFence |-> jobs[j].fence, domainFence |-> d.fence]
RowsCover(js, hs, j) == \A k \in ResourceKeys : \E r \in hs :
  r.job = j /\ r.fence = js[j].fence /\ r.key = k /\ r.exp >= js[j].exp
Cover(j) == RowsCover(jobs,held,j)
Exact(t) == IF t=NoToken THEN FALSE ELSE jobs[t.job].exists /\ t = Token(t.job)
SameExceptRevision(t) == t/=NoToken /\ jobs[t.job].exists /\
  TokenTuple(t) = StableTuple(t.job) /\ t.exp = jobs[t.job].exp
LiveBound(js, hs, b, rm, sample) ==
  /\ b.exists
  /\ js[b.job].exists /\ js[b.job].state = "running"
  /\ js[b.job].owner = [node |-> b.node, boot |-> b.boot, claim |-> b.claim]
  /\ js[b.job].fence = b.jobFence /\ js[b.job].exp > sample
  /\ RowsCover(js,hs,b.job) /\ b.node \notin rm
Consistent(js,hs,b,rm,d,sample) ==
  ~b.exists \/ b.domainFence /= d.fence \/ LiveBound(js,hs,b,rm,sample)
Replacement(pred,repl,sample) ==
  /\ repl.owner = pred.owner /\ repl.fence = pred.fence
  /\ repl.revision = pred.revision + 1 /\ repl.exp > pred.exp
  /\ (Backend = "sqlite" => repl.exp > sample)
Authority(js,hs,b,rm,d,pred,repl,sample) ==
  /\ d = pred /\ d.exp > sample /\ Replacement(pred,repl,sample)
  /\ Consistent(js,hs,b,rm,d,sample)
BoundAuthority(js,hs,b,rm,d,pred,repl,sample) ==
  /\ Authority(js,hs,b,rm,d,pred,repl,sample)
  /\ b.exists /\ b.domainFence = d.fence

Target == CHOOSE j \in Jobs : TRUE
Survivor == CHOOSE w \in Workers : TRUE

InitCommon ==
  /\ removed = {} /\ items = {}
  /\ environment = FALSE /\ renewed = FALSE /\ domainChanged = FALSE
  /\ replayed = [w \in Workers |-> FALSE]
  /\ resolved = [w \in Workers |-> FALSE]
  /\ pruned = FALSE /\ compacted = FALSE /\ retired = FALSE /\ quorum = TRUE
  /\ now = 0 /\ polled = FALSE /\ oldReply = FALSE
  /\ callStarted = FALSE
  /\ events = 0

  /\ requests = [w \in Workers |-> NoRequest]
  /\ claimRecords = [w \in Workers |-> NoRequest]
  /\ publications = [w \in Workers |-> [precheck |-> -1, successor |-> -1,
       backend |-> -1, samples |-> <<>>, pred |-> NoLease, repl |-> NoLease,
       left |-> 4, outcome |-> No, committed |-> FALSE,refusalSample |-> -1,boot |-> 0]]

InitFresh ==
  /\ InitCommon
  /\ recoveryDelay = 0
  /\ fragment \in IF Fragment = "all"
       THEN {"claim","publication","binding","retention"} ELSE {Fragment}
  /\ \E j \in Jobs, w \in Workers, b \in 1..MaxBoots,
         c \in (IF fragment="binding" THEN {1,2} ELSE IF fragment="retention" THEN {2} ELSE {1}) :
     /\ nextClaim = IF fragment="claim" THEN 1 ELSE c+1
     /\ jobs = [x \in Jobs |-> IF x = j /\ fragment /= "claim"
          THEN [FreshJob EXCEPT !.state = "running",
                !.owner = [node |-> w, boot |-> b, claim |-> c],
                !.fence = c+1, !.revision = c+1, !.exp = 1]
          ELSE FreshJob]
     /\ workerBoot = [v \in Workers |-> IF v = w THEN b ELSE 1]
     /\ tokens = [v \in Workers |-> IF v = w /\ fragment /= "claim" THEN Token(j) ELSE NoToken]
     /\ domain = [owner |-> w, fence |-> 1, revision |-> 1, exp |-> 2]
     /\ localLease = [v \in Workers |-> IF v = w /\ fragment /= "claim" THEN domain ELSE NoLease]
     /\ held = IF fragment = "claim" THEN {} ELSE
          {[job |-> j, fence |-> c+1, key |-> k, exp |-> 1] : k \in ResourceKeys}
     /\ binding = IF fragment="claim" \/ (fragment="binding" /\ c=1) THEN NoBinding
          ELSE IF fragment="binding" THEN
            [exists |-> TRUE,job |-> j,node |-> w,boot |-> 1,claim |-> 1,jobFence |-> 2,domainFence |-> 1]
          ELSE BindingFor(j,domain)
     /\ phase = [v \in Workers |-> IF v=w /\ fragment/="claim"
          THEN IF fragment="binding" THEN "unbound" ELSE "ready" ELSE "idle"]
     /\ attempts = IF fragment="claim" THEN {} ELSE
          {[job |-> j,claim |-> c,node |-> w,boot |-> b,fence |-> c+1,
            resolveUntil |-> 2,finished |-> FALSE]} \cup
          (IF c=2 THEN {[job |-> j,claim |-> 1,node |-> w,boot |-> 1,fence |-> 2,
                           resolveUntil |-> 2,finished |-> TRUE]} ELSE {})
     /\ ledger = [id \in 1..MaxClaims |-> IF id=c /\ fragment/="claim"
          THEN [job |-> j,node |-> w,boot |-> b,claim |-> c,fence |-> c+1]
          ELSE IF id=1 /\ c=2 THEN [job |-> j,node |-> w,boot |-> 1,claim |-> 1,fence |-> 2]
          ELSE NoToken]

InitRecovery ==
  /\ InitCommon
  /\ nextClaim = 2
  /\ fragment = "recovery"
  /\ recoveryDelay \in {YieldMs,RetryMs}
  /\ LET j == Target w == Survivor IN
     /\ jobs = [x \in Jobs |-> IF x = j THEN
          [FreshJob EXCEPT !.state = "running", !.owner = [node |-> w,boot |-> 1,claim |-> 1],
           !.fence = 2, !.revision = 2, !.exp = 2, !.notBefore = recoveryDelay]
          ELSE FreshJob]
     /\ workerBoot = [v \in Workers |-> 2]
     /\ tokens = [v \in Workers |-> NoToken]
     /\ domain = [owner |-> w,fence |-> 1,revision |-> 1,exp |-> 2]
     /\ localLease = [v \in Workers |-> NoLease]
     /\ held = {[job |-> j,fence |-> 2,key |-> k,exp |-> 1] : k \in ResourceKeys}
     /\ binding = BindingFor(j,domain)
     /\ phase = [v \in Workers |-> "idle"]
     /\ attempts = {[job |-> j,claim |-> 1,node |-> w,boot |-> 1,fence |-> 2,
                      resolveUntil |-> 2,finished |-> FALSE]}
     /\ ledger = [c \in 1..MaxClaims |-> IF c = 1 THEN
          [job |-> j,node |-> w,boot |-> 1,claim |-> 1,fence |-> 2] ELSE NoToken]
Init == IF Scenario = "recovery" THEN InitRecovery ELSE InitFresh

\* Environment interference is one selected displacement per fragment.
Cancel(j) ==
  /\ ~environment /\ jobs[j].exists /\ jobs[j].state \in {"queued","running"}
  /\ jobs' = [jobs EXCEPT ![j].state = IF @ = "running" THEN "cancelling" ELSE "cancelled",
               ![j].revision = @ + 1, ![j].demand = FALSE]
  /\ environment' = TRUE
  /\ UNCHANGED <<fragment,attempts,held,domain,binding,removed,workerBoot,tokens,localLease,
       phase,requests,publications,items,nextClaim,ledger,renewed,domainChanged,replayed,resolved,
       pruned,compacted,retired,quorum,now,polled,oldReply,recoveryDelay,claimRecords,callStarted>>
Remove(w) ==
  /\ ~environment /\ w \notin removed
  /\ removed' = removed \cup {w}
  /\ domain' = IF domain.owner = w THEN [domain EXCEPT !.owner = No,
                          !.revision = @+1, !.exp = Min(@,0)] ELSE domain
  /\ environment' = TRUE
  /\ UNCHANGED <<fragment,jobs,attempts,held,binding,workerBoot,tokens,localLease,phase,
       requests,publications,items,nextClaim,ledger,renewed,domainChanged,replayed,resolved,
       pruned,compacted,retired,quorum,now,polled,oldReply,recoveryDelay,claimRecords,callStarted>>
LoseResource(k) ==
  /\ ~environment
  /\ held' = {r \in held : r.key /= k}
  /\ environment' = TRUE
  /\ UNCHANGED <<fragment,jobs,attempts,domain,binding,removed,workerBoot,tokens,localLease,
       phase,requests,publications,items,nextClaim,ledger,renewed,domainChanged,replayed,resolved,
       pruned,compacted,retired,quorum,now,polled,oldReply,recoveryDelay,claimRecords,callStarted>>
Takeover(j,w,b,c,sample) ==
  /\ ~environment /\ jobs[j].state = "running" /\ jobs[j].exp <= sample
  /\ ledger[c] = NoToken /\ c >= nextClaim /\ w \notin removed
  /\ jobs' = [jobs EXCEPT ![j].owner = [node |-> w,boot |-> b,claim |-> c],
               ![j].fence = @+1, ![j].revision = @+1, ![j].exp = sample+LeaseMs]
  /\ attempts' = {IF a.job = j THEN [a EXCEPT !.finished = TRUE] ELSE a : a \in attempts}
       \cup {[job |-> j,claim |-> c,node |-> w,boot |-> b,fence |-> jobs[j].fence+1,
               resolveUntil |-> sample+WindowMs,finished |-> FALSE]}
  /\ held' = {r \in held : r.job /= j} \cup
       {[job |-> j,fence |-> jobs[j].fence+1,key |-> k,exp |-> sample+LeaseMs] : k \in ResourceKeys}
  /\ ledger' = [ledger EXCEPT ![c] = [job |-> j,node |-> w,boot |-> b,claim |-> c,fence |-> jobs[j].fence+1]]
  /\ nextClaim' = c+1 /\ environment' = TRUE
  /\ UNCHANGED <<fragment,domain,binding,removed,workerBoot,tokens,localLease,phase,requests,
       publications,items,renewed,domainChanged,replayed,resolved,pruned,compacted,retired,
       quorum,now,polled,oldReply,recoveryDelay,claimRecords,callStarted>>
Outage ==
  /\ ~environment /\ quorum /\ quorum' = FALSE /\ environment' = TRUE
  /\ UNCHANGED <<fragment,jobs,attempts,held,domain,binding,removed,workerBoot,tokens,localLease,
       phase,requests,publications,items,nextClaim,ledger,renewed,domainChanged,replayed,resolved,
       pruned,compacted,retired,now,polled,oldReply,recoveryDelay,claimRecords,callStarted>>

\* Crash/reboot clears locally held authority, preserves committed Store
\* state and in-flight request records. Old reply adoption checks boot.
Restart(w) ==
  /\ ~environment /\ workerBoot[w]<MaxBoots
  /\ workerBoot'=[workerBoot EXCEPT ![w]=@+1]
  /\ tokens'=[tokens EXCEPT ![w]=NoToken]
  /\ localLease'=[localLease EXCEPT ![w]=NoLease]
  /\ environment'=TRUE
  /\ UNCHANGED <<fragment,jobs,attempts,held,domain,binding,removed,phase,
       requests,publications,items,nextClaim,ledger,renewed,domainChanged,replayed,resolved,
       pruned,compacted,retired,quorum,now,polled,oldReply,recoveryDelay,claimRecords,callStarted>>

\* An exact request can remain queued while cancellation changes revision.
SendRenew(w,sample) ==
  /\ ~renewed /\ tokens[w] /= NoToken /\ requests[w] = NoRequest
  /\ requests' = [requests EXCEPT ![w] = [kind |-> "renew",token |-> tokens[w],sample |-> sample]]
  /\ renewed' = TRUE
  /\ UNCHANGED <<fragment,jobs,attempts,held,domain,binding,removed,workerBoot,tokens,
       localLease,phase,publications,items,nextClaim,ledger,environment,domainChanged,replayed,
       resolved,pruned,compacted,retired,quorum,now,polled,oldReply,recoveryDelay,claimRecords,callStarted>>
ApplyRenew(w) ==
  /\ quorum /\ requests[w].kind = "renew"
  /\ LET r == requests[w] t == r.token j == t.job
         ok == /\ (IF Mutation = "renewal_revives_cancel" THEN SameExceptRevision(t) ELSE Exact(t))
               /\ (jobs[j].state = "running" \/
                    (Mutation = "renewal_revives_cancel" /\ jobs[j].state = "cancelling"))
               /\ jobs[j].exp > r.sample /\ Cover(j) /\ t.node \notin removed
         row == [jobs[j] EXCEPT !.state = "running",!.revision = @+1,!.exp = r.sample+LeaseMs]
         fresh == [t EXCEPT !.revision = row.revision,!.exp = row.exp]
     IN /\ jobs' = IF ok THEN [jobs EXCEPT ![j] = row] ELSE jobs
        /\ held' = IF ok THEN {IF h.job=j THEN [h EXCEPT !.exp=row.exp] ELSE h : h \in held} ELSE held
        /\ requests' = [requests EXCEPT ![w] = [kind |-> "renew_reply",ok |-> ok,token |-> fresh]]
  /\ UNCHANGED <<fragment,attempts,domain,binding,removed,workerBoot,tokens,localLease,
       phase,publications,items,nextClaim,ledger,environment,renewed,domainChanged,replayed,
       resolved,pruned,compacted,retired,quorum,now,polled,oldReply,recoveryDelay,claimRecords,callStarted>>
DeliverRenew(w) ==
  /\ requests[w].kind = "renew_reply"
  /\ tokens' = IF requests[w].ok /\ requests[w].token.boot=workerBoot[w] THEN [tokens EXCEPT ![w] = requests[w].token] ELSE tokens
  /\ requests' = [requests EXCEPT ![w] = NoRequest]
  /\ UNCHANGED <<fragment,jobs,attempts,held,domain,binding,removed,workerBoot,localLease,
       phase,publications,items,nextClaim,ledger,environment,renewed,domainChanged,replayed,
       resolved,pruned,compacted,retired,quorum,now,polled,oldReply,recoveryDelay,claimRecords,callStarted>>

DomainChange(w,sample,kind) ==
  /\ ~domainChanged /\ quorum
  /\ localLease[w] /= NoLease
  /\ LET pred == localLease[w]
         ok == domain = pred /\ (kind = "release" \/ domain.exp > sample)
         repl == [domain EXCEPT !.revision = @+1,
                  !.exp = IF kind = "release" THEN Min(@,sample) ELSE sample+DomainTtlMs]
     IN /\ domain' = IF ok THEN repl ELSE domain
        /\ localLease' = IF ok THEN [localLease EXCEPT ![w] = repl] ELSE localLease
  /\ domainChanged' = TRUE
  /\ UNCHANGED <<fragment,jobs,attempts,held,binding,removed,workerBoot,tokens,phase,
       requests,publications,items,nextClaim,ledger,environment,renewed,replayed,resolved,
       pruned,compacted,retired,quorum,now,polled,oldReply,recoveryDelay,claimRecords,callStarted>>

BindAttempt(w,sample) ==
  /\ phase[w]="unbound" /\ quorum
  /\ LET t==tokens[w] l==localLease[w]
         ok== /\ Exact(t) /\ jobs[t.job].state="running" /\ jobs[t.job].exp>sample
               /\ Cover(t.job) /\ t.node \notin removed
               /\ domain=l /\ domain.owner=t.node /\ domain.exp>sample
               /\ (~binding.exists \/ binding.domainFence<l.fence \/ binding=BindingFor(t.job,l))
     IN /\ binding'=IF ok THEN BindingFor(t.job,l) ELSE binding
        /\ phase'=[phase EXCEPT ![w]=IF ok THEN "ready" ELSE "bind_refused"]
  /\ publications'=[publications EXCEPT ![w].refusalSample=sample]
  /\ UNCHANGED <<fragment,jobs,attempts,held,domain,removed,workerBoot,tokens,localLease,requests,
       items,nextClaim,ledger,environment,renewed,domainChanged,replayed,resolved,pruned,
       compacted,retired,quorum,now,polled,oldReply,recoveryDelay,claimRecords,callStarted>>
RefusalRelease(w,sample) ==
  /\ phase[w]="bind_refused" /\ quorum
  /\ domain'=IF domain=localLease[w] THEN [domain EXCEPT !.exp=Min(@,sample),!.revision=@+1] ELSE domain
  /\ phase'=[phase EXCEPT ![w]="refusal_settle"]
  /\ localLease'=[localLease EXCEPT ![w]=NoLease]
  /\ UNCHANGED <<fragment,jobs,attempts,held,binding,removed,workerBoot,tokens,requests,publications,
       items,nextClaim,ledger,environment,renewed,domainChanged,replayed,resolved,pruned,
       compacted,retired,quorum,now,polled,oldReply,recoveryDelay,claimRecords,callStarted>>
RefusalSettle(w,sample) ==
  /\ phase[w]="refusal_settle" /\ quorum
  /\ LET t==tokens[w] j==t.job ok==Exact(t) /\ jobs[j].state="running" /\ jobs[j].exp>sample IN
     /\ jobs'=IF ok THEN [jobs EXCEPT ![j].state="queued",![j].owner=NoOwner,
                            ![j].revision=@+1,![j].notBefore=sample+YieldMs] ELSE jobs
     /\ held'=IF ok THEN {h \in held : h.job/=j} ELSE held
     /\ attempts'=IF ok THEN {IF a.job=j /\ a.fence=t.fence THEN [a EXCEPT !.finished=TRUE] ELSE a : a \in attempts} ELSE attempts
  /\ phase'=[phase EXCEPT ![w]="yielded"]
  /\ UNCHANGED <<fragment,domain,binding,removed,workerBoot,tokens,localLease,requests,publications,
       items,nextClaim,ledger,environment,renewed,domainChanged,replayed,resolved,pruned,
       compacted,retired,quorum,now,polled,oldReply,recoveryDelay,claimRecords,callStarted>>

\* Deliberate caller-premise control; normal code never starts unbound work.
BypassBind(w) ==
  /\ Mutation="binding_ignored" /\ phase[w]="unbound"
  /\ phase'=[phase EXCEPT ![w]="ready"]
  /\ UNCHANGED <<fragment,jobs,attempts,held,domain,binding,removed,workerBoot,tokens,localLease,
       requests,publications,items,nextClaim,ledger,environment,renewed,domainChanged,replayed,
       resolved,pruned,compacted,retired,quorum,now,polled,oldReply,recoveryDelay,claimRecords,callStarted>>

FacadePrecheck(w,sample) ==
  /\ phase[w]="ready" /\ ~callStarted
  /\ callStarted'=TRUE
  /\ localLease[w] /= NoLease
  /\ publications'=[publications EXCEPT ![w].precheck=sample, ![w].pred=localLease[w],![w].boot=workerBoot[w]]
  /\ phase'=[phase EXCEPT ![w]=IF localLease[w].exp>sample THEN "successor" ELSE "facade_error"]
  /\ localLease'=IF localLease[w].exp>sample THEN localLease ELSE [localLease EXCEPT ![w]=NoLease]
  /\ UNCHANGED <<fragment,jobs,attempts,held,domain,binding,removed,workerBoot,tokens,requests,
       items,nextClaim,ledger,environment,renewed,domainChanged,replayed,resolved,pruned,
       compacted,retired,quorum,now,polled,oldReply,recoveryDelay,claimRecords>>
ComputeSuccessor(w,sample) ==
  /\ phase[w]="successor"
  /\ LET pred==publications[w].pred IN
     publications'=[publications EXCEPT ![w].successor=sample,
        ![w].repl=[pred EXCEPT !.revision=@+1,!.exp=Max(sample+PubTtlMs,pred.exp+1)]]
  /\ phase'=[phase EXCEPT ![w]="submit"]
  /\ UNCHANGED <<fragment,jobs,attempts,held,domain,binding,removed,workerBoot,tokens,localLease,
       requests,items,nextClaim,ledger,environment,renewed,domainChanged,replayed,resolved,
       pruned,compacted,retired,quorum,now,polled,oldReply,recoveryDelay,claimRecords,callStarted>>
Submit(w,sample,conflict) ==
  /\ phase[w]="submit" /\ requests[w]=NoRequest
  /\ requests'=[requests EXCEPT ![w]=[kind |-> "publish",sample |-> sample,conflict |-> conflict]]
  /\ publications'=[publications EXCEPT ![w].backend=sample,
                        ![w].samples=Append(@,sample),![w].left=@-1]
  /\ phase'=[phase EXCEPT ![w]="pending"]
  /\ UNCHANGED <<fragment,jobs,attempts,held,domain,binding,removed,workerBoot,tokens,localLease,
       items,nextClaim,ledger,environment,renewed,domainChanged,replayed,resolved,
       pruned,compacted,retired,quorum,now,polled,oldReply,recoveryDelay,claimRecords,callStarted>>
ApplyPublication(w,transactionSample) ==
  /\ quorum /\ phase[w]="pending" /\ requests[w].kind="publish"
  /\ LET p==publications[w] r==requests[w]
         sample==IF Backend="sqlite" THEN transactionSample ELSE r.sample
         valid==Replacement(p.pred,p.repl,sample)
         ok== /\ domain=p.pred /\ domain.exp>sample /\ valid
               /\ (Mutation="binding_ignored" \/ Consistent(jobs,held,binding,removed,domain,sample))
         committed==ok /\ ~r.conflict
         outcome==IF ~valid THEN "invalid" ELSE IF ~ok THEN "fence_rejected"
                  ELSE IF r.conflict THEN "identity_conflict" ELSE "ok"
         event==[jobRows |-> jobs,reservations |-> held,association |-> binding,
                  removedOwners |-> removed,preDomain |-> domain,pred |-> p.pred,
                  repl |-> p.repl,sample |-> sample,worker |-> w]
     IN /\ domain'=IF committed THEN p.repl ELSE domain
        /\ items'=IF committed THEN items \cup {event} ELSE items
        /\ publications'=[publications EXCEPT ![w].backend=sample,
                            ![w].outcome=outcome,![w].committed=committed]
        /\ requests'=[requests EXCEPT ![w]=[kind |-> "publication_reply",outcome |-> outcome]]
  /\ phase'=[phase EXCEPT ![w]="reply"]
  /\ UNCHANGED <<fragment,jobs,attempts,held,binding,removed,workerBoot,tokens,localLease,
       nextClaim,ledger,environment,renewed,domainChanged,replayed,resolved,
       pruned,compacted,retired,quorum,now,polled,oldReply,recoveryDelay,claimRecords,callStarted>>
DeliverPublication(w,outcome) ==
  /\ phase[w]="reply" /\ requests[w].kind="publication_reply"
  /\ outcome \in {requests[w].outcome,"lost_ack","revoked"}
  /\ (outcome="revoked" => requests[w].outcome="ok")
  /\ LET retry==outcome="identity_conflict" /\ Backend="hiqlite" /\ publications[w].left>0 IN
     /\ phase'=[phase EXCEPT ![w]=IF retry THEN "submit" ELSE IF outcome="ok" THEN "done" ELSE "facade_error"]
     /\ localLease'=[localLease EXCEPT ![w]=IF outcome \in {"ok","revoked"} /\ publications[w].boot=workerBoot[w]
                                                    THEN publications[w].repl ELSE NoLease]
  /\ requests'=[requests EXCEPT ![w]=NoRequest]
  /\ UNCHANGED <<fragment,jobs,attempts,held,domain,binding,removed,workerBoot,tokens,
       publications,items,nextClaim,ledger,environment,renewed,domainChanged,replayed,
       resolved,pruned,compacted,retired,quorum,now,polled,oldReply,recoveryDelay,claimRecords,callStarted>>

\* Claim requests retain dispatch/sample/observed revision. Read and CAS split.
SendClaim(w,j,sample) ==
  /\ phase[w]="idle" /\ nextClaim<=MaxClaims
  /\ requests'=[requests EXCEPT ![w]=[kind |-> "claim_read",job |-> j,node |-> w,
       boot |-> workerBoot[w],claim |-> nextClaim,expected |-> jobs[j].revision,
       dispatched |-> sample,sample |-> sample]]
  /\ claimRecords'=[claimRecords EXCEPT ![w]=requests'[w]]
  /\ nextClaim'=nextClaim+1 /\ phase'=[phase EXCEPT ![w]="claiming"]
  /\ UNCHANGED <<fragment,jobs,attempts,held,domain,binding,removed,workerBoot,tokens,localLease,
       publications,items,ledger,environment,renewed,domainChanged,replayed,resolved,pruned,
       compacted,retired,quorum,now,polled,oldReply,recoveryDelay,callStarted>>
ReadClaim(w) ==
  /\ quorum /\ requests[w].kind="claim_read"
  /\ requests'=[requests EXCEPT ![w].kind="claim_cas"]
  /\ UNCHANGED <<fragment,jobs,attempts,held,domain,binding,removed,workerBoot,tokens,localLease,
       phase,publications,items,nextClaim,ledger,environment,renewed,domainChanged,replayed,
       resolved,pruned,compacted,retired,quorum,now,polled,oldReply,recoveryDelay,claimRecords,callStarted>>
ApplyClaim(w) ==
  /\ quorum /\ requests[w].kind="claim_cas"
  /\ LET r==requests[w] j==r.job
         ok== /\ jobs[j].exists /\ jobs[j].revision=r.expected /\ jobs[j].demand
               /\ (jobs[j].state="queued" \/ (jobs[j].state="running" /\ jobs[j].exp<=r.sample))
               /\ jobs[j].notBefore<=r.sample /\ r.node \notin removed
               /\ ~\E a \in attempts : a.claim=r.claim
               /\ \A k \in ResourceKeys : Cardinality({h \in held : h.key=k /\ h.exp>r.sample /\ h.job/=j})<2
         row==[jobs[j] EXCEPT !.state="running",!.owner=[node |-> r.node,boot |-> r.boot,claim |-> r.claim],
                !.fence=@+1,!.revision=@+1,!.exp=r.sample+LeaseMs]
         token==[job |-> j,node |-> r.node,boot |-> r.boot,claim |-> r.claim,
                  fence |-> row.fence,revision |-> row.revision,exp |-> row.exp]
     IN /\ jobs'=IF ok THEN [jobs EXCEPT ![j]=row] ELSE jobs
        /\ attempts'=IF ok THEN {IF a.job=j THEN [a EXCEPT !.finished=TRUE] ELSE a : a \in attempts}
            \cup {[job |-> j,node |-> r.node,boot |-> r.boot,claim |-> r.claim,
                     fence |-> row.fence,resolveUntil |-> r.sample+WindowMs,finished |-> FALSE]} ELSE attempts
        /\ held'=IF ok THEN {h \in held : h.job/=j /\ h.exp>r.sample} \cup
            {[job |-> j,fence |-> row.fence,key |-> k,exp |-> row.exp] : k \in ResourceKeys} ELSE held
        /\ ledger'=IF ok THEN [ledger EXCEPT ![r.claim]=TokenTuple(token)] ELSE ledger
        /\ requests'=[requests EXCEPT ![w]=[kind |-> "claim_reply",ok |-> ok,token |-> token]]
  /\ UNCHANGED <<fragment,domain,binding,removed,workerBoot,tokens,localLease,phase,
       publications,items,nextClaim,environment,renewed,domainChanged,replayed,resolved,pruned,
       compacted,retired,quorum,now,polled,oldReply,recoveryDelay,claimRecords,callStarted>>
DeliverClaim(w,lose) ==
  /\ requests[w].kind="claim_reply" /\ phase[w]="claiming"
  /\ tokens'=IF requests[w].ok /\ ~lose /\ requests[w].token.boot=workerBoot[w] THEN [tokens EXCEPT ![w]=requests[w].token] ELSE tokens
  /\ phase'=[phase EXCEPT ![w]="claimed"]
  /\ requests'=[requests EXCEPT ![w]=NoRequest]
  /\ UNCHANGED <<fragment,jobs,attempts,held,domain,binding,removed,workerBoot,localLease,
       publications,items,nextClaim,ledger,environment,renewed,domainChanged,replayed,
       resolved,pruned,compacted,retired,quorum,now,polled,oldReply,recoveryDelay,claimRecords,callStarted>>
ReplayClaim(w,sample) ==
  /\ claimRecords[w].kind="claim_read" /\ ~replayed[w] /\ quorum
  /\ LET r==claimRecords[w] j==r.job
         current==jobs[j].exists /\ jobs[j].owner=[node |-> r.node,boot |-> r.boot,claim |-> r.claim]
         window==sample>=r.dispatched /\ sample-r.dispatched<WindowMs
         ok==current /\ window /\ r.boot=workerBoot[w] /\ jobs[j].state="running" /\ jobs[j].exp>sample /\ Cover(j) /\ r.node \notin removed
         mutate==Mutation="replay_fresh_fence" /\ current /\ window
     IN /\ jobs'=IF mutate THEN [jobs EXCEPT ![j].fence=@+1,![j].revision=@+1] ELSE jobs
        /\ tokens'=IF ok THEN [tokens EXCEPT ![w]=IF mutate THEN [Token(j) EXCEPT !.fence=@+1,!.revision=@+1] ELSE Token(j)] ELSE tokens
        /\ attempts'=IF mutate THEN {IF a.job=j /\ a.claim=r.claim THEN [a EXCEPT !.fence=@+1] ELSE a : a \in attempts} ELSE attempts
  /\ replayed'=[replayed EXCEPT ![w]=TRUE]
  /\ UNCHANGED <<fragment,held,domain,binding,removed,workerBoot,localLease,phase,
       requests,publications,items,nextClaim,ledger,environment,renewed,domainChanged,resolved,
       pruned,compacted,retired,quorum,now,polled,oldReply,recoveryDelay,claimRecords,callStarted>>
ResolveClaim(w,sample) ==
  /\ claimRecords[w].kind="claim_read" /\ ~resolved[w] /\ quorum
  /\ LET r==claimRecords[w] j==r.job
         retained==\E a \in attempts : a.job=j /\ a.claim=r.claim /\ a.node=r.node /\ a.boot=r.boot
         ok== /\ retained /\ r.boot=workerBoot[w] /\ jobs[j].exists /\ jobs[j].owner=[node |-> r.node,boot |-> r.boot,claim |-> r.claim]
               /\ sample>=r.dispatched /\ sample-r.dispatched<WindowMs
               /\ jobs[j].state="running" /\ jobs[j].exp>sample /\ Cover(j) /\ r.node \notin removed
     IN tokens'=IF ok THEN [tokens EXCEPT ![w]=Token(j)] ELSE tokens
  /\ resolved'=[resolved EXCEPT ![w]=TRUE]
  /\ UNCHANGED <<fragment,jobs,attempts,held,domain,binding,removed,workerBoot,localLease,
       phase,requests,publications,items,nextClaim,ledger,environment,renewed,domainChanged,replayed,
       pruned,compacted,retired,quorum,now,polled,oldReply,recoveryDelay,claimRecords,callStarted>>

AcquireDomain(w,sample) ==
  /\ phase[w]="claimed" /\ tokens[w]/=NoToken /\ ~domainChanged /\ quorum
  /\ tokens[w].node \notin removed /\ domain.exp<=sample
  /\ domain'=[owner |-> w,fence |-> domain.fence+1,revision |-> domain.revision+1,exp |-> sample+DomainTtlMs]
  /\ localLease'=[localLease EXCEPT ![w]=domain']
  /\ phase'=[phase EXCEPT ![w]="unbound"] /\ domainChanged'=TRUE
  /\ UNCHANGED <<fragment,jobs,attempts,held,binding,removed,workerBoot,tokens,
       requests,publications,items,nextClaim,ledger,environment,renewed,replayed,resolved,
       pruned,compacted,retired,quorum,now,polled,oldReply,recoveryDelay,claimRecords,callStarted>>

\* Retention eligibility classes: preserve conjuncts and two pruning branches.
PrepareRetention(j,age,pressure,pin,keep,user,legacy) ==
  /\ fragment="retention" /\ ~environment
  /\ jobs'=[jobs EXCEPT ![j].state="failed",![j].owner=NoOwner,![j].finished=TRUE,
       ![j].demand=pin,![j].ageEligible=age,![j].pressureEligible=pressure,
       ![j].retain=keep,![j].userScope=user,![j].legacy=legacy]
  /\ attempts'={IF a.job=j THEN [a EXCEPT !.finished=TRUE] ELSE a : a \in attempts}
  /\ environment'=TRUE
  /\ UNCHANGED <<fragment,held,domain,binding,removed,workerBoot,tokens,localLease,phase,requests,
       publications,items,nextClaim,ledger,renewed,domainChanged,replayed,resolved,pruned,
       compacted,retired,quorum,now,polled,oldReply,recoveryDelay,claimRecords,callStarted>>
PruneAttempts(sample) ==
  /\ fragment="retention" /\ environment /\ ~pruned
  /\ attempts'={a \in attempts : ~(a.finished /\ a.resolveUntil<=sample /\
                         (Cardinality({n \in attempts : n.job=a.job /\ n.finished /\ n.fence>a.fence})>=1
                          \/ (Cardinality(attempts)>=2 /\ \E n \in attempts : n.job=a.job /\ n.fence>a.fence)))}
  /\ pruned'=TRUE
  /\ UNCHANGED <<fragment,jobs,held,domain,binding,removed,workerBoot,tokens,localLease,phase,requests,
       publications,items,nextClaim,ledger,environment,renewed,domainChanged,replayed,resolved,
       compacted,retired,quorum,now,polled,oldReply,recoveryDelay,claimRecords,callStarted>>
CompactInternalReceipts(j,expired,pressure) ==
  /\ fragment="retention" /\ environment /\ ~compacted
  /\ jobs'=[jobs EXCEPT ![j].receipt=IF jobs[j].finished /\ ~jobs[j].retain /\
       (expired \/ (pressure /\ ~jobs[j].userScope /\ jobs[j].state \notin {"queued","running","cancelling"}))
       THEN FALSE ELSE @]
  /\ compacted'=TRUE
  /\ UNCHANGED <<fragment,attempts,held,domain,binding,removed,workerBoot,tokens,localLease,phase,requests,
       publications,items,nextClaim,ledger,environment,renewed,domainChanged,replayed,resolved,
       pruned,retired,quorum,now,polled,oldReply,recoveryDelay,claimRecords,callStarted>>
RetireTerminalJob(j) ==
  /\ fragment="retention" /\ environment /\ ~retired
  /\ jobs[j].exists /\ jobs[j].finished /\ ~jobs[j].demand /\ ~jobs[j].legacy
  /\ jobs[j].ageEligible \/ jobs[j].pressureEligible
  /\ jobs'=[jobs EXCEPT ![j].exists=FALSE]
  /\ attempts'={a \in attempts : a.job/=j} /\ held'={h \in held : h.job/=j}
  /\ retired'=TRUE
  /\ UNCHANGED <<fragment,domain,binding,removed,workerBoot,tokens,localLease,phase,requests,
       publications,items,nextClaim,ledger,environment,renewed,domainChanged,replayed,resolved,
       pruned,compacted,quorum,now,polled,oldReply,recoveryDelay,claimRecords,callStarted>>

SafetyNext ==
  \/ \E j \in Jobs : Cancel(j)
  \/ \E w \in Workers : Remove(w) \/ Restart(w)
  \/ \E k \in ResourceKeys : LoseResource(k)
  \/ \E j \in Jobs,w \in Workers,b \in 1..MaxBoots,c \in 1..MaxClaims,s \in Samples : Takeover(j,w,b,c,s)
  \/ Outage
  \/ \E w \in Workers,s \in Samples : SendRenew(w,s) \/ BindAttempt(w,s) \/ RefusalRelease(w,s) \/ RefusalSettle(w,s) \/ FacadePrecheck(w,s)
           \/ ComputeSuccessor(w,s) \/ ApplyPublication(w,s) \/ ReplayClaim(w,s) \/ ResolveClaim(w,s)
           \/ DomainChange(w,s,"renew") \/ DomainChange(w,s,"release") \/ AcquireDomain(w,s)
  \/ \E w \in Workers : ApplyRenew(w) \/ DeliverRenew(w) \/ BypassBind(w) \/ ReadClaim(w) \/ ApplyClaim(w)
  \/ \E w \in Workers,c \in BOOLEAN,s \in Samples : Submit(w,s,c)
  \/ \E w \in Workers,o \in {"ok","identity_conflict","invalid","fence_rejected","lost_ack","revoked"} : DeliverPublication(w,o)
  \/ \E w \in Workers,j \in Jobs,s \in Samples : SendClaim(w,j,s)
  \/ \E w \in Workers,l \in BOOLEAN : DeliverClaim(w,l)
  \/ \E j \in Jobs,a \in BOOLEAN,p \in BOOLEAN,pin \in BOOLEAN,k \in BOOLEAN,u \in BOOLEAN,l \in BOOLEAN :
            PrepareRetention(j,a,p,pin,k,u,l)
  \/ \E s \in Samples : PruneAttempts(s)
  \/ \E j \in Jobs,e \in BOOLEAN,p \in BOOLEAN : CompactInternalReceipts(j,e,p)
  \/ \E j \in Jobs : RetireTerminalJob(j)

\* Recovery reserves claim2 for the surviving boot. No identity exhaustion or
\* safety schedule budget can block the path before first fresh authority.
FreshClaimHeld == jobs[Target].state="running" /\ jobs[Target].owner.claim=2
RecoveryTick ==
  /\ ~FreshClaimHeld /\ now<3 /\ now'=now+1
  /\ UNCHANGED <<fragment,jobs,attempts,held,domain,binding,removed,workerBoot,tokens,localLease,
       phase,requests,publications,items,nextClaim,ledger,environment,renewed,domainChanged,replayed,
       resolved,pruned,compacted,retired,quorum,polled,oldReply,recoveryDelay,claimRecords,callStarted,events>>
RecoveryPoll ==
  /\ ~FreshClaimHeld /\ phase[Survivor]="idle"
  /\ ~(Mutation="retry_stalls" /\ polled)
  /\ polled'=TRUE
  /\ phase'=[phase EXCEPT ![Survivor]=IF now>=jobs[Target].notBefore /\ now>=jobs[Target].exp
                                                   THEN "send" ELSE "retry"]
  /\ UNCHANGED <<fragment,jobs,attempts,held,domain,binding,removed,workerBoot,tokens,localLease,
       requests,publications,items,nextClaim,ledger,environment,renewed,domainChanged,replayed,
       resolved,pruned,compacted,retired,quorum,now,oldReply,recoveryDelay,claimRecords,callStarted,events>>
RecoveryRetry ==
  /\ phase[Survivor]="retry" /\ phase'=[phase EXCEPT ![Survivor]="idle"]
  /\ UNCHANGED <<fragment,jobs,attempts,held,domain,binding,removed,workerBoot,tokens,localLease,
       requests,publications,items,nextClaim,ledger,environment,renewed,domainChanged,replayed,
       resolved,pruned,compacted,retired,quorum,now,polled,oldReply,recoveryDelay,claimRecords,callStarted,events>>
RecoverySend ==
  /\ phase[Survivor]="send"
  /\ requests'=[requests EXCEPT ![Survivor]=[kind |-> "recovery_claim",sample |-> now,expected |-> jobs[Target].revision]]
  /\ phase'=[phase EXCEPT ![Survivor]="apply"]
  /\ UNCHANGED <<fragment,jobs,attempts,held,domain,binding,removed,workerBoot,tokens,localLease,
       publications,items,nextClaim,ledger,environment,renewed,domainChanged,replayed,
       resolved,pruned,compacted,retired,quorum,now,polled,oldReply,recoveryDelay,claimRecords,callStarted,events>>
RecoveryApply ==
  /\ phase[Survivor]="apply" /\ quorum
  /\ LET j==Target w==Survivor r==requests[w] IN
     /\ r.expected=jobs[j].revision /\ r.sample>=jobs[j].exp /\ r.sample>=jobs[j].notBefore
     /\ jobs'=[jobs EXCEPT ![j].owner=[node |-> w,boot |-> 2,claim |-> 2],
                              ![j].fence=@+1,![j].revision=@+1,![j].exp=r.sample+LeaseMs]
     /\ attempts'={ [a EXCEPT !.finished=TRUE] : a \in attempts } \cup
           {[job |-> j,node |-> w,boot |-> 2,claim |-> 2,fence |-> jobs[j].fence+1,
                      resolveUntil |-> r.sample+WindowMs,finished |-> FALSE]}
     /\ held'={[job |-> j,fence |-> jobs[j].fence+1,key |-> k,exp |-> r.sample+LeaseMs] : k \in ResourceKeys}
     /\ ledger'=[ledger EXCEPT ![2]=[job |-> j,node |-> w,boot |-> 2,claim |-> 2,fence |-> jobs[j].fence+1]]
  /\ phase'=[phase EXCEPT ![Survivor]="ack"] /\ nextClaim'=3
  /\ UNCHANGED <<fragment,domain,binding,removed,workerBoot,tokens,localLease,
       requests,publications,items,environment,renewed,domainChanged,replayed,
       resolved,pruned,compacted,retired,quorum,now,polled,oldReply,recoveryDelay,claimRecords,callStarted,events>>
RecoveryAck ==
  /\ phase[Survivor]="ack"
  /\ tokens'=[tokens EXCEPT ![Survivor]=Token(Target)]
  /\ phase'=[phase EXCEPT ![Survivor]="done"]
  /\ UNCHANGED <<fragment,jobs,attempts,held,domain,binding,removed,workerBoot,localLease,
       requests,publications,items,nextClaim,ledger,environment,renewed,domainChanged,replayed,
       resolved,pruned,compacted,retired,quorum,now,polled,oldReply,recoveryDelay,claimRecords,callStarted,events>>
DeliverOldReply ==
  /\ ~oldReply /\ oldReply'=TRUE
  \* Reply belongs to boot1; current boot2 must not adopt it.
  /\ UNCHANGED <<fragment,jobs,attempts,held,domain,binding,removed,workerBoot,tokens,localLease,
       phase,requests,publications,items,nextClaim,ledger,environment,renewed,domainChanged,replayed,
       resolved,pruned,compacted,retired,quorum,now,polled,recoveryDelay,claimRecords,callStarted,events>>
RecoveryNext == RecoveryTick \/ RecoveryPoll \/ RecoveryRetry \/ RecoverySend
                \/ RecoveryApply \/ RecoveryAck \/ DeliverOldReply
Next == IF Scenario="recovery" THEN RecoveryNext /\ UNCHANGED events
        ELSE events<MaxEvents /\ events'=events+1 /\ SafetyNext
Spec == Init /\ [][Next]_vars
FairSpec == Spec /\ WF_vars(RecoveryTick) /\ WF_vars(RecoveryPoll)
                /\ WF_vars(RecoveryRetry) /\ WF_vars(RecoverySend)
                /\ WF_vars(RecoveryApply) /\ WF_vars(RecoveryAck)
                /\ WF_vars(DeliverOldReply)
L1_FreshClaimEventually == <>FreshClaimHeld

TokenType == [job : Jobs,node : Workers,boot : 1..MaxBoots,claim : 1..MaxClaims,
              fence : 1..5,revision : 1..8,exp : 0..8]
LeaseType == [owner : Workers \cup {No},fence : 0..5,revision : 0..8,exp : 0..8]
S0_TypeAndIdentity ==
  /\ jobs \in [Jobs -> JobType]
  /\ workerBoot \in [Workers -> 1..MaxBoots]
  /\ tokens \in [Workers -> TokenType \cup {NoToken}]
  /\ localLease \in [Workers -> LeaseType]
  /\ domain \in LeaseType
  /\ removed \subseteq Workers /\ quorum \in BOOLEAN
  /\ now \in 0..3 /\ events \in 0..MaxEvents
  /\ \A h \in held : h.job \in Jobs /\ h.key \in ResourceKeys /\ h.fence \in 1..5 /\ h.exp \in 0..8
  /\ \A a \in attempts : a.job \in Jobs /\ a.node \in Workers /\ a.boot \in 1..MaxBoots
       /\ a.claim \in 1..MaxClaims /\ a.fence \in 1..5 /\ a.resolveUntil \in 0..8 /\ a.finished \in BOOLEAN
  /\ (binding.exists => (binding.job \in Jobs /\ binding.node \in Workers
       /\ binding.boot \in 1..MaxBoots /\ binding.claim \in 1..MaxClaims
       /\ binding.jobFence \in 1..5 /\ binding.domainFence \in 1..5))
  /\ nextClaim<=MaxClaims+1
  /\ \A j \in Jobs : (jobs[j].state \in {"running","cancelling"}) <=> (jobs[j].owner/=NoOwner)
  /\ \A a,b \in attempts : (a.job=b.job /\ a.fence=b.fence) => a=b
S1_ExclusiveCurrentAttempt ==
  \A j \in Jobs : Cardinality({a \in attempts : a.job=j /\ a.fence=jobs[j].fence /\ ~a.finished})<=1
S2_PublicationAuthority == \A it \in items :
  BoundAuthority(it.jobRows,it.reservations,it.association,it.removedOwners,
                 it.preDomain,it.pred,it.repl,it.sample)
S3_Displacement == \A it \in items :
  LiveBound(it.jobRows,it.reservations,it.association,it.removedOwners,it.sample)
S4_ReplayStability ==
  /\ \A a \in attempts : ledger[a.claim]/=NoToken /\
       ledger[a.claim]=[job |-> a.job,node |-> a.node,boot |-> a.boot,claim |-> a.claim,fence |-> a.fence]
  /\ \A w \in Workers : tokens[w]/=NoToken => ledger[tokens[w].claim]=TokenTuple(tokens[w])
S5_CancellationPrecedence ==
  [][\A j \in Jobs : (jobs[j].exists /\ jobs[j].state="cancelling" /\ jobs'[j].exists)
                        => jobs'[j].state \in {"cancelling","cancelled"}]_vars
S6_BindingContinuity ==
  [][(binding.exists /\ binding' /= binding) => binding'.domainFence>binding.domainFence]_vars
S7_RejectionPreservesCatalogue ==
  [][\A w \in Workers :
      (phase[w]="pending" /\ phase'[w]="reply" /\
       publications'[w].outcome \in {"invalid","fence_rejected","identity_conflict"})
         => (items'=items /\ domain'=domain)]_vars
\* Reachability probes are deliberately false invariants. A violated probe
\* yields an actual reachable path. They never replace positive checks.
Witness_ThreeSamples == ~\E w \in Workers :
  publications[w].precheck=0 /\ publications[w].successor=2 /\ publications[w].backend=1
Witness_FourSubmissions == ~\E w \in Workers : Len(publications[w].samples)=4
Witness_CancelledRenewal == ~\E w \in Workers :
  requests[w].kind="renew" /\ jobs[requests[w].token.job].state="cancelling"
Witness_Replay == ~\E w \in Workers : replayed[w] /\ tokens[w]/=NoToken /\
  ledger[tokens[w].claim]=TokenTuple(tokens[w])
Witness_BindRefusal == ~\E w \in Workers :
  phase[w]="bind_refused" /\ binding.exists /\ tokens[w].claim/=binding.claim /\
  domain.fence=binding.domainFence /\ publications[w].refusalSample=0 /\
  Exact(tokens[w]) /\ domain=localLease[w] /\ Cover(tokens[w].job)
Witness_PrecommitRollback == ~\E w \in Workers :
  phase[w]="reply" /\ publications[w].outcome="identity_conflict" /\
  domain=publications[w].pred /\ items={}
Witness_Displacement == ~\E w \in Workers :
  publications[w].outcome="fence_rejected" /\ binding.exists /\
  jobs[binding.job].owner/=NoOwner /\ jobs[binding.job].owner.claim/=binding.claim
PublicationWitnessBound == ~environment /\ ~renewed /\ ~domainChanged /\ nextClaim=2

\* Positive-safety quotient. Observed stale revisions cannot become current:
\* revisions strictly increase, claim ids never recycle, deleted jobs never
\* re-admit. Numeric expiry is read only behind Exact for these tokens.
\* Retain a type bit so S0 cannot hide an ill-typed obsolete representative.
TokenView(t) == IF t=NoToken THEN NoToken ELSE
  [stable |-> TokenTuple(t),current |-> Exact(t),wellTyped |-> t \in TokenType]
RequestView(r) ==
  CASE r.kind="renew" -> [kind |-> r.kind,token |-> TokenView(r.token),sample |-> r.sample]
    [] r.kind \in {"renew_reply","claim_reply"} ->
       IF r.ok THEN [kind |-> r.kind,ok |-> TRUE,token |-> TokenView(r.token)]
       ELSE [kind |-> r.kind,ok |-> FALSE]
    [] r.kind \in {"claim_read","claim_cas"} ->
       [kind |-> r.kind,job |-> r.job,node |-> r.node,boot |-> r.boot,claim |-> r.claim,
        expectedCurrent |-> r.expected=jobs[r.job].revision,
        dispatched |-> r.dispatched,sample |-> r.sample]
    [] OTHER -> r
ClaimRecordView(r) == IF r=NoRequest THEN r ELSE
  [kind |-> r.kind,job |-> r.job,node |-> r.node,boot |-> r.boot,
   claim |-> r.claim,dispatched |-> r.dispatched]
\* Receipt diagnostics never affect a future authority action. Their only
\* live maintenance decision is the exact terminal-retirement predicate;
\* compaction changes only receipt (omitted) and its retained once budget.
JobView(row) ==
  [exists |-> row.exists,state |-> row.state,owner |-> row.owner,
   fence |-> row.fence,revision |-> row.revision,exp |-> row.exp,
   notBefore |-> row.notBefore,demand |-> row.demand,finished |-> row.finished,
   retirementEligible |-> row.finished /\ ~row.demand /\ ~row.legacy /\
                           (row.ageEligible \/ row.pressureEligible),
   wellTyped |-> row \in JobType]
PubView(w) == IF phase[w] \in {"successor","submit","pending","reply"} THEN
  [pred |-> publications[w].pred,repl |-> publications[w].repl,
   left |-> publications[w].left,outcome |-> publications[w].outcome,
   committed |-> publications[w].committed,boot |-> publications[w].boot]
  ELSE NoRequest
EventView(it) ==
  [authorized |-> BoundAuthority(it.jobRows,it.reservations,it.association,it.removedOwners,
                                it.preDomain,it.pred,it.repl,it.sample),
   current |-> LiveBound(it.jobRows,it.reservations,it.association,it.removedOwners,it.sample)]
\* Raw states/traces retain all data. This is a positive-only quotient,
\* justified in README; controls, witnesses and liveness do not use it.
ModelView == <<fragment,[j \in Jobs |-> JobView(jobs[j])],attempts,held,domain,binding,removed,workerBoot,
  [w \in Workers |-> TokenView(tokens[w])],localLease,phase,
  [w \in Workers |-> RequestView(requests[w])],
  [w \in Workers |-> PubView(w)],
  Cardinality(items),{EventView(it) : it \in items},
  nextClaim,ledger,environment,renewed,domainChanged,replayed,resolved,
  pruned,compacted,retired,quorum,now,polled,oldReply,recoveryDelay,
  [w \in Workers |-> ClaimRecordView(claimRecords[w])],callStarted,events>>
Symm == Permutations(Workers) \cup Permutations(ResourceKeys) \cup Permutations(Jobs)
=============================================================================
