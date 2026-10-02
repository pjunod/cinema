@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import androidx.media3.common.MediaItem
import androidx.media3.datasource.DataSource
import androidx.media3.datasource.okhttp.OkHttpDataSource
import androidx.media3.exoplayer.hls.HlsMediaSource
import androidx.media3.exoplayer.source.MediaSource
import java.io.IOException
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.*
import kotlinx.coroutines.channels.Channel
import kotlinx.serialization.json.*
import tv.plurx.app.data.Net

/** One retained player/source attachment. Network ordering, accepted queue
 * samples, hardware output and physical disposal remain independent facts. */
internal class ContinuousAttachment(
    private val enrollment: ContinuousEnrollment,
    val start: ContinuousEnrollment.Start,
    clientInstance: String,
    private val registry: ContinuousSourceRegistry,
    private val output: ContinuousOutputEvidence,
    private val transfers: AutoTransferEvidence,
    private val presented: (JsonObject, Long) -> Unit,
    private val failed: (Exception) -> Unit,
) {
    private val owner = Any()
    private val profile = enrollment.profile
    private val exchange = ContinuousQualityExchange(profile, start.schedulePath)
    private val protocol = ContinuousQualityProtocol(buildJsonObject {
        put("version", 1); put("generation", start.generation); put("control_epoch", start.epoch)
        put("attachment", buildJsonObject {
            put("client_instance_id", clientInstance); put("attachment_id", ContinuousQualityWire.newId())
            put("lifetime_id", requireNotNull(start.intent.intent?.lifetime_id)); put("family_id", start.family.getValue("family_id"))
        })
    }, exchange::exchange)
    private val selection = ContinuousVideoSelection(start.family, protocol)
    private val reservations = ContinuousReservations(start.family, protocol, selection, start.primaryRendition)
    private val media = ContinuousQualityMedia(profile.origin, start.schedulePath, start.family, protocol)
    private val videoReleaseEpoch = AtomicLong()
    private val audioDecoderReleaseEpoch = AtomicLong()
    private data class SinkRelease(val epoch: Long = 0, val allocation: Long = 0)
    private val sinkRelease = AtomicReference(SinkRelease())
    private val queues = ContinuousQueueOwnership(owner) { role ->
        if (role == "video") videoReleaseEpoch.get() to 0L
        else audioDecoderReleaseEpoch.get() to sinkRelease.get().epoch
    }
    private val loads = ContinuousLoads()
    private val mediaCalls = ContinuousMediaCalls(profile.http)
    private val closed = AtomicBoolean()
    private val periodReleaseRequested = AtomicBoolean()
    private val videoOwned = AtomicBoolean()
    private val audioDecoderOwned = AtomicBoolean()
    private val frame = AtomicReference<ContinuousOutputEvidence.Event.Frame?>()
    private val audioHead = AtomicReference<Long?>(null)
    private val wake = Channel<Unit>(Channel.CONFLATED)
    private val pending = Channel<ContinuousQueueOwnership.Appended>(128)
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val rows = start.family.getValue("video").jsonArray.map { it.jsonObject }
    private val awaiting = LinkedHashMap<String, ContinuousQueueOwnership.Appended>()
    private var pump: Job? = null
    private var deliveredRevision = -1L
    private val finishing = AtomicBoolean()
    private val finished = CompletableDeferred<Unit>()

    fun rendition(height: Int, candidateId: String? = null): JsonObject? = rows.singleOrNull {
        it.number("height") == height.toLong() && (candidateId == null || it.text("candidate_id") == candidateId)
    }

    suspend fun source(positionMs: Long, automatic: Boolean): MediaSource {
        val initial = rows.single { it.text("rendition_id") == start.primaryRendition }
        reservations.initial(frontier(initial, positionMs), automatic)
        check(!closed.get())
        output.subscribe(owner, ::observe)
        val upstream = OkHttpDataSource.Factory(mediaCalls).setTransferListener(transfers)
        val sources = DataSource.Factory {
            ContinuousReservedDataSource(owner, upstream, profile.origin, start.schedulePath, start.family,
                media, reservations::reserve, loads, mediaCalls::cancel)
        }
        val extractor = ContinuousHlsExtractorFactory(owner, queues::accepted, { verified ->
            queues.completed(verified)?.let { append ->
                if (!pending.trySend(append).isSuccess) throw IOException("Continuous append observation bound")
                wake.trySend(Unit)
            }
        })
        val hls = HlsMediaSource.Factory(sources).setExtractorFactory(extractor)
            .setLoadErrorHandlingPolicy(ContinuousLoadErrorPolicy())
            .createMediaSource(MediaItem.fromUri(profile.origin + start.playback.playlist_url))
        pump = scope.launch {
            while (isActive && !closed.get()) {
                withTimeoutOrNull(1000) { wake.receive() }
                if (closed.get()) break
                try { flushFacts() } catch (error: CancellationException) { throw error }
                catch (error: Exception) { runCatching { failed(error) } }
            }
        }
        return registry.source(hls, selection) { periodReleaseRequested.set(true) }
    }

    /** Call with the current buffered frontier, never the old tap position. */
    suspend fun change(row: JsonObject, positionMs: Long, automatic: Boolean): Boolean {
        if (closed.get() || row !in rows) return false
        val changed = reservations.change(requireNotNull(row.text("rendition_id")), frontier(row, positionMs),
            automatic, selection.supportedRenditions())
        wake.trySend(Unit)
        return changed
    }

    private fun observe(event: ContinuousOutputEvidence.Event) {
        when (event) {
            is ContinuousOutputEvidence.Event.Frame -> frame.set(event)
            is ContinuousOutputEvidence.Event.AudioHead -> audioHead.set(event.positionUs)
            ContinuousOutputEvidence.Event.VideoOwned -> videoOwned.set(true)
            ContinuousOutputEvidence.Event.VideoFreed -> { videoReleaseEpoch.incrementAndGet(); videoOwned.set(false); frame.set(null) }
            ContinuousOutputEvidence.Event.AudioDecoderOwned -> audioDecoderOwned.set(true)
            ContinuousOutputEvidence.Event.AudioDecoderFreed -> { audioDecoderReleaseEpoch.incrementAndGet(); audioDecoderOwned.set(false); audioHead.set(null) }
            ContinuousOutputEvidence.Event.AudioSinkFlushed -> {
                sinkRelease.updateAndGet { SinkRelease(it.epoch + 1, output.audioOutputs.allocationMark()) }
                audioHead.set(null)
            }
            else -> Unit // AudioTrack state is polled independently of callbacks.
        }
        wake.trySend(Unit)
    }

    private suspend fun flushFacts() {
        protocol.settlePending()
        queues.observeResets()
        output.audioOutputs.collectReleased()
        while (true) {
            val append = pending.tryReceive().getOrNull() ?: break
            val key = key(append.interval)
            if (awaiting.size >= 128 && key !in awaiting) throw IOException("Continuous pending append bound")
            awaiting[key] = append
        }
        for ((key, append) in awaiting.toMap()) {
            if (append.video) for (id in append.transactions) {
                val tx = transaction(id) ?: throw IOException("Continuous append transaction missing")
                if (tx.getValue("appended").jsonArray.none { it.jsonObject == append.interval }) {
                    protocol.transition(id, buildJsonObject { put("kind", "appended"); put("intervals", JsonArray(listOf(append.interval))) })
                }
            }
            awaiting.remove(key)
        }
        // A newer same-rendition reservation may own samples already retained
        // in this attachment. Credit only the exact physically accepted span,
        // while its queue still retains ownership; no second append is invented.
        for (load in queues.queuedArtifacts()) {
            val artifact = requireNotNull(load.authorized.interval.text("artifact_id"))
            if (load.resource.role != "video" || !queues.wasAppended(load.resource.rendition, artifact) ||
                queues.queueRetired(load.resource.rendition, artifact)) continue
            for (id in currentOwners(load)) {
                if (transaction(id)?.getValue("appended")?.jsonArray?.none { it.jsonObject == load.authorized.interval } == true)
                    protocol.transition(id, buildJsonObject { put("kind", "appended"); put("intervals", JsonArray(listOf(load.authorized.interval))) })
            }
        }
        val observed = frame.get()
        if (observed != null) {
            val matching = queues.queuedArtifacts().filter { load ->
                load.resource.role == "video" && observed.format.width.toLong() == load.resource.row.number("width") &&
                    observed.format.height.toLong() == load.resource.row.number("height") &&
                    frameTick(load.resource.row, observed.positionUs)?.let { contains(load.authorized.interval, it) } == true
            }
            if (matching.size == 1) {
                val load = matching.single()
                val tick = requireNotNull(frameTick(load.resource.row, observed.positionUs))
                for (id in currentOwners(load)) {
                    val tx = transaction(id) ?: continue
                    if (tx.getValue("appended").jsonArray.none { it.jsonObject == load.authorized.interval }) continue
                    if (tx["first_presented_tick"] == JsonNull) protocol.transition(id, buildJsonObject {
                        put("kind", "presented"); put("artifact_id", load.authorized.interval.getValue("artifact_id"))
                        put("film_tick", tick); put("observed_at_ms", observed.observedAtMs)
                    })
                    val accepted = transaction(id) ?: continue
                    if (accepted.number("intent_revision") == protocol.ledger?.number("latest_intent_revision") &&
                        accepted["first_presented_tick"] != JsonNull && accepted["intent_superseded"]?.wireBoolean() == false) {
                        val revision = requireNotNull(accepted.number("intent_revision"))
                        if (revision > deliveredRevision) {
                            presented(load.resource.row, revision)
                            deliveredRevision = revision
                        }
                    }
                }
            }
        }
        retirePassedMedia()
    }

    private suspend fun retirePassedMedia() {
        for (load in queues.queuedArtifacts()) {
            val interval = load.authorized.interval
            val artifact = requireNotNull(interval.text("artifact_id"))
            if (!queues.queueRetired(load.resource.rendition, artifact)) continue
            val through = requireNotNull(interval.number("through_tick"))
            val reset = queues.retiredByReset(load)
            val sink = sinkRelease.get()
            val resetReleased = reset != null && if (load.resource.role == "video") videoReleaseEpoch.get() > reset.first
                else audioDecoderReleaseEpoch.get() > reset.first && sink.epoch > reset.second &&
                    output.audioOutputs.releasedThrough(owner, sink.allocation)
            val retired = resetReleased || if (load.resource.role == "video") frame.get()?.let {
                frameTick(load.resource.row, it.positionUs)?.let { tick -> tick >= through }
            } == true else audioHead.get()?.let { head ->
                frameTick(load.resource.row, head)?.let { tick -> tick >= through + 1024 }
            } == true
            if (!retired) continue
            val ids = if (load.resource.role == "video") currentOwners(load)
                else transactions().lastOrNull()?.text("transaction_id")?.let(::setOf).orEmpty()
            if (ids.isEmpty()) continue
            for (id in ids) {
                if (transaction(id)?.getValue("disposed")?.jsonArray?.contains(JsonPrimitive(artifact)) != true)
                    protocol.transition(id, buildJsonObject { put("kind", "disposed"); put("artifacts", JsonArray(listOf(JsonPrimitive(artifact)))) })
            }
            queues.disposed(load.resource.rendition, artifact)
        }
    }

    /** Synchronous fence before the controller requests source/player release. */
    fun closeAdmission() {
        if (closed.compareAndSet(false, true)) { mediaCalls.cancel(); loads.cancel(); pump?.cancel(); wake.trySend(Unit) }
    }

    /** Own scope survives composition cancellation. End fences the server;
     * actual loader, queue, decoder and AudioTrack facts authorize disposal. */
    fun finishAfterRelease() {
        closeAdmission()
        if (!finishing.compareAndSet(false, true)) return
        scope.launch {
            try {
                withTimeout(5000) {
                    pump?.join()
                    Net.api(profile.origin, profile.http).endHlsSession(start.playback.session_id)
                    protocol.reconcileTerminal()
                    while (true) {
                        output.audioOutputs.collectReleased()
                        if ((periodReleaseRequested.get() || !registry.owns(selection)) && loads.isQuiescent() && mediaCalls.isQuiescent() && queues.queuesEmpty() &&
                            !videoOwned.get() && !audioDecoderOwned.get() && output.audioOutputs.isReleased(owner)) break
                        delay(10)
                    }
                    val audio = protocol.ledger?.get("shared_audio_reserved")?.jsonArray.orEmpty().map { it.jsonObject.getValue("artifact_id") }
                    val all = transactions()
                    for ((index, tx) in all.withIndex()) {
                        val disposed = tx.getValue("disposed").jsonArray
                        val artifacts = tx.getValue("reserved").jsonArray.map { it.jsonObject.getValue("artifact_id") }
                            .plus(if (index == all.lastIndex) audio else emptyList()).distinct().filter { it !in disposed }
                        if (artifacts.isNotEmpty()) protocol.transition(requireNotNull(tx.text("transaction_id")), buildJsonObject {
                            put("kind", "disposed"); put("artifacts", JsonArray(artifacts))
                        })
                    }
                    queues.pipelineReleased()
                }
            } catch (error: Exception) { runCatching { failed(error) } }
            finally { output.unsubscribe(owner); enrollment.close(); finished.complete(Unit); scope.cancel() }
        }
    }

    suspend fun awaitFinished() { finished.await() }

    private fun currentOwners(load: ContinuousLoadContext.Verified): Set<String> = transactions().filter { tx ->
        load.authorized.interval.getValue("artifact_id") !in tx.getValue("disposed").jsonArray &&
            tx.getValue("reserved").jsonArray.any { it.jsonObject == load.authorized.interval }
    }.mapNotNull { it.text("transaction_id") }.toSet()

    private fun transactions(): List<JsonObject> = protocol.ledger?.get("transactions")?.jsonArray.orEmpty().map { it.jsonObject }
    private fun transaction(id: String): JsonObject? = transactions().singleOrNull { it.text("transaction_id") == id }
    private fun key(interval: JsonObject) = "${interval.text("rendition_id")}:${interval.text("artifact_id")}"
    private fun contains(interval: JsonObject, tick: Long) = requireNotNull(interval.number("from_tick")) <= tick && tick < requireNotNull(interval.number("through_tick"))
    private fun frontier(row: JsonObject, ms: Long): Long = ContinuousFilmClock.frontier(
        ms, requireNotNull(row.number("timescale")), requireNotNull(row.number("segment_ticks")))
    private fun frameTick(row: JsonObject, us: Long): Long? = ContinuousFilmClock.frameTick(
        us, requireNotNull(row.number("timescale")), row.number("frame_ticks") ?: 1L)
}
