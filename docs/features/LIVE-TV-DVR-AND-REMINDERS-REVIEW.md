# Live TV DVR and reminders — changes required before implementation

**Status:** review complete, changes requested · **Reviewed:** 2026-09-13 UTC ·
**Against:** proposal verified at `main` `311683bc`

Review of the user-supplied **Live TV DVR and reminders — proposal and
implementation plan**, dated 2026-09-13, for `effort/live-tv-dvr`.
References to proposal sections and milestones below identify that pasted
version; they do not assert that the proposal is committed in this checkout.

Companion to [the guide and UI plan](LIVE-TV-GUIDE-AND-UI-PLAN.md),
[the hardware status](HDHOMERUN-LIVE-TV-STATUS.md), and
[the development pipeline](../DEVELOPMENT_PIPELINE.md).
This document records the review findings and the contract changes needed
before implementation. It does not replace the accepted product choices.

**Verdict: request changes before implementation.** The main risks are lost
recordings, cancelled airings returning, and tuner accounting that disagrees
with the intended behavior. There are eight P1 findings and three P2 findings.

## 1. [P1] Owner handoff cannot resume recordings as specified

**Proposal:** §3.7, owner handoff and the DVR loop.

The old owner marks captures `partial`, but the new owner starts only
`scheduled` rows; expansion preserves existing states. A process crash
instead leaves `recording` rows with no worker. Neither resumes under the
specified transitions, so the claim that handoff loses only seconds does
not hold.

Fencing database writes also does not stop an old process writing the
shared file. The existing [Live TV manager](../../crates/plurxd/src/live_tv.rs)
has explicit drain, shutdown, and serving-authority cancellation paths.
Captures must participate in those paths; a tick-time ownership check alone
does not supply the same guarantee.

**Required change:** define recovery transitions, capture attempts, and file
recovery explicitly. Extend drain/shutdown and serving-authority cancellation
to captures before claiming bounded handoff loss. Prove recovery after both
an orderly handoff and a process crash, including an abandoned `.part` file.

## 2. [P1] Cancelling a series airing allows it to return on the next tick

**Proposal:** §4.2, `dvr_recordings_airing`; §3.7, expansion.

The partial unique index excludes `cancelled` and `deleted`. With the exact
pasted schema, cancelling an airing and then inserting the same channel/start
as `scheduled` succeeds. Rule expansion therefore defeats *Skip* unless it
separately checks persistent suppression records.

The SQLite reproduction produced both rows:

| Recording ID | State | Channel | Airing start |
|---|---|---|---:|
| `first` | `cancelled` | `7.1` | 1000 |
| `next-tick` | `scheduled` | `7.1` | 1000 |

This output means the index enforces uniqueness among its included states,
but does not preserve a user's decision to skip an airing.

**Required change:** keep cancellation as a durable scheduling decision,
with an explicit action to schedule the airing again. Test cancellation
followed by repeated rule expansion.

## 3. [P1] The reserve arithmetic makes admission depend on start order

**Proposal:** §3.4, `free_slots`; M2 acceptance; M8 hardware prompt.

With four tuners and reserve one, the proposed arithmetic permits three
recordings followed by a viewer. A viewer followed by two recordings leaves
one free slot, yet refuses the third recording because it requires
`free_slots >= 1 + reserve`.

| Existing viewers | Existing recording transports | Free slots | Next request | Proposed result |
|---:|---:|---:|---|---|
| 0 | 3 | 1 | Viewer | Admitted |
| 1 | 2 | 1 | Recording | Refused |

Both requests would produce the same occupancy: one viewer and three
recording transports. Admission should not differ merely because of order.

**Required change:** if the intended rule is “recordings may hold at most
three tuners,” test both total capacity and the recording limit under the
same registry lock. Count recording transports as described in finding 4.

Fix M2's acceptance as well: three recordings alone should **allow** a
viewer. M8's one recording plus three viewers should likewise fit. Test
both start orders and a genuinely full tuner set.

## 4. [P1] Shared captures need a different ownership model

**Proposal:** §3.5, overlapping captures; §4.4, registry and `DvrCapture`.

The registry keys captures by recording ID, counts each entry as a tuner,
and gives each its own cancellation token. That cannot directly express two
overlapping recordings sharing one GET.

**Required change:** model one channel transport with multiple recording
sinks. Count transports, stop individual sinks, and release the tuner after
the last sink ends. Define each sink's start/end boundaries and byte count
so padding overlap writes to both files without doubling tuner occupancy.

The capacity dialog must also avoid promising that stopping one recording
frees a tuner when another still shares it. Test stopping either sink during
the overlap and verify that the surviving recording continues.

## 5. [P1] Expansion needs reconciliation rules for changed schedules

**Proposal:** §3.7, expansion; §3.2, incremental guide extension.

Expansion specifies upserts, but not what happens to previously materialised
airings when a rule is disabled, its match/channel changes, or the guide
moves a programme. Old scheduled rows can still run; a moved start can
create another row because start time is part of the airing identity.

**Required change:** define how pending rows are updated or withdrawn, how
manual requests survive rule edits, and how ownership changes when several
rules match. Preserve the cancellation decisions in finding 2 during this
reconciliation.

Cached future guide pages also need periodic revalidation. Extending only
the tail preserves old listings even when the provider has changed the
schedule. Test a moved programme, a disabled rule, and a priority change
after airings have already been materialised.

## 6. [P1] The proposed filename is not unique per recording

**Proposal:** §3.5, capture path.

The filename uses title, minute-resolution time, and episode. The same
programme recorded simultaneously on two channels produces the same path
despite having distinct database identities.

**Required change:** include `recording_id` in the basename, sanitise
guide-derived components, and define collision-safe creation. Recovery
attempts must not truncate an earlier partial capture. Test simultaneous
recordings with identical titles and start times on different channels.

## 7. [P1] Expired scheduled rows can record whatever is currently airing

**Proposal:** §3.7, Start and Finish steps.

Start checks only `capture_start <= now`. After downtime, or a conflict
clearing after the programme ends, the loop can open the current live
stream for an expired recording and then immediately finish it as `done`.

**Required change:** require `now < capture_end`, define a late-start
policy, and terminalise missed airings before admission. Test restart both
during and after an airing, and capacity becoming available after its end.

## 8. [P1] Stop is an unstructured command with no defined acknowledgement

**Proposal:** §4.3, non-owner DELETE behavior; §3.7, tick sequence.

The route stores a stop request in `state_reason`, but the tick sequence
never explicitly consumes it. Progress and state updates can overwrite
that field. Tuner-generation fencing does not resolve concurrent changes
to the same recording.

**Required change:** add durable stop-request fields and conditional
transitions. Return a pending operation and retry Watch only after capture
cleanup confirms capacity was released. Preserve requester attribution
through concurrent progress updates and repeated DELETEs.

Test Stop from a non-owner node while progress is being persisted, and
verify that the request survives until cleanup is acknowledged.

## 9. [P2] Reminders inherit recording prerequisites unnecessarily

**Proposal:** §3.7, loop admission; §3.8, phone mirroring.

The entire loop requires both Live TV and DVR enabled. A viewer can
therefore have a usable guide and armed reminders while server reminders
never fire.

**Required change:** separate reminder processing from capture admission.
Test reminder delivery with recording disabled.

Narrow the phone guarantee as well: foreground-only mirroring cannot
synchronise a reminder created, deleted, or moved elsewhere while that phone
remains closed. Apple's pending local notifications require app-side
scheduling and cancellation. Document this limitation and reconcile pending
notifications when the app next runs.
[Apple's local notification documentation](https://developer.apple.com/library/archive/documentation/NetworkingInternet/Conceptual/RemoteNotificationsPG/SchedulingandHandlingLocalNotifications.html)
describes that lifecycle.

## 10. [P2] Webhook retries can stall recording control

**Proposal:** §3.8, webhook delivery from the owner loop.

Three five-second attempts occur inside the owner loop. Several due
reminders against an unavailable endpoint can delay subsequent starts,
stops, and finishes for minutes.

**Required change:** dispatch through a bounded worker with explicit
best-effort delivery semantics. This does not require a durable outbox.
Test an unavailable webhook with several due reminders while a capture is
due to start or stop, and verify that recording control remains timely.

## 11. [P2] The reliability dependency is incomplete

**Proposal:** M0 dependency on reliability M0–M2; §3.4 admission assumptions.

The available reliability plan places guide durability in M1, client guide
polling in M2, and same-viewer stray eviction in **M3**. Waiting for M0–M2
does not provide the admission behavior that DVR assumes already exists.
This finding is based on the locally available reliability plan, not a
fresh check of PR #273's remote state.

**Required change:** pin the required implementation commits separately:
guide persistence before DVR M0, admission changes before DVR M2. Make the
integration order explicit so a task based on another effort does not
accidentally bring that effort into the DVR promotion.

## Raw TS capture is a reasonable starting point

SiliconDust documents MPEG-TS output and directly saving the HTTP stream to
a file. Raw capture is therefore a reasonable starting point. Keep the
planned playback acceptance test, including the second file created during
a shared capture; the format documentation does not prove the application's
complete playback path.
[HDHomeRun HTTP API](https://info.hdhomerun.com/info/http_api) ·
[SiliconDust capture examples](https://info.hdhomerun.com/info/troubleshooting:creating_a_sample).

## Evidence and limits

The review inspected relevant committed code at `311683bc`, including
[owner admission and cleanup](../../crates/plurxd/src/live_tv.rs),
[settings and owner transitions](../../crates/plurxd/src/http/system.rs),
and [guide normalisation](../../crates/plurxd/src/live_tv/guide.rs).

The exact pasted schema installed successfully in SQLite 3.53.4. The
cancellation/insertion reproduction confirmed finding 2, and direct
evaluation of the proposed admission expression confirmed finding 3.

The review did not run the hiqlite contract suite, Rust compilation,
native-client builds, or hardware tests. It did not change implementation
code. Suggested regression cases above are required evidence for future
implementation, not tests already executed by this review.
