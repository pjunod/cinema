# performance/ — where the seconds go

The index of `docs/performance/`: every document in this folder, the question it
answers, and its status. Routed from [the docs index](../README.md), which
defines the status words. Add a row here, in the same commit, when you add
a document to this folder.

[Video quality programme](VIDEO-QUALITY-PROGRAM.md) — approved calibration and performance build order; [execution ledger](VIDEO-QUALITY-STATUS.md) — parallel ownership, actual measurements, merges and remaining evidence. **Built; remaining acceptance resumed October 8 in three parallel lanes.**

[Video quality batch qualification](../evidence/video-quality-2026-10-03/) — final review corrections, native content analysis, packaged scorer, Developer settings, isolated VAAPI output and retained per-check fast-lane receipts. **Qualified; landing and cleanup in PR #766.**

[Video quality acceptance follow-up](../evidence/video-quality-2026-10-08/) — retained source-local title scoring, actual Plurx browser/native playback, HDR cause isolation and matched latency controls. **Measured; acceptance limits and landing status in the [execution ledger](VIDEO-QUALITY-STATUS.md).**

[HDR sharp-edge and autonomous client follow-up](../evidence/video-quality-2026-10-12/) — driver-feature ablations, source-matched rate/quality controls and retained Safari/native client validation; current integration and acceptance scope in the [execution ledger](VIDEO-QUALITY-STATUS.md).

Two rounds of performance work, each with its plan, review, and response.

| File | Answers | |
|---|---|---|
| [HDR-REFERENCE-SCORING.md](HDR-REFERENCE-SCORING.md) | Independent bounded PQ code-domain metrics/census and explicitly graded BT.709 reference provenance for SDR VMAF; diagnostic evidence does not close genuine corpus, GPU or physical acceptance. | open |
| [TONEMAP-SEGMENT-CONSISTENCY.md](TONEMAP-SEGMENT-CONSISTENCY.md) | Exact source-frame and rational-PTS comparison of continuous and restarted tone-map filters; separate from seek, encoded-HLS and display acceptance. | open |
| [Tone-map reset receipt](evidence/tonemap-reset-20261015.json) | Four authored CPU/Metal campaigns, exact raw-frame and rational-PTS results, the preserved fixture failure and metadata-only correction proof. | done |
| [PERF-PLAN.md](PERF-PLAN.md) | Round one: where the seconds go, and the plan to get them back. | built |
| [PERF-PLAN-REVIEW.md](PERF-PLAN-REVIEW.md) | What to correct before implementing it. | done |
| [PERF-REVIEW-RESPONSE.md](PERF-REVIEW-RESPONSE.md) | What the review got right, what already shipped, what changed. | done |
| [PERF-REVIEW-ASSESSMENT.md](PERF-REVIEW-ASSESSMENT.md) | What is resolved and what still needs a contract. | done |
| [PERF2-PLAN.md](PERF2-PLAN.md) | Round two: better starts, steadier streams, smarter bits. | open |
| [PERF2-PLAN-REVIEW.md](PERF2-PLAN-REVIEW.md) | The seam contracts are not implementation-ready. | done |
| [PERF2-REVIEW-RESPONSE.md](PERF2-REVIEW-RESPONSE.md) | All eight findings accepted. | done |
| [PERF2-IMPLEMENTATION-HANDOFF.md](PERF2-IMPLEMENTATION-HANDOFF.md) | How to build round two without breaking it. | open |
| [CODE-QUALITY-PERFORMANCE-REVIEW.md](CODE-QUALITY-PERFORMANCE-REVIEW.md) | A strong data plane with lifecycle gaps. | done |
