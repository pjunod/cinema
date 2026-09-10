package tv.plurx.app.livetv

import kotlinx.serialization.Serializable

/**
 * The programme guide, as the client sees it.
 *
 * Everything below the models is a pure function over a decoded guide, in the
 * same shape as [tv.plurx.app.player.PlayerInputPolicy]: a transcription the
 * JVM tests check against `tests/playback/live-tv-guide-cases.json`, never a
 * reader of that file at runtime. The web renderer and the Apple client answer
 * the same cases from the same fixture, so "what is on now" cannot mean two
 * different things on two clients.
 *
 * Wire names are snake_case verbatim, matching every other model in this
 * package; `Net.json` ignores unknown keys, so a server that grows a field
 * does not break an older build.
 */
@Serializable
data class LiveTvProgramme(
    val start: Long,
    val end: Long,
    val title: String,
    val episode_title: String? = null,
    val episode: String? = null,
    val synopsis: String? = null,
    val image_url: String? = null,
    val original_air_date: String? = null,
    val filters: List<String> = emptyList(),
) {
    val seconds: Long get() = (end - start).coerceAtLeast(0)
}

@Serializable
data class LiveTvGuideChannel(
    val id: String,
    val guide_number: String,
    val affiliate: String? = null,
    val image_url: String? = null,
    val programmes: List<LiveTvProgramme> = emptyList(),
)

@Serializable
data class LiveTvGuideWindow(val start: Long, val end: Long)

@Serializable
data class LiveTvGuide(
    val source: String = "off",
    val freshness: String = "unavailable",
    val age_seconds: Long = 0,
    val fetched_at: Long? = null,
    val window: LiveTvGuideWindow = LiveTvGuideWindow(0, 0),
    val refresh_error: String? = null,
    val matched_channels: Int = 0,
    val lineup_channels: Int = 0,
    val channels: List<LiveTvGuideChannel> = emptyList(),
) {
    /**
     * Off, empty, stale and erroring are all *rendered* states. A channel row
     * with no programme line is the degraded screen, not a failed one, and the
     * lineup and session start never consult any of this.
     */
    val hasProgrammes: Boolean get() = channels.any { it.programmes.isNotEmpty() }
}

/** What is on a channel now, what is next, and how far through we are. */
data class LiveTvAiring(
    val now: LiveTvProgramme? = null,
    val next: LiveTvProgramme? = null,
    /**
     * `null` exactly when nothing is on — a real hole in the guide is a state
     * to draw, not one to paper over with the programme that just ended.
     */
    val progress: Float? = null,
)

/** One positioned cell in the half-hour grid. */
data class LiveTvGridCell(
    val programme: LiveTvProgramme,
    val left: Float,
    val width: Float,
    val airing: Boolean,
    val clipped: Boolean,
)

data class LiveTvGridRow(val channel: LiveTvChannel, val cells: List<LiveTvGridCell>)

data class LiveTvGridLayout(
    val rows: List<LiveTvGridRow>,
    val totalWidth: Float,
    /**
     * `null` when now falls outside the drawn window, so the red rule is
     * simply not drawn rather than clamped to an edge it does not mean.
     */
    val nowX: Float?,
)

/** Stable guide identity used by television focus; geometry and list indexes never identify a cell. */
data class LiveTvGuideFocusTarget(
    val channelId: String,
    val programmeStart: Long? = null,
    val channelHeader: Boolean = false,
)

data class LiveTvGuideFocusMove(
    val target: LiveTvGuideFocusTarget?,
    val anchorTime: Long?,
    val toolbarBoundary: Boolean = false,
)

enum class LiveTvGuideFocusDirection { Left, Right, Up, Down }

data class LiveTvChannelFilter(
    val query: String = "",
    val favoritesOnly: Boolean = false,
    val hideProtected: Boolean = false,
)

object LiveTvGuideReducer {
    const val SLOT_SECONDS: Long = 1800
    const val VISIBLE_SLOTS: Int = 8

    fun channel(guide: LiveTvGuide?, channelId: String): LiveTvGuideChannel? =
        guide?.channels?.firstOrNull { it.id == channelId }

    /**
     * The start instant belongs to the programme that starts; the end instant
     * does not. Without that rule a viewer at exactly 8:30 sees two programmes
     * on air, and "what is on now" stops being a question with one answer.
     */
    fun airing(guide: LiveTvGuide?, channelId: String, now: Long): LiveTvAiring {
        val rows = channel(guide, channelId)?.programmes.orEmpty()
        var current: LiveTvProgramme? = null
        var next: LiveTvProgramme? = null
        for (row in rows) {
            if (row.start <= now && now < row.end) {
                current = row
                continue
            }
            if (row.start > now && (next == null || row.start < next.start)) next = row
        }
        val span = current?.seconds ?: 0
        val progress = if (current != null && span > 0) (now - current.start).toFloat() / span else null
        return LiveTvAiring(now = current, next = next, progress = progress)
    }

    /**
     * Positioned cells for the half-hour grid. Clips to the window, never
     * overlaps, and reports where the red now line goes. Geometry only: what a
     * cell looks like is the composable's business.
     */
    fun gridLayout(
        guide: LiveTvGuide?,
        channels: List<LiveTvChannel>,
        window: LiveTvGuideWindow,
        now: Long,
        slotSeconds: Long = SLOT_SECONDS,
        pxPerSlot: Float,
    ): LiveTvGridLayout {
        val slot = if (slotSeconds > 0) slotSeconds else SLOT_SECONDS
        val px = if (pxPerSlot > 0f) pxPerSlot else 240f
        fun scale(seconds: Long): Float = (seconds - window.start).toFloat() / slot * px
        // Named `entry`, not `channel`: shadowing the reducer's own `channel`
        // function with the loop variable reads as a call on the value, and is
        // the construct most likely to break under a rename nobody here can
        // compile.
        val rows = channels.map { entry ->
            val programmes = channel(guide, entry.id)?.programmes.orEmpty()
            val cells = programmes.mapNotNull { programme ->
                val start = maxOf(programme.start, window.start)
                val end = minOf(programme.end, window.end)
                if (end <= start) {
                    null
                } else {
                    LiveTvGridCell(
                        programme = programme,
                        left = scale(start),
                        width = scale(end) - scale(start),
                        airing = programme.start <= now && now < programme.end,
                        clipped = programme.start < window.start || programme.end > window.end,
                    )
                }
                // The grid composes a button per cell in a plain Box, not a
                // lazy row, and the guide is relayed third-party content whose
                // only server-side bound is the 2 MiB document cap. Without a
                // ceiling a feed of one-second programmes would compose tens of
                // thousands of buttons in one pass and hang the app.
            }.take(MAX_CELLS_PER_ROW)
            LiveTvGridRow(entry, cells)
        }
        return LiveTvGridLayout(
            rows = rows,
            totalWidth = scale(window.end),
            nowX = if (now in window.start..window.end) scale(now) else null,
        )
    }

    /** Half-hour column headings across the window. */
    fun gridSlots(window: LiveTvGuideWindow, slotSeconds: Long = SLOT_SECONDS): List<Long> {
        val slot = if (slotSeconds > 0) slotSeconds else SLOT_SECONDS
        val out = mutableListOf<Long>()
        var at = window.start
        while (at < window.end) {
            out.add(at)
            at += slot
        }
        return out
    }

    /**
     * Where the guide's data stops on a channel — the hatched marker. `null`
     * when it runs past the window, which is the ordinary case.
     */
    fun guideEnds(guide: LiveTvGuide?, channelId: String, window: LiveTvGuideWindow): Long? {
        val rows = channel(guide, channelId)?.programmes.orEmpty()
        val last = rows.lastOrNull() ?: return window.start
        return if (last.end >= window.end) null else last.end
    }

    /** The window the grid draws, anchored to the current half hour. */
    /** As many cells as a four-hour window can meaningfully draw. */
    const val MAX_CELLS_PER_ROW: Int = 240

    fun window(now: Long, slotSeconds: Long = SLOT_SECONDS, slots: Int = VISIBLE_SLOTS): LiveTvGuideWindow {
        val slot = if (slotSeconds > 0) slotSeconds else SLOT_SECONDS
        // `Math.floorMod` is API 24 and `minSdk` is 23 with no core-library
        // desugaring, so opening the guide would have thrown NoSuchMethodError
        // on an API 23 device — and a JVM unit test cannot see that.
        val start = now - now.mod(slot)
        return LiveTvGuideWindow(start, start + slots * slot)
    }

    /**
     * Search matches the number, the callsign, and what is on now — typing
     * what you can see on screen should find the channel showing it.
     */
    fun filter(
        channels: List<LiveTvChannel>,
        guide: LiveTvGuide?,
        options: LiveTvChannelFilter,
        now: Long,
    ): List<LiveTvChannel> {
        val query = options.query.trim()
        return channels.filter { channel ->
            when {
                options.favoritesOnly && !channel.favorite -> false
                options.hideProtected && !channel.watchable -> false
                query.isEmpty() -> true
                else -> {
                    val title = airing(guide, channel.id, now).now?.title.orEmpty()
                    "${channel.guide_number} ${channel.guide_name} $title".contains(query, ignoreCase = true)
                }
            }
        }
    }

    /**
     * Which channel is ±1 from [current] in the visible order, wrapping. An
     * unknown current lands on the first, so channel-up from a channel that
     * was just filtered away still goes somewhere.
     */
    fun adjacent(visible: List<String>, current: String?, delta: Int): String? {
        if (visible.isEmpty()) return null
        val at = visible.indexOf(current)
        if (at < 0) return visible.first()
        return visible[(at + delta).mod(visible.size)]
    }

    /**
     * Resolve one D-pad move by channel/programme identity and UTC time.
     * Vertical movement preserves the original time anchor across short and
     * long programmes instead of repeatedly adopting each cell's midpoint.
     */
    fun moveGuideFocus(
        layout: LiveTvGridLayout,
        current: LiveTvGuideFocusTarget,
        anchorTime: Long?,
        direction: LiveTvGuideFocusDirection,
    ): LiveTvGuideFocusMove {
        val rowIndex = layout.rows.indexOfFirst { it.channel.id == current.channelId }
        if (rowIndex < 0) return LiveTvGuideFocusMove(null, anchorTime)
        val row = layout.rows[rowIndex]
        val cells = row.cells.sortedBy { it.programme.start }

        if (direction == LiveTvGuideFocusDirection.Left || direction == LiveTvGuideFocusDirection.Right) {
            if (current.channelHeader) {
                val first = cells.firstOrNull()
                return if (direction == LiveTvGuideFocusDirection.Right && first != null) {
                    val midpoint = programmeMidpoint(first.programme)
                    LiveTvGuideFocusMove(
                        LiveTvGuideFocusTarget(row.channel.id, first.programme.start),
                        midpoint,
                    )
                } else {
                    LiveTvGuideFocusMove(current, anchorTime)
                }
            }
            if (current.programmeStart == null) {
                return if (direction == LiveTvGuideFocusDirection.Left) {
                    LiveTvGuideFocusMove(
                        LiveTvGuideFocusTarget(row.channel.id, channelHeader = true),
                        anchorTime,
                    )
                } else {
                    LiveTvGuideFocusMove(current, anchorTime)
                }
            }
            val cellIndex = cells.indexOfFirst { it.programme.start == current.programmeStart }
            if (cellIndex < 0) return LiveTvGuideFocusMove(current, anchorTime)
            val next = cellIndex + if (direction == LiveTvGuideFocusDirection.Right) 1 else -1
            if (next < 0) {
                return LiveTvGuideFocusMove(
                    LiveTvGuideFocusTarget(row.channel.id, channelHeader = true),
                    anchorTime,
                )
            }
            val target = cells.getOrNull(next) ?: return LiveTvGuideFocusMove(current, anchorTime)
            return LiveTvGuideFocusMove(
                LiveTvGuideFocusTarget(row.channel.id, target.programme.start),
                programmeMidpoint(target.programme),
            )
        }

        val nextRowIndex = rowIndex + if (direction == LiveTvGuideFocusDirection.Down) 1 else -1
        if (nextRowIndex < 0) return LiveTvGuideFocusMove(null, anchorTime, toolbarBoundary = true)
        val nextRow = layout.rows.getOrNull(nextRowIndex)
            ?: return LiveTvGuideFocusMove(current, anchorTime)
        if (current.channelHeader) {
            return LiveTvGuideFocusMove(
                LiveTvGuideFocusTarget(nextRow.channel.id, channelHeader = true),
                anchorTime,
            )
        }
        if (nextRow.cells.isEmpty()) {
            return LiveTvGuideFocusMove(LiveTvGuideFocusTarget(nextRow.channel.id), anchorTime)
        }
        val anchor = anchorTime ?: current.programmeStart
        val target = anchor?.let { at ->
            nextRow.cells.firstOrNull { it.programme.start <= at && at < it.programme.end }
                ?: nextRow.cells.minByOrNull { kotlin.math.abs(programmeMidpoint(it.programme) - at) }
        } ?: nextRow.cells.first()
        return LiveTvGuideFocusMove(
            LiveTvGuideFocusTarget(nextRow.channel.id, target.programme.start),
            anchor ?: programmeMidpoint(target.programme),
        )
    }

    private fun programmeMidpoint(programme: LiveTvProgramme): Long =
        programme.start + (programme.end - programme.start).coerceAtLeast(1) / 2
}
