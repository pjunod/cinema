#!/usr/bin/env python3
"""Select an affected augmented core replay after an independently passing default.

This narrows execution only. Full Cargo object manifests remain unchanged.
"""
import hashlib
import json
from pathlib import Path
import re
import subprocess


PREFIXES = ('cluster::', 'store::hiqlite')
EXPECTED_ADDITIONS = {
    'plurx-core': {'cluster-read-cost-validation', 'cluster-validation',
                   'hiqlite-contract-tests'},
    'plurxd': {'cluster-integration-tests'},
    'hiqlite': {'validation-test-helpers'},
}


def cargo_features(path):
    """Keep every observed feature set, including build/test profile variants."""
    packages = {}
    complete = False
    for line in Path(path).read_text().splitlines():
        if not line.strip():
            continue
        record = json.loads(line)
        if record.get('reason') == 'build-finished':
            if record.get('success') is not True:
                raise ValueError('Cargo build failed: ' + str(path))
            complete = True
        if record.get('reason') != 'compiler-artifact':
            continue
        package = record['package_id']
        manifest = Path(record['manifest_path'])
        if manifest.name != 'Cargo.toml':
            raise ValueError('unexpected Cargo manifest: ' + str(manifest))
        item = packages.setdefault(package, {'manifest': str(manifest), 'sets': set(), 'variants': {}})
        if item['manifest'] != str(manifest):
            raise ValueError('package manifest changed in artifact stream')
        features = frozenset(record['features'])
        item['sets'].add(features)
        variant = json.dumps({'target': record.get('target'),
                              'profile': record.get('profile')}, sort_keys=True)
        item['variants'].setdefault(variant, set()).add(features)
    if not complete or not packages:
        raise ValueError('incomplete Cargo artifact stream: ' + str(path))
    return packages


def audit_feature_delta(default_messages, augmented_messages, workspace_metadata):
    before = cargo_features(default_messages)
    after = cargo_features(augmented_messages)
    if before.keys() != after.keys():
        raise ValueError('package graph changed outside the reviewed feature-only boundary')
    named = {p['id']: p['name'] for p in workspace_metadata['packages']}
    changes = []
    observed = set()
    for package in sorted(before):
        first, second = before[package], after[package]
        if first['manifest'] != second['manifest']:
            raise ValueError('source manifest changed: ' + package)
        name = named.get(package)
        # --no-deps metadata omits the vendored Hiqlite package. Accept only
        # the exact frozen workspace vendor path, never an arbitrary namesake.
        vendor_manifest = Path(workspace_metadata['workspace_root']) / 'vendor/hiqlite/Cargo.toml'
        if Path(second['manifest']) == vendor_manifest:
            if name not in (None, 'hiqlite'):
                raise ValueError('unexpected identity for vendored Hiqlite')
            name = 'hiqlite'
        additions = EXPECTED_ADDITIONS.get(name, set())
        expected_sets = {features | additions for features in first['sets']}
        # Compare corresponding target/profile artifacts too; a package-wide
        # union must not hide feature redistribution between existing variants.
        if first['variants'].keys() - second['variants'].keys():
            raise ValueError('existing Cargo target/profile disappeared: ' + package)
        for variant, features in first['variants'].items():
            if second['variants'][variant] != {values | additions for values in features}:
                raise ValueError('unreviewed feature change for Cargo target/profile: ' + package)
        if second['sets'] != expected_sets:
            raise ValueError('unreviewed feature change for ' + package + ': ' + repr({
                'default': sorted(map(sorted, first['sets'])),
                'augmented': sorted(map(sorted, second['sets'])),
                'expected_additions': sorted(additions),
            }))
        if additions:
            if name in observed:
                raise ValueError('ambiguous package identity for ' + name)
            observed.add(name)
            # Each addition must really be absent in every default variant.
            if any(features & additions for features in first['sets']):
                raise ValueError('default already contains augmented features: ' + name)
            changes.append({'package': name, 'package_id': package,
                            'default_feature_sets': sorted(map(sorted, first['sets'])),
                            'augmented_feature_sets': sorted(map(sorted, second['sets'])),
                            'added_features': sorted(additions)})
    if observed != EXPECTED_ADDITIONS.keys():
        raise ValueError('required feature delta missing: ' + repr(EXPECTED_ADDITIONS.keys() - observed))
    return changes


def core_object(receipt):
    found = {target['path'] for target in receipt['targets']
             if target['package'] == 'plurx-core' and target['target'] == 'plurx_core'
             and 'lib' in target['kind'] and target['test_profile']}
    if len(found) != 1:
        raise ValueError('missing or ambiguous exact core harness: ' + repr(found))
    path = Path(next(iter(found)))
    records = [record for record in receipt['objects'] if record['path'] == str(path)]
    if len(records) != 1 or 'sha256' not in records[0]:
        raise ValueError('core object must have one full hash receipt')
    with path.open('rb') as stream:
        actual = hashlib.file_digest(stream, 'sha256').hexdigest()
    if actual != records[0]['sha256']:
        raise ValueError('core harness changed after Cargo manifest capture: ' + str(path))
    return path, actual


def listed_names(output):
    names = [line[:-6] for line in output.splitlines() if line.endswith(': test')]
    if len(names) != len(set(names)):
        raise ValueError('duplicate core test names')
    if not re.search(r'^\d+ tests?, \d+ benchmarks?$', output, re.MULTILINE):
        raise ValueError('core --list output has no libtest summary')
    return set(names)


def list_core(binary, cwd, env, ignored=False):
    command = ['timeout', '--kill-after=10s', '120s', str(binary), '--list']
    if ignored:
        command.append('--ignored')
    result = subprocess.run(command, cwd=cwd, env=env, text=True,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True)
    return listed_names(result.stdout)


def choose_names(default, augmented):
    if not default or not augmented:
        raise ValueError('empty core test inventory')
    if default - augmented:
        raise ValueError('augmented variant removed default core tests: ' + repr(sorted(default - augmented)))
    added = augmented - default
    selected = {name for name in augmented if name.startswith(PREFIXES)} | added
    if not selected:
        raise ValueError('affected replay selected zero core tests')
    return selected, added


def prepare_selection(campaign, expanded, metadata, env, reviewed_scope=None):
    campaign, expanded = Path(campaign), Path(expanded)
    if (campaign / 'test-exit-status.txt').read_text().strip() != '0':
        raise ValueError('affected replay requires the full default suite to have passed')
    default_receipt = json.loads((campaign / 'objects-default/objects.json').read_text())
    augmented_receipt = json.loads((expanded / 'objects/objects.json').read_text())
    if not default_receipt.get('source_sha256') or default_receipt['source_sha256'] != augmented_receipt.get('source_sha256'):
        raise ValueError('default and augmented harnesses must have identical frozen source')
    feature_delta = audit_feature_delta(campaign / 'artifacts.jsonl',
                                       expanded / 'artifacts.jsonl', metadata)
    default_binary, default_hash = core_object(default_receipt)
    augmented_binary, augmented_hash = core_object(augmented_receipt)
    cwd = next(str(Path(p['manifest_path']).parent) for p in metadata['packages'] if p['name'] == 'plurx-core')
    default = list_core(default_binary, cwd, env)
    augmented = list_core(augmented_binary, cwd, env)
    default_ignored = list_core(default_binary, cwd, env, ignored=True)
    augmented_ignored = list_core(augmented_binary, cwd, env, ignored=True)
    if not default_ignored <= default or not augmented_ignored <= augmented:
        raise ValueError('ignored names absent from full inventory')
    if default_ignored != augmented_ignored & default:
        raise ValueError('existing core tests changed ignored status')
    selected, added = choose_names(default, augmented)
    source_scope = None
    full_replay = False
    if reviewed_scope is not None:
        from source_scope import compare_scope
        source_scope = compare_scope(metadata['workspace_root'], reviewed_scope)
        full_replay = not source_scope['matches']
        if full_replay:
            selected = augmented
    if not selected - augmented_ignored:
        raise ValueError('affected replay selected no runnable core tests')
    record = {
        'schema': 1,
        'source_sha256': default_receipt['source_sha256'],
        'scope': ('full augmented core run because reviewed source inputs changed' if full_replay
                  else 'affected augmented core replay after full passing default; not a full augmented core run'),
        'execution': 'full' if full_replay else 'affected',
        'prefixes': [] if full_replay else list(PREFIXES), 'feature_delta': feature_delta,
        'source_scope_audit': source_scope,
        'default_binary': str(default_binary), 'default_binary_sha256': default_hash,
        'augmented_binary': str(augmented_binary), 'augmented_binary_sha256': augmented_hash,
        'default_names': sorted(default), 'augmented_names': sorted(augmented),
        'default_ignored': sorted(default_ignored), 'augmented_ignored': sorted(augmented_ignored),
        'added_names': sorted(added), 'selected_names': sorted(selected),
        'selected_runnable_names': sorted(selected - augmented_ignored),
        'selected_ignored_names': sorted(selected & augmented_ignored),
        'omitted_unaffected_names': sorted(augmented - selected),
        'object_scope': 'all default and augmented Cargo objects retained by unchanged exporter',
    }
    path = expanded / 'core-replay-selection.json'
    with path.open('x') as handle:
        json.dump(record, handle, indent=2)
        handle.write('\n')
    print(json.dumps({'core_replay_receipt': str(path), 'selected': len(selected),
                      'runnable': len(selected - augmented_ignored), 'added': len(added),
                      'omitted_unaffected': len(augmented - selected)}), flush=True)
    return record
