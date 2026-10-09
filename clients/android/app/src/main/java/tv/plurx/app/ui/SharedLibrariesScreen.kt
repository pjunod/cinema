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
import tv.plurx.app.remote.*
import java.util.UUID
import tv.plurx.app.ui.components.TvButton as Button
import tv.plurx.app.ui.components.TvTextButton as TextButton
import tv.plurx.app.ui.components.safeDisplayInsets

@Composable
fun SharedLibrariesEntry(onOpen: () -> Unit) {
    TextButton(modifier = Modifier.remoteAction("home:shared", "Shared libraries") { onOpen(); RemoteOutcome.Applied }, onClick = onOpen) { Text("Shared libraries · Browse by Source") }
}

private sealed interface SharedBrowseRoute {
    data object Sources : SharedBrowseRoute
    data class Library(val row: SharedLibraryRow, val parent: SharedPlaybackReference? = null, val title: String = row.name) : SharedBrowseRoute
    data class Detail(val reference: SharedPlaybackReference, val row: SharedLibraryRow) : SharedBrowseRoute
}

@Composable
fun SharedLibrariesScreen(onBack: () -> Unit) {
    var stack by remember { mutableStateOf<List<SharedBrowseRoute>>(listOf(SharedBrowseRoute.Sources)) }
    val navigation = LocalRemoteNavigation.current
    val route = stack.last()
    val scopes = remember { mutableMapOf<SharedBrowseRoute, String>() }
    val openers = remember { mutableMapOf<SharedBrowseRoute, String?>() }
    val ownerScope = scopes.getOrPut(route) { "shared:" + UUID.randomUUID().toString() }
    var rootLive by remember { mutableStateOf(true) }
    var restoration by remember { mutableLongStateOf(0) }
    fun push(next: SharedBrowseRoute): Boolean {
        if (!rootLive || stack.size >= 8 || next in stack || stack.last() != route) return false
        openers[route] = navigation?.focusedControl
        stack = stack + next
        return true
    }
    fun restoreParent(opener: String?) {
        if (!rootLive || stack.last() != route) return
        navigation?.enter(ownerScope, if (route is SharedBrowseRoute.Detail) "details" else "library") { if (stack.size > 1) { val discarded = stack.last(); stack = stack.dropLast(1); scopes.remove(discarded); openers.remove(discarded); restoration++ } else onBack(); true }
        if (opener != null) openers[route] = opener
        restoration++
    }
    fun backForRoute() {
        if (!rootLive || stack.last() != route) return
        if (stack.size > 1) { val discarded = stack.last(); stack = stack.dropLast(1); scopes.remove(discarded); openers.remove(discarded); restoration++ } else onBack()
    }
    DisposableEffect(Unit) { onDispose { rootLive = false } }
    val back by rememberUpdatedState<() -> Boolean>({ backForRoute(); true })
    DisposableEffect(navigation, ownerScope) {
        navigation?.enter(ownerScope, if (route is SharedBrowseRoute.Detail) "details" else "library") { back() }
        onDispose { navigation?.releaseScope(ownerScope) }
    }
    LaunchedEffect(ownerScope, restoration) {
        androidx.compose.runtime.withFrameNanos { }
        navigation?.restoreFocus(ownerScope, openers[route])
    }
    BackHandler(enabled = stack.size > 1) { back() }
    CompositionLocalProvider(LocalRemoteScope provides ownerScope) {
    Column(Modifier.fillMaxSize().windowInsetsPadding(safeDisplayInsets()).padding(20.dp)) {
        TextButton(modifier = Modifier.remoteAction("shared:back", "Back") { back(); RemoteOutcome.Applied }, onClick = { back() }) { Text("Back") }
        when (val route = stack.last()) {
            SharedBrowseRoute.Sources -> SharedSourceGroups(onLibrary = { row -> push(SharedBrowseRoute.Library(row)) }, onContinue = { reference, row -> push(SharedBrowseRoute.Detail(reference, row)) })
            is SharedBrowseRoute.Library -> key(route) { SharedLibraryItems(route) { reference -> push(SharedBrowseRoute.Detail(reference, route.row)) } }
            is SharedBrowseRoute.Detail -> key(route) { SharedLibraryDetails(route, restoreParent = ::restoreParent) { push(SharedBrowseRoute.Library(route.row, route.reference, it)) } }
        }
    }
    }
}

@Composable
private fun SharedSourceGroups(onLibrary: (SharedLibraryRow) -> Boolean, onContinue: (SharedPlaybackReference, SharedLibraryRow) -> Boolean) {
    var assignments by remember { mutableStateOf<List<SharedLibraryAssignment>>(emptyList()) }
    var history by remember { mutableStateOf<List<SharedContinueGroup>>(emptyList()) }
    var error by remember { mutableStateOf<String?>(null) }
    var loading by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    suspend fun load() {
        loading = true
        try { val client = SharedLibraryClient.create(); val result = client.assignments(); client.requireCurrent(); assignments = result; error = null
            try { history = client.continueGroups(); client.requireCurrent() }
            catch (failure: Exception) { if (failure is kotlinx.coroutines.CancellationException) throw failure; history = emptyList(); error = "Shared Continue Watching is unavailable." }
        }
        catch (failure: Exception) { if (failure is kotlinx.coroutines.CancellationException) throw failure; error = failure.message ?: "Shared libraries unavailable" }
        finally { loading = false }
    }
    LaunchedEffect(Unit) { load() }
    val groups = assignments.groupBy { it.identity.sourceId }.values.sortedWith(compareBy({ it.first().source_name }, { it.first().identity.sourceId }))
    val groupKeys = remember { mutableStateMapOf<String, List<String>>() }
    SideEffect { val currentSources = groups.take(16384).map { it.first().identity.sourceId }.toSet(); groupKeys.keys.filter { it !in currentSources }.forEach { groupKeys.remove(it) } }
    val sourceList = androidx.compose.foundation.lazy.rememberLazyListState()
    RemoteOrder((listOf("shared:back") + groups.flatMap { group -> groupKeys[group.first().identity.sourceId].orEmpty() + ("shared-source:" + group.first().identity.sourceId) } + "shared:sources-refresh").distinct().take(16384)) { key ->
        val index = groups.indexOfFirst { group -> key == "shared-source:" + group.first().identity.sourceId || key in groupKeys[group.first().identity.sourceId].orEmpty() }
        val offset = 2 + (if (loading) 1 else 0) + (if (error != null) 1 else 0)
        val target = if (index >= 0) offset + index else if (key == "shared:sources-refresh") offset + groups.size + (if (!loading && error == null && assignments.isEmpty()) 1 else 0) else null
        if (target != null) scope.launch { sourceList.scrollToItem(target) }
    }
    LazyColumn(state = sourceList, verticalArrangement = Arrangement.spacedBy(18.dp)) {
        item { Text("Shared libraries", style = MaterialTheme.typography.headlineMedium) }
        item { Text("Libraries shared with your account on this server.") }
        if (loading) item { CircularProgressIndicator() }
        error?.let { item { Text(it) } }
        if (!loading && error == null && assignments.isEmpty()) item { Text("No Shared libraries are assigned to your account.") }
        items(groups, key = { it.first().identity.sourceId }) { group -> SharedSourceGroup(group, history.firstOrNull { it.id == group.first().identity.sourceId }, { keys ->
            val source = group.first().identity.sourceId
            if (source in groups.take(16382).map { it.first().identity.sourceId }) {
                val reserved = groups.size.coerceAtMost(16382) + 2
                val others = groupKeys.filterKeys { it != source }.values.sumOf { it.size }
                groupKeys[source] = keys.take((16384 - reserved - others).coerceAtLeast(0))
            }
        }, onLibrary, onContinue) }
        item { Button(modifier = Modifier.remoteAction("shared:sources-refresh", "Refresh Shared libraries", !loading) { scope.launch { load() }; RemoteOutcome.Applied }, enabled = !loading, onClick = { scope.launch { load() } }) { Text("Refresh Shared libraries") } }
    }
}

@Composable
private fun SharedSourceGroup(assignments: List<SharedLibraryAssignment>, historyGroup: SharedContinueGroup?, onRemoteKeys: (List<String>) -> Unit, onLibrary: (SharedLibraryRow) -> Boolean, onContinue: (SharedPlaybackReference, SharedLibraryRow) -> Boolean) {
    var recent by remember(assignments) { mutableStateOf<SharedContinueItems?>(null) }
    var historyError by remember { mutableStateOf<String?>(null) }
    var rows by remember(assignments) { mutableStateOf<List<SharedLibraryRow>>(emptyList()) }
    var error by remember { mutableStateOf<String?>(null) }
    var loading by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    val source = assignments.first().source_name.ifBlank { "Shared Source" }
    suspend fun load() {
        loading = true; recent = null
        try {
            val client = SharedLibraryClient.create()
            try { rows = client.libraries(assignments); client.requireCurrent(); error = null }
            catch (failure: Exception) { if (failure is kotlinx.coroutines.CancellationException) throw failure; rows = emptyList(); error = "Source libraries unavailable." }
            if (historyGroup != null) {
                try { recent = client.continueItems(historyGroup, assignments); client.requireCurrent(); historyError = null }
                catch (failure: Exception) { if (failure is kotlinx.coroutines.CancellationException) throw failure; recent = null; historyError = "Continue Watching unavailable for this Source." }
            }
        } catch (failure: Exception) { if (failure is kotlinx.coroutines.CancellationException) throw failure; rows = emptyList(); recent = null; error = failure.message }
        finally { loading = false }
    }
    LaunchedEffect(assignments.map { it.id }, historyGroup) { load() }
    SideEffect { onRemoteKeys((recent?.items.orEmpty().map { "shared-continue:" + it.item.reference } + rows.map { "shared-library:" + it.identity }).distinct().take(16384)) }
    Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
        Text(source, style = MaterialTheme.typography.titleLarge)
        Text("Source · ${assignments.first().server_id.take(8)}")
        if (historyGroup != null) {
            Text("Continue Watching · ${historyGroup.source_name}", style = MaterialTheme.typography.titleMedium)
            recent?.let { reply ->
                if (reply.availability != "online") Text("Continue Watching unavailable for this Source.")
                reply.items.forEach { entry ->
                    val reference = entry.item.reference
                    val identity = SharedLibraryIdentity(reference.import_id, reference.server_id, reference.catalogue_epoch, reference.library_id)
                    val row = rows.firstOrNull { it.identity == identity } ?: SharedLibraryRow(identity, "Shared library", source, entry.item.kind)
                    Button(modifier = Modifier.remoteAction("shared-continue:" + reference, entry.item.title) { if (onContinue(reference, row)) RemoteOutcome.Applied else RemoteOutcome.Unavailable }, onClick = { onContinue(reference, row) }) { Text("${entry.item.title} · Resume at ${entry.watch.position_ms / 1000} s · $source") }
                }
            }
            historyError?.let { Text(it) }
        }
        if (loading) CircularProgressIndicator()
        error?.let { Text(it) }
        rows.forEach { row -> Button(modifier = Modifier.remoteAction("shared-library:" + row.identity, row.name) { if (onLibrary(row)) RemoteOutcome.Applied else RemoteOutcome.Unavailable }, onClick = { onLibrary(row) }) { Text("${row.name} · ${row.kind} · ${row.sourceName}") } }
        if (!loading && error == null && rows.isEmpty()) Text("No libraries are currently available from this Source.")
        TextButton(modifier = Modifier.remoteAction("shared-source:" + assignments.first().identity.sourceId, "Refresh $source", !loading) { scope.launch { load() }; RemoteOutcome.Applied }, enabled = !loading, onClick = { scope.launch { load() } }) { Text("Refresh $source") }
    }
}

@Composable
private fun SharedLibraryItems(route: SharedBrowseRoute.Library, onItem: (SharedPlaybackReference) -> Boolean) {
    var q by remember { mutableStateOf("") }
    var browse by remember { mutableStateOf(SharedBrowseAccumulator()) }
    var loading by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    var generation by remember { mutableLongStateOf(0) }
    val scope = rememberCoroutineScope()
    val remoteNavigation = LocalRemoteNavigation.current
    val remoteScope = LocalRemoteScope.current
    val searchNonce = remember(route) { UUID.randomUUID().toString() }
    val currentEdit by rememberUpdatedState<(String) -> Unit>({ q = it })
    DisposableEffect(remoteNavigation, remoteScope, searchNonce) {
        remoteNavigation?.search(remoteScope, searchNonce) { if (remoteNavigation.scope == remoteScope) currentEdit(it) }
        onDispose { remoteNavigation?.clearSearch(remoteScope, searchNonce) }
    }
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
    val grid = androidx.compose.foundation.lazy.rememberLazyListState()
    val currentItems by rememberUpdatedState(browse.items)
    val itemOffset = 2 + if (error != null) 1 else 0
    RemoteOrder(listOf("shared:back") + browse.items.take(16380).map { "shared-item:" + it.reference } + (if (browse.nextCursor != null) listOf("shared:more") else emptyList()) + "shared:refresh") { key ->
        val index = currentItems.indexOfFirst { "shared-item:" + it.reference == key }
        val footer = itemOffset + currentItems.size + (if (loading) 1 else 0) + (if (!loading && error == null && currentItems.isEmpty()) 1 else 0)
        val target = when { index >= 0 -> itemOffset + index; key == "shared:more" && browse.nextCursor != null -> footer; key == "shared:refresh" -> footer + if (browse.nextCursor != null) 1 else 0; else -> null }
        if (target != null) scope.launch { grid.scrollToItem(target) }
    }
    LazyColumn(state = grid, verticalArrangement = Arrangement.spacedBy(14.dp)) {
        item { Text(route.title, style = MaterialTheme.typography.headlineMedium); Text("Source · ${route.row.sourceName}") }
        item { OutlinedTextField(value = q, onValueChange = { q = it }, label = { Text("Find a title") }) }
        error?.let { item { Text(it) } }
        items(browse.items, key = { it.id }) { item -> Button(modifier = Modifier.remoteAction("shared-item:" + item.reference, item.title, enabled = browse.items.indexOf(item) < 16380) { if (onItem(item.reference)) RemoteOutcome.Applied else RemoteOutcome.Unavailable }, onClick = { onItem(item.reference) }) {
            SharedArtworkImage(item.artworkSubject, modifier = Modifier.size(width = 72.dp, height = 108.dp))
            Text("${item.title} · ${item.kind} · ${route.row.sourceName}")
        } }
        if (loading) item { CircularProgressIndicator() }
        if (!loading && error == null && browse.items.isEmpty()) item { Text("No matching titles in this Shared library.") }
        if (browse.nextCursor != null) item { Button(modifier = Modifier.remoteAction("shared:more", "Load more", enabled = !loading) { scope.launch { load(false) }; RemoteOutcome.Applied }, enabled = !loading, onClick = { scope.launch { load(false) } }) { Text("Load more") } }
        item { TextButton(modifier = Modifier.remoteAction("shared:refresh", "Refresh list", enabled = !loading) { scope.launch { load(true) }; RemoteOutcome.Applied }, enabled = !loading, onClick = { scope.launch { load(true) } }) { Text("Refresh list") } }
    }
}

@Composable
private fun SharedLibraryDetails(route: SharedBrowseRoute.Detail, restoreParent: (String?) -> Unit, onChildren: (String) -> Boolean) {
    val vm: AppViewModel = androidx.lifecycle.viewmodel.compose.viewModel()
    var playbackPlan by remember { mutableStateOf<SharedPlaybackPlan?>(null) }
    var preplayFile by remember { mutableStateOf<String?>(null) }
    var childOpener by remember { mutableStateOf<String?>(null) }
    var incumbent by remember { mutableStateOf<tv.plurx.app.player.SharedPlayerController?>(null) }
    /** The full Shared reference of what is playing; next episode resolves from it. */
    var playing by remember { mutableStateOf(route.reference) }
    val preferences by vm.preferences.collectAsState()
    var detail by remember { mutableStateOf<SharedLibraryDetail?>(null) }
    var error by remember { mutableStateOf<String?>(null) }
    var loading by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    val remoteNavigation = LocalRemoteNavigation.current
    val remoteScope = LocalRemoteScope.current
    val ownerToken = remember(route.reference) { UUID.randomUUID().toString() }
    var live by remember { mutableStateOf(true) }
    var preparation by remember { mutableLongStateOf(0) }
    val remotePreparation = remember { RemoteOwnedAttempt() }
    val autoplay = remember { RemoteOwnedAttempt() }
    DisposableEffect(ownerToken) { onDispose { live = false; preparation++; remotePreparation.retire(); autoplay.retire() } }
    fun fileEffect(fileId: String): RemoteDeferredEffect {
        val binding = RemoteDeferredBinding(remoteScope, ownerToken, route.reference.toString() + "|" + fileId)
        var selected: String? = null
        fun owned() = live && remoteNavigation?.scope == remoteScope && preplayFile == null && playbackPlan == null
        return RemoteDeferredEffect(binding, ::owned, { check ->
            if (loading || remotePreparation.busy || !owned() || !check()) RemoteOutcome.Unavailable else {
                val operation = ++preparation
                val busyOwner = remotePreparation.begin()
                try {
                    vm.prepareSharedPlayback(route.reference, fileId) { owned() && operation == preparation && check() }
                    if (!owned() || operation != preparation || !check()) RemoteOutcome.Unavailable
                    else { selected = fileId; RemoteOutcome.Applied }
                } finally { remotePreparation.finish(busyOwner) }
            }
        }, { outcome -> if (outcome == RemoteOutcome.Applied && owned()) { childOpener = remoteNavigation?.focusedControl; preplayFile = selected } })
    }
    suspend fun load() {
        loading = true
        try { val client = SharedLibraryClient.create(); val result = client.detail(route.reference); client.requireCurrent(); detail = result; error = null }
        catch (failure: Exception) { if (failure is kotlinx.coroutines.CancellationException) throw failure; error = failure.message ?: "Shared details unavailable" }
        finally { loading = false }
    }
    LaunchedEffect(route.reference) { load() }
    playbackPlan?.let { plan ->
        tv.plurx.app.player.PlayerScreen(vm, plan, incumbent = incumbent, onEnded = {
            // A finished episode continues with the next one in Source order,
            // started from fresh details under the current login.
            if (!preferences.autoplayNext) { playbackPlan = null; incumbent = null; restoreParent(childOpener); scope.launch { load() } }
            else scope.launch {
                val attempt = autoplay.begin(markBusy = false)
                val reference = playing
                fun currentPlayer() = live && autoplay.accepts(attempt) && playbackPlan === plan && playing == reference
                val next = try { vm.prepareNextSharedEpisode(reference, ::currentPlayer) } catch (failure: Exception) {
                    if (failure is kotlinx.coroutines.CancellationException) throw failure
                    if (currentPlayer()) error = failure.message ?: "The next shared episode is not available."; null
                }
                if (!currentPlayer()) return@launch
                if (next == null) { playbackPlan = null; incumbent = null; restoreParent(childOpener); load() } else { incumbent = null; playing = next.first; playbackPlan = next.second }
            }
        }) { autoplay.retire(); playbackPlan = null; incumbent = null; restoreParent(childOpener); scope.launch { load() } }
        return
    }
    preplayFile?.let { file ->
        SharedRemotePreplay(vm, route.reference, file, onBack = { preplayFile = null; restoreParent(childOpener) }) { plan, controller ->
            incumbent = controller; playing = route.reference; playbackPlan = plan; preplayFile = null
        }
        return
    }
    val detailList = androidx.compose.foundation.lazy.rememberLazyListState()
    val fileKeys = detail?.files.orEmpty().take(16380).filter { it.file_base != null }.map { "shared-file:" + it.file_id + ":" + it.revision }
    RemoteOrder(listOf("shared:back") + (if (detail?.item?.hasChildren == true) listOf("shared:children") else emptyList()) + fileKeys + "shared:details-refresh") { key ->
        val value = detail
        if (value != null) {
            val index = value.files.indexOfFirst { "shared-file:" + it.file_id + ":" + it.revision == key }
            val offset = 1 + (if (loading) 1 else 0) + (if (error != null) 1 else 0) +
                (if (value.item.backdrop_url != null) 1 else 0) + 1 + (if (value.item.overview != null) 1 else 0) +
                (if (value.item.genres.isNotEmpty()) 1 else 0) + (if (value.delivery_status != "available") 1 else 0) +
                (if (value.watch != null) 1 else 0) + (if (value.item.kind in listOf("movie", "episode")) 1 else 0) +
                (if (value.item.hasChildren) 1 else 0)
            val target = when { index >= 0 -> offset + index; key == "shared:children" && value.item.hasChildren -> offset - 1; key == "shared:details-refresh" -> offset + value.files.size; else -> null }
            if (target != null) scope.launch { detailList.scrollToItem(target) }
        }
    }
    LazyColumn(state = detailList, verticalArrangement = Arrangement.spacedBy(14.dp)) {
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
            if (value.item.hasChildren) item { Button(modifier = Modifier.remoteAction("shared:children", "Browse children") { if (onChildren(value.item.title)) RemoteOutcome.Applied else RemoteOutcome.Unavailable }, onClick = { onChildren(value.item.title) }) { Text("Browse children") } }
            items(value.files, key = { it.file_id + "|" + it.revision }) { file ->
                Column {
                    Text(listOfNotNull(file.container, file.video_codec).joinToString(" · "))
                    if (file.width != null && file.height != null) Text("${file.width} × ${file.height}")
                    file.duration_ms?.let { Text("Duration: ${it / 1000} seconds") }
                    if (value.delivery_status == "available" && file.file_base != null) Button(modifier = Modifier.remoteAction("shared-file:" + file.file_id + ":" + file.revision, "Prepare Shared file", enabled = value.files.indexOf(file) < 16380, deferred = { action -> if (action.type == "select") fileEffect(file.file_id) else null }) { RemoteOutcome.Unsupported }, enabled = !loading, onClick = {
                        scope.launch {
                            loading = true
                            try { val operation = ++preparation
                                val prepared = vm.prepareSharedPlayback(route.reference, file.file_id) { live && remoteNavigation?.scope == remoteScope && operation == preparation }
                                if (live && remoteNavigation?.scope == remoteScope && operation == preparation) { incumbent = null; playbackPlan = prepared; playing = route.reference; error = null } }
                            catch (failure: Exception) { if (failure is kotlinx.coroutines.CancellationException) throw failure; error = failure.message ?: "Shared playback unavailable" }
                            finally { loading = false }
                        }
                    }) { Text("Play Shared file") }
                }
            }
        }
        item { TextButton(modifier = Modifier.remoteAction("shared:details-refresh", "Refresh details", !loading) { scope.launch { load() }; RemoteOutcome.Applied }, enabled = !loading, onClick = { scope.launch { load() } }) { Text("Refresh details") } }
    }
}
