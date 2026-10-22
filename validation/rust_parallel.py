"""Manual M3 execution pilot; fresh native results never replace promotion evidence.

Build and discover once. Compare the ordinary Cargo command with three bounded
libtest lanes on the same executable bytes. Doctests and the two explicit serial
regressions remain required. This adapter does not qualify other platform lanes.
"""
from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import runpy
import re
import signal
import subprocess
import sys
import threading
import time

from validation.baseline_commands import run_process
from validation.baseline_rust_inventory import discover, git_identity, sha256
from validation.rust_host_admission import acquire, inspect_environment, quarantine
from validation.rust_parallel_manifest import BINDINGS, build_plan, digest, validate_receipts
from validation.rust_repair_evidence import load_policy, parse_bound, one_suite
from validation.baseline_timing import ANSI, DOC_TESTS

NATIVE = ['cargo', 'test', '--workspace', '--locked', '--exclude', 'plurx-cluster-check', '--no-fail-fast']
SERIAL = [
    'vodserve::tests::encoded_vod_vfr_input_is_sampled_on_the_declared_rational_grid',
    'vodserve::tests::encoded_vod_bitmap_burn_restores_cues_that_predate_video_seek_landing',
]
SOURCE_BUNDLE = None


def write(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + '.tmp')
    temporary.write_text(json.dumps(value, indent=2, sort_keys=True) + '\n')
    temporary.replace(path)


def read(path):
    return json.loads(Path(path).read_text())


def require(condition, message):
    if not condition:
        raise ValueError(message)


def check_inputs(inventory, binary=None):
    if SOURCE_BUNDLE is None:
        actual = git_identity()
    else:
        from validation.rust_source_bundle import identity
        actual = identity(Path.cwd(), SOURCE_BUNDLE)
    require(actual == inventory['source'], 'source changed since discovery')
    require(inventory['discovery_complete'] and not inventory['errors'], 'discovery incomplete')
    selected = inventory['binaries']
    if binary is not None:
        selected = [row for row in selected if row['executable'] == binary['executable']]
        require(len(selected) == 1, 'planned binary absent or ambiguous in discovery')
        original = selected[0]
        require(digest({key: original[key] for key in BINDINGS}) == digest({key: binary[key] for key in BINDINGS})
                and sorted(original['cases'], key=lambda row: row['name']) == binary['cases'],
                'planned binary differs from independent discovery')
    for binary in selected:
        require(binary['listing_complete'] and sha256(binary['executable']) == binary['sha256'],
                'inventoried executable changed: ' + binary['target']['name'])


def command(argv, output, name, timeout, lease_fd, *, combined=True, cancel_event=None):
    output = Path(output)
    output.mkdir(parents=True, exist_ok=True)
    stdout_path, stderr_path = output / (name + '.log'), output / (name + '.stderr')
    # Native stack and body selection match the qualified ordinary cell.
    argv = ['env', '-u', 'RUST_MIN_STACK', 'RUST_TEST_THREADS=4', *argv]
    with stdout_path.open('wb') as stdout:
        if combined:
            result = run_process(argv, stdout, stdout, timeout, pass_fds=(lease_fd,), cancel_event=cancel_event)
        else:
            with stderr_path.open('wb') as stderr:
                result = run_process(argv, stdout, stderr, timeout, pass_fds=(lease_fd,), cancel_event=cancel_event)
    if result.get('cleanup_complete') is not True and cancel_event is not None:
        # Record unsafe ownership before any log I/O can itself fail.
        cancel_event.unsafe_cleanup = True
        cancel_event.set()
    result.update(argv=argv, log=stdout_path.name, log_sha256=sha256(stdout_path))
    write(output / (name + '.process.json'), result)
    return result, stdout_path.read_text(errors='replace')


def safe(result):
    return (result.get('success') is True and result.get('returncode') == 0
            and result.get('cleanup_complete') is True and result.get('timed_out') is False)


def doctest_targets(metadata):
    require(metadata['version'] == 1, 'unsupported Cargo metadata')
    packages = {p['id']: p for p in metadata['packages']}
    members = set(metadata['workspace_members'])
    require(members and members <= packages.keys(), 'workspace metadata incomplete')
    targets = [target['name'].replace('-', '_') for identity in sorted(members)
               if packages[identity]['name'] != 'plurx-cluster-check'
               for target in packages[identity]['targets']
               if target.get('doctest') is True and set(target['kind']) &
               {'lib', 'rlib', 'dylib', 'cdylib', 'staticlib', 'proc-macro'}]
    require(targets and len(set(targets)) == len(targets), 'ambiguous or empty doctest target selection')
    return targets


def parse_doctests(text, expected):
    sections = []
    for line in text.splitlines():
        line = ANSI.sub('', line).strip()
        banner = DOC_TESTS.fullmatch(line)
        if banner:
            sections.append((banner[1], []))
        elif sections:
            sections[-1][1].append(line)
    require(sorted(name for name, _ in sections) == sorted(expected), 'missing/duplicate doctest target')
    suites = [dict(target=name, **one_suite(lines, None)) for name, lines in sections]
    require(all(s['status'] == 'passed' for s in suites), 'doctest failed')
    require(sum(s['result']['passed'] for s in suites) >= 1, 'no actual doctest body executed')
    return suites


def run_shard(shard, plan, inventory, output, lease_fd, stop):
    receipt = dict(plan_sha256=digest(plan), source=plan['source'], shard_index=shard['index'], binaries=[])
    destination = Path(output) / ('shard-' + str(shard['index']))
    policy = load_policy('validation/rust_repair_cases.json')
    for index, binary in enumerate(shard['binaries']):
        if stop.is_set():
            receipt['blocked'] = 'execution canceled or another lane cleanup unqualified'
            break
        observed = {key: binary[key] for key in BINDINGS}
        try:
            check_inputs(inventory, binary)
            result, text = command([binary['executable'], '--test-threads=' + str(plan['test_threads'])],
                                   destination, 'binary-%02d' % index, 5400, lease_fd, cancel_event=stop)
            evidence = parse_bound(text, inventory, root=Path.cwd(), policy=policy,
                                   selected={Path(binary['executable']).name: [c['name'] for c in binary['cases']]},
                                   direct_binary=Path(binary['executable']).name)
            observed.update({key: result[key] for key in ('returncode', 'success', 'cleanup_complete', 'timed_out')})
            observed['process'] = result
            observed['evidence'] = evidence
            observed['outcomes'] = evidence['observed_suites'][0]['outcomes'] if evidence['qualification_complete'] else []
            observed['parser_failure'] = not evidence['qualification_complete']
            require(safe(result), 'native process failed or cleanup incomplete')
            require(evidence['qualification_complete'] and evidence['native_passed'], 'native outcomes incomplete or failed')
            check_inputs(inventory, binary)
        except (OSError, ValueError, KeyError) as error:
            observed.update(success=False, error=str(error))
        receipt['binaries'].append(observed)
        write(destination / 'receipt.json', receipt)
        # A native assertion failure stays red but does not suppress unrelated
        # evidence. Unsafe cleanup blocks this lane's remaining execution.
        if observed.get('cleanup_complete') is not True:
            stop.set()
            break
    write(destination / 'receipt.json', receipt)
    return receipt


def execute(mode, inventory, metadata, output, lease_fd, *, stop_event=None):
    output = Path(output)
    output.mkdir(exist_ok=False)
    started = time.monotonic()
    report = dict(schema=1, mode=mode, source=inventory['source'], success=False,
                  errors=[], scope='ordinary Rust, doctests and two explicit serial regressions only')
    write(output / 'results.json', report)
    policy = load_policy('validation/rust_repair_cases.json')
    stop = stop_event if stop_event is not None else threading.Event()
    previous_handlers = {number: signal.getsignal(number) for number in (signal.SIGINT, signal.SIGTERM)}
    for number in previous_handlers:
        signal.signal(number, lambda _number, _frame: stop.set())
    try:
        check_inputs(inventory)
        targets = doctest_targets(metadata)
        if mode == 'legacy':
            result, text = command(NATIVE, output, 'ordinary', 5400, lease_fd, cancel_event=stop)
            evidence = parse_bound(text, inventory, root=Path.cwd(), policy=policy, doctest_targets=targets)
            report.update(process=result, evidence=evidence)
            require(safe(result) and evidence['qualification_complete'] and evidence['native_passed'],
                    'legacy ordinary command or exact outcomes failed')
            report['legacy_doctests'] = parse_doctests(text, targets)
        elif mode == 'parallel':
            plan = build_plan(inventory, workers=3, test_threads=4)
            write(output / 'plan.json', plan)
            with ThreadPoolExecutor(max_workers=plan['workers']) as pool:
                receipts = list(pool.map(lambda shard: run_shard(shard, plan, inventory, output, lease_fd, stop), plan['shards']))
            report['shards'] = receipts
            report['native'] = validate_receipts(plan, receipts)
            result, text = command([*NATIVE, '--doc'], output, 'doctests', 1800, lease_fd, cancel_event=stop)
            report['doctests'] = dict(process=result, suites=parse_doctests(text, targets))
            require(safe(result), 'doctest process failed or cleanup incomplete')
        elif mode == 'nextest':
            import hashlib
            from validation.rust_nextest import (PINNED_NEXTEST, document_sha256,
                                                  validate_list, validate_run)
            config_path = Path('validation/rust-nextest.toml')
            config = config_path.read_text()
            version, version_text = command(['cargo', 'nextest', '--version'], output, 'nextest-version', 30, lease_fd,
                                             combined=False, cancel_event=stop)
            require(safe(version) and version_text.startswith('cargo-nextest ' + PINNED_NEXTEST + ' '), 'wrong nextest tool')
            selection = ['cargo', 'nextest', '--config-file', str(config_path)]
            common = ['--workspace', '--locked', '--exclude', 'plurx-cluster-check', '--profile', 'm3', '--ignore-default-filter']
            process, text = command([*selection, 'list', *common, '--message-format', 'json'],
                                    output, 'nextest-list', 600, lease_fd, combined=False, cancel_event=stop)
            require(safe(process), 'nextest discovery failed')
            listing = json.loads(text)
            report['nextest_list'] = validate_list(inventory, listing)
            check_inputs(inventory)
            junit_path = Path(listing['rust-build-meta']['target-directory']) / 'nextest/m3/m3-junit.xml'
            require(not junit_path.exists(), 'refusing a preexisting nextest report; preserve it before another attempt')
            process, _events = command([*selection, 'run', *common, '--color', 'never',
                                       '--no-fail-fast', '--retries', '0', '--test-threads', '4'],
                                      output, 'nextest-run', 7200, lease_fd, combined=False, cancel_event=stop)
            junit = junit_path.read_text()
            stderr = (output / 'nextest-run.stderr').read_text()
            (output / 'nextest-junit.xml').write_text(junit)
            process.update(nextest_version=PINNED_NEXTEST, report_format='nextest-junit',
                           run_ignored='default', profile='m3', runner_stderr=stderr,
                           runner_stderr_sha256=hashlib.sha256(stderr.encode()).hexdigest(),
                           configuration_text=config, configuration_sha256=hashlib.sha256(config.encode()).hexdigest(),
                           inventory_sha256=document_sha256(inventory), listing_sha256=document_sha256(listing),
                           junit_sha256=hashlib.sha256(junit.encode()).hexdigest())
            write(output / 'nextest-binding.json', process)
            report['nextest'] = dict(process=process)
            report['nextest']['evidence'] = validate_run(inventory, listing, junit, process)
            result, text = command([*NATIVE, '--doc'], output, 'doctests', 1800, lease_fd, cancel_event=stop)
            report['doctests'] = dict(process=result, suites=parse_doctests(text, targets))
            require(safe(result), 'doctest process failed or cleanup incomplete')
        else:
            raise ValueError('unknown execution mode')
        # Deliberately outside fan-out, preserving both actual ignored bodies.
        report['serial'] = []
        daemon = [b for b in inventory['binaries'] if b['target']['name'] == 'plurxd']
        require(len(daemon) == 1, 'serial regressions need one exact daemon binary')
        binary = daemon[0]
        for index, name in enumerate(SERIAL):
            require(not stop.is_set(), 'execution canceled')
            require([c for c in binary['cases'] if c['name'] == name and c['ignored']] != [], 'serial regression missing/active')
            result, text = command([binary['executable'], name, '--exact', '--ignored', '--test-threads=1'],
                                   output, 'serial-' + str(index), 600, lease_fd, cancel_event=stop)
            evidence = parse_bound(text, inventory, root=Path.cwd(), policy=policy,
                                   selected={Path(binary['executable']).name: [name]}, execute_ignored=True,
                                   direct_binary=Path(binary['executable']).name)
            report['serial'].append(dict(process=result, evidence=evidence))
            require(safe(result) and evidence['qualification_complete'] and evidence['native_passed'], 'serial regression failed')
        check_inputs(inventory)
        require(not stop.is_set(), 'execution canceled')
        report['success'] = True
    except (OSError, ValueError, KeyError) as error:
        report['errors'].append(str(error))
    finally:
        for number, handler in previous_handlers.items():
            signal.signal(number, handler)
        process_records = [read(path) for path in output.glob('**/*.process.json')]
        report['safe_to_release'] = (not getattr(stop, 'unsafe_cleanup', False)
                                     and all(row.get('cleanup_complete') is True for row in process_records))
        report['canceled'] = stop.is_set()
        report['seconds'] = round(time.monotonic() - started, 3)
        write(output / 'results.json', report)
    return report


def normalized_runner_name(root, name):
    # Use the existing public-name mapping rather than adding private literals.
    return runpy.run_path(str(root / 'scripts/scrub-infra-names'))['rewrite'](name)[0]


def main(argv=None):
    global SOURCE_BUNDLE
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('phase', choices=['capacity', 'prepare', 'legacy', 'parallel', 'nextest'])
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--admission', type=Path, default=Path('/var/lock/plurx-ci'))
    parser.add_argument('--runner-name', default=os.environ.get('RUNNER_NAME', ''))
    parser.add_argument('--label', default=None)
    parser.add_argument('--source-bundle', type=Path, default=None,
                        help='Externally stored, cryptographically verified source-only archive manifest')
    args = parser.parse_args(argv)
    SOURCE_BUNDLE = args.source_bundle
    root = Path.cwd().resolve()
    output = args.output.resolve()
    require(not output.is_relative_to(root), 'pilot artifacts must be outside tested checkout')
    output.mkdir(parents=True, exist_ok=True)
    if args.phase == 'capacity':
        from validation.baseline_resources import metadata, discover_cgroup, cgroup_sample
        counters, missing, counter_root = cgroup_sample(discover_cgroup())
        record = dict(metadata('m3-capacity'), runner=args.runner_name,
                      memory=[row for row in Path('/proc/meminfo').read_text().splitlines()
                              if row.startswith(('MemTotal:', 'MemAvailable:', 'SwapTotal:', 'SwapFree:'))],
                      cgroup=counters, missing_counters=missing,
                      counter_root=str(counter_root) if counter_root is not None else None,
                      qualified=False,
                      limitation='read-only capacity observation; no native test execution or admission proof')
        write(output / 'capacity.json', record)
        print(json.dumps(record, indent=2))
        return 0
    runner = normalized_runner_name(root, args.runner_name)
    with acquire(args.admission, repository=root) as lease:
        environment = inspect_environment(runner, root, args.admission)
        write(output / ('environment-' + args.phase + '.json'), environment)
        if args.phase == 'prepare':
            stop = threading.Event()
            handlers = {number: signal.getsignal(number) for number in (signal.SIGTERM, signal.SIGINT)}
            try:
                for number in handlers:
                    signal.signal(number, lambda _number, _frame: stop.set())
                inventory = discover(output / 'inventory', NATIVE, compile_timeout=3600, list_timeout=20,
                                     pass_fds=(lease['lock_fd'],), cancel_event=stop, source_bundle=SOURCE_BUNDLE)
                require(inventory['discovery_complete'], 'compilation or inventory failed')
                process, text = command(['cargo', 'metadata', '--format-version=1', '--locked', '--offline', '--no-deps'],
                                        output, 'metadata', 120, lease['lock_fd'], combined=False, cancel_event=stop)
                require(safe(process), 'Cargo metadata failed')
                write(output / 'workspace-metadata.json', json.loads(text))
            finally:
                for number, handler in handlers.items():
                    signal.signal(number, handler)
            return 0
        inventory = read(output / 'inventory/inventory.json')
        metadata = read(output / 'workspace-metadata.json')
        label = args.label or args.phase
        require(re.fullmatch(r'[a-z0-9][a-z0-9-]{0,60}', label), 'invalid execution label')
        report = execute(args.phase, inventory, metadata, output / label, lease['lock_fd'])
        if not report['safe_to_release']:
            quarantine(lease, 'native process cleanup unqualified; require independent teardown confirmation')
        print(json.dumps({k: v for k, v in report.items() if k not in ('evidence', 'serial', 'doctests')}, indent=2))
        return 0 if report['success'] else 1


if __name__ == '__main__':
    sys.exit(main())
