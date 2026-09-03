# Client deploy — put the merged build on the phones and the Apple TVs

The four servers are current. The Apple and Android clients are not, and no
agent in a cloud session can change that: the builds need Xcode's signing
identity and a paired `adb`, both of which live on Paul's Mac and neither of
which leaves it. This is the hand-off for someone sitting at that machine.

Nothing here changes code. If a step refuses, the refusal is the answer —
report it and stop rather than working around it.

## 0. What you need

- Paul's Mac, unlocked, with Xcode signed in to team `YHK542LK23`
  (`roles/mobile_release/defaults/main.yml`).
- `~/code/plurx-agent/ansible/` — the playbooks. `ansible-playbook` must be
  installed on that machine; a cloud session does not have it, which is half
  of why this document exists.
- The devices attached: USB or an `adb`-paired network endpoint for Android,
  awake and unlocked for Apple. An asleep Apple TV is indistinguishable from
  one that does not exist — see §2 for what that costs you, which is less
  than the inventory comments claim.

## 1. Know which server generation you are shipping against

```bash
cd ~/code/plurx && git fetch origin && git log --oneline -1 origin/main
```

That is what `main` has, **not** what the fleet runs; the two are routinely a
generation apart, and neither is wrong. What matters is that the fleet agrees
with itself and carries the client-side change you are about to exercise. Read
the fleet with the loop in `docs/M5-VERIFICATION-PROMPT.md` §1 — note the
escape-stripping `sed` there, without which the command prints raw log lines.

If the nodes disagree with each other, deploy the servers first:

```bash
cd ~/code/plurx-agent/ansible/media
ansible-playbook -i inventory.yml deploy.yml
```

## 2. The deploy

```bash
cd ~/code/plurx-agent/ansible/media
ansible-playbook -i inventory.yml mobile-physical.yml
```

**The gate that actually stops a run before anything is built** is the
release-readiness assert in `roles/mobile_release/tasks/main.yml` — every
client repo must have had its version and build number bumped since its last
client change. That is the refusal you are most likely to meet, and no device
is named in it.

**The device roster is not that gate**, whatever the comments in
`inventory.yml` and the role's `defaults/main.yml` say. `completeness.yml`
runs *last*, after the build and after every reachable device has been
installed to — deliberately, so absent hardware cannot veto present hardware.
So a run with a required Android phone unplugged still builds everything, still
installs to every device that is there, and only then exits red. A red run has
usually changed the fleet. Read the summary, not the exit status.

What the roster does and does not enforce:

- `mobile_release_required_android_devices` — Pixel 11 Pro XL, Motorola razr
  ultra 2025, Xiaomi 25019PNF3C — is checked, at the end.
- `mobile_release_minimum_apple_devices` and
  `mobile_release_minimum_android_devices` are **dead variables**. They appear
  in the inventory and the defaults and in nothing that runs. The only floor
  enforced is a hardcoded "at least one device reached" over the *combined*
  Apple and Android count, so a run that reaches three phones and zero Apple
  devices passes green. If Apple coverage matters for what you are testing,
  confirm it from the summary yourself.

Two refusals the role makes on purpose:

- **It will not downgrade by default.** `adb install -d` is absent, because a
  downgrade keeps `/data` and hands older code whatever schema the newer build
  wrote. `mobile_release_allow_downgrade` overrides it; do not set it to get
  past a red run.
- **It will not guess a device's identity.** `model_pattern` is an unanchored,
  case-insensitive search against `ro.product.model` with spaces replaced by
  underscores — not against the label Device Manager shows, which prepends the
  manufacturer. A device that matches nothing lands in `unknown` and is
  skipped rather than deployed.

## 3. What to exercise by hand

Two things, because neither is covered by a suite that runs on hardware:

- **The ✕ leaves the film** (#853, `e31a6cb4`). On an iPhone or iPad: from
  transport, one tap exits; mid-scrub, one tap exits; from info Standard and
  from info Debug, the panel's backdrop takes the first tap and the second
  exits — the ✕ sits under the panel by design, so that is a pass, not a
  fault. On an Android phone the back arrow must do the same from transport
  and mid-scrub.
- **Dolby Vision on a native client** (#865). The rule is not two-way. A
  client that *enumerates* the source's profile receives the stream copied and
  preserved. A client that does not, on a title with an HDR10-compatible base,
  receives a **converted Profile 8.1 stream** — conversion is tried before
  stripping, because stripping is the strictly worse answer. HDR10 is what
  arrives only when conversion is unavailable, and a re-encode is the last
  resort. So the regression to watch for is a Profile 7 title that used to
  play on a device and now does not, in any of those three shapes.

  Native clients do not emit the web player's `stream_rejected` event; they
  post their own telemetry (`PlayerController.swift` on Apple,
  `PlaybackTelemetry.kt` on Android). For a device failure, capture that plus
  the server's decision line for the session — `docs/M5-VERIFICATION-PROMPT.md`
  §4 lists which line says what. The browser side of Profile 7 is that
  document, not this one.

## 4. What to report

The playbook's completeness summary — which devices received a build, which
were `not_detected`, and the Apple build number and Android `versionCode` it
shipped — plus a line for each of the two hand checks in §3. If the run
refused, the refusal, and whether it was the release-readiness assert (before
the build, fleet untouched) or the roster (after it, fleet already changed).
Nothing else.
