#ifndef PLURX_MOVIE_SOURCE_IO_H
#define PLURX_MOVIE_SOURCE_IO_H
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <limits.h>
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
struct budget_io {
  int fd;
  uint64_t bytes, requested_bytes;
  bool exhausted;
  bool remote;
  char authority[32];
  char path[34];
  int64_t position, length;
  int upstream;
};

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
  int result = AVERROR(EIO);
  unsigned port;
  if (sscanf(io->authority, "127.0.0.1:%u", &port) != 1)
    return AVERROR(EINVAL);
  io->upstream = socket(AF_INET, SOCK_STREAM, 0);
  if (io->upstream < 0) return AVERROR(errno);
  if (fcntl(io->upstream, F_SETFD, FD_CLOEXEC) < 0) goto failed;
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
  if (connect(io->upstream, (const struct sockaddr *)&endpoint, sizeof(endpoint)))
    goto failed;
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
#ifdef MSG_NOSIGNAL
    const int flags = MSG_NOSIGNAL;
#else
    const int flags = 0;
#endif
    ssize_t n = send(io->upstream, request + sent, (size_t)(count - sent), flags);
    if (n < 0 && errno == EINTR) continue;
    if (n <= 0) goto failed;
    sent += (int)n;
  }
  char headers[8192];
  size_t used = 0;
  while (used < sizeof(headers) - 1) {
    do { result = (int)recv(io->upstream, &headers[used], 1, 0); }
    while (result < 0 && errno == EINTR);
    if (result != 1) { result = AVERROR_INVALIDDATA; goto failed; }
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
  if (charged >= WINDOW_READ_LIMIT) {
    io->exhausted = true;
    return AVERROR(ENOSPC);
  }
  uint64_t available = WINDOW_READ_LIMIT - charged;
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
    do { result = (int)recv(io->upstream, buffer + used, (size_t)(size - used), 0); }
    while (result < 0 && errno == EINTR);
    if (result < 0) result = AVERROR(errno);
    if (result <= 0) break;
    used += result;
    io->bytes += (uint64_t)result;
    io->position += result;
  }
  if (io->upstream >= 0) close(io->upstream);
  io->upstream = -1;
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
