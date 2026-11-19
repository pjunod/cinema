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
import time

HARNESS = r'''
#define _POSIX_C_SOURCE 200809L
#define WINDOW_READ_LIMIT 64
#include <errno.h>
#include <poll.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdatomic.h>
#include <sys/socket.h>
#include <time.h>
// The one I/O owner resets framing for each actual HTTP connection. Observe
// body bytes by the received CRLFCRLF boundary, including one-byte tail reads.
static atomic_uint_fast64_t observed_body_bytes;
static bool receiving_body, saw_forced_final_byte;
static uint32_t header_tail;
static size_t split_body_used;
static int control_connect(int fd, const struct sockaddr *address, socklen_t length) {
  receiving_body = false;
  header_tail = 0;
  return connect(fd, address, length);
}
static ssize_t control_recv(int fd, void *buffer, size_t size, int flags) {
  bool splitting = receiving_body && split_body_used < 5;
  if (splitting) {
    size_t remaining = split_body_used < 4 ? 4 - split_body_used : 1;
    if (size > remaining) size = remaining;
  }
  ssize_t n = recv(fd, buffer, size, flags);
  if (n > 0) {
    if (splitting) {
      if (split_body_used == 4 && n == 1) saw_forced_final_byte = true;
      split_body_used += (size_t)n;
    }
    for (ssize_t i = 0; i < n; i++) {
      if (receiving_body) atomic_fetch_add(&observed_body_bytes, 1);
      else {
        header_tail = (header_tail << 8) | ((uint8_t *)buffer)[i];
        if (header_tail == UINT32_C(0x0d0a0d0a)) receiving_body = true;
      }
    }
  }
  return n;
}
static bool inject_poll_eintr;
static int control_poll(struct pollfd *fds, nfds_t count, int timeout) {
  if (inject_poll_eintr) {
    inject_poll_eintr = false;
    errno = EINTR;
    return -1;
  }
  return poll(fds, count, timeout);
}
#define connect control_connect
#define recv control_recv
#define poll control_poll
#include MOVIE_SOURCE_HEADER
#undef poll
#undef recv
#undef connect
#include SOURCE_READER_HEADER
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
struct queued_input { struct budget_io *io; bool short_first; size_t maximum; };
static ssize_t queued_read(void *opaque, uint8_t *buffer, size_t size) {
  struct queued_input *input = opaque;
  assert(size <= INT_MAX);
  if (size > input->maximum) size = input->maximum;
  if (input->short_first) {
    input->short_first = false;
    if (size > 5) size = 5;
  }
  int result = movie_read(input->io, buffer, (int)size);
  return result == AVERROR_EOF ? 0 : result;
}
static int64_t queued_seek(void *opaque, int64_t offset, int whence) {
  return movie_seek(((struct queued_input *)opaque)->io, offset, whence);
}
static uint64_t queued_charged(void *opaque) {
  struct budget_io *io = ((struct queued_input *)opaque)->io;
  return io->remote ? io->requested_bytes : io->bytes;
}
static int queued_prepare(void *opaque) {
  return movie_source_interrupt_reset(((struct queued_input *)opaque)->io->interrupt);
}
static void queued_interrupt(void *opaque) {
  movie_source_interrupt_stop(((struct queued_input *)opaque)->io->interrupt);
}
static void await_control(const char *descriptor) {
  char byte;
  ssize_t n;
  do { n = read(atoi(descriptor), &byte, 1); } while (n < 0 && errno == EINTR);
  assert(n == 1 && byte == 'X');
}
static void await_body_bytes(uint64_t expected) {
  struct timespec began, now, delay = {0, 1000000};
  assert(clock_gettime(CLOCK_MONOTONIC, &began) == 0);
  while (atomic_load(&observed_body_bytes) < expected) {
    nanosleep(&delay, NULL);
    assert(clock_gettime(CLOCK_MONOTONIC, &now) == 0);
    assert(now.tv_sec - began.tv_sec < 4);
  }
}
static void read_expected(struct source_reader *reader, int count, int first) {
  uint8_t bytes[80];
  int used = 0;
  while (used < count) {
    ssize_t n = source_reader_read(reader, bytes, (size_t)(count - used));
    assert(n > 0);
    for (int i = 0; i < n; i++) assert(bytes[i] == first + used + i);
    used += (int)n;
  }
}
int main(int argc, char **argv) {
  assert(argc == 3 || argc == 4);
  struct budget_io io = {.fd = -1, .upstream = -1};
  if (!strcmp(argv[1], "cancel-ready")) {
    struct movie_source_interrupt interrupt;
    assert(movie_source_interrupt_init(&interrupt) == 0);
    int sockets[2];
    assert(socketpair(AF_UNIX, SOCK_STREAM, 0, sockets) == 0);
    io.upstream = sockets[0];
    io.interrupt = &interrupt;
    assert(write(sockets[1], "X", 1) == 1);
    movie_source_interrupt_stop(&interrupt);
    assert(movie_socket_wait(&io, POLLIN) == AVERROR(ECANCELED));
    inject_poll_eintr = true;
    assert(movie_socket_wait(&io, POLLIN) == AVERROR(ECANCELED));
    assert(!inject_poll_eintr);
    char byte;
    assert(read(sockets[0], &byte, 1) == 1 && byte == 'X');
    movie_source_close(&io);
    close(sockets[1]);
    assert(movie_source_interrupt_reset(&interrupt) == 0);
    movie_source_interrupt_destroy(&interrupt);
    puts("movie-source-simultaneous-ready-eintr-cancel-wins-pass");
    return 0;
  }
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
  if (!strcmp(argv[1], "cancel-before")) {
    struct movie_source_interrupt interrupt;
    assert(movie_source_interrupt_init(&interrupt) == 0);
    assert(fcntl(interrupt.read_fd, F_GETFL) & O_NONBLOCK);
    assert(fcntl(interrupt.write_fd, F_GETFL) & O_NONBLOCK);
    assert(fcntl(interrupt.read_fd, F_GETFD) & FD_CLOEXEC);
    assert(fcntl(interrupt.write_fd, F_GETFD) & FD_CLOEXEC);
    io.interrupt = &interrupt;
    movie_source_interrupt_stop(&interrupt);
    movie_source_interrupt_stop(&interrupt);
    assert(movie_read(&io, data, 80) == AVERROR(ECANCELED));
    assert(io.bytes == 0 && io.requested_bytes == WINDOW_READ_LIMIT);
    assert(io.upstream == -1);
    assert(movie_source_interrupt_reset(&interrupt) == 0);
    assert(movie_read(&io, data, 1) == AVERROR(ENOSPC));
    assert(io.requested_bytes == WINDOW_READ_LIMIT && io.exhausted);
    movie_source_close(&io);
    // Only a fresh source/window owns a fresh byte budget; reset alone did not.
    assert(movie_source_open(&io, argv[2]) == 0);
    io.interrupt = &interrupt;
    assert(movie_read(&io, data, 20) == 20);
    assert(io.bytes == 20 && io.requested_bytes == 20);
    movie_source_close(&io);
    movie_source_interrupt_destroy(&interrupt);
    assert(interrupt.read_fd == -1 && interrupt.write_fd == -1);
    puts("movie-source-stop-before-connect-and-reset-pass");
    return 0;
  }
  if (!strcmp(argv[1], "cancel-headers") || !strcmp(argv[1], "queued-cancel") ||
      !strcmp(argv[1], "queued-resume") || !strcmp(argv[1], "queued-seek") ||
      !strcmp(argv[1], "queued-restart") || !strcmp(argv[1], "queued-bad-range")) {
    bool invalid = !strcmp(argv[1], "queued-bad-range");
    assert(argc == (invalid ? 3 : 4));
    bool short_first = strcmp(argv[1], "cancel-headers") != 0;
    bool resume = !strcmp(argv[1], "queued-resume");
    bool seek_restart = !strcmp(argv[1], "queued-seek");
    bool restart = !strcmp(argv[1], "queued-restart");
    struct movie_source_interrupt interrupt;
    assert(movie_source_interrupt_init(&interrupt) == 0);
    io.interrupt = &interrupt;
    struct queued_input input = {&io, short_first, seek_restart || restart ? 16 : INT_MAX};
    struct source_reader_io callbacks = {
      .context = &input, .read = queued_read, .seek = queued_seek,
      .charged = queued_charged, .limit = WINDOW_READ_LIMIT,
      .exhausted = &io.exhausted, .prepare = queued_prepare,
      .interrupt = queued_interrupt
    };
    struct source_reader reader = SOURCE_READER_INITIALIZER;
    assert(source_reader_start(&reader, &callbacks) == 0);
    if (short_first && !seek_restart) read_expected(&reader, 5, 0);
    if (invalid) {
      assert(source_reader_read(&reader, data, 1) == AVERROR_INVALIDDATA);
    } else {
      await_control(argv[3]);
    }
    // Five complete bytes preceded five bytes from the CURRENT paused Range.
    // The atomic observation is made by the real recv wrapper, never by racing
    // the I/O owner's counters. Cancel only once that partial read occurred.
    if (seek_restart || restart || !strcmp(argv[1], "queued-cancel"))
      await_body_bytes(10);
    if (seek_restart) {
      assert(source_reader_seek(&reader, &callbacks, 0, SEEK_SET) == 0);
      read_expected(&reader, 43, 0);
      assert(source_reader_read(&reader, data, 1) == -ENOSPC);
    } else if (restart) {
      assert(source_reader_stop(&reader) == 0);
      assert(io.bytes == 10 && io.requested_bytes == 21 && io.position == 5);
      assert(source_reader_read(&reader, data, 1) == -ECANCELED);
      assert(source_reader_start(&reader, &callbacks) == 0);
      read_expected(&reader, 43, 5);
      assert(source_reader_read(&reader, data, 1) == -ENOSPC);
    } else if (resume) {
      read_expected(&reader, 59, 5);
      assert(source_reader_read(&reader, data, 1) == -ENOSPC);
    }
    assert(source_reader_stop(&reader) == 0);
    assert(!reader.active && io.upstream == -1);
    assert(io.requested_bytes == WINDOW_READ_LIMIT);
    uint64_t expected = seek_restart || restart ? 53 : resume ? 64 :
                        invalid ? 5 : short_first ? 10 : 0;
    assert(io.bytes == expected);
    if (short_first) assert(saw_forced_final_byte);
    if (invalid) assert(source_reader_read(&reader, data, 1) == AVERROR_INVALIDDATA);
    else if (!resume && !seek_restart && !restart)
      assert(source_reader_read(&reader, data, 1) == -ECANCELED);
    // Stopping discarded lookahead without refunding the original Range.
    assert(movie_source_interrupt_reset(&interrupt) == 0);
    assert(movie_read(&io, data, 1) == AVERROR(ENOSPC));
    assert(io.requested_bytes == WINDOW_READ_LIMIT && io.exhausted);
    assert(source_reader_seek(&reader, &callbacks, 0, SEEK_SET) == 0);
    assert(source_reader_start(&reader, &callbacks) == 0);
    assert(source_reader_read(&reader, data, 1) == -ENOSPC);
    assert(source_reader_stop(&reader) == 0);
    source_reader_destroy(&reader);
    movie_source_close(&io);
    movie_source_interrupt_destroy(&interrupt);
    puts("movie-source-queued-transport-and-budget-pass");
    return 0;
  }
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
def source_server(mode, redirect=None, paused=None, release=None):
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
            if mode == 'pause-headers':
                paused.set()
                assert release.wait(10), 'header pause was not released'
                return
            self.send_response(206)
            self.send_header('Content-Length', str(end - start + 1))
            wrong_range = mode == 'bad-range' or (mode == 'queued-bad-range' and start == 5)
            self.send_header('Content-Range', f'bytes {start + wrong_range}-{end}/{len(payload)}')
            if mode == 'chunked':
                self.send_header('Transfer-Encoding', 'chunked')
            self.end_headers()
            first_five = sum(item[0] == 'GET' and item[2][0] == 5 for item in requests) == 1
            if mode in ('queued-cancel', 'queued-resume', 'queued-seek', 'queued-restart') and start == 5 and first_five:
                if mode != 'queued-resume':
                    self.wfile.write(payload[start:start + 5])
                    self.wfile.flush()
                paused.set()
                assert release.wait(10), 'body pause was not released'
                if mode != 'queued-resume':
                    return
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
        if release is not None:
            release.set()
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
    reader_header = source.parent / 'source_reader.h'
    sha = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
    record = {'scope': 'original-input URL policy, real loopback Range/redirect and seek/cumulative-byte controls; no DV/GPU',
              'source_sha256': sha(source), 'header_sha256': sha(header),
              'reader_header_sha256': sha(reader_header), 'commands': []}

    def command(argv, name, on_start=None, **kwargs):
        with (work / (name + '.stdout')).open('xb') as out, (work / (name + '.stderr')).open('xb') as err:
            process = subprocess.Popen(argv, stdout=out, stderr=err, env=env, **kwargs)
            try:
                if on_start is not None:
                    on_start(process)
                code = process.wait(timeout=5 if on_start is not None else 90)
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait()
        record['commands'].append({'argv': argv, 'exit_code': code, 'name': name})
        (work / 'RESULT.json').write_text(json.dumps(record, indent=2) + '\n')
        assert code == 0, (name, code)
        return (work / (name + '.stdout')).read_text()

    harness = work / 'movie-source.c'
    harness.write_text(HARNESS.replace('MOVIE_SOURCE_HEADER', json.dumps(str(header)))
                       .replace('SOURCE_READER_HEADER', json.dumps(str(reader_header))))
    flags = shlex.split(command(['pkg-config', *(['--static'] if static_ffmpeg else []), '--cflags', '--libs', 'libavformat', 'libavcodec', 'libavutil'], 'ffmpeg-flags'))
    binary = work / 'movie-source'
    command([os.environ.get('CC', 'cc'), '-O2', '-std=c11', '-Wall', '-Wextra', '-Werror', '-pthread', str(harness), '-o', str(binary), *flags], 'compile')
    if compile_only:
        record['status'] = 'compile-only; regression not executed'
    else:
        command([str(binary), 'policy', 'http://example.invalid/nested'], 'policy')
        command([str(binary), 'cancel-ready', 'unused'], 'simultaneous-ready-eintr')
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
        with source_server('read') as (url, requests):
            command([str(binary), 'cancel-before', url], 'stop-before-connect-reset')
            assert [item[2] for item in requests if item[0] == 'GET'] == [(0, 19)]
        with source_server('queued-bad-range') as (url, requests):
            command([str(binary), 'queued-bad-range', url], 'queued-invaliddata-unchanged')
            assert [item[2] for item in requests if item[0] == 'GET'] == [(0, 4), (5, 63)]
        for action, mode in (('cancel-headers', 'pause-headers'),
                             ('queued-cancel', 'queued-cancel'),
                             ('queued-resume', 'queued-resume'),
                             ('queued-seek', 'queued-seek'),
                             ('queued-restart', 'queued-restart')):
            paused, release = threading.Event(), threading.Event()
            control_read, control_write = os.pipe()
            try:
                with source_server(mode, paused=paused, release=release) as (url, requests):
                    stopped = []

                    def control(process):
                        assert paused.wait(5), 'source did not reach its verified pause'
                        assert process.poll() is None, 'read ended before stop/resume'
                        time.sleep(0.1)
                        assert process.poll() is None, 'paused read did not stay pending'
                        if mode == 'queued-resume':
                            release.set()
                        stopped.append(time.monotonic())
                        os.write(control_write, b'X')

                    command([str(binary), action, url, str(control_read)], action,
                            on_start=control, pass_fds=(control_read,))
                    assert time.monotonic() - stopped[0] < 2, 'owned stop/resume did not complete'
                    expected = {
                        'pause-headers': [(0, 63)],
                        'queued-seek': [(0, 4), (5, 20), (0, 15), (16, 31), (32, 42)],
                        'queued-restart': [(0, 4), (5, 20), (5, 20), (21, 36), (37, 47)],
                    }.get(mode, [(0, 4), (5, 63)])
                    assert [item[2] for item in requests if item[0] == 'GET'] == expected
            finally:
                release.set()
                os.close(control_read)
                os.close(control_write)
        assert sha(source) == record['source_sha256'] and sha(header) == record['header_sha256']
        assert sha(reader_header) == record['reader_header_sha256']
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
