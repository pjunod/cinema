package tv.plurx.app.player

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class AutoLinkMediaSegmentTest {
    private val parent = "/api/v1/hls/00000000-0000-4000-8000-000000000001"
    private val rendition = "a".repeat(64)

    @Test fun continuousVideoSegmentsAreLinkSamplesLikeOrdinarySegments() {
        // Auto never saw a completed transfer on a continuous attachment: its
        // child segment paths did not match the ordinary seg<N> name.
        assertTrue(autoLinkMediaSegment("$parent/video/$rendition/segment/42.m4s"))
        assertTrue(autoLinkMediaSegment("$parent/seg7.m4s"))
        assertTrue(autoLinkMediaSegment("$parent/seg7.ts"))
    }

    @Test fun playlistsInitsAudioAndForeignShapesAreNot() {
        assertFalse(autoLinkMediaSegment("$parent/video/$rendition/index.m3u8"))
        assertFalse(autoLinkMediaSegment("$parent/video/$rendition/init/${"b".repeat(64)}.mp4"))
        assertFalse(autoLinkMediaSegment("$parent/audio/$rendition/segment/42.m4s"))
        assertFalse(autoLinkMediaSegment("$parent/video/short/segment/42.m4s"))
        assertFalse(autoLinkMediaSegment("$parent/init.mp4"))
    }
}
