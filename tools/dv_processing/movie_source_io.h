#ifndef PLURX_MOVIE_SOURCE_IO_H
#define PLURX_MOVIE_SOURCE_IO_H
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <limits.h>
#include <poll.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <strings.h>
#include <sys/stat.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <netinet/in.h>
#include <unistd.h>
#include <libavformat/avformat.h>

#ifndef WINDOW_READ_LIMIT
#define WINDOW_READ_LIMIT (128ULL * 1024 * 1024)
#endif
// The controller latches stop without touching the I/O owner's socket.
// Reset and destruction require the reader to have joined.
struct movie_source_interrupt { int read_fd, write_fd; };

static inline int movie_source_interrupt_init(struct movie_source_interrupt *interrupt) {
  interrupt->read_fd = interrupt->write_fd = -1;
  int descriptors[2];
  if (pipe(descriptors)) return AVERROR(errno);
  for (int i = 0; i < 2; i++) {
    int flags = fcntl(descriptors[i], F_GETFL);
    if (flags < 0 || fcntl(descriptors[i], F_SETFL, flags | O_NONBLOCK) < 0 ||
        fcntl(descriptors[i], F_SETFD, FD_CLOEXEC) < 0) {
      int error = errno;
      close(descriptors[0]); close(descriptors[1]);
      return AVERROR(error);
    }
  }
  interrupt->read_fd = descriptors[0];
  interrupt->write_fd = descriptors[1];
  return 0;
}
static inline int movie_source_interrupt_reset(struct movie_source_interrupt *interrupt) {
  if (interrupt->read_fd < 0) return AVERROR(EINVAL);
  char bytes[64];
  for (;;) {
    ssize_t n = read(interrupt->read_fd, bytes, sizeof(bytes));
    if (n > 0 || (n < 0 && errno == EINTR)) continue;
    if (n < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) return 0;
    return n < 0 ? AVERROR(errno) : AVERROR(EPIPE);
  }
}
static inline void movie_source_interrupt_stop(struct movie_source_interrupt *interrupt) {
  if (interrupt->write_fd < 0) return;
  const char stopped = 1;
  ssize_t n;
  do { n = write(interrupt->write_fd, &stopped, 1); } while (n < 0 && errno == EINTR);
  // EAGAIN means unread stop bytes already keep the interrupt latched.
}
static inline void movie_source_interrupt_destroy(struct movie_source_interrupt *interrupt) {
  if (interrupt->read_fd >= 0) close(interrupt->read_fd);
  if (interrupt->write_fd >= 0) close(interrupt->write_fd);
  interrupt->read_fd = interrupt->write_fd = -1;
}

struct budget_io {
  int fd;
  uint64_t bytes, requested_bytes;
  bool exhausted;
  bool remote;
  char authority[32];
  char path[34];
  int64_t position, length;
  int upstream;
  struct movie_source_interrupt *interrupt;
  // Original-read budget, set to WINDOW_READ_LIMIT by movie_source_open. Only
  // a whole-title caller (segment_decode_render --refit-stats) lifts it.
  uint64_t read_limit;
};

static int movie_source_interrupt_status(const struct budget_io *io) {
  if (!io->interrupt) return 0;
  if (io->interrupt->read_fd < 0) return AVERROR(EINVAL);
  struct pollfd descriptor = {.fd = io->interrupt->read_fd, .events = POLLIN};
  int ready;
  do { ready = poll(&descriptor, 1, 0); } while (ready < 0 && errno == EINTR);
  if (ready < 0) return AVERROR(errno);
  return descriptor.revents ? AVERROR(ECANCELED) : 0;
}
static int movie_socket_wait(struct budget_io *io, short events) {
  struct pollfd descriptors[2] = {
    {.fd = io->upstream, .events = events},
    {.fd = io->interrupt ? io->interrupt->read_fd : -1, .events = POLLIN}
  };
  for (;;) {
    int ready = poll(descriptors, 2, -1);
    if (ready < 0 && errno == EINTR) continue;
    if (ready < 0) return AVERROR(errno);
    // Cancellation wins even when the socket became ready in the same poll.
    if (descriptors[1].revents) return AVERROR(ECANCELED);
    if (descriptors[0].revents & POLLNVAL) return AVERROR(EBADF);
    if (descriptors[0].revents & (events | POLLERR | POLLHUP)) return 0;
  }
}
static int movie_socket_transfer(struct budget_io *io, void *buffer, int size, bool sending) {
  for (;;) {
    int stopped = movie_source_interrupt_status(io);
    if (stopped < 0) return stopped;
#ifdef MSG_NOSIGNAL
    const int flags = sending ? MSG_NOSIGNAL : 0;
#else
    const int flags = 0;
#endif
    ssize_t n = sending ? send(io->upstream, buffer, (size_t)size, flags)
                        : recv(io->upstream, buffer, (size_t)size, 0);
    if (n >= 0) return (int)n;
    int error = errno;
    if (error == EINTR) continue;
    if (io->interrupt && (error == EAGAIN || error == EWOULDBLOCK)) {
      int result = movie_socket_wait(io, sending ? POLLOUT : POLLIN);
      if (result < 0) return result;
      continue;
    }
    return AVERROR(error);
  }
}

// The capability is minted by the held-source owner, never by media metadata.
// No DNS, escaping, alternate IP spelling, credentials or trailing components.
static bool movie_source_url(const char *source, struct budget_io *io) {
  const char prefix[] = "http://127.0.0.1:";
  if (strncmp(source, prefix, sizeof(prefix) - 1)) return false;
  const char *p = source + sizeof(prefix) - 1, *port = p;
  unsigned value = 0;
  while (*p >= '0' && *p <= '9') {
    if (p - port >= 5) return false;
    value = value * 10 + (unsigned)(*p++ - '0');
  }
  if (!value || value > 65535 || *p != '/' || (p - port > 1 && *port == '0'))
    return false;
  const char *path = p++;
  for (int i = 0; i < 32; i++, p++)
    if (!((*p >= '0' && *p <= '9') || (*p >= 'a' && *p <= 'f') ||
          (*p >= 'A' && *p <= 'F'))) return false;
  if (*p) return false;
  snprintf(io->authority, sizeof(io->authority), "127.0.0.1:%u", value);
  memcpy(io->path, path, 34);
  return true;
}
static void movie_source_close(struct budget_io *io) {
  if (io->upstream >= 0) close(io->upstream);
  io->upstream = -1;
  if (io->fd >= 0) close(io->fd);
  io->fd = -1;
}
static bool movie_decimal(const char **text, int64_t *value) {
  const char *p = *text;
  if (*p < '0' || *p > '9') return false;
  int64_t n = 0;
  do {
    int digit = *p++ - '0';
    if (n > (INT64_MAX - digit) / 10) return false;
    n = n * 10 + digit;
  } while (*p >= '0' && *p <= '9');
  *text = p;
  *value = n;
  return true;
}
// This connector implements only HEAD and bounded Range GET over a numeric
// loopback TCP socket. It never invokes FFmpeg's redirect-following HTTP stack
// or inherits proxy environment settings. Response framing stays within the
// explicitly requested Range, itself clipped to the original-byte budget.
static int movie_http_head(struct budget_io *io, bool head, int size) {
  char request[256];
  int result = movie_source_interrupt_status(io);
  if (result < 0) return result;
  result = AVERROR(EIO);
  unsigned port;
  if (sscanf(io->authority, "127.0.0.1:%u", &port) != 1)
    return AVERROR(EINVAL);
  io->upstream = socket(AF_INET, SOCK_STREAM, 0);
  if (io->upstream < 0) return AVERROR(errno);
  if (fcntl(io->upstream, F_SETFD, FD_CLOEXEC) < 0) goto failed;
  if (io->interrupt) {
    int flags = fcntl(io->upstream, F_GETFL);
    if (flags < 0 || fcntl(io->upstream, F_SETFL, flags | O_NONBLOCK) < 0) {
      result = AVERROR(errno); goto failed;
    }
  }
  // The existing child owner bounds work and owns cancellation. A socket wall
  // timer would continue running during a verified source-service pause.
#ifdef SO_NOSIGPIPE
  int no_sigpipe = 1;
  if (setsockopt(io->upstream, SOL_SOCKET, SO_NOSIGPIPE, &no_sigpipe, sizeof(no_sigpipe)))
    goto failed;
#endif
  struct sockaddr_in endpoint = {0};
  endpoint.sin_family = AF_INET;
  endpoint.sin_port = htons((uint16_t)port);
  endpoint.sin_addr.s_addr = htonl(UINT32_C(0x7f000001));
  for (;;) {
    if (!connect(io->upstream, (const struct sockaddr *)&endpoint, sizeof(endpoint))) break;
    int error = errno;
    if (error == EINTR && !io->interrupt) continue;
    if (!io->interrupt ||
        (error != EINPROGRESS && error != EALREADY && error != EINTR)) {
      result = AVERROR(error); goto failed;
    }
    result = movie_socket_wait(io, POLLOUT);
    if (result < 0) goto failed;
    socklen_t length = sizeof(error);
    int status;
    do { status = getsockopt(io->upstream, SOL_SOCKET, SO_ERROR, &error, &length); }
    while (status < 0 && errno == EINTR);
    if (status < 0) { result = AVERROR(errno); goto failed; }
    if (error) { result = AVERROR(error); goto failed; }
    break;
  }
  int count;
  if (head) {
    count = snprintf(request, sizeof(request),
        "HEAD %s HTTP/1.1\r\nHost: %s\r\nConnection: close\r\n\r\n",
        io->path, io->authority);
  } else {
    count = snprintf(request, sizeof(request),
        "GET %s HTTP/1.1\r\nHost: %s\r\nRange: bytes=%" PRId64 "-%" PRId64
        "\r\nConnection: close\r\n\r\n", io->path, io->authority,
        io->position, io->position + size - 1);
  }
  int sent = 0;
  while (sent < count) {
    result = movie_socket_transfer(io, request + sent, count - sent, true);
    if (result <= 0) {
      if (!result) result = AVERROR(EIO);
      goto failed;
    }
    sent += result;
  }
  char headers[8192];
  size_t used = 0;
  while (used < sizeof(headers) - 1) {
    result = movie_socket_transfer(io, &headers[used], 1, false);
    if (result != 1) {
      if (result >= 0) result = AVERROR_INVALIDDATA;
      goto failed;
    }
    used++;
    if (used >= 4 && !memcmp(headers + used - 4, "\r\n\r\n", 4)) break;
  }
  if (used < 4 || memcmp(headers + used - 4, "\r\n\r\n", 4)) {
    result = AVERROR_INVALIDDATA; goto failed;
  }
  headers[used] = 0;
  char *line_end = strstr(headers, "\r\n");
  if (!line_end) { result = AVERROR_INVALIDDATA; goto failed; }
  *line_end = 0;
  // Redirect responses are errors. No HTTP protocol handler ever sees them.
  const char *status = head ? "200" : "206";
  if ((strncmp(headers, "HTTP/1.1 ", 9) && strncmp(headers, "HTTP/1.0 ", 9)) ||
      strncmp(headers + 9, status, 3) || headers[12] != ' ') {
    result = AVERROR_INVALIDDATA; goto failed;
  }
  bool have_length = false, have_range = false;
  int64_t content_length = -1, start = -1, end = -1, total = -1;
  for (char *line = line_end + 2; *line != '\r'; line = line_end + 2) {
    line_end = strstr(line, "\r\n");
    if (!line_end) { result = AVERROR_INVALIDDATA; goto failed; }
    *line_end = 0;
    char *colon = strchr(line, ':');
    if (!colon) { result = AVERROR_INVALIDDATA; goto failed; }
    *colon++ = 0;
    while (*colon == ' ' || *colon == '\t') colon++;
    const char *value = colon;
    if (!strcasecmp(line, "Content-Length")) {
      if (have_length || !movie_decimal(&value, &content_length) || *value) {
        result = AVERROR_INVALIDDATA; goto failed;
      }
      have_length = true;
    } else if (!strcasecmp(line, "Content-Range")) {
      if (have_range || strncmp(value, "bytes ", 6)) {
        result = AVERROR_INVALIDDATA; goto failed;
      }
      value += 6;
      if (!movie_decimal(&value, &start) || *value++ != '-' ||
          !movie_decimal(&value, &end) || *value++ != '/' ||
          !movie_decimal(&value, &total) || *value) {
        result = AVERROR_INVALIDDATA; goto failed;
      }
      have_range = true;
    } else if (!strcasecmp(line, "Transfer-Encoding") ||
               !strcasecmp(line, "Location") ||
               (!strcasecmp(line, "Content-Encoding") && strcmp(value, "identity"))) {
      result = AVERROR_INVALIDDATA; goto failed;
    }
  }
  if (!have_length || (head ? have_range :
      (!have_range || content_length != size || start != io->position ||
       end != io->position + size - 1 || total != io->length))) {
    result = AVERROR_INVALIDDATA; goto failed;
  }
  if (head) io->length = content_length;
  return 0;
failed:
  if (io->upstream >= 0) close(io->upstream);
  io->upstream = -1;
  return result;
}
static int movie_source_open(struct budget_io *io, const char *source) {
  memset(io, 0, sizeof(*io));
  io->fd = -1;
  io->upstream = -1;
  io->read_limit = WINDOW_READ_LIMIT;
  if (movie_source_url(source, io)) {
    io->remote = true;
    int result = movie_http_head(io, true, 0);
    if (io->upstream >= 0) close(io->upstream);
    io->upstream = -1;
    return result;
  }
  // An invalid URL is never reinterpreted as a local path/protocol.
  if (strstr(source, "://")) return AVERROR(EINVAL);
  io->fd = open(source, O_RDONLY | O_CLOEXEC);
  struct stat st;
  if (io->fd < 0) return AVERROR(errno);
  if (fstat(io->fd, &st) || !S_ISREG(st.st_mode)) {
    movie_source_close(io);
    return AVERROR(EINVAL);
  }
  io->length = st.st_size;
  return 0;
}
static int movie_read(void *opaque, uint8_t *buffer, int size) {
  struct budget_io *io = opaque;
  if (size <= 0) return AVERROR(EINVAL);
  uint64_t charged = io->remote ? io->requested_bytes : io->bytes;
  if (charged >= io->read_limit) {
    io->exhausted = true;
    return AVERROR(ENOSPC);
  }
  uint64_t available = io->read_limit - charged;
  if ((uint64_t)size > available) size = (int)available;
  if (!io->remote) {
    ssize_t n;
    do { n = read(io->fd, buffer, (size_t)size); } while (n < 0 && errno == EINTR);
    if (n < 0) return AVERROR(errno);
    if (!n) return AVERROR_EOF;
    io->bytes += (uint64_t)n;
    return (int)n;
  }
  if (io->position >= io->length) return AVERROR_EOF;
  if (size > io->length - io->position) size = (int)(io->length - io->position);
  // A disconnected response may have already pulled its whole finite Range
  // from the held source into the socket. Charge the request before opening it;
  // failed/partial transfers never manufacture new original-read credit.
  io->requested_bytes += (uint64_t)size;
  int result = movie_http_head(io, false, size);
  if (result < 0) return result;
  int used = 0;
  while (used < size) {
    result = movie_socket_transfer(io, buffer + used, size - used, false);
    if (result <= 0) break;
    used += result;
    io->bytes += (uint64_t)result;
    io->position += result;
  }
  if (io->upstream >= 0) close(io->upstream);
  io->upstream = -1;
  if (result == AVERROR(ECANCELED)) return result;
  return used ? used : (result < 0 ? result : AVERROR_EOF);
}
static int64_t movie_seek(void *opaque, int64_t offset, int whence) {
  struct budget_io *io = opaque;
  if (whence & AVSEEK_SIZE) {
    if (io->remote) return io->length;
    struct stat st;
    return fstat(io->fd, &st) == 0 ? st.st_size : AVERROR(errno);
  }
  if (!io->remote) {
    off_t result = lseek(io->fd, (off_t)offset, whence & ~AVSEEK_FORCE);
    return result < 0 ? AVERROR(errno) : (int64_t)result;
  }
  int64_t base;
  switch (whence & ~AVSEEK_FORCE) {
  case SEEK_SET: base = 0; break;
  case SEEK_CUR: base = io->position; break;
  case SEEK_END: base = io->length; break;
  default: return AVERROR(EINVAL);
  }
  if ((offset > 0 && base > INT64_MAX - offset) ||
      (offset < 0 && offset < -base)) return AVERROR(EINVAL);
  io->position = base + offset;
  return io->position;
}
static int refuse_nested_movie_io(AVFormatContext *context, AVIOContext **pb,
                                 const char *url, int flags, AVDictionary **opts) {
  (void)context; (void)pb; (void)url; (void)flags; (void)opts;
  return AVERROR(EPERM);
}
static int movie_source_bind_demuxer(AVFormatContext *input) {
  input->io_open = refuse_nested_movie_io;
  input->protocol_whitelist = av_strdup("");
  return input->protocol_whitelist ? 0 : AVERROR(ENOMEM);
}

#endif
