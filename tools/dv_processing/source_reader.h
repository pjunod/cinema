#ifndef PLURX_SOURCE_READER_H
#define PLURX_SOURCE_READER_H
#include <errno.h>
#include <pthread.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <stdio.h>
#include <sys/types.h>

// All successful descriptor reads, including discarded lookahead, spend the
// same allowance. AVIO receives queued bytes before a terminal budget result.
#ifndef WINDOW_READ_LIMIT
#define WINDOW_READ_LIMIT (128ULL * 1024 * 1024)
#endif
struct budget_io {
  int fd;
  uint64_t bytes;
  bool exhausted;
};

#define SOURCE_READER_SLOTS 4
#define SOURCE_READER_CHUNK (1024U * 1024U)
#define SOURCE_READER_STACK (256U * 1024U)
#ifndef SOURCE_READER_SYSCALL
#define SOURCE_READER_SYSCALL read
#endif
#ifndef SOURCE_READER_CREATE
#define SOURCE_READER_CREATE pthread_create
#endif

struct source_reader {
  pthread_mutex_t mutex;
  pthread_cond_t readable, writable;
  pthread_t thread;
  struct budget_io *io;
  uint8_t *data;
  size_t lengths[SOURCE_READER_SLOTS], used, head, count;
  off_t delivered;
  bool active, stop, done, budget_end;
  int error;
};
#define SOURCE_READER_INITIALIZER \
  { .mutex = PTHREAD_MUTEX_INITIALIZER, .readable = PTHREAD_COND_INITIALIZER, \
    .writable = PTHREAD_COND_INITIALIZER }

static void *source_reader_worker(void *argument) {
  struct source_reader *r = argument;
  pthread_mutex_lock(&r->mutex);
  for (;;) {
    while (!r->stop && r->count == SOURCE_READER_SLOTS)
      pthread_cond_wait(&r->writable, &r->mutex);
    if (r->stop) break;
    if (r->io->bytes >= WINDOW_READ_LIMIT) {
      r->budget_end = true;
      break;
    }
    // This vacant slot is the in-flight slot: no fifth buffer exists.
    size_t slot = (r->head + r->count) % SOURCE_READER_SLOTS;
    uint64_t remaining = WINDOW_READ_LIMIT - r->io->bytes;
    size_t request = remaining < SOURCE_READER_CHUNK ? (size_t)remaining : SOURCE_READER_CHUNK;
    pthread_mutex_unlock(&r->mutex);
    ssize_t n;
    int error;
    for (;;) {
      n = SOURCE_READER_SYSCALL(r->io->fd, r->data + slot * SOURCE_READER_CHUNK, request);
      error = errno;
      if (n >= 0 || error != EINTR) break;
      pthread_mutex_lock(&r->mutex);
      bool stop = r->stop;
      pthread_mutex_unlock(&r->mutex);
      if (stop) break;
    }
    pthread_mutex_lock(&r->mutex);
    // Count completed reads even if stop discarded their data.
    if (n > 0) r->io->bytes += (uint64_t)n;
    if (r->stop) break;
    if (n <= 0) {
      if (n < 0) r->error = error;
      break;
    }
    r->lengths[slot] = (size_t)n;
    r->count++;
    pthread_cond_broadcast(&r->readable);
  }
  r->done = true;
  pthread_cond_broadcast(&r->readable);
  pthread_mutex_unlock(&r->mutex);
  return NULL;
}

static int source_reader_start(struct source_reader *r, struct budget_io *io) {
  if (r->active) return -EBUSY;
  off_t at = lseek(io->fd, 0, SEEK_CUR);
  if (at < 0) return -errno;
  if (!r->data) {
    r->data = malloc(SOURCE_READER_SLOTS * SOURCE_READER_CHUNK);
    if (!r->data) return -ENOMEM;
  }
  r->io = io;
  r->delivered = at;
  r->head = r->count = r->used = 0;
  r->stop = r->done = r->budget_end = false;
  r->error = 0;
  pthread_attr_t attributes;
  int error = pthread_attr_init(&attributes);
  if (error) return -error;
  error = pthread_attr_setstacksize(&attributes, SOURCE_READER_STACK);
  if (!error) error = SOURCE_READER_CREATE(&r->thread, &attributes, source_reader_worker, r);
  pthread_attr_destroy(&attributes);
  if (error) return -error;
  r->active = true;
  return 0;
}

static ssize_t source_reader_read(struct source_reader *r, uint8_t *buffer, size_t size) {
  if (!size) return 0;
  pthread_mutex_lock(&r->mutex);
  while (!r->count && !r->done && !r->stop)
    pthread_cond_wait(&r->readable, &r->mutex);
  ssize_t result;
  if (r->count) {
    size_t available = r->lengths[r->head] - r->used;
    size_t n = size < available ? size : available;
    memcpy(buffer, r->data + r->head * SOURCE_READER_CHUNK + r->used, n);
    r->used += n;
    r->delivered += (off_t)n;
    if (r->used == r->lengths[r->head]) {
      r->head = (r->head + 1) % SOURCE_READER_SLOTS;
      r->used = 0;
      r->count--;
      pthread_cond_broadcast(&r->writable);
    }
    result = (ssize_t)n;
  } else if (r->budget_end) {
    r->io->exhausted = true;
    result = -ENOSPC;
  } else if (r->error) {
    result = -r->error;
  } else {
    result = r->stop ? -ECANCELED : 0;
  }
  pthread_mutex_unlock(&r->mutex);
  return result;
}

static void source_reader_request_stop(struct source_reader *r) {
  if (!r->active) return;
  pthread_mutex_lock(&r->mutex);
  r->stop = true;
  pthread_cond_broadcast(&r->readable);
  pthread_cond_broadcast(&r->writable);
  pthread_mutex_unlock(&r->mutex);
}

// A blocked filesystem read has no promised completion bound. Its worker
// stays inside the owned helper; the daemon retains admission until actual reap.
static int source_reader_join(struct source_reader *r) {
  if (!r->active) return 0;
  int error = pthread_join(r->thread, NULL);
  if (error) return -error;
  r->active = false;
  r->head = r->count = r->used = 0;
  // AVIO owns any bytes already delivered by its callback, including unread
  // bytes in its own buffer. This is NOT avio_tell's consumer cursor.
  return lseek(r->io->fd, r->delivered, SEEK_SET) < 0 ? -errno : 0;
}

static int source_reader_stop(struct source_reader *r) {
  source_reader_request_stop(r);
  return source_reader_join(r);
}

static int64_t source_reader_seek(struct source_reader *r, struct budget_io *io,
                                  int64_t offset, int whence) {
  bool restart = r->active;
  int error = source_reader_stop(r);
  if (error) return error;
  off_t result = lseek(io->fd, (off_t)offset, whence);
  int seek_error = errno;
  if (restart) {
    error = source_reader_start(r, io);
    if (error && result >= 0) return error;
  }
  return result < 0 ? -seek_error : (int64_t)result;
}

static void source_reader_destroy(struct source_reader *r) {
  if (r->active) abort();
  free(r->data);
  r->data = NULL;
  pthread_cond_destroy(&r->readable);
  pthread_cond_destroy(&r->writable);
  pthread_mutex_destroy(&r->mutex);
}
#endif
