# DVR hardware verification — the only part of recording nobody has proved

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
- The FLEX 4K (device `10AF300E`) on the LAN, and the four nodes healthy.
- An Apple TV and an iPhone awake and unlocked, and one Android device.
  An asleep Apple TV is indistinguishable from one that does not exist.
- A mount every node can see, for `dvr.root`. A path that exists on one node
  is the single most likely way to get a confusing failure here.

## 1. Deploy the servers first

The fleet is a generation behind `main` until you do this.

```bash
cd ~/code/plurx && git fetch origin && git log --oneline -1 origin/main
cd ~/code/plurx-agent/ansible/media
ansible-playbook -i inventory.yml deploy.yml --limit plurx
```

**The recorded trap, which has bitten before:** a long `ansible-playbook` run
driven from a cloud session is killed while it waits on the remote build, and
it is killed *after* Compose has taken the container down — which once left
nynuc with both containers exited. Run it from the Mac, or fire the build on
the node itself with `ansible … -B 3600 -P 0` and poll `readyz`. Do not hold
the play open across the build from anywhere that can time out.

Confirm all four nodes report the same stamped build before going on. A fleet
that disagrees with itself makes every result below ambiguous.

## 2. Guide capability — does the tier answer days out?

This one gates whether series rules are useful at all, and it is worth doing
first because it is two curl commands and it changes how you read §4.

> On the FLEX 4K (device `10AF300E`, LAN), read
> `http://<tuner-ip>/discover.json` and take `DeviceAuth`. Fetch with curl and
> `--compressed`, twice:
> `https://api.hdhomerun.com/api/guide?DeviceAuth=<auth>` and the same with
> `&Channel=7.1&Start=<unix time 48 hours from now>`. For each, report HTTP
> status, gzip yes/no, channel count, and for channel 7.1 the earliest and
> latest `StartTime`/`EndTime` as local times. Paste one entry verbatim with
> `ImageURL` shortened, and say whether it has `SeriesID`, `ProgramID` and
> `Filter`. If the second call returns nothing past ~4 hours, say so — that is
> the free tier. **Replace `DeviceAuth` with `<auth>` in everything you paste.**

**How to read the answer.** The code is correct either way: a tier that stops
answering makes the refresh loop stop asking, and the horizon simply stays
short. But a series rule can only schedule from what the guide holds, so on a
free tier a rule records what is already on and nothing further out. If
`SeriesID` is absent, series matching falls back to the XMLTV `dd_progid` path
and an operator document becomes the only way to get series rules at all.
Report which of the three worlds this is; do not try to fix it.

## 3. Turn it on

In Settings → Developer, the DVR section lists what recording needs and whether
each part is met. **It is advisory and gates nothing** — it is there so that a
failure below has somewhere to point, not to stop you.

Set `dvr.root` to the shared mount and turn `dvr.enabled` on. Confirm the
Developer rows agree with reality before recording anything: a red row here and
a failure in §4 are almost certainly the same fact.

## 4. End to end

> With `dvr.enabled` on and `dvr.root` set to a mount every node sees:
>
> 1. From the **Apple TV** guide, Record a programme starting within 10 minutes
>    on an unprotected ATSC 1.0 channel. Confirm the Activity page shows a
>    `record` row with a **rising byte count**, and that `ps` on the owner shows
>    **no ffmpeg** for it — the tuner's bytes go straight to the file, and an
>    ffmpeg here would mean the capture path is not the one that shipped.
> 2. While it records, start **three live viewers** on other channels from three
>    clients. All three should play: one recording plus three viewers fits four
>    tuners. Then start a **fourth** viewer and report the exact message, and
>    whether it names the recording and offers to stop it.
> 3. Let it finish. Confirm the `.ts` and its `.json` sidecar under the root,
>    that it appears under Recordings on all three clients within two minutes,
>    and that it plays on the Apple TV and in Chrome.
> 4. Set a reminder **6 minutes** ahead on the **iPhone**, lock the phone, and
>    report the notification's text and whether *Record* from it actually
>    creates the recording.
> 5. With the app open on the **Apple TV**, report the overlay's appearance,
>    focus behaviour and auto-dismiss.
>
> Include screenshots and the owner's INFO log lines for the recording's start
> and finish.

### The two results that matter most

**Step 1's byte count.** It is the only direct evidence that a tuner was opened
and its bytes reached a file. Everything else in this document is downstream of
it.

**Step 2's fourth viewer.** The capacity branch counts recordings as tuner
holders, and the message is supposed to name which recording holds what and
offer to stop one — while never blaming a recording for a slot the viewer is
holding themselves. If the message is generic, or offers to stop a recording
that is not actually holding a tuner, that is a real defect and worth the
screenshot.

## 5. Back-to-back airings, if you have the patience

Not in the original acceptance, but it is the sharpest test of the design and
costs one more recording: record two programmes that run back to back on the
**same channel**. The expected result is **one tuner** held across both and
**two files**, with the overlapping padding written into both. Two tuners, or
one file, means the shared-transport design is not doing what it claims.

## 6. What to report

Per step: what you did, what happened, and the verbatim text of anything that
refused. A step that could not be run (device asleep, channel protected, no
shared mount) is reported as *not run*, never as passed. Partial credit is not
a thing here — the whole point of this pass is that reading the code has
already been done and proved nothing about the hardware.

If something fails, the useful attachment is the owner's INFO lines around the
failure plus the row from the Activity page, not a summary of them.
