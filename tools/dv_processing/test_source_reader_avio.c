#define _POSIX_C_SOURCE 200809L
#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
#include <libavformat/avio.h>
#include <libavutil/error.h>
#include <libavutil/mem.h>
#include "source_reader.h"
static struct source_reader reader = SOURCE_READER_INITIALIZER;
static int callback_read(void *opaque, uint8_t *buffer, int size) {
  struct budget_io *io = opaque;
  if (reader.active) {
    ssize_t n = source_reader_read(&reader, buffer, (size_t)size);
    return n == 0 ? AVERROR_EOF : (int)n;
  }
  if (io->bytes >= WINDOW_READ_LIMIT) { io->exhausted = true; return AVERROR(ENOSPC); }
  uint64_t remaining = WINDOW_READ_LIMIT - io->bytes;
  if ((uint64_t)size > remaining) size = (int)remaining;
  ssize_t n = read(io->fd, buffer, (size_t)size);
  if (n < 0) return AVERROR(errno);
  if (!n) return AVERROR_EOF;
  io->bytes += (uint64_t)n;
  return (int)n;
}
static int64_t callback_seek(void *opaque, int64_t offset, int whence) {
  struct budget_io *io = opaque;
  if (whence & AVSEEK_SIZE) {
    struct stat s;
    return fstat(io->fd, &s) ? AVERROR(errno) : s.st_size;
  }
  return source_reader_seek(&reader, io, offset, whence & ~AVSEEK_FORCE);
}
static void pattern(const uint8_t *buffer, int length, int64_t at) {
  for (int i = 0; i < length; i++) assert(buffer[i] == (uint8_t)((at + i) % 251));
}
int main(void) {
  assert(WINDOW_READ_LIMIT == 128ULL * 1024 * 1024);
  char path[] = "/tmp/plurx-reader-avio-XXXXXX";
  int fd = mkstemp(path);
  assert(fd >= 0 && unlink(path) == 0);
  uint8_t block[65536];
  for (off_t at = 0; at < 8 * 1024 * 1024; at += sizeof(block)) {
    for (size_t i = 0; i < sizeof(block); i++) block[i] = (uint8_t)((at + (off_t)i) % 251);
    assert(write(fd, block, sizeof(block)) == (ssize_t)sizeof(block));
  }
  assert(lseek(fd, 0, SEEK_SET) == 0);
  struct budget_io io = { .fd = fd };
  uint8_t *buffer = av_malloc(65536);
  assert(buffer);
  AVIOContext *avio = avio_alloc_context(buffer, 65536, 0, &io, callback_read, NULL, callback_seek);
  assert(avio);
  assert(avio_read(avio, block, 7) == 7);
  pattern(block, 7, 0);
  assert(avio_tell(avio) == 7 && lseek(fd, 0, SEEK_CUR) > 7);
  // Activate with AVIO still holding unread bytes from its earlier fill.
  assert(source_reader_start(&reader, &io) == 0);
  assert(reader.delivered != avio_tell(avio));
  assert(avio_size(avio) == 8 * 1024 * 1024);
  assert(avio_read(avio, block, 100) == 100);
  pattern(block, 100, 7);
  assert(avio_seek(avio, 3, SEEK_SET) == 3);
  assert(avio_read(avio, block, 100) == 100);
  pattern(block, 100, 3);
  int64_t target = 7 * 1024 * 1024 + 29;
  assert(avio_seek(avio, target, SEEK_SET) == target);
  assert(avio_read(avio, block, 513) == 513);
  pattern(block, 513, target);
  assert(avio_seek(avio, -19, SEEK_CUR) == target + 513 - 19);
  assert(avio_read(avio, block, 99) == 99);
  pattern(block, 99, target + 513 - 19);
  assert(avio_seek(avio, 39, SEEK_SET) == 39);
  assert(avio_read(avio, block, 271) == 271);
  pattern(block, 271, 39);
  assert(avio_seek(avio, 8 * 1024 * 1024 - 13, SEEK_SET) == 8 * 1024 * 1024 - 13);
  assert(avio_read(avio, block, 64) == 13);
  pattern(block, 13, 8 * 1024 * 1024 - 13);
  assert(avio_read(avio, block, 1) == AVERROR_EOF);
  assert(source_reader_stop(&reader) == 0);
  assert(!io.exhausted && io.bytes <= WINDOW_READ_LIMIT);
  av_freep(&avio->buffer);
  avio_context_free(&avio);
  source_reader_destroy(&reader);
  assert(close(fd) == 0);
  puts("pinned AVIO unread-buffer, absolute/relative/backward seek, size and EOF controls passed");
}
