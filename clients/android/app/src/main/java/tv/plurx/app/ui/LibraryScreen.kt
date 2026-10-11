package tv.plurx.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.lazy.LazyRow
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
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.window.DialogWindowProvider
import tv.plurx.app.ui.components.RequestInitialFocus
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
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.platform.testTag
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
import tv.plurx.app.remote.*

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
    val landscape = kind == "home"
    LaunchedEffect(preferences.libraryPresentation) { presentation = preferences.libraryPresentation }
    var shown by remember(pager) { mutableStateOf<List<Item>>(emptyList()) }
    RemoteOrder((listOf("library:back", "choice:Sort", "choice:Show", "choice:View") + if (rows) groups.flatMap { group -> listOf("row:library-group:" + group.key + ":all") + group.items.map { "row:library-group:" + group.key + ":item:" + it.id } } else shown.map { "item:" + it.id }).distinct().take(16384)) { key ->
        if (!rows) {
            val index = shown.indexOfFirst { "item:" + it.id == key }
            if (index >= 0) scope.launch { gridState.scrollToItem(index) }
        }
    }
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
    val rowWidth = if (landscape) posterWidth * 1.6f else posterWidth
    val retry: () -> Unit = { scope.launch { pager.ensure(if (rows || filter != WatchFilter.Everything || query.isNotBlank()) Int.MAX_VALUE else shown.size + 40) } }

    // This screen arrived with focus nowhere, so a television's first D-pad
    // press was spent finding a stop instead of moving between them. Back is
    // the one control that is always composed here.
    val backFocus = remember { FocusRequester() }
    RequestInitialFocus(backFocus)

    Column(Modifier.fillMaxSize().navigationBarsPadding()) {
        SafeTopRow(
            Modifier.fillMaxWidth().padding(start = side - 12.dp, end = side, top = 8.dp),
        ) {
            TvIconButton(onClick = onBack, modifier = Modifier.remoteAction("library:back", "Back") { onBack(); RemoteOutcome.Applied }.focusRequester(backFocus)) {
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
        }
        if (rows && groups.isNotEmpty()) {
            LazyRow(
                modifier = Modifier.fillMaxWidth().testTag("library-group-index"),
                contentPadding = PaddingValues(horizontal = side), horizontalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                rowItems(groups, key = { it.key }) { group ->
                    TextButton(
                        onClick = { jumpGroup = group.key; scope.launch { rowsState.animateScrollToItem(groups.indexOfFirst { it.key == group.key }) } },
                        modifier = Modifier.semantics { selected = jumpGroup == group.key },
                    ) { Text(group.label) }
                }
            }
        }
        LibraryLoadError(load.error, Modifier.padding(horizontal = side), retry)

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
                        remoteKey = "library-group:" + group.key,
                        items = group.items, posterWidth = rowWidth, landscape = landscape,
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
        val navigation = LocalRemoteNavigation.current
        val remoteScope = LocalRemoteScope.current
        val token = remember(remoteScope, expanded.key) { java.util.UUID.randomUUID().toString() }
        val prefix = "library-expanded:$token:item"
        val currentItems by rememberUpdatedState(expanded.items)
        val semanticItems = currentItems.distinctBy { it.id }.take(16382)
        val semanticIds = semanticItems.map { it.id }.toSet()
        val keys = listOf("library-expanded:$token:close") + (if (load.error != null) listOf("library-expanded:$token:retry") else emptyList()) + semanticItems.map { "$prefix:${it.id}" }
        val expandedGrid = rememberLazyGridState()
        val currentOpen by rememberUpdatedState(onOpenItem)
        val currentRetry by rememberUpdatedState(retry)
        var owned by remember(token) { mutableStateOf(false) }
        fun closeExpanded() { navigation?.closeMenu(token); expandedGroup = null }
        DisposableEffect(navigation, remoteScope, token) {
            owned = navigation?.enterMenu(token, keys, RemotePresentationKind.LibraryGroup, realize = { key ->
                val index = currentItems.indexOfFirst { "$prefix:${it.id}" == key }
                if (index >= 0) scope.launch { if (navigation.menuOwned(token)) expandedGrid.scrollToItem(index) }
            }) { expandedGroup = null } == true
            onDispose { navigation?.closeMenu(token) }
        }
        SideEffect { if (owned) navigation?.updateMenu(token, keys) }
        Dialog(onDismissRequest = { closeExpanded() }, properties = DialogProperties(usePlatformDefaultWidth = false)) {
            if (!owned) RemoteRestricted()
            val view = LocalView.current
            val windowView = (view.parent as? DialogWindowProvider)?.window?.decorView ?: view
            DisposableEffect(navigation, windowView, token, owned) {
                if (owned) navigation?.ownedWindow(token, windowView)
                onDispose { navigation?.ownedWindow(token, null) }
            }
            val first = remember(token) { FocusRequester() }
            RequestInitialFocus(first, enabled = owned, reinforce = false)
            Column(Modifier.fillMaxSize().background(MaterialTheme.colorScheme.background).navigationBarsPadding()) {
                SafeTopRow(Modifier.fillMaxWidth().padding(horizontal = side, vertical = 8.dp)) {
                    TextButton(onClick = { closeExpanded() }, modifier = Modifier.remoteAction("library-expanded:$token:close", "All rows", enabled = owned) {
                        if (navigation?.menuOwned(token) != true) RemoteOutcome.StaleContext else { closeExpanded(); RemoteOutcome.Applied }
                    }.focusRequester(first)) { Text("All rows") }
                    Text("${expanded.label} · ${expanded.items.size}${if (load.complete) "" else " loaded"}", style = MaterialTheme.typography.titleLarge)
                }
                if (load.error != null) Row(Modifier.fillMaxWidth().padding(horizontal = side)) {
                    Text("Incomplete library: ${load.error}", modifier = Modifier.weight(1f), color = MaterialTheme.colorScheme.error)
                    TextButton(onClick = { currentRetry() }, modifier = Modifier.remoteAction("library-expanded:$token:retry", "Retry", enabled = owned) {
                        if (navigation?.menuOwned(token) != true) RemoteOutcome.StaleContext else { currentRetry(); RemoteOutcome.Applied }
                    }) { Text("Retry") }
                }
                CompositionLocalProvider(LocalRemoteItemPrefix provides prefix) {
                    LazyVerticalGrid(
                        columns = GridCells.Adaptive(rowWidth), state = expandedGrid,
                        contentPadding = PaddingValues(side),
                        horizontalArrangement = Arrangement.spacedBy(16.dp), verticalArrangement = Arrangement.spacedBy(22.dp),
                    ) {
                        items(currentItems, key = { it.id }) { item -> PosterCard(item, width = rowWidth, landscape = landscape,
                            remoteEnabled = owned && item.id in semanticIds,
                            remoteActivate = {
                                if (navigation?.menuOwned(token) != true || item.id !in semanticIds) RemoteOutcome.StaleContext
                                else { closeExpanded(); currentOpen(item.id); RemoteOutcome.Applied }
                            }) { closeExpanded(); currentOpen(item.id) }
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun LibraryLoadError(error: String?, modifier: Modifier, retry: () -> Unit) {
    if (error != null) {
        Row(modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            Text("Incomplete library: $error", color = MaterialTheme.colorScheme.error, modifier = Modifier.weight(1f))
            TextButton(onClick = retry) { Text("Retry") }
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
