#!/usr/bin/env python3
"""Measure one frozen source archive with a fixed, software-only test corpus."""
import sys
sys.dont_write_bytecode = True

import argparse
from datetime import datetime, timezone
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import socket
import subprocess
import tarfile
import time

from affected_core_replay import listed_names, prepare_selection

RUST = '1.97.1'
LLVM_COV = '0.9.1'
FEATURES = ('plurx-core/cluster-read-cost-validation,'
            'plurx-core/cluster-validation,plurxd/cluster-integration-tests')
EXCLUDED = 'plurx-cluster-check'
HERE = Path(__file__).resolve().parent
AUGMENTED = (
    ('plurx-core', 'plurx_core', 'lib', 2),
    ('plurx-core', 'store_contract', 'test', 1),
    ('plurx-core', 'schema_lineage_upgrade', 'test', 1),
    ('plurxd', 'cluster_activation', 'test', 1),
    ('plurxd', 'cluster_activity', 'test', 1),
    ('plurxd', 'live_tv_two_node', 'test', 1),
)
NATIVE = (
    ('plurxd', 'vodserve::tests::encoded_vod_vfr_input_is_sampled_on_the_declared_rational_grid', 600),
    ('plurxd', 'vodserve::tests::encoded_vod_bitmap_burn_restores_cues_that_predate_video_seek_landing', 600),
    ('plurxd', 'decode_facts::tests::descriptor_bound_idet_overrules_misflag_and_confirms_interlace', 120),
    ('plurxd', 'transcode::media_origin::probe_media_origin_reads_the_preceding_keyframe', 60),
    ('plurxd', 'ffmpeg::tests::dovi_probe_output_receives_frame_hashes', 30),
    ('plurx-core', 'transcode::encoder::tests::benchmark_real_cpu_emits_a_complete_positive_measurement', 240),
    ('plurxd', 'transcoder_optimization::comparison::tests::real_media_production_tools_comparison', 900),
    ('plurxd', 'pipeprobe::census::tests::census_smoke_on_this_host', 300),
    ('plurxd', 'live_tv::caption_probe::live_caption_audit_on_this_node', 600),
    ('plurxd', 'vodserve::tests::encoded_vod_two_hour_audio_restart_budget', 300),
    ('plurxd', 'storeprobe::tests::a_real_file_yields_a_real_throughput_and_seek_number', 120),
    ('plurxd', 'http::publication::tests::epub_manifest_does_not_expand_large_resource', 180),
)


def digest(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def write_json(path, value):
    with Path(path).open('x') as stream:
        json.dump(value, stream, indent=2)
        stream.write('\n')


def verify_source(root, archive, expected_digest, commit):
    root, archive = Path(root).resolve(), Path(archive).resolve()
    if digest(archive) != expected_digest:
        raise ValueError('source archive SHA256 mismatch')
    expected = set()
    with tarfile.open(archive, 'r:*') as source:
        if source.pax_headers.get('comment') != commit:
            raise ValueError('git archive commit identity mismatch')
        for member in source:
            relative = Path(member.name)
            path = root / relative
            if relative.is_absolute() or '..' in relative.parts or '.git' in relative.parts:
                raise ValueError('unsafe source archive path: ' + member.name)
            if not path.resolve().is_relative_to(root):
                raise ValueError('source path escapes owned extraction: ' + member.name)
            if member.isdir():
                if not path.is_dir() or path.is_symlink():
                    raise ValueError('source directory mismatch: ' + member.name)
                continue
            if not member.isfile() or path.is_symlink() or not path.is_file():
                raise ValueError('non-regular source member: ' + member.name)
            if relative in expected:
                raise ValueError('duplicate source archive member: ' + member.name)
            expected.add(relative)
            with source.extractfile(member) as stream:
                wanted = hashlib.file_digest(stream, 'sha256').hexdigest()
            if digest(path) != wanted:
                raise ValueError('extracted source differs: ' + member.name)
    actual = set()
    for parent, dirs, files in os.walk(root, followlinks=False):
        for name in dirs:
            path = Path(parent) / name
            if path.is_symlink() or name == '.git':
                raise ValueError('unexpected source directory: ' + str(path))
        for name in files:
            path = Path(parent) / name
            if path.is_symlink():
                raise ValueError('unexpected source link: ' + str(path))
            actual.add(path.relative_to(root))
    if actual != expected:
        raise ValueError('unarchived or missing source files: ' + repr(sorted(map(str, actual ^ expected))[:20]))
    if not expected:
        raise ValueError('empty source archive')
    return {'source_sha256': expected_digest, 'source_commit': commit, 'files_verified': len(expected)}


def parse_show_env(text):
    result = {}
    for line in text.splitlines():
        words = shlex.split(line.rstrip(';'))
        if len(words) != 2 or words[0] != 'export' or '=' not in words[1]:
            raise ValueError('unexpected cargo-llvm-cov show-env output')
        name, value = words[1].split('=', 1)
        if not re.fullmatch('_*[A-Z][A-Z0-9_]*', name) or name in result:
            raise ValueError('invalid or duplicate instrumentation variable')
        result[name] = value
    flags = (result.get('RUSTFLAGS', '') + result.get('CARGO_ENCODED_RUSTFLAGS', '')
             + result.get('__CARGO_LLVM_COV_RUSTC_WRAPPER_RUSTFLAGS', ''))
    if 'instrument-coverage' not in flags or '--cfg=coverage' not in flags:
        raise ValueError('coverage instrumentation absent from pinned tool output')
    return result


def validated_lines(report):
    lines = report['data'][0]['totals']['lines']
    count, covered = lines['count'], lines['covered']
    if type(count) is not int or type(covered) is not int or count <= 0 or not 0 <= covered <= count:
        raise ValueError('invalid line coverage counters')
    return {'count': count, 'covered': covered, 'percent': 100 * covered / count}


class Campaign:
    def __init__(self, args):
        self.args = args
        self.root = Path.cwd().resolve()
        self.output = args.output.resolve()
        self.cache = Path('/opt/coverage-cache').resolve()
        if self.output.is_relative_to(self.root) or self.output.is_relative_to(self.cache):
            raise ValueError('coverage receipts must be outside source and compiler cache')
        if self.output.exists() and any(self.output.iterdir()):
            raise ValueError('coverage output must be a fresh directory')
        self.output.mkdir(parents=True, exist_ok=True)
        (self.output / 'logs').mkdir()
        (self.output / 'profiles').mkdir()
        self.records = []
        self.env = {key: value for key, value in os.environ.items()
                    if key in ('PATH', 'HOME', 'LANG', 'LC_ALL', 'TMPDIR')}
        defaults = {'CARGO_HOME': self.cache / 'cargo-home',
                    'RUSTUP_HOME': self.cache / 'rustup',
                    'CARGO_TARGET_DIR': self.cache / 'coverage/target'}
        for key, fallback in defaults.items():
            path = Path(os.environ.get(key, str(fallback))).resolve()
            if not path.is_relative_to(self.cache) or path == self.cache:
                raise ValueError(key + ' must be inside the owned coverage cache')
            self.env[key] = str(path)
        self.target = Path(self.env['CARGO_TARGET_DIR'])
        if self.target.name != 'llvm-cov-target':
            self.target /= 'llvm-cov-target'
        self.env.update(CARGO_TARGET_DIR=str(self.target), CARGO_BUILD_JOBS='1',
                        RUST_TEST_THREADS='2', RUST_MIN_STACK='8388608', CARGO_INCREMENTAL='0',
                        RUSTUP_TOOLCHAIN=RUST, PYTHONDONTWRITEBYTECODE='1',
                        CARGO_TERM_COLOR='never', PLURX_BUILD_SHA=args.source_commit,
                        PLURX_BUILD_REF='main', SOURCE_DATE_EPOCH=str(args.source_epoch),
                        PLURX_SHARING_FIXTURE_IP='100.127.250.2')
        self.env['PATH'] = self.env['CARGO_HOME'] + '/bin:' + self.env.get('PATH', '/usr/bin:/bin')
        self.env['LLVM_PROFILE_FILE'] = str(self.output / 'profiles/%p-%m.profraw')

    def run(self, name, command, *, cwd=None, env=None, stdout=None, bound=10800, check=True):
        command = list(map(str, command))
        logpath = self.output / 'logs' / (name + '.log')
        started = time.monotonic()
        with logpath.open('x') as log:
            if stdout is None:
                result = subprocess.run(['timeout', '--kill-after=30s', str(bound) + 's', *command],
                                        cwd=cwd or self.root, env=env or self.env,
                                        stdout=log, stderr=log)
            else:
                with Path(stdout).open('x') as out:
                    result = subprocess.run(['timeout', '--kill-after=30s', str(bound) + 's', *command],
                                            cwd=cwd or self.root, env=env or self.env,
                                            stdout=out, stderr=log)
        self.records.append({'phase': name, 'command': command, 'exit_status': result.returncode,
                             'seconds': round(time.monotonic() - started, 3), 'log': str(logpath),
                             'stdout': str(stdout) if stdout else None})
        # A progress file is diagnostic; receipt.json is written only on success.
        temporary = self.output / 'phases.tmp'
        temporary.write_text(json.dumps(self.records, indent=2) + '\n')
        temporary.replace(self.output / 'phases.json')
        print(json.dumps({key: self.records[-1][key] for key in ('phase', 'exit_status', 'seconds')}), flush=True)
        if check and result.returncode:
            print(logpath.read_text(errors='replace')[-6000:], file=sys.stderr, flush=True)
            raise RuntimeError(name + ' failed; full log: ' + str(logpath))
        return result.returncode

    def capture(self, name, command, **kwargs):
        path = self.output / (name + '.txt')
        self.run(name, command, stdout=path, **kwargs)
        return path.read_text().strip()

    def bootstrap(self):
        cargo_home = Path(self.env['CARGO_HOME'])
        cargo_home.mkdir(parents=True, exist_ok=True)
        rustup = cargo_home / 'bin/rustup'
        if not rustup.is_file():
            installer = self.output / 'rustup-init.sh'
            self.run('fetch-rustup', ['curl', '--fail', '--silent', '--show-error', '--location',
                                     '--proto', '=https', '--tlsv1.2', 'https://sh.rustup.rs',
                                     '--output', installer], bound=300)
            self.run('install-rustup', ['sh', installer, '-y', '--no-modify-path',
                                       '--profile', 'minimal', '--default-toolchain', 'none'], bound=1200)
        self.run('install-toolchain', [rustup, 'toolchain', 'install', RUST, '--profile', 'minimal',
                                       '--component', 'llvm-tools-preview'], bound=3600)
        rustc = self.capture('rustc-version', ['rustc', '--version'])
        cargo = self.capture('cargo-version', ['cargo', '--version'])
        if not rustc.startswith('rustc ' + RUST + ' ') or not cargo.startswith('cargo ' + RUST + ' '):
            raise ValueError('compiler and Cargo must both match pinned Rust ' + RUST)
        tool = cargo_home / 'bin/cargo-llvm-cov'
        version = subprocess.run([str(tool), '--version'], env=self.env, text=True,
                                 capture_output=True).stdout.strip() if tool.is_file() else ''
        if version != 'cargo-llvm-cov ' + LLVM_COV:
            install_env = dict(self.env)
            install_env['CARGO_TARGET_DIR'] = str(self.cache / 'coverage/tool-install-target')
            self.run('install-llvm-cov', ['cargo', 'install', 'cargo-llvm-cov', '--locked',
                                        '--version', '=' + LLVM_COV, '--root', cargo_home, '--force'],
                     env=install_env, bound=3600)
        version = self.capture('llvm-cov-version', [tool, '--version'])
        if version != 'cargo-llvm-cov ' + LLVM_COV:
            raise ValueError('cargo-llvm-cov version mismatch')
        sysroot = Path(self.capture('rust-sysroot', ['rustc', '--print', 'sysroot']))
        host = next(line[6:] for line in self.capture('rustc-detail', ['rustc', '-vV']).splitlines()
                    if line.startswith('host: '))
        self.llvm = sysroot / 'lib/rustlib' / host / 'bin'
        for name in ('llvm-profdata', 'llvm-cov'):
            if not (self.llvm / name).is_file():
                raise ValueError('pinned LLVM tool missing: ' + name)
        ffmpeg = self.capture('ffmpeg-version', ['ffmpeg', '-version']).splitlines()[0]
        if not re.match(r'ffmpeg version n?6\.', ffmpeg):
            raise ValueError('this corpus requires Ubuntu 24.04 FFmpeg major 6')
        self.tools = {'rustc': rustc, 'cargo': cargo, 'cargo_llvm_cov': version,
                      'ffmpeg': ffmpeg, 'llvm_cov_sha256': digest(self.llvm / 'llvm-cov'),
                      'llvm_profdata_sha256': digest(self.llvm / 'llvm-profdata')}
        self.run('fetch-dependencies', ['cargo', 'fetch', '--locked'], bound=3600)
        instrumentation = self.capture('instrumentation', ['cargo', 'llvm-cov', 'show-env', '--sh'])
        self.env.update(parse_show_env(instrumentation))
        self.env.update(CARGO_TARGET_DIR=str(self.target),
                        LLVM_PROFILE_FILE=str(self.output / 'profiles/%p-%m.profraw'))
        self.env['LD_LIBRARY_PATH'] = ':'.join(map(str, [self.target / 'debug/deps',
                self.target / 'debug', sysroot / 'lib', sysroot / 'lib/rustlib' / host / 'lib']))
        write_json(self.output / 'toolchain.json', self.tools)

    def build(self, directory, augmented=False):
        directory.mkdir(parents=True, exist_ok=True)
        tag = 'augmented' if augmented else 'default'
        flags = ['--features', FEATURES] if augmented else []
        self.run(tag + '-metadata', ['cargo', 'metadata', '--offline', '--locked', '--no-deps',
                 '--format-version', '1', *flags], stdout=directory / 'metadata.json')
        self.run(tag + '-build', ['cargo', 'test', '--offline', '--workspace', '--tests', '--locked',
                 '--exclude', EXCLUDED, *flags, '--no-run', '--message-format=json'],
                 stdout=directory / 'artifacts.jsonl')
        objects = directory / ('objects' if augmented else 'objects-default')
        self.run(tag + '-objects', [sys.executable, HERE / 'exact_coverage.py', 'select',
                 '--messages', directory / 'artifacts.jsonl', '--metadata', directory / 'metadata.json',
                 '--cargo-home', self.env['CARGO_HOME'], '--rustup-home', self.env['RUSTUP_HOME'],
                 '--hash-objects', '--snapshot-normal-bins', '--source-sha256', self.args.source_sha256,
                 '--output', objects])
        return json.loads((directory / 'metadata.json').read_text()), json.loads((objects / 'objects.json').read_text())

    def artifact(self, receipt, package, target, kind, test=True):
        matches = {item['path'] for item in receipt['targets'] if item['package'] == package
                   and item['target'] == target and kind in item['kind'] and item['test_profile'] == test}
        if len(matches) != 1:
            raise ValueError('missing/ambiguous harness: ' + repr((package, target, kind, matches)))
        path = next(iter(matches))
        records = [item for item in receipt['objects'] if item['path'] == path]
        if len(records) != 1 or records[0].get('sha256') != digest(path):
            raise ValueError('coverage object changed: ' + path)
        return path

    def verify_normal_bins(self, receipt):
        records = {item['path']: item for item in receipt['objects']}
        for target in receipt['targets']:
            if not target['test_profile'] and 'bin' in target['kind']:
                if digest(target['cargo_path']) != records[target['path']]['sha256']:
                    raise ValueError('normal binary overwritten before default phases finished')

    def names(self, tag, binary, cwd, *, ignored=False, filters=(), env=None):
        path = self.output / (tag + '-names.txt')
        self.run(tag + '-list', [binary, *filters, '--list', *(['--ignored'] if ignored else [])],
                 stdout=path, cwd=cwd, env=env, bound=120)
        names = listed_names(path.read_text())
        if not names:
            raise ValueError('selected zero tests: ' + tag)
        return names

    def test_env(self, metadata):
        return {p['name']: str(Path(p['manifest_path']).parent) for p in metadata['packages']}

    def sharing(self, metadata, receipt):
        if os.environ.get('PLURX_SHARING_FIXTURE_IP') != self.env['PLURX_SHARING_FIXTURE_IP']:
            raise ValueError('host wrapper must provide the declared CGNAT fixture endpoint')
        address = ipaddress.ip_address(self.env['PLURX_SHARING_FIXTURE_IP'])
        if address not in ipaddress.ip_network('100.64.0.0/10'):
            raise ValueError('sharing endpoint is not CGNAT')
        with socket.socket() as probe:
            probe.bind((str(address), 0))
        self.verify_normal_bins(receipt)
        directories = self.test_env(metadata)
        failed = False
        for target, kind, filters in [('plurxd', 'bin', ('sharing_',)),
                                       ('sharing_daemon_restart', 'test', ())]:
            binary = self.artifact(receipt, 'plurxd', target, kind)
            names = self.names('sharing-' + target, binary, directories['plurxd'],
                               ignored=True, filters=filters)
            failed |= bool(self.run('sharing-' + target, [binary, '--ignored', '--exact',
                           *sorted(names), '--test-threads=1'], cwd=directories['plurxd'],
                           bound=3600, check=False))
        self.verify_normal_bins(receipt)
        if failed:
            raise RuntimeError('one or more ignored sharing harnesses failed')

    def augmented(self):
        expanded = self.output / 'expanded'
        metadata, receipt = self.build(expanded, augmented=True)
        replay = prepare_selection(self.output, expanded, metadata, self.env, HERE / 'core_scope.json')
        directories = self.test_env(metadata)
        # The default Docker bridge supplies this route without extra privileges.
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as probe:
            probe.connect(('192.0.2.1', 80))  # route lookup only, sends no packet
            address = ipaddress.ip_address(probe.getsockname()[0])
        if not any(address in ipaddress.ip_network(block) for block in
                   ('10.0.0.0/8', '172.16.0.0/12', '192.168.0.0/16')):
            raise ValueError('replicated fixtures require the ordinary RFC1918 Docker route')
        failed = False
        for package, target, kind, threads in AUGMENTED:
            binary = self.artifact(receipt, package, target, kind)
            command = [binary, '--test-threads=' + str(threads)]
            if target == 'plurx_core':
                if binary != replay['augmented_binary']:
                    raise ValueError('affected core selection names a different executable')
                if replay['execution'] == 'affected':
                    command += ['--exact', *replay['selected_names']]
                # Full fallback uses the unfiltered harness, preserving normal
                # ignored handling without an unbounded argv of exact names.
            else:
                self.names('augmented-' + target, binary, directories[package])
            failed |= bool(self.run('augmented-' + target, command, cwd=directories[package], check=False))
        if failed:
            raise RuntimeError('one or more augmented harnesses failed')
        return expanded, replay['scope']

    def native(self, metadata, receipt):
        directories = self.test_env(metadata)
        env = dict(self.env)
        env.update(PLURX_CENSUS_SMOKE='software', PLURX_CAPTION_AUDIT_ENCODERS='copy,software')
        for key, name in [('PLURX_FFMPEG', 'ffmpeg'), ('PLURX_FFPROBE', 'ffprobe')]:
            path = shutil.which(name, path=env['PATH'])
            if path is None:
                raise ValueError('missing native software tool: ' + name)
            env[key] = path
        env['PLURX_TEST_NAMESPACE_BOOTSTRAP'] = self.artifact(receipt, 'plurxd', 'plurxd', 'bin', False)
        failed = False
        for index, (package, name, bound) in enumerate(NATIVE):
            target, kind = ('plurx_core', 'lib') if package == 'plurx-core' else ('plurxd', 'bin')
            binary = self.artifact(receipt, package, target, kind)
            tag = 'native-' + str(index)
            names = self.names(tag, binary, directories[package], ignored=True,
                               filters=(name, '--exact'), env=env)
            if names != {name}:
                raise ValueError('native selection is not the declared exact test')
            failed |= bool(self.run(tag, [binary, name, '--exact', '--ignored', '--test-threads=1'],
                           cwd=directories[package], env=env, bound=bound, check=False))
        if failed:
            raise RuntimeError('one or more native software tests failed')

    def collect(self):
        source = verify_source(self.root, self.args.source_archive,
                               self.args.source_sha256, self.args.source_commit)
        write_json(self.output / 'source-verification.json', source)
        # A new extraction can have older mtimes than the warm target. Refresh
        # compile inputs without changing bytes, so Cargo cannot reuse stale code.
        for path in self.root.rglob('*'):
            if path.is_file() and (path.suffix in ('.rs', '.sql', '.toml') or path.name == 'Cargo.lock'):
                path.touch()
        self.bootstrap()
        default_metadata, default_receipt = self.build(self.output)
        self.verify_normal_bins(default_receipt)
        status = self.run('default-tests', ['cargo', 'test', '--offline', '--workspace', '--tests',
                          '--locked', '--exclude', EXCLUDED, '--no-fail-fast'], bound=21600, check=False)
        (self.output / 'test-exit-status.txt').write_text(str(status) + '\n')
        if status:
            raise RuntimeError('full default workspace suite failed')
        self.sharing(default_metadata, default_receipt)
        expanded, core_scope = self.augmented()
        if self.args.native:
            self.native(default_metadata, default_receipt)
        verify_source(self.root, self.args.source_archive, self.args.source_sha256, self.args.source_commit)
        self.run('export', [sys.executable, HERE / 'exact_coverage.py', 'export',
                 '--objects', self.output / 'objects-default/objects.json',
                 '--objects', expanded / 'objects/objects.json',
                 '--profiles', self.output / 'profiles', '--llvm-profdata', self.llvm / 'llvm-profdata',
                 '--llvm-cov', self.llvm / 'llvm-cov', '--output', self.output / 'report'])
        report = self.output / 'report/coverage.json'
        lines = validated_lines(json.loads(report.read_text()))
        # These publication inputs appear only after every required phase passes.
        with (self.output / 'cov.json').open('xb') as destination, report.open('rb') as stream:
            shutil.copyfileobj(stream, destination)
        write_json(self.output / 'receipt.json', {
            'schema': 1, 'status': 'passed', **source,
            'measured_at_utc': datetime.now(timezone.utc).isoformat(),
            'source_epoch': self.args.source_epoch, 'toolchain': self.tools,
            'core_execution_scope': core_scope,
            'corpus': ['default-workspace', 'ignored-cgnat-sharing', 'augmented-core',
                       'augmented-full-integrations'] + (['native-software-12'] if self.args.native else []),
            'native_tests': [name for _, name, _ in NATIVE] if self.args.native else [],
            'line_coverage': lines, 'report_sha256': digest(self.output / 'cov.json'),
            'object_manifests': ['objects-default/objects.json', 'expanded/objects/objects.json'],
            'core_selection': 'expanded/core-replay-selection.json', 'phases': 'phases.json',
            'scope': 'Linux software coverage; no hardware, namespace-sandbox or formal qualification',
        })
        print(json.dumps({'status': 'passed', 'cov_json': str(self.output / 'cov.json'), **lines}), flush=True)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source-archive', type=Path, required=True)
    parser.add_argument('--source-sha256', required=True)
    parser.add_argument('--source-commit', required=True)
    parser.add_argument('--source-epoch', type=int, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--native', action='store_true')
    args = parser.parse_args(argv)
    if not re.fullmatch('[0-9a-f]{64}', args.source_sha256) or not re.fullmatch('[0-9a-f]{40}', args.source_commit):
        parser.error('source SHA256 and commit must be full lowercase hexadecimal identities')
    if args.source_epoch < 0:
        parser.error('source epoch must be nonnegative')
    campaign = None
    try:
        campaign = Campaign(args)
        campaign.collect()
    except Exception as error:
        if campaign is not None:
            # Never manufacture publication inputs after failure.
            write_json(campaign.output / 'failure.json', {'status': 'failed', 'error': str(error)})
        print('unit coverage failed: ' + str(error), file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
