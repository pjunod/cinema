# Async driver refinement — one finite selected episode

**Status: implemented, unqualified.** No new test or model-checker result is
claimed by these files. This package imports the production driver's finite
control graph and a closed graph produced by real SQLite operations. Its
conditional claims concern two fixed selected requests inside the declared
input domain D. It does not prove an unbounded fair Rust execution theorem.

NoTwoSourceReaders counts durable records with state running and queue expiry after the sampled time; it does not bound physical readers while expired work awaits cancellation or join.

`GuardBeforeRevoke` is a structural consistency check of the summarized
operation order, not independent publication-lock evidence. Driver `Work`
Debug strings pass an exact registry before mapping to stable model tags;
unknown formatting rejects import instead of widening join outcomes.

The stable-success model counts durable completion even if its acknowledgement
is lost. A resulting `WorkFailed` may request Retry after COMPLETE; the
actual Store relation retains a refused Retry self-edge for that input.

Read [CONTRACT.md](CONTRACT.md) for the exact claims, fairness premises,
source-transfer boundaries and controls. [store-schema.json](store-schema.json)
records the Store producer's schema and reduced input domain;
[selector-control.json](selector-control.json) defines the actual-source poll
policy mutation. [checker.py](checker.py) validates graph inventories and
creates the TLA tables. The repository CLI owns the exact command registry.

## Run after independent review and fixes

```bash
# Selected Rust regressions, real driver inventory, real-SQL closure and mutants.
scripts/check-async-driver-refinement build-inputs --output-dir target/async-driver-refinement
# All positive configurations, nonvacuity witness, semantic and rejection controls.
scripts/check-async-driver-refinement qualify \
  --driver-graph target/async-driver-refinement/driver-graph.json \
  --store-graph target/async-driver-refinement/store-graph.json \
  --provenance target/async-driver-refinement/provenance.json \
  --output-dir target/async-driver-refinement/tlc \
  --cache-dir target/formal-toolchain
```

Run these as affected steps in the existing Rust CI lane, after graph
production. Earlier preflight cannot consume graphs that do not exist yet.
The runner resolves the unchanged durable-jobs pinned Java/TLC manifest
through `scripts/check-formal`; it adds no floating tool or framework.

`affected --base REF` returns JSON for CI. Without a base it selects the
package conservatively. `inventory --driver-graph … --store-graph …
--output-dir …` validates and generates tables only: its receipt explicitly
has `acceptance:false`.

A failed Rust case can be rerun with `build-inputs … --case NAME`. A failed
model configuration can be rerun with `qualify … --config NAME`. Reusing the
same output directory preserves successful cases only when source, tools,
commands and raw-log hashes still match. A partial run never becomes a full
qualification. Project prose changes do not invalidate Rust source evidence.

## Read the artifacts

`provenance.json` records each exact Rust command, pinned compiler version,
source and graph hashes, selected-test count and raw log hash. The 15 exact
Store unit cases and one sampled backend history are defined in
[store-tests.json](store-tests.json). Backend conformance requires completed
markers for memory, file and the actual three-voter backend; enabling a
feature alone is not evidence. The 124 source-defined partition rows require
all four exact partition tests; the runtime 96-row marker covers only the
queue identity/expiry partition. The selector
mutant uses a standalone copy of the actual zero-dependency leaf crate;
it never modifies checkout sources. Its original source/test, mutation and
manifest hashes are retained.

`tlc/receipt.json` distinguishes complete positive configurations from named
counterexamples. It binds source, models, generated table, config, commands,
tools and input provenance; every configuration carries generated/distinct/
remaining state counts and a raw log hash. Controls and the nonvacuity witness
retain their traces. Timeouts, compiler errors, zero tests, unknown outcomes,
missing graph targets and incomplete queues never count as acceptance.

Retirement tracks the local token option separately from the held queue
handle. It composes heartbeat drain, token take, three wall samples and the
actual first-settle/resolve/optional-second-settle calls with separate
application and reply steps. Local cleanup needs these bounded calls to
return; errors may still leave durable authority. Each primitive uses current
Store graph time: settlement requires its captured sample to remain current,
and resolve requires its second sample to remain current. A reviewed source
lemma makes the first dispatch sample irrelevant for resolution with
`Some(original.fence)`; it is still recorded separately.
Captured-time races and the two inner authoritative reads of `resolve_claim`
remain conformance boundaries. Expired Running residue conservation and
same-owner domain reacquisition after its one acquisition are outside the
independent conservation/closed-input claim; foreign-holder is an independent
third domain owner. See [CONTRACT.md](CONTRACT.md) for the exact restrictions.

The qualification registry includes seven library-driver daemon cases and
four `background_jobs::tests` retirement cases. The latter use actual
finish/SQLite paths; they cover durable invariance after acknowledged settle,
retirement after local revocation, unobserved renewal revision, and
cancellation revision after child join. They do not prove no SQL was called,
or cover COMPLETE/errors/successors or resolve's inner-read schedules.

The imported relation is source-derived. Comparing it with itself would not
be independent SQL evidence. The separate checks are assertions on raw row
projections and operation preimages, Rust replay/rollback assertions, exact
input enumeration, and actual trigger mutations. SQL engine internals,
Tokio scheduling and catalogue publication remain explicit boundaries.
