# Desktop tray — qualification and promotion ledger

**Status:** open · **Updated:** 2026-10-19 · source built; Astra PR review completed,
R1–R8 fixes integrated; unit/merge evidence belongs to the linked PRs.
Native distribution qualification remains open. Nothing deployed or published.

[The implementation plan](DESKTOP-TRAY-IMPLEMENTATION.md) is the canonical P31
contract. [The operating guide](../server/DESKTOP-CONTROL.md) explains the CLI,
startup choices and release trust. This ledger records what the implementation
has actually proved and what still blocks promotion.

## Integration

The M1–M5 source components form one cohort task PR into `effort/desktop-tray`.
Their owned commits and focused regressions remain intact. Partial component
PRs are superseded by the cohort; no partial implementation receives acceptance
by implication. Astra reviewed [PR #1220](http://forge.lan:3000/noirr/plurx/pulls/1220) after it
was created, as requested. The owner then clarified: Astra only reviews; root
addresses findings, obtains passing unit tests and merges. The mandatory Effort
development and current-main promotion gates remain in force. Native distribution
qualification stays explicit below.
[PR #1213](http://forge.lan:3000/noirr/plurx/pulls/1213) promotes the qualified
current-main effort only after the Main promotion gate and receipt.

## PR review and remediation

Astra (`gpt-6-astra`, Hermes task `t_55bcc2df`) reviewed only PR #1220 at
`f1c8ac74e31e5cbe364c284b7c3bb2ead7defa19` against
`59332e0b8e0d63f21f32e0632bfbf9cbc63a719e`. The
[completed report](http://forge.lan:3000/noirr/plurx/pulls/1220#issuecomment-12942)
returned three P1 and five P2 findings. Root accepted all eight and the rollback
wording observation; Astra did not own, fix, test or merge the PR.

| Finding | Root's correction |
|---|---|
| R1 · Linux launcher | Select the real package binary; retain no-follow validation and test the shipped convenience symlink layout. |
| R2 · Debian configuration | Recognize only the exact package hook's transient dpkg state; keep final installed validation strict and cover server-first configuration plus desktop add/remove. |
| R3 · Upgrade identity | Stamp a full source commit separately from human build labels; readiness still requires the authenticated candidate commit. Cover release, dry-run, fixture, dirty and malformed identities. |
| R4 · Changed units | Reload systemd after exact-cohort success, then revalidate policy. Hash drop-in names/order/bytes to preserve outside-change detection. |
| R5 · Login location | Preserve raw XDG configuration on unprivileged controller routes; retain invalid-value refusal and exclude the preference from helper routes. |
| R6 · Tray host | Require positive registered-host proof; independently observe loss/return and show fallback without server mutation. Keep monitoring alive through queue pressure. |
| R7 · Fresh Mac login | Securely create missing LaunchAgents only on explicit On; Off/readback do not create it. |
| R8 · Legacy adoption | Distinguish fresh installation from identified stopped legacy adoption and preserve the latter's stopped/login choices. |

Both native confirmations now describe Recovery required and compatible recovery
options, without promising automatic rollback after failed health checks. Focused
remediation results and exact-source merge evidence are recorded on the PR as
each check completes; unchanged component evidence below remains valid. This report's original
SHA-256 is `5a76a7d2a50e4e18ab28d951c9bfc5e37bda3c4dab5a39f85427c3f321b40436`.

## Retained focused evidence

All component source commits used normal hooks with pinned Rust 1.97.1:
formatting, workspace all-target Clippy, catalog and embedded JavaScript syntax.
The source-only Linux compiler receives committed archives without repository
credentials or `.git`. Evidence on unchanged source is retained, with exact
integrated source/base checks required before promotion.

| Component | Observed result | Limit |
|---|---|---|
| Shared controller | 39 focused tests pass on native macOS and Linux ARM; strict scoped Linux Clippy; root/service-group launch-lock regression passes. | Not an installed production upgrade. |
| Daemon lifecycle | Native Mac desktop, serving-fence, DVR and both startup regressions pass; exact committed Linux ARM production check passes. | Suite counts overlap. Linux daemon test-target metadata exceeded the disposable 8 GB compiler VM; no passing Linux daemon test claim. |
| macOS companion | 71 Swift/AppKit tests pass; optimized arm64 and x86_64 slices compile and pass ad-hoc signature checks. Actual AppKit smoke driver covers Unknown/Quit, malformed arguments, menu deadline and fresh-status Stop/Restart. | Intel compilation is not Intel runtime acceptance. Private fixture smoke is not installed-service acceptance. |
| Linux companion | 34 focused tests pass on macOS and in three native Linux ARM repetitions; strict native all-target Clippy and binary build pass with the corrected GTK cohort. | Container smoke refuses with exit 3, no GUI and no controller calls. Complete-session acceptance remains pending. |
| Release/qualification | Packaging, manifest, dispatch, refusal and fixture-harness regressions pass on native Mac/Linux. Stage driver compiles against the canonical controller. | Scripts and mocked receipts do not establish four-platform runtime acceptance. |
| Native CI | Separate desktop macOS scope, both architecture builds and native test lane; workflow contracts pass. | Mobile release counters and receipts do not qualify this companion. |
| ARM build prerequisite | Generic native FP16 and the same binary under Cortex-A53 emulation pass; four intrinsic annotations preserve the generic runtime fallback. | No raised global CPU floor. |

## Distribution qualification — still open

The executable protocol is in [the disposable-host runbook](../../tests/desktop/README.md).
A read-only release dry run builds the exact candidate. A second dispatch of
that registered workflow consumes only its completed successful build ID and
invokes the same-branch native qualification workflow. It verifies source,
architecture, artifact inventory and downloaded bytes before mutating a
disposable runner.

Before distribution acceptance, the batch must exercise real native Aqua and
Linux PAM/logind X11/Wayland
sessions, real tray menus and independent supervisor observations, separate
startup/running combinations, privilege denial, selected package cohorts,
trusted fixture upgrades and prepared recovery. Record run URLs, exact source
and tree, receipts, failures and corrections here after execution. Do not infer
success from a workflow definition or from a unit test of its receipt parser.

The plan's remaining login/reboot, no-tray recovery, minimum-OS, distro/package
family, published legacy-artifact adoption, human approval and hardware rows
remain explicit acceptance work. They are not closed by fixture versions or
cross-compilation.

## Production release prerequisites

The checked-in Ed25519 public-root list is intentionally empty. Production
update trust remains fail-closed until an owner-approved protected signer and
matching public roots are provisioned. Apple Developer ID/notary credentials
are also not supplied by this work. Ad-hoc candidates are labelled accordingly;
the production publisher refuses missing trust or signing/notary evidence.
Ephemeral qualification keys are isolated test-build overlays, never production
trust or assets. No production release, real host startup change or fleet
upgrade is authorized by this implementation task.
