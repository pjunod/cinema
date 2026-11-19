#ifndef PLURX_TEST_SOURCE_READER_IO_H
#define PLURX_TEST_SOURCE_READER_IO_H
#include "source_reader.h"

#ifndef WINDOW_READ_LIMIT
#define WINDOW_READ_LIMIT (128ULL * 1024 * 1024)
#endif
#ifndef SOURCE_READER_SYSCALL
#define SOURCE_READER_SYSCALL read
#endif
// A local test source owns its returned-byte budget, just as movie_source_io
// owns production local/remote accounting. The queue never charges it again.
struct budget_io {
  int fd;
  uint64_t bytes;
  bool exhausted;
};
static ssize_t test_source_read(void *opaque, uint8_t *buffer, size_t size) {
  struct budget_io *io = opaque;
  if (io->bytes >= WINDOW_READ_LIMIT) return -ENOSPC;
  uint64_t remaining = WINDOW_READ_LIMIT - io->bytes;
  if (size > remaining) size = (size_t)remaining;
  ssize_t n = SOURCE_READER_SYSCALL(io->fd, buffer, size);
  if (n < 0) return -errno;
  if (n > 0) io->bytes += (uint64_t)n;
  return n;
}
static int64_t test_source_seek(void *opaque, int64_t offset, int whence) {
  struct budget_io *io = opaque;
  off_t result = lseek(io->fd, (off_t)offset, whence);
  return result < 0 ? -errno : (int64_t)result;
}
static uint64_t test_source_charged(void *opaque) {
  return ((struct budget_io *)opaque)->bytes;
}
static struct source_reader_io test_source_callbacks(struct budget_io *io) {
  return (struct source_reader_io){
    .context = io, .read = test_source_read, .seek = test_source_seek,
    .charged = test_source_charged, .limit = WINDOW_READ_LIMIT,
    .exhausted = &io->exhausted,
  };
}
static int test_reader_start(struct source_reader *r, struct budget_io *io) {
  struct source_reader_io source = test_source_callbacks(io);
  return source_reader_start(r, &source);
}
static int64_t test_reader_seek(struct source_reader *r, struct budget_io *io,
                                int64_t offset, int whence) {
  struct source_reader_io source = test_source_callbacks(io);
  return source_reader_seek(r, &source, offset, whence);
}
#endif
