# Name Apple TV settings beside their selected values

Build: 285
Issue: #1130

**Status:** built; physical Apple TV acceptance pending.

Apple TV settings show each setting name and selected value in one focusable
menu row. Playback, Appearance, Home and Live TV choices retain their names;
the tuner configuration and diagnostic report category use the same control.
iPhone and iPad keep their native Form pickers.

The focused Settings journey checks visible names, changing and reopening
Quality, and exact Back focus restoration. Its new name assertion failed on
the preceding implementation, which displayed only Auto and language values.
Physical Siri Remote acceptance remains pending; this note claims no device
deployment.
