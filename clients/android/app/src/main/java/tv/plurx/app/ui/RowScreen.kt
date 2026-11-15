package tv.plurx.app.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
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
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.CancellationException
import tv.plurx.app.data.Item
import tv.plurx.app.ui.components.LoadingBox
import tv.plurx.app.ui.components.PosterCard
import tv.plurx.app.ui.components.RequestInitialFocus
import tv.plurx.app.ui.components.SafeTopRow
import tv.plurx.app.ui.components.TvIconButton
import tv.plurx.app.ui.theme.Muted

private const val ROW_PAGE = 60

/** "See all" for a custom Home row: its titles as a poster grid, paged from `rows/{id}/items`. */
@Composable
fun RowScreen(
    vm: AppViewModel,
    rowId: Long,
    title: String,
    onOpenItem: (Long) -> Unit,
    onBack: () -> Unit,
) {
    val preferences by vm.preferences.collectAsStateWithLifecycle()
    val formFactor = currentFormFactor()
    val side = formFactor.horizontalPadding()
    val posterWidth = (preferences.posterSize.widthDp * formFactor.posterScale()).dp
    var shown by remember(rowId) { mutableStateOf<List<Item>>(emptyList()) }
    var total by remember(rowId) { mutableStateOf(0) }
    var error by remember(rowId) { mutableStateOf<String?>(null) }
    var loading by remember(rowId) { mutableStateOf(true) }
    var retry by remember(rowId) { mutableStateOf(0) }
    val grid = rememberLazyGridState()
    suspend fun loadMore() {
        try {
            val page = vm.rowItems(rowId, shown.size, ROW_PAGE)
            val known = shown.map { it.id }.toSet()
            shown = shown + page.items.filter { it.id !in known }
            total = page.total
            error = null
            if (page.items.isEmpty()) total = shown.size
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            error = e.message ?: "Failed to load"
        }
    }
    LaunchedEffect(rowId, retry) { loading = true; loadMore(); loading = false }
    LaunchedEffect(rowId, grid) {
        snapshotFlow { grid.layoutInfo.visibleItemsInfo.maxOfOrNull { it.index } ?: -1 }.collect { last ->
            if (!loading && error == null && shown.size < total && last >= shown.size - 12) {
                loading = true; loadMore(); loading = false
            }
        }
    }
    val backFocus = remember { FocusRequester() }
    RequestInitialFocus(backFocus)
    Column(Modifier.fillMaxSize().navigationBarsPadding()) {
        SafeTopRow(Modifier.fillMaxWidth().padding(start = side - 12.dp, end = side, top = 8.dp)) {
            TvIconButton(onClick = onBack, modifier = Modifier.focusRequester(backFocus)) {
                Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
            }
            Column(Modifier.weight(1f)) {
                Text(title, style = MaterialTheme.typography.titleLarge)
                if (total > 0) Text("${shown.size} of $total", color = Muted, style = MaterialTheme.typography.labelMedium)
            }
        }
        when {
            shown.isEmpty() && loading -> LoadingBox()
            shown.isEmpty() -> Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                if (error != null) {
                    Column(horizontalAlignment = Alignment.CenterHorizontally) {
                        Text(error.orEmpty(), color = MaterialTheme.colorScheme.error)
                        tv.plurx.app.ui.components.TvTextButton(onClick = { retry++ }) { Text("Retry") }
                    }
                } else Text("Nothing here yet.", color = Muted)
            }
            else -> LazyVerticalGrid(
                state = grid,
                columns = GridCells.Adaptive(minSize = posterWidth),
                contentPadding = PaddingValues(start = side, end = side, top = 8.dp, bottom = 32.dp),
                horizontalArrangement = Arrangement.spacedBy(16.dp),
                verticalArrangement = Arrangement.spacedBy(22.dp),
            ) {
                items(shown, key = { it.id }) { item -> PosterCard(item, width = posterWidth) { onOpenItem(item.id) } }
            }
        }
    }
}
