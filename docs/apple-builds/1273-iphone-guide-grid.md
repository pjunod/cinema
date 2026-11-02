# Restore the iPhone Live TV guide time grid

Build: 273
Issue: #1273

Guide on iPhone opens the existing multi-channel time grid with station logos,
pinned channel labels and duration-sized programme blocks. The single-channel
dropdown schedule and its layout switch are removed; an old saved `schedule`
preference cannot restore that view. The current Live TV page, player and
programme/DVR actions remain in place.

The guide's time heading is constrained to the viewport, preventing its wide
intrinsic size from pushing station rows off the side of a portrait phone.

The portrait native regression launches with the legacy preference and checks
two channel rows together, the absence of schedule controls, and the programme
Watch action. Simulator validation does not establish physical-device acceptance.
