package tv.plurx.app.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import tv.plurx.app.data.*
import tv.plurx.app.player.captionRead
import tv.plurx.app.player.captionSavedProfile
import tv.plurx.app.ui.components.ChoicePicker
import java.util.UUID

/** Administrator preferences save independently of local worker qualification. */
@Composable
internal fun CaptionGenerationSettings(vm: AppViewModel) {
    if (vm.currentUser?.is_admin != true) {
        Text("Cached generated captions are available in the player. An administrator manages caption models.",
            style = MaterialTheme.typography.bodySmall)
        return
    }
    val scope = rememberCoroutineScope()
    val authorization = remember { Session.playbackAuthorization().generation }
    fun authorized() = Session.playbackAuthorization().generation == authorization
    var saved by remember { mutableStateOf<CaptionSettings?>(null) }
    var catalog by remember { mutableStateOf<CaptionCatalog?>(null) }
    var profile by remember { mutableStateOf<CaptionProfile?>(null) }
    var language by remember { mutableStateOf("en") }
    var enabled by remember { mutableStateOf(false) }
    var message by remember { mutableStateOf("Loading caption settings…") }
    var busy by remember { mutableStateOf(false) }
    var setupJob by remember { mutableStateOf<Job?>(null) }

    suspend fun refresh() {
        val settings = captionRead<CaptionSettings>(vm.api().captionSettings())
        val models = captionRead<CaptionCatalog>(vm.api().captionModels())
        if (!authorized()) return
        require(settings.version == 1 && models.version == 1 && models.profiles.size <= 16 && models.workers.size <= 32)
        saved = settings; catalog = models; enabled = settings.enabled
        profile = captionSavedProfile(settings, models.profiles, models.default_profile_id)
        language = settings.preferred_spoken_language ?: profile?.input_languages?.firstOrNull() ?: "en"
        message = if (profile == null) "The saved caption model is unavailable. Choose a model and task below."
            else "Model readiness is advisory. Download and test the selected task on this worker."
    }
    LaunchedEffect(authorization) {
        try { refresh() }
        catch (e: CancellationException) { throw e }
        catch (e: Exception) { if (authorized()) message = e.message ?: "Caption settings unavailable" }
    }
    fun modelAction(action: String) {
        val selected = profile ?: return
        val current = catalog?.workers?.singleOrNull { it.current } ?: return
        setupJob?.cancel()
        setupJob = scope.launch {
            busy = true
            try {
                val clock = captionRead<CaptionSettings>(vm.api().captionSettings()).server_now_ms
                if (!authorized()) return@launch
                val result = captionRead<CaptionModelReceipt>(vm.api().captionModelAction(selected.model_id, action,
                    CaptionModelRequest(2, UUID.randomUUID().toString(), clock + 60_000, "translation", selected.profile_id, listOf(current.node_id))))
                if (!authorized()) return@launch
                val accepted = result.operations.singleOrNull() ?: error("Invalid model operation receipt")
                val id = accepted.job_id ?: error("Model action refused: ${accepted.outcome}")
                while (isActive && authorized()) {
                    val job = captionRead<CaptionModelJob>(vm.api().captionModelJob(id))
                    if (!authorized()) return@launch
                    message = "${job.phase.replace('_', ' ')}${job.reason?.let { ": $it" } ?: ""}"
                    if (job.state in setOf("succeeded", "failed", "cancelled")) {
                        if (job.state == "succeeded") { refresh(); message = "${if (action == "install") "Model download" else "Model test"} succeeded." }
                        else message = "${job.state}: ${job.reason ?: job.phase}"
                        break
                    }
                    delay(1_000)
                }
            } catch (e: CancellationException) { throw e }
            catch (e: Exception) { if (authorized()) message = e.message ?: "Model action failed" }
            finally { busy = false }
        }
    }
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text("Generated captions", style = MaterialTheme.typography.titleMedium)
        Text("Generate text in the spoken language, or translate speech to English. Processing runs on your server.",
            style = MaterialTheme.typography.bodySmall)
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween, verticalAlignment = Alignment.CenterVertically) {
            Text("Enable caption generation")
            Switch(checked = enabled, onCheckedChange = { enabled = it }, enabled = saved != null && !busy)
        }
        val profiles = catalog?.profiles.orEmpty()
        if (profile == null) profiles.forEach { available ->
            TextButton(onClick = { profile = available; language = available.input_languages.firstOrNull() ?: "en" }) {
                Text(available.label ?: "${available.model_id} — ${available.task}")
            }
        }
        profile?.let { selected ->
            ChoicePicker("Model and task", selected, profiles, {
                val model = catalog?.models?.firstOrNull { model -> model.id == it.model_id }
                "${model?.label ?: it.model_id} — ${if (it.task == "transcribe") "spoken-language captions" else "English translation"}"
            }, { profile = it; if (language !in it.input_languages) language = it.input_languages.firstOrNull() ?: "en" })
            if (selected.input_languages.isNotEmpty()) ChoicePicker("Preferred spoken language", language, selected.input_languages, { it }, { language = it })
            catalog?.models?.firstOrNull { it.id == selected.model_id }?.let {
                Text("Download size: ${it.download_bytes / (1024 * 1024)} MiB", style = MaterialTheme.typography.bodySmall)
            }
            catalog?.workers?.filter { it.current }?.forEach { worker ->
                val capability = worker.capabilities.firstOrNull { it.profile_id == selected.profile_id }
                Text("${worker.label}: ${capability?.status?.replace('_', ' ') ?: "unknown"}${capability?.reason?.let { ": $it" } ?: ""}",
                    style = MaterialTheme.typography.bodySmall)
            }
            Row {
                TextButton(onClick = { modelAction("install") }, enabled = !busy && catalog?.workers?.count { it.current } == 1) { Text("Download model") }
                TextButton(onClick = { modelAction("verify") }, enabled = !busy && catalog?.workers?.count { it.current } == 1) { Text("Test model") }
            }
        }
        TextButton(onClick = {
            val old = saved ?: return@TextButton
            val selected = profile ?: return@TextButton
            scope.launch {
                busy = true
                try {
                    // Refresh the server clock without changing the viewer's unsaved preference.
                    val current = captionRead<CaptionSettings>(vm.api().captionSettings())
                    if (!authorized()) return@launch
                    val answer = captionRead<CaptionSettings>(vm.api().saveCaptionSettings(CaptionSettingsWrite(1,
                        UUID.randomUUID().toString(), current.server_now_ms + 60_000, old.revision, enabled,
                        selected.model_id, selected.profile_id, language)))
                    if (!authorized()) return@launch
                    saved = answer; enabled = answer.enabled
                    message = "Preference saved. Worker readiness does not change the saved choice."
                } catch (e: CancellationException) { throw e }
                catch (e: Exception) { if (authorized()) message = e.message ?: "Save failed; refresh settings" }
                finally { busy = false }
            }
        }, enabled = saved != null && profile != null && !busy) { Text("Save generation preferences") }
        Text(message, style = MaterialTheme.typography.bodySmall)
        TextButton(onClick = { scope.launch { try { refresh() } catch (e: CancellationException) { throw e } catch (e: Exception) { message = e.message ?: "Refresh failed" } } },
            enabled = !busy) { Text("Refresh caption settings") }
    }
}
