#!/usr/bin/env python3
"""Finite translation P0 diagnostic; no production publication or playback claim.

Downloads nothing. Phases share one new private receipt directory and enforce
15 original engine attempts plus two explicitly approved replacement cores. Source/reference/model/package identities are pinned.
Full publisher references and raw failures remain available beside diagnostics.
"""
import argparse
from fractions import Fraction
import hashlib
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys
import time
import unittest
import wave

ROOT = Path(__file__).resolve().parents[3]
HERE = Path(__file__).resolve().parent
MAX_OUTPUT = 256 * 1024
RATE = 16000
CLOCK = 'aresample=16000:async=1:first_pts=0:min_comp=0:min_hard_comp=0:max_soft_comp=0'


def load(path, name):
    loader = importlib.machinery.SourceFileLoader(name, str(path))
    spec = importlib.util.spec_from_loader(name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


RECOGNITION = load(ROOT / 'scripts/qualify-subtitle-recognition', 'p0_recognition')
RECOGNITION.MAX_PCM = 8 * 1024 * 1024
RECIPE = load(ROOT / 'scripts/build-subtitle-engine', 'p0_engine_recipe')
digest = RECOGNITION.digest


def pressure():
    if sys.platform == 'darwin':
        value = int(subprocess.check_output(['/usr/sbin/sysctl', '-n', 'kern.memorystatus_vm_pressure_level'], text=True))
        if value != 1:
            raise RuntimeError('host_pressure_not_normal')


def save(path, value):
    temporary = path.with_name(path.name + '.tmp')
    temporary.write_text(json.dumps(value, ensure_ascii=False, indent=2) + '\n')
    temporary.replace(path)


def pcm(path):
    with wave.open(str(path), 'rb') as f:
        if (f.getnchannels(), f.getsampwidth(), f.getframerate()) != (1, 2, RATE):
            raise ValueError('unexpected_pcm_format')
        return f.getnframes()


def source_identity(path):
    s = path.stat()
    return [s.st_dev, s.st_ino, s.st_size, s.st_mtime_ns, s.st_ctime_ns]


def timed_child(argv, directory, label, pass_fds=(), stop=False):
    """Owned process group, bounded logs, per-invocation native time peak RSS."""
    pressure()
    started = time.monotonic()
    out = directory / (label + '.stdout')
    err = directory / (label + '.stderr')
    if sys.platform == 'darwin':
        timed = ['/usr/bin/time', '-l', *argv]
    else:
        timed = ['/usr/bin/time', '-v', *argv]
    child = None
    reason = None
    stopped_after = None
    observed_engine = None
    with out.open('xb') as stdout, err.open('xb') as stderr:
        child = subprocess.Popen(timed, env={'LC_ALL': 'C'}, stdin=subprocess.DEVNULL,
                                 stdout=stdout, stderr=stderr, pass_fds=pass_fds, start_new_session=True)
        try:
            while child.poll() is None:
                if (out.stat().st_size > MAX_OUTPUT or err.stat().st_size > MAX_OUTPUT or
                    any(f.stat().st_size > MAX_OUTPUT for f in directory.glob('caption.*'))):
                    reason = 'child_output_bound'
                    break
                if time.monotonic() - started > 120:
                    reason = 'child_timeout'
                    break
                try:
                    pressure()
                except RuntimeError:
                    reason = 'host_pressure_not_normal'
                    break
                if stop and time.monotonic() - started >= 0.75:
                    # The time wrapper remains in this same owned group. Its
                    # engine child is positively checked before deliberate Stop.
                    rows = subprocess.check_output(['/bin/ps', '-axo', 'pid,ppid,pgid,comm'], text=True)
                    engines = [r.split(None, 3) for r in rows.splitlines()[1:]
                               if len(r.split(None, 3)) == 4 and
                               r.split(None, 3)[1] == str(child.pid) and
                               r.split(None, 3)[2] == str(child.pid) and
                               r.split(None, 3)[3].endswith('whisper-cli')]
                    if engines:
                        observed_engine = {'wrapper_pid': child.pid, 'owned_pgid': child.pid, 'engine_pid': int(engines[0][0]), 'engine_pgid': int(engines[0][2])}
                        save(directory / (label + '.pre-signal-identity.json'), observed_engine)
                        reason = 'deliberate_stop'
                        stopped_after = time.monotonic()
                        break
                time.sleep(0.05)
        finally:
            if child.poll() is None or reason:
                try:
                    os.killpg(child.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            child.wait()
    if out.stat().st_size > MAX_OUTPUT or err.stat().st_size > MAX_OUTPUT:
        reason = reason or 'child_output_bound'
    raw = err.read_text(errors='replace')
    match = re.search(r'(\d+)\s+maximum resident set size', raw) if sys.platform == 'darwin' else re.search(r'Maximum resident set size \(kbytes\):\s*(\d+)', raw)
    rss = int(match.group(1)) * (1 if sys.platform == 'darwin' else 1024) if match else None
    group_observations = []
    if stopped_after:
        group_gone = False
        empty = 0
        until = time.monotonic() + 3
        while time.monotonic() < until:
            rows = subprocess.check_output(['/bin/ps', '-axo', 'pid,ppid,pgid,stat,comm'], text=True)
            members = [line.split(None, 4) for line in rows.splitlines()[1:]
                       if len(line.split(None, 4)) == 5 and line.split(None, 4)[2] == str(child.pid)]
            group_observations.append({'after_signal_seconds': time.monotonic()-stopped_after, 'members': members})
            empty = empty + 1 if not members else 0
            if empty >= 2:
                group_gone = True
                break
            time.sleep(0.05)
    else:
        group_gone = None
    result = {'argv': argv, 'wall_seconds': time.monotonic() - started,
              'exit_code': child.returncode, 'termination_reason': reason,
              'peak_rss_bytes': rss, 'rss_scope': 'this invocation via native time; absent when Stop kills wrapper',
              'stdout_bytes': out.stat().st_size, 'stderr_bytes': err.stat().st_size,
              'stop_to_reaped_seconds': time.monotonic() - stopped_after if stopped_after else None,
              'owned_process_group_gone': group_gone, 'pre_signal_identity': observed_engine, 'post_wait_group_observations': group_observations}
    if reason and reason != 'deliberate_stop':
        raise RuntimeError(json.dumps(result))
    return result


def clock_anchor(stderr):
    matches = re.findall(r'ashowinfo[^\n]*?\bn:\s*\d+\s+pts:\s*(-?\d+)[^\n]*?rate:(\d+)[^\n]*?nb_samples:(\d+)', stderr)
    if not matches or any(int(rate) != RATE for _, rate, _ in matches):
        raise ValueError('missing_actual_sample_anchor')
    return {'first_sample_pts': int(matches[0][0]), 'timebase': [1, RATE],
            'sample_count_observed': sum(int(n) for _, _, n in matches)}


def decode(decoder, source, destination, begin, end, directory, label):
    held = os.open(source, os.O_RDONLY | os.O_NOFOLLOW)
    identity = source_identity(source)
    # The controlled P0 source is zero-origin audio PCM. Preserve the actual
    # trim PTS before resetting the engine input; no requested -ss is an anchor.
    af = f'{CLOCK},atrim=start_sample={begin}:end_sample={end},ashowinfo,asetpts=PTS-STARTPTS'
    argv = [str(decoder), '-nostdin', '-v', 'info', '-xerror', '-err_detect', 'explode', '-y',
            '-copyts', '-start_at_zero', '-protocol_whitelist', 'file,pipe', '-threads', '2',
            '-i', f'/dev/fd/{held}', '-map', '0:a:0', '-vn', '-sn', '-dn', '-af', af,
            '-threads', '2', '-ac', '1', '-ar', str(RATE), '-c:a', 'pcm_s16le', '-fs', '8388608', str(destination)]
    try:
        result = timed_child(argv, directory, label, (held,))
    finally:
        os.close(held)
    if result['exit_code'] or source_identity(source) != identity:
        raise ValueError('decode_failed_or_source_changed')
    anchor = clock_anchor((directory / (label + '.stderr')).read_text())
    count = pcm(destination)
    if count != end - begin or anchor['sample_count_observed'] != count or anchor['first_sample_pts'] != begin:
        raise ValueError('controlled_pcm_sample_clock_mismatch')
    return {'process': result, 'anchor': anchor, 'samples': count, 'pcm_sha256': digest(destination)}


def cue_intersection(vtt, anchor, core_begin, core_end, decoded_end):
    """Temporal ownership only; text deduplication/word timing is not proved."""
    if not vtt.startswith('WEBVTT\n') or len(vtt.encode()) > MAX_OUTPUT:
        raise ValueError('invalid_vtt')
    cues, clipped, discarded, previous = [], 0, 0, None
    for block in re.split(r'\n\s*\n', vtt.strip())[1:]:
        lines = block.splitlines()
        if lines and not RECOGNITION.STAMP.fullmatch(lines[0]):
            lines = lines[1:]
        match = RECOGNITION.STAMP.fullmatch(lines[0]) if lines else None
        if not match or len(lines) < 2:
            raise ValueError('invalid_cue')
        fields = list(map(int, match.groups()))
        if any(fields[i] > 59 for i in [1, 2, 5, 6]):
            raise ValueError('invalid_cue_time')
        ms = [fields[i] * 3600000 + fields[i+1] * 60000 + fields[i+2] * 1000 + fields[i+3] for i in (0, 4)]
        if ms[1] <= ms[0] or previous is not None and ms[0] < previous or any(x % 10 for x in ms):
            raise ValueError('invalid_engine_10ms_clock')
        previous = ms[0]
        raw_start, raw_end = anchor + ms[0] * 16, anchor + ms[1] * 16
        start, end = max(raw_start, core_begin), min(raw_end, core_end, decoded_end)
        if end <= start:
            discarded += 1
            continue
        clipped += (start, end) != (raw_start, raw_end)
        cues.append({'start_sample': start, 'end_sample': end,
                     'start_ms_floor': start * 1000 // RATE, 'end_ms_floor': end * 1000 // RATE,
                     'text': '\n'.join(lines[1:])})
    return {'cues': cues, 'clipped': clipped, 'discarded': discarded,
            'text': ' '.join(x['text'] for x in cues),
            'seam_limit': 'Temporal intersection only; complete text may repeat across an independently inferred seam.'}


def baseline_text(sample):
    reference = json.loads((HERE / sample['reference']).read_text())
    value = reference['english_reference']
    if hashlib.sha256(value.encode()).hexdigest() != reference['english_reference_utf8_sha256']:
        raise ValueError('reference_changed')
    return value


def run(args):
    catalog = json.loads((HERE / 'catalog.json').read_text())
    for p in (args.engine_package, args.model, args.decoder, args.output):
        if not p.is_absolute() or p.is_symlink():
            raise ValueError('Require absolute regular paths')
    pressure()
    engine = RECIPE.verify(args.engine_package)
    model_spec = catalog['model'] if args.model_id == 'base' else next(x for x in catalog['comparison_models'] if x['id'] == args.model_id)
    if args.model.stat().st_size != model_spec['download_bytes'] or digest(args.model) != model_spec['sha256']:
        raise ValueError('requires_pinned_multilingual_base')
    pressure()
    if args.phase == 'prepare':
        args.output.mkdir(mode=0o700)
        state = {'source_commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
                 'driver_sha256': digest(Path(__file__)), 'catalog_sha256': digest(HERE / 'catalog.json'),
                 'engine': engine, 'decoder_sha256': digest(args.decoder), 'model_sha256': catalog['model']['sha256'],
                 'model_page_cache': 'warmed by required full digest preflight; not OS-cold evidence',
                 'calls': [], 'engine_attempts': 0, 'qualification': 'offline component diagnostic; no playback/service or human quality claim'}
        prepared = args.output / 'prepared'; prepared.mkdir(mode=0o700)
        for sample in catalog['samples']:
            source = HERE / sample['path']
            if digest(source) != sample['sha256']:
                raise ValueError('fixture_changed')
            destination = prepared / (sample['id'] + '.wav')
            data = decode(args.decoder, source, destination, 0, sample['samples'], prepared, sample['id'])
            state.setdefault('prepared', {})[sample['id']] = data
        boundaries, at = [], 0
        with wave.open(str(prepared / 'german-sequence.wav'), 'wb') as out:
            out.setparams((1, 2, RATE, 0, 'NONE', 'not compressed'))
            for key in catalog['german_sequence']['parts']:
                with wave.open(str(prepared / (key + '.wav')), 'rb') as source:
                    count = source.getnframes(); out.writeframes(source.readframes(count))
                boundaries.append({'id': key, 'start_sample': at, 'end_sample': at + count}); at += count
        state['sequence_boundaries'] = boundaries
        state['sequence_sha256'] = digest(prepared / 'german-sequence.wav')
        save(args.output / 'receipt.json', state)
        return
    state = json.loads((args.output / 'receipt.json').read_text())
    if state['driver_sha256'] != digest(Path(__file__)) or state['catalog_sha256'] != digest(HERE / 'catalog.json') or state['engine']['binary_sha256'] != engine['binary_sha256']:
        raise ValueError('profile_changed_mid_experiment')
    samples = {x['id']: x for x in catalog['samples']}
    total = catalog['german_sequence']['samples']
    if args.phase == 'baselines':
        cases = [(x['id'], x['id'], x['language'], 0, x['samples'], 0, x['samples'], baseline_text(x)) for x in catalog['samples'] if x['baseline']]
        cases.append(('sequence-full', 'german-sequence', 'de', 0, total, 0, total,
                      ' '.join(baseline_text(samples[k]) for k in catalog['german_sequence']['parts'])))
    elif args.phase == 'grid':
        cases = [(f'grid-w{w}-c{c}', 'german-sequence', 'de', max(0, (w-c)*RATE), min(total, (2*w+c)*RATE), w*RATE, min(total,2*w*RATE), None) for w in [10,20,30] for c in [2,5]]
    else:
        if args.width not in [10,20,30] or args.context not in [2,5]:
            raise ValueError('Explicit reviewed candidate width/context required')
        starts = [0,args.width,2*args.width] if args.phase == 'cores' else [args.width]
        cases = [(f'{args.phase}-w{args.width}-c{args.context}-at{s}', 'german-sequence', 'de', max(0,(s-args.context)*RATE),min(total,(s+args.width+args.context)*RATE),s*RATE,min(total,(s+args.width)*RATE),None) for s in starts]
    if args.only_label:
        cases = [x for x in cases if x[0] == args.only_label]
        if not cases:
            raise ValueError('Unknown phase case label')
    for label, key, language, begin, end, core_begin, core_end, reference in cases:
        if args.model_id != 'base':
            label = args.model_id + '-' + label
        prior = [x for x in state['calls'] if x.get('case_label', x['label']) == label]
        if prior:
            if any(x['status'] in ['measured', 'stopped'] for x in prior):
                continue
            if not args.retry_failed:
                raise ValueError('Failed case retained; explicit retry required')
            original_label = label
            label = label + '-attempt' + str(len(prior)+1)
        else:
            original_label = label
        if state['engine_attempts'] >= 18:
            raise ValueError('Original15 plus approved2 replacement attempts exhausted')
        if state['engine_attempts'] >= 15 and not (args.model_id == 'small' and ((args.phase == 'cores' and args.width == 30 and args.context == 5 and label in ['small-cores-w30-c5-at0', 'small-cores-w30-c5-at30']) or (args.phase == 'stop' and args.width == 30 and args.context == 5 and label == 'small-stop-w30-c5-at30'))):
            raise ValueError('Only two approved Small W30/L5/R5 replacement cores allowed')
        pressure()
        case = args.output / label; case.mkdir(mode=0o700)
        item = {'label': label, 'case_label': original_label, 'model_id': args.model_id, 'model_sha256': model_spec['sha256'], 'language': language, 'core_samples': [core_begin,core_end], 'decoded_samples': [begin,end], 'status':'failed'}
        state['calls'].append(item)
        save(args.output / 'receipt.json', state)
        try:
            decoded = case / 'input.wav'
            item['decode'] = decode(args.decoder, args.output / 'prepared' / (key+'.wav'), decoded, begin, end, case, 'decode')
            model_fd = os.open(args.model, os.O_RDONLY | os.O_NOFOLLOW)
            if model_fd != 5:
                os.dup2(model_fd,5); os.close(model_fd)
            model_identity = os.fstat(5)
            try:
                argv = [str(args.engine_package / 'whisper-cli'), '--threads','2','--processors','1','--no-gpu','--language',language,'--translate','--output-vtt','--output-json','--no-prints','--model','/dev/fd/5','--file',str(decoded),'--output-file',str(case/'caption')]
                state['engine_attempts'] += 1; save(args.output / 'receipt.json',state)
                item['inference'] = timed_child(argv,case,'inference',(5,),args.phase=='stop')
                if os.fstat(5) != model_identity or source_identity(args.model) != [model_identity.st_dev,model_identity.st_ino,model_identity.st_size,model_identity.st_mtime_ns,model_identity.st_ctime_ns]:
                    raise ValueError('held_model_changed')
            finally:
                os.close(5)
            if args.phase == 'stop':
                if item['inference']['termination_reason'] != 'deliberate_stop' or not item['inference']['owned_process_group_gone']:
                    raise ValueError('Stop did not interrupt a positively observed engine child')
                item['status'] = 'stopped'; continue
            if item['inference']['exit_code']:
                raise ValueError('inference_failed')
            raw = case/'caption.vtt'; item['raw_vtt_sha256'] = digest(raw)
            item['owned'] = cue_intersection(raw.read_text(),item['decode']['anchor']['first_sample_pts'],core_begin,core_end,end)
            if reference is not None:
                item['reference_token_edit_diagnostic'] = RECOGNITION.error_rate(reference,item['owned']['text'])
                item['reference_metric_limit'] = 'Automatic English token edits, not a translation-quality or listening verdict.'
            item['total_seconds'] = item['decode']['process']['wall_seconds'] + item['inference']['wall_seconds']
            item['rtf_per_owned_core'] = item['total_seconds'] / ((core_end-core_begin)/RATE)
            item['status'] = 'measured'
        except Exception as e:
            item['error'] = str(e); raise
        finally:
            save(args.output / 'receipt.json',state)


class MeasurementTests(unittest.TestCase):
    def test_absolute_sample_clock_and_owned_core_intersection(self):
        v='WEBVTT\n\n00:00:00.000 --> 00:00:02.000\nhello\n\n'
        got=cue_intersection(v,40000,48000,64000,72000)
        self.assertEqual([(x['start_sample'],x['end_sample']) for x in got['cues']],[(48000,64000)])
        self.assertEqual(got['clipped'],1)
    def test_context_only_cues_do_not_own_the_next_core(self):
        v='WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nleft\n\n00:00:03.000 --> 00:00:04.000\nright\n\n'
        got=cue_intersection(v,0,16000,48000,64000);self.assertEqual(got['cues'],[]);self.assertEqual(got['discarded'],2)
    def test_invalid_unordered_and_non_10ms_cues_refuse(self):
        for stamps in ['00:00:01.001 --> 00:00:02.000','00:00:02.000 --> 00:00:01.000','00:00:60.000 --> 00:01:01.000']:
            with self.assertRaises(ValueError):cue_intersection('WEBVTT\n\n'+stamps+'\nx\n\n',0,0,100000,100000)
    def test_actual_anchor_requires_observed_sample_clock(self):
        text='[Parsed_ashowinfo_1] n:0 pts:12000 pts_time:0.75 pos:0 fmt:s16 channels:1 chlayout:mono rate:16000 nb_samples:2048 checksum:0000'
        self.assertEqual(clock_anchor(text)['first_sample_pts'],12000)
        with self.assertRaises(ValueError):clock_anchor('requested -ss 0.75')


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--self-check',action='store_true')
    p.add_argument('--phase',choices=['prepare','baselines','grid','cores','seek','stop'])
    p.add_argument('--engine-package',type=Path);p.add_argument('--model',type=Path);p.add_argument('--decoder',type=Path);p.add_argument('--output',type=Path)
    p.add_argument('--model-id',choices=['base','small'],default='base')
    p.add_argument('--only-label');p.add_argument('--retry-failed',action='store_true')
    p.add_argument('--width',type=int);p.add_argument('--context',type=int)
    args=p.parse_args()
    if args.self_check:
        result=unittest.TextTestRunner().run(unittest.defaultTestLoader.loadTestsFromTestCase(MeasurementTests));return 0 if result.wasSuccessful() else 1
    if not args.phase or any(x is None for x in [args.engine_package,args.model,args.decoder,args.output]):p.error('Supply phase and explicit absolute engine/model/decoder/output paths')
    run(args);return 0


if __name__=='__main__':
    raise SystemExit(main())
