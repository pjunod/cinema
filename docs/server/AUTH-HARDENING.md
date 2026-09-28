# Auth hardening — queue the fence instead of refusing it, and bound what a stranger can try

**Status:** in execution — the sign-in expiry **option** (§3.4.1), which supersedes the M4 decision, is open in draft [PR #486](http://192.168.4.7:3000/noirr/plurx/pulls/486); M1–M3 and the M4 decision merged via [PR #433](http://192.168.4.7:3000/noirr/plurx/pulls/433) · **Executes:** C7 and C8 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
(assessment correction 1 and rows C7, F-core-8, C8, F-core-9 in
[ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md))
· **Written:** 2026-09-20 against `main` @ `88a3957a`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Read §2 first — it explains the two-phase revocation fence in the words of
the code that implements it, because the first draft of the review tried to
remove it and the assessment refused. The workboard's one-plan/one-PR rule
supersedes the original milestone-sized PR language: M1–M3 are logical commits
in PR #433, and M4 records the delegated product decision in the same PR.
`?token=` narrowing is not built here — §4 says why. Every `file:line` is from
`88a3957a`; re-verify by function name.

**If a step seems to require deleting a token without the Begin/End
peer fanout, answering an admin recovery read from the cache while a
revocation is in flight, extending the cache-only proof to ordinary auth,
or refusing `?token=` on a media route, stop and flag it.**

**Correction to the review:** none. The assessment's correction 1 is the
governing text and this plan is written to it. One addition the review
did not make: the web client aborts its logout request after 5 s
(`web/core/auth.js:41`, `setTimeout(()=>controller.abort(),5000)`) and
clears its local session either way, so any admission wait longer than
~3 s is invisible to the browser. M1's wait is sized below that.

---

## 1. Objective

1. Two people signing out at once on a cluster both get their tokens
   revoked and both get `{"ok":true}`; the second waits behind the first
   instead of receiving 503 and clearing a bearer the server still honours.
2. The fence that makes the cache-only admin proof safe — Begin on every
   committed member, the Store mutation under an exact claim, End on every
   member — is unchanged in order, scope and ambiguity handling.
3. A password guesser against one account from one address is slowed to a
   crawl after a handful of failures; a guesser cannot use that slowdown
   to lock the real user out; a reverse proxy does not make every client
   look like one address unless the operator says it is a proxy.
4. A user can see the devices holding tokens for their account and revoke
   one; an admin can do it for any user. Revoking goes through the same
   fence.
5. Token idle expiry is decided with its consequences written down (offline
   packages, TV boxes that sleep for weeks, the recovery proofs) before a
   number is chosen.

## 2. Contract today

Re-verify at build time.

### 2.1 The fence, as the code states it

`crates/plurxd/src/http/auth.rs:94-112` `logout`:

```rust
let hash = auth::hash_token(&token);
let proof_revocation = ClusterCacheRevocation::begin_digest(&state, &hash).await?;
if !state.store.delete_token_with_cache_admin_claim(&hash, proof_revocation.mutation_claim()).await? {
    return Err(ApiError::ServiceUnavailable(
        "logout lost its cache-revocation exclusion; retry the request".into()));
}
proof_revocation.finish(&state).await?;
```

The same bracket wraps user mutations: `users.rs:89`
(`begin_user` when a user loses admin or changes password) and `:183`
(`begin_user` before `delete_user_preserving_admin`).

Why it exists — `extract.rs:24-54`: `CacheOnlyAdminUser` answers the two
cluster-recovery reads **without asking the Store**, from a proof
published at the last ordinary authentication, for
`CACHE_ONLY_ADMIN_PROOF_TTL = 5 min` (`:56`), capped at
`MAX_CACHE_ONLY_ADMIN_PROOFS = 64` (`:57`). That is the property an
operator needs when the Store is wedged. It is also exactly the property
that would let a revoked administrator keep reading cluster state on a
peer for five minutes — unless every peer is told to drop its proof
*before* the token disappears and told again *after*, so that no
concurrent ordinary authentication can re-publish a proof from a stale
read in between. That is the two phases:

- **Begin** (`internal_auth_revocation.rs:285-363`): take the process-wide
  operation gate; commit a replicated membership exclusion claim
  (`MEMBERSHIP_EXCLUSION_DURATION = 15 s`, `:28`); wait until this node's
  own applied log shows the claim (`LOCAL_CLAIM_APPLY_TIMEOUT = 1.5 s`,
  `:31`); fan Begin out to every committed member (`FANOUT_TIMEOUT = 2 s`,
  `FANOUT_CONCURRENCY = 8`, `:24-25`) and repeat until two consecutive
  roster reads are identical with every member acknowledged
  (`StableBeginRoster`, `:55-63`; at most `MAX_STABLE_ROSTER_PASSES`
  passes, `:26`). Only then `local.arm_ambiguity()` (`:350`).
- **Mutation** under the exact claim: `delete_token_with_cache_admin_claim`
  is a no-op if the claim is no longer live (`store/mod.rs:2035-2037`), so
  a proposal that reaches Raft after the exclusion lapsed cannot cross it.
- **End** (`:368-`): read the roster again, union it with the Begin roster
  so a member added mid-operation is invalidated too, fan End out, and
  only then `complete()` the local guard.

Guard drop (`extract.rs:134-151`) bumps the generation and, when armed,
keeps the whole cache **closed for one proof lifetime**
(`local_ambiguity_expires_at`), because a Store write that returned without
a definite outcome may still commit after the future is gone (`:75-79`).
"Best-effort notify after a plain DELETE" deletes this ordering and this
ambiguity handling, and was withdrawn (review §0, assessment correction 1).
The plan keeps every line of it.

### 2.2 The contention point

`extract.rs:218-228`:

```rust
/// Admit at most one cache-admin mutation coordinator in this process.
/// Callers use a fail-fast acquire so a request burst cannot accumulate an
/// unbounded queue of futures, claims, or detached cleanup tasks.
pub(crate) fn try_acquire_revocation_operation(&self)
    -> Result<tokio::sync::OwnedMutexGuard<()>, &'static str> {
    self.revocation_operation_gate.clone().try_lock_owned()
        .map_err(|_| "cache-admin revocation already in progress")
}
```

Called at `internal_auth_revocation.rs:253, 266, 277` (activation, logout,
user mutation). A second concurrent logout gets `propagation_error()`
(`:625-631`: 503 `admin_revocation_propagation_failed`). The browser then
clears its session with "The server could not confirm revocation"
(`auth.js:52-54`); a native client does the same. The token row survives.

The comment's reason for `try_lock` is real: an unbounded queue of
coordinators, each holding a replicated claim and a fanout, is worse than
a 503. M1 must keep the queue bounded.

### 2.3 Login, tokens, the query credential

`auth.rs:44-91` `login`: password size cap; timing-uniform verify against
`DUMMY_HASH` for unknown users; Argon2 work bounded by
`PASSWORD_HASH_WORKERS = 2`, `PASSWORD_HASH_WAITERS = 16`,
`PASSWORD_HASH_ADMISSION_WAIT = 2 s` (`:21-23`; 503 beyond). No per-account
or per-address failure accounting anywhere in `crates/`.

`store/sqlite/mod.rs:72-78`:

```sql
CREATE TABLE tokens (
    token_hash   TEXT PRIMARY KEY,
    user_id      INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    device       TEXT,
    created_at   INTEGER NOT NULL DEFAULT (unixepoch()),
    last_seen_at INTEGER NOT NULL DEFAULT (unixepoch())
) STRICT;
```

No `expires_at`. `last_seen_at` is touched at most once a minute
(`sqlite/users.rs:288-295`; the hiqlite twin suppresses the write inside a
60 s window, [CLUSTER-PERFORMANCE-PLAN.md](../cluster/CLUSTER-PERFORMANCE-PLAN.md)
§2.2). `device` is whatever the client sent at login.

`extract.rs:547-577` `token_from_parts`: `Authorization: Bearer`, then
`X-Api-Key`, then `?token=` — on every route, because `<img>` and `<video>`
cannot set headers (`:4-6`). `?token=` is also how the web client builds
every poster and media URL (`core/api.js`), how Android's "Open in…" hands
a URL to an external player (D5), and what the Plex façade's `X-Plex-Token`
maps onto. The access log already omits the whole query
(`http/mod.rs:981-983`).

Peer address: `main.rs:2424` installs `ConnectInfo<SocketAddr>`;
`http/network.rs:57-85` reads `Forwarded`/`X-Forwarded-For`/`X-Real-IP`
**unconditionally** for the coarse `/24` network prior — acceptable for a
hint, not for a throttle key an attacker can choose. There is no
trusted-proxy configuration (`config.rs:52-57` `ServerConfig { name, bind }`).

## 3. Change

### 3.1 M1 — bounded admission to the fence

Replace `try_acquire_revocation_operation` with:

```rust
const REVOCATION_WAITERS: usize = 8;                       // queue depth, process-wide
const REVOCATION_ADMISSION_WAIT: Duration = Duration::from_millis(2_500);

pub(crate) async fn acquire_revocation_operation(&self)
    -> Result<RevocationAdmission, &'static str>
{
    let waiting = Arc::clone(&self.revocation_waiters)             // Semaphore(REVOCATION_WAITERS)
        .try_acquire_owned()
        .map_err(|_| "cache-admin revocation queue is full")?;
    let guard = tokio::time::timeout(
        REVOCATION_ADMISSION_WAIT,
        self.revocation_operation_gate.clone().lock_owned(),
    )
    .await
    .map_err(|_| "cache-admin revocation admission timed out")?;
    Ok(RevocationAdmission { guard, _waiting: waiting })
}
```

**Correction (2026-09-22, review #3378 P2).** The sketch above holds
`_waiting` for the operation's whole lifetime, which spends one of the eight
permits on the caller that is no longer waiting and leaves a queue seven
deep. The implementation drops the permit as soon as the mutex is acquired —
the mutex is the single active slot — so the depth is the eight waiters this
section promises. Nothing else in the sketch changed.

Both failures still map to `propagation_error()` — the 503 code and the
client behaviour on it are unchanged; what changes is how often it
happens. The three call sites (`:253, 266, 277`) become `.await`s. The
`OwnedMutexGuard` still travels into `CacheAdminMembershipExclusion`
(`:70`) and is released exactly where it is today.

Why these numbers. A full revocation on the three-node lab is two fanouts
of ≤2 s plus the local apply wait — typically well under a second, worst
case ~5.5 s. A waiter that arrives while one is in flight will usually be
admitted in under a second; 2.5 s covers the common case and stays under
the browser's 5 s abort with margin for the operation itself. Eight
waiters is more concurrent sign-outs than a household produces and small
enough that the worst-case queue (8 × 5.5 s) cannot outlive the 15 s
membership exclusion twice over — each waiter takes its *own* claim when
admitted, so the bound is on futures held, not on claim lifetime.

**The fence is unchanged.** `begin`, `finish`, `arm_ambiguity`, guard drop,
`StableBeginRoster`, `MEMBERSHIP_EXCLUSION_DURATION`: no diff. A test pins
that `begin_digest`'s body after admission is byte-identical to today's by
asserting the same sequence of peer requests in the existing mock fanout
tests (`internal_auth_revocation.rs:770-800` already compares phase order).

**Metric.** `plurx_auth_revocations_total{outcome="complete|queue_full|
admission_timeout|propagation_failed"}` (four values), rendered beside the
cluster activity metrics in `http/system.rs`. `queue_full` and
`admission_timeout` at zero on the fleet is the expected state.

### 3.2 M2 — bounded login backoff with proxy trust and lockout resistance

**Where.** A `LoginThrottle` in `AppState`, consulted at the top of
`login` (after `validate_password_size`, before the Store read) and
updated after the verdict.

**Keys.** Two independent maps, each bounded:

- `(username_lowercase, client_ip)` — the pair the review names. Bounded
  to `LOGIN_THROTTLE_ENTRIES = 4_096` with LRU eviction and a
  `LOGIN_THROTTLE_IDLE = 15 min` expiry.
- `client_ip` alone — bounded the same way; it is what stops a guesser
  who rotates usernames.

There is deliberately **no** `username`-alone key: that is the map an
attacker fills from a botnet to lock the real user out, which F-core-9
names. With only per-`(user, ip)` and per-`ip` keys, the legitimate user
on their own address is never slowed by someone else's failures.

**Policy.** Failures per key are counted in a sliding
`LOGIN_THROTTLE_WINDOW = 10 min`. After `LOGIN_FREE_FAILURES = 5`, each
further attempt from that key must wait
`min(2^(n-5) × 1 s, LOGIN_BACKOFF_CAP = 60 s)` since the last failure, or
it is refused **before the Store read and before the Argon2 work** with
`429 {"code":"login_backoff","retry_after_seconds":N}` and `Retry-After`.
A refused attempt does not count as a failure (so hammering does not
lengthen the wait), a success clears both keys for that pair. The cap of
60 s means the worst a hostile neighbour on the same NAT can do to the
real user is one minute per attempt, and only after five failures from
that shared address — a lockout resistance stated as a number, not a hope.
Refusing before the verify keeps the timing-uniform property of the
success/failure paths intact (`auth.rs:52`), because the refusal is a
different response class entirely.

**Client IP.** `fn client_ip(parts, trusted_proxies) -> IpAddr`:

- the socket peer from `ConnectInfo<SocketAddr>` **unless** that peer is
  in `server.trusted_proxies`, in which case the rightmost
  `X-Forwarded-For` entry that is *not* itself a trusted proxy
  (RFC 7239 §7.3's reasoning: only the hop the proxy appended can be
  trusted);
- `server.trusted_proxies: Vec<IpNet>` is a new **node config** key
  (`config.toml [server]`, env `PLURX_TRUSTED_PROXIES`), default empty.
  It is a node's network position — a Caddy in front of `media1` is not in
  front of `lab3` — so it is not a replicated setting and does not belong
  in Settings → Developer. With the default, a proxy makes all clients one
  address and the per-`ip` map degrades to a global throttle on that
  proxy's address; the `Settings → System` readiness list gets an
  *advisory* line "login throttle: all clients share one address; set
  server.trusted_proxies" when >50 % of the last hour's logins came from
  one address that also sent `X-Forwarded-For`. Advisory, never blocking.

`http/network.rs`'s unconditional header trust is **not** changed by this
PR (it feeds a coarse prior, and changing it is D-series client work).

**Metrics.** `plurx_login_attempts_total{outcome="ok|bad_credentials|
backoff|capacity"}` (four values). No username or IP labels, ever.

**Client impact.** A 429 with `Retry-After` on login is new. The web
client shows the server's message; native clients show their generic
login failure until they learn the code — no client blocks on this PR.

### 3.3 M3 — devices list and per-token revocation

**Store.** `list_tokens_for_user(user_id) -> Vec<TokenSummary { token_hash_prefix: String /* 8 hex */, device: Option<String>, created_at, last_seen_at }>`
and `delete_token_by_prefix_for_user(user_id, prefix, claim)` on both
backends (`sqlite/users.rs`, `hiqlite.rs:3934` neighbourhood), the delete
carrying the cache-admin claim exactly as
`delete_token_with_cache_admin_claim` does. The prefix is 8 hex of the
SHA-256 — enough to pick a row, useless for authenticating; the full hash
never leaves the Store.

**Routes.** `GET /api/v1/me/devices` (`AuthUser`) and
`DELETE /api/v1/me/devices/{prefix}`; `GET/DELETE
/api/v1/users/{id}/devices[/{prefix}]` (`AdminUser`). A delete runs
`ClusterCacheRevocation::begin_digest`-shaped bracketing — `begin_user` is
the right target when the caller cannot name the digest, so revocation of
another device uses `begin_user(user_id)` (broader, safe) and the delete
under the claim. Deleting the *current* token is a logout and is refused
here with a pointer to `/auth/logout` (so the client's own session
handling stays in one place).

Both routes are `json_long` in
[HTTP-LISTENER-TIMEOUTS-AND-ASSET-DELIVERY.md](HTTP-LISTENER-TIMEOUTS-AND-ASSET-DELIVERY.md)
§3.1's grouping, because they run the fence. [API.md](../API.md) gains the
four rows; the Settings → Account panel in the web client is the first
consumer, as a separate small PR.

### 3.4 M4 — token idle expiry, as a decision with its consequences

What the number would do, each read from the code:

| Consequence | Where | Why it matters |
|---|---|---|
| A TV box that sleeps past the window signs the household out | `last_seen_at` touched once a minute while in use (`users.rs:288-295`); a box idle for the whole window has no touch | The living-room Apple TV is used weekly in some households, monthly in others |
| Offline packages keep playing, but their next lease renewal fails | `offline::put_lease` (`http/mod.rs:362`) is `AuthUser`; the child media routes use the package capability, not the bearer | A phone that spent three weeks abroad comes home to a signed-out app with downloaded films it can still play but cannot renew |
| The cache-only recovery proof outlives an expired token by up to 5 min | `extract.rs:56`; the proof is published at authentication and never renewed by cache reads | Expiry must run the same fence as logout (`begin_digest` + delete under claim + `finish`), or an expired admin keeps the recovery reads for the TTL |
| Bulk expiry is a fanout per token, not one statement | one Begin/End per revocation operation; the queue from M1 bounds concurrency | A nightly sweep that expires 40 tokens is 40 fences; it must be a leader-singleton job under the job lease, paced (one per few seconds), never a `DELETE … WHERE last_seen_at < ?` |
| Plex façade clients hold `X-Plex-Token` forever | `plex.rs` maps it onto the same tokens | Kodi/PKC has no re-login UI; an expired token is a support call |

**Decision (2026-09-21): keep login tokens non-expiring for now.** The delegated
choice favours recoverability over an arbitrary lifetime: Plex/Kodi has no
reliable re-login flow, infrequently used TV clients can sleep for months, and
offline clients may return without network access to repair a session. M3's
visible inventory and fenced per-device revocation are the immediate safety
control. Automatic expiry remains follow-up work only after every client has a
tested refresh or re-login path; no schema, sweep, setting, or hidden gate is
introduced by this plan.

#### 3.4.1 Superseded 2026-09-24 — expiry as an option, on by default

Paul (2026-09-23): *"can't we add an option to make them expire or not
expire? Then we can have both and just make it default to expiring."* That
replaces the 2026-09-21 decision above; the consequence table still stands and
is what the choices below answer. Built in [PR #486](http://192.168.4.7:3000/noirr/plurx/pulls/486). Every item marked
**Decision** was taken on Paul's behalf and is his to overturn.

- **Decision — the option.** A server-wide "Sign-ins expire" switch
  (`auth.token_expiry_enabled`, absent = **on**) and window
  (`auth.token_idle_days`, default **90**, 1–3650), measured as a **sliding**
  window from the token's `last_seen_at`. A device in regular use is never
  signed out; one idle for the whole window is. Off is the old non-expiring
  behaviour.
- **Decision — no mass sign-out.** `auth.token_expiry_since` is seeded once at
  startup with `get_or_init_setting` (the first committed value wins on every
  voter) and rewritten, in the same `put_settings` commit, whenever an
  administrator switches expiry from off to on. A token's window is measured
  from `max(last_seen_at, since)`, so neither the upgrade that makes the
  default take effect nor turning the option on signs anyone out on the spot.
  An absent `since` means nothing can expire.
- **Decision — no new writes.** Expiry reads the `last_seen_at` that
  authentication already refreshes at most once a minute through the
  existing coalescing (SQLite's `last_seen_at < unixepoch() - 60` predicate;
  hiqlite's `ActivityRefreshGate` plus the same predicate,
  [CLUSTER-PERFORMANCE-PLAN.md](../cluster/CLUSTER-PERFORMANCE-PLAN.md) §2.2),
  so the option adds **zero** replicated writes, and an expired token adds
  none (it is not touched — touching would revive it). Widening the touch to
  once a day was considered and not taken: expiry only needs day resolution,
  but the same column is the devices list's "last used", and the touch
  cadence belongs to K-03's write-rate work rather than to this option.
- **Decision — lazy, no sweep, no deletion.** The policy is read in the same
  statement / consistent read as the token row on every authentication
  (hiqlite binds the three keys as `$1..$3` and the digest as `$4`, in
  first-appearance order), so every node judges by the committed policy with
  no extra round trip. Nothing is swept: the row stays, listed as `expired`
  in the devices list and revocable through the M3 fence. That sidesteps the
  table's "bulk expiry is a fence per token" row entirely.
- **The cache-only recovery proof cannot outlive expiry** (the table's third
  row). A proof is published only by a successful ordinary authentication;
  that authentication refreshes `last_seen_at` whenever it is more than 60 s
  old, and the minimum window is one day, so a token that just published a
  proof is at least ~a day from expiry — far beyond the 5-minute proof TTL.
  An expired token is also answered by dropping the local proof for its
  digest, exactly as an unknown token is.
- **Decision — revival on off.** Turning the option off makes every login
  token valid again until revoked, including ones it had signed out. That is
  what "off = today's behaviour" means; revoke a device explicitly to end it
  for good.
- **Decision — shortening applies at once.** Only switching expiry *on*
  restarts the clock. Lowering the window from 90 to 30 days signs out, on
  their next request, devices already idle longer than 30 days — the
  administrator's explicit choice.
- **Decision — where it lives.** Settings → **Users**, a "Sign-ins" card
  beside the accounts it governs, with one line of copy — a real product
  option, not a Developer experiment, per Paul. No readiness rows and no gate.
  This supersedes the §4 guardrail line that put a future window in
  Settings → Developer.
- **Decision — the error contract.** An expired token is a **401** (so every
  client that predates the code still signs out) with
  `{"code":"session_expired","idle_days":N,"message":"Signed out after N days
  of inactivity. Sign in again to continue."}`. Unknown and revoked tokens
  keep the bare `{"error":"authentication required"}`.
- **Scope.** Every login-token path honours it: `AuthUser` (Bearer,
  `X-Api-Key`, `?token=` media URLs), and — because `user_for_token` is now a
  provided method over `authenticate_token` — the Plex façade's
  `X-Plex-Token` and the recovery-read Store fallback. API keys (`plx_`) are
  not login tokens and do not expire. Offline packages keep playing (their
  child routes use the package capability); their next lease renewal needs a
  sign-in, as the table says.
- **Clients.** What each did on 401 before this, and what changed:
  web `core/api.js:33-40` ended the session with "Your session ended on the
  server." and `core/auth.js:127-128` cleared the notice on first paint (boot
  paints the login screen twice for a dead stored credential, so the reason
  was lost) — now the server's sentence is the notice and it survives until a
  sign-in succeeds. Android signed out only in launch validation
  (`AppViewModel.kt` `validateSavedSession`, 401/403 → `InvalidToken`, no
  message) and left a mid-session Home 401 as an error string — now
  `session_expired` becomes `Expired(message)` at launch and signs out to the
  login screen with it from a Home load. Apple kept every 401 as
  `.http(401)` (`PlurxAPI.check`) and signed out in `bootstrap`,
  `retrySavedSession` and `noteAuthFailure` with no reason — the status shape
  is unchanged and the sentence now rides `Session` to those three sign-outs.
- **Devices list.** No client had one; the web gains a per-account Devices
  drawer in Settings → Users showing last use and "Signs out in N days if
  unused" / "Signed out — unused too long" / "Doesn't expire", from the
  devices routes' new `expires_at` and `expired` fields.

## 4. Guardrails (non-goals)

- **The fence stays** (correction 1; C7; F-core-8). M1 changes one function
  — admission — and `begin`/`finish`/guard-drop are diff-free; the
  phase-order tests pin it.
- **The queue stays bounded** (the reason `extract.rs:219-220` gives for
  `try_lock`). Eight waiters, 2.5 s, 503 beyond; the metric shows when
  either bound is hit.
- **No ordinary-auth cache** (§0 of the review; correction 1 "extending
  that cache to ordinary auth expands the impact"). Nothing here touches
  `AuthUser::from_request_parts`; the Store is still asked on every
  authenticated request. Fewer consistent reads per request is S1's work.
- **No `?token=` narrowing** (C8 "narrowing `?token=` needs client
  migration first"; F-core-9 "query auth exists for media consumers").
  The web client's `<img src>`, Android's external-player hand-off and
  the Plex façade all depend on it. The narrowing plan is: (1) M3's
  devices list and D5's server-minted short-lived file grant land; (2)
  each client stops sending `?token=` on non-media routes; (3) a metric
  `plurx_query_credential_total{route_class}` reaches zero on the
  fleet for JSON routes; (4) then `token_from_parts` refuses the query
  form on JSON routes. Steps 2–4 are not in this plan.
- **No username-only throttle key** (F-core-9 "protection against …
  intentional account lockout"). Stated in §3.2 with the worst case.
- **Proxy headers are trusted only from configured proxies** (F-core-9
  "trusted-proxy handling"). Default empty means default *distrust*.
- **Timing uniformity kept** (`auth.rs:52`). The backoff refusal precedes
  the verify and is a distinct response; the ok/bad paths still both hash.
- **Token expiry is a decision, not a default** (C8; F-core-9 "not
  unexplained 90-day defaults"). §3.4 and §7.1. *Superseded 2026-09-24 by
  Paul's request (§3.4.1): expiry is now an option that defaults on, and the
  90 days is explained there.*
- **No feature gate.** M1–M3 need none. If M4 ships, its window is a
  replicated setting `auth.token_idle_days` surfaced in Settings →
  Developer with the advisory readiness line "devices list deployed to
  all clients", per the standing rule. *Superseded 2026-09-24 (§3.4.1): it
  shipped as a product option in Settings → Users, with no readiness rows and
  no gate.*

## 5. Milestones

### 5.1 M1 — bounded admission (`server/revocation-admission`)

1. `acquire_revocation_operation`, the waiter semaphore, three call-site
   awaits, the metric.
2. Tests (`extract.rs` / `internal_auth_revocation.rs` test modules):
   `two_concurrent_logouts_both_succeed` (mock peers; assert two
   `{"ok":true}` and two deleted rows);
   `the_ninth_waiter_is_refused_with_the_existing_code`;
   `admission_times_out_under_a_stuck_operation_and_leaves_no_claim`
   (hold the gate for 10 s; assert 503 within 2.6 s and no membership
   exclusion row from the refused waiter);
   `admitted_operations_run_the_unchanged_phase_sequence` (extend the
   existing phase-order assertion to a queued second operation).
3. `cargo test -p plurxd http::extract http::internal_auth_revocation
   http::auth` green.

Acceptance: on the three-node lab (`lab1`–`lab3`), two browsers signed in
as different users click Sign out within one second of each other; both
show "Sign-out was confirmed by the server."; `plurx_auth_revocations_total{outcome="complete"}`
rises by 2 and the other three outcomes stay at 0.

### 5.2 M2 — login backoff (`server/login-backoff`)

1. `LoginThrottle`, `client_ip`, `server.trusted_proxies`, the 429 path,
   the metric, the advisory readiness line, the two config docs
   ([OPERATIONS.md](../OPERATIONS.md) config table, [SECURITY.md](../SECURITY.md)
   §"Transport").
2. Tests: `five_failures_are_free_the_sixth_waits`;
   `a_refused_attempt_does_not_extend_the_wait`;
   `a_success_clears_the_pair`; `the_cap_is_sixty_seconds`;
   `another_address_is_not_slowed_by_this_ones_failures` (lockout
   resistance); `forwarded_for_is_ignored_from_an_untrusted_peer`;
   `forwarded_for_is_honoured_from_a_trusted_proxy_rightmost_untrusted_hop`;
   `the_maps_are_bounded` (5,000 distinct keys → 4,096 entries);
   `backoff_refusals_do_not_touch_the_store_or_the_hash_workers`.

Acceptance: `cargo test -p plurxd http::auth::throttle` green; against
`lab1`, `for i in $(seq 8); do curl -s -o /dev/null -w '%{http_code}\n'
-X POST -H 'content-type: application/json' -d '{"username":"paul","password":"wrong"}'
http://10.42.1.11:32400/api/v1/auth/login; done` prints five `401` then
`429`s with a `Retry-After`; a correct login from a *different* host
succeeds immediately during the backoff.

### 5.3 M3 — devices (`server/devices`)

1. Store methods on both backends, four routes, API.md rows.
2. Tests: `devices_list_shows_prefix_device_and_times_never_the_hash`;
   `revoking_another_device_runs_the_fence_and_the_peer_proof_is_dropped`
   (mock peers observe Begin/End); `revoking_the_current_token_is_refused`;
   `an_admin_can_revoke_for_another_user`; the `placeholder_census` and
   `cluster_auth` scope include the new hiqlite SQL (`validation/points.toml`
   row added, `validation/ci_scope.py` run — S10's lesson).

Acceptance: `cargo test -p plurxd http::users::devices` and
`cargo test -p plurx-core store::` green; `python3 validation/ci_scope.py`
lists the touched store files under `cluster_auth`.

### 5.4 M4 — token idle expiry decision

Decision complete: retain non-expiring tokens. A future expiry proposal must
first deliver and test client recovery semantics, then add the append-only
schema, paced leader-singleton fence sweep, advisory Developer readiness, and
client-facing expiry reason together. M4 intentionally changes no runtime
behaviour in this PR.

**Superseded 2026-09-24 — M4 option ([PR #486](http://192.168.4.7:3000/noirr/plurx/pulls/486)).** Built as §3.4.1: no schema
(three settings rows), no sweep (lazy judgement), no Developer readiness (a
Settings → Users option, per Paul), and the client-facing reason
(`session_expired`) handled by every client.

1. `TokenIdlePolicy` in `plurx_core::auth`; `UserStore::authenticate_token`
   on SQLite and hiqlite with the policy read in the token's own read;
   `user_for_token` as a provided method over it; `AuthUser`'s typed 401;
   the once-only `start_token_expiry_clock` at startup; the settings fields;
   the devices `expires_at`/`expired`; web, Android and Apple handling.
2. Tests: `auth::tests::*` (policy: default on, off, absent clock, bounds,
   sliding and `since`);
   `store::sqlite::users::tests::sign_in_expiry_is_a_sliding_idle_window_that_never_starts_before_it_took_effect`
   and `…::sign_in_expiry_keeps_activity_writes_coalesced` (50 in-window
   requests change one row, 50 expired requests change none);
   `store_contract::sign_in_expiry_is_judged_on_every_voter_against_the_replicated_policy`
   (three voters; 30 in-window requests append one Raft entry, 30 expired
   requests append none; off never expires; revocation);
   `http::tests::an_idle_sign_in_is_refused_as_session_expired_and_the_devices_list_says_when`,
   `…::sign_in_expiry_settings_round_trip_and_enabling_signs_nobody_out`,
   `…::the_sign_in_expiry_clock_is_started_once`;
   `tests/web/session-expiry.test.js`; Android
   `AppViewModelTest.idleExpiredSignInCarriesTheServersReasonToTheLoginScreen`,
   `…onlyAnUnauthorizedSessionExpiredCountsAsAnIdleExpiry`,
   `…anotherExplainedUnauthorizedStaysAnOrdinarySignOut`; Apple
   `testAnIdleExpiredSignInStillSignsOutAndKeepsItsReasonForTheLoginScreen`.

Acceptance (post-merge, device evidence — §6.4).

## 6. Verification and rollout

### 6.1 Lanes

Focused `cargo test -p plurxd` per milestone; `make unit` before un-WIP;
`make validate-staged` before every push. M3 additionally runs
`python3 -m unittest tests.operations` for the points/scope tests.

### 6.2 Rollout

M1 to the three-node lab first (it is the only place the contention
exists), acceptance as above, then `media1`. M2 to `lab1` for a day —
watch `plurx_login_attempts_total{outcome="backoff"}` stays at zero under
normal household use — then the fleet. M3 anywhere. Nothing changes
schema until M4; rollback is the previous `sha-` image.

### 6.3 What only devices can prove — GPT prompt

```text
Cluster lab1–lab3 running PR <M1 number>. Two phones (iPhone, Android)
signed in as two different users, plus the web app on a laptop as a third.
1. Tap Sign out on both phones and click Sign out on the laptop within two
   seconds of each other. Report each device's message verbatim.
2. From the laptop: curl -s http://10.42.1.11:32400/metrics | grep plurx_auth_revocations_total
   and paste the four lines.
3. Sign in again on the iPhone only. On the laptop, sign in as an admin
   and open Settings → Cluster; it must load. Then from the iPhone tap
   Sign out. Within five seconds, reload Settings → Cluster on the laptop
   as the admin: it must still load (a different user's revocation must
   not fence the admin's proof for longer than the operation).
Report anything that did not match, with the time of day for log lookup.
```

### 6.4 M4 option — what only devices can prove — GPT prompt

Two facts shape this prompt. The web picker offers 7, 30, 90, 180 and 365
days only (`users-admin.js` `SIGN_IN_DAYS`), so a one-day window has to be set
through the API. And a device's window runs from the **later** of its last use
and `auth.token_expiry_since` (the build's first start, or the last off → on
switch; `plurx_core::auth` `TokenIdlePolicy`), so ageing `last_seen_at` alone
expires nothing on a build that has run for less than the window — the clock
has to be moved back as well.

```text
Server running the build from the sign-in expiry PR (or later), deployed with
the ansible playbooks. Use a throwaway account, never Paul's. You need an admin
token (<admin>), the throwaway's username (<throwaway>) and a shell on a node:
sqlite3 on the node's plurx.db for a single-node server, or the hiqlite client
on the leader for a cluster (run every SQL statement below there).
CAUTION: while the window is 1 day and the clock is moved back (steps 3-6),
ANY account's device that has been unused for more than a day and makes a
request is signed out. Prefer a standalone lab server nobody else uses; on the
shared fleet, ask Paul first, keep steps 3-6 to a few minutes, and never skip
step 8.

1. Record the current clock and settings:
     curl -s http://<node>:32400/api/v1/settings -H "Authorization: Bearer <admin>" \
       | jq '{auth_token_expiry,auth_token_idle_days,auth_token_expiry_since}'
   Expect auth_token_expiry true, auth_token_idle_days 90, and a unix time for
   auth_token_expiry_since; write that time down as S0.
2. Web, as an admin: Settings -> Users. Confirm a "Sign-ins" card with
   "Sign-ins expire" ON and "Sign out after" = 90 days. Sign the throwaway
   account in on an iPhone, an Apple TV, an Android TV and a second web
   browser, then click Devices on its row: each of the four rows must say
   "Signs out in 90 days if unused (<date>)" or 89. Screenshot it. Then leave
   all four devices alone (do not open them) until step 5.
3. Age the throwaway's devices AND the clock two days back:
     UPDATE tokens SET last_seen_at = unixepoch() - 2*86400
       WHERE user_id = (SELECT id FROM users WHERE username = '<throwaway>');
     UPDATE settings SET value = CAST(unixepoch() - 2*86400 AS TEXT)
       WHERE key = 'auth.token_expiry_since';
   Then set the window to one day (the picker cannot):
     curl -s -X PUT http://<node>:32400/api/v1/settings -H "Authorization: Bearer <admin>" \
       -H 'Content-Type: application/json' -d '{"auth_token_idle_days":1}'
   The GET from step 1 must now show auth_token_idle_days 1, and
   auth_token_expiry_since about two days ago.
4. Reload Settings -> Users as the admin (the admin's own sign-in was just
   used, so it stays valid). "Sign out after" now shows "1 days" (the picker
   adds the current value). Open Devices on the throwaway's row: all four rows
   must say "Signed out - unused too long". Screenshot it.
5. Now open the app on each of the four devices - a cold launch, and an app
   that was already open brought to the foreground and then used (open Home).
   Each must land on its sign-in screen reading
   "Signed out after 1 day of inactivity. Sign in again to continue."
   Report the exact text per device (iPhone, Apple TV, Android TV, web), and
   say which of cold launch / foreground produced it.
6. Back in the Devices drawer, press Sign out on one of the four rows; that
   row must disappear.
7. Off, then on: sign the throwaway in again on the Android TV. Switch
   "Sign-ins expire" OFF in Settings -> Users and Save; age that device again
   (the tokens UPDATE from step 3) and use it: it must keep working. Switch
   "Sign-ins expire" back ON and Save; use it again: it must STILL keep
   working, because switching on restarts every device's clock
   (auth_token_expiry_since in the step 1 GET is now the time you saved).
8. Restore: set "Sign out after" to 90 days in the picker and Save, then put
   the clock back where the deploy left it:
     UPDATE settings SET value = '<S0>' WHERE key = 'auth.token_expiry_since';
   Paste the step 1 curl from two different nodes; both must show
   auth_token_expiry true, auth_token_idle_days 90, auth_token_expiry_since S0.
Report anything that did not match, with the time of day for log lookup.
```

## 7. Open questions

1. **Idle-expiry window — resolved 2026-09-21** (§3.4). No automatic expiry
   until Plex/Kodi, TV, and offline clients have tested recovery semantics.
   M3 inventory and explicit fenced revocation ship first. *Re-resolved
   2026-09-24 by Paul (§3.4.1): an option, on by default, 90 idle days,
   sliding. Plex/Kodi clients in regular use never expire; one idle for 90
   days needs its token re-entered.*
2. **`REVOCATION_ADMISSION_WAIT` on a five-voter cluster.** The numbers in
   §3.1 assume three members; a fanout to five is still under 2 s per
   phase but the tail is longer. Revisit if a five-voter lab appears.
3. **Advisory readiness heuristic for proxies** (§3.2, ">50 % of logins
   from one address that also sends `X-Forwarded-For`"). Good enough to
   catch an unconfigured Caddy; may be noisy on a household behind one
   NAT gateway that happens to run a proxy for something else. Advisory
   text, not a refusal, so the cost of noise is one line.
4. **Query-credential narrowing sequencing** (§4). Whether D5's
   short-lived file grant lands before or after the web client stops
   sending `?token=` on JSON routes decides which client PR goes first;
   this plan only requires that the metric exist before any refusal.

---

## Execution log

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | Claim | [#433](http://192.168.4.7:3000/noirr/plurx/pulls/433) | Claimed the whole C-04 plan on `plan/C-04`; draft PR preceded implementation. The existing Begin/Store/End revocation fence remains non-negotiable. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | M1 | [#433](http://192.168.4.7:3000/noirr/plurx/pulls/433) | `3c0696eca`: eight-slot admission, 2.5 s timeout, RAII cleanup ownership, and four fixed revocation outcomes. Pinned Rust 1.97.1 `cargo check -p plurxd --all-targets` and the focused queue regression passed; three-node simultaneous-device acceptance remains deployment evidence. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | M2 | [#433](http://192.168.4.7:3000/noirr/plurx/pulls/433) | `1373f1202`: bounded pair/address backoff, trusted-proxy boundary, fixed metrics, and advisory-only System notice. Seven focused throttle/proxy tests passed. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | M3 | [#433](http://192.168.4.7:3000/noirr/plurx/pulls/433) | `b87ce2554`: bounded prefix-only inventory and uniquely matched fenced revoke on SQLite/Hiqlite, self/admin routes, docs, and cluster-auth validation ownership. Focused Store and real-router regressions passed. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | M4 decision | [#433](http://192.168.4.7:3000/noirr/plurx/pulls/433) | Retain non-expiring tokens until all clients have tested refresh/re-login recovery. M3 inventory and explicit revocation are the compensating control; no expiry schema, sweep, setting, or feature gate was added. |
| 2026-09-22 | claude-opus-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | Sole-review fixes | [#433](http://192.168.4.7:3000/noirr/plurx/pulls/433) | Merged `origin/main` (`fec4d1a77`) and addressed both findings of review #3378. P1: `MAX_DEVICE_LABEL_BYTES = 256` refused at `/auth/login` with an explicit 8 KiB route body limit, capped at both Store writes, and capped in SQL plus on a character boundary where the inventory is projected, so a pre-bound row is truncated rather than dropped. P2: the queue permit is released once the operation mutex is held, so the depth is the eight waiters the contract states rather than seven, and the plan's missing end-to-end concurrency proof landed — two queued sign-outs both succeed, and a sign-out that waits out the admission window is refused having changed nothing. §3.1's `RevocationAdmission` sketch is superseded on the `_waiting` field only; `begin`, `finish`, `arm_ambiguity`, guard drop and every timing constant are unchanged. |
| 2026-09-24 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M4 option (supersedes the M4 decision) | [#486](http://192.168.4.7:3000/noirr/plurx/pulls/486) (draft) | Paul asked 2026-09-23 for expiry as an option defaulting on; built as §3.4.1, whose **Decision** items were taken on his behalf and are his to overturn. `b5c0381fd` server: `TokenIdlePolicy`, `UserStore::authenticate_token` on SQLite and hiqlite with the policy read in the token's own read, `user_for_token` as a provided method over it, typed 401 `session_expired`, once-only `start_token_expiry_clock` at startup, settings fields, devices `expires_at`/`expired`. `7a622454b` web: the server's sentence on the login screen (kept across boot's double paint), Settings → Users "Sign-ins" card and per-account Devices drawer. `de782aeca` + `1ca4861bf` Android and `33e132c97` Apple: `session_expired` lands on the login screen with the sentence. Evidence on nuc3 / mba: `cargo test --locked --no-fail-fast -p plurx-core -p plurxd` exit 0 (plurx-core lib 1245, store_contract SQLite 123, plurxd 2718 passed); the three-voter hiqlite lane `store_contract sign_in_expiry` exit 0 (30 in-window requests appended 1 Raft entry, 30 expired appended 0), with `token_activity*` and `login_token*` still green; fmt and workspace clippy `-D warnings` exit 0; `make web-check` exit 0; Android `testDebugUnitTest` 740 run, only the 4 known `PlaybackSurfaceReducerTest` failures, `lintDebug` exit 0; Apple iOS 671 and tvOS 657 tests with only the 5 known failing cases each, the new case passing on both. Each new behaviour's test was shown to fail with its production hunk reverted (SQLite and hiqlite expiry checks, the 401 mapping, the enable-restarts-clock write, the once-only seed, the devices expiry field, the web notice and notice retention, the web expiry label, Android `sessionExpiry`, Apple `noteSessionExpiry`). **needs:** device evidence — the §6.4 GPT prompt (every client's login screen after an aged token, Devices drawer, off/on behaviour, cross-node settings). No device or fleet result is claimed here. |
| 2026-09-24 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M4 option — sole-review follow-up | [#486](http://192.168.4.7:3000/noirr/plurx/pulls/486) (draft) | Merged `origin/main` `07fe785d3` as `c17e050ad` (docs only, no conflicts; `cargo test -p plurx-core -p plurxd` exit 0 on the merge before any change) and answered the three findings of review [4254](http://192.168.4.7:3000/noirr/plurx/pulls/486#issuecomment-4254). P1: the PR body reported `make operations-check` exit 0, which was false — the status line read as terminal against the index's `open`; the Status is now "in execution" (the option is open in #486), which is what `docs/README.md` says, and `make operations-check` exits 0 (486 tests). P2: `96c76784f` makes `startup_tests::a_measured_node_boots_serves_and_drains` assert that a real boot writes `auth.token_expiry_since` with a time from that boot, and that a second boot on the same data dir keeps an earlier stored start; with the boot-time `start_token_expiry_clock` call removed it fails ("boot must start the sign-in expiry clock"), and with the seed turned into an unconditional `put` it fails ("a second boot keeps the first start", left `1790258873`, right `1000`). P2: §6.4 is rewritten — the picker has no one-day choice and the window never starts before `since`, so the prompt now sets `auth_token_idle_days` to 1 through the API, moves `auth.token_expiry_since` back two days with the tokens, checks the drawer before opening the devices, and restores the recorded clock and 90 days at the end, with a caution that other accounts' idle devices are exposed while the window is one day. The C-04 workboard row gains a note (not a rewrite) that #486 supersedes M4's non-expiring decision on Paul's request. Gates on the final head: history-check, validation-lint, validation unittests (205), operations-check (486), spike-lock-check, fmt, workspace clippy `-D warnings`, `cargo test --locked --no-fail-fast -p plurx-core -p plurxd` (1245 / 123 / 2718 passed), `make web-check` and the four node suites all exit 0; Android 740 run with only the 4 known `PlaybackSurfaceReducerTest` failures, `lintDebug` 0; Apple on mba iOS 671 and tvOS 657 tests, each failing only the 5 known cases, the expiry test passing on both. **needs:** device evidence — §6.4, unchanged in scope. |
