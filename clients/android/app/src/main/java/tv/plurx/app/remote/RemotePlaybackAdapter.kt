package tv.plurx.app.remote

import kotlinx.serialization.json.JsonObject

/** Bridges the existing visible ExoPlayer owner; never allocates a player/session. */
internal class RemotePlaybackAdapter {
    data class Owner(val token: String, val scope: String, val capabilities: () -> Set<String>, val summary: () -> JsonObject?,
        val dispatch: (RemoteAction) -> RemoteOutcome, val cancelNetworkGesture: () -> Unit, val effectsEligible: () -> Boolean = { true })
    var owner: Owner? = null; private set
    fun attach(value: Owner) { if (owner?.token != value.token) owner?.cancelNetworkGesture?.invoke(); owner = value }
    fun detach(token: String) { if (owner?.token == token) { owner?.cancelNetworkGesture?.invoke(); owner = null } }
    fun contextFingerprint(scope: String): String? {
        val current = owner?.takeIf { it.scope == scope } ?: return null
        val summary = current.summary()
        return current.token + ":" + current.capabilities().sorted().joinToString(",") + ":" + summary?.get("media") + ":" + summary?.get("tracks")
    }
    fun physicalInput() { owner?.cancelNetworkGesture?.invoke() }
    fun dispatch(action: RemoteAction, scope: String): RemoteOutcome {
        val current = owner?.takeIf { it.scope == scope } ?: return RemoteOutcome.Unavailable
        if (!current.effectsEligible()) return RemoteOutcome.Restricted
        if (action.type !in current.capabilities()) return RemoteOutcome.Unsupported
        return current.dispatch(action)
    }
}
