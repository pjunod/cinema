#!/usr/bin/env python3
"""Exact-artifact coverage receipts; never scans a warm target for executables.

Run on the pinned Linux compiler. `select` is read-only except new receipt files.
`export` merges ONLY an explicitly supplied fresh profile directory into a NEW
profdata file. Baseline profiles, source files and Cargo caches are never changed.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys


def new_text(path, value):
    path = Path(path)
    with path.open('x') as handle:
        handle.write(value)


def file_receipt(path, full_hash=False):
    before = path.stat()
    if not stat.S_ISREG(before.st_mode):
        raise ValueError(f'not a regular object: {path}')
    digest = hashlib.sha256() if full_hash else None
    with path.open('rb') as handle:
        if handle.read(4) != b'\x7fELF':
            raise ValueError(f'not a Linux ELF coverage object: {path}')
        if digest:
            handle.seek(0)
            for chunk in iter(lambda: handle.read(1024 * 1024), b''):
                digest.update(chunk)
    after = path.stat()
    fields = ('st_dev', 'st_ino', 'st_size', 'st_mtime_ns')
    if any(getattr(before, key) != getattr(after, key) for key in fields):
        raise ValueError(f'object changed while recording: {path}')
    record = {'path': str(path), 'device': after.st_dev, 'inode': after.st_ino,
              'bytes': after.st_size, 'mtime_ns': after.st_mtime_ns}
    if digest:
        record['sha256'] = digest.hexdigest()
    return record


def default_ignore(root, target, cargo_home, rustup_home, excluded_roots):
    # cargo-llvm-cov 0.9.1 report.rs 893-978, Linux, no remap-path-prefix,
    # no dependency coverage, no custom ignore regex/vendor source directory.
    # Preserve lexical source paths, including src/../build_support aliases.
    root = re.escape(str(root))
    parts = [
        r'/rustc/([0-9a-f]+|[0-9]+\.[0-9]+\.[0-9]+)/',
        rf'^{root}(/.*)?/(tests|examples|benches)/',
        rf'^{root}(/.*)?/(tests\.rs|[0-9a-zA-Z_-]+[_-]tests\.rs)$',
        '^' + re.escape(str(target)) + r'($|/)',
        '^' + re.escape(str(cargo_home)) + r'/(registry|git)/',
        '^' + re.escape(str(Path(rustup_home) / 'toolchains')) + r'($|/)',
    ]
    parts += ['^' + re.escape(str(path)) + r'($|/)' for path in excluded_roots]
    return '|'.join(parts)


def select(args):
    if args.source_sha256 is not None and not re.fullmatch(r'[0-9a-f]{64}', args.source_sha256):
        raise ValueError('source SHA-256 must be the exact frozen archive digest')
    metadata = json.loads(Path(args.metadata).read_text())
    workspace = set(metadata['workspace_members'])
    packages = {p['id']: p for p in metadata['packages'] if p['id'] in workspace}
    excluded = {key for key, package in packages.items() if package['name'] in args.exclude}
    selected = workspace - excluded
    objects = {}
    targets = []
    finished = False
    observed = set()
    output = Path(args.output)
    output.mkdir(parents=True, exist_ok=True)
    snapshots = output / 'normal-bin-objects'
    if args.snapshot_normal_bins:
        snapshots.mkdir(exist_ok=False)
    for index, line in enumerate(Path(args.messages).read_text().splitlines(), 1):
        if not line.strip():
            continue
        record = json.loads(line)
        if record.get('reason') == 'build-finished':
            if not record.get('success'):
                raise ValueError('Cargo build-finished reported failure')
            finished = True
        if record.get('reason') != 'compiler-artifact' or record['package_id'] not in selected:
            continue
        target = record['target']
        if 'custom-build' in target['kind']:
            continue
        observed.add(record['package_id'])
        paths = []
        if record.get('executable'):
            paths.append(record['executable'])
        # Reporter also includes workspace shared-library objects. Do not add
        # rlibs/rmeta, dependency objects, build scripts or arbitrary old files.
        paths += [value for value in record.get('filenames', [])
                  if Path(value).suffix in ('.so', '.dylib', '.dll')]
        for value in paths:
            path = Path(value).absolute()
            if not path.is_file():
                raise ValueError(f'Cargo object disappeared: {path}')
            if not (path.stat().st_mode & 0o111):
                raise ValueError(f'Cargo object is not executable: {path}')
            cargo_path = path
            if args.snapshot_normal_bins and not re.search(r'-[0-9a-f]{8,}$', path.name):
                # Cargo's unhashed CARGO_BIN_EXE path is overwritten when a
                # later feature configuration builds the same normal binary.
                # Copy only these few objects into new receipts, never move,
                # hard-link or delete an object in the warm Cargo cache.
                before = file_receipt(path, True)
                path = snapshots / (cargo_path.name + '-' + before['sha256'])
                with cargo_path.open('rb') as source, path.open('xb') as destination:
                    shutil.copyfileobj(source, destination, length=1024 * 1024)
                path.chmod(cargo_path.stat().st_mode & 0o777)
                if file_receipt(cargo_path, True) != before:
                    raise ValueError(f'normal binary changed during snapshot: {cargo_path}')
                if file_receipt(path, True)['sha256'] != before['sha256']:
                    raise ValueError(f'normal binary snapshot hash differs: {cargo_path}')
            objects[str(path)] = file_receipt(path, args.hash_objects)
            targets.append({'package': packages[record['package_id']]['name'],
                            'target': target['name'], 'kind': target['kind'],
                            'test_profile': record['profile']['test'],
                            'fresh': record['fresh'], 'path': str(path),
                            'cargo_path': str(cargo_path)})
    if not finished or not objects:
        raise ValueError('need a complete successful Cargo JSON artifact stream with ELF objects')
    missing = sorted(packages[key]['name'] for key in selected - observed)
    if missing:
        raise ValueError(f'workspace packages absent from Cargo artifact stream: {missing}')
    root = Path(metadata['workspace_root'])
    target = Path(metadata['target_directory'])
    ignore = default_ignore(root, target, args.cargo_home, args.rustup_home,
                            [Path(packages[key]['manifest_path']).parent for key in sorted(excluded)])
    receipt = {'schema': 1, 'workspace_root': str(root), 'target_directory': str(target),
               'source_sha256': args.source_sha256,
               'excluded_packages': sorted(set(args.exclude)),
               'source_scope': 'cargo-llvm-cov 0.9.1 Linux defaults, no remapping/dependency coverage/custom exclusions',
               'cargo_messages_sha256': hashlib.sha256(Path(args.messages).read_bytes()).hexdigest(),
               'cargo_metadata_sha256': hashlib.sha256(Path(args.metadata).read_bytes()).hexdigest(),
               'objects': [objects[key] for key in sorted(objects)], 'targets': targets,
               'ignore_filename_regex': ignore}
    new_text(output / 'objects.json', json.dumps(receipt, indent=2) + '\n')
    new_text(output / 'objects.txt', ''.join(key + '\n' for key in sorted(objects)))
    new_text(output / 'ignore-regex.txt', ignore + '\n')
    print(json.dumps({'objects': len(objects), 'targets': len(targets), 'receipt': str(output / 'objects.json')}))


def export(args):
    receipts = [json.loads(Path(path).read_text()) for path in args.objects]
    receipt = receipts[0]
    if any(other['ignore_filename_regex'] != receipt['ignore_filename_regex'] for other in receipts):
        raise ValueError('object variants have different source exclusions')
    if len(receipts) > 1:
        identities = {other.get('source_sha256') for other in receipts}
        if None in identities or len(identities) != 1:
            raise ValueError('combining feature variants requires one identical frozen source archive SHA-256')
    records = {}
    for variant in receipts:
        for record in variant['objects']:
            old = records.get(record['path'])
            if old is not None and old != record:
                raise ValueError(f'object path was overwritten between feature variants: {record["path"]}')
            records[record['path']] = record
    records = [records[key] for key in sorted(records)]
    objects = [Path(record['path']) for record in records]
    # Ensure no concurrent rebuild swapped an object after manifest capture.
    for record, path in zip(records, objects):
        current = file_receipt(path, 'sha256' in record)
        if current != record:
            raise ValueError(f'object changed since Cargo manifest capture: {path}')
    output = Path(args.output)
    output.mkdir(parents=True, exist_ok=True)
    report = output / 'coverage.json'
    if report.exists():
        raise ValueError(f'refusing to overwrite report: {report}')
    if args.profdata:
        profdata = Path(args.profdata)
        if args.profiles:
            raise ValueError('choose existing profdata OR fresh raw profile pool')
    else:
        if not args.profiles:
            raise ValueError('supply an explicit fresh profile pool')
        pool = Path(args.profiles).resolve()
        target = Path(receipt['target_directory']).resolve()
        if pool == target or pool.is_relative_to(target):
            raise ValueError('refusing to merge a warm Cargo target profile pool')
        profiles = sorted(pool.glob('*.profraw'))
        if not profiles:
            raise ValueError('fresh profile pool has no .profraw files')
        profdata = output / 'exact.profdata'
        if profdata.exists():
            raise ValueError(f'refusing to overwrite profdata: {profdata}')
        profile_list = output / 'profiles.txt'
        new_text(profile_list, ''.join(str(path) + '\n' for path in profiles))
        with (output / 'merge.log').open('x') as log:
            subprocess.run([args.llvm_profdata, 'merge', '-sparse', '-failure-mode=all',
                            '-f', str(profile_list), '-o', str(profdata)],
                           stdout=log, stderr=log, check=True)
    command = [args.llvm_cov, 'export', '-format=text', '-instr-profile=' + str(profdata)]
    for path in objects:
        command += ['-object', str(path)]
    command += ['-ignore-filename-regex', receipt['ignore_filename_regex']]
    if not args.full:
        command += ['-summary-only']
    new_text(output / 'export-command.json', json.dumps(command, indent=2) + '\n')
    with report.open('x') as handle, (output / 'export.log').open('x') as log:
        subprocess.run(command, stdout=handle, stderr=log, check=True)
    result = json.loads(report.read_text())
    lines = result['data'][0]['totals']['lines']
    print(json.dumps({'report': str(report), 'lines': lines}))
    if args.compare:
        baseline = json.loads(Path(args.compare).read_text())
        before = baseline['data'][0]
        after = result['data'][0]
        totals_equal = before['totals'] == after['totals']
        before_files = {item['filename']: item['summary'] for item in before['files']}
        after_files = {item['filename']: item['summary'] for item in after['files']}
        files_equal = before_files == after_files
        def counters(value):
            if isinstance(value, dict):
                return {key: counters(item) for key, item in value.items() if key != 'percent'}
            return value
        counters_equal = counters(before['totals']) == counters(after['totals'])
        file_counters_equal = counters(before_files) == counters(after_files)
        print(json.dumps({'baseline_totals_exact': totals_equal,
                          'baseline_per_file_summaries_exact': files_equal,
                          'baseline_integer_totals_exact': counters_equal,
                          'baseline_per_file_integer_summaries_exact': file_counters_equal,
                          'added_files': sorted(after_files.keys() - before_files.keys()),
                          'removed_files': sorted(before_files.keys() - after_files.keys())}))
        # cargo-llvm-cov parses/reserializes LLVM JSON, and some file-level
        # percentage floats differ by one last bit. Exact count/covered
        # counters for every file, rather than float formatting, prove scope.
        if not counters_equal or not file_counters_equal:
            raise SystemExit(2)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command', required=True)
    choose = sub.add_parser('select')
    choose.add_argument('--messages', required=True)
    choose.add_argument('--metadata', required=True)
    choose.add_argument('--cargo-home', required=True)
    choose.add_argument('--rustup-home', required=True)
    choose.add_argument('--exclude', action='append', default=['plurx-cluster-check'], choices=['plurx-cluster-check'])
    choose.add_argument('--hash-objects', action='store_true')
    choose.add_argument('--snapshot-normal-bins', action='store_true')
    choose.add_argument('--source-sha256', help='SHA-256 of the exact frozen source archive shared by feature variants')
    choose.add_argument('--output', required=True)
    choose.set_defaults(run=select)
    report = sub.add_parser('export')
    report.add_argument('--objects', required=True, action='append', help='repeat for additional feature configurations from identical frozen source')
    report.add_argument('--profiles')
    report.add_argument('--profdata', help='existing read-only profile, e.g. baseline validation; never merged')
    report.add_argument('--llvm-profdata', required=True)
    report.add_argument('--llvm-cov', required=True)
    report.add_argument('--output', required=True)
    report.add_argument('--full', action='store_true')
    report.add_argument('--compare')
    report.set_defaults(run=export)
    args = parser.parse_args()
    args.run(args)


if __name__ == '__main__':
    main()
