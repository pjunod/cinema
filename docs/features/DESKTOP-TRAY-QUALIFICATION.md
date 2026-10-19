# Desktop tray — qualification and promotion ledger

**Status:** open · **Updated:** 2026-10-19 · source assembled; implementation
review and integrated native acceptance pending. Nothing deployed or published.

[The implementation plan](DESKTOP-TRAY-IMPLEMENTATION.md) is the canonical P31
contract. [The operating guide](../server/DESKTOP-CONTROL.md) explains the CLI,
startup choices and release trust. This ledger records what the implementation
has actually proved and what still blocks promotion.

## Integration

The M1–M5 source components form one cohort task PR into `effort/desktop-tray`.
Their owned commits and focused regressions remain intact. Partial component
PRs are superseded by the cohort; no partial implementation receives acceptance
by implication. Independent Astra review begins only after the complete PR
exists, as requested. Review fixes, the native transaction/package batch and
the manually dispatched Effort development gate precede its merge. Draft
[PR #1213](http://forge.lan:3000/noirr/plurx/pulls/1213) promotes the qualified
current-main effort only after the Main promotion gate and receipt.

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

## Required integrated evidence

The executable protocol is in [the disposable-host runbook](../../tests/desktop/README.md).
A read-only release dry run builds the exact candidate. A second dispatch of
that registered workflow consumes only its completed successful build ID and
invokes the same-branch native qualification workflow. It verifies source,
architecture, artifact inventory and downloaded bytes before mutating a
disposable runner.

The batch must exercise real native Aqua and Linux PAM/logind X11/Wayland
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
