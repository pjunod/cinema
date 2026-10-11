"""Source preparation contracts; hardware/backend qualification is separate."""
import importlib.machinery
import importlib.util
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


def preparer():
    path = ROOT / "scripts/prepare-linux-dolby-ffmpeg"
    loader = importlib.machinery.SourceFileLoader("linux_dolby_source", str(path))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


class LinuxDolbySourceCase(unittest.TestCase):
    def test_wrong_source_archive_cannot_create_a_candidate(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "wrong.tar.gz"
            source.write_bytes(b"unbound source")
            output = Path(directory) / "candidate"
            with self.assertRaisesRegex(ValueError, "checksum mismatch"):
                preparer().prepare(source, output)
            self.assertFalse(output.exists())

    def test_linux_strict_patch_preserves_reviewed_decoder_and_cpu_renderer(self):
        upstream = (ROOT / "scripts/macos-video-ffmpeg-patches/"
                    "0001-require-effective-dolby-and-hardware-decode.patch").read_text()
        retained = {"libavcodec/hevc/hevcdec.c", "libavcodec/hevc/hevcdec.h",
                    "libavfilter/vf_tonemapx.c"}
        expected = "".join("--- a/" + block for block in upstream.split("--- a/")[1:]
                           if block.splitlines()[0] in retained)
        actual = (ROOT / "scripts/linux-video-ffmpeg-patches/"
                  "0001-require-current-dolby-state.patch").read_text()
        self.assertEqual(actual, expected)
        self.assertNotIn("VideoToolbox", actual)
        self.assertNotIn("tonemap_videotoolbox", actual)
        self.assertIn("Required current-access-unit Dolby state missing", actual)
        self.assertIn("HEVC_NAL_EOB_NUT", actual)
        self.assertIn("signal_eotf", actual)


if __name__ == "__main__":
    unittest.main()
