# Prepare the resume point while Apple TV shows title details

Issue: #1827

Opening a title's details on Apple TV asks the server to prepare the media at
the saved position, so pressing Play resumes there without waiting for the
first segment to be produced. A title that is not prepared still plays through
the existing native route.
