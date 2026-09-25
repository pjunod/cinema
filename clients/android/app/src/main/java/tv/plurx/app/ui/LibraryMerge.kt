package tv.plurx.app.ui

import tv.plurx.app.data.Item

/** Server order across paged, individually sorted library cursors. */
internal class LibraryMerge(ids: List<Long>, private val sort: String) {
    private data class Cursor(
        val id: Long,
        var offset: Int = 0,
        var total: Int = 0,
        val buffer: ArrayDeque<Item> = ArrayDeque(),
        var exhausted: Boolean = false,
    )

    private val cursors = ids.map(::Cursor)
    val decided = mutableListOf<Item>()
    var missingSortKey = false
        private set
    val loadedCount get() = cursors.sumOf { it.offset }
    val total get() = cursors.sumOf { it.total }
    val complete get() = cursors.all { it.exhausted && it.buffer.isEmpty() }
    val nextRequest: Pair<Long, Int>?
        get() = cursors.firstOrNull { !it.exhausted && it.buffer.isEmpty() }?.let { it.id to it.offset }

    fun receive(id: Long, items: List<Item>, total: Int, limit: Int = 200) {
        val cursor = cursors.first { it.id == id }
        missingSortKey = missingSortKey || items.any { it.sort_title == null }
        cursor.buffer.addAll(items)
        cursor.offset += items.size
        cursor.total = total
        cursor.exhausted = items.isEmpty() || cursor.offset >= total || items.size < limit
        drainDecidable()
    }

    private fun drainDecidable() {
        while (nextRequest == null) {
            val heads = cursors.filter { it.buffer.isNotEmpty() }
            if (heads.isEmpty()) return
            val selected = heads.minWithOrNull { a, b -> compare(a.buffer.first(), b.buffer.first(), sort) }!!
            decided += selected.buffer.removeFirst()
        }
    }

    companion object {
        fun compare(a: Item, b: Item, sort: String): Int {
            fun title(): Int = compareUtf8(a.sort_title.orEmpty(), b.sort_title.orEmpty())
            fun <T : Comparable<T>> descending(left: T?, right: T?): Int =
                when {
                    left == right -> 0
                    left == null -> 1
                    right == null -> -1
                    else -> right.compareTo(left)
                }
            val primary = when (sort) {
                "added" -> descending(a.added_at, b.added_at)
                "year" -> descending(a.year, b.year).takeIf { it != 0 } ?: title()
                "resolution" -> descending(a.resolution ?: -1, b.resolution ?: -1).takeIf { it != 0 } ?: title()
                "recorded" -> descending(a.recorded_at, b.recorded_at).takeIf { it != 0 } ?: title()
                else -> title()
            }
            if (primary != 0) return primary
            return if (sort == "added") b.id.compareTo(a.id) else a.id.compareTo(b.id)
        }

        private fun compareUtf8(a: String, b: String): Int {
            val left = a.toByteArray(Charsets.UTF_8)
            val right = b.toByteArray(Charsets.UTF_8)
            for (i in 0 until minOf(left.size, right.size)) {
                val comparison = (left[i].toInt() and 0xff).compareTo(right[i].toInt() and 0xff)
                if (comparison != 0) return comparison
            }
            return left.size.compareTo(right.size)
        }
    }
}
