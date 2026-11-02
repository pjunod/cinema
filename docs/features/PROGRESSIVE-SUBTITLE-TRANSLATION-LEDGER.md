# Progressive subtitle translation — review ledger and completion record

**Completion effort, 2026-10-26:** [current implementation and final qualification](SUBTITLE-COMPLETION-STATUS.md) extends the initial scope with progressive transcription, native clients, multiple audio tracks, additional delivery routes and existing-text translation. The evidence below retains its original candidate and scope. The user owns deployment after the new merge.

**Status:** built — first route accepted on 2026-10-17; follow-ups are listed at the end of §2.

**Setup repair — 2026-10-25; [implementation, qualification and landing](SUBTITLE-SETUP-REPAIR-STATUS.md).**
The live web setup accepted an Install request, then displayed
`Invalid model operation response`: the version 2 server response omitted
`purpose: "translation"`, which the shipped client requires before it follows
jobs. A later status refresh showed the model cached; Activity showed its test
queued with `waiting_for_playback_capacity`. This reproduces a setup failure,
not a broken download URL or successful model qualification. A separate earlier
model operation on another worker failed with `output_invalid`; its cause remains
unresolved. No translation enablement was saved and no application was deployed.

The repair returns the missing response discriminator and tests the real HTTP
Install/Test routes, their queued job payloads and receipt replay. Setup suggests
the sole active profile without saving it, labels installation **Download and
test**, shows trusted failure/wait reasons, prevents duplicate requests while a
job is active, and recovers model/language choices after a failed catalog load.
An old setup clock is refreshed before a new action; ambiguous retries retain
their existing request identity. Model tests also retain a FIFO turn on the existing
heavy-background semaphore and publish `waiting_for_capacity`; CPU resources
are reserved after the shared gate, with foreground checks and cancellation
intact. The current background job still finishes before the test starts. These changes do not add native playback,
additional audio streams, remux/encode delivery or progressive same-language
transcription. The earlier first-route acceptance retains its original limits.

Companion to [the plan](PROGRESSIVE-SUBTITLE-TRANSLATION-PLAN.md): its review ledger, the
integration checkpoints and the completion record with the packaged acceptance evidence,
moved here so the plan stays readable.

## 1. Decisions still to freeze, and review ledger

| Decision | Current proposal / missing evidence | Owner |
|---|---|---|
| Useful lag and startup budget | User chose progressive, not a latency guarantee. P0 must propose numeric profile-specific thresholds from measurements for parent approval. | Engine + parent |
| Model/engine selection | P0 selected pinned Small multilingual for an experimental finite functional profile; Base and W20/L2/R2 were rejected. Grammar, meaning and boundary repetition remain explicit. Packaged translation-specific readiness is still required. | Engine |
| Stable overlap/context policy | Independent W30/L5/R5 cores, deterministic half-open cue clipping, no fuzzy deletion or linguistic dedup claim. Retain measured repeated straddling text. | Engine + core |
| Demand finishing/retention | Bound work after last viewer leaves, per-user requests, retained bytes and TTL. Set numbers from measured storage/admission budgets in P1. | Lead |
| Native growing-track adapter | Static VTT URL reload is not assumed. Select existing-owner-compatible text/overlay/HLS routes per client after a minimal measured proof. | Native owners |
| Viewer work-creation permissions | Initial admin-only demand creation/refresh/seek/release. Nonadmin reads cannot schedule; missing ranges stay unavailable until an admin demand produces them. Broader viewer requests require a reviewed permission/quota change. | Parent + lead |
| Current feature dependency | Generation merged as main `220157aba`, identical reviewed tree `20589a21`; its full post-merge CI and Linux lifecycle continue separately. Translation P0/P1 now execute against the merged interfaces. | Lead |
| Functional / broader-quality staging | Parent approved the §2.2 distinction. P0/P1 measured profiles and independent human-authored references can release bounded implementation/functional acceptance; new bilingual listening/adjudication remains open for broader quality. No human verdict or generally qualified movie translation is implied. | Parent + engine + lead |

| Review | State as of 2026-10-15 | Disposition |
|---|---|---|
| Parent adversarial design review | Completed 2026-10-15; revised R1–R9 and single-node acceptance floor approved in design | Design review does not close the unmeasured P0/P1 prerequisite gates. |
| Engine independent second review | Completed full-plan review 2026-10-15; R6–R8 and provenance correction recorded below | Parent and engine confirmed revised dispositions in design on 2026-10-15; P0/P1 measurement/protocol decisions remain prerequisite gates. |
| Implementation authorization | User authorized implementation after current subtitle usability completion and reviewed P0/P1 contracts | These are prerequisite gates, not another user-permission request. Structural implementation is active in one integrated task; runtime activation and actual user-path acceptance remain gated. |
| Functional / broader-quality staging review | Parent approved the staged evidence boundary on 2026-10-15 after prerequisite-input review | §2.2 and §7 retain ordinary single-node progressive delivery and measured correctness/resource gates. Published human-authored references plus labeled automatic diagnostics can support the limited profile; new bilingual listening/adjudication remains open. No measurements or human verdicts fabricated. |
| Independent staging-amendment review | Engine reviewer read the amendment on 2026-10-15; no blocking weakening or contradiction found | Selected-language reference evidence remains distinct from broader human listening; measured P0/P1 contracts and ordinary single-node, seam/timing, resource and Stop gates remain mandatory. |

### 1.1 Parent adversarial findings and proposed dispositions

Parent reviewed and approved the R1–R5 architectural dispositions on
2026-10-15. They are resolved in design, not implemented or qualified.
P0/P1 numeric bounds and semantic-seam decisions remain explicit prerequisites.

| Finding, 2026-10-15 | Revised contract / exact regression obligation | Owner and status |
|---|---|---|
| R1 · viewer consumption could implicitly create work | §1.2/§3.4: all work-producing demand mutations are admin-only; read-only viewer seek/poll cannot enqueue. Test nonadmin POST/PUT/DELETE refusal and unchanged job/priority counts after seeks into missing coverage; authorized existing segment reads still pass. | Lead; resolved in design by parent; implementation/regression outstanding. |
| R2 · a stable interior might wait forever for the next claim | §3.2: canonical independently finalizable core with its own bounded context; explicit beginning/EOF/disjoint/isolated-seek/cancel rules. Test final core with no successor, isolated middle core and cancellation at every publication boundary. Semantic seam policy remains a P0 quality decision, not an unproved dedup claim. | Engine + core; resolved architecture by parent; P0 seam decision/measurement outstanding. |
| R3 · manifest CAS could publish unreachable worker-local bytes | §3.3: bounded immutable cue bytes in authoritative replicated store before/with references; read-fenced routing, digest validation, orphan grace and GC/read leases. Genuine Hiqlite failover/read/GC race tests required. | Core + lead; resolved architecture by parent; P1 store-budget freeze/implementation outstanding. |
| R4 · chunk offset did not define playback time | §3.6: actual decoded-sample anchor and rational stream/container mapping, delay/resampler accounting, negative/nonzero PTS, VFR and client-only subtitle offset. Test against visible/audio source markers after seeks and rate/offset changes. | Engine + native/core; resolved architecture by parent; P1 mapping oracle/implementation outstanding. |
| R5 · growing arrays and lost revisions could break resync | §5.2: bounded immutable revision pages/cursors/read leases, ETag-gap/restart reset, pruning and backward re-fetch. Test concurrent updates while paging, expired cursors, duplicate pages and pruned backward seek without new inference. | Lead + web/native; resolved architecture by parent; P1 limits frozen; implementation and exact-tree regression checks in progress. |

### 1.2 Independent engine findings and proposed dispositions

The engine agent reviewed the complete `d78bfaf4` draft independently after
parent R1–R5 review. Parent approved these revised architectural dispositions on 2026-10-15.
The engine independently confirmed closure against revision `a0c3da8f` on
2026-10-15; neither review qualifies performance or implementation. The single-node acceptance floor in
§1 is mandatory and cannot be waived by unsupported-profile declarations.

| Finding, 2026-10-15 | Revised contract / exact regression obligation | Owner and status |
|---|---|---|
| R6 · active demand heartbeats could recreate cancelled work after Stop | §3.5: durable operation epoch and Stop latch fence demand scheduling, claims and publication. Only explicit fresh admin intent with observed epoch can resume. Test two continuing heartbeats/seeks after Stop, zero new claims/late publish, readable committed segments, crash/replay and explicit Retry. | Lead/core + clients; resolved in design by parent and engine; implementation/regression outstanding. |
| R7 · scanner metadata cannot establish cross-node source equivalence | §3.1: trustworthy portable cryptographic content revision; conservative full-source SHA-256/held immutable source candidate, measured cold-read cost and no sampled-hash shortcut. Test two different equal-size/equal-mtime sources refuse, identical bytes/different inode accept after verification, hashing/decoding replacement and cache invalidation. | Lead/core + engine; protocol frozen; actual source-hash wall time and first visible cue remain user-path acceptance. |
| R8 · current idle predicate would starve during encoded playback | §4: explicitly prohibit treating zero-admission idle policy as concurrent support. Measure actual full path under one-node direct/remux/encoded and two-node cases; freeze spare-capacity admission or declared source/topology restrictions without bypassing foreground protections. | Lead + engine; initial direct-only single-audio restriction frozen; actual protected direct playback/admission remains pending. |
| R9 · common adapter can hide platform inference differences | §3.3: per-segment actual platform/engine SHA plus source/model/profile/epoch/fence provenance; immutable first valid CAS winner, never overwrite with another platform's result. Test duplicate valid competing results retain the winner and its truthful provenance. | Engine/core; resolved provenance design by parent and engine; implementation/regression outstanding. |

### Current integration checkpoint — 2026-10-15

P0 characterization and structural P1 are frozen. Eighteen approved attempts
selected pinned Small multilingual with 30-second independent cores and five
seconds of context on each side. The 20-second candidate omitted verified
speech and was rejected. The selected profile preserves that speech but can
repeat a straddling sentence and retain grammar or meaning errors; no linguistic
seam-deduplication or broad film-quality pass is claimed. The initial route is
experimental web direct playback with one actual audio ordinal and aligned
source clocks. Explicit spoken-language confirmation can correct metadata.

P2–P5 now form one integration candidate. Core transactions, the manager,
shared installer, API/config and web owner are implemented and undergoing
focused checks. Readiness still requires actual installation and the common
foreign-to-English self-test; packaged first cue, playback continuity, seek,
Stop/Retry and source-hash timing remain acceptance prerequisites. Native,
multi-audio, encoded playback, fleet, long-film completion and broader human
bilingual listening remain open qualification and are not release claims.

Cache retirement is an explicit snapshot boundary: live demands and unexpired
read leases protect retained bytes. Idle retirement advances revisions, rejects
old leases/cursors and frees segment bodies; fresh verified demand may recompute
a core under the same deterministic ID with a newly verified digest. Root Stop
and retained request identities do not disappear with cache eviction.

Generation is merged through main `220157aba`; the focused bootstrap replay
correction is merged as `b2d83ae8`. Its local bootstrap/inventory regressions and
normal hook passed. Post-merge workflow attempts refused closed-PR eligibility,
so they are not recorded as compile passes. The user authorizes merge after
basic local checks with remaining CI watched afterwards; this does not fabricate
missing user-path evidence or authorize publication/deployment.

## 2. Completion — 2026-10-17

**Source.** The reviewed integration candidate was staged as tree
`01d8911f` on `b2d83ae8` and then lost with its temporary checkout before it
was committed. It was rebuilt byte-exactly from the agents' recorded tool
calls: two independent replays agreed on the application code, the P0
fixtures were re-fetched from the pinned FLEURS revision (the regenerated
catalog matches its recorded digest), and the rebuilt tree hashes to
`01d8911f` again. It was then composed onto main `89d790d2`; the only conflict
was the rolling-producer ownership census, resolved by adding the two
independent deltas (spawn 964, timers 1794, constructors 279). The normal
commit hook, which had not run on the frozen tree, required clippy fixes in
the new daemon modules (test unwraps, four redundant conversions, two idioms).

**Packaged acceptance.** One isolated daemon on the macOS arm64 runner, laid
out as `package-macos` lays it out: `bin/plurxd` (commit `43eff2ca`, test
profile), the verified whisper.cpp 1.9.2 CPU engine in `bin/subtitle-engine/`,
and sibling FFmpeg/FFprobe (Homebrew, sufficient for H.264/AAC direct play; not
the custom HDR FFmpeg package and not signed). Fresh data, fresh HOME, empty
model store, cleared environment. Headless Google Chrome drove the shipped web
app with no API interception, fake player, cue injection or time setter.
The movie is 61.14 s of controlled German read speech from five FLEURS
sentences (one AAC audio stream, synthetic H.264 picture, publisher German
captions as mov_text).

| Check | Observation |
|---|---|
| Settings install and test | Enable saved before install; Install fetched and verified the pinned Small model and passed the automatic English self-test; explicit Test passed |
| Original/Off preserved while partial | Off stayed selected until the explicit Use |
| First useful cue during playback | 20.4 s after Translate, playback unpaused at 20.4 s, cue 15.02–22.22 s: "According to the police, the driver of the vehicle, who drove the photographers, has probably not been complained about." |
| Stop | With core 1 reported `translating`, Stop latched the operation (epoch 1→2) and the owned whisper-cli process group was gone 244 ms later; no demand renewal in the next 11 s |
| Resume | Fresh epoch 3 |
| Seek | Forward to 35 s: a later range became live (cue 48–54 s at playhead 50.1 s); back to 8 s: cached cue 6.52–12.02 s immediately |
| Original, Off, Use | Original German caption selectable; Off disabled the translated track; Use restored cached English cues |
| Picture never reopened | Same video element, player, media attachment and source URL throughout; no translation-driven playback decision |

The harness's blanket "no failed request" assertion tripped on two `503`s
from `/api/remote/v1/receivers` and `/grants`, which the Settings page
requests on a server without companion remotes; they are unrelated to
translation and recorded here rather than filtered out.

**Defects the acceptance found, fixed at their cause.**

- The operation read built `active_job` in a scalar subquery; SQLite drops the
  JSON subtype across that boundary, so the job reached the daemon as a string
  and every read reported `active_job: null`. Wrapped in `json()`; the SQLite
  contract now asserts the queued preparation is projected as an object.
- A signal-bearing core whose recognizer cues all lay in its left/right
  context failed terminally with `output_invalid`. The 1.14 s last core of the
  acceptance movie always did: the recognizer dated the closing sentence
  inside the left context, which the previous core already published. An
  empty recognizer result over speech remains a failure; speech the recognizer
  attributed entirely to neighbouring cores now leaves this core validly
  empty. Residual risk: if two adjacent cores each attribute the same
  straddling speech to the other's core, that speech is published by neither.
  That joins boundary repetition as a known seam limitation of W30/L5/R5.

**Checks on the composed source.** SQLite translation contracts 12/12; the
replicated Hiqlite source-bind/publication/Stop/read-lease case; daemon
`subtitle_translation::` and `subtitle_models::` 58/58; web Node suites
(translation, Settings sections) 34/34; ownership census 7/7; web types, shape,
jsconfig and JS syntax. Three `subtitle_models::artifacts` tests fail under a
group-writable umask (0002) because their fixtures inherit it and trip the
service-ownership check; they pass under 022. That predates translation and is
recorded in the backlog rather than changed here.

**Adversarial review before merge.** Three independent reviews (daemon,
store/API, web/docs) found and this change fixed, each at its cause: a
restarted node recomputing ranges whose publication the store refused for
lacking this boot's source binding (only preparation verifies and binds; a
range runs on that binding or not at all, and a fresh request re-prepares);
finished cores discarded because the viewer had moved past them (publication
needs live demand, not the scheduling window); a 1 s loop for every subtitle
pass that walked all operations (only demanded operations are read, the loop
idles and is woken by translation mutations); the timeline taken from the
container rather than the audio stream; byte-cap refusals that could never
clear (least-recently-read eviction past 112 MiB); retired caches leaving old
range rows a new request could not rebind (retirement fences the epoch); a
demand that lapsed on one slow read or throttled tab being shown as Stop (the
demand has its own cadence, only acknowledged generations advance, and a
lapsed demand is renewed by its next heartbeat); a retired player leaving a
dead panel; and exhausted ranges still offered for retry.

**Current completion work.** [The completion ledger](SUBTITLE-COMPLETION-STATUS.md)
now records native clients, actual selected audio ordinals, remux/encoded
attachments and the three caption operations. Linux packages now stage the
readable fixture notice, and manifest rendering applies cue changes without
re-adding unchanged cues; their focused regressions passed. Atomic operation
completion, durable producer reassignment and rapid-seek lease reuse now pass
focused regression qualification in PR #1292. Fleet/device acceptance, long-film throughput and
bilingual human quality remain distinct from implementation and fixture
qualification. The P0 timing figures do not prove realtime on other hardware.

