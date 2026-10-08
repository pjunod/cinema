"""Prepare new scratch from this control and previously replayed pinned tools."""
from pathlib import Path
import json
import re
import shutil
import sys
from registered_limits import read_limits
from verify_prerequisites import verify
from registered_limits import read_json
from dependency_inventory import copy_verified

bundle, scratch, cpu, gpu = [Path(arg).resolve() for arg in sys.argv[1:]]
if scratch.exists():
    raise ValueError('new scratch directory required')

def image(path):
    value = path.read_text().strip()
    if not re.fullmatch(r'sha256:[0-9a-f]{64}',value):
        raise ValueError('actual replayed image ID required')
    return value

read_limits(bundle)
runtime = verify(bundle,cpu,gpu)
files = ['dependency_inventory.py','test_replay_contracts.py','verify_gpu_inputs.py','verify_refusals.py','registered_limits.py','dependency-lock.json','verify_prerequisites.py','preregistered-controls.json','p7-identity-input.nal','resolved-Cargo.lock',
         'generate_nonidentity.rs','generate.sh','cpu_processor.cpp','cpu_controls.sh',
         'scalar_cpu.py','prepare_frames.py','inject_rpu.py','inject_negative.py',
         'encode.sh','check_hdr10.py','check_injector.py','render.sh','scalar_gpu.py',
         'p81_nonidentity_probe.c','hdr10_baseline_probe.c','check_fixture_association.py',
         'test_nonidentity_checks.py','write_receipt.py']
for name in files:
    if not (bundle/name).is_file():
        raise FileNotFoundError(name)
scratch.mkdir()
for name in files:
    shutil.copyfile(bundle/name,scratch/name)
lock = read_json(bundle/'dependency-lock.json')
cpu_stage = scratch/'staged-cpu';gpu_stage = scratch/'staged-gpu'
copy_verified(cpu,cpu_stage,'cpu',lock['cpu_inventory'])
copy_verified(gpu,gpu_stage,'gpu',lock['gpu_inventory'])
(cpu_stage/'src/dovi_tool').rename(scratch/'libdovi-source')
(scratch/'cpu-source').mkdir()
(cpu_stage/'src/DoViBaker').rename(scratch/'cpu-source/DoViBaker')

(cpu_stage/'deps/lib').rename(scratch/'cpu-lib')
for name in ('prefix','include','lib'):
    (gpu_stage/name).rename(scratch/name)
shutil.copyfile(bundle/'resolved-Cargo.lock',scratch/'libdovi-source/Cargo.lock')
shutil.copyfile(bundle/'generate_nonidentity.rs',scratch/'libdovi-source/dolby_vision/examples/m0_nonidentity.rs')
(scratch/'runtime-inputs.json').write_text(json.dumps(runtime,indent=2)+'\n')
