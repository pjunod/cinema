package tv.plurx.app.livetv

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import tv.plurx.app.data.Net
import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.long

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

    @Test
    fun everySharedOverviewUsesRecorderAndClientFreshness() {
        val root = Net.json.parseToJsonElement(
            checkNotNull(javaClass.classLoader?.getResource("dvr-visibility-cases.json")) {
                "tests/playback/dvr-visibility-cases.json is not on the JVM test classpath"
            }.readText(),
        ).jsonObject
        val rows = root.getValue("overviews").jsonArray
        assertTrue(rows.isNotEmpty())
        rows.forEach { element ->
            val row = element.jsonObject
            val name = row.getValue("case").jsonPrimitive.content
            val overview = Net.json.decodeFromJsonElement<DvrOverview>(row.getValue("body"))
            val age = row.getValue("client_age_ms").jsonPrimitive.long
            assertEquals(name, row.getValue("fresh").jsonPrimitive.boolean, overview.isFresh(age))
            assertEquals(name, row["indicator"]?.jsonPrimitive?.contentOrNull, overview.indicatorText(age))
        }
    }
}
