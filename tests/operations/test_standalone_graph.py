"""Selected shipping graphs include build dependencies and every native component."""
from pathlib import Path
import os
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/check-standalone-graph"
ROOTS = "plurxd v0.8.0 (/source/crates/plurxd)\nplurx-desktop-control v0.8.0 (/source/crates/plurx-desktop-control)\nplurx-tray v0.8.0 (/source/crates/plurx-tray)\n"

class StandaloneGraphTests(unittest.TestCase):
    def run_graph(self, graph, status=0, arguments=()):
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            cargo = directory / "cargo"
            cargo.write_text("#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$ARGS\"\ncat \"$GRAPH\"\nexit \"$STATUS\"\n")
            cargo.chmod(0o700)
            (directory / "graph").write_text(graph)
            env = {**os.environ, "CARGO": str(cargo), "GRAPH": str(directory / "graph"),
                   "ARGS": str(directory / "args"), "STATUS": str(status)}
            result = subprocess.run([str(SCRIPT), *arguments], env=env, capture_output=True, text=True)
            args = (directory / "args").read_text().splitlines() if (directory / "args").exists() else []
            return result, args

    def test_checks_whole_shipping_closure_including_build_edges(self):
        result, args = self.run_graph(ROOTS + "[build-dependencies]\ncc v1.2.3\n")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(args, ["tree", "--locked", "--no-default-features", "--target", "all",
            "--edges", "normal,build", "--prefix", "none", "--format", "{p}",
            "-p", "plurxd", "-p", "plurx-desktop-control", "-p", "plurx-tray"])

    def test_rejects_sdk_in_normal_or_build_dependency_and_separate_tools(self):
        for package in ["hiqlite", "hiqlite-wal", "hiqlite-macros", "openraft", "openraft-macros",
                        "plurx-storage-migrate", "plurx-cluster-check"]:
            for prefix in ["", "[build-dependencies]\n"]:
                with self.subTest(package=package, prefix=prefix):
                    result, _ = self.run_graph(ROOTS + prefix + package + " v0.14.0\n")
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("cluster dependency", result.stderr)

    def test_refuses_missing_roots_unknown_records_and_failed_cargo(self):
        for graph, status in [("", 0), (ROOTS.replace("plurx-tray", "other-tray"), 0),
                              (ROOTS + "unrecognized output\n", 0), (ROOTS, 23)]:
            result, _ = self.run_graph(graph, status)
            self.assertNotEqual(result.returncode, 0)

    def test_target_is_explicit_and_arguments_cannot_change_package_selection(self):
        result, args = self.run_graph(ROOTS, arguments=("--target", "aarch64-apple-darwin"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(args[args.index("--target") + 1], "aarch64-apple-darwin")
        result, args = self.run_graph(ROOTS, arguments=("--package", "plurxd"))
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(args, [])

    def test_default_container_does_not_build_or_ship_cluster_tools(self):
        from validation.release_dockerfile import required_binaries, required_debug_binaries, render_binary_export
        source = (ROOT / "Dockerfile").read_text()
        self.assertEqual(required_binaries(source), ("plurxd",))
        self.assertEqual(required_debug_binaries(source), ("plurxd",))
        selected = render_binary_export(source)
        self.assertIn("scripts/check-standalone-graph", selected)
        self.assertNotIn("-p plurx-cluster-check", selected)
        self.assertNotIn("-p plurx-storage-migrate", selected)
        self.assertNotIn("--features cluster", selected)
        self.assertIn("AS runtime-cluster", source)
        self.assertIn("--features cluster", source)
        self.assertIn("AS storage-migration-helper", source)

    def test_cluster_distribution_keeps_real_sdk_and_excludes_validation_instrumentation(self):
        from validation.release_dockerfile import select_distribution, required_binaries, render_binary_export, render
        source = (ROOT / "Dockerfile").read_text()
        self.assertEqual(select_distribution(source, "standalone"), source)
        for old in [source.replace('LABEL tv.plurx.distribution="standalone"', ''),
                    source.replace('&& scripts/check-standalone-graph', '&& true')]:
            with self.assertRaises(ValueError):
                select_distribution(old, "standalone")
        selected = select_distribution(source, "cluster")
        self.assertEqual(required_binaries(selected), ("plurxd", "plurx-cluster-check"))
        exporter = render_binary_export(selected)
        self.assertIn("--features cluster", exporter)
        self.assertIn("cargo tree --locked --no-default-features -p plurxd --features cluster -e features >", exporter)
        self.assertIn("cluster-read-cost-validation", exporter)
        self.assertNotIn("scripts/check-standalone-graph", exporter)
        self.assertIn('tv.plurx.distribution="cluster"', render(selected))
        self.assertNotIn("plurx-storage-migrate", exporter)
        with self.assertRaises(ValueError):
            select_distribution(source.replace("AS runtime-cluster", "AS removed-cluster"), "cluster")
        with self.assertRaises(ValueError):
            select_distribution(source, "other")

    def test_fleet_publication_is_explicit_cluster_and_namespace_is_separate(self):
        script = (ROOT / "scripts/registry-push").read_text()
        self.assertIn('IMAGE="$REGISTRY/noirr/plurxd-cluster"', script)
        self.assertIn('docker build --target runtime-cluster', script)
        self.assertIn('scripts/container-restore-smoke "$ref"', script)
        publication = (ROOT / ".github/workflows/publish-release.yml").read_text()
        self.assertIn("'plurxd-cluster' || 'plurxd-standalone'", publication)
        self.assertEqual(publication.count('"tv.plurx.distribution"'), 3)
        self.assertIn('--distribution "$DISTRIBUTION"', publication)
        ci = (ROOT / ".github/workflows/ci.yml").read_text()
        self.assertIn('distribution: [standalone, cluster]', ci)
        self.assertIn('distribution: ${{ matrix.distribution }}', ci)

    def test_make_default_build_and_unit_exclude_sdk_tools(self):
        make = (ROOT / "Makefile").read_text()
        build = make.split("\nbuild:", 1)[1].split("\n\n", 1)[0]
        self.assertIn("standalone-graph-check", build)
        self.assertNotIn("--workspace", build)
        unit = make.split("\nunit:", 1)[1].split("\n\n", 1)[0]
        self.assertIn("--exclude plurx-storage-migrate", unit)
        self.assertIn("--exclude plurx-cluster-check", unit)

    def test_both_ci_rust_entry_points_compile_native_separately(self):
        make = (ROOT / "Makefile").read_text()
        for name in ["effort-rust-check", "ci-rust-gate"]:
            rule = make.split("\n" + name + ":", 1)[1].split("\n", 1)[0]
            self.assertIn("standalone-rust-check", rule)
            self.assertIn("cluster-rust-check", rule)
        native = make.split("\nstandalone-rust-check:", 1)[1].split("\n\n", 1)[0]
        self.assertIn("standalone-graph-check", native)
        self.assertIn("check --locked --no-default-features -p plurxd --all-targets", native)
        cluster = make.split("\ncluster-rust-check:", 1)[1].split("\n\n", 1)[0]
        self.assertIn("check --locked --no-default-features -p plurxd --features cluster --all-targets", cluster)
        self.assertIn("make effort-rust-check", (ROOT / ".github/workflows/effort-ci.yml").read_text())
        self.assertIn("make ci-rust-gate", (ROOT / ".github/workflows/ci.yml").read_text())

    def test_release_builds_use_actual_source_tree_and_native_graph_check(self):
        workflow = (ROOT / ".github/workflows/github-release.yml").read_text()
        self.assertIn('source_tree=$(git rev-parse HEAD^{tree})', workflow)
        self.assertEqual(workflow.count("PLURX_BUILD_TREE: ${{ needs.resolve.outputs.source_tree }}"), 4)
        self.assertEqual(workflow.count("scripts/check-standalone-graph"), 4)
        publication = (ROOT / ".github/workflows/publish-release.yml").read_text()
        self.assertIn('source_tree=$(git rev-parse "$commit_sha^{tree}")', publication)
        self.assertIn("PLURX_BUILD_TREE=${{ needs.resolve.outputs.source_tree }}", publication)
        docker = (ROOT / "Dockerfile").read_text()
        helper = docker.split("FROM build-source AS build-storage-migration-helper", 1)[1].split("FROM runtime AS default-runtime", 1)[0]
        self.assertIn('test "${#PLURX_BUILD_SHA}" = 40', helper)
        self.assertIn('test "${#PLURX_BUILD_TREE}" = 40', helper)
        exported = helper.split("FROM scratch AS storage-migration-helper", 1)[1]
        self.assertEqual(exported.count("COPY "), 1)
        self.assertIn("/plurx-storage-migrate /plurx-storage-migrate", exported)

if __name__ == "__main__":
    unittest.main()
