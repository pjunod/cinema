"""Installed desktop entry and privilege boundary contracts."""
from __future__ import annotations

import configparser
from pathlib import Path
import shlex
import unittest
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[2]
ASSETS = ROOT / "deploy/desktop"


class DesktopIntegrationCase(unittest.TestCase):
    def test_launcher_and_autostart_use_the_same_nonprivileged_binary(self):
        entries = {}
        for basename in ("tv.plurx.Server.desktop", "tv.plurx.Server-autostart.desktop"):
            entry = configparser.ConfigParser(interpolation=None)
            entry.read(ASSETS / basename)
            entries[basename] = entry["Desktop Entry"]
        self.assertEqual(shlex.split(entries["tv.plurx.Server.desktop"]["Exec"]),
                         ["/usr/bin/plurx-tray", "--window"])
        self.assertEqual(shlex.split(entries["tv.plurx.Server-autostart.desktop"]["Exec"]),
                         ["/usr/bin/plurx-tray", "--autostart"])
        for entry in entries.values():
            self.assertEqual(entry["TryExec"], "/usr/bin/plurx-tray")
            self.assertEqual(entry["Type"], "Application")
            self.assertEqual(entry["Icon"], "tv.plurx.Server")
            self.assertFalse(entry.getboolean("Terminal"))
            self.assertNotIn("X-KDE-SubstituteUID", entry)
            self.assertNotIn("X-KDE-Username", entry)

    def test_polkit_authorizes_only_the_installed_narrow_helper(self):
        policy = ET.parse(ASSETS / "tv.plurx.install.policy").getroot()
        actions = policy.findall("action")
        self.assertEqual(len(actions), 1)
        action = actions[0]
        self.assertEqual(action.attrib["id"], "tv.plurx.install")
        self.assertEqual({child.tag: child.text for child in action.find("defaults")},
                         {"allow_any": "no", "allow_inactive": "no", "allow_active": "auth_admin"})
        self.assertEqual({node.attrib["key"]: node.text for node in action.findall("annotate")},
                         {"org.freedesktop.policykit.exec.path":
                          "/usr/lib/plurx-control/plurx-install-helper"})


if __name__ == "__main__":
    unittest.main()
