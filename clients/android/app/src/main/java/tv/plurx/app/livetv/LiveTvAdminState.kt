package tv.plurx.app.livetv

import androidx.compose.runtime.Composable
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch
import tv.plurx.app.data.DeveloperReadiness
import tv.plurx.app.data.Session

/**
 * The server's Live TV settings as Settings → Live TV and Settings → Developer
 * both edit them. Load, save and the advisory reads have one owner, so moving a
 * card from one screen to the other moves where it is drawn and nothing else:
 * the same requests, the same generation CAS, the same messages.
 *
 * Administrator authority and generation CAS are server-enforced; nothing here
 * reads a readiness value back into an enable or a save.
 */
@Stable
class LiveTvAdminState internal constructor(
    private val api: LiveTvApi?,
    private val scope: CoroutineScope,
    private val readsGuide: Boolean,
) {
    var saved by mutableStateOf<LiveTvSettings?>(null)
        private set
    var readiness by mutableStateOf<LiveTvReadiness?>(null)
        private set
    var developerReadiness by mutableStateOf<DeveloperReadiness?>(null)
        private set
    var guideReadiness by mutableStateOf<LiveTvGuideReadiness?>(null)
        private set
    var ipv4 by mutableStateOf("")
    var owner by mutableStateOf("")
        private set
    var sessions by mutableStateOf(2)
    var height by mutableStateOf(0)
    var busy by mutableStateOf(false)
        private set
    var message by mutableStateOf("Administrator access is required.")
        private set

    val dirty: Boolean
        get() = saved?.let { ipv4 != it.live_tv_device_ipv4 ||
            sessions != it.live_tv_max_sessions || height != it.live_tv_max_output_height } ?: false

    private fun apply(settings: LiveTvSettings) {
        saved = settings
        Session.displayModeMatch = settings.playback_display_mode_match
        ipv4 = settings.live_tv_device_ipv4
        owner = settings.live_tv_owner_node_id
        sessions = settings.live_tv_max_sessions
        height = settings.live_tv_max_output_height
        readiness = null
    }

    private fun failure(error: Exception): String =
        (error as? LiveTvFailure)?.message ?: liveTvMessage("stream_failed")

    fun load() {
        val client = api ?: run { message = liveTvMessage("invalid_settings"); return }
        if (busy) return
        busy = true
        scope.launch {
            try {
                apply(client.settings())
                developerReadiness = client.developerReadiness()
                // Advisory in the strongest sense: a guide panel that cannot be
                // read must not cost the operator the settings form it sits
                // under, so its failure is not this load's failure. Developer
                // no longer draws the guide, so it does not ask for it.
                if (readsGuide) guideReadiness = runCatching { client.guideReadiness() }.getOrNull()
                message = "Settings loaded. Save, check readiness, then enable."
            }
            catch (error: Exception) { saved = null; message = failure(error) }
            finally { busy = false }
        }
    }

    fun write(change: LiveTvSettingsChange) {
        val previous = saved ?: return
        val client = api ?: run { message = liveTvMessage("invalid_settings"); return }
        if (busy) return
        busy = true
        scope.launch {
            try {
                val result = client.save(previous, change)
                apply(result)
                message = "Saved. Recording is ${if (result.dvr_enabled) "enabled" else "disabled"}; Library channels are ${if (result.library_channels_enabled) "enabled" else "disabled"}; Live TV is ${if (result.live_tv_enabled) "enabled" else "disabled"}."
            } catch (error: Exception) {
                // Never retry an uncertain mutation with stale generation/CAS.
                saved = null; readiness = null
                message = failure(error) + " Reload settings before trying again."
            } finally { busy = false }
        }
    }

    fun refreshDeveloperReadiness() {
        busy = true
        scope.launch {
            try { developerReadiness = (api ?: throw LiveTvFailure("invalid_settings")).developerReadiness() }
            catch (error: Exception) { message = failure(error) }
            finally { busy = false }
        }
    }

    fun refreshGuideReadiness() {
        busy = true
        scope.launch {
            try { guideReadiness = (api ?: throw LiveTvFailure("invalid_settings")).guideReadiness() }
            catch (error: Exception) { message = failure(error) }
            finally { busy = false }
        }
    }

    fun checkSavedConfiguration() {
        val settings = saved ?: return
        busy = true
        scope.launch {
            try {
                val result = (api ?: throw LiveTvFailure("invalid_settings")).readiness()
                if (result.generation != settings.live_tv_config_generation) throw LiveTvFailure("settings_conflict")
                readiness = result
                message = if (result.ready) "Ready. Enable is a separate action." else "Resolve the failed checks before enabling."
            } catch (error: Exception) { readiness = null; message = failure(error) }
            finally { busy = false }
        }
    }
}

/**
 * `LiveTvApi` rejects an unusable origin by throwing, which out of `remember`
 * is an uncaught crash on entering a settings screen rather than the typed
 * message every other Live TV surface shows.
 */
@Composable
fun rememberLiveTvAdminState(origin: String, readsGuide: Boolean): LiveTvAdminState {
    val scope = rememberCoroutineScope()
    val token = Session.token.orEmpty()
    return remember(origin, token, readsGuide) {
        LiveTvAdminState(runCatching { LiveTvApi(origin, token) }.getOrNull(), scope, readsGuide)
    }
}
