# M5 acceptance — the fleet has never run a client that can act

**Status:** ready to run · needs the operator's fleet, not a branch ·
**Gates:** M5c and M5h (every deletion in
[M5-CLIENT-ACTION-OWNERSHIP-HANDOFF.md](M5-CLIENT-ACTION-OWNERSHIP-HANDOFF.md)
§7) · **Written:** 2026-08-31 · **Baseline:** current `main` — see §3

Companion to [PLAYBACK-CONTROL-STATUS.md](PLAYBACK-CONTROL-STATUS.md) (where
the work is) — this is *the one thing standing between M5 being written and
M5 being true*, and the shape of the evidence that would settle it.

## 1. What is on `main` and what is running

M5 is complete in source on all three clients. Every automatic recovery owner
submits its evidence and waits, briefly, for a verdict; every one of them
falls through to its previous behaviour when none arrives.

**None of that has ever executed against a server that answered**, because no
client build carrying it has ever run on hardware.

| surface | on `main` | running |
|---|---|---|
| server (emits the actions) | yes | three nodes on `v0.2.8-106-g55abad8f`, nynuc on a later untagged build |
| web (served by the node) | yes | **whatever those nodes serve** — a server deploy ships the web client with it |
| Apple | whatever `project.yml` says on the commit you deploy | build 99 (installed 2026-08-31; no exchange) |
| Android | whatever `build.gradle.kts` says on it | 56 (installed 2026-08-31; no exchange) |

The web row is the useful one: **deploying the server deploys the web client**,
so one action gets a full-vocabulary client onto the fleet without touching a
phone.

## 2. The metric that decides it

```
plurx_playback_control_vocabulary_total{complete="true",platform="…"}
```

reads **zero on all four nodes**, and so does every counter beside it —
including `plurx_playback_control_exchanges_total{outcome="accepted"}`, which
means no client has completed a single control exchange with these nodes since
they last restarted.

Read it on any node:

```bash
ssh pjunod@192.168.4.7 'curl -s http://127.0.0.1:32400/metrics | grep plurx_playback_control'
```

Its companion is the one that says whether anything is being *withheld*:

```
plurx_playback_control_actions_suppressed_total{platform="…"}
```

counts exchanges where production was held or stopped, the server knew exactly
why, and the client was told nothing because it had not declared the action.

**Both must move before anything is deleted.** Removing a client's own recovery
while the installed build cannot receive the replacement is how a stall becomes
a dead player.

## 3. What to deploy

Two independent halves; the first is cheap and proves most of the protocol.

**Server, which also ships the web client.** Current `main`, to all four nodes,
`serial: 1` with each node's `readyz` passing before the next is touched — that
is the cluster quorum boundary and it is not optional. Deploy after the tag
exists, or each node stamps itself `v0.2.8-N-g…` instead of the release.

**Which commit, and which builds.** Read them; do not read them off this page.
This document named a specific sha and a specific pair of build numbers twice,
and both were stale within a day both times — the 2026-08-31 run shipped Apple
99 / Android 56 against a sha that three client slices had already moved past.
The commit is whatever `main` is when you start, and the builds are whatever
that commit carries:

```bash
git fetch origin main && git checkout origin/main
git rev-parse --short=8 HEAD                                    # the sha to deploy
grep CURRENT_PROJECT_VERSION clients/apple/project.yml          # the Apple build
grep versionCode clients/android/app/build.gradle.kts           # the Android build
```

A mobile build number that does not match its commit is the one failure mode
this run cannot recover from: the metric cannot tell you *which* client
answered, so an old client's exchange reads as the new one's.

**Mobile.** Those two builds, to the named roster:
Pixel 11 Pro XL · Motorola razr ultra 2025 · Xiaomi 25019PNF3C, plus the TCL
9445X when it is online, and whatever `xcrun devicectl list devices` reports.
`scripts/ship-physical` is the path that works when the Ansible controller
cannot run `media/mobile-physical.yml`; it has never been executed end to end,
so its first run is itself an acceptance.

## 4. What to look for, per ruling

Each of these was decided without the operator and each is falsifiable on
hardware. They are listed with the observation that would settle them.

### 4.0 Web first, because it needs no device

The 2026-08-31 run settled nothing partly because every device was locked or
asleep — and partly because the web reporter had never completed an exchange in
its life (see §"The first fleet run" in
[PLAYBACK-CONTROL-STATUS.md](PLAYBACK-CONTROL-STATUS.md)). That is fixed, and
web needs no hardware.

**Do this before touching a phone.** Open the web player on a deployed node,
play anything, and confirm two things: the Control panel leaves *"awaiting first
acceptance"*, and
`plurx_playback_control_vocabulary_total{complete="true",platform="web"}` on
that node goes above zero.

**If it does not,** capture the browser console verbatim and stop. Everything
else in this document is downstream of a client that can complete an exchange,
and web is the only platform that can prove it without a person holding a
device.

### 4.1 Ruling D1 — `terminal` arms the verdict, it does not tear the player down

Play a file the producer refuses — an unsupported source is the easiest, and
the server sends `terminal` for `unsupported` and `invalid_configuration`.

**Expect:** buffered media plays out; when playback can no longer continue the
viewer reads *the server's sentence*, not "Playback stopped." or a runtime
mismatch inference; *Try again* and *Force transcode* are both still offered.

**Would falsify it:** playback ending the instant the verdict arrives, or the
viewer reading a generic message while `/metrics` shows a `terminal` was sent.

**Also settled by the same file, and the reason this item matters more than it
did when it was written.** A `terminal` is emitted only for `unsupported` and
`invalid_configuration`, and the server scopes `is_permanent` to *"retrying
this source, **unchanged**"*. Both clients now act on that reading: the verdict
skips the retry that re-attaches the identical recipe and leaves the
compatibility fallback — which asks for a different pipeline — alone.

**Expect, additionally:** if the source is one AVFoundation or Media3 itself
rejects, the compatibility transcode still runs and still produces a picture,
and the client logs a ladder step naming it. **Would falsify the reading:**
playback stopping with the server's sentence on a file the transcode can play.
That would mean `terminal` is source-scoped after all and both clients are
scoped too narrowly — open decision 7 in
[PLAYBACK-CONTROL-STATUS.md](PLAYBACK-CONTROL-STATUS.md).

### 4.2 Ruling D3 — the ask costs 1.5 s, extended once to 3 s

**Expect:** on a real stall against a server answering `none`, recovery starts
no more than about a second and a half later than it used to. On web that is
on top of the eight seconds `persistentWait` already waits; on Apple, on top of
the delivery detector's sixteen.

**Would falsify it:** a visibly longer freeze before reconnecting. If so, the
number is wrong rather than the design — it is one constant per platform and
all three carry the same pair.

### 4.3 A `hold` explains itself and does not churn

Hardest to provoke deliberately; a second player on the same stream, or a busy
node, is the usual way.

**Expect:** on web and Apple, a message naming the reason in plain words and no
reopen. On **Android, deliberately nothing** — that platform's detector fires
when a stall *ends*, so the viewer is already watching and a banner would be
noise.

**Would falsify it:** repeated reconnects during a hold, or an Android banner
over playback that has resumed.

## 5. The evidence to keep

Fill this in; it is what M5c and M5h are waiting for.

| platform | `vocabulary_total{complete="true"}` | `actions_suppressed_total` | terminal seen | hold seen |
|---|---|---|---|---|
| web | | | | |
| apple | | | | |
| android | | | | |

**Acceptance:** `complete="true"` is non-zero for every platform that has run,
and `complete="false"` is zero for those platforms — a client declaring less
than the full vocabulary is an old build still installed somewhere, and it is
exactly what the deletions must not be done over.

## 6. Non-goals

- **Do not delete any client recovery path on this evidence alone.** §2's
  metric moving proves the clients can *receive*; M5c and M5h additionally
  want to have seen the replacement work.
- **Do not treat a green `android_jvm` or `apple` lane as acceptance.** CI
  covers simulator and emulator only — no archive, no codesign, no signed
  release — so signing and device breaks are invisible to it by construction.
- **Do not deploy before the tag exists.** §3.
