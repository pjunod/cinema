"""Run the uncovered tests with Cargo's actual per-target execution environment."""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
OUTPUT = ROOT / 'target/validation/workspace-rust'
SELECTION = OUTPUT.with_name('workspace-rust-selection.json')
WORKSPACE = ['--workspace', '--locked', '--exclude', 'plurx-cluster-check']
RUNNER_VARIABLE = 'CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER'


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def tree_inventory(lines):
    names = set()
    for line in lines:
        if not line or re.fullmatch(r'\d+ tests?, \d+ benchmarks?', line):
            continue
        require(line.endswith(': test'), f'Unexpected compiled test inventory row: {line[:120]}')
        name = line.removesuffix(': test')
        require(name and name not in names and not name.startswith('-'), 'Ambiguous native test identity')
        names.add(name)
    return names


def verify_observed(proof, key, universe, ignored):
    observed = set(proof['observed'].get(key, []))
    must_pass = set(proof['must_pass'].get(key, []))
    require(observed <= universe, 'Historical Rust body disappeared from compiled target ' + key)
    require(not must_pass & ignored, 'Historical failed or invalidated Rust body became ignored in ' + key)


def exact_selection(universe, retained):
    require(retained <= universe, 'A retained Rust body is missing from the compiled target')
    missing = universe - retained
    if not retained or not missing:
        return missing, []
    if len(retained) < len(missing):
        selector = ['--exact']
        for name in sorted(retained):
            selector.extend(['--skip', name])
    else:
        selector = ['--exact', *sorted(missing)]
    return missing, selector


def compiled_targets():
    result = subprocess.run(['cargo','test',*WORKSPACE,'--no-fail-fast','--no-run','--message-format=json'],
                            cwd=ROOT, stdout=subprocess.PIPE, check=True, text=True)
    metadata = json.loads(subprocess.check_output(['cargo','metadata','--locked','--no-deps',
                          '--format-version','1'], cwd=ROOT))
    packages = {p['id']:Path(p['manifest_path']).parent for p in metadata['packages']}
    targets = {}
    for line in result.stdout.splitlines():
        event = json.loads(line)
        if event.get('reason') == 'compiler-message':
            continue
        if event.get('reason') != 'compiler-artifact' or not event['profile']['test'] or not event['executable']:
            continue
        binary = Path(event['executable']).resolve()
        name = re.fullmatch(r'([a-zA-Z0-9_]+)-[0-9a-f]{16}', binary.name)
        require(name and binary.is_file() and event['package_id'] in packages, 'Unrecognized Cargo test executable')
        source = Path(event['target']['src_path']).relative_to(packages[event['package_id']])
        key = name[1] + '@' + str(source)
        require(key not in targets.values() and str(binary) not in targets, 'Duplicate compiled Cargo target')
        targets[str(binary)] = key
    require(targets, 'No compiled Cargo test artifacts')
    return targets


def list_body(command, env):
    output = subprocess.check_output([*command,'--list','--format','terse'],text=True,env=env)
    return tree_inventory(output.splitlines())


def run_target(binary):
    require(len(sys.argv) == 3, 'Unexpected Cargo test arguments')
    file = str(Path(binary).resolve(strict=True))
    targets = json.loads((OUTPUT / 'targets.json').read_text())
    key = targets[file]
    proof = json.loads(SELECTION.read_text())
    retained = set(proof['retained'].get(key, []))
    env = dict(os.environ)
    env.pop(RUNNER_VARIABLE, None)
    universe = list_body([file],env)
    ignored = list_body([file,'--ignored'],env) if proof['must_pass'].get(key) else set()
    verify_observed(proof,key,universe,ignored)
    missing, selector = exact_selection(universe,retained)
    print(f'{key}: {len(universe)} discovered, {len(retained)} authenticated passes, {len(missing)} selected',flush=True)
    if missing:
        require(list_body([file,*selector],env) == missing, 'Compiled libtest selection differs')
        subprocess.run([file,*selector],env=env,check=True)
    marker = OUTPUT / (hashlib.sha256(key.encode()).hexdigest() + '.json')
    with marker.open('x') as stream:
        json.dump({'target':key,'discovered':len(universe),'executed':len(missing),'retained':len(retained)},stream)


def docs_list(command):
    result = subprocess.run([*command,'--list','--format','terse'],cwd=ROOT,
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,check=True)
    sections = {}
    for block in re.split(r'(?m)^\s*Doc-tests (\w+)[ \t]*$', re.sub(r'\x1b\[[0-9;]*[A-Za-z]','',result.stdout))[1:]:
        if re.fullmatch(r'\w+',block):
            key = 'doc:' + block
            require(key not in sections,'Duplicate doctest package')
            sections[key] = set()
        else:
            rows = [line for line in block.splitlines() if line.endswith(': test')]
            sections[key] = tree_inventory(rows)
    require(sections,'Missing workspace doctest inventory')
    return sections


def run_docs(proof):
    retained = proof['retained']
    cmd = ['cargo','test',*WORKSPACE,'--doc','--no-fail-fast','--']
    sections = docs_list(cmd)
    used, missing = set(), set()
    ignored = docs_list([*cmd,'--ignored']) if any(proof['must_pass'].get(key) for key in sections) else {}
    for key, names in sections.items():
        verify_observed(proof,key,names,ignored.get(key,set()))
        known = set(retained.get(key, []))
        remain, _ = exact_selection(names, known)
        used.update(known)
        missing.update(remain)
    require(not used & missing,'Same doctest name has mixed applicability across packages')
    if missing:
        selector = ['--exact',*sorted(missing)]
        filtered = docs_list([*cmd,*selector])
        require(filtered == {key: names & missing for key,names in sections.items()},
                'Compiled doctest selection differs')
        subprocess.run([*cmd,*selector],cwd=ROOT,check=True)
    print(f'Workspace doctests: {len(used)} authenticated passes, {len(missing)} executed')
    return set(sections)


def main():
    if len(sys.argv) > 1 and sys.argv[1] == '--target':
        run_target(sys.argv[2])
        return
    proof = json.loads(SELECTION.read_text())
    require(os.geteuid() == 0 and sys.platform == 'linux' and os.uname().machine == 'x86_64'
            and os.environ.get('RUST_TEST_THREADS') == '2' and os.environ.get('CARGO_BUILD_JOBS') == '1'
            and not os.environ.get('RUST_MIN_STACK') and not os.environ.get(RUNNER_VARIABLE)
            and subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip() == proof['candidate'],
            'Rust continuation runtime or frozen source differs')
    from validation.qualification_unit_continuation import require_unmodified_cargo_environment
    require_unmodified_cargo_environment(os.environ)
    require(not any(os.environ.get(k) for k in ('RUSTDOCFLAGS','CARGO_ENCODED_RUSTDOCFLAGS',
            'RUSTDOC','CARGO_BUILD_RUSTDOC','CARGO_BUILD_TARGET_DIR')),
            'Unexpected Rust workspace compiler environment')
    compiler = subprocess.check_output(['rustc','-vV'],text=True).splitlines()
    require('commit-hash: 8bab26f4f68e0e26f0bb7960be334d5b520ea452' in compiler
            and 'host: x86_64-unknown-linux-gnu' in compiler, 'Rust compiler differs from the retained execution')
    OUTPUT.mkdir(parents=True,exist_ok=False)
    subprocess.run(['make','fmt-check','spike-lock-check','lint','test-socket-permission-check'],cwd=ROOT,check=True)
    targets = compiled_targets()
    (OUTPUT / 'targets.json').write_text(json.dumps(targets))
    env = {**os.environ,RUNNER_VARIABLE:shlex.join([sys.executable,str(Path(__file__).resolve()),'--target'])}
    subprocess.run(['cargo','test',*WORKSPACE,'--no-fail-fast','--lib','--bins','--tests'],cwd=ROOT,env=env,check=True)
    completed = {json.loads(p.read_text())['target'] for p in OUTPUT.glob('*.json') if p.name != 'targets.json'}
    require(completed == set(targets.values()),'Cargo did not execute the complete compiled workspace target set')
    docs = run_docs(proof)
    require(set(proof['observed']) <= completed | docs, 'Historical Rust target disappeared')
    subprocess.run(['make','vodencode-restart-check','CARGO=cargo'],cwd=ROOT,check=True)


if __name__ == '__main__':
    main()
