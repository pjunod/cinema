# Release the captured playback session through its owner

Build: 282
Issue: #1331

Stopping playback releases the attached HLS session exactly once through the
configured release owner, even after the controller clears its current session.
The regression checks the captured model and session identity and prevents a
second release during teardown.
