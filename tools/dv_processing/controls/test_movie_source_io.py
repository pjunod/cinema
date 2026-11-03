"""CPU-only original-input capability/Range/budget control (no DV/GPU SDK).

Compile with --compile-only before review. The default also executes the focused
regression and records commands; run it only after the required review.
"""
import argparse
import contextlib
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import threading

HARNESS = r'''
#define _POSIX_C_SOURCE 200809L
#define WINDOW_READ_LIMIT 64
#include MOVIE_SOURCE_HEADER
#include <assert.h>
struct playlist_input { const char *text; size_t offset, length; };
static int playlist_read(void *opaque, uint8_t *buffer, int size) {
  struct playlist_input *input = opaque;
  size_t n = input->length - input->offset;
  if (n > (size_t)size) n = (size_t)size;
  if (!n) return AVERROR_EOF;
  memcpy(buffer, input->text + input->offset, n);
  input->offset += n;
  return (int)n;
}
int main(int argc, char **argv) {
  assert(argc == 3);
  struct budget_io io = {.fd = -1, .upstream = -1};
  if (!strcmp(argv[1], "policy")) {
    assert(movie_source_url("http://127.0.0.1:1/0123456789abcdef0123456789abcdef", &io));
    assert(movie_source_url("http://127.0.0.1:65535/ABCDEF0123456789ABCDEF0123456789", &io));
    const char *bad[] = {
      "http://localhost:8080/0123456789abcdef0123456789abcdef",
      "http://127.0.0.2:8080/0123456789abcdef0123456789abcdef",
      "https://127.0.0.1:8080/0123456789abcdef0123456789abcdef",
      "http://user@127.0.0.1:8080/0123456789abcdef0123456789abcdef",
      "http://127.0.0.1:0/0123456789abcdef0123456789abcdef",
      "http://127.0.0.1:65536/0123456789abcdef0123456789abcdef",
      "http://127.0.0.1:080/0123456789abcdef0123456789abcdef",
      "http://127.0.0.1:80/0123456789abcdef0123456789abcdef?x",
      "http://127.0.0.1:80/0123456789abcdef0123456789abcdef#x",
      "http://127.0.0.1:80/0123456789abcdef0123456789abcdef/",
      "http://127.0.0.1:80/0123456789abcdef0123456789abcdeg",
      "http://127.0.0.1:80/short",
      "http://127.0.0.1:80/0123456789abcdef0123456789abcde%66"
    };
    for (size_t i = 0; i < sizeof(bad) / sizeof(*bad); i++) {
      assert(!movie_source_url(bad[i], &io));
      assert(movie_source_open(&io, bad[i]) < 0);
      assert(io.fd == -1 && io.upstream == -1);
    }
    AVIOContext *nested = NULL;
    AVDictionary *options = NULL;
    assert(refuse_nested_movie_io(NULL, &nested, argv[2], AVIO_FLAG_READ, &options) == AVERROR(EPERM));
    assert(nested == NULL);
    puts("movie-source-policy-pass");
    return 0;
  }
  if (!strcmp(argv[1], "nested")) {
    char playlist[512];
    int length = snprintf(playlist, sizeof(playlist),
        "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n"
        "#EXTINF:1,\n%s\n#EXT-X-ENDLIST\n", argv[2]);
    struct playlist_input content = {playlist, 0, (size_t)length};
    AVFormatContext *context = avformat_alloc_context();
    assert(context && movie_source_bind_demuxer(context) == 0);
    AVIOContext *pb = avio_alloc_context(av_malloc(4096), 4096, 0, &content,
                                        playlist_read, NULL, NULL);
    assert(pb);
    context->pb = pb;
    context->flags |= AVFMT_FLAG_CUSTOM_IO;
    assert(avformat_open_input(&context, "untrusted.m3u8", NULL, NULL) < 0);
    avformat_close_input(&context);
    av_freep(&pb->buffer);
    avio_context_free(&pb);
    puts("movie-source-nested-refused-pass");
    return 0;
  }
  if (!strcmp(argv[1], "reject")) {
    assert(movie_source_open(&io, argv[2]) < 0);
    assert(io.fd == -1 && io.upstream == -1 && io.bytes == 0);
    puts("movie-source-reject-pass");
    return 0;
  }
  assert(movie_source_open(&io, argv[2]) == 0);
  uint8_t data[80];
  if (!strcmp(argv[1], "partial")) {
    assert(movie_read(&io, data, 80) == 5);
    assert(io.bytes == 5 && io.requested_bytes == WINDOW_READ_LIMIT);
    assert(movie_read(&io, data, 1) == AVERROR(ENOSPC));
    assert(io.exhausted && io.upstream == -1);
    movie_source_close(&io);
    puts("movie-source-partial-remains-charged-pass");
    return 0;
  }
  if (!strcmp(argv[1], "bad-range")) {
    assert(movie_read(&io, data, 20) < 0);
    assert(io.upstream == -1 && io.bytes == 0);
    movie_source_close(&io);
    puts("movie-source-bad-range-pass");
    return 0;
  }
  assert(movie_seek(&io, 0, AVSEEK_SIZE) == 1024);
  assert(movie_seek(&io, 0, SEEK_END) == 1024);
  assert(movie_read(&io, data, 1) == AVERROR_EOF);
  assert(movie_seek(&io, 0, SEEK_SET) == 0);
  assert(movie_read(&io, data, 20) == 20);
  for (int i = 0; i < 20; i++) assert(data[i] == i);
  assert(movie_seek(&io, -10, SEEK_CUR | AVSEEK_FORCE) == 10);
  assert(movie_read(&io, data, 40) == 40);
  for (int i = 0; i < 40; i++) assert(data[i] == i + 10);
  assert(movie_seek(&io, -1025, SEEK_END) < 0);
  assert(movie_seek(&io, 0, SEEK_SET) == 0);
  assert(movie_read(&io, data, 80) == 4);
  for (int i = 0; i < 4; i++) assert(data[i] == i);
  assert(movie_seek(&io, 0, SEEK_SET) == 0);
  assert(movie_read(&io, data, 1) == AVERROR(ENOSPC));
  assert(io.bytes == WINDOW_READ_LIMIT && io.exhausted);
  movie_source_close(&io);
  assert(io.fd == -1 && io.upstream == -1);
  puts("movie-source-budget-pass");
  return 0;
}
'''


@contextlib.contextmanager
def source_server(mode, redirect=None):
    requests = []
    nonce = '/0123456789abcdef0123456789abcdef'
    payload = bytes(range(256)) * 4

    class Handler(BaseHTTPRequestHandler):
        protocol_version = 'HTTP/1.1'

        def log_message(self, *_):
            pass

        def do_HEAD(self):
            requests.append(('HEAD', self.path, None))
            assert self.path == nonce
            self.send_response(302 if mode == 'redirect' else 200)
            self.send_header('Content-Length', str(len(payload)))
            if redirect:
                self.send_header('Location', redirect)
            self.end_headers()

        def do_GET(self):
            match = re.fullmatch(r'bytes=(\d+)-(\d+)', self.headers['Range'])
            assert self.path == nonce and match
            start, end = map(int, match.groups())
            requests.append(('GET', self.path, (start, end)))
            self.send_response(206)
            self.send_header('Content-Length', str(end - start + 1))
            self.send_header('Content-Range', f'bytes {start + (mode == "bad-range")}-{end}/{len(payload)}')
            if mode == 'chunked':
                self.send_header('Transfer-Encoding', 'chunked')
            self.end_headers()
            try:
                self.wfile.write(payload[start:start + 5] if mode == 'partial' else payload[start:end + 1])
            except (BrokenPipeError, ConnectionResetError):
                pass

    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    thread = threading.Thread(target=server.serve_forever)
    thread.start()
    try:
        yield f'http://127.0.0.1:{server.server_port}{nonce}', requests
    finally:
        server.shutdown()
        server.server_close()
        thread.join()


def test_private_movie_source_url_budget_and_nested_io(source, work, compile_only=False, static_ffmpeg=False):
    source, work = source.resolve(), work.resolve()
    work.mkdir(exist_ok=False)
    scratch = work / 'scratch'
    scratch.mkdir()
    env = {**os.environ, 'TMPDIR': str(scratch)}
    header = source.parent / 'movie_source_io.h'
    sha = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
    record = {'scope': 'original-input URL policy, real loopback Range/redirect and seek/cumulative-byte controls; no DV/GPU',
              'source_sha256': sha(source), 'header_sha256': sha(header), 'commands': []}

    def command(argv, name, **kwargs):
        with (work / (name + '.stdout')).open('xb') as out, (work / (name + '.stderr')).open('xb') as err:
            result = subprocess.run(argv, stdout=out, stderr=err, env=env, timeout=90, **kwargs)
        record['commands'].append({'argv': argv, 'exit_code': result.returncode, 'name': name})
        (work / 'RESULT.json').write_text(json.dumps(record, indent=2) + '\n')
        assert result.returncode == 0, (name, result.returncode)
        return (work / (name + '.stdout')).read_text()

    harness = work / 'movie-source.c'
    harness.write_text(HARNESS.replace('MOVIE_SOURCE_HEADER', json.dumps(str(header))))
    flags = shlex.split(command(['pkg-config', *(['--static'] if static_ffmpeg else []), '--cflags', '--libs', 'libavformat', 'libavcodec', 'libavutil'], 'ffmpeg-flags'))
    binary = work / 'movie-source'
    command([os.environ.get('CC', 'cc'), '-O2', '-std=c11', '-Wall', '-Wextra', '-Werror', str(harness), '-o', str(binary), *flags], 'compile')
    if compile_only:
        record['status'] = 'compile-only; regression not executed'
    else:
        command([str(binary), 'policy', 'http://example.invalid/nested'], 'policy')
        local = work / 'original.bin'
        local.write_bytes(bytes(range(256)) * 4)
        command([str(binary), 'read', str(local)], 'regular-file')
        with local.open('rb') as held:
            command([str(binary), 'read', f'/dev/fd/{held.fileno()}'], 'inherited-fd', pass_fds=(held.fileno(),))
        with source_server('read') as (url, requests):
            command([str(binary), 'read', url], 'http-range')
            assert [item[2] for item in requests if item[0] == 'GET'] == [(0, 19), (10, 49), (0, 3)]
        with source_server('read') as (sink, sink_requests):
            command([str(binary), 'nested', sink], 'nested-media-refused')
            assert not sink_requests
            with source_server('redirect', sink) as (url, requests):
                command([str(binary), 'reject', url], 'redirect-refused')
                assert len(requests) == 1 and not sink_requests
        with source_server('partial') as (url, requests):
            command([str(binary), 'partial', url], 'partial-response-budget-retained')
            assert [item[2] for item in requests if item[0] == 'GET'] == [(0, 63)]
        for mode in ('bad-range', 'chunked'):
            with source_server(mode) as (url, requests):
                command([str(binary), 'bad-range', url], mode)
                assert len(requests) == 2
        assert sha(source) == record['source_sha256'] and sha(header) == record['header_sha256']
        record['status'] = 'movie-source-controls-pass'
    (work / 'RESULT.json').write_text(json.dumps(record, indent=2) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, required=True)
    parser.add_argument('--work', type=Path, required=True)
    parser.add_argument('--compile-only', action='store_true')
    parser.add_argument('--static-ffmpeg', action='store_true', help='link the SDK static FFmpeg dependency closure')
    args = parser.parse_args()
    test_private_movie_source_url_budget_and_nested_io(args.source, args.work, args.compile_only, args.static_ffmpeg)
