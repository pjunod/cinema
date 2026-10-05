package tv.plurx.app.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch
import tv.plurx.app.data.*
import tv.plurx.app.ui.components.TvButton as Button
import tv.plurx.app.ui.components.TvTextButton as TextButton
import tv.plurx.app.ui.components.safeDisplayInsets

@Composable
fun SharedLibrariesEntry(onOpen: () -> Unit) {
    TextButton(onClick = onOpen) { Text("Shared libraries · Browse by Source") }
}

private sealed interface SharedBrowseRoute {
    data object Sources : SharedBrowseRoute
    data class Library(val row: SharedLibraryRow, val parent: SharedPlaybackReference? = null, val title: String = row.name) : SharedBrowseRoute
    data class Detail(val reference: SharedPlaybackReference, val row: SharedLibraryRow) : SharedBrowseRoute
}

@Composable
fun SharedLibrariesScreen(onBack: () -> Unit) {
    var stack by remember { mutableStateOf<List<SharedBrowseRoute>>(listOf(SharedBrowseRoute.Sources)) }
    BackHandler(enabled = stack.size > 1) { stack = stack.dropLast(1) }
    Column(Modifier.fillMaxSize().windowInsetsPadding(safeDisplayInsets()).padding(20.dp)) {
        TextButton(onClick = { if (stack.size > 1) stack = stack.dropLast(1) else onBack() }) { Text("Back") }
        when (val route = stack.last()) {
            SharedBrowseRoute.Sources -> SharedSourceGroups { row -> stack = stack + SharedBrowseRoute.Library(row) }
            is SharedBrowseRoute.Library -> key(route) { SharedLibraryItems(route) { reference -> stack = stack + SharedBrowseRoute.Detail(reference, route.row) } }
            is SharedBrowseRoute.Detail -> key(route) { SharedLibraryDetails(route) { stack = stack + SharedBrowseRoute.Library(route.row, route.reference, it) } }
        }
    }
}

@Composable
private fun SharedSourceGroups(onLibrary: (SharedLibraryRow) -> Unit) {
    var assignments by remember { mutableStateOf<List<SharedLibraryAssignment>>(emptyList()) }
    var error by remember { mutableStateOf<String?>(null) }
    var loading by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    suspend fun load() {
        loading = true
        try { val client = SharedLibraryClient.create(); val result = client.assignments(); client.requireCurrent(); assignments = result; error = null }
        catch (failure: Exception) { if (failure is kotlinx.coroutines.CancellationException) throw failure; error = failure.message ?: "Shared libraries unavailable" }
        finally { loading = false }
    }
    LaunchedEffect(Unit) { load() }
    val groups = assignments.groupBy { it.identity.sourceId }.values.sortedWith(compareBy({ it.first().source_name }, { it.first().identity.sourceId }))
    LazyColumn(verticalArrangement = Arrangement.spacedBy(18.dp)) {
        item { Text("Shared libraries", style = MaterialTheme.typography.headlineMedium) }
        item { Text("Libraries shared with your account on this server.") }
        if (loading) item { CircularProgressIndicator() }
        error?.let { item { Text(it) } }
        if (!loading && error == null && assignments.isEmpty()) item { Text("No Shared libraries are assigned to your account.") }
        items(groups, key = { it.first().identity.sourceId }) { group -> SharedSourceGroup(group, onLibrary) }
        item { Button(enabled = !loading, onClick = { scope.launch { load() } }) { Text("Refresh Shared libraries") } }
    }
}

@Composable
private fun SharedSourceGroup(assignments: List<SharedLibraryAssignment>, onLibrary: (SharedLibraryRow) -> Unit) {
    var rows by remember(assignments) { mutableStateOf<List<SharedLibraryRow>>(emptyList()) }
    var error by remember { mutableStateOf<String?>(null) }
    var loading by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    val source = assignments.first().source_name.ifBlank { "Shared Source" }
    suspend fun load() {
        loading = true
        try { val client = SharedLibraryClient.create(); val result = client.libraries(assignments); client.requireCurrent(); rows = result; error = null }
        catch (failure: Exception) { if (failure is kotlinx.coroutines.CancellationException) throw failure; error = if ((failure as? RefusalException)?.code == "sharing_source_unavailable") "Source unavailable. Your other libraries remain available." else failure.message ?: "Source metadata unavailable" }
        finally { loading = false }
    }
    LaunchedEffect(assignments.map { it.id }) { load() }
    Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
        Text(source, style = MaterialTheme.typography.titleLarge)
        Text("Source · ${assignments.first().server_id.take(8)}")
        if (loading) CircularProgressIndicator()
        error?.let { Text(it) }
        rows.forEach { row -> Button(onClick = { onLibrary(row) }) { Text("${row.name} · ${row.kind} · ${row.sourceName}") } }
        if (!loading && error == null && rows.isEmpty()) Text("No libraries are currently available from this Source.")
        TextButton(enabled = !loading, onClick = { scope.launch { load() } }) { Text("Refresh $source") }
    }
}

@Composable
private fun SharedLibraryItems(route: SharedBrowseRoute.Library, onItem: (SharedPlaybackReference) -> Unit) {
    var q by remember { mutableStateOf("") }
    var browse by remember { mutableStateOf(SharedBrowseAccumulator()) }
    var loading by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    var generation by remember { mutableLongStateOf(0) }
    val scope = rememberCoroutineScope()
    suspend fun load(reset: Boolean) {
        if (reset) { generation++; browse = SharedBrowseAccumulator() }
        val expected = generation
        val cursor = if (reset) null else browse.nextCursor
        loading = true
        try {
            val client = SharedLibraryClient.create()
            val page = client.page(route.row.identity, route.parent, q, cursor)
            client.requireCurrent()
            if (generation == expected) { browse = browse.append(page, cursor, route.row.identity); error = null }
        } catch (failure: Exception) { if (failure is kotlinx.coroutines.CancellationException) throw failure; if (generation == expected) error = failure.message ?: "Shared titles unavailable" }
        finally { if (generation == expected) loading = false }
    }
    LaunchedEffect(q) { load(true) }
    LazyColumn(verticalArrangement = Arrangement.spacedBy(14.dp)) {
        item { Text(route.title, style = MaterialTheme.typography.headlineMedium); Text("Source · ${route.row.sourceName}") }
        item { OutlinedTextField(value = q, onValueChange = { q = it }, label = { Text("Find a title") }) }
        error?.let { item { Text(it) } }
        items(browse.items, key = { it.id }) { item -> Button(onClick = { onItem(item.reference) }) {
            SharedArtworkImage(item.artworkSubject, modifier = Modifier.size(width = 72.dp, height = 108.dp))
            Text("${item.title} · ${item.kind} · ${route.row.sourceName}")
        } }
        if (loading) item { CircularProgressIndicator() }
        if (!loading && error == null && browse.items.isEmpty()) item { Text("No matching titles in this Shared library.") }
        if (browse.nextCursor != null) item { Button(enabled = !loading, onClick = { scope.launch { load(false) } }) { Text("Load more") } }
        item { TextButton(enabled = !loading, onClick = { scope.launch { load(true) } }) { Text("Refresh list") } }
    }
}

@Composable
private fun SharedLibraryDetails(route: SharedBrowseRoute.Detail, onChildren: (String) -> Unit) {
    val vm: AppViewModel = androidx.lifecycle.viewmodel.compose.viewModel()
    var playbackPlan by remember { mutableStateOf<SharedPlaybackPlan?>(null) }
    /** The full Shared reference of what is playing; next episode resolves from it. */
    var playing by remember { mutableStateOf(route.reference) }
    val preferences by vm.preferences.collectAsState()
    var detail by remember { mutableStateOf<SharedLibraryDetail?>(null) }
    var error by remember { mutableStateOf<String?>(null) }
    var loading by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    suspend fun load() {
        loading = true
        try { val client = SharedLibraryClient.create(); val result = client.detail(route.reference); client.requireCurrent(); detail = result; error = null }
        catch (failure: Exception) { if (failure is kotlinx.coroutines.CancellationException) throw failure; error = failure.message ?: "Shared details unavailable" }
        finally { loading = false }
    }
    LaunchedEffect(route.reference) { load() }
    playbackPlan?.let { plan ->
        tv.plurx.app.player.PlayerScreen(vm, plan, onEnded = {
            // A finished episode continues with the next one in Source order,
            // started from fresh details under the current login.
            if (!preferences.autoplayNext) { playbackPlan = null; scope.launch { load() } }
            else scope.launch {
                val next = try { vm.prepareNextSharedEpisode(playing) } catch (failure: Exception) {
                    if (failure is kotlinx.coroutines.CancellationException) throw failure
                    error = failure.message ?: "The next shared episode is not available."; null
                }
                if (next == null) { playbackPlan = null; load() } else { playing = next.first; playbackPlan = next.second }
            }
        }) { playbackPlan = null; scope.launch { load() } }
        return
    }
    LazyColumn(verticalArrangement = Arrangement.spacedBy(14.dp)) {
        item { Text("Source · ${route.row.sourceName}") }
        if (loading) item { CircularProgressIndicator() }
        error?.let { item { Text(it) } }
        detail?.let { value ->
            if (value.item.backdrop_url != null) item { SharedArtworkImage(value.item.artworkSubject, backdrop = true, modifier = Modifier.fillMaxWidth().height(240.dp)) }
            item { Text(value.item.title, style = MaterialTheme.typography.headlineMedium); Text(listOfNotNull(value.item.kind, value.item.year?.toString()).joinToString(" · ")) }
            value.item.overview?.let { item { Text(it) } }
            if (value.item.genres.isNotEmpty()) item { Text(value.item.genres.joinToString(" · ")) }
            if (value.delivery_status != "available") item { Text("Playback is unavailable for this Shared title.") }
            value.watch?.let { watch -> item { Text(if (watch.watched) "Watched on this server" else "Position on this server: ${watch.position_ms / 1000} seconds") } }
            if (value.item.kind == "movie" || value.item.kind == "episode") item {
                val watched = value.watch?.watched == true
                Button(enabled = !loading, onClick = {
                    scope.launch {
                        loading = true
                        try { val client = SharedLibraryClient.create(); client.setWatched(route.reference, !watched); client.requireCurrent(); error = null }
                        catch (failure: Exception) { if (failure is kotlinx.coroutines.CancellationException) throw failure; error = failure.message ?: "Shared watch state unavailable" }
                        finally { loading = false }
                        load()
                    }
                }) { Text(if (watched) "Mark unwatched" else "Mark watched") }
            }
            if (value.item.hasChildren) item { Button(onClick = { onChildren(value.item.title) }) { Text("Browse children") } }
            items(value.files, key = { it.file_id + "|" + it.revision }) { file ->
                Column {
                    Text(listOfNotNull(file.container, file.video_codec).joinToString(" · "))
                    if (file.width != null && file.height != null) Text("${file.width} × ${file.height}")
                    file.duration_ms?.let { Text("Duration: ${it / 1000} seconds") }
                    if (value.delivery_status == "available" && file.file_base != null) Button(enabled = !loading, onClick = {
                        scope.launch {
                            loading = true
                            try { playbackPlan = vm.prepareSharedPlayback(route.reference, file.file_id); playing = route.reference; error = null }
                            catch (failure: Exception) { if (failure is kotlinx.coroutines.CancellationException) throw failure; error = failure.message ?: "Shared playback unavailable" }
                            finally { loading = false }
                        }
                    }) { Text("Play Shared file") }
                }
            }
        }
        item { TextButton(enabled = !loading, onClick = { scope.launch { load() } }) { Text("Refresh details") } }
    }
}
