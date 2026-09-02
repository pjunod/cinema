package tv.plurx.app.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Search
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay
import tv.plurx.app.data.Item
import tv.plurx.app.ui.components.LoadingBox
import tv.plurx.app.ui.components.PosterCard
import tv.plurx.app.ui.components.RequestInitialFocus
import tv.plurx.app.ui.components.SafeTopRow
import tv.plurx.app.ui.components.TvIconButton
import tv.plurx.app.ui.theme.Muted

@Composable
fun SearchScreen(
    vm: AppViewModel,
    onOpenItem: (Long) -> Unit,
    onBack: () -> Unit,
) {
    val preferences by vm.preferences.collectAsStateWithLifecycle()
    val formFactor = currentFormFactor()
    val side = formFactor.horizontalPadding()
    val posterWidth = (preferences.posterSize.widthDp * formFactor.posterScale()).dp
    // Retained across rotation and process death: retyping a query on a TV
    // remote because the app was backgrounded is the whole reason this exists.
    var query by rememberSaveable { mutableStateOf("") }
    var results by remember { mutableStateOf<List<Item>>(emptyList()) }
    var loading by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }

    LaunchedEffect(query) {
        if (query.isBlank()) {
            results = emptyList()
            loading = false
            error = null
            return@LaunchedEffect
        }
        delay(300)
        loading = true
        error = null
        try {
            results = vm.search(query)
        } catch (cancelled: CancellationException) {
            // The next keystroke superseding this query, not a failed search.
            // Reporting it flashed "StandaloneCoroutine was cancelled" under
            // the field for the length of the debounce on every fast typist.
            throw cancelled
        } catch (e: Exception) {
            error = e.message ?: "Search failed"
        } finally {
            loading = false
        }
    }

    // Search arrives with nothing focused, so the first D-pad press on a
    // television was spent finding something rather than doing something. Only
    // on a television: on a phone the field opens the soft keyboard on focus,
    // and taking over the screen on arrival is not this change's business.
    val searchFieldFocus = remember { FocusRequester() }
    RequestInitialFocus(
        searchFieldFocus,
        enabled = formFactor == FormFactor.Television,
    )

    Column(Modifier.fillMaxSize().navigationBarsPadding().imePadding()) {
        SafeTopRow(
            Modifier.fillMaxWidth().padding(start = side - 12.dp, end = side, top = 8.dp),
        ) {
            TvIconButton(onClick = onBack) {
                Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
            }
            // The TV field, not a bare OutlinedTextField: on a television a
            // focused text field that opens the keyboard on focus swallows the
            // D-pad, so Select enters editing and Back leaves it. AuthScreens
            // has done this since sign-in; Search was the one field that did
            // not.
            AuthTextField(
                value = query,
                onValueChange = { query = it },
                label = "Search",
                modifier = Modifier.weight(1f).focusRequester(searchFieldFocus),
                placeholder = "Search movies, shows, episodes, tags…",
                keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
                leadingIcon = { Icon(Icons.Filled.Search, contentDescription = null) },
            )
            // Clear sits beside the field rather than inside it: the field
            // takes Select to begin editing, so a button nested under that
            // preview handler needs two presses to answer one.
            if (query.isNotEmpty()) {
                TvIconButton(onClick = { query = "" }) {
                    Icon(Icons.Filled.Close, contentDescription = "Clear")
                }
            }
        }

        when {
            loading -> LoadingBox()
            error != null -> Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                Text(error.orEmpty(), color = MaterialTheme.colorScheme.error)
            }
            query.isBlank() -> Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                Text("Search your library", color = Muted)
            }
            results.isEmpty() -> Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                Text("No results for “$query”", color = Muted)
            }
            else -> LazyVerticalGrid(
                columns = GridCells.Adaptive(posterWidth),
                contentPadding = PaddingValues(start = side, end = side, top = 20.dp, bottom = 32.dp),
                horizontalArrangement = Arrangement.spacedBy(16.dp),
                verticalArrangement = Arrangement.spacedBy(22.dp),
            ) {
                items(results, key = { it.id }) { item ->
                    PosterCard(item, width = posterWidth) { onOpenItem(item.id) }
                }
            }
        }
    }
}
