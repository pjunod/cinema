# Keep playback control through a temporary serving fence

Build: 220
Issue: #962

Apple's local playback control reporter now retries `503 serving_fenced`
through its existing retry owner, respecting Retry-After and retaining the
exact captured exchange. The web reporter already recognizes this temporary
refusal. Definitive session-ended and unknown refusal responses still stop
reporting; this change does not bypass the server fence or add a watchdog.

The combined native correction and its remaining physical acceptance are
recorded in the [playback repair ledger](../clients/ANDROID-PLAYBACK-REPAIR-STATUS.md).
