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
fresh checkout and pinned toolchain lines. Current authority uses the shared
ordinary main-lane validator. Live main may advance while the authenticated
event baseline remains unchanged; head/ref/repository/readiness and immutable
API-event checks stay strict. Effort/manual expected-base binding is separate.
Legacy ZIP/member/log bounds are 16/64/64 KiB. Version-1 bundles have a
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

The closure policy pins Cargo.toml, package `build` and Package.swift bytes and
Git modes, and validates the bounded native Make scope rather than allowing
arbitrary whole-file revisions.
Cargo.toml supplies the version expanded
by Make; treating arbitrary manifest changes as harmless could introduce shell
inputs outside the closure. Alternate root Make selectors (`GNUmakefile` and
`makefile`, including case variants except exact `Makefile`), versioned package
manifests, Package.resolved and package `.swiftpm` configuration aliases are
refused. The committed root-name inventory is nonrecursive and bounded to
32 KiB/512 entries; package selectors use the same case-folded name policy. An unsupported recipe or dependency requires closure
review before any reuse, history classification or fresh execution. Ordinary
Swift, test, resource, brand and license changes remain eligible as new inputs.
The guard runs on every reconstructed manifest and again immediately before a
fresh start. Before any current applicability decision and again at the final
launch, it also checks actual root/package directory names
for untracked alternate selectors, without following child symlinks or probing
a case-insensitive alias of Makefile. These restrictions preserve the original 45-file provenance; they
do not claim that future command/dependency changes have the same closure.

The restricted Make recognizer classifies every physical line. It preserves
34 exact ordered global assignments/directives and their balanced conditional
context, plus the exact unconditional native build/test/check block and
prerequisites. It permits unrelated literal lowercase rules, tabbed recipes
and ordinary `.PHONY` names. Unknown syntax, dynamic or pattern headers,
includes/eval/define, new special targets, target overrides, Makefile-remake
selectors, duplicate/native co-targets and nonrecipe continuation escapes
refuse before source applicability. It is a finite source-language guard,
not a Make evaluator. Inputs stay within 256 KiB and 4096 physical lines.

The protected globals have SHA-256
`337ccf7fa94c1e2ba972697fb191f458b72a476e937cb1595ee85ff764cc66b8`;
the native block has SHA-256
`42dd53ee2e7741f227f6b3fbfcba4dc23eb0f8ec521ecc2f03e44e8ee1d04c54`.
Both match the original producer and the integrated main source. Every
manifest carries the actual Make bytes internally, and each applicability
projection revalidates their mode, Git blob and SHA-256 against its raw row
and reruns the scope guard. A row-only manifest cannot claim compatibility.

Only that validated applicability copy maps Make to the original representative
row. All other source entries remain exact. Actual raw current/original
manifests, checkout hashes, producer receipt equality and final launch equality
retain the real file identities. This preserves the prior 75-case key for
proven unchanged native inputs without treating a new web test as a native
execution. Scope/version/protected identities enter the policy fingerprint;
no older v1 adapter or policy check is waived. The known original path still
authenticates the actual legacy execution before considering v1 candidates.
Missing or corrupt original evidence refuses instead of running 75 again.

A v1 upload hash belongs to its unique native uploader transaction, bounded to
32 lines and 16 KiB. Its ordered start, byte count, completion, hash, finalization,
name, ID and final size must agree with the API archive. A separate TV upload in
the same Apple job supplies no native hash and does not invalidate one.

Workflow applicability pins the complete original preamble (triggers, global
environment and concurrency), the complete transitive `paperwork`, `scope` and
`preflight` jobs, and the complete original or reviewed adapted `apple_compile`
job. Current execution and v1 producers require the adapted kind. The bounded
UTF-8/LF plain-job-map splitter rejects tabs, duplicate or complex keys, late
globals, unsupported indentation and more than 64 jobs within 128 KiB.
Unrelated job bodies may change without changing the protected native scope.
One exact original DerivedData cache step is an optional isolated capability:
its complete 645 bytes and pinned action/path/key/condition are checked, and it
may occur once only at its original position immediately before the mobile/TV
compile step. Scope comparison removes only that exact block from both sides;
modified, moved, duplicated or additional cache recipes still refuse. Its
`clients/apple/build/DerivedData` path is outside the native tray source and
scratch closure. This supports cache removal without changing native commands
or accepting modified mobile/TV cases as old passes.
Receipts still record the SHA-256 of the actual complete producer/consumer
workflow bytes; scoped kind is separate and never replaces raw provenance.
The scope version and protected identities enter the policy fingerprint;
older v1 adapter/policy receipts receive no compatibility waiver. Existing
source, API, checkout, toolchain and upload authentication remain exact.
Workflow text does not additionally pin source helpers invoked by prerequisite
jobs. The native command, full checkout, evidence-upload path and all protected
mobile/TV steps remain unchanged. The current job
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
