package tv.plurx.app.remote

import android.content.Context
import android.os.Looper
import android.os.SystemClock
import androidx.compose.runtime.*
import kotlinx.coroutines.*
import kotlinx.serialization.json.*
import tv.plurx.app.data.Session
import tv.plurx.app.ui.AppViewModel
import java.util.UUID

internal data class RemoteDevice(val id: String, val name: String, val platform: String, val target: RemoteTarget?, val available: Boolean, val busy: Boolean)
internal data class RemoteControl(val epoch: String, val grant: String, val name: String)
internal object RemoteRuntime {
    private var instance: RemoteClientModel? = null
    fun get(context: Context): RemoteClientModel = instance ?: RemoteClientModel(context.applicationContext).also { instance = it }
    fun scene(resumed: Boolean, background: Boolean) { instance?.sceneChanged(resumed, background) }
}
internal class RemoteClientModel(private val app: Context) {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private val preferences = app.getSharedPreferences("cinema-remote-options", Context.MODE_PRIVATE)
    var enabled by mutableStateOf(preferences.getBoolean("enabled", false)); private set
    var sceneEligible by mutableStateOf(false); private set
    var status by mutableStateOf("Cinema remotes disabled."); private set
    var devices by mutableStateOf<List<RemoteDevice>>(emptyList()); private set
    var unavailableNodes by mutableStateOf<List<String>>(emptyList()); private set
    var target by mutableStateOf<RemoteTarget?>(null); private set
    var challenge by mutableStateOf<JsonObject?>(null); private set
    var pairingExpired by mutableStateOf(false); private set
    var pairingOpening by mutableStateOf(false); private set
    private val receiverPairLifetime = RemotePairingSurfaceLifetime()
    private var receiverPairJob: Job? = null
    private val pairingRestriction = UUID.randomUUID().toString()
    private val pairingRestricted = RemoteRestrictionLifetime()
    var pendingPairings by mutableStateOf<List<JsonObject>>(emptyList()); private set
    var selected by mutableStateOf<RemoteDevice?>(null); private set
    var state by mutableStateOf<JsonObject?>(null); private set
    var control by mutableStateOf<RemoteControl?>(null); private set
    var controlling by mutableStateOf(false); private set
    var commandStatus by mutableStateOf(""); private set
    var remotePresented by mutableStateOf(false)
    var grantList by mutableStateOf<List<JsonObject>>(emptyList()); private set
    var managementStatus by mutableStateOf(""); private set
    private var navigation: RemoteNavigationCoordinator? = null
    val playback = RemotePlaybackAdapter()
    private val receiverGuard = RemoteReceiverGuard()
    // A semantic slot may retire before its cleanup. Each task reserves one
    // retained slot before starting; cleanup never creates unbounded waiters.
    private val deferredWorkers = linkedMapOf<String, Job>()
    private var api: RemoteApi? = null
    private var vault: RemoteSecretStorage? = null
    private var receiver: RemoteSecretStorage.Receiver? = null
    private var receiverControl: RemoteControl? = null
    private var identity: String? = null
    var serverInstance: String? = null; private set
    private var active = false
    var mainWindowEligible: () -> Boolean = { false }
    private fun effectsEligible() = uiOwner() && RemoteEffectEligibility.semanticEffects(sceneEligible && scene.eligible, mainWindowEligible(), navigation?.ownedWindowFocused() == true)
    fun windowChanged() { retireNetworkWorkers(); receiverGuard.invalidate(); stopHolding(); playback.physicalInput(); navigation?.windowContextChanged() }
    private var television = false
    private val scene = RemoteSceneEligibility()
    private var lifecycle = UUID.randomUUID().toString()
    private var controllerGeneration = UUID.randomUUID().toString()
    private var acquiredGeneration: String? = null
    private var sequences = RemoteControlSequence()
    private var lastSequence = 0L
    private val commandResult = RemoteCommandResult()
    private var ownerRevision = 0L
    private var stateRevision = 1L
    private var playbackFingerprint: String? = null
    private fun refreshPlaybackContext() {
        val nav = navigation ?: return
        val next = playback.contextFingerprint(nav.scope)
        if (next != playbackFingerprint) { nav.changedContext(); receiverGuard.invalidate(); playbackFingerprint = next }
    }
    private var sendingToken: String? = null
    private var selectedGrant: RemoteSecretStorage.Grant? = null
    private var receiverJob: Job? = null
    private var presenceJob: Job? = null
    private var discoveryJob: Job? = null
    private var stateJob: Job? = null
    private var renewJob: Job? = null
    private var pairingJob: Job? = null
    private var pairingGeneration = UUID.randomUUID().toString()
    data class ScanTicket(val generation: String, val identity: String, val instance: String, val receiverId: String, val target: RemoteTarget, val deadline: Long)
    private var scanTicket: ScanTicket? = null
    private var pendingScan: Pair<String, String>? = null
    var scannedPairing by mutableStateOf<Pair<String, String>?>(null); private set
    var scanRevision by mutableLongStateOf(0); private set
    fun beginQrScan(): ScanTicket? {
        val device = selected ?: return null; val bound = device.target ?: return null
        val owner = identity ?: return null; val instance = serverInstance ?: return null
        if (!current(lifecycle)) return null
        return ScanTicket(UUID.randomUUID().toString(), owner, instance, device.id, bound, SystemClock.elapsedRealtime() + 120000).also { scanTicket = it; pendingScan = null; scannedPairing = null }
    }
    fun finishQrScan(ticket: ScanTicket, payload: String): Boolean {
        if (scanTicket != ticket || identity != ticket.identity || api?.current != true || SystemClock.elapsedRealtime() >= ticket.deadline) return false
        val parsed = RemotePairingQr.parse(payload, ticket.instance, ticket.target) ?: return false
        pendingScan = parsed; restoreScannedSelection(); return true
    }
    private fun restoreScannedSelection() {
        val ticket = scanTicket ?: return; val parsed = pendingScan ?: return
        if (!current(lifecycle)) return
        if (identity != ticket.identity || SystemClock.elapsedRealtime() >= ticket.deadline) { scanTicket = null; pendingScan = null; return }
        val device = devices.firstOrNull { it.id == ticket.receiverId && it.target == ticket.target && it.available } ?: return
        scanTicket = null; pendingScan = null
        select(device); scannedPairing = parsed; scanRevision++
    }

    private var holdJob: Job? = null
    private val dismissed = mutableStateListOf<String>()
    private val observation = Session.observeAuthorizationChanges {
        scope.launch { if (api?.current != true) { vault?.clear(); shutdown(true) } }
    }
    private fun uiOwner() = Looper.myLooper() == Looper.getMainLooper()
    private fun current(generation: String) = uiOwner() && active && RemoteEffectEligibility.sessionAlive(sceneEligible && scene.eligible) && lifecycle == generation && api?.current == true
    private fun retireNetworkWorkers() { deferredWorkers.values.toList().forEach { it.cancel() } }
    fun physicalInput() { navigation?.physicalInput() ?: run { receiverGuard.invalidate(); retireNetworkWorkers(); playback.physicalInput(); stopHolding() } }
    fun suspendNavigation() { shutdown(false); navigation?.suspend(); navigation = null }
    fun sceneChanged(resumed: Boolean, background: Boolean) {
        check(uiOwner()); scene.transition(resumed, background); sceneEligible = resumed
        if (!resumed) { receiverGuard.invalidate(); playback.physicalInput(); navigation?.suspend(); shutdown(false) }
    }
    fun saveEnabled(value: Boolean) { preferences.edit().putBoolean("enabled", value).apply(); enabled = value; if (!value) shutdown(false) }
    fun configure(vm: AppViewModel, nav: RemoteNavigationCoordinator, isTelevision: Boolean) {
        check(uiOwner()); navigation = nav; television = isTelevision
        nav.onPhysicalInput = { receiverGuard.invalidate(); retireNetworkWorkers(); stopHolding(); playback.physicalInput() }
        val instance = vm.serverInstanceId; val user = vm.currentUserId
        val auth = Session.playbackAuthorization()
        if (!enabled || !sceneEligible || instance == null || user == null || auth.token == null) { shutdown(false); return }
        val profile = runCatching { RemoteProfileScope.create(auth.origin, instance, user) }.getOrNull()
        if (profile == null) { shutdown(false); return }
        val nextIdentity = profile.identity
        if (active && identity == nextIdentity && api?.current == true) return
        val changed = identity != null && (identity != nextIdentity || api?.current != true)
        shutdown(changed); identity = nextIdentity; serverInstance = instance
        vault = RemoteSecretStorage(app, nextIdentity); api = RemoteApi(auth); active = true
        val legacyIdentity = profile.origin + ":" + instance + ":" + user
        if (RemoteSecretStorage.legacyPairingPresent(app, legacyIdentity)) managementStatus =
            "Android previously included the server origin in its saved key, but the record did not retain an unambiguous profile tuple. Pair this phone again if its old pairing is unavailable; old proofs are not imported."
        val generation = lifecycle
        discoveryJob = scope.launch { discoveryLoop(generation) }
        if (television) receiverJob = scope.launch { receiverLoop(generation) }
    }
    private fun shutdown(identityChange: Boolean) {
        retireNetworkWorkers()
        active = false; lifecycle = UUID.randomUUID().toString()
        receiverJob?.cancel(); presenceJob?.cancel(); discoveryJob?.cancel(); pairingJob?.cancel()
        receiverJob = null; presenceJob = null; discoveryJob = null; pairingJob = null
        receiverPairLifetime.retire(); receiverPairJob?.cancel(); receiverPairJob = null; pairingOpening = false
        pairingRestricted.reset(); navigation?.unrestrict(pairingRestriction)
        closeController(preserveScan = !identityChange); receiverGuard.deactivate(); target = null; receiverControl = null; receiver = null; challenge = null; pairingExpired = false
        pendingPairings = emptyList(); devices = emptyList(); unavailableNodes = emptyList(); grantList = emptyList()
        if (identityChange) { vault?.clear(); vault = null; identity = null; serverInstance = null; sequences = RemoteControlSequence(); navigation?.resetIdentity() }
        status = if (enabled) "Waiting for an active authenticated Cinema session." else "Cinema remotes disabled."
    }
    private fun parseControl(value: JsonElement?): RemoteControl? = value?.takeUnless { it == JsonNull }?.jsonObject?.let { RemoteControl(RemoteWire.uuid(it.string("control_epoch")), RemoteWire.uuid(it.string("active_grant_id")), RemoteWire.safeLabel(it.string("controller_name"), 80, "Phone")) }
    private suspend fun discoveryLoop(generation: String) {
        while (current(generation)) {
            try {
                val result = api!!.devices(); if (!current(generation)) return
                devices = result.getValue("receivers").jsonArray.take(100).map { element ->
                    val o = element.jsonObject
                    RemoteDevice(RemoteWire.uuid(o.string("receiver_id")), RemoteWire.safeLabel(o.string("name"), 80, "Screen"), o.string("platform"), o["target"]?.takeUnless { it == JsonNull }?.let { RemoteWire.target(it.jsonObject) }, o.getValue("available").jsonPrimitive.boolean, o.getValue("busy").jsonPrimitive.boolean)
                }
                unavailableNodes = result.getValue("unavailable_nodes").jsonArray.take(32).map { it.jsonPrimitive.content }
                selected?.let { old -> val new = devices.firstOrNull { it.id == old.id }; if (new == null || !new.available || new.target != old.target) closeController() }
                restoreScannedSelection()
                status = "Foreground remote discovery connected."
            } catch (cancel: CancellationException) { throw cancel }
            catch (_: Exception) { if (!current(generation)) return; devices = emptyList(); unavailableNodes = emptyList(); status = "Remote discovery unavailable."; closeController() }
            delay(5000)
        }
    }
    private suspend fun receiverLoop(generation: String) {
        var retry = 1000L
        while (current(generation)) {
            try {
                val api = api!!; val vault = vault!!
                if (receiver == null) {
                    receiver = vault.receiver ?: api.register("Android TV").let { result ->
                        if (!current(generation)) return
                        RemoteSecretStorage.Receiver(RemoteWire.uuid(result.string("receiver_id")), result.string("receiver_secret")).also { vault.saveReceiver(it) }
                    }
                }
                val installation = receiver!!
                val result = api.session(installation.id, scene.foregroundId, installation.secret); if (!current(generation)) return
                val bound = RemoteWire.target(result.getValue("target").jsonObject)
                target = bound; receiverControl = null; receiverGuard.deactivate(); restartPresence(generation, bound, installation.secret)
                var delivery = 0L; var revision = 0L
                retry = 1000
                while (current(generation) && target == bound) {
                    val batch = api.poll(bound, delivery, revision, installation.secret)
                    if (!current(generation) || target != bound || RemoteWire.target(batch.getValue("target").jsonObject) != bound) return
                    val nextRevision = batch.number("response_revision"); require(nextRevision in 1..REMOTE_MAX_INTEGER)
                    if (nextRevision >= revision) {
                        val nextControl = parseControl(batch["control"])
                        if (receiverControl != nextControl) { receiverControl = nextControl; receiverGuard.invalidate(); retireNetworkWorkers(); playback.physicalInput(); if (nextControl == null) receiverGuard.deactivate(); restartPresence(generation, bound, installation.secret) }
                        pendingPairings = batch.getValue("pairings").jsonArray.take(8).map { it.jsonObject }
                        syncPairingRestriction()
                        revision = nextRevision
                    }
                    val commands = RemoteWire.pollCommands(batch)
                    val outcomes = commands.mapNotNull { command -> apply(command)?.let { RemoteReceiverGuard.Ack(command.controlEpoch, command.sequence, it.wire) } }
                    if (outcomes.isNotEmpty()) { api.ack(bound, outcomes, installation.secret); if (!current(generation) || target != bound) return }
                    delivery = maxOf(delivery, batch.number("delivery_id"))
                }
            } catch (cancel: CancellationException) { throw cancel }
            catch (failure: Exception) {
                if (!current(generation)) return
                presenceJob?.cancel(); retireReceiverPairing(); target = null; receiverControl = null; receiverGuard.deactivate(); pendingPairings = emptyList(); challenge = null
                if (failure is RemoteHttpFailure && failure.outcome == RemoteOutcome.Unauthorized) {
                    vault?.clearReceiver(); receiver = null
                    status = "Receiver authorization rejected. Sign in again or reset remote registration."
                    return
                }
                status = "Receiver reconnecting; old commands retired."
                delay(retry + kotlin.random.Random.nextLong(0, 250)); retry = (retry * 2).coerceAtMost(30000)
            }
        }
    }
    private fun updateGuard(): RemoteOutcome? {
        val target = target ?: return RemoteOutcome.Unavailable; val control = receiverControl ?: return RemoteOutcome.Unavailable
        val snapshot = navigation?.snapshot() ?: return RemoteOutcome.Unavailable
        return receiverGuard.setContext(RemoteReceiverGuard.Context(control.grant, target, control.epoch, snapshot.context.revision, snapshot.context.focusRevision, snapshot.textNonce))
    }
    private fun restartPresence(generation: String, bound: RemoteTarget, secret: String) {
        presenceJob?.cancel()
        presenceJob = scope.launch {
            while (current(generation) && target == bound) {
                try { val state = receiverState(); api!!.presence(bound, state, secret); if (!current(generation)) return@launch }
                catch (cancel: CancellationException) { throw cancel }
                catch (_: Exception) { if (!current(generation)) return@launch; receiverGuard.invalidate() }
                delay(if (receiverControl == null) 5000 else 250)
            }
        }
    }
    private fun receiverState(): JsonObject {
        refreshPlaybackContext()
        val rawSnapshot = navigation!!.snapshot()
        val playbackEligible = playback.owner?.takeIf { it.scope == navigation!!.scope }?.effectsEligible?.invoke() != false
        val snapshot = if (effectsEligible() && playbackEligible) rawSnapshot else rawSnapshot.copy(route = "restricted", blocked = true, label = null, textNonce = null, capabilities = emptyList())
        if (snapshot.blocked) receiverGuard.invalidate()
        updateGuard(); stateRevision++
        val owner = playback.owner?.takeIf { it.scope == navigation!!.scope }
        if (!snapshot.blocked && receiverControl != null) { receiverGuard.mint(RemoteCreditKind.Interaction, SystemClock.elapsedRealtime()); receiverGuard.mint(RemoteCreditKind.Playback, SystemClock.elapsedRealtime()) }
        val summary = if (snapshot.blocked) null else owner?.summary?.invoke()
        val capabilities = if (snapshot.blocked) emptySet() else snapshot.capabilities.toSet() + owner?.capabilities?.invoke().orEmpty()
        return RemotePresenceState.encode(stateRevision, snapshot, capabilities, if (snapshot.blocked) emptyList() else receiverGuard.currentCredits, summary)
    }

    private fun apply(command: RemoteCommand): RemoteOutcome? {
        if (!current(lifecycle) || !effectsEligible()) return RemoteOutcome.Unavailable
        val navigation = navigation ?: return RemoteOutcome.Unavailable
        if (playback.owner?.takeIf { it.scope == navigation.scope }?.effectsEligible?.invoke() == false) { receiverGuard.invalidate(); return RemoteOutcome.Restricted }
        refreshPlaybackContext()
        updateGuard()?.let { return it }
        val snapshot = navigation.snapshot()
        receiverGuard.retainedResult(command, SystemClock.elapsedRealtime())?.let { return RemoteOutcome.entries.first { outcome -> outcome.wire == it.outcome } }
        if (receiverGuard.pending(command)) return null
        val incumbent = playback.owner?.takeIf { it.scope == navigation.scope }
        val deferred = if (!snapshot.blocked) incumbent?.deferred?.invoke(command.action) ?: navigation.deferred(command.action, snapshot.context) else null
        if (deferred != null) {
            if (deferredWorkers.size >= 4) return RemoteOutcome.Unavailable
            val bound = target ?: return RemoteOutcome.Unavailable
            val installation = receiver ?: return RemoteOutcome.Unavailable
            val api = api ?: return RemoteOutcome.Unavailable
            val reservation = receiverGuard.reserve(command, deferred.binding, SystemClock.elapsedRealtime())
            if (reservation is RemoteReservation.Refused) return reservation.outcome
            val permit = (reservation as RemoteReservation.Admitted).permit
            val generation = lifecycle
            val ticket = UUID.randomUUID().toString()
            fun owned(): Boolean {
                if (!current(generation) || target != bound || !effectsEligible() || navigation.context != snapshot.context || !deferred.stillOwned()) return false
                updateGuard()?.let { return false }
                return true
            }
            fun effectCheck() = owned() && receiverGuard.permits(permit, deferred.binding, SystemClock.elapsedRealtime())
            val job = scope.launch(start = CoroutineStart.LAZY) {
                try {
                    val remaining = permit.resultDeadline - SystemClock.elapsedRealtime()
                    val outcome = if (remaining <= 0) RemoteOutcome.Unavailable else try {
                        withTimeout(remaining) { if (effectCheck()) deferred.run(::effectCheck) else RemoteOutcome.Unavailable }
                    } catch (_: TimeoutCancellationException) { RemoteOutcome.Unavailable }
                    catch (cancel: CancellationException) { throw cancel }
                    catch (_: Exception) { RemoteOutcome.Unavailable }
                    val ack = if (owned()) receiverGuard.complete(permit, deferred.binding, SystemClock.elapsedRealtime(), outcome) else {
                        receiverGuard.abandon(permit); null
                    }
                    // This callback is synchronous and bound to the same exact
                    // owner. It runs only after the guard recorded terminal ACK.
                    if (ack != null && owned()) {
                        deferred.didComplete(outcome)
                        if (current(generation) && target == bound) api.ack(bound, listOf(ack), installation.secret)
                    }
                } finally {
                    if (owned()) receiverGuard.complete(permit, deferred.binding, SystemClock.elapsedRealtime(), RemoteOutcome.Unavailable) else receiverGuard.abandon(permit)
                    withContext(NonCancellable) { try { deferred.dispose() } finally { deferredWorkers.remove(ticket) } }
                }
            }
            deferredWorkers[ticket] = job; job.start()
            return null
        }
        return receiverGuard.apply(command, SystemClock.elapsedRealtime(), if (snapshot.blocked) RemoteOutcome.Restricted else null) {
            val owner = playback.owner?.takeIf { it.scope == navigation.scope }
            if (command.action.type in setOf("back", "home") || command.action.type == "stop" && owner?.capabilities?.invoke()?.contains("stop") == true) { receiverGuard.retireDeferred(); retireNetworkWorkers() }
            if (owner != null && command.action.type in owner.capabilities()) playback.dispatch(command.action, navigation.scope)
            else navigation.dispatch(command.action, snapshot.context)
        }
    }
    private fun syncPairingRestriction() {
        val next = pairingOpening || challenge != null || pairingExpired || pendingPairings.isNotEmpty()
        navigation?.restrict(pairingRestriction, next)
        if (pairingRestricted.transition(next)) { receiverGuard.invalidate(); retireNetworkWorkers(); playback.physicalInput() }
    }
    private fun retireReceiverPairing() { receiverPairLifetime.retire(); receiverPairJob?.cancel(); receiverPairJob = null; pairingOpening = false; challenge = null; pairingExpired = false; syncPairingRestriction() }
    fun startPairing() {
        val bound = target ?: return; val installation = receiver ?: return; val generation = lifecycle
        if (!current(generation)) return
        receiverPairJob?.cancel(); val ticket = receiverPairLifetime.begin()
        challenge = null; pairingExpired = false; pairingOpening = true; syncPairingRestriction()
        val startedAt = SystemClock.elapsedRealtime()
        receiverPairJob = scope.launch {
            try {
                val result = api!!.pairStart(bound, installation.secret)
                if (current(generation) && target == bound && receiverPairLifetime.accepts(ticket)) {
                    val expiry = RemoteChallengeExpiry(startedAt, result.getValue("expires_in_ms").jsonPrimitive.long)
                    val remaining = expiry.remaining(SystemClock.elapsedRealtime())
                    pairingOpening = false
                    if (remaining <= 0) pairingExpired = true else challenge = result
                    syncPairingRestriction()
                    if (remaining > 0) {
                        delay(remaining)
                        if (current(generation) && target == bound && receiverPairLifetime.accepts(ticket)) { challenge = null; pairingExpired = true; syncPairingRestriction() }
                    }
                }
            } catch (cancel: CancellationException) { throw cancel }
            catch (_: Exception) { if (current(generation) && receiverPairLifetime.accepts(ticket)) { pairingOpening = false; status = "Pairing unavailable."; syncPairingRestriction() } }
        }
    }
    fun hidePairing() { retireReceiverPairing() }
    fun approve(pending: JsonObject, allowed: Boolean) {
        val bound = target ?: return; val installation = receiver ?: return; val generation = lifecycle
        if (!current(generation)) return
        val ticket = receiverPairLifetime.begin() // approval owns this visible pairing surface
        receiverPairJob?.cancel(); receiverPairJob = null; pairingOpening = false; challenge = null; pairingExpired = false; syncPairingRestriction()
        scope.launch {
            try {
                api!!.pairApprove(bound, RemoteWire.uuid(pending.string("pending_id")), allowed, installation.secret)
                if (current(generation) && target == bound && receiverPairLifetime.accepts(ticket)) { pendingPairings = pendingPairings.filter { it != pending }; challenge = null; syncPairingRestriction() }
            } catch (cancel: CancellationException) { throw cancel }
            catch (_: Exception) { if (current(generation) && receiverPairLifetime.accepts(ticket)) status = "Pairing approval could not be confirmed." }
        }
    }
    val localReceiverId get() = receiver?.id ?: vault?.receiver?.id
    val isPaired get() = selectedGrant != null
    val controlledByOther get() = control != null && control?.grant != selectedGrant?.id
    val localGrants get() = vault?.grants.orEmpty()
    fun ownsCompanionProfile(profile: RemoteProfileScope): Boolean = enabled && active && sceneEligible && identity == profile.identity && api?.current == true
    fun select(device: RemoteDevice) {
        closeController(); selected = device
        if (!device.available || device.target == null) return
        selectedGrant = vault?.grants?.firstOrNull { it.receiverId == device.id }
        remotePresented = true
        if (selectedGrant != null) beginStatePoll()
    }
    fun closeController(preserveScan: Boolean = false) {
        if (!preserveScan) { scanTicket = null; pendingScan = null; scannedPairing = null }
        pairingGeneration = UUID.randomUUID().toString(); pairingJob?.cancel(); pairingJob = null
        controllerGeneration = UUID.randomUUID().toString(); commandResult.reset(); acquiredGeneration = null; stateJob?.cancel(); renewJob?.cancel(); stopHolding()
        stateJob = null; renewJob = null; controlling = false; selected = null; selectedGrant = null; state = null; control = null; ownerRevision = 0; lastSequence = 0; sendingToken = null
    }
    fun pair(code: String, challengeId: String?) {
        val device = selected ?: return; val target = device.target ?: return; val api = api ?: return; val vault = vault ?: return
        val generation = lifecycle
        require(Regex("[0-9]{8}").matches(code))
        pairingJob?.cancel(); pairingGeneration = UUID.randomUUID().toString()
        val pairing = pairingGeneration
        val deadline = RemotePairingDeadline(SystemClock.elapsedRealtime())
        val expiredMessage = "Pairing expired. Start a new TV code and try again."
        fun pairingCurrent() = current(generation) && RemotePairingAdmission.accepts(pairingGeneration, pairing, selected?.id, device.id, selected?.target, target)
        pairingJob = scope.launch {
            try {
                val remaining = deadline.remaining(SystemClock.elapsedRealtime())
                if (remaining <= 0) { if (pairingCurrent()) commandStatus = expiredMessage; return@launch }
                withTimeout(remaining) {
                    val claim = api.pairClaim(target, challengeId, code, "Android phone"); if (!pairingCurrent()) return@withTimeout
                    if (!deadline.admits(SystemClock.elapsedRealtime())) { commandStatus = expiredMessage; return@withTimeout }
                    val pending = RemoteWire.uuid(claim.string("pending_id")); val proof = claim.string("poll_secret")
                    repeat(120) {
                        delay(1000)
                        if (!pairingCurrent()) return@withTimeout
                        if (!deadline.admits(SystemClock.elapsedRealtime())) { commandStatus = expiredMessage; return@withTimeout }
                        val result = api.pairResult(target, pending, proof); if (!pairingCurrent()) return@withTimeout
                        if (!deadline.admits(SystemClock.elapsedRealtime())) { commandStatus = expiredMessage; return@withTimeout }
                        when (result.string("status")) {
                            "denied" -> { commandStatus = "Pairing declined on TV."; return@withTimeout }
                            "approved" -> {
                                require(RemoteWire.uuid(result.string("receiver_id")) == device.id)
                                if (!deadline.admits(SystemClock.elapsedRealtime())) { commandStatus = expiredMessage; return@withTimeout }
                                vault.saveGrant(RemoteSecretStorage.Grant(device.id, RemoteWire.uuid(result.string("grant_id")), result.string("grant_secret")))
                                pairingJob = null
                                select(device); commandStatus = "Paired. Tap Use as remote."; return@withTimeout
                            }
                        }
                        commandStatus = "Waiting for physical TV approval."
                    }
                    if (pairingCurrent()) commandStatus = expiredMessage
                }
            } catch (_: TimeoutCancellationException) { if (pairingCurrent()) commandStatus = expiredMessage }
            catch (cancel: CancellationException) { throw cancel }
            catch (_: Exception) { if (pairingCurrent()) commandStatus = if (deadline.admits(SystemClock.elapsedRealtime())) "Pairing failed. Start a new TV code." else expiredMessage }
        }
    }
    private fun acceptControl(next: RemoteControl?, revision: Long) {
        if (revision < ownerRevision) return
        ownerRevision = revision
        if (next?.epoch != control?.epoch) { state = null; stopHolding(); sendingToken = null; lastSequence = 0 }
        control = next
        val bound = selected?.target; val grant = selectedGrant
        controlling = acquiredGeneration == controllerGeneration && next != null && bound != null && grant != null && next.grant == grant.id && sequences.knows(bound, grant.id, next.epoch)
        if (!controlling) { acquiredGeneration = null; stopHolding() }
    }
    fun acquire(takeover: Boolean = false) {
        val bound = selected?.target ?: return; val grant = selectedGrant ?: return; val api = api ?: return
        val recoveryEpoch = control?.takeIf { it.grant == grant.id && !sequences.knows(bound, grant.id, it.epoch) }?.epoch
        controllerGeneration = UUID.randomUUID().toString(); commandResult.reset(); acquiredGeneration = null; stateJob?.cancel(); renewJob?.cancel(); stopHolding(); controlling = false; sendingToken = null; state = null
        val generation = controllerGeneration; val life = lifecycle
        scope.launch {
            try {
                val fresh = takeover || recoveryEpoch != null
                var reply = api.control(bound, grant.id, if (fresh) "takeover" else "acquire", if (takeover) null else recoveryEpoch, grant.secret)
                if (!current(life) || generation != controllerGeneration || selected?.target != bound) return@launch
                require(RemoteWire.target(reply.getValue("target").jsonObject) == bound)
                var owned = parseControl(reply["control"])
                if (!fresh && owned?.grant == grant.id && !sequences.knows(bound, grant.id, owned.epoch)) {
                    reply = api.control(bound, grant.id, "takeover", owned.epoch, grant.secret)
                    if (!current(life) || generation != controllerGeneration || selected?.target != bound) return@launch
                    require(RemoteWire.target(reply.getValue("target").jsonObject) == bound); owned = parseControl(reply["control"])
                }
                if (owned?.grant == grant.id) { sequences.acquired(bound, grant.id, owned.epoch); acquiredGeneration = generation }
                acceptControl(owned, reply.number("response_revision")); beginStatePoll(); if (controlling) beginRenewal()
            } catch (cancel: CancellationException) { throw cancel }
            catch (_: Exception) { if (current(life) && generation == controllerGeneration) { commandStatus = "Control unavailable. Tap again to request it."; beginStatePoll() } }
        }
    }
    private fun beginStatePoll() {
        stateJob?.cancel(); val generation = controllerGeneration; val life = lifecycle
        val bound = selected?.target ?: return; val grant = selectedGrant ?: return; val api = api ?: return
        stateJob = scope.launch {
            while (current(life) && generation == controllerGeneration) {
                try {
                    val result = api.state(bound, grant.id, ownerRevision, grant.secret)
                    if (!current(life) || generation != controllerGeneration || selected?.target != bound) return@launch
                    require(RemoteWire.target(result.getValue("target").jsonObject) == bound)
                    val revision = result.number("response_revision"); if (revision < ownerRevision) continue
                    val oldContext = state?.get("context_revision")
                    acceptControl(parseControl(result["control"]), revision)
                    val nextState = RemoteStateUpdate.applying(state, result["state"])
                    if (oldContext != nextState?.get("context_revision")) stopHolding()
                    state = nextState
                    result.getValue("outcomes").jsonArray.lastOrNull { it.jsonObject.string("control_epoch") == control?.epoch && it.jsonObject.number("sequence") == lastSequence }?.jsonObject?.let { ack ->
                        val outcome = RemoteOutcome.parse(ack.string("outcome"))
                        if (commandResult.observe(ack.string("control_epoch"), ack.number("sequence"), outcome)) commandStatus = outcome.viewerMessage()
                    }
                } catch (cancel: CancellationException) { throw cancel }
                catch (_: Exception) { if (!current(life) || generation != controllerGeneration) return@launch; controlling = false; acquiredGeneration = null; stopHolding(); commandStatus = "Connection lost; no command replayed."; delay(1000) }
            }
        }
    }
    private fun beginRenewal() {
        renewJob?.cancel(); val generation = controllerGeneration; val life = lifecycle
        val bound = selected?.target ?: return; val grant = selectedGrant ?: return; val api = api ?: return
        renewJob = scope.launch {
            while (current(life) && generation == controllerGeneration && controlling) {
                delay(5000); if (!current(life) || generation != controllerGeneration || !controlling) return@launch
                val epoch = control?.epoch ?: return@launch
                try { val reply = api.control(bound, grant.id, "renew", epoch, grant.secret); if (current(life) && generation == controllerGeneration) { require(RemoteWire.target(reply.getValue("target").jsonObject) == bound); acceptControl(parseControl(reply["control"]), reply.number("response_revision")) } }
                catch (cancel: CancellationException) { throw cancel }
                catch (_: Exception) { if (generation == controllerGeneration) { controlling = false; acquiredGeneration = null; stopHolding(); commandStatus = "Lease lost. Tap Use as remote." }; return@launch }
            }
        }
    }
    fun releaseControl() {
        val bound = selected?.target; val grant = selectedGrant; val epoch = control?.epoch; val api = api
        closeController()
        if (bound != null && grant != null && epoch != null && api?.current == true) scope.launch { runCatching { api.control(bound, grant.id, "release", epoch, grant.secret) } }
    }
    fun supports(action: String) = state?.get("capabilities")?.jsonArray?.any { it.jsonPrimitive.content == action } == true
    fun send(action: RemoteAction) {
        if (!current(lifecycle) || !effectsEligible() || !controlling || sendingToken != null) return
        val bound = selected?.target ?: return; val grant = selectedGrant ?: return; val control = control ?: return; val state = state ?: return; val api = api ?: return
        if (action.type !in state.getValue("capabilities").jsonArray.map { it.jsonPrimitive.content }) return
        if (runCatching { action.validate() }.isFailure) return
        val credit = state.getValue("credits").jsonArray.lastOrNull { it.jsonObject.string("kind") == action.creditKind.wire }?.jsonObject ?: return
        val sequence = sequences.next(bound, grant.id, control.epoch) ?: return; lastSequence = sequence; commandResult.begin(control.epoch, sequence)
        val command = RemoteCommand(bound, grant.id, control.epoch, sequence, RemoteWire.uuid(credit.string("nonce")), state.number("context_revision"), state.number("focus_revision"), action)
        val token = UUID.randomUUID().toString(); sendingToken = token; val generation = controllerGeneration; val life = lifecycle
        scope.launch {
            try {
                val result = api.send(command, grant.secret)
                if (!current(life) || generation != controllerGeneration || sendingToken != token || this@RemoteClientModel.control?.epoch != control.epoch) return@launch
                require(result.getValue("queued").jsonPrimitive.boolean && result.string("control_epoch") == control.epoch && result.number("sequence") == sequence)
                commandStatus = commandResult.known(control.epoch, sequence)?.viewerMessage() ?: "Sent. Waiting for TV outcome."
            } catch (cancel: CancellationException) { throw cancel }
            catch (failure: Exception) { if (current(life) && generation == controllerGeneration && sendingToken == token) { stopHolding(); commandStatus = commandResult.known(control.epoch, sequence)?.viewerMessage() ?: if (failure is RemoteHttpFailure) failure.outcome.viewerMessage() else "Outcome unknown; never replayed." } }
            finally { if (generation == controllerGeneration && sendingToken == token) sendingToken = null }
        }
    }
    fun beginHolding(direction: String) { stopHolding(); val action = RemoteAction("navigate", buildJsonObject { put("direction", direction) }); send(action); holdJob = scope.launch { delay(350); while (isActive && controlling) { send(action); delay(125) } } }
    fun stopHolding() { holdJob?.cancel(); holdJob = null }
    val suggestions get() = devices.filter { device -> device.available && device.target != null && vault?.grants?.any { it.receiverId == device.id } == true && device.id !in dismissed && preferences.getBoolean("suggest:" + device.id, true) }
    fun dismissSuggestion() { dismissed.addAll(suggestions.map { it.id }) }
    fun suggestionsEnabled(id: String) = preferences.getBoolean("suggest:" + id, true)
    fun setSuggestions(id: String, enabled: Boolean) { preferences.edit().putBoolean("suggest:" + id, enabled).apply() }
    fun refreshGrants() { val api = api ?: return; val life = lifecycle; scope.launch { try { val result = api.grants(); if (current(life)) grantList = result.getValue("grants").jsonArray.map { it.jsonObject }.filter { !television || it.string("receiver_id") == receiver?.id }.take(400) } catch (_: Exception) { if (current(life)) managementStatus = "Pairings unavailable." } } }
    fun revokeGrant(id: String) { val api = api ?: return; val life = lifecycle; if (selectedGrant?.id == id) releaseControl(); scope.launch { try { val result = api.revokeGrant(id); if (current(life) && result.getValue("revoked").jsonPrimitive.boolean) { vault?.forgetGrant(id); grantList = grantList.filter { it.string("id") != id }; if (receiverControl?.grant == id) { receiverControl = null; receiverGuard.deactivate() }; managementStatus = "Pairing revoked." } } catch (_: Exception) { if (current(life)) managementStatus = "Revocation not confirmed. Retry online." } } }
    fun forgetGrant(id: String) { if (selectedGrant?.id == id) releaseControl(); runCatching { vault?.forgetGrant(id) }.onSuccess { managementStatus = "Saved pairing forgotten; revoke separately on server." }.onFailure { managementStatus = "Could not forget pairing." } }
    fun resetReceiver() { val api = api ?: return; val id = receiver?.id ?: vault?.receiver?.id ?: return; val life = lifecycle; scope.launch { try { val result = api.unregister(id); if (current(life) && result.getValue("revoked").jsonPrimitive.boolean) { receiverJob?.cancel(); presenceJob?.cancel(); retireReceiverPairing(); vault?.clearReceiver(); receiver = null; target = null; receiverControl = null; receiverGuard.deactivate(); pendingPairings = emptyList(); challenge = null; grantList = emptyList(); managementStatus = "Registration removed. Toggle Cinema remotes off/on to register again." } } catch (_: Exception) { if (current(life)) managementStatus = "Reset could not be confirmed." } } }
}
internal val LocalRemoteClient = staticCompositionLocalOf<RemoteClientModel?> { null }
