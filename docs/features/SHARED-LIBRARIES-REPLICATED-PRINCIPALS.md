# Replicated session principals — what now uses canonical ownership

**Status:** open · **Scope:** S3 replicated runtime conversion

Companion to [the implementation contract](SHARED-LIBRARIES-IMPLEMENTATION.md)
§7 and §10 — this tracks production Hiqlite session statements as they move to the
candidate ownership layout. The migration remains uninstalled.

## Request claims and recipes use the rebuilt keys

The first checkpoint converts fresh requests, failed/expired request
reacquisition, Library-channel recipe persistence, owner assignment and
failure settlement. On a rebuilt schema, predicates and admission counts use
canonical owner keys, and both request and recipe conflict targets use the
new primary key. Insertions explicitly provide the complete local principal
projection: the real numeric user, canonical `local:<id>` owner key, local
kind and absent grant/viewer fields. No placeholder local account is created.

The rebuilt insertion, reacquisition, recipe and assignment statements require
the current local user to exist inside the same replicated write. A missing
or deleted owner cannot create or resurrect those rows. A refused claim keeps
the existing `Overloaded` classification; the guard authorizes no later write.
Legacy local query shapes retain their existing behavior. Library-channel
recipes retain their established incarnation binding and cannot be rebound
on a retry naming a different incarnation.

Request readers now decode the complete stored principal and return errors
for missing, wrongly typed, mixed or noncanonical ownership. A resolved local
request pointing at another principal's valid route returns `Conflict`,
without returning that route or its owner metadata. This also protects future
shared request decoding from losing its grant/viewer namespace.

## Inventory and private readers retain stored ownership

Node-owned lease inventories decode all principal fields with fallible access,
so a Shared row's nullable local user cannot panic or become a local account.
Their pointer and unresolved-request joins, and the expired-route inventory's
request join, use the canonical owner key on rebuilt tables. Each grant/viewer
namespace remains distinct in the global inventory. Legacy layouts retain the
numeric joins and explicitly project their actual Local owner.

Desired writes explicitly insert complete Local metadata and use the rebuilt
conflict key. The same-write user guard covers insertion and update; a refused
write returns an error even when an older desired row still exists. Changed
digests advance the revision and unchanged digests preserve the existing row.

Private Local desired and staged reads use the same layout-aware owner
predicate and complete projection. Staged, desired and lease decoders return
errors for incomplete or wrongly typed rows. Preparation replay additionally
checks that the returned route belongs to the Local principal whose pointer
was read; a corrupted pointer cannot disclose another principal's route.

## Recovery budgets keep the complete principal

Rebuilt Local recovery reservations explicitly provide the full principal
projection, conflict on canonical owner/playback/epoch and require the current
Local user inside the reservation write. Settlement addresses the same
canonical budget and preserves the failed-incarnation and terminal-state
fences. Original layouts retain the numeric key.

Recovery reads accept valid typed principals, bound playback and epoch keys,
select the complete stored projection, and compare it with the requested
principal. Two grants using the same viewer/playback/epoch remain independent.
Malformed ownership, metadata types and restriction bytes return errors. The
common converter from `5f58ce919` now accepts the decoded principal rather
than manufacturing a Local owner. Shared reservation and settlement still
refuse before a write.

The recovery contract filter passed 5/5 with zero ignored in 45.75 seconds,
including the historical-v32 migration fixture corrected by `1af1a26e4`. The
four runtime contracts cover single budget admission, racing identities,
settlement fences and corrupt restriction refusal.

## Preparation, rejoin and abort use canonical ledgers

The common preparation builder still submits lease, session and ledger writes
in one ordered Raft proposal. All rebuilt Local owner predicates, counts and
joins use canonical keys; session and preparation insertions supply full Local
metadata. Each admission statement includes the current-user predicate within
the write, and predecessor reads and write predicates require the same owner.

`LocalSessionSql` renders the fixed layout fragments while retaining each real
numeric Local user binding. Preparation uses `equals(1/3/5)` for ownership,
`insert_columns()` with `insert_values(1/3)` for full metadata, and
`existing_user(1/3/5)` for atomic admission. Abort uses `equals(1/3/4)` and
`column()` for preparation/pointer joins. Retirement checks the session owner
at parameter 3 as well as the ledger; pin cleanup repeats that owner in its
ended-session predicate, and lease cleanup repeats it at parameter 4. These
predicates preserve placeholder first-appearance order.

Rejoin consumes the guarded abort outputs before preparing the replacement.
Its guarded ledger delete uses the canonical owner, and its conditional rollback
assertion deliberately targets the layout's non-null ownership key. The error
classifier recognizes only that exact constraint target or a missing guarded
statement output. Abort additionally fences retirement and dependent pin/lease
effects to the ledger principal, so a corrupt Local ledger cannot end a Shared
route or shorten its lease.

The dedicated rebuilt preparation contract creates a current session with the
production legacy writer, rebuilds the fixture, then opens a fresh Store using
the production open-existing path. This respects the Store-lifetime schema
projection; it does not install an upgrade mechanism. Actual prepare, exact
replay, rejoin and abort preserve the current pointer, retain complete staged
ownership and refuse a corrupt cross-principal abort. It passed 1/1 with zero
ignored in 9.20 seconds. The four preparation safety unit tests passed. The
final combined candidate voter module passed 2/2 with zero ignored in 27.32
seconds after the lint-only string repair; feature-enabled denied-warning
Clippy passed in 46.23 seconds.

## Activation preserves the principal through every effect

Rebuilt Local activation addresses request, recipe, desired, session and
pointer rows by canonical owner. Session and pointer insertions provide the
full Local projection; their conflict targets and immutable owner comparisons
use the rebuilt keys. Lease admission, session admission and pointer writes
require the current Local user within the same proposal. Predecessor retirement
and returned predecessor metadata remain within the activation principal.

The initial job-lease insert and conflict update also refuse an incarnation
already belonging to another principal. A failed session conflict must not
leave that foreign lease modified. Dependent successor lease cleanup repeats
the real Local owner predicate. The actual voter regression attempts that
cross-principal collision and checks unchanged lease revision, expiry and
update coordinates; it also verifies Local activation/confirmation/replay,
complete stored session/pointer metadata, missing owner refusal and explicit
Shared admission refusal.

The preparation corruption fixture now includes a real foreign cache pin and
job lease. Its actual abort checks preserve the pin and the complete lease
coordinates, alongside the foreign active route. All three candidate voter
cases passed with zero ignored in 36.36 seconds. The activation unit filter
passed 16/16 with zero ignored in 31.34 seconds. Denied-warning Clippy with
`hiqlite-contract-tests` passed in 42.89 seconds.

## Commit CAS binds ownership and the actual predecessor receipt

The pointer-first commit proposal uses canonical pointer, preparation and
desired keys. Its CAS requires the current Local user and same-principal
predecessor. Draining the predecessor, extending the successor and gating the
receipt remain within that canonical owner; returned routes preserve the same
principal. The existing statement-output chain still makes a lost CAS mutation
free before exact replay or owner-fenced abort classification.

A control receipt must identify the actual same-principal predecessor session
inside that CAS: incarnation, session UUID, owner node and owner epoch all
match the stored row. A trusted incarnation/owner tuple alone cannot authorize
a receipt naming another session. The voter regression gives an otherwise
exact Local receipt an actual Shared session UUID, requires no pointer advance
and no foreign ack, then commits and replays a correct receipt. It passed 1/1
with zero ignored in 9.19 seconds; the commit replay safety unit passed.

Staged readers now accept typed principals on rebuilt tables, bind their
canonical key and decode the complete stored projection with expected-owner
comparison. Two Shared namespaces using one staged playback name remain
independent. The original layout continues to refuse Shared reads rather than
manufacturing a Local user; all Shared mutations remain refused.

## Activation settlement and publication retain the same owner

Confirmation, abandonment and request publication now use the canonical Local
key on rebuilt session, pointer and request predicates. Confirmation and
publication require the real user to exist inside the same write. Abandonment
remains available to retire an existing session after user removal, but each
dependent pointer, job-lease and cache-pin effect requires the ended session's
principal to agree with the activation. The owner-node and epoch fences remain
in place.

The rebuilt activation case includes an already-ended Shared incarnation with
an update timestamp equal to a corrupted Local abandonment call. Actual foreign
job-lease revision/expiry and cache-pin retention assertions distinguish cleanup
ownership from merely observing zero changed session rows. Legitimate Local
confirmation replay, pending activation abandonment and claimed-request
publication exercise the same production methods. The rebuilt case passed
1/1 with zero ignored in 9.32 seconds; the existing dyn-store activation and
settlement contract passed 1/1 in 11.68 seconds. The exact-state confirmation
retry regression, all-target check and denied-warning feature Clippy passed.

## Incarnation-only renewal and takeover keep authority closed

Renewal and takeover APIs carry incarnation and owner epoch rather than a
principal argument. Their pointer and request joins now use the ownership key.
On rebuilt tables, `LocalSessionSql::live_local_user(table)` requires the stored
principal kind to be Local and its real user to exist inside each session,
job-lease and pin authority extension. The original layout retains its existing
behavior. A readable Shared inventory row does not authorize renewal or
producer takeover before grant and member-floor proofs are wired.

The dedicated voter case attempts actual Shared renewal and takeover and checks
foreign lease owner, fence, revision, expiry and pin retention. It also renews
and takes over a legitimate Local session. After deleting the Local user, it
restores a deliberately corrupt active session and matching live lease, then
requires both operations to refuse without changing the lease. This isolates
the missing-user predicate from the deletion trigger's normal retirement.
The candidate case passed 1/1 with zero ignored in 9.09 seconds. The existing
dyn-store lifecycle passed 1/1 in 12.84 seconds; committed successor renewal
and staged-deadline refusal each passed 1/1 in 9.61 seconds. All-target check
and denied-warning feature Clippy passed.

## Terminal cleanup preserves principals and maintenance isolates ledgers

Both exact-owner and capability terminal cleanup bind the stored route's
canonical owner key. On the original schema this uses its real Local user
projection; on the rebuilt schema it uses `owner_key`. Dependent pointer,
lease and pin cleanup checks the same ended principal. Shared terminal cleanup
therefore preserves complete metadata and retires retained authority without a
Local user adapter.

The maintenance sweep joins expired preparation ledgers to sessions with the
same owner and playback before retiring them. Pointer, preparation and resolved
request retention joins likewise include ownership and playback. A corrupt
Local ledger naming a live Shared incarnation is reaped without ending that
foreign stream or releasing its lease/pin.

Active handoff arming/completion applies the current Local-user predicate to
both its mutation and replay readback. This prevents a refused Shared write
from returning an already-published Shared row as success. Ended terminal
projection completion remains available for Shared retirement. The candidate
case covers corruption cleanup, actual Shared exact/capability cleanup, Local
handoff, Shared handoff refusal and missing-user replay refusal. The candidate
passed 1/1 with zero ignored in 9.27 seconds. Existing maintenance, lifecycle
and terminal-ack/takeover cases passed 1/1 each in 9.65, 13.26 and 9.69
seconds. All-target check passed in 26.73 seconds and denied-warning feature
Clippy passed in 30.25 seconds. All eleven Hiqlite session safety/decoder unit
regressions passed with zero ignored.

## Shared terminal acknowledgements cannot clean another grant's pointer

Terminal acknowledgement cleanup now correlates each pointer's canonical
owner and playback with the exact ended session. Merely naming its incarnation
cannot authorize removing another grant's pointer, including a deliberately
corrupt foreign pointer. Session/node/epoch and exact ack replay fences remain
in place; lease and pin effects remain tied to the unique incarnation and
exact owner generation.

The dedicated voter case executes Shared terminal acknowledgements, rejects an
actual different session UUID, wrong node and wrong epoch, and checks another
grant's route, pointer, lease and pin remain unchanged. It then replays the exact
ack after maintenance, refuses a conflicting receipt, and arms/completes the
other Shared terminal projection while requiring ended state and unchanged
clamped leases. These operations retire authority; Shared producer admission
remains closed. The actual candidate three-voter case passed 1/1 with zero
ignored tests in 9.22 seconds. The existing terminal-ack/takeover contract
passed 1/1 in 9.66 seconds and all 11 Hiqlite safety/decoder units passed in
0.01 seconds. The pinned all-target check passed in 27.43 seconds. Denied-warning feature Clippy passed in 33.46 seconds.

## A rebuilt table is not shared admission authority

This historical checkpoint used a quorum-observed Store-lifetime projection.
The [live activation change](SHARING-LIVE-ACTIVATION.md) replaces that
fixed-layout assumption with guarded dispatch and transition-safe reads. Schema
shape proves
neither current export/grant scope nor compatible member capability. Shared
mutations still hit the explicit local-only refusal before any request write.
The Source grant/scope/lifetime proof, same-write all-member floor, source cap
of eight and per-grant cap of four remain required before shared admission.

The runtime census at base
`9e2e0e55c2f3946d6df7a14bd6e73f0e5662503f` contained 61 numeric-owner SQL
literals, including three unit-test literals and validation-only legacy
writers. Earlier counts of 65 preceded the pointer/desired-reader conversion.
These checkpoints cover request, desired, recovery, preparation, activation and
commit paths; they are not a complete census closure or qualification of all
seven ownership tables. The direct numeric runtime owner predicates have been converted; the controlled
legacy layout helper, original schema and validation-only historical writer
remain intentional. Shared authority extension remains closed.

## Exercise production behavior on three voters

The dedicated contract runs the same request lifecycle on the original and
rebuilt schemas. The rebuilt case includes two independent Shared principals,
then checks actual stored Local projections, failure/reacquisition, immutable
recipe binding, absent/deleted user refusal, explicit Shared admission refusal
and a deliberately corrupted cross-principal resolved replay. It also checks
that global owned/expired inventories retain two distinct Shared principals.

```bash
cargo test --locked -p plurx-core --features hiqlite-contract-tests \
  --test store_contract sharing_principal_runtime -- --nocapture
cargo test --locked -p plurx-core --features hiqlite-store \
  --lib preparation_ -- --nocapture
cargo test --locked -p plurx-core --features hiqlite-store \
  --lib sharing_request_ -- --nocapture
cargo test --locked -p plurx-core --features hiqlite-store \
  --lib sharing_inventory_and_staged_decoders -- --nocapture
cargo test --locked -p plurx-core --features hiqlite-contract-tests \
  --test store_contract media_session_ -- --nocapture
cargo clippy --locked -p plurx-core --features hiqlite-contract-tests \
  --all-targets -- -D warnings
```

**How to read it:** the request/inventory/recovery voter regression passed 1/1 with zero ignored
in 18.14 seconds using Rust 1.97.1. The two request decoder regressions and the incomplete
inventory/staged/desired decoder regression passed with zero ignored. All 28 existing local/replicated lifecycle contracts passed
with zero ignored in 258.30 seconds on the commit checkpoint. Its four rebuilt runtime cases
passed with zero ignored in 45.53 seconds. Denied-warning Clippy with
`hiqlite-contract-tests` passed. These tests do not enable shared worker ingress, install
a migration, advertise the member capability or qualify an upgrade/rollback.
