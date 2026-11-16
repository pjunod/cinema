# server/ — the HTTP layer, scanning, images, auth and observability

The index of `docs/server/`: every document in this folder, the question it
answers, and its status. Routed from [the docs index](../README.md), which
defines the status words. Add a row here, in the same commit, when you add
a document to this folder.

The daemon's request path and its background workers, as distinct from what
is streamed (`streaming/`) and what is replicated (`cluster/`).

| File | Answers | |
|---|---|---|
| [SOURCE-LEASE-RELEASE-PLAN.md](SOURCE-LEASE-RELEASE-PLAN.md) | Plan: lease expiry alone frees source capacity — why the physical-proof custody model is retired, the 60 s grace, and the removal steps. | open |
| [SOURCE-LEASE-TABLE-AND-NODE-CAPACITY-BUILD.md](SOURCE-LEASE-TABLE-AND-NODE-CAPACITY-BUILD.md) | Build plan and as-built record for source-lease steps 3 and 4: one `source_leases` table, per-node and per-domain source readers. | built |
| [SOURCE-LEASE-AS-BUILT.md](SOURCE-LEASE-AS-BUILT.md) | As-built record of source-lease steps 3 and 4: every place the build differs from its plan, and why. | built |
| [SOURCE-LEASES-OPERATIONS.md](SOURCE-LEASES-OPERATIONS.md) | Operator guide: how source leases count per node and per storage domain, the reader settings, how to read Activity, and the metrics. | current |
| [JOB-QUEUE-ROOT-CAUSE-AND-FIX.md](JOB-QUEUE-ROOT-CAUSE-AND-FIX.md) | November queue incident, recovered scan requests, cross-type scheduling, Activity completion rates, and remaining acceptance milestones. | open |
| [STORAGE-DOMAINS-DEFERRED-APPLY-STATUS.md](STORAGE-DOMAINS-DEFERRED-APPLY-STATUS.md) | Deferred storage-domain implementation, ownership, review and qualification state. | built |
| [STORAGE-DOMAINS-DEFERRED-APPLY-PLAN.md](STORAGE-DOMAINS-DEFERRED-APPLY-PLAN.md) | Plan: a storage-domain save always succeeds and applies once no live holder or library job depends on the changed keys; Settings shows it pending. | built |
| [PLAYBACK-READER-WITNESS-STATUS.md](PLAYBACK-READER-WITNESS-STATUS.md) | Playback witness ownership, phase execution, census safety findings and remaining qualification, as they stood before lease expiry replaced the witness model. | superseded |
| [PLAYBACK-READER-WITNESS-PLAN.md](PLAYBACK-READER-WITNESS-PLAN.md) | Plan: every child launch declares its source access, checked fail-closed at spawn, so playback witnesses stay valid and a daemon restart releases its readers with no host step. Phases B0–B3 superseded by the source lease release plan. | superseded |
| [HTTPS-IMPLEMENTATION.md](HTTPS-IMPLEMENTATION.md) | Optional public HTTPS, certificate ownership and administration contract; physical acceptance open. | built |
| [HTTPS-BUILD.md](HTTPS-BUILD.md) | HTTPS implementation built, physical acceptance open; qualification receipts and merge outcome tracked by [promotion PR #1134](http://forge.lan:3000/noirr/plurx/pulls/1134). | built |
| [HTTPS-EVIDENCE-LEDGER.md](HTTPS-EVIDENCE-LEDGER.md) | The HTTPS build's checkpoint-by-checkpoint evidence ledger (section 5 of HTTPS-BUILD). | built |
| [HTTPS-REVIEW.md](HTTPS-REVIEW.md) | Adversarial HTTPS design findings, the single-task workflow amendment, and the final implementation review with correction dispositions. | done |
| [DESKTOP-CONTROL.md](DESKTOP-CONTROL.md) | How to install and use native tray controls, choose server/tray startup independently, inspect local JSON status and recover signed-release upgrades. | open |
| [DETAIL-READS-AND-STORAGE-AVAILABILITY.md](DETAIL-READS-AND-STORAGE-AVAILABILITY.md) | Implementation plan from the 2026-09-20 architecture review: why an item-detail page stalls on an unmounted share, and how the index badge is decided without loading the index. | open |
| [TELEMETRY-BACKPRESSURE.md](TELEMETRY-BACKPRESSURE.md) | Implementation plan from the 2026-09-20 architecture review: what bounds playback telemetry when the node-local writer is slow, and why the metrics survive retention being turned off. | open |
| [PLEX-FACADE-PAGING.md](PLEX-FACADE-PAGING.md) | Implementation plan from the 2026-09-20 architecture review: how the Plex façade answers a 5,000-item section without an N+1, and what must be proven before the media-session route cache is touched. | open |
| [OBSERVABILITY-BASELINE.md](OBSERVABILITY-BASELINE.md) | Implementation plan from the 2026-09-20 architecture review: what plurx publishes about its own HTTP surface and its panics, and which metric is each release-evidence number. | open |
| [HTTP-LISTENER-TIMEOUTS-AND-ASSET-DELIVERY.md](HTTP-LISTENER-TIMEOUTS-AND-ASSET-DELIVERY.md) | Implementation plan from the 2026-09-20 architecture review: header-read and per-group handler deadlines the listener lacks, precompressed web assets with correct negotiation, hashed `hls.min.js` and sidecars, and the security headers — as three PRs. | built |
| [SCAN-AND-ENRICHMENT-HYGIENE.md](SCAN-AND-ENRICHMENT-HYGIENE.md) | Implementation plan from the 2026-09-20 architecture review: a bounded blocking walker, provider/join deadlines, and the bounded AniList transient-retry follow-up; fleet acceptance remains separate. | built |
| [IMAGE-SERVING-AND-DERIVATIVES.md](IMAGE-SERVING-AND-DERIVATIVES.md) | Implementation plan from the 2026-09-20 architecture review: verified digests instead of per-request hashing, separate local-read and peer-fetch permits, `ETag`s, and size variants added without reverting `original` backdrops. | open |
| [AUTH-HARDENING.md](AUTH-HARDENING.md) | Implementation plan from the 2026-09-20 architecture review: bounded logout admission that keeps the two-phase revocation fence, a lockout-resistant login throttle, and token expiry as a product decision with its client consequences. | open |
| [REMOTE-DIAGNOSTICS-OPERATIONS.md](REMOTE-DIAGNOSTICS-OPERATIONS.md) | Retained operating instructions for the deferred support service; archive attachments do not require its deployment. | open |
| [MACOS-PARSER-IDENTITY-DEADLINE.md](MACOS-PARSER-IDENTITY-DEADLINE.md) | Why a macOS start can lose explicit decoder facts: the cold source-parser compile runs inside the 10 s identity deadline. Root cause, impact, proposed fix and progress (paused). | open |
