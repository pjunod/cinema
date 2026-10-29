# Paged async proof — delivery and ownership

**Status:** Open — verification and acceptance pending. Updated 2026-10-29.

This extends
[the async refinement ledger](ASYNC-STORE-REFINEMENT-STATUS.md) after the actual
run 6115 product hit its global Store cap. It implements a paged exact proof
for the same selected finite domain. It adds no production feature switch.

Read [the schema 6 contract](../../verification/async-driver-refinement/PAGED-CONTRACT.md)
for the eight interfaces, concrete Rust signatures, canonical records, SQL
catalog, session protocol, resource formulas and temporal certificates. The
[finite inventory](../../verification/async-driver-refinement/finite-inventory.json)
records source-backed generators, all nine table writers and repeating paths;
independent Astra design conformance is recorded for the amended M0 packet.
Rust storage, actual Init/Oracle replay, checkpoint/backup, export, request
interfaces and complete predecessor transactions are committed and compile-clean
through `a3b584e9`. The paged default driver and expand/import/restore wiring
are committed; the bounded static ABI audit found no owned Rust mismatch. The
independent verifier baseline and input-continuation correction are committed
and integrated. Source and active-route review are approved through `ce1909d7`
and Main `85910425`. Four public runtime identities have been collected; full
runtime and source-export approval, capability probes, and global qualification
remain pending. No tests or paged qualification have run for this implementation;
no accepted receipt exists.

## Why the architecture changes

Run 6115 attempted its 20,001st exact Store identity after retaining 20,000
Stores and 43,221 Store edges. Its partial BASE product contained 813,183 nodes,
2,446,663 edges, 296,431 expanded nodes and 516,751 pending nodes. The serialized
estimate was 86,402,110 bytes and elapsed product time 1,384.342 seconds. The
failure was `DomainStateCap`, before complete product/TLC acceptance; fifteen
successful prefix cases remain historical evidence. These observations establish
neither physical-row distinctness nor a practical full-closure size.

The previous **global** 20,000 cap is retired in the target architecture. A
disk catalog retains every exact logical Store/product/demand/edge and FIFO
frontier entry. The 20,000 bound becomes a resident Store cache limit, with
additional measured/admitted byte and RSS ceilings. PhysicalRows and full raw
Episode are interned losslessly; logical Store identity remains their exact
existing pair. Actual initial database image/raw UUIDs and novel-state production
checks stay authoritative; equal PhysicalRows never permits general live-database
reuse. Capacity-only successors remain globally counted.

The resource policy amendment is implemented by the schema 6 runtime through
`a3b584e9`; its obsolete global encoder is removed. Complete graph closure and
practical fit remain unqualified.
The target keeps 256 MiB per page/SourceTables member, 512 MiB per retained or
extracted artifact, 300 members per artifact, 1,800 seconds per work unit,
TLC 4 GiB/one worker. Before dispatch, the coordinator must measure actual owned
runner storage and admit an immutable **total session** disk/artifact/unit plan
plus resident byte/RSS bounds. The example plan is unadmitted; it invents no
operational quota. Quota exhaustion preserves an incomplete session, not a pass.

## Milestones and sole writers

Parallel coding began after independent Astra design conformance of the
amended M0 packet. Each row owns its listed files; shared schema/registry edits
require an explicit integration handoff. All coding is lower Sol/Luna; actual
Astra provides design advice and a fresh, separate final adversarial review.

| Milestone / owner | Exact owned surfaces | Delivery and current state |
|---|---|---|
| M0 · primary lower Sol; coordinator integrates | `verification/async-driver-refinement/PAGED-CONTRACT.md`, `paged-proof.schema.json`, `paged-catalog.sql`, `paged-resource-plan.example.json`, `finite-inventory.json`, `paged-outcomes.json`; this proposal and canonical contract/README/status/index/backlog amendments | Freeze concrete records, SQL, Rust APIs, finite source lemma, monitors, resource/session protocol and ownership. Amended M0 design recheck conforms. Necessary transport, verifier continuation, operational resource and control-carrier designs are sealed. No acceptance credit. |
| M1 + M2 · one primary lower Sol owner | `crates/plurx-core/src/store/async_driver_catalog.rs`, `async_driver_catalog/{records,sqlite,memory}.rs`, `async_driver_refinement.rs`, `async_driver_product.rs`, `store/mod.rs`; associated Rust fixtures in those modules | Persistent exact catalog/Oracle then complete atomic product expansion/frontier/page production. One owner for both existing async-driver files. Storage, actual Init/Oracle, checkpoint/backup, export, request interfaces and complete predecessor transactions are committed and compile-clean; paged default/resume/import wiring and resource guards are committed through `a3b584e9`. The bounded static ABI audit found no owned Rust mismatch. New and adapted regression definitions are unexecuted. |
| M3 · verifier lower lane | `verification/async-driver-refinement/checker.py`, `AsyncDriverRefinement.tla`, new `paged_codec.py`, `paged_checker.py`, `paged_scc.py`, `PagedOneStep.tla`, `PagedComponent.tla`, `PropertyMonitors.tla`; paged verifier Python fixtures under `tests/operations/` | Baseline `372c` and final input-continuation batch `3f8d483f` are committed and integrated. Independent streamed global semantic/numeric/D/Next/demand/fair-pair checks, monitor/SCC certificates, exact original-Init witnesses and component TLC aggregation still require qualification. Existing boundary controls/config semantics remain required. |
| M4 · orchestration lower lane | `scripts/check-async-driver-refinement`, new `validation/async_refinement_session.py`, `.github/workflows/async-proof-session.yml`, relevant Main workflow integration, `validation/async_refinement_resume.py` and `validation/async_refinement_provenance.py`, corresponding receipt/session fixtures | Baseline `2a91c77` and final-prep `cb298490` are committed and integrated. Native CI session integration remains in progress; no capability probe or dispatch has run, and ordinary gate authority remains strict. |
| Shared registry · M4 sole writer after handoff | `verification/async-driver-refinement/store-tests.json` and runner invocation descriptors | M1/M2/M3 supply actual method/case/source requirements; M4 integrates once. Preserve existing ten invocation source contracts and existing backend fixtures unless a reviewed schema 6 necessity changes them. No duplicate concurrent edits. |
| Shared policy/docs · coordinator sole integrator | `validation/points.toml`, knowledge records, canonical docs/index/backlog and source registries outside the rows above | Integrate necessary source-backed descriptors after lane handoffs; no fabricated semantic pins or empty-coverage waiver. |
| M5 · fresh independent actual Astra; coordinator | Frozen complete source and exact evidence packet | After native CI integration, freeze exact source and compile it; obtain a separate fresh Astra final review, corrections and renewed freeze. Only after approval: affected tests, resource/capability checks, all seven contexts and eleven families, backend/numeric/causal/temporal qualification and complete exact-head Main acceptance; then merge, reviewed ledger closeout and owned-storage cleanup. Pending. |

The separate verifier and orchestration clones start from the frozen M0 source;
their existence grants no coding or execution authority. Module names above are
owned implementation targets, not claims that files are already linked. Existing
167 named regression references remain declarations, not new execution credit;
new cases must exist before the coordinator registers meaningful references.

## Completion and acceptance

A full predecessor expansion is one atomic transaction, including actual Oracle
outcomes, excluded classifications, capacity/dependent demands, all successors,
pair unions/full-graph enabledness and discovered FIFO IDs. Checkpoints occur
only between commits. The persisted initial raw fixture, complete source/schema,
plan and ordered work-unit/checkpoint chain bind one session. Corruption, poison,
source drift, missing dependencies and unrepresentable arithmetic fail closed.

The eleven existing **semantic outcome families** remain mandatory, with all
seven contexts closed. A family can require several component TLC processes;
eleven fixed process invocations are no longer the acceptance contract. Global
reachability and independent original-Next agreement precede component checks.
History monitors preserve completion already seen, cleanup pending across a
component boundary and capacity failure in a prefix. Full-graph enabledness,
original nonstuttering action occurrence, original stutter, complete independently
verified SCC partitions and actual original-Init fair witnesses remain mandatory.
An indivisible predecessor/component too large for its admitted limit remains
incomplete; no page-as-pass, domain quotient or arbitrary component slicing.

Work units use a new explicitly registered workflow/event protocol with actual
fresh run/job/task attempt 1 and exact source/PR/repository/base/session/prior
checkpoint/plan/toolchain/runner bindings. Credentials stay in the trusted
controller. Ordinary current/positive authority receives no historical exception.
Only completion mode can aggregate every unit, all families/backends and the
current ready exact-head Main requirements into an accepted root. It cannot
relabel an incomplete historical PR job or use a dummy commit for continuation.

M0 static source/format checks do not qualify these behaviors. Keep the canonical
ledger, contract, proposal and backlog open through independent final review and
actual complete qualification. Preserve all prior semantic obligations and old
receipts with their original source applicability. The final delivery includes
current Main acceptance, review/evidence links, merged documentation and storage
cleanup preserving unique commits, active leases and rollback material.

## M0 schema/API amendment — design recheck pending

The separate M0 amendment preserves the original91a packet and finite ledger
unchanged. It adds fixed cleanup-obligation component views, actual boundary and
enabledness append paths with private late-linked identities, a shared monotone
commit sequence and pinned read lifecycle, final-only singleton page records,
safe root artifact filenames and the M4 dispatch prerequisite above. Source
proof-contract hashes are distinct from actual installed SQLite schema inventory
hashes. An empty-cycle stutter witness uses its original prefix endpoint and full
original fairness even if outgoing edges exist. These are interface corrections,
not runtime implementation or execution results. Schema ownership stays with the
sole M0 integrator; M4 must return any exact workflow-path substitution there
before shared protocol code freezes. No coding lane starts before design recheck.
