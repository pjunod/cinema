"""Measure discovered test identities against outcomes for one candidate and cell.

This diagnostic format is not authenticated historical evidence. The adapter must
run discovery and execution on the same source/artifact and capture their return
codes. Use one libtest binary per cell: names alone are not workspace-unique.
No adapter here imports tests, runs discovery, or executes test bodies.
"""
from __future__ import annotations

import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import re

SCHEMA_VERSION = 1
STATUSES = frozenset({'passed', 'failed', 'error', 'skipped', 'ignored'})
CONTEXT_STRINGS = ('cell_id', 'framework', 'target', 'profile', 'toolchain')
MAX_CASES = 100000


class ManifestError(ValueError):
    """Malformed or incomplete runtime test evidence."""


def require(condition, message):
    if not condition:
        raise ManifestError(message)


def identifier(value):
    require(isinstance(value, str) and value.strip() == value and value,
            'Test identity must be a nonempty trimmed string')
    require(not any(ord(char) < 32 for char in value), 'Control character in test identity')
    return value


def canonical_context(context):
    require(isinstance(context, dict), 'Cell context must be an object')
    for key in ('candidate_sha', 'candidate_tree'):
        require(isinstance(context.get(key), str) and re.fullmatch('[0-9a-f]{40}', context[key]),
                f'Invalid {key}')
    for key in CONTEXT_STRINGS:
        identifier(context.get(key))
    features = context.get('features')
    require(isinstance(features, list), 'Cell features must be an explicit list')
    for feature in features:
        identifier(feature)
    require(len(features) == len(set(features)), 'Duplicate cell feature')
    normalized = dict(context, features=sorted(features))
    try:
        encoded = json.dumps(normalized, sort_keys=True, separators=(',', ':'), allow_nan=False)
        return json.loads(encoded), hashlib.sha256(encoded.encode()).hexdigest()
    except (ValueError, TypeError) as error:
        raise ManifestError(f'Non-JSON cell context: {error}') from error


def libtest_inventory(text):
    """Normalize a compiled binary's --list --format=terse output.

    The caller must separately pass the actual discovery exit code to assess_cell.
    A legitimate zero-test binary is permitted and remains visible as zero cases.
    """
    require(isinstance(text, str), 'libtest listing must be text')
    return [identifier(line[:-6]) for line in text.splitlines() if line.endswith(': test')]


def libtest_outcomes(text):
    """Normalize ordinary libtest results from a single test binary's log.

    Capture ordinary libtest format with output capture enabled. Arbitrary test
    stdout under --nocapture can impersonate harness rows and is not supported.
    """
    require(isinstance(text, str), 'libtest outcome log must be text')
    rows = []
    pattern = re.compile(r'^test (\S+) \.\.\. (ok|FAILED|ignored)(?:, .*)?$')
    for line in text.splitlines():
        match = pattern.fullmatch(line)
        if match:
            rows.append({'id': identifier(match[1]), 'status': {
                'ok': 'passed', 'FAILED': 'failed', 'ignored': 'ignored'}[match[2]]})
    return rows


def assess_cell(context, discovered, outcomes, *, discovery_returncode=0,
                execution_returncode=0, exceptions=(), scope_errors=()):
    """Return exact identity accounting, preserving failed/incomplete evidence.

    Exceptions must identify a single discovered skipped/ignored case and carry
    a nonempty owner and reason. Missing results cannot be excused. Evidence is
    complete if discovery succeeded and every identity has exactly one outcome;
    passing additionally requires a zero execution exit code and no failures or
    unqualified skips. Never drop an incomplete cell from a suite aggregate.
    """
    context, digest = canonical_context(context)
    require(isinstance(scope_errors, (list, tuple)) and len(scope_errors) <= MAX_CASES,
            'Scope errors must be a bounded list')
    scope_errors = [identifier(error) for error in scope_errors]
    require(type(discovery_returncode) is int and type(execution_returncode) is int,
            'Discovery and execution return codes must be integers')
    require(isinstance(discovered, list) and len(discovered) <= MAX_CASES,
            'Discovery must be a bounded identity list')
    require(isinstance(outcomes, list) and len(outcomes) <= MAX_CASES,
            'Outcomes must be a bounded case list')
    inventory = Counter(identifier(name) for name in discovered)
    result_counts = Counter()
    normalized_results = []
    status_counts = Counter()
    for row in outcomes:
        require(isinstance(row, dict) and set(row) == {'id', 'status'},
                'Outcome requires exactly id and status')
        name = identifier(row['id'])
        status = row['status']
        require(isinstance(status, str) and status in STATUSES, 'Unknown outcome status')
        result_counts[name] += 1
        status_counts[status] += 1
        normalized_results.append({'id': name, 'status': status})
    require(isinstance(exceptions, (list, tuple)) and len(exceptions) <= MAX_CASES,
            'Exceptions must be a bounded list')
    exemptions = {}
    for row in exceptions:
        require(isinstance(row, dict) and set(row) == {'id', 'owner', 'reason'},
                'Exception requires exactly id, owner and reason')
        name = identifier(row['id'])
        identifier(row['owner'])
        identifier(row['reason'])
        require(name in inventory, 'Exception names an undiscovered case')
        require(name not in exemptions, 'Duplicate exception identity')
        exemptions[name] = dict(row)
    missing = sorted(inventory.keys() - result_counts.keys())
    unexpected = sorted(result_counts.keys() - inventory.keys())
    duplicate_discovery = sorted(name for name, count in inventory.items() if count != 1)
    duplicate_outcomes = sorted(name for name, count in result_counts.items() if count != 1)
    skips = {row['id'] for row in normalized_results if row['status'] in {'ignored', 'skipped'}}
    require(set(exemptions) <= skips, 'Exception must describe an actually skipped or ignored case')
    unqualified_skips = sorted(skips - exemptions.keys())
    complete = (discovery_returncode == 0 and not scope_errors and not missing and not unexpected
                and not duplicate_discovery and not duplicate_outcomes)
    passed = (complete and execution_returncode == 0 and not status_counts['failed']
              and not status_counts['error'] and not unqualified_skips)
    return {
        'schema_version': SCHEMA_VERSION,
        'context': context,
        'context_sha256': digest,
        'discovery_returncode': discovery_returncode,
        'execution_returncode': execution_returncode,
        'discovered': sorted(discovered),
        'outcomes': sorted(normalized_results, key=lambda row: (row['id'], row['status'])),
        'exceptions': sorted(exemptions.values(), key=lambda row: row['id']),
        'counts': {'discovered': len(discovered), 'outcomes': len(outcomes),
                   **{status: status_counts[status] for status in sorted(STATUSES)}},
        'missing': missing,
        'unexpected': unexpected,
        'duplicate_discovery': duplicate_discovery,
        'duplicate_outcomes': duplicate_outcomes,
        'unqualified_skips': unqualified_skips,
        'scope_errors': scope_errors,
        'complete': complete,
        'passed': passed,
    }


def require_complete(manifest):
    require(manifest.get('complete') is True, 'Runtime test evidence is incomplete')
    return manifest


def aggregate_cells(manifests, expected_contexts):
    """Require the declared cells, including cells with failed discovery or no tests."""
    require(isinstance(manifests, list) and isinstance(expected_contexts, list),
            'Cells and expected contexts must be lists')
    expected = {}
    sources = set()
    for context in expected_contexts:
        normalized, digest = canonical_context(context)
        name = normalized['cell_id']
        sources.add((normalized['candidate_sha'], normalized['candidate_tree']))
        require(name not in expected, 'Duplicate expected cell')
        expected[name] = digest
    require(expected, 'Expected cell inventory cannot be empty')
    require(len(sources) == 1, 'A suite aggregate must bind one candidate and source tree')
    by_name = {}
    for manifest in manifests:
        require(isinstance(manifest, dict) and manifest.get('schema_version') == SCHEMA_VERSION,
                'Unsupported cell manifest')
        context, digest = canonical_context(manifest.get('context'))
        name = context['cell_id']
        require(name not in by_name, 'Duplicate cell result')
        require(name in expected and digest == expected[name], 'Unexpected or differently bound cell')
        # Recompute, rather than trusting a producer-supplied complete/passed flag.
        validated = assess_cell(context, manifest.get('discovered'), manifest.get('outcomes'),
                                discovery_returncode=manifest.get('discovery_returncode'),
                                execution_returncode=manifest.get('execution_returncode'),
                                exceptions=manifest.get('exceptions'),
                                scope_errors=manifest.get('scope_errors', []))
        require(manifest.get('context_sha256') == digest, 'Cell context digest mismatch')
        by_name[name] = validated
    missing_cells = sorted(expected.keys() - by_name.keys())
    complete = not missing_cells and all(row['complete'] for row in by_name.values())
    return {'schema_version': SCHEMA_VERSION, 'expected_cells': sorted(expected),
            'missing_cells': missing_cells, 'cells': [by_name[name] for name in sorted(by_name)],
            'complete': complete, 'passed': complete and all(row['passed'] for row in by_name.values())}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--input', type=Path, required=True,
                        help='JSON assess_cell arguments, with context/discovered/outcomes')
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args(argv)
    report = assess_cell(**json.loads(args.input.read_text()))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, sort_keys=True, indent=2) + '\n')
    print(f"{report['context']['cell_id']}: discovered={report['counts']['discovered']} "
          f"complete={report['complete']} passed={report['passed']}")
    return 0 if report['passed'] else 1


if __name__ == '__main__':
    try:
        raise SystemExit(main())
    except (ManifestError, OSError, json.JSONDecodeError) as error:
        raise SystemExit(f'cell-manifest: {error}') from error
