package tv.plurx.app.player

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import tv.plurx.app.data.*
import java.util.UUID

/** A separate full-caption text translation job; never a speech recognizer task. */
internal class SubtitleTextTranslationIntent {
    var jobId: String? = null
    var sourceSize: String? = null
    var sourceMtime: String? = null
    var cancelRequested: Boolean = false
}
internal fun subtitleTextResultForUse(job: SubtitleTextJob, fileId: Long, options: SubtitleTextOptions,
    intent: SubtitleTextTranslationIntent): Long {
    require(job.state == "succeeded" && job.job_id == intent.jobId)
    require(options.version == 1 && options.file_id == fileId.toString() && options.source_size == intent.sourceSize &&
        options.source_mtime == intent.sourceMtime) { "The subtitle source changed; refresh before using this result" }
    val result = requireNotNull(job.result)
    require(result.file_id == fileId.toString() && result.language == "en" && result.subtitle_index >= 0 && captionHash(result.downloaded_subtitle_id))
    val current = options.subtitles.singleOrNull { it.downloaded_subtitle_id == result.downloaded_subtitle_id }
    require(current?.text_eligible == true && current.index >= 0) { "The translated caption was removed or replaced; refresh tracks" }
    return current.index
}

internal class SubtitleTextTranslation(
    private val api: () -> PlurxApi, private val scope: CoroutineScope, private val fileId: Long,
    private val intent: SubtitleTextTranslationIntent, private val authorized: () -> Boolean,
    private val captionSelectionRevision: () -> Long,
) {
    var options: SubtitleTextOptions? by mutableStateOf(null); private set
    var job: SubtitleTextJob? by mutableStateOf(null); private set
    var status: String by mutableStateOf("Translate an existing text subtitle with the local translation model."); private set
    val canUse: Boolean get() = job?.let { it.state == "succeeded" && it.result?.file_id == fileId.toString() &&
        it.result.language == "en" && it.result.subtitle_index >= 0 } == true
    val busy: Boolean get() = job?.state in setOf("queued", "running") || submitting
    var submitting: Boolean by mutableStateOf(false); private set
    private var generation = 0L
    private var active = true
    private var polling: Job? = null
    private val actions = CaptionActionFence()
    private var createSelection: String? = null
    private var stopping = false
    private fun current(g: Long) = active && generation == g && authorized()
    private suspend fun readOptions(): SubtitleTextOptions = captionRead<SubtitleTextOptions>(api().subtitleTextOptions(fileId)).also {
        require(it.version == 1 && it.file_id == fileId.toString() && it.subtitles.size <= 4096 && it.workers.size <= 32 && it.input_languages.size <= 512)
    }
    fun loadOptions() {
        val g = generation
        scope.launch {
            try {
                val value = readOptions()
                if (!current(g)) return@launch
                options = value
                if (intent.jobId != null && polling?.isActive != true) poll(g)
            } catch (e: CancellationException) { throw e }
            catch (e: Exception) { if (current(g)) status = e.message ?: "Subtitle translation unavailable" }
        }
    }
    fun translate(index: Long, sourceLanguage: String) = translateInternal(index, sourceLanguage, resolveOnly = false)
    private fun translateInternal(index: Long, sourceLanguage: String, resolveOnly: Boolean) {
        if (busy || actions.inFlight || options?.permissions?.create != true || !authorized()) return
        if (!sourceLanguage.matches(Regex("[a-zA-Z]{2,3}"))) { status = "Choose a 2–3 letter source subtitle language"; return }
        val selection = "$index:${sourceLanguage.lowercase()}"
        if (createSelection != selection) { intent.cancelRequested = false; actions.invalidate(); createSelection = selection }
        else if (!resolveOnly && !actions.hasPending) intent.cancelRequested = false
        val g = ++generation; polling?.cancel(); job = null; submitting = true
        intent.jobId = null
        scope.launch {
            var ticket: CaptionActionFence.Ticket? = null
            var ambiguous = false
            try {
                val value = readOptions()
                if (!current(g)) return@launch
                options = value
                require(value.subtitles.singleOrNull { it.index == index }?.text_eligible == true) { "Bitmap subtitles need speech caption generation instead" }
                intent.sourceSize = value.source_size; intent.sourceMtime = value.source_mtime
                val owner = actions.begin("create:$fileId:$index:${value.source_size}:${value.source_mtime}:$selection:${value.model_id}") ?: return@launch
                ticket = owner
                if (intent.cancelRequested && !owner.replay) { status = "Stopped before text translation started"; return@launch }
                val result = captionRead<SubtitleTextJobReceipt>(api().translateSubtitleText(fileId, index,
                    SubtitleTextRequest(1, owner.requestId, value.source_size, value.source_mtime,
                        sourceLanguage.lowercase(), "en", value.model_id)))
                if (!current(g) || !actions.owns(owner)) return@launch
                require(result.version == 1)
                intent.jobId = result.job_id; job = observedReceipt(result); status = result.state
                if (intent.cancelRequested || actions.stoppedSince(owner)) cancelAndObserve(result.job_id, g)
                else poll(g)
            } catch (e: CancellationException) { throw e }
            catch (e: Exception) {
                ambiguous = captionActionAmbiguous(e)
                if (current(g) && (ticket == null || actions.owns(ticket))) status = e.message ?: "Subtitle translation failed"
            } finally {
                val finished = ticket?.let { actions.finish(it, ambiguous) } == true
                if (current(g)) submitting = false
                if (finished && ambiguous && current(g) && intent.cancelRequested && !resolveOnly)
                    translateInternal(index, sourceLanguage, resolveOnly = true)
            }
        }
    }
    private fun observedReceipt(value: SubtitleTextJobReceipt) = SubtitleTextJob(1, value.job_id, value.state,
        retryable = false, progress = SubtitleTextProgress(0, 0))
    private fun poll(g: Long) {
        val id = intent.jobId ?: return
        polling?.cancel()
        polling = scope.launch {
            try {
                while (isActive && current(g)) {
                    val value = captionRead<SubtitleTextJob>(api().subtitleTextJob(id))
                    if (!current(g)) return@launch
                    require(value.version == 1 && value.job_id == id && value.progress.completed >= 0 &&
                        value.progress.total >= value.progress.completed)
                    job = value
                    status = "${value.phase ?: value.state}: ${value.progress.completed}/${value.progress.total}${value.reason?.let { ": $it" } ?: ""}"
                    if (value.state in setOf("succeeded", "failed", "cancelled")) {
                        if (value.state == "succeeded") status = if (canUse) "English subtitle ready. Choose Use to refresh tracks and select it."
                            else "Job completed without a verified English subtitle result."
                        break
                    }
                    delay(1_000)
                }
            } catch (e: CancellationException) { throw e }
            catch (e: Exception) { if (current(g)) status = e.message ?: "Could not refresh subtitle translation" }
        }
    }
    fun stop() {
        if (options?.permissions?.create != true || !authorized()) return
        actions.stop(); intent.cancelRequested = true
        status = "Stopping subtitle text translation…"
        if (submitting || stopping) return // The accepted create/retry receipt supplies the exact job to stop.
        val id = intent.jobId ?: return
        val g = generation
        scope.launch { cancelAndObserve(id, g) }
    }
    private suspend fun cancelAndObserve(id: String, g: Long) {
        val stopRevision = actions.stopRevision
        stopping = true
        try { captionBytes(api().cancelSubtitleTextJob(id)) }
        catch (e: CancellationException) { throw e }
        catch (e: Exception) {
            if (current(g) && intent.jobId == id && stopRevision == actions.stopRevision)
                status = e.message ?: "Could not confirm subtitle translation Stop"
        } finally {
            stopping = false
            if (current(g) && intent.jobId == id) poll(g)
        }
    }
    fun retry() = retryInternal(resolveOnly = false)
    private fun retryInternal(resolveOnly: Boolean) {
        val id = intent.jobId ?: return
        if (busy || actions.inFlight || options?.permissions?.create != true || !authorized() || job?.retryable != true) return
        val owner = actions.begin("retry:$id") ?: return
        if (!owner.replay && !resolveOnly) intent.cancelRequested = false
        val g = ++generation; polling?.cancel(); submitting = true
        scope.launch {
            var ambiguous = false
            try {
                val value = captionRead<SubtitleTextJobReceipt>(api().retrySubtitleTextJob(id, SubtitleTextRetry(1, owner.requestId)))
                if (!current(g) || !actions.owns(owner)) return@launch
                require(value.version == 1)
                intent.jobId = value.job_id; job = observedReceipt(value)
                if (resolveOnly || intent.cancelRequested || actions.stoppedSince(owner)) cancelAndObserve(value.job_id, g)
                else poll(g)
            } catch (e: CancellationException) { throw e }
            catch (e: Exception) {
                ambiguous = captionActionAmbiguous(e)
                if (current(g) && actions.owns(owner)) status = e.message ?: "Subtitle retry failed"
            } finally {
                val finished = actions.finish(owner, ambiguous)
                if (current(g)) submitting = false
                if (finished && ambiguous && current(g) && intent.cancelRequested && !resolveOnly)
                    retryInternal(resolveOnly = true)
            }
        }
    }
    fun use(onUse: (Long) -> Unit) {
        val value = job ?: return
        val g = generation
        val selectedAt = captionSelectionRevision()
        scope.launch {
            try {
                val latest = readOptions()
                if (!current(g) || captionSelectionRevision() != selectedAt) return@launch
                val index = subtitleTextResultForUse(value, fileId, latest, intent)
                options = latest
                onUse(index)
            } catch (e: CancellationException) { throw e }
            catch (e: Exception) { if (current(g) && captionSelectionRevision() == selectedAt) status = e.message ?: "Could not use translated subtitle" }
        }
    }
    fun release() { active = false; generation++; actions.invalidate(); polling?.cancel() }
}
