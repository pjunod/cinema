# Live TV station startup

**Status:** built

Build: 189
Issue: #568

Live TV on iPhone, iPad, and Apple TV accepts the server's station playlist
again. Previously, the client escaped dots and hyphens in the session ID,
rejected the valid playlist URL, released the session, and displayed
“The live session expired.”

Start and resume now preserve the URL's unreserved characters. The client
still requires the exact session's master playlist on the selected server.

The regression uses a real-format capability in both start and resume
responses. Initial local evidence: 82 iOS and 81 tvOS Live TV tests passed,
and both Release simulator builds compiled on September 26, 2026. This
predates the fresh main base; PR #568 records the final candidate's review
and fast-lane result. These checks do not establish device playback or
TestFlight delivery.
