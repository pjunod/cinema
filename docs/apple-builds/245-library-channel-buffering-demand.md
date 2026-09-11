# Keep Library channel production active while buffering

Build: 141
Issue: #245

Library channels now report pause intent from the viewer's Pause action only.
AVPlayer's rate can be zero while it buffers; treating that as a pause sent
Hold demand to the server, suspending the producer needed to refill playback.
That could leave an EVENT playlist unchanged and stop Apple playback with
AVFoundation -11866 and CoreMedia -12888.

Playback also publishes a fresh control snapshot every second as its clock
advances. The reporter reads stored snapshots, so without these updates it
kept reporting the initial join position and the server stopped at its initial
production window. Replaced and stopped items revoke queued progress callbacks;
the time observer is removed on replacement, stop, and controller teardown.

The regression attaches an item with zero rate to the channel controller and
checks its mapped server demand. It failed with Hold before this change.
The change retains the existing direct/copy/transcode decision and the
notification error reporting tested on device in build 139. That reporting
reads the failed-to-play-to-end notification payload when item.error is nil,
falls back to the error log, and retains detail across later empty callbacks.
