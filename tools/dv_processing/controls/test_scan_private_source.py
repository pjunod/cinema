"""Complete-title private-source integration control; no decode or GPU.

Compile the actual scanner and its IO control using --compile-only before
review. After review, supply an immutable qualified HEVC/DV movie and retained
scanner aggregate. The control compares file, inherited FD and private Range
input, refuses a truncated container header, and exercises the scanner's actual adapter
beyond the renderer's 128 MiB boundary using a virtual source.
"""
import argparse
import contextlib
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import re
import secrets
import shlex
import subprocess
import threading

IO_CONTROL = r'''
#define main unused_scan_title_main
#include SCANNER_SOURCE
#undef main
#include <assert.h>
int main(int argc, char **argv) {
  assert(argc == 3);
  char *end;
  uint64_t expected = strtoull(argv[2], &end, 10);
  assert(!*end && expected > 128ULL * 1024 * 1024);
  AVFormatContext *input = avformat_alloc_context();
  assert(input);
  open_title_io(input, argv[1]);
  assert(movie_seek(&title_io, 0, AVSEEK_SIZE) == (int64_t)expected);
  // Inspect actual kernel socket policy, not only the compile-time constant.
  // This HEAD transfers no original bytes; the scanner's bounded Rust owner
  // owns pause/cancel rather than an unrelated socket wall timeout.
  assert(movie_http_head(&title_io, true, 0) == 0);
  struct timeval timeout;
  socklen_t timeout_size = sizeof(timeout);
  assert(getsockopt(title_io.upstream, SOL_SOCKET, SO_RCVTIMEO, &timeout, &timeout_size) == 0);
  assert(timeout.tv_sec == 0 && timeout.tv_usec == 0);
  timeout_size = sizeof(timeout);
  assert(getsockopt(title_io.upstream, SOL_SOCKET, SO_SNDTIMEO, &timeout, &timeout_size) == 0);
  assert(timeout.tv_sec == 0 && timeout.tv_usec == 0);
  close(title_io.upstream);
  title_io.upstream = -1;
  uint8_t buffer[65536];
  uint64_t bytes = 0;
  int n;
  while ((n = movie_read(&title_io, buffer, sizeof(buffer))) > 0) {
    assert(buffer[0] == bytes % 256);
    assert(buffer[n - 1] == (bytes + (uint64_t)n - 1) % 256);
    bytes += (uint64_t)n;
  }
  assert(n == AVERROR_EOF && bytes == expected);
  assert(title_io.requested_bytes == expected && !title_io.exhausted);
  input->pb = NULL;
  avformat_free_context(input);
  close_title_io();
  assert(!title_avio && title_io.fd == -1 && title_io.upstream == -1);
  puts("whole-title-private-source-beyond-renderer-window-pass");
  return 0;
}
'''


@contextlib.contextmanager
def private_source(length, read_at):
    path = '/' + secrets.token_hex(16)
    ranges = []

    class Handler(BaseHTTPRequestHandler):
        protocol_version = 'HTTP/1.1'

        def log_message(self, *_):
            pass

        def do_HEAD(self):
            if self.path != path:
                self.send_error(404)
                return
            self.send_response(200)
            self.send_header('Content-Length', str(length))
            self.end_headers()

        def do_GET(self):
            match = re.fullmatch(r'bytes=(\d+)-(\d+)', self.headers.get('Range', ''))
            if self.path != path or not match:
                self.send_error(400)
                return
            start, end = map(int, match.groups())
            if not 0 <= start <= end < length:
                self.send_error(416)
                return
            ranges.append((start, end))
            self.send_response(206)
            self.send_header('Content-Length', str(end - start + 1))
            self.send_header('Content-Range', f'bytes {start}-{end}/{length}')
            self.end_headers()
            try:
                at = start
                while at <= end:
                    payload = read_at(at, min(65536, end - at + 1))
                    if not payload:
                        break
                    self.wfile.write(payload)
                    at += len(payload)
            except (BrokenPipeError, ConnectionResetError):
                pass

    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    thread = threading.Thread(target=server.serve_forever)
    thread.start()
    try:
        yield f'http://127.0.0.1:{server.server_port}{path}', ranges
    finally:
        server.shutdown()
        server.server_close()
        thread.join()


def test_complete_title_private_source_matches_qualified_file_and_fd(args):
    source, work, deps = args.source.resolve(), args.work.resolve(), args.dependencies.resolve()
    if not args.compile_only and (args.movie is None or args.expected is None):
        raise ValueError('execution requires an immutable --movie and retained --expected aggregate')
    work.mkdir(exist_ok=False)
    scratch = work / 'scratch'
    scratch.mkdir()
    env = {**os.environ, 'TMPDIR': str(scratch), 'PKG_CONFIG_PATH': str(deps / 'ffmpeg/lib/pkgconfig')}
    sha = lambda p: hashlib.sha256(p.read_bytes()).hexdigest()
    record = {'scope': 'actual complete-title scanner private AVIO and whole-source extent; no media decode/GPU',
              'source_sha256': sha(source), 'header_sha256': sha(source.parent / 'movie_source_io.h'), 'commands': []}

    def command(argv, name, success=True, **kwargs):
        with (work / (name + '.stdout')).open('xb') as out, (work / (name + '.stderr')).open('xb') as err:
            result = subprocess.run(list(map(str, argv)), stdout=out, stderr=err, env=env, timeout=180, **kwargs)
        record['commands'].append({'argv': list(map(str, argv)), 'name': name, 'exit_code': result.returncode,
                                   'stdout_sha256': sha(work / (name + '.stdout')),
                                   'stderr_sha256': sha(work / (name + '.stderr'))})
        (work / 'RESULT.json').write_text(json.dumps(record, indent=2) + '\n')
        assert (result.returncode == 0) == success, (name, result.returncode)
        return (work / (name + '.stdout')).read_text()

    flags = shlex.split(command(['pkg-config', '--static', '--cflags', '--libs', 'libavformat', 'libavcodec', 'libavutil'], 'ffmpeg-flags'))
    common = [os.environ.get('CC', 'cc'), '-O2', '-std=c11', '-Wall', '-Wextra', '-Werror', '-I' + str(deps / 'include')]
    libraries = [str(deps / 'lib/libdovi.a'), *flags, '-lm', '-lpthread', '-ldl']
    scanner = work / 'scan_title'
    command([*common, source, '-o', scanner, *libraries], 'compile-scanner')
    harness = work / 'whole-title-io.c'
    harness.write_text(IO_CONTROL.replace('SCANNER_SOURCE', json.dumps(str(source))))
    io_binary = work / 'whole-title-io'
    command([*common, harness, '-o', io_binary, *libraries], 'compile-scanner-io-control')
    if args.compile_only:
        record['status'] = 'compile-only; controls not executed'
    else:
        movie = args.movie.resolve()
        expected = json.loads(args.expected.read_text())
        assert expected.get('complete') is True and expected.get('decoded_frames', 0) > 0
        record['movie_sha256_before'] = sha(movie)
        assert json.loads(command([scanner, movie, args.video_index], 'qualified-file')) == expected
        with movie.open('rb') as held:
            assert json.loads(command([scanner, f'/dev/fd/{held.fileno()}', args.video_index], 'qualified-fd', pass_fds=(held.fileno(),))) == expected
            with private_source(movie.stat().st_size, lambda at, count: os.pread(held.fileno(), count, at)) as (url, ranges):
                assert json.loads(command([scanner, url, args.video_index], 'qualified-private-url')) == expected
                assert ranges and all(start <= end for start, end in ranges)
                record['private_scan'] = {'range_requests': len(ranges), 'requested_bytes': sum(end - start + 1 for start, end in ranges)}
        # The virtual source exercises the adapter compiled FROM scan_title.c,
        # rather than a copied macro/reader, and needs no large media fixture.
        length = 129 * 1024 * 1024 + 17
        pattern = bytes(range(256))
        def virtual(at, count):
            phase = at % 256
            return (pattern * ((phase + count + 255) // 256))[phase:phase + count]
        with private_source(length, virtual) as (url, ranges):
            output = command([io_binary, url, length], 'whole-title-past-renderer-cap')
            assert output.splitlines() == ['whole-title-private-source-beyond-renderer-window-pass']
            assert sum(end - start + 1 for start, end in ranges) == length
            assert max(end - start + 1 for start, end in ranges) <= 65536
            record['whole_title_io'] = {'bytes': length, 'range_requests': len(ranges)}
        # A short container header is unambiguously incomplete; a generic
        # half-file cut could retain all selected video while dropping only
        # later audio or cues, which must not be rejected as incomplete video.
        truncated = work / 'truncated-header.bin'
        with movie.open('rb') as original, truncated.open('xb') as cut:
            remaining = min(64, movie.stat().st_size // 2)
            while remaining:
                chunk = original.read(min(65536, remaining))
                assert chunk
                cut.write(chunk)
                remaining -= len(chunk)
        with truncated.open('rb') as held:
            with private_source(truncated.stat().st_size, lambda at, count: os.pread(held.fileno(), count, at)) as (url, _):
                command([scanner, url, args.video_index], 'truncated-private-header-refused', success=False)
        assert sha(movie) == record['movie_sha256_before']
        assert sha(source) == record['source_sha256'] and sha(source.parent / 'movie_source_io.h') == record['header_sha256']
        record['status'] = 'complete-title-private-source-controls-pass'
    (work / 'RESULT.json').write_text(json.dumps(record, indent=2) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, required=True)
    parser.add_argument('--work', type=Path, required=True)
    parser.add_argument('--dependencies', type=Path, default=Path('/opt/dv-dependencies'))
    parser.add_argument('--movie', type=Path)
    parser.add_argument('--expected', type=Path)
    parser.add_argument('--video-index', type=int, default=0)
    parser.add_argument('--compile-only', action='store_true')
    test_complete_title_private_source_matches_qualified_file_and_fd(parser.parse_args())
