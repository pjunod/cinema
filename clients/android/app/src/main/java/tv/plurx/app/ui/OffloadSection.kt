package tv.plurx.app.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import tv.plurx.app.data.Item
import tv.plurx.app.data.OFFLOAD_UNREACHABLE
import tv.plurx.app.data.offloadStatusLine
import tv.plurx.app.ui.theme.Muted

/**
 * Offload: Curator removed this title's files to free disk. There is no file,
 * so Play is not offered; an administrator can ask Curator for it back and
 * everyone sees where it stands. Nothing is stored here: the status line is
 * read from Curator while the page is open.
 */
@Composable
internal fun OffloadSection(vm: AppViewModel, item: Item) {
    val admin = vm.currentUser?.is_admin == true
    val scope = rememberCoroutineScope()
    var line by remember(item.id) { mutableStateOf("Offloaded \u00b7 the files were removed to free disk space") }
    var asking by remember(item.id) { mutableStateOf(false) }
    if (item.offloaded) {
        LaunchedEffect(item.id) {
            while (true) {
                line = try {
                    offloadStatusLine(vm.offloadStatus(item.id))
                } catch (e: CancellationException) {
                    throw e
                } catch (e: Exception) {
                    OFFLOAD_UNREACHABLE
                }
                delay(5_000)
            }
        }
    }
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        if (admin) {
            Button(
                enabled = !asking,
                modifier = Modifier.testTag("offload-get-it-back"),
                onClick = {
                    asking = true
                    line = "Asking Curator\u2026"
                    scope.launch {
                        line = try {
                            "Get it back: " + vm.getItBack(item.id).message
                        } catch (e: CancellationException) {
                            throw e
                        } catch (e: Exception) {
                            e.message ?: "Curator could not be asked."
                        }
                        asking = false
                    }
                },
            ) { Text(if (item.offloaded) "Get it back" else "Get ${item.rollup?.offloaded ?: 0} back") }
        }
        Text(line, color = Muted, style = MaterialTheme.typography.bodyMedium, modifier = Modifier.testTag("offload-status"))
    }
}
