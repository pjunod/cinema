# Server label names the machine

**Status:** built.

Build: 269
Issue: #1204

Settings → Account → Server reads the server's `display_name` from the
signed-in `GET /api/v1/server/node` after sign-in, instead of the shared
`name` from the public `/api/v1/server`. A server left on the default name
`plurx` now shows its hostname (`media1`); a server with a chosen name keeps
it, and Settings adds the answering node's hostname after it
(`Living Room · lab4`). Older servers without the route still show `name`.
