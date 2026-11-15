# Fast-lane throughput

[throughput](../docs/ci/CI-THROUGHPUT.md), [Rust gate image](../docs/ci/RUST-GATE-IMAGE.md). Owner Claude (claude-opus-5-5, session 01BCWLA2), PR #1550. A push cancels the PR's in-flight run; derivable paperwork (counters, Markdown host names, index rows) is committed by the lane; the Rust gate runs once in a pinned image at `-j4` (8.2 min on a lab1 guest, was 22-25). The high-CPU pool stays unused until its runners apply the job's CPU, PID and swap limits.
