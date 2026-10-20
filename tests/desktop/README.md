# Desktop qualification — run native checks and read their limits

Companion to [the desktop implementation ledger](../../docs/features/DESKTOP-TRAY-IMPLEMENTATION.md).
This directory owns disposable-host test drivers and receipts. The canonical
ledger owns promotion status. A script's existence is not a passing native receipt.

## Run the unchanged candidate before creating any fixture

Before main promotion, use the already registered `github-release.yml`
workflow: first dispatch its ordinary dry run, then dispatch it again at the
same frozen candidate with `desktop_qualification_build_run_id` naming that
successful build. The second dispatch rejects a release tag, skips every build
and publisher, and calls the same-branch reusable native qualification workflow.
After the workflow is on the default branch, standalone
`desktop-qualification.yml` dispatch is available too.

```bash
# Coordinator only: freeze the reviewed candidate branch during both runs.
gh workflow run github-release.yml --repo pjunod/cinema --ref "$CANDIDATE_BRANCH"
# Wait for the exact candidate's successful dry-run build and record its numeric run ID.
gh workflow run github-release.yml --repo pjunod/cinema --ref "$CANDIDATE_BRANCH" \
  -f desktop_qualification_build_run_id="$BUILD_RUN_ID"
```

The workflow checks the prior build's repository, workflow path, event, SHA,
completed state, conclusion and complete unexpired artifact inventory. It then
checks source/packaging/tree identity, the dry-run-only fixture signature's
presence and every downloaded native payload digest before host mutation.
These are immutable build transport checks, not production signature trust.
It holds only `contents: read` and `actions: read`, uses four native hosted VMs,
and publishes no release or fixture feed. Fixture rebuild/apply is forced on
through the registered caller. Standalone `run_local_fixture_updates` defaults
on; disabling it explicitly leaves those acceptance rows open.

The Mac driver requires a real logged-in Aqua console owned by the hosted runner,
using both `/dev/console` and an independently compiled CoreGraphics observer.
Only after this proof and fresh-install checks does it bootstrap the actual
archive with `--no-start`. It checks all four server startup/running combinations,
then invokes the real app's smoke driver and checks daemon state independently
before and after Quit, Stop and Restart. Tray login registration remains under
SMAppService; the controller cannot toggle it.

The Linux driver installs the actual native `.deb` server/desktop pair, creates
one dedicated `plurxqual` account and configures LightDM PAM autologin. Its two
sessions use Xorg's dummy output with XFCE, then KWin's virtual Wayland output
with the distribution's Plasma panel. The qualification child starts through
XDG autostart. Only three public driver files are copied into a dedicated
root-owned readable directory, with session receipts in a separate dedicated
state directory; runner-home permissions and credentials stay private. It requires `Type=x11|wayland`, `Class=user`, `Remote=no`,
`Active=yes`, `Seat=seat0`, `Service=lightdm-autologin`, process membership,
an actual display round trip, and a registered distribution SNI host in that
same session. An unavailable hosted seat produces a failed receipt.

**Fixture authorization:** the Linux authorization cases install a disposable
polkit rule restricted to `tv.plurx.install`, the dedicated user and a local
active session. They first deny and then approve the real helper call. This
proves helper/denial behavior, not a human password prompt. Root, SSH and a
private container-marker namespace are bounded refusal cases; none counts as
positive GUI evidence.

Receipts are JSON with `status: running|passed|failed`, exact candidate SHA,
observed checks, explicit limits and the failure reason. They are replaced
atomically. The UI receipt is retained separately and must agree with independent
PID, session and controller observations. A failed observation never becomes a
passing receipt because a timeout or process returned zero.

## Build an isolated local trust fixture

Production roots and candidate release archives stay unchanged. Empty and
provisioned candidate root documents are validated before the isolated snapshot
is changed. The overlay receipt retains the original public document and its
SHA-256, so replacing it with a distinct disposable qualification key stays
auditable. The production private key is never used. Positive local
update testing uses separate, committed **test-build** overlays, an ephemeral
public root and two application version identities. No shipping trust override,
local feed URL or private signing key is added to the product.

1. In a disposable hosted VM, generate an ephemeral Ed25519 key outside source,
   asset and receipt directories. Derive its raw32-byte public key; retain only
   the public key and hashes. Never upload the private key.
2. Run `create-overlay.py` twice with the exact candidate checkout, fresh output
   directories, that public key and two increasing stable versions. Its normal
   tracked commit hook must pass. Each overlay receipt records the original
   candidate, local overlay commit/tree, public patch digest and source-archive
   digest. Overlay commits are never pushed or merged.
3. `build-local-fixtures.py` automates the native build, packaging, baseline install and four-state apply batch. It compiles each overlay's **archived `build-source`**, including the real daemon
   and all five control binaries, using the pinned toolchain. Set
   `PLURX_BUILD_SHA` to that overlay commit. Build the independent stage driver
   against the **target overlay**, setting
   `PLURX_QUALIFICATION_OVERLAY_SHA` to the target overlay commit. Its compile-time
   root and identity must match the retained receipt. No global `cfg(test)` is used.
4. Package complete native baseline/target cohorts in separate fixture asset
   directories with the ordinary packaging contracts. Keep media/parser tools,
   legal material and runtime closure correct. The automated builder preserves
   candidate media/UI payloads, replaces the daemon and all five controls with
   real compiled overlay binaries, and regenerates deb checksums/metadata or
   invokes the full Mac packager with matched dSYM and closure. Only then does
   it install the deliberate fixture baseline. Mac fixtures carry both real Rust slices.
5. `sign-local-cohort.py` accepts the real native `.deb` pair or canonical macOS
   archive, both overlay receipts and the private fixture key. It verifies package
   identity and the exact public root, then creates a raw64-byte signature over
   exact manifest bytes. This is a selected-cohort local staging fixture, not the
   complete official nine-asset feed. Delete the private key after signing.
6. Run `apply-local-fixture.py` with both receipts, the actual target stage driver,
   signed asset directory and a fresh output directory. It refuses a production
   installation, verifies the baseline's commit/version/root, checks tampered
   metadata, wrong-key and real artifact-hash refusals, recovers a real untouched prepared transaction, then stages
   again and invokes unchanged installed `updates apply`. It checks the installed
   target identity and preserves initial running/startup state.

For example, the fixture staging/apply boundary is:

```bash
# On the disposable VM only, after the real fixture baseline is installed.
export PLURX_QUALIFICATION_MUTATE=disposable-hosted-vm
python3 tests/desktop/apply-local-fixture.py \
  --candidate "$GITHUB_SHA" \
  --baseline-overlay "$FIXTURE_BASE/overlay.json" \
  --target-overlay "$FIXTURE_TARGET/overlay.json" \
  --stage-driver "$FIXTURE_TARGET/stage-bin/plurx-qualification-stage" \
  --assets "$FIXTURE_TARGET/assets" --output "$RECEIPTS/local-update"
```

The stage executable mirrors public controller preparation APIs: signature and
compatibility validation, secure transaction storage, digest-checked staging,
real package preflight/inventory or strict native wrapper/closure verification,
and durable acceptance. It then exits; the installed worker owns actual apply.
There is no network preparation substitution hidden behind a production option.

## Remaining M6 evidence stays explicit

| Case | Implemented driver | Acceptance still needed |
|---|---|---|
| Native ARM/Intel Aqua menu, bootstrap, server four states, quit/stop/restart | `macos-host.py` | Successful exact-candidate hosted receipts; minimum macOS13 separately. |
| Native ARM/x64 local X11 and Wayland, real tray host, helper denial/approval | `linux-host.py`, `linux-session.py` | Successful hosted PAM/seat receipts; physical desktop and distribution floors separately. |
| Ephemeral fixture roots, provenance, exact-byte signed local staging | Overlay/sign/stage drivers | Successful native overlay build/repackage/install receipts from the automated fixture batch. |
| Real unchanged worker apply and prepared-before-shutdown recovery | `apply-local-fixture.py` | Successful signed fixture run on each platform and desired initial-state matrix. |
| Wrong production key, empty roots, tamper, Apple distribution gate | Existing release/controller regressions; Mac empty-root runtime refusal | Successful native fixture negative receipts and production signing/notary evidence. |
| Crash during install, disk-full, package contention, rollback/recovery | Controller regressions | Native fault-injection batch across durable phases. |
| Server boot/login startup and independent tray login | Effective policy readback; Linux override basename | Actual reboot/logout/relogin, SMAppService approval/denial and stable replacement. |
| Old published legacy adoption | Native bootstrap implementation | Old real published binaries without IPC, preserved config/data/tools, conflicting system-domain refusal. |
| Production GitHub discovery/download and release upgrades | Production implementation | Owner-provisioned pinned roots, trusted signing/distribution pipeline and real release assets. |

Run the local harness regressions without host mutation:

```bash
python3 -m unittest discover -s tests/desktop -p 'test_*.py'
```

The session contract follows [systemd's logind API](https://www.freedesktop.org/software/systemd/man/257/org.freedesktop.login1.html),
[LightDM's configuration](https://github.com/ubuntu/lightdm/blob/main/data/lightdm.conf),
and [KWin's virtual-output implementation](https://github.com/KDE/kwin/blob/Plasma/5.27/src/main_wayland.cpp).
These sources specify the observation and launch mechanisms; they are not evidence
that this candidate has passed on a hosted graphical seat.

GitHub requires a workflow to exist on its default branch before manual dispatch;
the registered caller avoids making main promotion depend on a new workflow's
registration. See [GitHub's manual workflow documentation](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/manually-run-a-workflow).
