--------------------------- MODULE PropertyMonitors ---------------------------
EXTENDS Naturals, Sequences, FiniteSets
CONSTANTS MonitorObligations
Goal(p) == p.done_jobs = {0,1}
Trigger(p,o) == LET x == p.workers[o.node+1] IN
 x.stage = "driver" /\ x.started /\ x.job = o.job /\ x.attempt = o.claim_id
Discharge(p,o) == LET x == p.workers[o.node+1] IN
 x.stage = "driver" /\ x.dropped /\ x.job = o.job /\ x.attempt = o.claim_id
InitialHistory(p) == [seen_goal |-> Goal(p),capacity_ok |-> p.capacity,
 pending_attempts |-> {o \in MonitorObligations : Trigger(p,o) /\ ~Discharge(p,o)}]
AdvanceHistory(h,p) == [seen_goal |-> h.seen_goal \/ Goal(p),
 capacity_ok |-> h.capacity_ok /\ p.capacity,
 pending_attempts |-> {o \in MonitorObligations : (o \in h.pending_attempts \/ Trigger(p,o)) /\ ~Discharge(p,o)}]
=============================================================================
