# Give Live TV the proportions the web page has

Build: 146
Issue: #267

Live TV on Apple TV was sized by the platform's semantic text styles, which
tvOS resolves two to two and a half times larger than iOS does. Channel rows
ran the full width of the screen, the details title was 57 pt, and the live
preview ended up a fifth of the width with three bands of chrome stacked above
it. The screen now sizes itself through one explicit scale and spends one 48 pt
toolbar row, and it arranges content by the rule the web page follows: lists
are tall and narrow, grids are wide.

**On now** is a 620 pt channel column beside a large picture. The picture is a
focus target whose Select is Fullscreen, so "Return to live" no longer needs a
toolbar button. Under it sit the programme, its synopsis, what is up next, and
the playback message.

**Guide** is a short stage — picture on the left, what is focused on the right
— over a full-width grid. The grid's slot width is derived from the width it is
actually given, so two hours fit any screen; a hard-coded 300 pt filled 58% of a
1920 pt screen and could never fit two. The fixed grid height that pushed the
last rows off the bottom is gone, and Earlier / Now / Later are chips in the
grid's own header.

The Layout menu offers **Preview** and **Over picture**. A saved
`channel_browser` preference still decodes, keeps its stored value, and renders
as Preview: the two only ever differed in which browse view they opened with.

On iPhone and iPad the picture is full-bleed 16:9 with its channel chip and its
picture-in-picture and fullscreen actions on the picture itself, followed by one
caption line and one toolbar. The six-line now bar, the always-visible search
field and the filter toggles are gone, so four to five channel rows are visible
while a channel is playing instead of one.

Nothing about what plays, how the remote is routed, or how Live TV is enabled
changes. The Developer tab's enable switch and its advisory readiness reasons
remain the only runtime control.
