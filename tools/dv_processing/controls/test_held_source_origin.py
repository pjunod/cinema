"""Focused CPU-only held-origin/EOF protocol control; compile and run in the SDK.

Includes the actual helper source but calls only its pure origin/extent functions.
No media, demux probing, layer decoder, renderer, encoder or GPU is invoked.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shlex
import subprocess


HARNESS = r'''
#define main unused_segment_decode_render_main
#include HELPER_SOURCE
#undef main
#include <assert.h>
int main(void) {
  AVStream stream = {0};
  stream.time_base = (AVRational){1, 1000};
  stream.start_time = AV_NOPTS_VALUE;
  struct rational origin;
  int64_t ticks, endpoint;
  assert(bind_source_origin(true, (struct rational){1, 8}, &stream, &origin, &ticks));
  assert(ticks == 125 && origin.num == 1 && origin.den == 8);
  // Measured partial demux can change this value after reordered packet reads.
  // Its missing and later nonmissing states must never replace the held epoch.
  stream.start_time = 208;
  assert(bind_source_origin(true, (struct rational){1, 8}, &stream, &origin, &ticks));
  assert(ticks == 125);
  assert(av_compare_ts(ticks, stream.time_base, 125, (AVRational){1,1000}) == 0);
  assert(av_compare_ts(ticks, stream.time_base, 124, (AVRational){1,1000}) > 0);
  assert(!bind_source_origin(true, (struct rational){-1, 8}, &stream, &origin, &ticks));
  assert(!bind_source_origin(true, (struct rational){1, 0}, &stream, &origin, &ticks));
  assert(!bind_source_origin(true, (struct rational){1, 3}, &stream, &origin, &ticks));
  assert(!bind_source_origin(true, (struct rational){INT64_MAX, 1}, &stream, &origin, &ticks));
  stream.time_base = (AVRational){0, 1000};
  assert(!bind_source_origin(true, (struct rational){0, 1}, &stream, &origin, &ticks));
  stream.time_base = (AVRational){1, 1000};
  stream.start_time = AV_NOPTS_VALUE;
  assert(!bind_source_origin(false, (struct rational){0, 1}, &stream, &origin, &ticks));
  stream.start_time = 208;
  assert(!bind_source_origin(false, (struct rational){0, 1}, &stream, &origin, &ticks));
  stream.start_time = 0;
  assert(bind_source_origin(false, (struct rational){0, 1}, &stream, &origin, &ticks));
  assert(ticks == 0 && origin.num == 0);
  assert(declared_video_endpoint(125, 1000, &endpoint) && endpoint == 1125);
  stream.start_time = AV_NOPTS_VALUE;
  assert(declared_video_endpoint(125, 1000, &endpoint) && endpoint == 1125);
  stream.start_time = 208;
  assert(declared_video_endpoint(125, 1000, &endpoint) && endpoint == 1125);
  assert(!declared_video_endpoint(125, INT64_MAX, &endpoint));
  assert(!declared_video_endpoint(-1, 1000, &endpoint));
  assert(!declared_video_endpoint(0, 0, &endpoint));
  assert(declared_video_endpoint(INT64_MAX - 1, 1, &endpoint) && endpoint == INT64_MAX);
  puts("held-origin-pure-contract-pass");
  return 0;
}
'''


def test_held_origin_protocol_with_mutable_demux_state(source, work, dependencies):
    source, work, dependencies = source.resolve(), work.resolve(), dependencies.resolve()
    work.mkdir(exist_ok=False)
    (work / 'scratch').mkdir()
    sha = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
    record = {'scope': 'pure held-origin/EOF CPU contract only; zero media/GPU',
              'source_sha256': sha(source), 'commands': []}
    def command(argv, name, env=None):
        with (work / (name + '.stdout')).open('xb') as out, (work / (name + '.stderr')).open('xb') as err:
            result = subprocess.run(argv, stdout=out, stderr=err, env=env, timeout=240)
        record['commands'].append({'argv': argv, 'name': name, 'exit_code': result.returncode,
                                  'stdout_sha256': sha(work / (name + '.stdout')),
                                  'stderr_sha256': sha(work / (name + '.stderr'))})
        (work / 'RESULT.json').write_text(json.dumps(record, indent=2) + '\n')
        assert result.returncode == 0, (name, result.returncode)
        return (work / (name + '.stdout')).read_text()
    (work / 'held-origin.c').write_text(HARNESS.replace('HELPER_SOURCE', json.dumps(str(source))))
    env = {**os.environ, 'TMPDIR': str(work / 'scratch'),
           'LD_LIBRARY_PATH': str(dependencies / 'placebo/lib/x86_64-linux-gnu') +
           (':' + os.environ['LD_LIBRARY_PATH'] if os.environ.get('LD_LIBRARY_PATH') else '')}
    ff = shlex.split(command(['pkg-config', '--static', '--cflags', '--libs', 'libavformat', 'libavcodec', 'libavutil'],
        'ffmpeg-flags', {**env, 'PKG_CONFIG_PATH': str(dependencies / 'ffmpeg/lib/pkgconfig')}))
    pl = shlex.split(command(['pkg-config', '--cflags', '--libs', 'libplacebo'], 'placebo-flags',
        {**env, 'PKG_CONFIG_PATH': str(dependencies / 'placebo/lib/x86_64-linux-gnu/pkgconfig')}))
    command(['cc', '-O2', '-fno-math-errno', '-frounding-math', '-std=c11', '-Wall', '-Wextra', '-Werror',
             '-I' + str(dependencies / 'include'), str(work / 'held-origin.c'), str(dependencies / 'lib/libdovi.a'),
             '-o', str(work / 'held-origin'), *ff, *pl, '-lm', '-lpthread', '-ldl'], 'compile', env)
    output = command([str(work / 'held-origin')], 'pure-contract', env)
    assert output.splitlines() == ['held-origin-pure-contract-pass']
    assert sha(source) == record['source_sha256']
    record.update(status='pure-held-origin-contract-pass', source_unchanged=True)
    (work / 'RESULT.json').write_text(json.dumps(record, indent=2) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, required=True)
    parser.add_argument('--work', type=Path, required=True)
    parser.add_argument('--dependencies', type=Path, default=Path('/opt/dv-dependencies'))
    args = parser.parse_args()
    test_held_origin_protocol_with_mutable_demux_state(args.source, args.work, args.dependencies)
