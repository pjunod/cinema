"""plurx Player: the Linux launcher that plays a plurx title in mpv.

The web app hands a title over with a one-time code in a
``plurx-player://play?server=<origin>&code=<code>`` URL; this package
redeems it, asks the server how to deliver the file, plays it in mpv and
keeps the server informed until mpv closes. See
docs/clients/LINUX-PLAYER-PLAN.md section 4.2 and this folder's README.
"""

VERSION = "0.4.0"
DEVICE_NAME = "plurx Player (Linux)"
