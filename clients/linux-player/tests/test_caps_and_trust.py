"""The caps document's field names match caps.rs, and the trust list holds no secret."""

import json
import os
import re
import stat
import tempfile
import unittest

import support
from plurx_player.caps import device_caps
from plurx_player.trust import TrustStore, ensure_trusted

REPO = os.path.dirname(os.path.dirname(support.PACKAGE_ROOT))
CAPS_RS = os.path.join(REPO, "crates/plurx-core/src/playback/caps.rs")
AUDIO_RS = os.path.join(REPO, "crates/plurx-core/src/playback/audio.rs")


def rust_fields(path, struct):
    with open(path, encoding="utf-8") as handle:
        source = handle.read()
    body = re.search(r"pub struct " + struct + r" \{(.*?)\n\}", source, re.S).group(1)
    skipped = set(re.findall(r"#\[serde\(skip\)\]\s*(?:///[^\n]*\n\s*)*pub (\w+):", body))
    return set(re.findall(r"pub (\w+):", body)) - skipped


class CapsTests(unittest.TestCase):
    def setUp(self):
        self.caps = device_caps("0.1.0")

    @unittest.skipUnless(os.path.exists(CAPS_RS), "needs the plurx source tree")
    def test_every_field_is_a_caps_rs_field(self):
        self.assertLessEqual(set(self.caps), rust_fields(CAPS_RS, "DeviceCaps"))
        for video in self.caps["video"]:
            self.assertLessEqual(set(video), rust_fields(CAPS_RS, "VideoCaps"))
        self.assertLessEqual(set(self.caps["display"]), rust_fields(CAPS_RS, "DisplayCaps"))
        self.assertLessEqual(set(self.caps["client"]), rust_fields(CAPS_RS, "ClientInfo"))
        for sink in self.caps["audio_sinks"]:
            self.assertLessEqual(set(sink), rust_fields(AUDIO_RS, "AudioSink"))

    def test_plan_section_3_2_claims(self):
        caps = self.caps
        self.assertEqual(caps["v"], 2)
        hevc = next(v for v in caps["video"] if v["codec"] == "hevc")
        self.assertEqual(hevc, {"codec": "hevc", "profiles": ["main", "main10"], "max_height": 2160,
                                "present": ["sdr", "pq", "hlg"], "dv_profiles": [5, 7, 8]})
        self.assertEqual({v["codec"] for v in caps["video"]}, {"hevc", "h264", "av1"})
        self.assertIn(7, hevc["dv_profiles"], "P7 FEL is composed in the player, so the original stream is wanted")
        self.assertEqual(caps["display"], {"hdr": True, "dolby_vision": True}, "no max_nits: Unknown")
        self.assertEqual(caps["hls_hevc_sample_entries"], ["hvc1", "hev1"])
        self.assertEqual(caps["dv_transport"], "hls")
        self.assertEqual(caps["subtitle_overlays"], [])
        self.assertTrue(caps["audio_sinks"])
        for sink in caps["audio_sinks"]:
            self.assertFalse(sink["passthrough"])
            self.assertEqual(sink["max_channels"], 8)
        self.assertEqual({s["codec"] for s in caps["audio_sinks"]}, set(caps["audio"]))
        self.assertIn("hls", caps["transports"])


class TrustTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="plurx-player-trust-")
        self.dir = os.path.join(self.tmp, "plurx-player")

    def tearDown(self):
        import shutil
        shutil.rmtree(self.tmp, ignore_errors=True)

    def test_confirmed_origin_is_remembered_privately(self):
        store = TrustStore(self.dir)
        asked = []
        self.assertTrue(ensure_trusted(store, "http://media1:8096", lambda o: asked.append(o) or True))
        self.assertTrue(ensure_trusted(store, "http://media1:8096", lambda o: self.fail("asked twice")))
        self.assertEqual(asked, ["http://media1:8096"])
        self.assertEqual(stat.S_IMODE(os.stat(self.dir).st_mode), 0o700)
        self.assertEqual(stat.S_IMODE(os.stat(store.path).st_mode), 0o600)
        with open(store.path, encoding="utf-8") as handle:
            data = json.load(handle)
        self.assertEqual(list(data["servers"]), ["http://media1:8096"])
        self.assertEqual(set(data["servers"]["http://media1:8096"]), {"trusted_at"})

    def test_declined_origin_is_not_remembered(self):
        store = TrustStore(self.dir)
        self.assertFalse(ensure_trusted(store, "http://evil.example", lambda o: False))
        self.assertFalse(os.path.exists(store.path))

    def test_a_corrupt_list_trusts_nothing(self):
        os.makedirs(self.dir)
        with open(os.path.join(self.dir, "servers.json"), "w", encoding="utf-8") as handle:
            handle.write("{not json")
        self.assertFalse(TrustStore(self.dir).is_trusted("http://media1:8096"))

    def test_ca_is_stored_as_an_absolute_path(self):
        store = TrustStore(self.dir)
        ensure_trusted(store, "https://media1", lambda o: True, ca="ca.pem")
        self.assertEqual(store.entry("https://media1")["ca"], os.path.abspath("ca.pem"))


if __name__ == "__main__":
    unittest.main()
