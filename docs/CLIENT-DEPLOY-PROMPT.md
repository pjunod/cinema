# Client deploy — put the merged build on the phones and the Apple TVs

The four servers are current. The Apple and Android clients are not, and no
agent in a cloud session can change that: the builds need Xcode's signing
identity and a paired `adb`, both of which live on Paul's Mac and neither of
which leaves it. This is the hand-off for someone sitting at that machine.

Nothing here changes code. If a step refuses, the refusal is the answer —
report it and stop rather than working around it.

## 0. What you need

- Paul's Mac, unlocked, with Xcode signed in to team `YHK542LK23`.
- `~/code/plurx-agent/ansible/` — the playbooks. `ansible-playbook` must be
  installed on that machine; a cloud session does not have it.
- The devices attached: USB or an `adb`-paired network endpoint for Android,
  awake and unlocked for Apple. An asleep Apple TV is indistinguishable from
  one that does not exist, which is why the roster below matters.

## 1. Confirm what you are about to ship

```bash
cd ~/code/plurx && git fetch origin && git log --oneline -1 origin/main
```

That sha is what the servers run. Check it:

```bash
for h in nynuc m6 nuc4 nuc3; do
  printf '%-7s ' "$h"
  ssh pjunod@$h 'docker logs plurxd 2>&1 | grep -m1 "plurxd starting"' \
    | sed -E 's/.*build="([^"]+)".*/\1/'
done
```

All four said `v0.3.0-568-gd4c67ff4` on 2026-09-03 at 19:17 UTC. If a node
disagrees with the others, deploy the servers first
(`ansible-playbook -i inventory.yml deploy.yml`); a client built against one
server generation and pointed at another proves nothing.

## 2. The deploy

```bash
cd ~/code/plurx-agent/ansible/media
ansible-playbook -i inventory.yml mobile-physical.yml
```

The completeness contract is in `inventory.yml`, not in the run: three named
Android devices are **required** (Pixel 11 Pro XL, Motorola razr ultra 2025,
Xiaomi 25019PNF3C) and at least one Apple device must be reached. A required
Android device that is unplugged fails the run before anything is built,
deliberately — a run that reaches nothing and exits green is the one result
that really does lie.

Two things the role will not do for you, both on purpose:

- **It will not downgrade.** `adb install -d` is absent, because a downgrade
  keeps `/data` and hands older code whatever schema the newer build wrote.
  If a device is ahead, uninstall there first.
- **It will not guess a device's identity.** `model_pattern` matches
  `ro.product.model`, not the label Device Manager shows. A device landing in
  `unknown` is skipped, not deployed.

## 3. What the clients in this build changed

Two things worth exercising by hand after the install, because neither is
covered by a suite that runs on hardware:

- **The ✕ leaves the film** (#853, `e31a6cb4`). On an iPhone or iPad: from
  transport, one tap exits. Mid-scrub, one tap exits. From info Standard and
  from info Debug, the panel's backdrop takes the first tap and the second
  exits — the ✕ sits under the panel by design, so that is a pass, not a
  fault. On an Android phone the back arrow must do the same from transport
  and mid-scrub.
- **Dolby Vision Profile 7 delivery** — the server side of that is
  `docs/M5-VERIFICATION-PROMPT.md`, and it is a browser test. The clients
  matter here only as the negative case: an Apple TV that enumerates
  dual-layer should still receive the preserved stream, and a phone that does
  not should receive HDR10. If a Profile 7 title now fails on a device that
  played it before, that is a regression in #865's dual-layer rule and it
  wants a report with the `stream_rejected` line from the server's log.

## 4. What to report

The playbook's own completeness summary — which devices received a build,
which were `not_detected`, and the Apple build number and Android
`versionCode` it shipped — plus a line for each of the two hand checks in §3.
If the run refused, the refusal and which device caused it. Nothing else.
