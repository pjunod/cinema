# Select Meridian layout and theme on Apple

Build: 260
Issue: #1170

Appearance now offers independent Layout (Standard or Meridian) and Theme
choices. Meridian adds phone bottom navigation, tablet and TV library rails,
and a static featured title using Cinema's existing playback and data owners.
The shared native destination host retains implicit Settings/DVR pushes,
query/filter state and playback presenters across layout and width changes.

The reference above identifies the Meridian design work. Apple implementation
is reviewed in its own task pull request; this fragment does not claim that
implementation landed in the design pull request. Simulator fixture screenshots
and focused preference, native stack, and TV focus/Back tests accompany that
review. Apple 17 runtime and physical-device qualification remain separate.
