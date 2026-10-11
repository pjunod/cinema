"""Synthetic fixture controls; no network and no successful units replayed."""

import hashlib
import io
import os
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import urllib.error

from validation import hiqlite_upstream_fixture as fixture
from validation import python_unit_receipts as receipts


def crate(members):
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode='w:gz') as archive:
        for name, kind, body in members:
            member = tarfile.TarInfo(name)
            member.type = kind
            member.size = len(body)
            member.linkname = '../outside' if kind == tarfile.SYMTYPE else ''
            archive.addfile(member, io.BytesIO(body))
    return output.getvalue()


class FixtureTests(unittest.TestCase):
    def test_pinned_download_and_owned_extraction_reject_changed_sources(self):
        raw = crate([('hiqlite-0.14.0/src/lib.rs', tarfile.REGTYPE, b'upstream')])
        pins = {'hiqlite': (len(raw), hashlib.sha256(raw).hexdigest())}
        for payload in (raw, raw + b'x', raw[:-1], b'x' * len(raw)):
            with self.subTest(download=len(payload), same=payload == raw):
                response = io.BytesIO(payload)
                with patch.object(fixture, 'CRATES', pins), \
                        patch.object(fixture.urllib.request.OpenerDirector, 'open', return_value=response) as opened:
                    if payload == raw:
                        self.assertEqual(fixture.download_crate('hiqlite'), raw)
                        self.assertEqual(opened.call_args.args[0],
                            'https://static.crates.io/crates/hiqlite/hiqlite-0.14.0.crate')
                        self.assertEqual(opened.call_args.kwargs['timeout'], 20)
                    else:
                        with self.assertRaises(fixture.UpstreamFixtureError):
                            fixture.download_crate('hiqlite')
                self.assertTrue(response.closed)
        with patch.object(fixture.urllib.request.OpenerDirector, 'open',
                          side_effect=urllib.error.URLError('synthetic sensitive diagnostics')):
            with self.assertRaisesRegex(fixture.UpstreamFixtureError, '^Pinned upstream hiqlite 0.14.0 download failed$'):
                fixture.download_crate('hiqlite')
        with self.assertRaises(fixture.UpstreamFixtureError):
            fixture.NoRedirect().redirect_request(None, None, None, None, None, None)

        with patch.object(fixture, 'CRATES', pins), patch.object(fixture, 'download_crate', return_value=raw):
            with fixture.pinned_upstream() as root:
                self.assertEqual((root / 'hiqlite-0.14.0/src/lib.rs').read_bytes(), b'upstream')
            self.assertFalse(root.exists())
            with self.assertRaisesRegex(RuntimeError, 'synthetic method failure'):
                with fixture.pinned_upstream() as root:
                    raise RuntimeError('synthetic method failure')
            self.assertFalse(root.exists())
        owned = []
        original = tempfile.TemporaryDirectory
        def temporary(*args, **kwargs):
            result = original(*args, **kwargs)
            owned.append(Path(result.name))
            return result
        with patch.object(fixture.tempfile, 'TemporaryDirectory', side_effect=temporary), \
                patch.object(fixture, 'download_crate', side_effect=fixture.UpstreamFixtureError('download refused')):
            with self.assertRaises(fixture.UpstreamFixtureError):
                with fixture.pinned_upstream():
                    self.fail('failed fetch cannot enter comparison')
        self.assertTrue(owned)
        self.assertTrue(all(not path.exists() for path in owned))

        malformed = [
            [],
            [('/hiqlite-0.14.0/src/lib.rs', tarfile.REGTYPE, b'x')],
            [('hiqlite-0.14.0/../outside', tarfile.REGTYPE, b'x')],
            [('other-0.14.0/src/lib.rs', tarfile.REGTYPE, b'x')],
            [('hiqlite-0.14.0/src/..\\outside', tarfile.REGTYPE, b'x')],
            [('hiqlite-0.14.0/C:/outside', tarfile.REGTYPE, b'x')],
            [('hiqlite-0.14.0/src/link', tarfile.SYMTYPE, b'')],
            [('hiqlite-0.14.0/src/hard', tarfile.LNKTYPE, b'')],
            [('hiqlite-0.14.0/src/fifo', tarfile.FIFOTYPE, b'')],
            [('hiqlite-0.14.0/src/lib.rs', tarfile.REGTYPE, b'x')] * 2,
            [('hiqlite-0.14.0//src/lib.rs', tarfile.REGTYPE, b'x')],
            [(f'hiqlite-0.14.0/src/{i}.rs', tarfile.REGTYPE, b'x') for i in range(201)],
            [('hiqlite-0.14.0/src/lib.rs', tarfile.REGTYPE, b'x' * (fixture.MAX_EXPANDED_BYTES + 1))],
        ]
        for members in malformed:
            with self.subTest(archive=[name for name, _, _ in members][:2]), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                with self.assertRaises(fixture.UpstreamFixtureError):
                    fixture.extract_crate(crate(members), 'hiqlite', root)
                self.assertEqual(list(root.iterdir()), [])
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(fixture.UpstreamFixtureError):
                fixture.extract_crate(b'not gzip', 'hiqlite', Path(directory))

    def test_fetch_failure_is_an_ordinary_failed_method_in_complete_receipt(self):
        from test_hiqlite_patch_ledger import ForkSourceManifestCase
        class SuccessfulFixture(unittest.TestCase):
            def test_success(self):
                pass
        failed = ForkSourceManifestCase('test_manifest_matches_upstream')
        good = SuccessfulFixture('test_success')
        journal = {'version': 1, 'scope': {}, 'run': 1, 'commit': 'a' * 40,
                   'complete': False, 'fixture_errors': [], 'passes': {}}
        with tempfile.TemporaryDirectory() as directory, \
                patch.dict(os.environ, {'PLURX_HIQLITE_FETCH_UPSTREAM': '1'}):
            with patch.dict(os.environ) as environment:
                environment.pop('PLURX_HIQLITE_UPSTREAM_DIR', None)
                with patch.object(fixture, 'download_crate', side_effect=fixture.UpstreamFixtureError('synthetic fetch failure')):
                    result = receipts.execute(journal, Path(directory) / 'receipt.json',
                                              suites={'validation': [good], 'operations': [failed]})
        self.assertEqual(result, 1)
        self.assertIs(journal['complete'], True)
        self.assertEqual(journal['fixture_errors'], [])
        self.assertEqual(set(journal['passes']), {'validation:' + good.id()})
        self.assertNotIn('operations:' + failed.id(), journal['passes'])

    def test_explicit_override_is_read_only_and_local_absence_stays_offline(self):
        from test_hiqlite_patch_ledger import ForkSourceManifestCase
        test = ForkSourceManifestCase('test_manifest_matches_upstream')
        with patch.dict(os.environ) as environment, patch.object(fixture, 'download_crate') as download:
            environment.pop('PLURX_HIQLITE_UPSTREAM_DIR', None)
            environment.pop('PLURX_HIQLITE_FETCH_UPSTREAM', None)
            with self.assertRaises(unittest.SkipTest):
                test.test_manifest_matches_upstream()
            download.assert_not_called()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sentinel = root / 'caller-owned'; sentinel.write_text('retain')
            with patch.dict(os.environ, {'PLURX_HIQLITE_UPSTREAM_DIR': directory,
                                         'PLURX_HIQLITE_FETCH_UPSTREAM': '1'}), \
                    patch.object(fixture, 'download_crate') as download, \
                    patch('test_hiqlite_patch_ledger.LEDGERS', []):
                test.test_manifest_matches_upstream()
                download.assert_not_called()
            self.assertEqual(sentinel.read_text(), 'retain')
