"""Validate current CPU controls and bind replay artifacts to a fresh receipt."""
import hashlib
import json
import platform
from pathlib import Path
import re

from check_cpu_outputs import verify

ROOT = Path(__file__).resolve().parent
EXCLUDED = {'src', 'build', 'deps', '__pycache__', 'portable-evidence'}


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def artifacts():
    return {
        str(path.relative_to(ROOT)): digest(path)
        for path in ROOT.rglob('*')
        if path.is_file()
        and not any(part in EXCLUDED for part in path.relative_to(ROOT).parts)
        and path.name not in {'SHA256SUMS.json', 'receipt.json', 'replay-receipt.json', 'cpu_processor'}
        and not path.name.endswith('.tgz')
    }


if __name__ == '__main__':
    controls = verify(ROOT / 'cpu-outputs', ROOT / 'cpu-reference', ROOT / 'negative-controls.json')
    build_log = (ROOT / 'build.log').read_text()
    compiler = re.search(r'^rustc (.+)$', build_log, re.MULTILINE).group(1)
    if not compiler.startswith('1.97.1 '):
        raise ValueError('Unexpected compiler version')
    image_id = (ROOT / 'replay-image-id.txt').read_text().strip()
    receipt = {
        'schema': 'm0-baker-cpu-replay-v2',
        'result': 'pass',
        'scope': controls['scope'],
        'runtime': {
            'requested_container_platform': 'linux/amd64',
            'observed_container_uname': build_log.splitlines()[0],
            'replay_client_system': platform.system(),
            'replay_client_architecture': platform.machine(),
            'execution_mode': None,  # Client architecture does not prove Docker-host emulation.
            'base_image': 'rust@sha256:408fe88047cef61a2087653b0c5255fa51c0f2d6d94ddedd7a2562a9b91a46f6',
            'builder_image': image_id,
            'builder_recipe': 'Dockerfile',
            'rustc': compiler,
            'cpus': 2,
            'memory_bytes': 2147483648,
            'plugin_loaded_into_avisynth': False,
        },
        'reproducibility': {
            'entry_point': 'bash replay.sh in a fresh bundle copy without src/',
            'public_sources': json.loads((ROOT / 'dependency-pins.json').read_text()),
            'source_lock': 'resolved-Cargo.lock',
            'lock_origin': 'upstream lock plus recorded workspace/CAPI/dev-dependency resolution; original graph was not --locked buildable',
            'lock_resolution': 'lockfile-resolution.patch',
            'live_repository_limits': 'Debian apt and PyPI are live; cmake version is pinned but OS packages and package transport are not fully pinned. Fresh tooling image IDs may differ. Exact observed versions are retained.',
            'package_snapshots': ['debian-packages.tsv', 'python-build-packages.txt'],
            'docker_context': 'Dockerfile-only via .dockerignore; only bundle scratch is mounted at /work',
        },
        'commands': ['bash replay.sh', 'bash /work/build.sh', 'bash /work/controls.sh'],
        'controls': controls,
        'binary_sha256': {
            'libdovi.a': digest(ROOT / 'deps/lib/libdovi.a'),
            'libdovibaker..so': digest(ROOT / 'build/libdovibaker..so'),
            'cpu_processor': digest(ROOT / 'cpu_processor'),
        },
        'independent_dv_reference': None,
        'p81_conformance': None,
        'reference_fidelity_improvement': None,
        'remaining': [
            'AviSynth GetFrame/chroma resampling not tested',
            'HEVC BL/EL decoding and timestamp pairing not tested',
            'Nonstandard rgb_to_lms metadata ignored; unrestricted source conformance fails',
            'Independent DV-on and DV-off HDR10 base reference validation unavailable',
            'Real-time performance unmeasured',
        ],
        'artifacts': artifacts(),
    }
    text = json.dumps(receipt, indent=2) + '\n'
    (ROOT / 'receipt.json').write_text(text)
    (ROOT / 'replay-receipt.json').write_text(text)
    ledger = artifacts()
    ledger['receipt.json'] = digest(ROOT / 'receipt.json')
    ledger['replay-receipt.json'] = digest(ROOT / 'replay-receipt.json')
    (ROOT / 'SHA256SUMS.json').write_text(json.dumps(ledger, indent=2) + '\n')
    print(json.dumps({'result': 'pass', 'receipt_sha256': digest(ROOT / 'receipt.json')}))
