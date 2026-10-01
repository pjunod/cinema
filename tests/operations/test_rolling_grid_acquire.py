"""Small synthetic ownership contracts; never corpus/browser evidence."""
import importlib.machinery
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
ACQUIRE = importlib.machinery.SourceFileLoader("rolling_acquire", str(ROOT / "scripts/rolling-grid-acquire")).load_module()


class RollingAcquireOwnershipTests(unittest.TestCase):
    def test_actual_manifest_refuses_foreign_owner_short_source_and_changed_frame_page(self):
        with tempfile.TemporaryDirectory(prefix="rolling-contract-",dir="/private/tmp") as directory:
            root=Path(directory);root.chmod(0o700)
            nonce="a"*64
            (root/"owner.json").write_text(json.dumps({"nonce":nonce}))
            source=root/"source.bin";source.write_bytes(b"synthetic contract only")
            probe=root/"probe.json";probe.write_text(json.dumps({"format":{"duration":"64"},"streams":[{"codec_type":"video","height":1080}]}))
            page=root/"page.html";page.write_text(ACQUIRE.PAGE)
            manifest={"nonce":nonce,"root":str(root),"source":str(source),"source_sha256":ACQUIRE.sha(source),"probe":str(probe),"page":str(page),"height":720,"hls_sha256":ACQUIRE.sha(ROOT/"crates/plurxd/src/web/hls.min.js")}
            for key in ("test_binary","browser","ffmpeg","ffprobe"):
                manifest[key]=str(source);manifest[key+"_sha256"]=ACQUIRE.sha(source)
            self.assertEqual(ACQUIRE.validate(manifest),root)
            (root/"owner.json").write_text(json.dumps({"nonce":"b"*64}))
            with self.assertRaisesRegex(ValueError,"owner mismatch"):ACQUIRE.validate(manifest)
            (root/"owner.json").write_text(json.dumps({"nonce":nonce}))
            probe.write_text(json.dumps({"format":{"duration":"45"},"streams":[{"codec_type":"video","height":1080}]}))
            with self.assertRaisesRegex(ValueError,"full-duration"):ACQUIRE.validate(manifest)
            probe.write_text(json.dumps({"format":{"duration":"64"},"streams":[{"codec_type":"video","height":1080}]}))
            page.write_text(ACQUIRE.PAGE.replace("},500)","},33)"))
            with self.assertRaisesRegex(ValueError,"observation source"):ACQUIRE.validate(manifest)
            page.write_text(ACQUIRE.PAGE)
            (root/"ready.json").write_text("{}")
            with self.assertRaisesRegex(ValueError,"existing acquisition"):ACQUIRE.validate(manifest)

    def test_cleanup_refuses_foreign_process_identity_then_reaps_exact_owned_child(self):
        process=subprocess.Popen([sys.executable,"-c","import time;time.sleep(60)"],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        try:
            with self.assertRaisesRegex(ValueError,"identity mismatch"):
                ACQUIRE.stop_owned(process,process.pid+1)
            self.assertIsNone(process.poll(),"wrong identity must not signal child")
            ACQUIRE.stop_owned(process,process.pid)
            self.assertIsNotNone(process.poll(),"exact owned child reaped")
        finally:
            if process.poll() is None:
                process.kill();process.wait(timeout=5)
