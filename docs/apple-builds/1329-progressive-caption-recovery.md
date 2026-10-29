# Recover stopped caption generation explicitly

Build: 281
Issue: #1329

A fresh Generate action can now discover an existing stopped operation even
when it never published a caption asset. The client validates its current
source, audio, model profile and language before exposing explicit Resume.
Discovery creates no demand and does not restart stopped work. Resume retains
the current operation epoch and view checks.

Current uncovered caption ranges show nonretryable failure reasons without
inventing a Retry action. Available coverage and relevant active work retain
priority over historical failures. Focused validation is pending; physical
caption rendering and end-to-end inference acceptance remain open.
