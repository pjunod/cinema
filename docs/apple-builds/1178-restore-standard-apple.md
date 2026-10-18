# Restore Standard's native Apple navigation and Theater Home

Build: 262
Issue: #1178

**Status:** built and simulator-verified; physical acceptance pending.

Standard on iOS/tvOS 18 and later again uses the system tab bar: adaptive
floating tabs/sidebar on iPad, bottom tabs on iPhone, and top tabs on Apple
TV. Meridian's custom navigation no longer replaces Standard's shell.
Apple TV pushes replace the tab shell; Back restores it. Both layouts keep
the same destination controllers and nested navigation state.

Apple TV Standard Theater also regains its original 540-point hero,
52-point title, white copy, black artwork gradients and button styling.
Meridian keeps its separate presentation. Existing theme palettes, saved
theme choices and the independent Classic/Theater preference are unchanged.

Regression coverage checks native tabs, content geometry, feature visibility,
layout/theme independence, implicit navigation and playback-owner lifetime,
plus native Apple TV Back and rendered Classic/Theater fixtures. The older
17.x fallback is unchanged and has not been runtime-qualified.
Physical-device installation and acceptance remain pending.

Validation: 10 iPad tests, 15 tvOS tests and two Apple TV remote-navigation
UI tests passed. Release simulator builds passed for iOS and tvOS from an
isolated source snapshot containing this repair.
