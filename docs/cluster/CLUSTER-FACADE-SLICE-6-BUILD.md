# Cluster facade slice 6 — fresh sharing and custody capabilities

**Status:** open — plan committed before implementation. **Owner:** Codex.

Companion to [the facade contract](CLUSTER-FACADE-BUILD.md) and
[slice 5](CLUSTER-FACADE-SLICE-5-BUILD.md). Start from slice 5 checkpoint
`6e87406c2`; final integration uses the current effort and preserves C7
and the existing B3/C1/source-reader-capacity owners. Integration target:
`effort/cluster-facade`.

## Contract

Route sharing and Source callers through cloneable facade capabilities,
retaining A6's built policy. Advisory member-floor observations remain
coupled to the current serving-fence advisory epoch. Unknown, timeout and
not-ready remain distinct from affirmative revocation; SessionGrace,
UnknownSpan and existing body-owned monitors keep their original timing
and retirement rules. Authority subscriptions use the original fence
receiver, and accepting-new-media delegates the exact original predicate.

Source and receiver observations remain fresh at every existing boundary:
`Result<Option<SourceAdmissionMembers>, MembershipError>` and separately
`Result<Option<IngressCustodyMembers>, MembershipError>`. Return the
existing opaque core witnesses; never substitute cached booleans, exchange
the two purposes, expose a raw manager, or change their freshness guards,
write guards, purpose fingerprints or import-generation checks.

`SourceProducerAuthority` retains a cloneable Source capability rather than
a manager or a saved witness. Both immutable and direct producer factories
construct it. Every existing observation before spawn, publication, range
continuation and body retirement reacquires its original fresh witness.
Keep source-reader capacity, connection custody, principal registration,
physical retirement and publication ownership unchanged.

Incoming exact-request authorization and response signing move into one
cluster-owned authentication capability inside the existing inline `calls`
module. It delegates the existing core security methods without requiring
an outbound HTTP client. Preserve nonce consumption, role/member floors,
request body/path/method binding, response target/nonce/status/body binding,
all refusals, and the callers' existing deadlines and error mapping. Do not
create a second authorization algorithm or claim cached-proof B5 work.

Forwarding uses the existing route-sensitive media roster, preserving
whether the requested route admits learners. No new policy, background
task, watchdog, toggle, pool, transport protocol or durable format is added.

## Ownership

| Files | Work |
|---|---|
| `crates/plurxd/src/cluster.rs` | Typed sharing/Source/receiver observations, exact fence delegates, incoming authentication/signing capability inside the existing facade boundary. |
| `sharing.rs`, `http/shared_library.rs`, `http/shared_receiver_playback.rs` | Existing authority subscriptions and epoch-bound advisory member floor. Local `ListenerWake.serving` is a watch receiver, not a raw fence; retain it honestly. |
| `http/shared_receiver_ingress_custody.rs`, `http/shared_source_ingress.rs`, `http/shared_source_playback.rs` | Fresh purpose-specific witnesses and exact new-media admission predicate. |
| `transcode/source_actor.rs`, `transcode/source_actor/growing.rs`, `transcode/source_direct.rs` | Producer retains typed Source capability; all fresh observation boundaries unchanged. |
| `sharing_connection_custody.rs`, `http/shared_receiver_forwarding.rs`, `http/shared_source_forwarding.rs` | Single exact incoming authentication/signing owner and route-sensitive media lookup. |
| `cluster/census.rs`, ownership inventory and formal source mappings if measured | Lower actual pins only; retain matchers/exemptions, genuine local fields and all proof sentinels. |
| This plan, cluster index, project backlog, functionality catalog | Delivery and actual evidence. |

This task does not rebuild A6, change reconciliation or implement the
separately owned B3/C1 and cluster-wide source reader. It does not complete
B5's cached proofs, outcome health or first-success holder races.

## Validation and delivery

Establish the pinned Rust 1.97.1 local compiler loop before Rust edits. Run
focused existing A6 revocation/member-floor tests, principal registration
overlap with known versus unknown answers, receiver/source registration
coordination, Source actor cancellation/capacity/body retirement and core
member-observation age/floor/unresolved-intent guards with `hiqlite-store`.
Retain and run exact request/response signing tests affected by routing.

Run all-target checks, formatting, strict Clippy, measured census/ratchet,
ownership, formal source correspondence and paperwork. Record baseline or
environment failures without suppressions, pin increases or weakened
validators. Integrate the actual current effort before one whole-diff
adversarial review, fix findings, run the Effort gate once on its exact
head and land only on a pass with regression trailers. Main promotion,
deployment and fleet acceptance remain separate. No execution or review
success is claimed by this plan.
