# The television was the one surface that could not record what was on

Build: 156
Issue: #295

The DVR shipped Record, Record series and Remind me on every client, and on the
Apple TV none of them were reachable for a programme that was currently on the
air — which is the case a viewer asks for most, and the case the DVR acceptance
in `LIVE-TV-DVR-IMPLEMENTATION.md` §8 opens with.

The guide cell's action branched on the platform:

```swift
#if os(iOS)
onFuture(row.channel, cell.programme)
#else
if cell.airing { onAiring(row.channel) }
else { onFuture(row.channel, cell.programme) }
#endif
```

The phone had already been corrected, and its comment names the reason exactly
— "including the one on air, which is otherwise the only cell a viewer cannot
record". tvOS kept the other branch, so selecting a live cell tuned the channel
and returned before the programme sheet — the only place the verbs exist — was
ever presented.

The fix is to delete the `#if` and let every cell open the sheet on both
platforms. Nothing else changes: the sheet's first action is Watch, so a live
cell is one more press from the picture, and the On now list keeps its one-press
path. `DvrCellActions` has always answered for an on-air programme.

The same defect was live on the web and on Android and is corrected in the same
change. A fence in `tests/web/live-tv.test.js` now reads all three shipped
sources and fails if any of them branches on `airing` again, because neither
native suite runs in the lane that guards a merge.
