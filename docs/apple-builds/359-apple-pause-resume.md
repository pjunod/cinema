# Return from Pause to a moving picture within one bound

Build: 168
Issue: #359

On-demand playback now uses retained, ready media immediately when it has more
than ten wall-time seconds of contiguous runway. If presentation does not
resume, one same-delivery repair inherits the original 15-second deadline;
stale paused frames and a transport-only “playing” state do not count as a
moving picture. Pending seeks and stream replacements keep the predecessor
paused, and lock-screen Play/Pause uses the same ordered intent owner as the
on-screen controls.

Settings → Developer exposes this behavior as an on-by-default enable switch.
The requirements and physical-device qualification state shown beside it are
advisory and never disable the switch.
