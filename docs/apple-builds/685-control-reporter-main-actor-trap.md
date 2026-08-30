# Stop the control reporter crashing the player it reports on

Build: 91
Issue: #685

The passive control reporter took its snapshot by asking to be on the main
actor from inside its own actor, and `MainActor.assumeIsolated` traps rather
than falling back — so build 90 died the instant a controllable session
opened, on every title, on iOS and tvOS alike. The player now publishes what it
sees and the reporter reads that, so nothing reaches back into the player from
an executor that is not its own.
