# Select Meridian layout and theme on Apple

Build: 261
Issue: #1174

Appearance now offers independent Layout (Standard or Meridian) and Theme
choices. Meridian adds phone bottom navigation, tablet and TV library rails,
and a static featured title using Cinema's existing playback and data owners.
The shared native destination host retains implicit Settings/DVR pushes,
query/filter state and playback presenters across layout and width changes.

The reference above identifies the Apple implementation review. Standard
keeps a native sidebar with every enabled destination on wide iPad windows;
compact Meridian More includes recording-attention counts and VoiceOver text.
Simulator fixture screenshots and focused preference, native stack, attention,
and TV focus/Back tests accompany the review. Apple 17 runtime and physical-device qualification remain separate.
