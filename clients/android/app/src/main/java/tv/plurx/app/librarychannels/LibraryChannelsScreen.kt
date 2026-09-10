@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.librarychannels

import android.view.ViewGroup
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
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
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.media3.ui.PlayerView
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
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
    onOpenItem: (Long) -> Unit,
    onBack: () -> Unit,
) {
    val context = LocalContext.current
    val controller = remember(context) { LibraryChannelPlayer.get(context) }
    val state by controller.state.collectAsStateWithLifecycle()
    val television = currentFormFactor() == FormFactor.Television
    var editor by remember { mutableStateOf<LibraryChannel?>(null) }
    var creating by remember { mutableStateOf(false) }
    BackHandler(onBack = onBack)

    LaunchedEffect(vm.origin) {
        controller.load(vm.origin)
        while (true) {
            delay(30_000)
            controller.refresh()
        }
    }

    Column(
        Modifier.fillMaxSize().windowInsetsPadding(safeDisplayInsets()).padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
            TextButton(onClick = onBack) { Text("Back") }
            if (!television && vm.currentUser?.is_admin == true) {
                Button(onClick = { creating = true }) { Text("Make a channel") }
            }
        }
        Text("Library channels", style = MaterialTheme.typography.headlineMedium)
        Text(state.message, style = MaterialTheme.typography.bodySmall)
        if (television) {
            Row(Modifier.fillMaxSize(), horizontalArrangement = Arrangement.spacedBy(18.dp)) {
                LibraryChannelPlayerPane(controller, state, onOpenItem, Modifier.weight(1.35f))
                LibraryChannelList(controller, state.channels, state.programmes, false, { editor = it }, Modifier.weight(1f))
            }
        } else {
            LibraryChannelPlayerPane(controller, state, onOpenItem, Modifier.fillMaxWidth().height(280.dp))
            LibraryChannelList(controller, state.channels, state.programmes, true, { editor = it }, Modifier.weight(1f))
        }
    }

    if (creating || editor != null) {
        ModalBottomSheet(onDismissRequest = { creating = false; editor = null }) {
            LibraryChannelEditor(
                vm = vm,
                channel = editor,
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
    onOpenItem: (Long) -> Unit,
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
                    Button(onClick = { controller.stop(); onOpenItem(resolved.item_id) }) { Text("Watch from start") }
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
                programmes.asSequence().filter { it.channel_id == channel.id }.take(4).forEach { programme ->
                    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                        Text(programme.title, modifier = Modifier.weight(1f), maxLines = 1)
                        Text(DateFormat.getTimeInstance(DateFormat.SHORT).format(Date(programme.starts_at_ms)), style = MaterialTheme.typography.labelSmall)
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

@Composable
private fun LibraryChannelEditor(
    vm: AppViewModel,
    channel: LibraryChannel?,
    onSaved: () -> Unit,
    onCancel: () -> Unit,
) {
    val scope = rememberCoroutineScope()
    var name by remember(channel) { mutableStateOf(channel?.name.orEmpty()) }
    var description by remember(channel) { mutableStateOf(channel?.description.orEmpty()) }
    var shared by remember(channel) { mutableStateOf(channel?.visibility == LibraryChannelVisibility.shared) }
    var enabled by remember(channel) { mutableStateOf(channel?.enabled ?: true) }
    var movies by remember(channel) { mutableStateOf(channel?.recipe?.kinds?.contains("movie") ?: true) }
    var episodes by remember(channel) { mutableStateOf(channel?.recipe?.kinds?.contains("episode") ?: true) }
    var genres by remember(channel) { mutableStateOf(channel?.recipe?.genres_any?.joinToString(", ").orEmpty()) }
    var tags by remember(channel) { mutableStateOf(channel?.recipe?.tags_any?.joinToString(", ").orEmpty()) }
    var keywords by remember(channel) { mutableStateOf(channel?.recipe?.keywords_any?.joinToString(", ").orEmpty()) }
    var yearMin by remember(channel) { mutableStateOf(channel?.recipe?.year_min?.toString().orEmpty()) }
    var yearMax by remember(channel) { mutableStateOf(channel?.recipe?.year_max?.toString().orEmpty()) }
    var ordering by remember(channel) { mutableStateOf(channel?.recipe?.ordering ?: LibraryChannelOrdering.balanced_shuffle) }
    var specials by remember(channel) { mutableStateOf(channel?.recipe?.include_specials ?: false) }
    var autoRefresh by remember(channel) { mutableStateOf(channel?.recipe?.auto_refresh ?: true) }
    var preview by remember { mutableStateOf<LibraryChannelPreview?>(null) }
    var busy by remember { mutableStateOf(false) }
    var message by remember { mutableStateOf<String?>(null) }

    fun words(value: String) = value.split(',').map(String::trim).filter(String::isNotEmpty)
    fun recipe(): LibraryChannelRecipe {
        val old = channel?.recipe ?: LibraryChannelRecipe()
        val genresAny = words(genres)
        val tagsAny = words(tags)
        val keywordsAny = words(keywords)
        val min = yearMin.toIntOrNull()
        val max = yearMax.toIntOrNull()
        return old.copy(
            kinds = buildList { if (movies) add("movie"); if (episodes) add("episode") },
            genres_any = genresAny,
            tags_any = tagsAny,
            keywords_any = keywordsAny,
            year_min = min,
            year_max = max,
            ordering = ordering,
            include_specials = specials,
            auto_refresh = autoRefresh,
            match_all_in_scope = old.library_ids.isEmpty() && genresAny.isEmpty() && tagsAny.isEmpty() &&
                keywordsAny.isEmpty() && min == null && max == null && old.include_item_ids.isEmpty() && old.include_show_ids.isEmpty(),
        )
    }
    val valid = name.trim().isNotEmpty() && name.length <= 80 && description.length <= 500 && (movies || episodes)

    Column(
        Modifier.fillMaxHeight(0.92f).verticalScroll(rememberScrollState()).padding(horizontal = 20.dp, vertical = 8.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Text(if (channel == null) "Make a channel" else "Edit channel", style = MaterialTheme.typography.headlineSmall)
        Text("1 · Content", style = MaterialTheme.typography.titleMedium)
        Row { Checkbox(movies, { movies = it }); Text("Movies", Modifier.padding(top = 12.dp)); Checkbox(episodes, { episodes = it }); Text("Episodes", Modifier.padding(top = 12.dp)) }
        OutlinedTextField(genres, { genres = it }, label = { Text("Genres, comma separated") }, modifier = Modifier.fillMaxWidth())
        OutlinedTextField(tags, { tags = it }, label = { Text("Tags, comma separated") }, modifier = Modifier.fillMaxWidth())
        OutlinedTextField(keywords, { keywords = it }, label = { Text("Title keywords, comma separated") }, modifier = Modifier.fillMaxWidth())
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            OutlinedTextField(yearMin, { yearMin = it }, label = { Text("From year") }, modifier = Modifier.weight(1f))
            OutlinedTextField(yearMax, { yearMax = it }, label = { Text("Through year") }, modifier = Modifier.weight(1f))
        }
        Button(enabled = valid && !busy, onClick = {
            busy = true
            scope.launch {
                try { preview = vm.api().previewLibraryChannel(LibraryChannelPreviewRequest(recipe())); message = null }
                catch (error: Exception) { message = error.message }
                finally { busy = false }
            }
        }) { Text("Preview matches") }
        preview?.let { result ->
            Text("${result.eligible_count} titles · ${result.repeat_description}")
            result.matches.take(10).forEach { match ->
                Text("${match.candidate.title} — ${match.reasons.joinToString(" · ")}", style = MaterialTheme.typography.bodySmall)
            }
        }
        Text("2 · Playback", style = MaterialTheme.typography.titleMedium)
        ChoicePicker("Order", ordering, LibraryChannelOrdering.entries, { if (it == LibraryChannelOrdering.balanced_shuffle) "Balanced shuffle" else "Release order" }, { ordering = it })
        Row { Checkbox(specials, { specials = it }); Text("Include specials", Modifier.padding(top = 12.dp)) }
        Row { Checkbox(autoRefresh, { autoRefresh = it }); Text("Refresh future rotations automatically", Modifier.padding(top = 12.dp)) }
        Text("3 · Channel", style = MaterialTheme.typography.titleMedium)
        OutlinedTextField(name, { name = it }, label = { Text("Name") }, modifier = Modifier.fillMaxWidth())
        OutlinedTextField(description, { description = it }, label = { Text("Description") }, modifier = Modifier.fillMaxWidth())
        Row { Checkbox(shared, { shared = it }); Text("Shared with every viewer", Modifier.padding(top = 12.dp)) }
        Row { Checkbox(enabled, { enabled = it }); Text("Enabled", Modifier.padding(top = 12.dp)) }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Button(enabled = valid && !busy, onClick = {
                busy = true
                scope.launch {
                    try {
                        val definition = LibraryChannelDefinition(
                            name = name.trim(), description = description.trim(),
                            visibility = if (shared) LibraryChannelVisibility.shared else LibraryChannelVisibility.personal,
                            enabled = enabled, recipe = recipe(),
                        )
                        if (channel == null) vm.api().createLibraryChannel(definition)
                        else vm.api().updateLibraryChannel(channel.id, LibraryChannelUpdate(
                            expected_revision = channel.revision,
                            name = definition.name,
                            description = definition.description,
                            visibility = definition.visibility,
                            enabled = definition.enabled,
                            recipe = definition.recipe,
                        ))
                        onSaved()
                    } catch (error: Exception) { message = error.message; busy = false }
                }
            }) { Text("Save") }
            TextButton(onClick = onCancel) { Text("Cancel") }
        }
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
                        .onSuccess { onSaved() }.onFailure { message = it.message }
                }
            }) { Text("Delete channel") }
        }
        message?.let { Text(it, color = MaterialTheme.colorScheme.error) }
    }
}
