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

// The source owner remains authoritative for transport, offsets and charging.
// read returns bytes, zero for EOF, or an unchanged negative error. Remote
// requests charge their entire Range, including failed/discarded lookahead.
struct source_reader_io {
  void *context;
  ssize_t (*read)(void *, uint8_t *, size_t);
  int64_t (*seek)(void *, int64_t, int);
  uint64_t (*charged)(void *);
  uint64_t limit;
  bool *exhausted;
  int (*prepare)(void *);
  void (*interrupt)(void *);
};

#define SOURCE_READER_SLOTS 4
#define SOURCE_READER_CHUNK (1024U * 1024U)
#define SOURCE_READER_STACK (256U * 1024U)
#ifndef SOURCE_READER_CREATE
#define SOURCE_READER_CREATE pthread_create
#endif

struct source_reader {
  pthread_mutex_t mutex;
  pthread_cond_t readable, writable;
  pthread_t thread;
  struct source_reader_io io;
  uint8_t *data;
  size_t lengths[SOURCE_READER_SLOTS], used, head, count;
  int64_t delivered;
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
    uint64_t charged = r->io.charged(r->io.context);
    if (charged >= r->io.limit) {
      r->budget_end = true;
      break;
    }
    // This vacant slot is the in-flight slot: no fifth buffer exists.
    size_t slot = (r->head + r->count) % SOURCE_READER_SLOTS;
    uint64_t remaining = r->io.limit - charged;
    size_t request = remaining < SOURCE_READER_CHUNK ? (size_t)remaining : SOURCE_READER_CHUNK;
    pthread_mutex_unlock(&r->mutex);
    ssize_t n;
    for (;;) {
      n = r->io.read(r->io.context, r->data + slot * SOURCE_READER_CHUNK, request);
      if (n != -EINTR) break;
      pthread_mutex_lock(&r->mutex);
      bool stop = r->stop;
      pthread_mutex_unlock(&r->mutex);
      if (stop) break;
    }
    pthread_mutex_lock(&r->mutex);
    // The raw source callback has already charged completed/abandoned reads.
    if (r->stop) break;
    if (n <= 0) {
      if (n < 0) r->error = (int)n;
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

static int source_reader_start(struct source_reader *r, const struct source_reader_io *io) {
  if (r->active) return -EBUSY;
  if (!io || !io->read || !io->seek || !io->charged || !io->exhausted || !io->limit)
    return -EINVAL;
  if (io->prepare) {
    int error = io->prepare(io->context);
    if (error) return error;
  }
  int64_t at = io->seek(io->context, 0, SEEK_CUR);
  if (at < 0) return (int)at;
  if (!r->data) {
    r->data = malloc(SOURCE_READER_SLOTS * SOURCE_READER_CHUNK);
    if (!r->data) return -ENOMEM;
  }
  r->io = *io;
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
    r->delivered += (int64_t)n;
    if (r->used == r->lengths[r->head]) {
      r->head = (r->head + 1) % SOURCE_READER_SLOTS;
      r->used = 0;
      r->count--;
      pthread_cond_broadcast(&r->writable);
    }
    result = (ssize_t)n;
  } else if (r->budget_end) {
    *r->io.exhausted = true;
    result = -ENOSPC;
  } else if (r->error) {
    result = r->error;
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
  if (r->io.interrupt) r->io.interrupt(r->io.context);
}

// A blocked filesystem read has no promised completion bound. Interruptible
// service reads wake through their owner's cancel pipe; daemon settlement
// still retains admission until actual child reap and source-read completion.
static int source_reader_join(struct source_reader *r) {
  if (!r->active) return 0;
  int error = pthread_join(r->thread, NULL);
  if (error) return -error;
  r->active = false;
  r->head = r->count = r->used = 0;
  // AVIO owns any bytes already delivered by its callback, including unread
  // bytes in its own buffer. This is NOT avio_tell's consumer cursor.
  int64_t at = r->io.seek(r->io.context, r->delivered, SEEK_SET);
  return at < 0 ? (int)at : at == r->delivered ? 0 : -EIO;
}

static int source_reader_stop(struct source_reader *r) {
  source_reader_request_stop(r);
  return source_reader_join(r);
}

static int64_t source_reader_seek(struct source_reader *r, const struct source_reader_io *io,
                                  int64_t offset, int whence) {
  bool restart = r->active;
  int error = source_reader_stop(r);
  if (error) return error;
  int64_t result = io->seek(io->context, offset, whence);
  if (restart) {
    error = source_reader_start(r, io);
    if (error && result >= 0) return error;
  }
  return result;
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
