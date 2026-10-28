# Worker controller — check the production Rust decision body

**Status:** implemented; Fable final review delivered and amendments implemented.
Full six-target/three-control qualification and landing are tracked in [PR #1260](http://forge.lan:3000/noirr/plurx/pulls/1260).

This package checks the ordinary `library_lifecycle::step` body in the
zero-dependency `plurx-worker-control` crate. The production library worker calls
that same symbol; the proof module imports it directly, without stubs, contracts,
assumed conclusions or a second transition implementation. The coordinator's
[correspondence](CORRESPONDENCE.md) records the reviewed driver/effect boundary.
The existing worker-refinement model and its historical receipts are separate.

The runner archives the complete production `lib.rs`, `library_lifecycle.rs`
and `proofs.rs` byte-for-byte into its durable output. Their hashes are checked
against committed inputs before and after analysis. A standalone manifest derives
the actual workspace edition (2021) and version and requires the original leaf
crate to have no dependencies or build script. Only this packaging envelope is
new: it avoids unrelated workspace dependency resolution. Its exact bytes/hash,
original manifest hashes, and every source hash are recorded. No Rust source is
rewritten. The ordinary core must have one `step` definition and no alternate
`cfg`/`cfg_attr`/`cfg!` body. The reviewed `cfg(kani)` proof module in `lib.rs`
adds harnesses around that shared body.

## Run after independent review

```bash
python3 scripts/check-worker-control full \
  --cache-dir target/worker-control-tools \
  --output-dir target/verification/worker-control-run --provision
```

Use Python 3.12 or newer. The output directory must be new. `--provision` permits
only the checksum-pinned official native archive and the exact scoped nightly;
omit it to require existing tools. Supported platforms are Linux x86_64 and
macOS arm64. Unsupported platforms and unavailable tools fail closed. The
[tool manifest](toolchain.json) pins Kani 0.68.0, CBMC 6.11.0 and
nightly-2026-08-21, with archive digests published on the official release.
The runner checks extracted native files against that archive before execution.
Rustup, Cargo, Kani and temporary state stay under the selected cache on every
command. No global installation or production feature switch is involved.

`inventory` in place of `full` compiles metadata and requires exactly the six
reviewed harnesses; it runs no CBMC proof and its receipt says it is not
acceptance. `full` runs all six targets plus all three semantic mutation controls.
No omitted check flag or inherited compiler flag can weaken that command. A timeout, missing/duplicate harness, compiler failure, unexpected
property status or unwind failure rejects qualification.

If a full run partially fails, preserve its output and resume into a new one:

```bash
python3 scripts/check-worker-control full \
  --cache-dir target/worker-control-tools \
  --output-dir target/verification/worker-control-retry \
  --resume-from target/verification/worker-control-run
```

Independent cases continue after a target failure. Resume retains a prior success
only after validating its log digest, exact command, full strict result parsing,
identical solver inputs and tool hashes, and the exact mutation where applicable.
Its original candidate identity stays in the result, with an explicit source
bridge if only runner/control metadata changed. An earlier parser rejection may be retained only after the repaired parser fully
accepts the original complete log, with original rejection and all bindings preserved.
Failed solver executions, missing or incompatible cases run again; full qualification still requires all six plus three. Retained
logs are copied byte-for-byte into the new artifact. Preserve the original
output as well, including archived control sources.

## The six claims

| Harness suffix | Exact scope |
|---|---|
| `t1_all_typed_inputs_total_deterministic` | Every typed state/event combination returns deterministically; exactly the specified event kinds are accepted, and accepted transitions strictly increase DAG rank |
| `t2_every_reachable_prefix_gates_process` | Every reachable effect prefix starts domain acquisition once; Process occurs at most once and only after acquired-domain and accepted-bind outcomes |
| `t3_every_prefix_retains_resources_terminal_cleanup_ordered` | Every reachable effect prefix retains local resources until their boundary; join precedes publisher drop, publisher drop precedes domain release attempt, local release completion precedes queue retirement, and admission drops exactly once and last on every terminal trace |
| `t4_refusal_interrupt_and_completion_settlement_policy` | Refusals yield after +5000 ms; domain loss/pressure yield after +1000 ms; failed nonyielded work retries and successful nonyielded work does not settle |
| `t5_all_i64_settlement_arithmetic_saturates` | Every signed 64-bit timestamp/delay uses exact signed saturation, independently checked with i128 arithmetic; nonnegative delay implies result >= now |
| `t6_all_typed_stale_pairs_and_terminal_reentry_rejected` | Every invalid typed state/event pair and all terminal re-entry reject without effects; refused bind cannot restart work |

The actual production timestamp and delay types are `i64`. Arbitrary negative
retry delays remain in the arithmetic theorem; monotonicity is conditional on
nonnegative delay. The production retry policy supplies positive delays, a
trusted driver fact rather than a proof assumption.

Accepted transitions form an acyclic protocol with depth six, including
one-shot initialization and explicit release completion. The symbolic trace
harnesses cover every accepted trace and inspect every emitted effect prefix.
Stale events return no effects and retain the caller's state, so inserting or
removing arbitrary stale stutters preserves these safety claims. Arbitrary input
streams need not terminate. Harness loops retain Kani's unwinding assertions;
the bound includes loop exit and all six fixed-capacity effects.

## Controls and receipts

[controls.json](controls.json) changes the production body in three isolated,
durable source copies: swap queue/admission cleanup order, emit Process after
refused bind, and replace +5000 saturation with wrapping addition. Each anchor
must occur exactly once. The original/mutated source hashes and unchanged proof
hash are recorded. A control passes only when its named theorem has the expected
semantic assertion counterexample; missing harnesses, compilation errors,
unwind errors and unrelated failures are rejected.

Each run preserves `receipt.json`, full inventory and tool logs, and per-target
logs/JSON results. Target results bind the committed governed sources, exact
native tool hashes, full command and log digest. `acceptance: true` requires all
six positives and all three controls, with unchanged sources at completion.
Ordinary Rust 1.97.1 compilation, production regressions, driver/effect review
and CI qualification are separate evidence and must be composed by the landing
coordinator; a Kani receipt does not stand in for them.

Entry requires an already confirmed claim and a retained physical admission
guard. Real guards, Store authority and asynchronous operations remain in the
driver. A domain-release event means the local release attempt completed,
including ambiguous failure; it never proves durable lease disappearance.
Accepted bind is an environmental outcome, not a proof of SQL guards. This
package proves local controller safety, with no SQL/Store correctness, Tokio/OS
fairness, external-operation termination, fair simulation or cluster-wide
progress/deadlock theorem. The existing model's durable source-I/O fairness gap
remains open.

## Review amendments and result interpretation

Fable task `t_e69efaca` approved the production extraction with proof/runner amendments. T4 additionally pins the exact interruption effect list; T3 requires every outcome-producing effect to end its list, matching the driver handoff. Positive qualification requires present theorem-labelled specification assertions to report `SUCCESS`. Only unmarked compiler panic/expect checks and the exact source-pinned panic-formatting guard may report `UNREACHABLE`; unexpected statuses or inconsistent summary counts reject the result. The pinned tool's actual positive output is preserved as a parser fixture with recorded provenance. A partial native parser-calibration run is not full package acceptance.

Harness specification assertions call `kani::assert` with literal labels. The pinned frontend loses custom messages from core formatting-based Rust assertion macros in this no-std package. Checked result guards avoid that formatting path without assuming transition success or substituting production behavior. The parser remains strict about unexpected unreachable safety checks.

The observed formatting exception requires the exact emitted property name, left-shift description, `library/core/src/fmt/mod.rs:820:38` location and SHA-256 of the actual scoped rust-src file. The real T2 fixture records that one unreachable compiler guard and five successful specification assertions. Wrong source/location, failure, undetermined output or an unreachable specification assertion rejects qualification. Reinterpreting the immutable calibration log does not run the solver again and does not establish full package acceptance.

The Linux T6 fixture additionally records a proved-unreachable generic Effect slice-equality loop increment at `library/core/src/slice/cmp.rs:136:13`. Its exact property, description, location and actual scoped source hash are required. The extra source attestation is separate from solver tool identity, so unchanged historical results retain their original tool records. All eight T6 specification assertions must still be checked `SUCCESS`.

The CI helper `validation.worker_control_resume` authenticates a prior same-PR artifact, including a failed proof attempt, and safely hydrates only the files needed by `--resume-from`. It preserves origin and ZIP hash in the flat output artifact. It does not grant proof acceptance: the runner independently checks every retained target and records the original candidate, rejection and current parser/source bridges.

CI reuse additionally compares actual Git bytes for the historical and current
runner, workflow and tool manifest. Its default requires identical digests.
[reviewed-invocations.json](reviewed-invocations.json) can record an exact
historical commit and three-digest tuple with a review basis; this is an explicit
review trust decision, not a solver theorem or authority inferred from a name.
The retained origin records the actual digests and decision. Local manual
`--resume-from` assumes the previous invocation and result producer are trusted;
per-target source, tool, output and assertion validation still applies.
