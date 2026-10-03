"""A Safari receipt must describe the exact transmitted playlist revision."""
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]


class NativeRollingSeekFixtureTest(unittest.TestCase):
    def test_request_receipt_matches_payload_even_at_publication_boundary(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            text = ('#EXTM3U\n#EXT-X-TARGETDURATION:16\n'
                    '#EXTINF:16.000000,\nseg00000.m4s\n')
            for variant in ('baseline', 'short'):
                (root / variant).mkdir()
                (root / variant / 'index.m3u8').write_text(text)
            spec = importlib.util.spec_from_file_location(
                'native_seek_fixture', ROOT / 'tests/playback/native-rolling-seek-fixture.py')
            module = importlib.util.module_from_spec(spec)
            with patch('sys.argv', ['fixture', '--media', str(root), '--receipt', str(root / 'receipt.json')]), \
                    patch('http.server.ThreadingHTTPServer'):
                spec.loader.exec_module(module)
            run = {'variant': 'baseline', 'began': 0, 'publish_at': 0,
                   'position': 0, 'rate': 1, 'revision': 0, 'published': [], 'requests': [], 'events': []}
            module.runs['run'] = run
            calls = []

            def crossing_snapshot(current):
                # Re-sampling at this boundary returns a newer edge than the
                # already-selected payload. The old fixture logged that edge.
                calls.append(True)
                edge = 48 if len(calls) == 1 else 56
                current['revision'] = len(calls)
                current['published'] = [(edge, edge, 'seg00000.m4s')]
                return current['published']

            class Request:
                path = '/media/run/index.m3u8'
                payload = None

                def send(self, payload, *_):
                    self.payload = payload

            request = Request()
            with patch.object(module, 'snapshot', crossing_snapshot):
                module.Handler.do_GET(request)
            self.assertEqual(len(calls), 1)
            self.assertIn('#EXTINF:48.000000,', request.payload)
            self.assertEqual(run['requests'][0]['served_edge_s'], 48)
            self.assertEqual(run['requests'][0]['revision'], 1)

    def test_late_beacon_cannot_rewind_modeled_demand(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            text = '#EXTM3U\n#EXT-X-TARGETDURATION:16\n#EXTINF:16.000000,\nseg00000.m4s\n'
            for variant in ('baseline', 'short'):
                (root / variant).mkdir()
                (root / variant / 'index.m3u8').write_text(text)
            spec = importlib.util.spec_from_file_location(
                'native_seek_fixture', ROOT / 'tests/playback/native-rolling-seek-fixture.py')
            module = importlib.util.module_from_spec(spec)
            with patch('sys.argv', ['fixture', '--media', str(root), '--receipt', str(root / 'receipt.json')]), \
                    patch('http.server.ThreadingHTTPServer'):
                spec.loader.exec_module(module)
            run = {'position': 0, 'rate': 1, 'last_event_seq': 0, 'events': []}
            module.runs['run'] = run

            class Request:
                path = '/event'

                def __init__(self, event):
                    body = json.dumps({'run': 'run', 'event': event}).encode()
                    self.rfile = io.BytesIO(body)
                    self.headers = {'Content-Length': str(len(body))}

                def send(self, *_):
                    pass

            with patch.object(module, 'save'):
                for event in ({'event_seq': 2, 'position_s': 120, 'rate': 2},
                              {'event_seq': 1, 'position_s': 30, 'rate': 1}):
                    module.Handler.do_POST(Request(event))
            self.assertEqual(run['position'], 120)
            self.assertEqual(run['rate'], 2)
            self.assertEqual(run['last_event_seq'], 2)
            self.assertEqual(len(run['events']), 2, 'late evidence remains in the receipt')
