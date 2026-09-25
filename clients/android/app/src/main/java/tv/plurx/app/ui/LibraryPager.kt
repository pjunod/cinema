package tv.plurx.app.ui

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import tv.plurx.app.data.Item
import tv.plurx.app.data.Page

internal data class LibraryGridState(
    val decided: List<Item> = emptyList(),
    val loadedCount: Int = 0,
    val total: Int = 0,
    val complete: Boolean = false,
    val error: String? = null,
)

/** Owned by AppViewModel so a configuration change retains loaded pages. */
internal class LibraryPager(
    val ids: List<Long>,
    val sort: String,
    private val scope: CoroutineScope,
    private val fetch: suspend (Long, Int, String) -> Page,
) {
    private val merge = LibraryMerge(ids, sort)
    private val mutex = Mutex()
    private val mutable = MutableStateFlow(LibraryGridState())
    val state: StateFlow<LibraryGridState> = mutable
    private var drive: Job? = null

    suspend fun ensure(visibleThrough: Int) = mutex.withLock {
        try {
            while (merge.decided.size < visibleThrough && !merge.complete) {
                val (id, offset) = merge.nextRequest ?: break
                val page = fetch(id, offset, sort)
                merge.receive(id, page.items, page.total)
                mutable.value = LibraryGridState(
                    decided = merge.decided.toList(),
                    loadedCount = merge.loadedCount,
                    total = merge.total,
                    complete = merge.complete,
                )
                if (merge.missingSortKey) {
                    loadLegacyWholeCollection()
                    return@withLock
                }
            }
            if (ids.isEmpty()) mutable.value = LibraryGridState(complete = true)
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (e: Exception) {
            mutable.value = mutable.value.copy(error = e.message ?: "Couldn't load this library")
        }
    }

    fun setDriveToCompletion(enabled: Boolean) {
        drive?.cancel()
        drive = if (enabled) scope.launch { ensure(Int.MAX_VALUE) } else null
    }

    private suspend fun loadLegacyWholeCollection() {
        val all = mutableListOf<Item>()
        for (id in ids) {
            var offset = 0
            while (true) {
                val page = fetch(id, offset, sort)
                all += page.items
                offset += page.items.size
                if (page.items.isEmpty() || offset >= page.total || page.items.size < 200) break
            }
        }
        // An old server has no sort_title; preserve its full-walk behaviour.
        val ordered = all.map { it.copy(sort_title = legacySortTitle(it.title)) }
            .sortedWith { a, b -> LibraryMerge.compare(a, b, sort) }
        mutable.value = LibraryGridState(ordered, ordered.size, ordered.size, complete = true)
    }
}

private fun legacySortTitle(title: String): String {
    val lower = title.lowercase()
    return listOf("the ", "an ", "a ").firstOrNull { lower.startsWith(it) && lower.length > it.length }
        ?.let { lower.removePrefix(it) } ?: lower
}
