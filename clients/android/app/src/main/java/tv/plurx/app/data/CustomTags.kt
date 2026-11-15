package tv.plurx.app.data

import kotlinx.serialization.Serializable

/**
 * Custom tags (docs/features/CUSTOM-TAGS-CLIENTS.md). Every field has a
 * default and unknown keys are ignored (`Net.json`), so an older or newer
 * server still decodes.
 */
@Serializable
data class Tag(
    val id: Long,
    val name: String,
    /** `household` or `private`. */
    val scope: String = "household",
    val color: String = "slate",
    val suggest_terms: List<String> = emptyList(),
    val archived: Boolean = false,
    val item_count: Long = 0,
    val editable: Boolean = false,
) {
    val isPrivate get() = scope == "private"
}

@Serializable
data class ItemTags(val tags: List<Tag> = emptyList())

@Serializable
data class TagCatalog(
    val tags: List<Tag> = emptyList(),
    val can_manage_household: Boolean = false,
    val colors: List<String> = emptyList(),
)

@Serializable
data class TagIdsBody(val tag_ids: List<Long>)

/** `scope` has no default on purpose: defaults are not encoded and the server needs it. */
@Serializable
data class TagCreateBody(val name: String, val scope: String)

@Serializable
data class HomeLayoutEntry(
    val key: String,
    val kind: String = "builtin",
    val row_id: Long? = null,
)

@Serializable
data class HomeRow(
    val title: String = "",
    val total: Int = 0,
    val items: List<Item> = emptyList(),
)

/** `GET home/rows`. */
@Serializable
data class HomeRows(
    val layout: List<HomeLayoutEntry> = emptyList(),
    val rows: Map<String, HomeRow> = emptyMap(),
)

/** One Home section, in drawing order. */
sealed interface HomeSection {
    data class BuiltIn(val key: String) : HomeSection
    data class CustomRow(val id: Long, val row: HomeRow) : HomeSection
}

/** The built-ins the app draws, in the order it drew them before custom rows existed. */
val DEFAULT_HOME_BUILTINS = listOf(
    "continue_watching", "next_up", "watchlist", "favorites", "recently_added", "libraries",
)

/**
 * The Home sections to draw. No `home/rows` answer (404, error, not yet
 * arrived) gives exactly today's order. Otherwise the reader's `layout`
 * decides: built-ins the app does not draw (`coming_soon`, unknown keys) are
 * ignored, built-ins missing from the layout are hidden, and a row entry with
 * no matching `rows` object is dropped.
 */
fun homeSections(homeRows: HomeRows?): List<HomeSection> {
    if (homeRows == null) return DEFAULT_HOME_BUILTINS.map { HomeSection.BuiltIn(it) }
    val seen = HashSet<String>()
    return homeRows.layout.mapNotNull { entry ->
        if (!seen.add(entry.key)) return@mapNotNull null
        when {
            entry.key.startsWith("row:") || entry.kind == "row" -> {
                val id = entry.row_id ?: entry.key.removePrefix("row:").toLongOrNull()
                    ?: return@mapNotNull null
                homeRows.rows[id.toString()]?.let { HomeSection.CustomRow(id, it) }
            }
            entry.key in DEFAULT_HOME_BUILTINS -> HomeSection.BuiltIn(entry.key)
            else -> null
        }
    }
}

/** Colour name to ARGB, per the contract. Unknown names read as slate. */
fun tagColorArgb(name: String): Long = when (name) {
    "violet" -> 0xFFA78BFA
    "blue" -> 0xFF60A5FA
    "teal" -> 0xFF2DD4BF
    "green" -> 0xFF4ADE80
    "lime" -> 0xFFA3E635
    "amber" -> 0xFFFBBF24
    "orange" -> 0xFFFB923C
    "red" -> 0xFFF87171
    "pink" -> 0xFFF472B6
    else -> 0xFF94A3B8
}

/** Tags the checklist offers: editable and not archived. */
fun checklistTags(catalog: List<Tag>): List<Tag> = catalog.filter { it.editable && !it.archived }

/**
 * The PUT body: every ticked tag the reader may edit. Ids outside the catalog's
 * editable set are never sent (the server leaves those alone); an archived
 * editable tag already on the title stays ticked so saving does not strip it.
 */
fun tagIdsToSend(catalog: List<Tag>, ticked: Set<Long>): List<Long> =
    catalog.filter { it.editable && it.id in ticked }.map { it.id }.sorted()
