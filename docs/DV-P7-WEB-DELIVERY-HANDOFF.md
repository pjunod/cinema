# Dolby Vision Profile 7 on the web — what is actually left to build

**Status:** ready to build · **Executes:** the adversarial review of
`DV-REMUX-REFUSAL-DIAGNOSIS.md` (kit 2, 2026-09-03 01:38 UTC) and its
02:00–02:15 UTC addendum · **Analysed:** `main` @ `61982ab5` (which is
`c967d6db` plus docs and client commits; every server file cited here is
byte-identical between the two) · **Fleet at analysis time:** nuc4
`v0.3.0-466-gc967d6db`, m6 and nynuc `v0.3.0-464-g9646f99f`, all built
2026-09-03 01:25–01:28 UTC · **Written:** 2026-09-03 · **Builder:** opus

Companion to [M5A-VERIFICATION-ON-NUC4.md](M5A-VERIFICATION-ON-NUC4.md)
(how to prove the conversion on real media) and
[PLAYBACK-CAPS-V2-PLAN.md](PLAYBACK-CAPS-V2-PLAN.md) (the plan whose M3
this finishes for the web) — this is *what the diagnosis got right, what it
got wrong, and the four bounded pieces of work that remain*.

**Standing instruction.** Work milestone by milestone, one PR each, in
order; §5 says why the order matters. Every `file:line` below is against
`61982ab5` and will drift — re-verify each seam at build time before
editing it. If a milestone seems to require changing `decide()`, the wire
shape of `SessionRequest`, or the rule that a client cannot ask for a
conversion, stop and flag it: those are guardrails (§4), not oversights.
Set up [AGENT-COMPILE-LOOP.md](AGENT-COMPILE-LOOP.md) before writing any
Rust. Five milestones is the multi-task case in AGENTS.md: branch
`effort/dv-p7-web-delivery` from current `main`, one task PR per
milestone into it, commits with `PLURX_EFFORT_COMMIT=1`, then the effort
PR into `main` after `git merge --no-ff origin/main` and the promotion
gate. M0's STATUS.md correction may go direct to `main` as its own docs
PR if the effort will take days. Paul merges.

---

## 1. The verdict on the diagnosis — right mechanism, already merged

The diagnosis describes a real defect, precisely and with the right
mechanism. It is the defect that PR #842 (`c6892bf5`, "derive the DV
conversion for builds that send no caps document", merged 2026-09-03
01:20 UTC as `60e1be68`) fixed — and #842 is an ancestor of `c967d6db`,
the SHA the diagnosis names as its repo state. Its §3 root cause is closed
at the SHA it was written against, and on every node of the fleet.

### 1.1 §2.2 is false at `c967d6db`: the no-caps arm does return a review

The diagnosis says that without a v2 caps document `review = None`, so
`apply_plan_review` never runs and the session keeps
`convert_dolby_vision: false`. That was true before #842. At `c967d6db`
the `(None, Some(file))` arm reads
(`crates/plurxd/src/http/hls.rs:1470-1497`):

```rust
(None, Some(file)) => {
    plan_derivation::count_legacy_trusted();
    tracing::warn!(file_id = id, client_build = %client_build,
        "create trusted the client's plan echo: this build sends no caps document");
    // … So this arm derives the one field the echo cannot carry, and
    // nothing else.
    Some(legacy_trusted_review(
        file,
        &super::stream::render_caps(&state).await,
        req.preserve_dolby_vision == Some(true),
        req.hdr10 == Some(true),
    ))
}
```

and `legacy_trusted_review` (`hls.rs:855-870`) derives the flag the echo
cannot carry from the file and the node:

```rust
fn legacy_trusted_review(file, node, asked_preserve_dolby_vision, asked_hdr10) -> PlanReview {
    PlanReview {
        preserve_dolby_vision: asked_preserve_dolby_vision,
        convert_dolby_vision: asked_preserve_dolby_vision
            && node.dolby_vision_convert
            && plurx_core::playback::file_can_convert_to_p81(file),
        hdr10: asked_hdr10,
        notes: Vec::new(),
        mismatched: false,
    }
}
```

`resolve_plan` then applies it unconditionally
(`hls.rs:1350-1353`: `match review { Some(review) =>
apply_plan_review(&mut request, review), None => Vec::new() }`). Only two
arms still return `None`: an unreadable caps document
(`hls.rs:1455-1465`) and a missing file row on its way to a 404
(`hls.rs:1469`). The line the diagnosis cites for the log
(`hls.rs:1425-1428`) is the *caps-present* arm at `c967d6db`; the warn it
quotes is at `:1475`, inside the arm that returns `Some`. Whatever tree
the diagnosis read, it was not the one it named: at `26cb567b`
(2026-09-01, the last merge before #842's lane) the same warn sits at
`:1217` and `legacy_trusted_review` does not exist — a checkout a day
stale reads exactly as the diagnosis describes.

### 1.2 The observed log is from a pre-#842 build

The argv in §1 of the diagnosis is the *preserve* branch: `-strict
unofficial` and `filter_units=remove_types=32-34`. On a #842 build that
argv cannot be produced for file 70 from a browser. With #842, a create
whose `/decision` said "Profile 7 converted to Profile 8.1" builds the
session `Copy { preserve: true, convert: true }`; the VOD attempt fails
`vod_index_pending`; live-HLS recovery runs `start_live_recovery_session`
(`crates/plurxd/src/transcode.rs:13565-13616`), which carries
`convert_dolby_vision` into `CopySessionOptions`; and
`start_copy_with_audio_offset` applies `served_copy_options`
(`transcode.rs:15736-15746`), which turns `{preserve: true, convert: true}`
into `{preserve: false, convert: false}` and logs

```
INFO this copy cannot convert Dolby Vision, so it strips to the HDR10 base
     instead of preserving a profile this client did not claim
```

The strip branch then emits `dovi_rpu=strip=1,filter_units=remove_types=32-34|62-63`
with **no** `-strict unofficial`. The diagnosis's log has neither the
info line nor that bsf. It is the same file-70 observation that motivated
#842 (#842's commit message quotes it at 23:16 UTC 2026-09-02), on a
build the fleet no longer runs. As far as the logs this review saw go,
nobody has played file 70 from a browser since the 01:25 UTC deploy.
**That is M0.**

### 1.3 What holds, with corrections to the evidence

| Diagnosis § | Verdict | Correction |
|---|---|---|
| 2.1 `decide` elects convert; `preserve` follows `convert` | holds | `playback/mod.rs:1036-1045`, `:1129-1131` as cited |
| 2.2 the flag cannot cross the wire | **false at `c967d6db`** | §1.1 above. It *was* true before #842 |
| 2.3 `served_copy_options` is dead on the web ingress | **false at `c967d6db`** | it fires on every live-recovery copy of a converting session, and only there — the VOD path never calls it (`vodserve.rs:2137-2176` destructures `req.kind` directly) |
| 2.4 `remove_types=32-34` leaves NAL 62/63; RPU rewrite lives in `dvpipe`, whose only callers are `fragindex.rs` and `vodgen.rs`; `copyseg.rs` has no conversion | holds | the **marker argument is invalid**: `DV_CONVERT_MARKER` (`plurx-core/src/transcode/mod.rs:1509`) is stripped by `strip_plurx_markers` before exec in both pipes (`:1650`, `:1724`), and the log at `transcode.rs:15808-15816` prints the post-strip argv, so *no logged argv ever carries the marker* and its absence proves nothing. The bsf string alone (`32-34` vs `32-34\|63`) is the proof. The "warning at `:1432-1440`" is about promoting parameter sets into `hvcC` for `dvh1`, not about a non-compatible stream wearing a compatible entry |
| 2.5 `vod_index_pending` candidates | holds, incomplete | the addendum found the real reason (§2, D3): the analysis queue is not draining at all |
| 3 root cause | correct mechanism, **already fixed** (#842) | |

Two things the diagnosis did not see that matter to this handoff:

- A preserved Profile 7 stream is described inconsistently by the server
  itself: `tag_for` (`plurx-core/src/transcode/mod.rs:533-564`) picks
  `hvc1` whenever the base is HDR10-compatible, while `copied_hls_codecs`
  (`transcode.rs:7241-7256`) advertises `CODECS="dvh1.07.LL"` for the same
  stream — the master playlist and the init disagree about the sample-entry
  fourcc, and nothing in the code defends `hvc1` for Profile 7 (the doc
  comment at `mod.rs:496-502` only justifies it for 8.1/8.4). This state
  is reachable only for a client that declares Profile 7, which no plurx
  client does (Apple `{5,8}`, web `{5,8}`, Android `{4,5,8}`). It is a
  latent inconsistency, not a live bug; §6 R3 asks whether to make it
  unrepresentable.
- The existing test `a_copy_that_cannot_convert_serves_and_describes_the_hdr10_base`
  (`transcode.rs:26695-26729`) constructs `CopySessionOptions` directly
  and *asserts* that `{preserve: true, convert: false}` stays preserved —
  it pins the served-options function, not the call site, and it pins the
  state the diagnosis feared as correct. M0 adds the call-site tests.

### 1.4 The five review questions, answered

1. **Is fix A implementable?** No, and it would be wrong as stated. The
   session holds no client facts: `SessionRequest`
   (`transcode.rs:7766-7824`) and `CopySessionOptions` (`:7888-7894`)
   carry no DV profile list, no `DeviceProfile`, no caps; nothing is
   persisted anywhere (`media_session_requests` stores a fingerprint and
   the create *response*; `playback_events` has no caps column; every
   reader of `dolby_vision_profiles` outside construction is `/decision`
   and `review_client_plan`). `SessionRequest` is a durable cluster
   contract (`SessionKind` is `deny_unknown_fields`, `:7849`; validated
   by `worker_session_request_is_valid`), so adding a field there is a
   Store-contract change. And "keep preserve only if the declared profiles
   contain the file's profile" strips the converting case too (client
   declares 8, file is 7) unless gated on `convert == false` — on the VOD
   path `convert == true` is the normal state and
   `with_dolby_vision_conversion` re-forces `preserve = true`
   (`mod.rs:444-450`).
2. **Is the declared profile set trustworthy?** It is the same source:
   `/decision` builds `DeviceProfile` from the caps document
   (`stream.rs:274-336`), and `review_client_plan` runs `decide_forced`
   with a `DeviceProfile` built from the caps document in the create body
   (`hls.rs:904-931`). Same parser, same `decide`. They cannot disagree
   for a client that sends the same document to both — which is exactly
   what the web client does not do today (D1).
3. **Is `hvc1` correct?** For the *strip* both paths are right: the
   segmenter yields `hvc1`, no `dvcC`/`dvvC` (the `dovi_rpu=strip` bsf
   drops the side data), and `sanitize_stale_dolby_brand`
   (`copyseg.rs:600-610`) rewrites `dby1`; the VOD path does the same via
   `DolbyVisionPass::Remove` (`fragindex.rs:516-526` →
   `fmp4.rs:1211-1215`) and `vodgen.rs:253`. For a *preserved* Profile 7
   it is the undefended combination above. For the *converted* 8.1 stream
   `hvc1` + `SUPPLEMENTAL-CODECS` is the Apple HLS shape and is what
   `copied_hls_codecs` emits from its `convert_dolby_vision` arm
   (`transcode.rs:7228-7239`: `db1p` for compatibility id 1) — reachable
   only from the VOD path, since the copy session zeroes `convert`.
4. **Are the natives exposed?** No. Apple sends `caps: decisionCaps` on
   every create and refuses to create without one
   (`PlayerController.swift:2684-2731`); Android sets `caps = decisionCaps`
   in `bindDecisionPlan` (`SubtitlePolicy.kt:341-348`, called from
   `Controller.kt:1016,1107`). Both take the `review_client_plan` arm.
   Only the web client takes `legacy_trusted`. Apple's
   `forceCompatibleHDRBase` retry relies on the "downward echo stands"
   rule (`hls.rs:983-987`), not on `overrides.compatible_hdr_base`.
5. **Is the truncated-segment classification a second defect?** The
   classification is accurate; the *message* is wrong way round and the
   event is un-joinable. The `<video>` error path (`index.html:9427-9470`)
   sends `stream_rejected` with no `session`, so the server cannot join
   it to the session it superseded (`system.rs:831-836` joins only when
   `session_id` is present), and `METRICS.record` (`telemetry.rs:60-153`)
   does not count it. The client already holds `PLAYER.deliveredDvProfile`
   and `PLAYER.deliveredRange` (`index.html:7057-7061`) and its own
   `PLAY_CAPS.dvprofile` when it writes "browser refused the remux
   stream". That is M2.

### 1.5 The fixes, re-ranked

| | Diagnosis rank | This review |
|---|---|---|
| A derive served DV handling from client facts on the session | preferred | **not implementable** without a new field on a durable contract; strips the converting case as written |
| B carry `convert` on the wire / one enum | second | **unnecessary** after #842 and contradicts the standing rule (`hls.rs:652-658`) that a client cannot ask for a conversion |
| C web sends caps on create | companion | **the fix**: one client change; the server arm exists, is counted, and is the migration the plan already names (`docs/PLAYBACK.md`: `legacy_trusted` "zero is the migration finished") |
| D indexer honesty | separate | real, and the addendum found it is the *whole* reason file 70 is cold: the queue is not draining (§2 D3) |

---

## 2. What is actually left — four defects, none of them the one diagnosed

**D1 — the web player sends no caps on create.** `openSession`
(`index.html:7002-7028`) posts `opts ∪ {playback_id, request_id} ∪
vodClientContract().session` plus `native_subtitles`/`subtitle`/
`control_sequence` when they apply — no caps; the caller at `:7837-7839`
passes `{copy, aac, preserve_dolby_vision, start, audio, audio_offset_ms}`;
`transcodeOpts` (`:6575-6595`) passes height/start/hdr10/burn. `create`
takes `Json<CreateSession>` with no `Query` (`hls.rs:1364-1371`), so a
query-string `CAPS_Q` would be ignored anyway. Every web create therefore
lands in `legacy_trusted`, trusting an echo the server has no way to check.
#842 made that echo *safe for the conversion*; it is still an echo. The
fleet counter `plan_derivation.legacy_trusted` on `GET /api/v1/system`
will never reach zero while the web client is the straggler.

**D2 — the cold first play of a Profile 7 title is HDR10 by construction.**
The conversion exists only on the VOD path (`dvpipe::Converter`, called
from `fragindex.rs:120,202` and `vodgen.rs:229,258`); live-HLS recovery
cannot convert and strips instead (§1.2). Until the converting fragment
index exists for the file, every play — from every client — is HDR10.
That is the designed behaviour (index once per file, never per session:
the 2026-08-29 ruling in PLAYBACK-CAPS-V2-PLAN §9), and it is *correct*
that it plays. What is wrong is that (a) STATUS.md calls the 2026-09-02
failure "a DV Profile 7→8.1 remux the browser refused" — it was the raw
Profile 7 remux, on a pre-#842 build, and no converted stream has ever
been served to a browser; and (b) nothing tells the operator that a title
is *waiting* for its converting index rather than *having* one.

**D3 — the index is never built, and the reason is invisible.** Two
layers:

- Node-local pass (`state.rs:5511-5530`): `Truncated` and `Unsupported`
  outcomes are logged and the cursor moves on; nothing durable is
  written, so on the next library wrap the same file is tried again, and
  to `vodserve` it is `vod_index_pending` forever. The cluster worker
  (`state.rs:6722-6759`) *does* store them — `fail_cluster_fragment_index`
  with `"truncated"` or `"unsupported"`, both `retryable: false`
  (`:6733`, `:6752`), so in cluster mode a never-indexable file is at
  least visible as `failed`.
- Cluster queue: an attempt is charged **on claim**
  (`store/sqlite/fragment_index_cluster.rs:589-595`, jobs `:1990-1997`);
  a lease that expires is recycled to `queued` with
  `last_error_code='lease_expired'` and the attempt is *not* refunded
  (`:534-547`); at `attempts >= max_attempts` (default 5,
  `store/fragment_index_cluster.rs:521`) the row fails `attempt_limit`
  (`:557-563`). Only `pipeline_version_unavailable`, `foreground_preempted`
  and `queue_full_or_busy` refund (`state.rs:6099-6101, 6161-6180,
  6267-6269`). The
  heartbeat is `lease/3` = 20 s against a 60 s lease, while a build is
  budgeted up to 30 min (`index_file_budget`, `state.rs:1359-1369`), so
  one write outage longer than 40 s on the owning node costs a life. And
  `target_node_id` is pinned to the coordinator's node at request time
  (`state.rs:3147-3151`; claim predicate `:568-571`) — media is
  node-local, so nothing can retry elsewhere. The addendum's observation
  (643 queued, 11 running on dead leases 167–208 s past expiry, nynuc
  unable to send raft acks, 1,933 `attempt_limit` rows = the same
  condition counted five times) is exactly what this code does when one
  node cannot complete replicated writes. The code confirms the chain;
  the raft membership fault itself (§4.2 of the addendum: `[1,5,6,7]`
  with nodes 3 and 4 still referenced, six node ids for four hosts) is
  ops, not code, and is the gate on M5.

**D4 — the rejection report blames the browser and cannot be joined.**
§1.4 Q5. The client knows what it declared and what it was handed; it
says neither.

---

## 3. Contract — the seams, copied from `main` @ `61982ab5`

Re-verify every one of these at build time; the line numbers are the
ones that will move.

### 3.1 `create`'s three arms and the review they produce

`hls.rs:1420-1497`, keyed on `(req.caps.as_ref(), source.as_ref())`:

| Arm | Counter | Review |
|---|---|---|
| `(Some(caps), Some(file))` with `caps.v == DeviceCaps::VERSION && !caps.is_empty()` | `rederived` (+`mismatched`, `overridden`) | `review_client_plan(caps, req.overrides, file, render_caps, asked_preserve, asked_hdr10, now)` → `decide_forced` with the real `DeviceProfile`; `plan_mismatch` is a **warn**, never a refusal |
| `(Some(caps), Some(_))` otherwise | `unusable_caps` | `None` — trusts the echo, sets nothing |
| `(_, None)` | none | `None` — on its way to a 404 |
| `(None, Some(file))` | `legacy_trusted` | `legacy_trusted_review(...)` — §1.1 |

`PlanReview { preserve_dolby_vision, convert_dolby_vision, hdr10, notes,
mismatched }`. `apply_plan_review` (`hls.rs:810-826`) is the **only**
writer of `SessionKind::Copy.convert_dolby_vision`; keep it that way.
Counters are `plan_derivation` (`hls.rs:693-730`), read on
`GET /api/v1/system` as `plan_derivation.{legacy_trusted, unusable_caps,
rederived, mismatched, overridden}` (`system.rs:175-266`).

### 3.2 `SessionRequest` is a durable cluster contract

`transcode.rs:7766-7824` — `file_id, playback_id, request_id,
control_sequence, automatic, previous_session_id, reopen_reason, kind:
SessionKind, start_seconds, audio_index, subtitle_burn, audio_offset_ms,
hdr10, presentation, block_budget_secs`. `SessionKind::Copy { aac,
preserve_dolby_vision, convert_dolby_vision }` is `deny_unknown_fields`
(`:7849`). It is replicated to the owning node and validated by
`worker_session_request_is_valid` (`media_sessions.rs:954`). **Do not add
client capability fields here.** Caps belong on `CreateSession`
(`hls.rs:530-637`, field `caps: Option<DeviceCaps>` at `:626`), consumed
by `create`, and never travel further.

### 3.3 `served_copy_options` and the one place it runs

```rust
// transcode.rs:7205-7211
fn served_copy_options(options: CopySessionOptions) -> CopySessionOptions {
    CopySessionOptions {
        preserve_dolby_vision: options.preserve_dolby_vision && !options.convert_dolby_vision,
        convert_dolby_vision: false,
        ..options
    }
}
```

Applied at `transcode.rs:15736-15746` inside `start_copy_with_audio_offset`,
the live-HLS recovery copy, *before* `CopyVideoOptions::from_probe(...,
preserve)` (`:15747-15752`) and `DolbyVisionCopyOptions::new(have_dovi,
preserve)` (`:15764`) choose the bsf. Reached from
`start_live_recovery_session` (`:13565-13616`) which builds
`CopySessionOptions { convert_dolby_vision, transcode_audio: aac,
preserve_dolby_vision }` straight from `req.kind`. Live recovery is
gated by the Cargo feature `live-hls-recovery` (default on,
`plurxd/Cargo.toml:10`) and the setting `playback.vod_live_recovery`
(production: on unless `"0"`, `transcode.rs:13546-13562`); it is entered
on `vod_index_pending | vod_transcode_unavailable |
vod_subtitle_burn_unavailable | vod_source_unsupported`
(`:13457-13518`). Downstream consumers already read the *served* value:
`copied_hls_codecs` (`:15826-15827`), the returned `SessionKind`
(`:15828-15832`), and the create response's `delivered_dynamic_range` /
`delivered_dolby_vision_profile` (`http/hls.rs:1069-1112`).

### 3.4 The bsf and the sample entry

```rust
// plurx-core/src/transcode/mod.rs:293-316
pub fn hevc_copy_bsf_for_copy(hdr, have_dovi_bsf, preserve_dolby_vision, convert_dolby_vision) -> String {
    if hdr == Some("dolby_vision") && convert_dolby_vision {
        "filter_units=remove_types=32-34|63"          // EL dropped; RPU (62) kept for dvpipe
    } else if hdr == Some("dolby_vision") && preserve_dolby_vision {
        "filter_units=remove_types=32-34"             // RPU and EL both kept
    } else if hdr == Some("dolby_vision") {
        if have_dovi_bsf { "dovi_rpu=strip=1,filter_units=remove_types=32-34|62-63" }
        else             { "filter_units=remove_types=32-34|62-63" }
    } else {
        "filter_units=remove_types=32-34"
    }
}
```

32/33/34 = VPS/SPS/PPS (out-of-band in `hvcC`; `mod.rs:360-364`), 62 =
RPU, 63 = enhancement layer (`mod.rs:282-284`). Called from
`copy_video_args` at `mod.rs:1471-1476` with `(source.hdr,
options.have_dovi_bsf, options.preserve_dolby_vision, options.dv_convert)`.
Note the parameter-set-promotion branch (`mod.rs:1444-1468`) builds its
own filter string and the Profile 5 preserve branch emits no `-bsf:v`
at all — the four-way function is not the only bsf author.
`-strict unofficial` is emitted only when `hdr == dolby_vision &&
preserve` (`mod.rs:1428-1431`). `tag_for` (`mod.rs:533-564`): `dvh1` only
when `preserve && !compatible_base`; `compatible_base` = `bl_compat_id ∈
{1,4,6}` or a label containing "HDR10-compatible"/"HLG-compatible".

### 3.5 The web client

```js
// index.html:7002-7028 — the only session-create call site
async function openSession(fileId, opts){
  const contract=vodClientContract();
  const body=Object.assign({},opts||{},
    {playback_id:PLAYBACK_ID,request_id:newRequestId()},contract.session);
  // … native_subtitles / subtitle / control_sequence / height==null …
  return api(`/files/${fileId}/hls/sessions`,{method:"POST",body});
}
// index.html:7837-7839 — the remux open
info=await openSession(PLAYER.fileId,{copy:true,aac,
  preserve_dolby_vision:!!PLAYER.preserveDolbyVision,start:startSec||0,audio:aidx,
  audio_offset_ms:PLAYER.aoffset||0});
// index.html:5378-5379 — the only place caps are sent today
return await api(`/files/${fileId}/decision?${query}`,
  {method:"POST", body:{caps:capsDocument(PLAY_CAPS, decodeLimits())}});
```

`capsDocument(c, limits)` is at `index.html:7309`; `decodeLimits()` reads
`localStorage["plurx_decode_limits"]`. `askDecision` falls back to the
GET `CAPS_Q` form on 404/405/400 for a mixed fleet; create needs no such
fallback as long as every node runs a build with `CreateSession.caps`
(caps v2 M3) — true of the three builds read at analysis time, and worth
one `/api/v1/server` sweep before M1 deploys. The `<video>` error handler is `index.html:9427-9470`;
`clientLog` (`:3266-3282`) posts to `/client-log` with `{ua, method,
title, file_id, vcodec} ∪ ev`; `playbackContext()` (`:3362-3380`) is the
helper the hls.js path uses to attach `session: p.sessionId`. The client
holds `PLAYER.deliveredRange` and `PLAYER.deliveredDvProfile` from the
create response (`:7057-7061`) and from the decision (`:7652`).

The executing test harness is `tests/playback/web-policy.test.js`: it
slices a top-level function out of the shipped `index.html` by name
(`shippedSource("openSession")`), builds it with `new Function` against
fake `api`/`newRequestId`/`vodClientContract`/`PLAYER`, **runs it**, and
asserts on the captured request body (`:142-179`; `shippedSource` is at
`:47`). Use that pattern — never a source-grep on the JS, which passes
identically against a dead function. Runs under `make web-check`.

### 3.6 The indexer

```rust
// state.rs:5511-5530 — node-local pass outcome handling
crate::fragindex::IndexOutcome::Built(index) => { /* put_fragment_index */ }
crate::fragindex::IndexOutcome::Truncated { reason, rows } => {
    tracing::warn!(file_id, rows, "fragment index incomplete: {reason}");
}
crate::fragindex::IndexOutcome::Unsupported(reason) => {
    tracing::warn!(file_id, "file cannot be indexed: {reason}");
}
```

Bounds: `INDEX_MAX_PER_PASS = 4`, `INDEX_MAX_EXAMINED_PER_PASS = 200`,
`INDEX_WINDOW = 120 s`, per-file budget `clamp(film_secs/8 + 30, 90, 1800)` s
(`state.rs:1341-1369`). Identities per DV file
(`fragindex.rs:463-490 video_identities`): stripped, preserved, and —
when `convert && file_can_convert_to_p81(file)` — converting. The
converting identity needs the DV *columns* (`dv_profile`,
`dv_bl_compat_id`, `dv_level`; `playback/mod.rs:1073-1091`), not the
label; a label-only row gets no converting identity while `/decision`
may still route to a conversion (the doc comment at `:1055-1056` names
this). `fragment_index_requested_video_options` (`state.rs:1784-1803`)
returns the first identity *missing* an index.

Queue constants (`store/fragment_index_cluster.rs:521-528`):
`DEFAULT_ANALYSIS_MAX_ATTEMPTS = 5` (settings `analysis.max_attempts`, max
20), `DEFAULT_ANALYSIS_LEASE_SECS = 60` (`analysis.lease_secs`, max 600),
backoff base 5 s / max 300 s. Lifecycle accounting already exists:
`analysis_lifecycle_counters(event, reason)` with a trigger that counts
`('lease_loss','lease_expired')` on every recycle
(`store/fragment_index_cluster.rs:394-430`), and `analysis_attempts` rows
carry `terminal_code`. The store contract for the attempt policy is
pinned in `plurx-core/tests/store_contract.rs:16357-16369`
(`lease_loss/lease_expired` then `failure/attempt_limit`); any change to
the policy changes that contract and must run under
`make cluster-store-check` (about 21 minutes on the two-core cloud
container, measured 2026-09-02).

---

## 4. Non-goals — guardrails, each with its reason

- **Do not add `convert_dolby_vision` to `CreateSession`** (fix B). A
  client has no way to be right about it; the standing rule at
  `hls.rs:652-658` was written deliberately and #842 made it hold.
- **Do not put client capability facts on `SessionRequest` /
  `SessionKind`** (fix A). Durable, replicated, `deny_unknown_fields`;
  the reviewed-plan arm already does what A wanted, with the real
  `DeviceProfile`.
- **Do not change `decide()`'s ladder or reason strings.** Every string is
  pinned by tests and read by three clients.
- **Do not make live-HLS recovery convert.** The 2026-08-31 timeline
  ruling (`DV-CONVERT-TIMELINE-RULING.md` in kit 2; summarised in
  [M5A-VERIFICATION-ON-NUC4.md](M5A-VERIFICATION-ON-NUC4.md) §1) put the
  RPU rewrite after the mux, on fragments the VOD path already parses. The segmenter has no
  such stage and should not grow one per session.
- **Do not make the fragment-index identity ignore DV argv.** The bytes
  differ; that ruling is in PLAYBACK-CAPS-V2-PLAN §9.
- **Do not touch `exact_hls_context` without a master-playlist test.**
- **Do not retarget analysis jobs to another node** as the fix for D3.
  Media is node-local (m6 needs the qnap mounts); a job on the wrong node
  fails on the source, not the lease.
- **Do not fix the raft membership from code.** The stale members
  (addendum §4.2) are an ops repair — Paul's, or a GPT prompt from Paul —
  and M5 waits for it.
- **PRs, not merges.** Paul merges.

---

## 5. Milestones

Order: M0 first because it decides whether anything else is urgent; M1
before M2 so the rejection report has caps to compare against; M3 and M4
are independent of M1/M2 and of each other; M5 last, gated on ops.

### M0 — re-test on the deployed build, correct the record, pin the call sites

**Re-test.** Play file 70 from Safari on the fleet as deployed
(`v0.3.0-466-gc967d6db` on nuc4). Expected on a cold file:

```bash
docker logs plurxd --since 10m 2>&1 | grep -E \
  'playback capability decision|create trusted|vod_index_pending|cannot convert Dolby Vision|copy-video HLS ffmpeg args|stream_rejected'
```

- `decision … method=Remux preserve_dolby_vision=true` with the
  "converted to Profile 8.1" reason;
- `create trusted the client's plan echo` (D1 — still expected until M1);
- `VOD prerequisite unavailable … refusal="vod_index_pending"`;
- `this copy cannot convert Dolby Vision, so it strips to the HDR10 base`;
- argv with **no** `-strict unofficial`, `-tag:v hvc1`, and a filter
  carrying `dovi_rpu=strip=1` or `remove_types=…62-63` (the exact string
  depends on whether the source's `hvcC` needs parameter-set promotion,
  `plurx-core/src/transcode/mod.rs:1444-1468`);
- create response `delivered_dynamic_range="hdr10"`,
  `delivered_dolby_vision_profile` absent — and the picture plays.

If instead the preserve argv (`-strict unofficial … remove_types=32-34`)
appears, the fleet is not running what `/api/v1/server` says, or a node
answered create with `render_caps().dolby_vision_convert == false` —
check `GET /api/v1/system | jq .dovi_rpu` and `PLURX_DV_CONVERT` on that
node before doing anything else. Record the result in the PR.

**Correct STATUS.md.** Done in the PR that carries this document: the
#840 entry now says the refused stream was the raw Profile 7 remux a
pre-#842 build served through live-HLS recovery. Keep it that way, and
keep `tests/operations/test_status_pr_claims.py` green (no in-flight
phrase within 60 chars of a landed PR number) when adding M0's own entry.

**Pin the call sites** (both in `crates/plurxd`; the first must fail with
the pre-#842 arm restored, the second with the `served_copy_options` call
deleted):

1. **The arm.** Only `create()` reaches `(None, Some(file))`;
   `resolve_plan` takes the review as a parameter, so a test built on
   `bare_create()` + `resolve_plan(PlanInputs{..},
   Some(legacy_trusted_review(..)), body)` (`hls.rs:14726-14751`,
   `:14949`) pins the review, not the arm — and
   `a_build_with_no_caps_document_still_converts_profile_7`
   (`hls.rs:14615-14661`) already does that. Route this one through the
   router harness in `crates/plurxd/src/http/mod.rs` (`call(&app, …)`
   POSTing `/api/v1/files/{id}/hls/sessions`; existing uses at `:968`,
   `:1042`, `:7797`) with a no-caps `{copy:true, preserve_dolby_vision:true}`
   body on a Profile 7 fixture with DV columns (`dolby_vision_p8_file` at
   `hls.rs:14507` is the shape; build the P7 twin) and a node with
   `dolby_vision_convert = true`. Assert `plan_derivation::snapshot()`
   moved `legacy_trusted` by one and the create response says
   `delivered_dolby_vision_profile: 8` (the badge helpers at
   `hls.rs:1069-1112` read the built kind, so `8` there means
   `convert_dolby_vision = true` reached the request).
2. **The strip.** Enter via the `#[cfg(test)]` wrapper `start_copy`
   (`transcode.rs:15629-15654`) with `CopySessionOptions { preserve: true,
   convert: true }` on a Profile 7 fixture, the way
   `a_client_fetch_releases_a_held_session_and_restarts_progress`
   (`:28803`; `require_ffmpeg()` + `write_real_video` +
   `seed_file_with_probe_at`, `:29182`) does. **Nothing in the crate
   captures a spawned argv today** — the argv exists only in the
   `tracing::info!` at `:15808-15816` and the exec at `spawn_ffmpeg`
   (`:1484`, `PLURX_FFMPEG`). Add one of: a scoped `tracing` subscriber
   that captures the `copy-video HLS ffmpeg args` line, or a `PLURX_FFMPEG`
   shim script that appends its argv to a file in the session dir (the env
   var is process-global, so serialize the test). Assert the invariant,
   not one string — `copy_video_args` has three strip shapes
   (`plurx-core/src/transcode/mod.rs:311`, `:1444-1468`): `-strict
   unofficial` absent, `-tag:v hvc1`, and a filter containing
   `dovi_rpu=strip=1` or `remove_types=…62-63`. Also assert
   `info.kind == Copy { preserve: false, convert: false }`. Mutation
   check: with the `served_copy_options` call deleted the test must fail
   on the argv, not only on the kind.

**Acceptance:** the log above on a real play; `cargo test -p plurxd
--bin plurxd` green with both tests; each test shown red under its
mutation in the PR description.

### M1 — the web player sends its caps on every create (fix C)

**Change.** In `openSession` add `caps: capsDocument(PLAY_CAPS,
decodeLimits())` to the body when `PLAY_CAPS` is populated (it is, before
any play — `askDecision` already depends on it). Every create goes through
`openSession`, so the remux open, `transcodeOpts` fallback, seeks and
audio switches all pick it up in one place. Do **not** also fill
`reopen_reason` from the transcode fallback: `ReopenReason` has exactly
one variant, `Stall` (`transcode.rs:7873-7877`), which binds the new
session to its predecessor's rung and requires a `request_id`; an unknown
string is a 400 at the `Json` extractor. A `StreamRejected` variant is a
`SessionRequest` contract change — §6 R4, not this milestone.

**Server.** No change. Confirm `plan_mismatch` stays a warn and that a
web create with an honest echo produces `rederived` with `mismatched = 0`
and no `plan_notes` in the response.

**Test** (in `web-policy.test.js`, the executing harness): build
`openSession` with a fake `api` and a fake `capsDocument`, call it with
the remux opts, assert `body.caps` is the document and the four existing
fields are unchanged. A second assertion: `capsDocument` is called with
`(PLAY_CAPS, decodeLimits())` — the same arguments `askDecision` passes —
so the two calls cannot drift.

**Acceptance:** on the fleet, one web play and then
`curl -s -H "authorization: Bearer $T" $NODE/api/v1/system | jq
.plan_derivation` shows `rederived` incremented and `legacy_trusted`
unchanged (the `rederived` arm logs nothing on the happy path — the
absence of `create trusted the client's plan echo` and of `plan_mismatch`
in the log is the check). `make web-check` green.

### M2 — the rejection report says what was handed to whom

**Client.** In the `<video>` error path (`index.html:9427-9470`) and the
hls.js fatal path (`:6440-6485`), the `stream_rejected` event carries
`session: PLAYER.sessionId` (use `playbackContext()`), `delivered_range:
PLAYER.deliveredRange`, `delivered_dv_profile: PLAYER.deliveredDvProfile`,
and `declared_dv_profiles: PLAY_CAPS.dvprofile`. Message, when
`delivered_dv_profile` is set and not in the declared set:

```
the server handed this browser a Dolby Vision Profile 7 stream it did not
declare (declared: 5,8; code 3: the browser's decoder failed) — re-encoding
```

otherwise the current wording, with "the server's stream" in place of
"browser refused" — the browser did what its caps said it would.

**Server.** `ClientLog` (`system.rs:541-611`) gains the three optional
fields (`declared_dv_profiles` arrives as the CSV string `PLAY_CAPS.dvprofile`
is, e.g. `"5,8"` — `index.html:7246`, `:7270` — split it server-side);
`client_log_line` (`:1238-1352`) prints them; when the joined session's
`delivered_dolby_vision_profile` is outside `declared_dv_profiles`, add a
`caps_mismatch=true` field to the line and the stored event. Do **not**
overwrite `detail` — the hls.js path already fills it with `d.type`
(`:562`). `PlaybackEventsQuery` (`:419-424`) filters only on
`since`/`event`/`limit`; filtering on the new field is `jq` until someone
needs more. Do not add a `METRICS` label (fixed-label rule,
`telemetry.rs:60-153`); the event store is the durable record.

**Test:** the existing `client_log_line` unit (`system.rs`) extended with
the new fields; a web-policy test that runs the error handler's message
builder (slice it into a named function first — it is inline today) for
the two cases.

**Acceptance:** a forced rejection (M0's pre-#842 build is gone, so use a
title the browser genuinely cannot decode, or a fixture) yields a
`client[Safari] stream_rejected … session=… delivered_dv_profile=7
declared_dv_profiles=5,8 caps_mismatch=true` line.

### M3 — the node-local indexer remembers what it could not do (fix D)

**Change.** On `Truncated`/`Unsupported` in the node-local pass
(`state.rs:5511-5530`), record the outcome durably using the cluster
worker's error codes (`"truncated"`, `"unsupported"`). The cluster
worker marks both terminal; this milestone adds a policy neither layer
has today — `"truncated"` retried after a backoff (the per-file budget
is a wall-clock guess, and a slow mount can pass on a quiet day),
`"unsupported"` terminal until the file's `SourceIdentity` changes — and
applies it to the cluster worker too, so the two layers agree. And
have `ordered_index_paths` skip terminal rows. Expose it on the admin
"indexed" badge (`browse.rs:352-382`), which already has four states —
`unsupported · indexed · partial · pending` — by giving `unsupported` its
recorded `reason` and by making `pending` distinguishable from "tried,
truncated at N rows". Reuse `analysis_requests` failure rows if the
node-local pass can be routed through `request_file_analysis` cheaply;
otherwise a small `fragment_index_outcomes(file_id, identity_fingerprint,
outcome, reason, updated_at_ms)` table with a Store-contract test.

**Test:** an `Unsupported` outcome is stored once, the file is not
re-examined on the next pass, and a changed `mtime` makes it eligible
again; a `Truncated` outcome is retried after the backoff and its `rows`
are kept so the operator can see how far it got.

**Acceptance:** `make unit` green; if the Store contract changed,
`make cluster-store-check` green and the run recorded in the PR.

### M4 — a lost lease must not spend the attempt budget (ruling: §6 R1)

**Change** (after Paul's ruling). Separate the two budgets: on the lease
sweep (`sqlite/fragment_index_cluster.rs:534-547`, jobs `:1947-1960`)
refund the attempt (`attempts = MAX(attempts - 1, 0)`) and increment a
new `lease_losses` column; fail with `last_error_code='lease_limit'` when
`lease_losses >= max_lease_losses` (new setting `analysis.max_lease_losses`,
default 10, max 100). The schema work is larger than it reads:

- a `lease_losses` column on **both** `analysis_requests` and
  `cluster_fragment_index_jobs`;
- `'lease_limit'` in the `analysis_lifecycle_counters` reason CHECK — a
  `CREATE TABLE … STRICT` (`store/fragment_index_cluster.rs:393-404`),
  so SQLite needs a table-rebuild migration in the style of
  `ANALYSIS_COMPONENTS_SCHEMA` (`:160-212`) — and in **both** triggers
  (`analysis_requests_lifecycle_counters` `:405-466`,
  `cluster_fragment_index_lifecycle_counters` `:467-517`);
- the recycle/claim SQL in **both** backends: `store/sqlite/fragment_index_cluster.rs`
  (12 `lease_expired`/`attempt_limit` sites) and
  `store/hiqlite_fragment_index_cluster.rs` (19 sites, `:455-580` for the
  table and triggers);
- the store contract: `store_contract.rs:16306-16375` covers
  `cluster_fragment_index_jobs` only — add the `analysis_requests` twin.

A job that crashes its worker still terminates (via `lease_limit`), so
the poison-job protection the current design buys is kept.

**Why not just raise `max_attempts`?** Because the cause is one node that
cannot complete a replicated write, and the attempt budget exists for a
different failure (a job that fails on its own content). Mixing them is
how 643 jobs came to sit behind eleven dead leases with their lives spent.

**Acceptance:** the store contract passes with a lease loss leaving
`attempts` unchanged and `lease_losses = 1`; `make cluster-store-check`
green; the new key clamped like its siblings in
`store/fragment_index_cluster.rs:521-528` and exposed beside
`analysis.max_attempts` in the Settings UI (`index.html`).

### M5 — the first converted stream a browser ever plays

Gated on ops: the raft membership repair (addendum §4.2), then the queue
draining (`analysis_requests` `queued` falling, `running` leases fresh),
then file 70's converting identity built (`fragment_indexes` row for the
`dv_convert` fingerprint; `dv_conversions` non-empty on its owning node).
Then, per [M5A-VERIFICATION-ON-NUC4.md](M5A-VERIFICATION-ON-NUC4.md) §3
on the owning node, plus the web-specific reads:

```bash
# the init the browser is handed: dvcC must say profile 8, not 7
curl -s -H "authorization: Bearer $T" "$NODE/api/v1/…/init.mp4" | xxd | grep -i 'dvcc\|dvvc'
# the master playlist: hvc1 in CODECS, the DV profile in SUPPLEMENTAL-CODECS
curl -s -H "authorization: Bearer $T" "$NODE/api/v1/…/master.m3u8" | grep -i 'CODECS'
```

Expected: `CODECS="hvc1.2.4.L153.B0"`-shaped, `SUPPLEMENTAL-CODECS="dvh1.08.06/db1p"`,
create response `delivered_dolby_vision_profile=8`, Safari plays with the
DV badge; Chrome plays the same title as HDR10 (Chrome's caps probe never
answers yes to `dvh1.05.06`/`.08.07`, so `/decision` strips for it), and
`scripts/dv-timeline-check` passes from zero and from a seek. This is the
only milestone that *proves* the feature; everything above it removes a
reason it could not run.

---

## 6. Open rulings for Paul

- **R1 (M4).** Separate lease-loss budget as designed above, or leave
  the policy and only surface it? The current policy is defensible as
  written; it is wrong for a fleet where one node's replication can
  fail for minutes. Recommendation: separate budgets.
- **R2.** Should a *cold* Profile 7 play wait for its converting index
  (503 with a "converting, try again in N minutes" typed refusal, the
  way `vod_index_pending` already reads) rather than fall to the HDR10
  live recovery? Recommendation: no — playing HDR10 now beats a spinner,
  and M3 makes the wait visible to the operator instead. But the
  viewer-facing badge should say HDR10, which it does today
  (`delivered_dynamic_range` is served truth since `served_copy_options`).
- **R4 (M1).** Should the transcode fallback name its predecessor?
  `CreateSession` has `previous_session_id` and `reopen_reason`, but
  `ReopenReason` is `Stall` only and rides on `SessionRequest`. A
  `StreamRejected` variant would let the server join the rejection to the
  session it superseded without M2's client-side fields. Recommendation:
  not now — M2 gets the join through `session` on the event, and the
  contract change is not worth a Store round for one log line.
- **R3.** The preserved-Profile-7 state (`CODECS dvh1.07` over an `hvc1`
  entry) is reachable only by a client that declares 7, which none does.
  Leave it, or have `tag_for` refuse `hvc1` for profile 7 and
  `copied_hls_codecs` agree? Recommendation: leave it, note it in
  `PLAYBACK.md`, and revisit when Android claims 7 from a real decoder
  enumeration (PLAYBACK-CAPS-V2 M5a's Android half).

---

## 7. The fleet reads that settle each question

```bash
# which build each node runs (uptime tells you when it was deployed)
for h in 192.168.4.8 192.168.4.14 192.168.5.236; do curl -s http://$h:32400/api/v1/server | jq -c '{build,uptime_seconds}'; done
# the migration counter — web creates land in legacy_trusted until M1
curl -s -H "authorization: Bearer $T" http://$h:32400/api/v1/system | jq .plan_derivation
# the queue (on a node, with the WAL — copy plurx.db AND plurx.db-wal, or query in place)
sqlite3 plurx.db "SELECT state, COUNT(*) FROM analysis_requests GROUP BY state;"
sqlite3 plurx.db "SELECT event, reason, count FROM analysis_lifecycle_counters ORDER BY 1,2;"
sqlite3 plurx.db "SELECT request_id, owner_node_id, attempts, (lease_expires_ms/1000 - strftime('%s','now')) AS lease_left_s FROM analysis_requests WHERE state='running';"
# file 70's identities and which have an index
sqlite3 plurx.db "SELECT id, hdr_format, dv_profile, dv_bl_compat_id, dv_level FROM files WHERE id=70;"
sqlite3 plurx.db "SELECT file_id, argv_fingerprint, fragments, built_at_ms FROM fragment_indexes WHERE file_id=70;"
```

The addendum's own correction applies to every one of these: read the
database *with* its `-wal` or the last minutes are invisible.
