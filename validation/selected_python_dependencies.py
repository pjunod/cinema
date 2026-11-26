"""One source-bound NumPy test dependency, staged privately before selected bodies."""
from __future__ import annotations
import io
import json
import os
from pathlib import Path, PurePosixPath
import platform
import re
import stat
import subprocess
import sys
import tempfile
import urllib.request
import zipfile
from validation import promotion_unit_retention as retention
from validation import python_unit_receipts as r

CASE = 'operations:test_codec_hdr_cvvdp.CodecHdrCvvdpTests.test_rgb16_worker_known_channel_and_normalization_control'
TEST_SOURCE = 'tests/operations/test_codec_hdr_cvvdp.py'
TEST_SHA256 = '7715e4b7e2e0c7031eac8d1099b207d44bb002b77c7984fd440041c59503b6eb'
URL = 'https://files.pythonhosted.org/packages/8c/3d/1e1db36cfd41f895d266b103df00ca5b3cbe965184df824dec5c08c6b803/numpy-2.2.6-cp312-cp312-manylinux_2_17_x86_64.manylinux2014_x86_64.whl'
SIZE = 16527618
PROFILE = {'policy': 'selected-cvvdp-rgb16-numpy-v1', 'case': CASE, 'numpy_version': '2.2.6',
           'wheel_sha256': 'fd83c01228a688733f1ded5201c678f0c53ecc1006ffbc404db9f7a899ac6249',
           'tree_sha256': '6876f83bdb2e70d70e019f3073445771b24284758a08bc6a4369e44532b845b2',
           'platform': 'linux', 'machine': 'x86_64', 'python_abi': 'cpython-312'}
STAGING = Path('target/validation/python-deps')
ROOTS = {'numpy', 'numpy.libs', 'numpy-2.2.6.dist-info'}
RUNTIME_FIELDS = {'profile', 'root', 'python_sha256', 'python_version', 'python_implementation',
                  'platform', 'machine', 'numpy_version', 'numpy_file', 'tree_sha256', 'openblas_threads'}


def requirements(plan, candidate):
    if plan.get('comparison') is None or CASE.split(':', 1)[1] not in plan['suites']['operations']['uncovered']:
        return None
    raw = retention.git_bytes(candidate, TEST_SOURCE)
    r.require(raw is not None and retention.digest(raw) == TEST_SHA256,
              'NumPy profile is outside the reviewed packing-case source')
    return dict(PROFILE)


def host_profile():
    r.require(sys.implementation.name == 'cpython' and sys.version_info[:2] == (3, 12)
              and sys.platform == 'linux' and platform.machine() == 'x86_64',
              'Selected NumPy case requires its pinned CPython312 Linux x86_64 profile')


def archive_files(raw):
    r.require(len(raw) == SIZE and retention.digest(raw) == PROFILE['wheel_sha256'],
              'Selected public NumPy wheel differs from the official pin')
    result = {}
    with zipfile.ZipFile(io.BytesIO(raw)) as archive:
        entries = archive.infolist()
        r.require(0 < len(entries) <= 4096 and sum(row.file_size for row in entries) <= 128 * 1024 * 1024,
                  'Oversized NumPy wheel closure')
        for row in entries:
            path = PurePosixPath(row.filename)
            r.require(not path.is_absolute() and '..' not in path.parts and path.parts
                      and path.parts[0] in ROOTS and not stat.S_ISLNK(row.external_attr >> 16)
                      and row.file_size <= 64 * 1024 * 1024,
                      'Unsafe NumPy wheel member')
            if row.is_dir():
                continue
            r.require(row.filename not in result, 'Repeated NumPy wheel member')
            result[row.filename] = archive.read(row)
    hashes = {name: retention.digest(value) for name, value in result.items()}
    digest = retention.digest(json.dumps(hashes, sort_keys=True, separators=(',', ':')).encode())
    r.require(digest == PROFILE['tree_sha256'] and len(result) == 876,
              'Official NumPy package closure differs')
    return result


def tree_hash(root):
    hashes = {}
    total = 0
    for path in root.rglob('*'):
        r.require(not path.is_symlink(), 'NumPy dependency acquired a symlink')
        if path.is_file():
            name = path.relative_to(root).as_posix()
            r.require(name.split('/')[0] in ROOTS, 'Unexpected NumPy package root')
            total += path.stat().st_size
            r.require(len(hashes) < 876 and total <= 128 * 1024 * 1024
                      and path.stat().st_size <= 64 * 1024 * 1024, 'Oversized staged NumPy closure')
            hashes[name] = retention.digest(path.read_bytes())
    return retention.digest(json.dumps(hashes, sort_keys=True, separators=(',', ':')).encode())


def validate_runtime(value, expected):
    if expected is None:
        r.require(value is None, 'Unexpected selected Python dependency')
        return
    r.require(expected == PROFILE and isinstance(value, dict) and set(value) == RUNTIME_FIELDS
              and value['profile'] == PROFILE and value['tree_sha256'] == PROFILE['tree_sha256']
              and value['platform'] == 'linux' and value['machine'] == 'x86_64'
              and value['python_implementation'] == 'cpython'
              and isinstance(value['python_version'], str)
              and re.fullmatch(r'3\.12\.\d+', value['python_version']) is not None
              and value['numpy_version'] == '2.2.6' and value['numpy_file'] == 'numpy/__init__.py'
              and value['openblas_threads'] == 1 and isinstance(value['root'], str)
              and Path(value['root']).is_absolute()
              and isinstance(value['python_sha256'], str)
              and retention.DIGEST.fullmatch(value['python_sha256']) is not None,
              'Selected Python dependency runtime identity differs')


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        raise r.ReceiptError('Selected public dependency redirect refused')


def stage(expected, environment):
    r.require(expected == PROFILE, 'Unknown selected Python dependency profile')
    host_profile()
    STAGING.mkdir(parents=True, exist_ok=True)
    parent = STAGING.resolve(strict=True)
    root = Path(tempfile.mkdtemp(prefix='selected-python-numpy-', dir=parent))
    with urllib.request.build_opener(NoRedirect).open(URL, timeout=60) as response:
        raw = response.read(SIZE + 1)
    files = archive_files(raw)
    for name, value in files.items():
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(value)
    r.require(tree_hash(root) == PROFILE['tree_sha256'], 'Staged NumPy bytes changed')
    # Import/ABI eligibility only. The packing assertions remain one real test.
    code = '''import hashlib,json,pathlib,platform,sys
root=pathlib.Path(sys.argv[1]).resolve();sys.path.insert(0,str(root))
import numpy
origin=pathlib.Path(numpy.__file__).resolve()
assert origin==root/'numpy/__init__.py'
print(json.dumps(dict(python_sha256=hashlib.sha256(pathlib.Path(sys.executable).resolve().read_bytes()).hexdigest(),
python_version=platform.python_version(),python_implementation=sys.implementation.name,platform=sys.platform,
machine=platform.machine(),numpy_version=numpy.__version__,numpy_file=str(origin.relative_to(root)))))'''
    probe = subprocess.run([sys.executable, '-I', '-B', '-c', code, str(root)],
                           env={**environment, 'OPENBLAS_NUM_THREADS': '1'},
                           capture_output=True, timeout=30)
    r.require(probe.returncode == 0, 'Pinned NumPy cannot import before selected bodies')
    result = r.bounded_json(probe.stdout)
    runtime = {**result, 'profile': dict(PROFILE), 'root': str(root),
               'tree_sha256': tree_hash(root), 'openblas_threads': 1}
    validate_runtime(runtime, expected)
    r.atomic_json(STAGING.parent / 'python-dependency-profile.json', runtime)
    return runtime


def activate(plan, kind):
    expected, runtime = plan.get('dependency_profile'), plan.get('dependency_runtime')
    validate_runtime(runtime, expected)
    if expected is None:
        return
    host_profile()
    root = Path(runtime['root'])
    parent = STAGING.resolve(strict=True)
    r.require(root.parent == parent and root.name.startswith('selected-python-numpy-')
              and root.resolve(strict=True) == root and tree_hash(root) == PROFILE['tree_sha256']
              and retention.digest(Path(sys.executable).resolve().read_bytes()) == runtime['python_sha256'],
              'Selected NumPy source or interpreter changed after eligibility check')
    if kind != 'operations':
        return
    r.require(CASE.split(':', 1)[1] in plan['suites'][kind]['uncovered'] and 'numpy' not in sys.modules,
              'NumPy dependency activation escaped its only selected case')
    os.environ['OPENBLAS_NUM_THREADS'] = '1'
    sys.dont_write_bytecode = True
    sys.path.insert(0, str(root))
    import numpy
    r.require(numpy.__version__ == PROFILE['numpy_version']
              and Path(numpy.__file__).resolve() == root / 'numpy/__init__.py',
              'Selected NumPy import came from another environment')


def finish(plan):
    runtime = plan.get('dependency_runtime')
    if runtime is not None:
        r.require(tree_hash(Path(runtime['root'])) == PROFILE['tree_sha256'],
                  'NumPy package closure changed during selected execution')
