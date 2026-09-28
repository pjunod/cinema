# Choose Live TV captions and available channels

**Status:** open — the single batch review approved the source and all fifteen
focused cases pass. Fast qualification, deployment and physical acceptance
remain pending (2026-09-28).

Build: 198
Issue: #594

Ordinary Release Live TV exposes caption choices from the attached player's
actual text tracks. Apple offers Automatic, Off and discovered advertised
tracks through phone controls and More. Android exposes actual tracks and Off
through phone, wide-screen and fullscreen controls. Choosing captions preserves
the existing picture and pause state. Metadata callbacks and open-menu actions
remain tied to the player attachment that produced their choices.

Stream Info separates available or selected captions from evidence that text
reached the screen. Apple rejects synthetic in-band caption options when the
session has no advertised caption stream. Android reports selected-track and
actual cue observations separately. Real broadcast text is still a physical
acceptance requirement.

A capacity refusal now preserves the server's structured watchable alternatives
through both client leases. The viewer can choose a known playable lineup
channel explicitly. Old refusal actions lose their request-generation ownership;
the selected alternative follows the normal owned start and release path.

This batch reserves Apple198 and Android versionCode135 above the integrated
#590 Apple197 and current main Android134. It depends on #590 landing first,
after which the current main is integrated and qualified. Prior signed products
retain their original source-specific evidence. The earlier feature134 APK from
this draft is sealed as unqualified history and must not be installed.

The single adversarial review approved frozen7fd with no findings. Seven Apple
cases pass on an owned iOS26.5 simulator using SDK27 compiled products; the
simulator was deleted afterward. Eight Android caption, offer-identity and
actual lease cases pass. The initial four caption failures reached host Android
TextUtils stubs before assertions; a faithful test-only framework fixture fixes
the runtime while preserving every assertion. Production198/135 inputs remain
unchanged. [The execution status](../reviews/ARCHITECTURE-REVIEW-GPT-BUILD-STATUS.md)
records the actual source scopes, receipts and retained failures. Final rendered
captions, shared-capacity alternatives, DVR preservation and the named device
matrix remain open until qualified-build receipts exist.
