"""The desktop entry registers the scheme, the package never edits associations
as root, and the private mpv/libplacebo build is pinned where the launcher and
the package version expect it."""

import configparser
import glob
import os
import re
import unittest

import support
from plurx_player import mpv

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


def read(relative):
    with open(os.path.join(ROOT, relative), encoding="utf-8") as handle:
        return handle.read()


def pins():
    rows = {}
    for line in read("packaging/upstream.lock").splitlines():
        if line.strip() and not line.startswith("#"):
            name, version, sha256, url = line.split()
            rows[name] = (version, sha256, url)
    return rows


class PrivateBuild(unittest.TestCase):
    def test_sources_are_pinned_by_sha256(self):
        rows = pins()
        self.assertEqual(sorted(rows), ["libplacebo", "mpv"])
        for name, (version, sha256, url) in rows.items():
            self.assertRegex(sha256, r"^[0-9a-f]{64}$", name)
            self.assertTrue(url.startswith("https://"), url)
            self.assertIn(version, url, name)

    def test_package_version_names_the_pinned_versions(self):
        rows = pins()
        version = re.match(r"plurx-player \(([^)]+)\)", read("debian/changelog")).group(1)
        self.assertTrue(
            version.endswith(f"+lp{rows['libplacebo'][0]}.mpv{rows['mpv'][0]}"), version)

    def test_patch_series_directories_hold_only_patches(self):
        for name in ("libplacebo", "mpv"):
            directory = os.path.join(ROOT, "packaging/patches", name)
            for entry in os.listdir(directory):
                self.assertTrue(entry == ".gitkeep" or entry.endswith(".patch"), entry)

    def test_launcher_default_is_the_installed_private_mpv(self):
        rules = read("debian/rules")
        private = re.search(r"^PRIVATE = (\S+)$", rules, re.M).group(1)
        self.assertIn("$(PKG)/$(PRIVATE)/bin/mpv", rules)
        self.assertEqual(mpv.DEFAULT_BINARY, f"/{private}/bin/mpv")

    def test_description_states_what_hdr_needs(self):
        control = read("debian/control")
        self.assertIn("Architecture: amd64", control)
        self.assertIn("${shlibs:Depends}", control)
        self.assertIn("wp_color_manager_v1 in HDR mode", control)
        self.assertIn("reports the panel's real luminance", control)
        self.assertNotIn("Recommends: mpv", control)


if __name__ == "__main__":
    unittest.main()
