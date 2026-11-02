# Apple TV focus — isolate frequent input updates from the app shell

Build: 276
Issue: #1130

**Status:** simulator regressions passed on 2026-10-25; physical Apple TV
acceptance pending.

Follow-up to the [Apple TV navigation audit](../clients/APPLE-TV-NAVIGATION-AUDIT.md).
The viewer reports that Standard layout became unresponsive throughout the
app, including moving focus, during the last few updates.

Physical input and focus authority counters previously published through the
navigation object observed by the app shell and registered controls. A focused
regression counted 696 shell notifications for 100 input/focus transitions.
These counters now publish through an owned activity object observed only at
focus-bookmark and restoration modifier boundaries. Programmatic focus
requests, route changes and the navigation opener's focus preference retain
their shell notifications. Stale companion Select commands still compare the
synchronously updated authority revision.

The regression checks zero shell notifications for that sequence, settled
focus/input observations without rebuilding their parent, and opener-focus
preference retirement. Native Standard tab, library menu and bookmark return
journeys remain required. This is a reproduced source-level amplification
mechanism; it does not establish measured latency or resolution on the
viewer's Apple TV. No device deployment is claimed.

Validation on the current main base: 53 focused tvOS tests passed, including
zero shell notifications for the 100-transition regression and the hosted
observer test. Four native remote journeys passed: Standard tabs after Back,
library View-modal ownership and opener return, grid tools/Jump/bookmark
return, and Meridian panel/source selection. The complete whole-app native
journey matrix and physical-device latency acceptance remain separate.
