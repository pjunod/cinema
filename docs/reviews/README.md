# reviews/ — pull-request review records

The index of `docs/reviews/`: every document in this folder, the question it
answers, and its status. Routed from [the docs index](../README.md), which
defines the status words. Add a row here, in the same commit, when you add
a document to this folder.

| File | Answers | |
|---|---|---|
| [CINEMA-END-TO-END-REVIEW-2026-11-07.md](CINEMA-END-TO-END-REVIEW-2026-11-07.md) | End-to-end review of the playback start path against HEAD and the live fleet (2026-11-07): why starts are slow or fail (starved fragment-index queue, engine-keyed index identity, 16 s rolling gate, a 10-phase create, consensus for everything), how the October HDR/DV, capability and encoding changes sit on it, and the ranked design adjustments with their acceptance readings. | open |
| [PLAYBACK-START-D3-REVIEW-2026-11-12.md](PLAYBACK-START-D3-REVIEW-2026-11-12.md) | Adversarial design review of the D3 (create collapse) build contract: 6 blockers, 9 should-fix — write and read ledgers against the pinned per-shape counts, helper splits that reach takeover, the owner fold on a mixed fleet; dispositions in the contract's §13. | closed |
| [PLAYBACK-START-D3A-REVIEW-2026-11-12.md](PLAYBACK-START-D3A-REVIEW-2026-11-12.md) | Adversarial code review of lane D3a (store-method attribution): 1 blocker (stale formal mapping), 6 should-fix (helper-name labels, layout reads folded into the issuing method, an unexercised catalog read, an untested owner-start scope, `queue_sql`, the `cluster` metric's meaning), 9 nits; all dispositioned in its last section. | done |
| [PLAYBACK-START-D3T-REVIEW-2026-11-12.md](PLAYBACK-START-D3T-REVIEW-2026-11-12.md) | Adversarial code review of lane D3t (planning-key triggers): 2 blockers (SQLite v129 breaks three pinned fixtures, `subtitles.stored_sources` unbound), 4 should-fix (rollback promise, revisioned shape check, contract pin coverage, hand-typed chain list), 5 nits; all dispositioned in its last section. | done |
| [PLAYBACK-START-D3B-REVIEW-2026-11-13.md](PLAYBACK-START-D3B-REVIEW-2026-11-13.md) | Adversarial code review of lane D3b (one StartPlan): 1 blocker (a stale ingress view fails negotiated creates and is never closed), 4 should-fix (refuted plan offered to the next candidate, unbound remote starts, untested refusal path and floor writers, the owner read outside its stage deadline), 6 nits; all dispositioned in its last section. | done |
| [PLAYBACK-START-E1-REVIEW-2026-11-11.md](PLAYBACK-START-E1-REVIEW-2026-11-11.md) | Adversarial design review of the E1 build contract: 6 blockers, 9 should-fix; dispositions in the contract's §13. | closed |
| [PLAYBACK-START-E1C-REVIEW-2026-11-12.md](PLAYBACK-START-E1C-REVIEW-2026-11-12.md) | Adversarial code review of lane E1c (session routes from the applied view) at `claude/ps-e1c @ d6e41bdce`: 2 blockers (an unretired fact dropped without its floor; legacy-layout retirement raising the quorum rate), 5 should-fix, 8 nits; all dispositioned in its last section. | done |
| [PREPARATION-BUFFER-PRESSURE-IMPLEMENTATION-REVIEW.md](PREPARATION-BUFFER-PRESSURE-IMPLEMENTATION-REVIEW.md) | Actual storage-owner implementation review: seven findings corrected and final focused evidence approved. | done |
| [PREPARATION-BUFFER-PRESSURE-PLAN-REVIEW.md](PREPARATION-BUFFER-PRESSURE-PLAN-REVIEW.md) | Adversarial review of the October 8 playback-buffer repair: capacity handoff, early cancellation, restart and cleanup-failure ownership; findings and dispositions. | open |
| [S11-GRAIN720-NAL-EVIDENCE-20261002.md](S11-GRAIN720-NAL-EVIDENCE-20261002.md) | One retained synthetic Grain720 header census: 35 first IDRs, two extras, reviewable historical parser/control/result/process/cleanup snapshots and private audit-byte retention; sanitized-not-raw and original S11 qualification limits. | done |
| [S11-RETAINED-NAL-CONTINUATION-20261002.md](S11-RETAINED-NAL-CONTINUATION-20261002.md) | Sixteen-context retained-header ledger: thirteen Linux passes plus three retained Grain successes, preserved Grain720 extras, raw-private hash/metric/cleanup identities and historical route/local/pressure failures; reviewer local access, not portable raw evidence or original S11 qualification. | done |
| [ARCHITECTURE-REVIEW-2026-09-20.md](ARCHITECTURE-REVIEW-2026-09-20.md) | The end-to-end architecture review of 2026-09-20, revision 3 (adversarial assessment applied, Astra's independent review merged) — ranked findings for performance, stability, video quality and design, ten verified do-first items, and the sequencing. | open |
| [ARCHITECTURE-REVIEW-2026-09-20-APPENDIX.md](ARCHITECTURE-REVIEW-2026-09-20-APPENDIX.md) | The nine full area reports behind that review, unrevised, with every finding's quoted evidence and each area's "already good" list; the main document wins where they disagree. | open |
| [ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md](ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md) | Checks all 82 consolidated entries and 128 appendix findings against the source, with corrections, per-finding verdicts and evidence limits; revision 2 of the review applies its dispositions. | done |
| [ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md](ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) | The single shared status for every implementation plan from the review, across Claude, GPT and OpenRouter sessions — the claim protocol, the model/session identity rules, and one row per plan. | live |
| [ARCHITECTURE-REVIEW-2026-09-20-CLOSEOUT.md](ARCHITECTURE-REVIEW-2026-09-20-CLOSEOUT.md) | October 7 source reconciliation: integrated implementation, the facts-authoritative S-08 routing correction, conservative retained decisions and grouped physical/fleet, upstream and discretionary release acceptance; the workboard remains canonical. | open |
| [ARCHITECTURE-REVIEW-GPT-BUILD-STATUS.md](ARCHITECTURE-REVIEW-GPT-BUILD-STATUS.md) | Queue, next action and evidence locations for the GPT architecture-review build and fleet pass, last audited 2026-09-28; superseded by the work board. | superseded |
| [ARCHITECTURE-REVIEW-GPT-WORKBOARD-HISTORY-2026-09-28.md](ARCHITECTURE-REVIEW-GPT-WORKBOARD-HISTORY-2026-09-28.md) | Verbatim dated snapshots of the seven assigned rows before compacting current workboard notes; preserves original source and evidence scopes. | done |
| [ARCHITECTURE-REVIEW-FLEET-EVIDENCE-2026-09-24.md](ARCHITECTURE-REVIEW-FLEET-EVIDENCE-2026-09-24.md) | The four-node `f600d2823` deployment receipt, preliminary fleet readings, failed Chrome D3 trace and remaining evidence for each architecture-review workboard row. | open |
| [ARCHITECTURE-REVIEW-FLEET-CLIENT-BASELINES-2026-09-25.md](ARCHITECTURE-REVIEW-FLEET-CLIENT-BASELINES-2026-09-25.md) | Read-only L-01, L-02, C-03, S-04, S-05 and K-06 baselines on the deployed fleet, with the exact acceptance gaps. | open |
| [ARCHITECTURE-REVIEW-FLEET-READOUT-2026-09-25.md](ARCHITECTURE-REVIEW-FLEET-READOUT-2026-09-25.md) | Dated read-only K-02, C-05, C-08, P-02 and S-11 fleet measurements, collector windows and exact acceptance limits on current main. | open |
| [ARCHITECTURE-REVIEW-FLEET-EVIDENCE-2026-10-11.md](ARCHITECTURE-REVIEW-FLEET-EVIDENCE-2026-10-11.md) | Read-only K-06, L-02, C-08, C-01, P-02, K-08 and K-05 observations on the four nodes in the first quarter-hour of the #983 `05bf60381` build, with what each does not establish. | open |
| [Architecture review implementation status](ARCHITECTURE-REVIEW-2026-09-20-STATUS.html) | Live searchable view of the canonical architecture-review workboard, with a read-only same-origin Forgejo overlay for mapped open plan PRs and exact-head CI state; API failures leave every board row visible. | live |
| [SECURITY-ASSESSMENT-2026-09-13.md](SECURITY-ASSESSMENT-2026-09-13.md) | Which trust boundaries need work across the server, cluster, clients, media supply chain, and delivery process. | open |
| [WEEKLY-ARCHITECTURE-SECURITY-IMPLEMENTATION.md](WEEKLY-ARCHITECTURE-SECURITY-IMPLEMENTATION.md) | Bounded implementation handoff for the September architecture, security, recovery, and playback findings. | open |
| [Web seek Opus review](WEB-SEEK-LATENCY-OPUS-REVIEW.md) | Independent diagnosis approval and requested plan changes: MSE/MMS selection, required iPhone qualification, frame-proof defects and native successor startup. | done |
| [Fable's ATSC 1.0 review](LIVE-TV-VIDEOTOOLBOX-ATSC1-FABLE-REVIEW.md) | Supplied independent reproduction and blockers against the original stale-base candidate. | done |

The remaining files are one record per reviewed PR, each naming the verdict
and findings against a specific head commit. They are kept because other docs
cite them; none describes current behavior.

[PR #79](PR79-CINEMA-HANDOFF-REVIEW.md) ·
[#80](PR80-ARTWORK-RETRY-REVIEW.md) ·
[#81](PR81-ARTWORK-RETRY-RESPONSE-REVIEW.md) ·
[#82](PR82-CLUSTERING-M0-REVIEW.md) ·
[#83](PR83-CLUSTERING-M1A-REVIEW.md) ·
[#84](PR84-CLUSTERING-M1B-REVIEW.md) ·
[#88](PR88-CLUSTERING-M1C-REVIEW.md) ·
[#89](PR89-IPAD-SYSTEM-CHROME-REVIEW.md) ·
[#90](PR90-CLUSTERING-M1D-REVIEW.md) ·
[#91](PR91-APPLE-HLS-AHEAD-WINDOW-REVIEW.md) ·
[#92](PR92-SKIP-CREDITS-LAYOUT-REVIEW.md) ·
[#94](PR94-ARTWORK-CACHE-REVIEW.md) ·
[#98](PR98-CLUSTERING-IMPORT-BACKUP-REVIEW.md) ·
[effort/live-tv-guide](EFFORT-LIVE-TV-GUIDE-REVIEW.md)
