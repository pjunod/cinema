package tv.plurx.app.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import tv.plurx.app.data.MediaFileDto
import tv.plurx.app.data.ReadingLocator
import tv.plurx.app.data.ReadingRevision
import tv.plurx.app.data.ReadingState
import tv.plurx.app.data.ReaderCapability
import tv.plurx.app.data.ReaderSurfaceCapability

class ReaderPolicyTest {
    private val epub = MediaFileDto(id = 90, filename = "Contract.EPUB")

    @Test
    fun handoffKeepsTheBearerOutOfTheUrlAndEscapesTheTrustedScript() {
        val shell = NativeReaderHandoff.shellUrl("https://cinema.example:9443")
        assertEquals("https://cinema.example:9443/?native-reader=1", shell)
        assertFalse(checkNotNull(shell).contains("bearer"))

        val script = NativeReaderHandoff.startScript("bearer\"\\line", 9, 90)
        assertEquals("window.startNativeReader(\"bearer\\\"\\\\line\",\"9\",\"90\");", script)
        assertNotNull(script)
        assertEquals(
            "window.startNativeReader(\"bearer\",\"${Long.MAX_VALUE}\",\"${Long.MAX_VALUE}\");",
            NativeReaderHandoff.startScript("bearer", Long.MAX_VALUE, Long.MAX_VALUE),
        )
    }

    @Test
    fun webViewNavigationIsSameOriginAndPublicationScoped() {
        val origin = "https://cinema.example:9443"
        assertTrue(NativeReaderHandoff.permitsNavigation(
            "$origin/api/v1/publication/cap/Text/chapter.xhtml",
            origin,
        ))
        assertTrue(NativeReaderHandoff.permitsNavigation("$origin/assets/reader.js", origin))
        assertFalse(NativeReaderHandoff.permitsNavigation("$origin/api/v1/items/9", origin))
        assertFalse(NativeReaderHandoff.permitsNavigation("https://attacker.invalid/chapter", origin))
    }

    @Test
    fun shellUsesTheSameCanonicalServerIdentityAsTheSession() {
        val origins = mapOf(
            "http://plurx_b:32400" to "http://plurx_b:32400",
            "HTTP://PLURX_B:80/" to "http://plurx_b",
            "HTTPS://Cinema.Example:443/" to "https://cinema.example",
            "https://[::1]:9443" to "https://[::1]:9443",
            "http://[::1]:80/" to "http://[::1]",
        )
        origins.forEach { (origin, expected) ->
            assertEquals("$expected/?native-reader=1", NativeReaderHandoff.shellUrl(origin))
        }
        listOf(
            "file:///book.epub", "http:///missing-host", "http://viewer@plurx_b:32400",
            "http://plurx_b:bad", "http://plurx_b:0", "http://plurx_b:65536",
            "http://plurx_b:32400/other", "http://plurx_b:32400?token=secret",
            "http://plurx_b:32400#fragment", "http://a:b:32400",
        ).forEach { origin -> assertNull(origin, NativeReaderHandoff.shellUrl(origin)) }
    }

    @Test
    fun underscoreServerNavigationComparesAuthoritiesRatherThanMissingHosts() {
        val origin = "http://plurx_b:32400"
        assertTrue(NativeReaderHandoff.permitsNavigation("$origin/?native-reader=1", origin))
        assertTrue(NativeReaderHandoff.permitsNavigation("$origin/assets/reader.js", origin))
        assertTrue(NativeReaderHandoff.permitsNavigation(
            "HTTP://PLURX_B:32400/api/v1/publication/cap/chapter.xhtml", origin,
        ))
        listOf(
            "http://other_server:32400/assets/reader.js",
            "http://plurx_b:32401/assets/reader.js",
            "https://plurx_b:32400/assets/reader.js",
            "http://viewer@plurx_b:32400/assets/reader.js",
            "$origin/api/v1/items/9", "$origin/assets/../private",
            "$origin/assets/%2e%2e/private", "$origin/api/v1/publication/cap/%5cprivate",
            "http://a:b:32400/assets/reader.js",
        ).forEach { candidate ->
            assertFalse(candidate, NativeReaderHandoff.permitsNavigation(candidate, origin))
        }
    }

    @Test
    fun readerNavigationPreservesDefaultPortAndIpv6OriginEquivalence() {
        assertTrue(NativeReaderHandoff.permitsNavigation(
            "https://CINEMA.EXAMPLE:443/assets/reader.js", "https://cinema.example/",
        ))
        assertTrue(NativeReaderHandoff.permitsNavigation(
            "http://[::1]:80/assets/reader.js", "http://[::1]",
        ))
        assertTrue(NativeReaderHandoff.permitsNavigation(
            "https://[::1]:9443/api/v1/publication/cap/chapter.xhtml", "https://[::1]:9443",
        ))
        assertFalse(NativeReaderHandoff.permitsNavigation(
            "https://[::2]:9443/assets/reader.js", "https://[::1]:9443",
        ))
        assertFalse(NativeReaderHandoff.permitsNavigation(
            "https://cinema.example/assets/reader.js", "https://cinema.example/other",
        ))
        assertFalse(NativeReaderHandoff.permitsNavigation("http:/assets/reader.js", "http://null"))
    }

    @Test
    fun offlineReaderNeverPermitsPublisherNetworkNavigation() {
        assertTrue(OfflineBookWebPolicy.permitsNavigation(OfflineBookWebPolicy.SHELL))
        assertTrue(OfflineBookWebPolicy.permitsNavigation(
            "${OfflineBookWebPolicy.ORIGIN}/publication/Text/chapter.xhtml",
        ))
        assertEquals(
            "Text/a+b.xhtml",
            OfflineBookWebPolicy.publicationPath(
                "${OfflineBookWebPolicy.ORIGIN}/publication/Text/a+b.xhtml",
            ),
        )
        assertFalse(OfflineBookWebPolicy.permitsNavigation("https://publisher.invalid/track"))
        assertFalse(OfflineBookWebPolicy.permitsNavigation(
            "${OfflineBookWebPolicy.ORIGIN}/publication/../reader.js",
        ))
        assertFalse(OfflineBookWebPolicy.permitsNavigation(
            "${OfflineBookWebPolicy.ORIGIN}/offline-reader.html?remote=1",
        ))
    }

    @Test
    fun readerActionIsEpubOnlyAndNeverAppearsOnTelevision() {
        val handoff = ReaderCapability(
            format = "pdf",
            web = ReaderSurfaceCapability(online = "open_in"),
            apple = ReaderSurfaceCapability(online = "open_in"),
            android = ReaderSurfaceCapability(online = "open_in"),
        )
        val disguised = epub.copy(filename = "LooksLike.epub", reader = handoff)
        assertTrue(offersBookReader(FormFactor.Compact, epub))
        assertTrue(offersBookReader(FormFactor.Expanded, epub))
        assertFalse(offersBookReader(FormFactor.Television, epub))
        assertFalse(offersBookReader(FormFactor.Compact, epub.copy(filename = "Contract.pdf")))
        assertFalse(offersBookReader(FormFactor.Compact, epub.copy(available = false)))
        assertFalse(offersBookReader(FormFactor.Compact, disguised))
        assertFalse(disguised.supportsOfflineBookReader)
    }

    @Test
    fun bookFileSelectionPrefersTheRegisteredReaderOverListOrder() {
        val pdf = epub.copy(id = 91, filename = "Contract.pdf")
        assertEquals(epub, playbackFile(tv.plurx.app.data.Item(id = 1, kind = "book", title = "Contract"), listOf(pdf, epub), 0))
    }

    @Test
    fun actionLabelUsesReaderProgressAndExplicitCompletion() {
        val reading = ReadingState(
            file_id = epub.id,
            revision = ReadingRevision(size = 4096, mtime = 100),
            locator = ReadingLocator(version = 1, href = "Text/chapter.xhtml"),
            progression = 0.42,
            completed = false,
            updated_at = 200,
        )
        assertEquals("Resume reading · 42%", bookReadingLabel(reading, epub))
        assertEquals("Read again", bookReadingLabel(reading.copy(completed = true), epub))
        assertEquals("Read", bookReadingLabel(reading.copy(file_id = 91), epub))
    }
}
