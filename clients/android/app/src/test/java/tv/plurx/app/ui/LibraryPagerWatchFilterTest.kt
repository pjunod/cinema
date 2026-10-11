package tv.plurx.app.ui

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import tv.plurx.app.data.Item
import tv.plurx.app.data.Page
import tv.plurx.app.data.Watch

/** Actual pager/merge and watch predicate; only the page transport is local. */
@OptIn(ExperimentalCoroutinesApi::class)
class LibraryPagerWatchFilterTest {
    private fun items(): List<Item> = (1L..450L).map { id ->
        Item(id = id, kind = "movie", title = "Fixture $id", sort_title = "fixture %04d".format(id),
            watch = Watch(watched = id <= 200))
    }

    @Test fun watchFilterLoadsTheUnfetchedTailAndPublishesActualLoadedTotals() = runTest {
        val all = items()
        val secondStarted = CompletableDeferred<Unit>()
        val thirdStarted = CompletableDeferred<Unit>()
        val second = CompletableDeferred<Page>()
        val third = CompletableDeferred<Page>()
        val requests = mutableListOf<Int>()
        val pager = LibraryPager(listOf(7), "title", backgroundScope) { id, offset, order ->
            assertEquals(7L, id); assertEquals("title", order)
            requests += offset
            when (offset) {
                0 -> Page(all.take(200), total = 450)
                200 -> { secondStarted.complete(Unit); second.await() }
                400 -> { thirdStarted.complete(Unit); third.await() }
                else -> error("Unexpected page offset $offset")
            }
        }
        pager.ensure(40)
        val first = pager.state.value
        assertEquals(200, first.loadedCount); assertEquals(450, first.total)
        assertFalse(first.complete)
        assertTrue(first.decided.none { matchesFilter(it, WatchFilter.Unwatched) })
        assertEquals(listOf(0), requests)

        pager.setWatchFilter(WatchFilter.Unwatched)
        secondStarted.await()
        assertEquals(first, pager.state.value)
        second.complete(Page(all.drop(200).take(200), total = 450))
        thirdStarted.await()
        val partial = pager.state.value
        assertEquals(400, partial.loadedCount); assertEquals(450, partial.total)
        assertFalse(partial.complete)
        assertEquals(200, partial.decided.count { matchesFilter(it, WatchFilter.Unwatched) })
        third.complete(Page(all.drop(400), total = 450))
        val complete = pager.state.first { it.complete }
        assertEquals(450, complete.loadedCount); assertEquals(450, complete.total)
        assertEquals(250, complete.decided.count { matchesFilter(it, WatchFilter.Unwatched) })
        assertEquals(450L, complete.decided.filter { matchesFilter(it, WatchFilter.Unwatched) }.last().id)
        assertEquals((1L..450L).toList(), complete.decided.map { it.id })
        assertEquals(listOf(0, 200, 400), requests)
    }

    @Test fun clearingTheWatchFilterCancelsTheDriveWithoutDiscardingLoadedTitles() = runTest {
        val all = items()
        val started = CompletableDeferred<Unit>()
        val cancelled = CompletableDeferred<Unit>()
        val response = CompletableDeferred<Page>()
        val requests = mutableListOf<Int>()
        val pager = LibraryPager(listOf(7), "title", backgroundScope) { _, offset, _ ->
            requests += offset
            if (offset == 0) Page(all.take(200), total = 450) else {
                started.complete(Unit)
                try { response.await() } finally { cancelled.complete(Unit) }
            }
        }
        pager.ensure(40)
        val prefix = pager.state.value
        pager.setWatchFilter(WatchFilter.Watched)
        started.await()
        pager.setWatchFilter(WatchFilter.Everything)
        cancelled.await()
        pager.ensure(40)
        assertEquals(prefix, pager.state.value)
        assertEquals(listOf(0, 200), requests)
        assertEquals(200, pager.state.value.loadedCount)
        assertEquals(450, pager.state.value.total)
        assertFalse(pager.state.value.complete)
        assertEquals((1L..200L).toList(), pager.state.value.decided.map { it.id })
    }
}
