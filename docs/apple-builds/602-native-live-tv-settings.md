# Live TV server settings leave Developer

**Status:** open — PR #602, rebased above main's Apple 199; simulator tests
pass on the branch. Physical iOS and Apple TV checks of the new placement are
pending (2026-09-28).

Build: 200
Issue: #602

Settings now has a Live TV row on iPhone, iPad and Apple TV that opens the
server's tuner, programme guide, recording and Library channel settings, in the
web's order. Those four cards no longer appear under Developer. Saves, readiness
checks and messages are unchanged; only where the cards are drawn moved.

Developer keeps the cards still waiting on evidence — bounded pause/resume,
prepared quality handoff and the Live TV enable switch — and each now says what
it is waiting on and where it goes when that evidence lands. The Enable Live TV
card reads the server's current prerequisites when it opens and shows every row
as Met or Not met beside the button, as the web card does; none of them
disables it. Recording-off and incomplete-settings messages now point
administrators at Settings → Live TV.
