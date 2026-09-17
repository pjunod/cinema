package tv.plurx.app.player

/**
 * Measuring a prepared quality switch, with no player in it.
 *
 * Every function here is pure and every value is a plain number the caller has
 * already read off a listener callback. That is deliberate in two directions.
 * It makes the arithmetic — the window edges, the two-sided sum, the counter
 * reset — reachable by the unit lane, which is where this programme's defects
 * have actually lived. And it makes it structurally impossible for the
 * instrument to change what it measures: nothing below touches an `ExoPlayer`,
 * schedules anything, or blocks, so recording a switch cannot alter the timing
 * of one.
 *
 * Nothing here decides anything either. The rows it produces are advisory, in
 * the strict sense Paul's standing rule means: no caller reads them back, and
 * an unmeasurable window is reported as unmeasured rather than as a failure.
 * The bar these readings are compared against lives in
 * `docs/playback-control/QUALITY-SWITCH-CONTINUITY-RESULTS.md`, not in code.
 */

/**
 * Two seconds *either side* of the commit, so a four-second window.
 *
 * A prepared switch spans two pipelines: the predecessor's dropped-frame
 * counter stops at the commit and the successor's starts there. Taking one
 * player's delta across the swap would measure half the window and report it
 * as the whole, which is why [preparedSwitchCounterDelta] takes two series.
 */
internal const val PREPARED_SWITCH_WINDOW_MS = 2_000L

/** How many samples either side are kept. A bound, so an eight-hour film cannot grow it. */
internal const val PREPARED_SWITCH_SAMPLES_MAX = 32

/**
 * One reading of one cumulative counter: the monotonic clock it was taken at
 * and the count at that instant.
 */
internal data class PreparedSwitchSample(val atMs: Long, val count: Long)

/**
 * What a window came to. [count] is null when the window could not be
 * measured at all — never zero, because a zero nobody observed and a zero
 * that was observed are different facts and only one of them clears the bar.
 */
internal data class PreparedSwitchWindow(
    val count: Long?,
    val beforeMs: Long,
    val afterMs: Long,
    val windowMs: Long,
) {
    val coveredMs: Long get() = beforeMs + afterMs
    val spanMs: Long get() = windowMs * 2
}

/**
 * The delta of one cumulative counter over one side of the window.
 *
 * Null with fewer than two readings inside it: a delta needs two, and
 * inventing the missing one is exactly how an instrument comes to certify a
 * window it never saw.
 */
private fun side(
    samples: List<PreparedSwitchSample>,
    fromMs: Long,
    toMs: Long,
): Pair<Long, Long>? {
    val kept = samples.filter { it.atMs in fromMs..toMs }.sortedBy { it.atMs }
    if (kept.size < 2) return null
    val first = kept.first()
    val last = kept.last()
    // A counter that went backwards was reset — a new pipeline, a codec
    // reconfiguration. Everything it has counted since the reset is inside the
    // window, so the reading is the counter itself and never a negative delta.
    val delta = if (last.count >= first.count) last.count - first.count else last.count
    return maxOf(0L, delta) to (last.atMs - first.atMs)
}

/**
 * The two-sided counter delta around a commit.
 *
 * [before] is the predecessor's series and [after] the successor's; each is
 * clipped to its own side of [commitAtMs] so a stale sample cannot be counted
 * twice. Either may be empty, which is a real outcome rather than an error.
 */
internal fun preparedSwitchCounterDelta(
    before: List<PreparedSwitchSample>,
    after: List<PreparedSwitchSample>,
    commitAtMs: Long,
    windowMs: Long = PREPARED_SWITCH_WINDOW_MS,
): PreparedSwitchWindow {
    val bound = if (windowMs > 0) windowMs else PREPARED_SWITCH_WINDOW_MS
    val left = side(before, commitAtMs - bound, commitAtMs)
    val right = side(after, commitAtMs, commitAtMs + bound)
    if (left == null && right == null) {
        return PreparedSwitchWindow(count = null, beforeMs = 0, afterMs = 0, windowMs = bound)
    }
    return PreparedSwitchWindow(
        count = (left?.first ?: 0L) + (right?.first ?: 0L),
        beforeMs = left?.second ?: 0L,
        afterMs = right?.second ?: 0L,
        windowMs = bound,
    )
}

/**
 * How many of [events] fall inside the window around [commitAtMs].
 *
 * An event stream is not a counter and must not be reduced like one. Two
 * readings are needed to take a counter's delta, so a window with one reading
 * is unmeasured — but a window with no *events* in it is a measured zero, as
 * long as the listener was attached across it, which for both pipelines it is
 * by construction. Treating underruns as a counter would report "not measured"
 * for exactly the clean switch the bar is about.
 */
internal fun preparedSwitchEventCount(
    events: List<Long>,
    commitAtMs: Long,
    windowMs: Long = PREPARED_SWITCH_WINDOW_MS,
): Long {
    val bound = if (windowMs > 0) windowMs else PREPARED_SWITCH_WINDOW_MS
    return events.count { it in (commitAtMs - bound)..(commitAtMs + bound) }.toLong()
}

/**
 * Tap to the successor's first frame, in wall time.
 *
 * Null rather than a negative number: two clocks that disagree are not a
 * measurement. Reported, never judged — §1's bar says nothing about it.
 */
internal fun preparedSwitchVisibleInMs(tappedAtMs: Long?, firstFrameAtMs: Long?): Long? {
    if (tappedAtMs == null || firstFrameAtMs == null) return null
    if (firstFrameAtMs < tappedAtMs) return null
    return firstFrameAtMs - tappedAtMs
}

private fun seconds(ms: Long): String = String.format(java.util.Locale.US, "%.1f s", ms / 1_000.0)

/** One wording, shared with the web and Apple ledgers. */
internal fun preparedSwitchFramesRow(window: PreparedSwitchWindow?): String {
    val count = window?.count ?: return "Not measured"
    return "$count dropped · ±${seconds(window.windowMs)} · " +
        "${seconds(window.coveredMs)} of ${seconds(window.spanMs)} sampled"
}

/**
 * The audio row for a platform that observes discrete events. The window is
 * stated but no coverage clause is: a listener attached across the window has
 * seen all of it, and printing "4.0 s of 4.0 s sampled" for an event stream
 * would be describing a counter this is not.
 */
internal fun preparedSwitchAudioUnderrunRow(
    count: Long,
    windowMs: Long = PREPARED_SWITCH_WINDOW_MS,
): String {
    val word = if (count == 1L) "audio underrun" else "audio underruns"
    return "$count $word · ±${seconds(windowMs)}"
}

internal fun preparedSwitchVisibleRow(ms: Long?): String = if (ms == null) "Not measured" else "$ms ms"

/**
 * What a prepared switch measured, as the three strings the ledger shows.
 *
 * A value object, so the player can hand one to the info panel and to the
 * developer screen without either being able to change it.
 */
internal data class PreparedSwitchReading(
    val frames: String,
    val audio: String,
    val visibleIn: String,
) {
    companion object {
        val unmeasured = PreparedSwitchReading(
            frames = "Not measured",
            audio = "Not measured",
            visibleIn = "Not measured",
        )
    }
}

/**
 * The recorder the player pushes into.
 *
 * Deliberately not thread-safe and deliberately dumb: the controller owns it
 * and touches it from the player's own scope, and every method is a push onto
 * a bounded list. The reading is computed on demand, which means a closed info
 * panel costs nothing and a commit costs three list appends.
 */
internal class PreparedSwitchRecorder {
    /**
     * Samples are keyed by the pipeline that reported them rather than by a
     * before/after flag, because a second directed change in the same session
     * makes the first switch's successor the second switch's predecessor. A
     * flag captured at prepare time would then file both pipelines on the same
     * side and add a switch to itself.
     */
    private class Series {
        val frames = ArrayDeque<PreparedSwitchSample>()
        val underruns = ArrayDeque<Long>()
        var droppedTotal = 0L
    }

    private val series = LinkedHashMap<Int, Series>()

    /** Null until a commit has happened, which is what a fresh player looks like. */
    var commitAtMs: Long? = null
        private set
    private var predecessor: Int? = null
    private var successor: Int? = null
    private var tappedAtMs: Long? = null
    private var firstFrameAtMs: Long? = null

    private fun seriesFor(pipeline: Int): Series {
        series[pipeline]?.let { return it }
        // Two pipelines matter and a third means the switch being measured is
        // over. Evicting the oldest keeps this bounded without any timer.
        while (series.size >= 3) series.remove(series.keys.first())
        return Series().also { series[pipeline] = it }
    }

    private fun push(into: ArrayDeque<PreparedSwitchSample>, atMs: Long, count: Long) {
        into.addLast(PreparedSwitchSample(atMs, count))
        while (into.size > PREPARED_SWITCH_SAMPLES_MAX) into.removeFirst()
    }

    /**
     * `AnalyticsListener.onDroppedVideoFrames` reports an INCREMENT since the
     * last call, not a total, so the running sum is kept here.
     */
    fun noteDroppedFrames(atMs: Long, dropped: Int, pipeline: Int) {
        val entry = seriesFor(pipeline)
        entry.droppedTotal += maxOf(0, dropped).toLong()
        push(entry.frames, atMs, entry.droppedTotal)
    }

    /**
     * `AnalyticsListener.onAudioUnderrun` is an event, not a counter. It is
     * recorded as the instant it happened and counted by
     * [preparedSwitchEventCount], so a window with none in it is a measured
     * zero rather than an unmeasured window.
     */
    fun noteAudioUnderrun(atMs: Long, pipeline: Int) {
        val entry = seriesFor(pipeline)
        entry.underruns.addLast(atMs)
        while (entry.underruns.size > PREPARED_SWITCH_SAMPLES_MAX) entry.underruns.removeFirst()
    }

    /** The successor has the surface. Assignments only; nothing is read back. */
    fun noteCommit(atMs: Long, tappedAtMs: Long?, predecessor: Int, successor: Int) {
        commitAtMs = atMs
        this.predecessor = predecessor
        this.successor = successor
        this.tappedAtMs = tappedAtMs
        firstFrameAtMs = null
    }

    /** The successor rendered. Recorded once; a later frame does not move it. */
    fun noteFirstFrame(atMs: Long) {
        if (commitAtMs != null && firstFrameAtMs == null) firstFrameAtMs = atMs
    }

    /** The three rows, computed on demand. */
    fun reading(): PreparedSwitchReading {
        val commit = commitAtMs ?: return PreparedSwitchReading.unmeasured
        val before = predecessor?.let { series[it] }
        val after = successor?.let { series[it] }
        return PreparedSwitchReading(
            frames = preparedSwitchFramesRow(
                preparedSwitchCounterDelta(
                    before?.frames?.toList() ?: emptyList(),
                    after?.frames?.toList() ?: emptyList(),
                    commit,
                ),
            ),
            audio = preparedSwitchAudioUnderrunRow(
                preparedSwitchEventCount(
                    (before?.underruns?.toList() ?: emptyList()).filter { it <= commit } +
                        (after?.underruns?.toList() ?: emptyList()).filter { it >= commit },
                    commit,
                ),
            ),
            visibleIn = preparedSwitchVisibleRow(
                preparedSwitchVisibleInMs(tappedAtMs, firstFrameAtMs),
            ),
        )
    }
}

/**
 * What an audible-seam measurement would need on this platform, and whether
 * each condition currently holds.
 *
 * Advisory in the strict sense: there is no switch attached to any of these,
 * nothing reads them back, and a condition that is not met never stops a
 * quality change, never disables the prepared handoff, and never turns a row
 * into a failure. Android's underrun callback is the one audio observation that
 * costs the audible path nothing, which is why it is the one that ships.
 */
internal fun preparedSwitchAudioRequirements(
    underrunCallbackAvailable: Boolean,
): List<Triple<String, String, Boolean>> = listOf(
    Triple(
        "Audio-sink underrun reporting",
        "Met by Media3's AnalyticsListener. A pure observation of the audio sink: " +
            "it adds no processing to the audible path and cannot mute it.",
        underrunCallbackAvailable,
    ),
    Triple(
        "Waveform gap detection",
        "Not available. Reading the decoded waveform needs an audio processor in the " +
            "sink chain, which is the audible path itself; a processor that stalls is a " +
            "silence the viewer hears. The underrun count above is what ships instead.",
        false,
    ),
)
