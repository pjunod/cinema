---------------------------- MODULE PagedComponent ----------------------------
EXTENDS Naturals, Integers, Sequences, FiniteSets, TLC, ComponentTables
Mon == INSTANCE PropertyMonitors WITH MonitorObligations <- AllObligations
HistoryAgreement == /\ Histories[OriginalRoot] = Mon!InitialHistory(OriginalProducts[OriginalRoot])
 /\ \A e \in FullMonitorEdges : Histories[e.dst] = Mon!AdvanceHistory(Histories[e.src],OriginalProducts[e.dst])
VARIABLE node
cvars == <<node>>
InternalEdges == {e \in ComponentEdges : e.src \in ComponentMembers /\ e.dst \in ComponentMembers}
ComponentInit == node \in ComponentMembers
ComponentNext == \E e \in InternalEdges : node=e.src /\ node'=e.dst
(* FullEnabled is the independently checked original nonstuttering enabledness,
   including outgoing edges. No last-action variable can repeat a past event. *)
En(a) == a \in FullEnabled[node]
Occ(a) == \E e \in InternalEdges : node=e.src /\ node'=e.dst
 /\ e.nonstuttering /\ a \in e.labels
Weak(a) == ([]<>(~En(a))) \/ ([]<>Occ(a))
Strong(a) == (<>[](~En(a))) \/ ([]<>Occ(a))
OriginalSuffixFairness == /\ \A a \in WeakLabels : Weak(a)
 /\ \A a \in StrongLabels : Strong(a)
ComponentSpec == ComponentInit /\ [][ComponentNext]_cvars /\ OriginalSuffixFairness
ViolatingHistory(n) == CASE SelectedProperty="seen_goal_false" -> ~Histories[n].seen_goal
 [] SelectedProperty="pending_true" -> SelectedObligation \in Histories[n].pending_attempts
 [] SelectedProperty="capacity_ok_pending_true" -> Histories[n].capacity_ok
    /\ SelectedObligation \in Histories[n].pending_attempts
(* A fair path permanently in this exact history view refutes the source
   property. Prefix reachability/history and every component member are checked
   independently before TLC. Stuttering is original, including deadlocks. *)
EventuallyDischarged == <>~ViolatingHistory(node)
=============================================================================
