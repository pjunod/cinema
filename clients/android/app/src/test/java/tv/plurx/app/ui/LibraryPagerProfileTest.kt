package tv.plurx.app.ui

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Test
import tv.plurx.app.data.Item
import tv.plurx.app.data.Page

@OptIn(ExperimentalCoroutinesApi::class)
class LibraryPagerProfileTest {
    @Test fun anInvalidatedPagerCannotPublishAnOldProfilesLatePage() = runTest {
        val response = CompletableDeferred<Page>()
        val pager = LibraryPager(listOf(7), "title", backgroundScope) { _, _, _ -> response.await() }
        backgroundScope.launch { pager.ensure(1) }
        runCurrent()

        pager.invalidate()
        response.complete(Page(items = listOf(Item(id = 42, kind = "movie", title = "Private")), total = 1))
        runCurrent()

        assertEquals(LibraryGridState(), pager.state.value)
    }
}
