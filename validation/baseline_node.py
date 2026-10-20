"""Run the exact web-unit Make recipe as diagnostic script-level test cells.

Node does not provide independent non-executing discovery of dynamically created
node:test cases. This artifact therefore counts recipe commands, not individual
JavaScript test cases. Transitive require() tests retain their existing parent
command; globbing the files would execute them twice. Every selected command is
attempted even when an earlier command fails, with raw output preserved per row.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import time

from validation.baseline_python import verify_source
from validation.test_cell_manifest import ManifestError, assess_cell, canonical_context, require


def make_commands(makefile, target='web-unit-check'):
    """Parse only the deliberately simple node recipes we actually execute.

    Refuse unfamiliar Make/shell syntax instead of dropping a command. This is
    not a general Make interpreter and must be updated if this recipe changes.
    """
    match = re.search(rf'^{re.escape(target)}:[^\n]*\n((?:\t[^\n]*\n|\n(?=\t))*)',
                      makefile, re.MULTILINE)
    require(match is not None, f'Missing Make target {target}')
    commands = []
    for line in match[1].splitlines():
        if not line.strip():
            continue
        body = re.sub(r'^[@+\-\s]*', '', line.lstrip('\t')).strip()
        if not body or body.startswith('#'):
            continue
        require(not any(token in body for token in ('\\', '$', ';', '&&', '||', '|', '>', '<', '`')),
                'Unsupported Make/shell syntax in Node recipe')
        command = shlex.split(body)
        require(command and command[0] == 'node' and len(command) > 1,
                'Every web-unit recipe command must invoke node directly')
        require(not any('*' in token or '?' in token for token in command),
                'Node script inventory must not expand globs')
        commands.append(command)
    require(commands, 'Node recipe inventory is empty')
    return commands


def collect(context, root, output_directory, *, environment=None, source_receipt=None,
            command_timeout=180, include_desktop_remote=True):
    context, _ = canonical_context(context)
    require(context['framework'] == 'node-script', 'Node context framework must be node-script')
    require(type(command_timeout) is int and command_timeout > 0, 'Invalid command timeout')
    root = Path(root).resolve()
    provenance = verify_source(context, root, os.environ if environment is None else environment,
                               source_receipt)
    text = (root / 'Makefile').read_text()
    commands = make_commands(text)
    selected = [(f'web-unit-check:{index:03d}', command)
                for index, command in enumerate(commands, 1)]
    if include_desktop_remote:
        path = 'clients/desktop-remote/tests/extension.test.mjs'
        require((root / path).is_file(), 'Missing explicitly selected desktop-remote Node test')
        selected.append(('desktop-remote-extension', ['node', '--test', path]))
    require(context.get('include_desktop_remote') is include_desktop_remote,
            'Context desktop-remote selection differs')
    output_directory = Path(output_directory)
    output_directory.mkdir(parents=True, exist_ok=True)
    outcomes, command_rows = [], []
    for index, (identity, command) in enumerate(selected, 1):
        log = output_directory / f'node-{index:03d}.log'
        started = time.monotonic()
        timed_out = False
        with log.open('wb') as stream:
            try:
                result = subprocess.run(command, cwd=root, stdout=stream, stderr=subprocess.STDOUT,
                                        timeout=command_timeout, check=False)
                returncode = result.returncode
            except subprocess.TimeoutExpired:
                returncode, timed_out = 124, True
            except OSError as error:
                stream.write((str(error) + '\n').encode())
                returncode = 127
        outcomes.append({'id': identity, 'status': 'passed' if returncode == 0 else 'failed'})
        command_rows.append({'id': identity, 'command': command, 'returncode': returncode,
                             'timed_out': timed_out, 'duration_seconds': time.monotonic() - started,
                             'log': str(log), 'log_sha256': hashlib.sha256(log.read_bytes()).hexdigest()})
        print(f'{identity}: exit={returncode} log={log}', flush=True)
    scope_errors = []
    try:
        require(verify_source(context, root, os.environ if environment is None else environment,
                              source_receipt) == provenance,
                'Source transfer receipt changed during execution')
    except ManifestError as error:
        scope_errors.append(f'Source changed during Node execution: {error}')
    report = assess_cell(context, [identity for identity, _ in selected], outcomes,
                         execution_returncode=0 if all(row['returncode'] == 0 for row in command_rows) else 1,
                         scope_errors=scope_errors)
    report.update({'source_provenance': provenance, 'granularity': 'recipe-command',
                   'makefile_sha256': hashlib.sha256(text.encode()).hexdigest(),
                   'commands': command_rows,
                   'limitation': 'Script/recipe inventory only; does not claim independent node:test case discovery.'})
    return report


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--context', type=Path, required=True)
    parser.add_argument('--source-root', type=Path, default=Path.cwd())
    parser.add_argument('--source-receipt', type=Path)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--command-timeout', type=int, default=180)
    args = parser.parse_args(argv)
    report = collect(json.loads(args.context.read_text()), args.source_root,
                     args.output.parent / 'node-logs', source_receipt=args.source_receipt,
                     command_timeout=args.command_timeout)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, sort_keys=True, indent=2) + '\n')
    return 0 if report['passed'] else 1


if __name__ == '__main__':
    try:
        raise SystemExit(main())
    except (ManifestError, OSError, json.JSONDecodeError) as error:
        raise SystemExit(f'baseline-node: {error}') from error
