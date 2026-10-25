"""The desktop entry registers the scheme, and the package never edits associations as root."""

import configparser
import glob
import os
import unittest

import support

ROOT = support.PACKAGE_ROOT


class Packaging(unittest.TestCase):
    def test_desktop_entry_handles_the_scheme(self):
        entry = configparser.ConfigParser(interpolation=None)
        entry.optionxform = str
        entry.read(os.path.join(ROOT, "plurx-player.desktop"), encoding="utf-8")
        desktop = entry["Desktop Entry"]
        self.assertEqual(desktop["Type"], "Application")
        self.assertEqual(desktop["MimeType"], "x-scheme-handler/plurx-player;")
        self.assertEqual(desktop["Exec"], "/usr/bin/plurx-player %u")
        self.assertEqual(desktop["Terminal"], "false")

    def test_debian_installs_what_exists_and_never_runs_xdg_mime(self):
        with open(os.path.join(ROOT, "debian/install"), encoding="utf-8") as handle:
            rows = [line.split() for line in handle if line.strip() and not line.startswith("#")]
        for source, destination in rows:
            self.assertTrue(glob.glob(os.path.join(ROOT, source)), source)
            self.assertTrue(destination.startswith("usr/"), destination)
        self.assertIn(["plurx-player", "usr/bin/"], rows)
        for name in os.listdir(os.path.join(ROOT, "debian")):
            path = os.path.join(ROOT, "debian", name)
            if os.path.isfile(path):
                with open(path, encoding="utf-8") as handle:
                    self.assertNotIn("xdg-mime", handle.read(), name)
        self.assertFalse(os.path.exists(os.path.join(ROOT, "debian/postinst")))
        with open(os.path.join(ROOT, "debian/control"), encoding="utf-8") as handle:
            control = handle.read()
        self.assertIn("zenity", control)

    def test_launcher_script_is_executable(self):
        self.assertTrue(os.access(os.path.join(ROOT, "plurx-player"), os.X_OK))


if __name__ == "__main__":
    unittest.main()
