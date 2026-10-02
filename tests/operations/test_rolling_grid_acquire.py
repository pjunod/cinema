"""Small synthetic ownership contracts; never corpus/browser evidence."""
import importlib.machinery
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import time
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
ACQUIRE = importlib.machinery.SourceFileLoader("rolling_acquire", str(ROOT / "scripts/rolling-grid-acquire")).load_module()


class RollingAcquireOwnershipTests(unittest.TestCase):
    def test_frame_cadence_waits_after_response_without_hiding_real_refusal(self):
        # Synthetic execution of the actual reviewed page, not browser/media
        # acceptance. Model a delayed response crossing the next interval tick.
        harness = r'''
const assert=require('node:assert/strict'),vm=require('node:vm');
let input='';process.stdin.on('data',x=>input+=x);
process.stdin.on('end',async()=>{
 try {
  const page=JSON.parse(input).page;
  const ready={nonce:'fixture',session:'session',generation:'generation',producer_attempt:1};
  const script=page.split('<script>')[1].split('</script>')[0].replace('READY',JSON.stringify(ready));
  let clock=1000,callback,tick,pending,requests=[];
  const video={paused:false,seeking:false,playbackRate:1,currentTime:1,
   buffered:{length:1,start:()=>0,end:()=>20},
   requestVideoFrameCallback:f=>callback=f,play:()=>Promise.resolve()};
  class Hls {static Events={ERROR:'error',MANIFEST_PARSED:'manifest'};
   on(){} attachMedia(v){this.media=v} loadSource(){} }
  const context={Hls,document:{querySelector:()=>video},performance:{now:()=>clock},
   setInterval:f=>tick=f,window:{},fetch:(_url,options)=>{
    requests.push(JSON.parse(options.body));
    return new Promise(resolve=>pending=resolve);
   }};
  vm.runInNewContext(script,context);
  const frame=(media,frames)=>{video.currentTime=media;callback(clock,{mediaTime:media,presentedFrames:frames})};
  frame(1,1);let exchange=tick();assert.equal(requests.length,1);
  clock=1120;pending({ok:true,status:200});await exchange;
  assert.equal(context.window.labStatus().accepted_frames,1);
  clock=1500;frame(2,2);await tick();
  assert.equal(requests.length,1,'next interval must not enter the bridge only380ms after acceptance');
  clock=1620;frame(3,3);exchange=tick();assert.equal(requests.length,2);
  clock=2200;frame(4,4);await tick();assert.equal(requests.length,2,'one in-flight exchange');
  pending({ok:true,status:200});await exchange;
  clock=2500;frame(5,5);await tick();assert.equal(requests.length,2,'slow response renews only reporting cadence');
  clock=2700;video.paused=true;await tick();assert.equal(requests.length,2,'paused frame is not evidence');
  video.paused=false;frame(6,6);exchange=tick();assert.equal(requests.length,3);
  pending({ok:false,status:409});await exchange;
  assert.equal(context.window.labStatus().failed,'control_refused_409','real refusal remains a terminal failure');
  clock=5000;frame(7,7);await tick();assert.equal(requests.length,3,'do not retry or manufacture acceptance after refusal');
  assert.equal(context.window.labStatus().accepted_frames,3);
  assert.equal(requests[0].producer_attempt,1);
  assert.equal(requests[0].media_time,1);
  console.log('actual-page synthetic cadence PASS');
 } catch(error) {console.error(error);process.exitCode=1;}
});
'''
        result = subprocess.run(["node", "-e", harness], input=json.dumps({"page": ACQUIRE.PAGE}),
                                text=True, capture_output=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("actual-page synthetic cadence PASS", result.stdout)

    def test_explicit_test_stack_crosses_both_owned_child_boundaries_without_inherited_environment(self):
        class CapturedLaunch(Exception):
            pass
        class Group:
            def kill(self): pass
            def remove(self): pass
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = {"nonce": "a" * 64, "root": str(root), "source": str(root / "source"),
                        "source_sha256": "b" * 64, "probe": str(root / "probe"), "height": 360,
                        "page": str(root / "page"), "test_binary": str(root / "binary"),
                        "ffmpeg": str(root / "ffmpeg"), "ffprobe": str(root / "ffprobe"),
                        "test_stack_bytes": 8388608}
            Path(manifest["source"]).write_bytes(b"synthetic fixture")
            launched = []
            def capture(command, **kwargs):
                launched.append((command, kwargs["env"]))
                raise CapturedLaunch()
            with patch.dict(os.environ, {"RUST_MIN_STACK": "33554432", "SECRET_TOKEN": "excluded"}), \
                 patch.object(ACQUIRE, "admit_owner", return_value=root), \
                 patch.object(ACQUIRE, "validate", return_value=root), \
                 patch.object(ACQUIRE, "execution_envelope", return_value={"memory_max": "2147483648"}), \
                 patch.object(ACQUIRE, "OwnedNamespace", return_value=Group()), \
                 patch.object(ACQUIRE.subprocess, "Popen", side_effect=capture):
                with self.assertRaises(CapturedLaunch):
                    ACQUIRE.acquire(manifest)
                with self.assertRaises(CapturedLaunch):
                    ACQUIRE.acquire_worker(manifest, time.monotonic() + 20)
            self.assertEqual(len(launched), 2)
            self.assertIn("--exec-owned-cell", launched[0][0])
            self.assertIn("--exec-owned-test", launched[1][0])
            for _, environment in launched:
                self.assertEqual(environment["RUST_MIN_STACK"], "8388608")
                self.assertNotIn("SECRET_TOKEN", environment)
            self.assertEqual(set(launched[0][1]), {"PATH", "HOME", "RUST_MIN_STACK"})
            self.assertEqual(set(launched[1][1]), {"PATH", "HOME", "RUST_MIN_STACK",
                                                 "PLURX_ROLLING_CELL", "PLURX_FFMPEG", "PLURX_FFPROBE"})
            envelope = json.loads((root / "execution-envelope.json").read_text())
            self.assertEqual(envelope["test_process_environment"], launched[1][1])
            default = dict(manifest)
            del default["test_stack_bytes"]
            self.assertNotIn("RUST_MIN_STACK", ACQUIRE.owned_child_environment(default))
            for invalid in (True, "8388608", 2097151, 2097153, 8388609, 33554432):
                with self.subTest(invalid=invalid), self.assertRaisesRegex(ValueError, "test_stack_bytes"):
                    ACQUIRE.owned_child_environment(dict(manifest, test_stack_bytes=invalid))

    def test_absolute_supervisor_bounds_stalled_operation_including_drain_and_close(self):
        class Worker:
            pid = 123
            returncode = None
            waits = 0
            def wait(self, timeout):
                self.waits += 1
                if self.waits == 1:
                    time.sleep(timeout)
                    raise subprocess.TimeoutExpired('fake blocked browser/body/drain', timeout)
                self.returncode = -9
                return self.returncode
            def poll(self): return self.returncode
            def kill(self): self.returncode = -9
        class Group:
            killed = False
            def kill(self): self.killed = True
            def empty(self): return self.killed
            def remove(self): assert self.killed
        worker, group, receipts = Worker(), Group(), []
        start = time.monotonic()
        with self.assertRaisesRegex(RuntimeError, 'absolute wall deadline'):
            ACQUIRE.supervise_owned(worker, group, start + 15.05, lambda: None, receipts.append)
        self.assertLess(time.monotonic() - start, .5)
        self.assertTrue(group.killed)
        self.assertTrue(receipts[0]['expired'])
        self.assertEqual(worker.returncode, -9)

    def test_supervisor_cleanup_survives_close_stop_receipt_errors_and_covers_owned_descendants(self):
        class Worker:
            pid = 123
            returncode = 1  # fake worker's browser close raised
            def wait(self, timeout): return self.returncode
            def poll(self): return self.returncode
            def kill(self): raise AssertionError('already reaped worker')
        class Group:
            # Synthetic namespace model includes detached-session descendants.
            owned = {'ffmpeg', 'browser-setsid', 'driver'}
            removed = False
            def kill(self): self.owned = set()
            def empty(self): return not self.owned
            def remove(self): self.removed = True
        def stop(): raise OSError('fake stop write failure')
        def record(result):
            self.assertTrue(group.removed)
            self.assertFalse(group.owned)
            self.assertIn('worker failed: 1', result['errors'])
            raise OSError('fake receipt failure')
        group = Group()
        with self.assertRaisesRegex(RuntimeError, 'worker failed: 1.*stop.*supervisor receipt'):
            ACQUIRE.supervise_owned(Worker(), group, time.monotonic() + 20, stop, record)
        self.assertTrue(group.removed)
        self.assertFalse(group.owned)

    def test_actual_manifest_refuses_foreign_owner_short_source_and_changed_frame_page(self):
        # The validator permits these existing private roots; Linux need not
        # provide macOS's /private/tmp. Never create a host-global directory.
        owned_base = next(path for path in (Path("/private/tmp"), Path("/var/tmp"))
                          if path.is_dir() and path.resolve() == path)
        with tempfile.TemporaryDirectory(prefix="rolling-contract-", dir=owned_base) as directory:
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
