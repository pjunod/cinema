package tv.plurx.app.player

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The playback info panel reserves room above the transport block only while
 * the transport is actually composed. Keying the reserve on `controlsVisible`
 * alone squashed the Android TV info body to its header, because opening the
 * panel hides the transport without clearing `controlsVisible`.
 */
class PlaybackTransportReserveTest {
    private fun onScreen(
        panel: PlayerPanel? = null,
        mode: PlaybackStatsMode = PlaybackStatsMode.Standard,
        isInPip: Boolean = false,
        controlsVisible: Boolean = true,
        faulted: Boolean = false,
    ) = playbackTransportOnScreen(isInPip, controlsVisible, panel, mode, faulted)

    @Test
    fun infoPanelInAnyModeButMiniHidesTheTransport() {
        for (mode in listOf(PlaybackStatsMode.Standard, PlaybackStatsMode.Details, PlaybackStatsMode.Debug)) {
            assertFalse("$mode must not reserve transport height", onScreen(panel = PlayerPanel.Info, mode = mode))
        }
    }

    @Test
    fun miniInfoStripKeepsTheTransportAndItsReserve() {
        assertTrue(onScreen(panel = PlayerPanel.Info, mode = PlaybackStatsMode.Mini))
    }

    @Test
    fun otherPanelsAndHiddenControlsHideTheTransport() {
        assertFalse(onScreen(panel = PlayerPanel.Tracks))
        assertFalse(onScreen(panel = PlayerPanel.Settings))
        assertFalse(onScreen(controlsVisible = false))
        assertFalse(onScreen(isInPip = true))
        assertFalse(onScreen(faulted = true))
        assertTrue(onScreen())
    }
}
