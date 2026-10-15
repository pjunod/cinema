# HTTPS review — adversarial findings and disposition

**Status:** design approved with incorporated clarifications · **Reviewed:**
2026-10-15 · **Baseline:** `9eb7f6738`

Independent adversarial review of [the contract](HTTPS-IMPLEMENTATION.md) and
[build ledger](HTTPS-BUILD.md) against actual configuration, authorization,
startup and transport code. The reviewer made no source edits. This is design
approval, not evidence that implementation or device acceptance passed.

## 1. Verdict

Approve the architecture after the five clarifications below. All five are
incorporated into the contract before Sol 6.1 is released to implement it.
No architectural redesign is required.

## 2. Findings and resolutions

| Finding | Severity | Resolution and required proof |
|---|---|---|
| Revision counters could reset after restart and accept an old PUT. | P2 | Persist the revision in the versioned desired-file envelope; require node/revision guards and test stale PUT across restart. UI never blindly retries against another node. |
| Removing a generated identity alone recreates old SAN coverage. | P2 | Document exact file format and stop → edit stored SANs/env → increment revision → back up/remove identity → restart → re-trust. No rotation API needed. |
| A combined PEM bundle creates a risk of exporting the private key during trust setup. | P2 | Admin-only public-chain export derives from loaded DER; certificate-only parse, fingerprint, authorization and no-store tests. |
| Opening a FIFO can hang before regular-file validation. | P2 | Nonblocking/safe regular-file opening, bounded reads, pipe and pipe-symlink regressions. Do not copy existing sharing file reads as sufficient proof. |
| Browser hostname may identify a load balancer, not the edited node. | P2 | Label the constructed URL as an unverified candidate; display node identity; no automatic navigation or cross-node retry. |

The review confirmed that `AdminUser` authorizes without forwarding, the
startup observation owner's serving transition needs HTTPS drain ownership,
and TLS handshakes must not serialize the existing accept loop.

Sol's feasibility review also clarified first-enable Save without certificate
provisioning, `saved` versus effective `desired` under environment overrides,
and the explicit SAN editor/Developer lazy loader. These are in §3.

## 3. Limits and optional advice

The 365-day immutable generated leaf intentionally requires explicit
replacement and renewed trust on expiry. Show expiry in status and explain
the workflow; advance warning is useful but does not change identity policy.
Source implementation, real sockets, final review, gates and physical trust
acceptance remain owned by the build ledger. No live configuration changed.

## 4. Independent workflow amendment review — 2026-10-15

A separate workflow review approved consolidating H1–H4 into checkpoints of
one cohesive implementation task PR #1118 before any H2–H4 task branch,
commit or intermediate landing exists. The same builder owns shared files;
this does not invoke the disjoint-task exception or remove a validation
surface. Parent canceled only superseded run #5203 before its head changed;
completed checks remain history and cannot count as a green task gate.

Conditions: amend the canonical plan before restoring drafts, preserve all
prior findings/failures, verify combined current source locally and through
the normal hook, then require a fresh exact-final-head Effort gate. Final
freeze/fresh-main integration, exactly one fresh adversarial built-code review
with Sol-owned fixes, full qualification receipt and Main promotion gate remain
mandatory. Parent manages and reviews; Sol implements. This verdict approves
task sizing only, not built-code correctness or physical/device acceptance.
