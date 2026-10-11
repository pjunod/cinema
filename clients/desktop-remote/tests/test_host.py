# SPDX-License-Identifier: Apache-2.0
import ctypes
import io
import json
from pathlib import Path
import struct
import sys
import tempfile
import types
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from native_host import Engine, ProtocolError, read_frame, write_frame, load_config
from errors import BackendError
from libcec_backend import LibCecBackend
from kernel_cec import KernelCecBackend, Message, Caps, Addresses, request
import install

EPOCH = "84b4c59c-5e6b-4b66-8d4d-556c0c62e349"
CREDIT = "49a2a11e-f6bb-4840-8d9b-4b16e0aac54e"

class FakeBackend:
    def __init__(self, emit): self.emit, self.closed = emit, False
    def open(self): pass
    def poll(self): pass
    def close(self): self.closed = True

class HostTests(unittest.TestCase):
    def engine(self, backend=FakeBackend):
        now, outputs, created = [0.0], [], []
        def factory(emit):
            value = backend(emit); created.append(value); return value
        engine = Engine(factory, outputs.append, lambda: now[0])
        engine.command({"type":"bind", "epoch":EPOCH})
        engine.command({"type":"heartbeat", "epoch":EPOCH, "credit":CREDIT})
        engine.tick()
        return engine, now, outputs, created

    def test_framing_rejects_oversize_before_payload_read(self):
        class Stream:
            def __init__(self): self.reads = []
            def read(self, count):
                self.reads.append(count)
                return struct.pack("=I", 16385)
        stream = Stream()
        with self.assertRaises(ProtocolError): read_frame(stream)
        self.assertEqual(stream.reads, [4])

    def test_framing_rejects_duplicates_truncation_and_invalid_utf8(self):
        for payload in (b'{"type":1,"type":2}', b'\xff', b'[]'):
            with self.assertRaises(ProtocolError): read_frame(io.BytesIO(struct.pack("=I",len(payload))+payload))
        with self.assertRaises(ProtocolError): read_frame(io.BytesIO(b'\x03\x00'))
        output = io.BytesIO(); write_frame(output,{"type":"status","state":"bound"});output.seek(0)
        self.assertEqual(read_frame(output), {"type":"status","state":"bound"})

    def test_config_requires_exact_extension_origin(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)/"config.json"
            path.write_text(json.dumps({"extension_id":"a"*32,"backend":"kernel","device":"/dev/cec0"}))
            self.assertEqual(load_config(path,"chrome-extension://"+"a"*32+"/")["backend"],"kernel")
            for origin in ("chrome-extension://"+"b"*32+"/","https://cinema.invalid",None):
                with self.assertRaises(ProtocolError):load_config(path,origin)

    def test_select_and_toggle_never_repeat_and_press_captures_credit(self):
        engine,now,out,backends = self.engine()
        backends[0].emit("press",0);engine.tick()
        now[0]=.4;backends[0].emit("press",0);engine.tick()
        self.assertEqual([m["key"] for m in out if m["type"]=="input"],["select"])
        self.assertEqual(next(m for m in out if m["type"]=="input")["credit"],CREDIT)
        backends[0].emit("release",0);engine.tick();backends[0].emit("press",0x61);engine.tick()
        now[0]=.8;backends[0].emit("press",0x61);engine.tick()
        self.assertEqual([m["key"] for m in out if m["type"]=="input"],["select","play_pause"])

    def test_direction_repeat_is_bounded_and_deadman_stops_lost_release(self):
        engine,now,out,backends = self.engine()
        backends[0].emit("press",4);engine.tick()
        now[0]=.35;engine.tick();now[0]=.4;engine.tick();now[0]=.475;engine.tick()
        self.assertEqual(len([m for m in out if m["type"]=="input"]),3)
        now[0]=.75;engine.tick();self.assertIsNone(engine.held)
        self.assertEqual(len([m for m in out if m["type"]=="input"]),3)

    def test_backend_open_stall_cannot_replay_old_press(self):
        now=[0.0];out=[];created=[]
        class Slow(FakeBackend):
            def open(self): self.emit("press",0);now[0]=2.0
        def factory(emit): value=Slow(emit);created.append(value);return value
        engine=Engine(factory,out.append,lambda:now[0]);engine.command({"type":"bind","epoch":EPOCH});engine.command({"type":"heartbeat","epoch":EPOCH,"credit":CREDIT});engine.tick()
        self.assertFalse(any(m["type"]=="input" for m in out));self.assertIsNone(engine.epoch);self.assertTrue(created[0].closed)

    def test_poll_stall_negative_event_age_and_disconnect_reject_input(self):
        engine,now,out,backends=self.engine()
        backends[0].emit("press",0)
        backends[0].poll=lambda:now.__setitem__(0,2.0)
        now[0]=.25;engine.tick();self.assertFalse(any(m["type"]=="input" for m in out));self.assertTrue(backends[0].closed)
        engine,now,out,backends=self.engine();engine.events.put((engine.generation,"press",0,1.0,CREDIT));engine.tick()
        self.assertFalse(any(m["type"]=="input" for m in out))
        now[0]=1.0;engine.tick();self.assertIsNone(engine.backend)

    def test_rebind_and_overload_drop_old_generation_keys(self):
        engine,now,out,backends=self.engine();old=backends[0]
        new="173ed6a3-8c1c-4f81-8b57-9dcd299efb01";engine.command({"type":"bind","epoch":new});engine.command({"type":"heartbeat","epoch":new,"credit":CREDIT});engine.tick()
        old.emit("press",0);engine.tick();self.assertFalse(any(m["type"]=="input" for m in out));self.assertTrue(old.closed)
        for _ in range(65):backends[-1].emit("press",0)
        engine.tick();self.assertIsNone(engine.held);self.assertFalse(any(m["type"]=="input" for m in out))

    def test_native_commands_cannot_choose_processes_paths_or_power(self):
        engine,_,_,_=self.engine()
        for message in ({"type":"execute","epoch":EPOCH,"argv":["sh"]},{"type":"heartbeat","epoch":EPOCH,"credit":CREDIT,"path":"/tmp"},{"type":"bind","epoch":"old"}):
            with self.assertRaises(ProtocolError):engine.command(message)

    def fake_cec(self, version="8.1.6", adapters=None, opened=True):
        calls=[]
        class Config:
            serverVersion=1
            def __init__(self):self.deviceTypes=types.SimpleNamespace(Add=lambda value:calls.append(("device",value)))
            def SetLogCallback(self,value):self.log=value
            def SetKeyPressCallback(self,value):self.key=value
        class Adapter:
            def VersionToString(self,_):return version
            def DetectAdapters(self):return adapters if adapters is not None else [types.SimpleNamespace(strComName="chosen")]
            def Open(self,device):calls.append(("open",device));return opened
            def Close(self):calls.append("close")
            def PingAdapter(self):return True
        return types.SimpleNamespace(libcec_configuration=Config,CEC_DEVICE_TYPE_PLAYBACK_DEVICE=4,LIBCEC_VERSION_CURRENT=1,ICECAdapter=types.SimpleNamespace(Create=lambda config:Adapter())),calls

    def test_structured_libcec_callback_and_selected_adapter_cleanup(self):
        binding,calls=self.fake_cec();events=[];backend=LibCecBackend("chosen",lambda kind,key:events.append((kind,key)),binding)
        backend.open();backend.config.key(4,0);backend.config.key(4,50);backend.config.key(True,0);backend.poll();backend.close()
        self.assertEqual(events,[("press",4),("release",4)]);self.assertIn(("open","chosen"),calls);self.assertIn("close",calls)
        self.assertFalse(hasattr(backend,"Transmit"))

    def test_libcec_pin_missing_adapter_and_failed_open_are_truthful(self):
        for version,adapters,opened,state in (("8.1.7",None,True,"version_mismatch"),("8.1.6",[],True,"no_adapter"),("8.1.6",None,False,"open_failed")):
            binding,calls=self.fake_cec(version,adapters,opened);backend=LibCecBackend("chosen",lambda *_:None,binding)
            with self.assertRaises(BackendError) as caught:backend.open()
            self.assertEqual(caught.exception.state,state);self.assertIn("close",calls)
        with patch("libcec_backend.importlib.import_module",side_effect=ImportError):
            with self.assertRaises(BackendError) as caught:LibCecBackend("chosen",lambda *_:None)
            self.assertEqual(caught.exception.state,"missing_dependency")

    def test_kernel_uapi_layout_and_addressed_fresh_keys(self):
        self.assertEqual((ctypes.sizeof(Caps),ctypes.sizeof(Addresses),ctypes.sizeof(Message)),(76,92,56))
        self.assertEqual(request(6,Message),0xc0386106)
        events=[];backend=KernelCecBackend("/dev/cec0",lambda kind,key:events.append((kind,key)),lambda:1_000_000_000);backend.mask=1<<4
        message=Message();message.rx_ts=900_000_000;message.rx_status=1;message.length=3;message.message[:3]=[0x04,0x44,4];backend.decode(message)
        message.rx_ts=1;backend.decode(message);message.rx_ts=2_000_000_000;backend.decode(message)
        message.rx_ts=900_000_000;message.message[0]=0x0f;backend.decode(message)
        self.assertEqual(events,[("press",4)])

    def test_install_uninstall_preserves_foreign_files_and_refuses_modified_host(self):
        with tempfile.TemporaryDirectory() as directory:
            base=Path(directory);prefix=base/"cinema";manifest_dir=base/"manifests"
            files,manifest,registry,name=install.plan("linux","chromium",base,prefix,"a"*32,"kernel","/dev/cec0",Path(sys.executable),manifest_dir)
            install.install(files,prefix,manifest,registry,name)
            self.assertEqual(json.loads(manifest.read_text())["allowed_origins"],["chrome-extension://"+"a"*32+"/"])
            foreign=prefix/"my-note";foreign.write_text("keep")
            host=prefix/"native_host.py";original=host.read_bytes();host.write_text("modified")
            with self.assertRaises(ValueError):install.uninstall(prefix)
            host.write_bytes(original);install.uninstall(prefix);self.assertEqual(foreign.read_text(),"keep");self.assertFalse(manifest.exists())

    def test_reinstall_target_change_preserves_previous_manifest_and_receipt(self):
        with tempfile.TemporaryDirectory() as directory:
            base=Path(directory);prefix=base/"cinema"
            first=install.plan("linux","chrome",base,prefix,"a"*32,"kernel","/dev/cec0",Path(sys.executable),base/"chrome")
            install.install(first[0],prefix,*first[1:]);receipt=(prefix/"installation.json").read_bytes();manifest=first[1].read_bytes()
            second=install.plan("linux","edge",base,prefix,"a"*32,"kernel","/dev/cec0",Path(sys.executable),base/"edge")
            with self.assertRaisesRegex(ValueError,"registration target changed"):install.install(second[0],prefix,*second[1:])
            self.assertEqual((prefix/"installation.json").read_bytes(),receipt);self.assertEqual(first[1].read_bytes(),manifest);self.assertFalse(second[1].exists())
            install.uninstall(prefix);self.assertFalse(first[1].exists())

    def test_windows_foreign_registration_and_receipt_keys_are_refused(self):
        key=next(iter(install.WINDOWS_KEYS));manifest=Path("C:/Cinema/"+install.NAME+".json")
        with patch("install.read_windows_registration",return_value="C:/foreign/host.json"):
            for owned in (False,True):
                with self.assertRaises(ValueError):install.validate_windows_registration(key,manifest,owned)
        with patch("install.read_windows_registration",return_value=str(manifest)):
            with self.assertRaises(ValueError):install.validate_windows_registration(key,manifest,False)
            install.validate_windows_registration(key,manifest,True)
        with tempfile.TemporaryDirectory() as directory:
            prefix=Path(directory)
            (prefix/"installation.json").write_text(json.dumps({"manifest":str(manifest),"registry":"Software\\ForeignApp","files":{}}))
            with self.assertRaises(ValueError):install.uninstall(prefix)
            self.assertTrue((prefix/"installation.json").exists())

    def test_windows_plan_is_per_user_and_launcher_carries_isolated_interpreter(self):
        with tempfile.TemporaryDirectory() as directory:
            base=Path(directory)
            files,manifest,registry,name=install.plan("win32","edge",base,base/"cinema","a"*32,"libcec","COM3",Path("C:/Python/python.exe"))
            self.assertEqual(name,"launcher.cmd");self.assertTrue(registry.startswith("Software\\Microsoft\\Edge\\NativeMessagingHosts"))
            self.assertIn(b" -I ",files[base/"cinema"/name]);self.assertIn(b" %*",files[base/"cinema"/name])
            with self.assertRaises(ValueError):install.launcher("win32",Path('C:/bad%path/python.exe'),base)

if __name__ == "__main__":unittest.main()
