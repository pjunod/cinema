package tv.plurx.app.ui

import androidx.compose.foundation.layout.Arrangement
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
    val formFactor = currentFormFactor()
    val side = formFactor.horizontalPadding()
    val kind = home.libraries.firstOrNull { it.id in libraryIds }?.kind
    // Retained across configuration changes and process death: a rotation, or
    // coming back from a two-hour film, should not silently reset the grid the
    // viewer set up.
    var sort by rememberSaveable(libraryIds) { mutableStateOf(if (kind == "home") "recorded" else "title") }
    var filter by rememberSaveable(libraryIds) { mutableStateOf(WatchFilter.Everything) }
    val pager = remember(vm, libraryIds, sort) { vm.libraryPager(libraryIds, sort) }
    val load by pager.state.collectAsStateWithLifecycle()
    val gridState = rememberLazyGridState()
    var shown by remember(pager) { mutableStateOf<List<Item>>(emptyList()) }
    LaunchedEffect(pager) { pager.ensure(40) }
    LaunchedEffect(pager, gridState) {
        snapshotFlow {
            val visible = gridState.layoutInfo.visibleItemsInfo
            val last = visible.maxOfOrNull { it.index } ?: 0
            last + maxOf(12, visible.size)
        }.distinctUntilChanged().collectLatest { pager.ensure(it) }
    }
    LaunchedEffect(pager, filter) {
        pager.setDriveToCompletion(filter != WatchFilter.Everything)
    }
    LaunchedEffect(pager, filter) {
        combine(pager.state, snapshotFlow { filter }) { state, selected -> state.decided to selected }
            .mapLatest { (snapshot, selected) ->
                withContext(Dispatchers.Default) { snapshot.filter { matchesFilter(it, selected) } }
            }.collectLatest { shown = it }
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

        when {
            load.loadedCount == 0 && !load.complete && load.error == null -> LoadingBox()
            load.error != null && load.decided.isEmpty() -> Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                Text(load.error.orEmpty(), color = MaterialTheme.colorScheme.error)
            }
            shown.isEmpty() -> Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                Text(if (!load.complete) "Still loading — ${load.loadedCount} of ${load.total} titles checked" else if (filter == WatchFilter.Everything) "This library is empty." else "No titles match this filter.", color = Muted)
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

