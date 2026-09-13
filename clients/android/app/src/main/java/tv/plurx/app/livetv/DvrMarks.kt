package tv.plurx.app.livetv

/**
 * What a guide cell says about the DVR.
 *
 * Everything here is a pure function over one `/dvr/schedule` answer and one
 * `/dvr/reminders` answer, in the same shape as [LiveTvGuideReducer]: the grid
 * asks it per cell while it draws, so the index is built once per read and
 * never rebuilt per row. That is also why the marks are not polled — the two
 * reads happen on a guide load and after every mutation, and a timer that
 * re-read them would be asking the server to repeat an answer nothing had
 * changed.
 */

/** An airing's identity, here as everywhere: `(channel, start)`, never a row id. */
data class DvrAiring(val channelId: String, val airingStart: Long)

/**
 * The one glyph a cell draws for its recording, if any. A reminder's bell is
 * separate because the two are independent facts: a viewer may be reminded
 * about a programme that is also being recorded.
 */
enum class DvrMarkShape {
    /** A one-off somebody asked for. */
    Scheduled,

    /** A rule materialised it — two dots, so a series is legible at a glance. */
    Series,

    /** Planned, but no tuner is free. Amber; the Scheduled list says why. */
    Conflict,

    /** A rule stopped matching, or a manual row's programme moved. Hollow. */
    Withdrawn,

    /** Capturing now, with an underline showing how far through. */
    Recording,
}

data class DvrCellMark(
    val shape: DvrMarkShape? = null,
    val bell: Boolean = false,
    /** Set only for [DvrMarkShape.Recording]; it is the underline's fraction. */
    val progress: Float? = null,
) {
    val drawn: Boolean get() = shape != null || bell

    companion object {
        val NONE = DvrCellMark()
    }
}

/**
 * One schedule and one reminder list, indexed by airing.
 *
 * Cancelled rows are deliberately accepted and deliberately unmarked: the
 * schedule is fetched with `?cancelled=1` so the Scheduled list can offer
 * Restore, and a skipped episode drawing a dot in the guide would say the
 * opposite of what the viewer decided.
 */
class DvrGuideMarks(
    recordings: List<DvrRecording> = emptyList(),
    reminders: List<DvrReminder> = emptyList(),
) {
    private val rows: Map<DvrAiring, DvrRecording> =
        recordings.associateBy { DvrAiring(it.channel_id, it.airing_start) }

    // An acked or expired reminder is one the viewer is done with; only a
    // reminder still waiting to be told about draws a bell.
    private val bells: Set<DvrAiring> = reminders
        .filter { it.state == "armed" || it.state == "fired" }
        .mapTo(mutableSetOf()) { DvrAiring(it.channel_id, it.airing_start) }

    fun recording(channelId: String, airingStart: Long): DvrRecording? =
        rows[DvrAiring(channelId, airingStart)]

    fun reminded(channelId: String, airingStart: Long): Boolean =
        DvrAiring(channelId, airingStart) in bells

    fun mark(channelId: String, airingStart: Long, now: Long): DvrCellMark {
        val row = rows[DvrAiring(channelId, airingStart)]
        val shape = when (row?.state) {
            "recording" -> DvrMarkShape.Recording
            "conflict" -> DvrMarkShape.Conflict
            "withdrawn", "stale" -> DvrMarkShape.Withdrawn
            "scheduled" -> if (row.rule_id != null) DvrMarkShape.Series else DvrMarkShape.Scheduled
            else -> null
        }
        return DvrCellMark(
            shape = shape,
            bell = reminded(channelId, airingStart),
            progress = if (shape == DvrMarkShape.Recording) row?.captureProgress(now) else null,
        )
    }

    companion object {
        val EMPTY = DvrGuideMarks()
    }
}
