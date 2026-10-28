# Status — what the agent is working on and where it stands

**Updated:** 2026-10-28 · Newest effort first; each entry links to its current ledger.
<!-- status: insert new efforts below this line -->
- **2026-10-28 · Dolby Vision title scan without decoding:** [how it runs](docs/streaming/DISPLAY-TARGETED-DV-BUILD.md); [PR #1301](http://forge.lan:3000/noirr/plurx/pulls/1301). Owner Codex (resumed Claude session 01BCWL9z). `scan_title` parses RPUs without decoding pictures, and title demand is keyed by the scanner alone so renderer changes do not re-queue every title. During content analysis the scanner runs beside the index pass on a duplicate of the same held source; measured storage reads total one file copy. Adversarial review addressed; parity, mutation checks and final landing are tracked on the PR. Not deployed: Paul deploys plurxd, rebuilding the DV bundle; avoid mixed-version nodes during rollout.
- **2026-10-27 · VOD index recovery:** Owner Claude (Linux Dolby Vision session); [#1299](http://forge.lan:3000/noirr/plurx/pulls/1299). Large remuxes never got a VOD index, so every play fell back to live HLS: the index read's time budget ignored file size and a timed-out build was never retried yet reported "pending" forever (77 files on lab5), the local pass quit whenever a DV title scan held the background lane, and in the cluster a learner never resolved requests aimed at it while viewer requests were refused at the 4096-row queue cap. Fixed with a size- and rate-based budget, due/waiting/terminal outcomes with a distinct `vod_index_unavailable` reason, a queued local pass, and learner/viewer/removed-node queue handling; adversarial review's 12 findings fixed. Known limit: copy/VOD viewers without an admission do not preempt an index read. Not deployed.
- **2026-10-26 · Subtitle completion:** [live status](docs/features/SUBTITLE-COMPLETION-STATUS.md). Owner Codex /root; [PR #1292](http://forge.lan:3000/noirr/plurx/pulls/1292) batches all caption operations and native delivery. Review and moving-runtime qualification pass; useful captions at 13.3/14.6s on two CPUs. Final gate and landing are tracked on the PR. Paul deploys.
- **2026-10-26 · Linux player (Dolby Vision through plurx):** [plan and milestones](docs/clients/LINUX-PLAYER-PLAN.md). Owner Claude (Linux Dolby Vision session). M1–M5 (#1285, #1288, #1290, #1291) merged to `main` by [#1295](http://forge.lan:3000/noirr/plurx/pulls/1295); not deployed. Package 0.3.0 verified in a fresh container and rebuilds byte-identical; on the ROG (CI runners paused) both the RTX and the Arc iGPU play 4K CM v4.0, CM v2.9 and FEL clips without dropped frames. Paul's first real playback (Avatar, from the web app) found four faults, fixed in 0.3.1 ([#1297](http://forge.lan:3000/noirr/plurx/pulls/1297)): no display from the portal, the NVIDIA driver's colour surface closing the window, control capabilities missing (400/409), and copies narrowing Profile 7 to HDR10 for a client that names it (server). Deployed 2026-10-27; Avatar and Twilight now play in Dolby Vision from the web app. 0.3.2 ([#1298](http://forge.lan:3000/noirr/plurx/pulls/1298)) adds the Profile 7 enhancement layer over HLS (mpv's lavf demuxer path never split it), the whole film on the seek bar of a live session (mpv patch 0005), and a package build that survives snapshot.ubuntu.com outages. Next: M6 acceptance on the ROG.
- **2026-10-26 · CM29 analytic gamut reconstruction:** [current rendering ledger](docs/streaming/DISPLAY-TARGETED-DV-BUILD.md). Owner Codex chat 01a17554. Standard IPT D65, the published colorfulness equation, analytic gamut interpolation and separate stock SDR/output blacks replace fitted gains; method v5 invalidates older targeted renditions. Selected-frame comparisons improve across all four titles, including Smile’s dark band. Adversarial review approved; native parity passed. [PR #1289](http://forge.lan:3000/noirr/plurx/pulls/1289) records final fast-lane qualification and landing. Private raw evidence is retained. No deployment: Paul deploys.
- **2026-10-25 · Progressive subtitle setup repair:** [live status](docs/features/SUBTITLE-SETUP-REPAIR-STATUS.md). Owner Codex /root; [#1283](http://forge.lan:3000/noirr/plurx/pulls/1283) batches `codex/subtitle-setup-repair` to `main`. Install response and setup UX fixes are built locally; FIFO model-test admission is built locally. Adversarial review found no actionable issues; qualification and landing are tracked on the PR.
- **2026-10-25 · Display-targeted DV creative trims (Phase E finish):** [build plan, phase 2](docs/streaming/DISPLAY-TARGETED-DV-BUILD.md#phase-2-closed-form-dolby-display-management-l1-l3-l2-l8). PR #1276 serves the CM v4 L8 and CM v2.9 L2 trims (L2 interpolated linearly in target PQ, measured on the reference), accepted by pixel comparison on eight real titles: CM v4 mean ΔE_ITP ≤ 0.011, CM v2.9 0.14–0.46 (untrimmed was 4.9–14). Playback reports `creative_trims_applied` per session; Apple 275, Android 181. Owner Claude; merged as 072d0f65 after two review rounds and one green fast lane. Not deployed: Paul deploys #1275 and #1276 together, then a GPT physical-display check.
- **2026-10-25 · Display-targeted DV Phase E (closed-form Dolby display management):** [build plan, phase 2](docs/streaming/DISPLAY-TARGETED-DV-BUILD.md#phase-2-closed-form-dolby-display-management-l1-l3-l2-l8). PR #1275 replaces the Annex B.2 curve with our own CM v4 / CM v2.9 equations, checked privately against the licensed reference (CM v4 real P8 title mean ≤ 0.007 at 100 nits, ≤ 0.001 at 300–1600). Merged as 489e86e4 with served trims off; the trims PR #1276 turns them on. Paul deploys.
- **2026-10-23 · Display-targeted HDR10-E:** [A–F ledger](docs/streaming/DISPLAY-TARGETED-DV-BUILD.md). Owner direct GPT/root; #1216 and batched#1269 merged into the effort, latest3cc264068; main unchanged. Compile-only gate5889 all8 PASS, Rust382/C384/trim393 compile passes; earlier behavior/media/client evidence retains source attribution. Final integrated adoption, completed-candidate review and qualification remain. Root delivery decision covers HDR/SDR with E explicitly unfinished and truthful L1-only reporting; no full A–F completion. Promotion-test policy awaits user reconciliation; physical acceptance and the excluded full-app FEL EOF lane remain separate.
- **2026-10-22 · Rust admission refinement:** [live ledger](docs/cluster/RUST-ADMISSION-REFINEMENT-STATUS.md); production controller and exact-body harnesses built; Fable final review delivered, amendments implemented and five affected runtime schedules passed. Qualification and landing are tracked in the PR. Owner: Codex coordinator; [PR #1260](http://forge.lan:3000/noirr/plurx/pulls/1260).
- **2026-10-21 · Playback cache disk safety:** [shared budget contract](docs/streaming/PLAYBACK-CACHE-BUDGET.md); aggregate accounting, owner eviction and write admission built. Astra review addressed; focused qualification in progress; deployment acceptance remains open. Owner: Codex.
- **2026-10-21 · Worker refinement:** [live ledger](docs/cluster/WORKER-REFINEMENT-STATUS.md); seven Rust schedules pass; two complete workers, reviewed correspondence and observation projection built. Qualification and landing receipt: [PR #1251](http://forge.lan:3000/noirr/plurx/pulls/1251). Owner: Codex coordinator.
- **2026-10-21 · Avatar Dolby Vision:** [live diagnosis and evidence](docs/clients/AVATAR-WEB-PLAYBACK-STATUS.md); Intel repair #1250 deployed and Apple TV confirmed. Follow-up corrects shared capability/timeline contracts; Astra approved, Rust regression and 38 web tests passed; injected Safari reports FEL/RPU/DYN and 3,010 frames / zero drops. [Automatic frame-drop capture](docs/clients/FRAME-DROP-RECORDER.md) is built; production acceptance, slow startup and original display-drop trigger remain open. Paul owns deployment.
- **2026-10-21 · Operation progress:** [live ledger](docs/cluster/OPERATION-PROGRESS-STATUS.md); wait map/model/checker built and qualified; Fable approved; live promotion and landing are in PR #1249. Owner: Codex coordinator.
- **2026-10-20 · System settings:** [approved layout and comparison ledger](docs/clients/SYSTEM-SETTINGS-STATUS.md); built and independently rendered; native macOS comparison passes, Linux hardware acceptance open.
- **2026-10-20 · Safari resume:** [reviewed fix and 108 passing tests](docs/clients/SAFARI-PLAYBACK-ACTIVATION.md); physical Safari/HEVC acceptance pending.

## Processed VOD successor root reuse

Owner: Sol (source), coordinator (native/publication); [PR #1225](http://forge.lan:3000/noirr/plurx/pulls/1225), not deployed. Exact `a857f488b` passed pinned fmt/all-target Clippy and fresh test linkage; fifteen focused contracts have valid passes (eight current, seven retained). Independent review and bounded closures are approved. Exact-root reuse preserves identity, owner and reservation fences; serialized requests grant no authority. The PR records the main gate and landing.

B #1211 merged (`9753284e`, lane5600 green). Pixel diagnostics are captured; simulator fix #1215 merged (`7a7d04c6`, lane5601 green). The [handoff](docs/streaming/DV-FOLLOWUPS-HANDOFF.md#processed-root-repair) separates local qualification from Paul's deployment and live verification.

- **2026-10-19 · desktop Compose host controls:** native installer correction is
  recorded in PR #1226. Linux Docker companion builds only the host tray,
  pins the selected local deployment, and controls graceful lifecycle and
  durable startup policy; source/published-image upgrades retain their existing
  deployment paths. Focused fixture validation is recorded in its PR; real
  desktop/engine acceptance and custom Pi ownership migration remain open.

## macOS source installation and the missing tray

**Source correction built; PR review pending.** The first tray promotion packaged the companion for release archives but left `make install-macos` on the daemon-only path. Source installation now stages a complete native runtime and companion before migration; the explicit server-only path retains legacy prefix installs. [Desktop controls](docs/server/DESKTOP-CONTROL.md) explains installation, opening the app and separate login preferences. The focused controller/migration and installer regressions pass; independent PR review remains pending.

## Native server tray, startup policy and release upgrades

**Source built and reviewed on `effort/desktop-tray`; [PR #1213](http://forge.lan:3000/noirr/plurx/pulls/1213) records main promotion; not deployed.** The [implementation ledger](docs/features/DESKTOP-TRAY-IMPLEMENTATION.md) tracks macOS and Linux desktop controls, separate server/tray startup choices, and signed GitHub artifact upgrades. Astra completed the PR #1220 review; root addresses all eight findings and owns passing unit tests and merge gates. Controller, daemon, native UI and packaging source is built in cohort PR #1220. The [qualification ledger](docs/features/DESKTOP-TRAY-QUALIFICATION.md) distinguishes passing review regressions, repository merge evidence and remaining native distribution acceptance.

## Playback startup path: the per-read stall and the web scrim

**Branch `effort/startup-read-path`, in a draft pull request; not deployed.** Two fixes: consistent reads on a cluster no longer wait 40 ms each on a delayed ACK (44.0 ms to 1.7 ms per read on three-voter TLS loopback; a start makes about forty), and the web scrim clears about two frames after playback starts instead of 500 to 1000 ms later. Fleet effect unmeasured. The remaining waits, ranked with owners, are in [PLAYBACK-STARTUP-PATH-FINDINGS.md](docs/streaming/PLAYBACK-STARTUP-PATH-FINDINGS.md); its §5 client waits are unowned.

## Startup and tone-map: fewer create-path reads, a tone-map census, lab arms

**Branch `effort/startup-tone-map`.** Executes [STARTUP-AND-TONE-MAP-BUILD-HANDOFF.md](docs/streaming/STARTUP-AND-TONE-MAP-BUILD-HANDOFF.md)
rev 6.

- **Part B is built.** The create read-budget line had its authority and local labels swapped; that is corrected, and the line now carries `store_ms` and a per-phase split. Seven read reductions follow, none of which moves a read across a fence or changes its consistency.
  - On a two-voter hiqlite test cluster, store time is nearly all of the handler's time on every request shape.
  - Two regressions pin the source-replacement interval.
  - The candidate-route rolling fallback re-validates its binding before starting. Without that, a carried snapshot could start the producer from a source that had already been replaced.
- **The tone-map census (C-M0) and the single Dolby tonemapx builder (C-M1) are built.** The census is admin-started from System → HDR tone-map, yields to playback, can be stopped, and stores its results node-locally. Selection and filter strings are unchanged.
- **The A-M0 lab arms are built.** The lab names the bootstrap value B only from runs of at least 30 minutes.
- **Owed, all by the GPT session:**
  - the lab runs (§9.3), which pick B;
  - the census on every node (§9.2), which goes to Paul for R2;
  - the two-build B-M4 measurement (§9.1).
- **Waiting on those:** Part A's A-M1/A-M2 waits on B, and C-M2/C-M3 wait on R2.

## HDR tone-map on Apple Silicon runs on the GPU, held to CPU parity

**Branch `effort/macos-gpu-tonemap-parity`.** An Apple M4 node's Transcoding
backend row read "CPU tone-map — fell back" over a Vulkan `Cannot allocate
memory` and an OpenCL graph 59 levels from the CPU picture. Causes, each
reproduced on that node's Jellyfin FFmpeg 8.1.3: the macOS build has no
Vulkan compiled in, and the probe ran the graph anyway; the OpenCL graph
named `hable` but left Jellyfin's `tonemap_mode=auto` and desaturation on;
and the probe never tried the native Metal graph, whose BT.2390/ITP spelling
the Mac processing context used without any CPU comparison (29.5 levels
away). Now: the probe reads the build's device types and filters and reports
what it cannot express as not available; CUDA, OpenCL and Metal-on-PQ take
the CPU chain's operator from one source that also keys plan identity; the
Metal graph is the first VideoToolbox candidate (≈2.2× the CPU chain within
10.3 levels), and a pass ends the race with node routing left on the CPU
chain; and the Mac context offers its HDR10 Metal graphs only when that
parity passed. GPU graphs now get the CPU chain's peak. HLG through Metal is
unchanged. Use still goes through Mac video processing in Developer. Plan
identity gains the curve terms, so CUDA HDR and Mac Metal renditions (HLG
included) are made once more under new keys.

## Older efforts — where each one now lives

Sections older than those above moved verbatim on 2026-09-24, 2026-09-25, 2026-09-26, 2026-09-27, 2026-09-28, 2026-10-04, 2026-10-17, 2026-10-19 and 2026-10-26 into the status history of their subject folder. One row per section, newest first.

| First recorded | Effort | Now in |
|---|---|---|
| 2026-10-04 | Live TV station logos on Apple TV, iOS and Android | [docs/features/STATUS-HISTORY.md](docs/features/STATUS-HISTORY.md) |
| 2026-10-17 | Agent efficiency: smaller searches, a docs router, paperwork first | [docs/ci/STATUS-HISTORY.md](docs/ci/STATUS-HISTORY.md) |
| 2026-10-04 | A leader restart no longer ends progressive or Live TV playback | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-10-05 | Main-merge defects D1–D7 and the red gates: one batched PR | [docs/ci/STATUS-HISTORY.md](docs/ci/STATUS-HISTORY.md) |
| 2026-10-04 | The architecture effort is on `main`; the close-out is a draft PR | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-10-04 | Jellyfin facade: both pinned clients can now finish a play (J5) | [docs/clients/STATUS-HISTORY.md](docs/clients/STATUS-HISTORY.md) |
| 2026-10-04 | Apple TV: new HEVC WEB-DLs refused with 503 — ffmpeg 8 repeats the sample description | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-28 | Content analysis stopped: the queue's receipt bound is the next cliff after #608 | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-28 | Android Live TV fullscreen on tablets: every box gets its own player view | [docs/features/STATUS-HISTORY.md](docs/features/STATUS-HISTORY.md) |
| 2026-09-28 | Live TV said "all slots are busy" with every tuner idle | [docs/features/STATUS-HISTORY.md](docs/features/STATUS-HISTORY.md) |
| 2026-09-28 | Apple TV Live TV navigation: every press reversible, every control reachable | [docs/features/STATUS-HISTORY.md](docs/features/STATUS-HISTORY.md) |
| 2026-09-26 | Silo comparison: two implementation plans and one device census, no code | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-24 | Watch view: menus and Playback info escape the picture; the picture goes under the header | [docs/clients/STATUS-HISTORY.md](docs/clients/STATUS-HISTORY.md) |
| 2026-09-25 | P-02: four parser fuzz targets, one of which found a way to abort plurxd; the release profile measured | [docs/ci/STATUS-HISTORY.md](docs/ci/STATUS-HISTORY.md) |
| 2026-09-25 | P-02 M3: a priority class for every child process | [docs/ci/STATUS-HISTORY.md](docs/ci/STATUS-HISTORY.md) |
| 2026-09-24 | Live TV: direct play first, 5.1 stays 5.1 | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-25 | P-03: the regression ledger stops growing, and releases get a weekly cadence | [docs/ci/STATUS-HISTORY.md](docs/ci/STATUS-HISTORY.md) |
| 2026-09-22 | PGS subtitles stopped blocking the start path | [docs/clients/STATUS-HISTORY.md](docs/clients/STATUS-HISTORY.md) |
| 2026-09-22 | The full Rust suite and the release build are clean again | [docs/ci/STATUS-HISTORY.md](docs/ci/STATUS-HISTORY.md) |
| 2026-09-22 | Resume stopped working on every client — reproduced, half fixed | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-21 | An abandoned replacement held its player's key — reported, diagnosed, fixed | [docs/playback-control/STATUS-HISTORY.md](docs/playback-control/STATUS-HISTORY.md) |
| 2026-09-20 | Architecture review, revision 3 — Astra's review merged | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-20 | Implementation plans for the architecture review, and one work board for every vendor | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-20 | Architecture review, revision 2 — after the adversarial assessment | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-20 | End-to-end architecture review — ten verified do-first items, ranked | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-19 | The web app is a tree, and the bytes are the same ones | [docs/clients/STATUS-HISTORY.md](docs/clients/STATUS-HISTORY.md) |
| 2026-09-17 | The twenty red tests, and the three live defects three of them were reporting | [docs/ci/STATUS-HISTORY.md](docs/ci/STATUS-HISTORY.md) |
| 2026-09-17 | A held source is compared on its media facts, not its reporter's schema | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-17 | `make install` is one command on every platform | [docs/ci/STATUS-HISTORY.md](docs/ci/STATUS-HISTORY.md) |
| 2026-09-16 | The web item page is back to its pre-#317 layout | [docs/clients/STATUS-HISTORY.md](docs/clients/STATUS-HISTORY.md) |
| 2026-09-13 | Live TV start, stall, and tvOS surface has landed on main | [docs/features/STATUS-HISTORY.md](docs/features/STATUS-HISTORY.md) |
| 2026-09-13 | Apple Live TV now distinguishes a stall and owns its fullscreen surface | [docs/features/STATUS-HISTORY.md](docs/features/STATUS-HISTORY.md) |
| 2026-09-13 | Android now lets each HLS playlist choose its live hold-back | [docs/features/STATUS-HISTORY.md](docs/features/STATUS-HISTORY.md) |
| 2026-09-13 | Live TV starts now use a stable one-second cadence | [docs/features/STATUS-HISTORY.md](docs/features/STATUS-HISTORY.md) |
| 2026-09-13 | plurx records now, and tells you before a programme starts | [docs/features/STATUS-HISTORY.md](docs/features/STATUS-HISTORY.md) |
| 2026-09-13 | Apple's notice strip was a dead end, and two rows of the readiness card were false | [docs/playback-control/STATUS-HISTORY.md](docs/playback-control/STATUS-HISTORY.md) |
| 2026-09-13 | Every class in the playback surface contract can now be drawn on Apple | [docs/playback-control/STATUS-HISTORY.md](docs/playback-control/STATUS-HISTORY.md) |
| 2026-09-13 | The Android playback surface has no unreachable sources left | [docs/playback-control/STATUS-HISTORY.md](docs/playback-control/STATUS-HISTORY.md) |
| 2026-09-13 | The web half of the playback surface contract is reachable, and four rulings are closed | [docs/playback-control/STATUS-HISTORY.md](docs/playback-control/STATUS-HISTORY.md) |
| 2026-09-13 | `make web-check` is green, and `rust-gate` has actually been run | [docs/ci/STATUS-HISTORY.md](docs/ci/STATUS-HISTORY.md) |
| 2026-09-13 | The playback surface contract — built, both clients compiled, unverified on hardware | [docs/playback-control/STATUS-HISTORY.md](docs/playback-control/STATUS-HISTORY.md) |
| 2026-09-13 | Open rulings — the playback surface contract | [docs/playback-control/STATUS-HISTORY.md](docs/playback-control/STATUS-HISTORY.md) |
| 2026-09-13 | The player input fence was red on `main`, on two doc comments | [docs/playback-control/STATUS-HISTORY.md](docs/playback-control/STATUS-HISTORY.md) |
| 2026-09-13 | Live TV: the empty guide and the "wait 90 seconds" refusal, diagnosed | [docs/features/STATUS-HISTORY.md](docs/features/STATUS-HISTORY.md) |
| 2026-09-12 | Live TV gets the web page's proportions on Apple TV and iPhone | [docs/features/STATUS-HISTORY.md](docs/features/STATUS-HISTORY.md) |
| 2026-09-12 | Live TV gets the web page's proportions on Google TV and Android phones | [docs/features/STATUS-HISTORY.md](docs/features/STATUS-HISTORY.md) |
| 2026-09-09 | Fresh Dolby Vision recovery converts, and tvOS can arm Live TV starts | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-08 | The transport-recovery campaign asserted a property the system does not have | [docs/cluster/STATUS-HISTORY.md](docs/cluster/STATUS-HISTORY.md) |
| 2026-09-08 | Phase 3 is buildable today, and the first answer to that question was wrong | [docs/cluster/STATUS-HISTORY.md](docs/cluster/STATUS-HISTORY.md) |
| 2026-09-08 | The axis receipt said do not widen, and the fleet widened anyway — correctly | [docs/cluster/STATUS-HISTORY.md](docs/cluster/STATUS-HISTORY.md) |
| 2026-09-08 | A prepared commit hands the viewer a session that is refused from its first request | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-08 | The third client speaks the protocol, on a platform the server will not use it for | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-07 | Apple viewers were paying for encoders nobody told them about | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-07 | The CI fleet filled up because the bound was behind a flag nobody set | [docs/ci/STATUS-HISTORY.md](docs/ci/STATUS-HISTORY.md) |
| 2026-09-07 | Settings put the operator on the login page, and the cause was a tombstone | [docs/cluster/STATUS-HISTORY.md](docs/cluster/STATUS-HISTORY.md) |
| 2026-09-05 | A deploy that refused itself over an unmaintainable pair | [docs/ci/STATUS-HISTORY.md](docs/ci/STATUS-HISTORY.md) |
| 2026-09-04 | Streaming reliability is under end-to-end review and repair | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-03 | The fragment-index queue built nothing for three days | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-03 | Source attestation was hashing whole films to answer a question about their inode | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-03 | Dolby Vision Profile 7 on the web — what was actually left | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-03 | The ✕ on an iPhone could not leave a film | [docs/clients/STATUS-HISTORY.md](docs/clients/STATUS-HISTORY.md) |
| 2026-09-03 | Activity's Now playing row, read as a card | [docs/clients/STATUS-HISTORY.md](docs/clients/STATUS-HISTORY.md) |
| 2026-09-03 | Nothing played on the web, and every fallback was terminal | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-02 | The picker says which machine again | [docs/clients/STATUS-HISTORY.md](docs/clients/STATUS-HISTORY.md) |
| 2026-09-02 | The player input contract, reviewed and finished | [docs/playback-control/STATUS-HISTORY.md](docs/playback-control/STATUS-HISTORY.md) |
| 2026-09-02 | M7 M4 burn-join and current-main corrections | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-02 | M7 R-M3 — one playback owns one subtitle window | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-02 | Apple pacing-hold freeze — the hold that vetoed its own recovery | [docs/streaming/STATUS-HISTORY.md](docs/streaming/STATUS-HISTORY.md) |
| 2026-09-02 | Everything a read-only cluster member could not do | [docs/cluster/STATUS-HISTORY.md](docs/cluster/STATUS-HISTORY.md) |
| 2026-09-01 | Artwork repair-fence claim flake (same CI job) | [docs/ci/STATUS-HISTORY.md](docs/ci/STATUS-HISTORY.md) |
| 2026-09-01 | Exact-count cluster assertions (the `v0.3.0` release blocker) | [docs/cluster/STATUS-HISTORY.md](docs/cluster/STATUS-HISTORY.md) |
