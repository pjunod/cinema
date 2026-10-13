package tv.plurx.app.diagnostics

import android.content.ClipData
import android.content.Context
import android.content.Intent
import androidx.core.content.FileProvider
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.serialization.json.*
import tv.plurx.app.data.Session
import java.io.File
import java.util.UUID

internal data class DiagnosticUI(
    val localExportURL: String? = null, val visible: Boolean = false, val draft: DiagnosticDraft? = null, val pending: List<DiagnosticDraft> = emptyList(),
    val busy: Boolean = false, val message: String = "", val destination: String = "", val consent: Boolean = false, val selectionRevision: Long = 0,
    val includePlatform: Boolean = false, val includeTitle: Boolean = false, val invitation: String = "", val comparison: String? = null, val handoffState: String = "", val history: Boolean = true,
)

/** One report owner, independent of player retries, with one foreground task
 * and one bounded phone-status poll. It never starts an unsolicited upload. */
internal object DiagnosticRuntime {
    private lateinit var app: Context
    private lateinit var store: DiagnosticStore
    private lateinit var platform: DiagnosticPlatform
    lateinit var recorder: DiagnosticRecorder; private set
    private val owner = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private var workRevision = 0L
    private var localExportRevision = 0L
    private var localExport: DiagnosticLocalExport? = null
    private var operation: Job? = null
    private var poll: Job? = null
    private var transport: DiagnosticTransport? = null
    private var namespace = "anonymous"
    private var title: String? = null
    private var television = false
    private val mutable = MutableStateFlow(DiagnosticUI())
    val state = mutable.asStateFlow()
    private val preferences get() = app.getSharedPreferences("cinema-diagnostics",Context.MODE_PRIVATE)
    fun initialize(context: Context) {
        if (::app.isInitialized) return
        app = context.applicationContext; store = DiagnosticStore(File(app.noBackupFilesDir,"cinema-diagnostics"),removeSecrets={ scope,id -> for (name in listOf(id,id+":invitation",id+":invitation-proof")) DiagnosticSecrets(app,scope).delete(name) })
        platform = DiagnosticPlatform(app)
        recorder = DiagnosticRecorder(history={ preferences.getBoolean("history",true) }, persist={ store.saveRing(it) },complete={ capture ->
            val prior = store.drafts(capture.namespace).firstOrNull { it.id == capture.id } ?: DiagnosticDraft(capture)
            store.save(prior.copy(capture=capture))
        })
        mutable.update { it.copy(history=preferences.getBoolean("history",true)) }
        Session.observeAuthorizationChanges { auth ->
            recorder.retireAuthorization(auth); platform.retire()
            owner.launch { retire() }
        }
        record("lifecycle","state",mapOf("state" to JsonPrimitive("starting")))
        owner.launch(Dispatchers.IO) {
            File(app.cacheDir,"diagnostics-share").listFiles().orEmpty().take(32).filter { it.lastModified() < System.currentTimeMillis()-86400000 }.forEach { it.delete() }
        }
    }
    private fun retire() {
        stopLocalExport()
        workRevision++
        operation?.cancel(); poll?.cancel(); transport?.cancel(); operation=null; poll=null; transport=null
        mutable.update { DiagnosticUI(history=it.history) }; title=null
    }
    suspend fun configure(origin: String, instance: String?, user: Long?, television: Boolean) {
        this.television = television
        val auth = Session.playbackAuthorization()
        val next = if (instance != null && instance.isNotEmpty() && user != null && user > 0 && auth.token != null) DiagnosticFields.hash((requireNotNull(Session.canonicalOrigin(origin))+"\n"+instance+"\n"+user).toByteArray()) else "anonymous"
        namespace = next; store.activate(next); val recovered = store.ring(next)
        if (Session.playbackAuthorization().generation != auth.generation || namespace != next) return
        recorder.bind(next,auth.generation,recovered)
        platform.bind(next,auth.generation)
        if (recovered?.events?.isNotEmpty() == true) record("lifecycle","state",mapOf("state" to JsonPrimitive("interrupted"),"reason" to JsonPrimitive("interrupted")))
        record("lifecycle","state",mapOf("state" to JsonPrimitive("foreground")))
        val pending = store.drafts(next)
        if (Session.playbackAuthorization().generation != auth.generation || namespace != next) return
        mutable.update { it.copy(pending=pending) }
    }
    fun record(domain: String, kind: String, facts: Map<String,JsonPrimitive> = emptyMap(), generation: Long? = null) {
        if (::recorder.isInitialized) runCatching { recorder.record(domain,kind,facts,generation) }
    }
    fun playback(event: tv.plurx.app.player.PlaybackClientLog, generation: Long) {
        if (generation != Session.playbackAuthorization().generation) return
        val facts=mutableMapOf<String,JsonPrimitive>()
        event.method?.let { if (it == "direct_play") "direct" else it }?.takeIf { it in setOf("direct","remux","transcode") }?.let { facts["mode"]=JsonPrimitive(it) }
        event.height?.takeIf { it >= 0 }?.let { facts["height"]=JsonPrimitive(it) }
        event.ms?.takeIf { it in 0..604800000 }?.let { facts["duration_ms"]=JsonPrimitive(it) }
        event.runway?.takeIf { it.isFinite() && it >= 0 }?.let { facts["headroom_ms"]=JsonPrimitive((it*1000).toLong().coerceAtMost(604800000)) }
        event.vcodec?.lowercase()?.let { facts["source_codec"]=JsonPrimitive(if (it == "h265") "hevc" else if (it in setOf("h264","hevc","av1","vp9")) it else "other") }
        event.sessionId?.takeIf { it.toByteArray().size <= 256 }?.let { facts["correlation"]=JsonPrimitive(if (Regex("s-[a-f0-9]{64}").matches(it)) it else "s-"+DiagnosticFields.hash(it.toByteArray())) }
        val state=when(event.event) { "session_start" -> "starting"; "stall" -> "stalled"; "stall_end","stall_recovery" -> "recovered"; "suspend" -> "paused"; "stop" -> "stopped"; else -> null }
        if (state != null) facts["state"]=JsonPrimitive(state)
        record("playback",if (state == "stopped") "terminal" else if (state != null) "state" else "sample",facts,generation)
    }
    fun open(category: String = "other", positiveTitle: String? = null, newIncident: Boolean = false) {
        stopLocalExport()
        if (!::recorder.isInitialized) return
        workRevision++
        operation?.cancel(); poll?.cancel(); transport?.cancel(); title=positiveTitle
        if (newIncident) recorder.forgetReproduction()
        val capture = recorder.retainedReproduction ?: recorder.freeze(); val draft = DiagnosticDraft(capture,category=category)
        mutable.update { it.copy(visible=true,draft=draft,busy=true,message="Capturing this incident. A description is optional.",consent=false,includeTitle=false,includePlatform=false,comparison=null,handoffState="") }
        record("ui","action",mapOf("action" to JsonPrimitive("report")))
        val client = DiagnosticTransport(app,capture.namespace); transport=client
        operation=owner.launch {
            try {
            var result = store.drafts(capture.namespace).firstOrNull { it.id == capture.id }?.copy(capture=capture) ?: draft
            val evidence=platform.projection(capture); result=result.copy(platform=evidence.second,platformReason=evidence.first)
            try {
                val (events,components) = client.snapshot(capture); result=result.copy(serverEvents=events,components=components)
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (_: Exception) { result=result.copy(components=listOf(DiagnosticComponent("server","unavailable",if (capture.namespace == "anonymous") "context_unavailable" else "connection_failed"))) }
            client.current(); if (mutable.value.draft?.id != draft.id) return@launch
            val destination = runCatching { client.supportOrigin() }.getOrNull().orEmpty(); client.current()
            result=result.copy(description=mutable.value.draft?.description.orEmpty()); persist(result)
            mutable.update { if (it.draft?.id == result.id) it.copy(draft=result,destination=destination,busy=false,message="Frozen local report. Review its coverage before sending. Missing server evidence does not prevent saving.") else it }
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (_: Exception) { if (mutable.value.draft?.id == draft.id) mutable.update { it.copy(busy=false,message="Local report available. Server or protected support storage is unavailable.") } }

        }
    }
    fun resume(row: DiagnosticDraft) {
        stopLocalExport()
        val selected=mutable.value
        if (selected.busy || row.capture.namespace != namespace) return
        val previousWork=operation; val previousPoll=poll
        previousWork?.cancel(); previousPoll?.cancel(); transport?.cancel()
        val revision=++workRevision; val scope=namespace; val auth=Session.playbackAuthorization()
        val previous=selected.draft?.let { if (it.consented) it else it.copy(title=if (selected.includeTitle) title else null,includePlatform=selected.includePlatform) }
        transport=null; poll=null; mutable.update { it.copy(busy=true,comparison=null,handoffState="") }
        operation=owner.launch {
            previousWork?.join(); previousPoll?.join()
            if (!isActive || revision != workRevision || scope != namespace || auth.generation != Session.playbackAuthorization().generation) return@launch
            previous?.takeUnless { it.consented }?.let { store.save(it) }
            val rows=store.drafts(scope)
            if (!isActive || revision != workRevision || scope != namespace || auth.generation != Session.playbackAuthorization().generation) return@launch
            val restored=rows.firstOrNull { it.id == row.id }
            mutable.update { it.copy(pending=rows,busy=false) }
            if (restored == null) { mutable.update { it.copy(message="This local report expired or was deleted.") }; return@launch }
            // A retained row supplies identity, never an old consent or grant.
            val value=restored.copy(capture=restored.capture.copy(generation=auth.generation)); title=value.title
            mutable.update { it.copy(visible=true,draft=value,destination=value.origin.orEmpty(),consent=value.consented,includeTitle=value.title != null,includePlatform=value.includePlatform,selectionRevision=it.selectionRevision+1,comparison=null,handoffState="",message="Recovered local report. Save its diagnostic ZIP to attach it to an issue.") }
        }
    }
    private suspend fun persist(value: DiagnosticDraft) {
        store.save(value); val rows=store.drafts(value.capture.namespace)
        currentCoroutineContext().ensureActive()
        if (namespace == value.capture.namespace && Session.playbackAuthorization().generation == value.capture.generation) mutable.update { it.copy(pending=rows) }
    }
    fun edit(description: String? = null, category: String? = null) {
        if (mutable.value.busy) return
        stopLocalExport()
        mutable.update { value -> val draft=value.draft; if (draft == null || draft.consented) value else value.copy(draft=draft.copy(description=description?.let { text -> String(text.codePoints().limit(4000).toArray(),0,minOf(text.codePointCount(0,text.length),4000)) } ?: draft.description,category=category ?: draft.category)) }
    }
    fun destination(value: String) { mutable.update { if (it.draft?.consented == true) it else it.copy(destination=value.take(1024)) } }
    fun consent(value: Boolean) { mutable.update { it.copy(consent=value) } }
    fun includeTitle(value: Boolean) { if (mutable.value.busy) return; stopLocalExport(); mutable.update { if (it.draft?.consented == true) it else it.copy(includeTitle=value) } }
    fun includePlatform(value: Boolean) { if (mutable.value.busy) return; stopLocalExport(); mutable.update { if (it.draft?.consented == true) it else it.copy(includePlatform=value) } }
    fun invitation(value: String) { mutable.update { it.copy(invitation=value.take(10)) } }
    fun history(value: Boolean) {
        preferences.edit().putBoolean("history",value).apply(); mutable.update { it.copy(history=value) }
        owner.launch { store.clearRing(namespace); recorder.retireAuthorization(Session.playbackAuthorization().generation); recorder.bind(namespace,Session.playbackAuthorization().generation,null) }
    }
    fun close() {
        stopLocalExport()
        workRevision++
        mutable.value.draft?.takeUnless { it.consented }?.let { value -> owner.launch { store.save(value) } }
        operation?.cancel(); poll?.cancel(); transport?.cancel(); transport=null
        mutable.update { it.copy(visible=false,busy=false) }
    }
    fun background() {
        stopLocalExport()
        if (!::recorder.isInitialized) return; workRevision++; recorder.background()
        operation?.cancel(); poll?.cancel(); transport?.cancel(); transport=null
        mutable.update { it.copy(busy=false,message=if (it.visible) "Paused in background. Retry this same report when foreground." else it.message) }
    }
    fun signOut() {
        if (!::recorder.isInitialized) return; val previous=namespace; retire()
        owner.launch(Dispatchers.IO) { store.clear(previous); DiagnosticSecrets(app,previous).clear() }
    }
    suspend fun revokeEnrollment() { if (!::recorder.isInitialized) return; val client=DiagnosticTransport(app,namespace); try { client.revokeEnrollment() } finally { client.cancel() } }
    private fun perform(action: suspend (DiagnosticTransport, DiagnosticDraft) -> DiagnosticDraft) {
        val value=mutable.value; val draft=value.draft ?: return; if (value.busy || draft.capture.generation != Session.playbackAuthorization().generation) return
        stopLocalExport()
        val revision=++workRevision
        operation?.cancel(); transport?.cancel(); val client=DiagnosticTransport(app,draft.capture.namespace); transport=client
        mutable.update { it.copy(busy=true,message="Working…") }
        operation=owner.launch {
            try {
                val result=action(client,draft); client.current(); if (!isActive || workRevision != revision || mutable.value.draft?.id != draft.id) return@launch
                persist(result); mutable.update { it.copy(draft=result) }
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (error: Exception) {
                if (isActive && workRevision == revision && mutable.value.draft?.id == draft.id) {
                    val rows=store.drafts(draft.capture.namespace); val saved=rows.firstOrNull { it.id == draft.id }
                    if (isActive && workRevision == revision && mutable.value.draft?.id == draft.id) mutable.update { it.copy(draft=saved ?: it.draft,pending=rows,consent=saved?.consented ?: it.consent,includeTitle=if (saved?.consented == true) saved.title != null else it.includeTitle,includePlatform=if (saved?.consented == true) saved.includePlatform else it.includePlatform,message=if (error is DiagnosticTransport.Failure) error.message.orEmpty() else "The connection or protected storage failed. Retry or save this local report.") }
                }
            }
            finally { if (workRevision == revision && mutable.value.draft?.id == draft.id) mutable.update { it.copy(busy=false) } }
        }
    }
    private suspend fun archive(original: DiagnosticDraft): Pair<DiagnosticDraft, DiagnosticArchive> = withContext(Dispatchers.IO) {
        val prior=original.binding; val manifest=original.manifest
        if (prior != null && manifest != null) {
            val bytes=requireNotNull(store.archive(original.id,original.capture.namespace)); require(bytes.size == prior.length && DiagnosticFields.hash(bytes) == prior.sha256)
            return@withContext original to DiagnosticArchive(bytes,prior,manifest)
        }
        require(prior == null); val draft=original.copy(title=if (mutable.value.includeTitle) title else null,includePlatform=mutable.value.includePlatform)
        val value=runInterruptible { DiagnosticReport.archive(draft,television) }; require(value.bytes.size <= 8388608)
        if (draft.consented) {
            val frozen=draft.copy(binding=value.binding,manifest=value.manifest)
            require(store.saveArchive(frozen.id,frozen.capture.namespace,value.bytes,frozen.capture.generation) != "unavailable"); persist(frozen); frozen to value
        } else draft to value
    }
    fun stopLocalExport() {
        localExportRevision++
        localExport?.close(); localExport=null
        mutable.update { it.copy(localExportURL=null) }
    }
    fun shareOnLocalNetwork() {
        stopLocalExport()
        perform { _, original ->
            val offerRevision=localExportRevision
            val (draft,archive)=archive(original)
            val revision=workRevision
            if (offerRevision != localExportRevision) { mutable.update { it.copy(message="The report changed. Save its ZIP again when ready.") }; throw CancellationException() }
            val transfer=DiagnosticLocalExport.createOwned {
                DiagnosticLocalExport(archive.bytes,draft.id,ended={
                    owner.launch { if (workRevision == revision) stopLocalExport() }
                })
            }
            if (!currentCoroutineContext().isActive || workRevision != revision || localExportRevision != offerRevision || !mutable.value.visible) { transfer.close(); throw CancellationException() }
            localExport=transfer
            mutable.update { it.copy(localExportURL=transfer.url,message="Scan on a phone on the same network, then download the TV's ZIP. Keep this screen open.") }
            draft
        }
    }
    fun share() = perform { _, original ->
        val (draft,archive)=archive(original)
        val uri=withContext(Dispatchers.IO) {
            val folder=File(app.cacheDir,"diagnostics-share"); check(folder.mkdirs() || folder.isDirectory)
            val files=folder.listFiles().orEmpty(); require(files.size < 32); files.filter { it.lastModified() < System.currentTimeMillis()-86400000 }.forEach { it.delete() }
            val file=File(folder,"cinema-report-${UUID.randomUUID()}.zip"); file.writeBytes(archive.bytes)
            FileProvider.getUriForFile(app,app.packageName+".diagnostics-files",file)
        }
        currentCoroutineContext().ensureActive()
        val intent=Intent(Intent.ACTION_SEND).setType("application/zip").putExtra(Intent.EXTRA_STREAM,uri).addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        intent.clipData=ClipData.newUri(app.contentResolver,"Cinema diagnostic report",uri)
        app.startActivity(Intent.createChooser(intent,"Save or share diagnostic ZIP").addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
        mutable.update { it.copy(message="Diagnostic ZIP ready. Attach it to your bug or issue report.") }; draft
    }
    fun send(phone: Boolean = false) {
        if (recorder.recording) { mutable.update { it.copy(message="Stop reproduction before sending this frozen report.") }; return }
        val selected=mutable.value
        if (!selected.consent) { mutable.update { it.copy(message="Confirm this support destination before uploading.") }; return }
        perform { client, original ->
            val origin=DiagnosticTransport.origin(selected.destination,preferences.getBoolean("development_local",false))
            require(!original.consented || original.origin == origin)
            val (frozen,archive)=archive(original.copy(origin=origin,consented=true))
            var value=client.stage(archive,frozen) { progress -> client.current(); currentCoroutineContext().ensureActive(); persist(progress); withContext(Dispatchers.Main.immediate) { mutable.update { if (it.draft?.id == progress.id) it.copy(draft=progress) else it } } }
            value=if (phone && value.state != "submitted") client.offer(value) else client.submit(value)
            mutable.update { it.copy(draft=value,message=if (value.state == "submitted") "Submitted. Reference: ${value.reference}" else "Scan on the phone, then compare the phrase here before approving.") }
            if (value.state == "submitted") recorder.forgetReproduction(value.id)
            if (value.handoff != null) pollHandoff(); value
        }
    }
    fun redeem() {
        val selected=mutable.value
        if (!selected.consent) { mutable.update { it.copy(message="Confirm this support destination before using an invitation.") }; return }
        perform { client,draft ->
            val origin=DiagnosticTransport.origin(selected.destination,preferences.getBoolean("development_local",false))
            client.invitation(origin,selected.invitation.trim().uppercase(),draft.id)
            mutable.update { it.copy(invitation="",message="One-report invitation ready. Send only after reviewing this capture.") }; draft.copy(origin=origin)
        }
    }
    fun pollHandoff() {
        poll?.cancel(); val value=mutable.value.draft ?: return; if (value.handoff == null) return
        val client=DiagnosticTransport(app,value.capture.namespace)
        poll=owner.launch {
            var deadline=android.os.SystemClock.elapsedRealtime()+600000; var approved=false
            try {
                while (isActive && mutable.value.visible && mutable.value.draft?.id == value.id && android.os.SystemClock.elapsedRealtime() < deadline) {
                    val reply=client.handoffStatus(value); client.current(); currentCoroutineContext().ensureActive()
                    if (mutable.value.draft?.id != value.id || !mutable.value.visible) return@launch
                    val state=reply.getValue("state").jsonPrimitive.content
                    mutable.update { it.copy(handoffState=state,comparison=if (state == "claim_pending") reply["comparison"]?.jsonPrimitive?.content else null) }
                    if (state == "approved" && !approved) { approved=true; deadline=android.os.SystemClock.elapsedRealtime()+1800000; mutable.update { it.copy(message="Phone approved. It can finish this report even if the TV closes.") } }
                    if (state == "submitted") {
                        recorder.forgetReproduction(value.id)
                        val reference=reply["reference"]?.jsonPrimitive?.content; val finished=mutable.value.draft?.copy(state="submitted",reference=reference)
                        if (finished != null) persist(finished); mutable.update { it.copy(draft=finished,message="Submitted. Reference: $reference") }; return@launch
                    }
                    if (state in setOf("expired","cancelled","rejected","deleted")) { mutable.update { it.copy(message="Phone handoff ended. The local report remains available.") }; return@launch }
                    delay(2000)
                }
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (_: Exception) { mutable.update { it.copy(message="Handoff status unavailable. Refresh when foreground.") } }
            finally { client.cancel() }
        }
    }
    fun approve() {
        val phrase=mutable.value.comparison ?: return
        perform { client,draft -> client.approve(draft,phrase); mutable.update { it.copy(comparison=null,message="Phone approved for this report only.") }; draft }
    }
    fun discard(keep: Boolean = false) = perform { client,draft ->
        client.delete(draft,keep)
        if (!keep) { recorder.forgetReproduction(draft.id); store.delete(draft.id,draft.capture.namespace); withContext(Dispatchers.IO) { val secrets=DiagnosticSecrets(app,draft.capture.namespace); listOf(draft.id,draft.id+":invitation",draft.id+":invitation-proof").forEach { secrets.delete(it) } }; mutable.update { it.copy(draft=null,visible=false,busy=false) } }
        else mutable.update { it.copy(message="Phone handoff cancelled; staged draft retained.") }
        draft.copy(handoff=null)
    }
    fun startReproduction() { stopLocalExport();
        val value=mutable.value.draft ?: return; if (value.consented) return
        if (recorder.startReproduction(value.capture)) mutable.update { it.copy(message="Recording for at most five minutes. Reproduce the issue, then reopen Help and stop. Background ends recording.") }
    }
    fun stopReproduction() { stopLocalExport(); recorder.stopReproduction()?.let { capture -> mutable.update { it.copy(draft=it.draft?.copy(capture=capture),message="Reproduction stopped and frozen locally.") } } }
    fun systemSnapshot() = perform { client,draft ->
        require(!draft.consented); val (events,components)=client.snapshot(draft.capture,true)
        mutable.update { it.copy(message="Administrator snapshot saved: typed version, readiness, producer and hardware admission facts.") }
        draft.copy(serverEvents=events,components=components)
    }
    fun checks() = perform { client,draft ->
        require(!draft.consented); val events=client.connectionChecks()
        mutable.update { it.copy(message="Checks saved for this Cinema endpoint. No media transfer was requested.") }
        draft.copy(connection=(draft.connection+events).takeLast(100))
    }
    fun deleteLocal(row: DiagnosticDraft) {
        stopLocalExport()
        if (row.capture.namespace != namespace) return
        val selected=mutable.value.draft?.id == row.id
        val previousWork=if (selected) operation else null; val previousPoll=if (selected) poll else null
        if (selected) {
            workRevision++; previousWork?.cancel(); previousPoll?.cancel(); transport?.cancel()
            operation=null; poll=null; transport=null
            mutable.update { it.copy(draft=null,busy=false,comparison=null,handoffState="") }
        }
        mutable.update { it.copy(pending=it.pending.filter { pending -> pending.id != row.id }) }; recorder.forgetReproduction(row.id)
        val scope=namespace; val auth=Session.playbackAuthorization()
        owner.launch {
            store.delete(row.id,scope)
            previousWork?.join(); previousPoll?.join()
            store.delete(row.id,scope)
            withContext(Dispatchers.IO) { val secrets=DiagnosticSecrets(app,scope); for (name in listOf(row.id,row.id+":invitation",row.id+":invitation-proof")) secrets.delete(name) }
            val rows=store.drafts(scope)
            if (namespace == scope && Session.playbackAuthorization().generation == auth.generation) mutable.update { it.copy(pending=rows,message=if (selected) "Local report deleted." else it.message) }
        }
    }
}
