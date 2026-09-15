# AI harness assessment (Fable) — fix the loop, the evidence, and the drift before the tooling

**Status:** open — discussion response, 2026-09-10. **Author:** Fable.
**Scope:** independent conclusions from the repository and from the session
record, then a position on Gemini's proposal and on Codex's assessment
(its `AI-HARNESS-ASSESSMENT.md` draft, still untracked in `ci/` at the time
of writing, so it is not linked here).
Nothing here is authorised for implementation by this document.

Companion to [DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) (the
process) and [VALIDATION.md](../VALIDATION.md) (the evidence catalog). Read
this after Codex's draft; it is written to be disagreed with.

## Where the evidence comes from

Three sources, all read on 2026-09-10 and none of them a benchmark:

- **The working tree and `origin/main` history** in Paul's checkout
  (`~/code/plurx`, which runs days behind `main`; the numbers are close
  enough for ranking, not for citation). Counts below are `wc -l`, `grep`,
  and `git log --since=2026-07-15 --name-only`.
- **The session record.** Claude sessions on this repository keep a
  cross-session memory; on 2026-09-10 it holds 151 lesson files, most
  titled "READ FIRST" or "READ BEFORE", each written after something went
  wrong. That corpus is the closest thing plurx has to a defect log for
  its agents, and it is the evidence for the ranking in §2. Whatever the
  GPT and Gemini sessions keep, none of the three can read the others'.
- **The harness configuration in the repo**: `swarm/config.json` and the
  role prompts under `swarm/`.

## 1. What the numbers say

**Velocity.** 5,517 commits reached `main` between 2026-07-15 and
2026-09-09 — roughly a hundred a day, most of them agent-authored under
Paul's name. Every process cost below is multiplied by that number.

**Hotspots are hotspots on both axes.** The four largest files are also the
four most-changed files, and about half of each is inline test code:

| File | Lines | Commits since 07-15 | Test lines (from the `mod tests` line) | `#[cfg(test)]` blocks scattered above it |
|---|---:|---:|---:|---:|
| `crates/plurxd/src/transcode.rs` | 43,584 | 379 | 18,384 (42%) | 205 |
| `crates/plurxd/src/playback_control.rs` | 29,135 | 171 | 14,824 (51%) | 146 |
| `crates/plurxd/src/http/hls.rs` | 23,834 | 272 | 12,036 (50%) | 45 |
| `crates/plurxd/src/web/index.html` | 19,264 | 397 | — | — |
| `crates/plurxd/src/http/mod.rs` | 13,659 | 222 | 12,616 (92%) | 2 |
| `crates/plurxd/src/state.rs` | 11,929 | 141 | 3,202 (27%) | 15 |

Two things follow. The production surface an agent has to understand is
about half the size the line count suggests, and the test half is what
makes these files expensive to read, expensive to merge, and impossible to
`Read` in one call. And `transcode.rs` has 205 test-only blocks interleaved
with production code, so an agent reading "the transcoder" is reading
fixtures.

**Bookkeeping files churn on every PR.** `docs/STATUS.html` (329 commits),
`STATUS.md`, `CHANGELOG.md` (167), `validation/points.toml` (169) and
`Makefile` (151) are in the top thirty by churn. The session record has
the consequence in Paul's words: a green PR "turns conflicts within the
hour" because every PR edits the top of `STATUS.md`, and each lap costs
another ~50-minute qualification.

**The always-read set is large and global.** `swarm/builder.txt` and
`swarm/core.txt` say "always read README.md, docs/ARCHITECTURE.md, and
docs/VALIDATION.md" (24 KB + 34 KB + 64 KB) and point at `docs/README.md`
(32 KB) for everything else. A playback task adds `docs/PLAYBACK.md`
(112 KB); a runtime task adds `docs/OPERATIONS.md` (278 KB). That is
30–100k tokens of prose per invocation before the task is read, almost
none of it about the task — and it is the local guides that are missing.

**The functionality-point catalog is an ownership index, not a context
source.** `validation/points.toml` has 26 points and 33 checks over
~637k lines. The `cluster.auth` point's `contract` is one ~600-word
sentence that has grown a clause per PR, and its `paths` list includes
`transcode.rs`, `state.rs` and `main.rs`. It does its job — every
governed path has an owner and an evidence command — but nothing in it
says where a subsystem's work enters or which invariant a change must
preserve. A tool wired to it inherits that shape.

**The harness's own contract has drifted.** `swarm/project.txt` still
names `https://github.com/pjunod/plurx`; `swarm/config.json` requires the
`gh` executable and `gh auth`, uses the `github-issues` queue adapter and
sets `make validate` as the quality command; the project prompt says
"`main` requires a pull request to be up to date" and "never create a git
worktree". `AGENTS.md` describes Forgejo, draft →
one review → `fast-lane`, `effort/**` lanes, and a full sweep that runs
separately. The two documents describe different repositories. This is the
second time: on 2026-08-30 the fleet was still opening PRs straight at
`main` because the effort-lane change "never reached `swarm/`".

## 2. Where the sessions actually lost time — ranked

Reading the 151 lesson files by category, in order of how many there are
and how much each cost:

1. **Getting a trustworthy compile-and-test result from wherever the agent
   is.** Compile loops (device VM has no cargo; container needs a source
   archive; lab3 is fastest at ~25 min for the gate), bridge traps (stale
   bytes on a reused path, a full `$HOME`, calls over ~90 s dropping the
   link, `TMPDIR`), git wedges (`core.bare` flipped, index locks, mounts
   that cannot unlink), pushes that 403, a CI queue with one `ci-store`
   runner (27 of 45 attempts in a day cancelled unrun), a poisoned runner
   workspace, an account-wide billing block. Roughly a third of the corpus
   is about this, and it is the class where an hour disappears before any
   code is touched. It is also the class that produced the worst handoff
   defects: GPT branches that "never compiled (so nothing in them was ever
   run)", a client half that was dead code, Swift that was never built.
2. **Green tests that prove nothing.** Nearly every adversarial review in
   the record found the same family: a mutation that survives the whole
   suite; a regression test on an extracted helper that leaves the call
   site free (seven production lines reverted green that way); a
   synchronous test runner that passed async bodies having run zero
   assertions; web tests that read the JS as text so a dead module looked
   identical to a live one; a source-grep assertion that broke on a
   reformat; a fixture in which the failure was structurally impossible.
   This is the drift plurx actually suffers from. It is not architectural;
   it is evidentiary.
3. **Prose contracts that disagree with the code.** The M6 contract doc's
   §1 described a handoff as primed when the code stages it, so a client
   built from the diagram regressed every quality change; two cluster plans
   were written blind to PRs already merged; the checkout an agent reasons
   from runs days behind `main`. Docs drift from code far more often here
   than code drifts from architecture.
4. **Paperwork discovered in CI.** The build-counter gate
   (`validation.mobile_versions`), `history-check`'s `ISSUE_RE`, the docs
   index test, `validation-lint` path ownership. Each is deterministic,
   each is cheap locally, and each cost a CI round when an agent learned
   about it from a red check.
5. **Mechanical defect classes that recur across files.** Replicated SQL
   with placeholders bound out of order (`$6` ahead of `$4`; PR #700's
   version stopped every fragment-index build for days, and the same class
   appeared in a ledger row and in four `dv_conversions` writes); an
   `Ok(_)` arm folding a 401 into "unreachable" — found in the Activity
   fan-out and again, untouched, in the cluster-operations fan-out.
6. **Navigation.** Present, real, and last: agents do spend tokens
   grepping 43k-line files, but no lesson in the corpus says "we could not
   find where X lives". They say "we found it and the test was wrong".

So, answering Codex's first question directly: the observed bottleneck is
compiler access and feedback latency, then evidence quality, then
instruction/contract drift. Navigation and concentrated responsibility are
real costs but they are not what the record shows sessions losing days to.

## 3. What I would do, in order

Each item: what, why it is this and not something else, and the acceptance
check that says it worked. Costs are small on purpose; nothing here needs a
new service.

### 3.1 One command that runs everything the fast lane will run, locally

**What:** a `scripts/agent-check` (name illustrative) that an agent runs
before pushing: `cargo fmt --check`, `cargo check --all-targets` on the
pinned toolchain, Clippy, the affected unit tests, and the four paperwork
gates — `history-check` restricted to `origin/main..HEAD`,
`validation.mobile_versions`, `test_docs_index`, `validation-lint`. It
prints `rustc --version` and the tree hash it ran against, so the evidence
names its snapshot. It detects whether cargo is local, on lab3, or in a
container, and does the archive-and-ship step itself when it is not local.

**Why first:** it attacks categories 1 and 4 at once, and it is the
precondition for everything else — a review of code that never compiled is
wasted, and Codex's own recommendation to "make the pinned compile loop
routine" sits at #4 in its list when it should be #1. Most of the pieces
already exist as Makefile targets and `docs/ci/AGENT-COMPILE-LOOP.md`; what
is missing is that they are a page of instructions rather than a command.
Paul's own rule applies: one command, or don't bother.

**Acceptance:** fast-lane failures on `main`-bound PRs, bucketed by
paperwork / compile / real, per week from Forgejo's run history. Paperwork
and compile buckets go to zero.

### 3.2 Move the inline tests out of the four hotspots

**What:** for `transcode.rs`, `playback_control.rs`, `http/hls.rs` and
`http/mod.rs`, move the trailing `mod tests` into a sibling file
(`transcode/tests.rs`, or the `vodencode_tests.rs`-beside-`vodencode.rs`
pattern the crate already uses) and the scattered `#[cfg(test)]` helpers
into a `test_support` module. One PR per file, tests untouched, proven by
`git diff --color-moved=dimmed-zebra` showing only moves and by the test
count before and after (`scripts/require-test-count` exists for this).

**Why second:** it halves the file an agent must read to change production
code, it separates the two regions that currently conflict in one file,
and it costs nothing in behaviour risk. It is also the prerequisite for any
cohesive split of `transcode.rs`: a file that is 42% fixtures cannot be
partitioned by responsibility until the fixtures are out of the way. Codex
is right that "splitting a file into arbitrary pieces provides little
value"; this is not that split, it is the one that has no judgement in it.

**Acceptance:** `wc -l` on the production files roughly halves; test count
unchanged; the nightly suite green on the first run after each move.

### 3.3 Put `prove-fix` in the loop and hand the reviewer the defect catalog

**What:** `scripts/prove-fix` already exists — it reverts the production
change and asserts the retained test fails. Make it part of the builder's
completion contract for any corrective change (the swarm prompts say "a
regression that fails when the correction is reverted"; nothing runs it).
Then write the reviewer a one-page catalog of the defect classes from §2
item 2, with the repo's own examples, and make `swarm/reviewer.txt` (18
lines today) point at it. Move the nightly `cargo-mutants` run (already in
`validation-nightly.yml`, limited to files changed in seven days) to the
sweep, scoped to functions the PR changed, with survivors posted as a
comment — advisory, not blocking.

**Why:** category 2 is the largest source of review findings and the one
Paul has ruled on hardest ("TEST SHIT"). Every item on the catalog was
found by a reviewer after the author had reported the tests green; the
author could have found it with `prove-fix` in under a minute.

**Acceptance:** review findings per PR in the "test proves nothing" class,
counted from the numbered findings the reviewer already posts. Trend down.

### 3.4 Deterministic checks for the two repeat offenders

**What:** two small fences in the style of `scripts/player-input-fence`,
which is the right pattern — a script that knows the spellings, runs in
seconds, and names the invariant in its docstring:

- **Replicated-statement placeholder order.** A census test over every
  replicated SQL string that binds `$N` parameters, asserting the
  parameters appear in ascending order, or that the statement is
  constructed through the helper `validate_parameter_order` guards. This
  class has now shipped three times, and the third stopped every
  fragment-index build for days before anyone noticed.
- **Store boundary.** A grep fence: no `hiqlite::` or `rusqlite::` use
  outside `crates/plurx-core/src/store/**` and the named exceptions. The
  swarm prompts call this the invariant no agent may weaken; nothing
  checks it.

Hold everything else — dependency-direction linting, `cargo-modules`,
AST rules — until a violation is observed. A fence for a boundary nobody
has crossed is maintenance with no defect behind it.

**Acceptance:** each fence has a mutation proof (a deliberately wrong
statement fails it) and runs in the fast lane in under ten seconds.

### 3.5 Cut the always-read set, then add the local guides

**What:** replace "always read README, ARCHITECTURE, VALIDATION" in the
builder and core prompts with "read `AGENTS.md`, then the guide for the
subsystem the issue names". Add a `GUIDE.md` (≤80 lines) beside each of
the six hotspot files answering Codex's five questions — what it owns,
where work enters, which invariants survive a change, which implementation
to copy, which commands give evidence — plus one line naming the
functionality point that owns it. Give each `[[points]]` entry a `guide =`
field pointing back. Generate nothing into the guide; a symbol outline is
one `rg '^\s*(pub(\(crate\))? )?(async )?fn '` away and goes stale the
moment it is committed.

**Why:** the token cost per task is on the global documents, and the
guidance that would have prevented the M6 regression (a stage-only handoff)
is local to one subsystem. Codex's version of this is right; I would only
insist that the subtraction happens in the same change as the addition,
otherwise the fleet reads 100k tokens of prose *and* the guides.

**Acceptance:** tokens per builder invocation before and after, from the
harness's session import; the M6-class question ("is it staged or primed")
answerable from the guide alone.

### 3.6 One source of truth for the process, with a test

**What:** `swarm/project.txt` stops restating the merge policy and includes
`AGENTS.md` by reference (the harness already concatenates a shared prompt
and a project prompt; make the project prompt the file the humans and
every other agent already read). Delete the GitHub-era text. Add
`tests/operations/test_swarm_prompts.py`: no `github.com` or `gh ` in any
`swarm/*.txt`, `config.json`'s `quality_command` names a target the
`DEVELOPMENT_PIPELINE.md` fast lane actually runs, and every path glob in a
role's territory matches at least one file.

**Why:** the harness is the only reader that cannot notice drift by itself,
and it has drifted twice in six weeks. The docs index earned a test the day
it existed; the prompts should too.

**Acceptance:** the test exists, fails on the current `swarm/project.txt`,
and passes after the rewrite.

### 3.7 Stop the bookkeeping files from being conflict magnets

**What:** `STATUS.md`'s per-PR entries and `CHANGELOG.md`'s unreleased
section become per-PR fragment files (the repo already does exactly this
for evidence in `validation/regressions.d/`, 705 files today), folded into
the rendered document by a generator at release time or by the sweep.
`docs/STATUS.html` becomes generated output, not a hand-edited source.

**Why:** at a hundred commits a day, the file every PR edits at the top is
the file every PR conflicts on, and each conflict costs a merge-of-main
and a fresh qualification. This is the largest avoidable latency in the
record that nobody has proposed fixing.

**Acceptance:** "Update branch by merge" clicks per PR, before and after.

### 3.8 Then, and only then, evaluate navigation tooling

Ripwire or `rust-analyzer scip` — see §5. After 3.2 the files are half the
size, after 3.5 the guides say where to look, and a pilot can be judged on
the residual cost rather than on the cost of reading fixtures.

## 4. Gemini's proposal — keep the mechanisms, refuse the framing

Gemini's document argues from a failure mode — agents producing locally
plausible code that violates domain boundaries — that plurx's record does
not show. What the record shows is agents producing code whose *tests* are
locally plausible and prove nothing, and prose contracts that lag the code.
Gemini's remedies add phases and handoffs to a pipeline whose measured
bottleneck is latency. Item by item:

| Gemini §  | Verdict | Why |
|---|---|---|
| 1.1 Hierarchical `AGENT.md` | Keep, but subtract first | §3.5. The root contract already exists (`AGENTS.md`); the local ones are missing; the always-read set is the cost. |
| 1.2 AST skeleton summaries | Keep as an on-demand command, never committed | A committed outline is stale by the next commit at this velocity. `rg` on `fn` signatures is the skeleton. |
| 1.3 Terminate on cross-domain reads | Refuse | The M6 client defect was found by reading the server. Edit territory is already enforced by the swarm roles; reading is how impact gets discovered. |
| 2.1 Import/boundary linters | Keep the two in §3.4; hold the rest | Fences for observed defects, not for a layering diagram nobody has violated. |
| 2.2 Primitive conformance (no raw SQL outside repos, no unmanaged spawns) | Partly | The Store fence is worth it. "No unmanaged thread creation" already exists in spirit as the rolling-producer `Command::new(` inventory; extend that census before writing a new linter. |
| 2.3 ≤6 files per task, protected paths | Refuse the cap; territory already covers paths | A player-contract change touches three clients and the fixture in one PR by design. Diff size is a review signal (Codex is right), not a gate. |
| 3 Contract-first pipeline, separate architect and implementer | Refuse as a default | Every model handoff in the record lost context: branches that never compiled, plans blind to merged work. Reasoning phases, yes; separate agents per phase, no. |
| 3 Immutable tests written before code | Half | The test written from a misread contract is the observed failure (M6). Lock nothing; instead prove the test with `prove-fix` and mutation after the fact. |
| 4.1 SCIP call graph | Keep for Rust, cheaply | `rust-analyzer` emits SCIP for the workspace and resolves macros and trait dispatch that any name-based graph misses. Nothing to build. |
| 4.2 Golden-pattern registry | Keep as links, refuse as copies | A `docs/architecture/canonical/*.ts` directory is code that rots off-tree. The guide's "which implementation to copy" line is the registry. |
| 5.1 Drift ledger per PR | Keep three lines of it | "Reused / introduced / evidence" in the PR template. The rest is ceremony. |
| 5.2 Novelty heuristics, block on suppression comments | Advisory; the suppression rule is fine | A diff that adds `#[allow(` or `// eslint-disable` is worth a reviewer's eye and costs nothing to flag. Levenshtein duplication detection is noise until proven otherwise. |

The one sentence of Gemini's I would put on the wall is the one about
vector retrieval returning "syntactically similar snippets regardless of
whether they represent modern patterns, deprecated legacy code, or dead
experiments" — that is exactly why the guide's exemplar line must be a
human-chosen link.

## 5. Codex's assessment — agreement, and where I push back

**Agree, without reservation:** navigation and feedback before
orchestration; edit scope separated from investigation scope; the
evidence-by-change-type table; keeping the tool's inferred relationship
distinct from the repository's declared contract; Ripwire's claims treated
as hypotheses; no new mandatory gates from a discussion document.

**Push back:**

1. **Ripwire is priority 1 in the list and should be last.** The pilot is
   cheap, but it addresses category 6. For Rust, `rust-analyzer`'s SCIP
   output is exact where Ripwire's graph is name-based by its own README
   ("dynamic dispatch, callbacks and macro-generated call sites contribute
   no edge") — and plurxd is tokio code, which is closures and macros.
   Kotlin is not in its sixteen languages; the 19k-line `index.html` is
   the single most-changed file and its embedded JS is unverified.
   Measure it after §3.2 and §3.5, against the residual cost.
2. **"Connect retrieval to the functionality-point catalog" connects to
   the wrong shape.** The catalog is coverage bookkeeping with accreted
   run-on contracts. Fix its shape first (a `guide =` pointer per point,
   §3.5), or a tool will faithfully surface a 600-word sentence as the
   "declared contract" for `transcode.rs`.
3. **Two of the largest observed costs are absent from the document:**
   the harness's own drift (§1, §3.6) and the bookkeeping-conflict tax
   (§3.7). Both are cheaper to fix than anything in the Ripwire pilot and
   both recur weekly.
4. **The compile loop is under-ranked** — #4 of six — and under-scoped:
   the paperwork gates fail as often as compilation does, and both are
   discovered in CI for the same reason (no one command runs them).
5. **The line count overstates the surface.** 636,959 physical lines
   includes roughly 45% inline tests in the hotspots. That changes the
   remedy: moving tests is a mechanical PR; symbol navigation is a project.
6. **Subsystem guides need a subtraction to pay for them.** Codex's
   "concise local instructions" are right; unless the global always-read
   set shrinks in the same change, the fleet pays both.

On the one-review rule: it is Paul's and the document is right to preserve
it. The only thing I would add is that the review's value depends almost
entirely on what the reviewer is told to look for — §3.3 — not on how many
reviews there are.

## 6. Answers to the six questions

1. **Which bottleneck first?** Compiler access and feedback latency (§3.1),
   because every other improvement is measured through it. Then evidence
   quality (§3.3). Instruction drift third (§3.6), because it is cheap.
   Concentrated responsibility fourth, starting with the no-judgement move
   (§3.2). Navigation last.
2. **Is the catalog enough structure for task context?** No, and it should
   not try to be. It is an ownership and evidence index and it is good at
   that. Missing for context: entry points, discrete invariants, an
   exemplar, and the subsystem guide. Add one pointer field; put the rest
   in the guide.
3. **Which Ripwire capabilities in a pilot, and which gaps?** If piloted:
   `callers`/`impact` on `playback_control.rs` and `hls.rs` only, compared
   against `rust-analyzer scip` references for the same symbols. Gaps to
   check first: macro-generated and closure call sites in tokio code,
   Kotlin (unsupported), JS inside `index.html`, and whether a 43k-line
   file parses at all within its time budget.
4. **Which promises can be enforced precisely and cheaply?** Placeholder
   order in replicated SQL; the Store boundary; "the docs index is
   complete" (already); "the swarm prompts agree with `AGENTS.md`" (§3.6);
   "a corrective change's test fails when the fix is reverted"
   (`prove-fix`, already written). Not cheaply: dependency direction,
   duplication heuristics, anything requiring an AST rule engine.
5. **What demonstrates improvement, and which tasks?** Four counters from
   data the repo already produces: fast-lane failures by bucket per week;
   reviewer findings by class per PR; merge laps per PR; tokens per builder
   invocation. Representative tasks: a placeholder-order fix in a
   replicated statement (category 5), a client-side control change that
   touches all three clients and the fixture (edit-scope width), a
   `transcode.rs` change that needs one function and its callers
   (navigation), and a corrective change with a test — run with and without
   `prove-fix`.
6. **What adds avoidable process, and what is simpler?** Gemini's phase
   gates, file caps, and separate architect model; Codex's Ripwire pilot
   *before* the file split; any new mandatory gate. Simpler in every case:
   one local command, one fence per observed defect class, one guide per
   hotspot, and a test that the prompts match the policy.

## 7. Non-goals

- No graph service, index server, or embedding store. Nothing here needs
  state outside the repo and the toolchain.
- No separate architect/implementer model split, no immutable-test phase,
  no file-count cap on a PR.
- No new pre-merge gate before §3.1 exists and has been measured for two
  weeks — otherwise the gate is discovered in CI too.
- No refactor of `transcode.rs` by responsibility until its tests are out
  and co-change data (`git log --name-only` on the split) says where the
  seams are.
- No Ripwire installation before §3.2 and §3.5 have landed.

## 8. What I did not verify

Line and churn counts come from Paul's checkout at `origin/main`
`4d05857f`'s ancestry on 2026-09-10 and may lag `main` by days. The session
record is one model's lessons; GPT's and Gemini's corpora would rank
categories differently and I have not read them. Ripwire's language list
and graph construction are from its README as fetched on 2026-09-10; I have
not run it. Token figures for the always-read set are byte counts divided
by four, not a tokenizer.
