# Native macOS pass retention — keep the original execution identity

**Status:** open — source prepared; independent review and qualification pending.

Companion to [CI_TEST_OVERHAUL_PLAN.md](CI_TEST_OVERHAUL_PLAN.md) (gate ownership)
and [PYTHON-UNIT-PR-RECEIPTS.md](PYTHON-UNIT-PR-RECEIPTS.md) (separate Python journals).
This contract owns only `make desktop-macos-check` in the main fast lane.

## A retained result remains the prior run's result

The bounded legacy origin is PR 1263, source `36d25b2313852e4bbde9e7c7b43efa74de23f04c`,
run 5943, successful Apple job 57583 attempt 1, artifact 4225. The overall run
failed in Rust; that does not invalidate this completed desktop branch. Its ZIP
contains the original native log: release builds for arm64 and x86_64, 61 named
Core XCTest cases and 14 named UI cases. It proves no TV/iOS, installed-app,
physical Intel-runtime or daemon-runtime acceptance.

`validation.desktop_macos_retention` authenticates the current ready same-repo
PR/head, prior API run/event/job/attempt, artifact metadata, actual upload hash,
fresh checkout and pinned toolchain lines. Legacy ZIP/member/log bounds are 16/64/64 KiB. Version-1 bundles have a
separate 128 KiB ZIP, 128 KiB JSON/member and 512 KiB total bound; native/job
logs remain 64 KiB.
No committed summary or caller-supplied digest authorizes adoption. Redirects,
extra members, path aliases, special files and malformed/incomplete evidence
fail. Source test identities must equal starts and passes, each exactly once;
equal counts with different names cannot pass.

## Source applicability is reconstructed from authenticated Git objects

Compare every ordered regular path, mode, blob and SHA-256 under the native
package, whole Makefile/Cargo.toml/LICENSE and `brand/export/`. Membership changes,
symlinks and submodules are significant. Bounds are 128 files, 256 KiB per blob
and 2 MiB total. The known closure has 45 files. This is explicitly
`reconstructed_from_authenticated_checkout`; the old producer did not record
an input manifest. The original build/test commands, desktop scope, Apple labels
and Xcode 27.1/build 27A9275 contract remain unchanged.

The closure policy pins the original whole Makefile, Cargo.toml, package `build`
and Package.swift bytes and Git modes. Cargo.toml supplies the version expanded
by Make; treating arbitrary manifest changes as harmless could introduce shell
inputs outside the closure. Alternate root Make selectors (`GNUmakefile` and
`makefile`), versioned package manifests, Package.resolved and package `.swiftpm`
configuration are refused. An unsupported recipe or dependency requires closure
review before any reuse, history classification or fresh execution. Ordinary
Swift, test, resource, brand and license changes remain eligible as new inputs.
The guard runs on every reconstructed manifest and again immediately before a
fresh start. The final launch also checks actual root/package directory names
for untracked alternate selectors, without following child symlinks or probing
a case-insensitive alias of Makefile. These restrictions preserve the original 45-file provenance; they
do not claim that future command/dependency changes have the same closure.

A v1 upload hash belongs to its unique native uploader transaction, bounded to
32 lines and 16 KiB. Its ordered start, byte count, completion, hash, finalization,
name, ID and final size must agree with the API archive. A separate TV upload in
the same Apple job supplies no native hash and does not invalidate one.

Only the exact legacy workflow plus the reviewed Apple-only adapter replacement,
full Git checkout and evidence-upload path are supported. Scope selection,
Main promotion dependencies and mobile/TV steps are unchanged. The current job
rechecks Xcode and records its actual output. API auth tokens remain in the
parent client and are removed from Git, toolchain and native child environments.

The applicability receipt says `retained_prior_execution`, names the original
commit/tree/run/job/attempt, records all archive/member/job-log/source/policy and
adapter identities, and attaches original evidence. It emits no new native pass
events or current build-product claim. Original host OS release, compiler binary
hash, SDK inventory and build-product digests are unrecorded and remain null.
Checkout/scratch paths are provenance, not a binary reproducibility guarantee.

## Version-1 receipts resolve to one real original execution

The legacy origin bootstraps adoption; it is not the only valid future run.
Fresh v1 archives contain exactly the receipt, start, finish and native log.
Their API upload envelope authenticates the actual producer, successful job,
ZIP and members. Source/key, exact case set, policy fingerprint and adapter
bytes are recomputed. The pinned checkout action emits its final `%H` record;
the adapter separately logs a validated clean-HEAD/workspace/source-manifest
attestation. Fresh start and actual zero finish must appear once in order.
The producer does not invent its own not-yet-uploaded ZIP hash or artifact ID.

A retained v1 receipt names the original legacy or v1-fresh execution directly.
The consumer authenticates that root again, comparing every origin claim and
copied log/archive. Retained-to-retained chains and cycles fail. The original
checkout path stays original provenance even on another current checkout.
Adapter/policy changes need reviewed compatibility; a producer's assertion of
`fresh` or `passed` is insufficient. Missing root evidence fails, never reruns.

## Missing known evidence never means run the suite again

For matching known inputs the adapter must retain the authenticated result or
fail. It cannot fall back after missing, expired, corrupt or unreadable evidence.
The explicit workflow option `--fresh-on-new-inputs` expresses ready-PR authority
for genuinely new inputs, without an extra repository variable or manual gate.

Fresh execution requires complete bounded pagination of same-PR and associated
branch history (at most 100 runs), including prior heads and attempts. Every
potential attempt must be positively different in native key or explicitly not
started. A limit, unavailable source, unsupported recipe/manual producer,
queued/running matching work, failed/partial matching execution or missing log
is a conflict. Absence of an artifact proves no nonexecution. The first-run case
is an empty, completely enumerated relevant history. A matching complete version-1 result can retain before the absence scan;
failed/partial matching results cannot enable fresh execution.

Before a proven fresh start, recheck live PR/head and atomically preserve and
print its exact key/run/job/attempt marker. Run the original make once, preserve
its real exit and full log, and require complete exact-source outcomes before
job success. Missing finish is partial. A second local invocation is refused.
Apple job concurrency still serializes these decisions.

## Controller and async evidence keep their own policies

An Apple workflow edit changes the controller invocation tuple. The existing
`reviewed-invocations.json` path can retain its nine results only after completed
independent original/delta review and factual coordinator attestation. A pending
draft outside Git is not approval. Async refinement still requires identical
invocations and receives no exception. Python/Node retain their own exact-source
journals; new adapter fixtures remain unrun until independent source approval.
