-------------------------- MODULE BoundaryControls --------------------------
EXTENDS Naturals, Integers, Sequences, FiniteSets, SourceTables
CONSTANT Mutation
VARIABLES sql, phase, prior, revoked, acceptedAfterRevoke, completed
vars == <<sql,phase,prior,revoked,acceptedAfterRevoke,completed>>
Done(s) == {j \in 0..1 : StoreStates[s+1].jobs[j+1].done}
Init == /\ sql = StoreInitial /\ prior = sql /\ phase = "ready"
 /\ revoked = FALSE /\ acceptedAfterRevoke = FALSE /\ completed = Done(sql)
LocalRevoke == /\ Mutation = "local-revocation" /\ ~revoked /\ revoked' = TRUE
 /\ UNCHANGED <<sql,phase,prior,acceptedAfterRevoke,completed>>
Apply == /\ phase = "ready"
 /\ \E e \in StoreEdges :
    /\ e.src = sql /\ sql' = e.dst /\ prior' = sql /\ phase' = "applied"
    /\ completed' = completed \cup Done(e.dst)
    /\ acceptedAfterRevoke' = acceptedAfterRevoke \/
       (revoked /\ e.op = "complete" /\ e.result = "accepted")
 /\ UNCHANGED revoked
Reply == /\ phase = "applied" /\ phase' = "ready"
 /\ sql' = IF Mutation = "unknown-as-rollback" /\ Done(sql) # Done(prior) THEN prior ELSE sql
 /\ UNCHANGED <<prior,revoked,acceptedAfterRevoke,completed>>
Next == LocalRevoke \/ Apply \/ Reply
Spec == Init /\ [][Next]_vars
NoCommitAfterLocalRevocation == ~acceptedAfterRevoke
ResultAtomicity == completed \subseteq Done(sql)
NoStaleCommit == \A e \in StoreEdges : e.fresh
=============================================================================
