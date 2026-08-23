package tv.plurx.app.data

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class SessionTest {
    @Test
    fun clusterMediaFailoverUsesValidatedNodesWithoutMovingAccountOrigin() {
        Session.origin = "http://primary.local:32400"
        Session.token = "bearer"
        Session.configureNodeOrigins(
            listOf(
                "http://primary.local:32400",
                "http://node-b.local:32400/",
                "ftp://bad.local:32400",
                "http://user:pass@bad.local:32400",
                "http://node-b.local:32400",
                "https://node-c.local:443",
            ),
            Session.origin,
        )

        assertEquals(
            "http://node-b.local:32400/api/v1/hls/cap/index.m3u8",
            Session.nextMediaFailoverUrl("/api/v1/hls/cap/index.m3u8", authenticated = false),
        )
        assertEquals(
            "https://node-c.local:443/api/v1/files/7/content?token=bearer",
            Session.nextMediaFailoverUrl("/api/v1/files/7/content", authenticated = true),
        )
        assertNull(Session.nextMediaFailoverUrl("/api/v1/hls/cap/index.m3u8", authenticated = false))
        assertEquals("http://primary.local:32400", Session.origin)
    }
}
