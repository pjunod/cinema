# Playback reader witnesses — implementation and qualification status

**Status:** open · **Owner:** Codex · **Updated:** 2026-11-13

Companion to [the implementation plan](PLAYBACK-READER-WITNESS-PLAN.md).
Work is integrated on `effort/source-reader-witness` in one batched task,
authorized by the operator on 2026-11-13. Qualification precedes promotion.

| Phase | State | Evidence or next action |
|---|---|---|
| B0 launch attribution | Implemented; unqualified | Static purpose at the central launcher, bounded snapshots, first-purpose sidecars, node-local Developer observation and metrics |
| B1 access verification and descriptor builder | Local focused checks pass; gates pending | Exact no-input profiles, isolated probe environment and identity-bound descriptor construction; media inputs remain unqualified |
| B2 source reads through the broker | Dependency open | Reconcile playback-start-phase2 and lent-input ownership before overlapping source-input work |
| B3 residual raw lifetime lock | Conditional; unbuilt | Required where B2 leaves raw readers; inherits the exact owner lock and qualifies packaged children |
| B4 native and fleet qualification | Open | Native Linux/macOS crash evidence and measured playback performance remain required |
| Final review and gates | Review addressed; local focused checks pass | One final adversarial review and scoped disposition complete; effort and promotion gates pending |

## Safety decisions

1. **Do not merge the current census PR #1561.** Its final review found that
   a pathname stat is not an opened-inode binding and counting that inode
   cannot prove that an unaudited child cannot read another object or reopen
   the path. Production object-version identities also differ from its
   fixture. Green CI does not resolve either issue. The PR records ownership
   and the required physical-proof correction.
2. **B0 does not depend on that unsafe settlement path.** Existing main
   already names witness families. New sidecars report the first invalidating
   purpose and never authorize release. Existing settlement, live-ledger and
   namespace replay rules are preserved.
3. **Namespace recovery has a narrower implemented contract.** Main's #1566
   records namespace/init evidence and recovers only through its old-init,
   new-init, different-namespace and free-anchor proof. This does not claim
   general native recovery; [the recovery reference](SOURCE-CUSTODY-RECOVERY.md)
   defines its deployment ordering assumptions.
4. **B9 remains scoped.** Standalone SQLite lacks the authoritative witness
   backend; its existing recovery behavior is unchanged. A read-only card
   must report that limit.
5. **Media inputs need more than a URL or inode registration.** FFmpeg can
   select referencing formats and follow their embedded paths. B1 verifies
   only no-input capability probes and exact metadata discovery. Brokered,
   tuner and media-bearing cache inputs remain unqualified until an input
   profile bound to a packaged executable establishes confinement. Playback
   continues; diagnostics never turn missing qualification into an enable gate.
   [FFmpeg formats](https://ffmpeg.org/ffmpeg-formats.html#concat) and
   [protocol options](https://ffmpeg.org/ffmpeg-protocols.html#Protocol-Options)
   describe the nested-input boundary. Q4 therefore remains a declaration,
   without granting verified recovery authority.
6. **No feature gate.** Developer reports support, declarations and remaining
   evidence. It does not decide whether an operator may enable a feature.
7. **Probe approval is a positive profile.** A trusted tool directory alone
   cannot establish that arbitrary tool arguments avoid media. Only exact
   FFmpeg inventory/version/filter-help, ffprobe version, the native no-op
   probe and metadata-find shapes qualify. Their absolute executable path
   preserves resolution when the final launcher clears inherited and caller
   environment overrides and supplies only the fixed C locale. Installed
   executables remain trusted operator artifacts; this is not a qualified
   media-demuxer confinement profile.
8. **Diagnostics do not occupy playback runtime workers.** Filesystem scans
   run on a blocking worker with one process-wide permit. A concurrent read
   returns unavailable immediately; cancellation keeps the permit held until
   the active scan ends. Errors never become a zero-poison count.

## Execution log

| Date | Owner | Result |
|---|---|---|
| 2026-11-13 | Codex | Claimed B0/B1 and conditional B3; lower-cost agents implemented bounded attribution, durable poisoning diagnostics and read-only Settings reporting. Tests await the final batch review. |

Final adversarial review identified arbitrary-tool grammar, loader-environment
and synchronous diagnostic-scan gaps. The batch now has positive no-input
profiles, final-launch environment isolation and one bounded blocking scan.
All-target compiler check passed before review. Focused native macOS evidence
now passes: 55 core process cases, 27 daemon witness/descriptor/broker cases,
82 Settings cases cumulatively and both ownership inventory contracts. The
five failing Settings cases and one inventory case were individually retried
after correction; passing cases were retained. Compiler lint and CI gates
remain pending. The Linux loader-isolation case still needs native execution.
No B2, B3 or full native/fleet acceptance is claimed.

Verifier-root setup remains advisory: a startup qualification error logs a
warning and leaves declarations unverified; it does not prevent playback or
daemon startup. Formal correspondence pins retain the same authority anchors
while binding startup setup, shared descriptor construction and read-only
diagnostics to the reviewed source. Settlement transitions are unchanged.
