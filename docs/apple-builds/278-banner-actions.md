# The banner said what went wrong and offered no way to answer it

Build: 152
Issue: #278

A reachability audit of `main` found one real defect left on Apple, and it was
a dead end for the viewer.

**The notice strip rendered no actions at all.** §3.1's `banner` is "a notice
strip with **the fault's actions**; the picture is untouched", and
`failureView` was the only view in the client that drew `surface.actions` —
gated on a blocking surface. So `refused`, the one class whose entire point is
that the predecessor keeps playing while the change the viewer asked for did
not, drew a sentence and nothing else: physical recipe (b) requires a Retry
that re-issues the change, and there was none. §3.2's demotion lost its actions
the same way, which defeats the reason the demotion keeps them — so the viewer
gets the specific reopen when the buffer drains.

The banner has its own action row now, drawn with the same button and the same
labels as the full screen, so "Try Again" cannot come to mean two things. It
does **not** add a guaranteed Close the way the full screen does: behind a
banner is the viewer's film, and a Close on a notice is a button that ends
playback which is fine. A Close the fault itself carries — a demoted terminal
has one — is still drawn, because actions are a property of the fault and the
presenter does not second-guess the owner about them.

Two smaller faults in the same code went with it.

**A demoted banner announced what had failed.** The strip read
`detail ?? title`, and §3.2's demotion rewrites the title to "Playback
recovered" while deliberately keeping the old failure sentence in `detail` so
the fault's Try again still knows what it is about. The strip now reads the
title for a demoted fault, and the failure sentence stays on the fault for the
ledger.

**One fault drew two overlays.** The strip asked whether the viewer was
blocked rather than what kind of surface this is, so every `indicator` drew the
in-chrome capsule and the strip at once. It asks for the `banner` kind now,
which is the only kind it was ever about.

No threshold, budget, detector or ladder moved, and nothing about which fault
is raised changed.
