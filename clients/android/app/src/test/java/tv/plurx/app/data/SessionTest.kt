package tv.plurx.app.data

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class SessionTest {
    @Test
    fun clusterMediaFailoverUsesValidatedNodesWithoutMovingAccountOrigin() {
        Session.origin = "http://primary.local:32400"
        Session.token = "bearer"
        Session.configureNodeOrigins(
            listOf(
                "http://primary.local:32400",
                "HTTP://PRIMARY.LOCAL:32400",
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
            Session.nextMediaFailoverUrl("/api/v1/hls/cap/index.m3u8"),
        )
        assertEquals(
            "https://node-c.local/api/v1/files/7/content",
            Session.nextMediaFailoverUrl("/api/v1/files/7/content"),
        )
        assertNull(Session.nextMediaFailoverUrl("/api/v1/hls/cap/index.m3u8"))
        assertEquals("http://primary.local:32400", Session.origin)
    }

    /**
     * A candidate becomes a request authority the moment it is used, and this
     * app's media data source attaches the account bearer to every request on
     * every host. Anything that is not plainly one of this cluster's origins
     * has to be refused here, before it can be concatenated with a path.
     */
    @Test
    fun onlyAPlainHttpOriginIsAcceptedAsAFailoverCandidate() {
        assertEquals("http://h.local:32400", Session.canonicalOrigin("HTTP://H.Local:32400"))
        assertEquals("http://h.local", Session.canonicalOrigin("http://h.local:80"))
        assertEquals("https://h.local", Session.canonicalOrigin("https://h.local:443/"))
        assertEquals("http://[::1]:32400", Session.canonicalOrigin("http://[::1]:32400"))
        assertEquals("http://plurx_b:32400", Session.canonicalOrigin("http://plurx_b:32400"))

        for (refused in listOf(
            "ftp://h.local:32400",
            "http://user:pass@h.local:32400",
            "http://h.local:32400/api",
            "http://h.local:32400/?a=b",
            "http://h.local:32400#f",
            "http://h.local:0",
            "http://h.local:99999",
            "http://h.local:",
            "//h.local:32400",
            "h.local:32400",
            "",
        )) {
            assertNull("must refuse $refused", Session.canonicalOrigin(refused))
        }
    }

    /**
     * The path is concatenated onto another origin verbatim. A value that is
     * not a server-relative path — an absolute URL, or the scheme-relative
     * `//host/x` form, which would silently retarget the request at `host` —
     * must not produce a candidate at all.
     */
    @Test
    fun onlyAServerRelativePathIsRebound() {
        Session.origin = "http://primary.local:32400"
        Session.configureNodeOrigins(listOf("http://node-b.local:32400"), Session.origin)

        assertNull(Session.nextMediaFailoverUrl("//evil.example/api/v1/hls/cap/index.m3u8"))
        assertNull(Session.nextMediaFailoverUrl("http://evil.example/api/v1/hls/cap/index.m3u8"))
        assertNull(Session.nextMediaFailoverUrl("api/v1/hls/cap/index.m3u8"))
        assertNull(Session.nextMediaFailoverUrl(""))
        assertEquals(
            "http://node-b.local:32400/api/v1/hls/cap/index.m3u8",
            Session.nextMediaFailoverUrl("/api/v1/hls/cap/index.m3u8"),
        )
    }

    /**
     * An `https` household must not be moved onto an `http` sibling: the
     * bearer travels with every media request, so a downgraded candidate puts
     * the account credential on the wire in cleartext.
     */
    @Test
    fun anHttpsSessionRefusesToFailOverToACleartextNode() {
        Session.origin = "https://primary.local"
        Session.configureNodeOrigins(
            listOf("http://node-b.local:32400", "https://node-c.local"),
            Session.origin,
        )

        val first = Session.nextMediaFailoverUrl("/api/v1/hls/cap/index.m3u8")
        assertTrue("cleartext candidate was admitted: $first", first!!.startsWith("https://"))
        assertNull(Session.nextMediaFailoverUrl("/api/v1/hls/cap/index.m3u8"))
    }

    /**
     * The index advances as nodes are tried and is reset when a fresh stream
     * starts. Without the reset, one film's failover exhausts the list for
     * every film after it in the same process.
     */
    @Test
    fun aFreshStreamStartsAtTheHeadOfTheNodeList() {
        Session.origin = "http://primary.local:32400"
        Session.configureNodeOrigins(
            listOf("http://node-b.local:32400", "http://node-c.local:32400"),
            Session.origin,
        )

        assertEquals("http://node-b.local:32400/x", Session.nextMediaFailoverUrl("/x"))
        Session.resetMediaFailover()
        assertEquals("http://node-b.local:32400/x", Session.nextMediaFailoverUrl("/x"))
        assertEquals("http://node-c.local:32400/x", Session.nextMediaFailoverUrl("/x"))
        assertNull(Session.nextMediaFailoverUrl("/x"))
    }
}
