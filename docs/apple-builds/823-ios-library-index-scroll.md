# Scroll to the native library row selected in the index

**Status:** open — implementation ready for review and focused UI validation.

Build: 213
Issue: #823

The letter/year index and the vertical rows previously shared raw group IDs
inside one scroll reader. SwiftUI could resolve the index's own `ForEach`
entry first, so tapping a letter did not move the library vertically. Row
scroll destinations now use a distinct typed ID; the index retains its stable
group identity and every tap targets the matching row, including a repeat tap
after manual scrolling.

The focused iOS UI fixture exercises the shipped login and library screen
against a loopback API with 416 movies. It checks distant letter and year
jumps, returning to A, and tapping the same letter after manual scrolling.
No feature switch, delayed retry, or alternate loader is introduced.
