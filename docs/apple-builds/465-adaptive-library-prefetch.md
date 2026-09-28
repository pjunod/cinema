# Prefetch two rows from the measured native library grid

**Status:** open — source compiles; the batch review and qualification follow.

Build: 199
Issue: #465

Apple library demand now uses the actual adaptive column count and an exclusive
item count covering the visible card plus two complete rows. Android uses the
measured grid span for the same boundary. Empty and overflowing boundaries stay
safe. Existing item identity, merged order, search and focus ownership remain.

The batch reserves Apple199 and Android136 above actual main569ed6e16,
which carries198/135. Three Apple and four Android focused regression sources
compile. Their execution follows the single adversarial review of the completed
batch. The 6000-title tail, page-arrival focus and physical frame matrix remain
open; source compilation does not establish those device results.

Android additions include debug-only existing-dispatcher timing, bounded Live
terminal diagnostics, a real-player screen-on instrumentation case and signed
release profile capture/paired measurement tooling. Capture prerequisites appear
as advisory Developer information. No dispatcher split or measured performance
gain is claimed. The separate test harness signer never replaces the app signer.

[Execution status](../reviews/ARCHITECTURE-REVIEW-GPT-BUILD-STATUS.md) and the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) retain
failed observations and the physical evidence still owed.
