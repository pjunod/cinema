# Decoder selection and recovery — handoff

**Written:** 2026-09-08 · **By:** the Claude session that carried M3–M7b ·
**For:** whoever continues the effort · **Effort branch:**
`effort/decoder-selection-recovery`

This is a working handoff, not a summary. It assumes you have not read the
effort's history and gives you what you need to pick it up and finish it. The
two documents it sits beside are the plan,
[`DECODER_SELECTION_AND_RECOVERY_PLAN.md`](DECODER_SELECTION_AND_RECOVERY_PLAN.md),
and the execution ledger,
[`../DECODER_SELECTION_RECOVERY_STATUS.md`](../DECODER_SELECTION_RECOVERY_STATUS.md).
Read this first, then the ledger's most recent sections. The plan changes
rarely; the ledger is where the reasoning lives.

## 1. The one thing to understand before anything else

**The automatic decode recovery this effort built cannot fire on any node, and
that is a property of two code sites rather than a bug in either.**

- `DiagnosticObservation::resolve` (`crates/plurxd/src/transcode.rs`) refuses a
  diagnostic grammar to any plan that does not *name* its decoder. Without a
  grammar there is no fault sink, so no `ProducerDecodeFault` is ever latched
  and `action_qualified` is never set.
- `PrepublicationTranscodeRetry::prepare_decode_restricted` (same file) answers
  `Ok(None)` when the software alternate's plan digest equals the delivered
  plan's — which is exactly when the delivered plan already decodes in
  software. A software-decode plan has no software-decode alternate, because it
  would be the same command.

Until M7b, a hardware-decode plan could not be named at all, so: hardware
decode → no evidence; software decode → no alternate; no plan in between.
M7b slice 1 (merged) made a hardware contract *expressible*. What still does
not exist is a contract qualified against a hardware decoder on a real host,
and the inventory that measures one. **That is the effort's critical path.**

Everything else — the budget, the vocabulary, the two frozen recipes, the
client contract, the ledger, the tests — is built, reviewed and passing. It is
all downstream of that one missing input.

## 2. Where things stand

Merged into the effort branch, most recent first:

| PR | What |
|---|---|
| #167 | M7b specification — the hardware decoder contract, measured before written |
| #165 | Status roll-up for M5c3, M7a and the `main` merge |
| #164 | **`main` merged into the effort** — see §5 |
| #157 | M7a — the Settings → Developer enable section, and the finding in §1 |
| #156 | M5c3 — the durable recovery budget: reserve, settle, refuse a second |
| #152 | M5c3 slice 2 — `ProducerRecoveryLedger` |
| #149 | M5c3 slice 1 — the recovery identity down the start chain |
| #144, #145 | M5c3's specification and its refinement |
| #139, #135, #133, #124 | M5c2a–d — the decode-restricted alternate |
| #119, #114 | M5c1 — a qualified fault renames the verdict |
| #113 | The three M6 client build briefs (since moved to `docs/playback-control/`) |

**Open when this was written:** #168, M7b slice 1 (the backend joins the
contract's coverage key). Its checks are green locally; see §6 for the merge
rule.

M0–M5 are complete. M6's server half was already complete before this effort
touched it; all three client halves are on `main`, and only Apple's reaches a
viewer (Android's `dual_player_preparation` is deliberately `false`). M7 is
partly done — M7a and M7b slice 1 — and M8 has not started.

## 3. What is left, in the order to do it

### 3a. M7b slice 2 — make the inventory measure per `(codec, backend)`

The only remaining buildable step on the critical path. Fully specified in the
ledger's section *"M7b specification — the hardware decoder contract, measured
before it is written"*; the summary is:

`plurx_core::transcode::decoder_inventory::measure_selected_decoders` runs one
probe per codec with no hwaccel and reads the decoder name out of FFmpeg's own
`[vist#0:0/<codec> @ …] [dec:<name> @ …]` context. Make it run once per
advertised hwaccel as well, keyed by `(codec, backend)`.

**The rule that matters:** record a pair *only* when the probe's stderr carries

```
Selecting decoder '<name>' because of requested hwaccel method <backend>
```

for that exact backend. Do not use exit status, and do not infer the backend
from `pix_fmt`. FFmpeg is documented to fall back to software in some
configurations; a silent fallback recorded as a hardware measurement is
precisely how a software grammar gets qualified as a hardware one, and a
grammar that matches nothing certifies every stream as clean. Absent evidence
is an unmeasured pair, which every caller already handles.

Then `DiagnosticObservation::for_plan` reads the measured name for
`(input_codec, backend)` instead of `plan.decode().software_decoder()`, and
`artifact_qualification_readiness` in `crates/plurxd/src/transcode.rs` stops
passing `DecodeBackend::Software.name()` out loud (there is a comment there
marking exactly that spot).

Measured on the local Apple toolchain, FFmpeg 9.0.1, and the reason the backend
had to join the key at all:

| decode | context FFmpeg prints |
|---|---|
| h264 software | `[vist#0:0/h264 @ …] [dec:h264 @ …]` |
| h264 VideoToolbox | `[vist#0:0/h264 @ …] [dec:h264 @ …]` |

Identical. `hevc` behaves the same way.

### 3b. The hardware diagnostic contract — needs a qualifying host

Not buildable without hardware. A contract is a claim about a real binary on a
real backend: run the capture on a qualifying host, add a row to
`tests/playback/decoder-health/diagnostic-contracts.toml` with its
`decode_backend` set, and the recovery becomes reachable for that
(build, codec, backend) triple and no other.

`tests/web/settings-sections.test.js` asserts the literal sentence
`Not true on any node today` in the Settings → Developer card. That is a
tripwire, not a description: when the first hardware contract lands, that test
fails, and the card has to be corrected. Do not delete the assertion — correct
the card and move the assertion to whatever is then true.

### 3c. M7's remaining four items

From the plan §12.8, and **not yet surveyed** — treat these as unknown work
until you have read the code:

- **Job-scoped budget persistence.** The offline path
  (`crates/plurxd/src/offline.rs`) already fails rather than requeues on
  `HealthRefused`, which is half of the obligation the ledger records for
  `offline.prepare` / `offline.publish_ready`. The other half is a durable
  budget for offline jobs, analogous to `ProducerRecoveryLedger` but keyed by
  job rather than playback. Note it is downstream of §1 like everything else.
- **Alternate result references** and **part/assembly receipt verification**.
  Much of this landed in M3e/M3f; check before building.
- **Versioned worker capabilities.**
- **Owner handoff coverage.**
- The exit criterion *"Default and no-live-recovery builds both compile and
  exercise their shipping producer paths"* is partly free: `cargo check -p
  plurxd` with no features passes today. The *exercise* half does not exist.

### 3d. M8 — fleet qualification

Evidence against real hardware: the workload matrix, false-positive
classification, ordinary startup and concurrency, recovered-playback latency.
This is Paul's lab, not a coding task. The ledger's *"Remaining evidence before
release"* section is the list.

### 3e. The promotion PR

Freeze task merges, merge current `main` into the effort **again** (it moves
hourly), requalify on that exact tree, and follow the repository's Main
promotion gate. A passing effort gate does not make the feature releasable.

## 4. The constraints, verbatim

These came from the original commission and hold for the rest of the effort.

- **Forgejo only. Do not push to GitHub.** Run `git remote -v` before every
  push and verify the destination.
- **Do not use or modify `/Users/pjunod/code/plurx`** — that is Paul's own
  working clone. Make your own clone. This session used
  `/private/tmp/claude-plurx-decoder-m3`.
- **Do not print or copy credential contents** into logs, commits, archives,
  prompts, or cloud containers.
- **Do not put decoder features behind code feature gates.** The enable surface
  is a section in Settings → Developer stating prerequisites and whether each
  is met — advisory, never blocking the enable.
- **Keep the status document current**, but never commit a status-only update
  onto a head while that exact head is being qualified.
- **Establish the pinned compile loop before Rust edits. Do not use CI as a
  compiler.** `rustup run 1.97.1 cargo …`, always.
- Use `PLURX_EFFORT_COMMIT=1 git commit …` for effort task commits. Never use
  that override for an ordinary direct-to-`main` change.
- If Paul is absent for a decision, choose the best engineering option and
  record the assumption in the status document and the handoff.

## 5. The `main` merge, and why it will need doing again

`main` moves hourly and the effort merges it *before* opening into `main`.
#164 brought it forward deliberately rather than waiting for the promotion
gate, and doing so found a collision neither branch could see alone:

**Both branches had independently appended a v48 and a v49 to the SQLite
migration list, and a 28 and a 29 to the Hiqlite chain.** Both lists are
positional and append-only — the version *is* the index — so two branches
cannot both hold a version. `main`'s pair (the desired-selection row and the
pointer fence) kept 48/49 and 28/29 because they had already shipped; this
effort's producer-recovery ledger and recovery epoch became **v50/v51 and
30/31**. The Hiqlite migration sources moved with them, the v43 downgrade
fixture in `crates/plurx-core/src/store/sqlite/dv_conversion.rs` winds all four
back newest-first, and `DROPPED_BY_THE_FIXTURE` carries both parents' entries.

**Expect this again at promotion.** If `main` has added another migration since
#164, this effort's two move again. There is no way to avoid it and no reason
to fear it; just do not resolve such a conflict by taking one side.

The other half of #164: the M6 Android client had been merged into this effort
by mistake (#118, #129, #131, #134) and was later ported to `main` (#136,
#140), where reviewing the port as a *change* found three silent defects that
existed only in the effort's copy. So all seven Android files, the
wire-conformance test and `tests/client-fixes.toml` took `main`'s bytes
wholesale. **Do not resolve an Android conflict toward this branch.** The five
`client-fixes.toml` anchor rows for the effort's own Android commits are
appended back on top of `main`'s copy, because those commits are reachable from
this branch and `history-check` requires them — they are not for `main`.

## 6. How to work here

**Build.** `rustup run 1.97.1 cargo …` for everything. The repository pins
1.97.1 and the effort gate compiles with it.

**The gates, in the order they cost you time:**

```
rustup run 1.97.1 cargo fmt --all -- --check
make history-check          # ~50s
make validation-lint
python3 -m unittest discover -s tests/validation -p 'test_*.py'   # ~35s
make operations-check
make effort-rust-check      # fmt-check + spike-lock-check + cargo check --workspace --locked --all-targets
```

`make operations-check` has **ten pre-existing failures on macOS** —
`test_ci_cache` and `test_ci_janitor` — because bash 3.2 has no `mapfile`. They
fail identically on a clean checkout of `main`. Verify that before blaming
yourself for them.

**`make history-check` is the one that will surprise you.** A commit whose
subject contains words like *recovery*, *fix*, *missing* is classified as
corrective and needs an explicit mapping in
`validation/regressions.d/<shorthash>-<slug>.toml`. The mapping names the
commit, the affected `points` and `checks` from `validation/points.toml`, and a
`reason` explaining what retained test rejects the base implementation. The
error message tells you which points and checks it expects — read it rather
than guessing. Because the mapping names the commit's hash, add it in a
*following* commit, and if you later rebase, rename the file and update the
hash inside it.

**Effort-branch workflow.** Task branches PR into
`effort/decoder-selection-recovery`, never into `main`. The blocking check is
`Effort development gate`; full suites are deferred to the Main promotion gate,
so do not run `cargo test --workspace` on every slice — run the tests you
added, and the full suite once when a PR is ready. Keep a PR as a draft until
it is ready to merge; Forgejo has no draft flag, it is a `WIP:` title prefix,
and un-drafting is `PATCH /pulls/<n>` removing it.

**Forgejo API.** `/private/tmp/claude-runner/fj2` on Paul's Mac wraps it.
Paths start `/api/v1/repos/noirr/plurx`; a request body is a **file path**, not
inline JSON. Merge with `{"Do":"merge"}`. Retriggering CI: a close/reopen often
does not, an amended-and-force-pushed commit reliably does.

**The lab runner is contended and it will cost you.** Effort jobs share
`[self-hosted, Linux, X64, lab, general]` with whatever main-gate suite is
running. The `effort policy and contract preflight` job has a **3-minute**
budget and `make history-check` alone takes ~50s of it, so under contention it
times out on content that is perfectly green. Runners seen: `forgejo-runner`
and `forgejo-runner-02`. **Paul's rule: if a job fails for runner reasons and
the same check passes locally, merge the PR.**

## 7. Traps this effort has already fallen into

Each of these cost a review cycle. They are cheap to re-enter.

- **The name trap.** Clients declare the action `"prepare_replacement"`
  (`PREPARE_REPLACEMENT_ACTION`), but the wire tag is `"prepare"` — the enum
  carries `#[serde(tag = "type", rename_all = "snake_case")]`. `vocabulary_name()`
  is the mapping. Declaring and arriving are different strings for this one
  action only.
- **Two spellings of `videotoolbox`.** `DecodeBackend::name()` gives
  `videotoolbox`; serde gives `video_toolbox`. Only the first parses, and only
  the first is what FFmpeg takes for `-hwaccel`. The diagnostic contract's
  `decode_backend` uses `name()`.
- **`recovery_epoch_for(None)` is not pure** — it mints a fresh UUID. Calling
  it twice in one start gives a session and its row two different budgets. Mint
  once, bind to a local, read it twice. There is a source-text test that says
  so.
- **A source-text test counts itself.** `include_str!` includes the test
  module, so an occurrence count is off by however many times your assertion
  names the string. Split on `"\nmod tests {"` and count only the production
  half.
- **`cargo fmt` reflows your patch.** If a job runs `cargo fmt --all` before a
  python string-replacement, the replacement silently matches nothing. Format
  last, or re-read before patching.
- **Heredoc hazards.** Writing Rust through an outer python string turns `\n`
  into a real newline and breaks `\`-continuations inside Rust string literals.
  Write the Rust to a file with a quoted heredoc, then splice it.
- **`git commit --fixup`** produces a `fixup!` subject that trips the corrective
  classifier. Use `reset --soft` and a plain subject.
- **Do not relax a refusal because it blocks you.** Twice in this effort the
  obvious fix was to widen a match — the guessed decoder name in M3a, the
  software-only backend refusal in M7b — and both times the widening would have
  produced a grammar that matches nothing, which certifies every stream as
  clean and caches the result as reusable under the qualified artifact
  identity. Move the refusal to where the danger is; do not delete it.

## 8. Owed evidence

Recorded and not produced. Every item needs hardware this effort did not have.

- A diagnostic contract qualified against a **hardware** decoder — §3b, the
  critical path.
- FFmpeg 8.0.1 and 5.1.9 fleet diagnostic captures; an FFmpeg 8 banner
  re-capture.
- Original `#913` media on an Apple VideoToolbox node, software decode compared
  against hardware while retaining the hardware encoder.
- Pixel, metadata, startup, concurrency and recovered-playback-latency evidence
  for the fleet workload matrix.
- Web, Apple and Android prepare/readiness/commit/retirement runs with one
  replacement, preserved position, pause, tracks and grade, and no reopen loop.
- Exact-tree Main promotion qualification after current `main` is merged into
  the frozen effort branch.

Recorded, understood, and deliberately not fixed: the pointer tombstone, the
full-workspace gate wiring, the v14 fixture arithmetic, the `cluster::migration`
flake under load, empty-epoch observability, and the relayed-worker epoch gap.
The ledger's *"Adversarial review ledger"* and *"Decisions and deviations"*
sections carry the reasoning for each.
