package tv.plurx.app.player

import org.junit.Assert.assertEquals
import org.junit.Test
import tv.plurx.app.data.PlaybackQuality

class PlayerSettingsFocusTest {
    @Test
    fun firstAvailableRungReceivesFocusWhenStoredQualityIsAbsent() {
        val options = listOf(
            QualityOption(PlaybackQuality.Q720, "720p"),
            QualityOption(PlaybackQuality.Q480, "480p"),
        )

        assertEquals(0, playerSettingsFocusIndex(PlaybackQuality.Q1080, options))
        assertEquals(1, playerSettingsFocusIndex(PlaybackQuality.Q480, options))
    }
}
