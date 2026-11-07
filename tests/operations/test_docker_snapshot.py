from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class DockerSnapshotTests(unittest.TestCase):
    def test_live_mirrors_serve_only_debs_and_indexes_stay_pinned(self):
        with tempfile.TemporaryDirectory() as directory:
            apt = Path(directory)
            (apt / "sources.list.d").mkdir()
            source = apt / "sources.list.d/debian.sources"
            source.write_text("Types: deb\nURIs: http://deb.debian.org/debian\nSuites: bookworm bookworm-updates\nComponents: main\nSigned-By: /usr/share/keyrings/debian-archive-keyring.gpg\n\nTypes: deb\nURIs: http://deb.debian.org/debian-security\nSuites: bookworm-security\nComponents: main\n")
            subprocess.run(["sh", str(ROOT / "scripts/configure-debian-snapshot"), "20260928T000000Z", directory], check=True)
            for archive in ("debian", "debian-security"):
                rows = (apt / f"plurx-{archive}.mirrors").read_text().splitlines()
                self.assertEqual(rows[0], f"http://deb.debian.org/{archive}/\tpriority:1 type:deb")
                for row in rows[1:]:
                    self.assertIn(f"/archive/{archive}/20260928T000000Z/", row)
                self.assertIn(f"mirror+file:{apt}/plurx-{archive}.mirrors", source.read_text())
            self.assertIn("Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg", source.read_text())
            self.assertIn("Suites: bookworm bookworm-updates", source.read_text())
            self.assertIn('Acquire::Retries "5";', (apt / "apt.conf.d/99plurx-snapshot").read_text())

    def test_archive_metadata_requests_retain_filters_before_the_next_mirrorlist(self):
        with tempfile.TemporaryDirectory() as directory:
            apt = Path(directory)
            (apt / "sources.list.d").mkdir()
            source = apt / "sources.list.d/debian.sources"
            source.write_text(
                "Types: deb\nURIs: http://deb.debian.org/debian\n"
                "Suites: bookworm\nComponents: main\n\n"
                "Types: deb\nURIs: http://deb.debian.org/debian-security\n"
                "Suites: bookworm-security\nComponents: main\n"
            )
            subprocess.run([
                "sh", str(ROOT / "scripts/configure-debian-snapshot"),
                "20260928T000000Z", directory,
            ], check=True)
            configuration = (apt / "apt.conf.d/99plurx-snapshot").read_text()
            self.assertIn('Acquire::Max-Pipeline-Depth "1";', configuration)
            for archive in ("debian", "debian-security"):
                mirrors = (apt / f"plurx-{archive}.mirrors").read_text().splitlines()
                self.assertTrue(mirrors[0].endswith("priority:1 type:deb"))
                self.assertIn(f"/archive/{archive}/20260928T000000Z/", mirrors[1])

    def test_every_snapshot_stage_configures_package_fallback(self):
        dockerfile = (ROOT / "Dockerfile").read_text()
        for stage in dockerfile.split("\nFROM "):
            if "ARG DEBIAN_SNAPSHOT=" in stage:
                self.assertIn("source=scripts/configure-debian-snapshot", stage)
                self.assertLess(stage.index('sh /tmp/configure-debian-snapshot "$DEBIAN_SNAPSHOT"'), stage.index("apt-get update"))
