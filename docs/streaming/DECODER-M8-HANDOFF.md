# Decoder M8 — prove the finished path on the fleet

**Status:** ready to execute · **Executes:** M8 of the decoder selection and
recovery plan · **Written:** 2026-09-09 · **Baseline:** `main` at
`b36397cf20d2a7f2917edc1f6d680591fb8ce640`

The decoder selection, one-shot recovery, prepared replacement, client, and
Developer-settings implementation is complete. M8 is the remaining physical
evidence campaign: identify the exact deployed FFmpeg and hardware paths, run
the real workload matrix, look for false positives and resource conflicts,
measure recovery, and exercise replacement on web, Apple, and Android. It is
not another implementation milestone and it does not gate either operator
switch.

Read the live
[status](../DECODER_SELECTION_RECOVERY_STATUS.md) first. Use this document as
the execution ledger from then on. A result which exposes a product defect
belongs in a new, focused repair pull request; do not silently change code
while collecting a qualification receipt.

---

## 1. Outcome and definition of done

M8 is complete when one retained result names exactly what ran and supplies
all of the following:

1. Exact build and backend identity for every server path claimed as tested.
2. Hardware-versus-software results for the applicable workload rows.
3. Clean-control and injected-fault results, including any false positive.
4. Cold-start and concurrent-session behavior with resource counts.
5. Fault, preparation, commit, and first-recovered-frame timestamps.
6. One measured replacement run on web, Apple, and Android, or an explicit
   unavailable result for a client class which could not be exercised.
7. A row-by-row disposition: pass, product defect, infrastructure failure, or
   missing evidence.

Promotion and implementation are already complete. Missing access to a node,
device, or private medium is a visible evidence gap, not a reason to describe
the feature as unbuilt.

## 2. Current contract — do not reintroduce a gate

Settings → Developer has two independent controls relevant to this run:

- **Prepared quality handoff** is a direct checkbox and is on by default.
- **Automatic decoder recovery** is a direct checkbox and is off by default.
  Check it for the recovery trials. The choice applies to new attempts
  immediately.

`GET /api/v1/developer/readiness` reports advisory facts. Missing measurements
or retained hardware contracts never override either checkbox. The separate
qualified-artifact setting controls path-scoped cache qualification and is
applied on restart; it is not required to try decoder recovery.

The old wording in
[plan §12.9](DECODER_SELECTION_AND_RECOVERY_PLAN.md#129-m8--qualify-the-fleet-and-promote-the-completed-effort)
about enabling only qualified combinations is superseded by the shipped
operator contract and the owner's explicit decision: readiness informs the
operator but does not gate activation. Do not add a compile-time feature,
hidden allow-list, backend qualification prerequisite, or client
qualification prerequisite.

## 3. Guardrails and non-goals

- Keep the original issue-913 media private. Record a stable private fixture
  label and a digest where policy permits; do not commit the media.
- Do not commit credentials, credential-bearing URLs, machine secrets, or
  unsanitized logs.
- An advertised hardware accelerator is not evidence that FFmpeg selected it.
- Process exit zero, an output pixel format, and decoded frames are not by
  themselves evidence of the selected decoder.
- Advisory diagnostic evidence can drive the bounded recovery path when the
  operator enabled it. It cannot qualify an artifact for cache reuse.
- Do not change client capability literals during an evidence run. A mismatch
  is a finding for a repair PR.
- Do not broaden M8 into adaptive backend blacklisting, fleet-wide automatic
  bans, retry-policy redesign, or unrelated playback work.
- Preserve the one-recovery-per-logical-epoch bound. A test harness must not
  manufacture a new playback merely to obtain a second retry.

## 4. Evidence bundle contract

Create one result document under `docs/streaming/` and add it to
[`docs/README.md`](../README.md) in the same commit. The suggested name is
`DECODER-M8-RESULTS-<short-sha>.md`. Record these fields before interpreting a
run:

| Field | Required value |
|---|---|
| Server | Full Git SHA and container image digest |
| FFmpeg | Version line, binary SHA-256, and build-configuration SHA-256 |
| Node | Hostname, model, GPU, driver, operating system, and container runtime |
| Backend path | Requested accelerator, selected backend, and selected decoder |
| Client | Platform, device model, OS version, and app or web build |
| Source | Private fixture label, permitted digest, container, codec, profile, bit depth, dimensions, frame rate, and selected video stream |
| Playback state | Requested grade, position, pause state, audio track, subtitle track, and presentation path |
| Controls | Prepared handoff, automatic recovery, and qualified-artifact values |
| Timing | Synchronized clock source and every timestamp named in §9 |

Repeat the read-only inventory from
[`fleet-ffmpeg-2026-09-05.toml`](../../tests/playback/decoder-health/fleet-ffmpeg-2026-09-05.toml)
on the current deployment. Its core container commands are:

```bash
docker exec plurxd sha256sum /usr/bin/ffmpeg
docker exec plurxd sh -c 'ffmpeg -buildconf 2>&1 | sha256sum'
docker exec plurxd ffmpeg -hide_banner -decoders
docker exec plurxd ffmpeg -hide_banner -hwaccels
docker inspect --format '{{.Image}}' plurxd
```

**How to read it:** all identity fields must belong to the process which
produced the playback under test. A host FFmpeg hash does not identify the
container FFmpeg, and a previous deployment's image digest does not identify
the current one.

## 5. M8.1 — qualify actual hardware diagnostic paths

The retained fleet inventory proves only what nodes advertised on 2026-09-05.
For each backend/codec/decoder path that will be called measured:

1. Capture a clean decode and a known decoder fault from the same exact build.
2. Sanitize stderr without changing order or wording. A replay fixture is one
   monotonic-millisecond timestamp, a tab, and one stderr line per record.
3. Preserve the selected input stream and build identity with the fixture.
4. Add a diagnostic contract only when the exact build, codec, backend, and
   decoder identity was observed.
5. Replay the sanitized fixture through the shipped parser:

```bash
scripts/decoder-diagnostic-qualification \
  --selected-stream 0:0 \
  --diagnostic-contract <contract-id> \
  <sanitized-fixture>
python3 -m unittest tests.operations.test_decoder_diagnostic_qualification
python3 -m unittest tests.validation.test_decoder_recovery_status
```

**How to read it:** the replay must attribute only the selected video stream,
name the intended contract, and report the expected clean or fault state. A
zero-test result is not evidence. The two current contracts are software
grammar evidence; neither proves a deployed hardware decoder path.

Positive hardware selection needs backend-specific diagnostic output that
identifies the decoder actually chosen. Advertised `-hwaccels`, output pixel
format, or encoder selection alone is insufficient. If the build offers no
stable diagnostic for the selected decoder, record the path as
`advertised-only`; do not invent a contract.

**Acceptance:** every path claimed as measured has reproducible identity and a
clean/fault replay. Every other path is explicitly `advertised-only`,
`unavailable`, or `not run`.

## 6. M8.2 — run the workload matrix

Run each applicable row through a software reference and the actual hardware
path. Keep the output presentation and hardware encoder the same when the test
is comparing input decoders.

| Workload | Minimum assertion |
|---|---|
| H.264 SDR clean control | Correct picture, metadata, startup, and no recovery |
| HEVC Main10 HDR10 clean control | Correct ten-bit path, HDR metadata, startup, and no recovery |
| Issue-913 MPEG-4 ASP/XVID AVI | Hardware result versus software reference; intended software fallback produces frames |
| Dolby Vision route | Required software input decode still retains the selected hardware encoder where supported |
| Text subtitle | Selection and position survive; no unrelated decoder fault |
| Bitmap/PGS subtitle | Burn path remains correct through the selected decoder and any recovery |
| Multiple video streams or attached artwork | Only the selected video stream contributes decoder health |

For every cell record: selected stream, selected decoder/backend, encoder,
first-frame time, output pixel/grade facts, health result, recovery count, and
viewer-visible outcome. If a row is unsupported on a backend, say why and do
not turn it into a synthetic pass.

**Acceptance:** every supported combination has a result, no clean control
recovers, and the recovered alternate preserves the requested presentation.

## 7. M8.3 — classify false positives and one-shot behavior

Run at least these classes on every backend path under test:

- Repeated clean controls long enough to cover ordinary startup and steady
  playback.
- A selected-video decoder fault which crosses the retained threshold.
- Audio, encoder, subtitle, filename, and unselected-video error controls.
- An unqualified build or path with automatic recovery checked.
- A late decoder error near process completion.

For a fault trial, retain the evidence that the recovery belongs to the same
logical playback epoch. Prove that a second qualifying fault cannot authorize
another recovery, including after seek, track change, reopen, owner handoff,
or worker restart.

**Acceptance:** clean and unrelated controls produce zero automatic
recoveries. A qualifying selected-video fault can produce no more than one
alternate. An unqualified observation may drive that one-shot action but does
not produce a qualified cache artifact.

Any clean-control recovery is a product blocker. Save the smallest reproducer
and open a focused repair PR before continuing that backend's campaign.

## 8. M8.4 — startup and concurrency

Exercise a cold daemon/container start and simultaneous sessions that compete
for the real decoder and encoder resources. Include at least one unaffected
session while another session recovers.

Record:

- daemon ready time and first ordinary playback start time;
- permits before the fault, during preparation, and after retirement;
- decoder and encoder assignments for predecessor and successor;
- whether any session waited, failed admission, or changed grade;
- whether owner loss, restart, or handoff changed the recovery count; and
- final permit counts after all sessions stop.

**Acceptance:** measurement does not block ordinary startup, one hardware slot
is never double-owned, the recovered software decoder retains the intended
hardware encoder when the node can support it, unaffected sessions remain
healthy, and all permits return exactly once.

## 9. M8.5 — measure recovery latency

Use synchronized clocks and retain these landmarks:

| Landmark | Meaning |
|---|---|
| `oldest_threshold_record` | Oldest decoder-error record in the window that caused the fault |
| `fault_latched` | Server accepted the selected-video health fault |
| `successor_prepared` | Durable alternate reserved and media preparation began |
| `successor_ready` | Client reported the replacement ready |
| `replacement_committed` | Client committed the prepared replacement |
| `first_recovered_frame` | First visible frame from the successor |
| `predecessor_retired` | Old producer/session finished retirement |

Report both diagnostic detection latency
(`fault_latched - oldest_threshold_record`) and viewer recovery latency
(`first_recovered_frame - fault_latched`). Also report the complete disruption
visible to the viewer; do not substitute server event spacing for an external
picture measurement.

**Acceptance:** all landmarks are present or explicitly inapplicable, event
order matches the protocol, only one successor is prepared, and the old path
retires after the replacement owns presentation.

## 10. M8.6 — exercise web, Apple, and Android

On each client, start playing for long enough to establish its ordinary state,
then induce one postpublication selected-video fault with automatic recovery
enabled. Capture the actual HTTP control exchanges, not only local enums.

For each client prove:

1. At most one `prepare` action builds at most one replacement pipeline.
2. The acknowledgement sequence ends in `committed`, or in a recorded
   `failed`/`aborted` followed by the bounded fallback.
3. `committed` carries `first_frame_unix_ms`; the `buffer_ready`
   acknowledgement carries `buffered_through_ms`.
4. Position, pause state, audio track, subtitle selection, and grade survive.
5. The predecessor remains authoritative until commit and then retires.
6. The client does not reopen-loop after a terminal decoder result.
7. A node-relative replacement URL is used; a non-relative URL is refused.

Use the shared acceptance in
[`M6-CLIENT-REPLACEMENT-CONTRACT.md`](../playback-control/M6-CLIENT-REPLACEMENT-CONTRACT.md#12-acceptance-shared-by-all-three-clients).
For physical Apple devices, also follow
[`M6-APPLE-HARDWARE-ACCEPTANCE.md`](../playback-control/M6-APPLE-HARDWARE-ACCEPTANCE.md).
That Apple document correctly warns that an observed fallback is a measured
result, not automatically a failure.

**Acceptance:** each available client has one retained end-to-end result and
all preservation assertions are explicit. Unavailable hardware is named as
missing evidence, never inferred from simulator or unit coverage.

## 11. Result ledger

Use this table in the result document. Add links to sanitized logs, fixtures,
or repair PRs rather than pasting unbounded output.

| ID | Node/backend | Client | Workload | Controls | Result | Evidence | Disposition |
|---|---|---|---|---|---|---|---|
| M8-001 | | | | | | | |

The disposition vocabulary is exact:

- `pass` — the observable acceptance passed on the named combination;
- `product-defect` — reproducible shipped behavior violated a contract;
- `infrastructure` — the intended path did not run because the environment
  failed;
- `missing-evidence` — a node, device, medium, or trustworthy diagnostic was
  unavailable; and
- `unsupported` — the combination is outside the declared platform contract.

## 12. Failure routing and handback

Treat failures according to the thing the evidence actually established:

| Observation | Action |
|---|---|
| Accelerator is advertised but selection cannot be proven | Keep it `advertised-only`; no code change |
| Sanitized grammar does not replay on the exact build | Fix the evidence or open a focused parser-contract PR |
| Clean media triggers recovery | Stop that backend campaign and open a focused correctness PR |
| Recovery exceeds one attempt in one epoch | Open a focused durability/fencing PR |
| Position, pause, track, grade, or retirement invariant fails | Open a focused server/client PR with the smallest reproducer |
| Device, private media, node, or driver is unavailable | Record `missing-evidence`; do not claim pass or failure |
| CI or deployment fails before the path runs | Record `infrastructure`; rerun only after identifying the failed layer |

After the matrix is complete:

1. Add the result document to the docs index in the same commit.
2. Update the live decoder status with tested combinations, measured latency,
   false positives, explicit gaps, and any repair PRs.
3. Open one documentation/evidence PR. Keep code repairs in separate focused
   PRs so the retained receipt identifies the exact behavior it measured.
4. Report plainly that M8 is complete, incomplete because named evidence is
   unavailable, or blocked by named product defects.

Do not call a chat transcript, an advertised capability, or a passing unit
suite fleet qualification. The durable M8 output is a reproducible result tied
to an exact server, FFmpeg build, backend, source, and client.
