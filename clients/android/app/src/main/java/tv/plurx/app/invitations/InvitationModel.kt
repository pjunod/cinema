package tv.plurx.app.invitations

import android.content.Context
import androidx.compose.runtime.*
import kotlinx.coroutines.*
import tv.plurx.app.data.Session
import tv.plurx.app.remote.*
import tv.plurx.app.ui.AppViewModel
import java.security.MessageDigest
import java.util.UUID

internal object InvitationRuntime {
    private var model: InvitationModel? = null
    private var pendingToken: String? = null
    @Synchronized fun get(context: Context): InvitationModel = model ?: InvitationModel(context.applicationContext).also {
        model = it; pendingToken?.let(it::rotate)
    }
    @Synchronized fun tokenRotated(token: String) {
        if (token.toByteArray(Charsets.UTF_8).size !in 1..4096 || token.any { Character.isISOControl(it) || it.isWhitespace() }) return
        pendingToken = token; model?.rotate(token)
    }
}

internal class InvitationModel(private val app: Context) {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private var profile: RemoteProfileScope? = null
    private var authorization: Session.PlaybackAuthorization? = null
    private var remote: RemoteClientModel? = null
    private var epoch = 0L
    private var job: Job? = null
    private val index = InvitationProfileIndex(app)
    var authorizationGeneration by mutableLongStateOf(Session.playbackAuthorization().generation); private set
    var status by mutableStateOf("Sign in to manage screen invitations."); private set
    var busy by mutableStateOf(false); private set
    var local by mutableStateOf<InvitationLocalState?>(null); private set
    var installations by mutableStateOf<List<InvitationPhone>>(emptyList()); private set
    var nextCursor by mutableStateOf<String?>(null); private set
    var corrupt by mutableStateOf(false); private set
    private val rotation = InvitationRotationQueue()
    init {
        Session.observeAuthorizationChanges { observed ->
            // Immediate local effect fence; obsolete observer callbacks cannot retire a replacement login.
            synchronized(InvitationNotifications.lock) {
                if (Session.playbackAuthorization().generation == observed) runCatching { index.activate(null) }
            }
            scope.launch {
                val current = Session.playbackAuthorization()
                if (current.generation != observed) return@launch
                authorizationGeneration = observed
                if (authorization != current) {
                    retire(); status = "Sign in again; explicitly rebind invitations to this login."
                }
            }
        }
    }
    private fun storage() = InvitationStorage(app, requireNotNull(profile))
    val screenIds get() = InvitationScreenInventory.ids(local?.choices.orEmpty(), local?.consents.orEmpty(), pairedGrants)
    val pairedGrants get() = profile?.let { RemoteSecretStorage(app, it.identity).grants }.orEmpty()
    private fun current(generation: Long) = epoch == generation && authorization != null && Session.playbackAuthorization() == authorization
    private fun retire() { epoch++; job?.cancel(); job = null; busy = false; rotation.clearAuthority(); InvitationResidentService.stop(app) }
    fun configure(vm: AppViewModel, owner: RemoteClientModel) {
        val auth = Session.playbackAuthorization()
        val selected = if (auth.token != null && vm.serverInstanceId != null && vm.currentUserId != null)
            runCatching { RemoteProfileScope.create(auth.origin, requireNotNull(vm.serverInstanceId), requireNotNull(vm.currentUserId)) }.getOrNull() else null
        if (selected == profile && auth == authorization) { remote = owner; resume(); return }
        retire(); profile = selected; authorization = if (selected == null) null else auth; remote = owner
        installations = emptyList(); nextCursor = null; local = null; corrupt = false
        if (selected == null) { status = "Sign in to manage screen invitations."; return }
        runCatching {
            synchronized(InvitationNotifications.lock) {
                index.add(selected); val saved = storage().read()
                // A remembered service choice is never a restart instruction.
                val next = saved.copy(runRequested = false, pendingAvailabilityOff = saved.pendingAvailabilityOff || saved.runRequested)
                storage().save(next); local = next
                if (next.loginFingerprint == fingerprint(auth)) index.activate(selected) else index.activate(null)
            }
            status = if (local?.loginFingerprint == fingerprint(auth)) "Screen choices are saved on this phone." else "Register or explicitly rebind this phone to the current login."
        }.onFailure { corrupt = true; status = "Saved invitation records cannot be read or the saved-profile limit was reached. Reset locally, then inspect home installations for orphan records." }
        resume()
    }
    private fun fingerprint(auth: Session.PlaybackAuthorization): String = MessageDigest.getInstance("SHA-256")
        .digest(requireNotNull(auth.token).toByteArray(Charsets.UTF_8)).joinToString("") { "%02x".format(it) }
    private fun update(transform: (InvitationLocalState) -> InvitationLocalState): InvitationLocalState = synchronized(InvitationNotifications.lock) {
        val store = storage(); val next = transform(store.read()); store.save(next); local = next; next
    }
    private fun api() = InvitationApi(requireNotNull(authorization))
    private fun launch(allowCorrupt: Boolean = false, action: suspend (Long) -> Unit) {
        if (busy || corrupt && !allowCorrupt || authorization == null) return
        val generation = epoch; busy = true
        job = scope.launch {
            try { action(generation) }
            catch (_: CancellationException) { }
            catch (error: Exception) { if (current(generation)) status = when ((error as? InvitationHttpFailure)?.code) {
                "stale_generation" -> "Screen settings changed. Refresh before trying again."
                "login_changed" -> "Explicitly rebind this phone to the current login."
                else -> "Could not confirm the change. Your local choice is saved; refresh or retry explicitly."
            } }
            finally { if (epoch == generation) { busy = false; job = null; drainRotation() } }
        }
    }
    fun register() = launch { generation ->
        val saved = requireNotNull(local)
        check(!saved.registrationAttempted && saved.installation == null) { "Remove the uncertain home installation before registering again." }
        index.add(requireNotNull(profile))
        val id = UUID.randomUUID().toString()
        update { it.copy(installation = id, registrationAttempted = true, lostProof = true) }
        val reply = api().register(id, "Android phone")
        if (!current(generation)) return@launch
        val secret = requireNotNull(reply.phone_secret)
        update { it.copy(phone = reply.phone, phoneSecret = secret, lostProof = false, loginFingerprint = fingerprint(requireNotNull(authorization))) }
        index.activate(requireNotNull(profile)); status = "Phone registered. Choose invitations separately for each paired screen."
    }
    fun refresh() = launch { generation ->
        var state = requireNotNull(local)
        if (state.pendingDeletion) {
            api().remove(requireNotNull(state.installation)); if (!current(generation)) return@launch
            storage().reset(); local = storage().empty(); index.activate(null); status = "Pending installation deletion confirmed."; return@launch
        }
        val id = state.installation ?: return@launch
        var cursor: String? = null; var proved: InvitationPhone? = null
        do {
            val page = api().phones(cursor); if (!current(generation)) return@launch
            proved = proved ?: page.phones.firstOrNull { it.installation_id == id }
            require(page.next_cursor == null || page.next_cursor != cursor); cursor = page.next_cursor
        } while (cursor != null)
        val phone = requireNotNull(proved); require(phone.platform == "android")
        update { it.copy(phone = phone) }; state = requireNotNull(local)
        val secret = state.phoneSecret ?: return@launch
        if (state.pendingAvailabilityOff && !state.runRequested) { availability(false, false, generation); if (!current(generation)) return@launch }
        var after: String? = null; val rows = mutableListOf<InvitationConsent>()
        do {
            val page = api().consents(phone.installation_id, secret, after)
            if (!current(generation)) return@launch
            rows += page.consents; require(rows.size <= 160); require(page.next_cursor == null || page.next_cursor != after)
            after = page.next_cursor
        } while (after != null)
        update { it.copy(consents = rows) }
        status = "Screen readiness refreshed."
    }
    fun choose(receiver: String, enabled: Boolean, mode: String) {
        RemoteWire.uuid(receiver); require(mode in setOf("fcm", "android_resident"))
        if (corrupt || local == null) return
        epoch++; job?.cancel(); busy = false
        if (!enabled) rotation.explicitRetry(receiver)
        update { state ->
            val previous = state.choices.firstOrNull { it.receiver == receiver }
            val revision = (previous?.intentRevision ?: 0) + 1; InvitationWire.counter(revision)
            state.copy(choices = state.choices.filter { it.receiver != receiver } + InvitationChoice(receiver, enabled, mode, revision, true))
        }
        status = "Choice saved on this phone. Save to the home when connected."
    }
    fun save(receiver: String) = launch { generation ->
        val state = requireNotNull(local); val phone = requireNotNull(state.phone); val secret = requireNotNull(state.phoneSecret)
        val choice = requireNotNull(state.choices.firstOrNull { it.receiver == receiver })
        val grant = pairedGrants.firstOrNull { it.receiverId == receiver }
        val previous = state.consents.firstOrNull { it.receiver_id == receiver }
        val reply = api().consent(phone.installation_id, receiver, phone.phone_generation, previous?.consent_generation ?: 0,
            choice.enabled, if (choice.enabled) grant?.id else null, if (choice.enabled) choice.transport else null, secret, if (choice.enabled) grant?.secret else null)
        if (!current(generation)) return@launch
        update { it.copy(consents = it.consents.filter { row -> row.receiver_id != receiver } + reply.consent,
            choices = it.choices.map { row -> if (row.receiver == receiver && row.intentRevision == choice.intentRevision) row.copy(pendingSync = false) else row }) }
        status = "Screen choice saved. " + reply.consent.readiness.status.replace('_', ' ')
    }
    private suspend fun availability(permission: Boolean, resident: Boolean, generation: Long) {
        val state = requireNotNull(local); val phone = requireNotNull(state.phone)
        val reply = api().availability(phone.installation_id, phone.phone_generation, permission, resident, requireNotNull(state.phoneSecret))
        if (!current(generation)) return
        update { it.copy(phone = reply.phone, pendingAvailabilityOff = false) }
    }
    fun updatePermission() {
        if (local?.runRequested == true) { status = "The running resident receiver owns availability. Stop it before refreshing permission."; return }
        launch { generation -> availability(InvitationNotifications.permission(app), false, generation) }
    }
    fun enroll(receiver: String) {
        rotation.explicitRetry(receiver)
        launch { generation ->
            try { enrollNow(receiver, generation, null) }
            catch (error: Exception) { if (current(generation)) rotation.unknown(receiver); throw error }
        }
    }
    private suspend fun enrollNow(receiver: String, generation: Long, rotated: String?) {
        check(InvitationFirebase.configured && InvitationNotifications.permission(app))
        val state = requireNotNull(local); val phone = requireNotNull(state.phone); val secret = requireNotNull(state.phoneSecret)
        check(state.loginFingerprint == fingerprint(requireNotNull(authorization)))
        val choice = requireNotNull(state.choices.firstOrNull { it.receiver == receiver && it.enabled && !it.pendingSync && it.transport == "fcm" })
        val grant = requireNotNull(pairedGrants.firstOrNull { it.receiverId == receiver })
        val consent = requireNotNull(state.consents.firstOrNull { it.receiver_id == receiver && it.enabled && it.transport == "fcm" && it.grant_id == grant.id })
        val token = rotated ?: InvitationFirebase.token()
        if (!current(generation)) return
        val start = api().start(phone.installation_id, receiver, grant.id, phone.phone_generation, consent.consent_generation, secret, grant.secret)
        if (!current(generation)) return
        update { it.copy(consents = it.consents.filter { row -> row.receiver_id != receiver } + start.consent) }
        val ticket = start.ticket ?: run { status = "Push is not configured on this home. Saved ON is preserved; resident transport is an explicit alternative."; return }
        val valid = { current(generation) && local?.choices?.any { it.receiver == receiver && it.intentRevision == choice.intentRevision && it.enabled && !it.pendingSync } == true }
        InvitationBrokerApi().claim(ticket, token, valid)
        if (!valid()) return
        val reply = api().confirm(phone.installation_id, receiver, grant.id, ticket.ticket_id, phone.phone_generation,
            start.consent.consent_generation, start.consent.transport_generation, secret, grant.secret)
        if (!valid()) return
        update { it.copy(consents = it.consents.filter { row -> row.receiver_id != receiver } + reply.consent) }
        rotation.complete(receiver, token, requireNotNull(rotationBindings()[receiver])); status = "Push enrollment confirmed. Physical delivery remains unverified."
    }
    private fun rotationBindings(): Map<String, String> {
        val state = local ?: return emptyMap(); val phone = state.phone ?: return emptyMap()
        if (state.loginFingerprint != authorization?.let(::fingerprint)) return emptyMap()
        return state.consents.filter { consent -> consent.enabled && consent.transport == "fcm" && consent.readiness.eligible &&
            state.choices.any { it.receiver == consent.receiver_id && it.enabled && !it.pendingSync && it.transport == "fcm" } &&
            pairedGrants.any { it.receiverId == consent.receiver_id && it.id == consent.grant_id }
        }.associate { consent -> consent.receiver_id to listOf(phone.installation_id, phone.phone_generation, state.loginFingerprint,
            consent.grant_id, consent.consent_generation, consent.transport_generation,
            state.choices.first { it.receiver == consent.receiver_id }.intentRevision).joinToString(":") }
    }
    fun rotate(token: String) { scope.launch { rotation.offer(token); drainRotation() } }
    private fun drainRotation() {
        if (busy || authorization == null || !InvitationFirebase.configured) return
        val work = rotation.remaining(rotationBindings())
        if (work.isEmpty()) return
        launch { generation ->
            work.forEach { (receiver, token) ->
                if (rotationBindings()[receiver] == null) return@forEach
                if (!current(generation)) return@launch
                try { enrollNow(receiver, generation, token) }
                catch (error: Exception) {
                    // Keep the pending screen. An unknown attempt needs fresh explicit renewal, not an automatic replay.
                    rotation.unknown(receiver)
                    throw error
                }
            }
        }
    }
    fun rebind() = launch { generation ->
        val state = requireNotNull(local); val phone = requireNotNull(state.phone)
        val proof = requireNotNull(state.phoneSecret)
        chooseAllOff(); InvitationResidentService.stop(app); index.activate(null); update { it.copy(loginFingerprint = null, consents = emptyList()) }
        val reply = api().rebind(phone.installation_id, phone.phone_generation, proof)
        if (!current(generation)) return@launch
        update { it.copy(phone = reply.phone, consents = emptyList(), loginFingerprint = fingerprint(requireNotNull(authorization)), pendingAvailabilityOff = false) }
        index.activate(requireNotNull(profile)); status = "Bound to this login. Saved screen choices require an explicit Save again."
    }
    fun listInstallations(after: String? = null) = launch(allowCorrupt = true) { generation ->
        val page = api().phones(after)
        if (!current(generation)) return@launch
        installations = if (after == null) page.phones else (installations + page.phones).distinctBy { it.installation_id }.also { require(it.size <= 20) }
        nextCursor = page.next_cursor; status = "Select a home installation explicitly to remove it."
    }
    fun removeInstallation(id: String) {
        RemoteWire.uuid(id)
        if (local?.installation == id) {
            chooseAllOff(); update { it.copy(pendingDeletion = true) }; InvitationResidentService.stop(app)
        }
        launch(allowCorrupt = true) { generation ->
            api().remove(id)
            if (!current(generation)) return@launch
            if (local?.installation == id) { storage().reset(); local = storage().empty(); index.activate(null) }
            installations = installations.filter { it.installation_id != id }; status = "Home installation removed."
        }
    }
    private fun chooseAllOff() { update { it.copy(runRequested = false, residentChoice = false,
        choices = it.choices.map { row -> row.copy(enabled = false, pendingSync = true) }, pendingAvailabilityOff = true) } }
    fun resetLocal() {
        retire(); runCatching { storage().reset(); index.activate(null); local = storage().empty(); corrupt = false }
        status = "Local records reset. Home installations may remain; use the signed-in installation list to remove them."
    }
    fun resetIndex() { retire(); index.resetCorruptIndex(); local = null; corrupt = false; status = "Local profile index reset. Unknown home installations were not revoked." }
    fun residentChoice(value: Boolean) { update { it.copy(residentChoice = value) }; if (!value) stopResident() }
    fun startResident() {
        if (profile == null || corrupt) return
        local = synchronized(InvitationNotifications.lock) { storage().read() }
        val state = local ?: return
        if (state.runRequested) { status = "Resident receiver is already requested; Stop before starting another run."; return }
        if (!state.residentChoice || state.phoneSecret == null || state.loginFingerprint != authorization?.let(::fingerprint) || !InvitationNotifications.permission(app)) {
            status = "Enable resident choice, grant notifications, and explicitly bind this phone first."; return
        }
        epoch++; job?.cancel(); busy = false
        update { it.copy(runRequested = true, residentRunId = UUID.randomUUID().toString()) }
        runCatching { InvitationResidentService.start(app) }.onFailure {
            update { it.copy(runRequested = false, pendingAvailabilityOff = true) }
            status = "Resident service could not start. Your saved choice is unchanged; check notification and device permissions."
        }
    }
    fun stopResident() {
        epoch++; job?.cancel(); busy = false
        update { it.copy(runRequested = false, pendingAvailabilityOff = true) }
        InvitationResidentService.stop(app); status = "Resident receiver stopped locally. Home availability will reconcile when connected."
    }
    data class ResidentContext(val profile: RemoteProfileScope, val authorization: Session.PlaybackAuthorization, val runId: String, val installation: String)
    fun residentContext(): ResidentContext? = profile?.let { selected -> authorization?.let { auth ->
        val saved = local
        if (saved?.runRequested == true && saved.residentChoice && saved.phoneSecret != null && saved.loginFingerprint == fingerprint(auth) && Session.playbackAuthorization() == auth) ResidentContext(selected, auth, requireNotNull(saved.residentRunId), requireNotNull(saved.installation)) else null
    } }
    fun residentChanged(expectedProfile: RemoteProfileScope, run: String) {
        if (profile != expectedProfile) return
        runCatching { storage().read() }.onSuccess { saved ->
            if (saved.residentRunId == run) local = saved
        }
    }
    val savedProfiles get() = runCatching { index.read() }.getOrDefault(emptyList())
    fun forgetProfile(saved: InvitationSavedProfile) {
        val selected = saved.scope()
        if (selected == profile) retire()
        synchronized(InvitationNotifications.lock) {
            if (runCatching { index.active() == selected }.getOrDefault(false)) index.activate(null)
            InvitationStorage(app, selected).reset(); index.remove(selected)
        }
        if (selected == profile) local = storage().empty()
        status = "Local saved profile forgotten. Its home installations were not revoked; sign in there and use installation recovery."
    }
    fun canOpenTap(id: String): Boolean {
        val installation = runCatching { InvitationWire.installation(id) }.getOrNull() ?: return false
        val state = local
        if (corrupt || state?.pendingDeletion == true || state?.lostProof == true) { status = "Recover this phone installation before opening its invitation."; return false }
        if (profile?.let { remote?.ownsCompanionProfile(it) } != true) {
            status = "Enable Cinema remotes explicitly for this signed-in profile before opening the invited remote."
            return false
        }
        if (busy || state?.installation != installation || state.phoneSecret == null || authorization == null ||
            state.loginFingerprint != fingerprint(requireNotNull(authorization))) {
            val saved = runCatching { index.read().firstOrNull { row ->
                runCatching { InvitationStorage(app, row.scope()).read().installation == installation }.getOrDefault(false)
            } }.getOrNull()
            status = if (saved == null) "This invitation does not match a readable saved phone installation. Sign in and use installation recovery." else
                "Select and sign in to saved profile " + saved.origin + " (account " + saved.account + ") before opening this invitation."
            return false
        }
        return true
    }
    fun resume() {
        if (profile == null || corrupt) return
        runCatching { synchronized(InvitationNotifications.lock) { storage().read() } }.onSuccess { local = it }
            .onFailure { corrupt = true; status = "Saved invitation records cannot be read. Use explicit local reset and home installation recovery." }
        drainRotation()
        val state = local ?: return
        if (!busy && !corrupt && authorization != null && (state.pendingDeletion || state.pendingAvailabilityOff && !state.runRequested)) refresh()
    }
    fun tap(id: String) = launch { generation ->
        val state = requireNotNull(local); val phone = requireNotNull(state.phone)
        val tapPermit = InvitationTapPermit.capture(state, id, fingerprint(requireNotNull(authorization)),
            remote?.ownsCompanionProfile(requireNotNull(profile)) == true, corrupt)
        val initialGrants = pairedGrants.associateBy { it.receiverId }
        val reply = api().lookup(phone.installation_id, id, requireNotNull(state.phoneSecret))
        if (!current(generation)) return@launch
        check(reply.expires_at > System.currentTimeMillis() / 1000)
        val latest = requireNotNull(local)
        check(tapPermit.permits(latest, reply.receiver_id, fingerprint(requireNotNull(authorization)),
            remote?.ownsCompanionProfile(requireNotNull(profile)) == true, corrupt))
        val grant = pairedGrants.firstOrNull { it.receiverId == reply.receiver_id } ?: error("Pair this phone again")
        check(initialGrants[reply.receiver_id] == grant)
        val owner = requireNotNull(remote)
        check(owner.ownsCompanionProfile(requireNotNull(profile)) && owner.localGrants.any { it.receiverId == grant.receiverId && it.id == grant.id && it.secret == grant.secret })
        owner.select(RemoteDevice(reply.receiver_id, owner.devices.firstOrNull { it.id == reply.receiver_id }?.name ?: "Invited screen", owner.devices.firstOrNull { it.id == reply.receiver_id }?.platform ?: "screen", reply.target, true, false))
        check(grant.receiverId == reply.receiver_id)
        status = "Remote opened. Take control explicitly."
    }
}
