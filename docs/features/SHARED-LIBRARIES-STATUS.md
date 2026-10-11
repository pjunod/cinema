# Shared libraries — build status and remaining acceptance

**Status:** implementation integrated; final promotion corrections in progress.
**Updated:** 2026-10-07 · **Owner:** Root coordinating GPT-6.1 Sol builders.

[PR #828](http://forge.lan:3000/noirr/plurx/pulls/828) is the live status page
and qualification receipt ledger. It records the exact candidate, individual
failed-only retries, retained passes, review findings, and merge state without
changing the source being qualified. The
[implementation contract](SHARED-LIBRARIES-IMPLEMENTATION.md) remains the
authority for architecture and acceptance boundaries.

## Implemented and integrated

The effort combines PRs #794, #804, #809 and #827, current-main integration
#829, and qualification corrections #830, #831, #833 and #834. Main promotion
is authorized; the inherited hold was lifted on 2026-10-06.

| Surface | Implemented behavior |
|---|---|
| Catalogue and identity | Authenticated pairing, grants, catalogue/history/assets and original-account refusal recovery |
| Playback | Source-owned direct/HLS preparation, subtitles, HDR decisions and typed unsupported Dolby Vision refusal |
| Handoff | Receiver-owned prepared successors across web, Apple and Android; frame evidence before history; bounded Apple preparation |
| Cleanup | Exact retained g0/g1 ownership, receiver orphan recovery, approved endpoint/pin refresh and bounded physical transport retirement |
| Cluster custody | Actual accepted-driver identity and closure, independent receiver admission, sealed ledgers and authenticated exact member receipts |
| Main compatibility | Local rolling/continuous/retained playback, quality controls, native link receipts and Jellyfin watch authority preserved |
| Schema | SQLite104/105; replicated sharing80 and custody81; explicit Source activation82. Frozen Source layout71 is separate from migration order |
| Hosting | Explicit Docker bridge profile and operator/API/security guidance; readiness remains advisory in Settings → Developer |

## Qualification progress

The independent promotion review found an unbounded initial Apple Shared
successor seek. Its correction uses the existing generation-fenced seek owner
within the original preparation budget; the reviewer verified it.

Rust1.97.1 formatting and workspace/all-target Clippy, iOS/tvOS app and test
compilation, Android app/unit-source compilation, web/static policy checks,
and the non-Rust fast-lane checks have recorded evidence in PR #828. Earlier
receipts apply to their named revisions and actual build inputs.

The first targeted Rust execution on effort `fa19ef4f44` completed92 checks:
68 passed and24 failed. All six cluster migration cases and both new trigger
preservation/refusal cases passed. One real pinned Source/B network playback
fixture passed. The failing checks are being corrected and retried individually;
they are not waived. The latest exact counts and candidate are in PR #828.

| Correction | Concrete cause and intended result |
|---|---|
| Current-schema rebuild | Preserve the two dependent Jellyfin watch triggers atomically and use the current81→82 activation ordinals |
| SQL inspection | Bound composed principal argument parsing to its actual format call; distinguish exact helper names; keep test adapters below production source |
| Store-only settlement fixtures | Assert unsealed refusal, then use the real guarded seal on their known empty custody ledger before settlement |
| Actor admission fixtures | Use the real heartbeat publication; raw legacy heartbeat writes correctly erase incomplete capability proofs |
| Forwarding fixtures | Advertise the actual listening address, compose one strict fallback and use wire-valid generated credentials |
| H2 fixture | Inspect legal empty terminal DATA frames through bounded EOF; retain actual driver-closure evidence and successor protection |
| Early Source refusal | Preserve the retained typed failure through the admission wait and register notification before reading its state |

The broad Rust unit run was stopped under Paul's policy, preserving1683
passes and64 failures. Ten other failures are handed to the separate unit
repair effort; they have not all been proved pre-existing. Passing tests are
not repeated. Windows qualification was explicitly canceled and is not a pass.

Forgejo's original automated Main promotion gate remains failed. It cannot
consume a combined manual failed-only receipt; no synthetic passing status is
posted. The live PR distinguishes actual manual evidence from that gate.

## Remaining acceptance boundaries

Compiler and isolated fixture evidence do not prove the full live matrix:

- Tailscale and two-home networking.
- Physical Apple/Android rendering and prepared handoff.
- Remote-only media mounts and sustained full-topology playback.
- Active Shared upgrade/restore with historical binaries.

The [operator guide](SHARED-LIBRARIES-OPERATIONS.md) and implementation
contract describe these boundaries. Optional readiness stays advisory;
no guard silently overrides the user's saved enable choice.

## Ownership and cleanup

Root and the three builders use owned clones under `/private/tmp`; Paul's
checkout is not used for changes. Remote compilers receive committed source
archives without Git metadata or repository credentials.

Temporary Apple and Linux runner registrations/processes and the canceled
Windows compiler have been removed. The owned manual Rust compiler/cache and
isolated CGNAT network remain only through qualification. Final cleanup and
retained evidence links are recorded in PR #828. Shared infrastructure and
other sessions' workspaces are left intact.
