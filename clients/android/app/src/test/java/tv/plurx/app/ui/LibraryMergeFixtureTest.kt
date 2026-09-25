package tv.plurx.app.ui

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.int
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.longOrNull
import kotlinx.serialization.json.content
import kotlinx.serialization.json.contentOrNull
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import tv.plurx.app.data.Item

class LibraryMergeFixtureTest {
    @Test
    fun sharedServerOrderAcrossOneTwoAndThreePagedLibraries() {
        val text = checkNotNull(javaClass.classLoader?.getResource("library-sort-cases.json")) .readText()
        val fixture = Json.parseToJsonElement(text).jsonObject
        val items = fixture.getValue("items").jsonArray.map { element ->
            val row = element.jsonObject
            val id = row.getValue("index").jsonPrimitive.int
            Item(
                id = id.toLong(),
                kind = row["kind"]?.jsonPrimitive?.contentOrNull ?: "movie",
                title = row.getValue("title").jsonPrimitive.content,
                sort_title = row.getValue("sort_title").jsonPrimitive.content,
                year = row["year"]?.jsonPrimitive?.intOrNull,
                recorded_at = row["recorded_at"]?.jsonPrimitive?.contentOrNull,
                resolution = row["resolution"]?.jsonPrimitive?.longOrNull,
                added_at = id.toLong(),
            )
        }
        val expected = fixture.getValue("expected_order").jsonObject
        for (sort in listOf("title", "added", "year", "resolution", "recorded")) {
            val order = expected.getValue(sort).jsonArray.map { it.jsonPrimitive.int.toLong() }
            for (count in 1..3) {
                val cursors = (0 until count).map { cursor ->
                    items.filter { (it.id - 1) % count == cursor.toLong() }
                        .sortedWith { a, b -> LibraryMerge.compare(a, b, sort) }
                }
                val merge = LibraryMerge((0 until count).map { it.toLong() }, sort)
                var requests = 0
                while (merge.nextRequest != null) {
                    val (id, offset) = merge.nextRequest!!
                    val page = cursors[id.toInt()].drop(offset).take(7)
                    merge.receive(id, page, cursors[id.toInt()].size, 7)
                    requests++
                    assertTrue(requests < 50)
                }
                assertTrue(merge.complete)
                assertEquals("$sort across $count cursors", order, merge.decided.map { it.id })
            }
        }
    }

    @Test
    fun firstPaintRequestsOnePagePerLibrary() {
        val items = (1L..600L).map { Item(id = it, kind = "movie", title = "Title $it", sort_title = "title %04d".format(it)) }
        val cursors = (0..2).map { cursor -> items.filter { (it.id - 1) % 3 == cursor.toLong() } }
        val merge = LibraryMerge(listOf(0, 1, 2), "title")
        val requests = intArrayOf(0, 0, 0)
        while (merge.decided.size < 40 && merge.nextRequest != null) {
            val (id, offset) = merge.nextRequest!!
            val page = cursors[id.toInt()].drop(offset).take(200)
            requests[id.toInt()]++
            merge.receive(id, page, cursors[id.toInt()].size)
        }
        assertEquals(listOf(1, 1, 1), requests.toList())
        assertTrue(merge.decided.size >= 40)
    }
}
