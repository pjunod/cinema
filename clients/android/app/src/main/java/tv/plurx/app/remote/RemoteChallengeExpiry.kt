package tv.plurx.app.remote

/** A received duration never grants a new deadline after network delay. */
internal class RemoteChallengeExpiry(startedAt: Long, ttlMs: Long) {
    private val start = startedAt.also { require(it >= 0) }
    private val deadline = startedAt + ttlMs.also { require(it in 1..120_000 && startedAt <= Long.MAX_VALUE - it) }
    fun remaining(now: Long): Long = if (now < start) 0 else (deadline - now).coerceAtLeast(0)
}
