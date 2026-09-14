@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.librarychannels

import android.view.ViewGroup
import tv.plurx.app.data.SubjectPreview
import tv.plurx.app.data.SubjectPreviewRequest
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.coroutines.delay
import kotlinx.coroutines.CancellationException
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Checkbox
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import tv.plurx.app.ui.components.TvButton as Button
import tv.plurx.app.ui.components.TvTextButton as TextButton
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.runtime.SideEffect
import androidx.compose.ui.Modifier
import androidx.compose.ui.Alignment
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.media3.ui.PlayerView
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.serialization.decodeFromString
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import tv.plurx.app.data.LibraryChannel
import tv.plurx.app.data.LibraryChannelDefinition
import tv.plurx.app.data.LibraryChannelOrdering
import tv.plurx.app.data.LibraryChannelPreview
import tv.plurx.app.data.LibraryChannelPreviewRequest
import tv.plurx.app.data.LibraryChannelRebuild
import tv.plurx.app.data.LibraryChannelRecipe
import tv.plurx.app.data.LibraryChannelUpdate
import tv.plurx.app.data.LibraryChannelVisibility
import tv.plurx.app.ui.AppViewModel
import tv.plurx.app.ui.FormFactor
import tv.plurx.app.ui.components.ChoicePicker
import tv.plurx.app.ui.components.safeDisplayInsets
import tv.plurx.app.ui.currentFormFactor
import java.text.DateFormat
import java.util.Date

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun LibraryChannelsScreen(
    vm: AppViewModel,
    seedItemId: Long? = null,
    seedKind: String? = null,
    seedTitle: String? = null,
    onWatchFromStart: (Long, Long, String) -> Unit,
    onBack: () -> Unit,
) {
    val context = LocalContext.current
    val controller = remember(context) { LibraryChannelPlayer.get(context) }
    val state by controller.state.collectAsStateWithLifecycle()
    val television = currentFormFactor() == FormFactor.Television
    var editor by remember { mutableStateOf<LibraryChannel?>(null) }
    var creating by remember(seedItemId) { mutableStateOf(seedItemId != null) }
    var tvLayout by rememberSaveable { mutableStateOf("guide_preview") }
    var focusedProgrammeId by rememberSaveable { mutableStateOf<String?>(null) }
    BackHandler(onBack = onBack)

    LaunchedEffect(vm.origin) {
        controller.load(vm.origin)
        while (true) {
            delay(30_000)
            controller.refresh()
        }
    }
    DisposableEffect(controller) { onDispose { controller.stop() } }

    Column(
        Modifier.fillMaxSize().windowInsetsPadding(safeDisplayInsets()).padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
            TextButton(onClick = onBack) { Text("Back") }
            if (!television) {
                Button(onClick = { creating = true }) { Text("Make a channel") }
            } else {
                TextButton(onClick = {
                    tvLayout = when (tvLayout) {
                        "guide_preview" -> "guide_over_picture"
                        "guide_over_picture" -> "channel_browser"
                        else -> "guide_preview"
                    }
                }) {
                    Text("Layout · ${when (tvLayout) {
                        "guide_preview" -> "Guide + preview"
                        "guide_over_picture" -> "Guide over picture"
                        else -> "Channel browser"
                    }}")
                }
            }
        }
        Text("Library channels", style = MaterialTheme.typography.headlineMedium)
        Text(state.message, style = MaterialTheme.typography.bodySmall)
        if (television) {
            when (tvLayout) {
                "guide_over_picture" -> Box(Modifier.fillMaxSize()) {
                    LibraryChannelPlayerPane(controller, state, onWatchFromStart, Modifier.fillMaxSize())
                    LibraryChannelList(
                        controller, state.channels, state.programmes, false, { editor = it },
                        focusedProgrammeId, { focusedProgrammeId = it },
                        Modifier.align(Alignment.CenterStart).fillMaxWidth(0.42f).fillMaxHeight()
                            .background(MaterialTheme.colorScheme.surface.copy(alpha = 0.9f)).padding(12.dp),
                    )
                }
                "channel_browser" -> Row(Modifier.fillMaxSize(), horizontalArrangement = Arrangement.spacedBy(18.dp)) {
                    LibraryChannelList(controller, state.channels, state.programmes, false, { editor = it }, focusedProgrammeId, { focusedProgrammeId = it }, Modifier.weight(1.35f))
                    LibraryChannelPlayerPane(controller, state, onWatchFromStart, Modifier.weight(0.8f))
                }
                else -> Row(Modifier.fillMaxSize(), horizontalArrangement = Arrangement.spacedBy(18.dp)) {
                    LibraryChannelPlayerPane(controller, state, onWatchFromStart, Modifier.weight(1.35f))
                    LibraryChannelList(controller, state.channels, state.programmes, false, { editor = it }, focusedProgrammeId, { focusedProgrammeId = it }, Modifier.weight(1f))
                }
            }
        } else {
            LibraryChannelPlayerPane(controller, state, onWatchFromStart, Modifier.fillMaxWidth().height(280.dp))
            LibraryChannelList(controller, state.channels, state.programmes, true, { editor = it }, focusedProgrammeId, { focusedProgrammeId = it }, Modifier.weight(1f))
        }
    }

    if (creating || editor != null) {
        ModalBottomSheet(onDismissRequest = { creating = false; editor = null }) {
            LibraryChannelEditor(
                vm = vm,
                channel = editor,
                seedItemId = seedItemId.takeIf { editor == null },
                seedKind = seedKind.takeIf { editor == null },
                seedTitle = seedTitle.takeIf { editor == null },
                onSaved = {
                    creating = false
                    editor = null
                    controller.refresh()
                },
                onCancel = { creating = false; editor = null },
            )
        }
    }
}

@Composable
private fun LibraryChannelPlayerPane(
    controller: LibraryChannelPlayer,
    state: LibraryChannelPlayerState,
    onWatchFromStart: (Long, Long, String) -> Unit,
    modifier: Modifier,
) {
    Column(modifier, verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Box(Modifier.fillMaxWidth().weight(1f).background(Color.Black)) {
            AndroidView(
                factory = { context ->
                    PlayerView(context).apply {
                        layoutParams = ViewGroup.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT)
                        useController = false
                        player = controller.player
                    }
                },
                update = { it.player = controller.player },
                modifier = Modifier.fillMaxSize(),
            )
        }
        state.watching?.let { channel ->
            Text(channel.name, style = MaterialTheme.typography.titleLarge)
            Text(state.title ?: "On now", style = MaterialTheme.typography.titleMedium)
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(onClick = controller::togglePause) { Text(if (state.paused) "Resume live" else "Pause") }
                state.resolved?.let { resolved ->
                    Button(onClick = { controller.stop(); onWatchFromStart(resolved.item_id, resolved.file_id, channel.id) }) { Text("Watch from start") }
                }
                TextButton(onClick = { controller.stop() }) { Text("Stop") }
            }
        }
    }
}

@Composable
private fun LibraryChannelList(
    controller: LibraryChannelPlayer,
    channels: List<LibraryChannel>,
    programmes: List<tv.plurx.app.data.LibraryChannelProgramme>,
    authoring: Boolean,
    onEdit: (LibraryChannel) -> Unit,
    focusedProgrammeId: String?,
    onFocusProgramme: (String) -> Unit,
    modifier: Modifier,
) {
    LazyColumn(modifier, verticalArrangement = Arrangement.spacedBy(12.dp)) {
        items(channels, key = { it.id }) { channel ->
            Column(
                Modifier.fillMaxWidth().background(MaterialTheme.colorScheme.surfaceVariant, MaterialTheme.shapes.medium).padding(14.dp),
                verticalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                    Text(channel.name, style = MaterialTheme.typography.titleMedium)
                    TextButton(onClick = { controller.setFavourite(channel) }) {
                        Text(if (channel.favourite) "★" else "☆")
                    }
                }
                if (channel.description.isNotBlank()) Text(channel.description, style = MaterialTheme.typography.bodySmall)
                Row(Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    programmes.asSequence().filter { it.channel_id == channel.id }.forEach { programme ->
                        val watching = stateIdentity(controller, channel, programme)
                        Button(onClick = {
                            onFocusProgramme(programme.identity)
                            val now = System.currentTimeMillis()
                            if (programme.starts_at_ms <= now && programme.ends_at_ms > now) controller.tune(channel)
                        }, modifier = Modifier.width(((programme.ends_at_ms - programme.starts_at_ms) / 300_000f * 80f).coerceIn(120f, 360f).dp)) {
                            Text((if (watching) "▶ " else if (focusedProgrammeId == programme.identity) "• " else "") + programme.title, maxLines = 1)
                        }
                    }
                }
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Button(enabled = channel.enabled, onClick = { controller.tune(channel) }) {
                        Text(if (channel.enabled) "Watch live" else "Disabled")
                    }
                    if (authoring && channel.can_edit) TextButton(onClick = { onEdit(channel) }) { Text("Edit") }
                }
            }
        }
    }
}

private fun stateIdentity(
    controller: LibraryChannelPlayer,
    channel: LibraryChannel,
    programme: tv.plurx.app.data.LibraryChannelProgramme,
): Boolean {
    val current = controller.state.value
    val resolved = current.resolved ?: return false
    return current.watching?.id == channel.id && resolved.generation_id == programme.generation_id &&
        resolved.occurrence.cycle == programme.cycle && resolved.occurrence.ordinal == programme.ordinal
}

@Composable
private fun LibraryChannelEditor(
    vm: AppViewModel,
    channel: LibraryChannel?,
    seedItemId: Long?,
    seedKind: String?,
    seedTitle: String?,
    onSaved: () -> Unit,
    onCancel: () -> Unit,
) {
    val scope = rememberCoroutineScope()
    val context = LocalContext.current
    val home by vm.home.collectAsStateWithLifecycle()
    val draftKey = "library-channel-draft-v2:${vm.serverInstanceId}:${vm.currentUserId}:${channel?.id ?: "new"}"
    val preferences = remember(draftKey) { context.getSharedPreferences("library-channel-drafts", android.content.Context.MODE_PRIVATE) }
    val restored = remember(draftKey) { preferences.getString(draftKey, null)?.let { runCatching { Json.decodeFromString<LibraryChannelDefinition>(it) }.getOrNull() } }
    val initialRecipe = restored?.recipe ?: channel?.recipe ?: LibraryChannelRecipe()
    var name by remember(channel, seedItemId) {
        mutableStateOf(restored?.name ?: channel?.name ?: seedTitle?.let { "$it channel" }.orEmpty())
    }
    var description by remember(channel) { mutableStateOf(restored?.description ?: channel?.description.orEmpty()) }
    var shared by remember(channel) { mutableStateOf((restored?.visibility ?: channel?.visibility) == LibraryChannelVisibility.shared) }
    var enabled by remember(channel) { mutableStateOf(restored?.enabled ?: channel?.enabled ?: true) }
    var movies by remember(channel) { mutableStateOf(initialRecipe.kinds.contains("movie")) }
    var episodes by remember(channel) { mutableStateOf(initialRecipe.kinds.contains("episode")) }
    var subject by remember(channel) { mutableStateOf(initialRecipe.subject.jsonPrimitive.contentOrNull.orEmpty()) }
    var subjectPreview by remember { mutableStateOf<SubjectPreview?>(null) }
    var subjectJobId by remember(draftKey) { mutableStateOf(preferences.getString("$draftKey:subjectJob", null) ?: channel?.matching?.job_id) }
    var subjectRecipe by remember(draftKey) { mutableStateOf(preferences.getString("$draftKey:subjectRecipe", null)?.let { runCatching { Json.decodeFromString<LibraryChannelRecipe>(it) }.getOrNull() } ?: channel?.recipe) }
    var showAdvanced by rememberSaveable { mutableStateOf(false) }
    var subjectFilter by remember { mutableStateOf("match") }
    var subjectCursor by remember { mutableStateOf<String?>(null) }
    var genres by remember(channel) { mutableStateOf(initialRecipe.genres_any.joinToString(", ")) }
    var tags by remember(channel) { mutableStateOf(initialRecipe.tags_any.joinToString(", ")) }
    var keywords by remember(channel) { mutableStateOf(initialRecipe.keywords_any.joinToString(", ")) }
    var yearMin by remember(channel) { mutableStateOf(initialRecipe.year_min?.toString().orEmpty()) }
    var yearMax by remember(channel) { mutableStateOf(initialRecipe.year_max?.toString().orEmpty()) }
    var ordering by remember(channel) { mutableStateOf(initialRecipe.ordering) }
    var specials by remember(channel) { mutableStateOf(initialRecipe.include_specials) }
    var autoRefresh by remember(channel) { mutableStateOf(initialRecipe.auto_refresh) }
    var matchAllInScope by remember(channel) { mutableStateOf(initialRecipe.match_all_in_scope) }
    var libraryIds by remember(channel) { mutableStateOf(initialRecipe.library_ids) }
    var includeItemIds by remember(channel, seedItemId) {
        mutableStateOf((initialRecipe.include_item_ids + listOfNotNull(seedItemId.takeIf { seedKind != "show" })).distinct().sorted())
    }
    var includeShowIds by remember(channel, seedItemId) {
        mutableStateOf((initialRecipe.include_show_ids + listOfNotNull(seedItemId.takeIf { seedKind == "show" })).distinct().sorted())
    }
    var excludeItemIds by remember(channel) { mutableStateOf(initialRecipe.exclude_item_ids) }
    var excludeShowIds by remember(channel) { mutableStateOf(initialRecipe.exclude_show_ids) }
    var searchText by remember { mutableStateOf("") }
    var searchResults by remember { mutableStateOf(emptyList<tv.plurx.app.data.Item>()) }
    var preview by remember { mutableStateOf<LibraryChannelPreview?>(null) }
    var previewSeed by remember(channel) {
        mutableStateOf(restored?.preview_seed ?: channel?.seed?.joinToString("") { "%02x".format(it) })
    }
    var busy by remember { mutableStateOf(false) }
    var message by remember { mutableStateOf<String?>(null) }
    var step by rememberSaveable(draftKey) { mutableStateOf(preferences.getInt("$draftKey:step", 0)) }
    val canShare = vm.currentUser?.is_admin == true

    fun words(value: String) = value.split(',').map(String::trim).filter(String::isNotEmpty)
    fun recipe(): LibraryChannelRecipe {
        val old = channel?.recipe ?: LibraryChannelRecipe()
        val genresAny = words(genres)
        val tagsAny = words(tags)
        val keywordsAny = words(keywords)
        val min = yearMin.toIntOrNull()
        val max = yearMax.toIntOrNull()
        return old.copy(
            subject = subject.trim().takeIf { it.isNotEmpty() }?.let { JsonPrimitive(java.text.Normalizer.normalize(it, java.text.Normalizer.Form.NFC)) } ?: JsonNull,
            library_ids = libraryIds,
            kinds = buildList { if (movies) add("movie"); if (episodes) add("episode") },
            genres_any = genresAny,
            tags_any = tagsAny,
            keywords_any = keywordsAny,
            year_min = min,
            year_max = max,
            ordering = ordering,
            include_specials = specials,
            auto_refresh = autoRefresh,
            include_item_ids = includeItemIds,
            include_show_ids = includeShowIds,
            exclude_item_ids = excludeItemIds,
            exclude_show_ids = excludeShowIds,
            // An empty form is an editable draft, not an implicit request for
            // every video on the server. Preserve only an explicit existing
            // all-in-scope recipe until a dedicated library preset changes it.
            match_all_in_scope = matchAllInScope,
        )
    }
    val valid = (name.isNotBlank() || (channel == null && subject.isNotBlank())) && name.length <= 80 && description.length <= 500 && subject.codePointCount(0, subject.length) <= 500 && (movies || episodes)
    val trimmedSubject = subject.trim()
    val effectiveName = name.trim().ifEmpty { if (channel == null) trimmedSubject.substring(0, trimmedSubject.offsetByCodePoints(0, minOf(80, trimmedSubject.codePointCount(0, trimmedSubject.length)))) else "" }
    var saveAttempt by remember(draftKey) { mutableStateOf(preferences.getString("$draftKey:saveAttempt", null)?.let { runCatching { Json.decodeFromString<LibraryChannelDefinition>(it) }.getOrNull() }) }
    val payload = LibraryChannelDefinition(
        request_id = "",
        name = effectiveName, description = description.trim(),
        visibility = if (canShare && shared) LibraryChannelVisibility.shared else LibraryChannelVisibility.personal,
        enabled = enabled, recipe = recipe(), preview_seed = previewSeed,
    )
    val requestId = remember(payload) { if (restored?.copy(request_id = "") == payload) restored.request_id else java.util.UUID.randomUUID().toString() }
    val persistedDefinition = payload.copy(request_id = requestId)
    val currentRecipe = recipe()
    val lifecycleState by LocalLifecycleOwner.current.lifecycle.currentStateFlow.collectAsState()
    LaunchedEffect(channel?.id) {
        if (channel != null && subjectJobId == null) {
            runCatching { vm.api().libraryChannel(channel.id) }.getOrNull()?.let { fresh ->
                if (fresh.recipe == recipe()) { subjectRecipe = fresh.recipe; subjectJobId = fresh.matching?.job_id }
            }
        }
    }
    LaunchedEffect(subjectJobId, subjectFilter, currentRecipe, subjectCursor, lifecycleState) {
        if (!lifecycleState.isAtLeast(Lifecycle.State.STARTED)) return@LaunchedEffect
        val id = subjectJobId ?: return@LaunchedEffect
        if (currentRecipe != subjectRecipe) { subjectPreview = null; return@LaunchedEffect }
        while (true) {
            try {
                val result = vm.api().subjectPreview(id, subjectFilter, subjectCursor)
                subjectPreview = result; previewSeed = result.preview_seed
                if (result.complete || result.state in listOf("failed", "cancelled", "superseded")) break
                delay(if (result.state == "waiting_for_provider") 10000L else 2000L)
            } catch (error: CancellationException) { throw error }
            catch (error: Exception) { if (subjectCursor != null) subjectCursor = null; message = error.message; delay(10000) }
        }
    }
    SideEffect {
        preferences.edit().putString("$draftKey:subjectJob", subjectJobId).putString("$draftKey:subjectRecipe", subjectRecipe?.let { Json.encodeToString(it) }).apply()
        preferences.edit().putString(draftKey, Json.encodeToString(persistedDefinition))
            .putInt("$draftKey:step", step).apply()
    }

    Column(
        Modifier.fillMaxHeight(0.92f).verticalScroll(rememberScrollState()).padding(horizontal = 20.dp, vertical = 8.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Text(if (channel == null) "Make a channel" else "Edit channel", style = MaterialTheme.typography.headlineSmall)
        if (step == 0) {
        Text("1 · Content", style = MaterialTheme.typography.titleMedium)
        OutlinedTextField(subject, { subject = it; subjectJobId = null; subjectPreview = null }, label = { Text("Subject — what belongs and what to exclude") }, modifier = Modifier.fillMaxWidth())
        Text("Save now; matching continues in the background.")
        TextButton(onClick = { subject = "Stand-up comedy performances and specials. Exclude sitcoms, comedy movies, talk shows, and documentaries about comedians."; genres = ""; tags = ""; keywords = ""; yearMin = ""; yearMax = ""; matchAllInScope = false; if (name.isBlank()) name = "Stand-up comedy" }) { Text("Stand-up comedy") }
        Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            TextButton(onClick = { subject = "Documentaries about space exploration and astronomy."; genres = ""; keywords = ""; tags = ""; yearMin = ""; yearMax = ""; matchAllInScope = false }) { Text("Space docs") }
            TextButton(onClick = { subject = "Comedy films and television episodes released between 1990 and 1999."; genres = ""; keywords = ""; tags = ""; yearMin = ""; yearMax = ""; matchAllInScope = false }) { Text("’90s comedy") }
            TextButton(onClick = { subject = "Film noir crime stories with morally ambiguous characters and a dark, fatalistic style."; genres = ""; keywords = ""; tags = ""; yearMin = ""; yearMax = ""; matchAllInScope = false }) { Text("Film noir") }
            TextButton(onClick = { subject = ""; genres = ""; tags = ""; keywords = ""; yearMin = ""; yearMax = ""; matchAllInScope = true }) { Text("All in scope") }
        }
        Row { Checkbox(movies, { movies = it }); Text("Movies", Modifier.padding(top = 12.dp)); Checkbox(episodes, { episodes = it }); Text("Episodes", Modifier.padding(top = 12.dp)) }
        Text("Libraries", style = MaterialTheme.typography.labelLarge)
        home.libraries.filter { it.kind == "movies" || it.kind == "shows" }.forEach { library ->
            Row {
                Checkbox(library.id in libraryIds, { checked ->
                    libraryIds = if (checked) (libraryIds + library.id).distinct() else libraryIds - library.id
                })
                Text(library.name, Modifier.padding(top = 12.dp))
            }
        }
        TextButton(onClick = { showAdvanced = !showAdvanced }) { Text(if (showAdvanced) "Hide advanced metadata filters" else "Advanced metadata filters") }
        if (showAdvanced) {
        OutlinedTextField(genres, { genres = it }, label = { Text("Genres, comma separated") }, modifier = Modifier.fillMaxWidth())
        OutlinedTextField(tags, { tags = it }, label = { Text("Tags, comma separated") }, modifier = Modifier.fillMaxWidth())
        OutlinedTextField(keywords, { keywords = it }, label = { Text("Title keywords, comma separated") }, modifier = Modifier.fillMaxWidth())
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            OutlinedTextField(yearMin, { yearMin = it }, label = { Text("From year") }, modifier = Modifier.weight(1f))
            OutlinedTextField(yearMax, { yearMax = it }, label = { Text("Through year") }, modifier = Modifier.weight(1f))
        }
        }
        Text("Explicit titles and exclusions", style = MaterialTheme.typography.labelLarge)
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            OutlinedTextField(searchText, { searchText = it }, label = { Text("Search movies, series, episodes") }, modifier = Modifier.weight(1f))
            Button(enabled = searchText.trim().isNotEmpty() && !busy, onClick = {
                scope.launch {
                    runCatching { vm.api().search(searchText.trim(), 30).results }
                        .onSuccess { searchResults = it.filter { item -> item.kind in setOf("movie", "show", "episode") } }
                        .onFailure { message = it.message }
                }
            }) { Text("Search") }
        }
        searchResults.take(12).forEach { item ->
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                Text(item.title, modifier = Modifier.weight(1f), maxLines = 1)
                TextButton(onClick = {
                    if (item.kind == "show") includeShowIds = (includeShowIds + item.id).distinct()
                    else includeItemIds = (includeItemIds + item.id).distinct()
                }) { Text("Include") }
                TextButton(onClick = {
                    if (item.kind == "show") excludeShowIds = (excludeShowIds + item.id).distinct()
                    else excludeItemIds = (excludeItemIds + item.id).distinct()
                }) { Text("Exclude") }
            }
        }
        if (includeItemIds.isNotEmpty() || includeShowIds.isNotEmpty() || excludeItemIds.isNotEmpty() || excludeShowIds.isNotEmpty()) {
            Text("Included items ${includeItemIds.joinToString()} · series ${includeShowIds.joinToString()}", style = MaterialTheme.typography.bodySmall)
            Text("Excluded items ${excludeItemIds.joinToString()} · series ${excludeShowIds.joinToString()}", style = MaterialTheme.typography.bodySmall)
            includeItemIds.forEach { id -> TextButton(onClick = { includeItemIds = includeItemIds - id }) { Text("Remove included item $id") } }
            includeShowIds.forEach { id -> TextButton(onClick = { includeShowIds = includeShowIds - id }) { Text("Remove included series $id") } }
            excludeItemIds.forEach { id -> TextButton(onClick = { excludeItemIds = excludeItemIds - id }) { Text("Remove excluded item $id") } }
            excludeShowIds.forEach { id -> TextButton(onClick = { excludeShowIds = excludeShowIds - id }) { Text("Remove excluded series $id") } }
        }
        Button(enabled = (movies || episodes) && !busy, onClick = {
            busy = true
            scope.launch {
                try {
                    val captured = recipe()
                    if (subject.isNotBlank()) {
                        val result = vm.api().createSubjectPreview(SubjectPreviewRequest(captured, preview_seed = previewSeed))
                        if (captured == recipe()) { subjectRecipe = captured; subjectPreview = result; subjectJobId = result.job_id; previewSeed = result.preview_seed; preview = null }
                    } else {
                        val result = vm.api().previewLibraryChannel(LibraryChannelPreviewRequest(captured, preview_seed = previewSeed))
                        if (captured == recipe()) { preview = result; previewSeed = result.preview_seed; subjectPreview = null; subjectJobId = null }
                    }
                    message = null
                }
                catch (error: Exception) { message = error.message }
                finally { busy = false }
            }
        }) { Text("Preview matches") }
        subjectPreview?.let { result ->
            Text("${result.matched} matches; checked ${result.processed} of ${result.total}")
            Text(if (result.complete) "Scan complete" else "Partial selection · ${result.state}")
            result.error?.let { Text(it) }
            Row { listOf("match" to "Matched", "uncertain" to "Uncertain", "no_match" to "Excluded").forEach { (value, label) -> TextButton(onClick = { subjectFilter = value; subjectCursor = null }) { Text(label) } } }
            result.rows.forEach { row ->
                Text(row.title); Text(row.reason, style = MaterialTheme.typography.bodySmall)
                Row {
                    TextButton(onClick = { row.item_id.toLongOrNull()?.let { includeItemIds = (includeItemIds + it).distinct(); excludeItemIds = excludeItemIds - it } }) { Text("Include") }
                    TextButton(onClick = { row.item_id.toLongOrNull()?.let { excludeItemIds = (excludeItemIds + it).distinct() } }) { Text("Exclude") }
                }
            }
            result.next_cursor?.let { cursor -> TextButton(onClick = { subjectCursor = cursor }) { Text("More results") } }
        }
        preview?.let { result ->
            Text("${result.eligible_count} titles · ${result.repeat_description}")
            result.matches.forEach { match ->
                Text("${match.candidate.title} — ${match.reasons.joinToString(" · ")}", style = MaterialTheme.typography.bodySmall)
            }
        }
        }
        if (step == 1) {
        Text("2 · Playback", style = MaterialTheme.typography.titleMedium)
        ChoicePicker("Order", ordering, LibraryChannelOrdering.entries, { if (it == LibraryChannelOrdering.balanced_shuffle) "Balanced shuffle" else "Release order" }, { ordering = it })
        Row { Checkbox(specials, { specials = it }); Text("Include specials", Modifier.padding(top = 12.dp)) }
        Row { Checkbox(autoRefresh, { autoRefresh = it }); Text("Refresh future rotations automatically", Modifier.padding(top = 12.dp)) }
        preview?.let { Text("${it.eligible_count} titles · ${it.repeat_description}") }
        }
        if (step == 2) {
        Text("3 · Channel", style = MaterialTheme.typography.titleMedium)
        OutlinedTextField(name, { name = it }, label = { Text("Name") }, modifier = Modifier.fillMaxWidth())
        OutlinedTextField(description, { description = it }, label = { Text("Description") }, modifier = Modifier.fillMaxWidth())
        if (canShare) {
            Row { Checkbox(shared, { shared = it }); Text("Shared with every viewer", Modifier.padding(top = 12.dp)) }
        } else {
            Text("Personal channel · an administrator can make it shared", style = MaterialTheme.typography.bodySmall)
        }
        Row { Checkbox(enabled, { enabled = it }); Text("Enabled", Modifier.padding(top = 12.dp)) }
        channel?.let { existing ->
            Button(enabled = !busy, onClick = {
                scope.launch {
                    runCatching { vm.api().rebuildLibraryChannel(existing.id, LibraryChannelRebuild(existing.revision, activation = "next_programme")) }
                        .onSuccess { message = "The new schedule will begin after this programme." }
                        .onFailure { message = it.message }
                }
            }) { Text("Apply after this programme") }
            Button(enabled = !busy, onClick = {
                scope.launch {
                    runCatching { vm.api().rebuildLibraryChannel(existing.id, LibraryChannelRebuild(existing.revision, activation = "next_rotation", reshuffle = true)) }
                        .onSuccess { message = "The next rotation was reshuffled." }
                        .onFailure { message = it.message }
                }
            }) { Text("Reshuffle next rotation") }
            TextButton(enabled = !busy, onClick = {
                scope.launch {
                    runCatching { vm.api().deleteLibraryChannel(existing.id, existing.revision) }
                        .onSuccess {
                            preferences.edit().remove(draftKey).remove("$draftKey:step").remove("$draftKey:subjectJob").remove("$draftKey:subjectRecipe").remove("$draftKey:saveAttempt").apply()
                            onSaved()
                        }.onFailure { message = it.message }
                }
            }) { Text("Delete channel") }
        }
        }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Button(enabled = valid && !busy, onClick = {
                busy = true
                scope.launch {
                    try {
                        val definition = saveAttempt?.takeIf { it.copy(request_id = "", preview_seed = null) == persistedDefinition.copy(request_id = "", preview_seed = null) } ?: persistedDefinition
                        saveAttempt = definition
                        preferences.edit().putString("$draftKey:saveAttempt", Json.encodeToString(definition)).commit()
                        if (channel == null) vm.api().createLibraryChannel(definition)
                        else vm.api().updateLibraryChannel(channel.id, LibraryChannelUpdate(
                            expected_revision = channel.revision,
                            request_id = definition.request_id,
                            name = definition.name,
                            description = definition.description,
                            visibility = definition.visibility,
                            enabled = definition.enabled,
                            recipe = definition.recipe,
                            preview_seed = definition.preview_seed,
                        ))
                        preferences.edit().remove(draftKey).remove("$draftKey:step").remove("$draftKey:subjectJob").remove("$draftKey:subjectRecipe").remove("$draftKey:saveAttempt").apply()
                        onSaved()
                    } catch (error: Exception) { message = error.message; busy = false }
                }
            }) { Text("Save") }
        }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            if (step > 0) TextButton(onClick = { step -= 1 }) { Text("Back") }
            if (step < 2) Button(enabled = step != 0 || movies || episodes, onClick = { step += 1 }) { Text("Next") }
            TextButton(onClick = onCancel) { Text("Cancel") }
        }
        message?.let { Text(it, color = MaterialTheme.colorScheme.error) }
    }
}
