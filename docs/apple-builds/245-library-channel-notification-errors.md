# Retain the error from a Library channel playback failure

Build: 139
Issue: #245

Library channels now read the error carried by Apple's failed-to-play-to-end
notification. That error can exist while the player item's own error is nil;
ignoring it previously replaced a useful failure code with a generic message.
The latest AVPlayer error-log domain and status provide a fallback. A later
callback without error details cannot erase an already captured error.

This improves diagnosis of the reported freeze after initial playback. It
does not claim to resolve that freeze or change the direct/copy decision.
