# Playback surface physical verification — the four recipes on real hardware

**Status:** open · **For:** a session at the physical devices ·
**Executes:** M4, §4.5 of
[PLAYBACK-SURFACE-CONTRACT-IMPLEMENTATION.md](PLAYBACK-SURFACE-CONTRACT-IMPLEMENTATION.md),
against the allowed-outcome table in its §7

M0, M1, M2, M3 and M5 of the [playback surface
contract](PLAYBACK-SURFACE-CONTRACT.md) are merged. The presenter is a pure
function of faults, evidence and identities on all three clients, every
blocking surface is supposed to sit over a player its recovery owner already
stopped, and every surface is supposed to retire on the picture's own
presentation evidence. **None of that has been observed on hardware.** M4 is
the observation.

This is not a test suite. Acceptance is a judgement against §7's table:
**every observed outcome is in the allowed set for its recipe, and every
`surface_disagreement` row names a documented exception.** The only documented
exception today is the iOS lock-screen `playCommand`. A recipe whose outcome
is in neither column is a finding, not a fail-and-move-on.

Nothing here changes code. If a device refuses — asleep, unpaired, no
fixture content — the refusal is the answer: record it as a verification gap,
which is not the same as a pass.

## 0. What you need

- **Three devices**: an Apple TV, an iPhone, and an Android TV (or Google TV).
  §4.5 names exactly these.
- The merged builds installed on them. Source claims **Apple build 149** and
  **Android versionCode 92**; check what actually installed rather than what
  the README claims. The install path is
  [CLIENT-DEPLOY-PROMPT.md](CLIENT-DEPLOY-PROMPT.md) — deploy is Paul's.
- A plurx server you can interfere with: pull its network, spin its NAS down,
  read its session log.
- A **4K remux** title (recipe a), a title on a NAS that can be made cold
  (recipe b), a **progressive-remux** title — Playback debug shows `Remux` and
  **no session id** (recipe c), and a **known-bad file** that reliably fails
  to decode (recipe d).

**Before you start, know where the evidence is.** On every client, the
Playback debug panel has a **SURFACE** section with five rows —
`Surface`, `Fault`, `Source`, `Attached/intent`, `History` — where `History`
is a notes strip of up to sixteen entries. That panel, plus the client log's
`surface_raised` / `surface_cleared` / `surface_disagreement` events, is the
whole record. Nothing else is asked for.

## 1. The four recipes, verbatim from §7

Run each on **every** device it applies to. Recipes (a), (b) and (d) are the
surface contract's; (c) is also the M6 gate and has its own, longer prompt at
[PLAYBACK-SURFACE-REMUX-ORIGIN-MEASUREMENT-PROMPT.md](PLAYBACK-SURFACE-REMUX-ORIGIN-MEASUREMENT-PROMPT.md)
— run it here for the surface outcome, and there for the measurement.

### (a) Network drop mid-film — Apple TV, iPhone, Android TV

**Steps.** 4K remux, ≥ 20 s buffered, pull the server's network for 5 s at
minute 2, restore.

**Allowed.**

- `recovering` indicator while the picture plays from buffer, then
  `cleared(by: presenting)` on the same or a new attached generation; **or**
- buffer drains → full-screen `recovering`/`buffering` → existing reopen →
  cleared; **or**
- ladder spent → `exhausted` with **rate 0** and Keep waiting works after the
  network returns.

**Not allowed.**

- any blocking surface with the picture moving;
- any surface that persists after 10 s of continuous presenting;
- any `surface_disagreement`.

> Note on "Keep waiting", because it is drawn differently on each client and
> the recipe names it. **Android** offers a real Keep waiting button
> (`PlayerScreen` maps it to `controller.keepWaiting()`). **Apple** does not
> draw one: `PlayerView.failureActions` removes `keep_waiting` whenever
> `retry` is present, because §3.1 says Apple's Keep waiting *is*
> `retryAfterPlaybackFailure` and two buttons doing one thing is worse than
> one — so the prompt reads **Try Again / Close**, and Try Again is the
> affordance the recipe means. Say which button the prompt actually offered
> and which you pressed. On the **web**, no site offers it at all (open ruling
> 1 in `STATUS.md`); the web is not one of M4's three devices, and a web
> observation may not stand in for one of them.

### (b) Failed change on a cold NAS — Apple TV, iPhone, Android TV

**Steps.** While playing, change quality so the create 503s (NAS spun down).

**Allowed.** `refused` banner with **Retry**; predecessor keeps playing;
banner retires when the change is re-issued and succeeds, or after 10 s
continuous presenting if the viewer does nothing.

**Not allowed.** full-screen anything; the predecessor's surface changing
because of the failed change.

> This is the recipe M5's blocker B1 broke on Android: the create-retry
> watchdog was armed on **every** create, not only in the `start` context, so
> a change against a cold NAS raced a 60 s stop-the-player timer against that
> client's own 60 s read timeout. Fixed in `b4cf8e81` and pinned by
> `CreateRetryTest.aChangeContextCreateIsNotRetriedAndArmsNoWatchdogAtAll`,
> which has never been compiled. Give this recipe more than one attempt on
> Android, and let one of them run past 60 s.

### (c) Android progressive-remux seek — Android TV

**Steps.** `Remux` title, **no session id**, seek +10 min, wait 25 s.

**Allowed.** picture resumes near the target; **or** `recovering` (the
target-deadline `recover` rung) then resumes. Capture the achieved origin and
the first-frame position either way.

**Not allowed.** a terminal *"couldn't reach the requested position"* over a
moving picture. **That is the M6 defect** — if it happens, record it and then
read the measurement prompt: M6 is built only if the measurement says so.

### (d) Lock screen — iPhone

**Steps.** Force `stopped` (e.g. a decoder failure on a known-bad file), lock
the phone, press play on the lock screen.

**Allowed.** banner *"Playback recovered"* with the fault's actions, and a
`surface_disagreement` row naming `lock_screen_play`.

**Not allowed.** a blocking surface over audio; no ledger row.

> This is the one documented `surface_disagreement` exception. A row here is
> expected; a row anywhere else is not.

## 2. What to capture

For **each recipe on each device**, paste the Playback debug **SURFACE**
section as it read at the moment of the outcome — all five rows:

```
Surface           <kind> / <class>
Fault             <title> — <detail>
Source            <source id>
Attached/intent   <attached generation> / <intent, or —>
History           <up to 16 entries, newest first>
```

Plus, separately:

- **Every `surface_disagreement` row seen anywhere in the run**, from any
  device, in any recipe — with the exception that explains it. The only
  documented exception is the iOS lock-screen `playCommand` (recipe d). A
  `surface_disagreement` with no documented exception behind it is the single
  most important thing this run can find: each one is a place where something
  started the player after its owner stopped it (contract §5), and it ends as
  either a fixed call site or a new documented exception. Do not average them
  away.
- For recipe (a): whether `rate` read **0** under any `exhausted`.
- For recipe (b): whether the predecessor kept playing, and whether the banner
  retired by re-issue or by the 10 s presenting rule.
- For recipe (c): the achieved `X-Plurx-Media-Origin-Ms` and the first-frame
  `realPosition()`, per the measurement prompt.

## 3. Where the result goes

One new document, **`docs/clients/PLAYBACK-SURFACE-PHYSICAL-VERIFICATION-<date>.md`**
(ISO date of the run), **and a row for it in `docs/README.md`'s `clients/`
table in the same commit** — `tests/operations/test_docs_index.py` fails the
build if a `docs/` Markdown file is not indexed. Follow
[PLAYER-INPUT-PHYSICAL-VERIFICATION-2026-09-02.md](PLAYER-INPUT-PHYSICAL-VERIFICATION-2026-09-02.md)
for tone: every required row explicit, PASS and FAIL both stated, and a
verification gap recorded as FAIL rather than converted into a passing claim.

The row goes in the `clients/` table. Its link text and its link target are
both the file name — the target prefixed with `clients/` — the middle column
reads *What the physical devices did with the surface contract's four recipes
on `<date>`.*, and the status column is `done`. (The row is not spelled out
here as a copyable line on purpose: a Markdown link to a file that does not
exist yet fails `test_relative_links_between_documents_resolve` the moment it
is committed.)

### The record's skeleton

````markdown
# Playback surface physical verification — device results from <date>

Companion to
[PLAYBACK-SURFACE-CONTRACT.md](PLAYBACK-SURFACE-CONTRACT.md) (the behaviour
contract) and
[PLAYBACK-SURFACE-CONTRACT-IMPLEMENTATION.md](PLAYBACK-SURFACE-CONTRACT-IMPLEMENTATION.md)
§7 (the recipes and the allowed-outcome table) — this records what the
connected devices did.

**Result:** N PASS · N FAIL across N required device/recipe rows.

## Scope — the tested tree and devices

Source commit `<sha>`. Apple build `<n>`, Android versionCode `<n>` — as
INSTALLED, not as claimed. The devices were <list>. Say which title was used
for each recipe, and whether the Swift and Kotlin suites had been run against
this tree (see the Apple and Android build prompts); a device result on
uncompiled code is still a device result, but the reader needs to know.

FAIL means either the device contradicted §7's allowed set, or the required
fixture, content or device was unavailable. The note distinguishes those
cases; an unavailable fixture is not evidence that the behaviour works.

## Results — every required row is explicit

| Recipe | Device | Result | Observed surface | In §7's allowed set? |
|---|---|:---:|---|---|
| (a) network drop | Apple TV | | | |
| (a) network drop | iPhone | | | |
| (a) network drop | Android TV | | | |
| (b) failed change | Apple TV | | | |
| (b) failed change | iPhone | | | |
| (b) failed change | Android TV | | | |
| (c) remux seek | Android TV | | | |
| (d) lock screen | iPhone | | | |

## SURFACE sections, per recipe per device

### (a) network drop mid-film — Apple TV

```
Surface
Fault
Source
Attached/intent
History
```

*(repeat for every row of the table above)*

## surface_disagreement rows

| # | Device | Recipe | Row | Documented exception |
|---|---|---|---|---|
| 1 | iPhone | (d) | `lock_screen_play` | Yes — the iOS lock-screen `playCommand`, contract §5 |

Every row with **no** documented exception is a defect: name the call site it
points at, or say that it could not be identified.

## How to read this — product failures and verification gaps

Which results contradict §7's table (product failures), and which are missing
content, missing devices or automation that would not run (verification gaps).
They are not the same thing and this section is where the difference is
stated.

## Evidence — retained outside the repository

Where the screenshots, device logs and any `.xcresult` bundles live, and why
they are not committed.
````

## 4. What to report

1. The filled-in record, committed with its `docs/README.md` row.
2. Every recipe/device row that was **not** in §7's allowed set, with its
   SURFACE section.
3. Every `surface_disagreement` row and the exception behind it, or the
   absence of one.
4. Every verification gap — a device you could not reach, content you could
   not find — named as a gap rather than folded into a pass.
5. Whether recipe (c) produced the M6 defect, and the two measurements the
   [remux-origin measurement prompt](PLAYBACK-SURFACE-REMUX-ORIGIN-MEASUREMENT-PROMPT.md)
   asks for.
