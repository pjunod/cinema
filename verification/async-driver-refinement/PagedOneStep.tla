----------------------------- MODULE PagedOneStep -----------------------------
EXTENDS AsyncDriverRefinement, PagedTables
VARIABLE stepped
allvars == <<vars,stepped>>
At(p) == /\ sql = p.sql /\ w = CertW(p) /\ available = CertAvailable(p)
 /\ completed = SeqSet(p.completed) /\ mono_now_ms = p.mono_now_ms
 /\ wall_anchors = SeqSet(p.wall_anchors) /\ mono_anchors = SeqSet(p.mono_anchors)
 /\ claim_loss_keys = SeqSet(p.claim_loss_keys)
PageInit == /\ ~stepped /\ \E p \in OwnedPredecessors : At(p)
PageStep == /\ ~stepped /\ Next /\ stepped' = TRUE
PageSpec == PageInit /\ [][PageStep]_allvars
(* Original Next is evaluated with full outgoing dependencies, never guarded by
   page membership. A missing target fails this separate coverage invariant. *)
NoOmittedOriginalStep == \A p \in CurrentCert :
 ~ENABLED (Next /\ ~(\E q \in CertTargets(p) : TargetEquals(q)))
PageAgreement == ~stepped =>
 /\ CertifiedReachable /\ DemandCovered /\ CapacityAgrees
 /\ CertificateNextAgreement /\ NoOmittedOriginalStep
 /\ FairPairAgreement /\ FairEnablednessAgreement
PageSafety == ~stepped => RequiredSourceInvariants
=============================================================================
