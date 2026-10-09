# Explain the playback buffer limit accurately

Build: 218
Issue: #944

Apple playback now says “The server’s playback buffer limit has been reached.”
when the server reports `working_set` or `no_room`, so the warning identifies
the configured playback budget instead of implying that the filesystem is full.
Recovery actions and timing are unchanged. The server ownership repair and its
remaining deployment/device acceptance are recorded in
[the preparation buffer ledger](../streaming/PREPARATION-BUFFER-PRESSURE-RCA-AND-IMPLEMENTATION.md).
