package tv.plurx.app.livetv

import androidx.compose.ui.unit.dp
import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The proportions the native Live TV screens are built to. Lists are tall and
 * narrow; grids are wide — and a grid whose slot width is a constant cannot
 * be either, which is what left 35% of a television screen empty.
 */
class LiveTvProportionsTest {
    @Test
    fun televisionSlotWidthFollowsTheScreenAndThePhoneKeepsItsFixedColumns() {
        val dimensions = LiveTvGridMetrics.forTelevision(880.dp)
        assertEquals(195.dp, dimensions.slotWidth)
        assertEquals(37.dp, dimensions.rowHeight)
        assertEquals(100.dp, dimensions.channelColumnWidth)
        assertEquals(
            "the channel column plus the visible slots must be exactly the content width",
            880.dp,
            dimensions.channelColumnWidth +
                dimensions.slotWidth * LiveTvGridMetrics.TELEVISION_VISIBLE_SLOTS,
        )
        // A wider panel gets wider slots; the number of them never changes.
        assertEquals(220.dp, LiveTvGridMetrics.forTelevision(980.dp).slotWidth)
        assertEquals(4, LiveTvGridMetrics.TELEVISION_VISIBLE_SLOTS)

        assertEquals(LiveTvGridMetrics.slotWidth, LiveTvGridMetrics.phone.slotWidth)
        assertEquals(LiveTvGridMetrics.rowHeight, LiveTvGridMetrics.phone.rowHeight)
        assertEquals(LiveTvGridMetrics.channelColumnWidth, LiveTvGridMetrics.phone.channelColumnWidth)
        assertEquals(160.dp, LiveTvGridMetrics.phone.slotWidth)
    }

    /**
     * The arrangement itself, not just the constants behind it. Each of these
     * survives a change to the numbers and none of them survives reverting the
     * rewrite.
     */
    @Test
    fun theTelevisionScreenIsWiredToTheDerivedGeometryAndOneToolbar() {
        fun source(name: String): String = listOf(
            File("app/src/main/java/tv/plurx/app/livetv/$name"),
            File("src/main/java/tv/plurx/app/livetv/$name"),
            File("clients/android/app/src/main/java/tv/plurx/app/livetv/$name"),
        ).firstOrNull(File::isFile)?.readText() ?: error("$name source not found")
        val screen = source("LiveTvScreen.kt")
        val guideUi = source("LiveTvGuideUi.kt")

        // The grid draws from the dimensions it is handed, never from the
        // shared object — that is what makes the slot width follow the screen.
        val body = guideUi.substringAfter("fun LiveTvGuideGrid(")
            .substringBefore("private fun LiveTvGridCellButton(")
        assertTrue("the grid must draw from its dimensions", body.contains("dimensions.slotWidth"))
        assertTrue(body.contains("dimensions.rowHeight"))
        assertTrue(body.contains("dimensions.channelColumnWidth"))
        assertTrue(
            "the grid must not reach past its own dimensions",
            !body.contains("LiveTvGridMetrics.slotWidth") &&
                !body.contains("LiveTvGridMetrics.rowHeight"),
        )
        // The television call sites measure before they size.
        assertTrue(screen.contains("LiveTvGridMetrics.forTelevision(maxWidth)"))
        assertTrue(screen.contains("BoxWithConstraints"))
        // One toolbar, one status line, and none of the bands they replaced.
        assertTrue(screen.contains("LiveTvStatusLine("))
        assertTrue(screen.contains("private fun LiveTvPhoneToolbar("))
        assertTrue("the six-line now bar is gone", !screen.contains("private fun LiveTvNowBar("))
        // FlowRow is intentional: the current toolbar wraps at larger font scales.
        // Its presence no longer identifies the retired filter row.
        // The picture is a focus target with a bounded focus rect.
        assertTrue(screen.contains("private fun LiveTvPicture("))
        assertTrue(screen.contains("focusModifier"))
        // Preview/list pane proportions vary by the selected layout; the
        // derived grid dimensions and bounded picture focus above are shared.
    }

    /**
     * Two entries, three stored values. An existing `channel_browser`
     * preference must keep decoding rather than becoming the default by
     * accident, and it must render as Preview.
     */
    @Test
    fun theChannelBrowserLayoutIsPresentedAsPreview() {
        assertEquals(TvLiveLayout.GuidePreview, TvLiveLayout.fromStorage("channel_browser"))
        assertEquals(TvLiveLayout.GuidePreview, TvLiveLayout.ChannelBrowser.presented)
        assertEquals(TvLiveLayout.GuidePreview, TvLiveLayout.fromStorage("guide_preview"))
        assertEquals(TvLiveLayout.GuideOverlay, TvLiveLayout.fromStorage("guide_overlay"))
        assertEquals(TvLiveLayout.GuidePreview, TvLiveLayout.fromStorage(null))
        assertEquals(
            listOf(TvLiveLayout.GuidePreview, TvLiveLayout.GuideOverlay),
            TvLiveLayout.offered,
        )
        assertEquals(listOf("Preview", "Over picture"), TvLiveLayout.offered.map { it.label })
        assertEquals("channel_browser", TvLiveLayout.ChannelBrowser.storageValue)
        assertEquals(3, TvLiveLayout.entries.size)
    }

    /**
     * Material's semantic styles are sized for a phone held at arm's length.
     * The ten-foot scale is explicit so a `titleLarge` cannot quietly become a
     * banner on a television.
     */
    @Test
    fun theTelevisionTypeScaleIsExplicitAndOrdered() {
        val scale = LiveTvTypography.television
        val sizes = listOf(
            scale.title.fontSize.value,
            scale.primary.fontSize.value,
            scale.secondary.fontSize.value,
            scale.tertiary.fontSize.value,
            scale.badge.fontSize.value,
            scale.eyebrow.fontSize.value,
        )
        assertEquals(listOf(15f, 11f, 10f, 9f, 8f, 7f), sizes)
        assertEquals(scale.primary.fontSize, scale.cell.fontSize)
        assertTrue("every size must be specified", sizes.all { it > 0f })
    }
}
