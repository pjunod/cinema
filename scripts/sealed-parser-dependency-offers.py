#!/usr/bin/env python3
"""Retain authenticated source offers for the sealed probe's static providers.

This supplements provenance, never compiles or replaces an executable. The
recorded compile recipe remains intact. The Bookworm amd64 shipping path calls
this automatically; other existing parser paths retain their current contract.
"""
import argparse
import hashlib
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import time
import urllib.request
import urllib.parse


def sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def load_archive_owner():
    path = Path(__file__).with_name('prepare-linux-dolby-sdk')
    loader = importlib.machinery.SourceFileLoader('sealed_archive_owner', str(path))
    module = importlib.util.module_from_spec(importlib.util.spec_from_loader(loader.name, loader))
    loader.exec_module(module)
    return module


def safe_member(name):
    value = Path(name)
    if value.is_absolute() or '..' in value.parts or str(value) != name or not name:
        raise ValueError('unsafe static-provider member')
    return value


def link_contract(config):
    values = {}
    for line in config.splitlines():
        match = re.fullmatch(r'([A-Za-z0-9_-]+)=(.*)', line)
        if match:
            values[match[1]] = match[2]
    if '-static' not in values.get('LDEXEFLAGS', '').split():
        raise ValueError('dependency offers require the actual static link contract')
    if values.get('CC') not in ['gcc', '/usr/bin/gcc'] or values.get('LD') not in ['gcc', '/usr/bin/gcc']:
        raise ValueError('unsupported static compiler/linker authority')
    # These are recorded family link requirements, not an assertion that every
    # archive member in a provider package was included in the final executable.
    return sorted({name for key, value in values.items() if key.startswith('EXTRALIBS')
                   for name in re.findall(r'(?:^|\s)-l([A-Za-z0-9_+.-]+)', value)})


def verify_installed_members(providers, system_root):
    records = {}
    for role, row in providers.items():
        records[role] = {}
        for name, claim in row['installed_members'].items():
            member = safe_member(name)
            if not name.startswith('usr/lib/'):
                raise ValueError('static provider is outside its distribution library namespace')
            path = system_root / member
            if path.is_symlink() or not path.is_file() or path.stat().st_size != claim['bytes'] or sha(path) != claim['sha256']:
                raise ValueError('installed static provider differs from its authenticated package: ' + name)
            records[role][name] = claim
    return records


def resolve_copyright(name, regular, links):
    path = 'usr/share/doc/' + name + '/copyright'
    original = path
    chain, seen = [], set()
    while path not in regular:
        if path in seen:
            raise ValueError('cyclic copyright archive alias')
        seen.add(path)
        parent = next((key for key in links if path == key or path.startswith(key + '/')), None)
        if parent is None:
            raise ValueError('missing authenticated copyright endpoint')
        target = links[parent]
        safe_member(target)
        chain.append({'member': parent, 'archive_target': target})
        path = target + path[len(parent):]
    return regular[path], {'original_member': original, 'regular_endpoint': path, 'original_aliases': chain}


def expected_inputs(lock):
    rows = {}
    packages = {name: row['package'] for name, row in lock['providers'].items()}
    packages.update(lock.get('copyright_packages', {}))
    for package in packages.values():
        row = {'url': package['url'], 'sha256': package['sha256'], 'bytes': package['bytes']}
        name = package['archive']
        if name in rows and rows[name] != row:
            raise ValueError('conflicting static-provider archive identity')
        rows[name] = row
    for provider in lock['providers'].values():
        for name, row in provider['source_offer']['files'].items():
            if name in rows and rows[name] != row:
                raise ValueError('conflicting source-offer identity')
            rows[name] = row
    for name, row in rows.items():
        if safe_member(name).name != name or not re.fullmatch(r'[0-9a-f]{64}', row['sha256']) or not 0 < row['bytes'] <= 128 * 1024**2:
            raise ValueError('invalid immutable static-provider input')
        url = urllib.parse.urlsplit(row['url'])
        if url.scheme != 'https' or url.netloc != 'deb.debian.org' or Path(url.path).name != name:
            raise ValueError('static-provider input is not its canonical public archive')
    return rows


def obtain_inputs(rows, directory, offline, deadline):
    directory.mkdir(parents=True, exist_ok=True)
    total = 0
    for name, row in rows.items():
        path = directory / name
        if not path.exists():
            if offline:
                raise ValueError('missing retained static-provider archive: ' + name)
            with urllib.request.urlopen(row['url'], timeout=min(60, max(1, deadline-time.monotonic()))) as response, path.open('xb') as output:
                while block := response.read(1024 * 1024):
                    total += len(block)
                    if total > 128 * 1024**2 or time.monotonic() >= deadline or output.tell()+len(block) > row['bytes']:
                        raise ValueError('static-provider download exceeds its finite input budget')
                    output.write(block)
        if path.is_symlink() or not path.is_file() or path.stat().st_size != row['bytes'] or sha(path) != row['sha256']:
            raise ValueError('static-provider input checksum/size mismatch: ' + name)


def emit(parser, provenance, inputs, offline, seconds):
    if not 1 <= seconds <= 600:
        raise ValueError('finite static-provider deadline required')
    release = Path('/etc/os-release').read_text()
    if platform.system() != 'Linux' or platform.machine() not in ['x86_64', 'amd64'] or 'ID=debian' not in release or 'VERSION_ID="12"' not in release:
        raise ValueError('this source offer profile is Bookworm amd64 only')
    provenance = provenance.resolve(strict=True)
    parser = parser.resolve(strict=True)
    original_bytes = (provenance / 'manifest.json').read_bytes()
    original = json.loads(original_bytes)
    if 'static_dependencies' in original or sha(parser) != original['executable_sha256'] or original['build_recipe_sha256'] != sha(provenance / 'build-static-ffprobe'):
        raise ValueError('parser image/recorded compile recipe changed or offer already attached')
    program = subprocess.check_output(['/usr/bin/readelf', '-lW', str(parser)], text=True, timeout=30)
    if 'INTERP' in program:
        raise ValueError('sealed parser has an executable interpreter')
    if re.search(r'\bDYNAMIC\b', program):
        raise ValueError('sealed parser has a dynamic segment')
    lock_path = Path(__file__).with_name('sealed-parser-dependency-sources.json')
    lock = json.loads(lock_path.read_text())
    if lock.get('schema_version') != 1 or lock.get('platform') != {'distribution': 'debian', 'version': '12', 'architecture': 'amd64'}:
        raise ValueError('unsupported static-provider lock schema/platform')
    if set(lock['providers']) != {'libc6-dev', 'libgcc-12-dev', 'zlib1g-dev', 'libbz2-dev', 'liblzma-dev'}:
        raise ValueError('static-provider family inventory changed')
    if set(lock.get('copyright_packages', {})) != {'gcc-12-base', 'libbz2-1.0'}:
        raise ValueError('copyright endpoint provider inventory changed')
    names = link_contract((provenance / 'config.mak').read_text())
    covered = {name for row in lock['providers'].values() for name in row['link_names']}
    if not set(names) <= covered or not {'libc6-dev', 'libgcc-12-dev'} <= set(lock['providers']):
        raise ValueError('recorded link dependency is missing an authenticated source provider')
    rows = expected_inputs(lock)
    obtain_inputs(rows, inputs, offline, time.monotonic()+seconds)
    owner = load_archive_owner()
    packages = {name: row['package'] for name, row in lock['providers'].items()}
    packages.update(lock.get('copyright_packages', {}))
    regular, aliases, controls = {}, {}, {}
    for name, package in packages.items():
        (files, links), (control, control_links) = owner.package_members((inputs / package['archive']).read_bytes(), allow_unresolved=True)
        fields = owner.control_fields(control['control'])
        if control_links or fields.get('Package') != name or fields.get('Version') != package['version'] or fields.get('Architecture') not in ['amd64', 'all']:
            raise ValueError('actual provider package metadata differs from its role')
        actual = subprocess.check_output(['/usr/bin/dpkg-query', '-W', '-f=${Version}', name], text=True, timeout=30)
        if actual != package['version']:
            raise ValueError('installed provider version differs from the authenticated source offer')
        controls[name] = control['control']
        for member, data in files.items():
            if member.startswith('usr/share/doc/') and member.endswith('/copyright'):
                if member in regular and regular[member] != data:
                    raise ValueError('copyright endpoint collision')
                regular[member] = data
        for member, target in links.items():
            if member.startswith('usr/share/doc/'):
                if member in aliases and aliases[member] != target:
                    raise ValueError('copyright alias collision')
                aliases[member] = target
        if name in lock['providers']:
            provider = lock['providers'][name]
            source = fields.get('Source', name)
            match = re.fullmatch(r'([^ ]+)(?: \(([^)]+)\))?', source)
            offer = provider['source_offer']
            if match is None or (match[1], match[2] or fields['Version']) != (offer['package'], offer['version']):
                raise ValueError('binary/source version association differs')
            owner.validate_debian_source_offer(offer, {n:(inputs/n).read_bytes() for n in offer['files']})
            for member, claim in provider['installed_members'].items():
                if member not in files or len(files[member]) != claim['bytes'] or hashlib.sha256(files[member]).hexdigest() != claim['sha256']:
                    raise ValueError('static-member claim differs from actual package bytes')
    members = verify_installed_members(lock['providers'], Path('/'))
    output = provenance / 'static-dependencies'
    output.mkdir()  # Failed evidence remains; never overwrite an earlier offer.
    shutil.copy2(lock_path, output / 'source-lock.json')
    shutil.copy2(Path(__file__), output / 'dependency-offer-recipe.py')
    shutil.copy2(Path(__file__).with_name('prepare-linux-dolby-sdk'), output / 'archive-owner.py')
    (output / 'original-parser-manifest.json').write_bytes(original_bytes)
    archive_dir = output / 'originals'
    archive_dir.mkdir()
    for name in rows:
        shutil.copy2(inputs / name, archive_dir / name)
    copyrights = {}
    for name, data in controls.items():
        folder = output / 'packages' / name
        folder.mkdir(parents=True)
        (folder / 'control').write_bytes(data)
        text, witness = resolve_copyright(name, regular, aliases)
        (folder / 'copyright').write_bytes(text)
        copyrights[name] = {**witness, 'sha256': hashlib.sha256(text).hexdigest()}
    inventory = {str(p.relative_to(output)): {'sha256': sha(p), 'bytes': p.stat().st_size} for p in output.rglob('*') if p.is_file()}
    facts = {'schema_version': 1, 'mode': 'supplement-recorded-parser', 'parser_sha256': sha(parser),
             'original_compile_recipe_sha256': original['build_recipe_sha256'],
             'original_parser_manifest_sha256': hashlib.sha256(original_bytes).hexdigest(),
             'recorded_link_config_sha256': sha(provenance / 'config.mak'), 'recorded_link_names': names,
             'provider_catalog': members, 'copyrights': copyrights, 'files': inventory,
             'scope': 'Authenticated provider catalog for the recorded static link; not every catalog member is claimed to be linked.'}
    (output / 'manifest.json').write_text(json.dumps(facts, indent=2)+'\n')
    original['static_dependencies'] = {'path': 'static-dependencies/manifest.json', 'sha256': sha(output/'manifest.json')}
    (provenance / 'manifest.json').write_text(json.dumps(original, indent=2)+'\n')
    return facts


def verify_offer(parser, provenance):
    parent = json.loads((provenance / 'manifest.json').read_text())
    pointer = parent.get('static_dependencies', {})
    if pointer.get('path') != 'static-dependencies/manifest.json':
        raise ValueError('missing canonical static-provider offer')
    output = provenance / 'static-dependencies'
    if sha(output / 'manifest.json') != pointer.get('sha256'):
        raise ValueError('static-provider manifest changed')
    if output.is_symlink() or not output.is_dir():
        raise ValueError('static-provider provenance root must be a real directory')
    facts = json.loads((output / 'manifest.json').read_text())
    if facts.get('schema_version') != 1 or facts.get('mode') != 'supplement-recorded-parser':
        raise ValueError('unsupported static-provider offer schema/mode')
    if facts['parser_sha256'] != sha(parser) or facts['parser_sha256'] != parent['executable_sha256']:
        raise ValueError('static offer belongs to a different parser')
    if facts['original_compile_recipe_sha256'] != parent['build_recipe_sha256'] or sha(provenance / 'build-static-ffprobe') != parent['build_recipe_sha256']:
        raise ValueError('original compile authority changed')
    if facts['recorded_link_config_sha256'] != sha(provenance / 'config.mak'):
        raise ValueError('recorded static link contract changed')
    if any(p.is_symlink() for p in output.rglob('*')):
        raise ValueError('static-provider provenance must contain regular bytes')
    actual = {str(p.relative_to(output)) for p in output.rglob('*') if p.is_file() and p != output / 'manifest.json'}
    if actual != set(facts['files']):
        raise ValueError('static-provider provenance inventory differs')
    for name, claim in facts['files'].items():
        path = output / safe_member(name)
        if path.stat().st_size != claim['bytes'] or sha(path) != claim['sha256']:
            raise ValueError('static-provider provenance bytes changed: ' + name)
    original = (output / 'original-parser-manifest.json').read_bytes()
    if hashlib.sha256(original).hexdigest() != facts['original_parser_manifest_sha256']:
        raise ValueError('original parser manifest changed')
    old = json.loads(original)
    unchanged = dict(parent)
    unchanged.pop('static_dependencies')
    if unchanged != old:
        raise ValueError('offer supplement altered the recorded parser compilation')
    return facts


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--parser', type=Path, required=True)
    parser.add_argument('--provenance', type=Path, required=True)
    parser.add_argument('--inputs', type=Path)
    parser.add_argument('--verify', action='store_true')
    parser.add_argument('--offline', action='store_true')
    parser.add_argument('--deadline-seconds', type=int, default=180)
    args = parser.parse_args()
    if args.verify:
        facts = verify_offer(args.parser, args.provenance)
    else:
        if args.inputs is None:
            parser.error('emission requires an explicit owned input directory')
        facts = emit(args.parser, args.provenance, args.inputs, args.offline, args.deadline_seconds)
        verify_offer(args.parser, args.provenance)
    print(json.dumps({'state':'static-provider-offers-recorded', 'parser_sha256':facts['parser_sha256']}))


if __name__ == '__main__':
    main()
