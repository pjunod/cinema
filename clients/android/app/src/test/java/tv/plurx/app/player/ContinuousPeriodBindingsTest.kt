@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import androidx.media3.exoplayer.source.MediaSource
import org.junit.Assert.*
import org.junit.Test

class ContinuousPeriodBindingsTest {
    @Test fun maskedPlaylistPeriodsRequireTheirSourceSequenceAndLiveOwner() {
        val bindings = ContinuousPeriodBindings<Any>()
        val owner = Any()
        val child = MediaSource.MediaPeriodId(Any(), 17L)
        val masked = child.copyWithPeriodUid(Any())
        bindings.bind(child, "cq-owned-source", owner)
        assertSame(owner, bindings.binding(masked, "cq-owned-source"))
        assertNull(bindings.binding(masked, "cq-foreign-source"))
        assertNull(bindings.binding(MediaSource.MediaPeriodId(masked.periodUid, 18L), "cq-owned-source"))
        assertNull(bindings.binding(MediaSource.MediaPeriodId(masked.periodUid, 0, 0, 17L), "cq-owned-source"))
        bindings.release(child, Any())
        assertSame(owner, bindings.binding(masked, "cq-owned-source"))
        bindings.release(child, owner)
        assertNull(bindings.binding(masked, "cq-owned-source"))
        assertFalse(bindings.owns(owner))
        bindings.bind(child, "cq-owned-source", owner)
        bindings.bind(child.copyWithPeriodUid(Any()), "cq-owned-source", Any())
        assertNull(bindings.binding(masked, "cq-owned-source"))
    }
}
