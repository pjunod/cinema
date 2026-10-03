# Sharing upgrade qualification — what the old Store can still do

**Status:** open · **Scope:** S3 historical Store compatibility

Companion to [the implementation contract](SHARED-LIBRARIES-IMPLEMENTATION.md)
§10 — this records execution of the pre-migration production Store against
its original schema and the candidate seven-table ownership rebuild.

## The runner compiles the historical implementation

[`qualify-sharing-old-store.py`](../../scripts/qualify-sharing-old-store.py)
archives committed production source
`971265536a259dea38b0f7a9a8752a5a74e8c025`. The archive contains no Git metadata
or repository credentials. It adds qualification tests to that disposable
source extraction; production Store methods and SQL remain unchanged.
The historical library is compiled into native test executables, and the
replicated case starts three actual voter processes using the historical
contract harness. This exercises compiled old production Store calls; it
does not launch a historical `plurxd` process or qualify a rolling deployment.

The runner archives the exact committed review HEAD and takes the candidate
rebuild, populated local fixture and probe from that source archive. Dirty
working-tree fixtures cannot silently change a committed receipt. The fixture's diagnostic incarnation/session strings become
valid UUIDs consistently across their references. Its pending request becomes
resolved, and its drain deadline clears, so the original active route is
eligible for the old owner and expiry inventories. No shared-principal row
is inserted during this compatibility check.

Each backend runs independently with both the original and candidate schema.
The probe invokes production named-column route and playback-pointer reads,
owner/expiry inventory counts, fresh request insertion, desired-selection
upsert, terminal settlement and maintenance cleanup. Baseline writes must
succeed. Candidate SQLite inserts must report the omitted non-null owner key;
the old replicated insert and both desired-selection upserts must report
the obsolete conflict target. An unrelated
failure cannot satisfy those assertions. The runner refuses an unpinned
compiler and requires one executed passing test with zero ignored for each
Cargo command.

## Reproduce the compatibility receipt

Use a new disposable source directory and a dedicated warm target directory:

```bash
export PATH=/Users/pjunod/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin:$PATH
python3 scripts/qualify-sharing-old-store.py \
  --source-dir /private/tmp/plurx-sharing-old-store-qualification \
  --target-dir /private/tmp/plurx-sharing-upgrade-target
```

**How to read it:** `OLD-STORE` lines identify backend and original/candidate
schema. `INCOMPATIBLE` is an expected observed writer refusal, not a passing
rolling-upgrade verdict. A passing test establishes only the asserted Store
compatibility matrix. The output prints the exact old source hash, candidate
source commit, archive/rebuild/runner/fixture/harness SHA256 values and compiler.

## Executed compatibility matrix

Rust `1.97.1 (8bab26f4f 2026-07-14)` compiled the historical Store. The
candidate was taken from source commit
`2521f7d42a0eb634216f85eec9f9f7383e076415`, with rebuild SHA256
`6fc8faf9f494cffe9b02806770e340b6d762358fa0ce50cf7b99f6dff455f164`.
Qualification harness SHA256:
`aea33fa82d734b3abbf5577c56b1d6a849a2d5dec4eef344709c412dec887c71`.
The SQLite test passed 1/1 with zero ignored in 1.96 seconds; the actual
three-voter test passed 1/1 with zero ignored in 18.40 seconds.

| Backend/schema | Retained route/pointer reads | Owner/expiry counts | Fresh request insert | Desired insert/update | Terminal/maintenance cleanup |
|---|---|---|---|---|---|
| SQLite memory/original | pass | 1 / 1 | pass | pass, revision 1→2 | pass, remaining owners 0 |
| SQLite pooled/original | pass | 1 / 1 | pass | pass, revision 1→2 | pass, remaining owners 0 |
| Three voters/original | pass | 1 / 1 | pass | pass, revision 1→2 | pass, remaining owners 0 |
| SQLite memory/candidate | pass | 1 / 1 | refused: omitted `owner_key` | refused: old conflict target | pass, remaining owners 0 |
| SQLite pooled/candidate | pass | 1 / 1 | refused: omitted `owner_key` | refused: old conflict target | pass, remaining owners 0 |
| Three voters/candidate | pass | 1 / 1 | refused: old conflict target | refused: old conflict target | pass, remaining owners 0 |

SQLite reported `NOT NULL constraint failed: media_session_requests.owner_key`.
Both desired-selection writers and the old replicated request writer reported
`ON CONFLICT clause does not match any PRIMARY KEY or UNIQUE constraint`.
The bare rebuild therefore cannot support keeping these old local writers
live. This observed failure supports testing the coordinated fallback; it
does not rule out a separately implemented and qualified compatibility bridge.

The runner's source-overlay and unpinned-compiler refusal regressions passed
2/2; the docs index passed 4/4. Their commands are:

```bash
python3 -m unittest discover -s tests/sharing -p 'test_old_store_qualification.py' -v
python3 -m unittest tests.operations.test_docs_index
```

## The deployment decision remains bounded by the evidence

The bare candidate rebuild changes ownership keys and conflict targets.
Read compatibility alone cannot preserve old local writers. No compatibility
bridge is installed by this probe. A coordinated drain/backup/upgrade path
still needs execution evidence, compatible-member admission/rejoin fencing,
backup restoration and rollback qualification before S3 can land.

The inventory counts exercise old session inventories and request admission
paths, rather than a census of every server metrics/count endpoint. Shared
rows, complete daemon startup, mixed-member running clusters and in-place
old-binary rollback are outside this receipt.


## Positive-local-owner candidate requalification

The exact integrated candidate `46e924454258f8925da91eed37be2f02f7c227b9`
was requalified after all seven Local owner constraints were tightened to
require a positive user ID. Its rebuild SHA256 is
`e849c479d3a138c8485f7ffc76b7842d61ed613e7e085412d338e4b1e820a3a6`.
The unchanged historical production source remains
`971265536a259dea38b0f7a9a8752a5a74e8c025`. Rust 1.97.1 executed the SQLite
probe in both connection modes (one test, zero ignored, 1.89 seconds) and
the actual three-voter probe (one test, zero ignored, 18.20 seconds).
The compatibility matrix above was unchanged: baseline writers passed,
and rebuilt-schema request/desired writers had the same observed refusals.

```sh
python3 scripts/qualify-sharing-old-store.py \
  --source-dir /private/tmp/plurx-sharing-old-store-positive-46e924 \
  --target-dir /private/tmp/plurx-sharing-upgrade-target
```

The source directory must be new when reproducing this command. The log
`/private/tmp/sharing-upgrade-positive-exact.log` records candidate, archive,
runner, fixture, harness and DDL hashes. This qualification does not change
the outstanding deployment and mixed-member boundaries above.
