# Rolling-producer owners: frozen narrative

This is the narrative comment block that preceded `version = 1` in
`tests/playback/rolling-producer-owners.toml`, moved out verbatim on 2026-11-23
and frozen. It explains how the census reached its pre-per-file counts. New
reasons for a count change go in the pull request description, not here and not
at the top of the manifest.

```text
# Current PR #1732 + Main 9c9faf631 source union: 195 existing ownership counts
# across 550 Rust files. Structural census only.

# Historical source census notes from both merge parents.
# PR #1732 and main f45c8d14e source union: 195 unchanged ownership rules
# measured over 549 Rust files in their existing scopes.
# After merging #1727, the inventory contains 1188 namespaced task spawns,
# 2283 namespaced time constructors and 958 process-capable methods.
# Q3 integration source union: pure unchanged structural scan measures 1178
# namespaced task spawns, 2273 timers, 953 broad launch-shaped methods and 401
# lifecycle shapes. Q3 adds one joined request task, two fixture bounds, one
# HTTP status read and one Source-factory wait; no matcher/scope/zero change.
# Static inventory only; no runtime/acceptance claim.
# PR #1803 + Main f45c8d14 conflict resolution: measured the actual merged
# source with the existing structural census over 549 Rust files, preserving
# both parents' owner families, patterns, roots and historical explanations.
# Task/method-spawn/timer/Command/launch-method/kill-on-drop counts are
# 1188/107/2288/369/951/132. These are source shapes, not runtime settlement.

# Native FEL Resume clock. The optional reader drains the rolling producer's
# bounded picture records and keeps draining discarded data until its inherited
# descriptor closes. It owns only the read endpoint and observation channel.
# The existing admitted producer owner still reaps FFmpeg and settles source
# custody. A 50 ms wait caps startup's demand for complete clock records;
# expiry declines the layout and neither stops the child nor proves settlement.
# The shape counter adds one Tokio task, its production and test wrapper calls,
# and the wrapper declaration. There is no additional FFmpeg launch.

# Prepared native FEL startup. One source-copy command is run and reaped by
# the existing bounded background child owner under a source job. The one
# startup staging task holds a scratch writer and may only offer a complete
# batch under the current producer transition and first-publication lock.
# Its timer limits a private wait and does not authorize publication or
# establish process settlement. Rejected objects go through rolling retention.
# Subtitle identity fixture: +1 time shape is the one-millisecond sleep while
# waiting for an observable ctime mutation, bounded by a two-second assertion.
# It runs on the existing test thread; no production work or settlement owner.
# PR1727/main8630 structural union (2026-11-18): preserve both reviewed
# ownership audits below. Compared with actual main, five task shapes are
# joined test workers/controllers in decode_facts, source_generation,
# source_input and vodencode_tests. Five observation deadlines are the three
# DV supervisor bounds, source custody receive and admission init fetch.
# Four Command constructors are two fixture policy-command builders in
# copy_analysis/gap_fill_tests (neither launched there), plus the two owned DV
# children described below. Three process-capable matches are the controlled
# decode-facts fixture spawn and the two DV children. The two added kill-on-drop
# guards remain fallback only; existing owners wait/reap before assertions.
# No production task/timer/process shape, pattern, root or exclusion changed.
# Static counts are not runtime/physical-settlement evidence.

# Static raw-retirement fixture follow-up (2026-11-17): one controlled worker
# thread and one scoped controller thread are both joined. The controller's
# five-second acknowledgement bound fails observation only; raw custody remains
# held until actual worker join. Production accounting adds no task, process or timer.
# RootQ B6 source union: measured 1177 namespaced spawns, 105 method spawns,
# 363 command constructors and 952 broad process-capable shapes. The extra
# broad shape is the joined scoped controller's spawn, not another child.
# Patterns, roots, test inclusion and forbidden-zeroes remain unchanged.

# HDR source-lifetime/admission and capture-boundary fixture audit (2026-11-16).
# Authored static comparison of f83484653 with fbe197ed9, using the unchanged
# rust_structural_source extractor and module_roots; not runtime qualification.
# Exact source deltas: namespaced tasks 1166->1170 (+4), method spawns
# 99->101 (+2), time constructors 2251->2256 (+5), Commands 352->354 (+2),
# process-capable methods 937->939 (+2), lifecycle methods 395->400 (+5),
# and kill-on-drop constructors 127->129 (+2). The independent prepared-status
# baseline correction 936->937 from #1752 is preserved below; it is not part
# of this change's +2 process-capable delta.
# All added matches are test-only. Four fixture tasks are joined on their
# normal paths: two controlled telemetry declarations in source_generation.rs,
# SourceInput close_and_wait behind a held reader slot in source_input/tests.rs,
# and the admission-purpose init fetch in vodencode_tests.rs. Source custody
# stays with the physical close/join; observer publication revokes separately.
# Five deadline constructors are failing observation bounds: one controlled
# policy-command receive, one admission init fetch, and three DV supervisor
# observations. A timeout fails the fixture, never proves task/child settlement.
# Two actual child-launch sites in dv_runtime.rs create the stdin-held encoder
# and failing helper. Both have kill-on-drop fallbacks; the encoder is attached
# to ChildJob and registered with ProducerSlot. The registered owner retains
# writer/reap responsibility. Both DV boundary fixtures settle writers, request
# exact retirement, confirm reap and join the helper before assertions. Five
# lifecycle matches are one actual child.wait plus four retained supervised
# HelperCompletion.wait observations, not five new process waits.
# No production task, timer or process-launch shape is added. Roots, patterns,
# extractor, aliases, forbidden-zeroes and unrelated expected rows are unchanged.
# F+ PR1565 ownership review (2026-11-13), independently reviewed against
# main e503f8bf3 with the unchanged rust_structural_source extractor and the
# declared module_roots. Static source ownership evidence only; no runtime or
# physical-settlement qualification is inferred from these counts.
# namespaced-task-spawn 1152 -> 1162 (+10): eight production spawn_blocking
# sites are awaited spool I/O/parsing (source_probe/dv_refit.rs:2), fit solving
# (refit_work.rs:1), disk RPU author/readback (dv_disk.rs:2), and Developer
# spool list/delete/readiness (http/developer.rs:3). They retain their captured
# data/handles through actual closure return; an HTTP waiter disappearing does
# not establish that blocking work stopped. Two refit_work_tests execute tasks
# receive cancellation and are joined on normal fixture completion; their
# twenty-second timeout is a test failure, not child-reap evidence.
# bare-task-spawn 22 -> 25 (+3): one definition and two awaited calls of the
# local source_probe::dv_refit::spawn helper, not detached Tokio tasks. The
# helper delegates to the existing managed child owner, with a clean bundle
# environment, inherited held descriptors and kernel-bounded direct file output.
# namespaced-time-constructor 2211 -> 2219 (+8): one production publication
# retry sleep releases the claim lock so the existing heartbeat can renew;
# the same exact token/authority fences every retry. Six refit lifecycle
# fixture sleeps/timeouts and the real-acceptance output deadline are test
# observations; expiration never establishes process or writer settlement.
# process-command-construction 342 -> 351 (+9): two production commands are
# the managed refit helper and the held-source, cancellation-bound, capped
# first-packet identity probe. Their existing process owners join/reap before
# returning and retain admission/source ownership. Seven commands are fixtures:
# the kernel file-limit child, explicit ignored disk helper, four finite
# nonzero-clock FFmpeg/ffprobe commands, and the ignored acceptance tool helper.
# process-capable-launch-method 927 -> 930 (+3): awaited fixture output() calls
# in disk acceptance, nonzero-clock coverage and the real acceptance helper.
# No new production direct launch/kill/wait, alias, producer-policy owner or
# forbidden-zero shape is admitted. Patterns, roots, exclusions and every other
# structural count remain unchanged. Passing source review is not test evidence.
#
# S5 source custody / VOD Source-copy yield audit (2026-11-09), authored
# structural comparison of origin/main c8f8b5aa9 with 3e17df55f using the
# unchanged rust_structural_source extractor over module_roots. This is static
# ownership evidence, not runtime or physical qualification. Exact count
# changes: task shapes 1113->1121 (+8), time constructors 2152->2164 (+12),
# Command constructors 335->336 (+1), process-capable method matches
# 908->913 (+5), lifecycle method matches 383->384 (+1), and free/UFCS
# process-lifecycle matches 87->88 (+1). The task delta is +3 production
# blocking jobs and +4 fixture tasks:
# source_resource.rs adds actual cache read/publication jobs whose closures
# retain SourceResourceCustody and whose JoinHandles are awaited; cancellation
# of a waiter cannot release the Source body/file custody before closure return.
# source_subtitles.rs replaces the old detached native-text tokio::spawn with
# custody-owned, bounded parsing/input staging; cancellation signals the job
# token and then awaits the actual parser job. Its source body guard stays live
# through blocking work and the reaper still owns registered child/writer
# settlement. The other four task shapes are joined test callers/servers in
# source_actor.rs and http/shared_source_playback/tests.rs. The added
# source_lifetime.rs fixture task drives a real registered Source-copy child
# through a forced first wait error, retry, and actual writer settlement;
# capacity remains charged through the SourceReaper receipt until child wait
# succeeds and writers settle. Timers are +2 awaited Source subtitle operation
# deadlines, +6 bounded actor/HTTP fixture observations whose timeout is a test
# failure rather than settlement evidence, and +1 bounded Source-copy yield
# fixture wait. One test-only `Command::new("sleep")` creates the actual
# registered Source-copy child; ProducerSlot owns its failed-wait retry, actual
# child wait, and writers through SourceReaper settlement before releasing the
# four-thread permit. A test-only `kill(pid, 0)` checks ESRCH after that receipt
# and sends no signal. The +3 process-capable matches are HTTP Response::status
# assertions in the new HTTP fixture; they launch no child. The +1 lifecycle
# method match is an `entry.wait` registry observation, not an OS process wait.
# The VOD driver contention helper adds no detached task or timer: its existing
# owned driver now checks the Source-copy permit as well as Encoding permits,
# allowing an ahead-hold to yield to live work only through the existing
# registered child and writer reaper. No production process-launch shape,
# pattern, scope, exclusion, alias, fingerprint mapping, or forbidden-zero
# contract changes.
# Final source-custody paired counterfactual and HTTP pinned caption follow-up
# (e5ad9e66b) leaves all six module-wide structural counts unchanged. The
# caption control-window factory reuses the existing Source guard and task;
# sparse-input reservation remains held through actual file close. Its paired
# old-predicate counterfactual is test-only. No task, timer, process, lifecycle,
# or registration shape is added by this follow-up.
# The final source/relay fixture follow-up adds one joined HTTP `serving` task
# with a five-second failing bound; it owns only the finite fixture response.
# Typed subtitle bound cases add two awaited deadline constructors. Decoder and
# relay assertions add two broad `.status()` matches (response/builder status,
# not process launches). These cases fail on timeout and do not claim child,
# writer, or Source settlement. Command-constructor and process-lifecycle
# counts are unchanged in this follow-up.
#
# Shared-library playback ownership audit (2026-11-09): authored static
# comparison of main 30f4343ba with candidate 2285907fc, using the unchanged
# rust_structural_source extractor over the declared module roots. This is
# source ownership evidence, not fresh runtime or physical qualification.
# Exact deltas: task shapes 1100->1113 (+13), time constructors 2122->2152
# (+30), process-capable launch shapes 895->908 (+13).
# The sole new production task is shared_artwork.rs's awaited spawn_blocking
# cache read. It owns the open cache file and ArtBodyLease until the bounded
# byte read/hash actually returns; its two-second waiter timeout cannot release
# that closure's lease or prove its join. It starts no media reader or child.
# Seven production timers are that cache-read bound, two fixed rejoin-window
# wakes in the existing Receiver owner, the Source's answer-only deadline,
# and sharing.rs's bounded retry sleep, artwork fetch and scope-permit wait.
# Receiver retirement still uses its retained ordinary End owner; an answer
# deadline drops a waiter, not the accepted Start, source reader or cleanup.
# The other 12 task shapes and 23 timers are tests: shared_receiver_fixture
# adds five tasks/twelve timers, shared_receiver_playback one task/two timers,
# shared_source_playback/tests six tasks/eight timers, and forwarding_tests
# one timer. Server handles retain shutdown channels, caller/End handles are
# awaited, and normal fixture paths join/drain servers and release held seams.
# Fixture timeouts remain failures, not evidence of process reap or settlement.
# Three additional timer matches only moved (one receiver wait, two forwarding
# deadlines); their three removed matches cancel them in the net count.
# All 13 new process-capable shapes are observations: eleven HTTP status()
# reads and two TLS status() reads. No new OS process launch is hidden here.
# Patterns, roots, exclusions, aliases, forbidden zeroes, producer registrations
# and every unrelated count retain their reviewed identities.
#
# Mediated reader/FEL close ownership audit (2026-11-08), relative to
# main 29d61 plus source commits 51bc2fb3e/5cff6f430. Authored static review
# only; this does not assert runtime qualification or legacy-reader settlement.
# FEL QualificationStage owns both Child/ChildJob pairs and SourceInput. Its
# two production tasks retain bridge-close ownership and transfer cancellation
# cleanup through actual child reap and physical bridge join. The added three
# kill calls never substitute for wait; the 100ms reap-error retry retains the
# exact child and job. Two fixture tasks check cancellation ordering; three
# deadlines, two polls and one isolated child sleep have test-only owners.
# Two FEL fixture Commands are self-execs with owned kill/wait supervision;
# the output-only descriptor handoff cannot export the original source FD.
# Reader fixtures add four Commands, one direct spawn and three child lifecycle
# calls. OwnedChild kills/reaps its exact fixture process on Drop. Two bounded
# pipe-drainer threads carry no source/process authority and terminate on EOF
# or receiver closure. A deliberately parked physical-reader thread lives only
# in the isolated fixture process, whose actual death is the recovery boundary.
# The fourth reader thread owns the HTTP source bridge and is explicitly joined;
# its nested consumer goes through the shared owned-child launcher. The added
# reader invalidation sleep is a synchronous 10ms retry with a two-second
# refusal deadline; inability to advance kernel ctime rejects exec, never custody.
# Exact structural deltas: tasks +8, method-spawn +1, time constructors +8,
# Commands +6, launch shapes +1, lifecycle methods +6, kill-on-drop +2.
# Existing lower census scopes, exclusions, aliases, forbidden zeroes and all
# other counts are unchanged. Kill-on-drop and fixture timeouts prove no release.
#
# P42 source-custody recovery ownership audit (2026-11-04), relative to
# edf6d08fa. This is authored source review, including Astra's fixture-cleanup
# corrections; it establishes no runtime or host-reboot qualification.
# No production process/task launcher is added. The four method-spawn sites
# are three owned Linux survivor-fixture children and one bounded re-exec of the
# activation fixture. Each supervisor retains, terminates on failure, and
# waits/reaps its exact child; the Linux subreaper additionally owns the escaped
# reader after the deliberately killed fixture daemon. Its five libc lifecycle
# sites observe/terminate/reap owned fixture PIDs/groups, never a guessed
# production PID. Same-boot recovery retains the live reader's physical charge.
# Four Command constructors and four launch shapes belong to these fixtures;
# eight lifecycle methods retain the actual Child handles through confirmed wait.
# The outer Linux owner observes exit without reaping its process-group leader,
# kills the reserved group before collection on success/error/timeout, and reads
# bounded file diagnostics. The activation fixture has an immediate kill/wait
# guard through polling errors and unwind; neither uses inherited output pipes.
# The nine namespaced time additions are four awaited 250ms retries inside
# existing activation-abort owners (before input is exposed), and five fixture
# deadlines/polls. No detached watchdog or timeout-based release is added.
# Exact module structure updates: method-spawn 85->89; time constructors
# 2062->2071; Commands 321->325; launch shapes 875->879; lifecycle methods
# 370->378; free/UFCS lifecycle 78->83. All other categories, forbidden zeroes,
# structural roots, aliases, registrations and historical audits stay intact.
#
# Playback-startup source ownership audit (2026-11-03): reviewed base
# bd213c81942175f591912e4bb0a2cc10f2a7a195 against source candidate
# e3490c587d5c12fc00eadac6fda9c191d445c0fb. This is authored source review,
# not fresh runtime or physical qualification. Historical audits below retain
# their original candidate identities. Exact equality, roots, alias guards,
# forbidden zeroes and all structural categories stay in force.
#
# The two rolling/session.rs production registrations now call
# AttemptChild::new_with_source_job rather than new_with_job. Both still enter
# attempt_child.rs::new_with_hooks before publication and share its sole child,
# ChildJob, command receiver and terminal receipt. Its supervisor owns SourceInput
# until successful child wait followed by close_and_wait; wait failure instead
# marks cleanup_pending. Drop sends termination to that retained supervisor.
# Extend both registration patterns to cover the new constructor spelling;
# retain 23 registrations, rather than allowing those two owners to disappear.
#
# The module task-shaped increase is +25, composed of 11 actual production
# tasks, 13 fixture tasks and one static child-builder spelling:
# - source_custody.rs::SourceRequest::register has two run_owner launches:
#   ordinary registration, and ambiguous-registration cleanup before input was
#   exposed. Each owns the exact token/Store/receiver independently of its
#   caller, renews its visible charge and resolves journal/proof/deletion.
#   These are deliberately detached durable settlement owners, not joined
#   daemon-shutdown receipts. Channel closure requests settlement; uncertain
#   physical cleanup retains a charge instead of inventing successful reap.
# - source_custody.rs::file_body adds one directly awaited blocking read. Its
#   closure retains the DirectReader Arc (held file and custody) plus actual
#   read activity, so dropping an HTTP waiter cannot release a live read.
# - source_input.rs::launch adds the authority-loss watcher and private server.
#   The watcher owns only observer/close-token state and ends on authority loss
#   or close; its JoinHandle is not retained. The server handle is retained for
#   graceful close_and_wait. Drop requests close, without claiming a join.
# - source_input.rs adds three blocking helpers: close_and_wait closes the
#   descriptor/custody only while owning the read semaphore; read_source's
#   cached-identity check and physical positioned-read closure own that same
#   semaphore and Arc state until they return. Cancellation can drop a waiter,
#   but cannot release the actual read slot. close_and_wait awaits that slot.
# - process/bounded/pause.rs::verified_group directly awaits one blocking
#   /proc census; it observes the caller's retained group, owns no child and
#   never treats a task timeout as proof of stopped/reaped source readers.
# - transcode/manager/create.rs::FallbackAnalysisInterestOwner::drop transfers
#   one exact viewer/source/generation restoration receipt to a detached Store
#   call. Failure leaves durable suspension state; this task does not retry or
#   restart a producer. After transfer to Session, rolling/retirement.rs retries
#   the same receipt after confirmed child retirement, retaining the session.
# - vod/generation.rs::spawn_generation retains prepared-source custody in one
#   detached waiter on the exact ProducerRegistration::wait_confirmed_reap.
#   That wait returns only a confirmed generation receipt, not a deadline or
#   a current-slot guess; the source guard drops only after that receipt.
# - The remaining task-shaped addition is subtitles.rs's static
#   BoundedDiagnosticChild::spawn call. It constructs the existing child owner,
#   not a new Tokio task. Its source input is joined by output or retained by
#   the existing Drop reaper on cancellation.
#
# The 13 added fixture tasks are: bounded.rs's one pause/cancel child driver;
# pause.rs's one coalesced-revision group driver; scan/probe/input.rs's three
# provider/cancellation/active-budget probes; decode_facts/tests.rs's one
# stopped-thread probe; source_input/tests.rs's six range-read, cancelled
# waiter/close, blocking range/error, and processed-input close workers; and
# source_probe.rs's one finite-hold RPC revalidation worker. Their successful
# paths join them; the cancellation fixture explicitly aborts/awaits its waiter
# while the actual read retains the permit until the gate releases. Fixture
# timeouts diagnose failure, not physical settlement or runtime guarantees.
# Existing decode_facts::spawn_configured_probe, subtitle hash workers and
# fragindex's companion scanner changed source plumbing without net new tasks.
# The scanner now receives the caller's cancellation/budget/pause scopes, and
# refused/expired companion collection cancels then awaits it and its bridge.
#
# Other shape deltas, including non-process lexical matches:
# - +3 method spawns are pause.rs's owned fixture child, fragindex/tests.rs's
#   owned companion call, and mkv_sparse.rs's scoped/joined fixture thread.
#   The latter's four added .wait calls are paired barriers, not child waits.
# - +1 bare spawn is run_source_part's passed child factory, not a task. Its
#   operation owner retains child/ChildJob, kills on non-finished outcomes,
#   retries failed wait with capacity retained, then joins input and custody.
#   This adds one raw child.wait().await and one operation-owned 100ms sleep.
# - +4 Command constructors: pause.rs and source_input/tests.rs each add an
#   explicitly killed/waited fixture child; ffmpeg.rs adds held-source metadata
#   builders collected through the existing bounded child owner. The moved
#   fragindex companion constructor is still owned by its joined scanner.
# - +11 launch-shaped methods comprise those three method spawns, five new
#   SourceInput HTTP status observations (one production, four tests), two
#   subtitle HTTP status observations, and BoundedDiagnosticChild::output.
#   Status observations launch nothing; output awaits the existing child owner.
# - +10 lifecycle methods: bounded collector +1, pause fixture +2, DV helper
#   settlement +1, ffmpeg byte-output collection +3, scoped fixture barriers +4,
#   cache run_source_part +3, and subtitle-source refactoring -4. The DV
#   SourceInputSettlement waits for the encoder registration and helper before
#   input close. BoundedDiagnosticChild drains output concurrently with wait,
#   cancels then awaits on expiry, and transfers a dropped unreaped child plus
#   input to its existing reaper. Subtitle passes now share one group-kill and
#   retrying wait owner instead of repeated best-effort kill/wait branches.
# - +4 free/UFCS lifecycle sites: bounded and companion fixture signal-0 probes
#   +2, verified pause group signal +1, fragindex exact-group cleanup sends +2,
#   source_custody's Linux group/platform dispatch +2, minus the three old
#   prodrun/AttemptChild platform-dispatch calls now routed through that helper.
#   Only the unreaped child's owner may signal its fresh group; signal-0 tests
#   are existence observations. Quiescence polling retains the group owner.
# - kill_on_drop changes by +1: two fixture constructors added, while the DOVI
#   pixel probe's inline constructor flag moves to BoundedDiagnosticChild's
#   existing shared builder. Explicit child/read settlement remains required.
#
# Timer changes are awaited by their named owners, not detached watchdogs.
# Net +93 namespaced constructors are exactly:
#   bounded.rs +11; bounded/pause.rs +18; scan/probe/input.rs +9;
#   background_jobs.rs +6; background_jobs/source.rs +8; decode_facts.rs -1;
#   decode_facts/source_session.rs +5; decode_facts/tests.rs +6;
#   fragindex.rs +1; fragindex/tests.rs +2; http/shared_source_playback.rs +1;
#   source_custody.rs +9; source_input.rs +2; source_input/tests.rs +3;
#   source_probe.rs +5; state.rs +2; subtitle_translation/read_attestation.rs +3;
#   transcode/manager/produce.rs +1; transcode/rolling/retirement.rs +1;
#   vod/tests/chunk_03.rs +1.
# Source controller timers bound active time and finite wall age; only verified
# group/read quiescence plus acknowledged source-slot state can grant finite
# quota credit. Slot/authority RPC waits consume active time. Collection expiry
# in decode_facts, ffmpeg and subtitles cancels then awaits the same owner;
# expiry itself never proves reap. The one tokio::time import is pause.rs's
# Instant. Background/source/read-attestation timers pace byte grants, renew
# exact receipts or retry journal/proof/deletion with their owner retained.
# The library/source driver awaits its 250ms pause/reacquisition checks while
# retaining physical admission; state subtitle/group cleanup waits preserve
# the claim until join. Fixture timers only observe those orders and budgets.
# The extra PREPUBLICATION_REAP_RETRY use belongs to the existing retirement
# owner's exact fallback-interest restore after reap, not producer recovery.
#
# Reviewed census: tasks 1052->1077; method spawns 82->85; bare spawns 17->18;
# timers 1954->2047; tokio-time imports 6->7; Commands 315->319;
# launch shapes 862->873; lifecycle methods 360->370; UFCS lifecycle 74->78;
# kill-on-drop 119->120; supervisor registrations stay 23 with expanded coverage.
# Transcode-local tasks 120->121, sleeps 86->88, raw waits 1->2, and physical
# cleanup retry references 5->6 follow the exact owners described above.
# Cleanup/join can remain pending on stuck filesystem/group/Store/journal
# dependencies; these source counts establish no global liveness or shutdown
# join. No new playback verdict timer, symptom watchdog or recovery owner is
# admitted by this audit. Runtime tests/proofs remain attributable to their
# actual recorded source candidates, separate from this static source audit.

# Latest main5ac integration source audit (2026-10-29; 460 Rust files):
# exact counts are 1029 namespaced tasks, 81 method spawns, 1917 timers,
# 312 Commands, 838 launch-shaped methods, 360 lifecycle methods, 74 UFCS
# lifecycle sites and 117 kill-on-drop constructors. Roots, patterns, row IDs,
# exact equality and all forbidden zeroes remain unchanged.
# Native fixture stress replaces the unbounded fork loop with four joined
# workers and 300 barrier rounds: Builder.spawn adds one method site, five
# Barrier.wait calls add lifecycle-shaped observations, and one additional
# Command.status shape is an owned, checked fork/exec. Its removed namespaced
# thread spawn is balanced by sharing_host's joined cancellation-test task.
# sharing_host now uses the existing bounded inherited-environment child owner;
# its direct wait/kill and kill-on-drop sites disappear. Three awaited fixture
# timers replace its old production timeout site (net +2). The announced PID's
# libc::kill(pid,0) is a post-settlement existence check, never a signal.
# One additional transcode timer bounds failure diagnostics after the original
# retention deadline; it neither extends success nor reaps a child. The new
# bounded-process Command constructor is a test of inherited environment.
# Existing lifecycle fixture sites also require raw-symbol correspondence:
# DVR adds one always-ready ServingAuthority; diagnostic route setup adds two
# publication_ready_at_ms fields and one blocked sentinel. These are test-only
# authority observations, not new publication or serving permission.
# Main10ef metadata integration (2026-10-29): retain both parent ownership
# audits. Relative to main859, the incoming bounded-pipe tests add two tasks,
# two awaited timers and three Command constructors; all parent repair sites
# remain. The new diagnostics snapshot tests add two awaited timeout calls;
# these are observation bounds, not detached work or child-reap evidence.
# Two new diagnostics Response::status assertions add launch-shaped matches
# without starting children. The frozen source census totals 836 such matches.
# Patterns, roots, exact equality and forbidden zeroes are unchanged. Final
# source census must include any additional in-flight operator fixtures.
# Main metadata integration (2026-10-29; main 85910425c): preserve both
# parent ownership audits and the repair fixture counts. Main changes since
# 6c8c98c79 affect no Rust/Cargo or served-asset bytes; exact source census,
# roots, patterns and forbidden zeroes retain their reviewed identities.
# Cache regression fixture review (2026-10-29): one added awaited 2100 ms
# settling delay in ffmpeg/tests.rs preserves the production metadata grace.
# It starts no task or child and supplies no reap or publication authority.
# Exact namespaced timer count is 1910; roots, patterns and zeroes unchanged.
# Integration with main 10f45ae5f: retain its two awaited Live TV fixture timers
# and three HTTP response observations alongside the reviewed repair fixtures.
# Final exact counts are verified by the unchanged structural inventory.
# Additional upstream ownership audit (66a33d723 -> 10f45ae5f, 2026-10-29):
# +3 namespaced tasks belong to private sharing, not rolling media producers.
# main.rs starts sharing_host::run and sharing_network::run with the existing
# BackgroundLoopGuard cancellation token. Their JoinHandles are not retained;
# guard Drop requests cancellation, without proving either loop has joined.
# sharing_network::issue detaches one invitation owner from its request waiter.
# It bounds issue_inner at 22s, serializes persisted provider intent/cleanup
# through network_invite_lock, and abandons/cancels the invitation on error or
# lost response delivery before attempting cleanup. The cleanup continuation
# has no overall 22s deadline and is not a confirmed daemon-shutdown receipt.
# +4 timers: that 22s issue bound, the network loop's cancellation-aware 30s
# cadence, sharing_host's cancellation-aware 5s cadence, and its 8s child bound.
# +1 Command/+1 kill-on-drop/+2 lifecycle methods are sharing_host::command:
# the existing background ChildWork launcher registers its child; stdout is
# limited to 256 KiB plus one refusal byte, stderr discarded, and normal success
# drains stdout then awaits child.wait. Returned inner errors await child.kill;
# timeout returns before that branch and relies on kill-on-drop. Neither drop
# nor the deadline proves confirmed reap or descendant settlement on every exit.
# +9 launch-shaped methods start no child: sharing_host has three production
# status reads and four test-only SetupState::status reads, while sharing_network
# reads the same HTTP response status twice. The child above is launched through
# the existing shared launcher, adding no .spawn/.output/.status call site here.
# Additional reviewed totals: 1027 tasks, 1909 timers, 308 Commands, 833 launch
# shapes, 357 lifecycle methods and 118 kill-on-drop sites. Source roots,
# structural patterns, exact equality and every forbidden zero remain unchanged.
# Integration with main 66a33d723 (2026-10-29): retain its four progressive
# subtitle admission timers plus the four test-only repair timers. Exact
# namespaced timer census is 1903; all other merged counts match source.
# Roots, patterns, and forbidden zeroes stay unchanged.
# Unit coverage repair ownership author audit (2026-10-28):
# Base 24d1bad72 -> repair candidate, including its native HEVC fixture.
# Codex coverage_opportunities authored the HTTP/subtitle fixtures and audited
# the exact source delta here; this is author audit, not independent approval.
# Independent Codex coverage_review checked all 15 deltas, source hashes and
# ownership claims on 2026-10-28; that review did not execute the fixtures.
# No production owner, new task site or kill-on-drop constructor is introduced.
# +1 method-spawn/+1 Command/+1 launch is observe_tests.rs's /bin/true child.
# The test transfers it to ProducerSlot::attach_registered_job_owned, settles
# writers, requests exact registration retirement and awaits confirmed reap
# before retaining only its registration identity for observation. This normal
# path is a reap receipt, not a claim of cleanup after every fixture panic.
# +1 Command/+1 launch is hevc_census.rs's synchronous output() observation of
# the production copy argv over the finite four-second 160x96 fixture. output
# owns/drains both pipes and waits for the direct child before returning; it
# has no separate wall timeout/output cap or asynchronous cancellation owner.
# +2 Commands are runtime_tests.rs's real FFmpeg/FFprobe fixture construction.
# Both await runtime::child_output with 15s/64KiB limits through the existing
# clean-environment bounded collector/ChildWork guard; no launcher is added.
# The other +11 launch-shaped matches are HTTP Response::status assertions:
# DVR five, library channels one, Live TV one, subtitle translation API four.
# They inspect response values and start no process or producer.
# +4 timers are test-only: API's awaited 5ms source-verifying poll under a 5s
# observation horizon; runtime's awaited 10ms start-marker poll, bounded by
# its 15s startup timeout, and its 5s cancellation/join observation. The
# operation is pinned in the test, not detached. The latter completes before
# attempt-directory and exact announced PID absence assertions. Timeout/panic
# alone is not confirmed reap. +1 UFCS lifecycle call is kill(pid, 0): an
# existence observation, never a signal, after successful cancellation return.
# +1 lifecycle-method is ReleaseSettlement::wait after the elected fixture
# response owner has awaited complete_release_settlement. It observes a
# completed shared response, not a child wait or an independent repair owner.
# All nine durable-symbol deltas are in media_sessions/cleanup_tests.rs:
# projection completion +1, exact release completion +3, publication fence +13,
# durable safety window +2, blocked sentinel +2, terminal safety window +3,
# shared settlement +1, client release intent +7, terminal cause +2. Real
# SQLite activation/End/projection calls and awaited cleanup use the existing
# exact owner/epoch fence; assertions preserve pending proof and cache authority
# for unreachable peers, early boundaries, wrong epochs and uncommitted End.
# Updated repair-only structural totals before main integration: 80 method spawns, 1899 namespaced timers,
# 307 Commands, 821 launch shapes, 355 lifecycle methods, 73 UFCS lifecycle
# sites. Module roots, patterns, exact equality and forbidden zeroes unchanged.

# Integration review (2026-10-29): preserve both reviewed owner sets below.
# Relative to the native-fixture source: +2 joined selected-stderr test tasks,
# +2 awaited test bounds, +3 bounded test Command constructors and +1 HTTP
# status projection. Exact combined counts are 1029/1910/308/822; all other
# reviewed counts, scan patterns, roots and forbidden zeroes are unchanged.

# Subtitle repair ownership review (issue #1329, 2026-10-29;
# reviewed corrective source 9996d26f4, current tree c4e32f291):
# +2 namespaced tasks are cfg(test) bounded.rs duplex-pipe writers in
# selected_stderr_ignores_large_metadata_and_preserves_split_records and
# selected_stderr_overflow_and_overlong_lines_drain_before_refusal. Each
# writer is explicitly joined after the fully draining reader reaches EOF.
# +2 namespaced timers are those tests' awaited 10s/5s observation bounds;
# they create no detached timer or new production deadline policy.
# +3 Command constructors are cfg(test): bounded.rs first constructs a bare
# `sh`, rejected by absolute-path validation before spawn, then an absolute
# `/bin/sh` collected by the existing clean-environment child owner. The
# third constructor is runtime.rs's actual_multichannel_decoder_keeps_clock_
# frames_past_side_data_log_budget E-AC-3 fixture encoder; child_output awaits
# it under the existing 20s bounded process-group/cancellation/reap owner.
# +1 launch-shaped match is the stopped_create_conflict_exposes_only_a_
# validated_read_handle test's Axum Response::status() assertion. It launches
# no child and grants no producer authority.
# The live stderr task changes from direct drain_capped to an optional
# selected-line drain inside the same single tokio::spawn (net zero tasks).
# Existing command owner, cancellation, process group, kill/reap and old
# caller paths remain unchanged. No watchdog, gate or repair owner is added.
# Counts change only: task 1027->1029, timer 1905->1907, Command 304->307,
# launch-shaped 820->821. Patterns, scan roots, equality checks and all other
# counts/forbidden-shape zeroes are unchanged. Source review is not execution.

# Native fixture ownership review (2026-10-29): four test-only forkers keep
# all 300 immediate fixture executions, with at most 1200 checked child
# attempts. Each round drains child outcomes; stop/barrier release joins the
# workers after fixture failure, and partial creation drops unarmed channels.
# A bounded async-signal-safe pre_exec poll forces the inheritance window.
# Replacing namespaced thread::spawn with Builder.spawn gives -1 namespaced
# task spelling and +1 method/broad launch spelling; no new producer owner.
# Five new .wait() matches are Barrier synchronization, not child lifecycle.
# One timer diagnoses an already-failed retained-copy completion after its
# unchanged 30s bound; it never reaps/signals a child or permits late success.
# Patterns, roots, equality checks and forbidden zeroes remain unchanged.

# Sharing-host and invitation ownership review (2026-10-28):
# main.rs starts sharing_host::run and sharing_network::run with the existing
# BackgroundLoopGuard token. Their polling loops observe cancellation; the
# daemon does not retain/join these background handles, so this is cooperative
# cancellation rather than a joined daemon-shutdown receipt.
# sharing_network::issue retains its state and persisted invitation job while
# its spawned owner delivers an oneshot outcome. Its 22s issue timeout does not
# bound subsequent abandonment/revocation cleanup; the periodic cleanup owner
# polls every 30s. These are node-local invitation owners, not rolling producers.
# sharing_host::command now delegates its eight-second, LIMIT+1 capture to the
# existing bounded process-group owner. Native user/IPC environment is preserved
# explicitly; normal exit, timeout and cooperative cancellation settle the child
# and pipe readers before this directly awaited helper returns. Dropping an
# unrelated caller still uses the bounded owner's best-effort Drop backstop.
# This removes the unawaited timeout/drop collection path and the duplicate
# direct wait/kill/kill_on_drop sites; existing shared-owner counts remain.
# New regression owners: one joined cancellation driver, three bounded fixture
# waits/polls, and one shell constructor testing explicit environment retention.
# One libc::kill(pid, 0) observes only the exact fixture PID to prove it is reaped;
# it sends no signal and introduces no process owner. Nine .status() projections
# (seven sharing-host, two network HTTP response reads) launch no child/task.
# Net measured additions: +4 namespaced tasks, +6 timers, +2 constructors,
# +9 launch-shaped status reads and +1 free lifecycle observation. Patterns,
# scanned roots, exact equality, and forbidden-shape zeroes stay unchanged.

# Live TV upload regression ownership review (PR #1312, 2026-10-28;
# Rust source 9c5aa95df): +2 namespaced timers are the awaited five-second
# fixture observation deadline and its five-millisecond poll. Neither starts
# a task, child, detached timer, repair owner or producer authority. The test
# owns its loopback PutSink and HTTP streams; the segment body is completed
# and its response drained before the fixture ends. The existing PutSink
# Drop closes the receiver and aborts its acceptor.
# +1 launch-shaped match is reqwest::Response::status(), an HTTP assertion
# that starts no process. Structural patterns, roots, exact equality and
# forbidden-shape zeroes remain unchanged.

# DV title analysis ownership review (PR #1301, 2026-10-28; source 7d2d4a9c7):
# +1 namespaced task and +1 Command are DvTitleRide's companion scanner.
# build_attested_pass retains DvTitleScan through the index pass and finish;
# the task owns a cloned held source descriptor, inherited by the child as fd3.
# The existing bounded collector registers ChildWork::background, isolates its
# process group, caps each output stream at 64 KiB, and bounds child runtime.
# Successful collection waits for the child and joins both output readers.
# Lead-output refusal or join timeout aborts and awaits the scan task.
# Dropping the pass drops DvTitleScan and requests task abort; bounded child
# Drop kills the owned group/direct child and schedules its wait on the runtime.
# Abort/drop is not a confirmed-reap receipt before the pass returns.
# +1 timer bounds the finish join. +1 bare spawn is the method declaration;
# +1 method-spawn/+1 launch shape is the same Rust helper invocation.
# No new rolling-producer authority or independent repair owner is introduced.
# Patterns, roots, exact equality and forbidden-shape zeroes remain unchanged.

# HTTPS Barrier::wait adds one test-only joined observation to main’s lifecycle census.
# HTTPS integration 2026-10-25: retain main lifecycle owners and compose the
# reviewed HTTPS additions: 13 awaited test/serving tasks, one owned JoinSet
# spawn, 12 bounded TLS/test timers and seven test/client launch-shaped calls.
# Exact source census remains enforced; no scan roots or patterns change.
# Shared playback cache ownership review (2026-10-21, source 99a968993):
# Codex reviewed the lifecycle sites and their exact census against origin/main.
# Astra independently approved all seven count changes and their lifecycle owners.
# +10 namespaced tasks: one node-local 15s upkeep loop stops on its shutdown
# token; three coordinator owners perform physical inventory and owner callbacks;
# six test-only drivers cover timeout/fencing, cancelled copying and VOD replay.
# Blocking inventory retains its single I/O permit until the scan actually ends.
# Callback tasks retain their single-flight permit after waiter timeout; eviction
# additionally retains its prefix fence through callback completion. Cancellation
# cannot expose the namespace while unlink is running. A timeout does not
# forcibly cancel detached filesystem work. Existing copyseg and
# subtitle copy workers now carry their reservations/pins through physical work;
# moving those constructors adds no production task site or producer authority.
# +19 timers: nine bounded coordinator waits/cadence, two scratch PUT/DELETE
# framing bounds, one post-child pretranscode upload drain and one dependency
# read before VOD eviction; the other six are finite fixture observations.
# +2 Command/+2 launch sites are test-only: a 40s kill-on-drop FFmpeg graph
# output and an awaited finite 16KiB shell writer with a 4KiB child file limit.
# The other +7 launch-shaped methods are HTTP response status assertions.
# +1 kill-on-drop is that same FFmpeg fixture, not a confirmed-reap receipt.
# +1 free/UFCS lifecycle site ignores SIGXFSZ only in the child's pre_exec hook;
# RLIMIT_FSIZE then returns EFBIG to tee's onfail=ignore, preserving the index.
# +1 rendition_build_gate reference takes the existing exact-key gate during
# VOD eviction, before reader/producer/dependency checks; it starts no builder.
# Resulting structural totals: 995 tasks, 1846 timers, 294 Commands, 792 launch
# shapes, 72 free/UFCS lifecycle calls and 111 kill-on-drop constructors.
# Patterns, scan roots, exact equality and every forbidden-shape zero stay intact.

# System comparison ownership review (PR #1243, 2026-10-20):
# The shared launcher/guard was adversarially reviewed by Astra; Sol reviewed
# this exact census delta. comparison.rs adds one background job-owned FFmpeg
# Command, kill_on_drop and awaited child.wait. Its job guard spans that wait
# and the two joined 64 KiB tail readers; timeout/cancellation drop that owner.
# The 60s production run timeout and 600s operator-test timeout replace the
# removed 180s startup synthetic-benchmark timeout: net +1 namespaced timer.
# No new task, producer authority, independent reader or cleanup loop is added.
# Structural patterns, scan roots and exact-equality requirements stay intact.

# Media support ownership review (PR #1239, 2026-10-20; Astra reviewed):
# ffmpeg_support.rs adds one awaited spawn_blocking for local package hashing.
# It owns only paths/hash inputs, no child or producer authority. An already
# started blocking hash finishes even if its awaiting startup caller is dropped;
# pre-read metadata caps are 2 MiB for the manifest and 256 MiB per companion.
# There is no hash cancellation deadline; trusted local package reads reach EOF.
# Three Command constructors are inventory (10s/2MiB), synthetic processing
# (15s/1MiB), and Windows GPU inventory (10s/64KiB). All use the existing
# bounded_command_*_cancellable collector with ChildWork::background: owned
# registration, bounded output, kill-on-drop, and explicit kill/wait on timeout.
# Capture byte limits apply separately to stdout and stderr.
# These boot-only probes are cached by OnceCell; Settings polls launch none.
# There is no session/rolling-producer authority or independent task/reap loop.
# Patterns, roots and exact-equality requirements remain unchanged.

# Desktop native control ownership review (PR #1220, 2026-10-19):
# +6 namespaced task sites in desktop_control.rs: DesktopServer retains the
# listener JoinHandle and cancels/joins it in finish; prepare/commit retain
# the planned-outage operation guard through caller disconnection and finish
# their bounded replicated lease transition; three test-only drivers (HTTP,
# metrics and serving) are stopped and awaited before fixture teardown.
# +1 method-spawn is the listener's bounded (32) JoinSet of IPC connections;
# cancellation requests graceful HTTP closure with IPC_DRAIN, then joins all
# connections before removing the socket. It starts no operating-system child.
# +8 timers: three awaited production IPC/admission/topology bounds, the
# daemon's 30s native store-close bound (timeout records failure, never clean
# closure), two finite sole-voter fixture waits and two fence-expiry sleeps.
# +10 launch-shaped matches: the same JoinSet spawn, membership.status(),
# four desktop HTTP status assertions and four drain HTTP status assertions.
# Nine are readback/assertions, with no process or producer authority. Process
# constructors/lifecycle counts, patterns, module roots and equality stay intact.

# CQ #1192 Linux isolated subtitle reap fixture (runtime.rs, cfg(test) only):
# +1 method-spawn, +1 process constructor and +1 launch-shaped method are the
# same exact-filter self-exec child in isolated_reaper_process. A fresh 0700
# directory, 0600 exclusive nonce/parent/filter guard and one-use claim bind it;
# only that child enables subreaper state before its own Tokio runtime starts.
# Its parent observes/reaps it via four try_wait sites plus one refusal kill
# (+5 lifecycle-methods). The 30s/8MiB protocol bounds trigger owned-group
# cleanup and direct-child kill/reap; a bounded diagnostic tail preserves failure.
# +3 namespaced timers are synchronous 10ms std::thread::sleep polls: the exact
# adopted descendant's 2s cleanup, direct-child 2s refusal reap and parent 30s
# protocol observation. They create no detached task or producer authority.
# +3 free/UFCS lifecycle sites are cleanup_announced_groups' SIGKILL after live
# PPID/PGID ownership checks and two WNOHANG waitpid sites for the exact known
# descendant. One is panic/refusal cleanup; the other requires actual SIGKILL
# and reap before two empty-PGID observations within the original 3s bound.
# Production timeout/cancellation, 300ms/10s operation budgets and non-Linux
# semantics remain unchanged. This records finite test owners, not a general
# post-panic cleanup guarantee. Patterns, roots and exact equality stay intact.

# Current integrated HDR + subtitle ownership census: 968 namespaced tasks,
# 1784 timers, 272 process constructors, 760 launch-shaped methods,
# 67 free/UFCS lifecycle calls and 104 kill-on-drop constructors.
# Both dated ownership reviews below remain; patterns, scope and equality stay.

# R03 task on main-synchronized effort 2b7fd17fc: compose the existing
# 949 task/1740 timer sites with R03's reviewed +7 task/+8 timer sites.
# The scan deadline stays captured at catalogue entry alongside the metadata
# description key; no extra timer or ownership scope is introduced here.

# R03 processed Auto/reopen ownership reconciliation (b647b151f):
# +7 namespaced task sites: the weak one-shot expiry timer, the weak finite
# probe expiry timer, cancellation-independent exact probe-reader detach, the
# bounded ten-second abandoned-instance cleanup, and three test-only owners
# (catalogue timeout fixture, joined one-shot race threads, joined/cancelled
# terminal waiter). Timers hold weak graph/token links; the instance allowance
# bounds physical graphs, and terminal/cancel/expiry retire probe waits before
# detached cleanup. No new OS child is launched.
# +8 timer constructors: two strict recovery/link reads within the inherited
# create deadline, the whole finite refresh on that same deadline, three weak
# expiry/abandoned-cleanup timers, and two bounded test observations (one-shot
# expiry and scheduler hold). No timeout supplies completion evidence.
# The expanded transcode source counts one of those task sites (+1).
# +1 exact-capture rendition build-gate reference pins only the captured Arc.
# +4 dormant purge references settle terminal, idle and abandoned private
# instances plus the unlink/retry fixture; failed physical unlink stays charged.
# +2 broad lifecycle-method matches are joined test-only Barrier.wait calls;
# they launch no process. Scan scopes, patterns and equality bars are unchanged.
# HDR effort main synchronization (main 0da656d3a + effort 998083c85):
# Preserve R01's joined/cancelled admission owner and three test-only deadline
# observations alongside main's cluster and HEVC catalog owner changes.
# Static recount yields 949 namespaced tasks and 1740 timer constructors;
# no scope, pattern or equality requirement changes. No units executed here.
# F6 controlled real-media qualification: +1 awaited fence-loop task;
# +3 awaited sleeps (bounded segment polling, 2468ms loss, stable grace);
# +2 bounded timeouts for segment observation and independent decode.
# +2 Command/kill-on-drop/launch sites are the 30s realtime publisher and
# deadline-bound independent decoder. The publisher is AttemptChild-owned,
# retired and reaped by the actual manager after sustained authority loss;
# the decoder output is awaited and kill-on-drop covers its deadline.
# +1 lifecycle observation is try_wait, proving the negative control starts
# with a live publisher rather than one that naturally reached its30s EOF.
# All additions are test-only in transcode/tests/chunk_06.rs.
# Integrated census: main 5b8f8fe78 plus managed subtitle owners from 75bc459b.
# Reviewed independent deltas compose: spawn 960, timers 1773, constructors 272,
# launch-shaped methods 760, UFCS lifecycle 67 and kill-on-drop 104.
# Preserve both dated attributions, every pattern/scope and exact equality.

# Managed subtitle setup census (main 443c44c -> reviewed single-server runtime):
# +8 namespaced spawns are awaited blocking filesystem work: artifacts.rs owns
# engine inspection, cancellable model verification, reserved temporary creation,
# final hash/sync and corrupt-artifact inspection; subtitle_transcription.rs owns
# decoded-PCM inspection, private macOS executable staging and Advanced header
# discovery. The manager retains its operation through cancellation/cleanup;
# refresh may time-bound read-only engine inspection, never large model hashing.
# +12 namespaced timers: three manager observation/admission bounds; Advanced
# enqueue's bounded model open; the capacity watcher replacing enable watching
# (one removed timer); and eight finite regression startup/completion/Stop waits.
# +5 process constructors: four bounded.rs test-only environment constructors
# (one bare executable is refused before spawn) and the shutdown fixture's
# isolated self-invocation. The decoder/recognizer
# construction moved into Pipeline with zero net change; production children
# retain the existing bounded collector, held descriptors and confirmed reap.
# +4 launch-shaped matches: two reqwest Response::status reads start no process;
# two test-only .output calls collect isolated environment/shutdown children.
# +1 UFCS lifecycle call is libc::kill(pid, 0), proving the managed shutdown
# fixture's child has been reaped; it sends no signal. +1 kill-on-drop belongs
# to that fixture's 30-second bounded isolated self-invocation. No scope or
# structural pattern changes, new detached inference owner or rolling producer.

# PR #1048 continuous-quality census since d6326a764 (exact source deltas):
# Bounded VOD lookup 9 -> 11: response.rs adds the acquisition-start facade's
# definition and delegating call; both spend the original request deadline.
# DurableRouteResolution 101 -> 102: one OwnerTransition assertion in the
# prepared-successor status regression, not another authority.
# Authoritative control-route reclassification 11 -> 9: three status.rs reads
# use the existing media-only classifier; one new chunk_03 control-fence
# assertion retains the original classifier (-3 +1). Control stays fenced.
# VodResponsePublication 30 -> 32: one response.rs and one manager/create.rs
# acquisition-start wrapper signature preserve the same response owner.
# VodPublication 53 -> 54: delivery.rs's acquisition-start playlist signature
# names the existing typed owner-bound result; it creates no producer.
# Namespaced task spawn 951 -> 952: the inflight predecessor-admission fixture
# in vod/tests/chunk_03.rs spawns one driver pass and awaits its exact handle.
# Namespaced timers 1744 -> 1761: two production timeout_at calls spend the
# existing prime deadline (preparation.rs) and ingress deadline (relay.rs).
# The other fifteen are awaited fixture observations: ten sleeps/timeouts in
# preparation.rs's real-lock cancellation regressions; one five-second
# retained-assembly observation in retained.rs; one accepted-wait setup sleep
# in delivery.rs; and two admission/driver timeouts plus one handoff-expiry
# sleep in vod/tests/chunk_03.rs. No new independent timer owner is introduced.
# Process-shaped methods 753 -> 756: three response.status() assertions in
# HTTP regression fixtures (chunk_03 +1, chunk_05 +2), not child launches.
# Patterns, module scopes, exact equality, and zero forbidden-shape counts
# remain unchanged; this records reviewed ownership rather than allowances.

# Subtitle batch census, base0c4ae0aa5 -> reviewed worker/owner delivery:
# +2 publication_ready_at_ms fields belong only to cached_owner_delivery's
# admitted activation fixtures; no new production publication fence is added.
# +6 namespaced task sites: main's shutdown-bound transcription loop; two
# awaited no-follow model open/identity spawn_blocking calls; one receiver
# duplex fixture server that aborts and awaits; and cached_owner_delivery's
# graceful HTTP server and awaited learner-client drain. Its dedicated
# stack-sized test thread is joined (+1 method-spawn).
# +9 timer sites: three shutdown/model/idle worker timers, three bounded
# Stop fixture observations, one retained-window publication fixture timeout,
# one remote-owner fixture timeout and the existing request deadline wrapped
# around cached-caption forwarding. All stay awaited by their current owner.
# +4 process constructors are the worker decoder/whisper and finite fixture
# generator/probe; each uses bounded::output_command and its reap ownership.
# +5 broad launch-shaped method matches are response.status assertions/checks
# (five in cached_owner_delivery, one in subtitle_playlist, minus one removed
# chunk_02 assertion). They start no OS child. No pattern/scope relaxation.

# PR #947 inherited census reconciliation (source-only base 079960dae):
# vod/tests/chunk_03.rs additions in 142d9f4e07 and b38387a133 implement
# retirement_releases_background_admission_with_publication_locks_held and
# completed_output_survives_retirement_and_manifest_contention. Their finite
# sleep children are kill-on-drop, ChildJob-attached, ProducerSlot-registered
# with the background permit, and explicitly writer-settled and confirmed-reaped.
# These fixtures account for one existing key gate, two method/OS spawn,
# Command and kill-on-drop sites, and five bounded timeout observations.
# No inherited production owner or pattern/scope/equality exception is added.

# Corrected predecessor809be33c integration: the unchanged structural scanner
# over the unchanged module roots measures1176 namespaced task shapes,104
# method spawns,2271 timer shapes,361 Command constructors and951 launch-shaped
# methods. These combine the separately reviewed predecessor/effort owners;
# patterns and source scopes are unchanged. Counts are lexical ownership
# inventory evidence, not process execution or playback qualification.
# M4 Source processed-upgrade integration census (2026-11-16), measured with
# the inventory's pure structural scanner over module_roots and source scopes.
# Structural deltas from the prior ledger: detached tokio tasks 121->126,
# task shapes 1161->1166, timers 2236->2251, method-spawn 99->102, Command
# constructors 356->359, and launch-shaped methods 934->943. The new
# SourceProcessedOperation owns one detached operation task for the actual
# Source assignment: its sole trial deadline cancels the token and the owner
# joins the closure before publishing settlement or releasing the preparation
# slot. The two test-only `sleep` children and two reader tasks are gated until
# real child retirement/EOF and reader release; they test joined physical
# settlement, not a new production process owner. The signed Activity/Stop
# requests and HTTP assertions are control-plane operations, not subprocesses.
# Exact Source processed-root attachment adds one keyed rendition_build_gate
# occurrence (34->35), rechecking the captured physical graph before reuse.
# B suppresses retries only for its processed successor after decline/failure,
# preserving the ordinary retry path. Counts are source shape
# only, not test execution or physical qualification.
# The Linux-only `admin_stop_of_a_source_processed_session_settles_and_ends_the_start`
# fixture adds two joined tasks (processed reader and admin-Stop caller), four
# finite fixture bounds, and one real `sleep` child attached to the existing
# Source `ProducerSlot`. Stop retains the activity and Source owner while the
# actual reader is gated after EOF, then joins settlement; this is test-only
# lifecycle evidence, not a new production process or timer owner. One added
# HTTP `Response::status()` assertion is a protocol observation. No matcher,
# source scope, or ownership allowance changed.

# Final-review source-shape recount (2026-11-16), after authenticated
# Activity/Stop route hardening. Structural recount only; not test, runtime,
# or subprocess qualification. The full-router learner fixture owns one
# `tokio::spawn` gateway task and aborts/awaits its handle during teardown,
# adding one joined task shape. Its three `.status()` reads inspect in-process
# HTTP responses (including signed absent-owner behavior), not child processes.
# The Source Stop fixture still uses the actual ProducerSlot and joins reader
# and Stop tasks through physical settlement. Exact-count rows below are
# refreshed from the pure inventory scanner. No matcher, source scope,
# forbidden-zero rule, or ownership allowance changed.
```
