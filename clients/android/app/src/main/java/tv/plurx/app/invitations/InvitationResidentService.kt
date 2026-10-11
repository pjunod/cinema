package tv.plurx.app.invitations

import android.app.*
import android.content.*
import android.content.pm.ServiceInfo
import android.net.*
import android.os.IBinder
import androidx.core.app.NotificationCompat
import kotlinx.coroutines.*
import okhttp3.Dns
import tv.plurx.app.data.Session
import java.net.InetAddress

/** User-started connected-device networking. No process-wide binding and no restart owner. */
class InvitationResidentService : Service() {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private var callback: ConnectivityManager.NetworkCallback? = null
    private var poll: Job? = null
    private var setup: Job? = null
    @Volatile private var running = false
    private val lifecycleLock = Any()
    private val lifetime = InvitationResidentLifetime()
    private val teardown = InvitationServiceTeardown()
    private fun InvitationModel.ResidentContext.run() = InvitationResidentLifetime.Run(profile.digest, installation, runId)
    private var context: InvitationModel.ResidentContext? = null
    companion object {
        private const val START = "tv.plurx.invitation.START"
                private const val CHANNEL = "cinema_resident"
        @Volatile private var activeRun: String? = null
        fun stopOwned(context: Context, run: String) { if (activeRun == run) context.stopService(Intent(context, InvitationResidentService::class.java)) }
        fun start(context: Context) { context.startForegroundService(Intent(context, InvitationResidentService::class.java).setAction(START)) }
        fun stop(context: Context) { context.stopService(Intent(context, InvitationResidentService::class.java)) }
    }
    override fun onBind(intent: Intent?): IBinder? = null
    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        teardown.observe(startId)
        if (intent?.action != START) { if (!running) stopSelfResult(startId); return START_NOT_STICKY }
        val owner = InvitationRuntime.get(this).residentContext()
        if (owner != null && !teardown.start(owner.run(), startId)) {
            if (owner != null && context?.runId != owner.runId) {
                // A rapid Stop/Start waits for old socket cleanup; it never silently queues a restart.
                synchronized(InvitationNotifications.lock) { runCatching {
                    val storage = InvitationStorage(this, owner.profile); val saved = storage.read()
                    if (saved.residentRunId == owner.runId) storage.save(saved.copy(runRequested = false, pendingAvailabilityOff = true))
                } }
                InvitationRuntime.get(this).residentChanged(owner.profile, owner.runId)
            }
            teardown.rejectedStopId()?.let(::stopSelfResult)
            return START_NOT_STICKY
        }
        if (owner == null) { stopSelfResult(startId); return START_NOT_STICKY }
        if (!InvitationNotifications.permission(this)) { stopOwned(owner); return START_NOT_STICKY }
        check(lifetime.begin(owner.run()))
        context = owner; running = true; activeRun = owner.runId
        val manager = getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(NotificationChannel(CHANNEL, "Cinema resident receiver", NotificationManager.IMPORTANCE_LOW))
        val stopIntent = PendingIntent.getBroadcast(this, 0, Intent(this, InvitationStopReceiver::class.java).setAction("tv.plurx.invitation.STOP")
            .putExtra("origin", owner.profile.origin).putExtra("instance", owner.profile.instance).putExtra("account", owner.profile.account)
            .putExtra("run", owner.runId).putExtra("installation", owner.installation)
            .setData(Uri.parse("cinema-resident:" + owner.runId)), PendingIntent.FLAG_IMMUTABLE)
        val notification = NotificationCompat.Builder(this, CHANNEL).setSmallIcon(android.R.drawable.ic_dialog_info)
            .setContentTitle("Cinema invitation receiver is running").setContentText("Receiving screen invitations over Wi-Fi")
            .setOngoing(true).addAction(android.R.drawable.ic_menu_close_clear_cancel, "Stop", stopIntent).build()
        try { androidx.core.app.ServiceCompat.startForeground(this, 4201, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE) }
        catch (_: Exception) { stopOwned(owner); return START_NOT_STICKY }
        val connectivity = getSystemService(ConnectivityManager::class.java)
        val listener = object : ConnectivityManager.NetworkCallback() {
            override fun onAvailable(network: Network) {
                synchronized(lifecycleLock) {
                    if (!running || !lifetime.available(owner.run(), network)) return
                     setup?.cancel(); setup = null
                    poll = scope.launch(start = CoroutineStart.LAZY) { runReceiver(owner, network) }.also { it.start() }
                }
            }
            override fun onLost(network: Network) { if (synchronized(lifecycleLock) { lifetime.lost(owner.run(), network) }) { stopOwned(owner) } }
            override fun onUnavailable() { stopOwned(owner) }
        }
        callback = listener
        runCatching { connectivity.requestNetwork(NetworkRequest.Builder().addTransportType(NetworkCapabilities.TRANSPORT_WIFI).build(), listener, 15000) }
            .onFailure { stopOwned(owner) }
        setup = scope.launch { delay(16000); if (poll == null) { stopOwned(owner) } }
        return START_NOT_STICKY
    }
    private fun eligible(owner: InvitationModel.ResidentContext): Boolean = running && lifetime.owns(owner.run()) && Session.playbackAuthorization() == owner.authorization && runCatching {
        val state = InvitationStorage(this, owner.profile).read()
        state.runRequested && state.residentRunId == owner.runId && state.installation == owner.installation && state.residentChoice && !state.pendingDeletion && !state.lostProof && InvitationNotifications.permission(this)
    }.getOrDefault(false)
    private suspend fun runReceiver(owner: InvitationModel.ResidentContext, network: Network) {
        try {
            val client = InvitationTransport.client().newBuilder().socketFactory(network.socketFactory)
                .dns(object : Dns { override fun lookup(hostname: String): List<InetAddress> = network.getAllByName(hostname).toList() }).build()
            val api = InvitationApi(owner.authorization, client) { eligible(owner) }
            val storage = InvitationStorage(this, owner.profile)
            var first = synchronized(InvitationNotifications.lock) { storage.read() }
            var cursor: String? = null; var proved: InvitationPhone? = null
            do {
                val page = api.phones(cursor); if (!eligible(owner)) return
                proved = proved ?: page.phones.firstOrNull { it.installation_id == owner.installation }
                require(page.next_cursor == null || page.next_cursor != cursor); cursor = page.next_cursor
            } while (cursor != null)
            val phone = requireNotNull(proved); require(phone.platform == "android")
            synchronized(InvitationNotifications.lock) {
                val saved = storage.read(); check(saved.runRequested && saved.residentRunId == owner.runId)
                storage.save(saved.copy(phone = phone)); first = storage.read()
            }
            val availability = api.availability(phone.installation_id, phone.phone_generation, true, true, requireNotNull(first.phoneSecret))
            if (!eligible(owner)) return
            synchronized(InvitationNotifications.lock) {
                val current = storage.read(); check(current.runRequested && current.residentRunId == owner.runId && (current.phone?.phone_generation == phone.phone_generation || current.phone?.phone_generation == availability.phone.phone_generation))
                storage.save(current.copy(phone = availability.phone, pendingAvailabilityOff = false))
            }
            var after: String? = null
            val consents = mutableListOf<InvitationConsent>()
            do {
                val page = api.consents(phone.installation_id, requireNotNull(first.phoneSecret), after)
                if (!eligible(owner)) return
                consents += page.consents; require(consents.size <= 160)
                require(page.next_cursor == null || page.next_cursor != after); after = page.next_cursor
            } while (after != null)
            synchronized(InvitationNotifications.lock) {
                val latest = storage.read(); check(latest.runRequested && latest.residentRunId == owner.runId)
                storage.save(latest.copy(consents = consents))
            }
            withContext(Dispatchers.Main) { InvitationRuntime.get(this@InvitationResidentService).residentChanged(owner.profile, owner.runId) }
            while (eligible(owner)) {
                val saved = synchronized(InvitationNotifications.lock) { storage.read() }
                val page = api.poll(requireNotNull(saved.installation), saved.revision, requireNotNull(saved.phoneSecret))
                if (!eligible(owner)) break
                var processed = true
                for (item in page.invitations) {
                    if (!eligible(owner)) { processed = false; break }
                    // Deduped/expired/locally OFF entries are processed; failures never create notification retries.
                    if (InvitationNotifications.receiveOwned(this, item.invitation_id, "android_resident", item.expires_at) == InvitationNotifications.Receipt.Unavailable) { processed = false; break }
                }
                check(processed) { "Local notification state unavailable" }
                if (eligible(owner)) synchronized(InvitationNotifications.lock) {
                    val latest = storage.read(); if (latest.runRequested && latest.residentRunId == owner.runId) storage.save(latest.copy(revision = maxOf(latest.revision, page.revision)))
                }
                if (page.invitations.isEmpty()) delay(200)
            }
        } catch (_: CancellationException) { }
        catch (_: Exception) { }
        finally { stopOwned(owner) }
    }
    private fun stopOwned(owner: InvitationModel.ResidentContext) {
        if (!teardown.retire(owner.run())) return
        running = false; setup?.cancel(); poll?.cancel()
        val ownedCallback = synchronized(lifecycleLock) { callback.also { callback = null } }
        ownedCallback?.let { runCatching { getSystemService(ConnectivityManager::class.java).unregisterNetworkCallback(it) } }
        endLocally(owner)
        teardown.completed(owner.run())?.let(::stopSelfResult)
    }
    private fun endLocally(owner: InvitationModel.ResidentContext) {
        if (lifetime.end(owner.run())) running = false
        synchronized(InvitationNotifications.lock) { runCatching {
            val storage = InvitationStorage(this, owner.profile); val saved = storage.read()
            if (saved.residentRunId == owner.runId && saved.installation == owner.installation) storage.save(saved.copy(runRequested = false, pendingAvailabilityOff = true))
        } }
        android.os.Handler(android.os.Looper.getMainLooper()).post { InvitationRuntime.get(this).residentChanged(owner.profile, owner.runId) }
    }
    override fun onDestroy() {
        running = false; poll?.cancel(); setup?.cancel(); scope.cancel()
        callback?.let { runCatching { getSystemService(ConnectivityManager::class.java).unregisterNetworkCallback(it) } }; callback = null
        context?.let { owner -> endLocally(owner); if (activeRun == owner.runId) activeRun = null }; context = null
        stopForeground(STOP_FOREGROUND_REMOVE)
        super.onDestroy()
    }
}

class InvitationStopReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val run = intent.getStringExtra("run") ?: return
        val installation = intent.getStringExtra("installation") ?: return
        val owned = synchronized(InvitationNotifications.lock) { runCatching {
            tv.plurx.app.remote.RemoteWire.uuid(run); tv.plurx.app.remote.RemoteWire.uuid(installation)
            val profile = tv.plurx.app.remote.RemoteProfileScope.create(requireNotNull(intent.getStringExtra("origin")),
                requireNotNull(intent.getStringExtra("instance")), intent.getLongExtra("account", 0))
            val storage = InvitationStorage(context, profile); val state = storage.read()
            if (state.residentRunId != run || state.installation != installation || !state.runRequested) false else {
                storage.save(state.copy(runRequested = false, pendingAvailabilityOff = true)); true
            }
        }.getOrDefault(false) }
        if (owned) {
            val profile = runCatching { tv.plurx.app.remote.RemoteProfileScope.create(requireNotNull(intent.getStringExtra("origin")), requireNotNull(intent.getStringExtra("instance")), intent.getLongExtra("account", 0)) }.getOrNull()
            if (profile != null) android.os.Handler(android.os.Looper.getMainLooper()).post { InvitationRuntime.get(context).residentChanged(profile, run) }
            InvitationResidentService.stopOwned(context, run)
        }
    }
}
