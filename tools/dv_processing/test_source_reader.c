#define _POSIX_C_SOURCE 200809L
#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static pthread_mutex_t hook_mutex = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t hook_changed = PTHREAD_COND_INITIALIZER;
static bool gate, entered, release_read, interrupt_first;
static unsigned calls, fail_after;
static size_t short_limit;
static bool fail_start;
static int controlled_create(pthread_t *thread, const pthread_attr_t *attributes,
                              void *(*entry)(void *), void *argument) {
  return fail_start ? EAGAIN : pthread_create(thread, attributes, entry, argument);
}

static ssize_t controlled_read(int fd, void *buffer, size_t size) {
  pthread_mutex_lock(&hook_mutex);
  unsigned call = ++calls;
  if (gate) {
    entered = true;
    pthread_cond_broadcast(&hook_changed);
    while (!release_read) pthread_cond_wait(&hook_changed, &hook_mutex);
  }
  bool interrupt = interrupt_first && call == 1;
  bool fail = fail_after && call > fail_after;
  size_t limit = short_limit;
  pthread_mutex_unlock(&hook_mutex);
  if (interrupt || fail) { errno = interrupt ? EINTR : EIO; return -1; }
  return read(fd, buffer, limit && size > limit ? limit : size);
}
#define SOURCE_READER_SYSCALL controlled_read
#define SOURCE_READER_CREATE controlled_create
#include "test_source_reader_io.h"

static void reset_hook(void) {
  gate = entered = release_read = interrupt_first = false;
  calls = fail_after = 0;
  short_limit = 0;
}
static void check_pattern(const uint8_t *bytes, size_t size, off_t at) {
  for (size_t i = 0; i < size; i++) assert(bytes[i] == (uint8_t)((at + (off_t)i) % 251));
}
static void exact(struct source_reader *r, uint8_t *buffer, size_t size, off_t at) {
  size_t copied = 0;
  while (copied < size) {
    ssize_t n = source_reader_read(r, buffer + copied, size - copied);
    assert(n > 0 && (size_t)n <= size - copied);
    copied += (size_t)n;
  }
  check_pattern(buffer, size, at);
}
static void await_done(struct source_reader *r) {
  pthread_mutex_lock(&r->mutex);
  while (!r->done) pthread_cond_wait(&r->readable, &r->mutex);
  pthread_mutex_unlock(&r->mutex);
}

static void cursor_and_seek(int fd) {
  reset_hook();
  struct budget_io io = { .fd = fd };
  struct source_reader r = SOURCE_READER_INITIALIZER;
  uint8_t *buffer = malloc(2 * SOURCE_READER_CHUNK);
  assert(buffer && lseek(fd, 4096, SEEK_SET) == 4096);
  assert(test_reader_start(&r, &io) == 0);
  assert(test_reader_start(&r, &io) == -EBUSY);
  exact(&r, buffer, 31, 4096);
  assert(r.delivered == 4127);
  // Callback-delivered cursor, regardless of how much AVIO has consumed.
  assert(test_reader_seek(&r, &io, -20, SEEK_CUR) == 4107);
  exact(&r, buffer, SOURCE_READER_CHUNK + 73, 4107);
  assert(test_reader_seek(&r, &io, 0, 99999) == -EINVAL);
  exact(&r, buffer, 31, 4107 + SOURCE_READER_CHUNK + 73);
  assert(test_reader_seek(&r, &io, 7, SEEK_SET) == 7);
  exact(&r, buffer, 99, 7);
  assert(test_reader_seek(&r, &io, -13, SEEK_END) > 0);
  off_t at = r.delivered;
  exact(&r, buffer, 13, at);
  assert(source_reader_read(&r, buffer, 1) == 0);
  assert(source_reader_stop(&r) == 0);
  assert(io.bytes > SOURCE_READER_CHUNK && io.bytes <= WINDOW_READ_LIMIT);
  source_reader_destroy(&r);
  free(buffer);
}

static void short_reads_and_interrupt(int fd) {
  reset_hook(); short_limit = 17; interrupt_first = true;
  assert(lseek(fd, 0, SEEK_SET) == 0);
  struct budget_io io = { .fd = fd };
  struct source_reader r = SOURCE_READER_INITIALIZER;
  uint8_t buffer[997];
  assert(test_reader_start(&r, &io) == 0);
  exact(&r, buffer, sizeof(buffer), 0);
  assert(source_reader_stop(&r) == 0);
  assert(io.bytes >= sizeof(buffer) && !io.exhausted);
  source_reader_destroy(&r);
}

static void queued_error(int fd) {
  reset_hook(); short_limit = 17; fail_after = 2;
  assert(lseek(fd, 0, SEEK_SET) == 0);
  struct budget_io io = { .fd = fd };
  struct source_reader r = SOURCE_READER_INITIALIZER;
  uint8_t buffer[34];
  assert(test_reader_start(&r, &io) == 0);
  await_done(&r);
  exact(&r, buffer, sizeof(buffer), 0);
  assert(source_reader_read(&r, buffer, 1) == -EIO);
  assert(source_reader_stop(&r) == 0);
  assert(io.bytes == 34 && !io.exhausted);
  source_reader_destroy(&r);
}

static void budget_and_discard(int fd) {
  reset_hook();
  assert(lseek(fd, 0, SEEK_SET) == 0);
  struct budget_io io = { .fd = fd, .bytes = WINDOW_READ_LIMIT - 47 };
  struct source_reader r = SOURCE_READER_INITIALIZER;
  uint8_t buffer[47];
  assert(test_reader_start(&r, &io) == 0);
  await_done(&r);
  assert(io.bytes == WINDOW_READ_LIMIT && !io.exhausted);
  exact(&r, buffer, sizeof(buffer), 0);
  assert(!io.exhausted);
  assert(source_reader_read(&r, buffer, 1) == -ENOSPC && io.exhausted);
  assert(test_reader_seek(&r, &io, 0, SEEK_SET) == 0);
  assert(source_reader_read(&r, buffer, 1) == -ENOSPC);
  assert(io.bytes == WINDOW_READ_LIMIT);
  assert(source_reader_stop(&r) == 0);
  source_reader_destroy(&r);

  reset_hook();
  io = (struct budget_io){ .fd = fd, .bytes = WINDOW_READ_LIMIT - 47 };
  struct source_reader discarded = SOURCE_READER_INITIALIZER;
  assert(test_reader_start(&discarded, &io) == 0);
  await_done(&discarded);
  exact(&discarded, buffer, 3, 0);
  assert(test_reader_seek(&discarded, &io, 100, SEEK_SET) == 100);
  assert(source_reader_read(&discarded, buffer, 1) == -ENOSPC);
  assert(io.bytes == WINDOW_READ_LIMIT); // Discarded bytes never create credit.
  assert(source_reader_stop(&discarded) == 0);
  source_reader_destroy(&discarded);
}

static void stop_full(int fd) {
  reset_hook();
  assert(lseek(fd, 0, SEEK_SET) == 0);
  struct budget_io io = { .fd = fd };
  struct source_reader r = SOURCE_READER_INITIALIZER;
  assert(test_reader_start(&r, &io) == 0);
  pthread_mutex_lock(&r.mutex);
  while (r.count < SOURCE_READER_SLOTS) pthread_cond_wait(&r.readable, &r.mutex);
  pthread_mutex_unlock(&r.mutex);
  assert(source_reader_stop(&r) == 0);
  assert(io.bytes == SOURCE_READER_SLOTS * SOURCE_READER_CHUNK);
  assert(lseek(fd, 0, SEEK_CUR) == 0 && !r.active);
  source_reader_destroy(&r);
}

static void stop_inflight(int fd) {
  reset_hook(); gate = true;
  assert(lseek(fd, 0, SEEK_SET) == 0);
  struct budget_io io = { .fd = fd };
  struct source_reader r = SOURCE_READER_INITIALIZER;
  assert(test_reader_start(&r, &io) == 0);
  pthread_mutex_lock(&hook_mutex);
  while (!entered) pthread_cond_wait(&hook_changed, &hook_mutex);
  pthread_mutex_unlock(&hook_mutex);
  source_reader_request_stop(&r);
  pthread_mutex_lock(&hook_mutex);
  release_read = true;
  pthread_cond_broadcast(&hook_changed);
  pthread_mutex_unlock(&hook_mutex);
  assert(source_reader_join(&r) == 0);
  assert(io.bytes == SOURCE_READER_CHUNK); // The discarded in-flight read is charged.
  assert(lseek(fd, 0, SEEK_CUR) == 0 && !r.active);
  source_reader_destroy(&r);
}

static void start_failure(int fd) {
  reset_hook();
  assert(lseek(fd, 11, SEEK_SET) == 11);
  struct budget_io io = { .fd = fd };
  struct source_reader r = SOURCE_READER_INITIALIZER;
  fail_start = true;
  assert(test_reader_start(&r, &io) == -EAGAIN);
  assert(!r.active && io.bytes == 0 && lseek(fd, 0, SEEK_CUR) == 11);
  fail_start = false;
  assert(test_reader_start(&r, &io) == 0);
  uint8_t byte;
  exact(&r, &byte, 1, 11);
  assert(source_reader_stop(&r) == 0);
  source_reader_destroy(&r);
}

int main(void) {
  assert(WINDOW_READ_LIMIT == 128ULL * 1024 * 1024);
  char path[] = "/tmp/plurx-source-reader-XXXXXX";
  int fd = mkstemp(path);
  assert(fd >= 0 && unlink(path) == 0);
  uint8_t block[65536];
  for (off_t at = 0; at < 8 * 1024 * 1024; at += sizeof(block)) {
    for (size_t i = 0; i < sizeof(block); i++) block[i] = (uint8_t)((at + (off_t)i) % 251);
    assert(write(fd, block, sizeof(block)) == (ssize_t)sizeof(block));
  }
  cursor_and_seek(fd);
  short_reads_and_interrupt(fd);
  queued_error(fd);
  budget_and_discard(fd);
  stop_full(fd);
  stop_inflight(fd);
  start_failure(fd);
  assert(close(fd) == 0);
  puts("source reader cursor, seek, EOF, short-read, EINTR, error, budget and stop controls passed");
}
