# Require immutable VOD for segmented playback

Build: 84
Issue: #572

Apple session requests now require the immutable VOD presentation and reject a
mixed-version server response that reports `vod:false`. Unsupported segmented
media receives the server's typed refusal instead of entering the retired live
HLS engine.
