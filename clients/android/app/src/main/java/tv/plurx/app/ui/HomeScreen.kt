package tv.plurx.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBars
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.filled.Download
import androidx.compose.material.icons.filled.LiveTv
import androidx.compose.material.icons.filled.VideoLibrary
import androidx.compose.material.icons.filled.RadioButtonChecked
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusProperties
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import tv.plurx.app.data.HomeGrouping
import tv.plurx.app.data.Item
import tv.plurx.app.data.Library
import tv.plurx.app.data.ThemeId
import tv.plurx.app.ui.components.ChoicePicker
import tv.plurx.app.ui.components.LoadingBox
import tv.plurx.app.ui.components.MediaFactChip
import tv.plurx.app.ui.components.MediaRow
import tv.plurx.app.ui.components.NetworkImage
import tv.plurx.app.ui.components.PosterResolutionPlacement
import tv.plurx.app.ui.components.RequestInitialFocus
import tv.plurx.app.ui.components.safeDisplayInsets
import tv.plurx.app.ui.components.TvTextButton
import tv.plurx.app.livetv.DvrController
import tv.plurx.app.ui.components.TvIconButton
import tv.plurx.app.ui.components.imageUrl
import tv.plurx.app.ui.components.itemResolutionFact
import tv.plurx.app.ui.theme.Accent
import tv.plurx.app.ui.theme.Muted

/** The "Group by" picker's place in the vertical D-pad chain. */
private const val GROUPING_KEY = "grouping"

private data class HomeCollection(
    val title: String,
    val libraries: List<Library>,
    val items: List<Item>,
)

@Composable
fun HomeScreen(
    vm: AppViewModel,
    dvrController: DvrController? = null,
    onOpenItem: (Long) -> Unit,
    onOpenCollection: (title: String, libraries: List<Library>) -> Unit,
    onSearch: () -> Unit,
    onOpenDownloads: () -> Unit,
    onOpenSettings: () -> Unit,
    onOpenLiveTv: () -> Unit = {},
    onOpenRecordings: () -> Unit = {},
    onOpenRecording: (String) -> Unit = {},
    onOpenLibraryChannels: () -> Unit = {},
) {
    val state by vm.home.collectAsStateWithLifecycle()
    val preferences by vm.preferences.collectAsStateWithLifecycle()
    val formFactor = currentFormFactor()
    val side = formFactor.horizontalPadding()
    val posterWidth = (preferences.posterSize.widthDp * formFactor.posterScale()).dp

    val recordings = dvrController?.state?.collectAsStateWithLifecycle()?.value?.library.orEmpty()
    LaunchedEffect(dvrController) { dvrController?.refreshLibrary() }
    val recordingLibraries = state.libraries.filter { it.kind == "recordings" }.map { it.id }.toSet()
    val recent = state.hubs.recently_added.filter { it.library_id !in recordingLibraries }
    val continuing = state.hubs.continue_watching
    var showAll by rememberSaveable { mutableStateOf(false) }
    val firstFocus = remember { FocusRequester() }
    RequestInitialFocus(firstFocus, enabled = state.hasContent && (recent.isNotEmpty() || continuing.isNotEmpty()))
    Column(Modifier.fillMaxSize().navigationBarsPadding()) {
        HomeTopBar(theme = preferences.theme, username = vm.username, formFactor = formFactor, side = side,
            onRefresh = vm::loadHome, onSearch = onSearch, onOpenDownloads = onOpenDownloads,
            onOpenSettings = onOpenSettings, onOpenLiveTv = onOpenLiveTv,
            onOpenRecordings = onOpenRecordings, onOpenLibraryChannels = onOpenLibraryChannels)
        if (!state.hasContent && state.loading) { LoadingBox() }
        else {
            Column(Modifier.verticalScroll(rememberScrollState()).padding(bottom = 32.dp), verticalArrangement = Arrangement.spacedBy(20.dp)) {
                MediaRow("Recently added", recent, posterWidth,
                    resolutionPlacement = PosterResolutionPlacement.BelowArtwork,
                    rowFocusRequester = if (recent.isNotEmpty()) firstFocus else null,
                    onOpen = { onOpenItem(it.id) })
                if (continuing.isNotEmpty()) {
                    Column(Modifier.padding(horizontal = side), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        Text("Continue watching", style = MaterialTheme.typography.titleMedium)
                        continuing.take(if (showAll) continuing.size else 3).forEachIndexed { index, item ->
                            TvTextButton(onClick = { onOpenItem(item.id) }, modifier = Modifier.fillMaxWidth().then(if (recent.isEmpty() && index == 0) Modifier.focusRequester(firstFocus) else Modifier)) {
                                NetworkImage(imageUrl(item.poster ?: item.backdrop), Modifier.width(48.dp).height(60.dp).clip(MaterialTheme.shapes.small))
                                Spacer(Modifier.width(12.dp))
                                Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                                    Text(item.show_title ?: item.title, style = MaterialTheme.typography.titleSmall)
                                    Text(listOfNotNull(item.season_number?.let { "S$it" }, item.episode_number?.let { "E$it" }, if (item.kind == "episode") item.title else null).joinToString(" · "), color = Muted, style = MaterialTheme.typography.bodySmall)
                                    LinearProgressIndicator(progress = { compactProgress(item) }, modifier = Modifier.fillMaxWidth(.6f))
                                }
                                Icon(Icons.Default.PlayArrow, contentDescription = "Continue")
                            }
                        }
                        if (continuing.size > 3) TvTextButton(onClick = { showAll = !showAll }) { Text(if (showAll) "Show fewer" else "More in progress (${continuing.size - 3})") }
                    }
                }
                if (recordings.isNotEmpty()) {
                    Column(Modifier.padding(horizontal = side), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        TvTextButton(onClick = onOpenRecordings) { Text("Recently recorded", style = MaterialTheme.typography.titleMedium) }
                        recordings.take(3).forEach { recording ->
                            TvTextButton(onClick = { onOpenRecording(recording.id) }, modifier = Modifier.fillMaxWidth()) {
                                Column(Modifier.weight(1f)) {
                                    Text(recording.title, style = MaterialTheme.typography.titleSmall)
                                    Text(listOfNotNull(recording.channel_name, recording.guide_number).joinToString(" · "), color = Muted)
                                }
                                Icon(Icons.Default.PlayArrow, contentDescription = "Recording details")
                            }
                        }
                    }
                }
                if (state.error != null) Text(state.error!!, color = Muted, modifier = Modifier.padding(horizontal = side))
                if (recent.isEmpty() && continuing.isEmpty() && recordings.isEmpty()) Text("Your library additions and recordings will appear here.", color = Muted, modifier = Modifier.padding(horizontal = side))
                val collections = homeCollections(state.libraries, state.libraryItems, preferences.homeGrouping)
                Column(Modifier.padding(horizontal = side)) {
                    Text("Libraries", style = MaterialTheme.typography.titleMedium)
                    collections.forEach { collection -> TvTextButton(onClick = { onOpenCollection(collection.title, collection.libraries) }) { Text(collection.title) } }
                }
            }
        }
    }
}

internal fun continueWatchingShelfItems(items: List<Item>, formFactor: FormFactor): List<Item> =
    items

internal fun compactRuntimeLabel(milliseconds: Long): String {
    val totalMinutes = milliseconds / 60_000
    val hours = totalMinutes / 60
    val minutes = totalMinutes % 60
    return if (hours > 0) "${hours}h ${minutes}m" else "${minutes}m"
}

private fun compactHomeFacts(item: Item): List<String> = buildList {
    item.year?.let { add(it.toString()) }
    item.runtime_ms?.takeIf { it > 0 }?.let { add(compactRuntimeLabel(it)) }
}

private fun compactEpisodeSubtitle(item: Item): String = buildList {
    if (item.season_number != null && item.episode_number != null) {
        add("S${item.season_number} E${item.episode_number}")
    }
    add(item.title)
}.joinToString("  ")

private fun compactProgress(item: Item): Float {
    val watch = item.watch ?: return 0f
    val position = watch.position_ms
    val duration = watch.duration_ms ?: item.runtime_ms ?: return 0f
    if (duration <= 0) return 0f
    return (position.toFloat() / duration).coerceIn(0f, 1f)
}

private fun compactTimeRemaining(item: Item): String? {
    val watch = item.watch ?: return null
    val position = watch.position_ms
    val duration = watch.duration_ms ?: item.runtime_ms ?: return null
    if (duration <= position) return null
    return "${maxOf(1, (duration - position) / 60_000)}m left"
}

@Composable
internal fun HomeTopBar(
    theme: ThemeId,
    username: String?,
    formFactor: FormFactor,
    side: Dp,
    onRefresh: () -> Unit,
    onSearch: () -> Unit,
    onOpenDownloads: () -> Unit = {},
    onOpenSettings: () -> Unit,
    safeInsets: WindowInsets = safeDisplayInsets(),
    onOpenLiveTv: () -> Unit = {},
    onOpenRecordings: () -> Unit = {},
    onOpenLibraryChannels: () -> Unit = {},
) {
    val brand: @Composable () -> Unit = {
        Text(
            when (theme) {
                ThemeId.Classic -> "cinema"
                ThemeId.Terminal -> ":~\$ cinema ▊"
                ThemeId.Noirr -> "noirr ▬"
            },
            color = Accent,
            fontSize = if (formFactor == FormFactor.Television) 32.sp else 26.sp,
            fontWeight = FontWeight.Bold,
        )
    }
    val user: @Composable () -> Unit = {
        username?.let {
            Text(it, color = Muted, style = MaterialTheme.typography.labelMedium, modifier = Modifier.padding(end = 4.dp))
        }
    }
    val actions: @Composable () -> Unit = {
        TvIconButton(onClick = onRefresh) {
            Icon(Icons.Filled.Refresh, contentDescription = "Refresh", tint = Muted)
        }
        TvIconButton(onClick = onSearch) {
            Icon(Icons.Filled.Search, contentDescription = "Search", tint = Muted)
        }
        TvIconButton(onClick = onOpenLiveTv) {
            Icon(Icons.Filled.LiveTv, contentDescription = "Live TV", tint = Muted)
        }
        TvIconButton(onClick = onOpenRecordings) {
            Icon(Icons.Filled.RadioButtonChecked, contentDescription = "Recordings", tint = Muted)
        }
        TvIconButton(onClick = onOpenLibraryChannels) {
            Icon(Icons.Filled.VideoLibrary, contentDescription = "Library channels", tint = Muted)
        }
        if (formFactor != FormFactor.Television) {
            TvIconButton(onClick = onOpenDownloads) {
                Icon(Icons.Filled.Download, contentDescription = "Downloads", tint = Muted)
            }
        }
        TvIconButton(onClick = onOpenSettings) {
            Icon(Icons.Filled.Settings, contentDescription = "Settings", tint = Muted)
        }
    }
    val chrome = Modifier.fillMaxWidth().windowInsetsPadding(safeInsets)
        .padding(start = side, end = side - 8.dp, top = 14.dp, bottom = 4.dp)
    if (formFactor == FormFactor.Compact) {
        // Five actions need their own row on narrow phones; the title and
        // account must not push the last controls outside the viewport.
        Column(chrome) {
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                brand(); Box(Modifier.weight(1f)); user()
            }
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) { actions() }
        }
    } else {
        Row(chrome, verticalAlignment = Alignment.CenterVertically) {
            brand(); Box(Modifier.weight(1f)); user(); actions()
        }
    }
}

private fun homeCollections(
    libraries: List<Library>,
    items: Map<Long, List<Item>>,
    grouping: HomeGrouping,
): List<HomeCollection> {
    if (grouping == HomeGrouping.Library) {
        return libraries.map { lib -> HomeCollection(lib.name, listOf(lib), items[lib.id].orEmpty()) }
    }
    return libraries
        .groupBy { it.kind }
        .entries
        .sortedWith(compareBy({ libraryKindOrder(it.key) }, { it.key }))
        .map { (kind, libs) ->
            val title = when (kind) {
                "movie", "movies" -> "Movies"
                "show", "shows" -> "TV shows"
                "book", "books" -> "Books"
                "home" -> "Home videos"
                else -> kind.replaceFirstChar { it.uppercase() }
            }
            HomeCollection(
                title = title,
                libraries = libs,
                items = libs.flatMap { items[it.id].orEmpty() }.distinctBy { it.id }.take(24),
            )
        }
}

private fun libraryKindOrder(kind: String): Int = when (kind) {
    "movie", "movies" -> 0
    "show", "shows" -> 1
    "book", "books" -> 2
    "home" -> 3
    else -> Int.MAX_VALUE
}
