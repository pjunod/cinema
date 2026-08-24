# Membership credential split — make admission an authorization decision

**Status:** designed, not started · **Follows:** P6 in
[CLUSTER-PERFORMANCE-PLAN.md](CLUSTER-PERFORMANCE-PLAN.md) §6.7 ·
**Written:** 2026-08-24 against `agent/builder/p6-learner-protocol`
`7c9b46c8`

Companion to [CLUSTERING-PLAN.md](CLUSTERING-PLAN.md), which owns membership
and the join protocol, and to [SECURITY.md](SECURITY.md), which owns the
credential inventory. P6 admits a non-voting learner and refuses, on the plurx
side, everything a learner must not do. This document is the milestone that
turns those refusals into authorization rather than defence in depth.

## 1. Objective — one credential should not authorize four different things

`secret_api` is currently four credentials wearing one value:

| It authorizes | Reached through | Who needs it |
|---|---|---|
| Raft membership mutation | `POST /cluster/become_member/{raft_type}`, `POST /cluster/membership/{raft_type}`, `DELETE /cluster/membership/{raft_type}` | the coordinator, and any joining node exactly once |
| Replicated read/write | every Hiqlite client request and the leader-forwarding stream | every member, continuously |
| Cluster API management | `/cluster/metrics`, `/cluster/elect`, backup, listen/notify | every member, continuously |
| Artwork HMAC | peer artwork fetch proofs | every member, continuously |

It ships whole inside the join token, before the joining process starts, because
the joiner needs it to open its own store. So a node holding it can call
`become_member` against the leader and promote itself to a voter. Hiqlite
honours nothing else on that route (`vendor/hiqlite/src/network/management.rs`
`become_member`, whose only check is `validate_secret` from
`vendor/hiqlite/src/network/mod.rs`), and plurx is not consulted at all.

That is why P6's operator documentation says a learner is a capacity role and
not a security boundary. The role bound in the token record, the `learner_only`
startup hint, and the cluster job gate each stop a *mistake*. None of them stops
a node that already holds the credential.

**Objective:** membership mutation requires a credential a joining node never
receives, so that admitting a node stops being the same act as trusting it with
the cluster's shape.

**Non-objective:** this does not make a learner untrusted for data. A member
still reads and writes replicated state with a shared credential, and splitting
*that* is a much larger change with its own plan. What this milestone buys is
that a compromised or misbehaving member cannot change who is a voter.

## 2. Starting point — the template already exists in this repo

Per-node asymmetric authority is not new work here. The activity and internal
peer routes already use it, and this milestone is the same shape applied to a
route that currently lives in the vendored crate.

### 2.1 What `main` already provides

- `ActivitySigningKey` (`crates/plurx-core/src/cluster/membership.rs`) wraps one
  Ed25519 key pair created from a 32-byte seed, exposes only `public_key_hex`
  and `sign_hex`, and is deliberately neither `Clone` nor `Debug`.
- The seed is persisted by the migration coordinator as an owner-only file named
  by `ACTIVITY_SIGNING_KEY_FILENAME`
  (`crates/plurx-core/src/cluster/migration.rs`). Only the public half is
  replicated, into `cluster_node_activity_keys`.
- A key is immutable for a node id. Losing the private file is a fail-closed
  recovery event: an operator recovers the data directory or rejoins with a new
  node identity.
- `MembershipManager::sign_internal_peer_request` signs method, normalized
  route, the SHA-256 of the raw body, both node identities, a timestamp and a
  nonce. `authorize_internal_peer_request` verifies all of it against the
  replicated public key, enforces a time window, a replay cache, and a rate
  bucket, and finishes with a consistent read that the signer is a live member.

That last point is the property this milestone needs and the shared secret can
never have: **the proof names both ends, and the verifier decides against
replicated state rather than against a string every member holds.**

### 2.2 What is in the way

Membership mutation is not a plurx route. It is three Hiqlite routes registered
in `vendor/hiqlite/src/start.rs` under `/cluster`, and their only authentication
is `validate_secret`. Hiqlite's own startup reconciliation calls two of them on
itself (`vendor/hiqlite/src/init.rs`), which is how a joining node becomes a
learner and then, unless `learner_only` is set, a voter.

So this milestone cannot be implemented in `plurx-core` alone. It needs a
vendored change, and therefore a tenth entry in
[`vendor/hiqlite/PLURX-PATCH.md`](../vendor/hiqlite/PLURX-PATCH.md) to carry
forward across every future Hiqlite bump.

## 3. Design — a second, non-shipping credential on the membership routes

### 3.1 A membership authority key per node

Each node gains a second Ed25519 key pair beside its activity key:

- seed persisted as `membership_authority_key`, owner-only, in the data
  directory, created at activation or first boot exactly as the activity seed
  is;
- public half replicated into a new `cluster_node_membership_keys` table,
  written under the same "immutable for a node id" rule as
  `cluster_node_activity_keys`;
- **never** placed in a join token, an environment variable, a config file, or
  any response body.

A **membership-mutation proof** is an Ed25519 signature over the same canonical
message shape the internal peer routes already use: signer node id, target node
id, timestamp, nonce, method, normalized path, and the SHA-256 of the raw body.
Reusing the existing message builder is deliberate — one canonicalization means
one place to get the framing right, and the domain separation lives in the route
that is being signed for.

### 3.2 The vendored change is a hook, not a policy

Hiqlite must not learn about plurx membership. The patch adds an optional
authorizer to `NodeConfig`:

```rust
pub type MembershipAuthorizer =
    Arc<dyn Fn(&HeaderMap, &Method, &str, &[u8]) -> BoxFuture<'static, bool> + Send + Sync>;

pub struct NodeConfig {
    // …
    /// Consulted *in addition to* `validate_secret` on the three membership
    /// mutation routes. `None` keeps the upstream behaviour exactly.
    pub membership_authorizer: Option<MembershipAuthorizer>,
}
```

`become_member`, `post_membership`, and `leave_cluster` call it after
`validate_secret` and before touching Raft. `add_learner` does **not**: see §3.4.
A `None` authorizer is upstream behaviour byte for byte, which is what keeps the
patch reviewable against the next release and keeps the vendored crate's own
tests passing unchanged.

plurx supplies an authorizer that calls
`MembershipManager::authorize_membership_mutation`, which is
`authorize_internal_peer_request` with a different key table and a different
final predicate: the signer must be a **committed voter**, not merely a live
member. A learner's proof therefore verifies cryptographically and is still
refused, and the refusal is authorization.

### 3.3 The coordinator becomes the only mutator

Today a joining node promotes itself. After this milestone it cannot, because it
holds no membership key the cluster has ever seen. Promotion moves to the
coordinator, which is the node the operator is already talking to:

1. the joiner starts, and Hiqlite's reconciliation adds it as a Raft learner
   (§3.4);
2. the joiner redeems and finalizes exactly as it does today;
3. for a **voter** token, the coordinator — a committed voter — signs and sends
   `become_member` on the joiner's behalf as part of finalization;
4. for a **learner** token, nobody sends `become_member` at all, which is the
   role's definition rather than a hint the joiner is trusted to honour.

`learner_only` stops being load-bearing and becomes what its comment already
claims: a startup hint. Removing a learner and promoting a learner — neither of
which P6 implements — become ordinary coordinator-signed mutations rather than
new trust decisions.

### 3.4 `add_learner` stays on the shared secret, on purpose

A node that has never been a member has no replicated public key, so it cannot
sign anything the cluster can verify. Requiring a proof on `add_learner` would
be a chicken-and-egg failure that no key distribution scheme inside this design
resolves.

`add_learner` therefore keeps `validate_secret` alone, and the join token keeps
shipping `secret_api`. The security claim is precisely bounded, and it should be
written down in the same words wherever it is claimed:

> A join token admits a node as a **non-voting learner**. It does not authorize
> becoming a voter, promoting anything, or removing anything.

That is strictly better than today, where the token authorizes all four, and it
is honest about what it is not: a leaked token still admits an attacker's node
as a replica holding every byte of replicated state. Closing *that* needs the
read/write credential split this milestone explicitly defers.

### 3.5 Refusals

| Code | HTTP | Meaning |
|---|---|---|
| `membership_proof_missing` | 401 | The membership route was reached with a valid API secret and no per-node proof. During rollout this is the signal that a node is still on a pre-split binary. |
| `membership_proof_invalid` | 403 | The proof did not verify, was outside its time window, replayed a nonce, or named a different target. |
| `membership_mutation_forbidden` | 403 | The proof verified and the signer is not a committed voter. This is the learner case, and it is the one refusal in this design that is authorization rather than authentication. |

## 4. Migration and rollout — a mixed cluster must never wedge

This is the part that decides whether the design is shippable, because the
cluster being changed is the thing that performs the change.

The route is P6's own: **a protocol range, a heartbeat-coupled capability, and
an explicit activation.** That machinery exists and is proven, and reusing it
means this milestone introduces no second upgrade mechanism.

### 4.1 Protocol 6 and its capability

- Binaries implement `4..=6`; protocol 6 is "membership mutation carries a
  per-node proof".
- Every node's heartbeat writes a `membership_authority_v6` capability row in
  the same Raft transaction, coupled to the heartbeat's own timestamp, exactly
  as `learner_protocol_v5` is. A rolled-back node stops being ready within one
  interval.
- Publishing the public key is a precondition for the capability, so "ready"
  means the key is replicated and verifiable, not merely that the binary is new.

### 4.2 Four ordered phases

| Phase | State | Membership routes accept | Notes |
|---|---|---|---|
| 1. Deploy | active `5..=5`, mixed binaries | secret only | Every node writes its membership key and publishes the public half on first boot. Nothing else changes. |
| 2. Dual-accept | active `5..=5`, every node upgraded | proof **or** secret | The authorizer is installed and *logs* every secret-only mutation with the route and the caller's node id. This is the phase that finds the caller nobody remembered. |
| 3. Activate | active `6..=6` | proof only | Refused until every active node proves `membership_authority_v6`, with the same named-node refusal P6 uses. From here a pre-split binary cannot boot, join, or rejoin. |
| 4. Steady | active `6..=6` | proof only | `add_learner` still accepts the secret alone, permanently, by §3.4. |

Phase 2 is not optional and not a formality. A cluster that jumps from 1 to 3
discovers its unsigned callers by losing the ability to change membership, which
is also the ability to fix it.

### 4.3 The wedge cases, and why each is survivable

| Case | What happens | Why it recovers |
|---|---|---|
| A node loses its membership seed | It cannot sign. It can still vote, replicate, and serve. | Any other committed voter performs the mutation. The node is repaired by data-directory recovery or by rejoining under a new node id, as with the activity key. |
| Every voter loses its seed | Membership is frozen; the cluster keeps serving. | Deactivation back to `5..=5` is a secret-authenticated mutation, and it is the documented break-glass. Unlike P6's rollback it is always available, because nothing durable depends on protocol 6 — that is a deliberate design constraint, not an accident. |
| A joiner is admitted and the coordinator dies before promoting it | It sits as a Raft learner. | Any voter finishes the promotion from the durable token record; finalization is already idempotent. |
| Clock skew | Proofs outside the window are refused. | The existing internal-peer window and the operations doc's "synchronize clocks before cluster work" instruction both already apply. |

### 4.4 Rollback is available at every phase

Phase 3 → 2 is `deactivate`, symmetric with P6's, and it is refused by nothing,
because no durable state is created that only protocol 6 can read. Phase 2 → 1
is uninstalling the authorizer. Phase 1 → 0 is a binary downgrade; the extra
seed file and the extra table are inert to an older build.

This is the property P6 could not have — a learner's existence makes P6's
rollback unavailable — and it is worth the constraint it imposes on the design.

## 5. Evidence

Every clause below needs a separate process, so it belongs in
`crates/plurx-cluster-check` beside the P6 learner scenario rather than in a
unit test:

- a learner's membership proof verifies and is refused with
  `membership_mutation_forbidden`, and the learner is still not a voter
  afterwards — the assertion P6 cannot make today;
- a voter's proof over a *different* route or body is refused, so the signature
  binds what it claims to bind;
- a captured voter proof replayed against a second node is refused, because the
  message names its target;
- with the authorizer installed and the range still `5..=5`, a secret-only
  mutation succeeds and is logged — phase 2's whole contract;
- with the range on `6..=6`, the same secret-only mutation is refused;
- activation is refused, naming the node, while one process runs a binary that
  does not write `membership_authority_v6`, using the same emulation flag P6's
  scenario uses;
- a voter whose seed file is removed can still be promoted and removed by
  another voter.

The vendored patch needs its own evidence in `vendor/hiqlite`: with
`membership_authorizer: None`, the three routes behave exactly as upstream.

## 6. Milestones

| # | Lands | Invariant it proves |
|---|---|---|
| 1 | The per-node membership key, its seed file, its replicated table, and its signing/verifying pair in `plurx-core`. No route consults it. | A key is per node, immutable, never leaves its data directory, and verifies against replicated state. |
| 2 | The vendored `membership_authorizer` hook, `None` by default, plus the `PLURX-PATCH.md` entry. | Upstream behaviour is unchanged when the hook is absent. |
| 3 | plurx installs the authorizer in dual-accept mode, with the secret-only mutation log. | A mixed cluster changes membership exactly as before, and every unsigned caller is named. |
| 4 | Protocol 6, its heartbeat capability, activation and deactivation. | Proof-only membership mutation, and a learner refused by authorization. |
| 5 | Coordinator-driven promotion; `learner_only` demoted to a hint in fact as well as in its comment. | A joining node never mutates membership, including its own. |
