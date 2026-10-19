"""Local regressions for qualification provenance, guards and honest failure receipts."""
import importlib.util
import json
import os
import shutil
import subprocess
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from common import Receipt, check_build_artifacts, check_build_metadata, check_identity, git_environment, hosted_guard, sha256


class QualificationCase(unittest.TestCase):
    def record(self):
        return {"id": 123, "head_sha": "a" * 40, "status": "completed", "conclusion": "success",
                "event": "workflow_dispatch", "path": ".github/workflows/github-release.yml",
                "repository": {"full_name": "pjunod/cinema"}}

    def test_candidate_run_requires_exact_dispatched_successful_release_source(self):
        check_identity("123", "a" * 40, "a" * 40, self.record(), "pjunod/cinema")
        for key, value in {"id": 124, "head_sha": "b" * 40, "status": "in_progress", "conclusion": "failure",
                           "event": "push", "path": ".github/workflows/another.yml",
                           "repository": {"full_name": "attacker/example"}}.items():
            with self.subTest(key=key), self.assertRaises(ValueError):
                check_identity("123", "a" * 40, "a" * 40, {**self.record(), key: value}, "pjunod/cinema")
        for run_id in ("0", "01", "-1", "123; echo bad", "１２３"):
            with self.subTest(run_id=run_id), self.assertRaises(ValueError):
                check_identity(run_id, "a" * 40, "a" * 40, self.record(), "pjunod/cinema")
        with self.assertRaises(ValueError):
            check_identity("123", "a" * 40, "b" * 40, self.record(), "pjunod/cinema")

    def test_prior_qualification_run_cannot_impersonate_a_complete_dry_run_build(self):
        names = ["release-linux-amd64", "release-linux-arm64", "release-macos", "release-windows", "desktop-update-candidate"]
        artifacts = [{"id": i + 1, "name": name, "expired": False, "size_in_bytes": 100} for i, name in enumerate(names)]
        self.assertEqual(len(check_build_artifacts({"total_count": 5, "artifacts": artifacts})), 5)
        with self.assertRaises(ValueError):
            check_build_artifacts({"total_count": 1, "artifacts": [{"name": "desktop-qualification-macos-15"}]})
        with self.assertRaises(ValueError):
            check_build_artifacts({"total_count": 5, "artifacts": [*artifacts[:-1], {**artifacts[-1], "expired": True}]})

    def test_build_metadata_binds_downloaded_bytes_before_host_mutation(self):
        directory = Path(__file__).resolve().parents[2] / "target/desktop-qualification-tests"
        directory.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=directory) as fixture:
            root = Path(fixture)
            asset = root / "fixture.deb"
            asset.write_bytes(b"transport provenance fixture, not package acceptance")
            manifest = {"source_commit": "a" * 40, "packaging_commit": "a" * 40, "source_tree": "b" * 40,
                        "artifacts": [{"name": asset.name, "size": asset.stat().st_size, "sha256": sha256(asset)}]}
            check_build_metadata(manifest, b"x" * 64, "a" * 40, "b" * 40, root)
            with self.assertRaises(ValueError):
                check_build_metadata(manifest, b"", "a" * 40, "b" * 40, root)
            with self.assertRaises(ValueError):
                check_build_metadata(manifest, b"x" * 64, "c" * 40, "b" * 40, root)
            asset.write_bytes(b"substituted native artifact")
            with self.assertRaises(ValueError):
                check_build_metadata(manifest, b"x" * 64, "a" * 40, "b" * 40, root)

    def test_fixture_git_commands_drop_inherited_repository_routing(self):
        value = git_environment({"GIT_DIR": "/wrong/repository", "GIT_INDEX_FILE": "/wrong/index",
                                 "GIT_WORK_TREE": "/wrong/tree", "GIT_CONFIG_PARAMETERS": "core.bare=true",
                                 "GITHUB_SHA": "a" * 40, "PATH": "/usr/bin"})
        self.assertEqual(value, {"GITHUB_SHA": "a" * 40, "PATH": "/usr/bin"})

    def test_owner_host_and_missing_mutation_acknowledgement_refuse(self):
        with patch.dict(os.environ, {}, clear=True), self.assertRaisesRegex(RuntimeError, "hosted"):
            hosted_guard("a" * 40, mutate=True)
        environment = {"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted",
                       "GITHUB_SERVER_URL": "https://github.com", "GITHUB_SHA": "a" * 40}
        with patch.dict(os.environ, environment, clear=True), self.assertRaisesRegex(RuntimeError, "acknowledgement"):
            hosted_guard("a" * 40, mutate=True)

    def test_failed_native_observation_cannot_leave_a_passing_receipt(self):
        directory = Path(__file__).resolve().parents[2] / "target/desktop-qualification-tests"
        directory.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=directory) as fixture:
            path = Path(fixture) / "receipt.json"
            with self.assertRaisesRegex(RuntimeError, "no real seat"):
                with Receipt(path, "a" * 40, "native-session") as receipt:
                    receipt.check("artifact", {"sha256": "b" * 64})
                    raise RuntimeError("no real seat")
            value = json.loads(path.read_bytes())
            self.assertEqual(value["status"], "failed")
            self.assertEqual(value["failure"], "no real seat")
            self.assertFalse(path.with_suffix(".json.partial").exists())

    @unittest.skipUnless(shutil.which("dpkg-deb"), "real deb archive qualification requires native Linux dpkg-deb")
    def test_real_deb_fixture_repackage_updates_exact_cohort_identity_and_checksums(self):
        spec = importlib.util.spec_from_file_location("qualification_fixture_builder", HERE / "build-local-fixtures.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        directory = Path(__file__).resolve().parents[2] / "target/desktop-qualification-tests"
        directory.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=directory) as fixture:
            root = Path(fixture)
            originals = root / "originals"
            originals.mkdir()
            arch = subprocess.check_output(["dpkg", "--print-architecture"], text=True).strip()
            for package in ("plurxd", "plurx-desktop"):
                tree = root / package
                (tree / "DEBIAN").mkdir(parents=True)
                depends = "Depends: plurxd (= 1.0.0-1)\n" if package == "plurx-desktop" else ""
                (tree / "DEBIAN/control").write_text(f"Package: {package}\nVersion: 1.0.0-1\nArchitecture: {arch}\n"
                    f"Maintainer: Fixture <fixture@example.invalid>\nDescription: packaging fixture only\n{depends}")
                if package == "plurxd":
                    (tree / "usr/lib/plurx").mkdir(parents=True)
                    (tree / "usr/lib/plurx-control").mkdir(parents=True)
                subprocess.run(["dpkg-deb", "--build", "--root-owner-group", tree,
                                originals / f"{package}_1.0.0-1_{arch}.deb"], check=True, capture_output=True)
            binaries = root / "binaries"
            binaries.mkdir()
            for name in ["plurxd", *module.BINS]:
                shutil.copy2("/bin/true", binaries / name)
            (binaries / "plurxd.dwp").write_bytes(b"fixture package symbols; not runtime acceptance")
            overlay = {"version": "1.0.1", "committed_overlay_sha": "b" * 40}
            output = root / "repackaged"
            module.repackage_debs(originals, output, binaries, overlay, arch)
            for package in ("plurxd", "plurx-desktop"):
                archive = output / f"{package}_1.0.1-1_{arch}.deb"
                actual = subprocess.check_output(["dpkg-deb", "-f", archive, "Version"], text=True).strip()
                self.assertEqual(actual, "1.0.1-1")
            desktop = output / f"plurx-desktop_1.0.1-1_{arch}.deb"
            depends = subprocess.check_output(["dpkg-deb", "-f", desktop, "Depends"], text=True)
            self.assertIn("plurxd (= 1.0.1-1)", depends)
            extracted = root / "check"
            subprocess.run(["dpkg-deb", "--raw-extract", output / f"plurxd_1.0.1-1_{arch}.deb", extracted], check=True)
            release = json.loads((extracted / "usr/lib/plurx-control/release.json").read_bytes())
            self.assertEqual(release["source_commit"], "b" * 40)
            self.assertEqual(release["version"], "1.0.1")
            subprocess.run(["md5sum", "-c", "DEBIAN/md5sums"], cwd=extracted, check=True, capture_output=True)

    def test_session_setup_masks_only_the_production_autostart_entry(self):
        spec = importlib.util.spec_from_file_location("qualification_linux_host", HERE / "linux-host.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        directory = Path(__file__).resolve().parents[2] / "target/desktop-qualification-tests"
        directory.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=directory) as fixture:
            home = Path(fixture) / "home"
            environment = {"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted",
                "GITHUB_SERVER_URL": "https://github.com", "GITHUB_SHA": "a" * 40,
                "PLURX_QUALIFICATION_MUTATE": "disposable-hosted-vm"}
            with patch.dict(os.environ, environment, clear=True):
                module.write_session_autostart(home, Path(fixture) / "receipts", "a" * 40, "x11")
            entries = home / ".config/autostart"
            mask = entries / "tv.plurx.Server-autostart.desktop"
            self.assertIn("Hidden=true", mask.read_text())
            self.assertIn("Exec=/usr/bin/plurx-tray --autostart", mask.read_text())
            driver = entries / "plurx-qualification.desktop"
            self.assertNotIn("Hidden=true", driver.read_text())
            self.assertIn(str(home / "qualification-session-driver"), driver.read_text())
            self.assertIn("linux-session.py", (home / "qualification-session-driver").read_text())

    def test_gui_evidence_requires_real_active_local_pam_seat(self):
        spec = importlib.util.spec_from_file_location("qualification_linux_session", HERE / "linux-session.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        session = {"Type": "x11", "Class": "user", "Remote": "no", "Active": "yes",
                   "Seat": "seat0", "User": "1001", "Service": "lightdm-autologin"}
        module.require_session(session, "x11", 1001)
        for key, value in {"Type": "tty", "Class": "manager", "Remote": "yes", "Active": "no",
                           "Seat": "", "User": "0", "Service": "su"}.items():
            with self.subTest(key=key), self.assertRaises(RuntimeError):
                module.require_session({**session, key: value}, "x11", 1001)


if __name__ == "__main__":
    unittest.main()
