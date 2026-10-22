"""One-lease, build-once M3 rehearsal; observations never authorize adoption.

Run only as a dedicated CLI process: rust_parallel.SOURCE_BUNDLE is fixed for
the complete campaign. Native executable bytes and stat identity must remain
unchanged after preparation. Required doctest compilation still occurs in
each arm. An empty Cargo target does not establish cold OS/registry caches.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import platform
import signal
import stat
import subprocess
import sys
import threading
import time

from validation import baseline_resources as resources
from validation import rust_parallel as parallel
from validation.rust_host_admission import acquire, inspect_environment, quarantine
from validation.rust_parallel_compare import compare

ARMS = [('cold-parallel', 'parallel'), ('legacy', 'legacy'),
        *[(f'warm-{index:02}', 'parallel') for index in range(1, 4)], ('nextest', 'nextest')]
INPUTS = ('Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', '.cargo/config', '.cargo/config.toml',
          'validation/rust-nextest.toml', 'validation/rust_parallel.py',
          'validation/rust_parallel_manifest.py', 'validation/rust_nextest.py',
          'validation/baseline_commands.py', 'validation/baseline_rust_inventory.py')
ENVIRONMENT = ('CARGO_TARGET_DIR', 'CARGO_HOME', 'CARGO_BUILD_JOBS', 'CARGO_NET_OFFLINE',
               'RUSTUP_TOOLCHAIN', 'RUSTC', 'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS',
               'RUST_TEST_THREADS', 'RUST_MIN_STACK', 'CARGO_PROFILE_DEV_DEBUG',
               'CARGO_PROFILE_TEST_DEBUG', 'CARGO_INCREMENTAL')


def require(condition, message):
    if not condition:
        raise ValueError(message)


def source_identity(bundle):
    if bundle is not None:
        from validation.rust_source_bundle import identity
        return identity(Path.cwd(), bundle)
    identity = parallel.git_identity()
    status = subprocess.check_output(['git', 'status', '--porcelain=v1', '--untracked-files=all'],
                                     text=True, stderr=subprocess.PIPE, timeout=30)
    require(not status, 'Git source must be frozen, including untracked files')
    return identity


def input_hashes(root):
    return {name: parallel.sha256(root / name) for name in INPUTS if (root / name).is_file()}


def execution_environment():
    return {name: os.environ[name] for name in ENVIRONMENT if name in os.environ}


def new_target(root, output):
    raw = os.environ.get('CARGO_TARGET_DIR', '')
    require(raw and Path(raw).is_absolute(), 'CARGO_TARGET_DIR must be an explicit absolute owned directory')
    path = Path(raw)
    require(not path.is_symlink(), 'Cargo target must not be a symlink')
    path = path.resolve()
    require(not path.is_relative_to(root) and not root.is_relative_to(path),
            'Cargo target must be outside and separate from source')
    require(path != output and not output.is_relative_to(path), 'Cargo target must not contain artifact output')
    existed = path.exists()
    if not existed:
        path.mkdir(parents=True, mode=0o700)
    info = path.lstat()
    require(stat.S_ISDIR(info.st_mode) and info.st_uid == os.getuid() and not info.st_mode & 0o022,
            'Cargo target must be a private owned directory')
    require(not any(path.iterdir()), 'Cargo target is nonempty; preserve it and choose another path')
    return dict(path=str(path), initial_state='owned-empty' if existed else 'new-directory',
                device=info.st_dev, inode=info.st_ino, owner_uid=info.st_uid,
                cold_definition='empty Cargo target only; OS page cache and registry cache state unqualified')


def create_immutable(path, value):
    with path.open('x', encoding='utf-8') as stream:
        json.dump(value, stream, indent=2, sort_keys=True, allow_nan=False)
        stream.write('\n')


def native_snapshot(inventory):
    result = {}
    for binary in inventory['binaries']:
        path = Path(binary['executable'])
        info = path.stat()
        require(not path.is_symlink() and parallel.sha256(path) == binary['sha256'],
                'Native executable payload changed')
        result[str(path)] = [info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, binary['sha256']]
    require(result, 'No native payload to freeze')
    return result


def discovery_cleanup(report):
    records = [report.get('compile', {})]
    records.extend(value for binary in report.get('binaries', [])
                   for value in binary.get('listing', {}).values())
    return all(row.get('cleanup_complete') is True for row in records)


def sampler_exit(state, *, proc_root=Path('/proc'), timeout=3.0):
    """Observe termination of the exact sampler; never signal a reused PID.

    A zombie has exited even if its Popen object was not retained for reaping.
    Inaccessible/malformed procfs evidence cannot establish process cleanup.
    """
    pid, ticks = state.get('pid'), state.get('start_ticks')
    require(type(pid) is int and pid > 1 and type(ticks) is int and ticks >= 0,
            'Resource sampler identity unavailable')
    require(proc_root.is_dir(), 'Resource sampler procfs unavailable')
    deadline = time.monotonic() + timeout
    while True:
        try:
            raw = (proc_root / str(pid) / 'stat').read_text(encoding='utf-8')
        except FileNotFoundError:
            return dict(pid=pid, start_ticks=ticks, disposition='gone')
        except (OSError, UnicodeError) as error:
            raise ValueError('Resource sampler termination unobservable') from error
        observed = resources.proc_stat(raw)
        require(observed is not None, 'Resource sampler proc identity malformed')
        if observed['start_ticks'] != ticks:
            return dict(pid=pid, start_ticks=ticks, disposition='original-exited-pid-reused')
        process_state = raw[raw.rfind(')') + 1:].split()[0]
        if process_state in ('Z', 'X', 'x'):
            return dict(pid=pid, start_ticks=ticks, disposition='exited-' + process_state)
        require(time.monotonic() < deadline, 'Resource sampler remains alive after finish')
        time.sleep(min(0.05, max(0, deadline - time.monotonic())))


def observe(output, label, stop, callback):
    """Finish the independent sampler even on failure; uncertainty is unsafe."""
    directory = output / 'resources' / label
    started = False
    try:
        resources.start(directory, 'm3-rehearsal-' + label)
        started = True
        return callback()
    finally:
        try:
            if started:
                resources.finish(directory)
                state = parallel.read(directory / 'state.json')
                require(state.get('status') == 'finished', 'Resource sampler cleanup unqualified')
                parallel.write(directory / 'sampler-exit.json', sampler_exit(state))
                require((directory / 'summary.json').is_file() and
                        not (directory / 'telemetry-incomplete.json').exists(),
                        'Resource sampler evidence incomplete')
                summary = parallel.read(directory / 'summary.json')
                require(summary.get('status') == 'complete' and summary.get('unavailable') == [] and
                        type(summary.get('samples')) is int and summary['samples'] > 0,
                        'Resource sampler counters incomplete')
            else:
                raise ValueError('Resource sampler start did not qualify')
        except BaseException:
            stop.unsafe_cleanup = True
            stop.set()
            raise


def rehearse(output, admission, runner_name, *, source_bundle=None, without_nextest=False, stop_event=None):
    """Retain first outcomes, continue independent arms only after safe cleanup."""
    root = Path.cwd().resolve()
    raw_output = Path(output)
    require(not raw_output.is_symlink(), 'Artifact output must not be a symlink')
    output = raw_output.resolve()
    require(not output.is_relative_to(root) and not root.is_relative_to(output),
            'Artifact output must be outside and separate from source')
    require(not output.exists(), 'Artifact output already exists; first results must not be overwritten')
    original_bundle = parallel.SOURCE_BUNDLE
    parallel.SOURCE_BUNDLE = source_bundle
    stop = stop_event if stop_event is not None else threading.Event()
    handlers = {}
    report = dict(schema=1, success=False, adoption_ready=False, errors=[], arms=[], canceled=False,
                  nextest_comparison='owed-explicitly-disabled' if without_nextest else 'required',
                  owed=['source-bound runner-loss and lease-release drill', 'host capacity/resource qualification',
                        'broader feature/platform lanes and promotion contract'],
                  authentication='manual source-bound observation; no authenticated CI origin or result-adoption claim')
    planned = ARMS[:-1] if without_nextest else ARMS
    if without_nextest:
        report['owed'].append('qualified nextest comparison')
    lease = None
    prepared = False
    native = None
    binding_sha = None
    began = time.monotonic()
    try:
        source = source_identity(source_bundle)
        target = new_target(root, output)
        output.mkdir(parents=True, mode=0o700, exist_ok=False)
        runner = parallel.normalized_runner_name(root, runner_name)
        require(runner, 'Explicit rostered runner identity is required')
        frozen_inputs = input_hashes(root)
        for number in (signal.SIGTERM, signal.SIGINT):
            handlers[number] = signal.getsignal(number)
            signal.signal(number, lambda _number, _frame: stop.set())
        with acquire(admission, repository=root) as lease:
            # Admission may wait while another campaign fills or replaces this
            # target. No native children exist yet: retain refusal and return
            # normally so this known-empty process scope can release its lease.
            try:
                require(not stop.is_set(), 'Rehearsal canceled before cold-target revalidation')
                observed_target = new_target(root, output)
                require(all(observed_target[key] == target[key]
                            for key in ('path', 'device', 'inode', 'owner_uid')),
                        'Cargo target identity changed while waiting for admission')
                require(source_identity(source_bundle) == source and input_hashes(root) == frozen_inputs,
                        'Source or execution inputs superseded while waiting for admission')
                environment = inspect_environment(runner, root, admission)
            except (Exception, KeyboardInterrupt) as error:
                report.update(errors=[str(error) or type(error).__name__], safe_to_release=True,
                              canceled=stop.is_set(), preparation_started=False,
                              cold_target_validation='refused', seconds=round(time.monotonic() - began, 3))
                parallel.write(output / 'rehearsal.json', report)
                return report
            frozen_environment = execution_environment()
            pre_run = dict(schema=1, source=source, source_bundle_sha256=parallel.sha256(source_bundle)
                           if source_bundle else None, source_input_sha256=frozen_inputs,
                           runner=runner, admission=environment, cargo_target=target,
                           arms=[dict(label=label, mode=mode) for label, mode in planned],
                           resource_policy=dict(cpu_max=12, memory_max_bytes=15 * 1024**3,
                                                swap_max_bytes=0,
                                                host_memavailable_reserve_bytes=8 * 1024**3, pids_max=4096,
                                                global_nextest_threads=4, libtest_lanes=3, threads_per_lane=4),
                           commands=dict(prepare_native=parallel.NATIVE,
                                         metadata=['cargo', 'metadata', '--format-version=1', '--locked', '--offline', '--no-deps'],
                                         cargo_version=['cargo', '--version'],
                                         execution_driver_sha256=frozen_inputs.get('validation/rust_parallel.py'),
                                         declared_modes=planned),
                           cold_definition=target['cold_definition'],
                           environment=frozen_environment,
                           image=dict(identifier=os.environ.get('PLURX_CI_IMAGE_DIGEST'),
                                      provenance='caller environment, not independently inspected or CI-authenticated'),
                           kernel=dict(system=platform.system(), release=platform.release(), machine=platform.machine()),
                           limits=['Native executable bytes and stat identity freeze after prepare; doctests still compile within arms.',
                                   'No claim that unobserved dependency compilation or shared-host activity is absent.'])
            create_immutable(output / 'pre-run-manifest.json', pre_run)
            manifest_sha = parallel.sha256(output / 'pre-run-manifest.json')
            report['pre_run_manifest_sha256'] = manifest_sha
            parallel.write(output / 'rehearsal.json', report)
            try:
                def prepare():
                    nonlocal prepared, native, binding_sha
                    require(not stop.is_set(), 'Rehearsal canceled before preparation')
                    try:
                        inventory = parallel.discover(output / 'inventory', parallel.NATIVE,
                                                      compile_timeout=3600, list_timeout=20,
                                                      source_bundle=source_bundle, pass_fds=(lease['lock_fd'],),
                                                      cancel_event=stop)
                    except BaseException:
                        stop.unsafe_cleanup = True
                        raise
                    if not discovery_cleanup(inventory):
                        stop.unsafe_cleanup = True
                        stop.set()
                    discovery_processes = [inventory.get('compile', {}),
                                           *(item for binary in inventory.get('binaries', [])
                                             for item in binary.get('listing', {}).values())]
                    if any(item.get('canceled') is True for item in discovery_processes):
                        stop.set()
                    require(inventory.get('discovery_complete') is True and not inventory.get('errors'),
                            'Native preparation/discovery failed')
                    require(not stop.is_set(), 'Rehearsal canceled after preparation')
                    try:
                        process, text = parallel.command(pre_run['commands']['metadata'], output, 'metadata',
                                                         120, lease['lock_fd'], combined=False, cancel_event=stop)
                    except BaseException:
                        stop.unsafe_cleanup = True
                        raise
                    if process.get('cleanup_complete') is not True:
                        stop.unsafe_cleanup = True
                        stop.set()
                    if process.get('canceled') is True:
                        stop.set()
                    require(parallel.safe(process) and not stop.is_set(), 'Owned Cargo metadata command failed/canceled')
                    metadata = json.loads(text)
                    parallel.write(output / 'workspace-metadata.json', metadata)
                    try:
                        process, cargo = parallel.command(['cargo', '--version'], output, 'cargo-version',
                                                          30, lease['lock_fd'], combined=False, cancel_event=stop)
                    except BaseException:
                        stop.unsafe_cleanup = True
                        raise
                    if process.get('cleanup_complete') is not True:
                        stop.unsafe_cleanup = True
                        stop.set()
                    if process.get('canceled') is True:
                        stop.set()
                    require(parallel.safe(process) and not stop.is_set(), 'Owned Cargo version command failed/canceled')
                    require(source_identity(source_bundle) == source == inventory['source'], 'Source superseded during preparation')
                    require(execution_environment() == frozen_environment, 'Compile environment changed during preparation')
                    parallel.check_inputs(inventory)
                    native = native_snapshot(inventory)
                    create_immutable(output / 'source-binding.json', dict(
                        pre_run_manifest_sha256=manifest_sha, source=source, compiler=inventory.get('compiler'),
                        cargo_version=cargo.strip(), native_payload=native, configuration_and_cargo_sha256=frozen_inputs,
                        image=pre_run['image'], kernel=pre_run['kernel'], observed_environment=pre_run['environment'],
                        authentication=report['authentication']))
                    binding_sha = parallel.sha256(output / 'source-binding.json')
                    report['source_binding_sha256'] = binding_sha
                    prepared = True
                    return inventory, metadata
                inventory, metadata = observe(output, 'prepare', stop, prepare)
                for label, mode in planned:
                    require(not stop.is_set(), 'Rehearsal canceled or earlier cleanup unqualified')
                    require(execution_environment() == frozen_environment, 'Compile environment changed between arms')
                    require(source_identity(source_bundle) == source, 'Source superseded between arms')
                    require(input_hashes(root) == frozen_inputs, 'Execution configuration or Cargo inputs changed')
                    require(parallel.sha256(output / 'pre-run-manifest.json') == manifest_sha,
                            'Immutable pre-run manifest changed')
                    require(parallel.sha256(output / 'source-binding.json') == binding_sha,
                            'Immutable source binding changed')
                    parallel.check_inputs(inventory)
                    require(native_snapshot(inventory) == native, 'Native payload rebuilt or replaced between arms')
                    try:
                        result = observe(output, label, stop, lambda: parallel.execute(
                            mode, inventory, metadata, output / label, lease['lock_fd'], stop_event=stop))
                    except BaseException:
                        stop.unsafe_cleanup = True
                        raise
                    report['arms'].append(dict(label=label, mode=mode, success=result.get('success') is True,
                                               safe_to_release=result.get('safe_to_release')))
                    if result.get('canceled') is True:
                        stop.set()
                    parallel.write(output / 'rehearsal.json', report)
                    if result.get('safe_to_release') is not True:
                        stop.unsafe_cleanup = True
                        stop.set()
                        raise ValueError('Arm cleanup unqualified: ' + label)
                    require(source_identity(source_bundle) == source, 'Source superseded during arm')
                    require(execution_environment() == frozen_environment, 'Compile environment changed during arm')
                    require(input_hashes(root) == frozen_inputs and
                            parallel.sha256(output / 'pre-run-manifest.json') == manifest_sha and
                            parallel.sha256(output / 'source-binding.json') == binding_sha,
                            'Immutable inputs or pre-run policy changed during arm')
                    parallel.check_inputs(inventory)
                    require(native_snapshot(inventory) == native, 'Native payload rebuilt or replaced during arm')
                report['success'] = all(row['success'] for row in report['arms'])
            except (Exception, KeyboardInterrupt) as error:
                report['errors'].append(str(error) or type(error).__name__)
            finally:
                for label, mode in planned:
                    if not (output / label).exists():
                        parallel.write(output / label / 'results.json', dict(
                            schema=1, mode=mode, source=source, success=False, executed=False,
                            safe_to_release=None, errors=['blocked by earlier incomplete preparation, source drift or cancellation']))
                try:
                    comparison = compare(output)
                    parallel.write(output / 'comparison.json', comparison)
                    report['success'] = (report['success'] and comparison.get('qualification_complete_for_runtime') is True)
                except Exception as error:
                    report['errors'].append('comparison: ' + str(error))
                    report['success'] = False
                report['canceled'] = stop.is_set()
                report['safe_to_release'] = not getattr(stop, 'unsafe_cleanup', False)
                report['success'] = report['success'] and prepared and not report['errors'] and not stop.is_set()
                if not report['safe_to_release']:
                    quarantine(lease, 'rehearsal owned-process or sampler cleanup unqualified; require external teardown proof')
                report['seconds'] = round(time.monotonic() - began, 3)
                parallel.write(output / 'rehearsal.json', report)
        return report
    finally:
        for number, handler in handlers.items():
            signal.signal(number, handler)
        parallel.SOURCE_BUNDLE = original_bundle


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--admission', type=Path, default=Path('/var/lock/plurx-ci'))
    parser.add_argument('--runner-name', default=os.environ.get('RUNNER_NAME', ''))
    parser.add_argument('--source-bundle', type=Path)
    parser.add_argument('--without-nextest', action='store_true', help='Record nextest comparison as owed, never adopted')
    args = parser.parse_args(argv)
    try:
        report = rehearse(args.output, args.admission, args.runner_name,
                          source_bundle=args.source_bundle, without_nextest=args.without_nextest)
    except (OSError, ValueError) as error:
        print(str(error), file=sys.stderr)
        return 1
    print(json.dumps({key: report[key] for key in ('success', 'adoption_ready', 'errors', 'canceled')}))
    return 0 if report['success'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
