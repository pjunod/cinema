# Seek 10 or 30 seconds in either direction

Build: 203
Issue: #689

The player now offers back 30 seconds, back 10 seconds, play/pause, forward
10 seconds, and forward 30 seconds on iPhone, iPad, and Apple TV. Each button
uses the existing relative seek path, including pending-destination handling
and title bounds. TV focus can return to either new control.
