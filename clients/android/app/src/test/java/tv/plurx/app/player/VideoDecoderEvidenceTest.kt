package tv.plurx.app.player

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class VideoDecoderEvidenceTest {
    @Test
    fun onlyTheCurrentSuccessfullyInitializedCodecOwnsItsEvidence() {
        val evidence = VideoDecoderEvidence()
        assertNull(evidence.codecMimeType)
        val native = evidence.initialized("dolby", "video/dolby-vision")
        assertEquals("video/dolby-vision", evidence.codecMimeType)
        val fallback = evidence.initialized("hevc", "video/hevc")
        evidence.released(native)
        assertEquals("video/hevc", evidence.codecMimeType)
        evidence.released(fallback)
        assertNull(evidence.codecMimeType)
    }

    @Test
    fun aLateReleaseCannotClearAReinitializedCodecWithTheSameName() {
        val evidence = VideoDecoderEvidence()
        val retired = evidence.initialized("dolby", "video/dolby-vision")
        val current = evidence.initialized("dolby", "video/dolby-vision")
        // Codec names are reused. Only the initialization's identity owns the
        // report, so an old release cannot remove the replacement's evidence.
        evidence.released(retired)
        assertEquals("video/dolby-vision", evidence.codecMimeType)
        evidence.released(current)
        assertNull(evidence.codecMimeType)
    }
}
