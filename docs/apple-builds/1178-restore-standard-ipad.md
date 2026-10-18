# Restore Standard's native iPad navigation

Build: 262
Issue: #1178

**Status:** built; focused iPad and tvOS simulator tests passed, physical acceptance pending.

Standard on iOS 18 and later again uses the system's adaptive tab bar and
sidebar: floating top tabs on iPad, native bottom tabs on iPhone, and the
native sidebar toggle. Meridian's fixed rail no longer replaces Standard's
navigation or reserves part of its content width. Existing theme palettes
and saved theme selections are unchanged.

Both layouts retain the same native destination controllers. Switching
layouts preserves nested navigation and restores Standard's sidebar choice.
The regression checks native tab ownership, full-width geometry, feature
visibility, theme independence and retained navigation/lifecycle state.

Validation: the focused `MeridianTests` suite passes on the iPad Pro 11-inch
simulator (10 tests) and tvOS simulator (6 tests). The iPad run includes a
render of Standard with the original Noirr palette and synthetic media.
Physical-device installation and acceptance remain pending.
