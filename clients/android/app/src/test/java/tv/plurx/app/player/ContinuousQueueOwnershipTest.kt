@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class ContinuousQueueOwnershipTest {
    @Test fun exposedBytesWithoutAcceptedMetadataCannotInventAnAppendOrQueueRetirement() {
        val owner = Any()
        val row = buildJsonObject {
            put("rendition_id", "b".repeat(64)); put("timescale", 24); put("frame_ticks", 1); put("segment_ticks", 48)
        }
        val interval = buildJsonObject {
            put("artifact_id", "c".repeat(64)); put("rendition_id", row.getValue("rendition_id"))
            put("timescale", 24); put("from_tick", 0); put("through_tick", 48); put("byte_length", 1000)
        }
        val load = ContinuousLoadContext.Verified(owner, ContinuousQualityMedia.Resource("video", row, false, 0),
            ContinuousQualityMedia.Authorized(interval, setOf("transaction")))
        val queues = ContinuousQueueOwnership(owner)
        queues.opened(load.copy(owner = Any()))
        assertTrue(queues.queuedArtifacts().isEmpty())
        queues.opened(load)
        assertEquals(listOf(load), queues.queuedArtifacts())
        assertTrue(queues.noAcceptedSamples(load))
        assertNull(queues.completed(load))
        assertFalse(queues.wasAppended("b".repeat(64), "c".repeat(64)))
        assertFalse(queues.queueRetired("b".repeat(64), "c".repeat(64)))
    }
}
