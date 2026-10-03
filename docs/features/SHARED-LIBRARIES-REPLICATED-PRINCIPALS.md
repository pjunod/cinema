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

## A rebuilt table is not shared admission authority

The schema shape comes from the existing quorum-observed Store-lifetime
projection. The shape remains fixed between coordinated restarts. It proves
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
seven ownership tables. Lease, takeover, settlement and maintenance write
predicates still need conversion and qualification.

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
