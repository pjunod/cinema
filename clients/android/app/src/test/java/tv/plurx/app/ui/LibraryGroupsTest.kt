package tv.plurx.app.ui

import java.time.Instant
import java.time.ZoneId
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.runTest
import org.junit.Assert.*
import org.junit.Test
import tv.plurx.app.data.Item
import tv.plurx.app.data.Page
import tv.plurx.app.data.LibraryPresentation

class LibraryGroupsTest {
    private fun item(id: Long, title: String = "Title") = Item(id = id, kind = "movie", title = title)

    @Test fun titleRowsUseServerKeysAndKeepEveryItemInPagerOrder() {
        val items = listOf(item(1, "The Apple"), item(2, "Zulu").copy(sort_title = "aardvark"),
            item(3, "Éclair"), item(4, "2001"), item(5, "Bee"))
        val groups = libraryGroups(items, "title")
        assertEquals(listOf("#", "A", "B"), groups.map { it.key })
        assertEquals(listOf(listOf(3L, 4L), listOf(1L, 2L), listOf(5L)), groups.map { it.items.map { it.id } })
        assertEquals(421, libraryGroups((1L..421).map { item(it, "Alpha") }, "title").single().items.size)
    }

    @Test fun metadataGroupsKeepUnknownsAndDescendingYears() {
        val items = listOf(item(1).copy(year = 2025, recorded_at = "2024-02-01", resolution = 1080),
            item(2).copy(year = 2026, recorded_at = "2026-01-01", resolution = 2160), item(3))
        assertEquals(listOf("year-2026", "year-2025", "unknown"), libraryGroups(items, "year").map { it.key })
        assertEquals(listOf("year-2026", "year-2024", "unknown"), libraryGroups(items, "recorded").map { it.key })
        assertEquals(listOf("4K", "1080p", "Unknown resolution"), libraryGroups(items, "resolution").map { it.label })
    }

    @Test fun addedPeriodsUseLocalCalendarBoundariesAndKeepMissingDates() {
        val now = Instant.parse("2026-03-10T16:00:00Z")
        val times = listOf("2026-03-11T00:00:00Z", "2026-03-10T05:00:00Z", "2026-03-04T05:00:00Z", "2026-03-02T12:00:00Z", "2026-02-12T12:00:00Z")
        val items = times.mapIndexed { index, time -> item(index.toLong()).copy(added_at = Instant.parse(time).epochSecond) } + item(99)
        assertEquals(listOf("future", "today", "week", "month", "added-2026-2", "unknown"),
            libraryGroups(items, "added", now, ZoneId.of("America/New_York")).map { it.key })
    }

    @Test fun rowDemandLoadsTheTailAndRetriesWithoutDuplicates() = runTest {
        val all = (1L..421).map { item(it, if (it <= 400) "Alpha $it" else "Zulu $it").copy(sort_title = if (it <= 400) "alpha" else "zulu") }
        var fail = true
        val offsets = mutableListOf<Int>()
        val pager = LibraryPager(listOf(1), "title", backgroundScope) { _, offset, _ ->
            offsets += offset
            if (offset == 200 && fail) { fail = false; error("controlled failure") }
            Page(all.drop(offset).take(200), total = all.size)
        }
        pager.ensure(40)
        pager.setDriveToCompletion(true)
        val partial = pager.state.first { it.error != null }
        assertFalse(partial.complete)
        assertEquals(200, partial.decided.size)
        pager.ensure(Int.MAX_VALUE)
        val complete = pager.state.value
        assertTrue(complete.complete)
        assertNull(complete.error)
        assertEquals(listOf(0, 200, 200, 400), offsets)
        assertEquals(listOf("A", "Z"), libraryGroups(complete.decided, "title").map { it.key })
        assertEquals(421, complete.decided.map { it.id }.toSet().size)
    }

    @Test fun presentationPreferenceDefaultsToRowsAndRestoresGrid() {
        assertEquals(LibraryPresentation.Rows, LibraryPresentation.fromStorage(null))
        assertEquals(LibraryPresentation.Grid, LibraryPresentation.fromStorage("grid"))
        assertEquals(LibraryPresentation.Rows, LibraryPresentation.fromStorage("future-format"))
    }
}
