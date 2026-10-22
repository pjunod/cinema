"""Synthetic navigation boundary tests; no paid model or frozen study cases."""
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import sys
import tempfile
import unittest

from validation.knowledge.evaluation import CaseSpec, EvaluationError, VisibleFile
from validation.knowledge.evaluation_navigation import (
    NavigationConfig, NavigatorAssets, VisibleRipwire,
)

ROOT = Path(__file__).resolve().parents[2]


def assets():
    binary = b'synthetic executable bytes, not a release qualification'
    key = {'Darwin': 'macos', 'Linux': 'linux'}[platform.system()] + '-' + {
        'arm64': 'arm64', 'aarch64': 'arm64', 'x86_64': 'x64'}[platform.machine()]
    digest = hashlib.sha256(binary).hexdigest()
    lock = json.dumps({'schema_version': 1, 'upstream': 'redhat-et/ripwire', 'version': '0.5.0',
                       'source_commit': 'a' * 40, 'platforms': {key: {'archive_sha256': 'b' * 64}}}).encode()
    installation = json.dumps({'version': '0.5.0', 'platform': key, 'source_commit': 'a' * 40,
                              'archive_sha256': 'b' * 64, 'binary_sha256': digest}).encode()
    adapter = b'''import hashlib,json,pathlib,subprocess,sys
root=pathlib.Path(__file__).resolve().parent.parent
metadata=json.loads(next((root/'target/ripwire/tools/0.5.0').glob('*/install.json')).read_bytes())
head=subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip()
print(json.dumps({'binary_sha256':metadata['binary_sha256'],'head':head,'root':str(root)}),file=sys.stderr)
if sys.argv[-1]=='large': print('\\u03bb'*10000)
else: print(sys.argv[-1])
'''
    return NavigatorAssets(adapter, lock, binary, installation, hashlib.sha256(adapter).hexdigest(),
                           hashlib.sha256(lock).hexdigest(), digest)


def case():
    raw = 'def visible():\n    return 1\n\n\n'
    file = VisibleFile('src/example.py', raw, ((1, 2),), 'sha256:' + 'c' * 64)
    return CaseSpec('synthetic', 'routing', 'investigation', 'Read the public function.', (file,))


class NavigationBoundaryTests(unittest.TestCase):
    def setUp(self):
        base = ROOT / 'target/agent-fixtures/.hermes/hdr-collaboration/workspaces/navigation-tests'
        base.mkdir(parents=True, exist_ok=True)
        self.owned = tempfile.TemporaryDirectory(dir=base, prefix='owner-')
        self.addCleanup(self.owned.cleanup)
        self.config = NavigationConfig(Path(self.owned.name), assets(),
                                       Path(shutil.which('git')), Path(sys.executable))

    def test_materialization_contains_only_visible_bytes_and_synthetic_git(self):
        source = case()
        # Defense in depth even if a trusted caller bypasses dataclass validation.
        object.__setattr__(source.files[0], 'content', 'def visible():\n    return 1\nHIDDEN_MARKER\nSECRET_BODY\n')
        nav = VisibleRipwire(self.config, source)
        try:
            self.assertEqual((nav.root / 'src/example.py').read_text(), 'def visible():\n    return 1\n\n\n')
            self.assertFalse((nav.root / '.git/config').read_text().find('[remote ') >= 0)
            self.assertNotIn('ANTHROPIC_API_KEY', nav.environment)
            self.assertEqual(nav('find', 'visible')['status'], 'ok')
            self.assertEqual(nav('body', 'src/example.py:visible')['text'].strip(), 'src/example.py:visible')
        finally:
            root = nav.root
            nav.close()
        self.assertFalse(root.exists())
        self.assertEqual(len(list((Path(self.owned.name) / 'audit').glob('*.json'))), 1)

    def test_response_bound_counts_utf8_and_discloses_truncation(self):
        nav = VisibleRipwire(self.config, case())
        try:
            answer = nav('find', 'large')
            self.assertEqual(answer['status'], 'partial')
            self.assertGreater(answer['omitted_utf8_bytes'], 0)
            self.assertLessEqual(len(json.dumps(answer, sort_keys=True, separators=(',', ':'),
                                                 ensure_ascii=False).encode()), 8192)
            self.assertTrue(answer['text'].startswith('\u03bb'))
        finally:
            nav.close()

    def test_selectors_cannot_read_control_files_or_host_paths(self):
        nav = VisibleRipwire(self.config, case())
        try:
            for mode, selector in [('body', '/etc/passwd:x'), ('body', '../source.py:x'),
                                   ('body', '.git/config:x'), ('body', 'scripts/ripwire:x'),
                                   ('body', 'target/home:x'), ('body', 'symbol_without_file'),
                                   ('find', 'file:///etc/passwd'), ('find', 'bad\nselector')]:
                with self.subTest(mode=mode, selector=selector):
                    with self.assertRaises(EvaluationError):
                        nav(mode, selector)
        finally:
            nav.close()

    def test_membership_source_and_executable_mutations_refuse(self):
        for mutation in ('source', 'new-member', 'binary', 'symlink'):
            with self.subTest(mutation=mutation):
                nav = VisibleRipwire(self.config, case())
                try:
                    source = nav.root / 'src/example.py'
                    if mutation == 'source':
                        source.chmod(0o600)
                        source.write_text('def changed():\n    return 2\n')
                    elif mutation == 'new-member':
                        (nav.root / 'unexpected.py').write_text('pass\n')
                    elif mutation == 'binary':
                        binary = nav.install / 'ripwire'
                        binary.chmod(0o700)
                        binary.write_bytes(b'changed tool')
                    else:
                        source.unlink()
                        source.symlink_to(ROOT / 'README.md')
                    with self.assertRaises((EvaluationError, OSError)):
                        nav('find', 'visible')
                finally:
                    nav.close()

    def test_invalid_asset_hash_and_control_collision_refuse_before_materialization(self):
        changed = assets()
        object.__setattr__(changed, 'binary', b'not pinned')
        with self.assertRaises(EvaluationError):
            VisibleRipwire(NavigationConfig(Path(self.owned.name), changed,
                                           Path(shutil.which('git')), Path(sys.executable)), case())
        file = VisibleFile('scripts/ripwire', 'pass\n', ((1, 1),), 'sha256:' + 'd' * 64)
        with self.assertRaises(EvaluationError):
            VisibleRipwire(self.config, CaseSpec('collision', 'routing', 'investigation', 'Synthetic.', (file,)))
        self.assertFalse(list(Path(self.owned.name).glob('navigation-run-*')))
