package tv.plurx.app.player

import org.junit.Assert.assertEquals
import org.junit.Test

class PlaybackLifecycleTest {
    @Test fun videoBackgroundPauseRetainsViewerIntentForForegroundResume() {
        val stopped = lifecyclePlaybackTransition(false, false, false, false, true)
        assertEquals(LifecyclePlaybackTransition(true, LifecyclePlaybackEffect.PauseVideo), stopped)
        assertEquals(
            LifecyclePlaybackTransition(false, LifecyclePlaybackEffect.ResumeVideo),
            lifecyclePlaybackTransition(stopped.ownerPaused, true, false, false, true),
        )
    }

    @Test fun viewerPauseWhileBackgroundedCannotBeUndoneOnReturn() {
        val stopped = lifecyclePlaybackTransition(false, false, false, false, true)
        assertEquals(
            LifecyclePlaybackTransition(false, LifecyclePlaybackEffect.None),
            lifecyclePlaybackTransition(stopped.ownerPaused, true, false, false, false),
        )
    }

    @Test fun visiblePipDoesNotOwnerPauseVideo() {
        assertEquals(
            LifecyclePlaybackTransition(false, LifecyclePlaybackEffect.None),
            lifecyclePlaybackTransition(false, false, true, false, true),
        )
    }

    @Test fun audioOnlyContinuesWhenActivityStops() {
        assertEquals(
            LifecyclePlaybackTransition(false, LifecyclePlaybackEffect.None),
            lifecyclePlaybackTransition(false, false, false, true, true),
        )
    }

    @Test fun repeatedBackgroundCallbacksSpendOneOwnerPause() {
        val stopped = lifecyclePlaybackTransition(false, false, false, false, true)
        assertEquals(
            LifecyclePlaybackTransition(true, LifecyclePlaybackEffect.None),
            lifecyclePlaybackTransition(stopped.ownerPaused, false, false, false, true),
        )
    }
}
