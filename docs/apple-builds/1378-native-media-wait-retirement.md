# Retire native media waits with their destination intent

Build: 289
Issue: #1378

**Status:** built; focused native tests passed; full CI and device acceptance pending.

Apple and Android media-wait faults retire when their destination intent settles
or is superseded. The source-row override matches the existing shared fixture
and web implementation; the native class defaults had omitted these two events.

Full recovery qualification found both native table comparisons failing on the
unchanged baseline. The correction preserves the fixture and its ordered-event
expectations. The local iOS simulator passed table parity and every ordered
contract case; the Android surface-reducer suite passed all ten tests. PR #1378
records the required full native CI results; physical device acceptance is not
claimed.
