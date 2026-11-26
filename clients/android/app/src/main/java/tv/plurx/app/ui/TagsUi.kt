package tv.plurx.app.ui

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.selection.toggleable
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Lock
import androidx.compose.material3.Checkbox
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import retrofit2.HttpException
import tv.plurx.app.data.Tag
import tv.plurx.app.data.checklistTags
import tv.plurx.app.data.tagColorArgb
import tv.plurx.app.data.tagIdsToSend
import tv.plurx.app.ui.components.RequestInitialFocus
import tv.plurx.app.ui.components.TvOutlinedButton
import tv.plurx.app.ui.components.TvTextButton
import tv.plurx.app.ui.theme.Muted

/** One tag as a small coloured chip; a private tag carries a lock. */
@Composable
internal fun TagChip(tag: Tag) {
    val color = Color(tagColorArgb(tag.color))
    Row(
        Modifier.border(BorderStroke(1.dp, color), MaterialTheme.shapes.small).padding(horizontal = 8.dp, vertical = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        if (tag.isPrivate) Icon(Icons.Filled.Lock, contentDescription = "Private tag", tint = color, modifier = Modifier.size(12.dp))
        Text(tag.name, color = color, style = MaterialTheme.typography.labelMedium, maxLines = 1)
    }
}

/**
 * Tag chips under a movie's or show's title and the "Tags…" editor. The chips
 * and the catalog arrive beside the page; neither ever holds it.
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun ItemTagsSection(vm: AppViewModel, itemId: Long) {
    var tags by remember(itemId) { mutableStateOf<List<Tag>>(emptyList()) }
    var editing by remember(itemId) { mutableStateOf(false) }
    LaunchedEffect(itemId) {
        tags = catchingUnlessCancelled { vm.itemTags(itemId) }.getOrDefault(emptyList())
    }
    if (tags.isNotEmpty()) {
        FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            tags.forEach { TagChip(it) }
        }
    }
    TvOutlinedButton(onClick = { editing = true }) { Text("Tags…") }
    if (editing) TagChecklistDialog(vm, itemId, tags, onTags = { tags = it }, onDismiss = { editing = false })
}

@Composable
private fun TagChecklistDialog(
    vm: AppViewModel,
    itemId: Long,
    current: List<Tag>,
    onTags: (List<Tag>) -> Unit,
    onDismiss: () -> Unit,
) {
    val scope = rememberCoroutineScope()
    val tv = currentFormFactor() == FormFactor.Television
    var catalog by remember { mutableStateOf<List<Tag>?>(null) }
    var ticked by remember { mutableStateOf(current.map { it.id }.toSet()) }
    var busy by remember { mutableStateOf(false) }
    var message by remember { mutableStateOf<String?>(null) }
    var newName by remember { mutableStateOf("") }
    LaunchedEffect(Unit) {
        val loaded = catchingUnlessCancelled { vm.tagCatalog().tags }
        catalog = loaded.getOrDefault(emptyList())
        if (loaded.isFailure) message = "Couldn't load tags"
    }

    /** Send every ticked editable id and redraw from the answer. */
    fun save(next: Set<Long>) {
        val all = catalog ?: return
        val before = ticked
        ticked = next
        busy = true
        scope.launch {
            try {
                val answer = vm.setItemTags(itemId, tagIdsToSend(all, next, current))
                onTags(answer); ticked = answer.map { it.id }.toSet(); message = null
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                message = if ((e as? HttpException)?.code() == 403) "You can't change that tag" else "Couldn't save tags"
                // Redraw from what the server holds, falling back to the pre-toggle state.
                val fresh = catchingUnlessCancelled { vm.itemTags(itemId) }.getOrNull()
                if (fresh != null) { onTags(fresh); ticked = fresh.map { it.id }.toSet() } else ticked = before
            } finally {
                busy = false
            }
        }
    }

    fun createTag() {
        val name = newName.trim()
        if (name.isEmpty() || busy) return
        busy = true
        scope.launch {
            try {
                val tag = vm.createPrivateTag(name)
                if (catalog.orEmpty().none { it.id == tag.id }) catalog = catalog.orEmpty() + tag
                newName = ""
                busy = false
                save(ticked + tag.id)
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                message = "Couldn't create that tag"
                busy = false
            }
        }
    }

    Dialog(onDismissRequest = onDismiss, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        Surface(shape = MaterialTheme.shapes.large, modifier = Modifier.fillMaxWidth(if (tv) .5f else .92f)) {
            Column(Modifier.padding(20.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                Text("Tags", style = MaterialTheme.typography.titleLarge)
                val rows = catalog?.let { checklistTags(it) }
                val firstFocus = remember { FocusRequester() }
                when {
                    rows == null -> Text("Loading…", color = Muted)
                    rows.isEmpty() -> Text("No tags you can edit yet.", color = Muted)
                    else -> {
                        RequestInitialFocus(firstFocus)
                        LazyColumn(Modifier.heightIn(max = 360.dp)) {
                            items(rows, key = { it.id }) { tag ->
                                val on = tag.id in ticked
                                Row(
                                    Modifier.fillMaxWidth()
                                        .then(if (tag.id == rows.first().id) Modifier.focusRequester(firstFocus) else Modifier)
                                        .toggleable(value = on, enabled = !busy, role = Role.Checkbox) {
                                            save(if (it) ticked + tag.id else ticked - tag.id)
                                        }
                                        .padding(vertical = 6.dp),
                                    verticalAlignment = Alignment.CenterVertically,
                                    horizontalArrangement = Arrangement.spacedBy(10.dp),
                                ) {
                                    Checkbox(checked = on, onCheckedChange = null)
                                    TagChip(tag)
                                }
                            }
                        }
                    }
                }
                if (!tv) {
                    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        OutlinedTextField(
                            value = newName, onValueChange = { newName = it }, label = { Text("New tag") },
                            singleLine = true, modifier = Modifier.weight(1f),
                        )
                        TvTextButton(onClick = { createTag() }, enabled = newName.isNotBlank() && !busy && catalog != null) { Text("Add") }
                    }
                }
                message?.let { Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall) }
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
                    TvTextButton(onClick = onDismiss) { Text("Done") }
                }
            }
        }
    }
}
