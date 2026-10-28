# ci/ — the build, the gate, and the agents that use it

The index of `docs/ci/`: every document in this folder, the question it
answers, and its status. Routed from [the docs index](../README.md), which
defines the status words. Add a row here, in the same commit, when you add
a document to this folder.

| File | Answers | |
|---|---|---|
| [ENGINEERING-KNOWLEDGE.md](ENGINEERING-KNOWLEDGE.md) | Code/diff queries, source-backed record maintenance, advisory PR capture and evidence limits for the bounded knowledge pilot. | open |
| [DOCKER-HARDWARE-STATUS.md](DOCKER-HARDWARE-STATUS.md) | Automatic Docker GPU passthrough, device groups, NVIDIA libraries, review and validation progress. | open |
| [NATIVE-CI-RECEIPTS.md](NATIVE-CI-RECEIPTS.md) | Authenticated native successes, unchanged source closures, build-specification boundaries and original-run provenance. | built |
| [PYTHON-UNIT-PR-RECEIPTS.md](PYTHON-UNIT-PR-RECEIPTS.md) | Once-per-PR Python successes: authenticated journals are adopted, anything else simply runs again — history never blocks a PR. | open |
| [EFFORT-WEB-STATIC-CONTRACTS.md](EFFORT-WEB-STATIC-CONTRACTS.md) | Current-source effort web lint, preserved shape obligations, and explicitly deferred behavioral qualification. | open |
| [STATUS-HISTORY.md](STATUS-HISTORY.md) | Includes the 2026-09-24/25 sections moved on 2026-10-05. Status history: the `STATUS.md` sections for this folder's efforts, moved verbatim on 2026-09-24 and dated by when each was first recorded; `STATUS.md` keeps a one-line row per section pointing here. | done |
| [LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md](LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md) | Implementation plan from the 2026-09-20 architecture review: a PR-level regression field bound to the merged tree that replaces per-commit receipts without deleting one, the narrowed corrective rule, the text-contract pruning protocol, the STATUS.md split, and the release-tag restart with `sha-` images kept as deploy identity. | open |
| [ARCHITECTURE-DOC-RECONCILIATION.md](ARCHITECTURE-DOC-RECONCILIATION.md) | Implementation plan from the 2026-09-20 architecture review: every wrong number in ARCHITECTURE.md verified against its constant, the one PR that rewrites it with named constants, the DVR reversal as a dated decision, a `doc_versions.py` extension, and the 46 index rows whose doc header disagrees. | done |
| [RUST-TEST-EXECUTION-POLICY.md](RUST-TEST-EXECUTION-POLICY.md) | Current compile-only ready-Rust policy and recorded known-failure close-out; historical gaps, original failed receipts and source/environment adoption limits remain explicit. | done |
| [SERVICE-LIMITS-CHILD-PRIORITIES-AND-BUILD-HYGIENE.md](SERVICE-LIMITS-CHILD-PRIORITIES-AND-BUILD-HYGIENE.md) | Implementation plan from the 2026-09-20 architecture review: systemd and Compose limits each with its validation step, child priorities and OOM adjustment, CI on the shipped ffmpeg 8, the corrected release profile, and parser fuzz targets. | open |
| [RIPWIRE-PILOT.md](RIPWIRE-PILOT.md) | Measured query cost, source/fixture omissions, and the trial's opt-in verdict (superseded 2026-10-17: Ripwire is the default navigator). | done |
| [RIPWIRE.md](RIPWIRE.md) | Explicit setup, bounded navigation, output meanings, and coverage limits. | live |
| [RIPWIRE-STATUS.md](RIPWIRE-STATUS.md) | Ripwire implementation, measured evidence, decisions, and promotion progress. | open |
| [CI_TEST_OVERHAUL_PLAN.md](CI_TEST_OVERHAUL_PLAN.md) | Fast failures, selective evidence, safe reuse. | open |
| [APPLE-RUNNER-FAILOVER.md](APPLE-RUNNER-FAILOVER.md) | The M4 Apple primary, M3 Max standby, outage detection, installation and job-local DerivedData. | live |
| [CI_EXECUTION_ACCELERATION_PLAN.md](CI_EXECUTION_ACCELERATION_PLAN.md) · [review](CI_EXECUTION_ACCELERATION_REVIEW.md) | Persistent caches, native packaging, exact sharding. | open |
| [AGENT-COMPILE-LOOP.md](AGENT-COMPILE-LOOP.md) | A compiler for a checkout that has none. | live |
| [PAPERWORK.md](PAPERWORK.md) | Which bookkeeping rules run first — on push and as every pull request's first job — and the fix for each. | live |
| [AI-HARNESS-ASSESSMENT.md](AI-HARNESS-ASSESSMENT.md) | Why agent navigation and dependable feedback should precede new orchestration, and where Ripwire fits. | open |
| [AI-HARNESS-FABLE-ASSESSMENT.md](AI-HARNESS-FABLE-ASSESSMENT.md) | Fable's assessment: fix the compile loop, test evidence, and prompt drift before navigation tooling; positions on Gemini's and Codex's proposals. | open |
| [AI-HARNESS-IMPLEMENTATION-PLAN.md](AI-HARNESS-IMPLEMENTATION-PLAN.md) | Ten milestones, in that order, with exact contracts and acceptance checks: agent-check, tests out of the hotspots, prove-fix, fences, guides, swarm/ alignment, status fragments, PR ledger, Ripwire pilot, first extraction. | open |
| [RIPWIRE-IMPLEMENTATION-HANDOFF.md](RIPWIRE-IMPLEMENTATION-HANDOFF.md) | CLI-first build contract for bounded repository navigation, benchmark evidence, validation integration, and rollout limits. | open |
| [FORGEJO-MAIN-IMAGE-HANDOFF.md](FORGEJO-MAIN-IMAGE-HANDOFF.md) | Publishing the main image from Forgejo. | open |
| [RUNNER-DISK.md](RUNNER-DISK.md) | What fills a runner, what bounds it, how to reclaim it. | live |
| [MEDIA1-RUNNER-ORPHANED-PROCESSES.md](MEDIA1-RUNNER-ORPHANED-PROCESSES.md) | Why effort preflight leaked stopped children on media1, and how cleanup is proved. | open |
| [MEDIA1-RUNNER-ORPHANED-SLEEP-PROCESSES.md](MEDIA1-RUNNER-ORPHANED-SLEEP-PROCESSES.md) | Evidence for stopped orphaned sleeps in the media1 runner cgroups and the safe cleanup boundary. | open |
