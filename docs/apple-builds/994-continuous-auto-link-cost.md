# Recover from link pressure even when no lower rung fits, and climb back below the display

Build: 230
Issue: #994

Under link pressure Auto used to admit only a lower rung whose cost fitted
the measured link, and held the current rung when none did, although the
link had just proved it could not carry it. It now takes the cheapest costed
lower rung in that case, provided it costs less than the current one.

When no affordable rung covers the display, an Auto upgrade now takes the
largest affordable rung, as the web client already does, instead of choosing
nothing and staying on a lower rung after any downgrade.
