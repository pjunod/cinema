# Retry a native subtitle when its demanded window becomes ready

Build: 110
Issue: #700

The Apple control client now consumes native-subtitle readiness and reselects
the current subtitle once when the demanded cache window changes from not ready
to ready. Repeated `ready` exchanges do nothing, and the retry never replaces
the video item.
