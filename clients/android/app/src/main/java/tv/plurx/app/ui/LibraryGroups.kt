package tv.plurx.app.ui

import java.time.Instant
import java.time.ZoneId
import java.time.ZonedDateTime
import java.time.format.DateTimeFormatter
import java.util.Locale
import tv.plurx.app.data.Item

internal data class LibraryGroup(val key: String, val label: String, val order: Int, val items: List<Item> = emptyList())

/** Group the filtered snapshot without changing the pager's item order. */
internal fun libraryGroups(items: List<Item>, sort: String, now: Instant = Instant.now(), zone: ZoneId = ZoneId.systemDefault()): List<LibraryGroup> {
    val buckets = linkedMapOf<String, LibraryGroup>()
    val members = linkedMapOf<String, MutableList<Item>>()
    val monthLabels = mutableMapOf<String, String>()
    val formatter by lazy { DateTimeFormatter.ofPattern("MMMM yyyy") }
    for (item in items) {
        val group = libraryGroup(item, sort, now, zone) { date ->
            monthLabels.getOrPut("${date.year}-${date.monthValue}") { date.format(formatter) }
        }
        buckets.putIfAbsent(group.key, group)
        members.getOrPut(group.key) { mutableListOf() }.add(item)
    }
    return buckets.values.sortedBy { it.order }.map { it.copy(items = members.getValue(it.key)) }
}

private fun libraryGroup(item: Item, sort: String, now: Instant, zone: ZoneId, monthLabel: (ZonedDateTime) -> String): LibraryGroup {
    fun group(key: String, label: String, order: Int) = LibraryGroup(key, label, order)
    return when (sort) {
        "title" -> {
            val lower = item.title.lowercase(Locale.ROOT)
            val key = item.sort_title ?: listOf("the ", "an ", "a ")
                .firstOrNull { lower.startsWith(it) && lower.length > it.length }?.let { lower.removePrefix(it) } ?: lower
            val first = key.firstOrNull()
            val letter = if (first in 'a'..'z' || first in 'A'..'Z') first!!.uppercaseChar().toString() else "#"
            group(letter, letter, if (letter == "#") 0 else letter[0].code)
        }
        "year", "recorded" -> {
            val year = if (sort == "year") item.year else item.recorded_at?.take(4)?.toIntOrNull()
            if (year != null && year > 0) group("year-$year", year.toString(), -year)
            else group("unknown", if (sort == "year") "Unknown year" else "Unknown recording date", Int.MAX_VALUE)
        }
        "resolution" -> {
            val height = item.resolution ?: 0
            val index = listOf(1700L, 1300L, 900L, 650L, 400L, 1L).indexOfFirst { height >= it }.let { if (it < 0) 6 else it }
            group("res-$index", listOf("4K", "1440p", "1080p", "720p", "480p", "SD", "Unknown resolution")[index], index)
        }
        else -> {
            val date = item.added_at?.let { runCatching { Instant.ofEpochSecond(it).atZone(zone) }.getOrNull() }
                ?: return group("unknown", "Unknown date added", Int.MAX_VALUE)
            val current = now.atZone(zone)
            val today = current.toLocalDate().atStartOfDay(zone)
            val week = today.minusDays(6)
            val month = today.withDayOfMonth(1)
            when {
                date.toInstant() > now -> group("future", "Future dates", -1)
                date >= today -> group("today", "Today", 0)
                date >= week -> group("week", "Previous 6 days", 1)
                date >= month -> group("month", "Earlier this month", 2)
                else -> group("added-${date.year}-${date.monthValue}", monthLabel(date),
                    3 + (current.year - date.year) * 12 + current.monthValue - date.monthValue)
            }
        }
    }
}
