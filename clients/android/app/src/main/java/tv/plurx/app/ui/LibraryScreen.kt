package tv.plurx.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items as rowItems
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material3.TextButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import kotlinx.coroutines.launch
import tv.plurx.app.data.LibraryPresentation
import tv.plurx.app.ui.components.MediaRow
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.lazy.grid.rememberLazyGridState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.snapshotFlow
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.debounce
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.mapLatest
import kotlinx.coroutines.withContext
import tv.plurx.app.data.Item
import tv.plurx.app.data.ViewerPreferences
import tv.plurx.app.ui.components.ChoicePicker
import tv.plurx.app.ui.components.RequestInitialFocus
import tv.plurx.app.ui.components.LoadingBox
import tv.plurx.app.ui.components.PosterCard
import tv.plurx.app.ui.components.SafeTopRow
import tv.plurx.app.ui.components.TvIconButton
import tv.plurx.app.ui.theme.Muted

internal enum class WatchFilter(val label: String) {
    Everything("Everything"), Unwatched("Unwatched"), InProgress("In progress"), Watched("Watched")
}

@Composable
fun LibraryScreen(
    vm: AppViewModel,
    libraryIds: List<Long>,
    title: String,
    onOpenItem: (Long) -> Unit,
    onBack: () -> Unit,
) {
    val preferences by vm.preferences.collectAsStateWithLifecycle()
    val home by vm.home.collectAsStateWithLifecycle()
    val kind = home.libraries.firstOrNull { it.id in libraryIds }?.kind
    val pagerFactory = remember(vm, libraryIds) { { order: String -> vm.libraryPager(libraryIds, order) } }
    LibraryScreen(libraryIds, title, preferences, kind, pagerFactory, onOpenItem, onBack, vm::setLibraryPresentation)
}

/** The production grid with its account-bound dependencies supplied by the
 * public view-model route. Local loaders can exercise this same focus/paging
 * composition without constructing a view model that reads saved accounts. */
@OptIn(kotlinx.coroutines.FlowPreview::class, kotlinx.coroutines.ExperimentalCoroutinesApi::class)
@Composable
internal fun LibraryScreen(
    libraryIds: List<Long>,
    title: String,
    preferences: ViewerPreferences,
    kind: String?,
    pagerFactory: (String) -> LibraryPager,
    onOpenItem: (Long) -> Unit,
    onBack: () -> Unit,
    onPresentationChange: (LibraryPresentation) -> Unit = {},
) {
    val formFactor = currentFormFactor()
    val side = formFactor.horizontalPadding()
    // Retained across configuration changes and process death: a rotation, or
    // coming back from a two-hour film, should not silently reset the grid the
    // viewer set up.
    var sort by rememberSaveable(libraryIds) { mutableStateOf(if (kind == "home") "recorded" else "title") }
    var filter by rememberSaveable(libraryIds) { mutableStateOf(WatchFilter.Everything) }
    val pager = remember(pagerFactory, libraryIds, sort) { pagerFactory(sort) }
    val load by pager.state.collectAsStateWithLifecycle()
    val gridState = rememberLazyGridState()
    val rowsState = rememberLazyListState()
    val scope = rememberCoroutineScope()
    var presentation by rememberSaveable { mutableStateOf(preferences.libraryPresentation) }
    var query by rememberSaveable(libraryIds) { mutableStateOf("") }
    var expandedGroup by rememberSaveable(libraryIds, sort, filter, query) { mutableStateOf<String?>(null) }
    var jumpGroup by rememberSaveable(libraryIds, sort) { mutableStateOf("") }
    var groups by remember(pager) { mutableStateOf<List<LibraryGroup>>(emptyList()) }
    val rows = presentation == LibraryPresentation.Rows
    LaunchedEffect(preferences.libraryPresentation) { presentation = preferences.libraryPresentation }
    var shown by remember(pager) { mutableStateOf<List<Item>>(emptyList()) }
    LaunchedEffect(pager) { pager.ensure(40) }
    LaunchedEffect(pager, gridState, rows) {
        if (rows) return@LaunchedEffect
        snapshotFlow {
            val layout = gridState.layoutInfo
            libraryPrefetchExclusive(layout.visibleItemsInfo.maxOfOrNull { it.index } ?: -1, layout.maxSpan)
        }.distinctUntilChanged().collectLatest { pager.ensure(it) }
    }
    LaunchedEffect(pager, filter, query, rows) {
        pager.setDriveToCompletion(rows || filter != WatchFilter.Everything || query.isNotBlank())
    }
    DisposableEffect(pager) { onDispose { pager.setDriveToCompletion(false) } }
    LaunchedEffect(pager, filter, query) {
        combine(pager.state, snapshotFlow { filter to query }.debounce(150)) { state, selected -> state.decided to selected }
            .mapLatest { (snapshot, selected) ->
                withContext(Dispatchers.Default) {
                    val filtered = snapshot.filter { matchesFilter(it, selected.first) && it.title.contains(selected.second, ignoreCase = true) }
                    filtered to libraryGroups(filtered, sort)
                }
            }.collectLatest { (filtered, grouped) -> shown = filtered; groups = grouped }
    }
    val posterWidth = (preferences.posterSize.widthDp * formFactor.posterScale()).dp

    // This screen arrived with focus nowhere, so a television's first D-pad
    // press was spent finding a stop instead of moving between them. Back is
    // the one control that is always composed here.
    val backFocus = remember { FocusRequester() }
    RequestInitialFocus(backFocus)

    Column(Modifier.fillMaxSize().navigationBarsPadding()) {
        SafeTopRow(
            Modifier.fillMaxWidth().padding(start = side - 12.dp, end = side, top = 8.dp),
        ) {
            TvIconButton(onClick = onBack, modifier = Modifier.focusRequester(backFocus)) {
                Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
            }
            Column(Modifier.weight(1f)) {
                Text(title, style = MaterialTheme.typography.titleLarge)
                if (load.error == null) {
                    Text("${load.loadedCount} of ${load.total} loaded · ${shown.size} match", color = Muted, style = MaterialTheme.typography.labelMedium)
                }
            }
        }

        Row(
            Modifier.fillMaxWidth().padding(horizontal = side, vertical = 12.dp),
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            ChoicePicker(
                label = "Sort",
                value = sort,
                options = listOf("title", "added", "recorded", "year", "resolution"),
                optionLabel = {
                    when (it) {
                        "title" -> "Title (A–Z)"
                        "added" -> "Recently added"
                        "recorded" -> "Date recorded"
                        "year" -> "Year"
                        else -> "Resolution"
                    }
                },
                onSelect = { sort = it },
                modifier = Modifier.weight(1f),
            )
            ChoicePicker(
                label = "Show",
                value = filter,
                options = WatchFilter.entries,
                optionLabel = { it.label },
                onSelect = { filter = it },
                modifier = Modifier.weight(1f),
            )
        }

        OutlinedTextField(
            value = query, onValueChange = { query = it }, label = { Text("Find a title") },
            singleLine = true, modifier = Modifier.fillMaxWidth().padding(horizontal = side),
        )
        Row(Modifier.fillMaxWidth().padding(horizontal = side, vertical = 12.dp), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            ChoicePicker("View", presentation, LibraryPresentation.entries, { it.label }, {
                presentation = it; onPresentationChange(it)
            }, Modifier.weight(1f))
            if (rows && groups.isNotEmpty()) {
                ChoicePicker("Jump to", jumpGroup, groups.map { it.key }, { key -> groups.firstOrNull { it.key == key }?.label ?: "Choose group" }, { key ->
                    jumpGroup = key
                    val index = groups.indexOfFirst { it.key == key }
                    if (index >= 0) scope.launch { rowsState.animateScrollToItem(index) }
                }, Modifier.weight(1f))
            }
        }
        if (load.error != null) {
            Row(Modifier.fillMaxWidth().padding(horizontal = side), verticalAlignment = Alignment.CenterVertically) {
                Text("Incomplete library: ${load.error}", color = MaterialTheme.colorScheme.error, modifier = Modifier.weight(1f))
                TextButton(onClick = { scope.launch { pager.ensure(if (rows || filter != WatchFilter.Everything || query.isNotBlank()) Int.MAX_VALUE else shown.size + 40) } }) { Text("Retry") }
            }
        }

        when {
            load.loadedCount == 0 && !load.complete && load.error == null -> LoadingBox()
            load.error != null && load.decided.isEmpty() -> Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                Text(load.error.orEmpty(), color = MaterialTheme.colorScheme.error)
            }
            shown.isEmpty() -> Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                Text(if (!load.complete) "Still loading — ${load.loadedCount} of ${load.total} titles checked" else if (filter == WatchFilter.Everything) "This library is empty." else "No titles match this filter.", color = Muted)
            }
            rows -> LazyColumn(state = rowsState, contentPadding = PaddingValues(bottom = 32.dp)) {
                rowItems(groups, key = { it.key }) { group ->
                    MediaRow(
                        title = "${group.label} · ${group.items.size}${if (load.complete) "" else " loaded"}",
                        items = group.items, posterWidth = posterWidth,
                        onViewAll = { expandedGroup = group.key }, onOpen = { onOpenItem(it.id) },
                    )
                }
            }
            else -> LazyVerticalGrid(
                state = gridState,
                columns = GridCells.Adaptive(minSize = posterWidth),
                contentPadding = PaddingValues(start = side, end = side, top = 8.dp, bottom = 32.dp),
                horizontalArrangement = Arrangement.spacedBy(16.dp),
                verticalArrangement = Arrangement.spacedBy(22.dp),
            ) {
                items(shown, key = { it.id }) { item ->
                    PosterCard(item, width = posterWidth) { onOpenItem(item.id) }
                }
            }
        }
    }
    // A full-screen group sheet leaves the keyed rows composed underneath it,
    // so closing View all returns to the same horizontal and vertical position.
    val expanded = groups.firstOrNull { it.key == expandedGroup }
    if (expanded != null) {
        Dialog(onDismissRequest = { expandedGroup = null }, properties = DialogProperties(usePlatformDefaultWidth = false)) {
            Column(Modifier.fillMaxSize().background(MaterialTheme.colorScheme.background).navigationBarsPadding()) {
                SafeTopRow(Modifier.fillMaxWidth().padding(horizontal = side, vertical = 8.dp)) {
                    TextButton(onClick = { expandedGroup = null }) { Text("All rows") }
                    Text("${expanded.label} · ${expanded.items.size}${if (load.complete) "" else " loaded"}", style = MaterialTheme.typography.titleLarge)
                }
                LazyVerticalGrid(
                    columns = GridCells.Adaptive(posterWidth),
                    contentPadding = PaddingValues(side),
                    horizontalArrangement = Arrangement.spacedBy(16.dp), verticalArrangement = Arrangement.spacedBy(22.dp),
                ) {
                    items(expanded.items, key = { it.id }) { item -> PosterCard(item, width = posterWidth) { onOpenItem(item.id) } }
                }
            }
        }
    }
}

/**
 * Which bucket this card belongs in.
 *
 * A show, season, or folder has no watch row of its own — the state lives on
 * its episodes, which are not in this response — so filtering a TV library on
 * `item.watch` alone put every show in "Unwatched" and left "Watched" and "In
 * progress" empty. `list_items` now attaches the same `rollup` the detail
 * endpoint returns for containers (remediation §8.1), and a container
 * classifies by it, exactly as `orderedSeasonCandidates` already reasons
 * elsewhere:
 *
 * ```
 * watched == leaves (leaves > 0)  → watched
 * 0 < watched < leaves            → in progress
 * otherwise                       → unwatched
 * ```
 *
 * A container from an older server sends no rollup and keeps the leaf
 * behaviour, which is the pre-§8.1 answer rather than a new wrong one.
 */
internal fun matchesFilter(item: Item, filter: WatchFilter): Boolean {
    val rollup = item.rollup?.takeIf { it.leaves > 0 }
    val watched = if (rollup != null) rollup.watched >= rollup.leaves else item.watch?.watched == true
    val inProgress = if (rollup != null) {
        rollup.watched in 1 until rollup.leaves
    } else {
        item.watch?.let { it.position_ms > 3_000 && !it.watched } == true
    }
    return when (filter) {
        WatchFilter.Everything -> true
        WatchFilter.Unwatched -> !watched && !inProgress
        WatchFilter.InProgress -> inProgress
        WatchFilter.Watched -> watched
    }
}
