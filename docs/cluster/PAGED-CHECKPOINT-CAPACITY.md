# Paged checkpoint capacity and publication

**Status:** Open — capacity amendment proposed; implementation, calibration and qualification pending. **Updated:** 2026-10-29.

This proposal records the checkpoint-capacity amendment for the paged async proof. Read it with the [delivery and ownership ledger](PAGED-ASYNC-PROOF-IMPLEMENTATION.md) and the [schema 6 proof contract](../../verification/async-driver-refinement/PAGED-CONTRACT.md). It addresses admission and checkpoint transfer; it does not change the proof domain or claim that the complete proof fits the available runner. The existing resource plan is not admissible at historical scale, and no immutable session plan or capability trio has been started.

## 1. Decision and scope

Retain complete SQLite catalog and verifier backups, their existing restore validation, and the exact proof state they contain. Replace the current whole-backup slot requirement with authenticated, bounded publication batches over immutable content-addressed chunks. Replace the aggregate record-times-every-table allowance with a source-pinned ownership map and explicit physical-file reservations enforced by both writers.

The amendment leaves the algorithm, exact Raw Store identity, original Init, seven contexts, eleven outcome families, global D/Next/SCC and WF/SF obligations unchanged. It does not increase the existing upload slots, part or artifact limits, resident-memory guards, or TLC heap. It is a design proposal; no source interface shapes are frozen by this document.

## 2. Why the existing admission cannot cover historical scale

The current admission has two independent capacity problems. Both have to be addressed before an immutable full-proof plan can be admitted.

1. **The encoded-record allowance is charged to every table and index.** Calibration combines the total operational record count with all per-kind maximum encoded sizes, then multiplies the count by the table/index geometry of each database. A Product, control record, or auxiliary entry is therefore reserved against every verifier table whether or not that table stores it. This is a conservative allocation rule, not a theorem about actual SQLite growth. Shrinking it by an unexplained multiplier would be unsafe.
2. **Both full backups must fit the fixed upload slots at once.** Admission calculates the complete catalog and verifier backup size and requires their combined reservation to fit the current set of native parts. The workflow has 32 upload slots, each limited to 256 MiB, including a fixed prefix reservation. Without a publication continuation, chunking inside a single job does not help when the next complete snapshot exceeds the remaining slots.

Let `N` be the sum of operational record counts, `R` the sum of each count multiplied by its full-encoding cap, and, for measured SQLite geometry `g`, `p_g` its page size, `q_g` its existing page count, and `k_g` the number of tables plus indexes (at least one). The current calibration has the lower-bound form:

```text
I = Σg p_g × (q_g + N × k_g) + 2R
maximum combined backup reservation = 2(R + I)
ceil(2(R + I) / 256 MiB) + fixed_prefix_parts ≤ 32
```

With the optimistic minimum of one fixed prefix part, `R + I` is at most 3.875 GiB. The verifier has 82 tables and 18 explicit indexes before automatic indexes, so its geometry alone contributes at least `100 × page_size × N`. At a 4 KiB page size, that permits fewer than 10,158 total records under the optimistic bound; positive `R`, existing pages, automatic indexes and the Rust catalog lower that limit.

The historical 813,183-Product prefix from run 6115 is a capacity diagnostic, not a current-source proof or a complete graph. It is far above that optimistic record ceiling. More disk, more work units, or smaller proof quanta cannot remove the existing slot inequality. The [session controller](../../validation/async_refinement_session.py), [paged verifier](../../verification/async-driver-refinement/paged_checker.py) and [proof contract](../../verification/async-driver-refinement/PAGED-CONTRACT.md) show the current admission and proof boundaries.

## 3. Capacity amendment

### 3.1 Charge physical storage by its actual owner

Keep the complete semantic record receipt and every per-kind record and encoding cap. Add a source-pinned ownership map for every installed catalog and verifier table, tied to its DDL and actual record relationship. The map distinguishes global, current-input, current-work-unit and session-retained lifetimes. It must not assume that unrelated record kinds occupy every table or that rows have an unexplained one-to-one relation.

Use a reviewed physical policy with explicit reservations for each database file family and its SQLite journal/shared-memory files, temporary work, complete snapshot spool, restore staging, transfer staging and verified chunk cache. Include measured source/runtime scratch and retained evidence. Each file family has one owner and one reservation. Unknown tables, indexes, source drift, ineffective page limits, and exhausted reservations fail closed.

Calibration must inspect the actual installed tables and measured SQLite page geometry, check that both writers enforce their own effective page ceilings, and account for overlapping backups, restores and batches before admission. Retain pre-reservation before writes/copies and the existing rollback, commit, export and install guards. A declared disk or page ceiling is not an observed size and does not guarantee that the full logical workload fits.

### 3.2 Publish complete snapshots in authenticated batches

Continue to create full consistent backups with the existing backup APIs. Bind each complete snapshot to its exact source, plan, Init, work-unit origin, accepted predecessor, checkpoint and complete byte length/hash. Do not reconstruct a live database from selected rows or copy a live WAL/main file.

Represent each complete file with immutable content-addressed chunks and a complete bounded manifest. The reviewed initial chunk size is 1 MiB. The manifest covers every interval in the file, including unchanged bytes; local cache presence alone is not authority. Reuse is allowed only through authenticated predecessor lineage or already verified batches of the same pending snapshot. Preserve the existing 256 MiB part and 512 MiB artifact limits and all 32 upload slots.

A source-owned publication continuation sends missing chunks and their bounded manifest nodes over as many native jobs as required. Those jobs perform no new proof operation. Each batch preserves the original execution identity separately from its actual upload job and binds its predecessor batch, exact members, cumulative inventory and retained-byte charge. A partial batch reports no acceptance and does not advance the proof checkpoint, counters, cursor, completion flag or outcome obligations. The ordinary proof result advances once, only after publication is complete.

After all chunks and evidence are present, stream-reconstruct both entire database files into fresh owned staging. Verify each full length and hash, then run the existing Rust private validation, full-D/raw-Store/source correspondence, restored-verifier/result-chain checks and original TLC/member authentication. Only after both restorations pass may the controller install the new generation and advance the original work unit. A missing unuploaded spool returns to the last accepted checkpoint; it cannot turn uploaded chunks into proof progress.

### 3.3 Keep semantic evidence and limits intact

Publication metadata is operational auxiliary state. It is not one of the 21 semantic source-global kinds and cannot satisfy proof coverage. Charge complete encodings, manifests, chunk bytes, logs and retained copies. Keep records indivisible and retain the original TLC logs and full historical artifact authentication.

This amendment removes the two admission bottlenecks above. It does not prove full-Outcome RAM fit, cumulative-history fit, complete snapshot reconstruction fit, or that a measured physical plan will be sufficient. Any such limit remains an explicit refusal or incomplete session, never partial acceptance.

## 4. Builder ownership and integration

Freeze the small shared root and physical-policy interfaces before either implementation starts. Keep each file with one writer; integrate the matching validators before refreshing source metadata.

| Owner | Owned implementation files | Responsibility |
|---|---|---|
| Builder A | `validation/async_refinement_provenance.py`, `validation/async_refinement_session.py`, new `validation/async_snapshot_transport.py`, `.github/workflows/async-proof-session.yml`, catalog-transport and control-carrier auxiliary schemas, and their operation/transport fixtures | Own the single controller's pending-publication state, batch sequencing and native artifact authentication. Supply the verifier-root fields through the frozen interface. |
| Builder B | `crates/plurx-core/src/store/async_driver_catalog.rs`, its SQLite resource/backup/export helpers, `verification/async-driver-refinement/paged_codec.py`, `paged_checker.py`, `paged-proof.schema.json` ResourcePlan changes, verifier-continuation schema adaptations, and a focused physical-accounting module/map with fixtures | Enforce the named catalog/verifier file reservations and page limits, preserve complete backup/restore semantics, and implement the verifier-side root validator. Preserve the proof algorithm. |

The builders must not edit the same schema or module concurrently. Builder A owns the transport-root declaration; Builder B owns its verifier implementation. Keep new publication types in closed operational schemas and bind the new helpers, maps, schemas and policies into source registration, accounting and capability provenance before integration.

## 5. Acceptance

Implementation is complete only when the exact integrated source has a fresh independent review and all of these conditions hold:

- Physical calibration accounts for every installed table, index and file family; both database writers enforce the effective page ceilings; pre-reservations, rollback and full-file guards refuse safely at their boundaries.
- Mutation fixtures force at least three publication batches, verify unchanged-chunk reuse plus full reconstruction, and reject truncated chunks, holes, overlap, duplicate ranges, stale cache data, wrong source/plan/Init/predecessor, foreign or altered artifacts, failed/replayed batches, and interruptions before activation. No checkpoint advances until both complete restores validate.
- A native capability run exercises the real multi-batch route and full download/restore on the exact reviewed source, registered runner and measured storage. Synthetic fixtures are protocol evidence only; they are not native capacity evidence.
- Complete qualification still covers all seven contexts and eleven semantic outcome families, full global D/Next and SCC obligations, WF/SF and stutter checks, exact Raw Store identity, original Init/Oracle correspondence, TLC component results, and backend, numeric, causal and temporal checks.
- The exact-head Main gate and required merge evidence pass after qualification. The report states measured limits and any remaining capacity refusal honestly; there is no full-fit promise in this proposal.

The capacity amendment is open. No immutable plan, native capability trio, or complete proof qualification is claimed here.

## 6. Source map

- [Schema 6 proof contract](../../verification/async-driver-refinement/PAGED-CONTRACT.md) and [finite inventory](../../verification/async-driver-refinement/finite-inventory.json) define the unchanged proof domain and source premises.
- [Session admission and publication workflow](../../validation/async_refinement_session.py) and [workflow](../../.github/workflows/async-proof-session.yml) define current resource and native upload boundaries.
- [Rust catalog](../../crates/plurx-core/src/store/async_driver_catalog.rs), [catalog SQLite backend](../../crates/plurx-core/src/store/async_driver_catalog/sqlite.rs), [verifier codec](../../verification/async-driver-refinement/paged_codec.py) and [verifier checker](../../verification/async-driver-refinement/paged_checker.py) implement the current storage and restore guards.
- [Source-bound formulas](../../verification/async-driver-refinement/source-bound-formulas.json) bind the logical record contract; they do not substitute for physical SQLite calibration.
