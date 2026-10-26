# server/ — the HTTP layer, scanning, images, auth and observability

The index of `docs/server/`: every document in this folder, the question it
answers, and its status. Routed from [the docs index](../README.md), which
defines the status words. Add a row here, in the same commit, when you add
a document to this folder.

The daemon's request path and its background workers, as distinct from what
is streamed (`streaming/`) and what is replicated (`cluster/`).

| File | Answers | |
|---|---|---|
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
