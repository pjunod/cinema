# Choose Live TV captions and available channels

**Status:** open — source committed and compiled; the batch review, focused tests,
fast qualification, deployment and physical acceptance remain pending.

Build: 197
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

This batch reserves Apple197 and Android versionCode134. It depends on #590
landing first, after which the current main is integrated and qualified. Existing
signed196/133 products remain separate and retain their original evidence.

Seven Apple regression sources and Android caption, offer-identity and actual
lease regressions are prepared. Their compilation is source evidence; they have
not run before this batch's single adversarial review. Final rendered captions,
shared-capacity alternatives, DVR preservation and the named device matrix
remain open until actual qualified-build receipts exist.
