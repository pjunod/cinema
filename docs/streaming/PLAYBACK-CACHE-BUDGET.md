# Playback cache budget — one allocation authority for every playback writer

**Status:** built; Astra review addressed; fleet acceptance pending · **Owner:** Codex · **Updated:** 2026-10-21

Companion to [OPERATIONS.md](../OPERATIONS.md#split-durable-state-persistent-cache-and-scratch) (storage paths) — this records the shared playback disk policy and the repair for completed renditions filling a server disk.

## One node-local budget

`cache.max_gb` is the aggregate node-local playback cache ceiling in GiB. An absent setting means 50 GiB; zero refuses new cache writes and retires idle files. The budget covers completed VOD renditions, retained outputs, background/offline transcodes, extracted subtitle sidecars, retained subtitle sources, PGS overlays, transcription stages and live/rolling playback scratch. Durable catalogue/Raft state, artwork, source media and user downloads outside managed cache roots are excluded.

The previous completed-rendition policy deliberately had no aggregate ceiling. A per-title admission threshold did not limit the sum of cached titles, and a separate pre-transcode limit and subtitle-source cap could each spend space independently. That policy is superseded by this one authority. Existing optional background-production and retention switches retain their purpose; this correctness fix adds no Developer toggle.

## Reserve before writing, reclaim through the owner

Each writer obtains capacity before bytes land and retains the reservation through publication or child settlement. Concurrent writers cannot all spend the same free-space sample. Physical inventory includes old directories after restart, pending grants and committed growth between scans. Hard-linked files are counted once by filesystem identity; unlinking one name does not create credit while another name still retains its allocation.

Pressure selection orders eligible entries across namespaces by age. Each cache owner performs its own deletion, preserving its existing reader, writer, preparation, durable-interval and publication rules. Active response bodies retain pins until their file descriptors close. Filesystem/store errors refuse new work rather than authorizing deletion from an incomplete inventory.

Before new production, and every 15 seconds independently of production schedules, the coordinator enforces both the aggregate ceiling and a filesystem reserve of at least 1 GiB or 2% of each cache filesystem, whichever is larger. If active or durable dependencies prevent enough eviction, new production is refused cleanly; current readers are protected. A reduced budget can therefore remain temporarily exceeded by protected bytes, but cannot grant more growth.

External FFmpeg HLS output crosses the owned loopback upload receiver for foreground, background and Live TV output. Live TV retains its sliding-window segment deletion. Subtitle tee stages have per-file child limits, and PGS/transcription attempts reserve bounded output before launch. Unmanaged CUDA/Mesa disk caches are disabled so driver writes cannot bypass admission; older known runtime cache directories remain eligible for cleanup. On platforms without an enforceable child file-size limit, direct-file subtitle jobs refuse before launch rather than permit unreserved output.

## Validation and delivery

Focused tests exercise global LRU across cache types, concurrent reservations, budget shrink/off, active pins, restart leftovers, hard links, failed deletion, cancellation during cleanup, rolling/background writes and owner-specific VOD/subtitle protections. Run them with the pinned Rust 1.97.1 compiler. The PR carries exact commands and `Regression-Test:` anchors.

A passing compiler/test run is source evidence. Production deployment and the resulting fleet cache footprint remain separate acceptance evidence.
