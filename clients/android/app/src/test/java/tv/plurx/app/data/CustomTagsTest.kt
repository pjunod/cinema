package tv.plurx.app.data

import kotlinx.serialization.encodeToString
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class CustomTagsTest {
    private val json = Net.json

    private val body = """
        {"layout":[
          {"key":"row:12","kind":"row","row_id":12},
          {"key":"continue_watching","kind":"builtin","row_id":null},
          {"key":"coming_soon","kind":"builtin"},
          {"key":"mystery","kind":"builtin","extra":1},
          {"key":"row:99","kind":"row","row_id":99},
          {"key":"recently_added","kind":"builtin"}],
         "rows":{"12":{"title":"Gay-themed movies & shows","total":38,"future":true,
            "items":[{"id":5,"kind":"movie","title":"A","new_field":"x"}]}},
         "unknown_top":1}
    """.trimIndent()

    @Test
    fun decodesHomeRowsIgnoringUnknownKeys() {
        val rows = json.decodeFromString<HomeRows>(body)
        assertEquals(6, rows.layout.size)
        assertEquals(12L, rows.layout[0].row_id)
        assertEquals("Gay-themed movies & shows", rows.rows.getValue("12").title)
        assertEquals(38, rows.rows.getValue("12").total)
        assertEquals(5L, rows.rows.getValue("12").items.single().id)
    }

    @Test
    fun sectionsFollowLayoutAndHideAbsentBuiltIns() {
        val sections = homeSections(json.decodeFromString<HomeRows>(body))
        assertEquals(3, sections.size)
        assertTrue(sections[0] is HomeSection.CustomRow && (sections[0] as HomeSection.CustomRow).id == 12L)
        assertEquals(HomeSection.BuiltIn("continue_watching"), sections[1])
        assertEquals(HomeSection.BuiltIn("recently_added"), sections[2])
        // next_up, watchlist, favorites, libraries were not in layout: hidden.
        assertTrue(sections.none { it == HomeSection.BuiltIn("libraries") })
    }

    @Test
    fun missingHomeRowsFallsBackToDefaultOrder() {
        assertEquals(DEFAULT_HOME_BUILTINS, homeSections(null).map { (it as HomeSection.BuiltIn).key })
    }

    @Test
    fun emptyLayoutHidesEverything() {
        assertTrue(homeSections(HomeRows()).isEmpty())
    }

    @Test
    fun tagDecodesAndColoursMap() {
        val tags = json.decodeFromString<TagCatalog>(
            """{"tags":[{"id":1,"name":"a","scope":"private","color":"violet","editable":true,"zzz":1}],"colors":["violet"]}""",
        )
        assertTrue(tags.tags.single().isPrivate)
        assertEquals(0xFFA78BFAL, tagColorArgb("violet"))
        assertEquals(0xFF94A3B8L, tagColorArgb("nonsense"))
    }

    @Test
    fun putBodyHoldsOnlyEditableTickedIds() {
        val catalog = listOf(
            Tag(1, "a", editable = true),
            Tag(2, "b", editable = false),
            Tag(3, "c", editable = true),
            Tag(4, "old", editable = true, archived = true),
        )
        assertEquals(listOf(1L, 3L, 4L), tagIdsToSend(catalog, setOf(1, 2, 3, 4, 77)))
        assertEquals(listOf(1L), tagIdsToSend(catalog, setOf(1, 2)))
        assertEquals(listOf(1L, 3L), checklistTags(catalog).map { it.id }.filter { it != 4L })
        assertTrue(checklistTags(catalog).none { it.archived || !it.editable })
        assertEquals("""{"tag_ids":[1,3]}""", json.encodeToString(TagIdsBody(listOf(1, 3))))
        assertEquals("""{"name":"x","scope":"private"}""", json.encodeToString(TagCreateBody("x", "private")))
    }
}
