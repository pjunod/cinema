# DVR hardware verification — the only part of recording nobody has proved

> **Status:** open · **For:** a session at the physical devices, on Paul's LAN ·
> **Updated:** 2026-09-13

Recording and reminders merged to `main` in
[#294](http://192.168.4.7:3000/noirr/plurx/pulls/294). The server, both Store
backends, three clients, the fast lane's Rust, web, Apple and Android compiles,
and every repository contract are green. **No tuner has ever been opened by
this code.** No `.ts` exists, no sidecar has been written, no recording has
been played back, and no phone has fired a reminder.

That is not a gap in the tests. It is the part a cloud session cannot reach: it
needs the FLEX 4K on Paul's LAN, an Apple TV, an iPhone and an Android device.
This document is the hand-off for someone sitting at that machine.

Nothing here changes code. **If a step refuses, the refusal is the answer** —
report it verbatim and stop rather than working around it. A recording that
does not start is a finding; a recording that starts because you disabled
something is not a result.

## 0. What you need

- Paul's Mac, unlocked, with `~/code/plurx-agent/ansible/` and
  `ansible-playbook` installed. A cloud session has neither.
- The FLEX 4K (device `10AF300E`, `192.168.4.20`) on the LAN, and the four
  nodes healthy.
- An Apple TV and an iPhone awake and unlocked, and one Android device.
  An asleep Apple TV is indistinguishable from one that does not exist.
- **A writable bind for `dvr.root`, which the shipped Compose file does not
  create.** Every media mount plurx ships is read-only on purpose, so until a
  `:rw` bind exists in `docker-compose.override.yml` the DVR has nowhere to
  write and turning `dvr.enabled` on does nothing by itself. See
  [`deploy/README.md`](../../deploy/README.md) — "Recording needs a writable DVR
  root". It must also be the same underlying filesystem on every node, because
  the owner writes the capture and any node may serve it back; a path that
  exists on one node only is the single most likely way to get a confusing
  failure here, and §3 explains why the Developer card cannot catch that one
  for you.

## 1. Deploy the servers, then prove the DVR is actually in the build

The fleet is a generation behind `main` until you do this.

```bash
cd ~/code/plurx && git fetch origin && git log --oneline -1 origin/main
cd ~/code/plurx-agent/ansible/media
ansible-playbook -i inventory.yml deploy.yml
```

That is the same invocation
[`CLIENT-DEPLOY-PROMPT.md`](CLIENT-DEPLOY-PROMPT.md) gives. Do not add a
`--limit` unless you know the group exists: a `--limit` pattern that matches no
host does not fail loudly — Ansible warns, runs against zero hosts and exits 0,
and you will believe you deployed.

**The recorded trap, which has bitten before:** a long `ansible-playbook` run
driven from a cloud session is killed while it waits on the remote build, and
it is killed *after* Compose has taken the container down — which once left
nynuc with both containers exited. Run it from the Mac and let it finish.

Then check the property that actually matters. Four nodes agreeing on a build
with no DVR in it is a perfectly uniform fleet that cannot record, and you
would not find out until §3 sends you looking for a settings section that does
not exist:

```bash
for n in nynuc m6 nuc4 nuc3; do
  echo -n "$n: "
  curl -fsS -H "Authorization: Bearer $TOKEN" "http://$n:32400/api/v1/dvr/status" \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d["owner_node_id"], d["enabled"])' \
    || echo "NO DVR ROUTE — this node is not carrying #294"
done
```

Keep the `owner_node_id` it prints. §4 step 1 needs it, and recordings run
only there.

## 2. Guide capability — does the tier answer days out?

Worth doing first: it is three curl commands and it changes how you read §4.

> On the FLEX 4K (device `10AF300E`, `192.168.4.20`), read
> `http://192.168.4.20/discover.json` and take `DeviceAuth`. Fetch with curl and
> `--compressed`, twice:
> `https://api.hdhomerun.com/api/guide?DeviceAuth=<auth>` and the same with
> `&Channel=7.1&Start=<unix time 48 hours from now>`. For each report HTTP
> status, gzip yes/no, channel count, and for channel 7.1 the earliest and
> latest `StartTime`/`EndTime` as local times; paste one entry verbatim
> with `ImageURL` shortened, and say whether it has `SeriesID`,
> `ProgramID` and `Filter`. If the second call returns nothing past ~4 h,
> say so — that is the free tier, and the plan's M0 still stands. Replace
> `DeviceAuth` with `<auth>` in the report.

That block is §8's first prompt, with `<tuner-ip>` filled in as `192.168.4.20`
and nothing else changed.

**How to read the answer.** The code is correct in every case; what changes is
how useful a series rule is. Report which of these three it is:

1. **Deep guide with `SeriesID`** — series rules schedule out to the full
   14-day horizon and match on the tuner's own series identity.
2. **Deep guide without `SeriesID`** — series matching falls back to the XMLTV
   `dd_progid` path, so an operator document becomes the only way to get series
   rules working.
3. **Free tier, nothing past ~4 hours** — the refresh loop stops asking and the
   horizon simply stays short. A series rule then records what is already on
   and nothing further out. This is not a defect and needs no fix; the plan's
   M0 stands either way.

## 3. Turn it on

Three settings, not one — and the writable bind from §0 has to exist first, or
none of the rest can work:

- `dvr.root` — the **container** path of that bind (`/dvr` if you followed
  `deploy/README.md`), not the host path.
- `dvr.enabled` — on.
- **`live_tv.max_sessions` — set it to `4`.** It defaults to **2**, and §4
  step 2's arithmetic assumes the FLEX 4K's four tuners. Leave it at 2 and the
  recording holds one slot and the *first* viewer is refused, not the fourth,
  which looks exactly like a capacity bug and is not one. Leave
  `dvr.tuner_reserve` at its default.

In Settings → Developer, the DVR section lists what recording needs and whether
each part is met. **It is advisory and gates nothing** — it is there so a
failure below has somewhere to point.

**Two of its rows are permanently `Unobservable` by construction**, and one of
them is the prerequisite §0 calls the likeliest source of confusion: a node can
see its own filesystem and no peer's, so "Every node can read the DVR root"
cannot be answered from inside the product and neither can root writability on
a non-owner node. Verify the shared mount yourself with `ls` on each of the
four nodes. Do not read `Unobservable` as met.

## 4. End to end

> With `dvr.enabled` on and `dvr.root` set to a mount every node sees:
> (1) from the Apple TV guide, Record a programme starting within 10
> minutes on an unprotected ATSC 1.0 channel; confirm the Activity page
> shows a "record" row with a rising byte count and that `ps` on the owner
> shows no ffmpeg for it; (2) while it records, start three live viewers
> on other channels from three clients — all three should play (one
> recording + three viewers fits four tuners); then start a fourth viewer
> and report the exact message and whether it offers to stop the
> recording; (3) let
> it finish; confirm the file and `.json` sidecar under the root, that it
> appears on Recordings on all three clients within two minutes, and that
> it plays on the Apple TV and in Chrome; (4) set a reminder 6 minutes
> ahead on the iPhone, lock the phone, and report the notification's text
> and whether *Record* from it creates the recording; (5) with the app
> open on the Apple TV, report the overlay's appearance, focus behaviour
> and auto-dismiss. Include screenshots and the owner's INFO log lines
> for the recording's start and finish.

That block is §8 of
[the implementation plan](../features/LIVE-TV-DVR-IMPLEMENTATION.md#8-hardware-prompts-gpt),
unchanged. Everything below is commentary on it, not part of it.

### Running step 1

`ps` means `ps` **on the owner node** — the `owner_node_id` §1 printed.
Recordings run only there, and looking on the wrong node shows you nothing and
proves nothing.

The rising byte count is the single most important result in this document. It
is the only direct evidence that a tuner was opened and its bytes reached a
file; everything else here is downstream of it. The absence of ffmpeg matters
for the same reason — the capture writes the tuner's transport stream straight
to disk, and an ffmpeg in that path would mean the code running is not the code
that shipped.

### Running step 2, and what the fourth viewer will actually say

**Take the fourth viewer from the Apple TV or the Android device, not Chrome.**

The server's refusal string is generic by design — "all N plurx Live TV session
slots are in use" — and the recordings holding the tuners ride along in a
separate `holders` field on the error body. Apple composes a sentence from that
field and Android renders it; **the web client does not read `holders` at all**
and shows a fixed "All Live TV slots are busy". That is a known gap, already
recorded here, so a generic message *in Chrome* is not a finding and should not
be filed as one.

What is worth a screenshot: an Apple or Android refusal that fails to name the
recording, or that offers to stop a recording which is not actually holding a
tuner, or that blames a recording for a slot the viewer is holding themselves.

Two occupancy conditions decide admission, so read the arithmetic carefully:
three recordings still *allow* a viewer, and one recording plus three viewers
fits four tuners.

### An existing harness does the device side better

For the tuner-accounting half of step 2, `make live-tv-hardware-check
DEVICE=192.168.4.20 TUNERS=4` asserts capacity from the **device's own**
`/status.json` — that the run holds exactly the tuners it opened and that the
device reports them free again after release. That is stronger evidence than a
hand-run fourth viewer, which proves only what one client was told. Run both:
the harness for the device's view, step 2 for the viewer's.

## 5. Back-to-back airings, if you have the patience

Not in the original acceptance, but the sharpest test of the design, and it
costs one more recording: record two programmes that run back to back on the
**same channel**. Expect **one tuner** held across both and **two files**, with
the overlapping padding written into both.

This works because the tail pad of the earlier capture overlaps the head pad of
the later one, so **leave `dvr.pad_start_s` and `dvr.pad_end_s` at their
defaults** (60 s and 120 s). With padding set to zero the two windows do not
touch and two transports is correct behaviour, not a bug.

Two tuners *with the default padding*, or a single merged file, means the
shared-transport design is not doing what it claims.

## 6. What to report

Per step: what you did, what happened, and the verbatim text of anything that
refused. A step that could not be run (device asleep, channel protected, no
shared mount) is reported as *not run*, never as passed. Partial credit is not
a thing here — the whole point of this pass is that reading the code has
already been done and proved nothing about the hardware.

If something fails, the useful attachment is the owner's INFO lines around the
failure plus the row from the Activity page, not a summary of them.
