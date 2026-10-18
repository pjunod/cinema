# Restore Standard's native Apple navigation and Theater Home

Build: 262
Issue: #1178
Restoration PR: #1180

**Status:** built and simulator-verified; physical acceptance pending.

Standard on iOS/tvOS 18 and later again uses the system tab bar: adaptive
floating tabs/sidebar on iPad, bottom tabs on iPhone, and top tabs on Apple
TV. Meridian's custom navigation no longer replaces Standard's shell.
Apple TV pushes replace the tab shell; Back restores it. Both layouts share
one framework-owned SwiftUI TabView and retain nested navigation state.
UIKit configures its native chrome without taking ownership of destination
controllers or their fullscreen presentations. This prevents fullscreen
playback from remounting after another tab presents a pushed choice panel.

Apple TV Standard Theater also regains its original 540-point hero,
52-point title, white copy, black artwork gradients and button styling.
Meridian keeps its separate presentation. Existing theme palettes, saved
theme choices and the independent Classic/Theater preference are unchanged.

Regression coverage checks native tabs, content geometry, feature visibility,
layout/theme independence, implicit navigation and playback-owner lifetime,
plus native Apple TV Back and rendered Classic/Theater fixtures. The older
17.x fallback is unchanged and has not been runtime-qualified.
Physical-device installation and acceptance remain pending.

Validation: the original full Live TV fullscreen remote journey passed on
the framework-owned tab implementation, including idle hiding, overlay
reveal, recording choices and explicit exit, with one fullscreen appearance.
The focused iPad and tvOS layout suites passed; their lifecycle fixture now
compares against a bare SwiftUI TabView control and passes on both
platforms. The native remote journeys run in the current-head Main
promotion gate, which is required before merge.
