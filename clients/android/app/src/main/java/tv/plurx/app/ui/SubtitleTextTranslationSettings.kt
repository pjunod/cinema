package tv.plurx.app.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import tv.plurx.app.data.*
import tv.plurx.app.player.captionRead
import tv.plurx.app.player.captionCounter
import tv.plurx.app.ui.components.ChoicePicker
import java.util.UUID

@Composable
internal fun SubtitleTextTranslationSettings(vm: AppViewModel) {
    if (vm.currentUser?.is_admin != true) return
    val scope = rememberCoroutineScope()
    val authorization = remember { Session.playbackAuthorization().generation }
    fun authorized() = Session.playbackAuthorization().generation == authorization
    var saved by remember { mutableStateOf<SubtitleTextConfig?>(null) }
    var catalog by remember { mutableStateOf<SubtitleTextCatalog?>(null) }
    var worker by remember { mutableStateOf<SubtitleTextWorker?>(null) }
    var enabled by remember { mutableStateOf(false) }
    var busy by remember { mutableStateOf(false) }
    var message by remember { mutableStateOf("Loading text translation settings…") }
    suspend fun refresh() {
        val config = captionRead<SubtitleTextConfig>(vm.api().subtitleTextConfig())
        val models = captionRead<SubtitleTextCatalog>(vm.api().subtitleTextModels())
        if (!authorized()) return
        require(config.version == 1 && config.target_language == "en" && models.version == 1 && models.workers.size <= 32)
        captionCounter(config.revision)
        saved = config; catalog = models; enabled = config.enabled
        worker = models.workers.firstOrNull { it.node_id == worker?.node_id } ?: models.workers.firstOrNull()
        message = "This model translates subtitle text to English. It does not transcribe audio or read bitmap subtitles."
    }
    LaunchedEffect(authorization) {
        try { refresh() }
        catch (e: CancellationException) { throw e }
        catch (e: Exception) { if (authorized()) message = e.message ?: "Text translation settings unavailable" }
    }
    fun setup(action: String) {
        val node = worker ?: return
        val config = saved ?: return
        scope.launch {
            busy = true
            try {
                val result = captionRead<SubtitleTextJobReceipt>(vm.api().subtitleTextModelAction(action,
                    SubtitleTextModelRequest(1, UUID.randomUUID().toString(), node.node_id, node.boot_id, config.model_id)))
                require(result.version == 1)
                while (isActive && authorized()) {
                    val job = captionRead<SubtitleTextJob>(vm.api().subtitleTextJob(result.job_id))
                    if (!authorized()) return@launch
                    require(job.version == 1 && job.job_id == result.job_id)
                    message = "${job.phase ?: job.state}: ${job.progress.completed}/${job.progress.total}${job.reason?.let { ": $it" } ?: ""}"
                    if (job.state in setOf("succeeded", "failed", "cancelled")) {
                        if (job.state == "succeeded") { refresh(); message = "Text translation model ${if (action == "install") "download" else "test"} succeeded." }
                        break
                    }
                    delay(1_000)
                }
            } catch (e: CancellationException) { throw e }
            catch (e: Exception) { if (authorized()) message = e.message ?: "Text model action failed" }
            finally { busy = false }
        }
    }
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text("Translate existing subtitles", style = MaterialTheme.typography.titleMedium)
        Text("Local Marian multilingual → English model", style = MaterialTheme.typography.bodySmall)
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween, verticalAlignment = Alignment.CenterVertically) {
            Text("Enable subtitle text translation")
            Switch(enabled, { enabled = it }, enabled = saved != null && !busy)
        }
        worker?.let { node ->
            ChoicePicker("Model worker", node, catalog?.workers.orEmpty(), { it.label }, { worker = it })
            Text("${node.status.replace('_', ' ')}${node.reason?.let { ": $it" } ?: ""}", style = MaterialTheme.typography.bodySmall)
            Row {
                TextButton(onClick = { setup("install") }, enabled = !busy) { Text("Download text model") }
                TextButton(onClick = { setup("verify") }, enabled = !busy) { Text("Test text model") }
            }
        }
        TextButton(onClick = {
            val current = saved ?: return@TextButton
            scope.launch {
                busy = true
                try {
                    val value = captionRead<SubtitleTextConfig>(vm.api().saveSubtitleTextConfig(
                        SubtitleTextConfigWrite(1, current.revision, enabled, current.model_id)))
                    if (!authorized()) return@launch
                    require(value.version == 1); saved = value; enabled = value.enabled
                    message = "Preference saved independently of model readiness."
                } catch (e: CancellationException) { throw e }
                catch (e: Exception) { if (authorized()) message = e.message ?: "Save failed; refresh settings" }
                finally { busy = false }
            }
        }, enabled = saved != null && !busy) { Text("Save text translation preference") }
        Text(message, style = MaterialTheme.typography.bodySmall)
        TextButton(onClick = { scope.launch { try { refresh() } catch (e: CancellationException) { throw e } catch (e: Exception) { message = e.message ?: "Refresh failed" } } },
            enabled = !busy) { Text("Refresh text translation settings") }
    }
}
