"""Verify exact reviewed prerequisite bytes and Docker identities before replay."""
from pathlib import Path
import hashlib
import json
import subprocess
import re
from registered_limits import read_json
from dependency_inventory import validate


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def verify_files(root, expected):
    for name, sha in expected.items():
        path = root/name
        if not path.is_file() or digest(path) != sha:
            raise ValueError('prerequisite file identity differs: '+name)


def verify_image(image, architecture):
    result = subprocess.run(['docker','image','inspect',image],check=True,capture_output=True,text=True)
    records = json.loads(result.stdout)
    if len(records) != 1 or records[0]['Id'] != image or records[0]['Os'] != 'linux' or records[0]['Architecture'] != architecture:
        raise ValueError('actual prerequisite Docker identity/platform differs')


EXPECTED_LOCK_SHA256 = '7e9231d7ab33c33839e5c78a223e7395ef95f280d59eb4256f263c89ade44ba7'


def verify(bundle, cpu, gpu):
    if digest(bundle/'dependency-lock.json') != EXPECTED_LOCK_SHA256:
        raise ValueError('immutable prerequisite lock differs')
    lock = read_json(bundle/'dependency-lock.json')
    cpu_receipt = read_json(cpu/'receipt.json')
    if cpu_receipt['schema'] != 'm0-baker-cpu-replay-v2' or cpu_receipt['result'] != 'pass' or cpu_receipt['runtime']['rustc'] != '1.97.1 (8bab26f4f 2026-07-14)':
        raise ValueError('CPU prerequisite receipt/toolchain differs')
    pins = {item['repo']:item['sha'] for item in cpu_receipt['reproducibility']['public_sources']}
    if pins.get('erazortt/DoViBaker') != 'ffba39830b694ddca0bf5f73dcf1b462713bb7f4' or pins.get('quietvoid/dovi_tool') != '83e1fdad6dcd5995556235946e7c5c0f9010d5a1':
        raise ValueError('reviewed CPU source pins differ')
    validate(cpu,'cpu',lock['cpu_inventory']);validate(gpu,'gpu',lock['gpu_inventory'])
    if cpu_receipt['binary_sha256']['libdovi.a'] != digest(cpu/'deps/lib/libdovi.a'):
        raise ValueError('CPU static library does not match original build receipt')
    gpu_receipt = read_json(gpu/'parsed-build-identity.json')
    if gpu_receipt['status'] != 'passed' or gpu_receipt['platform'] != 'linux/arm64' or gpu_receipt['libplacebo_library_sha256'] != digest(gpu/'prefix/lib/aarch64-linux-gnu/libplacebo.so.374'):
        raise ValueError('GPU library does not match original build receipt')
    cpu_image = (cpu/'replay-image-id.txt').read_text().strip()
    gpu_image = (gpu/'image-id.txt').read_text().strip()
    if not re.fullmatch(r'sha256:[0-9a-f]{64}',cpu_image) or not re.fullmatch(r'sha256:[0-9a-f]{64}',gpu_image):
        raise ValueError('full actual image digest required')
    if cpu_receipt['runtime']['builder_image'] != cpu_image or gpu_receipt['image_id'] != gpu_image:
        raise ValueError('prerequisite declared image identity differs')
    verify_image(cpu_image,'amd64');verify_image(gpu_image,'arm64')
    return {'cpu_image':cpu_image,'gpu_image':gpu_image,'dependencies_verified':True,
            'provenance':'Exact reviewed source/header/library snapshot hashes with matching pinned-source build receipt fields; actual caller-supplied image IDs/platform checked, not runtime attestation',
            'cpu_prerequisite_receipt_sha256':digest(cpu/'receipt.json'),
            'gpu_prerequisite_receipt_sha256':digest(gpu/'parsed-build-identity.json')}
