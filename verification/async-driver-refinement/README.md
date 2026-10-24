# Async driver refinement — a selected two-worker product

**Status: schema 4 wire implemented; feasibility and proof pending.** The
wire preserves the logical product through lossless shared dictionaries and
fixed Worker rows, but no completed schema 4 product or TLC receipt exists.
Run 5911 stopped at the schema 3 conservative serialized-output estimate; that
incomplete historical result is not a semantic counterexample or acceptance.
The claim remains a complete finite product for two selected workers
over the declared input domain, using a source-bound Store oracle. Ordinary
settlement transfer is modeled from the selected production adapter and Store
path, including payload, captured and request times, local guards, validation
outcomes, and queued eligibility. The bundle and inventory mark this boundary
`pending`; only a complete accepted receipt may report `closed_bounded` for
the selected normal-clock product. This is not a full Rust/Tokio/cluster trace
theorem.

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

The six current external-input qualification tests are explicitly ignored by
ordinary unit runs. This runner supplies their required inputs and selects
each once with `--ignored --exact`, requiring one executed test and zero
ignored tests in that selected invocation. Missing inputs fail; an ordinary
unit run's ignored count is never proof evidence. Self-contained units remain
ordinary tests. No obsolete raw-closure copy is retained.

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
no product-node cap. Schema 4's exact compact JSON bundle and generated
`SourceTables.tla` member each remain bounded by 256 MiB; the total retained
and extracted artifact remains bounded by 512 MiB. The conservative
serialized-component estimate is a byte-budget estimate, not an RSS or heap
bound; a bounded exact-size check decides whether output fits. Exceeding any
limit leaves the result incomplete. Feasibility under these unchanged limits
is unknown.

`tlc/receipt.json` separates complete positive configurations from named
counterexamples. It binds generated tables, models, configurations, commands,
tools, and input provenance. The qualification inventory and TLC receipt keep
`ordinary_settlement_payload_transfer` pending until accepted evidence exists.
A partial TLC receipt also remains pending. A complete accepted receipt records
`closed_bounded` only after the source-bound actual-settlement cases, causal
controls, product validation, and all eleven configurations pass. The receipt
binds source, provenance, graph, model, configuration, and tool identities.
Missing demands or successors, incomplete
frontiers, timeouts, resource failures, and unrelated invariant failures are
not acceptance or control evidence. Seven main contexts feed nine main
configurations; two sparse boundary controls bring the total to eleven.

The source-bound ordinary path retains captured Yield deadlines (+5000 ms for
refusal and +1000 ms for interruption), Retry delay from raw job-ID bytes and
the immutable failed-attempt snapshot, and `library_execution_failed`. The
adapter samples request time after local lost/deadline/token guards; Store
validation distinguishes invalid Yield/Retry times from a refused CAS, and
claim eligibility follows the stored deadline. The model also records local
no-call exits, clock/transport errors, lost responses, and observed results.
Stable-success time movement is constrained by live lease windows, with a
separate eligibility-advance relation for future-dated work. These obligations
are defined but have no execution result in this candidate. C4/C9 remain named
paired causal controls; they do not replace the ordinary-settlement cases.

The selected model remains a finite abstraction. NoTwoSourceReaders counts durable records with state running and queue expiry after the sampled time; it does not bound physical readers while expired work awaits cancellation or join.
`GuardBeforeRevoke` is structural and does not independently prove lock
publication. The three retirement wall samples, logical `resolve_claim`'s
two authoritative reads, captured-time races, cancellation, child lifecycle,
SQL-engine/Raft behavior, and Tokio scheduling remain reviewed or conformance
boundaries. See [CONTRACT.md](CONTRACT.md) before interpreting a receipt.
