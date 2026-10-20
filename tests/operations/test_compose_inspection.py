"""Exercise the actual Docker CLI template against an isolated fake HTTP API.

No Docker engine, real containers or user Docker configuration are accessed.
"""
import http.server
import importlib.machinery
import importlib.util
import json
from pathlib import Path
import shutil
import socketserver
import subprocess
import sys
import tempfile
import threading
import unittest

ROOT = Path(__file__).resolve().parents[2]
loader = importlib.machinery.SourceFileLoader('compose_inspection_control', str(ROOT / 'scripts/plurx-compose-control'))
spec = importlib.util.spec_from_loader(loader.name, loader)
control = importlib.util.module_from_spec(spec)
loader.exec_module(control)
DOCKER = shutil.which('docker')


class UnixServer(socketserver.UnixStreamServer):
    allow_reuse_address = True


class ComposeInspectionTests(unittest.TestCase):
    @unittest.skipUnless(DOCKER, 'actual Docker CLI required; no daemon required')
    def test_real_docker_template_accepts_missing_and_present_optional_health_fields(self):
        payload = {'Id': 'a' * 64, 'Image': 'sha256:' + 'b' * 64,
                   'State': {'Running': True, 'Status': 'running'},
                   'Config': {'Labels': {'com.docker.compose.project': 'plurx', 'com.docker.compose.service': 'plurx-discovery'}},
                   'HostConfig': {'RestartPolicy': {'Name': 'unless-stopped'}}}
        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                if self.path.endswith('/json'):
                    body = json.dumps(payload).encode()
                    self.send_response(200)
                    self.send_header('Content-Type', 'application/json')
                    self.send_header('Content-Length', str(len(body)))
                    self.end_headers()
                    self.wfile.write(body)
                else:
                    self.send_response(404)
                    self.end_headers()
            def log_message(self, *_args):
                pass
        with tempfile.TemporaryDirectory() as work:
            root = Path(work)
            socket = root / 'api.sock'
            with UnixServer(str(socket), Handler) as server:
                thread = threading.Thread(target=server.serve_forever, daemon=True)
                thread.start()
                try:
                    for health in (None, {'Status': 'healthy'}):
                        with self.subTest(health=health):
                            if health is None:
                                payload['State'].pop('Health', None)
                            else:
                                payload['State']['Health'] = health
                            result = subprocess.run([DOCKER, '--host', 'unix://' + str(socket), 'inspect', '--type', 'container', '--format', control.INSPECT, 'a' * 64],
                                                    env={'HOME': work, 'DOCKER_CONFIG': work, 'DOCKER_API_VERSION': '1.47', 'PATH': '/usr/bin:/bin'}, capture_output=True, text=True, timeout=15)
                            self.assertEqual(result.returncode, 0, result.stderr)
                            observed = json.loads(result.stdout)
                            self.assertEqual(observed['health'], None if health is None else 'healthy')
                            self.assertIsNone(observed['stop_timeout'])
                            self.assertEqual(observed['service'], 'plurx-discovery')
                finally:
                    server.shutdown()
                    thread.join(timeout=5)

    def test_failed_command_names_phase_without_exposing_stderr_or_arguments(self):
        command = [sys.executable, '-c', 'import sys;sys.stderr.write("PRIVATE_CONTAINER_VALUE");sys.exit(7)', 'inspect']
        with self.assertRaises(control.ControlError) as error:
            control.run(command)
        self.assertEqual(error.exception.code, 'unavailable')
        self.assertIn('Docker inspect', error.exception.message)
        self.assertIn('exit 7', error.exception.message)
        self.assertNotIn('PRIVATE_CONTAINER_VALUE', error.exception.message)
