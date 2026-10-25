# Meridian source changes open the chosen collection

Build: 267
Issue: #1195

**Status:** built; physical Apple TV acceptance pending.

After the Standard restoration moved both layouts onto one framework-owned
SwiftUI TabView, choosing a different source from the Meridian navigation
panel replaced the Libraries stack's only entry with another collection.
The stack updated the existing collection screen in place: it kept the old
collection's state, its controls never registered under the new route, and
focus fell back to the panel opener.

Each navigation destination now takes its identity from its route scope, so
a different route at the same depth is a new screen, while refreshed data for
the same route keeps the screen. The native journey
`TVMeridianNavigationTests/testPanelOwnsNativeFocusAndBackPreservesLibraryRoute`
fails without this change and passes with it.

The Apple native TV job's time limit rises from 60 to 120 minutes: its 32
journeys take about an hour (up to 87 minutes) after the compile, and the
60-minute limit cut the run off before its last journeys.
