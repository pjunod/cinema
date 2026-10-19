# Server label names the machine

**Status:** built.

Build: 269
Issue: #1204

Settings → Account → Server, and the sign-in screens, read the server's
`display_name` from `/api/v1/server` instead of its shared `name`. A server
left on the default name `plurx` now shows its hostname (`media1`); a server
with a chosen name keeps it, and Settings adds the answering node's hostname
after it (`Living Room · lab4`). Older servers without `display_name` still
show `name`.
