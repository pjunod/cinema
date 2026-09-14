package tv.plurx.app.livetv

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import tv.plurx.app.data.Net

class DvrVisibilityTest {
    @Test
    fun recordingsUseThePagedServerEnvelope() {
        val page = Net.json.decodeFromString<DvrRecordingsPage>(
            """{"rows":[],"next":"older-page"}""",
        )
        assertTrue(page.rows.isEmpty())
        assertEquals("older-page", page.next)
        assertTrue(runCatching { Net.json.decodeFromString<DvrRecordingsPage>("[]") }.isFailure)
    }
}

