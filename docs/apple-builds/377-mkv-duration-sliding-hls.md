# Stop treating an intentional HLS pause as a retryable failure

Build: 172
Issue: #377

Apple now recognizes the server's typed terminal pause-expiry response and
stops retrying that rolling presentation. This keeps a deliberately paused
viewer from reopening a session whose bounded server-side pause window has
already expired.
