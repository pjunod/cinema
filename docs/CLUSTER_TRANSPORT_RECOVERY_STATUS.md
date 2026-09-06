# Cluster transport recovery — live implementation status

**Status:** active · **Effort:** `effort/cluster-transport-recovery` ·
**Started:** 2026-09-05 · **Last updated:** 2026-09-06

Companion to [OPERATIONS.md](OPERATIONS.md) (operator contracts) and
[DEVELOPMENT_PIPELINE.md](DEVELOPMENT_PIPELINE.md) (branch, gate, and
qualification rules) — this page answers *what is implemented, what has been
proved, and what remains*. Update it in the same commit that changes a
milestone's state. A green helper test is evidence for that helper, not for the
whole recovery contract.

## Outcome — bounded recovery without weakening authority

After transient transport backpressure, every Raft and cluster-API frame must
be written and flushed, and a connection whose writer fails must be torn down
without waiting for its reader. Snapshot retry keeps OpenRaft's typed mismatch,
vote, snapshot-ID, and offset semantics while one absolute attempt budget
bounds chunk transfer and final installation.

The completed work will expose process-local transport progress before the
public HTTP listener opens. It will not authorize reads, replay ambiguous
mutations, weaken readiness, change election timers, or introduce a wire-format
change.

## Delivery — one effort, reviewable milestone PRs

| Milestone | Task branch | PR | State | Blocking evidence |
|---|---|---|---|---|
| M1 · frame completion | `codex/cluster-transport-m1` | [!56](http://192.168.4.7:3000/noirr/plurx/pulls/56) | merged into the effort after three exact-candidate adversarial reviews and the green effort gate | Both raw buffered-write failures are reproduced after transport writability returns; every production writer and terminal path passes its focused regression |
| M2 · connection and snapshot ownership | `codex/cluster-transport-m1` | same merged review | foundations merged; end-to-end acceptance in progress | Cancellation-safe admission, shutdown ordering, node-owned snapshot execution, real-file partial-write ownership, and 100 in-memory WebSocket reader/writer task cycles pass; production Raft install/socket-disconnect coverage is reserved for the M5 harness and is not yet claimed |
| M3 · recovery budgets | `codex/cluster-transport-m3` | [!60](http://192.168.4.7:3000/noirr/plurx/pulls/60) | second exact-SHA review found the prebuilt proof was not bound to the pulled artifact; the replacement candidate freezes the image ID, requires clean source/revision identity, and is awaiting exact-SHA review and the effort gate | Virtual-time exact bounds · config/env/Compose precedence |
| M4 · transport status | `codex/cluster-transport-m4` | not opened | combined candidate is undergoing review-finding fixes before exact-SHA re-review | Authenticated pre-HTTP status · zero store calls · stale samples expire |
| M5 · recovery campaign | isolated review-fix branch | not opened | campaign harness and resource evidence are undergoing review-finding fixes before final integration | Actual TLS transport matrix · 20 learner and 20 voter cycles |
| Final promotion | `effort/cluster-transport-recovery` | not opened | not started | Full suite once after all review fixes · current-tree qualification receipt |

## Current evidence — Rust 1.97.1 is the compiler of record

The M3 candidate's exact merge-base with the effort is `5f96469a94`. Current
`main` will be merged into the frozen effort before final qualification. The
local Homebrew default is Rust 1.95.0, so every recorded Rust command uses
`rustup run 1.97.1`; unpinned results do not count.

| Check | Result | Detail |
|---|---|---|
| Vendored Hiqlite compile | pass | SQLite+cache and dashboard+SQLite+cache feature matrices completed on Rust 1.97.1 |
| Buffered-write reproduction | pass · 2 pinned cases | With underlying writability already restored, raw fastwebsockets `write_frame` leaves a 32-byte server response and 3 MiB client request unavailable for 200 ms; explicit flush delivers each on the same socket |
| Production writer regressions | pass · 5 surfaces | Serialized Raft/API requests and responses plus the proxy response use their production writer seams over real TLS; queue-consuming Raft/API client and server writers report injected flush failures to their supervisors |
| Deadline and failure regressions | pass | Flush error, blocked combined write/flush budget, 250 ms Close budget, and scaled server-handshake budget all terminate at their documented boundary |
| Vendored network suite | pass · 62 tests | Prescribed SQLite+cache+auto-heal+macros `network::` library filter on Rust 1.97.1; no failures or ignored tests |
| API client stream suite | pass · 21 tests | Includes cancellation-safe mutation ownership, decoded-response-before-handoff ordering, exact-socket proxy refusal ownership after caller cancellation, cancellation-safe retention of a decoded response blocked behind the bounded reader queue, concurrent-refusal coalescing, proxy/non-proxy EOF-before-drain handling, a dedicated leader-control queue that bypasses an application backlog during writer backpressure, stale/same-target leader handling, production request writer, and malformed-frame reader outcome |
| Snapshot ownership foundations | pass · 6 executor tests + 5 real SQLite state-machine tests | One running/one queued admission, cancellation-safe deadline/shutdown ownership, a controlled real-file partial write with digest verification, and real SQLite pending/current recovery pass. The remaining full Raft/socket-disconnect acceptance is explicitly open for M5. |
| Broad optional-feature probe | invalid baseline lane | `--all-features` enables mutually exclusive `cast_ints` modes and reaches unrelated existing optional-feature compile defects; the repository-prescribed feature matrices remain authoritative |
| Daemon check and denied-warning Clippy | pass | `plurxd --all-targets` completed on Rust 1.97.1 with no warnings |
| Vendored denied-warning Clippy | pass | Prescribed auto-heal+cache+macros+SQLite and cache-only library lanes pass with `-D warnings`; seven pre-existing Rust 1.97.1 lint findings were corrected rather than suppressed |
| Dependency resolution | pass | Standalone vendor lock now matches the daemon transport stack: Tokio 1.53.1 and rustls 0.23.42 |
| Persistent regression map | pass | The transport tests are in `cluster-wal-check`; a command wrapper fails exact filters unless one test executes, the broad snapshot filter requires ten passing tests, and operations contracts preserve both guards |
| Focused cluster/WAL fast lane | pass | `make cluster-wal-check` completed after the M3 review fixes, including both production `full_snapshot` wrappers, final-mismatch reread ownership, simultaneous timer/phase transition, hard-TTL shorter than C, configured receiver admission, and bounded standalone durations; every exact filter executed and loopback tests used the normal unsandboxed allowance |
| Operations contracts | pass · 197 tests | The prebuilt-image rollout contract requires a labeled image ID matching clean source, identical server/discovery images, single derivation, exact-value proof, and no-build/no-pull replacement in that order; the complete lane passed with the port-reservation fixture using the normal unsandboxed loopback allowance |
| M3 snapshot RPC suite | pass · 37 tests | The standalone `raft_client` suite covers one absolute transfer deadline, a latched final-install deadline, caller cancellation, exact-socket reset ownership, typed mismatch restoration before reread, simultaneous phase/timer readiness, both production snapshot wrappers, and stale-attempt isolation |
| M3 configuration suite | pass · 11 root tests + 1 standalone bound test | Chunk, transfer, and install budgets accept documented bounds, reject invalid ordering and oversized values, preserve defaults for empty environment variables, and cannot reach unchecked `Instant` arithmetic |
| M3 production timing seam | pass | The `hiqlite-store` migration test proves the production startup catch-up deadline is exactly transfer + install + 45 seconds |
| M3 deployment and operations contracts | pass · 197 tests | Compose health timing derives from the three startup phases; the 1,200-second install case requires 2,535 seconds, and the prebuilt-image target binds its exact resolved period to the inspected runtime revision before replacement |
| M3 compile and lint | pass | Root all-target check and denied-warning Clippy pass; standalone vendored SQLite, SQLite+cache, and full library matrices pass; root and standalone formatting pass |
| Full repository suite | deferred | Run once on the final fixed promotion candidate, as requested |

## Decisions to review — autonomous choices

1. **Work only in independent clones.** M3 and the isolated M4/M5 review-fix
   streams live under `/private/tmp`; no implementation or review edits occur
   in the user's checkout. A mistakenly created worktree and its two branches
   in the shared repository were removed after a byte-for-byte patch comparison;
   no pre-existing shared changes were reset or deleted.
2. **Keep recovery always on; add no production feature gate.** Settings → Dev
   explains safe rollout prerequisites and current support state but does not
   activate or disable the corrected transport. Readiness and deployment
   checks remain fail-closed. Validation-only fault injection remains test-only
   because exposing a production fault switch would itself be unsafe.
3. **Keep protocol compatibility.** New budgets and observations stay local;
   serialized Raft and binary API variants do not change during the rolling
   update.
4. **Fold the first review's M2 findings into the M1 task PR.** Writer failure
   is only safe when the connection owns both split tasks and accepted
   snapshot work has a longer-lived owner. Keeping these coupled changes in
   one review prevents the flush fix from shipping with known teardown races.
5. **Open and merge only reviewed exact candidates.** M1 pull request !56 was
   merged as `5f96469a94` only after its replacement effort gate passed. Its
   first preflight exposed an unmapped validation helper; the merged correction
   routes changes to that helper through the cluster/WAL lane it protects and
   has its own scope regression.
6. **Treat receiver admission as the configured chunk budget.** The first M3
   adversarial pass found that sender C had been configured while the receiving
   snapshot executor still admitted work under the old fixed frame-write
   timeout. The node-owned executor now captures the validated C value at
   startup, and its capacity-one regression uses a non-default scaled budget.

## Next checkpoint — review, gate, and merge M3

M1/M2 are merged into the effort at `5f96469a94`. The first exact-SHA M3 review
found eight boundary gaps: final mismatch restored T too late, a stale T timer
could beat a ready final-stage update, standalone durations could overflow
`Instant`, both production wrappers lacked direct pins, hard TTL below C was
untested, numeric overflow evidence was missing, the Dockerfile comment was
stale, and receiver admission still used the old fixed timeout. All eight are
fixed and the focused fast lane is green. The replacement SHA passed the same
three adversarial tracks and is now in PR !60. The first replacement
review also caught a cache-only denied-warning lint in a helper's oversized
`Result`; the helper now performs only the phase restoration and the trait
implementations retain error mapping. All three prescribed standalone Clippy
matrices and the 37-test snapshot client suite pass after that change. A final
independent pass then found that the documented prebuilt-image command bypassed
the startup-budget proof. The replacement candidate adds a dedicated
pull/prove/no-build fleet target and persistent ordering coverage. Its first
exact review found that the host checker could still describe a different
revision from the pulled image and that the rollback runbook named the
release-only `latest` alias. The current candidate stamps and inspects the
runtime revision on the frozen image ID, requires a tracked-clean matching
checkout and identical server/discovery images, pins proof and replacement to
that ID, and restores the fleet to `main` or a named immutable tag. Real
socket/install acceptance remains explicitly open for M5.

**How to read this page:** “pass” means the named command completed against the
named tree. “In progress” does not mean shippable. The effort is complete only
after every milestone is merged into the effort branch, the current `main` is
integrated, the full suite passes once on that fixed tree, and the promotion
receipt exists.
