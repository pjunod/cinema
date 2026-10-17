# Dolby Vision fleet HDR repair — running log

**Status:** open — running log for `effort/dv-hdr-repair`; moved verbatim out of
[DV_HDR_PROCESSING_STATUS.md](DV_HDR_PROCESSING_STATUS.md) §24 on 2026-10-17
so that status page stays a current-state summary. Append new entries at the end.

### 24. Fleet HDR repair — reproduced failures and acceptance still owed

**Status:** repair implementation built; integrated qualification and physical acceptance open · 2026-10-13 · Integration: `effort/dv-hdr-repair`.

The fleet acceptance at the original processing build found no HDR10-E report
and a Profile 5 request falling back to SDR. Source-window reproduction found
four independent causes: processing preparation never requested background
capacity to yield; valid RPU curves ended below code 1023 and were refused;
ordinary Profile 5 HDR negotiation excluded the proved VAAPI Main10 route;
and the runtime's ordinary Intel Vulkan driver could not open the installed
GPU, selecting software Vulkan instead. Auto additionally skipped processing
whenever its ordinary candidate identity was present. Idempotent VOD recovery
filtered encoding facts to reconstructed Profile 8.1 and reported other HDR
routes as SDR.

Astra reviewed the diagnosis and proposal. Admission, bounded signalled curves,
composed VAAPI proof and receipt-backed logging were supported. Auto must get
its own frozen processed recipe identity; ordinary production evidence cannot
authorize it. The complete processing pipeline must sustain the existing
1.15× margin rather than merely finish its first window before a deadline.

| Task | Reviewable change | State / evidence |
|---|---|---|
| R01 | Foreground preparation admission, bounded RPU curves, ordinary Profile 5 VAAPI HDR10, recovery grade/encoder, Intel ICD registration and accepted-report logging | Merged into the effort as `bb25937ea` through #1033 after Effort development gate run 4869 passed on `33989598b`. Focused admission, RPU, recovery, driver-registration and report-lifetime regressions passed on the task tree. |
| R02 | Explicit export geometry/policy and bounded reconstructed VAAPI output; driver cache and software-driver CPU bounds | Implemented native reconstruction followed by explicit RGB/PQ bilinear export, bounded VAAPI Main10 upload and four frame workers per layer. The helper decoder now skips already-suppressed RASL slices before consulting a fresh frame worker’s picture binding. Exact float conversion is retained; an integer-upload experiment failed zero-residual controls and was rejected. Native BL/EL/RPU/RGB seek comparisons pass at four source intervals. The actual Rust runtime passes Profile 5, Profile 8 and Profile 7 FEL at 720p with AAC, 48 verified pictures per window, adjacent windows and a seek; recipe identity stays fixed. Eight CPU bilinear-reference controls differ by at most one RGB48 code. The final selected-stream demux path and a per-call binary32 normalization table preserve exact native/output hashes, including dynamic rounding checked against independent expected bits. In the actual Rust runtime, the clean FEL adjacent intervals deliver 4.004 seconds in 3.140 seconds (1.275×), above Auto’s unchanged 1.15× bar; first preparation takes 1.650 seconds and seek preparation 2.144 seconds. Profile 5 and Profile 8 also pass with AAC and unchanged picture counts. This is bounded runtime evidence; served-client throughput and physical acceptance remain owed. Integration with current main also freezes the proved completed-VAAPI driver policy in first/later execution, output arguments and processing backend identity; ordinary Software/NVENC policy scoping stays unchanged. The final integrated runtime rerun passes all three profiles. FEL adjacent intervals deliver 4.004 seconds in 3.282 seconds (1.220×); first preparation takes 1.695 seconds and seek preparation 2.100 seconds. The throughput PR #1036 landed in the effort as `998083c85` after exact-head Effort development gate run 4882 passed on `b117cdafb`; run 4881 had timed out during Rust compilation without a compiler error. |
| R03 | Admitted processed HDR10 Auto selection and exact live-parent reopen | Merged into the effort as `763956fc9` through PR #1039 after exact-head compile-only Effort development gate run 4910 passed on `6b970a7ba`. Core, web, Apple and Android prefer a compatible, healthy, link-admitted processed HDR10 recipe with actual sustainable output. Qualification follows durable create/predecessor admission, preserves original HDR intent, resolves encoded audio from the same client claim, and binds one-shot execution to the full source, recipe, planning and owner contract. Original startup deadlines and strict negative reads remain authoritative. Private rendition storage cannot enter complete-cache or retained-output admission; a four-instance allowance and failed-cleanup accounting bound its lifetime. Reopen captures only the exact live predecessor, physical rendition and reservation, then rechecks custody at final attachment. Stale production evidence is refreshed by the existing producer through at most seven missing windows; only fresh contiguous verified publications contribute, with a forward-only cursor and the unchanged two-interval/two-second/1.15× bar. End, replacement, expiry and cancellation retire private waits synchronously. Catalogue descriptions remain metadata-only and cannot authorize execution. Astra's read-only review found no remaining custody or current-main integration blocker. Apple 232 and Android 166 identify the changed clients. Selection/native/root regressions passed on attributable checkpoints before the effort-only unit instruction; subsequent expiry, storage, catalogue/audio, interrupted-sampling and reuse controls are written and compiled only. Static source ownership and client regression mappings are reconciled without executing units. The final effort qualification, deployment and physical acceptance remain owed. |
| R04 | Preserve the single recovery epoch through processed qualification | Test-only PR #1046 landed in the effort as `13cc78d68` after compile-only gate 4927 passed every selected check on `185f53852`. Both Python and Rust contracts still require one mint and now cover the qualifier, worker and durable activation sharing that epoch. Final qualification attempt 4916 stopped in preflight on the old Python count: 359 validation tests ran with one failure; runtime suites did not start. The failed result is preserved. The corrected Python guard passed among the 359 validation tests in qualification 4931; the Rust suite did not start, so its corrected runtime guard still awaits integrated qualification. Production behavior is unchanged. |
| R05 | Reconcile generated mobile build claims and HDR report index status | Documentation PR #1050 landed as `a93db85e2` after compile-only gate 4947 passed all seven jobs on exact task head `fdfb808ad`, based on effort `c5320dcb9` containing main `b2adc5f78`. Apple claim surfaces now match the existing build 232 and current Android claims match 166; historical prose and release counters are preserved. Current main independently corrected the HDR report index to open, and integration retains its scoped acceptance wording. Qualification 4931 passed 359 validation tests, then stopped after 1,021 operations tests with seven documentation consistency failures; runtime suites did not start. The earlier failed result remains preserved. Task attempts 4933/4936 timed out on the constrained compiler runner (4936 finished workspace checking before the whole-job deadline, without completing vendor Clippy); 4941 refused the stale main history binding. The green current-base gate ran on an alternate compiler runner. All temporary runner adjustments were explicitly restored. No units ran on this task; final effort qualification and physical acceptance remain pending. |
| R06 | Restore deterministic Apple callback fixture coverage | Test-only PR #1054 landed as `c760865cd` after compile-only gate 4972 passed all seven jobs on exact head `671407aa7`, based on effort `e112f137b`. The repair captures a complete response callback batch using the next request as a completion fence and arms a one-shot terminal barrier only for the terminal response. All nine affected methods passed once on iOS on integrated effort `3b1a0ba31`; the first wrong-target invocation ran no tests and remains a preserved invocation failure. An isolated checkout of that effort completed all 994 iPad cases successfully; all 978 tvOS named cases and its outer command completed successfully. The ten-minute simulator diagnostic collection timeout occurred after the tests and caused no test failure. iOS and tvOS build-for-testing pass without task units. Earlier stale-history and compiler-timeout failures remain preserved. |
| R07 | Preserve resumable ownership for positioned paused startup seeks | PR #1055 landed as `3b1a0ba31` after compile-only gate 4979 passed all seven jobs on `0ecf3d5e0`, based on effort `77642f0b3` containing current main `5874dbb59`. Positioning without frame callbacks retains exact loader custody and the original, possibly tightened deadline. Play, consecutive Pause, local subtitle preservation and an executed replacement seek carry only their current ownership. Positioning grants no presented-frame or processing-report authority. All 21 cases in `node --test tests/playback/paused-hls-resume.test.js`, including the eight new controls, passed once on integrated effort `3b1a0ba31`. No task units executed. |
| R08 | Qualification fixture lineage, complete transport batch and real membership URL publication | PR #1057 landed as `2d3695541` after compile-only gate 4981 passed all seven jobs on exact head `ffa163816`. Store fixtures remove only empty sharing tables before constructing historical fixtures, preserve the production lineage guard, and cover the actual migration baseline. The transport batch requires 70 passes and each of seven named regressions once and retains distinct vendor checks. Topology uses the existing manager and the production publication transaction with durable restoration. An isolated UI fixture reproduces an occupied default Jellyfin port and boot caption-probe children in Activity; the fixture reserves its own fourth port and waits for this boot's audit retirement plus a stable empty local process inventory before captures. Six affected/new UI controls are written; no golden drift is blessed. Affected effort units and stable UI capture remain owed. |
| R09 | Bound the effort compiler resource budget | PR #1056 landed as `e112f137b` after gate 4970 passed all seven jobs on `77e999c50`. The Rust compile job uses one Cargo build worker and a 20-minute whole-job deadline; every compiler and static/history check remains enabled. The green Rust job took about six minutes. Temporary runner routing and memory adjustments were restored; previous timeout results remain failures. |
| R10 | Bound Android emulator post-boot service readiness | PR #1058 landed as `3c4a7d626` after compile-only gate 4985 passed all seven jobs on exact head `b2247d003`. It vendors the exact pinned upstream action and changes only its post-boot startup block. One shared 60-second deadline requires positive package/input service readiness before unlock and settings, retries only the failed stage, and kills/reaps a timed-out child. Four new focused cases and the affected operations contract passed once on integrated effort `a7fa0053e`; its exact committed-source compiler loop passed, and the original instrumentation checks remain unchanged. |
| R11 | Reproduce the unchanged Fontconfig source archive | PR #1059 landed as `a7fa0053e` after compile-only gate 4987 passed all seven jobs on exact head `d3528414b`. It replaces the broken HTML-returning source URL acquisition with an explicit public exact-commit Git archive. Reproduction matches the existing locked SHA-256; the source, hash and recipe are unchanged. Fresh isolated Git state, shared deadlines, bounded scratch/archive size and process-group retirement remain enforced. Seven new cases and the affected source contract passed once on integrated effort `a7fa0053e`; the exact current-base pinned compiler loop passed; package qualification remains owed. |
| R12 | Authenticate complete retained embedded Rust unit cohorts | Reviewable Rust-only continuation binds exact committed source, feature graph, compiler/profile/flags, complete native inventories, original commands and raw logs. It preserves interrupted executor outcomes with unknown native exit, accepts actual Cargo JSON argv/exit records, and binds the finite original Store/lineage capture corpus to original log and sidecar digests with complete ordinals and chronology. Recorded measurements remain distinct from the closed compatibility policy; bulk failure without a recorded numeric exit stays unknown. It references only the two independently isolated auth cases from a fully validated Store result into matching 8 MiB serial WAL contracts. Current compilers and actual non-unit topology/transport campaigns remain required. Complete current applicable coverage and later passes for every historical failure are mandatory; no pending-body executor or arbitrary failure waiver exists. Python/JVM enrollment is explicitly refused until its separate execution/environment provenance is implemented. Periodic normal-hook checkpoints and independent source review are complete. Actual Store/lineage component evidence passes source/log/inventory parsing and live original CI scope/workflow/log authentication; current Linux compilation and native inventories have no body execution. This is component evidence, not a final qualification receipt. The exact-head compile-only task gate and first execution of new/affected controls on the effort remain required. |
| R13 | Model actual composed library-path SQL and reconcile enumerated source audits | PR #1061 landed as `e52f1e360` after compile-only gate 4992 passed all seven jobs on exact head `05cf54397`. The helper model, two read pins and transaction census compiled with pinned Rust and denied-lint Clippy. All 19 affected cases then ran once on the integrated effort: 15 passed and four failed. Two negative census fixtures assume a saturated ceiling; two scanner cases expose completed-expression and lexical-use handling. These failures remain preserved and require a follow-up; source review and compilation did not establish runtime test success. |
| R14 | Align historical Store fixtures with the current unactivated schema and exact WAL inventory | PR #1062 landed as `bde3f0366` after compile-only gate 4994 passed all seven jobs on exact head `5f4c82f38`. Ordinary migration fixtures require baseline 81 and explicitly refuse accidental Source activation. Historical downgrades retain and restore the exact Jellyfin/background trigger definitions; import fixtures verify the complete 92-table inventory and only remove empty later sharing declarations. Watch read counts include the preference query, and retained ready handoff checks compare every stored job, waiter and request field. The snapshot-metrics batch requires its actual 13 cases, and two WAL Store commands use the same documented 8 MiB stack as their source-bound feature Store results. Production schema and lineage guards remain unchanged. Pinned feature-qualified Store check and denied-lint Clippy pass; Astra found no remaining source blocker. The exact `d74076d76` effort run completed 14 affected Store passes and five failures; its one operations guard passed. The failures are the populated current import, retained analysis handoff, v26/v27 stale-marker negatives and v32 recovery layout. Original outcomes are preserved; only changed or failed cases remain owed. |
| R16 | Preserve current SQL-use scope and saturated census negative controls | PR #1064 landed as `13607f68b` after compile-only gate 4998 passed all seven jobs on exact head `e58a44333`. Follow-up to the four effort R13 failures. The production unannotated ceiling remains 237; synthetic controls use the unchanged source total as a zero-headroom test fixture. Named helper bindings retain shadow, alias and mutability refusal, while a deeper unrelated SQL statement no longer claims their lexical scope. Only reviewed completed format expressions and the actual SQLite query wrapper can be treated as independent earlier SQL, with every helper occurrence confined to its modeled format. Three controls cover nested lease SQL, later SQLite mutations/read and unmodeled helper wrappers. Pinned affected check and denied-lint Clippy pass. The exact `d74076d76` effort run completed 17 passes and one completed atomic-publication failure. The original should-panic wrapper interruption is preserved, and its eight missing cases ran once; no successful body was repeated. R18 addresses the sole remaining shape. |
| R15 | Exercise installed Source ownership through actual three-voter startup | PR #1063 landed as `d74076d76` after compile-only gate 5001 passed on exact head `9887e4d19`. Ten runtime fixtures previously applied candidate-only principal DDL while their marker remained baseline 81; the production installed-layout refusal was correct. A scoped test owner now joins three real selected voters through signed invitations and actual redeem/finalize routes, verifies their exact applied voter set, seeds Local rows at baseline, and invokes the existing purpose-master and sequential Source installer at each original rebuild boundary. It verifies false/false/true startup readiness, committed 81/81/82 and each running member's installed-layout readiness. Existing assertions and Store lifetimes remain intact; the independent candidate-only atomic DDL case remains separate. Setup and body deadlines stay inside the owner, all voter shutdowns and auxiliary tasks are awaited, and any failed/uncertain operation retains its complete storage owner. Pinned affected check and denied-lint Clippy pass; Astra's final source review has no remaining blocker. The exact `d74076d76` effort run completed eight passes and two receiver-retirement failures; both lack their now-required sealed ingress metadata fixture. The distinct Source reservation failure still requires diagnosis. |
| R18 | Model completed atomic-publication path SQL without losing inner bindings | Merged with R19/R20 through PR #1069 as `dd270bdf3` after all seven compile-only gate 5020 jobs passed on exact head `a06e00e10`, based on effort `9375ac4ea`. The R16 affected-only run completed 17 passes and one failure: the replicated census misread the completed publication result as a later direct helper binding. The production shape is exact `self.atomic_publication(lease, replacement, vec![(SQL, params), ...]).await?`. A narrow recognizer requires that receiver, call, explicit SQL/params tuples and suffix, and confines every helper to an independently modeled SQL format; each inner statement still undergoes the ordinary census. Three controls cover the actual two-statement publication followed by a read, helpers in routing/params/non-SQL/unknown wrappers, and dynamic placeholders. Existing parenthesis parsing is unchanged through its closing-parenthesis delegate. Pinned affected Clippy passes and Astra found no source blocker. Only the failed census and three new controls await effort execution; the other 17 passes remain source-applicable. |
| R17 | Repair daemon qualification fixtures and custody-refusal classification | PR #1065 landed as `71fef2229` after compile-only gate 5012 passed all seven jobs on exact head `c2571e545`. Six daemon failures are addressed by restoring parent logging targets and their enable guards, classifying changed preparation custody as terminal Refuse, supplying an actual AAC sink, preserving the exact ordinary cached recipe key, registering the cleanup fixture rendition, and generating a real discontinuous source tone. Packet and decoded-frame coverage now require exact delayed spans and 9,600 source samples before the existing 22,400-sample output silence and tone-position checks. The production PCM filter and all custody/cleanup guards remain intact. Pinned affected compilation passes; Astra found no remaining source blocker; All eleven affected daemon cases passed once on integrated effort `9375ac4ea`; the exact native inventory, source archive, binary and serial 8 MiB environment are bound to the preserved log. The separate failed Store diagnostic remains open. The earlier remote timing failure has a preserved isolated pass with its original source; no production fix is claimed for it. The separate Source reservation still fails in isolation; its strict acquire/replay assertion now names unexpected outcome variants without exposing binding contents, solely to support the remaining diagnosis. |
| R19 | Preserve exact historical import, handoff, receiver and lineage fixtures | Merged with R18/R20 through PR #1069 after gate 5020 passed. The exact `d74076d76` run preserved six failed Store cases and the previously missing feature-qualified lineage failure. The current import stages the prequeue schema and restores its exact durable storage-domain and provider-budget rows after actual upgrade, retaining full import assertions. The handoff publishes its actual common worker before the complete retained-receipt comparison. Stale-marker negatives remove later schema while retaining the target duplicate column. Receiver fixtures first require absent-custody refusal, seal the current metadata-only empty ingress route, and retain every grant, owner, deletion and replay fence. The lineage fixture rewinds only empty unactivated sharing and requires ordinary baseline 81 without Source installation. Production guards remain unchanged. Both feature-qualified integration targets compile; Astra and independent source review found no remaining blockers. The seven affected-only effort executions remain owed. The v32 historical-layout omission is separate; its truthful fixture stays intact. |
| R20 | Recognize the exact published fresh-session upgrade layouts | Merged with R18/R19 through PR #1069 after gate 5020 passed. Source and isolated SQLite DDL witnesses prove fresh main v18 and v31 upgrade through the actual drain/recovery ALTERs into two exact raw SQL definitions missing from the closed Local predecessor inventory. Those two definitions are appended without changing any structural, object-count, trigger, marker or installed-Source guard. Frozen historical declarations and one control body exercise both actual additive paths, retain historical row fields and the empty recovery default, and refuse altered owner constraints or an extra index. The existing three-voter v32 fixture is truthful and remains intact. Pinned feature-qualified all-target Clippy passes; final source review and exact committed checks precede publication. Only the failed v32 case and the new historical-shape control require effort execution from this change. The combined follow-up requires 13 affected bodies overall; no task units run. |
| R21 | Bind raw Source reservation fixture capabilities in actual parameter order | The failed-only `9375ac4ea` effort diagnostic returns `Unavailable` for both proposals. Its raw hiqlite INSERT first encounters placeholders as 1, 2, 4, 3; sequential binding therefore writes the boot marker into the heartbeat and the heartbeat as a capability. Reorder the exact SQL and parameters to 1, 2, 3, 4. A six-row witness requires both exact capabilities for the three actual nodes, integer timestamps equal to the captured heartbeat and each node heartbeat. Candidate-only schema and all production custody, capacity, replay and release guards are preserved. PR #1071 landed after exact-head compile-only gate 5028 passed. The failed-only effort `2a5122a34` execution now passes acquisition and capacity controls, then fails at ordinary activation because its candidate-only schema has no installed Source marker; R23 preserves that refusal and separates the runtime fixture. |
| R22 | Close exact census inventory and historical fixture rewinds | The 13 affected bodies ran once on effort `dd270bdf3`: ten pass and three fail. The remaining census reaches its final inventory check and reports ten genuine fragments rather than the documented eight; the two reconcile guard/lease fragments are documented and the exact count updated, preserving every placeholder validator and unchecked ceiling. The populated import must remove v92/v93 additive columns before replay from v70; local guards first require all dropped fields to be default and compatibility tables empty, then reuse the existing helper without changing shared behavior. The v32 fixture's actual SQLite DROP removes the drain comment before re-ADD, producing SQL absent from every published variant; an isolated SQLite 3.53.4 witness confirms this. Rebuild only its empty session table from frozen published v31, preserving all named indexes and triggers. The two real historical production variants remain intact; no synthetic variant is admitted. PR #1071 landed after gate 5028 passed. All three failed bodies pass once on exact effort `2a5122a34`; their original failures and the earlier ten passes are retained. Native binaries, complete inventories, raw logs and serial 8 MiB execution are source-bound. |
| R23 | Separate candidate reservation from installed Source lifecycle | The remaining Source body reaches `activate_source_media_session` with manually rebuilt principal tables and baseline marker 81. The production ownership-layout guard correctly refuses that mixed state. Retain the original native ID and candidate-only stale membership, racing claim, caps, replay, resource refusal and never-dispatched release controls, with explicit marker 81 and no ordinary session. Move the complete assignment/activation/publication/renewal/no-spawn/terminal accounting tail into a new real installed three-voter fixture. It uses actual selected IDs and credential, manager-published metadata-only ingress boot and installer-owned revision material, retaining held counts 8→7→6 and every late refusal/rollback/replay assertion. No physical producer is started. Independent source review confirms the tail is unchanged except real identity substitutions. PR #1075 landed as `dd742d66c` after all seven exact-head compile-only gate 5042 checks passed. The two effort-only bodies ran once: the candidate passes; the installed lifecycle reaches publication and renewal, then correctly refuses no-spawn release of unsealed custody. The original failure and source/binary/inventory bindings remain preserved; R24 repairs only the installed fixture. |
| R24 | Seal actual empty installed custody before SQL release controls | The installed lifecycle fixture admitted no driver or producer but never sealed its actual ingress ledger. Read that exact retained empty ledger and seal through the public production CAS before the no-spawn rollback trigger and before the live-route terminal refusal. Preserve an explicit unsealed refusal, every foreign-resource, rollback and replay control, and held counts 8→7→6. No production guard changes and no physical closure claim. PR #1078 landed as `314734701` after all seven exact-head compile-only gate 5050 checks passed. The failed-only installed body then passes on that exact effort with feature-qualified binary, complete 368-ID inventory and serial 8 MiB execution; the candidate pass and original failure remain preserved without replay. |
| R25 | Supply JVM platform time and dispatcher only in the watched read-after fixture | The original selected watched-write pass predates the diagnostic observer added to the shared authenticated client. Its `Net.client` probe now invokes Android elapsed time and owns a Main scope on the plain JVM, invalidating that earlier result. Construct the same production authenticated client with an injected deterministic clock and install/reset the test Main dispatcher inside this one body with `finally`. Retain all receipt advancement, absence and failed-write clearing assertions and real middleware. No production change. Current affected compilation and normal hooks pass. Exact-head gate 5060 passes all seven jobs and PR #1080 lands on the effort. The missing/invalidated-only Android batch on effort `4a44a2279` passes all 40 selected cases, including this fixture, on measured OpenJDK 25.0.2 with original logs/XML retained and no applicable prior passing body repeated. |
| R32 | Authenticate two bounded disjoint unit proofs | Built in #1094 after all seven exact-head gate 5113 jobs passed. Six new refusal/composition controls passed once on integrated effort `ddf2397a4`; no task bodies ran. Both complete partitions validate within the unchanged bounds, and all seven command cohorts plus native original CI logs authenticate. The complete Apple CI tail is preserved alongside its earlier exact prefix. Signed promotion enrollment, final qualification and physical HDR acceptance remain owed. |
| R33 | Retain the two original Node preflight scripts | Built in #1095 after all seven exact-head gate 5116 jobs passed. Five new controls and the one source-invalidated YAML reader passed once on effort `60715bd54`. All 15 current-source compiler actions and 266 filtered listings completed without bodies; both final bounded partitions and their shared structural budget validate. Signed enrollment exposed Forgejo's unpaginated per-issue comment endpoint; original enrollment bytes and quota failures remain preserved. |
| R34 | Retrieve every signed evidence comment through real pagination | Build a Forgejo-only scoped reader using the repository comments endpoint, which applies pagination in SQL. Authenticated issue metadata and exact issue/pull URLs scope the selected comments; raw repository batches determine continuation before filtering. Duplicate identities, unknown filters and existing byte/page/request bounds still refuse. Five new controls await first execution on the main-bound effort; no old unit replay is indicated. |
| A01 | Current-main fleet build and physical Profile 5, Profile 8 and Profile 7 FEL acceptance on web, Android and Apple TV | Owed after qualification/deployment. Retain startup, stalls, seek/reopen report clearing, stats, screenshots and exact effective-processing logs; repeat with both preferences off. |

Tasks integrate serially through pull requests into the effort after the
manually dispatched Effort development gate. Freeze task merges, incorporate
current main, and qualify the final main-bound candidate before promotion.
Private media, real infrastructure names and screenshots remain outside the
tracked tree. Numeric/helper tests do not establish independent mastered-film
fidelity or physical-display acceptance.

The operator directed that subsequent unit execution run only on the main-bound
`effort/dv-hdr-repair` candidate. Task branches receive compile, lint and static
checks; new regression cases are compiled and inventoried there, then executed
with the integrated effort qualification. The operator explicitly approved an
optional `compile_only: true` dispatch on the task Effort gate, which leaves
history/catalog policy, formatting, web static contracts and every affected
compiler enabled. It creates no unit receipt and skips only Python unit
execution and its receipt/oracle setup. The default dispatch and main
qualification retain all their existing checks. Earlier checkpoint test results above
remain attributable to their original trees and are not relabelled.

Qualification 4954 on `59440187d` (tree `95c55dbc2`, base `b2adc5f78`)
passed 359 validation and 1,021 operations cases. The remaining web unit
commands passed once on that effort, excluding the two already executed in
preflight; two sandbox-denied loopback fixtures were retried individually.
Android JVM/lint and arm64 package smoke passed. Apple ran 994 iOS cases and
reported four assertions across three methods, diagnosed as the R06 fixture
error. Other failures include a stale transport count (70 passing cases versus
67 expected), a topology fixture constructing a second already-bound manager,
an Android emulator startup failure before any instrumentation ran, UI structural
golden drift, registry setup, and compiler/runtime deadlines. The amd64 package
failed because the Fontconfig source URL returned HTML instead of the locked
archive; R11 reproduces the original archive without changing its pin. The partial Store
run retained named passes and three failures before timeout. These failures
remain preserved; none is a passing whole-lane receipt. Current main advanced
with the paused reopened-seek change. Effort `a7fa0053e` contains the incorporated main `5874dbb59` and the R06–R11
landings. Current-source applicability and complete inventory closure remain open. R07 repairs
a reviewed positioned-resume gap in that change. Successful named unit evidence
must remain source-bound; missing and invalidated cases need focused execution.
No repaired fleet build or physical HDR acceptance is claimed.

The subsequent effort-only source archive at `2d3695541` completed the core
target with 1,861 passing cases, seven source-audit failures and four existing
ignored cases. Its enclosing 30-minute workspace invocation then timed out
during later targets; completed passes are retained, and missing cases remain
explicit. The separate feature-qualified Store continuation completed with
173 passes, 65 failures and one existing ignored measurement helper among its
239 selected cases. Its outer driver also refused the result; neither execution
is a passing lane receipt. A single previously failed media contract passed
on an isolated retry after stale source-only extractions were removed to
restore the compiler's fixed storage allowance. Original failures and exact
source/binary bindings remain preserved. R13 repairs the core source audits;
the remaining Store failures and missing qualification cohorts still require
diagnosis and bounded effort-only continuation. No repaired fleet deployment
or physical acceptance is claimed.

All 22 selected new or changed Python regressions from R08, R10 and R11
passed once on the integrated main-bound effort `a7fa0053e`, with no failures
or skips. The earlier preflight suites were not replayed. This focused result
does not close UI captures, Android device execution, package qualification,
Store/runtime failures or the final promotion receipt.

The resource retry completed all 35 selected previously failed Store cases
successfully, retaining the distinct earlier isolated media pass. The feature
Store inventory now has 335 passing, 29 failing and three ignored named cases;
R14 addresses 18 of those failures and invalidates one earlier v23 fixture pass.
The ordinary workspace Store continuation ran only its 43 previously missing
cases and passed all 43, completing its 264-case source-bound inventory without
replaying the first 221 passes.

The WAL continuation retained 125 previously completed commands and executed
126 missing commands. All native test bodies passed, including 13 cases in the
snapshot-metrics batch; its wrapper failed because it expected ten, and that
failure remains preserved. Two Store commands were withheld from duplicate
execution: their existing feature-qualified results can be adopted only after
source, inventory and runtime-environment compatibility are proved, including
the explicit stack setting now recorded by R14. This evidence does not claim
a successful complete WAL lane or a promotion receipt. Remaining Source
fixtures, other qualification cohorts, deployment and physical acceptance are
still open.

R12 landed as #1082 on effort `2d0d58648` after all seven checks in task gate
5081 passed on its exact head. Its 44 new controls ran once on that effort:
43 passed and one errored because its negative fixture updated a path-map key
instead of the nested hash. The correction is committed for failed-only
continuation. Fourteen source-dependent existing controls and three incoming
main ownership-count checks also passed once. Original failures remain in the
private capture set; these results are not a final qualification receipt.

R26 adds typed Python/JVM source cohorts and independent current compiled JVM
discovery. Compiled membership is explicitly separate from historical runtime
outcomes. The collector uses the pinned Android built-in Kotlin output and
the Java source-set output. A parser-only check reads the actual warm compiled
files and discovers their 1,145 methods; final current-source discovery remains
required. It retains the original opaque full-command successes, measured
named overlays, source-impact reviews and failure chronology. Default test
recipes still run normally; adoption requires a complete authenticated proof.
The original JVM XML gaps remain opaque. Eight finite original JVM capture
adapters preserve reviewed source equivalence separately from unknown execution
SHAs and runtime/image fingerprints. The latest 19 actual native XML passes
cover the incoming build-171 diagnostics and shared-library fixtures, with the
original three-test failure retained as a separate witness. Original Python
157-case evidence retains its explicit error and the exact one-case repair;
opaque successful membership never becomes named historical outcomes. That
repair closes only its original source variant. Current coverage still requires
the separately applicable effort fixture result.

Astra's original 51 pending Python source readers reduce to 32 after narrow
input review of the recovered original cohort. The 31 new protocol and collector
controls, corrected previously failed fixture and freshly invalidated YAML
check remain effort-only obligations. The failed task gate 5089 executed no
unit bodies or compilers: live main was not an ancestor of the effort base.
Main through the build-171 diagnostics landing is now integrated normally into
the effort; a fresh exact-head task gate is required. Gate 5092 then found
eight malformed class-prefixed Python regression references in the immutable
R12 landing. Their canonical methods resolve in that exact original tree; the
required permanent erratum records the mistake without changing the resolver
or boundary. Current task references are checked locally before another gate. Complete authenticated
qualification, fleet deployment and physical HDR ON/OFF acceptance remain open.

R26 landed as #1086 on effort `da8d67f18` after all seven compile-only checks
in gate 5093 passed. The reviewed 65 selectors ran once there: all passed,
including the corrected prior failure. The enclosing capture wrapper then
failed its final clean-tree assertion. Its original `source_clean_after=false`,
wrapper exit 1 and native unittest exit 0 remain separate measurements. A
digest-bound custody supplement compares all 5,874 tracked inputs with the
original source archive: bytes, types and Git executable bits match; every
tracked mtime and ctime predates execution. The original dirty-path string was
not captured. Overlapping Kotlin compiler residue is an inference, not a
reconstructed observation. R27 permits only this exact binding/supplement
pair and refuses other false-clean captures. R27 landed as #1087 on effort
`52dfb9128` after all seven compile-only checks in gate 5096 passed; its three
new controls then passed once with native and wrapper exit 0 and clean source.

Both current JVM source sets compiled successfully without executing tests.
Independent compiled discovery on `da8d67f18` finds 1,149 methods in 664 class
files; these are membership facts, not runtime outcomes. Full authenticated
qualification, deployment and the 18 physical HDR ON/OFF cases remain open.

R28 preserves native Apple evidence whose original text omitted a terminal
line: the whole saved Xcode result tree reports the missing method as Success.
Explicit Apple-only class and native-result formats retain original namespaces,
source/runtime review and exact raw trees; they do not invent a text terminal or
turn namespace normalization into cross-platform execution. Strict hierarchy,
target/URI/name agreement, failure counts, duplicate identities/JSON keys and
traversal bounds refuse malformed evidence. Version-two evidence may share
digest-bound embedded logs and source manifests, retaining every source, path
hash and affected-ID check within the existing size limit. R28 landed as #1088 on effort `1d489c2ba` after all seven exact-head
gate 5099 jobs passed. Its six new controls passed once there with native
and wrapper exit 0 and unchanged clean source.

On effort `52dfb9128`, the complete Python components validate 443 validation
and 1,042 operations memberships; Android JVM validates 1,149 memberships,
including 64 actual named passes and 1,085 opaque historical memberships. Store
and lineage components validate their 368 and one native inventories. Live
original CI source, scope, job, command and log authentication passed for these
components. Astra's core source review accounts for 1,899 methods, with 1,895
actual passing results and four unchanged ignored cases. The one daemon
diagnostics projection with no original native result passed once on this
effort. Apple compilation and enumeration find 1,029 iOS and 1,013 tvOS unit
methods without executing bodies. These are component facts: remaining full
retention, non-unit qualification, promotion, deployment and physical HDR
acceptance are still open.

R29 landed as #1089 on effort `630c4f978` after all seven exact-head
gate 5102 jobs passed. Its ten new controls passed once there with native
and wrapper exit 0 and clean stable source. It preserves the complete original
daemon capture, accounts separately for its single retired passing method,
and requires the current successor to pass later. Only the exact reviewed
source and log can use that retirement rule; duplicate enrollment refuses.
Bounded version-two structural references address a measured 5,210,534-byte
partial proof without raising the 4 MiB attachment limit. Expanded suites
and commands retain their original validation and authentication. Unique
structural data is limited to 32 MiB, individual objects to 4 MiB, traversal
to 64 levels and 131,072 nodes per object, and a proof to 524,288 nodes.
Iterator traversal stops before consuming a broad child list. Generic and
typed CI authentication share one 64 MiB raw-log cache; bounded decimal
identities convert to integers before live authority checks. Original
failures, source invalidation and current compiler obligations remain.
R30 landed as #1090 on effort `bf1eee9c4` after all seven exact-head
gate 5104 jobs passed. Its eight new controls passed once there with native
and wrapper exit 0 and clean stable source. Exact task source `c3aa37e86`
also passed the source-only Linux Rust 1.97.1 compile and native enumeration
of 197 Openraft methods; no unit bodies ran in that compiler check. Complete qualification, deployment and physical HDR
acceptance remain open.

R30 recognizes only the exact committed Openraft metadata/test wrapper and
the unique `openraft` 0.9.25 registry entry with its checksum in both locks.
The descriptor pins the archive, own manifest and own lock, including its
default debug-2 test profile. Current compilation authenticates all package
source bytes and copies only archive files into an owned directory. Existing
registry `target/` build outputs are neither imported nor compiled. Bounded
file-descriptor reads and lazy directory traversal reject growth, aliases,
extra source, excessive entries and depth. Current native artifacts must
name the owned lib source, exact package, feature graph, profile and discovered
selector. The original WAL-237 capture keeps its whole shell wrapper, original
sidecar, preserved binary and compiler fingerprint, native exit and complete
terminal; the adapter does not retrofit new command/exit markers. The actual
original capture and all 246 archived source files passed parser/source
inspection without compiler commands or unit execution. Remaining Rust
workspace evidence validates 472 passes and one ignored case, retaining three
empty targets only as compiler facts. Those source-reviewed component checks
do not constitute complete qualification or physical HDR acceptance.

All R29 and R30 new controls have actual passing effort results. The daemon
component validates 4,192 passes and 37 ignored cases with its single retired
original pass accounted separately; original execution events prove the
failed-only PCM attempt preceded R17 closure. Remaining workspace evidence
validates 472 passes and one ignored case, preserving three empty targets
as compiler facts. Complete source-reviewed components retain every original
raw capture, unknown measurement and failure disposition. The frozen final
candidate still needs the complete authenticated unit union, current native
command discovery, non-unit qualification and promotion receipt. Fleet
deployment and the physical web/Android/Apple TV HDR ON/OFF checks remain open.

R31 preserves the complete original eight-pass R30 capture when mocked compiler
stdout interleaves one verbose unittest terminal. Its parsing projection is
limited to the exact raw digest, effort source, validation kind and measured
binding; only the known six-line span and standalone `ok` are collapsed for
the strict named-result parser. Original proof bytes remain unchanged and no
mocked compiler output becomes compiler evidence. The two new refusal controls
await an integrated effort execution; the eight passing R30 bodies are not
repeated. Complete qualification and physical acceptance remain open.


### R32 — complete evidence can use two authenticated bounded proofs

The complete retained Core, daemon, workspace, Apple, Python/JVM and Store/WAL
evidence exceeds one 4 MiB upload even after real filtered native enumeration
and existing structural deduplication. Astra found no sufficient lossless
single-envelope representation. A two-child manifest preserves both complete
canonical partitions with the existing per-attachment limit. Its digest,
attachment identity and writer attestation use the ordinary enrollment path.
Each child is independently reauthenticated by the existing verifier, including
at final qualification; adoption is published only after both pass.

The bundle reader belongs to [promotion_unit_bundle.py](../../validation/promotion_unit_bundle.py),
with the six focused controls in
[test_promotion_unit_bundle.py](../../tests/validation/test_promotion_unit_bundle.py).
The shared [structural budget](../../validation/unit_evidence_objects.py) charges
both child-local decoders. Inline child slots, recursive bundles, identity drift,
duplicate attachments/digests, conflicting table entries and overlapping unit
authority are refused. Unit outcomes, original provenance and source
applicability remain governed by the existing child validators. The manifest
does not certify deployment, runtime campaigns or physical HDR playback.

### R33 — preserve complete original Node preflight outcomes

The original qualification preflight already passed both player-input and
player-DOM scripts. The ordinary journal adopter authenticates a different
workflow and cannot adopt this manual full-CI capture. The finite
[Node reader](../../validation/retained_web_preflight.py) binds the original
source, run, successful job, full raw digest, unconditional workflow commands
and recorded Node version. It requires all 18 and six custom success lines,
plus the DOM script's one native TAP test and complete successful terminal.
Original binary hash, flags and individual script exit codes remain unknown.

The current script bytes must match the original declarations. The ordinary
changed-input manifests still enumerate every changed dependency, including
its before/after hashes and affected script IDs. Astra's applicability review
found the thirteen executed function slices and the presentation slice
unchanged. An affected script cannot borrow its old pass. Four focused
[Node controls](../../tests/validation/test_retained_web_preflight.py) cover incomplete
or contradictory native outcomes, foreign execution provenance, current
source drift, dependency invalidation and the exact workflow guard.

Only the two Node commands are guarded by the verified `retain_web_preflight`
output. The preflight job and its other contracts still run; web layout,
browser playback and all physical HDR checks remain required. This change
adds no new runtime claim and executes no old Node body.

R32's preserved interpreter binding records the actual invocation alias and the
measured canonical interpreter path separately. The ordinary named-capture
reader previously required literal path equality. A finite branch in
[nonrust_unit_cohort.py](../../validation/nonrust_unit_cohort.py) accepts only
that complete binding digest, original source and raw log, validation kind and
exact recorded invocation/interpreter strings. Runtime, source archive,
origin argv, selectors, count and completion checks still apply. It neither
resolves a path during verification nor claims an at-execution symlink proof.
The original binding and six-outcome log are immutable fixtures for one new
[refusal control](../../tests/validation/test_python_invocation_notation.py).
New captures invoke the measured canonical interpreter directly.

### R34 — Forgejo evidence comments require repository pagination

The installed server returns all issue comments from its per-issue endpoint,
ignoring `page` and `limit`. Enrollment of the first validated partition
therefore refused its 72-row response. The original asset, 71 signed chunks,
attestation, raw responses and failed enrollment log remain preserved.

The [scoped reader](../../validation/issue_comment_pages.py) obtains issue
identity and creation time from authenticated metadata, then reads ordinary
comments from the genuinely paginated repository endpoint. It starts one
second before issue creation, compares exact API/browser issue or pull URLs,
and decides pagination from every repository row before selecting the issue.
It refuses repeated identities, foreign scope URLs and unknown filters. The
4 MiB response, 50-row page, 20-page and 200-request bounds are unchanged.
Timeline pagination is unsuitable because that handler can filter rows after
pagination. The generic API pagination loop remains unchanged.

[Five controls](../../tests/validation/test_issue_comment_pages.py) cover late
target comments after a full foreign page, metadata and URL substitution,
duplicate/page limits, empty/filter cases and unchanged non-Forgejo/generic
paths. Their first body execution is reserved for the integrated effort.
Whole qualification, deployment and physical HDR ON/OFF acceptance remain open.

### R35 — Reclaim mobile build counters above current main

The full promotion's evidence scope passed. Its release-version check refused
Apple 238 and Android 171 because current main already owns both counters.
The repair claims Apple 239 and Android 172, regenerates the current claims,
and updates only the current JVM build-input digest. Original JVM captures
and historical build pins retain their actual sources and bytes. Fresh Android
compilation and discovery remain required; deployment and physical acceptance
remain open.

### R36 — Use protocol identities and await asynchronous snapshot publication

Promotion 5127 passed evidence scope, mobile counters, policy preflight and
Android compilation/lint. Its real topology campaign then refused synthetic
`node-N` membership identities at the existing canonical UUID check. The
harness now maps each numeric Raft ID to one deterministic canonical UUID in
bootstrap, join, ownership, authentication and removal. Numeric process paths
and hostnames remain separate. Production reference validation is unchanged.

The voter campaign failed during warm-up and the learner campaign failed in
cycle 15 immediately after their bounded 768-write bursts, without awaiting
asynchronous snapshot publication. Later completion was not observed. The
harness now polls after the burst
under the same original cycle deadline, without additional writes. Newer-index,
purge, retained-log and actual transport proofs remain required. Three focused
controls cover UUID admission, delayed publication and unchanged-index expiry;
all three pass once on integrated effort `0e6bc1c98` on macOS. The two
transport controls also pass once on Linux with actual default scheduling
(0.20 and 0.75 seconds), without replaying the original 70 applicable passes.

The original failures remain recorded. Compiler timeouts, daemon readiness,
web focus baselines, Android transport focus and the package source download
still require separate diagnosis or qualification. No repair deployment or
physical HDR acceptance is claimed.

### R37 — Preserve Android transport option adjacency

Promotion 5127 ran 78 Android instrumentation cases; the shared transport-order
case failed because the diagnostics action intercepted Settings → Info focus.
The report action now follows optional PiP, preserving Tracks → Settings → Info
→ PiP and keeping diagnostic reporting available with its original generation
binding. The existing failed case remains the regression; no duplicate body is
added. Android build 173 and current generated claims identify the correction.
Only the current JVM build-input digest changes; historical captures retain
their original inputs. Local application and instrumentation-source compilation
pass. Physical HDR acceptance and integrated failed-only execution remain open.


### R38 — Serialize daemon activity fixtures and refresh the UI baseline

Promotion 5127 passed the seven activation cases and two activity parsers,
but its six concurrent real-daemon activity cases exhausted their unchanged
readiness budgets. The failed-only Linux continuation passed one case in
74.99 seconds and the remaining five in 303.73 seconds with serial libtest
scheduling. It repeated none of the applicable passing cases. That probe
used a different FFmpeg build from CI; it supports startup contention without
proving the complete CI environment or a GPU cause.

Only the activity integration invocation now uses `--test-threads=1`.
Activation retains its existing scheduling and process mutex. Every activity
case retains its four-worker Tokio runtime, concurrent requests, original
readiness deadline and assertions. Full promotion still executes this lane.

The normal self-hosted UI baseline update captured all 78 routes, layouts and
viewports: 10,597 structural facts with no console or page errors. Reviewed
changes add the existing index-coverage action, sign-in/player reporting
controls and Developer reporting card, with their resulting focus positions.
Request counts and existing focus labels are unchanged. The capture used the
previous committed effort's server binary; its embedded web source is identical
to the current effort. The ordinary qualification comparison remains required.

Original failures remain preserved. Main promotion, deployment and physical
HDR acceptance remain open.


### R39 — Capture missing native cases with their actual default scheduling

The first current Linux cluster-check compilation and native listing after
R36 passed with no unit bodies: 145 full cases, including 72 transport cases.
The normal exact-count guard and its existing operations control now require
72 rather than the previous 70. The UUID control's focused macOS pass is
separate; only the two new transport controls enter this Linux cohort.

The existing local-binary evidence adapter previously accepted only serial
single-case execution. Its additional default-scheduled form requires one
exact selector, matching canonical scheduling, the recorded absence of
`RUST_TEST_THREADS`, an unambiguous allowed argv and one complete native result
terminal. Source archive, compiler fingerprint, target, features, binary
custody and complete outcomes remain required. Existing serial records and
their schema are unchanged. The existing runtime mismatch control gains
positive and refusal cases; its first changed-body execution and the changed
operations count control belong to the integrated main-bound effort.

This permits the two genuinely missing Linux outcomes to be captured without
replaying 70 applicable passes. It invents no Linux result from a macOS pass
and does not turn serial scheduling into default scheduling. Current proof,
full promotion, deployment and physical HDR acceptance remain open.


### R40 — Give full-source compilation the established effort budget

Promotion 5127's compile-only Rust lane exhausted ten minutes without a Rust
diagnostic. The same source compiled successfully on another allowed runner
in the R37 effort gate: 16 minutes 51 seconds including setup and vendor
Clippy. Its actual service had a three-core CPU quota and the existing
six/seven-GiB memory thresholds; read-only samples showed continued compiler
progress and no new OOM events. These observations do not prove a CPU or
memory root cause.

Only the full workflow's compile-only Rust job now has the existing effort
lane's 20-minute budget. Compiler commands, warnings, required gates and all
runtime/admission deadlines are unchanged. The existing aggregate-CI contract
now asserts that job-scoped budget. Its first changed-body execution belongs
to the integrated effort. The obsolete follow-up gate was cancelled while
its expensive compiler lanes were still blocked, before dispatching the new
exact candidate.


### R41 — Bound complete retained-evidence verification separately from diff selection

The final `254f6d88a` source compiles and lists all current native memberships
without unit execution. Its four source-invalidated Python controls pass once.
Both bounded proof partitions pass the actual validator; all 297 predecessor
nonstructural objects and 272 unique command authorities remain conserved.
Authenticated live enrollment passes in 319.38 seconds on the controller.

Promotion 5136 then exhausts the scope job's two-minute deadline while verifying
that evidence, before the compiler/runtime fan-out. The original failure is
preserved. Expected permission-lookup refusals use the existing explicit owner
enrollment; they are not an evidence-rejection diagnosis.

The scope and final-receipt jobs each get a ten-minute orchestration budget.
The final receipt repeats the same complete live authentication, so fixing only
scope would defer the identical deadline failure until after the runtime sweep.
Both use literal values to avoid relying on Forgejo's expression-level fallback
semantics. Verifier commands, immutable source binding,
writer authorization, API timeouts, request/byte/node limits and all compiler
and runtime deadlines are unchanged. The existing aggregate-CI contract checks
both metadata-job budgets; only its changed body is owed on the integrated
effort. Current exact-tree qualification, normal fleet deployment and physical
HDR acceptance remain open.


### Current-main integration — Live TV ready-burst fairness

Main advanced to `ac01c7ab8` during the journaled R41 evidence enrollment. The
exact-base guard stopped before further enrollment or promotion dispatch. The
pending upload outcome and original logs remain preserved. The effort integrates
main's ready-burst scheduling fix and two joined-reader regressions; the producer
ownership census combines both reviewed additions at 959 sites. No HDR behavior
changes in this integration. Current-source compilation, native membership and
applicability review must precede promotion. Applicable existing unit passes are
retained; only genuinely missing or invalidated outcomes may execute. Deployment
and all physical HDR acceptance remain open.


### Current-main integration — P5 rolling proof and QSV shipping package

Main advanced again to `a0b8cb72c` before the next native-cluster capture or
promotion dispatch. The effort integrates its strict rolling dvh1 fragment
validation and QSV shipping dependency fix without a merge conflict. Existing
Live TV source-bound captures remain original: 24 genuinely invalidated daemon
cases pass once in 92.46 seconds on `bcd90828e`; the integrated ownership control
passes once in 16.523 seconds. Five recovered incoming Rust passes are retained
with their actual Mac origin and unknown execution commit explicitly preserved.
Current-source applicability review must distinguish those measurements from
new P5/QSV outcomes. No full promotion or repair deployment is claimed; physical
web, Android and Apple TV ON/OFF acceptance remains pending.


### R43 — Preserve the intact P5 capture through its finite test retirement

Current main replaces the old retaining-copy refusal regression with the
strict rolling-options publication/unsafe-fragment regression. The original
three-outcome capture also contains two still-applicable fragindex passes.
It remains whole: no cropped log, renamed outcome or invented current test ID.
A second finite source/raw-digest/retired-ID/successor policy accepts exactly
that original PASS and requires a later applicable successor PASS. Both known
retirements may coexist; duplicates are refused per exact policy. The original
retirement, raw-outcome equality and failure/successor ordering stay intact.
The representative source of the incoming old capture remains distinguished
from its unknown actual execution commit.

Two additional finite Python capture identities preserve the original named QSV
shipping checks and documentation-index repair. Their exact raw digest,
representative source, original tool, count and unknown execution/interpreter
measurements use the existing named-capture decoder; no parser, writer,
request, byte, node or runtime rule changes.

One new coexistence/refusal control is owed on the integrated main-bound effort.
Its eleven negative variants include wrong source/log/extra identity, omitted
sibling outcome, failed or ignored retirement, failed/ignored/earlier successor,
duplicate original and source-invalidated successor. Existing retirement
controls and applicable core/daemon passes remain retained. Current source
qualification, deployment and physical HDR acceptance remain open.


The first R43 task gate (5154) stops in history before any unit or compiler
execution. Incoming PR #1104's immutable API merge keeps its raw corrective
title without the PR number, although all three canonical regression fields
are present. One existing verified-landing record now binds that exact commit,
tree, ordered parents, title and PR. It waives no trailer or qualification
requirement. The original failure remains preserved; the updated exact task
head still needs its normal compile-only gate.


### R44 — Current membership and browser fixtures

PR #1121 lands in the effort as `d3b45761f` after all seven exact-head
compile-only checks pass in run 5202. The fixture uses the daemon database
layout, starts target heartbeats at live removal transitions, and tests a
candidate below the supported bootstrap schema. Production admission,
compatibility, deadlines and storage thresholds are unchanged.

On that main-bound effort, the four affected Rust controls pass once:
readiness after bootstrap/reopen, the voter configuration directory, the
complete request/FTS/preflight protocol, and snapshot evidence across rotation.
The affected Python script-syntax control passes once in 1.007 seconds.
No applicable prior passing body is selected again.

The actual browser fixture passes on `47947df9a`: decoded H.264/AAC frames
advance, terminal failures retain visible feedback after release, and a real
lost DELETE leaves an owner-bound hint. After the real liveness interval,
reload rejoins the same session without an eleventh allocation; final Stop
confirms the tenth release and removes its hint. The effort integration changes
only CI inputs, so the consumed browser, fake API and server inputs are
unchanged. This synthetic fixture is not physical HDR acceptance.

The source-bound cluster serving/failure-drill campaign exits zero on
`73b06b9fc`, with its verified commit supplied through the supported runtime
`GITHUB_SHA` input for the source-only checkout. The earlier missing-identity
failure remains preserved. The later, separate topology campaign reports zero
errors and zero Authority queries but rejects 512 local queries against its
stale 256-query contract. R46 addresses that exact mismatch; no topology
success is claimed from this failed command.

### R45 — Compiler and WAL placement within existing memory limits

PR #1119 lands as `45a8ab1c7` after all seven exact-head compile-only checks
pass in run 5191 and independent source review clears its final patch.
The main and effort compiler jobs and retained WAL compilation use the existing
larger-memory topology queue with one Cargo job. Their 20-minute compiler and
30-minute WAL deadlines, commands, cache policy and retained-unit requirements
are preserved; no shared runner configuration changes.

Read-only measurements distinguish the original guest timeout from later
bare-metal reclamation: general executors reclaim above a 4 GiB soft limit,
while the selected existing executor has a 24 GiB soft limit. The one changed
workflow contract passes once on `45a8ab1c7` in 1.099 seconds. It remains
applicable after R44 and is not repeated.

**How to read these results:** green task gates establish policy and compiler
acceptance. They do not grant a main qualification receipt, deploy the repair,
or establish HDR playback on a physical display.

### R46 — Count both local queries in each bounded catalogue read

Each of the 256 catalogue operations checks the local preference and reads the
item under the same bounded permit. The exact expected SQL count is therefore
512. Runtime checking, artifact validation and the pinned JSON schema use that
count while still requiring zero read errors and zero consistent queries.
The existing schema/validator control rejects the old 256 count and both
one-query deviations, 511 and 513. Latency samples still represent 256 complete
catalogue operations; this does not reduce the workload or hide its preference
lookup cost.

PR #1124 lands as `d326ed2d6` after all seven current-head task checks pass
in run 5221, with current `main` already integrated. The actual three/four-voter
campaign records 256 operations, 512 local SQL calls, zero consistent queries
and zero errors in each topology. Its original source identity is retained;
imported main changes no Rust or topology input. The updated served web source
also passes its complete synthetic browser campaign. Its changed schema control
passes once on main-bound effort `d326ed2d6` under actual default scheduling,
with the native binary and compiler fingerprint unchanged across execution.
Complete current-main
qualification, normal deployment and all 18 physical HDR ON/OFF cases remain
open. Original captures and failures remain intact.

### R47 — Retain measured execution and complete target authority

The existing snapshot-rotation regression passed once on the main-bound effort
with one selected body and explicit serial scheduling. Its original Cargo
output, complete sidecar and compiler artifact are preserved. A separate
read-only observation confirms that the recorded binary hash still matches
its compiler fingerprint. A finite decoder binds those exact objects and
retains the actual serial runtime; it does not invent command headers, remove
the recorded offline argument or label the execution as default scheduling.
Its compatibility policy is limited to this isolated temporary-directory case.

New missing-only native captures for implicit package commands retain the
complete compiled target footprint. Binary stem and fingerprint target kind
jointly select one target, and its native roster must contain the exact
canonical-filtered selector. Default scheduling remains default. A library-only
build cannot claim whole-package compilation, and an auxiliary matching test
cannot disappear from the inventory.

Astra reviewed both designs without executing units. The existing runtime
mismatch control gains substitution, scheduling, partial-capture and target
authority refusal cases; its changed body passed once on integrated effort
`c6d80bf30`.
This decoder work supplies no new playback, deployment or qualification result.

### R48 — Preserve the successful Android instrumentation command

The original promotion's Android instrumentation job succeeded and reported
78 cases. Its complete log has class headings and a summary, but no named
method outcomes. Fixture assumptions mean that this is not evidence of 78
executed passing bodies. A finite adoption format retains one successful
`make android-instrumentation-run` command with 78 reported cases; named pass
and ignored counts remain unknown.

The format binds the original source, successful job, attempt and complete log,
the Android and emulator source trees, shared reader assets, and consumed
workflow and Make recipes. Changed selectors, runner, inputs, scope, raw output
or job status refuse retention. Existing per-ID and failed-aggregate formats
remain unchanged. Astra reviewed the design. The expanded existing aggregate
Android control passed once on integrated effort `d30d16674`; no task unit executes.
Current-main qualification, deployment and all physical HDR cases remain open.

### R49 — Integrate qualified prepared playback without cold HDR admission

Main's continuous-quality batch introduces bounded manual-to-Auto preparation,
prepared-reader draining and capacity-pressure accounting. Integration keeps its
bootstrap geometry bound before HDR's existing complete-row eligibility check,
and excludes both processing probes and drained readers from proactive work.
Actual queued GET obligations and the dedicated bounded processing sampler
retain their respective owners. Session state preserves both processing-root
admission and the immutable preferred acquisition start.

Frozen processing preparation now queues fairly before its first attempt and
marks capacity pressure only after an actual resource refusal. The incoming
cold bootstrap fallback also requires processed candidates to satisfy their
existing executable HDR10/HEVC and production checks. Ordinary cold encoding
can still acquire its first window. Existing mixed-catalog, bootstrap and two
processing-admission controls cover these compositions. Seven incoming Auto
preparation events retain their phase diagnostics under the existing parent HLS
tracing target. The four behavior controls and existing split-child tracing
audit passed once on integrated effort `62f0b6845`. Its fifteen Rust, five Python
and five iOS targeted bodies all pass; current native discovery records 1,906
core, 4,267 daemon, 1,090 iOS and 1,074 tvOS identities. Discovery executes no
unit body. No unit body executes on the task branch.

The normal pinned Rust 1.97.1 integration hook passed formatting, workspace
all-target Clippy, catalog and served JavaScript syntax. Incoming native/web
qualification captures remain preserved with their actual sources; Android's
thirteen command-input projections match the original successful command.
R48's expanded aggregate control already passed once on the effort. Its earlier
two-proof preparation describes the previous main base and cannot qualify this
new combined source. Current-source qualification, deployment and all eighteen
physical HDR checks remain open. Apple source build252 exceeds main251;
Android173 is unchanged. Physical mobile builds and installs remain main-only.


### R50 — Keep original incoming Python CI failures beside their repairs

The incoming continuous-quality main change has two intact Python CI captures.
Run 5249 contains sixteen real failures and two still-needed documentation
passes. Run 5254 executes two validation and fourteen operations repairs. Its
other 1,372 journal entries are inherited from 5249 and supply no new native
execution credit.

A closed decoder for those exact two runs preserves both complete logs, their
original journal artifacts, source identities, actual job dispositions and
ordered failures and repairs. Live authentication checks the historical PR
payload, source-pinned producers, job identity and whole-log bytes. Native
repair frames supply only their sixteen actual outcomes. Numeric suite exits
and the interpreter binary digest remain unknown. Current-source applicability
still decides whether an original pass can be retained. This exception applies
only to the existing main-bound HDR promotion; it authorizes no other CI run.
The existing finite-capture control gains paired-source, chronology, inherited
membership, live-authority and current-invalidation refusals. Its changed body
is owed once after effort integration; no task unit runs.

The subtitle integration adds only browser commands to the complete Makefile.
All twelve other Android command-input projections remain identical. Retention
admits that one reviewed complete Makefile digest beside the original, while
still requiring the original thirteen inputs exactly. Arbitrary additions,
shadow assignments and changed Android recipes remain refused. The existing
aggregate control gains an explicit additive-Make admission/refusal check; its
changed body is owed once on the integrated effort. No instrumentation body
is replayed for this source-identical Android command.

The newer subtitle work on main is being reconciled separately. Current-main
qualification, deployment and all eighteen physical HDR checks remain open.


R50 also reconciles the incoming v111 subtitle schema with historical test
rewinds. The shared test helper refuses populated model receipts, then removes
the new managed tables and triggers before dropping the common queue. Its
existing view-removal loop still removes the replaced resource view; historical
replay restores the historical schema before applying v111. The downgrade
coverage guard records the new migration and checks that refusal preserves a
model receipt. Two deliberate current-end assertions advance to 111; their
historical markers and data-preservation assertions stay intact. Only the
source-invalidated guard, rewind callers, current-version checks and changed
queue SQL validator need fresh observations. Five incoming subtitle Store
functional passes remain preserved, but lack canonical Linux/serial/8 MiB
provenance, so those five canonical observations are still missing.


The incoming subtitle Python suites report 359 and 1,049 cases with one
unidentified skip each. Their complete originals, source binding, executor
receipt and public-oracle manifest remain preserved. A finite, source-reviewed
whitelist admits 52 operations methods whose module, class and helper closure
cannot skip. They receive opaque subset credit only. No validation method is
admitted from that capture, no skipped identity or runtime measurement is
invented, and opaque credit cannot discharge an earlier named failure.
Current-source classification still removes invalidated cases. The existing
finite-capture control covers these exclusions, unknowns, exact custody and
failure-repair separation; its changed body remains owed once on the effort.


Task gate 5288 stopped in history preflight before compilation or unit execution:
the additive main merge duplicated one immutable landing record. The records
have identical commit, tree, parents, title and pull identity. They are now
consolidated with both original reasons retained; all three own-tree regression
trailers remain mandatory. No audit boundary, erratum, waiver or execution
result changes. The failed gate and complete original log remain preserved;
the corrected head requires a new task gate.

The ordinary local `make history-check` now parses the corrected ledger. Its
remaining six diagnostics concern unpromoted effort commits, absent from the
current main ancestry. That invocation has no authenticated effort-task
baseline and is retained as a limited diagnostic; no additional main-landing
errata are added. The real task gate must establish its own live baseline.

### R51 — Restore complete historical replicated fixtures

PR #1139 passed all seven task checks in gate 5291 and landed on the effort.
Current-main integration `1ea377504` passed all fifteen native compilation
configurations plus check, Clippy and formatting, without executing test bodies.
Its seven selected Python cases passed once. The twenty selected Rust cases
produced nineteen passes and one failure: the replicated stale-marker retry
fixture left the two managed subtitle tables behind after removing their view
and triggers. The production shape verifier correctly refused that hybrid.

The test-only rewind now checks that model receipts are empty before any
mutation, then removes those two exact later tables with the existing ordered
extension teardown. The production verifier remains strict. Original passes,
the failed native output and the separate default-target listing are retained;
only genuinely missing or invalidated observations may run on the effort after
this task passes its gate. Final qualification, deployment and all eighteen
physical HDR cases remain open.

The original twenty-command runner kept native output and executor metadata in
separate intact files. A closed reader now binds only that exact fifteen-command
Store cohort to its original native logs, compiler JSON, executor sidecars and
later source-only observation. It admits every original ordinal, including the
failed last command. The missing post-failure fingerprint and source-clean
measurements remain unknown; the later observation does not replace them. No
command headers are added to old output. The existing sidecar control now checks
complete admission and refusal of omissions, reordering, invented measurements,
wrong targets and a missing failure repair. That changed control is owed once on
the main-bound effort alongside the two genuinely affected Store cases.


### R52 — Store repeated applicability data once

R51 passed all seven compile-only task checks in gate 5307 and landed as
`4178879f7`. Its current-source compiler/inventory checks passed without unit
bodies. The two affected feature-qualified Store cases and the expanded
sidecar control passed once on that effort. The earlier nineteen Rust and seven
Python passes remain retained, alongside their original failure and repair.

The required evidence tables exceed the two-child limit even after complete
cross-child deduplication: their unique encoded data totals 10,871,952 bytes.
Dropping original logs or custody is not an acceptable packing operation. A
map-specific column dictionary stores repeated paths, source hashes, affected
identity lists and reasons once. Canonical ASCII index rows reconstruct each
exact source-bound applicability map before the existing path, source-hash and
invalidation checks run. Full manifests and inline version-one proofs retain
their existing forms; this adds no general reference chaining or source
exemption. Signed inputs and cached structural slots remain unchanged.

Dictionary bytes, compact-map bytes and every unique reconstructed map all
consume the existing shared decoding budget. Repeated identity visits are
bounded before constructing a row; expanded bytes are streamed and charged
without refunds. Child-local references, duplicate paths, malformed indices,
source/candidate mismatches and mutated tables are refused. The two 4 MiB
children, 1,024-table ceiling, 32 MiB/524,288-node aggregate and per-object
limits remain unchanged. The current-source measurement reconstructs all 120
maps and preserves every original runtime field: both children fit, using
25,246,265 decoded bytes and 337,798 nodes together. This is unqualified
measurement, not authenticated enrollment or permission to deploy.

Four existing controls cover exact reconstruction, bounds, cache mutation,
shared accounting, generic-suite integration and canonical-command integration.
They remain owed once on the main-bound effort after this task passes its gate.
No applicable Rust, Apple, Android or playback body is repeated for encoding.
Current-main qualification, deployment and all eighteen physical HDR cases
remain open.


### R53 — Declare the generic control fixture's finite lane

R52 passed all seven compile-only task checks in gate 5316 and landed as
`a23a42305`. Its once-only four-control effort run produced three passes and
one error. The generic map control supplied only the synthetic `core` suite
but left the production three-suite Rust cohort active; the unchanged
completeness verifier correctly refused that incomplete fixture. The local
control now declares its finite one-suite cohort while exercising the real
source/path/hash validator. No production cohort or decoder changes.

The original four-case log, error and three passes remain preserved. After
this task's gate and normal effort merge, only that failed existing control
is owed again. Compiler/inventory checks, final evidence enrollment and
qualification remain distinct; deployment and all eighteen physical HDR
cases remain open.
