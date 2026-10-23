# Async driver refinement — a selected two-worker product

**Status: implemented, unqualified.** The product design and wire contract are
defined, but this candidate has no new Rust, control, or model-checker results.
The earlier arbitrary Store-D closure hit its historical state cap; that
failed harness is retained as history and is not an acceptance result. The
replacement claim is a complete finite product for two selected workers
over the declared input domain, using a source-bound Store oracle. Production
ordinary Yield/Retry payload and deadline transfer remains explicitly
`unclosed`; the bounded representative protocol is not full adapter
refinement.

Companion to [CONTRACT.md](CONTRACT.md), which defines the claim, independent
coverage checks, fairness, controls, and limits. The source schema is
[product-schema.json](product-schema.json); [store-tests.json](store-tests.json)
owns the exact Rust test registry. The checker binds the production driver
graph, product schema, TLA modules, and eleven configurations before generating
tables.

## Build the inputs and qualify them

Run these after completed final review, fixes and compiler checks. The first
command executes the registered Rust tests and input producers; it does not
require a duplicate unit batch first. The normal CI lane qualifies affected
work after Rust graph generation; earlier preflight has no graph inputs.

```bash
# Build the source-bound driver graph, selected product bundle, controls, and logs.
python3 scripts/check-async-driver-refinement build-inputs \
  --output-dir target/async-driver-refinement

# Validate both inputs and run the complete configured TLC qualification.
python3 scripts/check-async-driver-refinement qualify \
  --driver-graph target/async-driver-refinement/driver-graph.json \
  --product-bundle target/async-driver-refinement/product-bundle.json \
  --provenance target/async-driver-refinement/provenance.json \
  --output-dir target/async-driver-refinement/tlc \
  --cache-dir target/formal-toolchain
```

The product test is
`store::async_driver_product::actual_sql_two_worker_product_is_closed`; it
reads `PLURX_ASYNC_DRIVER_GRAPH` and writes the bundle path named by
`PLURX_ASYNC_PRODUCT_BUNDLE`. C4 and C9 run separate prefix-replay controls for
`LastRequestAtomicRetirement` and `ReservationConservation` through
`product_prefix_completion_mutation_fails_last_request_atomic_retirement`
and `product_prefix_reservation_mutation_fails_reservation_conservation`.
The checker also binds the raw driver-graph bytes, source registries, semantic
model/config files, exact commands, toolchain, and logs.

The CI runner resolves the existing durable-jobs Java/TLC pins through
[`check-formal`](../../scripts/check-formal); it adds no floating tool or
solver framework. `affected --base REF` returns the selected-surface decision.
Without a base, it conservatively selects the package. `inventory
--driver-graph PATH --product-bundle PATH --output-dir DIR` validates the
inputs and generates tables with `acceptance:false`; it is not proof.

`build-inputs --case NAME` and `qualify --config NAME` can resume a matching
working directory. Reuse requires the identical invocation and unchanged
source, tool, command, log, and input identities. A changed source binding
invalidates affected cases; there is no historical compatibility exception,
and a partial journal never becomes a complete receipt.

## Read the evidence and limits

`provenance.json` records the exact Rust commands, source hashes, test counts,
raw logs, and graph identities. Rust closure is limited to the selected
two-worker product and its explicitly enumerated demands. It does not claim
arbitrary Store-D closure. The 20,000 cap counts distinct exact Store states
across all seven main contexts, including capacity-only successors. There is
no new product-node cap. The bundle remains bounded by 256 MiB and the total
artifact by 512 MiB; exceeding a bound leaves the result incomplete.

`tlc/receipt.json` separates complete positive configurations from named
counterexamples. It binds generated tables, models, configurations, commands,
tools, and input provenance. `inventory.json` and `tlc/receipt.json` carry
`ordinary_settlement_payload_transfer: "unclosed"` alongside the shared named
claim-boundary map. Missing demands or successors, incomplete
frontiers, timeouts, resource failures, and unrelated invariant failures are
not acceptance or control evidence. Seven main contexts feed nine main
configurations; two sparse boundary controls bring the total to eleven.

Production Yield uses captured-release deadlines of +5000 ms for refusal and
+1000 ms for pressure/domain-loss interruption. Retry uses release time plus
jitter from raw job-ID bytes and the adapter's failed-attempt snapshot, with
`library_execution_failed`; the selected
representative uses application time and `transient`. Store validation can
reject a captured deadline older than application time, and claim
availability depends on the stored deadline. The retained T4 Kani policy proof
does not prove this daemon-to-Store payload/time transfer. C9 is therefore a
causal trigger-mutation witness only for the declared representative product.

The selected model remains a finite abstraction. NoTwoSourceReaders counts durable records with state running and queue expiry after the sampled time; it does not bound physical readers while expired work awaits cancellation or join.
`GuardBeforeRevoke` is structural and does not independently prove lock
publication. The three retirement wall samples, logical `resolve_claim`'s
two authoritative reads, captured-time races, cancellation, child lifecycle,
SQL-engine/Raft behavior, and Tokio scheduling remain reviewed or conformance
boundaries. See [CONTRACT.md](CONTRACT.md) before interpreting a receipt.
