# Copy memory budget (`PLURX_COPY_MEMORY_BUDGET`)

**Status:** built · owned by Claude · **Updated:** 2026-11-26 · **Source baseline:** `origin/main` at `635d3c40b`

Effort E-62BDA51817.

How plurxd bounds the heap its verified copies hold. Routed from the
environment table in [OPERATIONS](../OPERATIONS.md).

`PLURX_COPY_MEMORY_BUDGET` is the number of bytes this node's verified copies
(shared growing copies and Analysis gap fills) may hold in plurxd's heap at
once. The default is ⅛ of node memory. Node memory is the container's cgroup
limit where one is set, `hw.memsize` on macOS, and 1 GiB otherwise.

## Admission

Each copy is admitted for one segment of the file's bitrate, counted twice
(its pending AV and that AV's merge clone), before its FFmpeg child starts.

- A copy that does not fit waits in arrival order, viewers first, and is
  answered busy at its start deadline.
- A lone copy larger than the whole budget still runs on an idle node.
- Only a copy larger than the node's own memory is refused
  (`copy_range_input_limit`).

## After admission

A copy's charge follows what it holds:

- A viewer's copy always grows, past the budget if it must; it never pauses a
  stream.
- A gap fill pauses until there is room.
- Every copy shrinks back after each segment.

## Scope

The budget counts plurxd's own heap only, **not** FFmpeg children. It is per
node and never replicated. Used / total is shown on Activity → node.
