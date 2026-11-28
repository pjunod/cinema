package tv.plurx.app.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import tv.plurx.app.data.AISubtitleDownload
import tv.plurx.app.data.AISubtitleMatch
import tv.plurx.app.data.AISubtitleOperation
import tv.plurx.app.data.AISubtitleOptions
import tv.plurx.app.data.AISubtitleStart
import tv.plurx.app.data.AISubtitleTiming
import tv.plurx.app.data.MediaFileDto
import tv.plurx.app.data.Session
import tv.plurx.app.player.captionRead
import tv.plurx.app.ui.components.TvTextButton
import tv.plurx.app.ui.theme.Muted
import java.util.UUID

@Composable
internal fun AISubtitleItemSection(vm: AppViewModel, file: MediaFileDto, enabled: Boolean, choose: (Long) -> Unit) {
    val auth = Session.playbackAuthorization().generation
    val scope = rememberCoroutineScope()
    var opened by remember(file.id, auth) { mutableStateOf(false) }
    var settings by remember(file.id, auth) { mutableStateOf<AISubtitleOptions?>(null) }
    var operation by remember(file.id, auth) { mutableStateOf<AISubtitleOperation?>(null) }
    var removed by remember(file.id, auth) { mutableStateOf(emptySet<String>()) }
    var matches by remember(file.id, auth) { mutableStateOf(emptyList<AISubtitleMatch>()) }
    var status by remember(file.id, auth) { mutableStateOf("") }
    var busy by remember(file.id, auth) { mutableStateOf(false) }
    fun current() = auth == Session.playbackAuthorization().generation
    val stored = file.subtitle_streams.filter { it.ai && it.track_id != null && it.track_id !in removed }
    val language = file.playback_defaults?.subtitle?.preferred_language?.ifBlank { "en" } ?: "en"
    val languageName = java.util.Locale.forLanguageTag(language).displayLanguage
    val hasPreferred = file.subtitle_streams.any {
        it.language?.let { code -> java.util.Locale.forLanguageTag(code).displayLanguage == languageName } == true &&
            !it.forced && it.track_id !in removed
    }
    suspend fun fetch(): AISubtitleOptions = captionRead(vm.api().aiSubtitleOptions(file.id, target = language))
    fun start(route: String, quality: String? = null) {
        busy = true
        scope.launch {
            try {
                val request = AISubtitleStart(UUID.randomUUID().toString(), route = route, quality = quality)
                val result = captionRead<AISubtitleOperation>(vm.api().aiSubtitleStart(file.id, request))
                if (current()) operation = result
            } catch (e: CancellationException) { throw e }
            catch (e: Exception) { if (current()) status = e.message ?: "AI subtitles could not start." }
            finally { busy = false }
        }
    }
    LaunchedEffect(opened, operation?.id, auth, file.id) {
        if (!opened) return@LaunchedEffect
        try {
            val options = fetch()
            if (!current()) return@LaunchedEffect
            settings = options
            if (operation == null) operation = options.operation
            while (current() && operation?.active == true) {
                delay(3_000)
                val result = fetch()
                if (current()) operation = result.operation
            }
            val done = operation
            if (current() && done?.state == "complete") {
                val track = vm.decision(file.id).subtitles.singleOrNull { track ->
                    done.track_id?.let { track.downloaded_subtitle_id == it } ?: (track.index == done.track_index)
                }
                if (current() && track != null) choose(track.index)
            }
        } catch (e: CancellationException) { throw e }
        catch (e: Exception) { if (current()) status = e.message ?: "Subtitle status unavailable." }
    }
    if (!hasPreferred || stored.isNotEmpty() || operation?.active == true) {
        TvTextButton(enabled = enabled, onClick = { opened = !opened }) {
            Text(if (stored.isNotEmpty()) "Manage English AI subtitles" else "+ ${languageName}")
        }
    }
    if (!opened) return
    Column {
        val options = settings
        if (options?.allowed == true && options.existing_index == null && options.route != null) {
            TvTextButton(enabled = !busy && operation?.active != true, onClick = { start("auto") }) {
                Text(options.label ?: "AI subtitles")
            }
            options.alternatives.forEach { choice ->
                TvTextButton(enabled = !busy && operation?.active != true, onClick = { start(choice.route) }) { Text(choice.label) }
            }
        }
        if (options?.search_online == true) {
            TvTextButton(enabled = !busy, onClick = {
                busy = true
                scope.launch {
                    try {
                        val found = vm.api().aiSubtitleSearch(file.id, options.target ?: language).results
                        if (current()) { matches = found; status = if (found.isEmpty()) "No subtitle matches found." else "" }
                    } catch (e: CancellationException) { throw e }
                    catch (e: Exception) { if (current()) status = e.message ?: "Subtitle search failed." }
                    finally { busy = false }
                }
            }) { Text("Search online") }
            matches.forEach { match ->
                TvTextButton(enabled = !busy, onClick = {
                    busy = true
                    scope.launch {
                        try {
                            val downloaded = vm.api().aiSubtitleDownload(file.id, AISubtitleDownload(match.file_id, match.language))
                            if (current()) { choose(downloaded.subtitle_index); status = "Subtitle downloaded." }
                        } catch (e: CancellationException) { throw e }
                        catch (e: Exception) { if (current()) status = e.message ?: "Subtitle download failed." }
                        finally { busy = false }
                    }
                }) { Text(match.release) }
            }
        }
        stored.forEach { track ->
            TvTextButton(enabled = !busy && operation?.active != true, onClick = { start("audio", "best") }) {
                Text("Redo at Best quality")
            }
            val index = track.index
            val stable = track.track_id
            if (index != null && stable != null) {
                listOf(-250L to "Earlier by 250 ms", 250L to "Later by 250 ms").forEach { (delta, label) ->
                    TvTextButton(enabled = !busy, onClick = {
                        busy = true
                        scope.launch {
                            try {
                                val transport = vm.api()
                                val currentOffset = transport.aiSubtitleTiming(file.id, index, stable)
                                if (current()) {
                                    val value = AISubtitleTiming((currentOffset.offset_ms + delta).coerceIn(-15_000, 15_000))
                                    val saved = transport.saveAISubtitleTiming(file.id, index, stable, value)
                                    if (current()) status = "Subtitle timing ${saved.offset_ms} ms"
                                }
                            } catch (e: CancellationException) { throw e }
                            catch (e: Exception) { if (current()) status = e.message ?: "Could not shift the subtitle." }
                            finally { busy = false }
                        }
                    }) { Text(label) }
                }
                TvTextButton(enabled = !busy, onClick = {
                    busy = true
                    scope.launch {
                        try {
                            val response = vm.api().aiSubtitleDelete(file.id, index, stable)
                            check(response.isSuccessful) { "Could not delete the subtitle." }
                            if (current()) { removed = removed + stable; status = "AI subtitle deleted." }
                        } catch (e: CancellationException) { throw e }
                        catch (e: Exception) { if (current()) status = e.message ?: "Could not delete the subtitle." }
                        finally { busy = false }
                    }
                }) { Text("Delete English AI subtitle") }
            }
        }
        operation?.let {
            when {
                it.active -> Text("English · AI · ${(it.progress * 100).toInt()}% · You can play this title now.", color = Muted)
                it.state == "complete" -> Text("English AI subtitles are ready.", color = Muted)
                it.state == "failed" -> Text("AI subtitles could not finish.", color = Muted)
            }
        }
        if (status.isNotEmpty()) Text(status, color = Muted)
    }
}
