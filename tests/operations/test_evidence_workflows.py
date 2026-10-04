from __future__ import annotations

from pathlib import Path
import os
import re
import tempfile
import unittest
import shlex
import subprocess
import importlib.util
from unittest import mock
from validation.rust_modules import module_source


ROOT = Path(__file__).resolve().parents[2]


class EvidenceWorkflowCase(unittest.TestCase):
    def read(self, path: str) -> str:
        return (ROOT / path).read_text(encoding="utf-8")

    def test_release_cost_measurement_is_serial_cold_bounded_and_not_ci_acceptance(self) -> None:
        script = self.read("scripts/p02-release-cost.py")
        for required in ('(("thin", 16), ("thin", 1), ("fat", 16), ("fat", 1))',
                         '"trial_seconds": 2700', '"total_seconds": 10800',
                         '"scratch": 20 * GIB', '"--memory-swap=24g"',
                         '"--cpus=8"', '"--pids-limit=1024"', '"--cap-drop=ALL"',
                         '"--security-opt=no-new-privileges"', 'shutil.rmtree(target)',
                         '"high-cpu-runner-host-not-workflow-job"',
                         '"CARGO_PROFILE_RELEASE_PANIC=unwind"',
                         '"CARGO_PROFILE_RELEASE_DEBUG=0"',
                         '"CARGO_PROFILE_RELEASE_STRIP=symbols"',
                         '"CARGO_PROFILE_RELEASE_OVERFLOW_CHECKS=false"',
                         '"--user=" + f"{os.getuid()}:{os.getgid()}"',
                         'data["Config"]["User"] == f"{os.getuid()}:{os.getgid()}"',
                         '"HOME=/tmp/home"', 'mkdir -p "$HOME"',
                         '"/source": (str(mounts[0]), False)',
                         '"/target": (str(mounts[1]), True)',
                         '"/cargo": (str(mounts[2]), True)',
                         '"archive_sha256": archive_sha', 'state["OOMKilled"]'):
            self.assertIn(required, script)
        self.assertNotIn('"--privileged"', script)
        self.assertNotIn('/var/run/docker.sock', script)
        self.assertNotIn('cargo test', script)
        # Dry description executes no compiler/Docker workload.
        result = subprocess.run(["python3", str(ROOT / "scripts/p02-release-cost.py")],
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('"execution": "not started"', result.stdout)
        # Invalid setup fails before Docker and still retains a compact receipt.
        with tempfile.TemporaryDirectory() as directory:
            result = subprocess.run([
                "python3", str(ROOT / "scripts/p02-release-cost.py"), "--run",
                "--source", "0" * 40, "--archive", directory + "/missing.tar",
                "--archive-sha", "0" * 64, "--image", "sha256:" + "0" * 64,
                "--receipts", directory + "/receipts"], capture_output=True, text=True)
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertEqual(len(list(Path(directory).glob("receipts/setup-failure-*.json"))), 1)
            self.assertNotIn("Traceback", result.stderr)

    def test_release_cost_running_guard_allows_own_load_but_refuses_pressure(self) -> None:
        spec = importlib.util.spec_from_file_location("release_cost", ROOT / "scripts/p02-release-cost.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        baseline = {"swap": 0, "restarts": 0}
        with tempfile.TemporaryDirectory() as directory, \
                mock.patch.object(module, "capacity") as capacity, \
                mock.patch.object(module, "inspect", return_value={"RestartCount": 0, "State": {"Health": {"Status": "healthy"}}}), \
                mock.patch.object(module.shutil, "disk_usage", return_value=mock.Mock(free=48 * module.GIB)), \
                mock.patch.object(module, "command", return_value="0 owned"), \
                mock.patch.object(module.urllib.request, "urlopen") as ready:
            ready.return_value.__enter__.return_value.status = 200
            capacity.return_value = {"available": 40 * module.GIB, "swap": 0, "load": 9}
            module.guard(Path(directory), baseline, "http://localhost/readyz")
            with self.assertRaisesRegex(RuntimeError, "load/swap"):
                module.guard(Path(directory), baseline, "http://localhost/readyz", initial=True)
            capacity.return_value["load"] = 13
            with self.assertRaisesRegex(RuntimeError, "load/swap"):
                module.guard(Path(directory), baseline, "http://localhost/readyz")

    def test_release_cost_effective_user_and_bind_roles_preserve_host_cleanup(self) -> None:
        spec = importlib.util.spec_from_file_location("release_cost", ROOT / "scripts/p02-release-cost.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        mounts = tuple(Path("/owned") / name for name in ("source", "target", "cargo"))
        data = {"Config": {"User": f"{os.getuid()}:{os.getgid()}"}, "HostConfig": {
            "NanoCpus": 8 * 10 ** 9, "Memory": 24 * module.GIB, "MemorySwap": 24 * module.GIB,
            "PidsLimit": 1024, "Privileged": False, "Devices": [], "NetworkMode": "none",
            "CapDrop": ["ALL"], "SecurityOpt": ["no-new-privileges"], "ReadonlyRootfs": True,
            "Tmpfs": {"/tmp": "rw,nosuid,nodev,size=1g"},
            "LogConfig": {"Type": "local", "Config": {"max-size": "10m", "max-file": "1", "compress": "false"}}},
            "Mounts": [{"Type": "bind", "Source": str(p), "Destination": "/" + name, "RW": name != "source"}
                       for p, name in zip(mounts, ("source", "target", "cargo"))]}
        module.validate_caps(data, mounts)
        data["Config"]["User"] = ""
        with self.assertRaises(AssertionError):
            module.validate_caps(data, mounts)
        data["Config"]["User"] = f"{os.getuid()}:{os.getgid()}"
        data["Mounts"][0]["RW"] = True
        with self.assertRaises(AssertionError):
            module.validate_caps(data, mounts)

    def test_release_cost_cooldown_is_passive_bounded_and_keeps_original_preflight(self) -> None:
        spec = importlib.util.spec_from_file_location("release_cost", ROOT / "scripts/p02-release-cost.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        clock = [0]
        def sleep(seconds):
            clock[0] += seconds
        receipt = {}
        with mock.patch.object(module.time, "monotonic", side_effect=lambda: clock[0]), \
                mock.patch.object(module.time, "sleep", side_effect=sleep), \
                mock.patch.object(module, "guard") as guard:
            guard.side_effect = [RuntimeError("host load/swap pressure crossed"), {}, {}]
            module.preflight(Path("/owned"), {}, "http://localhost/readyz", 10800, receipt, True)
            self.assertEqual(receipt["preflight_wait"]["seconds"], 2)
            self.assertEqual(guard.call_args.kwargs, {"initial": True})
            clock[0] = 0
            guard.side_effect = lambda *a, **k: (_ for _ in ()).throw(RuntimeError("load")) if k.get("initial") else {}
            with self.assertRaisesRegex(RuntimeError, "bounded cooldown"):
                module.preflight(Path("/owned"), {}, "http://localhost/readyz", 10800, receipt, True)
            self.assertEqual(receipt["preflight_wait"]["seconds"], 60)
            clock[0] = 0
            with self.assertRaisesRegex(RuntimeError, "bounded cooldown"):
                module.preflight(Path("/owned"), {}, "http://localhost/readyz", 10800, receipt, False)
            self.assertEqual(clock[0], 0)
    def test_manual_fuzz_only_keeps_all_five_campaigns_and_skips_runtime_sweeps(self) -> None:
        workflow = self.read(".github/workflows/validation-nightly.yml")
        inputs = workflow.split("    inputs:\n", 1)[1].split("\nenv:", 1)[0]
        selector = inputs.split("      fuzz_only:\n", 1)[1].split("      seed_pgs_crash:", 1)[0]
        self.assertIn("type: boolean", selector)
        self.assertIn("default: false", selector)
        self.assertNotIn("  schedule:", workflow)
        jobs = dict(re.findall(r"^  ([\w-]+):\n(.*?)(?=^  [\w-]+:\n|\Z)",
                               workflow.split("\njobs:\n", 1)[1], re.M | re.S))
        self.assertEqual(set(jobs), {"deep-validation", "pgs-fuzz", "parser-fuzz",
                                     "ffmpeg8-pacing", "mutation"})
        for name in ("deep-validation", "ffmpeg8-pacing", "mutation"):
            self.assertIn("    if: ${{ !inputs.fuzz_only }}\n", jobs[name])
        for name in ("pgs-fuzz", "parser-fuzz"):
            self.assertNotRegex(jobs[name], r"(?m)^    (if|needs):", name)
            self.assertNotIn("inputs.fuzz_only", jobs[name])
        self.assertIn("target: [fmp4_reader, rpu_rewrite, nfo_parse, epub_facts]",
                      jobs["parser-fuzz"])
        self.assertIn('echo "corpus_before=$before" >> "$GITHUB_OUTPUT"', jobs["pgs-fuzz"])
        self.assertIn("find fuzz/corpus/inspect_sup -type f", jobs["pgs-fuzz"])
        self.assertIn('"${{ steps.pgs_fuzz.outputs.corpus_before }}"', jobs["pgs-fuzz"])

    def test_fuzz_summary_records_growth_and_refuses_an_unexecuted_clean_receipt(self) -> None:
        # Run the summary code on disposable corpora/logs, never a fuzzer.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "scripts").mkdir()
            script = root / "scripts/fuzz-campaign"
            script.write_text(self.read("scripts/fuzz-campaign"), encoding="utf-8")
            corpus = root / "fuzz/corpus/inspect_sup"
            corpus.mkdir(parents=True)
            for name in ("seed", "new-edge"):
                (corpus / name).write_bytes(b"seed")
            log = root / "campaign.log"
            summary = root / "summary.md"
            env = {**os.environ, "GITHUB_STEP_SUMMARY": str(summary)}
            command = ["bash", str(script), "--summarize", "inspect_sup", str(log)]
            for line in ("stat::number_of_executed_units: 123", "Done 123 runs"):
                log.write_text(line + "\n", encoding="utf-8")
                result = subprocess.run(command + ["0", "1"], env=env,
                                        capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("| `inspect_sup` | 123 | 1 → 2 files", result.stdout)
                self.assertIn("budget spent, no finding", result.stdout)
            log.write_text("#99 crash found\n", encoding="utf-8")
            result = subprocess.run(command + ["1", "1"], env=env,
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("| `inspect_sup` | 99 | 1 → 2 files", result.stdout)
            self.assertIn("finding (exit 1)", result.stdout)
            before = summary.read_text(encoding="utf-8")
            log.write_text("compilation failed before fuzzing\n", encoding="utf-8")
            result = subprocess.run(command + ["0", "1"], env=env,
                                    capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(summary.read_text(encoding="utf-8"), before)
            result = subprocess.run(command + ["0", ""], env=env,
                                    capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)

    def test_vod_restart_checks_reuse_workspace_features_and_stay_exact_serial(self) -> None:
        # Selecting just plurxd changes Cargo's dependency feature union and
        # recompiles the daemon after the workspace suite has already passed.
        # Dry-run the real recipe without starting FFmpeg or any Rust tests.
        result = subprocess.run(
            ["make", "-n", "vodencode-restart-check", "CARGO=unit-proof"],
            cwd=ROOT, check=True, capture_output=True, text=True,
        )
        commands = [
            shlex.split(line) for line in result.stdout.replace("\\\n", " ").splitlines()
            if line.startswith("unit-proof test ")
        ]
        selectors = [
            "vodserve::tests::encoded_vod_vfr_input_is_sampled_on_the_declared_rational_grid",
            "vodserve::tests::encoded_vod_bitmap_burn_restores_cues_that_predate_video_seek_landing",
        ]
        self.assertEqual(len(commands), len(selectors))
        for command, selector in zip(commands, selectors):
            self.assertIn("--locked", command)
            self.assertIn("--workspace", command)
            self.assertEqual(command[command.index("--exclude") + 1], "plurx-cluster-check")
            self.assertNotIn("-p", command)
            self.assertNotIn("--bin", command)
            self.assertEqual(command[-5:], [selector, "--", "--exact", "--ignored", "--test-threads=1"])

    def test_runtime_sweeps_do_not_trigger_on_prs_or_main_pushes(self) -> None:
        for name in ("ci", "effort-ci", "lint", "cluster-store-backstop",
                     "release-readiness", "validation-nightly"):
            workflow = self.read(f".github/workflows/{name}.yml")
            triggers = workflow.split("on:\n", 1)[1].split("\njobs:", 1)[0]
            self.assertIn("workflow_dispatch:", triggers)
            self.assertNotIn("  pull_request:", triggers)
            self.assertNotIn("branches: [main]", triggers)
            if name == "release-readiness":
                # The one scheduled runtime sweep, and only once a week: Paul
                # chose weekly release tags cut from a green scheduled run on
                # 2026-09-23 (docs/RELEASING.md "The weekly release tag").
                self.assertEqual(triggers.count("- cron:"), 1)
                self.assertIn('  schedule:\n    - cron: "0 6 * * 1"\n', triggers)
                continue
            self.assertNotIn("  schedule:", triggers)
        fast = self.read(".github/workflows/main-fast-lane.yml")
        self.assertIn(
            "types: [opened, synchronize, reopened, ready_for_review, "
            "converted_to_draft]",
            fast,
        )
        self.assertIn("github.event.pull_request.draft == false", fast)
        # The lane is not opt-in. It was `draft == false` AND a `fast-lane`
        # label, and a PR could be marked ready, reviewed and merged without
        # anyone applying it - which is how three lanes merged in one day with
        # this workflow never having run, the third of them carrying a fix for
        # what the first had shipped to the fleet. Draft is the only gate now,
        # so an unlabelled PR cannot be a gateless one.
        self.assertNotIn("labels.*.name", fast)
        self.assertNotIn("'fast-lane'", fast)
        # Ready is what starts it and draft is what stops it, so neither state
        # change depends on a human remembering a label.
        self.assertIn("ready_for_review", fast)
        self.assertIn("converted_to_draft", fast)

    def test_no_document_still_tells_a_reader_to_apply_the_lane_label(self) -> None:
        """The prose has to move with the workflow.

        `test_runtime_sweeps_do_not_trigger_on_prs_or_main_pushes` pins the
        gate itself, which is the part CI enforces. Nothing pinned the
        sentences that tell a person - or an agent reading a handoff - what to
        do, and those are what the fleet actually follows. A document that
        still says "mark ready, then apply `fast-lane`" sends the next reader
        looking for a control that is not there, and a plan document that
        still describes the trigger as `labeled` teaches the wrong shape to
        whoever builds against it next.

        Records of what happened are not instructions and are left alone: a
        status page saying PR #259 took the label on 2026-09-10 is evidence,
        and rewriting evidence to match today is how a repository starts
        lying about its own history. Only the imperative forms are refused.
        """
        instructions = (
            "apply `fast-lane`",
            "apply the `fast-lane`",
            "takes `fast-lane`",
            "take `fast-lane`",
            "then `fast-lane`",
            "→ `fast-lane` →",
            "`pull_request` `labeled` with `fast-lane`",
            "remove the label before",
        )
        # `swarm/` and AGENTS.md carry none of these today, and they are in
        # scope precisely so they still carry none tomorrow: the fleet reads
        # its role prompts, not this repository's docs, and the harness plan
        # records two separate occasions when a process change reached docs/
        # and never reached swarm/.
        candidates = [
            *ROOT.glob("docs/**/*.md"),
            *ROOT.glob("swarm/*.txt"),
            ROOT / "AGENTS.md",
        ]
        offenders = []
        for path in sorted(candidates):
            if "/archive/" in path.as_posix():
                continue
            text = path.read_text(encoding="utf-8").lower()
            for phrase in instructions:
                if phrase.lower() in text:
                    offenders.append(
                        f"{path.relative_to(ROOT)}: {phrase}"
                    )
        self.assertEqual(
            offenders,
            [],
            "the fast lane runs on every ready PR and has no label; these "
            "documents still instruct a reader to apply one",
        )

    def test_fix_evidence_is_manual_and_report_only(self) -> None:
        workflow = self.read(".github/workflows/fix-evidence.yml")
        self.assertIn("workflow_dispatch:", workflow)
        self.assertNotIn("  pull_request:", workflow)
        self.assertIn("inputs.base_sha", workflow)
        self.assertIn("scripts/prove-fix", workflow)
        self.assertIn("continue-on-error: true", workflow)
        self.assertIn("This is report-only", workflow)
        self.assertIn("timeout-minutes: 60", workflow)

    def test_nightly_mutation_scope_is_bounded_and_artifacted(self) -> None:
        workflow = self.read(".github/workflows/validation-nightly.yml")
        self.assertIn("git log --since=7.days --name-only", workflow)
        self.assertIn("cargo mutants --timeout 300", workflow)
        self.assertIn("--file", workflow)
        self.assertIn("timeout-minutes: 60", workflow)
        self.assertIn("continue-on-error: true", workflow)
        self.assertIn("target/mutants", workflow)
        self.assertIn("GITHUB_STEP_SUMMARY", workflow)
        self.assertIn("name: report-only weekly mutation spot-check", workflow)
        self.assertIn("name: nightly-mutation-evidence", workflow)

    def test_pgs_fuzzer_is_bounded_seeded_artifacted_and_gating(self) -> None:
        workflow = self.read(".github/workflows/validation-nightly.yml")
        self.assertIn(
            "cargo +nightly-2026-08-01 fuzz run inspect_sup fuzz/corpus/inspect_sup",
            workflow,
        )
        self.assertIn("-max_total_time=900", workflow)
        self.assertIn("fuzz/artifacts", workflow)
        self.assertIn("steps.pgs_fuzz.outcome == 'failure'", workflow)
        self.assertIn("name: bounded PGS parser fuzz campaign", workflow)
        self.assertIn("name: nightly-pgs-fuzz-evidence", workflow)
        self.assertIn("target/validation/pgs-fuzz.log", workflow)
        self.assertIn("seed_pgs_crash:", workflow)
        self.assertIn("PLURX_FUZZ_SEEDED_CRASH", workflow)
        self.assertIn(
            'seeded_crash_enabled(std::env::var_os("PLURX_FUZZ_SEEDED_CRASH").as_deref())',
            self.read("fuzz/fuzz_targets/inspect_sup.rs"),
        )
        self.assertIn(
            "only_the_explicit_seed_enables_the_crash_proof",
            self.read("fuzz/src/lib.rs"),
        )
        self.assertIn("test --manifest-path fuzz/Cargo.toml --lib", workflow)
        self.assertIn("exit 1", workflow)
        self.assertTrue((ROOT / "fuzz/corpus/inspect_sup/minimal-header").is_file())

    def test_parser_fuzzers_are_bounded_seeded_artifacted_and_gating(self) -> None:
        """P-02 M8: the four parser seams run nightly on the PGS campaign's terms.

        Each target is a `[[bin]]` in fuzz/parsers/, a cargo-fuzz package of
        its own with a seed corpus committed beside it; the matrix job runs
        every one on the same bounded budget, uploads its log and artefacts
        whatever happens, and fails from the recorded outcome; and the campaign
        script writes executions and corpus growth into each job's step
        summary, so the run's summary page lists all five campaigns. The PGS
        package in fuzz/ must not depend on plurx-core: the PGS job builds it
        under AddressSanitizer inside a 20-minute step budget, and compiling
        plurx-core there would spend that budget on the parser targets' crate.
        """
        targets = ("fmp4_reader", "rpu_rewrite", "nfo_parse", "epub_facts")
        manifest = self.read("fuzz/parsers/Cargo.toml")
        workflow = self.read(".github/workflows/validation-nightly.yml")
        campaign = self.read("scripts/fuzz-campaign")
        seeds = self.read("scripts/fuzz-seeds")

        self.assertNotIn("plurx-core", self.read("fuzz/Cargo.toml"))
        self.assertIn('name = "plurx-parser-fuzz"', manifest)
        self.assertIn("cargo-fuzz = true", manifest)
        self.assertIn('plurx-core = { path = "../../crates/plurx-core" }', manifest)
        self.assertIn('dolby_vision = { path = "../../vendor/dolby_vision" }', manifest)
        self.assertTrue((ROOT / "fuzz/parsers/Cargo.lock").is_file())
        self.assertEqual(
            self.read("fuzz/parsers/rust-toolchain.toml"),
            self.read("fuzz/rust-toolchain.toml"),
        )
        for target in targets:
            self.assertIn(f'name = "{target}"', manifest)
            self.assertIn(f'path = "fuzz_targets/{target}.rs"', manifest)
            self.assertFalse((ROOT / f"fuzz/fuzz_targets/{target}.rs").exists())
            source = self.read(f"fuzz/parsers/fuzz_targets/{target}.rs")
            self.assertIn("#![no_main]", source)
            self.assertIn("fuzz_target!", source)
            self.assertIn("MAX_FUZZ_BYTES", source, f"{target} has no input bound")
            corpus = ROOT / "fuzz/parsers/corpus" / target
            self.assertTrue(
                any(path.is_file() for path in corpus.iterdir()) if corpus.is_dir() else False,
                f"fuzz/parsers/corpus/{target} has no seeds; run scripts/fuzz-seeds",
            )
            self.assertIn(target, seeds)
        self.assertIn('CORPUS="$ROOT/fuzz/parsers/corpus"', seeds)
        # Sliced threads would make the fMP4 seeds follow the host's CPU count.
        self.assertIn("-threads 1", seeds)

        # The filesystem seam reads only its own tempfile (assessment F-build-13).
        epub = self.read("fuzz/parsers/fuzz_targets/epub_facts.rs")
        self.assertIn("tempfile::NamedTempFile", epub)
        self.assertIn("read_epub_facts(input.path())", epub)

        parser = workflow.split("\n  parser-fuzz:\n", 1)[1].split("\n  ffmpeg8-pacing:\n", 1)[0]
        self.assertIn("target: [fmp4_reader, rpu_rewrite, nfo_parse, epub_facts]", parser)
        self.assertIn("fail-fast: false", parser)
        self.assertIn("fuzz/parsers -> target", parser)
        self.assertIn("scripts/fuzz-seeds --check", parser)
        self.assertIn("scripts/fuzz-campaign ${{ matrix.target }} 900", parser)
        self.assertIn("continue-on-error: true", parser)
        self.assertIn("steps.campaign.outcome == 'failure'", parser)
        self.assertIn("fuzz/parsers/artifacts", parser)
        self.assertIn("target/validation/fuzz-${{ matrix.target }}.log", parser)
        self.assertIn("name: nightly-fuzz-${{ matrix.target }}-evidence", parser)
        self.assertIn("exit 1", parser)
        self.assertNotIn("needs:", parser)

        # Five campaigns on one summary page: the PGS job reports through the
        # same script with its recorded outcome, and the script records
        # executions and corpus growth.
        self.assertIn(
            "scripts/fuzz-campaign --summarize inspect_sup target/validation/pgs-fuzz.log "
            "${{ steps.pgs_fuzz.outcome == 'failure' && 1 || 0 }}",
            workflow,
        )
        self.assertIn(
            'fuzz run --fuzz-dir fuzz/parsers "$TARGET" "fuzz/parsers/corpus/$TARGET"',
            campaign,
        )
        self.assertIn("fuzz/parsers/artifacts", campaign)
        self.assertIn("-max_total_time=", campaign)
        self.assertIn("-print_final_stats=1", campaign)
        self.assertIn("stat::number_of_executed_units", campaign)
        self.assertIn("Done [0-9]+ runs", campaign)
        self.assertIn('STATUS="${PIPESTATUS[0]}"', campaign)
        self.assertIn("GITHUB_STEP_SUMMARY", campaign)
        self.assertIn("| Target | Executions | Corpus before → after | Outcome |", campaign)

    def test_nightly_deep_fuzz_and_mutation_jobs_are_independent(self) -> None:
        workflow = self.read(".github/workflows/validation-nightly.yml")
        deep = workflow.split("\n  deep-validation:\n", 1)[1].split("\n  pgs-fuzz:\n", 1)[0]
        fuzz = workflow.split("\n  pgs-fuzz:\n", 1)[1].split("\n  parser-fuzz:\n", 1)[0]
        parser = workflow.split("\n  parser-fuzz:\n", 1)[1].split("\n  mutation:\n", 1)[0]
        mutation = workflow.split("\n  mutation:\n", 1)[1]

        self.assertIn("run: make validate-nightly", deep)
        self.assertNotIn("cargo-fuzz", deep)
        self.assertNotIn("cargo-mutants", deep)
        self.assertIn("cargo-fuzz", fuzz)
        self.assertNotIn("needs:", fuzz)
        self.assertIn("cargo-fuzz", parser)
        self.assertNotIn("needs:", parser)
        self.assertIn("cargo-mutants", mutation)
        self.assertNotIn("needs:", mutation)

    def test_nightly_runs_and_retains_the_auto_cliff_acceptance(self) -> None:
        makefile = self.read("Makefile")
        points = self.read("validation/points.toml")
        workflow = self.read(".github/workflows/validation-nightly.yml")
        action = self.read(".github/actions/playwright/action.yml")

        self.assertIn("playback-stall-recovery:", makefile)
        self.assertIn("--suite stall-recovery", makefile)
        self.assertIn("--network-profile 8mbps-to-1.5mbps", makefile)
        self.assertIn("target/playback-lab/acceptance/auto-cliff.json", makefile)
        self.assertIn('id = "playback-auto-cliff"', points)
        auto = points.split('id = "playback-auto-cliff"', 1)[1].split("[[checks]]", 1)[0]
        self.assertIn('profiles = ["nightly"]', auto)
        self.assertIn('missing = "fail"', auto)
        self.assertIn("make playback-stall-recovery", auto)
        self.assertIn("target/playback-lab", workflow)
        self.assertIn("PLURX_PLAYBACK_CHROME=$chromium", action)

    def test_pgs_periodic_refresh_and_completion_prune_remain_wired(self) -> None:
        apple = self.read("clients/apple/Sources/PlayerController.swift")
        server = self.read("crates/plurxd/src/pgs_overlay.rs")
        self.assertIn("PGSOverlayPolicy.periodicRefreshPosition", apple)
        # The observer hands its position to the overlay step XCTest drives,
        # and that step asks for a tick refresh, never a forced one.
        self.assertIn("self.pgsOverlayPeriodicTick(currentMs: self.currentMs)", apple)
        self.assertIn("refreshPGSOverlayWindow(at: overlayPosition, reason: .tick)", apple)
        self.assertEqual(server.count("prune(&root).await;"), 1)

    def test_media_origin_and_contract_routing_remain_wired(self) -> None:
        android = self.read("clients/android/app/src/main/java/tv/plurx/app/player/Controller.kt")
        android_builder = self.read(
            "clients/android/app/src/main/java/tv/plurx/app/player/PlurxPlayerBuilder.kt"
        )
        android_screen = self.read(
            "clients/android/app/src/main/java/tv/plurx/app/player/PlayerScreen.kt"
        )
        android_adapter = self.read(
            "clients/android/app/src/main/java/tv/plurx/app/player/PlayerKeyAdapter.kt"
        )
        android_policy = self.read(
            "clients/android/app/src/main/java/tv/plurx/app/player/PlayerInputPolicy.kt"
        )
        apple = self.read("clients/apple/Sources/PlayerController.swift")
        apple_view = self.read("clients/apple/Sources/PlayerView.swift")
        apple_adapter = self.read("clients/apple/Sources/PlayerRemoteAdapter.swift")
        apple_policy = self.read("clients/apple/Sources/PlayerInputRouting.swift")
        hls = module_source("crates/plurxd/src/http/hls.rs")

        self.assertIn("return realMediaPositionMs(", android)
        self.assertIn("val timeline = sessionPlaybackTimeline(hls, requestedStartMs = ms)", android)
        pipeline = android.split("private fun buildPipeline(", 1)[1].split("\n}\n", 1)[0]
        self.assertIn("val autoTransfers = AutoTransferEvidence(progressiveMediaOrigin)", pipeline)
        self.assertIn("transferListener = autoTransfers,", pipeline)
        self.assertIn("return BuiltPlayer(player, progressiveMediaOrigin, autoTransfers)", pipeline)
        self.assertIn("autoTransfers.complete(loadEventInfo, mediaLoadData)", pipeline)
        self.assertIn("mediaLoadData.dataType == C.DATA_TYPE_MEDIA", pipeline)
        self.assertIn("autoTransfers.discard(loadEventInfo.uri.toString())", pipeline)
        composite = android.split("internal class AutoTransferEvidence(", 1)[1].split("class BuiltPlayer", 1)[0]
        for callback in ("onTransferInitializing", "onTransferStart", "onBytesTransferred", "onTransferEnd"):
            with self.subTest(callback=callback):
                self.assertIn(f"delegate.{callback}(source, dataSpec, isNetwork", composite)
        self.assertIn("sample.bodyBytes != load.bytesLoaded", composite)
        self.assertIn("dataSource.setTransferListener(transferListener)", android_builder)
        self.assertIn(".playerInputAdapter(", android_screen)
        self.assertIn("PlayerInputPolicy.route(surface, state(), input)", android_adapter)
        self.assertIn("PlayerInputState.Hidden ->", android_policy)
        self.assertNotIn("HiddenSeekAccumulator", android_screen)
        self.assertIn("nextBaseMs = Self.sessionMediaOriginMs(hls, requestedStartMs: startMs)", apple)

        self.assertIn(".playerRemoteAdapter(", apple_view)
        self.assertIn("PlayerInputRouting.route(", apple_adapter)
        hidden = apple_policy.split("case .hidden:", 1)[1].split("case .transport:", 1)[0]
        self.assertIn("case .left, .right, .up, .down, .select, .tapSurface: .reveal", hidden)
        self.assertNotIn("controller.skip", hidden)
        self.assertIn("skipped Apple HEVC tier normalization", hls)

    def test_prepared_switches_preserve_authority_and_real_frame_evidence(self) -> None:
        apple = self.read("clients/apple/Sources/PlayerController.swift")
        apple_commit = apple.split("func commitPreparedSuccessor(", 1)[1].split(
            "private func awaitPreparedFirstFrame", 1
        )[0]
        self.assertNotIn("release(session:", apple_commit)
        self.assertIn("guard let firstFrameUnixMs", apple_commit)

        android = self.read(
            "clients/android/app/src/main/java/tv/plurx/app/player/Controller.kt"
        )
        android_poll = android.split("private fun pollPreparedReplacement()", 1)[1].split(
            "private fun commitPreparedReplacement", 1
        )[0]
        self.assertIn("parkSuccessor(hold.park(monotonicNowMs(), realPosition()), originMs, successor)", android_poll)
        self.assertIn("successor.seekTo(successorAttachPositionMs", android_poll)
        self.assertIn("val restored = rollbackSwitchedReplacement()", android_poll)
        self.assertIn("failSwitchedReplacement()", android_poll)
        self.assertIn('restartAt(realPosition(), "prepared successor rendered no frame")', android_poll)
        self.assertNotIn("settleCommitOnFirstFrame(System.currentTimeMillis())", android_poll)
        self.assertEqual(
            android.count("settleCommitOnFirstFrame(System.currentTimeMillis())"),
            1,
            "only Media3's real rendered-frame callback may publish a commit",
        )
        collect = android.split("fun collectRetiredPlayer()", 1)[1].split(
            "private var pendingAcknowledgement", 1
        )[0]
        self.assertIn("awaitingCommitFrameSinceMs != null", collect)
        rollback = android.split("private fun rollbackSwitchedReplacement()", 1)[1].split(
            "private fun failSwitchedReplacement()", 1
        )[0]
        self.assertIn("player = predecessor.player", rollback)
        self.assertIn("preparedRollbackReopen", rollback)
        self.assertIn("predecessor.player.playbackParameters", rollback)
        self.assertIn("predecessor.player.playWhenReady", rollback)
        commit = android.split("private fun commitPreparedReplacement", 1)[1].split(
            "fun collectRetiredPlayer()", 1
        )[0]
        self.assertIn("previous.playbackParameters", commit)
        self.assertIn("previous.playWhenReady", commit)
        self.assertIn("PREPARED_ALIGNMENT_SLACK_MS", commit)
        release = android.split("fun release()", 1)[1].split("fun switchAudio", 1)[0]
        self.assertIn(
            "endPlaybackControl(settling) { endingSession?.let(vm::endHlsSession) }",
            release,
        )
        self.assertNotIn("sessionId?.let { vm.endHlsSession(it) }", release)
        session = self.read(
            "clients/android/app/src/main/java/tv/plurx/app/player/PlaybackControlSession.kt"
        )
        finish = session.split("fun endAfterFinalExchange(", 1)[1].split(
            "private companion object", 1
        )[0]
        self.assertIn("subject.settle(outerScope, settling, ending)", finish)
        self.assertLess(finish.index("subject.stop()"), finish.rindex("afterFinalExchange()"))

    def test_native_sign_out_is_bounded_to_the_captured_session(self) -> None:
        apple_model = self.read("clients/apple/Sources/AppModel.swift")
        apple_api = self.read("clients/apple/Sources/PlurxAPI.swift")
        self.assertIn("let capturedOrigin = origin", apple_model)
        self.assertIn("Session.shared.credentials.token == token", apple_model)
        self.assertIn("where code == 401", apple_model)
        self.assertIn(
            "PlurxAPI(origin: capturedOrigin).logout(token: token)", apple_model
        )
        self.assertIn("configuration.timeoutIntervalForRequest = 5", apple_api)
        self.assertIn('req.setValue("Bearer \\(token)"', apple_api)

        android = self.read(
            "clients/android/app/src/main/java/tv/plurx/app/ui/AppViewModel.kt"
        )
        logout = android.split("private fun logout(removeDownloads: Boolean)", 1)[1].split(
            "fun changeServer()", 1
        )[0]
        self.assertIn("val capturedOrigin = Session.origin", logout)
        self.assertIn("val capturedToken = Session.token", logout)
        self.assertIn("withTimeoutOrNull(5_000L)", logout)
        self.assertIn("Net.profileClient(capturedToken)", logout)
        self.assertIn("withContext(NonCancellable)", logout)
        self.assertIn("Session.origin == capturedOrigin", logout)
        self.assertIn("Session.token == capturedToken", logout)
        self.assertIn("OfflineDownloads.removeProfile(instance, user)", logout)
        self.assertNotIn("error.code() == 401 || error.code() == 403", logout)

    def test_library_channel_collection_urls_match_router_and_fail_truthfully(self) -> None:
        http = self.read("crates/plurxd/src/http/mod.rs")
        routes = self.read("crates/plurxd/src/http/library_channels.rs")
        web = self.read("crates/plurxd/src/web/pages/library-channels-page.js")
        web_errors = self.read("crates/plurxd/src/web/library-channels.js")
        apple = self.read("clients/apple/Sources/PlurxAPI.swift")
        android = self.read(
            "clients/android/app/src/main/java/tv/plurx/app/data/PlurxApi.kt"
        )

        self.assertIn("library_channels::collection_router()", http)
        self.assertIn(
            "library_channel_collection_routes_accept_rollout_spellings_with_same_guards",
            http,
        )
        self.assertIn(
            '.route("/library-channels", get(list).post(create))', routes
        )
        self.assertIn(
            '.route("/library-channels/", get(list).post(create))', routes
        )
        self.assertEqual(routes.count("DefaultBodyLimit::max(64 * 1024)"), 2)
        self.assertIn('api(`/library-channels?${query}`)', web)
        self.assertIn('api("/library-channels",{method:"POST"', web)
        self.assertNotIn("/library-channels/?", web)
        self.assertIn('get("library-channels", query: query)', apple)
        self.assertIn('post("library-channels", body: definition)', apple)
        self.assertIn('@GET("library-channels")', android)
        self.assertIn('@POST("library-channels")', android)

        error_view = web_errors.split("function errorView(error, context)", 1)[1].split(
            "return {code, title: selected[0], detail: selected[1]};", 1
        )[0]
        self.assertIn(
            'status === 404 && collectionRoute ? "channel_route_unavailable"',
            error_view,
        )
        self.assertIn("known[code] || known.channel_request_failed", error_view)
        self.assertIn("channel_store_unavailable", error_view)
        self.assertIn(
            "LibraryChannelCore.errorView(error,{collection:true})", web
        )

    def test_android_final_alignment_completes_before_surface_transfer(self) -> None:
        controller = self.read(
            "clients/android/app/src/main/java/tv/plurx/app/player/Controller.kt"
        )
        poll = controller.split("private fun pollPreparedReplacement()", 1)[1].split(
            "private fun commitPreparedReplacement", 1
        )[0]
        commit = controller.split("private fun commitPreparedReplacement", 1)[1].split(
            "private fun settleCommitOnFirstFrame", 1
        )[0]
        self.assertIn("successorReady = hold.isReady", poll)
        self.assertIn("successorFilmMs = successorFilmPositionMs(originMs, successor.currentPosition)", poll)
        self.assertIn("is RendezvousHold.Step.Commit ->", poll)
        self.assertIn("successor.seekTo", poll)
        self.assertIn("commitPreparedReplacement(step.filmMs)", poll)
        self.assertIn("successorFilmMs - commitFilmMs", commit)
        self.assertIn("successorIsBuffered(bufferedThrough, commitFilmMs)", commit)
        self.assertNotIn("successor.seekTo", commit)

    def test_apple_prepared_enablement_is_visible_and_advisory(self) -> None:
        view = self.read("clients/apple/Sources/LiveTvDeveloperView.swift")
        section = view.split(
            'Section("Prepared quality handoff · advisory enablement")', 1
        )[1].split('Section("Enable Live TV · advisory enablement")', 1)[0]
        self.assertIn('Toggle("Enable two-player prepared handoff"', section)
        self.assertIn("Not met", section)
        self.assertIn("Checked during playback", section)
        self.assertNotIn(".disabled", section)


if __name__ == "__main__":
    unittest.main()
