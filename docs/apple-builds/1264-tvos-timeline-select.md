# Commit the Apple TV timeline preview with Select

Build: 272
Issue: #1264

Apple TV playback controls now commit the position previewed with Left or
Right when the viewer presses Select. The timeline uses native button
activation with a label-only style, retaining its thin focus ring. Menu still
cancels a preview, and committing preserves the existing play/pause state.

The native remote regression drives the shipping player against loopback
media, commits forward and backward previews, checks the committed positions,
and cancels another preview. Physical Apple TV acceptance remains pending.
